"""Mounted V1 chat outcome contract; real source guard, controlled Runtime IO."""
from __future__ import annotations

import asyncio
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from fastapi import HTTPException

from src.api import deps
from src.api.v1._assistant_routes import chat
from src.core.auth.user_resolver import UserContext

RUN = "00000000-0000-0000-0000-000000000001"
THREAD = "00000000-0000-0000-0000-000000000002"
SESSION = "session-one"


def event(kind, **changes):
    return {"event_type": kind, "data": {
        "run_id": RUN, "session_id": SESSION, "thread_id": THREAD, **changes,
    }}


def finished(**changes):
    return event("run_finished", status="succeeded",
                 **{"terminal_envelope": envelope(), **changes})


def envelope(**changes):
    return {"run_id": RUN, "session_id": SESSION, "thread_id": THREAD,
            "status": "succeeded", "exit_reason": "succeeded", **changes}


@pytest.fixture
def host(monkeypatch):
    from fastapi import FastAPI

    app = FastAPI()
    app.include_router(chat.router, prefix="/api/v1/assistant")
    user = UserContext(user_id="owner", tenant_id="tenant", is_authenticated=True)
    app.dependency_overrides[deps.get_user_context] = lambda: user
    state = SimpleNamespace(events=[], starts=0, closed=0, reads=0, denied=False,
                            block=None, entered=asyncio.Event(), cleanup_error=None)
    turn = SimpleNamespace(run_id=RUN, runtime_thread_id=THREAD)

    async def start(*args, **kwargs):
        state.starts += 1
        return turn

    async def frames(**kwargs):
        assert kwargs == {"turn": turn, "tenant_id": "tenant", "user_id": "owner",
                          "session_id": SESSION}
        try:
            for sequence, raw in enumerate(state.events, 1):
                if callable(raw):
                    raw = raw()
                if isinstance(raw, BaseException):
                    raise raw
                if isinstance(raw, bytes):
                    yield raw
                else:
                    yield ("data: " + json.dumps({"sequence": sequence, **raw}) + "\n\n").encode()
            if state.block:
                state.entered.set()
                await state.block.wait()
        finally:
            state.closed += 1
            if state.cleanup_error:
                raise state.cleanup_error

    async def fetch(query, *args):
        state.reads += 1
        assert args == (SESSION, "tenant", "owner")
        return []

    async def visible(actor):
        assert actor is user
        return [] if state.denied else [{"dataset_id": "dataset-one"}]

    async def documents(actor, dataset, ids):
        assert actor is user and dataset == "dataset-one"
        return set() if state.denied else set(ids)

    async def versions(actor, dataset, refs):
        assert actor is user and dataset == "dataset-one"
        return set() if state.denied else {
            (r["document_id"], r["source_version"], r["source_hash"]) for r in refs
        }

    app.state.database = SimpleNamespace(fetch=fetch, fetchrow=AsyncMock(return_value=None))
    app.state.kb_proxy = SimpleNamespace(list_datasets=visible, authorize_documents=documents,
                                        authorize_document_sources=versions)
    app.state.agent_runtime_control = SimpleNamespace(stream_events=frames)
    monkeypatch.setattr(deps, "enforce_rate_limit", AsyncMock())
    monkeypatch.setattr(chat, "effective_chat_model_id", lambda *_args: "model-one")
    monkeypatch.setattr(chat, "validate_chat_session_access", AsyncMock())
    monkeypatch.setattr(chat, "ensure_agent_runtime_session", AsyncMock())
    monkeypatch.setattr(chat, "_start_agent_runtime_turn", start)

    async def post():
        async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app),
                                     base_url="http://offline.test") as client:
            return await client.post("/api/v1/assistant/chat", json={
                "message": "question", "session_id": SESSION,
            })

    state.post = post
    return state


def assert_error(response, code, status=502):
    assert response.status_code == status, "non-success Runtime outcome must not return HTTP 200"
    value = response.json()["detail"]
    assert value["code"] == code
    assert value["run_id"] == RUN and value["session_id"] == SESSION
    assert "private partial" not in response.text and "internal-secret" not in response.text


@pytest.mark.asyncio
async def test_failed_terminal_is_not_success(host):
    host.events = [event("text_delta", content="private partial"),
                   event("run_error", status="failed", message="internal-secret")]
    assert_error(await host.post(), "ASSISTANT_CHAT_FAILED", 409)
    assert host.starts == host.closed == 1


@pytest.mark.asyncio
async def test_eof_is_unknown(host):
    host.events = [event("text_delta", content="private partial")]
    assert_error(await host.post(), "ASSISTANT_CHAT_OUTCOME_UNKNOWN")
    assert host.starts == host.closed == 1


@pytest.mark.asyncio
async def test_genuine_success_retains_shape(host):
    host.events = [event("text_delta", content="answer"), finished(terminal_envelope=envelope())]
    response = await host.post()
    assert response.status_code == 200
    value = response.json()
    assert value["content"] == "answer" and value["run_id"] == RUN
    assert value["session_id"] == SESSION and value["model_id"] == "model-one"
    assert value["contexts"] == [] and value["usage"] == {}
    assert host.starts == host.closed == 1 and host.reads > 0


@pytest.mark.asyncio
async def test_recovery_terminal_without_envelope_and_empty_text(host):
    recovered = finished(recovery="original_turn_reconciled")
    recovered["data"].pop("terminal_envelope")
    host.events = [recovered]
    response = await host.post()
    assert response.status_code == 200 and response.json()["content"] == ""


