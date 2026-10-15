"""Revoked sources cannot escape through replay, inherited turns or downloads."""
from __future__ import annotations

import json
from types import SimpleNamespace
from unittest.mock import AsyncMock
from uuid import UUID

import pytest
from ai_gateway_core.session.models import Session
from fastapi import HTTPException
from starlette.requests import Request

from src.api.schemas.assistant import AssistantChatRequest
from src.api.v1 import quiz as quiz_routes
from src.api.v1._assistant_routes import artifacts, chat
from src.api.v2 import agent
from src.core.auth.user_resolver import UserContext
from src.services.assistant_entry.source_access import (
    ConversationEventGuard,
    conversation_sources,
    quiz_source_ids,
    require_conversation_source_access,
)

USER = UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)
THREAD = SimpleNamespace(session_id="session-a", runtime_thread_id="00000000-0000-0000-0000-000000000001")


def request(*, visible=(), snapshots=None, legacy=None, kb_error=False, contexts=None, visible_docs=None, visible_versions=None):
    if snapshots is None:
        snapshots = [snapshot("run-a", ["private-a"]), snapshot("run-b", [])]
    async def fetch(query, *args):
        assert "FROM assistant_runtime_snapshots" in query or "FROM assistant_runtime_items" in query
        assert "tenant_id = $2 AND user_id = $3" in query
        assert "ORDER BY created_at" in query or "ORDER BY sequence" in query
        assert args == ("session-a", "tenant-a", "user-a")
        return snapshots if "FROM assistant_runtime_snapshots" in query else (contexts or [])
    async def fetchrow(query, *args):
        assert "SELECT history FROM assistant.sessions" in query
        assert args == ("session-a", "tenant-a", "user-a")
        return {"history": legacy} if legacy else None
    async def list_datasets(user):
        assert user is USER
        if kb_error:
            raise RuntimeError("internal secret must not appear")
        return [{"dataset_id": item} for item in visible]
    async def authorize_documents(user, dataset_id, document_ids):
        assert user.user_id == "user-a"
        if kb_error:
            raise RuntimeError("internal secret must not appear")
        permitted = visible_docs if visible_docs is not None else document_ids
        return set(document_ids) & set(permitted)
    async def authorize_document_sources(user, dataset_id, references):
        assert user.user_id == "user-a"
        if kb_error:
            raise RuntimeError("internal secret must not appear")
        requested = {
            (item["document_id"], item["source_version"], item["source_hash"])
            for item in references
        }
        permitted = visible_versions if visible_versions is not None else requested
        return requested & set(permitted)
    session = Session(session_id="session-a", user_id=USER.user_id, tenant_id=USER.tenant_id)

    async def get_session(session_id):
        return session if session_id == session.session_id else None

    state = SimpleNamespace(
        session_manager=SimpleNamespace(get=get_session),
        database=SimpleNamespace(fetch=fetch, fetchrow=fetchrow),
        kb_proxy=SimpleNamespace(
            list_datasets=list_datasets, authorize_documents=authorize_documents,
            authorize_document_sources=authorize_document_sources,
        ),
        agent_runtime_control=SimpleNamespace(
            stream_thread_events=AsyncMock(), start_turn=AsyncMock(),
            get_approval=AsyncMock(), decide_approval=AsyncMock(return_value={"status": "rejected"}),
        ),
    )
    return Request({"type": "http", "headers": [], "app": SimpleNamespace(state=state)})


def snapshot(run_id, dataset_ids):
    return {"run_id": run_id, "snapshot": {"readonly_capabilities": {"items": [
        {"kind": "knowledge", "payload": {"dataset_id": item}} for item in dataset_ids
    ]}}}


@pytest.mark.asyncio
async def test_source_influence_follows_later_turns_but_not_earlier_public_turns():
    req = request(snapshots=[snapshot("public", []), snapshot("private", ["private-a"]), snapshot("later-off", [])])
    sources = await conversation_sources(req, USER, "session-a")
    assert sources.inherited_by_run == {"public": frozenset(), "private": frozenset({"private-a"}), "later-off": frozenset({"private-a"})}


@pytest.mark.asyncio
async def test_legacy_context_is_inherited_and_string_snapshots_are_supported():
    req = request(legacy=json.dumps({"messages": [{"role": "assistant", "metadata": {"contexts": [{"dataset_id": "legacy-a"}]}}]}), snapshots=[{"run_id": "later", "snapshot": json.dumps(snapshot("unused", ["private-a"])["snapshot"])}])
    sources = await conversation_sources(req, USER, "session-a")
    assert sources.inherited_by_run["later"] == frozenset({"legacy-a", "private-a"})


