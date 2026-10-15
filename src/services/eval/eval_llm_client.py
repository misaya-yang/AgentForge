"""Gateway Agent Runtime backed LLM completion for eval judge runs."""

from __future__ import annotations

import json
import os
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta

import httpx
import jwt
from ai_gateway_core.eval.evaluator_executor import LlmCompleteContext
from ai_gateway_core.logging import get_logger

from ...core.auth.jwt_config import get_jwt_algorithms, get_jwt_secret

logger = get_logger(__name__)

RESPONSES_PATH = "/v1/responses"
DEFAULT_JUDGE_MODEL_ID = "qwen3.7-plus"
EVAL_WORKER_USER_ID = "eval-worker"


def _read_float_env(name: str, default: float) -> float:
    raw = os.getenv(name, "").strip()
    if not raw:
        return default
    try:
        return float(raw)
    except ValueError:
        logger.warning("Invalid %s=%r; using default %s", name, raw, default)
        return default


@dataclass(frozen=True)
class EvalLlmSettings:
    enabled: bool = True
    gateway_base_url: str = "http://gateway:8080"
    default_judge_model_id: str = DEFAULT_JUDGE_MODEL_ID
    system_tenant_id: str = "default"
    timeout_read_s: float = 120.0


def load_eval_llm_settings() -> EvalLlmSettings:
    enabled = os.getenv("EVAL_LLM_ENABLED", "true").strip().lower() not in {
        "0",
        "false",
        "no",
        "off",
    }
    return EvalLlmSettings(
        enabled=enabled,
        gateway_base_url=os.getenv(
            "EVAL_LLM_GATEWAY_URL",
            os.getenv("GATEWAY_URL", "http://gateway:8080"),
        ).rstrip("/"),
        default_judge_model_id=os.getenv("EVAL_JUDGE_MODEL_ID", DEFAULT_JUDGE_MODEL_ID).strip()
        or DEFAULT_JUDGE_MODEL_ID,
        system_tenant_id=os.getenv("EVAL_SYSTEM_TENANT_ID", "default").strip() or "default",
        timeout_read_s=_read_float_env("EVAL_LLM_TIMEOUT_READ_S", 120.0),
    )


def _build_internal_jwt(*, tenant_id: str, delegation: dict[str, str] | None = None) -> str:
    user_id = EVAL_WORKER_USER_ID
    if delegation is not None:
        if (delegation.get("tenant_id") != tenant_id or delegation.get("actor") != "eval-worker"
                or not all(delegation.get(key) for key in ("subject", "run_id", "job_id"))):
            raise RuntimeError("AGENT_EVAL_DELEGATION_INVALID")
        user_id = delegation["subject"]
    configured_secret = (
        os.getenv("GATEWAY_AUTHENTICATION__JWT__SECRET", "").strip()
        or os.getenv("JWT_SECRET", "").strip()
    )
    algorithms_raw = os.getenv("GATEWAY_AUTHENTICATION__JWT__ALGORITHMS", "").strip()
    configured_algorithms: list[str] | None = None
    if algorithms_raw:
        try:
            parsed = json.loads(algorithms_raw)
        except json.JSONDecodeError as exc:
            raise RuntimeError("Eval judge JWT algorithms are invalid") from exc
        if isinstance(parsed, list):
            configured_algorithms = [str(value) for value in parsed]
    algorithm = get_jwt_algorithms(configured_algorithms)[0]
    now = datetime.now(UTC)
    payload: dict[str, object] = {
        "sub": user_id,
        "user_id": user_id,
        "tenant_id": tenant_id,
        "roles": ["user"],
        "permissions": [],
        "tier": "normal",
        "iat": now,
        "exp": now + timedelta(minutes=5),
    }
    if delegation is not None:
        # On behalf of a DB-verified job creator. Gateway may resolve current
        # permissions from that user's DB grants; token roles are not a ceiling.
        payload["act"] = {"sub": "eval-worker", "purpose": "eval_candidate",
                          "tenant_id": tenant_id, "run_id": delegation["run_id"],
                          "job_id": delegation["job_id"]}
    audience = os.getenv("GATEWAY_AUTHENTICATION__JWT__AUDIENCE", "").strip()
    issuer = os.getenv("GATEWAY_AUTHENTICATION__JWT__ISSUER", "").strip()
    if audience:
        payload["aud"] = audience
    if issuer:
        payload["iss"] = issuer
    return jwt.encode(payload, get_jwt_secret(configured_secret), algorithm=algorithm)


class EvalGatewayLlmClient:
    """Call the Gateway's single Agent Runtime path for bounded judge prompts."""

    def __init__(self, settings: EvalLlmSettings | None = None) -> None:
        self.settings = settings or load_eval_llm_settings()

    async def complete(
        self,
        model_id: str,
        prompt: str,
        context: LlmCompleteContext,
    ) -> str:
        tenant_id = context.tenant_id or self.settings.system_tenant_id
        body = {
            "input": prompt,
            "model": model_id or self.settings.default_judge_model_id,
            "temperature": 0,
            "max_output_tokens": 512,
            "tools": [],
            "tool_choice": "none",
            "parallel_tool_calls": False,
            "stream": False,
        }
        if context.model_ref is not None:
            body["expected_model_ref"] = context.model_ref
        if context.provider_id:
            body["provider_id"] = context.provider_id
        encoded = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        headers = self._headers(tenant_id=tenant_id)

        timeout = httpx.Timeout(connect=5.0, read=self.settings.timeout_read_s, write=30.0, pool=10.0)
        async with httpx.AsyncClient(
            base_url=self.settings.gateway_base_url, timeout=timeout,
            trust_env=False, follow_redirects=False,
        ) as client:
            response = await client.post(RESPONSES_PATH, headers=headers, content=encoded)

        if response.status_code < 200 or response.status_code >= 300:
            raise RuntimeError(f"Gateway Agent Runtime judge failed with HTTP {response.status_code}")

        payload = response.json()
        content = str(payload.get("output_text") or "").strip()
        if not content:
            raise RuntimeError("Gateway Agent Runtime judge returned empty content")
        return content

    def _headers(self, *, tenant_id: str) -> dict[str, str]:
        headers = {
            "Authorization": f"Bearer {_build_internal_jwt(tenant_id=tenant_id)}",
            "Content-Type": "application/json",
            "Accept": "application/json",
        }
        return headers


def build_eval_llm_complete(
    settings: EvalLlmSettings | None = None,
) -> Callable[..., Awaitable[str]] | None:
    """Factory for EvaluatorExecutor.llm_complete."""
    resolved = settings or load_eval_llm_settings()
    if not resolved.enabled:
        logger.info("Eval LLM judge disabled via EVAL_LLM_ENABLED")
        return None

    client = EvalGatewayLlmClient(resolved)

    async def _complete(model_id: str, prompt: str, context: LlmCompleteContext | None = None) -> str:
        ctx = context or LlmCompleteContext(tenant_id=resolved.system_tenant_id)
        judge_model = model_id or resolved.default_judge_model_id
        return await client.complete(judge_model, prompt, ctx)

    return _complete
