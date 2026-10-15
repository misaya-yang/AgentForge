"""
Metrics API - 系统指标统计接口

Provides dashboard metrics including:
- Request metrics (count, success rate, latency)
- Token consumption (input/output tokens, cost)
- LangGraph run metrics (executions, success rate)
- Time-series data with custom date range support
"""

from datetime import date, datetime, timedelta

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from pydantic import BaseModel

from ...api.deps import (
    AuthContext,
    get_auth_context,
    require_gateway_capability,
    require_platform_admin,
)
from ...core.auth.permissions import Capability
from ...services.billing.pricing_catalog import microcents_to_usd
from ...services.metrics import compute_data_status, get_metrics_recorder

router = APIRouter(prefix="/metrics", tags=["metrics"])


# ============ Response Models ============


class HourlyMetric(BaseModel):
    """每小时指标"""

    hour: str
    count: int


class MetricsSummary(BaseModel):
    """指标摘要 - 扩展版本包含 token 和 LLM 指标"""

    # 基础请求指标
    total_requests: int
    success_rate: float
    avg_latency_ms: int
    active_services: int
    requests_by_hour: list[HourlyMetric]

    # 延迟百分位数
    latency_p50: int = 0
    latency_p95: int = 0
    latency_p99: int = 0

    # Token 消耗指标
    total_tokens: int = 0
    prompt_tokens: int = 0
    completion_tokens: int = 0
    estimated_cost_usd: float = 0.0

    # LangGraph Run 指标
    total_runs: int = 0
    run_success_rate: float = 100.0
    avg_run_duration_ms: int = 0

    # 元数据
    last_updated: str
    is_simulated: bool = False
    data_status: str = "empty"
    data_freshness_minutes: int = 9999
    last_ingested_at: str | None = None
    data_source: str = "none"


class TimeSeriesPoint(BaseModel):
    """时间序列数据点"""

    timestamp: str
    value: float


class TimeSeriesResponse(BaseModel):
    """时间序列响应"""

    metric: str
    granularity: str
    start: str
    end: str
    data: list[TimeSeriesPoint]


class TokenUsagePeriod(BaseModel):
    """Token 使用周期"""

    period: str
    input_tokens: int
    output_tokens: int
    cost_usd: float


class TokenUsageResponse(BaseModel):
    """Token 使用响应"""

    total_input_tokens: int
    total_output_tokens: int
    total_tokens: int
    estimated_cost_usd: float
    by_period: list[TokenUsagePeriod]


class BreakdownItem(BaseModel):
    """指标分解项"""

    name: str
    count: int
    percentage: float


class BreakdownResponse(BaseModel):
    """指标分解响应"""

    dimension: str
    items: list[BreakdownItem]


class SecurityEventBreakdownItem(BaseModel):
    """Security event breakdown item."""

    name: str
    count: int
    percentage: float


class SecurityEventBreakdownResponse(BaseModel):
    """Security event breakdown response."""

    dimension: str
    event_type: str
    items: list[SecurityEventBreakdownItem]
    start_date: str
    end_date: str
    data_status: str
    data_freshness_minutes: int
    last_ingested_at: str | None = None


class SecurityEventTimeSeriesPoint(BaseModel):
    """Security event time series point."""

    date: str
    count: int


class SecurityEventTimeSeriesResponse(BaseModel):
    """Security event time series response."""

    dimension: str
    event_type: str
    data: list[SecurityEventTimeSeriesPoint]
    start_date: str
    end_date: str
    data_status: str
    data_freshness_minutes: int
    last_ingested_at: str | None = None


# ============ API Endpoints ============


