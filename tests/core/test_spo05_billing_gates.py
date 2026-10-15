"""SPO-05 gate tests: G1 GREATEST conflict, half-open date interval, pricing prefix.

The G1 tests drive the shipped ``UsageRecorder`` write path against a fake
connection that simulates the ON CONFLICT contract. Repeated cumulative
observations update the stored maximum and expose only the accounting delta,
while asserting the shipped SQL carries the monotonic-max clauses.
"""

from __future__ import annotations

from contextlib import asynccontextmanager
from datetime import date, datetime, timezone
from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_core.billing.pricing_catalog import resolve_pricing_with_status
from ai_gateway_core.metrics.usage_recorder import UsageRecord, UsageRecorder


def _record(*, request_id: str, input_tokens: int = 0, output_tokens: int = 0) -> UsageRecord:
    return UsageRecord(
        tenant_id="tenant-a",
        user_id="user-a",
        request_id=request_id,
        service_id="assistant",
        request_type="chat",
        model="test-model",
        provider="test",
        input_tokens=input_tokens,
        output_tokens=output_tokens,
        input_cost_cents=0,
        output_cost_cents=0,
        latency_ms=0,
        first_token_ms=0,
        request_total_duration_ms=0,
        llm_inference_duration_ms=0,
        retrieval_duration_ms=0,
        tool_call_duration_ms=0,
        agent_or_graph_overhead_ms=0,
        tool_call_breakdown={},
        error_type=None,
        status="success",
        metadata={},
        timestamp=datetime.now(timezone.utc).timestamp(),
    )


class _FakePool:
    def __init__(self, conn: _FakeConn) -> None:
        self._conn = conn

    def acquire(self):
        @asynccontextmanager
        async def _acquire():
            yield self._conn

        return _acquire()


class _FakeConn:
    """Simulates the shipped ON CONFLICT semantics for usage_records."""

    def __init__(self) -> None:
        self.rows: dict[tuple[str, str, str, str], dict[str, Any]] = {}
        self.queries: list[str] = []

    def transaction(self):
        @asynccontextmanager
        async def _tx():
            yield

        return _tx()

    async def fetch(self, sql: str, *args: Any) -> list[dict[str, Any]]:
        self.queries.append(sql)
        assert "ON CONFLICT" in sql
        rows: list[dict[str, Any]] = []
        # Column order: tenant_id, user_id, request_id, service_id, assistant_id,
        # model, provider, input_tokens, output_tokens, input_cost_cents,
        # output_cost_cents, latency_ms, first_token_ms, ..., request_type,
        # metadata, created_at (24 columns per the VALUES list).
        for offset in range(0, len(args), 24):
            row = args[offset : offset + 24]
            # Column order: tenant(0), user(1), request_id(2), service_id(3),
            # assistant_id(4), model(5), provider(6), tokens/costs(7-10), ...,
            # error_type(19), status(20), request_type(21), metadata(22), created_at(23).
            identity = (str(row[0]), str(row[2]), str(row[3]), str(row[21]))
            inserted = identity not in self.rows
            if inserted:
                stored = {
                    "input_tokens": int(row[7]),
                    "output_tokens": int(row[8]),
                    "input_cost_cents": int(row[9]),
                    "output_cost_cents": int(row[10]),
                }
                self.rows[identity] = stored
            else:
                stored = self.rows[identity]
                # GREATEST semantics from the shipped SQL.
                stored["input_tokens"] = max(stored["input_tokens"], int(row[7]))
                stored["output_tokens"] = max(stored["output_tokens"], int(row[8]))
                stored["input_cost_cents"] = max(stored["input_cost_cents"], int(row[9]))
                stored["output_cost_cents"] = max(stored["output_cost_cents"], int(row[10]))
            rows.append(
                {
                    "inserted": inserted,
                    "tenant_id": row[0],
                    "request_id_key": row[2],
                    "service_id_key": row[3],
                    "request_type_key": row[21],
                }
            )
        return rows

    async def fetchrow(self, sql: str, *_args: Any) -> dict[str, Any] | None:
        self.queries.append(sql)
        return {"inserted": True}

    async def fetchval(self, _sql: str, *_args: Any) -> Any:
        return None

    async def execute(self, _sql: str, *_args: Any) -> str:
        return "OK"

    async def executemany(self, _sql: str, _rows: list[tuple[Any, ...]]) -> None:
        return None


def _recorder(conn: _FakeConn) -> UsageRecorder:
    pool = _FakePool(conn)
    recorder = UsageRecorder(database=SimpleNamespace(_pool=pool))
    return recorder