@pytest.mark.asyncio
async def test_legacy_context_uses_chunk_dataset_when_outer_dataset_is_missing():
    legacy = {"messages": [{"role": "assistant", "metadata": {"contexts": [{
        "chunks": [{"dataset_id": "private-a", "document_id": "doc-a", "content": "Private source"}],
    }]}}]}
    req = request(snapshots=[], visible=["private-a"], visible_docs=[], legacy=legacy)
    with pytest.raises(HTTPException) as denied:
        await require_conversation_source_access(req, USER, "session-a")
    assert denied.value.status_code == 403


@pytest.mark.asyncio
@pytest.mark.parametrize("kb_error", [False, True])
async def test_revocation_and_authority_outage_fail_closed(kb_error):
    with pytest.raises(HTTPException) as caught:
        await require_conversation_source_access(request(kb_error=kb_error), USER, "session-a")
    assert caught.value.status_code == 403
    assert caught.value.detail["code"] == "ASSISTANT_SOURCE_ACCESS_REVOKED"
    assert "secret" not in json.dumps(caught.value.detail)


@pytest.mark.asyncio
async def test_no_knowledge_and_current_authorized_sources_keep_working():
    await require_conversation_source_access(request(snapshots=[], kb_error=True), USER, "session-a")
    await require_conversation_source_access(request(visible=["private-a"]), USER, "session-a")


def document_context(run_id="run-a", document_id="doc-a"):
    return {"run_id": run_id, "chunks": [{
        "dataset_id": "private-a", "document_id": document_id,
        "content": "Private source text",
    }]}


SOURCE_HASH = "a" * 64


def versioned_context(run_id="run-a", document_id="doc-a", *, source_version=1, source_hash=SOURCE_HASH):
    context = document_context(run_id, document_id)
    context["chunks"][0]["source_version"] = source_version
    context["chunks"][0]["source_hash"] = source_hash
    return context


@pytest.mark.asyncio
async def test_versioned_source_requires_exact_historical_identity_in_addition_to_live_document_acl():
    context = versioned_context()
    req = request(visible=["private-a"], visible_docs=["doc-a"], contexts=[context])
    sources = await conversation_sources(req, USER, "session-a")
    expected = frozenset({("private-a", "doc-a", 1, SOURCE_HASH)})
    assert sources.source_versions == expected
    assert sources.versions_by_run == {"run-a": expected, "run-b": expected}
    await require_conversation_source_access(req, USER, "session-a")
    req.app.state.kb_proxy.authorize_document_sources = AsyncMock(return_value=set())
    with pytest.raises(HTTPException) as denied:
        await require_conversation_source_access(req, USER, "session-a")
    assert denied.value.status_code == 403


@pytest.mark.asyncio
async def test_partial_version_identity_fails_closed_while_legacy_event_uses_document_acl():
    legacy = request(
        visible=["private-a"], visible_docs=["doc-a"],
        contexts=[document_context()],
    )
    await require_conversation_source_access(legacy, USER, "session-a")
    partial = versioned_context(source_hash=None)
    req = request(visible=["private-a"], visible_docs=["doc-a"], contexts=[partial])
    with pytest.raises(HTTPException) as denied:
        await require_conversation_source_access(req, USER, "session-a")
    assert denied.value.status_code == 403


@pytest.mark.asyncio
async def test_null_top_level_identity_uses_metadata_and_null_pair_remains_legacy():
    exact = document_context()
    exact["chunks"][0].update({
        "document_id": None, "source_version": None, "source_hash": None,
        "metadata": {
            "document_id": "doc-a", "source_version": 2, "source_hash": SOURCE_HASH,
        },
    })
    req = request(visible=["private-a"], visible_docs=["doc-a"], contexts=[exact])
    assert (await conversation_sources(req, USER, "session-a")).source_versions == frozenset({
        ("private-a", "doc-a", 2, SOURCE_HASH),
    })
    await require_conversation_source_access(req, USER, "session-a")

    legacy = document_context()
    legacy["chunks"][0]["metadata"] = {
        "source_version": None, "source_hash": None,
    }
    legacy_req = request(visible=["private-a"], visible_docs=["doc-a"], contexts=[legacy])
    assert not (await conversation_sources(legacy_req, USER, "session-a")).source_versions
    await require_conversation_source_access(legacy_req, USER, "session-a")


@pytest.mark.asyncio
async def test_partial_metadata_identity_cannot_hide_behind_complete_top_level_identity():
    context = versioned_context()
    context["chunks"][0]["metadata"] = {"source_version": 1, "source_hash": None}
    req = request(visible=["private-a"], visible_docs=["doc-a"], contexts=[context])
    with pytest.raises(HTTPException) as denied:
        await require_conversation_source_access(req, USER, "session-a")
    assert denied.value.status_code == 403


