"""
Dashboard API - LangSmith-style Enterprise Monitoring Dashboard

Provides:
- Real-time metrics via WebSocket
- Historical metrics with time range filtering
- Multi-tenant user filtering
- Aggregated statistics
- Alert status
"""

import hashlib
import inspect
import logging
from datetime import date, datetime, timedelta
from typing import Any

from fastapi import (
    APIRouter,
    Depends,
    HTTPException,
    Query,
    Request,
    WebSocket,
    WebSocketDisconnect,
)
from pydantic import BaseModel

from ...api.deps import (
    AuthContext,
    get_auth_context,
    require_gateway_capability,
    require_platform_admin,
)
from ...core.auth.jwt import decode_jwt_token
from ...core.auth.jwt_config import get_jwt_algorithms, get_jwt_secret
from ...core.auth.permissions import Capability
from ...services.billing.pricing_catalog import microcents_to_usd
from ...services.metrics import compute_data_status, get_metrics_recorder
from ...services.metrics.realtime_metrics import RealtimeSnapshot, get_realtime_metrics
from ...services.metrics.usage_recorder import get_usage_recorder

logger = logging.getLogger(__name__)

router = APIRouter(prefix="/dashboard", tags=["dashboard"])


def _require_platform_metrics(request: Request | WebSocket, auth: AuthContext) -> None:
    """Keep the structural capability guard and enforce global scope."""

    require_gateway_capability(request, auth, Capability.GATEWAY_METRICS_READ)
    require_platform_admin(request, auth, Capability.GATEWAY_METRICS_READ)


# ============ Response Models ============


class LatencyMetrics(BaseModel):
    """Latency metrics with percentiles"""

    p50: int
    p95: int
    p99: int
    avg: int


class ErrorMetrics(BaseModel):
    """Error rate metrics"""

    rate: float
    rate_4xx: float
    rate_5xx: float


class UserMetrics(BaseModel):
    """Active user metrics"""

    active: int
    threads_total: int
    threads_by_user: dict[str, int]


class CapacityMetrics(BaseModel):
    """System capacity metrics"""

    queue_depth: int
    concurrent: int
    max_concurrent: int
    utilization: float


class TokenMetrics(BaseModel):
    """Token consumption metrics"""

    total: int
    input_tokens: int
    output_tokens: int
    cost_usd: float
    per_minute: float


class RunMetrics(BaseModel):
    """LangGraph run metrics"""

    total: int
    success_rate: float | None
    avg_duration_ms: int


class RealtimeDashboard(BaseModel):
    """Real-time dashboard response"""

    # Throughput
    rps: float
    rps_1m: float
    rps_5m: float

    # Sub-metrics
    latency: LatencyMetrics
    errors: ErrorMetrics
    users: UserMetrics
    capacity: CapacityMetrics
    tokens: TokenMetrics
    runs: RunMetrics

    # Timestamp
    timestamp: str
    is_live: bool = True


class TimeSeriesDataPoint(BaseModel):
    """Single data point in time series"""

    timestamp: str
    value: float
    label: str | None = None


class TimeSeriesResponse(BaseModel):
    """Time series data response"""

    metric: str
    granularity: str
    start: str
    end: str
    data: list[TimeSeriesDataPoint]


class AlertStatus(BaseModel):
    """Alert status"""

    name: str
    level: str  # ok, warning, critical
    message: str
    threshold: float
    current_value: float
    triggered_at: str | None = None


class AlertsResponse(BaseModel):
    """Active alerts response"""

    alerts: list[AlertStatus]
    last_check: str


class UserDashboard(BaseModel):
    """Per-user dashboard for multi-tenant view"""

    user_id: str
    tokens: TokenMetrics
    active_threads: int
    requests_today: int
    avg_latency_ms: int
    error_rate: float | None
    timestamp: str


class UsageBreakdown(BaseModel):
    """Usage breakdown item"""

    dimension_value: str
    requests: int
    input_tokens: int
    output_tokens: int
    total_tokens: int
    cost_usd: float
    percentage: float


# ============ Alert Thresholds ============

ALERT_THRESHOLDS = {
    "latency_p95": {"warning": 2000, "critical": 5000},  # ms
    "error_rate": {"warning": 5, "critical": 10},  # %
    "queue_depth": {"warning": 50, "critical": 100},
    "utilization": {"warning": 80, "critical": 95},  # %
}


