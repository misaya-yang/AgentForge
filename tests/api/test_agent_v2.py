from __future__ import annotations

from types import SimpleNamespace
from uuid import uuid4

import pytest
from ai_gateway_contracts.agent_launch import ResolvedAgentLaunchV1
from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_core.exceptions import PermissionDeniedError
from ai_gateway_core.persistence.repositories.agent_repository import AgentNotFoundError
from starlette.requests import Request

from src.api.v2 import agent as agent_module
from src.api.v2.agent import (
    ApprovalDecisionRequest,
    ThreadCreateRequest,
    TurnCreateRequest,
    _get_thread,
    _reject_unmigrated_turn_capabilities,
    create_thread,
    create_turn,
    decide_thread_approval,
    get_thread_approval,
    router,
    thread_events,
)
from src.core.auth.user_resolver import UserContext
from src.services.agent_runtime.control_plane import AgentRuntimeControlError


def _fixed_snapshot(agent_id: str, version_id: str) -> dict:
    return {
        "schema_version": "agent-runtime/v1",
        "tenant_id": "tenant-a",
        "agent_id": agent_id,
        "agent_version_id": version_id,
        "publication": {"id": None, "channel": "preview", "auth_mode": "private"},
        "model": {"id": "agent-model", "provider": "provider-a", "parameters": {}},
        "agent_spec": {"developerInstructions": "Version one instructions: R3_OLD"},
        "knowledge": {"datasets": [], "retrieval": {"mode": "off"}},
        "capabilities": [],
        "memory": {"mode": "session"},
        "fingerprints": {"spec": "sha256:" + "a" * 64},
    }


def test_v2_routes_are_additive_and_cursor_based() -> None:
    paths = {route.path for route in router.routes}
    assert "/agent/threads" in paths
    assert "/agent/threads/{thread_id}/turns" in paths
    assert "/agent/threads/{thread_id}/turns/{turn_id}:interrupt" in paths
    assert "/agent/threads/{thread_id}/events" in paths
    assert "/agent/threads/{thread_id}/approvals/{approval_id}" in paths
    assert "/agent/threads/{thread_id}/approvals/{approval_id}/decision" in paths


class _Database:
    def __init__(self) -> None:
        self.thread = None

    async def fetch(self, query: str, *_args):
        assert "FROM assistant_runtime_snapshots" in query or "FROM assistant_runtime_items" in query
        return []

    async def fetchrow(self, query: str, *args):
        if "SELECT history FROM assistant.sessions" in query:
            return None
        if "FROM user_memory" in query:
            assert args == ("tenant-a", "user-a", "__assistant_memory_control__")
            return None
        if "ensure_assistant_runtime_thread" in query:
            self.thread = {
                "runtime_thread_id": args[0], "tenant_id": args[1],
                "user_id": args[2], "session_id": args[3],
                "kernel_owner": "agent", "source_kind": "native",
                "import_status": "not_required", "last_sequence": 0,
            }
            return {"ok": True}
        if "import_assistant_legacy_session" in query:
            self.thread["source_kind"] = "legacy_import"
            self.thread["import_status"] = "ready"
            return {"import_status": "ready"}
        if "FROM assistant_runtime_snapshots" in query:
            return {
                "snapshot": {
                    "reasoning": {
                        "requested_option": "auto",
                        "effective_option": "minimal",
                        "adapter_id": "reasoning/test-v1",
                        "fallback_reason": None,
                    }
                },
                "capability_revision": 7,
                "kernel_revision": "kernel-1",
            }
        if "FROM assistant_runtime_threads" in query:
            expected_tenant = args[0] if "session_id = $3" in query else args[1]
            expected_user = args[1] if "session_id = $3" in query else args[2]
            if self.thread and (
                str(self.thread["tenant_id"]) != str(expected_tenant)
                or str(self.thread["user_id"]) != str(expected_user)
            ):
                return None
            return self.thread
        raise AssertionError(query)


def _request(state: SimpleNamespace) -> Request:
    if not hasattr(state, "session_manager"):
        state.session_manager = SimpleNamespace(
            get=lambda session_id: _unbound_session(session_id)
        )
    app = SimpleNamespace(state=state)
    return Request({"type": "http", "method": "POST", "path": "/", "headers": [], "app": app})