@pytest.mark.asyncio
@pytest.mark.parametrize("malformed", ["boolean_metadata_version", "numeric_hash", "padded_hash"])
async def test_malformed_source_version_fields_fail_closed(malformed):
    context = versioned_context()
    chunk = context["chunks"][0]
    if malformed == "boolean_metadata_version":
        chunk["metadata"] = {"source_version": True, "source_hash": SOURCE_HASH}
    elif malformed == "numeric_hash":
        chunk["source_hash"] = int("1" * 64)
    else:
        chunk["source_hash"] = f" {SOURCE_HASH} "
    req = request(visible=["private-a"], visible_docs=["doc-a"], contexts=[context])
    with pytest.raises(HTTPException) as denied:
        await require_conversation_source_access(req, USER, "session-a")
    assert denied.value.status_code == 403


@pytest.mark.asyncio
async def test_same_dataset_document_revocation_blocks_history_and_derived_reads(monkeypatch):
    req = request(
        visible=["private-a"], visible_docs=[], contexts=[document_context()],
    )
    sources = await conversation_sources(req, USER, "session-a")
    assert sources.documents_by_run == {
        "run-a": frozenset({("private-a", "doc-a")}),
        "run-b": frozenset({("private-a", "doc-a")}),
    }
    with pytest.raises(HTTPException) as denied:
        await require_conversation_source_access(req, USER, "session-a")
    assert denied.value.status_code == 403
    owned = SimpleNamespace(
        session_id="session-a", runtime_thread_id=THREAD.runtime_thread_id,
        import_status="ready", last_sequence=9, source_kind="native",
        kernel_owner="agent_runtime",
    )
    monkeypatch.setattr(agent, "_get_thread", AsyncMock(return_value=owned))
    thread = await agent.get_thread(THREAD.runtime_thread_id, req, USER)
    assert thread["thread"]["restricted_source_run_ids"] == ["run-a", "run-b"]
    from src.services.assistant_entry.source_access import require_artifact_source_access
    with pytest.raises(HTTPException) as artifact_denied:
        await require_artifact_source_access(
            req, USER, SimpleNamespace(source="ai", session_id="session-a", created_at=None),
        )
    assert artifact_denied.value.status_code == 403


@pytest.mark.asyncio
async def test_orphan_context_event_still_restricts_its_runtime_run(monkeypatch):
    req = request(
        visible=["private-a"], visible_docs=[], snapshots=[],
        contexts=[document_context()],
    )
    sources = await conversation_sources(req, USER, "session-a")
    assert sources.documents_by_run == {"run-a": frozenset({("private-a", "doc-a")})}
    owned = SimpleNamespace(
        session_id="session-a", runtime_thread_id=THREAD.runtime_thread_id,
        import_status="ready", last_sequence=9, source_kind="native",
        kernel_owner="agent_runtime",
    )
    monkeypatch.setattr(agent, "_get_thread", AsyncMock(return_value=owned))
    thread = await agent.get_thread(THREAD.runtime_thread_id, req, USER)
    assert thread["thread"]["restricted_source_run_ids"] == ["run-a"]


@pytest.mark.asyncio
async def test_runless_context_event_cannot_leave_earlier_run_unrestricted(monkeypatch):
    from datetime import datetime, timedelta, timezone

    start = datetime.now(timezone.utc)
    admitted = snapshot("run-a", ["private-a"])
    admitted["created_at"] = start
    context = document_context()
    context["run_id"] = None
    context["created_at"] = start + timedelta(minutes=1)
    req = request(
        visible=["private-a"], visible_docs=[], snapshots=[admitted], contexts=[context],
    )
    owned = SimpleNamespace(
        session_id="session-a", runtime_thread_id=THREAD.runtime_thread_id,
        import_status="ready", last_sequence=9, source_kind="native",
        kernel_owner="agent_runtime",
    )
    monkeypatch.setattr(agent, "_get_thread", AsyncMock(return_value=owned))
    thread = await agent.get_thread(owned.runtime_thread_id, req, USER)
    assert thread["thread"]["restricted_source_run_ids"] == ["run-a"]