# ============ WebSocket Connection Manager ============


class DashboardConnectionManager:
    """Manage WebSocket connections for real-time updates"""

    def __init__(self):
        self.active_connections: list[WebSocket] = []

    async def connect(self, websocket: WebSocket):
        await websocket.accept()
        self.active_connections.append(websocket)

    def disconnect(self, websocket: WebSocket):
        if websocket in self.active_connections:
            self.active_connections.remove(websocket)

    async def broadcast(self, data: dict[str, Any]):
        """Broadcast to all connected clients"""
        disconnected = []
        for connection in self.active_connections:
            try:
                await connection.send_json(data)
            except Exception:
                disconnected.append(connection)

        # Clean up disconnected
        for ws in disconnected:
            self.disconnect(ws)


# Global connection manager
manager = DashboardConnectionManager()


# ============ WebSocket Authentication Helper ============


async def _merge_db_permissions_for_user(
    app,
    *,
    user_id: str,
    roles: list[str],
    permissions: list[str],
) -> tuple[list[str], list[str]]:
    merged_roles = list(roles)
    merged_permissions = list(permissions)
    for perm in merged_permissions:
        if perm not in merged_roles:
            merged_roles.append(perm)
    db = getattr(app.state, "database", None)
    if not db or not getattr(db, "enabled", False) or not user_id:
        return merged_roles, merged_permissions
    try:
        db_permissions = await db.get_user_permissions(user_id)
        for perm in db_permissions:
            if perm not in merged_permissions:
                merged_permissions.append(perm)
            if perm not in merged_roles:
                merged_roles.append(perm)
    except Exception as e:
        logger.warning(
            f"[AUTH] Failed to fetch DB permissions for WebSocket user {user_id}: {e}"
        )
    return merged_roles, merged_permissions


async def authenticate_websocket(
    websocket: WebSocket,
) -> AuthContext | None:
    """
    Authenticate WebSocket connection using credentials from handshake headers.

    WebSocket connections don't support standard FastAPI Depends for auth,
    so we need to manually extract and verify the token.

    Args:
        websocket: The WebSocket connection

    Returns:
        AuthContext if authenticated, None if auth fails
    """
    app = websocket.app
    settings = getattr(app.state, "settings", None)
    if not settings:
        return None

    auth_cfg = settings.authentication

    # If auth is disabled, allow guest access
    if not auth_cfg.jwt.enabled and not auth_cfg.api_key.enabled:
        return AuthContext(user_id="guest", tenant_id="public", roles=["guest"])

    # Credentials must stay in the WebSocket handshake headers. Query strings
    # are routinely captured by access logs, browser history, and telemetry.
    token = None
    auth_header = websocket.headers.get("authorization", "")
    if auth_header.lower().startswith("bearer "):
        token = auth_header.split(" ", 1)[1].strip()

    # Also check for API key
    api_key = websocket.headers.get(auth_cfg.api_key.header_name)

    # JWT authentication
    if token and auth_cfg.jwt.enabled:
        try:
            jwt_secret = get_jwt_secret(auth_cfg.jwt.secret)
            jwt_algorithms = get_jwt_algorithms(auth_cfg.jwt.algorithms)

            payload = decode_jwt_token(
                token,
                secret=jwt_secret,
                algorithms=jwt_algorithms,
                audience=auth_cfg.jwt.audience,
                issuer=auth_cfg.jwt.issuer,
            )

            token_id = payload.get("jti")
            redis = getattr(app.state, "redis", None)
            if (
                token_id
                and redis
                and getattr(redis, "enabled", False)
                and not await redis.validate_token(str(token_id))
            ):
                return None

            user_id = str(payload.get("sub") or payload.get("user_id") or "")
            if not user_id:
                return None

            tenant_id = str(payload.get("tenant_id") or "")
            raw_roles = payload.get("roles") or payload.get("role") or ["user"]
            if isinstance(raw_roles, str):
                roles = [raw_roles]
            else:
                roles = [str(r) for r in raw_roles] if raw_roles else ["user"]

            permissions = []
            raw_permissions = payload.get("permissions")
            if isinstance(raw_permissions, str):
                permissions = [raw_permissions]
            elif isinstance(raw_permissions, list):
                permissions = [str(p) for p in raw_permissions if p is not None]

            roles, permissions = await _merge_db_permissions_for_user(
                app,
                user_id=user_id,
                roles=roles,
                permissions=permissions,
            )

            return AuthContext(
                user_id=user_id,
                tenant_id=tenant_id,
                roles=roles,
                permissions=permissions,
                is_authenticated=True,
            )

        except Exception as e:
            logger.warning(f"WebSocket JWT auth failed: {e}")
            return None

    # API key authentication
    if api_key and auth_cfg.api_key.enabled:
        try:
            db = getattr(app.state, "database", None)
            key_hash = hashlib.sha256(api_key.encode("utf-8")).hexdigest()

            if db and getattr(db, "enabled", False):
                key_info = await db.get_api_key(key_hash)
                if key_info:
                    roles = key_info.get("roles") or ["user"]
                    if isinstance(roles, str):
                        roles = [roles]
                    else:
                        roles = [str(r) for r in roles] if roles else ["user"]
                    key_permissions = key_info.get("permissions") or []
                    if isinstance(key_permissions, str):
                        permissions = [key_permissions]
                    else:
                        permissions = [
                            str(p) for p in key_permissions if p is not None
                        ]
                    user_id = str(
                        key_info.get("user_id") or f"apikey:{key_hash[:16]}"
                    )
                    roles, permissions = await _merge_db_permissions_for_user(
                        app,
                        user_id=user_id,
                        roles=roles,
                        permissions=permissions,
                    )
                    return AuthContext(
                        user_id=user_id,
                        tenant_id=str(key_info.get("tenant_id") or ""),
                        roles=roles,
                        permissions=permissions,
                        is_authenticated=True,
                    )

            # Static API key check
            if api_key in auth_cfg.api_key.keys:
                return AuthContext(
                    user_id=f"apikey:{key_hash[:16]}",
                    tenant_id="",
                    roles=["user"],
                    permissions=[],
                )

        except Exception as e:
            logger.warning(f"WebSocket API key auth failed: {e}")
            return None

    return None


