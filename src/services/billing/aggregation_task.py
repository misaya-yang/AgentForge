"""
Daily Aggregation and Cleanup Tasks for Usage Metrics.

Provides:
- Daily aggregation from usage_records to usage_daily_aggregates
- Data cleanup for expired usage_records (configurable retention)
- Quota reset tasks (daily/monthly)
"""

import asyncio
import logging
from datetime import datetime, timedelta

from ...persistence.database import DatabaseStorage

logger = logging.getLogger(__name__)


class AggregationTask:
    """
    Handles daily aggregation of usage records and data cleanup.

    Designed to run as a scheduled task (e.g., cron job or APScheduler).
    """

    def __init__(
        self,
        db: DatabaseStorage,
        retention_days: int = 30,
    ):
        """
        Initialize aggregation task.

        Args:
            db: Database manager instance
            retention_days: Number of days to retain detailed usage_records
        """
        self.db = db
        self.retention_days = retention_days

    async def run_daily_aggregation(self, target_date: datetime | None = None) -> dict:
        """
        Aggregate usage records for a specific date into daily aggregates.

        Args:
            target_date: Date to aggregate (defaults to yesterday)

        Returns:
            Summary of aggregation results
        """
        if target_date is None:
            target_date = datetime.utcnow().date() - timedelta(days=1)
        elif isinstance(target_date, datetime):
            target_date = target_date.date()

        logger.info(f"Starting daily aggregation for {target_date}")

        async with self.db._pool.acquire() as conn:
            # Aggregate by tenant, user, model, assistant, service
            # Use COALESCE to convert NULLs to empty strings for proper UNIQUE constraint matching
            # The constraint uq_usage_daily_aggregates_dimensions requires non-NULL values
            aggregation_query = """
                INSERT INTO usage_daily_aggregates (
                    tenant_id, user_id, model, assistant_id, service_id,
                    date, request_count, success_count, error_count,
                    total_input_tokens, total_output_tokens, total_cost_cents,
                    avg_latency_ms, p95_latency_ms
                )
                SELECT
                    tenant_id,
                    COALESCE(user_id, '') as user_id,
                    COALESCE(model, '') as model,
                    COALESCE(assistant_id, '') as assistant_id,
                    COALESCE(service_id, '') as service_id,
                    $1::date as date,
                    COUNT(*) as request_count,
                    COUNT(*) FILTER (WHERE status = 'success') as success_count,
                    COUNT(*) FILTER (WHERE status != 'success') as error_count,
                    COALESCE(SUM(input_tokens), 0) as total_input_tokens,
                    COALESCE(SUM(output_tokens), 0) as total_output_tokens,
                    COALESCE(SUM(total_cost_cents), 0) as total_cost_cents,
                    COALESCE(AVG(latency_ms)::integer, 0) as avg_latency_ms,
                    COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY latency_ms)::integer, 0) as p95_latency_ms
                FROM usage_records
                WHERE created_at >= $1::date
                  AND created_at < ($1::date + interval '1 day')
                GROUP BY tenant_id, COALESCE(user_id, ''), COALESCE(model, ''), COALESCE(assistant_id, ''), COALESCE(service_id, '')
                ON CONFLICT (tenant_id, user_id, model, assistant_id, service_id, date)
                DO UPDATE SET
                    request_count = EXCLUDED.request_count,
                    success_count = EXCLUDED.success_count,
                    error_count = EXCLUDED.error_count,
                    total_input_tokens = EXCLUDED.total_input_tokens,
                    total_output_tokens = EXCLUDED.total_output_tokens,
                    total_cost_cents = EXCLUDED.total_cost_cents,
                    avg_latency_ms = EXCLUDED.avg_latency_ms,
                    p95_latency_ms = EXCLUDED.p95_latency_ms,
                    updated_at = CURRENT_TIMESTAMP
            """

            result = await conn.execute(aggregation_query, target_date)

            # Parse result to get row count
            rows_affected = int(result.split()[-1]) if result else 0

            logger.info(
                f"Aggregation complete for {target_date}: {rows_affected} dimension combinations"
            )

            return {
                "date": str(target_date),
                "aggregations_created": rows_affected,
                "status": "success",
            }

    async def cleanup_old_records(self) -> dict:
        """
        Delete usage_records older than retention period.

        Returns:
            Summary of cleanup results
        """
        cutoff_date = datetime.utcnow() - timedelta(days=self.retention_days)

        logger.info(f"Starting cleanup of records older than {cutoff_date.date()}")

        async with self.db._pool.acquire() as conn:
            # Delete old records in batches to avoid long locks
            total_deleted = 0
            batch_size = 10000

            while True:
                result = await conn.execute(
                    """
                    DELETE FROM usage_records
                    WHERE id IN (
                        SELECT id FROM usage_records
                        WHERE created_at < $1
                        LIMIT $2
                    )
                    """,
                    cutoff_date,
                    batch_size,
                )

                deleted_count = int(result.split()[-1]) if result else 0
                total_deleted += deleted_count

                if deleted_count < batch_size:
                    break

                # Brief pause between batches
                await asyncio.sleep(0.1)

            logger.info(f"Cleanup complete: {total_deleted} old records deleted")

            return {
                "cutoff_date": str(cutoff_date.date()),
                "records_deleted": total_deleted,
                "status": "success",
            }

    async def run_full_maintenance(self) -> dict:
        """
        Run both aggregation and cleanup tasks.

        Returns:
            Combined summary of all maintenance tasks
        """
        results = {}

        # Run aggregation first
        try:
            results["aggregation"] = await self.run_daily_aggregation()
        except Exception as e:
            logger.error(f"Aggregation failed: {e}")
            results["aggregation"] = {"status": "error", "error": str(e)}

        # Then cleanup
        try:
            results["cleanup"] = await self.cleanup_old_records()
        except Exception as e:
            logger.error(f"Cleanup failed: {e}")
            results["cleanup"] = {"status": "error", "error": str(e)}

        return results


