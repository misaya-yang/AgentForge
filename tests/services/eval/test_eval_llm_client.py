from __future__ import annotations

import json

import pytest
from ai_gateway_core.eval.evaluator_executor import LlmCompleteContext

from src.services.eval.eval_llm_client import (
    EvalGatewayLlmClient,
    EvalLlmSettings,
    build_eval_llm_complete,
    load_eval_llm_settings,
)
from src.services.eval.eval_outbox_worker import init_eval_outbox_worker


@pytest.mark.asyncio
async def test_eval_judge_posts_bounded_tool_free_response(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: dict[str, object] = {}

    class _FakeResponse:
        status_code = 200

        def json(self) -> dict[str, str]:
            return {"output_text": '{"numeric_value": 0.8, "confidence": 0.7, "label": "pass"}'}

    async def _fake_post(self, url: str, **kwargs):
        captured["url"] = url
        captured["headers"] = kwargs.get("headers")
        captured["body"] = json.loads(kwargs["content"].decode("utf-8"))
        return _FakeResponse()

    monkeypatch.setattr("httpx.AsyncClient.post", _fake_post)
    monkeypatch.setenv("GATEWAY_AUTHENTICATION__JWT__SECRET", "x" * 32)

    client = EvalGatewayLlmClient(
        EvalLlmSettings(
            enabled=True,
            gateway_base_url="http://gateway.test",
            default_judge_model_id="judge-model",
        )
    )
    text = await client.complete(
        "judge-model",
        "Score this answer",
        LlmCompleteContext(tenant_id="tenant-a", trace_family="assistant"),
    )

    assert "0.8" in text
    assert captured["url"] == "/v1/responses"
    headers = captured["headers"]
    assert isinstance(headers, dict)
    assert str(headers["Authorization"]).startswith("Bearer ")
    assert "X-Tenant-Id" not in headers
    assert "X-User-Roles" not in headers
    body = captured["body"]
    assert isinstance(body, dict)
    assert body["tools"] == []
    assert body["tool_choice"] == "none"
    assert body["parallel_tool_calls"] is False
    assert body["max_output_tokens"] == 512
    assert body["stream"] is False


def test_build_eval_llm_complete_disabled_returns_none(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVAL_LLM_ENABLED", "false")
    assert build_eval_llm_complete() is None


def test_load_eval_llm_settings_ignores_invalid_timeout(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVAL_LLM_TIMEOUT_READ_S", "not-a-number")

    settings = load_eval_llm_settings()

    assert settings.timeout_read_s == 120.0


@pytest.mark.asyncio
async def test_build_eval_llm_complete_passes_context(monkeypatch: pytest.MonkeyPatch) -> None:
    seen: dict[str, str] = {}

    class _StubClient:
        async def complete(self, model_id: str, prompt: str, context: LlmCompleteContext) -> str:
            seen["model_id"] = model_id
            seen["tenant_id"] = context.tenant_id
            seen["prompt"] = prompt
            return '{"numeric_value": 1}'

    monkeypatch.setattr(
        "src.services.eval.eval_llm_client.EvalGatewayLlmClient",
        lambda _settings: _StubClient(),
    )
    complete = build_eval_llm_complete(EvalLlmSettings(enabled=True))
    assert complete is not None
    result = await complete(
        "judge-model",
        "hello",
        LlmCompleteContext(tenant_id="tenant-b", trace_family="rag"),
    )
    assert result == '{"numeric_value": 1}'
    assert seen["tenant_id"] == "tenant-b"


def test_init_eval_outbox_worker_wires_llm_complete(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVAL_LLM_ENABLED", "true")

    class _Database:
        enabled = True

    worker = init_eval_outbox_worker(_Database())
    assert worker is not None
    assert worker.executor.llm_complete is not None
