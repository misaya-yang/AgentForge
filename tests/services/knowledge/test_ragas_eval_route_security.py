from __future__ import annotations

import json
from types import SimpleNamespace

import pytest
from ai_gateway_core.auth.gateway_secret import GatewaySecret
from ai_gateway_core.auth.gateway_secret_middleware import GatewaySecretAuthMiddleware
from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient
from knowledge_service.api.routes import eval as eval_route
from knowledge_service.api.routes.eval import (
    RagasEvalRequest,
    RagasJudgeSelector,
    _build_server_llm_config,
    _validate_request_budget,
    require_ragas_eval_enabled,
    require_verified_gateway,
)
from knowledge_service.config import RagasEvalSettings, Settings
from knowledge_service.services.eval import MetricResult
from pydantic import ValidationError

from src.api.schemas.eval import KbRagasScoreRetrievalRequest


def _request_state(*, verified: bool = False) -> SimpleNamespace:
    return SimpleNamespace(
        state=SimpleNamespace(gateway_secret_verified=verified),
    )


def test_internal_ragas_route_requires_verified_gateway_state() -> None:
    with pytest.raises(HTTPException) as exc_info:
        require_verified_gateway(_request_state())  # type: ignore[arg-type]

    assert exc_info.value.status_code == 401
    require_verified_gateway(_request_state(verified=True))  # type: ignore[arg-type]


def test_internal_ragas_endpoint_accepts_only_middleware_verified_request(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class _FakeService:
        def __init__(self, llm_config, *, embedding=None) -> None:
            self.llm_config = llm_config
            self.embedding = embedding

        async def evaluate_retrieval(self, **_kwargs):
            return [
                MetricResult(
                    metric="context_relevancy",
                    score=0.9,
                    explanation="relevant",
                    label="pass",
                )
            ]

        async def close(self) -> None:
            return None

    monkeypatch.setattr(eval_route, "KBRagasEvalService", _FakeService)
    monkeypatch.setattr(
        eval_route,
        "resolve_dashscope",
        lambda _domain: ("server-owned-test-key", "https://judge.test/compatible-mode"),
    )
    settings = Settings(
        ragas_eval=RagasEvalSettings(
            enabled=True,
            model="qwen-test",
            allowed_models=["qwen-test"],
        )
    )
    secret = GatewaySecret(secret="shared-test-secret")
    app = FastAPI()
    app.dependency_overrides[eval_route.get_settings] = lambda: settings
    app.include_router(eval_route.router)
    app.add_middleware(
        GatewaySecretAuthMiddleware,
        gateway_secret=secret,
        allow_anonymous=False,
    )
    payload = {"query": "refund policy", "contexts": ["30 day refunds"]}
    body = json.dumps(payload, separators=(",", ":")).encode()

    unsigned = TestClient(app).post("/internal/eval/ragas", json=payload)
    signed = TestClient(app).post(
        "/internal/eval/ragas",
        content=body,
        headers={
            "content-type": "application/json",
            secret.header_name: secret.sign(
                method="POST",
                path="/internal/eval/ragas",
                body=body,
            ),
        },
    )

    assert unsigned.status_code == 401
    assert signed.status_code == 200
    assert signed.json()["judge_model"] == "qwen-test"


def test_internal_ragas_defaults_off_before_provider_or_service_resolution(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def forbidden_provider(_domain: str):
        raise AssertionError("provider resolution started while RAGAS was disabled")

    class _ForbiddenService:
        def __init__(self, *_args, **_kwargs) -> None:
            raise AssertionError("RAGAS service started while route was disabled")

    monkeypatch.setattr(eval_route, "resolve_dashscope", forbidden_provider)
    monkeypatch.setattr(eval_route, "KBRagasEvalService", _ForbiddenService)
    settings = Settings()
    secret = GatewaySecret(secret="shared-test-secret")
    app = FastAPI()
    app.dependency_overrides[eval_route.get_settings] = lambda: settings
    app.include_router(eval_route.router)
    app.add_middleware(
        GatewaySecretAuthMiddleware,
        gateway_secret=secret,
        allow_anonymous=False,
    )

    payload = {"query": "refund policy", "contexts": ["30 day refunds"]}
    body = json.dumps(payload, separators=(",", ":")).encode()
    response = TestClient(app).post(
        "/internal/eval/ragas",
        content=body,
        headers={
            "content-type": "application/json",
            secret.header_name: secret.sign(
                method="POST",
                path="/internal/eval/ragas",
                body=body,
            ),
        },
    )

    assert response.status_code == 403
    with pytest.raises(HTTPException) as exc_info:
        require_ragas_eval_enabled(settings)
    assert exc_info.value.status_code == 403


@pytest.mark.parametrize(
    "unsafe_field",
    ["api_key", "base_url", "system_prompt", "timeout_seconds", "max_tokens"],
)
def test_ragas_requests_reject_caller_owned_judge_controls(unsafe_field: str) -> None:
    payload = {
        "query": "refund policy",
        "contexts": ["Refunds are available for 30 days."],
        "llm_config": {unsafe_field: "caller-controlled"},
    }

    with pytest.raises(ValidationError):
        RagasEvalRequest.model_validate(payload)
    with pytest.raises(ValidationError):
        KbRagasScoreRetrievalRequest.model_validate(payload)


def test_ragas_judge_uses_server_credentials_endpoint_and_timeout(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        eval_route,
        "resolve_dashscope",
        lambda _domain: ("server-owned-test-key", "https://judge.test/compatible-mode"),
    )
    settings = RagasEvalSettings(
        provider="dashscope",
        model="qwen-test",
        allowed_providers=["dashscope"],
        allowed_models=["qwen-test"],
        timeout_seconds=17,
    )

    llm_config = _build_server_llm_config(
        RagasJudgeSelector(provider="dashscope", model="qwen-test"),
        settings,
    )

    assert llm_config.api_key == "server-owned-test-key"
    assert llm_config.base_url == "https://judge.test/compatible-mode/v1"
    assert llm_config.timeout_seconds == 17
    assert llm_config.max_tokens == 512


@pytest.mark.parametrize(
    ("selector", "message"),
    [
        (RagasJudgeSelector(provider="gemini"), "provider is not allowlisted"),
        (RagasJudgeSelector(model="unapproved-model"), "model is not allowlisted"),
    ],
)
def test_ragas_judge_rejects_non_allowlisted_selector(
    selector: RagasJudgeSelector,
    message: str,
) -> None:
    with pytest.raises(ValueError, match=message):
        _build_server_llm_config(selector, RagasEvalSettings())


def test_ragas_request_budget_bounds_contexts_and_metric_calls() -> None:
    settings = RagasEvalSettings(
        max_contexts=1,
        max_context_chars=2_000,
        max_total_context_chars=1_000,
        max_metrics=1,
    )
    body = RagasEvalRequest(
        query="question",
        contexts=["12345678"],
        metrics=["context_relevancy"],
    )
    _validate_request_budget(body, settings)

    with pytest.raises(ValueError, match="at most 1 contexts"):
        _validate_request_budget(
            body.model_copy(update={"contexts": ["one", "two"]}),
            settings,
        )
    with pytest.raises(ValueError, match="total character budget"):
        _validate_request_budget(
            body.model_copy(update={"contexts": ["x" * 1_001]}),
            settings,
        )
    with pytest.raises(ValueError, match="at most 1 metrics"):
        _validate_request_budget(
            body.model_copy(update={"metrics": ["context_relevancy", "faithfulness"]}),
            settings,
        )
