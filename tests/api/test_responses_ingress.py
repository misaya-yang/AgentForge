"""Gateway boundary tests for the public ``POST /v1/responses`` alias."""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_contracts.agent_launch import ResolvedAgentLaunchV1
from fastapi import FastAPI
from fastapi.testclient import TestClient

from src.api.v1 import responses as responses_route
from src.core.auth.user_resolver import UserContext


class _ModelMeta:
    async def get_access_level(self, tenant_id: str, model_id: str) -> str | None:
        assert tenant_id == "tenant-1"
        return "public" if model_id == "qwen3.7-plus" else None


def _user(*, authenticated: bool = True, tenant_id: str = "tenant-1") -> UserContext:
    return UserContext(
        user_id="user-1",
        tenant_id=tenant_id,
        tier="normal",
        roles=["user"],
        is_authenticated=authenticated,
    )


def _app(user: UserContext) -> FastAPI:
    app = FastAPI()
    app.include_router(responses_route.router, prefix="/v1")
    app.state.model_meta = _ModelMeta()
    app.state.multi_rate_limiter = None
    app.dependency_overrides[responses_route.get_user_context] = lambda: user
    return app


def test_public_responses_requires_authenticated_tenant_identity(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    with TestClient(_app(_user(authenticated=False))) as client:
        response = client.post(
            "/v1/responses",
            json={"model": "qwen3.7-plus", "input": "hello"},
        )

    assert response.status_code == 401
    assert response.json()["error"]["type"] == "authentication_error"


def test_public_responses_requires_runtime_for_authenticated_default_tenant(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def ensure_session(*_args: Any, **_kwargs: Any) -> None:
        return None

    monkeypatch.setattr(responses_route, "ensure_agent_runtime_session", ensure_session)
    class DefaultModelMeta:
        async def get_access_level(self, tenant_id: str, model_id: str) -> str | None:
            assert tenant_id == "default"
            assert model_id == "qwen3.7-plus"
            return "public"

    app = _app(_user(tenant_id="default"))
    app.state.model_meta = DefaultModelMeta()

    with TestClient(app) as client:
        response = client.post(
            "/v1/responses",
            json={"model": "qwen3.7-plus", "input": "hello"},
        )

    assert response.status_code == 503
    assert response.json()["error"]["code"] == "agent_runtime_unavailable"


def test_public_responses_rejects_public_tenant_even_when_authenticated(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    with TestClient(_app(_user(tenant_id="public"))) as client:
        response = client.post(
            "/v1/responses",
            json={"model": "qwen3.7-plus", "input": "hello"},
        )

    assert response.status_code == 401
    assert response.json()["error"]["code"] == "authentication_required"


def test_public_responses_proxies_exact_body_and_preserves_idempotency_header(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def ensure_session(*_args: Any, **_kwargs: Any) -> None:
        return None

    monkeypatch.setattr(responses_route, "ensure_agent_runtime_session", ensure_session)
    payload = {"model": "qwen3.7-plus", "input": "hello", "store": False}

    with TestClient(_app(_user())) as client:
        response = client.post(
            "/v1/responses",
            json=payload,
            headers={"Idempotency-Key": "idem-1"},
        )

    assert response.status_code == 503
    assert response.json()["error"]["code"] == "agent_runtime_unavailable"


def test_public_responses_checks_model_permission_before_proxy(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    with TestClient(_app(_user())) as client:
        response = client.post(
            "/v1/responses",
            json={"model": "unknown-model", "input": "hello"},
        )

    assert response.status_code == 400
    assert response.json()["error"]["code"] == "model_not_found"


def test_public_responses_fails_closed_when_model_authorizer_is_unavailable(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    app = _app(_user())
    app.state.model_meta = None

    with TestClient(app) as client:
        response = client.post(
            "/v1/responses",
            json={"model": "qwen3.7-plus", "input": "hello"},
        )

    assert response.status_code == 503
    assert response.json()["error"] == {
        "message": "Model authorization is temporarily unavailable.",
        "type": "server_error",
        "param": "model",
        "code": "model_authorization_unavailable",
    }


def test_public_responses_fails_closed_when_model_authorizer_errors(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class BrokenModelMeta:
        async def get_access_level(self, _tenant_id: str, _model_id: str) -> str | None:
            raise RuntimeError("database connection contains private diagnostics")

    app = _app(_user())
    app.state.model_meta = BrokenModelMeta()

    with TestClient(app) as client:
        response = client.post(
            "/v1/responses",
            json={"model": "qwen3.7-plus", "input": "hello"},
        )

    assert response.status_code == 503
    assert response.json()["error"]["code"] == "model_authorization_unavailable"
    assert "private diagnostics" not in response.text


def test_public_responses_rejects_query_parameters_and_agent_forgery(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    with TestClient(_app(_user())) as client:
        query = client.post(
            "/v1/responses?foo=bar",
            json={"model": "qwen3.7-plus", "input": "hello"},
        )
        forged = client.post(
            "/v1/responses",
            json={
                "model": "qwen3.7-plus",
                "input": "hello",
                "runtime_envelope": {"forged": True},
            },
        )

    assert query.status_code == 400
    assert query.json()["error"]["code"] == "unsupported_query_parameters"
    assert forged.status_code == 422
    assert forged.json()["error"]["code"] == "agent_runtime_field_forbidden"


def test_gateway_application_registers_exact_v1_responses_path() -> None:
    from src.main import create_app

    paths = create_app().openapi()["paths"]
    assert "/v1/responses" in paths
    assert set(paths["/v1/responses"]) == {"post"}
    assert "/api/v1/responses" not in paths


def test_public_responses_uses_agent_runtime_for_nonstream_and_stream(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class RuntimeControl:
        def __init__(self) -> None:
            self.calls: list[dict[str, Any]] = []

        async def start_turn(self, **kwargs: Any) -> Any:
            self.calls.append(kwargs)
            return SimpleNamespace(run_id="11111111-1111-4111-8111-111111111111")

        async def stream_events(self, **_kwargs: Any):
            yield b'data: {"event_type":"text_delta","data":{"content":"hello"}}\n\n'
            yield b'data: {"event_type":"run_finished","data":{"status":"succeeded"}}\n\n'

    control = RuntimeControl()
    app = _app(_user())
    app.state.agent_runtime_control = control

    async def ensure_session(*_args: Any, **_kwargs: Any) -> None:
        return None

    monkeypatch.setattr(responses_route, "ensure_agent_runtime_session", ensure_session)
    with TestClient(app) as client:
        nonstream = client.post(
            "/v1/responses",
            json={
                "model": "qwen3.7-plus",
                "input": "hello",
                "instructions": "Use the signed runtime contract.",
            },
        )
        stream = client.post(
            "/v1/responses",
            json={"model": "qwen3.7-plus", "input": "hello", "stream": True},
        )

    assert nonstream.status_code == 200
    assert nonstream.json()["output_text"] == "hello"
    assert nonstream.json()["usage"]["total_tokens"] > 0
    assert stream.status_code == 200
    assert "response.output_text.delta" in stream.text
    assert "response.content_part.added" in stream.text
    assert "response.content_part.done" in stream.text
    assert "response.completed" in stream.text
    assert '"total_tokens":0' not in stream.text
    assert len(control.calls) == 2
    assert isinstance(control.calls[0]["resolved_agent_launch"], ResolvedAgentLaunchV1)
    assert control.calls[0]["resolved_agent_launch"].identity["entrypoint"] == "responses"
    assert control.calls[0]["developer_instructions"] == "Use the signed runtime contract."
    assert control.calls[0]["enable_dynamic_tools"] is False
    assert control.calls[0]["readonly_capabilities"] == {
        "responses_tool_choice": "none",
        "responses_parallel_tool_calls": True,
    }
    assert control.calls[0]["memory_mode"] == "off"


def test_public_responses_projects_function_tools_to_runtime_capability_selectors(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class RuntimeControl:
        def __init__(self) -> None:
            self.calls: list[dict[str, Any]] = []

        async def start_turn(self, **kwargs: Any) -> Any:
            self.calls.append(kwargs)
            return SimpleNamespace(run_id="11111111-1111-4111-8111-111111111111")

        async def stream_events(self, **_kwargs: Any):
            yield b'data: {"event_type":"run_finished","data":{"status":"succeeded"}}\n\n'

    control = RuntimeControl()
    app = _app(_user())
    app.state.agent_runtime_control = control

    async def ensure_session(*_args: Any, **_kwargs: Any) -> None:
        return None

    monkeypatch.setattr(responses_route, "ensure_agent_runtime_session", ensure_session)
    with TestClient(app) as client:
        response = client.post(
            "/v1/responses",
            json={
                "model": "qwen3.7-plus",
                "input": "look this up",
                "tools": [
                    {
                        "type": "function",
                        "name": "search_knowledge_base",
                        "description": "Search the configured knowledge base.",
                        "parameters": {"type": "object", "properties": {}},
                    }
                ],
                "tool_choice": {"type": "function", "name": "search_knowledge_base"},
                "parallel_tool_calls": False,
            },
        )

    assert response.status_code == 200
    assert control.calls[0]["enable_dynamic_tools"] is True
    assert isinstance(control.calls[0]["resolved_agent_launch"], ResolvedAgentLaunchV1)
    assert control.calls[0]["readonly_capabilities"] == {
        "responses_tool_names": ["search_knowledge_base"],
        "responses_tool_choice": {"type": "function", "name": "search_knowledge_base"},
        "responses_parallel_tool_calls": False,
    }


def test_public_responses_rejects_unknown_responses_tool_types() -> None:
    with TestClient(_app(_user())) as client:
        response = client.post(
            "/v1/responses",
            json={
                "model": "qwen3.7-plus",
                "input": "hello",
                "tools": [{"type": "web_search_preview"}],
            },
        )

    assert response.status_code == 409
    assert response.json()["error"]["code"] == "responses_tool_type_not_migrated"


def test_public_responses_projects_runtime_tool_events_to_responses_sse(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class RuntimeControl:
        async def start_turn(self, **_kwargs: Any) -> Any:
            return SimpleNamespace(run_id="11111111-1111-4111-8111-111111111111")

        async def stream_events(self, **_kwargs: Any):
            yield (
                b'data: {"event_type":"tool_call_start","data":'
                b'{"tool_call_id":"call-1","tool_name":"search_knowledge_base",'
                b'"arguments":"{\\"query\\":\\"transformer\\"}"}}\n\n'
            )
            yield (
                b'data: {"event_type":"tool_call_result","data":'
                b'{"tool_call_id":"call-1","status":"succeeded"}}\n\n'
            )
            yield (
                b'data: {"event_type":"tool_call_end","data":'
                b'{"tool_call_id":"call-1","status":"completed"}}\n\n'
            )
            yield b'data: {"event_type":"text_delta","data":{"content":"done"}}\n\n'
            yield b'data: {"event_type":"run_finished","data":{"status":"succeeded"}}\n\n'

    app = _app(_user())
    app.state.agent_runtime_control = RuntimeControl()

    async def ensure_session(*_args: Any, **_kwargs: Any) -> None:
        return None

    monkeypatch.setattr(responses_route, "ensure_agent_runtime_session", ensure_session)
    with TestClient(app) as client:
        response = client.post(
            "/v1/responses",
            json={"model": "qwen3.7-plus", "input": "use the tool", "stream": True},
        )

    assert response.status_code == 200
    assert "response.output_item.added" in response.text
    assert "response.function_call_arguments.delta" in response.text
    assert "response.function_call_arguments.done" in response.text
    assert "response.output_item.done" in response.text
    assert "response.completed" in response.text


def test_public_responses_rejects_unimplemented_store_semantics(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def ensure_session(*_args: Any, **_kwargs: Any) -> None:
        raise AssertionError("store=true must fail before Runtime session creation")

    monkeypatch.setattr(responses_route, "ensure_agent_runtime_session", ensure_session)
    with TestClient(_app(_user())) as client:
        response = client.post(
            "/v1/responses",
            json={"model": "qwen3.7-plus", "input": "hello", "store": True},
        )

    assert response.status_code == 409
    assert response.json()["error"]["code"] == "responses_fields_not_migrated"


def test_public_responses_nonstream_idempotency_replays_without_second_turn(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class RuntimeControl:
        def __init__(self) -> None:
            self.calls = 0

        async def start_turn(self, **_kwargs: Any) -> Any:
            self.calls += 1
            return SimpleNamespace(run_id="11111111-1111-4111-8111-111111111111")

        async def stream_events(self, **_kwargs: Any):
            yield b'data: {"event_type":"text_delta","data":{"content":"hello"}}\n\n'
            yield b'data: {"event_type":"run_finished","data":{"status":"succeeded"}}\n\n'

    control = RuntimeControl()
    app = _app(_user())
    app.state.agent_runtime_control = control

    async def ensure_session(*_args: Any, **_kwargs: Any) -> None:
        return None

    monkeypatch.setattr(responses_route, "ensure_agent_runtime_session", ensure_session)
    headers = {"Idempotency-Key": "idem-1"}
    with TestClient(app) as client:
        first = client.post(
            "/v1/responses",
            json={"model": "qwen3.7-plus", "input": "hello"},
            headers=headers,
        )
        replay = client.post(
            "/v1/responses",
            json={"model": "qwen3.7-plus", "input": "hello"},
            headers=headers,
        )

    assert first.status_code == 200
    assert replay.status_code == 200
    assert replay.headers["x-idempotency-replayed"] == "true"
    assert replay.json() == first.json()
    assert control.calls == 1


def test_public_responses_stream_without_terminal_fails_in_band(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class RuntimeControl:
        async def start_turn(self, **_kwargs: Any) -> Any:
            return SimpleNamespace(run_id="11111111-1111-4111-8111-111111111111")

        async def stream_events(self, **_kwargs: Any):
            yield b'data: {"event_type":"text_delta","data":{"content":"partial"}}\n\n'

    app = _app(_user())
    app.state.agent_runtime_control = RuntimeControl()

    async def ensure_session(*_args: Any, **_kwargs: Any) -> None:
        return None

    monkeypatch.setattr(responses_route, "ensure_agent_runtime_session", ensure_session)
    with TestClient(app) as client:
        response = client.post(
            "/v1/responses",
            json={"model": "qwen3.7-plus", "input": "hello", "stream": True},
        )

    assert response.status_code == 200
    assert "response.failed" in response.text
    assert "response.completed" not in response.text
    assert "agent_runtime_stream_incomplete" in response.text


@pytest.mark.parametrize("mode", ["exception", "eof"])
def test_public_responses_observation_loss_replays_accepted_run(
    monkeypatch: pytest.MonkeyPatch,
    mode: str,
) -> None:
    calls: list[str] = []

    class RuntimeControl:
        async def start_turn(self, **_kwargs: Any) -> Any:
            calls.append(_kwargs["session_id"])
            return SimpleNamespace(run_id="11111111-1111-4111-8111-111111111111")

        async def stream_events(self, **_kwargs: Any):
            if mode == "exception":
                raise RuntimeError("transport failed")
            return
            yield  # pragma: no cover

    app = _app(_user())
    app.state.agent_runtime_control = RuntimeControl()

    async def ensure_session(*_args: Any, **_kwargs: Any) -> None:
        return None

    monkeypatch.setattr(responses_route, "ensure_agent_runtime_session", ensure_session)
    with TestClient(app) as client:
        first = client.post(
            "/v1/responses",
            json={"model": "qwen3.7-plus", "input": "hello"},
            headers={"Idempotency-Key": "retryable"},
        )
        second = client.post(
            "/v1/responses",
            json={"model": "qwen3.7-plus", "input": "hello"},
            headers={"Idempotency-Key": "retryable"},
        )

    assert first.status_code == (503 if mode == "exception" else 502)
    assert second.status_code == first.status_code
    assert len(calls) == 1
    assert second.headers["x-idempotency-replayed"] == "true"
    assert second.headers["x-run-id"] == "11111111-1111-4111-8111-111111111111"
    assert second.headers["x-session-id"] == calls[0]
    assert second.json() == first.json()


def json_bytes(value: bytes) -> dict[str, Any]:
    import json

    return json.loads(value)