class QuotaResetTask:
    """
    Handles periodic quota resets for users.
    """

    def __init__(self, db: DatabaseStorage):
        self.db = db

    async def reset_daily_quotas(self) -> dict:
        """
        Reset daily token usage for all users.

        Returns:
            Summary of reset operation
        """
        logger.info("Starting daily quota reset")

        async with self.db._pool.acquire() as conn:
            result = await conn.execute(
                """
                UPDATE user_quotas
                SET
                    current_daily_tokens = 0,
                    daily_reset_at = CURRENT_TIMESTAMP,
                    updated_at = CURRENT_TIMESTAMP
                WHERE daily_reset_at < CURRENT_DATE
                   OR daily_reset_at IS NULL
                """
            )

            rows_affected = int(result.split()[-1]) if result else 0

            logger.info(f"Daily quota reset complete: {rows_affected} users")

            return {"users_reset": rows_affected, "reset_type": "daily", "status": "success"}

    async def reset_monthly_quotas(self) -> dict:
        """
        Reset monthly cost usage for all users (run on 1st of month).

        Returns:
            Summary of reset operation
        """
        logger.info("Starting monthly quota reset")

        async with self.db._pool.acquire() as conn:
            result = await conn.execute(
                """
                UPDATE user_quotas
                SET
                    current_monthly_cost_cents = 0,
                    monthly_reset_at = CURRENT_TIMESTAMP,
                    updated_at = CURRENT_TIMESTAMP
                WHERE monthly_reset_at < DATE_TRUNC('month', CURRENT_DATE)
                   OR monthly_reset_at IS NULL
                """
            )

            rows_affected = int(result.split()[-1]) if result else 0

            logger.info(f"Monthly quota reset complete: {rows_affected} users")

            return {"users_reset": rows_affected, "reset_type": "monthly", "status": "success"}

    async def check_and_reset_quotas(self) -> dict:
        """
        Check if quota resets are needed and perform them.

        This can be called frequently (e.g., every hour) and will
        only reset quotas that actually need resetting.

        Returns:
            Summary of any resets performed
        """
        results = {}

        # Always check daily resets
        try:
            results["daily"] = await self.reset_daily_quotas()
        except Exception as e:
            logger.error(f"Daily quota reset failed: {e}")
            results["daily"] = {"status": "error", "error": str(e)}

        # Check if it's the first day of the month for monthly reset
        if datetime.utcnow().day == 1:
            try:
                results["monthly"] = await self.reset_monthly_quotas()
            except Exception as e:
                logger.error(f"Monthly quota reset failed: {e}")
                results["monthly"] = {"status": "error", "error": str(e)}

        return results


# Singleton instances
_aggregation_task: AggregationTask | None = None
_quota_reset_task: QuotaResetTask | None = None


def init_aggregation_task(db: DatabaseStorage, retention_days: int = 30) -> AggregationTask:
    """Initialize the aggregation task singleton."""
    global _aggregation_task
    _aggregation_task = AggregationTask(db, retention_days)
    return _aggregation_task


def get_aggregation_task() -> AggregationTask | None:
    """Get the aggregation task singleton."""
    return _aggregation_task


def init_quota_reset_task(db: DatabaseStorage) -> QuotaResetTask:
    """Initialize the quota reset task singleton."""
    global _quota_reset_task
    _quota_reset_task = QuotaResetTask(db)
    return _quota_reset_task


def get_quota_reset_task() -> QuotaResetTask | None:
    """Get the quota reset task singleton."""
    return _quota_reset_task
