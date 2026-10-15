from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest

from src.services.eval.trace_retention_scheduler import TraceRetentionScheduler


class _PurgeRepository:
    def __init__(self, *, batches: list[int]) -> None:
        self.batches = list(batches)
        self.calls: list[dict[str, Any]] = []

    async def purge_expired_traces(self, **kwargs: Any) -> int:
        self.calls.append(kwargs)
        if not self.batches:
            return 0
        return self.batches.pop(0)


@pytest.mark.asyncio
async def test_trace_retention_scheduler_purges_in_batches(monkeypatch) -> None:
    repo = _PurgeRepository(batches=[500, 120])
    database = SimpleNamespace(enabled=True)
    scheduler = TraceRetentionScheduler(database, retention_days=45, batch_size=500)

    monkeypatch.setattr(
        "src.services.eval.trace_retention_scheduler.AgentTraceRepository",
        lambda _database: repo,
    )

    deleted = await scheduler.run_cleanup_once()

    assert deleted == 620
    assert len(repo.calls) == 2
    assert repo.calls[0]["default_retention_days"] == 45
    assert repo.calls[0]["batch_size"] == 500


@pytest.mark.asyncio
async def test_trace_retention_scheduler_skips_when_database_disabled() -> None:
    scheduler = TraceRetentionScheduler(SimpleNamespace(enabled=False))
    deleted = await scheduler.run_cleanup_once()
    assert deleted == 0