async def _unbound_session(session_id: str) -> SimpleNamespace:
    return SimpleNamespace(session_id=session_id, user_id="user-a", tenant_id="tenant-a")


@pytest.mark.asyncio
@pytest.mark.parametrize("provider_id", [None, "second-provider"])
async def test_create_thread_provisions_kernel_then_imports_legacy_history(provider_id) -> None:
    db = _Database()
    runtime_thread_id = str(uuid4())
    calls: list[str] = []

    class _Sessions:
        async def get(self, session_id: str):
            return SimpleNamespace(session_id=session_id, user_id="user-a", tenant_id="tenant-a")

        async def history(self, session_id: str, limit: int = 1):
            del session_id, limit
            return [SimpleNamespace(role="user", content="legacy")]

    class _Assignments:
        async def bind(self, **kwargs):
            assert kwargs["runtime_owner"] == "agent_runtime"
            return SimpleNamespace(runtime_owner="agent_runtime", kernel_revision="kernel-1")

        async def resolve(self, **kwargs):
            assert kwargs["tenant_id"] == "tenant-a"
            return SimpleNamespace(runtime_owner="agent_runtime", kernel_revision="kernel-1")

    class _Control:
        async def ensure_thread(self, **_kwargs):
            assert _kwargs.get("provider_id") == provider_id
            assert _kwargs["model_id"] == "qwen3.7-plus"
            calls.append("ensure")
            db.thread = {
                "runtime_thread_id": runtime_thread_id, "tenant_id": "tenant-a",
                "user_id": "user-a", "session_id": "session-a",
                "kernel_owner": "agent", "source_kind": "native",
                "import_status": "pending", "last_sequence": 0,
            }
            return {"runtime_thread_id": runtime_thread_id, "last_sequence": 0}

    state = SimpleNamespace(
        database=db,
        session_manager=_Sessions(),
        assistant_runtime_assignments=_Assignments(),
        assistant_runtime_default_owner="agent_runtime",
        assistant_runtime_kernel_revision="kernel-1",
        agent_runtime_control=_Control(),
        settings=SimpleNamespace(default_model="qwen3.7-plus"),
    )
    response = await create_thread(
        ThreadCreateRequest(session_id="session-a", model_id="qwen3.7-plus", provider_id=provider_id),
        _request(state),
        UserContext(
            user_id="user-a", tenant_id="tenant-a", tier="normal",
            is_authenticated=True, roles=["user"], ip="127.0.0.1",
        ),
    )
    assert calls == ["ensure"]
    assert response["thread"]["id"] == runtime_thread_id
    assert response["thread"]["import_status"] == "ready"


