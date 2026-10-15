"""
Quota API - User quota management endpoints.

Provides:
- Get/Set user quotas
- Quota status checking
- Alerts management
- User blocking/unblocking
- Users overview for dashboard panel
"""

import logging
from datetime import datetime, timezone
from typing import Any, Literal

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from pydantic import BaseModel, Field

from ...api.deps import AuthContext, get_auth_context, require_gateway_capability
from ...core.auth.permissions import Capability
from ...services.billing import get_quota_service
from ...services.billing.quota_service import OverageStrategy, resolve_effective_quota_limits
from ...services.metrics.audit_event_writer import record_config_change

logger = logging.getLogger(__name__)

router = APIRouter(prefix="/quota", tags=["quota"])


def _require_quota_read(request: Request, auth: AuthContext) -> None:
    require_gateway_capability(request, auth, Capability.GATEWAY_QUOTA_READ)


def _require_quota_write(request: Request, auth: AuthContext) -> None:
    require_gateway_capability(request, auth, Capability.GATEWAY_QUOTA_WRITE)


def _is_quota_operator(auth: AuthContext) -> bool:
    return any(role in {"admin", "operator"} for role in auth.roles)


def _require_quota_tenant_scope(request: Request, auth: AuthContext, capability: Capability) -> None:
    if auth.tenant_id:
        return
    permission = (
        "console:quota:edit"
        if capability == Capability.GATEWAY_QUOTA_WRITE
        else "console:quota:view"
    )
    raise HTTPException(
        status_code=403,
        detail={
            "message": "Permission denied: tenant scope required for quota operations",
            "required_capability": capability.value,
            "required_permission": permission,
            "trace_id": str(getattr(request.state, "request_id", "") or ""),
        },
    )


def _require_quota_subject(
    request: Request,
    auth: AuthContext,
    target_user_id: str,
    capability: Capability,
) -> None:
    _require_quota_tenant_scope(request, auth, capability)
    if target_user_id == auth.user_id or _is_quota_operator(auth):
        return
    permission = (
        "console:quota:edit"
        if capability == Capability.GATEWAY_QUOTA_WRITE
        else "console:quota:view"
    )
    raise HTTPException(
        status_code=403,
        detail={
            "message": "Permission denied: cannot access another user's quota",
            "required_capability": capability.value,
            "required_permission": permission,
            "trace_id": str(getattr(request.state, "request_id", "") or ""),
        },
    )


# ============ Request/Response Models ============


class QuotaLimits(BaseModel):
    """Quota limit configuration."""

    daily_tokens: int | None = Field(None, description="Daily token limit")
    monthly_tokens: int | None = Field(None, description="Monthly token limit")
    monthly_cost_cents: int | None = Field(None, description="Monthly cost limit in cents")
    requests_per_minute: int | None = Field(None, description="Requests per minute limit")
    requests_per_day: int | None = Field(None, description="Requests per day limit")


class QuotaUsage(BaseModel):
    """Current quota usage."""

    daily_tokens: int = 0
    monthly_tokens: int = 0
    monthly_cost_cents: int = 0
    daily_requests: int = 0


class QuotaStatus(BaseModel):
    """Quota status information."""

    is_blocked: bool = False
    blocked_reason: str | None = None


class QuotaResets(BaseModel):
    """Quota reset times."""

    daily_reset_at: str | None = None
    monthly_reset_at: str | None = None


class QuotaPolicy(BaseModel):
    """Quota governance policy."""

    overage_strategy: Literal["hard_block", "rate_limit", "downgrade_model", "allow_but_alert"] = (
        "allow_but_alert"
    )
    downgraded_model: str | None = None


class TemporaryBoost(BaseModel):
    """Temporary quota boost."""

    extra_tokens: int = 0
    extra_cost_cents: int = 0
    expires_at: str | None = None


class QuotaResponse(BaseModel):
    """Full quota response."""

    tenant_id: str
    user_id: str
    limits: QuotaLimits
    current_usage: QuotaUsage
    status: QuotaStatus
    policy: QuotaPolicy = Field(default_factory=QuotaPolicy)
    temporary_boost: TemporaryBoost = Field(default_factory=TemporaryBoost)
    resets: QuotaResets
    warning_threshold: int = 80