@router.get("/summary", response_model=MetricsSummary)
async def get_metrics_summary(
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
) -> MetricsSummary:
    """
    获取系统指标摘要（仅管理员/运营角色）

    返回:
    - total_requests: 总请求数
    - success_rate: 成功率 (%)
    - avg_latency_ms: 平均延迟 (ms)
    - latency_p50/p95/p99: 延迟百分位数
    - total_tokens: 总 Token 数
    - prompt_tokens: 输入 Token 数
    - completion_tokens: 输出 Token 数
    - estimated_cost_usd: 估算成本 (USD)
    - total_runs: LangGraph 执行次数
    - run_success_rate: 执行成功率
    - active_services: 活跃服务数
    - requests_by_hour: 24小时请求趋势
    """
    require_platform_admin(request, auth, Capability.GATEWAY_METRICS_READ)

    # 从 MetricsRecorder 获取今日摘要
    metrics_recorder = get_metrics_recorder()
    summary = await metrics_recorder.get_today_summary()

    # 获取活跃服务数
    database = getattr(request.app.state, "database", None)
    active_services = 0

    if database:
        try:
            services = await database.get_all_services()
            active_services = len([s for s in services if s.get("status") == "healthy"])
        except Exception:
            active_services = 0

    # 标记是否为模拟数据（没有真实请求数据时）
    is_simulated = summary["total_requests"] == 0

    # 构建响应
    return MetricsSummary(
        total_requests=summary["total_requests"],
        success_rate=summary["success_rate"],
        avg_latency_ms=summary["avg_latency_ms"],
        active_services=active_services,
        requests_by_hour=[
            HourlyMetric(hour=h["hour"], count=h["count"]) for h in summary["requests_by_hour"]
        ],
        latency_p50=summary.get("latency_p50", 0),
        latency_p95=summary.get("latency_p95", 0),
        latency_p99=summary.get("latency_p99", 0),
        total_tokens=summary.get("total_tokens", 0),
        prompt_tokens=summary.get("prompt_tokens", 0),
        completion_tokens=summary.get("completion_tokens", 0),
        estimated_cost_usd=summary.get("estimated_cost_usd", 0.0),
        total_runs=summary.get("total_runs", 0),
        run_success_rate=summary.get("run_success_rate", 100.0),
        avg_run_duration_ms=summary.get("avg_run_duration_ms", 0),
        last_updated=datetime.now().isoformat(),
        is_simulated=is_simulated,
        data_status=summary.get("data_status", "empty"),
        data_freshness_minutes=summary.get("data_freshness_minutes", 9999),
        last_ingested_at=summary.get("last_ingested_at"),
        data_source=summary.get("data_source", "none"),
    )