@pytest.mark.asyncio
async def test_fixed_version_thread_and_turn_ignore_later_draft_and_recheck_acl(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    agent_id, version_id = str(uuid4()), str(uuid4())
    snapshot = _fixed_snapshot(agent_id, version_id)
    db = _Database()
    session_id = "version-session"
    runtime_thread_id = str(uuid4())
    actor = UserContext(
        user_id="user-a", tenant_id="tenant-a", tier="normal",
        is_authenticated=True, roles=["user"],
    )
    calls: dict[str, list] = {"repository": [], "launch": [], "start": []}

    class _Repository:
        revoked = False
        current_draft_revision = 2

        async def resolve_version_runtime(self, **kwargs):
            calls["repository"].append(kwargs)
            if self.revoked:
                raise AgentNotFoundError("AGENT_NOT_FOUND")
            assert kwargs["agent_id"] == agent_id
            assert kwargs["agent_version_id"] == version_id
            assert kwargs["user_id"] == actor.user_id
            assert kwargs["tenant_id"] == actor.tenant_id
            return {
                "agent": {"agent_id": agent_id, "tenant_id": "tenant-a"},
                "version": {"agent_version_id": version_id, "source_draft_revision": 1},
                "spec": {"model": {"model_id": "agent-model"}},
                "capabilities": [], "knowledge": [], "publication": None,
            }

    class _Sessions:
        item = None

        async def get(self, requested):
            return self.item if requested == session_id else None

        async def bind_agent_runtime(self, **kwargs):
            if self.item is not None:
                raise PermissionDeniedError("already bound")
            self.item = SimpleNamespace(**kwargs)
            return self.item

        async def history(self, _session_id, limit=1):
            del limit
            return []

        async def delete(self, _session_id):
            self.item = None

    class _Assignments:
        async def bind(self, **_kwargs):
            return SimpleNamespace(runtime_owner="agent_runtime")

        async def resolve(self, **_kwargs):
            return SimpleNamespace(runtime_owner="agent_runtime")

    class _Control:
        async def ensure_thread(self, **kwargs):
            assert kwargs["model_id"] == "agent-model"
            assert kwargs["provider_id"] == "provider-a"
            assert kwargs["capability_allowlist"] == []
            assert kwargs["developer_instructions"] == "Version one instructions: R3_OLD"
            return {"runtime_thread_id": runtime_thread_id}

        async def start_turn(self, **kwargs):
            calls["start"].append(kwargs)
            assert kwargs["model_id"] == "agent-model"
            assert kwargs["resolved_agent_launch"] is launch
            return SimpleNamespace(
                run_id=str(uuid4()), requested_reasoning_option="auto",
                effective_reasoning_option="minimal", after_sequence=0,
            )

        async def recover_turn(self, **_kwargs):
            raise AssertionError("revoked Version must not recover")

    async def build_snapshot(_request, _resolution, _user, *, channel):
        assert channel == "preview"
        return snapshot

    async def resolve_launch(**kwargs):
        calls["launch"].append(kwargs)
        return launch

    launch = object()
    repository = _Repository()
    sessions = _Sessions()
    state = SimpleNamespace(
        database=db, session_manager=sessions,
        assistant_runtime_assignments=_Assignments(),
        agent_runtime_control=_Control(), agent_repository=repository,
        settings=SimpleNamespace(default_model="builtin-default"),
    )
    monkeypatch.setattr(agent_module, "_build_snapshot", build_snapshot)
    monkeypatch.setattr(agent_module, "resolve_agent_launch", resolve_launch)

    created = await create_thread(
        ThreadCreateRequest(
            session_id=session_id, agent_id=agent_id, agent_version_id=version_id
        ),
        _request(state), actor,
    )
    target = created["thread"]["agent_version_target"]
    assert target == {
        "agent_id": agent_id, "agent_version_id": version_id,
        "agent_spec_hash": "a" * 64,
        "runtime_snapshot_hash": runtime_sha256(snapshot),
    }
    assert sessions.item.channel == "preview"
    assert sessions.item.agent_version_id == version_id
    assert repository.current_draft_revision == 2
    fetched = await agent_module.get_thread(runtime_thread_id, _request(state), actor)
    assert fetched["thread"]["agent_version_target"] == target

    turn = await create_turn(
        runtime_thread_id, TurnCreateRequest(message="use version one"), _request(state), actor
    )
    assert turn["turn"]["status"] == "in_progress"
    assert calls["launch"][0]["entrypoint"] == "studio_preview"
    assert calls["launch"][0]["legacy_snapshot"] is snapshot
    assert calls["start"][0]["style_guidance"] is None
    with pytest.raises(Exception) as override:
        await create_turn(
            runtime_thread_id,
            TurnCreateRequest(message="override", model_id="builtin-default"),
            _request(state), actor,
        )
    assert getattr(override.value, "status_code", None) == 422
    assert len(calls["start"]) == 1

    sessions.item.runtime_fingerprint = "sha256:wrong-pin"
    with pytest.raises(Exception) as stale_pin:
        await create_turn(
            runtime_thread_id, TurnCreateRequest(message="wrong pin"), _request(state), actor
        )
    assert getattr(stale_pin.value, "status_code", None) == 409
    sessions.item.runtime_fingerprint = runtime_sha256(snapshot)

    repository.revoked = True
    readable = await agent_module.get_thread(runtime_thread_id, _request(state), actor)
    assert readable["thread"]["agent_version_target"] == target
    with pytest.raises(Exception) as revoked:
        await create_turn(
            runtime_thread_id, TurnCreateRequest(message="after revoke"), _request(state), actor
        )
    assert getattr(revoked.value, "status_code", None) == 404
    with pytest.raises(Exception) as recovery:
        await agent_module.recover_turn(runtime_thread_id, "run-a", _request(state), actor)
    assert getattr(recovery.value, "status_code", None) == 404
    assert len(calls["start"]) == 1


@pytest.mark.asyncio
async def test_fixed_version_rejects_an_existing_unpinned_session(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    agent_id, version_id = str(uuid4()), str(uuid4())
    snapshot = _fixed_snapshot(agent_id, version_id)

    class _Sessions:
        async def get(self, _session_id):
            return SimpleNamespace(user_id="user-a", tenant_id="tenant-a")

        async def bind_agent_runtime(self, **_kwargs):
            raise PermissionDeniedError("existing builtin session")

    async def version_snapshot(*_args, **_kwargs):
        return snapshot

    monkeypatch.setattr(agent_module, "_version_snapshot", version_snapshot)
    state = SimpleNamespace(session_manager=_Sessions())
    actor = UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)
    with pytest.raises(Exception) as conflict:
        await create_thread(
            ThreadCreateRequest(
                session_id="builtin-session", agent_id=agent_id, agent_version_id=version_id
            ),
            _request(state), actor,
        )
    assert getattr(conflict.value, "status_code", None) == 409


def test_fixed_version_request_requires_paired_target_without_model_override() -> None:
    with pytest.raises(ValueError):
        ThreadCreateRequest(agent_id=uuid4())
    with pytest.raises(ValueError):
        ThreadCreateRequest(agent_id=uuid4(), agent_version_id=uuid4(), model_id="override")


@pytest.mark.asyncio
async def test_fixed_version_thread_retry_keeps_one_pin_after_runtime_failure(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    agent_id, version_id = str(uuid4()), str(uuid4())
    snapshot = _fixed_snapshot(agent_id, version_id)
    thread_id = str(uuid4())
    actor = UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)

    class _Sessions:
        item = None
        bind_count = 0

        async def get(self, _session_id):
            return self.item

        async def bind_agent_runtime(self, **kwargs):
            self.bind_count += 1
            if self.item is None:
                self.item = SimpleNamespace(**kwargs)
            elif any(getattr(self.item, key) != value for key, value in kwargs.items()):
                raise PermissionDeniedError("pin changed")
            return self.item

        async def history(self, _session_id, limit=1):
            del limit
            return []

    class _Assignments:
        async def bind(self, **_kwargs):
            return SimpleNamespace(runtime_owner="agent_runtime")

        async def resolve(self, **_kwargs):
            return SimpleNamespace(runtime_owner="agent_runtime")

    class _Control:
        attempts = 0

        async def ensure_thread(self, **kwargs):
            assert kwargs["capability_allowlist"] == []
            self.attempts += 1
            if self.attempts == 1:
                raise AgentRuntimeControlError("RUNTIME_TEMPORARILY_UNAVAILABLE", status_code=503)
            return {"runtime_thread_id": thread_id}

        async def start_turn(self, **_kwargs):
            raise AssertionError("thread creation must not dispatch a turn")

    class _Store:
        async def get_for_session(self, **_kwargs):
            return None

        async def ensure_native(self, **_kwargs):
            return SimpleNamespace(
                runtime_thread_id=thread_id, session_id="same-session",
                kernel_owner="agent", source_kind="native",
                import_status="not_required", last_sequence=0,
            )

    async def version_snapshot(*_args, **_kwargs):
        return snapshot

    sessions = _Sessions()
    control = _Control()
    state = SimpleNamespace(
        session_manager=sessions,
        assistant_runtime_assignments=_Assignments(),
        agent_runtime_control=control,
        settings=SimpleNamespace(default_model="builtin-default"),
    )
    monkeypatch.setattr(agent_module, "_version_snapshot", version_snapshot)
    monkeypatch.setattr(agent_module, "_store", lambda _request: _Store())
    body = ThreadCreateRequest(
        session_id="same-session", agent_id=agent_id, agent_version_id=version_id
    )
    with pytest.raises(Exception) as outage:
        await create_thread(body, _request(state), actor)
    assert getattr(outage.value, "status_code", None) == 503
    assert sessions.item.agent_version_id == version_id
    retried = await create_thread(body, _request(state), actor)
    assert retried["thread"]["id"] == thread_id
    assert sessions.bind_count == 2
    assert control.attempts == 2


@pytest.mark.asyncio
async def test_v2_thread_lookup_does_not_cross_user_scope() -> None:
    db = _Database()
    db.thread = {
        "runtime_thread_id": uuid4(), "tenant_id": "tenant-a", "user_id": "user-a",
        "session_id": "session-a", "kernel_owner": "agent", "source_kind": "native",
        "import_status": "not_required", "last_sequence": 0,
    }
    state = SimpleNamespace(database=db, assistant_runtime_assignments=SimpleNamespace())
    user = UserContext(
        user_id="user-b", tenant_id="tenant-a", tier="normal",
        is_authenticated=True, roles=["user"], ip="127.0.0.1",
    )
    with pytest.raises(Exception) as exc_info:
        await _get_thread(_request(state), user, str(db.thread["runtime_thread_id"]))
    assert getattr(exc_info.value, "status_code", None) == 404


@pytest.mark.asyncio
async def test_v2_events_use_runtime_live_stream_and_preserve_terminal() -> None:
    db = _Database()
    runtime_thread_id = str(uuid4())
    db.thread = {
        "runtime_thread_id": runtime_thread_id, "tenant_id": "tenant-a", "user_id": "user-a",
        "session_id": "session-a", "kernel_owner": "agent", "source_kind": "native",
        "import_status": "not_required", "last_sequence": 4,
    }

    class _Assignments:
        async def resolve(self, **_kwargs):
            return SimpleNamespace(runtime_owner="agent_runtime", kernel_revision="kernel-1")

    class _Control:
        async def stream_thread_events(self, **_kwargs):
            turn_id = _kwargs["turn_id"]
            yield {
                "schema_version": "assistant-turn-contract/v1", "sequence": 5,
                "event_type": "run_started", "data": {"run_id": turn_id},
                "timestamp": "2026-08-21T00:00:00Z",
            }
            yield {
                "schema_version": "assistant-turn-contract/v1", "sequence": 6,
                "event_type": "text_delta", "data": {"run_id": turn_id, "content": "hello"},
                "timestamp": "2026-08-21T00:00:00Z",
            }
            yield {
                "schema_version": "assistant-turn-contract/v1", "sequence": 7,
                "event_type": "run_finished", "data": {"run_id": turn_id, "status": "succeeded"},
                "timestamp": "2026-08-21T00:00:01Z",
            }

    state = SimpleNamespace(
        database=db,
        assistant_runtime_assignments=_Assignments(),
        agent_runtime_control=_Control(),
    )
    response = await thread_events(
        runtime_thread_id,
        _request(state),
        after_sequence=4,
        limit=10,
        turn_id=str(uuid4()),
        user=UserContext(
            user_id="user-a", tenant_id="tenant-a", tier="normal",
            is_authenticated=True, roles=["user"], ip="127.0.0.1",
        ),
    )
    chunks = [chunk async for chunk in response.body_iterator]
    assert len(chunks) == 3
    assert b'"sequence":5' in chunks[0]
    assert b'"effective_reasoning_option":"minimal"' in chunks[0]
    assert b'"event_type":"run_finished"' in chunks[2]


@pytest.mark.asyncio
async def test_v2_turn_uses_gateway_default_when_model_is_omitted() -> None:
    db = _Database()
    runtime_thread_id = str(uuid4())
    db.thread = {
        "runtime_thread_id": runtime_thread_id, "tenant_id": "tenant-a", "user_id": "user-a",
        "session_id": "session-a", "kernel_owner": "agent", "source_kind": "native",
        "import_status": "not_required", "last_sequence": 4,
    }

    class _Assignments:
        async def resolve(self, **_kwargs):
            return SimpleNamespace(runtime_owner="agent_runtime", kernel_revision="kernel-1")

    class _Control:
        async def start_turn(self, **kwargs):
            assert kwargs["model_id"] == "qwen-default"
            assert kwargs["temperature"] is None
            assert isinstance(kwargs["resolved_agent_launch"], ResolvedAgentLaunchV1)
            return SimpleNamespace(
                run_id=str(uuid4()), runtime_thread_id=runtime_thread_id,
                requested_reasoning_option="auto", effective_reasoning_option="minimal",
                after_sequence=4,
            )

    state = SimpleNamespace(
        database=db, assistant_runtime_assignments=_Assignments(),
        agent_runtime_control=_Control(), settings=SimpleNamespace(default_model="qwen-default"),
    )
    response = await create_turn(
        runtime_thread_id, TurnCreateRequest(message="hello"), _request(state),
        UserContext(user_id="user-a", tenant_id="tenant-a", tier="normal", is_authenticated=True, roles=["user"], ip="127.0.0.1"),
    )
    assert response["turn"]["status"] == "in_progress"


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "field, value",
    [
        ("execution_profile", "balanced"),
        ("memory_mode", "user"),
        ("resume_run_id", "run-a"),
        ("resume_approval_id", "approval-a"),
    ],
)
async def test_v2_turn_rejects_unmigrated_capabilities(
    field: str, value: object,
) -> None:
    body = TurnCreateRequest(message="hello", **{field: value})
    with pytest.raises(Exception) as exc_info:
        _reject_unmigrated_turn_capabilities(body)
    assert getattr(exc_info.value, "status_code", None) == 409


