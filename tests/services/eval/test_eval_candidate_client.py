from __future__ import annotations

import json
from typing import Any

import pytest

from src.services.eval import eval_candidate_client as candidate_module
from src.services.eval.eval_candidate_client import EvalCandidateClient


def _v2_event(event_type: str, data: Any, sequence: int) -> str:
    envelope = {
        "schema_version": "agent-event/v2",
        "thread_id": "thread-1",
        "sequence": sequence,
        "event": {
            "id": f"event-{sequence}",
            "key": f"event-{sequence}",
            "type": event_type,
            "turn_id": "turn-1",
            "payload": {"event_type": event_type, "data": data},
        },
    }
    return f"data: {json.dumps(envelope)}"


class _FakeResponse:
    def __init__(self, payload: dict[str, Any] | None = None, lines: list[str] | None = None) -> None:
        self.status_code = 200
        self._payload = payload or {}
        self.lines = lines or []

    def json(self) -> dict[str, Any]:
        return self._payload

    async def aiter_lines(self):
        for line in self.lines:
            yield line
            yield ""


class _FakeStream:
    def __init__(self, response: _FakeResponse) -> None:
        self.response = response

    async def __aenter__(self) -> _FakeResponse:
        return self.response

    async def __aexit__(self, *_args: Any) -> None:
        return None


class _FakeClient:
    def __init__(self, responses: list[_FakeResponse], captured: list[dict[str, Any]]) -> None:
        self.responses = responses
        self.captured = captured

    async def __aenter__(self):
        return self

    async def __aexit__(self, *_args: Any) -> None:
        return None

    async def get(self, path: str, **kwargs: Any) -> _FakeResponse:
        self.captured.append({"method": "GET", "path": path, **kwargs})
        if path.startswith("/api/v2/agent/threads/"):
            return self.responses.pop(0)
        return _FakeResponse({"user_id": "eval-user", "tenant_id": "tenant-a"})

    async def post(self, path: str, **kwargs: Any) -> _FakeResponse:
        self.captured.append({"method": "POST", "path": path, **kwargs})
        return self.responses.pop(0)

    def stream(self, method: str, path: str, **kwargs: Any) -> _FakeStream:
        self.captured.append({"method": method, "path": path, **kwargs})
        return _FakeStream(self.responses.pop(0))


def _install_fake_client(
    monkeypatch: pytest.MonkeyPatch,
    captured: list[dict[str, Any]],
    responses: list[_FakeResponse],
) -> None:
    monkeypatch.setattr(
        candidate_module.httpx,
        "AsyncClient",
        lambda **_kwargs: _FakeClient(responses, captured),
    )


@pytest.mark.asyncio
@pytest.mark.parametrize("auth_mode", ["jwt", "api_key"])
async def test_candidate_client_uses_v2_thread_turn_events_and_runtime_owner(
    monkeypatch: pytest.MonkeyPatch,
    auth_mode: str,
) -> None:
    captured: list[dict[str, Any]] = []
    _install_fake_client(
        monkeypatch,
        captured,
        [
            _FakeResponse(
                {"thread": {"id": "thread-1", "thread_id": "thread-1", "runtime": {"owner": "agent_runtime"}}}
            ),
            _FakeResponse(
                {"turn": {"id": "turn-1", "events_url": "/api/v2/agent/threads/thread-1/events?turn_id=turn-1"}}
            ),
            _FakeResponse(
                lines=[
                    _v2_event("run_started", {"run_id": "turn-1"}, 1),
                    _v2_event("context_budget", {"runtime_revision": "runtime-a"}, 2),
                    _v2_event("subagent_started", {"agent_id": "researcher"}, 3),
                    _v2_event("text_delta", {"content": "answer"}, 4),
                    _v2_event("subagent_finished", {"agent_id": "researcher", "status": "succeeded"}, 5),
                    _v2_event("run_finished", {"status": "succeeded"}, 6),
                ]
            ),
        ],
    )
    monkeypatch.setenv("AGENT_EVAL_AUTH_TOKEN", "test-token")

    started: list[str] = []

    async def remember(trace_id: str) -> None:
        started.append(trace_id)

    candidate = EvalCandidateClient()
    candidate.token = "test-token" if auth_mode == "jwt" else ""
    candidate.api_key = "test-key" if auth_mode == "api_key" else ""
    result = await candidate.run(
        tenant_id="tenant-a",
        run_case_id="run-case-1",
        message="hello",
        config={"model_id": "qwen3.7-plus", "temperature": 0.2},
        on_run_started=remember,
    )

    assert [item["path"] for item in captured] == [
        "/api/v1/auth/me",
        "/api/v2/agent/threads",
        "/api/v2/agent/threads/thread-1/turns",
        "/api/v2/agent/threads/thread-1/events?turn_id=turn-1",
    ]
    auth_header, auth_value = (
        ("Authorization", "Bearer test-token") if auth_mode == "jwt"
        else ("X-API-Key", "test-key")
    )
    assert all(item["headers"][auth_header] == auth_value for item in captured)
    assert captured[1]["json"]["model_id"] == "qwen3.7-plus"
    assert captured[1]["json"]["expected_tenant_id"] == "tenant-a"
    assert captured[2]["json"]["model_id"] == "qwen3.7-plus"
    assert captured[2]["json"]["temperature"] == 0.2
    assert started == ["turn-1"]
    assert result.trace_id == "turn-1"
    assert result.output == "answer"
    assert result.fingerprint["runtime_revision"] == "runtime-a"