@pytest.mark.asyncio
async def test_unrelated_run_events_do_not_change_answer(host):
    host.events = [event("text_delta", run_id="other", content="internal-secret"),
                   event("run_error", run_id="other", status="failed"),
                   event("text_delta", content="answer"), finished()]
    response = await host.post()
    assert response.status_code == 200 and response.json()["content"] == "answer"


@pytest.mark.asyncio
async def test_source_revocation_stays_sticky(host):
    context = event("context_retrieved", chunks=[{
        "dataset_id": "dataset-one", "document_id": "document-one", "segment_id": "segment-one",
        "content": "internal-secret", "metadata": {"source_version": 2, "source_hash": "a" * 64},
    }])

    def revoke():
        host.denied = True
        return event("text_delta", content="internal-secret")

    def restore():
        host.denied = False
        return finished()

    host.events = [context, event("text_delta", content="private partial"), revoke, restore]
    assert_error(await host.post(), "ASSISTANT_SOURCE_ACCESS_REVOKED", 403)
    assert host.starts == host.closed == 1


@pytest.mark.asyncio
async def test_request_cancellation_closes_runtime_once(host):
    host.block = asyncio.Event()
    task = asyncio.create_task(host.post())
    await host.entered.wait()
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert host.starts == host.closed == 1


@pytest.mark.asyncio
async def test_stream_failure_is_safe_and_closes(host):
    host.events = [event("text_delta", content="private partial"), RuntimeError("internal-secret")]
    assert_error(await host.post(), "ASSISTANT_CHAT_OUTCOME_UNKNOWN")
    assert host.starts == host.closed == 1


@pytest.mark.asyncio
async def test_cleanup_failure_is_not_success(host):
    host.events = [finished()]
    host.cleanup_error = ValueError("internal-secret")
    assert_error(await host.post(), "ASSISTANT_CHAT_OUTCOME_UNKNOWN")
    assert host.starts == host.closed == 1


_CASES = [
    (event("run_error", status="cancelled"), "ASSISTANT_CHAT_CANCELLED", 409),
    (event("cancelled"), "ASSISTANT_CHAT_CANCELLED", 409),
    (event("run_finished", status="blocked"), "ASSISTANT_CHAT_BLOCKED", 409),
    (event("side_effect_unknown"), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (event("error", message="internal-secret"), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (event("tool_finished", status="succeeded"), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (event("run_finished", status="completed"), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (event("run_finished", status="future"), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (event("run_finished"), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (event("run_error", status="succeeded"), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (finished(run_id="other"), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (finished(session_id="other"), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (finished(thread_id="other"), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (finished(terminal_envelope={}), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (finished(terminal_envelope="invalid"), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (finished(terminal_envelope=envelope(run_id="other")), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (finished(terminal_envelope=envelope(status="failed")), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (finished(terminal_envelope=envelope(exit_reason="side_effect_unknown")),
     "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
    (finished(durable=False), "ASSISTANT_CHAT_OUTCOME_UNKNOWN", 502),
]


@pytest.mark.asyncio
@pytest.mark.parametrize("terminal,code,status", _CASES, ids=[f"case{i}" for i in range(19)])
async def test_terminal_contract(host, terminal, code, status):
    host.events = [event("text_delta", content="private partial"), terminal]
    assert_error(await host.post(), code, status)
    assert host.starts == host.closed == 1


@pytest.mark.asyncio
async def test_conflicting_terminals_fail_closed(host):
    host.events = [finished(), event("run_error", status="failed")]
    assert_error(await host.post(), "ASSISTANT_CHAT_OUTCOME_UNKNOWN")


@pytest.mark.asyncio
async def test_unscoped_text_cannot_enter_success(host):
    host.events = [{"event_type": "text_delta", "data": {"content": "internal-secret"}}, finished()]
    response = await host.post()
    assert response.status_code == 200 and response.json()["content"] == ""


@pytest.mark.asyncio
async def test_upstream_http_exception_is_sanitized(host):
    host.events = [HTTPException(418, detail={"message": "internal-secret", "run_id": "other"})]
    assert_error(await host.post(), "ASSISTANT_CHAT_OUTCOME_UNKNOWN")
    assert host.starts == host.closed == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", [RuntimeError("internal-secret"), b"data: malformed\n\n"],
                         ids=["stream-error", "malformed-frame"])
async def test_revocation_precedes_later_read_failure(host, failure):
    host.denied = True
    host.events = [event("context_retrieved", chunks=[{
        "dataset_id": "dataset-one", "document_id": "document-one", "content": "private partial",
    }]), failure]
    assert_error(await host.post(), "ASSISTANT_SOURCE_ACCESS_REVOKED", 403)
    assert host.starts == host.closed == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("field,missing", [
    ("run_id", True), ("run_id", False), ("session_id", True),
    ("session_id", False), ("thread_id", True), ("thread_id", False),
], ids=["run-missing", "run-null", "session-missing", "session-null", "thread-missing", "thread-null"])
async def test_terminal_requires_bound_identity(host, field, missing):
    value = finished()
    if missing:
        value["data"].pop(field)
    else:
        value["data"][field] = None
    host.events = [value]
    assert_error(await host.post(), "ASSISTANT_CHAT_OUTCOME_UNKNOWN")


@pytest.mark.asyncio
async def test_null_envelope_is_not_absent_recovery_envelope(host):
    host.events = [finished(recovery="original_turn_reconciled", terminal_envelope=None)]
    assert_error(await host.post(), "ASSISTANT_CHAT_OUTCOME_UNKNOWN")


@pytest.mark.asyncio
async def test_absent_envelope_requires_recovery_producer(host):
    value = finished()
    value["data"].pop("terminal_envelope")
    host.events = [value]
    assert_error(await host.post(), "ASSISTANT_CHAT_OUTCOME_UNKNOWN")