@pytest.mark.parametrize(
    "field, value",
    [
        ("system_prompt", "Be concise."),
        ("os_agent_enabled", True),
        ("local_node_device_id", "node-a"),
        ("local_node_grant_ids", ["grant-a"]),
    ],
)
def test_v2_turn_accepts_style_and_local_node_fields(field: str, value: object) -> None:
    _reject_unmigrated_turn_capabilities(TurnCreateRequest(message="hello", **{field: value}))


@pytest.mark.parametrize("memory_mode", ["auto", "strict", "off"])
def test_v2_turn_accepts_migrated_memory_modes_and_temperature(memory_mode: str) -> None:
    _reject_unmigrated_turn_capabilities(
        TurnCreateRequest(
            message="hello",
            temperature=0.2,
            execution_profile="safe",
            memory_mode=memory_mode,
        )
    )


@pytest.mark.asyncio
async def test_v2_session_race_only_adopts_existing_session_error() -> None:
    class _Sessions:
        async def get(self, _session_id):
            return None

        async def create(self, **_kwargs):
            raise RuntimeError("database unavailable")

    state = SimpleNamespace(session_manager=_Sessions())
    with pytest.raises(RuntimeError, match="database unavailable"):
        await create_thread(
            ThreadCreateRequest(session_id="session-race"), _request(state),
            UserContext(user_id="user-a", tenant_id="tenant-a", tier="normal", is_authenticated=True, roles=["user"], ip="127.0.0.1"),
        )