@pytest.mark.asyncio
async def test_later_orphan_run_inherits_earlier_source_without_retroactive_mask(monkeypatch):
    from datetime import datetime, timedelta, timezone

    from src.services.assistant_entry.source_access import source_documents_at_creation

    start = datetime.now(timezone.utc)
    first = snapshot("run-a", ["private-a"])
    first["created_at"] = start
    context_a = document_context("run-a", "doc-a")
    context_a["created_at"] = start + timedelta(minutes=1)
    context_b = document_context("run-b", "doc-b")
    context_b["created_at"] = start + timedelta(minutes=3)
    req = request(
        visible=["private-a"], visible_docs=["doc-a"],
        snapshots=[first], contexts=[context_a, context_b],
    )
    sources = await conversation_sources(req, USER, "session-a")
    assert sources.documents_by_run == {
        "run-a": frozenset({("private-a", "doc-a")}),
        "run-b": frozenset({("private-a", "doc-a"), ("private-a", "doc-b")}),
    }
    assert source_documents_at_creation(
        sources, start + timedelta(minutes=2),
    ) == frozenset({("private-a", "doc-a")})
    owned = SimpleNamespace(
        session_id="session-a", runtime_thread_id=THREAD.runtime_thread_id,
        import_status="ready", last_sequence=9, source_kind="native",
        kernel_owner="agent_runtime",
    )
    monkeypatch.setattr(agent, "_get_thread", AsyncMock(return_value=owned))
    thread = await agent.get_thread(THREAD.runtime_thread_id, req, USER)
    assert thread["thread"]["restricted_source_run_ids"] == ["run-b"]
    req.app.state.kb_proxy.authorize_documents = AsyncMock(return_value={"doc-b"})
    thread = await agent.get_thread(THREAD.runtime_thread_id, req, USER)
    assert thread["thread"]["restricted_source_run_ids"] == ["run-a", "run-b"]


@pytest.mark.asyncio
async def test_document_rights_recheck_allows_other_active_document():
    req = request(
        visible=["private-a"], visible_docs=["doc-a"], contexts=[document_context()],
    )
    await require_conversation_source_access(req, USER, "session-a")
    req.app.state.kb_proxy.authorize_documents = AsyncMock(return_value=set())
    with pytest.raises(HTTPException):
        await require_conversation_source_access(req, USER, "session-a")


@pytest.mark.asyncio
async def test_malformed_persisted_context_fails_closed() -> None:
    req = request(
        visible=["private-a"], visible_docs=["doc-a"],
        contexts=[{"run_id": "run-a", "chunks": None}],
    )
    with pytest.raises(HTTPException) as denied:
        await require_conversation_source_access(req, USER, "session-a")
    assert denied.value.status_code == 403
    assert denied.value.detail["code"] == "ASSISTANT_SOURCE_ACCESS_REVOKED"


@pytest.mark.asyncio
async def test_new_context_event_rechecks_document_before_text_delta():
    req = request(visible=["private-a"], contexts=[], visible_docs=[])
    guard = ConversationEventGuard(req, USER, await conversation_sources(req, USER, "session-a"), session_id="session-a")
    event = {"sequence": 1, "event_type": "context_retrieved", "data": {
        "run_id": "run-a", "chunks": document_context()["chunks"],
    }}
    projected = await guard.project(event)
    assert projected["data"]["source_access_revoked"] is True
    delta = await guard.project({
        "sequence": 2, "event_type": "text_delta",
        "data": {"run_id": "run-a", "content": "Private source text"},
    })
    assert "Private source text" not in json.dumps(delta)


@pytest.mark.asyncio
async def test_skipped_context_event_rechecks_persisted_source_before_delta():
    contexts = []
    req = request(visible=["private-a"], visible_docs=[], contexts=contexts)
    guard = ConversationEventGuard(req, USER, await conversation_sources(req, USER, "session-a"), session_id="session-a")
    started = await guard.project({
        "sequence": 1, "event_type": "run_started", "data": {"run_id": "run-a"},
    })
    assert started["event_type"] == "run_started"
    contexts.append(document_context())  # Sequence 2 was excluded by the client's cursor.
    delta = await guard.project({
        "sequence": 3, "event_type": "text_delta",
        "data": {"run_id": "run-a", "content": "Private source text"},
    })
    assert delta["data"]["source_access_revoked"] is True
    assert "Private source text" not in json.dumps(delta)


@pytest.mark.asyncio
async def test_open_stream_document_revocation_blocks_next_delta():
    req = request(
        visible=["private-a"], visible_docs=["doc-a"],
        contexts=[document_context()],
    )
    guard = ConversationEventGuard(req, USER, await conversation_sources(req, USER, "session-a"), session_id="session-a")
    raw = {"sequence": 1, "event_type": "text_delta", "data": {
        "run_id": "run-a", "content": "Private source text",
    }}
    assert (await guard.project(raw))["data"]["content"] == "Private source text"
    req.app.state.kb_proxy.authorize_documents = AsyncMock(return_value=set())
    revoked = await guard.project({**raw, "sequence": 2})
    assert revoked["data"]["source_access_revoked"] is True
    assert "Private source text" not in json.dumps(revoked)


