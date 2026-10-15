from __future__ import annotations

import json
from datetime import datetime, timezone

import pytest

from src.services.agent_runtime.thread_store import (
    AgentThreadStore,
    ThreadStoreError,
    _project_knowledge_contexts,
)


def test_context_projection_preserves_exact_version_and_old_excerpt() -> None:
    source_hash = "a" * 64
    events = [{"event_type": "context_retrieved", "data": {"chunks": [
        {"dataset_id": "visible", "document_id": "doc-a", "content": "new excerpt",
         "source_version": None, "metadata": {"source_version": 2, "source_hash": source_hash}},
        {"dataset_id": "visible", "document_id": "doc-b", "content": "old excerpt"},
    ]}}]
    contexts = _project_knowledge_contexts(events, {"visible": "Visible KB"})
    new_meta, old_meta = [chunk["metadata"] for chunk in contexts[0]["chunks"]]
    assert (new_meta["source_version"], new_meta["source_hash"]) == (2, source_hash)
    assert new_meta["document_id"] == "doc-a"
    assert "source_version" not in old_meta and "source_hash" not in old_meta


@pytest.mark.asyncio
async def test_retrieved_context_restores_only_for_currently_visible_dataset() -> None:
    class _ContextDB:
        async def fetch(self, query: str, *_args):
            if "FROM assistant_runtime_snapshots AS s" in query:
                return []
            if "assistant_capability_executions" in query:
                return []
            assert "compat/v1/context_retrieved" in query
            return [{
                "role": "assistant", "content": "Answer", "sequence": 3,
                "run_id": "run-a", "created_at": datetime.now(timezone.utc),
                "thinking_content": None, "tool_calls": None, "tool_results": None,
                "runtime_events": [{"event_type": "context_retrieved", "data": {"chunks": [
                    {"dataset_id": "visible", "document_id": "doc-a", "content": "Allowed excerpt", "score": 0.8},
                    {"dataset_id": "revoked", "document_id": "doc-b", "content": "Private excerpt", "score": 0.9},
                ]}}],
                "total": 1,
            }]

    store = AgentThreadStore(_ContextDB())
    scope = {
        "tenant_id": "tenant-a", "user_id": "user-a",
        "runtime_thread_id": "00000000-0000-0000-0000-000000000001", "limit": 10,
    }
    messages, _ = await store.history_messages(
        **scope, allowed_datasets={"visible": "Visible KB", "revoked": "Other KB"},
    )
    assert messages[0]["metadata"]["contexts"][0]["chunks"][0]["content"] == "Allowed excerpt"
    assert messages[0]["content"] == "Answer"
    restricted, _ = await store.history_messages(**scope, allowed_datasets={"visible": "Visible KB"})
    assert restricted[0]["content"] == ""
    assert restricted[0]["metadata"]["source_access_revoked"] is True
    assert "Private excerpt" not in json.dumps(restricted)
    assert "Allowed excerpt" not in json.dumps(restricted)
    denied, _ = await store.history_messages(**scope)
    assert denied[0]["content"] == ""
    assert "Allowed excerpt" not in json.dumps(denied)
    assert "context_retrieved" not in json.dumps(denied)


@pytest.mark.asyncio
async def test_partial_delta_fallback_redacts_revoked_knowledge_source() -> None:
    class _PartialDB:
        async def fetch(self, query: str, *_args):
            if "assistant_capability_executions" in query:
                return []
            # Both agent_message and text_delta fallback must carry source
            # events; otherwise a partial failed answer escapes revocation.
            assert query.count("'compat/v1/context_retrieved'") == 2
            return [{
                "role": "assistant", "content": "Private partial answer",
                "sequence": 7, "run_id": "run-partial", "created_at": datetime.now(timezone.utc),
                "thinking_content": "Private reasoning", "tool_calls": None, "tool_results": None,
                "runtime_events": [
                    {"event_type": "context_retrieved", "data": {"chunks": [
                        {"dataset_id": "revoked", "content": "Private source text"},
                    ]}},
                    {"event_type": "run_error", "data": {"status": "failed"}},
                ],
                "total": 1,
            }]

    messages, _ = await AgentThreadStore(_PartialDB()).history_messages(
        tenant_id="tenant-a", user_id="user-a",
        runtime_thread_id="00000000-0000-0000-0000-000000000001", limit=10,
        allowed_datasets={},
    )
    assert messages[0]["content"] == ""
    assert messages[0]["metadata"]["source_access_revoked"] is True
    assert messages[0]["metadata"]["process_summary"]["status"] == "failed"
    assert "Private" not in json.dumps(messages)