@pytest.mark.asyncio
async def test_v2_existing_session_is_bound_before_thread_creation() -> None:
    db = _Database()
    runtime_thread_id = str(uuid4())
    bound: list[str] = []

    class _Sessions:
        async def get(self, session_id: str):
            return SimpleNamespace(session_id=session_id, user_id="user-a", tenant_id="tenant-a")

        async def history(self, _session_id: str, limit: int = 1):
            del limit
            return []

    class _Assignments:
        async def bind_new_session(self, **kwargs):
            bound.append(kwargs["session_id"])
            return SimpleNamespace(runtime_owner="agent_runtime", kernel_revision="kernel-1")

        async def resolve(self, **_kwargs):
            if not bound:
                return None
            return SimpleNamespace(runtime_owner="agent_runtime", kernel_revision="kernel-1")

    class _Control:
        async def ensure_thread(self, **_kwargs):
            db.thread = {
                "runtime_thread_id": runtime_thread_id,
                "tenant_id": "tenant-a",
                "user_id": "user-a",
                "session_id": "existing-session",
                "kernel_owner": "agent",
                "source_kind": "native",
                "import_status": "not_required",
                "last_sequence": 0,
            }
            return {"runtime_thread_id": runtime_thread_id, "last_sequence": 0}

    state = SimpleNamespace(
        database=db,
        session_manager=_Sessions(),
        assistant_runtime_assignments=_Assignments(),
        assistant_runtime_assignment_policy=SimpleNamespace(),
        agent_runtime_control=_Control(),
        settings=SimpleNamespace(default_model="qwen3.7-plus"),
    )
    response = await create_thread(
        ThreadCreateRequest(session_id="existing-session"),
        _request(state),
        UserContext(
            user_id="user-a", tenant_id="tenant-a", tier="normal",
            is_authenticated=True, roles=["user"], ip="127.0.0.1",
        ),
    )
    assert bound == ["existing-session"]
    assert response["thread"]["id"] == runtime_thread_id


