"""Mounted V1 chat outcome contract; real source guard, controlled Runtime IO."""
from __future__ import annotations

import asyncio
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from ai_assistant.models.response import ChatResponse

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
                            block=None, entered=asyncio.Event(), cleanup_error=None, deny_on_close=False, close_action=None, names={"dataset-one": "Current name"})
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
            if state.close_action:
                state.close_action()
            if state.deny_on_close:
                state.denied = True
            if state.cleanup_error:
                raise state.cleanup_error

    async def fetch(query, *args):
        state.reads += 1
        assert args == (SESSION, "tenant", "owner")
        return []

    async def visible(actor):
        assert actor is user
        return [] if state.denied else [{"dataset_id": key, "name": name} for key, name in state.names.items()]

    async def documents(actor, dataset, ids):
        assert actor is user and dataset in state.names
        return set() if state.denied else set(ids)

    async def versions(actor, dataset, refs):
        assert actor is user and dataset in state.names
        return set() if state.denied else {
            (r["document_id"], r["source_version"], r["source_hash"]) for r in refs
        }

    app.state.database = SimpleNamespace(fetch=fetch, fetchrow=AsyncMock(return_value=None))
    app.state.kb_proxy = SimpleNamespace(list_datasets=visible, authorize_documents=documents,
                                        authorize_document_sources=versions)
    state.proxy = app.state.kb_proxy
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


HASH = "a" * 64


def chunk(**changes):
    return {"dataset_id": "dataset-one", "document_id": "doc-one", "segment_id": "seg-one",
            "content": "cited excerpt", "score": 0.75,
            "metadata": {"source_version": 3, "source_hash": HASH,
                         "private_token": "internal-secret", "source_reference": "secret-url"},
            **changes}


def context(*chunks, **changes):
    return event("context_retrieved", chunks=list(chunks or [chunk()]),
                 **{"tool_call_id": "call-one", **changes})


async def answer(host, *events):
    host.events = [*events, event("text_delta", content="answer"), finished()]
    return await host.post()


@pytest.mark.asyncio
async def test_success_returns_authorized_citation(host):
    response = await answer(host, context())
    assert response.status_code == 200
    value = response.json()
    assert value["contexts"], "successful chat must retain authorized citations"
    citation = value["contexts"][0]
    assert citation == {"dataset_id": "dataset-one", "dataset_name": "Current name", "chunks": [{
        "content": "cited excerpt", "score": 0.75,
        "metadata": {"document_id": "doc-one", "segment_id": "seg-one",
                     "source_version": 3, "source_hash": HASH}}]}
    assert value["content"] == "answer" and value["usage"] == {}
    assert "internal-secret" not in response.text and "secret-url" not in response.text
    decoded = ChatResponse.from_dict(value)
    assert decoded.contexts == value["contexts"] and decoded.run_id == RUN
    assert host.starts == host.closed == 1


@pytest.mark.asyncio
async def test_post_stream_revocation_blocks_response(host):
    host.deny_on_close = True
    response = await answer(host, context())
    assert response.status_code == 403, "final citation rights must be rechecked after stream cleanup"
    assert response.json()["detail"]["code"] == "ASSISTANT_SOURCE_ACCESS_REVOKED"
    assert "cited excerpt" not in response.text and "answer" not in response.text
    assert host.starts == host.closed == 1


@pytest.mark.asyncio
async def test_plain_answer_retains_empty_contexts(host):
    response = await answer(host)
    assert response.status_code == 200 and response.json()["contexts"] == []


@pytest.mark.asyncio
async def test_unversioned_legacy_excerpt_remains_unversioned(host):
    response = await answer(host, context(chunk(metadata={})))
    assert response.status_code == 200
    assert response.json()["contexts"][0]["chunks"][0]["metadata"] == {
        "document_id": "doc-one", "segment_id": "seg-one"}


@pytest.mark.asyncio
async def test_current_catalog_names_and_multiple_datasets(host):
    host.names["dataset-two"] = "Second current name"
    response = await answer(host, context(chunk(), chunk(dataset_id="dataset-two", document_id="doc-two")))
    assert response.status_code == 200
    assert [(x["dataset_id"], x["dataset_name"]) for x in response.json()["contexts"]] == [
        ("dataset-one", "Current name"), ("dataset-two", "Second current name")]


@pytest.mark.asyncio
@pytest.mark.parametrize("field", ["session_id", "thread_id"], ids=["session", "thread"])
async def test_mismatched_context_binding_is_unknown(host, field):
    response = await answer(host, context(**{field: "other"}))
    assert response.status_code == 502
    assert "cited excerpt" not in response.text


@pytest.mark.asyncio
async def test_other_run_context_is_not_attached(host):
    response = await answer(host, context(run_id="other-run"))
    assert response.status_code == 200 and response.json()["contexts"] == []


@pytest.mark.asyncio
@pytest.mark.parametrize("call", [None, "", "   "], ids=["null", "empty", "blank"])
async def test_context_requires_tool_call_identity(host, call):
    response = await answer(host, context(tool_call_id=call))
    assert response.status_code == 200 and response.json()["contexts"] == []