# ============ API Endpoints ============


@router.get("/realtime", response_model=RealtimeDashboard)
async def get_realtime_dashboard(
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
) -> RealtimeDashboard:
    """
    Get real-time dashboard metrics (LangSmith-style)

    Returns current system state including:
    - RPS (requests per second) - instant, 1m, 5m
    - Latency percentiles (P50/P95/P99)
    - Error rates (total, 4xx, 5xx)
    - Active users and threads
    - Queue depth and capacity utilization
    - Token consumption and cost
    - LangGraph run statistics
    """
    _require_platform_metrics(request, auth)
    realtime = get_realtime_metrics()
    snapshot = await realtime.get_realtime_snapshot()

    return RealtimeDashboard(
        rps=snapshot.rps,
        rps_1m=snapshot.rps_1m,
        rps_5m=snapshot.rps_5m,
        latency=LatencyMetrics(
            p50=snapshot.latency_p50,
            p95=snapshot.latency_p95,
            p99=snapshot.latency_p99,
            avg=snapshot.latency_avg,
        ),
        errors=ErrorMetrics(
            rate=snapshot.error_rate,
            rate_4xx=snapshot.error_rate_4xx,
            rate_5xx=snapshot.error_rate_5xx,
        ),
        users=UserMetrics(
            active=snapshot.active_users,
            threads_total=snapshot.total_threads,
            threads_by_user=snapshot.threads_by_user,
        ),
        capacity=CapacityMetrics(
            queue_depth=snapshot.queue_depth,
            concurrent=snapshot.concurrent_requests,
            max_concurrent=snapshot.max_concurrent,
            utilization=round(
                snapshot.concurrent_requests / max(snapshot.max_concurrent, 1) * 100, 1
            ),
        ),
        tokens=TokenMetrics(
            total=snapshot.total_tokens,
            input_tokens=snapshot.input_tokens,
            output_tokens=snapshot.output_tokens,
            cost_usd=snapshot.token_cost_usd,
            per_minute=snapshot.tokens_per_minute,
        ),
        runs=RunMetrics(
            total=snapshot.total_runs,
            success_rate=snapshot.run_success_rate if snapshot.total_runs > 0 else None,
            avg_duration_ms=snapshot.avg_run_duration_ms,
        ),
        timestamp=snapshot.timestamp,
        is_live=True,
    )