@pytest.mark.asyncio
async def test_open_stream_rechecks_dataset_revocation_on_next_delta():
    req = request(visible=["private-a"], contexts=[])
    guard = ConversationEventGuard(req, USER, await conversation_sources(req, USER, "session-a"), session_id="session-a")
    raw = {"sequence": 1, "event_type": "text_delta", "data": {
        "run_id": "run-a", "content": "Knowledge-derived answer",
    }}
    assert (await guard.project(raw))["data"]["content"] == "Knowledge-derived answer"
    req.app.state.kb_proxy.list_datasets = AsyncMock(return_value=[])
    revoked = await guard.project({**raw, "sequence": 2})
    assert revoked["data"]["source_access_revoked"] is True
    assert "Knowledge-derived answer" not in json.dumps(revoked)


@pytest.mark.asyncio
async def test_quiz_share_source_ids_fail_closed_when_context_document_has_no_dataset():
    req = request(
        snapshots=[snapshot("run-a", [])],
        contexts=[{"run_id": "run-a", "chunks": [{"document_id": "doc-a", "content": "Private text"}]}],
    )
    original_fetchrow = req.app.state.database.fetchrow

    async def fetchrow(query, *args):
        if "FROM assistant.quizzes q" in query:
            return {"dataset_ids": [], "created_by": "user-a", "session_id": "session-a", "run_id": "run-a"}
        return await original_fetchrow(query, *args)

    req.app.state.database.fetchrow = fetchrow
    with pytest.raises(HTTPException) as denied:
        await quiz_source_ids(req, UUID(int=1), USER.tenant_id, require_origin=True)
    assert denied.value.status_code == 409


@pytest.mark.asyncio
async def test_revoked_event_cursor_keeps_only_safe_terminal_and_cursor(monkeypatch):
    req = request()
    monkeypatch.setattr(agent, "_get_thread", AsyncMock(return_value=THREAD))
    async def events(**kw):
        for seq, kind, data in [(1, "text_delta", {"content": "Private source"}), (2, "run_finished", {"status": "succeeded"})]:
            yield {"sequence": seq, "event_type": kind, "data": {"run_id": "run-a", **data}}
    req.app.state.agent_runtime_control.stream_thread_events = events
    result = await agent.thread_events(THREAD.runtime_thread_id, req, after_sequence=0, limit=100, turn_id=None, user=USER)
    body = b"".join([chunk async for chunk in result.body_iterator])
    assert b"Private" not in body
    assert b'"sequence":1' in body and b'"sequence":2' in body
    assert b'"source_access_revoked":true' in body
    assert b'"status":"succeeded"' in body


@pytest.mark.asyncio
async def test_revoked_run_start_does_not_reinject_snapshot_settings(monkeypatch):
    req = request()
    monkeypatch.setattr(agent, "_get_thread", AsyncMock(return_value=THREAD))
    monkeypatch.setattr(agent, "_store", lambda _request: SimpleNamespace(
        turn_metadata=AsyncMock(return_value={
            "effective_reasoning_option": "PRIVATE_REASONING_SENTINEL",
            "model_id": "PRIVATE_MODEL_SENTINEL",
        }),
    ))

    async def events(**_kwargs):
        yield {"sequence": 1, "event_type": "run_started", "data": {"run_id": "run-a"}}

    req.app.state.agent_runtime_control.stream_thread_events = events
    result = await agent.thread_events(THREAD.runtime_thread_id, req, after_sequence=0, limit=100, turn_id="run-a", user=USER)
    body = b"".join([chunk async for chunk in result.body_iterator])
    assert b'"sequence":1' in body and b'"source_access_revoked":true' in body
    assert b"PRIVATE_REASONING_SENTINEL" not in body
    assert b"PRIVATE_MODEL_SENTINEL" not in body


@pytest.mark.asyncio
@pytest.mark.parametrize("entry", ["v1", "v2"])
async def test_turn_with_knowledge_off_cannot_resample_revoked_history(entry, monkeypatch):
    req = request()
    monkeypatch.setattr(agent, "_get_thread", AsyncMock(return_value=THREAD))
    with pytest.raises(HTTPException) as caught:
        if entry == "v1":
            await chat._start_agent_runtime_turn(req, USER, AssistantChatRequest(message="Repeat earlier source", kb_mode="off"), session_id="session-a", model_id="qwen3.8-flash")
        else:
            await agent.create_turn(THREAD.runtime_thread_id, agent.TurnCreateRequest(message="Repeat earlier source", kb_mode="off"), req, USER)
    assert caught.value.status_code == 409
    req.app.state.agent_runtime_control.start_turn.assert_not_called()