@pytest.mark.asyncio
async def test_raw_tool_result_is_not_a_citation(host):
    response = await answer(host, event("tool_result", tool_call_id="call-one", chunks=[chunk()]))
    assert response.status_code == 200 and response.json()["contexts"] == []


@pytest.mark.asyncio
async def test_identical_chunks_deduplicate_distinct_excerpts_remain(host):
    response = await answer(host, context(chunk(), chunk()), context(chunk(content="different excerpt")))
    assert response.status_code == 200
    assert [x["content"] for x in response.json()["contexts"][0]["chunks"]] == [
        "cited excerpt", "different excerpt"]


@pytest.mark.asyncio
async def test_excerpt_bound_keeps_full_source_identity(host):
    response = await answer(host, context(chunk(content="a" * 3000)))
    result = response.json()["contexts"][0]["chunks"][0]
    assert len(result["content"]) == 2000
    assert result["metadata"]["source_hash"] == HASH and result["metadata"]["source_version"] == 3


@pytest.mark.asyncio
async def test_chunk_and_dataset_limits(host):
    host.names.update({f"dataset-{i}": f"Name {i}" for i in range(25)})
    chunks = [chunk(segment_id=f"seg-{i}") for i in range(25)]
    chunks += [chunk(dataset_id=f"dataset-{i}") for i in range(25)]
    response = await answer(host, context(*chunks))
    assert response.status_code == 200
    values = response.json()["contexts"]
    assert len(values) == 20 and len(values[0]["chunks"]) == 20


@pytest.mark.asyncio
@pytest.mark.parametrize("score", [None, True, "0.5", float("nan"), float("inf")],
                         ids=["null", "bool", "string", "nan", "infinity"])
async def test_nonfinite_or_absent_score_is_not_invented(host, score):
    response = await answer(host, context(chunk(score=score)))
    assert response.status_code == 200
    assert "score" not in response.json()["contexts"][0]["chunks"][0]


@pytest.mark.asyncio
async def test_conflicting_segment_identity_is_not_returned(host):
    response = await answer(host, context(chunk(metadata={"segment_id": "other"})))
    assert response.status_code == 502 and "cited excerpt" not in response.text


@pytest.mark.asyncio
async def test_failed_run_never_returns_collected_citations(host):
    host.events = [context(), event("run_error", status="failed")]
    response = await host.post()
    assert response.status_code == 409 and "cited excerpt" not in response.text


@pytest.mark.asyncio
async def test_revocation_remains_sticky_after_new_context(host):
    def revoke():
        host.denied = True
        return event("text_delta", content="private")
    response = await answer(host, context(), revoke)
    assert response.status_code == 403 and "cited excerpt" not in response.text


@pytest.mark.asyncio
@pytest.mark.parametrize("method", ["authorize_documents", "authorize_document_sources"],
                         ids=["document", "immutable-version"])
async def test_exact_source_rights_rechecked_after_cleanup(host, method):
    host.close_action = lambda: setattr(host.proxy, method, AsyncMock(return_value=set()))
    response = await answer(host, context())
    assert response.status_code == 403 and "cited excerpt" not in response.text


@pytest.mark.asyncio
async def test_catalog_outage_does_not_leak_answer(host):
    host.close_action = lambda: setattr(host.proxy, "list_datasets", AsyncMock(side_effect=TimeoutError("internal-secret")))
    response = await answer(host, context())
    assert response.status_code == 403
    assert "internal-secret" not in response.text and "cited excerpt" not in response.text


@pytest.mark.asyncio
@pytest.mark.parametrize("case", ["chunk-cap", "dataset-cap", "duplicate", "missing-call"],
                         ids=["chunk-cap", "dataset-cap", "duplicate", "missing-call"])
async def test_hidden_or_duplicate_refs_rechecked_after_cleanup(host, case):
    host.names.update({f"dataset-{i}": f"Name {i}" for i in range(21)})
    target = "doc-revoked"
    if case == "chunk-cap":
        chunks = [chunk(document_id=f"doc-{i}") for i in range(20)] + [chunk(document_id=target)]
        events = [context(*chunks)]
    elif case == "dataset-cap":
        events = [context(*[chunk(dataset_id=f"dataset-{i}", document_id=f"doc-{i}")
                            for i in range(20)], chunk(dataset_id="dataset-20", document_id=target))]
    elif case == "duplicate":
        events = [context(chunk(document_id=target), chunk(document_id=target))]
    else:
        events = [context(chunk(document_id=target), tool_call_id=None)]

    async def remaining(_actor, _dataset, ids):
        return set(ids) - {target}

    host.close_action = lambda: setattr(host.proxy, "authorize_documents", remaining)
    response = await answer(host, *events)
    assert response.status_code == 403
    assert "answer" not in response.text and "cited excerpt" not in response.text


@pytest.mark.asyncio
async def test_authority_reference_overflow_fails_closed(host):
    response = await answer(host, context(*[chunk(document_id=f"doc-{i}") for i in range(130)]))
    assert response.status_code == 502 and "cited excerpt" not in response.text
    assert host.starts == host.closed == 1