@router.websocket("/ws")
async def websocket_dashboard(
    websocket: WebSocket,
):
    """
    WebSocket endpoint for real-time dashboard updates

    Sends metrics snapshot every 5 seconds.

    Authentication: Pass JWT or API-key credentials via handshake headers.
    For tenant isolation, metrics are filtered by tenant_id from the authenticated user.
    """
    # Authenticate the WebSocket connection
    auth = await authenticate_websocket(websocket)

    # Check if authentication is required
    settings = getattr(websocket.app.state, "settings", None)
    auth_required = False
    if settings:
        auth_cfg = settings.authentication
        auth_required = auth_cfg.jwt.enabled or auth_cfg.api_key.enabled

    if auth_required and not auth:
        await websocket.close(code=4001, reason="Authentication required")
        return
    if not auth:
        await websocket.close(code=4001, reason="Authentication configuration unavailable")
        return
    try:
        _require_platform_metrics(websocket, auth)
    except HTTPException:
        await websocket.close(code=4003, reason="Metrics permission required")
        return

    # Accept the connection after authentication
    await manager.connect(websocket)

    # Get tenant_id for filtering (if multi-tenant)
    tenant_id = auth.tenant_id or ""

    realtime = get_realtime_metrics()

    try:
        while True:
            # Get latest snapshot
            snapshot = await realtime.get_realtime_snapshot()
            data = snapshot.to_dict()
            if snapshot.total_runs == 0:
                data["runs"]["success_rate"] = None
            data["type"] = "metrics"

            # Add tenant context to response
            if tenant_id:
                data["tenant_id"] = tenant_id

            # Check alerts
            alerts = _check_alerts(snapshot)
            data["alerts"] = [a.dict() for a in alerts]

            await websocket.send_json(data)

            # Wait for next update (or client message)
            try:
                # Non-blocking receive with timeout
                import asyncio

                msg = await asyncio.wait_for(websocket.receive_text(), timeout=5.0)
                # Handle ping/pong or commands
                if msg == "ping":
                    await websocket.send_json({"type": "pong"})
            except asyncio.TimeoutError:
                pass  # Normal timeout, continue loop

    except WebSocketDisconnect:
        disconnected = manager.disconnect(websocket)
        if inspect.isawaitable(disconnected):
            await disconnected
    except Exception:
        disconnected = manager.disconnect(websocket)
        if inspect.isawaitable(disconnected):
            await disconnected