@pytest.mark.asyncio
async def test_candidate_client_does_not_inherit_host_proxy_settings(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: list[dict[str, Any]] = []
    client_options: dict[str, Any] = {}
    responses = [
        _FakeResponse(
            {
                "thread": {
                    "thread_id": "thread-1",
                    "runtime": {"owner": "agent_runtime"},
                }
            }
        ),
        _FakeResponse({"turn": {"id": "turn-1", "events_url": "/api/v2/agent/threads/thread-1/events"}}),
        _FakeResponse(
            lines=[
                _v2_event("run_started", {"run_id": "turn-1"}, 1),
                _v2_event("text_delta", {"content": "answer"}, 2),
                _v2_event("run_finished", {"status": "succeeded"}, 3),
            ]
        ),
    ]

    def factory(**kwargs: Any) -> _FakeClient:
        client_options.update(kwargs)
        return _FakeClient(responses, captured)

    monkeypatch.setattr(candidate_module.httpx, "AsyncClient", factory)
    monkeypatch.setenv("AGENT_EVAL_AUTH_TOKEN", "test-token")

    await EvalCandidateClient().run(
        tenant_id="tenant-a",
        run_case_id="run-case-proxy",
        message="hello",
        config={},
    )

    assert client_options["trust_env"] is False


@pytest.mark.asyncio
async def test_candidate_client_rejects_non_runtime_owner(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: list[dict[str, Any]] = []
    _install_fake_client(
        monkeypatch,
        captured,
        [_FakeResponse({"thread": {"thread_id": "thread-1", "runtime": {"owner": "python"}}})],
    )
    monkeypatch.setenv("AGENT_EVAL_AUTH_TOKEN", "test-token")

    with pytest.raises(RuntimeError, match="not owned by agent_runtime"):
        await EvalCandidateClient().run(
            tenant_id="tenant-a", run_case_id="run-case-1", message="hello", config={}
        )


@pytest.mark.asyncio
async def test_agent_version_candidate_sends_only_ids_and_message_with_creator_delegation(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from src.services.eval import eval_llm_client

    monkeypatch.setattr(eval_llm_client, "_build_internal_jwt", lambda **_kwargs: "delegated-jwt")
    agent_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    version_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
    config = {
        "candidate_type": "agent_version", "agent_id": agent_id,
        "agent_version_id": version_id, "agent_spec_hash": "a" * 64,
        "agent_runtime_snapshot_hash": "sha256:" + "b" * 64,
        "model_id": "frozen-model", "provider_id": "frozen-provider",
        "knowledge_dataset_ids": ["server-kb"],
    }
    target = {
        "agent_id": agent_id, "agent_version_id": version_id,
        "agent_spec_hash": config["agent_spec_hash"],
        "runtime_snapshot_hash": config["agent_runtime_snapshot_hash"],
    }
    captured: list[dict[str, Any]] = []
    _install_fake_client(monkeypatch, captured, [
        _FakeResponse({"thread": {
            "thread_id": "thread-1", "runtime": {"owner": "agent_runtime"},
            "agent_version_target": target,
        }}),
        _FakeResponse({"turn": {
            "id": "turn-1", "events_url": "/api/v2/agent/threads/thread-1/events",
        }}),
        _FakeResponse(lines=[
            _v2_event("run_started", {"run_id": "turn-1"}, 1),
            _v2_event("text_delta", {"content": "answer"}, 2),
            _v2_event("run_finished", {"status": "succeeded"}, 3),
        ]),
    ])
    candidate = EvalCandidateClient(allow_service_identity=True)
    candidate.token = "admin-token"
    result = await candidate.run(
        tenant_id="tenant-a", run_case_id="run-case-1", message="hello", config=config,
        delegation={
            "actor": "eval-worker", "subject": "eval-user", "tenant_id": "tenant-a",
            "run_id": "run", "job_id": "job",
        },
    )

    assert all(item["headers"]["Authorization"] == "Bearer delegated-jwt" for item in captured)
    assert captured[1]["json"] == {
        "session_id": "run-case-1", "expected_tenant_id": "tenant-a",
        "agent_id": agent_id, "agent_version_id": version_id,
    }
    assert captured[2]["json"] == {"message": "hello"}
    assert result.fingerprint["agent_version_id"] == version_id


@pytest.mark.asyncio
async def test_agent_version_candidate_rejects_mismatched_pin_before_turn(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from src.services.eval import eval_llm_client

    monkeypatch.setattr(eval_llm_client, "_build_internal_jwt", lambda **_kwargs: "delegated-jwt")
    captured: list[dict[str, Any]] = []
    _install_fake_client(monkeypatch, captured, [
        _FakeResponse({"thread": {
            "thread_id": "thread-1", "runtime": {"owner": "agent_runtime"},
            "agent_version_target": {
                "agent_id": "agent-a", "agent_version_id": "version-b",
                "agent_spec_hash": "wrong", "runtime_snapshot_hash": "sha256:wrong",
            },
        }}),
    ])
    candidate = EvalCandidateClient(allow_service_identity=True)
    candidate.token = "admin-token"
    with pytest.raises(RuntimeError, match="AGENT_EVAL_VERSION_PIN_MISMATCH"):
        await candidate.run(
            tenant_id="tenant-a", run_case_id="run-case-1", message="hello",
            config={
                "candidate_type": "agent_version", "agent_id": "agent-a",
                "agent_version_id": "version-a", "agent_spec_hash": "a" * 64,
                "agent_runtime_snapshot_hash": "sha256:" + "b" * 64,
            },
            delegation={
                "actor": "eval-worker", "subject": "eval-user", "tenant_id": "tenant-a",
                "run_id": "run", "job_id": "job",
            },
        )
    assert [item["path"] for item in captured] == [
        "/api/v1/auth/me", "/api/v2/agent/threads",
    ]


@pytest.mark.asyncio
async def test_agent_version_candidate_cannot_use_worker_admin_token() -> None:
    candidate = EvalCandidateClient(allow_service_identity=True)
    candidate.token = "worker-admin-token"
    with pytest.raises(RuntimeError, match="AGENT_EVAL_DELEGATION_REQUIRED"):
        await candidate.run(
            tenant_id="tenant-a", run_case_id="run-case-1", message="hello",
            config={
                "candidate_type": "agent_version", "agent_id": "agent-a",
                "agent_version_id": "version-a", "agent_spec_hash": "a" * 64,
                "agent_runtime_snapshot_hash": "sha256:" + "b" * 64,
            },
        )


@pytest.mark.asyncio
async def test_agent_version_resume_rechecks_exact_thread_pin(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    from src.services.eval import eval_llm_client

    monkeypatch.setattr(eval_llm_client, "_build_internal_jwt", lambda **_kwargs: "delegated-jwt")
    target = {
        "agent_id": "agent-a", "agent_version_id": "version-a",
        "agent_spec_hash": "a" * 64, "runtime_snapshot_hash": "sha256:" + "b" * 64,
    }
    captured: list[dict[str, Any]] = []
    _install_fake_client(monkeypatch, captured, [
        _FakeResponse({"thread": {
            "thread_id": "thread-1", "runtime": {"owner": "agent_runtime"},
            "agent_version_target": target,
        }}),
        _FakeResponse(lines=[
            _v2_event("run_started", {"run_id": "turn-1"}, 1),
            _v2_event("text_delta", {"content": "answer"}, 2),
            _v2_event("run_finished", {"status": "succeeded"}, 3),
        ]),
    ])
    candidate = EvalCandidateClient(allow_service_identity=True)
    result = await candidate.run(
        tenant_id="tenant-a", run_case_id="run-case-1", message="hello",
        config={
            "candidate_type": "agent_version", "agent_id": target["agent_id"],
            "agent_version_id": target["agent_version_id"],
            "agent_spec_hash": target["agent_spec_hash"],
            "agent_runtime_snapshot_hash": target["runtime_snapshot_hash"],
        },
        delegation={
            "actor": "eval-worker", "subject": "eval-user", "tenant_id": "tenant-a",
            "run_id": "run", "job_id": "job",
        },
        resume_handle={
            "tenant_id": "tenant-a", "user_id": "eval-user", "run_case_id": "run-case-1",
            "thread_id": "thread-1", "turn_id": "turn-1",
            "events_url": "/api/v2/agent/threads/thread-1/events",
        },
    )

    assert result.fingerprint["agent_version_id"] == "version-a"
    assert not any(item["method"] == "POST" for item in captured)
    assert captured[1]["path"] == "/api/v2/agent/threads/thread-1"


@pytest.mark.asyncio
async def test_candidate_client_preserves_terminal_error_after_v2_stream(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    captured: list[dict[str, Any]] = []
    _install_fake_client(
        monkeypatch,
        captured,
        [
            _FakeResponse({"thread": {"thread_id": "thread-1", "runtime": {"owner": "agent_runtime"}}}),
            _FakeResponse({"turn": {"id": "turn-1", "events_url": "/api/v2/agent/threads/thread-1/events"}}),
            _FakeResponse(
                lines=[
                    _v2_event("run_started", {"run_id": "turn-1"}, 1),
                    _v2_event("run_error", {"message": "tool failed", "status": "failed"}, 2),
                ]
            ),
        ],
    )
    monkeypatch.setenv("AGENT_EVAL_AUTH_TOKEN", "test-token")

    result = await EvalCandidateClient().run(
        tenant_id="tenant-a",
        run_case_id="run-case-2",
        message="hello",
        config={"model_id": "current"},
    )

    assert result.trace_id == "turn-1"
    assert result.error == "tool failed"
    assert "model_id" not in captured[1]["json"]
    assert "model_id" not in captured[2]["json"]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "stream_events, expected",
    [
        ([(_v2_event("run_started", {"run_id": "turn-1"}, 1))], "exactly one terminal"),
        (
            [
                _v2_event("run_started", {"run_id": "turn-1"}, 1),
                _v2_event("run_finished", {"status": "succeeded"}, 2),
                _v2_event("run_finished", {"status": "succeeded"}, 3),
            ],
            "exactly one terminal",
        ),
    ],
)
async def test_candidate_client_rejects_missing_or_duplicate_terminal(
    monkeypatch: pytest.MonkeyPatch,
    stream_events: list[str],
    expected: str,
) -> None:
    captured: list[dict[str, Any]] = []
    _install_fake_client(
        monkeypatch,
        captured,
        [
            _FakeResponse({"thread": {"thread_id": "thread-1", "runtime": {"owner": "agent_runtime"}}}),
            _FakeResponse({"turn": {"id": "turn-1", "events_url": "/api/v2/agent/threads/thread-1/events"}}),
            _FakeResponse(lines=stream_events),
        ],
    )
    monkeypatch.setenv("AGENT_EVAL_AUTH_TOKEN", "test-token")

    with pytest.raises(RuntimeError, match=expected):
        await EvalCandidateClient().run(
            tenant_id="tenant-a", run_case_id="run-case-terminal", message="hello", config={}
        )


@pytest.mark.asyncio
async def test_candidate_client_requires_explicit_live_auth(monkeypatch: pytest.MonkeyPatch) -> None:
    for name in (
        "AGENT_EVAL_AUTH_TOKEN",
        "GATEWAY_TOKEN",
        "GATEWAY_ADMIN_JWT",
        "AGENT_EVAL_API_KEY",
        "GATEWAY_API_KEY",
    ):
        monkeypatch.delenv(name, raising=False)

    with pytest.raises(RuntimeError, match="V2 live eval"):
        await EvalCandidateClient().run(
            tenant_id="tenant-a", run_case_id="run-case-3", message="hello", config={}
        )


def test_candidate_client_uses_compose_gateway_port_by_default(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("AGENT_EVAL_GATEWAY_URL", raising=False)
    monkeypatch.delenv("GATEWAY_URL", raising=False)
    assert EvalCandidateClient().base_url == "http://gateway:8080"


async def test_candidate_trace_uses_verified_subject_and_retains_kernel_fingerprint(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: list[dict[str, Any]] = []
    _install_fake_client(monkeypatch, captured, [
        _FakeResponse({"thread": {"thread_id": "thread-1", "runtime": {"owner": "agent_runtime"}}}),
        _FakeResponse({"turn": {"id": "turn-1", "events_url": "/api/v2/agent/threads/thread-1/events"}}),
        _FakeResponse(lines=[
            _v2_event("run_started", {"run_id": "turn-1", "kernel_revision": "target-sha"}, 1),
            _v2_event("context_budget", {"tool_schema_hash": "tools"}, 2),
            _v2_event("text_delta", {"content": "answer"}, 3),
            _v2_event("run_finished", {"status": "succeeded"}, 4),
        ]),
    ])
    monkeypatch.setenv("AGENT_EVAL_AUTH_TOKEN", "test-token")
    monkeypatch.setattr(candidate_module, "build_assistant_runtime_trace", lambda **kwargs: kwargs)
    result = await EvalCandidateClient().run(tenant_id="tenant-a", run_case_id="case", message="hello", config={})
    assert result.trace_payload["user_id"] == "eval-user"
    assert result.fingerprint == {"runtime_revision": "target-sha", "tool_schema_hash": "tools"}