class _Database:
    def __init__(self) -> None:
        self.thread: dict[str, object] | None = None
        self.events: list[dict[str, object]] = []

    async def fetchrow(self, query: str, *args):
        if "FROM assistant_runtime_snapshots" in query:
            assert "snapshot->>'kernel_revision'" in query
            return {
                "snapshot": {
                    "kernel_revision": "kernel-1",
                    "reasoning": {"requested_option": "auto", "effective_option": "minimal"},
                },
                "capability_revision": 7,
                "kernel_revision": "kernel-1",
            }
        if "ensure_assistant_runtime_thread" in query:
            if self.thread is None:
                self.thread = {
                    "runtime_thread_id": args[0],
                    "tenant_id": args[1],
                    "user_id": args[2],
                    "session_id": args[3],
                    "kernel_owner": "agent",
                    "source_kind": "native",
                    "import_status": "not_required",
                    "last_sequence": 0,
                }
            return {"ok": True}
        if "import_assistant_legacy_session" in query:
            if self.thread and self.thread["import_status"] == "ready":
                return {"import_status": "ready"}
            self.thread = {
                "runtime_thread_id": args[0],
                "tenant_id": args[1],
                "user_id": args[2],
                "session_id": args[3],
                "kernel_owner": "agent",
                "source_kind": "legacy_import",
                "import_status": "ready",
                "last_sequence": 1,
            }
            return {"import_status": "ready"}
        if "FROM assistant_runtime_threads" in query and "session_id = $3" in query:
            return self.thread
        if "FROM assistant_runtime_threads" in query:
            return self.thread
        if "append_assistant_runtime_item" in query:
            self.events.append(
                {
                    "sequence": len(self.events) + 1,
                    "event_id": args[5],
                    "event_key": args[6],
                    "turn_id": args[7],
                    "item_id": args[8],
                    "event_type": args[9],
                    "item_type": args[10],
                    "status": args[11],
                    "payload": args[12],
                }
            )
            if self.thread:
                self.thread["last_sequence"] = len(self.events)
            return {"sequence": len(self.events)}
        raise AssertionError(query)

    async def fetch(self, _query: str, *_args):
        return self.events


@pytest.mark.asyncio
async def test_legacy_import_is_idempotent_and_scoped() -> None:
    db = _Database()
    store = AgentThreadStore(db)
    first = await store.import_legacy(
        tenant_id="tenant-a", user_id="user-a", session_id="session-a"
    )
    second = await store.import_legacy(
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        runtime_thread_id=first.runtime_thread_id,
    )
    assert first == second
    assert first.source_kind == "legacy_import"
    assert first.import_status == "ready"


@pytest.mark.asyncio
async def test_append_event_uses_stable_event_key_and_cursor() -> None:
    db = _Database()
    store = AgentThreadStore(db)
    thread = await store.ensure_native(
        tenant_id="tenant-a", user_id="user-a", session_id="session-a"
    )
    sequence = await store.append_event(
        thread=thread,
        event_key="turn-1:item-1",
        event_type="text",
        item_id="item-1",
        payload={"text": "ok"},
        status="completed",
    )
    assert sequence == 1
    assert (
        await store.events(
            tenant_id="tenant-a",
            user_id="user-a",
            runtime_thread_id=thread.runtime_thread_id,
            after_sequence=0,
            limit=10,
        )
    )[0]["event_key"] == "turn-1:item-1"