class QuotaCheckResponse(BaseModel):
    """Quota check response."""

    status: str  # ok, warning, exceeded, blocked
    message: str
    can_proceed: bool
    daily_tokens: dict[str, Any]
    monthly_cost: dict[str, Any]
    daily_requests: dict[str, Any]
    policy: dict[str, Any] = Field(default_factory=dict)


class QuotaForecastWindow(BaseModel):
    """Forecast window information."""

    month_start: str
    today: str
    days_elapsed: int
    days_remaining: int
    lookback_days: int


class QuotaForecastTokens(BaseModel):
    """Token forecast details."""

    current: int
    avg_daily: float
    projected_month_end: int
    limit: int | None = None
    projected_usage_pct: float | None = None
    predicted_breach_date: str | None = None


class QuotaForecastCost(BaseModel):
    """Cost forecast details."""

    current_cents: float
    current_usd: float
    avg_daily_cents: float
    projected_month_end_cents: int
    projected_month_end_usd: float
    limit_cents: int | None = None
    limit_usd: float | None = None
    projected_usage_pct: float | None = None
    predicted_breach_date: str | None = None


class QuotaForecastResponse(BaseModel):
    """Quota usage forecast response."""

    tenant_id: str
    user_id: str
    window: QuotaForecastWindow
    tokens: QuotaForecastTokens
    cost: QuotaForecastCost


class SetQuotaRequest(BaseModel):
    """Request to set user quota."""

    daily_token_limit: int | None = Field(
        None, ge=0, description="Daily token limit (0 for unlimited)"
    )
    monthly_token_limit: int | None = Field(None, ge=0, description="Monthly token limit")
    monthly_cost_limit_cents: int | None = Field(
        None, ge=0, description="Monthly cost limit in cents"
    )
    requests_per_minute: int | None = Field(None, ge=0, description="RPM limit")
    requests_per_day: int | None = Field(None, ge=0, description="Daily request limit")
    warning_threshold: int = Field(80, ge=0, le=100, description="Warning threshold percentage")
    overage_strategy: Literal["hard_block", "rate_limit", "downgrade_model", "allow_but_alert"] = (
        Field(
            "allow_but_alert",
            description="Policy when quota is exceeded",
        )
    )
    downgraded_model: str | None = Field(
        None,
        max_length=128,
        description="Model to use when overage_strategy=downgrade_model",
    )
    temporary_extra_tokens: int | None = Field(
        None,
        ge=0,
        description="Temporary extra daily tokens (optional)",
    )
    temporary_extra_cost_cents: int | None = Field(
        None,
        ge=0,
        description="Temporary extra monthly cost in cents (optional)",
    )
    temporary_expires_at: str | None = Field(
        None,
        description="ISO8601 timestamp for temporary boost expiry",
    )


class AlertResponse(BaseModel):
    """Quota alert response."""

    id: str
    user_id: str
    alert_type: str
    threshold_value: int | None
    current_value: int | None
    limit_value: int | None
    message: str | None
    is_acknowledged: bool
    created_at: str | None


class AlertsListResponse(BaseModel):
    """List of alerts response."""

    alerts: list[AlertResponse]
    total: int


class BlockUserRequest(BaseModel):
    """Request to block a user."""

    reason: str = Field(..., min_length=1, max_length=256)


# ============ Response Models for Overview ============


class QuotaEffectiveLimits(BaseModel):
    daily_tokens: int | None = None
    monthly_tokens: int | None = None
    monthly_cost_cents: int | None = None


class QuotaLimitSources(BaseModel):
    daily_tokens: Literal["configured", "configured_plus_temporary_boost", "unlimited"]
    monthly_tokens: Literal["configured", "configured_plus_temporary_boost", "unlimited"]
    monthly_cost_cents: Literal["configured", "configured_plus_temporary_boost", "unlimited"]


class QuotaObservedBoost(TemporaryBoost):
    active: bool


class QuotaLimitObservation(BaseModel):
    """Point-in-time effective limits; not a reservation or admission guarantee."""

    observed_at: str
    effective: QuotaEffectiveLimits
    sources: QuotaLimitSources
    temporary_boost: QuotaObservedBoost


