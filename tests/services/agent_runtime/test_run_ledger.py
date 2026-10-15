from __future__ import annotations

import json
import re
import sqlite3
import uuid
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import httpx
import pytest

from src.services.agent_runtime.control.event_stream import stream_events
from src.services.agent_runtime.control.run_ledger import (
    TERMINAL_USAGE_PROJECTION_SQL,
    complete_run,
)
from src.services.agent_runtime.control.types import AgentTurn

CASES = json.loads(
    (Path(__file__).resolve().parents[2] / "fixtures/agent-runtime/terminal-status-v1.json")
    .read_text()
)


class LedgerDatabase:
    """Run the production UPDATE predicate locally, without a PostgreSQL claim.

    Cast/time substitutions do not simulate PostgreSQL locks or crash atomicity.
    """

    def __init__(self, run_id: uuid.UUID, status: str = "running", engine: str = "agent_runtime") -> None:
        self.projection_attempts: list[uuid.UUID] = []
        self.connection = sqlite3.connect(":memory:")
        self.connection.row_factory = sqlite3.Row
        self.connection.executescript(
            "CREATE TABLE assistant_runs(run_id TEXT PRIMARY KEY, engine TEXT, status TEXT, "
            "usage TEXT, finished_at TEXT, updated_at TEXT);"
            "CREATE TABLE assistant_runtime_model_leases(run_id TEXT, status TEXT, "
            "revoked_at TEXT, revoked_reason TEXT, updated_at TEXT);"
        )
        self.connection.execute(
            "INSERT INTO assistant_runs VALUES(?, ?, ?, '{}', 'original', 'original')",
            (str(run_id), engine, status),
        )
        self.connection.execute(
            "INSERT INTO assistant_runtime_model_leases VALUES(?, 'active', NULL, NULL, NULL)",
            (str(run_id),),
        )

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any]:
        if "FROM assistant_runtime_model_calls" in query:
            return {"input_tokens": 3, "output_tokens": 5, "cost_microusd": 8}
        if "FROM assistant_runtime_snapshots" in query:
            return {"payload": {}}
        raise AssertionError(f"unexpected query: {query}")

    async def execute(self, query: str, *args: Any) -> str:
        if query == TERMINAL_USAGE_PROJECTION_SQL:
            # PostgreSQL JSON/CAS behavior has its own actual-role fixture.
            # This SQLite seam proves the production call occurs after status
            # handling, without pretending to execute PostgreSQL syntax.
            self.projection_attempts.append(args[0])
            return "UPDATE 0"
        sql = re.sub(r"\$(\d+)", r"?\1", query.replace("::jsonb", ""))
        sql = sql.replace("NOW()", "CURRENT_TIMESTAMP")
        bound = tuple(str(arg) if isinstance(arg, uuid.UUID) else arg for arg in args)
        cursor = self.connection.execute(sql, bound)
        return f"UPDATE {cursor.rowcount}"

    def run(self) -> dict[str, Any]:
        return dict(self.connection.execute("SELECT * FROM assistant_runs").fetchone())


@pytest.mark.parametrize("case", CASES["statuses"], ids=lambda case: case["input"] or "empty")
async def test_complete_run_uses_canonical_status_and_fails_closed(case: dict[str, Any]) -> None:
    run_id = uuid.uuid4()
    database = LedgerDatabase(run_id)
    try:
        await complete_run(SimpleNamespace(database=database), run_id, case["input"])
        assert database.run()["status"] == case["completion_status"]
        assert json.loads(database.run()["usage"]) == {
            "input_tokens": 3, "output_tokens": 5, "cost_microusd": 8,
        }
        lease = database.connection.execute(
            "SELECT status, revoked_reason FROM assistant_runtime_model_leases"
        ).fetchone()
        assert tuple(lease) == ("revoked", f"turn_{case['completion_status']}")
    finally:
        database.connection.close()


