from __future__ import annotations

from typing import Any

import pytest

from src.services.metrics.usage_recorder import UsageRecord, UsageRecorder


class _Tx:
    async def __aenter__(self):
        return self

    async def __aexit__(self, *_exc):
        return False


class _Acquire:
    def __init__(self, conn: _QuotaConn):
        self._conn = conn

    async def __aenter__(self):
        return self._conn

    async def __aexit__(self, *_exc):
        return False


class _Pool:
    def __init__(self, conn: _QuotaConn):
        self._conn = conn

    def acquire(self):
        return _Acquire(self._conn)


class _DB:
    def __init__(self, conn: _QuotaConn):
        self._pool = _Pool(conn)


class _QuotaConn:
    def __init__(self, *, fail_quota: bool = False):
        self.fail_quota = fail_quota
        self.usage_rows = 0
        self.quota_updates: list[tuple[Any, ...]] = []
        self.reconciliation_events = 0
        self.daily_tokens = 0

    def transaction(self):
        return _Tx()

    async def fetchrow(self, query: str, *_args: Any):
        if "INSERT INTO usage_records" in query:
            self.usage_rows += 1
            return {"inserted": True}
        return None

    async def executemany(self, query: str, rows: list[tuple[Any, ...]]):
        if "UPDATE user_quotas" in query:
            if self.fail_quota:
                raise RuntimeError("quota table unavailable")
            self.quota_updates.extend(rows)
            return
        if "INSERT INTO request_traces" in query:
            return

    async def fetchval(self, query: str, *args: Any):
        if "UPDATE usage_daily_aggregates" in query:
            self.daily_tokens += int(args[9]) + int(args[10])
        return None

    async def execute(self, query: str, *_args: Any):
        if "INSERT INTO billing_events" in query:
            self.reconciliation_events += 1


def _record(**overrides: Any) -> UsageRecord:
    base = {
        "tenant_id": "tenant-a",
        "user_id": "user-a",
        "model": "gemini-3-flash-preview",
        "provider": "google",
        "input_tokens": 100,
        "output_tokens": 50,
        "request_id": "req-quota-1",
        "service_id": "svc-a",
        "request_type": "proxy_run_wait",
        "status": "success",
        "metadata": {"source": "transparent_proxy_non_stream", "token_source": "upstream"},
    }
    base.update(overrides)
    return UsageRecord(**base)


async def _recorder(conn: _QuotaConn) -> UsageRecorder:
    return UsageRecorder(
        database=_DB(conn),
        buffer_size=100,
        normal_trace_sample_rate=0.0,
        default_trace_p95_threshold_ms=999_999,
    )


@pytest.mark.asyncio
async def test_accepted_usage_increments_quota_and_daily_aggregate_once():
    conn = _QuotaConn()
    recorder = await _recorder(conn)

    await recorder.record(_record())
    await recorder._flush_buffer()

    assert conn.usage_rows == 1
    assert len(conn.quota_updates) == 1
    tenant_id, user_id, tokens, cost_cents, requests = conn.quota_updates[0]
    assert (tenant_id, user_id) == ("tenant-a", "user-a")
    assert tokens == 150
    assert cost_cents >= 0
    assert requests == 1
    assert conn.daily_tokens == 150


@pytest.mark.asyncio
async def test_quota_update_failure_keeps_usage_and_records_reconciliation_event():
    conn = _QuotaConn(fail_quota=True)
    recorder = await _recorder(conn)

    await recorder.record(_record())
    await recorder._flush_buffer()

    assert conn.usage_rows == 1
    assert recorder._buffer == []
    assert conn.reconciliation_events == 1
