from __future__ import annotations

import json
import uuid
from datetime import datetime, timedelta, timezone
from typing import Any

import pytest
from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository

from src.services.eval import assistant_trace_reconciler
from src.services.eval.assistant_trace_capture import build_assistant_runtime_trace


def _run(*, status: str = "completed") -> dict[str, Any]:
    now = datetime.now(timezone.utc)
    return {
        "run_id": str(uuid.uuid4()),
        "tenant_id": "tenant-a", "user_id": "user-a", "session_id": "session-a",
        "runtime_thread_id": str(uuid.uuid4()), "status": status,
        "request_preview": "token=should-redact", "usage": {},
        "snapshot": {"model": {"id": "model-a", "provider_id": "provider-a"}},
        "started_at": now - timedelta(seconds=2), "ended_at": now,
    }


def _terminal(run: dict[str, Any], *, event_type: str | None = None) -> dict[str, Any]:
    successful = run["status"] in {"completed", "succeeded"}
    return {
        "event_id": str(uuid.uuid4()), "sequence": 1265,
        "event_type": event_type or (
            "compat/v1/run_finished" if successful else "compat/v1/run_error"
        ),
        "status": "succeeded" if successful else run["status"],
        "payload": {"data": {
            "run_id": run["run_id"], "session_id": run["session_id"],
            "status": "succeeded" if successful else run["status"],
        }},
        "created_at": run["ended_at"],
    }


class _ReadOnlyRuntimeRepository:
    def __init__(self, run: dict[str, Any], terminal: dict[str, Any]):
        self.run = run
        self.terminal = terminal
        self.scopes: list[dict[str, Any]] = []
        self.written: list[dict[str, Any]] = []
        self.pages = 0

    async def list_assistant_runtime_trace_reconciliation_candidates(self, **kwargs):
        assert kwargs["limit"] == 100
        assert kwargs["since"] > self.run["ended_at"] - timedelta(days=91)
        return [self.run]

    async def get_assistant_runtime_trace_terminal(self, **scope):
        self.scopes.append(scope)
        return self.terminal

    async def count_assistant_runtime_trace_events(self, **scope):
        self.scopes.append(scope)
        return {"text_delta": 381, "thinking_delta": 866, "run_error": 1}

    async def read_assistant_runtime_trace_text_page(self, **scope):
        self.scopes.append(scope)
        self.pages += 1
        return [{"sequence": 1001, "content": "visible token=private answer"}]

    async def get_assistant_runtime_trace_model_usage(self, **scope):
        self.scopes.append(scope)
        return {
            "dispatched_calls": 1, "measured_calls": 1,
            "input_tokens": 12, "output_tokens": 3,
            "cost_measured_calls": 1, "cost_microusd": 42,
        }

    async def insert_reconciled_assistant_runtime_trace(self, **kwargs):
        self.written.append(kwargs)
        return True


@pytest.mark.parametrize(
    ("run_status", "trace_status", "event_type"),
    [("completed", "succeeded", None), ("succeeded", "succeeded", None),
     ("failed", "failed", None), ("cancelled", "cancelled", None),
     ("cancelled", "cancelled", "compat/v1/cancelled")],
)
async def test_reconciler_reads_original_turn_and_preserves_identity(
    monkeypatch: pytest.MonkeyPatch, run_status: str, trace_status: str,
    event_type: str | None,
) -> None:
    run = _run(status=run_status)
    terminal = _terminal(run, event_type=event_type)
    repository = _ReadOnlyRuntimeRepository(run, terminal)
    monkeypatch.setattr(assistant_trace_reconciler, "AgentTraceRepository", lambda _db: repository)

    inserted, cursor = await assistant_trace_reconciler.reconcile_assistant_runtime_trace_page(
        type("Database", (), {"enabled": True})(),
    )

    assert (inserted, cursor) == (1, None)
    assert len(repository.written) == 1
    trace = repository.written[0]["trace"]
    assert trace["trace_id"] == trace["run_id"] == run["run_id"]
    assert trace["status"] == trace_status
    assert trace["provider"] == "provider-a"
    assert trace["metrics"]["total_tokens"] == 15
    assert trace["metadata"]["runtime_model_usage"]["cost_microusd"] == 42
    assert trace["input_preview"] == "token=[redacted]"
    assert trace["output_preview"] == "visible token=[redacted] answer"
    assert trace["events"][0]["payload"]["runtime_sequence"] == 1265
    assert repository.written[0]["terminal"]["event_id"] == terminal["event_id"]
    assert all(scope["tenant_id"] == "tenant-a" and scope["user_id"] == "user-a"
               and scope["session_id"] == "session-a" and scope["run_id"] == run["run_id"]
               for scope in repository.scopes)
    assert repository.pages == 1
    # Raw thinking/tool payloads never reach the trace; only event counts do.
    assert "thinking_delta" in json.dumps(trace["metadata"]["runtime_trajectory"])
    assert "private" not in json.dumps(trace)