@pytest.mark.asyncio
async def test_revoked_approval_cannot_be_read_or_approved_but_can_be_rejected(monkeypatch):
    req = request()
    monkeypatch.setattr(agent, "_get_thread", AsyncMock(return_value=THREAD))
    req.app.state.agent_runtime_control.get_approval.return_value = {"approval_id": "approval-a", "status": "pending", "arguments": {"secret": "Private source"}}
    summary = await agent.get_thread_approval(THREAD.runtime_thread_id, "approval-a", req, USER)
    assert summary["approval"]["status"] == "pending"
    assert summary["preview"]["can_approve"] is False
    assert "Private" not in json.dumps(summary)
    with pytest.raises(HTTPException):
        await agent.decide_thread_approval(THREAD.runtime_thread_id, "approval-a", agent.ApprovalDecisionRequest(approved=True), req, USER)
    req.app.state.agent_runtime_control.get_approval.assert_awaited_once()
    result = await agent.decide_thread_approval(THREAD.runtime_thread_id, "approval-a", agent.ApprovalDecisionRequest(approved=False), req, USER)
    assert result["approval"]["status"] == "rejected"
    req.app.state.agent_runtime_control.decide_approval.assert_awaited_once()


@pytest.mark.asyncio
@pytest.mark.parametrize("handler", [artifacts.get_artifact, artifacts.download_artifact])
async def test_revoked_derived_artifact_denies_before_content_or_storage_url(handler, monkeypatch):
    req = request()
    storage = AsyncMock()
    storage.get_artifact.return_value = SimpleNamespace(artifact_id="art_a", session_id="session-a", tenant_id=USER.tenant_id, user_id=USER.user_id)
    monkeypatch.setattr(artifacts, "get_artifact_storage", lambda: storage)
    with pytest.raises(HTTPException) as caught:
        await handler("art_a", req, USER)
    assert caught.value.status_code == 403
    storage.get_presigned_download_url.assert_not_awaited()
    storage.download_artifact.assert_not_awaited()


@pytest.mark.asyncio
async def test_open_event_stream_rechecks_current_rights_before_later_delivery():
    req = request(visible=["private-a"])
    guard = ConversationEventGuard(req, USER, await conversation_sources(req, USER, "session-a"), session_id="session-a")
    raw = {"sequence": 1, "event_type": "text_delta", "data": {"run_id": "run-a", "content": "Private source"}}
    assert (await guard.project(raw))["data"]["content"] == "Private source"
    req.app.state.kb_proxy.list_datasets = AsyncMock(return_value=[])
    second = await guard.project({**raw, "sequence": 2})
    assert second["sequence"] == 2 and second["data"]["source_access_revoked"]
    assert "Private" not in json.dumps(second)
    terminal = await guard.project({"sequence": 3, "event_type": "run_error", "data": {"run_id": "run-a", "status": "failed", "error": "Private exception"}})
    assert terminal["data"]["status"] == "failed" and "Private" not in json.dumps(terminal)


@pytest.mark.asyncio
async def test_explicit_recover_cannot_restart_with_revoked_sources(monkeypatch):
    req = request()
    monkeypatch.setattr(agent, "_get_thread", AsyncMock(return_value=THREAD))
    req.app.state.agent_runtime_control.recover_turn = AsyncMock()
    with pytest.raises(HTTPException) as caught:
        await agent.recover_turn(THREAD.runtime_thread_id, "run-b", req, USER)
    assert caught.value.status_code == 409
    req.app.state.agent_runtime_control.recover_turn.assert_not_called()


@pytest.mark.asyncio
async def test_cloud_artifact_download_stays_on_authenticated_gateway(monkeypatch):
    req = request(visible=["private-a"])
    storage = AsyncMock()
    storage.get_artifact.return_value = SimpleNamespace(artifact_id="art_a", session_id="session-a", tenant_id=USER.tenant_id, user_id=USER.user_id, filename="real.txt", mime_type="text/plain")
    storage.get_presigned_download_url.return_value = "https://storage.example/file?signature=private"
    storage.download_artifact.return_value = b"real artifact bytes"
    monkeypatch.setattr(artifacts, "get_artifact_storage", lambda: storage)
    result = await artifacts.download_artifact("art_a", req, USER)
    assert result.status_code == 200
    assert "location" not in result.headers
    assert b"".join([chunk async for chunk in result.body_iterator]) == b"real artifact bytes"
    storage.get_presigned_download_url.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("entry", ["get", "submit", "result", "attempts"])