@router.get("/timeseries", response_model=TimeSeriesResponse)
async def get_metrics_timeseries(
    request: Request,
    metric: str = Query(..., description="Metric name: requests, tokens, latency, errors, runs"),
    start: datetime = Query(..., description="Start datetime (ISO format)"),
    end: datetime = Query(..., description="End datetime (ISO format)"),
    granularity: str = Query("hour", description="Granularity: minute, hour, day"),
    auth: AuthContext = Depends(get_auth_context),
) -> TimeSeriesResponse:
    """
    获取时间序列数据（仅管理员/运营角色）

    支持的指标:
    - requests: 请求数
    - tokens: Token 使用量
    - latency: 平均延迟
    - errors: 错误数
    - runs: LangGraph 执行数
    """
    require_platform_admin(request, auth, Capability.GATEWAY_METRICS_READ)

    redis = getattr(request.app.state, "redis", None)
    data = []

    if redis and redis._client:
        try:
            # 生成时间点列表
            current = start
            delta = {
                "minute": timedelta(minutes=1),
                "hour": timedelta(hours=1),
                "day": timedelta(days=1),
            }.get(granularity, timedelta(hours=1))

            while current <= end:
                date_str = current.strftime("%Y-%m-%d")
                hour_str = current.strftime("%H")

                # 根据指标类型获取值
                value = 0
                if metric == "requests":
                    if granularity == "hour":
                        key = f"metrics:requests:hour:{date_str}:{hour_str}"
                        value = int(await redis._client.get(key) or 0)
                    else:
                        key = f"metrics:requests:total:{date_str}"
                        value = int(await redis._client.get(key) or 0)
                elif metric == "tokens":
                    input_key = f"metrics:tokens:input:{date_str}"
                    output_key = f"metrics:tokens:output:{date_str}"
                    input_val = int(await redis._client.get(input_key) or 0)
                    output_val = int(await redis._client.get(output_key) or 0)
                    value = input_val + output_val
                elif metric == "errors":
                    if granularity == "hour":
                        key = f"metrics:errors:hour:{date_str}:{hour_str}"
                        value = int(await redis._client.get(key) or 0)
                elif metric == "runs":
                    key = f"metrics:runs:total:{date_str}"
                    value = int(await redis._client.get(key) or 0)

                data.append(
                    TimeSeriesPoint(
                        timestamp=current.isoformat(),
                        value=float(value),
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


@router.get("/tokens", response_model=TokenUsageResponse)
async def get_token_usage(
    request: Request,
    start_date: date | None = Query(None, description="Start date (YYYY-MM-DD)"),
    end_date: date | None = Query(None, description="End date (YYYY-MM-DD)"),
    auth: AuthContext = Depends(get_auth_context),
) -> TokenUsageResponse:
    """
    获取 Token 使用统计（仅管理员/运营角色）

    返回指定日期范围内的 Token 消耗情况
    """
    # 权限检查：仅管理员或运营角色可查看 metrics
    require_platform_admin(request, auth, Capability.GATEWAY_METRICS_READ)

    redis = getattr(request.app.state, "redis", None)

    # 默认为过去 7 天
    if not end_date:
        end_date = date.today()
    if not start_date:
        start_date = end_date - timedelta(days=6)

    total_input = 0
    total_output = 0
    total_cost_usd = 0.0
    by_period = []

    if redis and redis._client:
        try:
            current = start_date
            while current <= end_date:
                date_str = current.strftime("%Y-%m-%d")

                input_key = f"metrics:tokens:input:{date_str}"
                output_key = f"metrics:tokens:output:{date_str}"
                cost_key = f"metrics:tokens:cost:{date_str}"
                cost_micro_key = f"metrics:tokens:cost_micro:{date_str}"

                input_val = int(await redis._client.get(input_key) or 0)
                output_val = int(await redis._client.get(output_key) or 0)
                cost_val = int(await redis._client.get(cost_key) or 0)
                cost_micro_val = int(await redis._client.get(cost_micro_key) or 0)
                period_cost_usd = (
                    microcents_to_usd(cost_micro_val)
                    if cost_micro_val > 0
                    else round(cost_val / 100, 6)
                )

                total_input += input_val
                total_output += output_val
                total_cost_usd += period_cost_usd

                by_period.append(
                    TokenUsagePeriod(
                        period=date_str,
                        input_tokens=input_val,
                        output_tokens=output_val,
                        cost_usd=round(period_cost_usd, 6),
                    )
                )

                current += timedelta(days=1)

        except Exception:
            pass

    return TokenUsageResponse(
        total_input_tokens=total_input,
        total_output_tokens=total_output,
        total_tokens=total_input + total_output,
        estimated_cost_usd=round(total_cost_usd, 6),
        by_period=by_period,
    )


@router.get("/breakdown", response_model=BreakdownResponse)
async def get_metrics_breakdown(
    request: Request,
    dimension: str = Query(..., description="Dimension: assistant, service, user"),
    limit: int = Query(10, description="Max items to return"),
    date_str: str | None = Query(None, description="Date (YYYY-MM-DD), defaults to today"),
    auth: AuthContext = Depends(get_auth_context),
) -> BreakdownResponse:
    """
    获取指标分解（仅管理员/运营角色）

    按维度分解指标，返回 Top N 项目
    """
    # 权限检查：仅管理员或运营角色可查看 metrics
    require_platform_admin(request, auth, Capability.GATEWAY_METRICS_READ)

    redis = getattr(request.app.state, "redis", None)
    items = []

    if not date_str:
        date_str = datetime.now().strftime("%Y-%m-%d")

    if redis and redis._client:
        try:
            # 根据维度构建 key 模式
            if dimension == "assistant":
                pattern = f"metrics:runs:assistant:*:{date_str}"
            elif dimension == "service":
                pattern = f"metrics:service:*:{date_str}"
            else:
                pattern = f"metrics:tokens:user:*:input:{date_str}"

            # 扫描匹配的 keys
            cursor = 0
            counts = {}
            while True:
                cursor, keys = await redis._client.scan(cursor, match=pattern, count=100)
                for key in keys:
                    value = int(await redis._client.get(key) or 0)
                    # 提取 ID
                    parts = key.split(":")
                    if dimension == "assistant":
                        item_id = parts[3] if len(parts) > 3 else "unknown"
                    elif dimension == "service":
                        item_id = parts[2] if len(parts) > 2 else "unknown"
                    else:
                        item_id = parts[3] if len(parts) > 3 else "unknown"
                    counts[item_id] = counts.get(item_id, 0) + value

                if cursor == 0:
                    break

            # 排序并取 Top N
            total = sum(counts.values()) or 1
            sorted_items = sorted(counts.items(), key=lambda x: x[1], reverse=True)[:limit]

            items = [
                BreakdownItem(
                    name=name,
                    count=count,
                    percentage=round(count / total * 100, 1),
                )
                for name, count in sorted_items
            ]

        except Exception:
            pass

    return BreakdownResponse(dimension=dimension, items=items)


@router.get("/security/breakdown", response_model=SecurityEventBreakdownResponse)
async def get_security_event_breakdown(
    request: Request,
    dimension: str = Query("user", description="Dimension: user, service"),
    event_type: str = Query("auth_failed", description="Event type: auth_failed, rate_limited"),
    start_date: date | None = Query(None, description="Start date (YYYY-MM-DD)"),
    end_date: date | None = Query(None, description="End date (YYYY-MM-DD)"),
    limit: int = Query(20, ge=1, le=100, description="Max items to return"),
    auth: AuthContext = Depends(get_auth_context),
) -> SecurityEventBreakdownResponse:
    """Get security event breakdown by user or service."""
    require_gateway_capability(request, auth, Capability.GATEWAY_METRICS_READ)

    valid_dimensions = {"user", "service"}
    if dimension not in valid_dimensions:
        raise HTTPException(status_code=400, detail=f"Invalid dimension: {dimension}")

    valid_event_types = {"auth_failed", "rate_limited", "quota_exceeded"}
    if event_type not in valid_event_types:
        raise HTTPException(status_code=400, detail=f"Invalid event_type: {event_type}")

    if not end_date:
        end_date = date.today()
    if not start_date:
        start_date = end_date - timedelta(days=30)

    db = getattr(request.app.state, "database", None)
    items: list[SecurityEventBreakdownItem] = []
    last_ingested_at = None

    tenant_id = auth.tenant_id or "public"
    if db and getattr(db, "enabled", False):
        rows = await db.get_security_event_breakdown(
            tenant_id=tenant_id,
            dimension=dimension,
            event_type=event_type,
            start_date=start_date,
            end_date=end_date,
            limit=limit,
        )
        total = sum(int(r.get("total_events") or 0) for r in rows) or 1
        items = [
            SecurityEventBreakdownItem(
                name=str(r.get("dimension_value") or "unknown"),
                count=int(r.get("total_events") or 0),
                percentage=round(int(r.get("total_events") or 0) / total * 100, 1),
            )
            for r in rows
        ]
        last_ingested_at = await db.get_security_event_last_ingested_at(
            tenant_id=tenant_id,
            event_type=event_type,
            start_date=start_date,
            end_date=end_date,
        )

    total_events = sum(item.count for item in items)
    data_status, freshness_minutes = compute_data_status(
        last_ingested_at,
        total_requests=total_events,
    )

    return SecurityEventBreakdownResponse(
        dimension=dimension,
        event_type=event_type,
        items=items,
        start_date=start_date.isoformat(),
        end_date=end_date.isoformat(),
        data_status=data_status,
        data_freshness_minutes=freshness_minutes,
        last_ingested_at=last_ingested_at.isoformat() if last_ingested_at else None,
    )


@router.get("/security/timeseries", response_model=SecurityEventTimeSeriesResponse)
async def get_security_event_timeseries(
    request: Request,
    dimension: str = Query("user", description="Dimension: user, service"),
    event_type: str = Query("auth_failed", description="Event type: auth_failed, rate_limited"),
    start_date: date | None = Query(None, description="Start date (YYYY-MM-DD)"),
    end_date: date | None = Query(None, description="End date (YYYY-MM-DD)"),
    user_id: str | None = Query(None, description="Filter by user ID"),
    service_id: str | None = Query(None, description="Filter by service ID"),
    auth: AuthContext = Depends(get_auth_context),
) -> SecurityEventTimeSeriesResponse:
    """Get security event time series."""
    require_gateway_capability(request, auth, Capability.GATEWAY_METRICS_READ)

    valid_dimensions = {"user", "service"}
    if dimension not in valid_dimensions:
        raise HTTPException(status_code=400, detail=f"Invalid dimension: {dimension}")

    valid_event_types = {"auth_failed", "rate_limited", "quota_exceeded"}
    if event_type not in valid_event_types:
        raise HTTPException(status_code=400, detail=f"Invalid event_type: {event_type}")

    if not end_date:
        end_date = date.today()
    if not start_date:
        start_date = end_date - timedelta(days=30)

    db = getattr(request.app.state, "database", None)
    data: list[SecurityEventTimeSeriesPoint] = []
    last_ingested_at = None

    tenant_id = auth.tenant_id or "public"
    if db and getattr(db, "enabled", False):
        rows = await db.get_security_event_timeseries(
            tenant_id=tenant_id,
            event_type=event_type,
            start_date=start_date,
            end_date=end_date,
            user_id=user_id if dimension == "user" else None,
            service_id=service_id if dimension == "service" else None,
        )
        data = [
            SecurityEventTimeSeriesPoint(
                date=str(r.get("date")),
                count=int(r.get("total_events") or 0),
            )
            for r in rows
        ]
        last_ingested_at = await db.get_security_event_last_ingested_at(
            tenant_id=tenant_id,
            event_type=event_type,
            start_date=start_date,
            end_date=end_date,
        )

    total_events = sum(point.count for point in data)
    data_status, freshness_minutes = compute_data_status(
        last_ingested_at,
        total_requests=total_events,
    )

    return SecurityEventTimeSeriesResponse(
        dimension=dimension,
        event_type=event_type,
        data=data,
        start_date=start_date.isoformat(),
        end_date=end_date.isoformat(),
        data_status=data_status,
        data_freshness_minutes=freshness_minutes,
        last_ingested_at=last_ingested_at.isoformat() if last_ingested_at else None,
    )