async def test_reconciler_rejects_other_turn_terminal(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    run = _run()
    terminal = _terminal(run)
    terminal["payload"]["data"]["run_id"] = str(uuid.uuid4())
    repository = _ReadOnlyRuntimeRepository(run, terminal)
    monkeypatch.setattr(assistant_trace_reconciler, "AgentTraceRepository", lambda _db: repository)

    inserted, _ = await assistant_trace_reconciler.reconcile_assistant_runtime_trace_page(
        type("Database", (), {"enabled": True})(),
    )

    assert inserted == 0
    assert repository.written == []
    assert repository.pages == 0


class _Context:
    def __init__(self, value: Any):
        self.value = value

    async def __aenter__(self):
        return self.value

    async def __aexit__(self, *_args):
        return False


class _WriteConnection:
    def __init__(self):
        self.traces = 0
        self.spans = 0
        self.events = 0
        self.jobs = 0
        self.allowed = True
        self.existing_scope_matches = True
        self.queries: list[str] = []
        self.query_args: list[tuple] = []

    def transaction(self):
        return _Context(self)

    async def fetchrow(self, query: str, *args):
        self.queries.append(query)
        self.query_args.append(args)
        if "SELECT r.run_id FROM assistant_runs" in query:
            return {"run_id": args[0]} if self.allowed else None
        if "INSERT INTO agent_traces" in query:
            if self.traces:
                return None
            self.traces += 1
            return {"trace_id": args[0]}
        if "SELECT trace_id FROM agent_traces" in query:
            return {"trace_id": args[0]} if self.existing_scope_matches else None
        if "INSERT INTO agent_trace_outbox" in query:
            if self.jobs:
                return None
            self.jobs += 1
            return {"job_id": str(uuid.uuid4())}
        raise AssertionError(query)

    async def execute(self, query: str, *_args):
        self.queries.append(query)
        if "INSERT INTO agent_trace_spans" in query:
            if self.spans and "ON CONFLICT (span_id) DO NOTHING" in query:
                return "INSERT 0 0"
            self.spans += 1
        elif "INSERT INTO agent_trace_events" in query:
            if self.events and "ON CONFLICT (trace_id, sequence_no) DO NOTHING" in query:
                return "INSERT 0 0"
            self.events += 1
        elif "UPDATE agent_traces t" in query:
            return "UPDATE 1"
        else:
            raise AssertionError(query)
        return "INSERT 0 1"


class _Pool:
    def __init__(self, connection: _WriteConnection):
        self.connection = connection

    def acquire(self):
        return _Context(self.connection)


class _Holder:
    enabled = True

    def __init__(self, connection: _WriteConnection):
        self._pool = _Pool(connection)


@pytest.mark.parametrize(
    ("existing_children", "scope_matches"),
    [(None, True), ((0, 0), True), ((1, 0), True), ((1, 1), True), ((0, 0), False)],
)
async def test_reconciled_trace_is_inserted_once_with_one_outbox_job(
    existing_children, scope_matches: bool,
) -> None:
    run = _run(status="failed")
    terminal = _terminal(run)
    trace = build_assistant_runtime_trace(
        run_id=run["run_id"], request_id=run["run_id"],
        tenant_id=run["tenant_id"], user_id=run["user_id"],
        session_id=run["session_id"], message="input", snapshot=run["snapshot"],
        status="failed", started_at=run["started_at"].timestamp(),
        ended_at=run["ended_at"].timestamp(), first_token_latency_ms=0,
        output="output", event_counts={"run_error": 1}, usage={}, error_type=None,
    )
    trace["retention_expires_at"] = (run["ended_at"] + timedelta(days=90)).isoformat()
    connection = _WriteConnection()
    connection.existing_scope_matches = scope_matches
    if existing_children is not None:
        connection.traces = 1
        connection.spans, connection.events = existing_children
    repository = AgentTraceRepository(_Holder(connection))

    first = await repository.insert_reconciled_assistant_runtime_trace(
        run=run, terminal=terminal, trace=trace,
    )
    second = await repository.insert_reconciled_assistant_runtime_trace(
        run=run, terminal=terminal, trace=trace,
    )

    assert (first, second) == (existing_children is None, False)
    if not scope_matches:
        assert (connection.traces, connection.spans, connection.events, connection.jobs) == (1, 0, 0, 0)
        return
    assert (connection.traces, connection.spans, connection.events, connection.jobs) == (1, 1, 1, 1)
    assert "FOR UPDATE OF r" in connection.queries[0]
    assert "event_id" in next(query for query in connection.queries if "INSERT INTO agent_trace_events" in query)
    assert "status = 'queued'" not in next(query for query in connection.queries if "INSERT INTO agent_trace_outbox" in query)
    assert "status = $6" in next(query for query in connection.queries if "SELECT trace_id FROM agent_traces" in query)


async def test_reconciled_trace_accepts_durable_cancel_receipt() -> None:
    run = _run(status="cancelled")
    terminal = _terminal(run, event_type="compat/v1/cancelled")
    trace = build_assistant_runtime_trace(
        run_id=run["run_id"], request_id=run["run_id"],
        tenant_id=run["tenant_id"], user_id=run["user_id"],
        session_id=run["session_id"], message="input", snapshot=run["snapshot"],
        status="cancelled", started_at=run["started_at"].timestamp(),
        ended_at=run["ended_at"].timestamp(), first_token_latency_ms=0,
        output="", event_counts={"cancelled": 1}, usage={}, error_type=None,
    )
    trace["retention_expires_at"] = (run["ended_at"] + timedelta(days=90)).isoformat()
    connection = _WriteConnection()
    repository = AgentTraceRepository(_Holder(connection))

    assert await repository.insert_reconciled_assistant_runtime_trace(
        run=run, terminal=terminal, trace=trace,
    )
    assert "compat/v1/cancelled" in connection.queries[0]
    assert (connection.traces, connection.spans, connection.events, connection.jobs) == (1, 1, 1, 1)


async def test_reconciled_trace_requires_current_scoped_terminal_run() -> None:
    run = _run()
    terminal = _terminal(run)
    connection = _WriteConnection()
    connection.allowed = False
    repository = AgentTraceRepository(_Holder(connection))
    trace = {"metrics": {}}

    inserted = await repository.insert_reconciled_assistant_runtime_trace(
        run=run, terminal=terminal, trace=trace,
    )

    assert inserted is False
    assert (connection.traces, connection.spans, connection.events, connection.jobs) == (0, 0, 0, 0)


async def test_live_runtime_ingest_dedupes_even_succeeded_outbox_job() -> None:
    connection = _WriteConnection()
    connection.traces = 1
    connection.jobs = 1  # Existing job may already have succeeded.
    repository = AgentTraceRepository(_Holder(connection))

    created = await repository.create_trace_ingested_outbox_job(
        tenant_id="tenant-a", trace_id=str(uuid.uuid4()),
        trace_family="assistant", status="failed",
        source_adapter="gateway.agent_runtime",
    )

    assert created is None
    assert connection.jobs == 1
    assert "FOR UPDATE" in connection.queries[0]
    assert "($4::boolean OR status IN" in connection.queries[1]
    assert connection.query_args[1][-1] is True