async def test_quiz_reads_and_grading_deny_revoked_inherited_sources(entry, monkeypatch):
    req = request()
    original_fetchrow = req.app.state.database.fetchrow
    async def fetchrow(query, *args):
        if "FROM assistant.quizzes q" in query:
            assert args == (UUID(int=1), "tenant-a")
            return {"dataset_ids": [], "created_by": "user-a", "session_id": "session-a", "run_id": "run-b"}
        return await original_fetchrow(query, *args)
    req.app.state.database.fetchrow = fetchrow
    service = AsyncMock()
    monkeypatch.setattr(quiz_routes, "_get_quiz_service", lambda _req: service)
    with pytest.raises(HTTPException) as caught:
        if entry == "get":
            await quiz_routes.get_quiz(UUID(int=1), req, USER)
        elif entry == "submit":
            await quiz_routes.submit_quiz(UUID(int=1), quiz_routes.QuizSubmitRequest(answers={}), req, USER)
        elif entry == "result":
            await quiz_routes.get_attempt_result(UUID(int=1), UUID(int=2), req, USER)
        else:
            await quiz_routes.list_attempts(UUID(int=1), req, limit=100, offset=0, user=USER)
    assert caught.value.status_code == 403
    assert service.mock_calls == []


@pytest.mark.asyncio
async def test_quiz_checks_current_reader_rights_for_creator_direct_sources():
    req = request(visible=["private-a"], snapshots=[])
    req.app.state.database.fetchrow = AsyncMock(return_value={"dataset_ids": '["private-a"]', "created_by": "someone-else", "session_id": None, "run_id": None})
    await quiz_routes._require_quiz_source_access(UUID(int=1), req, USER)
    req.app.state.kb_proxy.list_datasets = AsyncMock(return_value=[])
    with pytest.raises(HTTPException) as caught:
        await quiz_routes._require_quiz_source_access(UUID(int=1), req, USER)
    assert caught.value.status_code == 403


@pytest.mark.asyncio
async def test_quiz_origin_document_revocation_denies_when_dataset_still_visible():
    req = request(
        visible=["private-a"], visible_docs=[], contexts=[document_context()],
    )
    original_fetchrow = req.app.state.database.fetchrow

    async def fetchrow(query, *args):
        if "FROM assistant.quizzes q" in query:
            return {"dataset_ids": '["private-a"]', "created_by": "user-a",
                    "session_id": "session-a", "run_id": "run-a"}
        return await original_fetchrow(query, *args)

    req.app.state.database.fetchrow = fetchrow
    with pytest.raises(HTTPException) as denied:
        await quiz_routes._require_quiz_source_access(UUID(int=1), req, USER)
    assert denied.value.status_code == 403


@pytest.mark.asyncio
async def test_background_model_source_check_uses_current_identity_not_recovered_roles():
    from src.services.assistant_entry.source_access import runtime_source_access_checker
    req = request()
    req.app.state.database.get_user = AsyncMock(return_value={"status": "active", "tenant_id": "tenant-a", "roles": ["user"], "tier": "normal"})
    req.app.state.kb_proxy.list_datasets = AsyncMock(return_value=[{"dataset_id": "private-a"}])
    check = runtime_source_access_checker(req.app)
    assert await check(tenant_id="tenant-a", user_id="user-a", session_id="session-a", run_id="run-b") is True
    actor = req.app.state.kb_proxy.list_datasets.call_args.args[0]
    assert actor.roles == ["user"] and actor.tier == "normal"
    req.app.state.kb_proxy.list_datasets.return_value = []
    assert await check(tenant_id="tenant-a", user_id="user-a", session_id="session-a", run_id="run-b") is False


@pytest.mark.asyncio
async def test_background_model_recovery_refuses_revoked_document_inside_visible_dataset():
    from src.services.assistant_entry.source_access import runtime_source_access_checker

    req = request(
        visible=["private-a"], visible_docs=[], contexts=[document_context()],
    )
    req.app.state.database.get_user = AsyncMock(return_value={
        "status": "active", "tenant_id": "tenant-a", "roles": ["user"], "tier": "normal",
    })
    check = runtime_source_access_checker(req.app)
    assert not await check(
        tenant_id="tenant-a", user_id="user-a", session_id="session-a", run_id="run-b",
    )
    req.app.state.database.get_user.return_value["status"] = "disabled"
    assert await check(tenant_id="tenant-a", user_id="user-a", session_id="session-a", run_id="run-b") is False


@pytest.mark.asyncio
async def test_artifact_before_later_knowledge_and_uploaded_original_stay_available():
    from datetime import datetime, timedelta, timezone

    from src.services.assistant_entry.source_access import require_artifact_source_access
    start = datetime.now(timezone.utc)
    public, private = snapshot('public', []), snapshot('private', ['private-a'])
    public['created_at'], private['created_at'] = start, start + timedelta(minutes=2)
    req = request(snapshots=[public, private])
    artifact = SimpleNamespace(session_id='session-a', source='ai', created_at=start + timedelta(seconds=20))
    await require_artifact_source_access(req, USER, artifact)
    artifact.created_at = start + timedelta(minutes=3)
    with pytest.raises(HTTPException) as caught:
        await require_artifact_source_access(req, USER, artifact)
    assert caught.value.status_code == 403
    artifact.source = 'user'
    await require_artifact_source_access(req, USER, artifact)