@pytest.mark.asyncio
async def test_v2_new_session_assignment_failure_cleans_up_session() -> None:
    deleted: list[str] = []

    class _Sessions:
        async def get(self, _session_id):
            return None

        async def create(self, **kwargs):
            return SimpleNamespace(session_id=kwargs["session_id"] if kwargs.get("session_id") else "new-session")

        async def delete(self, session_id):
            deleted.append(session_id)

    class _Assignments:
        async def bind_new_session(self, **_kwargs):
            raise RuntimeError("assignment unavailable")

    state = SimpleNamespace(session_manager=_Sessions(), assistant_runtime_assignments=_Assignments())
    with pytest.raises(Exception) as exc_info:
        await create_thread(
            ThreadCreateRequest(session_id="new-session"), _request(state),
            UserContext(user_id="user-a", tenant_id="tenant-a", tier="normal", is_authenticated=True, roles=["user"], ip="127.0.0.1"),
        )
    assert getattr(exc_info.value, "status_code", None) == 409
    assert deleted == ["new-session"]


@pytest.mark.asyncio
async def test_v2_approval_routes_forward_the_thread_scope_and_reject_repeat_decisions() -> None:
    db = _Database()
    thread_id = str(uuid4())
    db.thread = {
        "runtime_thread_id": thread_id, "tenant_id": "tenant-a", "user_id": "user-a",
        "session_id": "session-a", "kernel_owner": "agent", "source_kind": "native",
        "import_status": "not_required", "last_sequence": 4,
    }

    class _Assignments:
        async def resolve(self, **_kwargs):
            return SimpleNamespace(runtime_owner="agent_runtime", kernel_revision="kernel-1")

    class _Control:
        def __init__(self) -> None:
            self.decisions = []

        async def get_approval(self, **kwargs):
            assert kwargs["tenant_id"] == "tenant-a"
            assert kwargs["user_id"] == "user-a"
            assert kwargs["session_id"] == "session-a"
            return {"approval_id": "approval-1", "status": "pending"}

        async def decide_approval(self, **kwargs):
            self.decisions.append(kwargs)
            if len(self.decisions) > 1:
                raise AgentRuntimeControlError("AI_PLATFORM_AGENT_RUNTIME_APPROVAL_DECISION_FAILED", status_code=409)
            return {"approval_id": kwargs["approval_id"], "status": "consumed"}

    control = _Control()
    state = SimpleNamespace(
        database=db,
        assistant_runtime_assignments=_Assignments(),
        agent_runtime_control=control,
    )
    user = UserContext(
        user_id="user-a", tenant_id="tenant-a", tier="normal",
        is_authenticated=True, roles=["user"], ip="127.0.0.1",
    )
    response = await get_thread_approval(thread_id, "approval-1", _request(state), user)
    assert response["approval"]["status"] == "pending"
    decision = await decide_thread_approval(
        thread_id, "approval-1", ApprovalDecisionRequest(approved=False, reason="no"),
        _request(state), user,
    )
    assert decision["approval"]["status"] == "consumed"
    with pytest.raises(Exception) as exc_info:
        await decide_thread_approval(
            thread_id, "approval-1", ApprovalDecisionRequest(approved=False),
            _request(state), user,
        )
    assert getattr(exc_info.value, "status_code", None) == 409


