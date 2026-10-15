"""Gateway client for knowledge-service KB RAGAS evaluation."""

from __future__ import annotations

import math
import os
import threading
from dataclasses import dataclass
from typing import Any

import httpx
from ai_gateway_core.auth.gateway_secret import GatewaySecret
from ai_gateway_core.comm.client import (
    InternalServiceClient,
    InternalServiceClientConfig,
    InternalServiceHTTPError,
)
from ai_gateway_core.knowledge import KB_SERVICE_URL
from ai_gateway_core.logging import get_logger

logger = get_logger(__name__)

_INVALID_RESULTS_ERROR = "knowledge-service RAGAS eval returned invalid results"

_gateway_secret_signer: GatewaySecret | None = None
_signer_lock = threading.Lock()


def _get_signer() -> GatewaySecret | None:
    global _gateway_secret_signer
    if _gateway_secret_signer is not None:
        return _gateway_secret_signer
    with _signer_lock:
        if _gateway_secret_signer is not None:
            return _gateway_secret_signer
        secret = os.environ.get("AI_PLATFORM_INTERNAL_TOKEN", "").strip()
        if not secret:
            return None
        _gateway_secret_signer = GatewaySecret(
            secret=secret,
            caller_service="gateway",
            audience="knowledge-service",
            allowed_path_prefixes=("/api/v1",),
        )
        return _gateway_secret_signer


@dataclass(frozen=True)
class KbRagasMetricResult:
    metric: str
    score: float
    explanation: str
    label: str
    judge_model: str | None = None
    failure_kind: str | None = None


class KbRagasClient:
    def __init__(self, *, base_url: str | None = None, timeout_s: float = 120.0) -> None:
        self.base_url = (base_url or os.getenv("KB_SERVICE_URL", KB_SERVICE_URL)).rstrip("/")
        self.timeout = httpx.Timeout(connect=5.0, read=timeout_s, write=30.0, pool=10.0)
        self._service_client: InternalServiceClient | None = None

    def _get_service_client(self) -> InternalServiceClient:
        if self._service_client is None:
            self._service_client = InternalServiceClient(
                InternalServiceClientConfig(
                    name="knowledge-service",
                    base_url=self.base_url,
                    timeout=self.timeout,
                    gateway_secret=_get_signer(),
                )
            )
        return self._service_client

    async def close(self) -> None:
        if self._service_client:
            await self._service_client.close()

    async def evaluate_retrieval(
        self,
        *,
        query: str,
        contexts: list[str],
        answer: str | None = None,
        metrics: list[str] | None = None,
        ground_truth: str | None = None,
        llm_config: dict[str, Any] | None = None,
    ) -> list[KbRagasMetricResult]:
        safe_llm_config = None
        if llm_config:
            unsupported = sorted(set(llm_config) - {"provider", "model"})
            if unsupported:
                raise ValueError(
                    "KB RAGAS judge selector only accepts provider and model"
                )
            safe_llm_config = {
                key: llm_config[key]
                for key in ("provider", "model")
                if llm_config.get(key) is not None
            }
        payload = {
            "query": query,
            "contexts": contexts,
            "answer": answer,
            "metrics": metrics,
            "ground_truth": ground_truth,
            "llm_config": safe_llm_config,
        }
        try:
            body = await self._get_service_client().request_json(
                "POST",
                "/api/v1/internal/eval/ragas",
                json=payload,
                headers={"Content-Type": "application/json"},
            )
        except InternalServiceHTTPError as exc:
            raise RuntimeError(
                f"knowledge-service RAGAS eval failed with HTTP {exc.status_code}"
            ) from exc

        if not isinstance(body, dict):
            raise ValueError(_INVALID_RESULTS_ERROR)
        raw_results = body.get("results")
        if not isinstance(raw_results, list) or not raw_results:
            raise ValueError(_INVALID_RESULTS_ERROR)

        judge_model = str(body.get("judge_model") or "")
        results: list[KbRagasMetricResult] = []
        seen_metrics: set[str] = set()
        for item in raw_results:
            if not isinstance(item, dict):
                raise ValueError(_INVALID_RESULTS_ERROR)
            metric_value = item.get("metric")
            metric = metric_value.strip() if isinstance(metric_value, str) else ""
            score_value = item.get("score")
            if (
                not metric
                or metric in seen_metrics
                or isinstance(score_value, bool)
                or not isinstance(score_value, int | float)
            ):
                raise ValueError(_INVALID_RESULTS_ERROR)
            score = float(score_value)
            if not math.isfinite(score) or score < 0.0 or score > 1.0:
                raise ValueError(_INVALID_RESULTS_ERROR)

            label_value = item.get("label")
            label = label_value.strip() if isinstance(label_value, str) else ""
            failure_kind_value = item.get("failure_kind")
            failure_kind = (
                failure_kind_value.strip()
                if isinstance(failure_kind_value, str)
                else failure_kind_value
            )
            if label not in {"pass", "fail", "review"}:
                raise ValueError(_INVALID_RESULTS_ERROR)
            if label == "review":
                if failure_kind not in {"semantic_review", "infrastructure"}:
                    raise ValueError(_INVALID_RESULTS_ERROR)
            elif failure_kind is not None:
                raise ValueError(_INVALID_RESULTS_ERROR)

            seen_metrics.add(metric)
            results.append(
                KbRagasMetricResult(
                    metric=metric,
                    score=score,
                    explanation=str(item.get("explanation") or ""),
                    label=label,
                    judge_model=judge_model or None,
                    failure_kind=failure_kind,
                )
            )
        return results


def build_kb_ragas_complete(client: KbRagasClient | None = None) -> KbRagasClient:
    return client or KbRagasClient()