@pytest.mark.asyncio
async def test_same_run_artifact_before_retrieval_does_not_inherit_later_document():
    from datetime import datetime, timedelta, timezone

    from src.services.assistant_entry.source_access import require_artifact_source_access

    start = datetime.now(timezone.utc)
    admitted = snapshot("run-a", ["private-a"])
    admitted["created_at"] = start
    context = document_context()
    context["created_at"] = start + timedelta(minutes=2)
    req = request(
        visible=["private-a"], visible_docs=[],
        snapshots=[admitted], contexts=[context],
    )
    artifact = SimpleNamespace(
        session_id="session-a", source="ai", created_at=start + timedelta(minutes=1),
    )
    await require_artifact_source_access(req, USER, artifact)
    artifact.created_at = start + timedelta(minutes=3)
    with pytest.raises(HTTPException) as denied:
        await require_artifact_source_access(req, USER, artifact)
    assert denied.value.status_code == 403


@pytest.mark.asyncio
async def test_whole_thread_stream_tracks_sources_added_after_open():
    snapshots = [snapshot('public', [])]
    req = request(snapshots=snapshots)
    guard = ConversationEventGuard(req, USER, await conversation_sources(req, USER, 'session-a'), session_id='session-a')
    public = {'sequence': 1, 'event_type': 'text_delta', 'data': {'run_id': 'public', 'content': 'ordinary'}}
    assert (await guard.project(public))['data']['content'] == 'ordinary'
    snapshots.append(snapshot('new-private', ['private-a']))
    private = {'sequence': 2, 'event_type': 'text_delta', 'data': {'run_id': 'new-private', 'content': 'Private newly selected source'}}
    projected = await guard.project(private)
    assert projected['sequence'] == 2 and projected['data']['source_access_revoked']
    assert 'Private' not in json.dumps(projected)
    # The later denied source does not retroactively hide an earlier public run.
    assert (await guard.project(public))['data']['content'] == 'ordinary'


@pytest.mark.asyncio
async def test_thread_read_reports_restricted_run_ids_without_private_content(monkeypatch):
    req = request(snapshots=[snapshot('public', []), snapshot('private', ['private-a']), snapshot('inherited', [])])
    owned = SimpleNamespace(session_id='session-a', runtime_thread_id='00000000-0000-0000-0000-000000000001', import_status='ready', last_sequence=9, source_kind='native', kernel_owner='agent_runtime')
    monkeypatch.setattr(agent, '_get_thread', AsyncMock(return_value=owned))
    result = await agent.get_thread(owned.runtime_thread_id, req, USER)
    assert result['thread']['restricted_source_run_ids'] == ['private', 'inherited']
    assert 'private-a' not in json.dumps(result)


@pytest.mark.asyncio
@pytest.mark.parametrize("entry", ["thread", "turn", "recover", "approval", "approve"])
async def test_runtime_source_routes_fail_closed_without_session_storage(entry, monkeypatch):
    req = request(visible=["private-a"])
    req.app.state.session_manager = None
    monkeypatch.setattr(agent, "_get_thread", AsyncMock(return_value=THREAD))
    control = req.app.state.agent_runtime_control
    control.recover_turn = AsyncMock()
    with pytest.raises(HTTPException) as caught:
        if entry == "thread":
            await agent.get_thread(THREAD.runtime_thread_id, req, USER)
        elif entry == "turn":
            await agent.create_turn(
                THREAD.runtime_thread_id, agent.TurnCreateRequest(message="Continue"), req, USER,
            )
        elif entry == "recover":
            await agent.recover_turn(THREAD.runtime_thread_id, "run-b", req, USER)
        elif entry == "approval":
            await agent.get_thread_approval(THREAD.runtime_thread_id, "approval-a", req, USER)
        else:
            await agent.decide_thread_approval(
                THREAD.runtime_thread_id, "approval-a",
                agent.ApprovalDecisionRequest(approved=True), req, USER,
            )
    assert caught.value.status_code == 503
    assert caught.value.detail == {"code": "SESSION_STORAGE_UNAVAILABLE"}
    control.start_turn.assert_not_awaited()
    control.recover_turn.assert_not_awaited()
    control.get_approval.assert_not_awaited()
    control.decide_approval.assert_not_awaited()