@pytest.mark.parametrize("case", CASES["existing_rows"])
async def test_complete_run_preserves_existing_terminal_rows_and_other_engines(
    case: dict[str, Any],
) -> None:
    run_id = uuid.uuid4()
    database = LedgerDatabase(run_id, case["status"], case["engine"])
    try:
        await complete_run(SimpleNamespace(database=database), run_id, "succeeded")
        row = database.run()
        expected = "succeeded" if case["may_complete"] else case["status"]
        assert row["status"] == expected
        if not case["may_complete"]:
            assert row["finished_at"] == "original"
            assert row["usage"] == "{}"
    finally:
        database.connection.close()


@pytest.mark.parametrize(
    "event_type,data_status,expected",
    [
        ("run_finished", "succeeded", "succeeded"),
        ("run_error", "failed", "failed"),
        ("run_error", "cancelled", "cancelled"),
        ("cancelled", None, "cancelled"),
        ("cancelled", "failed", "cancelled"),
    ],
)
async def test_stream_terminal_updates_actual_ledger_before_delivery(
    event_type: str, data_status: str | None, expected: str,
) -> None:
    run_id = uuid.uuid4()
    database = LedgerDatabase(run_id)
    event = {"event_type": event_type, "data": {"run_id": str(run_id)}}
    if data_status is not None:
        event["data"]["status"] = data_status
    content = f"event: {event_type}\ndata: {json.dumps(event)}\n\n"
    client = httpx.AsyncClient(
        transport=httpx.MockTransport(
            lambda request: httpx.Response(200, request=request, content=content)
        )
    )
    plane = SimpleNamespace(
        database=database, runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token", http_client=client,
    )
    plane._complete_run = lambda target, status: complete_run(plane, target, status)
    turn = AgentTurn(
        str(uuid.uuid4()), str(run_id), str(uuid.uuid4()), str(uuid.uuid4()),
        0, "auto", "auto", "adapter", 1, None,
    )
    stream = stream_events(
        plane, turn=turn, tenant_id="tenant-a", user_id="user-a", session_id="session-a",
    )
    try:
        await anext(stream)
        assert database.run()["status"] == expected
        await stream.aclose()
        assert database.run()["status"] == expected
    finally:
        await stream.aclose()
        await client.aclose()
        database.connection.close()



def test_terminal_usage_projection_sql_is_identical_across_gateway_languages() -> None:
    repo = Path(__file__).resolve().parents[3]
    native = (repo / "rust/platform-native/crates/platform-gateway/src/migration_agent/control.rs").read_text()
    match = re.search(r'const TERMINAL_USAGE_PROJECTION_SQL: &str = r#"(.*?)"#;', native, re.S)
    assert match is not None
    fixture = (repo / "tests/fixtures/agent-runtime/terminal-usage-projection.sql").read_text()
    assert match.group(1).strip() == TERMINAL_USAGE_PROJECTION_SQL.strip() == fixture.strip()
    # These are contract checks only; the isolated PostgreSQL matrix executes
    # the query, including scope, no-zero, replay and compare-and-set cases.
    assignment = TERMINAL_USAGE_PROJECTION_SQL.split("UPDATE assistant_runs r SET", 1)[1].split("FROM projection", 1)[0]
    assert assignment.strip() == "usage=p.next_usage,updated_at=NOW()"
    assert "'provider_usage_verified',false" in TERMINAL_USAGE_PROJECTION_SQL
    assert "'settlement','not_established'" in TERMINAL_USAGE_PROJECTION_SQL
    assert "r.usage IS NOT DISTINCT FROM p.usage" in TERMINAL_USAGE_PROJECTION_SQL


@pytest.mark.parametrize("status", ["succeeded", "failed", "cancelled", "completed"])
async def test_complete_run_attempts_separate_usage_projection_after_terminal(status: str) -> None:
    run_id = uuid.uuid4()
    database = LedgerDatabase(run_id, status)
    try:
        await complete_run(SimpleNamespace(database=database), run_id, "succeeded")
        await complete_run(SimpleNamespace(database=database), run_id, "failed")
        assert database.projection_attempts == [run_id, run_id]
        assert database.run()["status"] == status
        assert database.run()["finished_at"] == "original"
    finally:
        database.connection.close()
