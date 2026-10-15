"""Background scheduler for agent trace retention cleanup."""

from __future__ import annotations

import asyncio
import logging
import os
from datetime import datetime, timedelta, timezone

from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository

logger = logging.getLogger(__name__)


def _affected_row_count(result: object) -> int:
    """Extract the row count from a DatabaseStorage execute result."""
    if isinstance(result, str):
        parts = result.split()
        if parts and parts[-1].isdigit():
            return int(parts[-1])
    try:
        return int(result)  # type: ignore[arg-type]
    except (TypeError, ValueError):
        return 0

_trace_retention_scheduler: TraceRetentionScheduler | None = None


class TraceRetentionScheduler:
    def __init__(
        self,
        database,
        *,
        retention_days: int = 90,
        request_trace_retention_days: int = 14,
        cleanup_hour: int = 2,
        cleanup_minute: int = 15,
        batch_size: int = 500,
    ) -> None:
        self.database = database
        self.retention_days = retention_days
        # SPO-05 / D3: request_traces retention (7–14 days default window).
        self.request_trace_retention_days = max(
            7, min(int(request_trace_retention_days), 14)
        )
        self.cleanup_hour = cleanup_hour
        self.cleanup_minute = cleanup_minute
        self.batch_size = batch_size
        self._running = False
        self._task: asyncio.Task[None] | None = None
        self._last_cleanup_date: datetime | None = None

    async def start(self) -> None:
        if self._running:
            return
        self._running = True
        self._task = asyncio.create_task(self._run_loop())
        logger.info(
            "TraceRetentionScheduler started (retention_days=%s, cleanup=%02d:%02d UTC)",
            self.retention_days,
            self.cleanup_hour,
            self.cleanup_minute,
        )

    async def stop(self) -> None:
        if not self._running:
            return
        self._running = False
        if self._task is not None:
            self._task.cancel()
            await asyncio.gather(self._task, return_exceptions=True)
        self._task = None
        logger.info("TraceRetentionScheduler stopped")

    async def _run_loop(self) -> None:
        while self._running:
            try:
                now = datetime.now(timezone.utc)
                if self._should_run_cleanup(now):
                    deleted = await self.run_cleanup_once()
                    self._last_cleanup_date = now.date()
                    logger.info("Agent trace retention cleanup removed %s traces", deleted)
                await asyncio.sleep(300)
            except asyncio.CancelledError:
                raise
            except Exception:
                logger.exception("TraceRetentionScheduler loop error")
                await asyncio.sleep(300)

    def _should_run_cleanup(self, now: datetime) -> bool:
        if self._last_cleanup_date == now.date():
            return False
        return now.hour > self.cleanup_hour or (
            now.hour == self.cleanup_hour and now.minute >= self.cleanup_minute
        )

    async def run_cleanup_once(self) -> int:
        if not getattr(self.database, "enabled", False):
            return 0
        repository = AgentTraceRepository(self.database)
        total_deleted = 0
        while True:
            deleted = await repository.purge_expired_traces(
                default_retention_days=self.retention_days,
                batch_size=self.batch_size,
            )
            total_deleted += deleted
            if deleted < self.batch_size:
                break
        total_deleted += await self._purge_expired_request_traces()
        return total_deleted

    async def _purge_expired_request_traces(self) -> int:
        """SPO-05 / D3: bound request_traces growth to the retention window."""
        cutoff = datetime.now(timezone.utc) - timedelta(
            days=self.request_trace_retention_days
        )
        try:
            result = await self.database.execute(
                "DELETE FROM request_traces WHERE created_at < $1",
                cutoff,
            )
            deleted = _affected_row_count(result)
            if deleted:
                logger.info("Request trace retention removed %s traces", deleted)
            return deleted
        except Exception as exc:
            logger.exception("Request trace retention cleanup failed: %s", exc)
            return 0


def init_trace_retention_scheduler(database) -> TraceRetentionScheduler | None:
    global _trace_retention_scheduler
    if not getattr(database, "enabled", False):
        _trace_retention_scheduler = None
        return None
    retention_days = int(os.getenv("AGENT_TRACE_RETENTION_DAYS", "90"))
    _trace_retention_scheduler = TraceRetentionScheduler(
        database,
        retention_days=retention_days,
    )
    return _trace_retention_scheduler


def get_trace_retention_scheduler() -> TraceRetentionScheduler | None:
    return _trace_retention_scheduler