class QuotaUserOverviewItem(BaseModel):
    """Individual user quota overview item."""

    user_id: str
    daily_tokens_used: int = 0
    daily_tokens_limit: int | None = None
    monthly_cost_used_cents: int = 0
    monthly_cost_limit_cents: int | None = None
    monthly_tokens_used: int = 0
    monthly_tokens_limit: int | None = None
    is_blocked: bool = False
    blocked_reason: str | None = None
    overage_strategy: str = "allow_but_alert"
    downgraded_model: str | None = None
    status: str = "ok"  # Effective usage snapshot, not admission for a future request.
    limit_observation: QuotaLimitObservation | None = None


class QuotaUsersOverviewResponse(BaseModel):
    """Batch quota overview for dashboard panel."""

    users: list[QuotaUserOverviewItem]
    summary: dict[str, int]


# ============ API Endpoints ============
# IMPORTANT: Static routes MUST be defined BEFORE /{user_id} dynamic route
# to prevent FastAPI from matching e.g. "summary" as a user_id.


@router.get("/summary")
async def get_quota_summary(
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    """
    Get summary of quota status across all users.

    Returns counts of users by quota status and total alerts.
    """
    _require_quota_read(request, auth)
    _require_quota_tenant_scope(request, auth, Capability.GATEWAY_QUOTA_READ)
    quota_service = get_quota_service()

    summary = {
        "total_users": 0,
        "blocked_users": 0,
        "warning_users": 0,
        "exceeded_users": 0,
        "unacknowledged_alerts": 0,
    }

    if quota_service.database and quota_service.database._pool:
        async with quota_service.database._pool.acquire() as conn:
            # Get user counts
            row = await conn.fetchrow(
                """
                SELECT
                    COUNT(*) as total_users,
                    COUNT(*) FILTER (WHERE is_blocked = TRUE) as blocked_users,
                    COUNT(*) FILTER (
                        WHERE daily_token_limit IS NOT NULL
                        AND current_daily_tokens::float / NULLIF(daily_token_limit, 0) >= warning_threshold / 100.0
                        AND current_daily_tokens < daily_token_limit
                    ) as warning_users,
                    COUNT(*) FILTER (
                        WHERE (daily_token_limit IS NOT NULL AND current_daily_tokens >= daily_token_limit)
                        OR (monthly_cost_limit_cents IS NOT NULL AND current_monthly_cost_cents >= monthly_cost_limit_cents)
                    ) as exceeded_users
                FROM user_quotas
                WHERE tenant_id = $1
                """,
                auth.tenant_id,
            )

            if row:
                summary["total_users"] = row["total_users"] or 0
                summary["blocked_users"] = row["blocked_users"] or 0
                summary["warning_users"] = row["warning_users"] or 0
                summary["exceeded_users"] = row["exceeded_users"] or 0

            # Get alert count
            alert_row = await conn.fetchrow(
                """
                SELECT COUNT(*) as count
                FROM quota_alerts
                WHERE tenant_id = $1 AND is_acknowledged = FALSE
                """,
                auth.tenant_id,
            )

            if alert_row:
                summary["unacknowledged_alerts"] = alert_row["count"] or 0

    return summary


@router.get("/users-overview", response_model=QuotaUsersOverviewResponse)
async def get_quota_users_overview(
    request: Request,
    limit: int = Query(50, ge=1, le=200, description="Max users to return"),
    sort_by: str = Query("daily_tokens", description="Sort by: daily_tokens, monthly_cost, status"),
    auth: AuthContext = Depends(get_auth_context),
) -> QuotaUsersOverviewResponse:
    """
    Batch return user quota statuses for dashboard panel.

    Returns real quota limits, current usage, and computed status for each user.
    """
    _require_quota_read(request, auth)
    _require_quota_tenant_scope(request, auth, Capability.GATEWAY_QUOTA_READ)
    quota_service = get_quota_service()
    observed_at = datetime.now(timezone.utc)
    users: list[QuotaUserOverviewItem] = []
    summary_counts = {"total": 0, "blocked": 0, "exceeded": 0, "warning": 0, "ok": 0}

    pool = quota_service._get_pool()
    if not pool:
        return QuotaUsersOverviewResponse(users=users, summary=summary_counts)

    try:
        async with quota_service._acquire_connection(pool) as conn:
            # Determine sort column
            sort_col = "current_daily_tokens"
            if sort_by == "monthly_cost":
                sort_col = "current_monthly_cost_cents"
            elif sort_by == "status":
                sort_col = "is_blocked DESC, current_daily_tokens"

            rows = await conn.fetch(
                f"""
                SELECT
                    user_id,
                    current_daily_tokens,
                    daily_token_limit,
                    current_monthly_tokens,
                    monthly_token_limit,
                    current_monthly_cost_cents,
                    monthly_cost_limit_cents,
                    is_blocked,
                    blocked_reason,
                    overage_strategy,
                    downgraded_model,
                    warning_threshold,
                    temporary_extra_tokens,
                    temporary_extra_cost_cents,
                    temporary_expires_at
                FROM user_quotas
                WHERE tenant_id = $1
                ORDER BY {sort_col} DESC
                LIMIT $2
                """,
                auth.tenant_id,
                limit,
            )

            for row in rows:
                daily_used = int(row["current_daily_tokens"] or 0)
                daily_limit = row["daily_token_limit"]
                monthly_cost_used = int(row["current_monthly_cost_cents"] or 0)
                monthly_cost_limit = row["monthly_cost_limit_cents"]
                monthly_tokens_used = int(row["current_monthly_tokens"] or 0)
                monthly_tokens_limit = row["monthly_token_limit"]
                is_blocked = bool(row["is_blocked"])
                warning_threshold = int(row["warning_threshold"] or 80)

                observation = resolve_effective_quota_limits(
                    daily_token_limit=daily_limit,
                    monthly_token_limit=monthly_tokens_limit,
                    monthly_cost_limit_cents=monthly_cost_limit,
                    temporary_extra_tokens=int(row["temporary_extra_tokens"] or 0),
                    temporary_extra_cost_cents=int(row["temporary_extra_cost_cents"] or 0),
                    temporary_expires_at=row["temporary_expires_at"],
                    now=observed_at,
                )
                effective_daily = observation["effective"]["daily_tokens"]
                effective_monthly_cost = observation["effective"]["monthly_cost_cents"]
                effective_monthly_tokens = observation["effective"]["monthly_tokens"]

                # Status uses this response's effective snapshot. Configured
                # fields below remain unchanged for existing clients.
                if is_blocked:
                    status = "blocked"
                elif (
                    (effective_daily and daily_used >= effective_daily)
                    or (effective_monthly_cost and monthly_cost_used >= effective_monthly_cost)
                    or (effective_monthly_tokens and monthly_tokens_used >= effective_monthly_tokens)
                ):
                    status = "exceeded"
                elif (
                    effective_daily
                    and effective_daily > 0
                    and (daily_used / effective_daily * 100) >= warning_threshold
                    or effective_monthly_cost
                    and effective_monthly_cost > 0
                    and (monthly_cost_used / effective_monthly_cost * 100) >= warning_threshold
                ):
                    status = "warning"
                else:
                    status = "ok"

                summary_counts[status] = summary_counts.get(status, 0) + 1
                summary_counts["total"] += 1

                users.append(
                    QuotaUserOverviewItem(
                        user_id=row["user_id"],
                        daily_tokens_used=daily_used,
                        daily_tokens_limit=daily_limit,
                        monthly_cost_used_cents=monthly_cost_used,
                        monthly_cost_limit_cents=monthly_cost_limit,
                        monthly_tokens_used=monthly_tokens_used,
                        monthly_tokens_limit=monthly_tokens_limit,
                        is_blocked=is_blocked,
                        blocked_reason=row["blocked_reason"],
                        overage_strategy=row["overage_strategy"] or "allow_but_alert",
                        downgraded_model=row["downgraded_model"],
                        status=status,
                        limit_observation=QuotaLimitObservation(**observation),
                    )
                )

    except Exception as e:
        logger.error(f"Failed to get quota users overview: {e}")

    return QuotaUsersOverviewResponse(users=users, summary=summary_counts)


@router.get("/alerts/list", response_model=AlertsListResponse)
async def get_quota_alerts(
    request: Request,
    unacknowledged_only: bool = Query(True, description="Only show unacknowledged alerts"),
    limit: int = Query(50, ge=1, le=200, description="Maximum alerts to return"),
    auth: AuthContext = Depends(get_auth_context),
) -> AlertsListResponse:
    """
    Get quota alerts for the tenant.

    Returns alerts for users approaching or exceeding limits.
    """
    _require_quota_read(request, auth)
    _require_quota_tenant_scope(request, auth, Capability.GATEWAY_QUOTA_READ)
    quota_service = get_quota_service()

    alerts = await quota_service.get_quota_alerts(
        tenant_id=auth.tenant_id,
        limit=limit,
        unacknowledged_only=unacknowledged_only,
    )

    return AlertsListResponse(
        alerts=[AlertResponse(**a) for a in alerts],
        total=len(alerts),
    )


@router.post("/alerts/{alert_id}/acknowledge")
async def acknowledge_alert(
    alert_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    """
    Acknowledge a quota alert.

    Marks the alert as acknowledged so it won't appear in unacknowledged list.
    """
    _require_quota_write(request, auth)
    _require_quota_tenant_scope(request, auth, Capability.GATEWAY_QUOTA_WRITE)
    quota_service = get_quota_service()

    if quota_service.database and quota_service.database._pool:
        async with quota_service.database._pool.acquire() as conn:
            result = await conn.execute(
                """
                UPDATE quota_alerts
                SET
                    is_acknowledged = TRUE,
                    acknowledged_at = CURRENT_TIMESTAMP,
                    acknowledged_by = $2
                WHERE id = $1 AND tenant_id = $3
                """,
                alert_id,
                auth.user_id,
                auth.tenant_id,
            )

            if "UPDATE 0" in result:
                raise HTTPException(status_code=404, detail="Alert not found")

    return {
        "success": True,
        "alert_id": alert_id,
        "acknowledged": True,
    }


# ---- Dynamic routes below (/{user_id} patterns) ----


@router.get("/{user_id}", response_model=QuotaResponse)
async def get_user_quota(
    user_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
) -> QuotaResponse:
    """
    Get quota configuration and usage for a user.

    Returns limits, current usage, status, and reset times.
    """
    _require_quota_read(request, auth)
    _require_quota_subject(request, auth, user_id, Capability.GATEWAY_QUOTA_READ)
    quota_service = get_quota_service()

    quota = await quota_service.get_user_quota(auth.tenant_id, user_id)

    if not quota:
        # Return default empty quota
        return QuotaResponse(
            tenant_id=auth.tenant_id,
            user_id=user_id,
            limits=QuotaLimits(),
            current_usage=QuotaUsage(),
            status=QuotaStatus(),
            policy=QuotaPolicy(),
            temporary_boost=TemporaryBoost(),
            resets=QuotaResets(),
        )

    return QuotaResponse(
        tenant_id=quota.get("tenant_id", auth.tenant_id),
        user_id=quota.get("user_id", user_id),
        limits=QuotaLimits(
            daily_tokens=quota.get("limits", {}).get("daily_tokens"),
            monthly_tokens=quota.get("limits", {}).get("monthly_tokens"),
            monthly_cost_cents=quota.get("limits", {}).get("monthly_cost_cents"),
            requests_per_minute=quota.get("limits", {}).get("requests_per_minute"),
            requests_per_day=quota.get("limits", {}).get("requests_per_day"),
        ),
        current_usage=QuotaUsage(
            daily_tokens=quota.get("current_usage", {}).get("daily_tokens", 0),
            monthly_tokens=quota.get("current_usage", {}).get("monthly_tokens", 0),
            monthly_cost_cents=quota.get("current_usage", {}).get("monthly_cost_cents", 0),
            daily_requests=quota.get("current_usage", {}).get("daily_requests", 0),
        ),
        status=QuotaStatus(
            is_blocked=quota.get("status", {}).get("is_blocked", False),
            blocked_reason=quota.get("status", {}).get("blocked_reason"),
        ),
        policy=QuotaPolicy(
            overage_strategy=quota.get("policy", {}).get("overage_strategy", "allow_but_alert"),
            downgraded_model=quota.get("policy", {}).get("downgraded_model"),
        ),
        temporary_boost=TemporaryBoost(
            extra_tokens=quota.get("temporary_boost", {}).get("extra_tokens", 0),
            extra_cost_cents=quota.get("temporary_boost", {}).get("extra_cost_cents", 0),
            expires_at=quota.get("temporary_boost", {}).get("expires_at"),
        ),
        resets=QuotaResets(
            daily_reset_at=quota.get("resets", {}).get("daily_reset_at"),
            monthly_reset_at=quota.get("resets", {}).get("monthly_reset_at"),
        ),
        warning_threshold=quota.get("warning_threshold", 80),
    )


@router.put("/{user_id}", response_model=QuotaResponse)
async def set_user_quota(
    user_id: str,
    quota_request: SetQuotaRequest,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
) -> QuotaResponse:
    """
    Set or update quota limits for a user.

    Pass null/None for fields to keep existing values.
    Pass 0 to set unlimited.
    """
    _require_quota_write(request, auth)
    _require_quota_subject(request, auth, user_id, Capability.GATEWAY_QUOTA_WRITE)
    quota_service = get_quota_service()

    temporary_expires_at = None
    if quota_request.temporary_expires_at:
        try:
            temporary_expires_at = datetime.fromisoformat(
                quota_request.temporary_expires_at.replace("Z", "+00:00")
            )
        except ValueError as exc:
            raise HTTPException(
                status_code=400, detail=f"Invalid temporary_expires_at: {exc}"
            ) from exc

    result = await quota_service.set_user_quota(
        tenant_id=auth.tenant_id,
        user_id=user_id,
        daily_token_limit=quota_request.daily_token_limit,
        monthly_token_limit=quota_request.monthly_token_limit,
        monthly_cost_limit_cents=quota_request.monthly_cost_limit_cents,
        requests_per_minute=quota_request.requests_per_minute,
        requests_per_day=quota_request.requests_per_day,
        warning_threshold=quota_request.warning_threshold,
        overage_strategy=OverageStrategy(quota_request.overage_strategy),
        downgraded_model=quota_request.downgraded_model,
        temporary_extra_tokens=quota_request.temporary_extra_tokens,
        temporary_extra_cost_cents=quota_request.temporary_extra_cost_cents,
        temporary_expires_at=temporary_expires_at,
    )

    if "error" in result:
        raise HTTPException(status_code=500, detail=result["error"])

    await record_config_change(
        request=request,
        auth=auth,
        resource_type="quota",
        resource_id=user_id,
        action="set",
        before=None,
        after=quota_request.model_dump(exclude_none=True),
    )

    # Return updated quota
    return await get_user_quota(user_id, request, auth)


@router.get("/{user_id}/check", response_model=QuotaCheckResponse)
async def check_user_quota(
    user_id: str,
    request: Request,
    estimated_tokens: int = Query(0, ge=0, description="Estimated tokens for this request"),
    auth: AuthContext = Depends(get_auth_context),
) -> QuotaCheckResponse:
    """
    Check if user has sufficient quota.

    Used before making requests to pre-validate quota availability.
    """
    _require_quota_read(request, auth)
    _require_quota_subject(request, auth, user_id, Capability.GATEWAY_QUOTA_READ)
    quota_service = get_quota_service()

    result = await quota_service.check_quota(
        tenant_id=auth.tenant_id,
        user_id=user_id,
        estimated_tokens=estimated_tokens,
    )

    return QuotaCheckResponse(**result.to_dict())


@router.get("/{user_id}/forecast", response_model=QuotaForecastResponse)
async def get_user_quota_forecast(
    user_id: str,
    request: Request,
    lookback_days: int = Query(7, ge=1, le=30, description="Recent days used for trend projection"),
    auth: AuthContext = Depends(get_auth_context),
) -> QuotaForecastResponse:
    """
    Get quota usage forecast for the current month.

    Uses recent daily usage trend to project month-end usage and predicted breach dates.
    """
    _require_quota_read(request, auth)
    _require_quota_subject(request, auth, user_id, Capability.GATEWAY_QUOTA_READ)
    quota_service = get_quota_service()
    forecast = await quota_service.get_quota_forecast(
        tenant_id=auth.tenant_id,
        user_id=user_id,
        lookback_days=lookback_days,
    )
    if "error" in forecast:
        raise HTTPException(status_code=500, detail=forecast["error"])
    return QuotaForecastResponse(**forecast)


@router.post("/{user_id}/reset", response_model=QuotaResponse)
async def reset_user_quota(
    user_id: str,
    request: Request,
    reset_type: str = Query("daily", description="Reset type: daily or monthly"),
    auth: AuthContext = Depends(get_auth_context),
) -> QuotaResponse:
    """
    Manually reset a user's quota.

    Can reset daily or monthly quota counters.
    """
    _require_quota_write(request, auth)
    _require_quota_subject(request, auth, user_id, Capability.GATEWAY_QUOTA_WRITE)
    quota_service = get_quota_service()

    if reset_type == "daily":
        # Reset daily counters for specific user
        if quota_service.database and quota_service.database._pool:
            async with quota_service.database._pool.acquire() as conn:
                await conn.execute(
                    """
                    UPDATE user_quotas
                    SET
                        current_daily_tokens = 0,
                        current_daily_requests = 0,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE tenant_id = $1 AND user_id = $2
                    """,
                    auth.tenant_id,
                    user_id,
                )
    elif reset_type == "monthly":
        if quota_service.database and quota_service.database._pool:
            async with quota_service.database._pool.acquire() as conn:
                await conn.execute(
                    """
                    UPDATE user_quotas
                    SET
                        current_monthly_tokens = 0,
                        current_monthly_cost_cents = 0,
                        updated_at = CURRENT_TIMESTAMP
                    WHERE tenant_id = $1 AND user_id = $2
                    """,
                    auth.tenant_id,
                    user_id,
                )
    else:
        raise HTTPException(
            status_code=400, detail="Invalid reset_type. Must be 'daily' or 'monthly'"
        )

    await record_config_change(
        request=request,
        auth=auth,
        resource_type="quota",
        resource_id=user_id,
        action=f"reset_{reset_type}",
        before={"user_id": user_id, "reset_type": reset_type},
        after={"reset_type": reset_type},
    )

    return await get_user_quota(user_id, request, auth)


@router.post("/{user_id}/block")
async def block_user(
    user_id: str,
    block_request: BlockUserRequest,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    """
    Block a user from making requests.

    Blocked users will receive quota exceeded errors.
    """
    _require_quota_write(request, auth)
    _require_quota_subject(request, auth, user_id, Capability.GATEWAY_QUOTA_WRITE)
    quota_service = get_quota_service()

    success = await quota_service.block_user(
        tenant_id=auth.tenant_id,
        user_id=user_id,
        reason=block_request.reason,
    )

    if not success:
        raise HTTPException(status_code=500, detail="Failed to block user")

    await record_config_change(
        request=request,
        auth=auth,
        resource_type="quota",
        resource_id=user_id,
        action="block",
        before={"user_id": user_id},
        after={"reason": block_request.reason, "is_blocked": True},
    )

    return {
        "success": True,
        "user_id": user_id,
        "is_blocked": True,
        "reason": block_request.reason,
    }


@router.post("/{user_id}/unblock")
async def unblock_user(
    user_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    """
    Unblock a user.

    Removes the block status and allows requests again.
    """
    _require_quota_write(request, auth)
    _require_quota_subject(request, auth, user_id, Capability.GATEWAY_QUOTA_WRITE)
    quota_service = get_quota_service()

    success = await quota_service.unblock_user(
        tenant_id=auth.tenant_id,
        user_id=user_id,
    )

    if not success:
        raise HTTPException(status_code=500, detail="Failed to unblock user")

    await record_config_change(
        request=request,
        auth=auth,
        resource_type="quota",
        resource_id=user_id,
        action="unblock",
        before={"user_id": user_id, "is_blocked": True},
        after={"is_blocked": False},
    )

    return {
        "success": True,
        "user_id": user_id,
        "is_blocked": False,
    }