@pytest.mark.asyncio
async def test_v2_approval_decision_requires_verified_action_for_approve(monkeypatch) -> None:
    db = _Database()
    thread_id = str(uuid4())
    db.thread = {
        "runtime_thread_id": thread_id, "tenant_id": "tenant-a", "user_id": "user-a",
        "session_id": "session-a", "kernel_owner": "agent", "source_kind": "native",
        "import_status": "not_required", "last_sequence": 0,
    }

    class _Assignments:
        async def resolve(self, **_kwargs):
            return SimpleNamespace(runtime_owner="agent_runtime", kernel_revision="kernel-1")

    class _Control:
        def __init__(self) -> None:
            self.decisions = []

        async def get_approval(self, **_kwargs):
            return {"status": "pending", "run_id": "run-a", "tool_name": "execute_python_code"}

        async def decide_approval(self, **kwargs):
            self.decisions.append(kwargs)
            return {"status": "approved"}

    control = _Control()
    state = SimpleNamespace(
        database=db, assistant_runtime_assignments=_Assignments(), agent_runtime_control=control,
    )
    user = UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)
    previews = []

    async def _preview(*_args, **kwargs):
        previews.append(kwargs)
        return {"can_approve": False}

    monkeypatch.setattr("src.api.v2.agent.owner_approval_preview", _preview)
    with pytest.raises(Exception) as exc_info:
        await decide_thread_approval(
            thread_id, "approval-1", ApprovalDecisionRequest(approved=True), _request(state), user,
        )
    assert getattr(exc_info.value, "status_code", None) == 409
    assert control.decisions == []
    assert previews[0]["runtime_thread_id"] == thread_id

    # Rejection remains available when action details cannot be verified.
    await decide_thread_approval(
        thread_id, "approval-1", ApprovalDecisionRequest(approved=False), _request(state), user,
    )
    assert len(control.decisions) == 1 and control.decisions[0]["approved"] is False

    async def _verified(*_args, **_kwargs):
        return {"can_approve": True}

    monkeypatch.setattr("src.api.v2.agent.owner_approval_preview", _verified)
    await decide_thread_approval(
        thread_id, "approval-1", ApprovalDecisionRequest(approved=True), _request(state), user,
    )
    assert control.decisions[-1]["approved"] is True