@router.get("/timeseries/{metric}")
async def get_timeseries(
    metric: str,
    request: Request,
    start: datetime | None = Query(None, description="Start time (ISO format)"),
    end: datetime | None = Query(None, description="End time (ISO format)"),
    granularity: str = Query("hour", description="minute | hour | day"),
    user_id: str | None = Query(None, description="Filter by user ID"),
    auth: AuthContext = Depends(get_auth_context),
) -> TimeSeriesResponse:
    """
    Get time-series data for a specific metric

    Supported metrics:
    - requests: Request count
    - latency: Average latency
    - errors: Error count
    - tokens: Token usage
    - cost: Token cost (USD)
    - runs: LangGraph runs
    """
    _require_platform_metrics(request, auth)
    # Default to last 24 hours
    if not end:
        end = datetime.now()
    if not start:
        start = end - timedelta(hours=24)

    # Try to use UsageRecorder for granular data if available
    recorder = get_usage_recorder()
    if recorder and recorder.database:
        try:
            # Convert datetime to date for UsageRecorder
            start_date = start.date()
            end_date = end.date()

            # Map metric names
            metric_key = metric
            if metric == "latency":
                metric_key = "avg_latency_ms"
            elif metric == "tokens":
                metric_key = "total_tokens"
            elif metric == "cost":
                metric_key = "cost_usd"
            elif metric == "runs":
                # Runs may be incomplete in UsageRecorder; use requests as fallback.
                metric_key = "requests"  # Approximation if runs not separate

            ts_data = await recorder.get_usage_timeseries(
                tenant_id=auth.tenant_id,  # UsageRecorder needs tenant_id
                start_date=start_date,
                end_date=end_date,
                user_id=user_id,
                granularity=granularity,
            )

            data = []
            for item in ts_data:
                val = item.get(metric_key, 0)
                # Ensure we have a float
                if val is None:
                    val = 0.0

                data.append(
                    TimeSeriesDataPoint(
                        timestamp=item["date"],
                        value=float(val),
                    )
                )

            return TimeSeriesResponse(
                metric=metric,
                granularity=granularity,
                start=start.isoformat(),
                end=end.isoformat(),
                data=data,
            )

        except Exception as e:
            logger.warning(f"Failed to fetch timeseries from UsageRecorder: {e}")

    # Fallback to Redis implementation
    redis = getattr(request.app.state, "redis", None)
    data: list[TimeSeriesDataPoint] = []

    if redis and redis._client:
        try:
            current = start
            delta = {
                "minute": timedelta(minutes=1),
                "hour": timedelta(hours=1),
                "day": timedelta(days=1),
            }.get(granularity, timedelta(hours=1))

            while current <= end:
                date_str = current.strftime("%Y-%m-%d")
                hour_str = current.strftime("%H")

                value = 0.0

                if metric == "requests":
                    if granularity == "hour":
                        key = f"metrics:requests:hour:{date_str}:{hour_str}"
                    else:
                        key = f"metrics:requests:total:{date_str}"
                    value = float(await redis._client.get(key) or 0)

                elif metric == "latency":
                    sum_key = f"metrics:latency:sum:{date_str}"
                    count_key = f"metrics:latency:count:{date_str}"
                    lat_sum = int(await redis._client.get(sum_key) or 0)
                    lat_count = int(await redis._client.get(count_key) or 0)
                    value = lat_sum / lat_count if lat_count > 0 else 0

                elif metric == "errors":
                    if granularity == "hour":
                        key = f"metrics:errors:hour:{date_str}:{hour_str}"
                    else:
                        # Sum all hourly errors for the day
                        total = 0
                        for h in range(24):
                            hkey = f"metrics:errors:hour:{date_str}:{h:02d}"
                            total += int(await redis._client.get(hkey) or 0)
                        value = float(total)

                elif metric == "tokens":
                    if user_id:
                        tenant_id = auth.tenant_id or "public"
                        input_key = (
                            f"metrics:tokens:tenant:{tenant_id}:user:{user_id}:"
                            f"input:{date_str}"
                        )
                        output_key = (
                            f"metrics:tokens:tenant:{tenant_id}:user:{user_id}:"
                            f"output:{date_str}"
                        )
                    else:
                        input_key = f"metrics:tokens:input:{date_str}"
                        output_key = f"metrics:tokens:output:{date_str}"
                    input_val = int(await redis._client.get(input_key) or 0)
                    output_val = int(await redis._client.get(output_key) or 0)
                    value = float(input_val + output_val)

                elif metric == "cost":
                    cost_key = f"metrics:tokens:cost:{date_str}"
                    cost_micro_key = f"metrics:tokens:cost_micro:{date_str}"
                    cost_cents = int(await redis._client.get(cost_key) or 0)
                    cost_microcents = int(await redis._client.get(cost_micro_key) or 0)
                    value = (
                        microcents_to_usd(cost_microcents)
                        if cost_microcents > 0
                        else round(cost_cents / 100, 6)
                    )

                elif metric == "runs":
                    key = f"metrics:runs:total:{date_str}"
                    value = float(await redis._client.get(key) or 0)

                data.append(
                    TimeSeriesDataPoint(
                        timestamp=current.isoformat(),
                        value=value,
                    )
                )

                current += delta

        except Exception:
            pass

    return TimeSeriesResponse(
        metric=metric,
        granularity=granularity,
        start=start.isoformat(),
        end=end.isoformat(),
        data=data,
    )