def test_thread_store_error_has_stable_http_contract() -> None:
    error = ThreadStoreError("AI_PLATFORM_AGENT_RUNTIME_IMPORT_IN_FLIGHT", status_code=409)
    assert error.code == "AI_PLATFORM_AGENT_RUNTIME_IMPORT_IN_FLIGHT"
    assert error.status_code == 409


@pytest.mark.asyncio
async def test_turn_metadata_reads_kernel_revision_from_immutable_snapshot() -> None:
    store = AgentThreadStore(_Database())
    metadata = await store.turn_metadata(
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        runtime_thread_id="00000000-0000-0000-0000-000000000001",
        turn_id="00000000-0000-0000-0000-000000000002",
    )
    assert metadata is not None
    assert metadata["kernel_revision"] == "kernel-1"


@pytest.mark.asyncio
async def test_history_effective_settings_use_only_owner_snapshot_and_skip_revoked_source() -> None:
    class _SnapshotDB:
        async def fetch(self, query: str, *args):
            assert "r.session_id = s.session_id" in query
            assert args[1:3] == ("tenant-a", "user-a")
            assert set(args[3]) == {"run-a"}
            return [{
                "run_id": "run-a", "model_id": "qwen3.8-flash", "temperature": 0.7,
                "reasoning": {"requested_option": "auto", "effective_option": "minimal", "secret": "PRIVATE"},
            }]

    messages = [
        {"role": "assistant", "metadata": {"runtime_run_id": "run-a", "runtime_events": [{"event_type": "run_finished"}]}},
        {"role": "assistant", "metadata": {"runtime_run_id": "run-revoked", "source_access_revoked": True}},
    ]
    await AgentThreadStore(_SnapshotDB())._attach_turn_parameters(
        messages, tenant_id="tenant-a", user_id="user-a",
        runtime_thread_id="00000000-0000-0000-0000-000000000001",
    )
    summary = messages[0]["metadata"]["process_summary"]
    assert (summary["model_id"], summary["temperature"]) == ("qwen3.8-flash", 0.7)
    assert summary["reasoning"]["effective_option"] == "minimal"
    assert "PRIVATE" not in json.dumps(messages)
    assert "process_summary" not in messages[1]["metadata"]


@pytest.mark.asyncio
async def test_history_messages_projects_runtime_rollout_in_chronological_order() -> None:
    now = datetime.now(timezone.utc)

    class _HistoryDatabase(_Database):
        async def fetch(self, query: str, *args):
            if "FROM assistant_runtime_snapshots AS s" in query:
                return []
            assert "FROM assistant_runs AS run" in query
            assert "item_type = 'event_msg'" in query
            assert "JOIN LATERAL" in query
            assert "delta.event_type IN" in query
            assert "jsonb_array_elements" in query
            assert args[1:] == ("tenant-a", "user-a", 10)
            return [
                {
                    "role": "assistant",
                    "content": "Hello",
                    "sequence": 2,
                    "run_id": "run-1",
                    "created_at": now,
                    "thinking_content": "Reasoning summary",
                    "tool_calls": [{"id": "call-1", "name": "search", "arguments": {}}],
                    "tool_results": [{"tool_call_id": "call-1", "name": "search", "result": "ok"}],
                    "runtime_events": [
                        {
                            "event_type": "subagent_started",
                            "data": {"agent_id": "child-a", "agent_type": "task"},
                            "timestamp": 1.0,
                        },
                        {
                            "event_type": "subagent_finished",
                            "data": {"agent_id": "child-a", "status": "completed"},
                            "timestamp": 2.0,
                        },
                        {
                            "event_type": "context_compaction",
                            "data": {"compacted": True},
                            "timestamp": 3.0,
                        },
                    ],
                    "total": 2,
                },
                {
                    "role": "user",
                    "content": "Hi",
                    "sequence": None,
                    "run_id": "run-1",
                    "created_at": now,
                    "thinking_content": None,
                    "tool_calls": None,
                    "tool_results": None,
                    "runtime_events": None,
                    "total": 2,
                },
            ]

    messages, total = await AgentThreadStore(_HistoryDatabase()).history_messages(
        tenant_id="tenant-a",
        user_id="user-a",
        runtime_thread_id="00000000-0000-0000-0000-000000000001",
        limit=10,
    )

    assert total == 2
    assert [(message["role"], message["content"]) for message in messages] == [
        ("user", "Hi"),
        ("assistant", "Hello"),
    ]
    assert messages[1]["metadata"]["thinking_content"] == "Reasoning summary"
    assert messages[1]["metadata"]["tool_calls"][0]["id"] == "call-1"
    assert messages[1]["metadata"]["tool_results"][0]["result"] == "ok"
    assert messages[1]["metadata"]["runtime_events"][0]["event_type"] == "subagent_started"
    assert messages[1]["metadata"]["process_summary"]["steps"][0]["id"] == "context-compaction"


