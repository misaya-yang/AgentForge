"""
Quota Service - User quota management and enforcement.

This service handles:
- Checking user quotas before requests
- Updating usage after requests
- Daily and monthly quota resets
- Quota alerts and blocking
"""

from __future__ import annotations

import asyncio
import inspect
import logging
import math
import time
from collections import deque
from contextlib import asynccontextmanager
from dataclasses import dataclass
from datetime import date, datetime, timedelta, timezone
from enum import Enum
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from ...persistence.database import DatabaseStorage

logger = logging.getLogger(__name__)

# Global singleton instance
_quota_service: QuotaService | None = None


class QuotaStatus(str, Enum):
    """Quota check status."""

    OK = "ok"
    WARNING = "warning"
    EXCEEDED = "exceeded"
    BLOCKED = "blocked"


class OverageStrategy(str, Enum):
    """Policy when quota is exceeded."""

    HARD_BLOCK = "hard_block"
    RATE_LIMIT = "rate_limit"
    DOWNGRADE_MODEL = "downgrade_model"
    ALLOW_BUT_ALERT = "allow_but_alert"


@dataclass
class QuotaCheckResult:
    """Result of a quota check."""

    status: QuotaStatus
    message: str = ""
    daily_tokens_used: int = 0
    daily_tokens_limit: int | None = None
    monthly_cost_used: int = 0  # in cents
    monthly_cost_limit: int | None = None  # in cents
    daily_requests_used: int = 0
    daily_requests_limit: int | None = None
    requests_per_minute_used: int = 0
    requests_per_minute_limit: int | None = None
    retry_after_seconds: int = 0
    warning_threshold: int = 80
    overage_strategy: OverageStrategy = OverageStrategy.ALLOW_BUT_ALERT
    downgraded_model: str | None = None

    @property
    def can_proceed(self) -> bool:
        """Whether the request can proceed."""
        if self.status in (QuotaStatus.OK, QuotaStatus.WARNING):
            return True
        return self.status == QuotaStatus.EXCEEDED and self.overage_strategy in {
            OverageStrategy.ALLOW_BUT_ALERT,
            OverageStrategy.DOWNGRADE_MODEL,
        }

    @property
    def daily_tokens_percentage(self) -> float:
        """Percentage of daily token quota used."""
        if not self.daily_tokens_limit:
            return 0.0
        return round(self.daily_tokens_used / self.daily_tokens_limit * 100, 1)

    @property
    def monthly_cost_percentage(self) -> float:
        """Percentage of monthly cost quota used."""
        if not self.monthly_cost_limit:
            return 0.0
        return round(self.monthly_cost_used / self.monthly_cost_limit * 100, 1)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary."""
        return {
            "status": self.status.value,
            "message": self.message,
            "can_proceed": self.can_proceed,
            "daily_tokens": {
                "used": self.daily_tokens_used,
                "limit": self.daily_tokens_limit,
                "percentage": self.daily_tokens_percentage,
            },
            "monthly_cost": {
                "used_cents": self.monthly_cost_used,
                "used_usd": round(self.monthly_cost_used / 100, 2),
                "limit_cents": self.monthly_cost_limit,
                "limit_usd": round(self.monthly_cost_limit / 100, 2)
                if self.monthly_cost_limit
                else None,
                "percentage": self.monthly_cost_percentage,
            },
            "daily_requests": {
                "used": self.daily_requests_used,
                "limit": self.daily_requests_limit,
            },
            "minute_requests": {
                "used": self.requests_per_minute_used,
                "limit": self.requests_per_minute_limit,
                "retry_after_seconds": self.retry_after_seconds,
            },
            "policy": {
                "overage_strategy": self.overage_strategy.value,
                "downgraded_model": self.downgraded_model,
            },
        }


@dataclass
class UserQuota:
    """User quota configuration and current usage."""

    tenant_id: str
    user_id: str
    daily_token_limit: int | None = None
    monthly_token_limit: int | None = None
    monthly_cost_limit_cents: int | None = None
    requests_per_minute: int | None = None
    requests_per_day: int | None = None
    current_daily_tokens: int = 0
    current_monthly_tokens: int = 0
    current_monthly_cost_cents: int = 0
    current_daily_requests: int = 0
    daily_reset_at: datetime | None = None
    monthly_reset_at: datetime | None = None
    is_blocked: bool = False
    blocked_reason: str | None = None
    warning_threshold: int = 80
    overage_strategy: OverageStrategy = OverageStrategy.ALLOW_BUT_ALERT
    downgraded_model: str | None = None
    temporary_extra_tokens: int = 0
    temporary_extra_cost_cents: int = 0
    temporary_expires_at: datetime | None = None


def resolve_effective_quota_limits(
    *,
    daily_token_limit: int | None,
    monthly_token_limit: int | None,
    monthly_cost_limit_cents: int | None,
    temporary_extra_tokens: int,
    temporary_extra_cost_cents: int,
    temporary_expires_at: datetime | None,
    now: datetime,
) -> dict[str, Any]:
    """Pure limit observation shared by admission and read-only overview.

    Preserve stored zero semantics: an active boost can make zero finite, while
    None always stays unlimited. No normalization, usage reservation or I/O.
    """
    active = bool(temporary_expires_at and temporary_expires_at > now)
    tokens = temporary_extra_tokens if active else 0
    cost = temporary_extra_cost_cents if active else 0
    effective = {
        "daily_tokens": daily_token_limit + tokens if daily_token_limit is not None else None,
        "monthly_tokens": monthly_token_limit + tokens if monthly_token_limit is not None else None,
        "monthly_cost_cents": (
            monthly_cost_limit_cents + cost if monthly_cost_limit_cents is not None else None
        ),
    }
    sources = {
        name: (
            "unlimited" if value is None or value == 0
            else "configured_plus_temporary_boost" if extra != 0
            else "configured"
        )
        for name, value, extra in (
            ("daily_tokens", effective["daily_tokens"], tokens),
            ("monthly_tokens", effective["monthly_tokens"], tokens),
            ("monthly_cost_cents", effective["monthly_cost_cents"], cost),
        )
    }
    return {
        "observed_at": now.isoformat(),
        "effective": effective,
        "sources": sources,
        "temporary_boost": {
            "extra_tokens": temporary_extra_tokens,
            "extra_cost_cents": temporary_extra_cost_cents,
            "expires_at": temporary_expires_at.isoformat() if temporary_expires_at else None,
            "active": active,
        },
    }


class QuotaService:
    """
    Service for managing user quotas and usage limits.

    Features:
    - Pre-request quota checking
    - Post-request usage updates
    - Automatic daily and monthly resets
    - Alert generation for approaching limits
    """

    def __init__(self, database: DatabaseStorage | None = None):
        self.database = database
        self._rpm_requests: dict[tuple[str, str], deque[float]] = {}
        self._rpm_lock = asyncio.Lock()

    def set_database(self, database: DatabaseStorage) -> None:
        """Set or update the database storage instance."""
        self.database = database

    def _get_pool(self):
        if not self.database:
            return None
        # Prefer explicitly assigned attributes (works with both real DatabaseStorage and test doubles).
        if hasattr(self.database, "__dict__"):
            for attr in ("_pool", "pool"):
                if attr in self.database.__dict__:
                    pool = self.database.__dict__.get(attr)
                    if pool is not None:
                        return pool
        return getattr(self.database, "_pool", None) or getattr(self.database, "pool", None)

    @asynccontextmanager
    async def _acquire_connection(self, pool):
        """Acquire DB connection compatible with asyncpg pool and test doubles."""
        acquired = pool.acquire()
        if hasattr(acquired, "__aenter__"):
            async with acquired as conn:
                yield conn
            return

        if inspect.isawaitable(acquired):
            acquired = await acquired

        if hasattr(acquired, "__aenter__"):
            async with acquired as conn:
                yield conn
            return

        conn = acquired
        try:
            yield conn
        finally:
            release = getattr(pool, "release", None)
            if callable(release):
                released = release(conn)
                if inspect.isawaitable(released):
                    await released

    @staticmethod
    def _normalize_limit(value: int | None) -> int | None:
        """Normalize quota limits: treat 0 as unlimited (None)."""
        if value is None:
            return None
        parsed = int(value)
        return parsed if parsed > 0 else None

    @staticmethod
    def _coerce_overage_strategy(value: Any) -> OverageStrategy:
        if isinstance(value, OverageStrategy):
            return value
        try:
            return OverageStrategy(str(value or "").strip())
        except Exception:
            return OverageStrategy.ALLOW_BUT_ALERT

    async def _check_requests_per_minute(
        self,
        *,
        tenant_id: str,
        user_id: str,
        limit: int | None,
    ) -> tuple[bool, int, int]:
        """Single-node RPM pre-check for quota governance."""
        normalized_limit = self._normalize_limit(limit)
        if normalized_limit is None:
            return True, 0, 0

        now = time.monotonic()
        window_start = now - 60
        key = (tenant_id, user_id)

        async with self._rpm_lock:
            timestamps = self._rpm_requests.setdefault(key, deque())
            while timestamps and timestamps[0] < window_start:
                timestamps.popleft()

            current_count = len(timestamps)
            if current_count >= normalized_limit:
                retry_after = int(max((timestamps[0] + 60) - now, 1)) if timestamps else 60
                return False, current_count, retry_after

            timestamps.append(now)
            return True, current_count + 1, 0

    async def _record_quota_exceeded_event(
        self,
        tenant_id: str,
        user_id: str,
    ) -> None:
        """Record a quota_exceeded security event for dashboard visibility."""
        try:
            from ...services.metrics.security_event_recorder import get_security_event_recorder

            recorder = get_security_event_recorder()
            await recorder.record_event(
                tenant_id=tenant_id,
                user_id=user_id,
                service_id=None,
                event_type="quota_exceeded",
            )
        except Exception as e:
            logger.debug(f"Failed to record quota_exceeded event: {e}")

    async def check_quota(
        self,
        tenant_id: str,
        user_id: str,
        estimated_tokens: int = 0,
        record_security_event: bool = True,
    ) -> QuotaCheckResult:
        """
        Check if user has sufficient quota for a request.

        Args:
            tenant_id: Tenant ID
            user_id: User ID
            estimated_tokens: Estimated tokens for this request

        Returns:
            QuotaCheckResult with status and details
        """
        quota = await self._get_or_create_quota(tenant_id, user_id)

        if not quota:
            # No quota record means unlimited
            return QuotaCheckResult(status=QuotaStatus.OK, message="No quota restrictions")

        # Check if blocked
        if quota.is_blocked:
            if record_security_event:
                await self._record_quota_exceeded_event(tenant_id, user_id)
            return QuotaCheckResult(
                status=QuotaStatus.BLOCKED,
                message=f"User is blocked: {quota.blocked_reason or 'Unknown reason'}",
                requests_per_minute_limit=quota.requests_per_minute,
                overage_strategy=quota.overage_strategy,
                downgraded_model=quota.downgraded_model,
            )

        now = datetime.now(timezone.utc)
        observation = resolve_effective_quota_limits(
            daily_token_limit=quota.daily_token_limit,
            monthly_token_limit=quota.monthly_token_limit,
            monthly_cost_limit_cents=quota.monthly_cost_limit_cents,
            temporary_extra_tokens=quota.temporary_extra_tokens,
            temporary_extra_cost_cents=quota.temporary_extra_cost_cents,
            temporary_expires_at=quota.temporary_expires_at,
            now=now,
        )
        effective_daily_token_limit = observation["effective"]["daily_tokens"]
        effective_monthly_cost_limit = observation["effective"]["monthly_cost_cents"]
        effective_monthly_token_limit = observation["effective"]["monthly_tokens"]

        def _result(
            status: QuotaStatus,
            message: str,
            *,
            rpm_used: int = 0,
            retry_after_seconds: int = 0,
        ) -> QuotaCheckResult:
            return QuotaCheckResult(
                status=status,
                message=message,
                daily_tokens_used=quota.current_daily_tokens,
                daily_tokens_limit=effective_daily_token_limit,
                monthly_cost_used=quota.current_monthly_cost_cents,
                monthly_cost_limit=effective_monthly_cost_limit,
                daily_requests_used=quota.current_daily_requests,
                daily_requests_limit=quota.requests_per_day,
                requests_per_minute_used=rpm_used,
                requests_per_minute_limit=quota.requests_per_minute,
                retry_after_seconds=retry_after_seconds,
                warning_threshold=quota.warning_threshold,
                overage_strategy=quota.overage_strategy,
                downgraded_model=quota.downgraded_model,
            )

        async def _apply_overage_policy(
            base_message: str,
            *,
            rpm_used: int = 0,
            retry_after_seconds: int = 0,
        ) -> QuotaCheckResult:
            strategy = quota.overage_strategy
            # Record quota_exceeded security event
            if record_security_event:
                await self._record_quota_exceeded_event(tenant_id, user_id)

            if strategy == OverageStrategy.HARD_BLOCK:
                return _result(
                    QuotaStatus.BLOCKED,
                    base_message,
                    rpm_used=rpm_used,
                    retry_after_seconds=retry_after_seconds,
                )
            if strategy == OverageStrategy.RATE_LIMIT:
                return _result(
                    QuotaStatus.EXCEEDED,
                    f"{base_message}; strategy=rate_limit",
                    rpm_used=rpm_used,
                    retry_after_seconds=retry_after_seconds,
                )
            if strategy == OverageStrategy.DOWNGRADE_MODEL:
                model_msg = (
                    f"; downgrade={quota.downgraded_model}" if quota.downgraded_model else ""
                )
                return _result(
                    QuotaStatus.EXCEEDED,
                    f"{base_message}; strategy=downgrade_model{model_msg}",
                    rpm_used=rpm_used,
                    retry_after_seconds=retry_after_seconds,
                )
            return _result(
                QuotaStatus.EXCEEDED,
                f"{base_message}; strategy=allow_but_alert",
                rpm_used=rpm_used,
                retry_after_seconds=retry_after_seconds,
            )

        # Check daily token limit
        if (
            effective_daily_token_limit
            and quota.current_daily_tokens + estimated_tokens > effective_daily_token_limit
        ):
            return await _apply_overage_policy("Daily token limit exceeded")

        # Check monthly token limit
        if (
            effective_monthly_token_limit
            and quota.current_monthly_tokens + estimated_tokens > effective_monthly_token_limit
        ):
            return await _apply_overage_policy("Monthly token limit exceeded")

        # Check monthly cost limit
        if (
            effective_monthly_cost_limit
            and quota.current_monthly_cost_cents >= effective_monthly_cost_limit
        ):
            return await _apply_overage_policy("Monthly cost limit exceeded")

        # Check daily request limit
        if quota.requests_per_day and quota.current_daily_requests >= quota.requests_per_day:
            return await _apply_overage_policy("Daily request limit exceeded")

        rpm_allowed, rpm_used, rpm_retry_after = await self._check_requests_per_minute(
            tenant_id=tenant_id,
            user_id=user_id,
            limit=quota.requests_per_minute,
        )
        if not rpm_allowed:
            return await _apply_overage_policy(
                "Minute request limit exceeded",
                rpm_used=rpm_used,
                retry_after_seconds=rpm_retry_after,
            )

        # Check warning threshold
        warning_messages = []

        if effective_daily_token_limit:
            pct = quota.current_daily_tokens / effective_daily_token_limit * 100
            if pct >= quota.warning_threshold:
                warning_messages.append(f"Daily token usage at {pct:.1f}%")

        if effective_monthly_cost_limit:
            pct = quota.current_monthly_cost_cents / effective_monthly_cost_limit * 100
            if pct >= quota.warning_threshold:
                warning_messages.append(f"Monthly cost at {pct:.1f}%")

        if warning_messages:
            return _result(QuotaStatus.WARNING, "; ".join(warning_messages), rpm_used=rpm_used)

        return _result(QuotaStatus.OK, "Quota check passed", rpm_used=rpm_used)

    async def update_usage(
        self,
        tenant_id: str,
        user_id: str,
        tokens_used: int,
        cost_cents: int,
    ) -> None:
        """
        Update user's usage after a request.

        Args:
            tenant_id: Tenant ID
            user_id: User ID
            tokens_used: Tokens consumed
            cost_cents: Cost in cents
        """
        pool = self._get_pool()
        if not pool:
            return

        try:
            async with self._acquire_connection(pool) as conn:
                await conn.execute(
                    """
                    UPDATE user_quotas
                    SET
                        current_daily_tokens = current_daily_tokens + $3,
                        current_monthly_tokens = current_monthly_tokens + $3,
                        current_monthly_cost_cents = current_monthly_cost_cents + $4,
                        current_daily_requests = current_daily_requests + 1,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE tenant_id = $1 AND user_id = $2
                    """,
                    tenant_id,
                    user_id,
                    tokens_used,
                    cost_cents,
                )
        except Exception as e:
            logger.error(f"Failed to update user quota usage: {e}")

    async def get_user_quota(
        self,
        tenant_id: str,
        user_id: str,
    ) -> dict[str, Any] | None:
        """Get user quota details."""
        quota = await self._get_or_create_quota(tenant_id, user_id)
        if not quota:
            return None

        return {
            "tenant_id": quota.tenant_id,
            "user_id": quota.user_id,
            "limits": {
                "daily_tokens": quota.daily_token_limit,
                "monthly_tokens": quota.monthly_token_limit,
                "monthly_cost_cents": quota.monthly_cost_limit_cents,
                "requests_per_minute": quota.requests_per_minute,
                "requests_per_day": quota.requests_per_day,
            },
            "current_usage": {
                "daily_tokens": quota.current_daily_tokens,
                "monthly_tokens": quota.current_monthly_tokens,
                "monthly_cost_cents": quota.current_monthly_cost_cents,
                "daily_requests": quota.current_daily_requests,
            },
            "status": {
                "is_blocked": quota.is_blocked,
                "blocked_reason": quota.blocked_reason,
            },
            "policy": {
                "overage_strategy": quota.overage_strategy.value,
                "downgraded_model": quota.downgraded_model,
            },
            "resets": {
                "daily_reset_at": quota.daily_reset_at.isoformat()
                if quota.daily_reset_at
                else None,
                "monthly_reset_at": quota.monthly_reset_at.isoformat()
                if quota.monthly_reset_at
                else None,
            },
            "temporary_boost": {
                "extra_tokens": quota.temporary_extra_tokens,
                "extra_cost_cents": quota.temporary_extra_cost_cents,
                "expires_at": quota.temporary_expires_at.isoformat()
                if quota.temporary_expires_at
                else None,
            },
            "warning_threshold": quota.warning_threshold,
        }

    @staticmethod
    def _projected_breach_date(
        *,
        current_value: float,
        limit_value: float | None,
        average_daily_delta: float,
        today: date,
        days_remaining: int,
    ) -> str | None:
        if not limit_value or limit_value <= 0:
            return None
        if current_value >= limit_value:
            return today.isoformat()
        if average_daily_delta <= 0:
            return None

        days_to_limit = math.ceil((limit_value - current_value) / average_daily_delta)
        if days_to_limit <= 0:
            return today.isoformat()
        if days_to_limit > days_remaining:
            return None
        return (today + timedelta(days=days_to_limit - 1)).isoformat()

    async def get_quota_forecast(
        self,
        tenant_id: str,
        user_id: str,
        lookback_days: int = 7,
    ) -> dict[str, Any]:
        """
        Predict month-end quota usage based on recent daily trend.

        Forecast uses usage_daily_aggregates for the lookback window.
        """
        pool = self._get_pool()
        if not pool:
            return {"error": "Database not available"}

        quota = await self._get_or_create_quota(tenant_id, user_id)
        if not quota:
            return {"error": "Quota not found"}

        lookback_days = max(int(lookback_days), 1)
        today = datetime.now(timezone.utc).date()
        month_start = today.replace(day=1)
        next_month = (month_start + timedelta(days=32)).replace(day=1)
        days_in_month = (next_month - month_start).days
        days_elapsed = max((today - month_start).days + 1, 1)
        days_remaining = max(days_in_month - days_elapsed, 0)
        window_start = today - timedelta(days=lookback_days - 1)

        try:
            async with self._acquire_connection(pool) as conn:
                month_row = await conn.fetchrow(
                    """
                    SELECT
                        COALESCE(SUM(total_input_tokens + total_output_tokens), 0) AS month_tokens,
                        COALESCE(SUM(total_cost_cents), 0) AS month_cost_microcents
                    FROM usage_daily_aggregates
                    WHERE tenant_id = $1 AND user_id = $2
                      AND date >= $3 AND date <= $4
                    """,
                    tenant_id,
                    user_id,
                    month_start,
                    today,
                )
                recent_row = await conn.fetchrow(
                    """
                    SELECT
                        COALESCE(SUM(total_input_tokens + total_output_tokens), 0) AS recent_tokens,
                        COALESCE(SUM(total_cost_cents), 0) AS recent_cost_microcents
                    FROM usage_daily_aggregates
                    WHERE tenant_id = $1 AND user_id = $2
                      AND date >= $3 AND date <= $4
                    """,
                    tenant_id,
                    user_id,
                    window_start,
                    today,
                )
        except Exception as e:
            logger.error(f"Failed to forecast quota for {tenant_id}/{user_id}: {e}")
            return {"error": str(e)}

        month_tokens = int((month_row or {}).get("month_tokens", 0) or 0)
        month_cost_cents = float((month_row or {}).get("month_cost_microcents", 0) or 0) / 10000
        recent_tokens = int((recent_row or {}).get("recent_tokens", 0) or 0)
        recent_cost_cents = float((recent_row or {}).get("recent_cost_microcents", 0) or 0) / 10000

        avg_daily_tokens = recent_tokens / lookback_days
        avg_daily_cost_cents = recent_cost_cents / lookback_days

        projected_tokens = int(round(month_tokens + avg_daily_tokens * days_remaining))
        projected_cost_cents = int(round(month_cost_cents + avg_daily_cost_cents * days_remaining))

        now = datetime.now(timezone.utc)
        temporary_active = bool(quota.temporary_expires_at and quota.temporary_expires_at > now)
        extra_tokens = quota.temporary_extra_tokens if temporary_active else 0
        extra_cost = quota.temporary_extra_cost_cents if temporary_active else 0

        effective_monthly_token_limit = (
            (quota.monthly_token_limit + extra_tokens)
            if quota.monthly_token_limit is not None
            else None
        )
        effective_monthly_cost_limit_cents = (
            (quota.monthly_cost_limit_cents + extra_cost)
            if quota.monthly_cost_limit_cents is not None
            else None
        )

        token_breach_date = self._projected_breach_date(
            current_value=float(month_tokens),
            limit_value=float(effective_monthly_token_limit)
            if effective_monthly_token_limit is not None
            else None,
            average_daily_delta=avg_daily_tokens,
            today=today,
            days_remaining=days_remaining,
        )
        cost_breach_date = self._projected_breach_date(
            current_value=month_cost_cents,
            limit_value=float(effective_monthly_cost_limit_cents)
            if effective_monthly_cost_limit_cents is not None
            else None,
            average_daily_delta=avg_daily_cost_cents,
            today=today,
            days_remaining=days_remaining,
        )

        return {
            "tenant_id": tenant_id,
            "user_id": user_id,
            "window": {
                "month_start": month_start.isoformat(),
                "today": today.isoformat(),
                "days_elapsed": days_elapsed,
                "days_remaining": days_remaining,
                "lookback_days": lookback_days,
            },
            "tokens": {
                "current": month_tokens,
                "avg_daily": round(avg_daily_tokens, 2),
                "projected_month_end": projected_tokens,
                "limit": effective_monthly_token_limit,
                "projected_usage_pct": round(
                    projected_tokens / effective_monthly_token_limit * 100, 2
                )
                if effective_monthly_token_limit
                else None,
                "predicted_breach_date": token_breach_date,
            },
            "cost": {
                "current_cents": round(month_cost_cents, 2),
                "current_usd": round(month_cost_cents / 100, 4),
                "avg_daily_cents": round(avg_daily_cost_cents, 2),
                "projected_month_end_cents": projected_cost_cents,
                "projected_month_end_usd": round(projected_cost_cents / 100, 4),
                "limit_cents": effective_monthly_cost_limit_cents,
                "limit_usd": round(effective_monthly_cost_limit_cents / 100, 4)
                if effective_monthly_cost_limit_cents
                else None,
                "projected_usage_pct": round(
                    projected_cost_cents / effective_monthly_cost_limit_cents * 100, 2
                )
                if effective_monthly_cost_limit_cents
                else None,
                "predicted_breach_date": cost_breach_date,
            },
        }

    async def set_user_quota(
        self,
        tenant_id: str,
        user_id: str,
        daily_token_limit: int | None = None,
        monthly_token_limit: int | None = None,
        monthly_cost_limit_cents: int | None = None,
        requests_per_minute: int | None = None,
        requests_per_day: int | None = None,
        warning_threshold: int = 80,
        overage_strategy: OverageStrategy = OverageStrategy.ALLOW_BUT_ALERT,
        downgraded_model: str | None = None,
        temporary_extra_tokens: int | None = None,
        temporary_extra_cost_cents: int | None = None,
        temporary_expires_at: datetime | None = None,
    ) -> dict[str, Any]:
        """Set or update user quota limits."""
        pool = self._get_pool()
        if not pool:
            return {"error": "Database not available"}

        try:
            daily_token_limit = self._normalize_limit(daily_token_limit)
            monthly_token_limit = self._normalize_limit(monthly_token_limit)
            monthly_cost_limit_cents = self._normalize_limit(monthly_cost_limit_cents)
            requests_per_minute = self._normalize_limit(requests_per_minute)
            requests_per_day = self._normalize_limit(requests_per_day)
            strategy = self._coerce_overage_strategy(overage_strategy)

            now = datetime.now(timezone.utc)
            tomorrow = (now + timedelta(days=1)).replace(hour=0, minute=0, second=0, microsecond=0)
            next_month = (now.replace(day=1) + timedelta(days=32)).replace(
                day=1, hour=0, minute=0, second=0, microsecond=0
            )

            async with self._acquire_connection(pool) as conn:
                await conn.execute(
                    """
                    INSERT INTO user_quotas (
                        tenant_id, user_id,
                        daily_token_limit, monthly_token_limit, monthly_cost_limit_cents,
                        requests_per_minute, requests_per_day,
                        warning_threshold, overage_strategy, downgraded_model,
                        temporary_extra_tokens, temporary_extra_cost_cents, temporary_expires_at,
                        daily_reset_at, monthly_reset_at
                    ) VALUES (
                        $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                        $13::timestamptz, $14::timestamptz, $15::timestamptz
                    )
                    ON CONFLICT (tenant_id, user_id) DO UPDATE SET
                        daily_token_limit = COALESCE($3, user_quotas.daily_token_limit),
                        monthly_token_limit = COALESCE($4, user_quotas.monthly_token_limit),
                        monthly_cost_limit_cents = COALESCE($5, user_quotas.monthly_cost_limit_cents),
                        requests_per_minute = COALESCE($6, user_quotas.requests_per_minute),
                        requests_per_day = COALESCE($7, user_quotas.requests_per_day),
                        warning_threshold = $8,
                        overage_strategy = $9,
                        downgraded_model = COALESCE($10, user_quotas.downgraded_model),
                        temporary_extra_tokens = COALESCE($11, user_quotas.temporary_extra_tokens),
                        temporary_extra_cost_cents = COALESCE($12, user_quotas.temporary_extra_cost_cents),
                        temporary_expires_at = COALESCE($13::timestamptz, user_quotas.temporary_expires_at),
                        updated_at = CURRENT_TIMESTAMP
                    """,
                    tenant_id,
                    user_id,
                    daily_token_limit,
                    monthly_token_limit,
                    monthly_cost_limit_cents,
                    requests_per_minute,
                    requests_per_day,
                    warning_threshold,
                    strategy.value,
                    downgraded_model,
                    temporary_extra_tokens,
                    temporary_extra_cost_cents,
                    temporary_expires_at,
                    tomorrow,
                    next_month,
                )

            return await self.get_user_quota(tenant_id, user_id) or {}

        except Exception as e:
            logger.error(f"Failed to set user quota: {e}")
            return {"error": str(e)}

    async def reset_daily_quotas(self) -> int:
        """Reset daily quotas for all users. Returns count of reset users."""
        pool = self._get_pool()
        if not pool:
            return 0

        try:
            now = datetime.now(timezone.utc)
            tomorrow = (now + timedelta(days=1)).replace(hour=0, minute=0, second=0, microsecond=0)

            async with self._acquire_connection(pool) as conn:
                result = await conn.execute(
                    """
                    UPDATE user_quotas
                    SET
                        current_daily_tokens = 0,
                        current_daily_requests = 0,
                        daily_reset_at = $1::timestamptz,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE daily_reset_at IS NULL OR daily_reset_at <= $2::timestamptz
                    """,
                    tomorrow,
                    now,
                )
                count = int(result.split()[-1])
                logger.info(f"Reset daily quotas for {count} users")
                return count

        except Exception as e:
            logger.error(f"Failed to reset daily quotas: {e}")
            return 0

    async def reset_monthly_quotas(self) -> int:
        """Reset monthly quotas for all users. Returns count of reset users."""
        pool = self._get_pool()
        if not pool:
            return 0

        try:
            now = datetime.now(timezone.utc)
            next_month = (now.replace(day=1) + timedelta(days=32)).replace(
                day=1, hour=0, minute=0, second=0, microsecond=0
            )

            async with self._acquire_connection(pool) as conn:
                result = await conn.execute(
                    """
                    UPDATE user_quotas
                    SET
                        current_monthly_tokens = 0,
                        current_monthly_cost_cents = 0,
                        monthly_reset_at = $1::timestamptz,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE monthly_reset_at IS NULL OR monthly_reset_at <= $2::timestamptz
                    """,
                    next_month,
                    now,
                )
                count = int(result.split()[-1])
                logger.info(f"Reset monthly quotas for {count} users")
                return count

        except Exception as e:
            logger.error(f"Failed to reset monthly quotas: {e}")
            return 0

    async def block_user(
        self,
        tenant_id: str,
        user_id: str,
        reason: str,
    ) -> bool:
        """Block a user from making requests."""
        pool = self._get_pool()
        if not pool:
            return False

        try:
            async with self._acquire_connection(pool) as conn:
                await conn.execute(
                    """
                    UPDATE user_quotas
                    SET
                        is_blocked = TRUE,
                        blocked_reason = $3,
                        blocked_at = CURRENT_TIMESTAMP,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE tenant_id = $1 AND user_id = $2
                    """,
                    tenant_id,
                    user_id,
                    reason,
                )
                return True

        except Exception as e:
            logger.error(f"Failed to block user: {e}")
            return False

    async def unblock_user(
        self,
        tenant_id: str,
        user_id: str,
    ) -> bool:
        """Unblock a user."""
        pool = self._get_pool()
        if not pool:
            return False

        try:
            async with self._acquire_connection(pool) as conn:
                await conn.execute(
                    """
                    UPDATE user_quotas
                    SET
                        is_blocked = FALSE,
                        blocked_reason = NULL,
                        blocked_at = NULL,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE tenant_id = $1 AND user_id = $2
                    """,
                    tenant_id,
                    user_id,
                )
                return True

        except Exception as e:
            logger.error(f"Failed to unblock user: {e}")
            return False

    async def get_quota_alerts(
        self,
        tenant_id: str,
        limit: int = 50,
        unacknowledged_only: bool = True,
    ) -> list[dict[str, Any]]:
        """Get quota alerts for a tenant."""
        pool = self._get_pool()
        if not pool:
            return []

        try:
            async with self._acquire_connection(pool) as conn:
                query = """
                    SELECT *
                    FROM quota_alerts
                    WHERE tenant_id = $1
                """
                if unacknowledged_only:
                    query += " AND is_acknowledged = FALSE"
                query += " ORDER BY created_at DESC LIMIT $2"

                rows = await conn.fetch(query, tenant_id, limit)

                return [
                    {
                        "id": str(row["id"]),
                        "user_id": row["user_id"],
                        "alert_type": row["alert_type"],
                        "threshold_value": row["threshold_value"],
                        "current_value": row["current_value"],
                        "limit_value": row["limit_value"],
                        "message": row["message"],
                        "is_acknowledged": row["is_acknowledged"],
                        "created_at": row["created_at"].isoformat() if row["created_at"] else None,
                    }
                    for row in rows
                ]

        except Exception as e:
            logger.error(f"Failed to get quota alerts: {e}")
            return []

    async def create_alert(
        self,
        tenant_id: str,
        user_id: str,
        alert_type: str,
        threshold_value: int,
        current_value: int,
        limit_value: int,
        message: str,
    ) -> None:
        """Create a quota alert."""
        pool = self._get_pool()
        if not pool:
            return

        try:
            async with self._acquire_connection(pool) as conn:
                await conn.execute(
                    """
                    INSERT INTO quota_alerts (
                        tenant_id, user_id, alert_type,
                        threshold_value, current_value, limit_value, message
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                    """,
                    tenant_id,
                    user_id,
                    alert_type,
                    threshold_value,
                    current_value,
                    limit_value,
                    message,
                )

        except Exception as e:
            logger.error(f"Failed to create quota alert: {e}")

    async def _get_or_create_quota(
        self,
        tenant_id: str,
        user_id: str,
    ) -> UserQuota | None:
        """Get or create a user quota record."""
        pool = self._get_pool()
        if not pool:
            return None

        try:
            async with self._acquire_connection(pool) as conn:
                row = await conn.fetchrow(
                    """
                    SELECT *
                    FROM user_quotas
                    WHERE tenant_id = $1 AND user_id = $2
                    """,
                    tenant_id,
                    user_id,
                )

                if not row:
                    # Create default quota record
                    now = datetime.now(timezone.utc)
                    tomorrow = (now + timedelta(days=1)).replace(
                        hour=0, minute=0, second=0, microsecond=0
                    )
                    next_month = (now.replace(day=1) + timedelta(days=32)).replace(
                        day=1, hour=0, minute=0, second=0, microsecond=0
                    )

                    await conn.execute(
                        """
                        INSERT INTO user_quotas (
                            tenant_id, user_id, daily_reset_at, monthly_reset_at
                        ) VALUES ($1, $2, $3::timestamptz, $4::timestamptz)
                        ON CONFLICT (tenant_id, user_id) DO NOTHING
                        """,
                        tenant_id,
                        user_id,
                        tomorrow,
                        next_month,
                    )

                    row = await conn.fetchrow(
                        """
                        SELECT *
                        FROM user_quotas
                        WHERE tenant_id = $1 AND user_id = $2
                        """,
                        tenant_id,
                        user_id,
                    )

                if row:

                    def _value(key: str, default: Any = None) -> Any:
                        if isinstance(row, dict):
                            return row.get(key, default)
                        getter = getattr(row, "get", None)
                        if callable(getter):
                            try:
                                return getter(key, default)
                            except TypeError:
                                pass
                        try:
                            return row[key]
                        except Exception:
                            return default

                    return UserQuota(
                        tenant_id=str(_value("tenant_id", tenant_id)),
                        user_id=str(_value("user_id", user_id)),
                        daily_token_limit=_value("daily_token_limit"),
                        monthly_token_limit=_value("monthly_token_limit"),
                        monthly_cost_limit_cents=_value("monthly_cost_limit_cents"),
                        requests_per_minute=_value("requests_per_minute"),
                        requests_per_day=_value("requests_per_day"),
                        current_daily_tokens=int(_value("current_daily_tokens", 0) or 0),
                        current_monthly_tokens=int(_value("current_monthly_tokens", 0) or 0),
                        current_monthly_cost_cents=int(
                            _value("current_monthly_cost_cents", 0) or 0
                        ),
                        current_daily_requests=int(_value("current_daily_requests", 0) or 0),
                        daily_reset_at=_value("daily_reset_at"),
                        monthly_reset_at=_value("monthly_reset_at"),
                        is_blocked=bool(_value("is_blocked", False)),
                        blocked_reason=_value("blocked_reason"),
                        warning_threshold=int(_value("warning_threshold", 80) or 80),
                        overage_strategy=self._coerce_overage_strategy(_value("overage_strategy")),
                        downgraded_model=_value("downgraded_model"),
                        temporary_extra_tokens=int(_value("temporary_extra_tokens", 0) or 0),
                        temporary_extra_cost_cents=int(
                            _value("temporary_extra_cost_cents", 0) or 0
                        ),
                        temporary_expires_at=_value("temporary_expires_at"),
                    )

                return None

        except Exception as e:
            logger.error(f"Failed to get/create user quota: {e}")
            return None


def get_quota_service() -> QuotaService:
    """Get the global QuotaService singleton."""
    global _quota_service
    if _quota_service is None:
        _quota_service = QuotaService()
    return _quota_service


def init_quota_service(database: DatabaseStorage) -> QuotaService:
    """Initialize the global QuotaService with database storage."""
    global _quota_service
    if _quota_service is None:
        _quota_service = QuotaService(database)
    else:
        _quota_service.set_database(database)
    return _quota_service