@router.get("/breakdown", response_model=list[UsageBreakdown])
async def get_usage_breakdown(
    request: Request,
    dimension: str = Query(..., description="model | user | service | provider"),
    start: datetime | None = Query(None, description="Start time"),
    end: datetime | None = Query(None, description="End time"),
    limit: int = 20,
    auth: AuthContext = Depends(get_auth_context),
) -> list[UsageBreakdown]:
    """
    Get usage breakdown by dimension (Service, User, Vendor, Model)
    """
    require_gateway_capability(request, auth, Capability.GATEWAY_METRICS_READ)
    if not end:
        end = datetime.now()
    if not start:
        start = end - timedelta(days=7)

    recorder = get_usage_recorder()
    if not recorder:
        return []

    breakdown = await recorder.get_usage_breakdown(
        tenant_id=auth.tenant_id or "public",
        dimension=dimension,
        start_date=start.date(),
        end_date=end.date(),
        limit=limit,
    )

    return [
        UsageBreakdown(
            dimension_value=item.get(dimension, "unknown"),
            requests=item.get("requests", 0),
            input_tokens=item.get("input_tokens", 0),
            output_tokens=item.get("output_tokens", 0),
            total_tokens=item.get("total_tokens", 0),
            cost_usd=item.get("cost_usd", 0.0),
            percentage=item.get("percentage", 0.0),
        )
        for item in breakdown
    ]


@router.get("/alerts", response_model=AlertsResponse)
async def get_alerts(
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
) -> AlertsResponse:
    """Get current alert status"""
    _require_platform_metrics(request, auth)
    realtime = get_realtime_metrics()
    snapshot = await realtime.get_realtime_snapshot()

    alerts = _check_alerts(snapshot)

    return AlertsResponse(
        alerts=alerts,
        last_check=datetime.now().isoformat(),
    )


@router.get("/user/{user_id}", response_model=UserDashboard)
async def get_user_dashboard(
    user_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
) -> UserDashboard:
    """
    Get per-user dashboard (multi-tenant support)

    Shows metrics filtered to a specific user.
    """
    require_gateway_capability(request, auth, Capability.GATEWAY_METRICS_READ)
    realtime = get_realtime_metrics()
    tenant_id = auth.tenant_id or "public"
    user_metrics = await realtime.get_user_metrics(user_id, tenant_id=tenant_id)

    # Get additional metrics from UsageRecorder first, fall back to MetricsRecorder
    usage_recorder = get_usage_recorder()

    requests_today = 0
    avg_latency = 0
    error_rate = None
    cost_usd = 0.0

    if usage_recorder and usage_recorder.database:
        try:
            summary = await usage_recorder.get_usage_summary(
                tenant_id=auth.tenant_id,
                user_id=user_id,
                start_date=date.today(),
                end_date=date.today(),
            )
            requests_today = summary.get("total_requests", 0)
            avg_latency = summary.get("avg_latency_ms", 0)
            success_rate = summary.get("success_rate")
            error_rate = 100 - success_rate if requests_today > 0 and success_rate is not None else None
            cost_usd = summary.get("total_cost_usd", 0.0)
        except Exception as e:
            logger.warning(f"Failed to get user dashboard from UsageRecorder: {e}")

    tokens = user_metrics.get("tokens", {})

    return UserDashboard(
        user_id=user_id,
        tokens=TokenMetrics(
            total=tokens.get("total", 0),
            input_tokens=tokens.get("input", 0),
            output_tokens=tokens.get("output", 0),
            cost_usd=cost_usd,
            per_minute=0.0,
        ),
        active_threads=user_metrics.get("active_threads", 0),
        requests_today=requests_today,
        avg_latency_ms=avg_latency,
        error_rate=error_rate,
        timestamp=datetime.now().isoformat(),
    )