@pytest.mark.asyncio
async def test_history_messages_preserves_empty_cancelled_turn() -> None:
    now = datetime.now(timezone.utc)

    class _CancelledHistoryDatabase(_Database):
        async def fetch(self, query: str, *args):
            if "FROM assistant_runtime_snapshots AS s" in query:
                return []
            assert "delta.event_type IN" in query
            assert "'compat/v1/cancelled'" in query
            assert "jsonb_array_elements" in query
            assert args[1:] == ("tenant-a", "user-a", 10)
            return [
                {
                    "role": "assistant",
                    "content": "",
                    "sequence": 3,
                    "run_id": "run-cancelled",
                    "created_at": now,
                    "thinking_content": "partial reasoning",
                    "tool_calls": None,
                    "tool_results": None,
                    "runtime_events": [
                        {
                            "event_type": "cancelled",
                            "data": {"status": "cancelled"},
                            "timestamp": 1.0,
                        },
                        {
                            "event_type": "run_error",
                            "data": {
                                "status": "cancelled",
                                "error_code": "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_ORPHANED",
                                "error": "private internal detail",
                            },
                            "timestamp": 2.0,
                        },
                    ],
                    "total": 1,
                }
            ]

    messages, total = await AgentThreadStore(_CancelledHistoryDatabase()).history_messages(
        tenant_id="tenant-a",
        user_id="user-a",
        runtime_thread_id="00000000-0000-0000-0000-000000000001",
        limit=10,
    )

    assert total == 1
    assert [(message["role"], message["content"]) for message in messages] == [("assistant", "")]
    assert messages[0]["metadata"]["thinking_content"] == "partial reasoning"
    assert messages[0]["metadata"]["process_summary"]["status"] == "cancelled"
    assert messages[0]["metadata"]["process_summary"]["terminal_reason"] == "runtime_restart_interrupted"
    assert "private internal detail" not in json.dumps(messages)