@pytest.mark.asyncio
async def test_conflict_branch_takes_greatest_for_partial_then_final_usage() -> None:
    conn = _FakeConn()
    recorder = _recorder(conn)

    partial = _record(request_id="req-1", input_tokens=10, output_tokens=2)
    final = _record(request_id="req-1", input_tokens=12, output_tokens=20)

    accepted = await recorder._write_records(conn, [partial])
    assert len(accepted) == 1
    accepted_again = await recorder._write_records(conn, [final])
    assert len(accepted_again) == 1
    assert accepted_again[0].input_tokens == 2
    assert accepted_again[0].output_tokens == 18
    assert accepted_again[0].metadata["_accounting_request_delta"] == 0

    identity = ("tenant-a", "req-1", "assistant", "chat")
    stored = conn.rows[identity]
    assert stored["input_tokens"] == 12
    assert stored["output_tokens"] == 20

    # The shipped SQL carries the monotonic-max conflict clauses.
    conflict_sql = next(
        sql for sql in conn.queries if "ON CONFLICT" in sql and "GREATEST" in sql
    )
    assert "input_tokens = GREATEST" in conflict_sql
    assert "output_tokens = GREATEST" in conflict_sql
    assert "input_cost_cents = GREATEST" in conflict_sql
    assert "output_cost_cents = GREATEST" in conflict_sql


@pytest.mark.asyncio
async def test_quota_counters_receive_final_delta_without_second_request() -> None:
    conn = _FakeConn()
    recorder = _recorder(conn)
    partial = _record(request_id="req-2", input_tokens=5, output_tokens=1)
    final = _record(request_id="req-2", input_tokens=50, output_tokens=10)

    first = await recorder._write_records(conn, [partial])
    second = await recorder._write_records(conn, [final])

    assert len(first) == 1
    assert len(second) == 1
    assert second[0].input_tokens == 45
    assert second[0].output_tokens == 9
    assert second[0].metadata["_accounting_request_delta"] == 0


def test_pricing_prefix_only_matches_requested_prefix() -> None:
    """SPO-05 gate: gpt-4 must never be priced as gpt-4o."""
    gpt4_pricing, gpt4_status = resolve_pricing_with_status("gpt-4")
    gpt4o_pricing, gpt4o_status = resolve_pricing_with_status("gpt-4o")

    assert gpt4_status != "unknown"
    assert gpt4o_status in {"catalog", "provider_model"}
    # The variant direction is allowed: dated snapshots inherit base pricing.
    variant_pricing, variant_status = resolve_pricing_with_status("gpt-4o-2024-11-20")
    assert variant_status in {"catalog", "provider_model"}
    assert variant_pricing == gpt4o_pricing
    # The reverse direction is forbidden.
    assert gpt4_pricing != gpt4o_pricing


@pytest.mark.asyncio
async def test_usage_summary_provider_path_uses_half_open_interval() -> None:
    """The provider-filtered summary query must not cast created_at with ::date."""
    captured: dict[str, Any] = {}

    class _Conn:
        async def fetchrow(self, sql: str, *args: Any) -> dict[str, Any]:
            captured["sql"] = sql
            captured["args"] = list(args)
            return {
                "total_requests": 0,
                "success_count": 0,
                "error_count": 0,
                "total_input_tokens": 0,
                "total_output_tokens": 0,
                "total_cost_cents": 0,
                "avg_latency_ms": 0,
                "p95_latency_ms": 0,
            }

    class _Pool:
        def acquire(self):
            @asynccontextmanager
            async def _acquire():
                yield _Conn()

            return _acquire()

    recorder = UsageRecorder(database=SimpleNamespace(_pool=_Pool()))
    await recorder.get_usage_summary(
        tenant_id="tenant-a",
        start_date=date(2026, 8, 1),
        end_date=date(2026, 8, 7),
        provider="dashscope",
    )

    sql = captured["sql"]
    assert "created_at::date" not in sql
    assert "created_at >= $2::date" in sql
    assert "created_at < ($3::date + interval '1 day')" in sql


@pytest.mark.asyncio
async def test_trace_sampling_p95_reads_latest_nonzero_daily_aggregate() -> None:
    captured: dict[str, Any] = {}

    class _Conn:
        async def fetchval(self, sql: str, *args: Any) -> int:
            captured["sql"] = sql
            captured["args"] = args
            return 4321

    recorder = UsageRecorder(database=None, default_trace_p95_threshold_ms=9999)
    threshold = await recorder._get_trace_p95_threshold_ms(_Conn(), "tenant-a")

    sql = captured["sql"]
    assert threshold == 4321
    assert captured["args"] == ("tenant-a",)
    assert "FROM usage_daily_aggregates" in sql
    assert "p95_latency_ms > 0" in sql
    assert "ORDER BY date DESC, p95_latency_ms DESC" in sql
    assert "LIMIT 1" in sql
    assert "usage_records" not in sql
    assert "PERCENTILE_CONT" not in sql


@pytest.mark.asyncio
async def test_trace_sampling_p95_uses_default_when_daily_aggregate_is_missing() -> None:
    class _Conn:
        async def fetchval(self, _sql: str, *_args: Any) -> None:
            return None

    recorder = UsageRecorder(database=None, default_trace_p95_threshold_ms=9876)

    assert await recorder._get_trace_p95_threshold_ms(_Conn(), "tenant-empty") == 9876