@router.get("/summary")
async def get_dashboard_summary(
    request: Request,
    period: str = Query("today", description="today | week | month"),
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    """
    Get aggregated dashboard summary

    Returns key metrics for the specified period.
    """
    _require_platform_metrics(request, auth)
    recorder = get_metrics_recorder()
    realtime = get_realtime_metrics()
    usage_recorder = get_usage_recorder()

    # Get summary from UsageRecorder (Postgres)
    summary_data = {}
    if usage_recorder and usage_recorder.database:
        try:
            start_date = date.today()
            if period == "week":
                start_date = date.today() - timedelta(days=7)
            elif period == "month":
                start_date = date.today() - timedelta(days=30)

            summary = await usage_recorder.get_usage_summary(
                tenant_id=auth.tenant_id or "public", start_date=start_date, end_date=date.today()
            )
            summary_data = {
                "total_requests": summary.get("total_requests", 0),
                "success_rate": summary.get("success_rate"),
                "avg_latency_ms": summary.get("avg_latency_ms", 0),
                "total_tokens": summary.get("total_tokens", 0),
                "estimated_cost_usd": summary.get("total_cost_usd", 0),
                "total_runs": 0,  # UsageRecorder doesn't track runs fully separate yet
            }
            last_ingested_at = await usage_recorder.get_last_ingested_at(
                tenant_id=auth.tenant_id or "public",
                start_date=start_date,
                end_date=date.today(),
                granularity="day",
            )
            data_status, data_freshness_minutes = compute_data_status(
                last_ingested_at,
                total_requests=summary_data["total_requests"],
            )
            if summary.get("data_status") in {"collection_error", "unavailable"}:
                data_status = summary["data_status"]
            summary_data.update(
                {
                    "data_status": data_status,
                    "data_freshness_minutes": data_freshness_minutes,
                    "last_ingested_at": last_ingested_at.isoformat()
                    if last_ingested_at
                    else None,
                    "data_source": "usage_aggregates",
                }
            )
        except Exception as e:
            logger.warning(f"Failed to fetch summary from UsageRecorder: {e}")

    # If UsageRecorder failed or empty (and period is today), try Redis fallback
    if not summary_data and period == "today":
        today_summary = await recorder.get_today_summary()
        summary_data = {
            "total_requests": today_summary.get("total_requests", 0),
            "success_rate": today_summary.get("success_rate"),
            "avg_latency_ms": today_summary.get("avg_latency_ms", 0),
            "total_tokens": today_summary.get("total_tokens", 0),
            "estimated_cost_usd": today_summary.get("estimated_cost_usd", 0),
            "total_runs": today_summary.get("total_runs", 0),
            "data_status": today_summary.get("data_status", "empty"),
            "data_freshness_minutes": today_summary.get("data_freshness_minutes", 9999),
            "last_ingested_at": today_summary.get("last_ingested_at"),
            "data_source": today_summary.get("data_source", "none"),
        }

    # If still empty, use defaults
    if not summary_data:
        summary_data = {
            "total_requests": 0,
            "success_rate": None,
            "avg_latency_ms": 0,
            "total_tokens": 0,
            "estimated_cost_usd": 0,
            "total_runs": 0,
            "data_status": "empty",
            "data_freshness_minutes": 9999,
            "last_ingested_at": None,
            "data_source": "none",
        }

    if not summary_data.get("total_requests"):
        summary_data["success_rate"] = None
    # Get real-time snapshot for non-historical data
    snapshot = await realtime.get_realtime_snapshot()

    # Get hourly trend (mix of Redis and UsageRecorder depending on period?)
    # For now keep Redis for hourly trend on "today", but could enhance later
    requests_by_hour = []
    if period == "today":
        ts_today = await recorder.get_today_summary()
        requests_by_hour = ts_today.get("requests_by_hour", [])

    return {
        "period": period,
        "overview": {
            "total_requests": summary_data.get("total_requests", 0),
            "success_rate": summary_data.get("success_rate"),
            "avg_latency_ms": summary_data.get("avg_latency_ms", 0),
            "total_tokens": summary_data.get("total_tokens", 0),
            "estimated_cost_usd": summary_data.get("estimated_cost_usd", 0),
            "total_runs": summary_data.get("total_runs", 0),
        },
        "realtime": {
            "rps": snapshot.rps,
            "active_users": snapshot.active_users,
            "concurrent_requests": snapshot.concurrent_requests,
            "queue_depth": snapshot.queue_depth,
        },
        "latency": {
            "p50": summary_data.get(
                "latency_p50", 0
            ),  # Note: UsageRecorder summary doesn't have percentiles yet
            "p95": summary_data.get("latency_p95", 0),
            "p99": summary_data.get("latency_p99", 0),
        },
        "hourly_trend": requests_by_hour,
        "timestamp": datetime.now().isoformat(),
        "data_status": summary_data.get("data_status", "ok"),
        "data_freshness_minutes": summary_data.get("data_freshness_minutes", 0),
        "last_ingested_at": summary_data.get("last_ingested_at"),
        "data_source": summary_data.get("data_source", "usage_aggregates"),
    }


# ============ Helper Functions ============


def _check_alerts(snapshot: RealtimeSnapshot) -> list[AlertStatus]:
    """Check metrics against thresholds and return alerts"""
    alerts = []

    # Latency P95 alert
    lat_thresholds = ALERT_THRESHOLDS["latency_p95"]
    if snapshot.latency_p95 > lat_thresholds["critical"]:
        alerts.append(
            AlertStatus(
                name="latency_p95",
                level="critical",
                message=f"P95 latency ({snapshot.latency_p95}ms) exceeds critical threshold",
                threshold=lat_thresholds["critical"],
                current_value=snapshot.latency_p95,
                triggered_at=datetime.now().isoformat(),
            )
        )
    elif snapshot.latency_p95 > lat_thresholds["warning"]:
        alerts.append(
            AlertStatus(
                name="latency_p95",
                level="warning",
                message=f"P95 latency ({snapshot.latency_p95}ms) exceeds warning threshold",
                threshold=lat_thresholds["warning"],
                current_value=snapshot.latency_p95,
                triggered_at=datetime.now().isoformat(),
            )
        )
    else:
        alerts.append(
            AlertStatus(
                name="latency_p95",
                level="ok",
                message="P95 latency is within normal range",
                threshold=lat_thresholds["warning"],
                current_value=snapshot.latency_p95,
            )
        )

    # Error rate alert
    err_thresholds = ALERT_THRESHOLDS["error_rate"]
    if snapshot.error_rate > err_thresholds["critical"]:
        alerts.append(
            AlertStatus(
                name="error_rate",
                level="critical",
                message=f"Error rate ({snapshot.error_rate:.1f}%) exceeds critical threshold",
                threshold=err_thresholds["critical"],
                current_value=snapshot.error_rate,
                triggered_at=datetime.now().isoformat(),
            )
        )
    elif snapshot.error_rate > err_thresholds["warning"]:
        alerts.append(
            AlertStatus(
                name="error_rate",
                level="warning",
                message=f"Error rate ({snapshot.error_rate:.1f}%) exceeds warning threshold",
                threshold=err_thresholds["warning"],
                current_value=snapshot.error_rate,
                triggered_at=datetime.now().isoformat(),
            )
        )
    else:
        alerts.append(
            AlertStatus(
                name="error_rate",
                level="ok",
                message="Error rate is within normal range",
                threshold=err_thresholds["warning"],
                current_value=snapshot.error_rate,
            )
        )

    # Queue depth alert
    queue_thresholds = ALERT_THRESHOLDS["queue_depth"]
    if snapshot.queue_depth > queue_thresholds["critical"]:
        alerts.append(
            AlertStatus(
                name="queue_depth",
                level="critical",
                message=f"Queue depth ({snapshot.queue_depth}) exceeds critical threshold",
                threshold=queue_thresholds["critical"],
                current_value=snapshot.queue_depth,
                triggered_at=datetime.now().isoformat(),
            )
        )
    elif snapshot.queue_depth > queue_thresholds["warning"]:
        alerts.append(
            AlertStatus(
                name="queue_depth",
                level="warning",
                message=f"Queue depth ({snapshot.queue_depth}) exceeds warning threshold",
                threshold=queue_thresholds["warning"],
                current_value=snapshot.queue_depth,
                triggered_at=datetime.now().isoformat(),
            )
        )
    else:
        alerts.append(
            AlertStatus(
                name="queue_depth",
                level="ok",
                message="Queue depth is within normal range",
                threshold=queue_thresholds["warning"],
                current_value=snapshot.queue_depth,
            )
        )

    # Utilization alert
    util_thresholds = ALERT_THRESHOLDS["utilization"]
    utilization = snapshot.concurrent_requests / max(snapshot.max_concurrent, 1) * 100
    if utilization > util_thresholds["critical"]:
        alerts.append(
            AlertStatus(
                name="utilization",
                level="critical",
                message=f"System utilization ({utilization:.1f}%) exceeds critical threshold",
                threshold=util_thresholds["critical"],
                current_value=utilization,
                triggered_at=datetime.now().isoformat(),
            )
        )
    elif utilization > util_thresholds["warning"]:
        alerts.append(
            AlertStatus(
                name="utilization",
                level="warning",
                message=f"System utilization ({utilization:.1f}%) exceeds warning threshold",
                threshold=util_thresholds["warning"],
                current_value=utilization,
                triggered_at=datetime.now().isoformat(),
            )
        )
    else:
        alerts.append(
            AlertStatus(
                name="utilization",
                level="ok",
                message="System utilization is within normal range",
                threshold=util_thresholds["warning"],
                current_value=utilization,
            )
        )

    return alerts