@pytest.mark.asyncio
async def test_history_messages_preserves_failed_and_uncertain_turns() -> None:
    now = datetime.now(timezone.utc)

    class _FailedHistoryDatabase(_Database):
        async def fetch(self, query: str, *args):
            if "FROM assistant_runtime_snapshots AS s" in query:
                return []
            # Both completed messages and fallback deltas must carry terminal facts.
            assert query.count("'compat/v1/run_error'") >= 3
            assert query.count("'compat/v1/side_effect_unknown'") >= 3
            assert args[1:] == ("tenant-a", "user-a", 10)
            return [
                {
                    "role": "assistant", "content": "Partial answer", "sequence": 5,
                    "run_id": "run-uncertain", "created_at": now,
                    "thinking_content": None, "tool_calls": None, "tool_results": None,
                    "runtime_events": [
                        {"event_type": "side_effect_unknown", "data": {}, "timestamp": 2.0},
                        {"event_type": "run_error", "data": {
                            "error": "private internal detail",
                            "terminal_envelope": {"exit_reason": "side_effect_unknown"},
                        }, "timestamp": 3.0},
                    ], "total": 2,
                },
                {
                    "role": "assistant", "content": "", "sequence": 2,
                    "run_id": "run-failed", "created_at": now,
                    "thinking_content": None, "tool_calls": None, "tool_results": None,
                    "runtime_events": [{"event_type": "run_error", "data": {
                        "error": "private internal detail", "status": "failed",
                    }, "timestamp": 1.0}], "total": 2,
                },
            ]

    messages, total = await AgentThreadStore(_FailedHistoryDatabase()).history_messages(
        tenant_id="tenant-a",
        user_id="user-a",
        runtime_thread_id="00000000-0000-0000-0000-000000000001",
        limit=10,
    )

    assert total == 2
    assert [(item["content"], item["metadata"]["process_summary"]["status"])
            for item in messages] == [("", "failed"), ("Partial answer", "failed")]
    assert messages[1]["metadata"]["process_summary"]["outcome_uncertain"] is True
    assert "error" not in messages[1]["metadata"]["process_summary"]
    assert "private internal detail" not in json.dumps(messages)


@pytest.mark.asyncio
async def test_history_messages_preserves_empty_successful_turn() -> None:
    now = datetime.now(timezone.utc)

    class _EmptySuccessDatabase(_Database):
        async def fetch(self, query: str, *args):
            if "FROM assistant_runtime_snapshots AS s" in query:
                return []
            assert query.count("'compat/v1/run_finished'") >= 3
            assert "'side_effect_unknown', 'run_finished', 'run_error', 'cancelled'" in query
            assert args[1:] == ("tenant-a", "user-a", 10)
            return [{
                "role": "assistant", "content": "", "sequence": 3,
                "run_id": "run-empty", "created_at": now,
                "thinking_content": None, "tool_calls": None, "tool_results": None,
                "runtime_events": [{"event_type": "run_finished", "data": {}, "timestamp": 1.0}],
                "total": 1,
            }]

    messages, total = await AgentThreadStore(_EmptySuccessDatabase()).history_messages(
        tenant_id="tenant-a",
        user_id="user-a",
        runtime_thread_id="00000000-0000-0000-0000-000000000001",
        limit=10,
    )

    assert total == 1
    assert messages[0]["content"] == ""
    assert messages[0]["metadata"]["process_summary"]["status"] == "succeeded"


@pytest.mark.asyncio
async def test_history_messages_strip_readonly_catalog_dumps() -> None:
    now = datetime.now(timezone.utc)

    class _HistoryDatabase(_Database):
        async def fetch(self, query: str, *args):
            del query, args
            return [
                {
                    "role": "assistant",
                    "content": (
                        "runtime-ok[AI_PLATFORM_READONLY_CONTEXT_V1]\n"
                        '{"capability_allowlist":[{"id":"generate_quiz"}]}\n'
                        "[/AI_PLATFORM_READONLY_CONTEXT_V1]"
                    ),
                    "sequence": 2,
                    "run_id": "run-1",
                    "created_at": now,
                    "thinking_content": None,
                    "tool_calls": None,
                    "tool_results": None,
                    "runtime_events": None,
                    "total": 1,
                }
            ]

    messages, total = await AgentThreadStore(_HistoryDatabase()).history_messages(
        tenant_id="tenant-a",
        user_id="user-a",
        runtime_thread_id="00000000-0000-0000-0000-000000000001",
        limit=10,
    )

    assert total == 1
    assert len(messages) == 1
    assert messages[0]["role"] == "assistant"
    assert messages[0]["content"] == "runtime-ok"
    assert "generate_quiz" not in messages[0]["content"]
    assert "AI_PLATFORM_READONLY_CONTEXT_V1" not in messages[0]["content"]