@pytest.mark.asyncio
async def test_v2_approval_route_does_not_leak_cross_tenant_approval() -> None:
    db = _Database()
    db.thread = {
        "runtime_thread_id": str(uuid4()), "tenant_id": "tenant-a", "user_id": "user-a",
        "session_id": "session-a", "kernel_owner": "agent", "source_kind": "native",
        "import_status": "not_required", "last_sequence": 0,
    }
    state = SimpleNamespace(
        database=db,
        assistant_runtime_assignments=SimpleNamespace(resolve=lambda **_: None),
        agent_runtime_control=SimpleNamespace(),
    )
    user = UserContext(
        user_id="user-b", tenant_id="tenant-b", tier="normal",
        is_authenticated=True, roles=["user"], ip="127.0.0.1",
    )
    with pytest.raises(Exception) as exc_info:
        await get_thread_approval(str(db.thread["runtime_thread_id"]), "approval-1", _request(state), user)
    assert getattr(exc_info.value, "status_code", None) == 404


@pytest.mark.asyncio
async def test_recovery_reuses_original_identity_and_denies_foreign_actor() -> None:
    from fastapi import HTTPException

    from src.api.v2.agent import recover_turn

    db = _Database()
    thread, run = str(uuid4()), str(uuid4())
    db.thread = {
        "runtime_thread_id": thread, "tenant_id": "tenant-a", "user_id": "user-a", "session_id": "session-a",
        "kernel_owner": "agent", "source_kind": "native", "import_status": "not_required", "last_sequence": 4,
    }
    calls = []

    class _Assignments:
        async def resolve(self, **_kwargs):
            return SimpleNamespace(runtime_owner="agent_runtime", kernel_revision="kernel-1")

    class _Control:
        async def recover_turn(self, **kwargs):
            calls.append(kwargs)
            return {"status": "recovery_requested"}

    state = SimpleNamespace(database=db, assistant_runtime_assignments=_Assignments(), agent_runtime_control=_Control())
    actor = UserContext(user_id="user-a", tenant_id="tenant-a", tier="normal", is_authenticated=True, roles=["user"], ip="127.0.0.1")
    for _ in range(2):
        result = await recover_turn(thread, run, _request(state), actor)
        assert result["turn_id"] == run
    assert calls == [{"runtime_thread_id": thread, "turn_id": run, "tenant_id": "tenant-a", "user_id": "user-a", "session_id": "session-a"}] * 2
    with pytest.raises(HTTPException) as denied:
        await recover_turn(thread, run, _request(state), UserContext(user_id="user-other", tenant_id="tenant-a", tier="normal", is_authenticated=True, roles=["user"], ip="127.0.0.1"))
    assert denied.value.status_code == 404
    assert len(calls) == 2
