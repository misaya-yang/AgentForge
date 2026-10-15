"""
Usage Metrics Scheduler - Periodic aggregation and cleanup tasks.

Handles:
- Daily aggregation of usage_records to usage_daily_aggregates
- Cleanup of old usage_records (beyond retention period)
- Daily/monthly quota resets
"""

import asyncio
import logging
from datetime import datetime

from ...persistence.database import DatabaseStorage
from .aggregation_task import AggregationTask, QuotaResetTask

logger = logging.getLogger(__name__)


class UsageScheduler:
    """
    Background scheduler for usage metrics tasks.

    Runs periodic tasks:
    - Every hour: Check and reset quotas that need resetting
    - Daily at 00:30 UTC: Aggregate previous day's usage
    - Daily at 01:00 UTC: Cleanup old records
    """

    def __init__(
        self,
        db: DatabaseStorage,
        retention_days: int = 30,
        aggregation_hour: int = 0,
        aggregation_minute: int = 30,
        cleanup_hour: int = 1,
        cleanup_minute: int = 0,
        quota_check_interval: int = 3600,  # 1 hour in seconds
    ):
        """
        Initialize usage scheduler.

        Args:
            db: Database manager instance
            retention_days: Days to keep detailed usage_records
            aggregation_hour: Hour (UTC) to run daily aggregation
            aggregation_minute: Minute to run daily aggregation
            cleanup_hour: Hour (UTC) to run daily cleanup
            cleanup_minute: Minute to run daily cleanup
            quota_check_interval: Seconds between quota checks
        """
        self.db = db
        self.retention_days = retention_days
        self.aggregation_hour = aggregation_hour
        self.aggregation_minute = aggregation_minute
        self.cleanup_hour = cleanup_hour
        self.cleanup_minute = cleanup_minute
        self.quota_check_interval = quota_check_interval

        self._aggregation_task: AggregationTask | None = None
        self._quota_reset_task: QuotaResetTask | None = None

        self._running = False
        self._tasks: list[asyncio.Task] = []

        # Track last run times
        self._last_aggregation_date: datetime | None = None
        self._last_cleanup_date: datetime | None = None
        self._last_quota_check: datetime | None = None

    async def start(self) -> None:
        """Start the scheduler."""
        if self._running:
            logger.warning("UsageScheduler is already running")
            return

        self._running = True

        # Initialize task instances
        self._aggregation_task = AggregationTask(self.db, self.retention_days)
        self._quota_reset_task = QuotaResetTask(self.db)

        # Start background tasks
        self._tasks = [
            asyncio.create_task(self._run_quota_check_loop()),
            asyncio.create_task(self._run_daily_tasks_loop()),
        ]

        logger.info(
            f"UsageScheduler started: "
            f"aggregation at {self.aggregation_hour:02d}:{self.aggregation_minute:02d} UTC, "
            f"cleanup at {self.cleanup_hour:02d}:{self.cleanup_minute:02d} UTC, "
            f"quota check every {self.quota_check_interval}s, "
            f"retention {self.retention_days} days"
        )

    async def stop(self) -> None:
        """Stop the scheduler."""
        if not self._running:
            return

        self._running = False

        # Cancel all tasks
        for task in self._tasks:
            task.cancel()

        # Wait for tasks to complete
        if self._tasks:
            await asyncio.gather(*self._tasks, return_exceptions=True)

        self._tasks = []
        logger.info("UsageScheduler stopped")

    async def _run_quota_check_loop(self) -> None:
        """Run quota check loop."""
        while self._running:
            try:
                await self._run_quota_check()
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error(f"Quota check error: {e}")

            try:
                await asyncio.sleep(self.quota_check_interval)
            except asyncio.CancelledError:
                break

    async def _run_daily_tasks_loop(self) -> None:
        """Run daily aggregation and cleanup tasks."""
        while self._running:
            try:
                now = datetime.utcnow()

                # Check if it's time for aggregation
                if self._should_run_aggregation(now):
                    await self._run_aggregation()
                    self._last_aggregation_date = now.date()

                # Check if it's time for cleanup
                if self._should_run_cleanup(now):
                    await self._run_cleanup()
                    self._last_cleanup_date = now.date()

            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error(f"Daily tasks error: {e}")

            # Check every minute
            try:
                await asyncio.sleep(60)
            except asyncio.CancelledError:
                break

    def _should_run_aggregation(self, now: datetime) -> bool:
        """Check if aggregation should run now."""
        # Skip if already ran today
        if self._last_aggregation_date == now.date():
            return False

        # Check if it's the right time
        return now.hour == self.aggregation_hour and now.minute >= self.aggregation_minute

    def _should_run_cleanup(self, now: datetime) -> bool:
        """Check if cleanup should run now."""
        # Skip if already ran today
        if self._last_cleanup_date == now.date():
            return False

        # Check if it's the right time
        return now.hour == self.cleanup_hour and now.minute >= self.cleanup_minute

    async def _run_quota_check(self) -> None:
        """Run quota check and reset."""
        if not self._quota_reset_task:
            return

        logger.debug("Running quota check...")
        try:
            result = await self._quota_reset_task.check_and_reset_quotas()
            if result.get("daily", {}).get("users_reset", 0) > 0:
                logger.info(f"Daily quotas reset: {result['daily']['users_reset']} users")
            if result.get("monthly", {}).get("users_reset", 0) > 0:
                logger.info(f"Monthly quotas reset: {result['monthly']['users_reset']} users")
            self._last_quota_check = datetime.utcnow()
        except Exception as e:
            logger.error(f"Quota check failed: {e}")

    async def _run_aggregation(self) -> None:
        """Run daily aggregation."""
        if not self._aggregation_task:
            return

        logger.info("Running daily aggregation...")
        try:
            result = await self._aggregation_task.run_daily_aggregation()
            logger.info(
                f"Daily aggregation complete: {result.get('aggregations_created', 0)} "
                f"dimension combinations for {result.get('date')}"
            )
        except Exception as e:
            logger.error(f"Daily aggregation failed: {e}")

    async def _run_cleanup(self) -> None:
        """Run daily cleanup."""
        if not self._aggregation_task:
            return

        logger.info("Running daily cleanup...")
        try:
            result = await self._aggregation_task.cleanup_old_records()
            logger.info(
                f"Daily cleanup complete: {result.get('records_deleted', 0)} "
                f"records deleted (older than {result.get('cutoff_date')})"
            )
        except Exception as e:
            logger.error(f"Daily cleanup failed: {e}")

    # Manual trigger methods for API use

    async def trigger_aggregation(self, target_date: datetime | None = None) -> dict:
        """
        Manually trigger aggregation for a specific date.

        Args:
            target_date: Date to aggregate (defaults to yesterday)

        Returns:
            Aggregation result
        """
        if not self._aggregation_task:
            self._aggregation_task = AggregationTask(self.db, self.retention_days)

        return await self._aggregation_task.run_daily_aggregation(target_date)

    async def trigger_cleanup(self) -> dict:
        """
        Manually trigger cleanup.

        Returns:
            Cleanup result
        """
        if not self._aggregation_task:
            self._aggregation_task = AggregationTask(self.db, self.retention_days)

        return await self._aggregation_task.cleanup_old_records()

    async def trigger_quota_reset(self, reset_type: str = "daily") -> dict:
        """
        Manually trigger quota reset.

        Args:
            reset_type: "daily" or "monthly"

        Returns:
            Reset result
        """
        if not self._quota_reset_task:
            self._quota_reset_task = QuotaResetTask(self.db)

        if reset_type == "monthly":
            return await self._quota_reset_task.reset_monthly_quotas()
        else:
            return await self._quota_reset_task.reset_daily_quotas()

    def get_status(self) -> dict:
        """Get scheduler status."""
        return {
            "running": self._running,
            "retention_days": self.retention_days,
            "aggregation_time": f"{self.aggregation_hour:02d}:{self.aggregation_minute:02d} UTC",
            "cleanup_time": f"{self.cleanup_hour:02d}:{self.cleanup_minute:02d} UTC",
            "quota_check_interval_seconds": self.quota_check_interval,
            "last_aggregation_date": str(self._last_aggregation_date)
            if self._last_aggregation_date
            else None,
            "last_cleanup_date": str(self._last_cleanup_date) if self._last_cleanup_date else None,
            "last_quota_check": self._last_quota_check.isoformat()
            if self._last_quota_check
            else None,
        }


# Singleton
_usage_scheduler: UsageScheduler | None = None


def init_usage_scheduler(
    db: DatabaseStorage,
    retention_days: int = 30,
    **kwargs,
) -> UsageScheduler:
    """Initialize the usage scheduler singleton."""
    global _usage_scheduler
    _usage_scheduler = UsageScheduler(db, retention_days, **kwargs)
    return _usage_scheduler


def get_usage_scheduler() -> UsageScheduler | None:
    """Get the usage scheduler singleton."""
    return _usage_scheduler