@pytest.mark.asyncio
async def test_attach_quiz_ids_keys_assistant_messages_from_the_capability_ledger() -> None:
    quiz_id = "01a03bee-e650-7f01-9e96-8b71052a5282"
    captured: dict[str, object] = {}

    class _Ledger:
        async def fetch(self, query: str, *args):
            captured["query"] = query
            captured["args"] = args
            return [{"run_id": "run-1", "quiz_id": quiz_id}]

    store = AgentThreadStore(_Ledger())
    messages = [
        {"role": "user", "content": "出题", "metadata": {"runtime_run_id": "run-1"}},
        {"role": "assistant", "content": "好的", "metadata": {"runtime_run_id": "run-1"}},
        {"role": "assistant", "content": "无关", "metadata": {"runtime_run_id": "run-2"}},
    ]

    await store._attach_quiz_ids(messages, tenant_id="tenant-a", user_id="user-a")

    assert messages[1]["metadata"]["quiz_id"] == quiz_id
    # A user turn is never a quiz carrier, and an unrelated run stays untouched.
    assert "quiz_id" not in messages[0]["metadata"]
    assert "quiz_id" not in messages[2]["metadata"]
    assert captured["args"] == ("tenant-a", "user-a", ["run-1", "run-2"])


@pytest.mark.asyncio
async def test_attach_quiz_ids_is_advisory_when_the_ledger_is_unavailable() -> None:
    class _BrokenLedger:
        async def fetch(self, *_args):
            raise RuntimeError("ledger unavailable")

    store = AgentThreadStore(_BrokenLedger())
    messages = [{"role": "assistant", "content": "好的", "metadata": {"runtime_run_id": "run-1"}}]

    await store._attach_quiz_ids(messages, tenant_id="tenant-a", user_id="user-a")

    assert "quiz_id" not in messages[0]["metadata"]


@pytest.mark.asyncio
async def test_restricted_message_never_recovers_a_quiz_card():
    class Ledger:
        async def fetch(self, *_args):
            raise AssertionError("revoked message must not load quiz identifiers")
    messages = [{"role": "assistant", "content": "", "metadata": {
        "runtime_run_id": "private-run", "source_access_revoked": True,
    }}]
    await AgentThreadStore(Ledger())._attach_quiz_ids(messages, tenant_id="tenant-a", user_id="user-a")
    assert "quiz_id" not in messages[0]["metadata"]


@pytest.mark.asyncio
async def test_selected_upload_history_uses_scoped_snapshot_and_deduplicates() -> None:
    class UploadsDB:
        async def fetch(self, query, *args):
            assert "r.harness_thread_id = s.runtime_thread_id" in query
            assert "a.tenant_id = s.tenant_id AND a.user_id = s.user_id AND a.session_id = s.session_id" in query
            assert "s.tenant_id = $2 AND s.user_id = $3" in query
            assert "AND a.source = 'user' AND a.size_bytes > 0" in query
            assert args[1:] == ("tenant-a", "user-a", ["run-a", "run-b"])
            image = {"run_id": "run-a", "artifact_id": "art_0123456789abcdef", "mime_type": "image/png", "filename": "saved.png", "original_filename": "original.png"}
            return [image, image, {"run_id": "run-b", "artifact_id": "art_1111111111111111", "mime_type": "text/plain", "filename": "saved.txt"}, {**image, "run_id": "unselected"}]

    messages = [{"role": "user", "metadata": {"runtime_run_id": run}} for run in ["run-a", "run-b"]]
    await AgentThreadStore(UploadsDB())._attach_user_uploads(
        messages, tenant_id="tenant-a", user_id="user-a", runtime_thread_id="00000000-0000-0000-0000-000000000001",
    )
    assert messages[0]["metadata"]["attachments"] == [{"type": "image", "filename": "original.png", "url": "/api/v1/assistant/artifacts/art_0123456789abcdef/download"}]
    assert messages[1]["metadata"]["attachments"][0]["filename"] == "saved.txt"
