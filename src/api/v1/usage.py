"""
Usage API - Usage tracking and analytics endpoints.

Provides:
- Usage summary and statistics
- Breakdown by model/user/service/assistant
- Time-series data for charts
- Export functionality
"""

import csv
from datetime import date, datetime, timedelta
from io import StringIO
from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from fastapi.responses import StreamingResponse
from pydantic import BaseModel, Field

from ...api.deps import AuthContext, get_auth_context, require_gateway_capability
from ...core.auth.permissions import Capability
from ...services.metrics import compute_data_status, get_usage_recorder

router = APIRouter(prefix="/usage", tags=["usage"])


def _require_usage_read(request: Request, auth: AuthContext) -> None:
    require_gateway_capability(request, auth, Capability.GATEWAY_USAGE_READ)


def _is_usage_operator(auth: AuthContext) -> bool:
    return any(role in {"admin", "operator"} for role in auth.roles)


def _require_usage_tenant_scope(request: Request, auth: AuthContext) -> None:
    if auth.tenant_id:
        return
    raise HTTPException(
        status_code=403,
        detail={
            "message": "Permission denied: tenant scope required for usage queries",
            "required_capability": Capability.GATEWAY_USAGE_READ.value,
            "required_permission": "console:usage:view",
            "trace_id": str(getattr(request.state, "request_id", "") or ""),
        },
    )


def _scope_usage_user_id(request: Request, auth: AuthContext, user_id: str | None) -> str | None:
    _require_usage_tenant_scope(request, auth)
    if _is_usage_operator(auth):
        return user_id
    return auth.user_id or user_id


# ============ Response Models ============


class UsageSummaryResponse(BaseModel):
    """Usage summary response."""

    total_requests: int
    success_rate: float | None
    total_input_tokens: int
    total_output_tokens: int
    total_tokens: int
    total_cost_usd: float
    avg_latency_ms: int
    start_date: str
    end_date: str
    data_status: str
    data_freshness_minutes: int
    last_ingested_at: str | None = None


class UsageBreakdownItem(BaseModel):
    """Single item in usage breakdown."""

    dimension_value: str = Field(..., alias="model")
    requests: int
    input_tokens: int
    output_tokens: int
    total_tokens: int
    cost_usd: float
    percentage: float

    class Config:
        populate_by_name = True


class UsageBreakdownResponse(BaseModel):
    """Usage breakdown response."""

    dimension: str
    items: list[dict[str, Any]]
    start_date: str
    end_date: str
    total_cost_usd: float
    data_status: str
    data_freshness_minutes: int
    last_ingested_at: str | None = None


class UsageTimeSeriesPoint(BaseModel):
    """Single point in time series."""

    date: str
    requests: int
    input_tokens: int
    output_tokens: int
    total_tokens: int
    cost_usd: float
    avg_latency_ms: int


class UsageTimeSeriesResponse(BaseModel):
    """Usage time series response."""

    data: list[UsageTimeSeriesPoint]
    start_date: str
    end_date: str
    granularity: str = "day"
    data_status: str
    data_freshness_minutes: int
    last_ingested_at: str | None = None


class UserUsageResponse(BaseModel):
    """Per-user usage response."""

    user_id: str
    summary: UsageSummaryResponse
    top_models: list[dict[str, Any]]
    daily_trend: list[UsageTimeSeriesPoint]


class LatencyBreakdownPoint(BaseModel):
    """Latency breakdown point."""

    date: str
    request_count: int
    avg_total_ms: int
    p50_total_ms: int
    p95_total_ms: int
    p99_total_ms: int
    avg_first_token_ms: int
    avg_llm_inference_ms: int
    avg_retrieval_ms: int
    avg_tool_call_ms: int
    avg_overhead_ms: int


class LatencyBreakdownResponse(BaseModel):
    """Latency breakdown response."""

    data: list[LatencyBreakdownPoint]
    start_date: str
    end_date: str
    granularity: str
    data_status: str
    data_freshness_minutes: int
    last_ingested_at: str | None = None


class FailureBreakdownItem(BaseModel):
    """Failure breakdown item."""

    dimension: str
    dimension_value: str
    error_type: str
    failure_count: int
    success_count: int
    total_count: int
    success_rate: float


class FailureBreakdownResponse(BaseModel):
    """Failure breakdown response."""

    dimension: str
    items: list[FailureBreakdownItem]
    start_date: str
    end_date: str


class RequestTraceResponse(BaseModel):
    """Sampled request trace response."""

    trace_id: str
    request_id: str
    tenant_id: str
    user_id: str
    service_id: str
    assistant_id: str
    provider: str
    model: str
    status: str
    error_type: str | None = None
    request_total_duration_ms: int
    first_token_latency_ms: int
    llm_inference_duration_ms: int
    retrieval_duration_ms: int
    tool_call_duration_ms: int
    agent_or_graph_overhead_ms: int
    input_tokens: int
    output_tokens: int
    total_cost_usd: float
    sample_reason: str
    trace_steps: list[dict[str, Any]]
    metadata: dict[str, Any]
    timestamp: str | None = None


# ============ API Endpoints ============


@router.get("/summary", response_model=UsageSummaryResponse)
async def get_usage_summary(
    request: Request,
    start_date: date | None = Query(None, description="Start date (YYYY-MM-DD)"),
    end_date: date | None = Query(None, description="End date (YYYY-MM-DD)"),
    user_id: str | None = Query(None, description="Filter by user ID"),
    model: str | None = Query(None, description="Filter by model"),
    service_id: str | None = Query(None, description="Filter by service ID"),
    assistant_id: str | None = Query(None, description="Filter by assistant ID"),
    provider: str | None = Query(None, description="Filter by provider"),
    auth: AuthContext = Depends(get_auth_context),
) -> UsageSummaryResponse:
    """
    Get aggregated usage summary.

    Returns total requests, tokens, cost, and latency for the specified period.
    """
    _require_usage_read(request, auth)
    user_id = _scope_usage_user_id(request, auth, user_id)
    recorder = get_usage_recorder()

    # Default to last 7 days
    if not end_date:
        end_date = date.today()
    if not start_date:
        start_date = end_date - timedelta(days=7)

    summary = await recorder.get_usage_summary(
        tenant_id=auth.tenant_id,
        start_date=start_date,
        end_date=end_date,
        user_id=user_id,
        model=model,
        service_id=service_id,
        assistant_id=assistant_id,
        provider=provider,
    )

    last_ingested_at = await recorder.get_last_ingested_at(
        tenant_id=auth.tenant_id,
        start_date=start_date,
        end_date=end_date,
        granularity="day",
    )
    data_status, freshness_minutes = compute_data_status(
        last_ingested_at,
        total_requests=summary.get("total_requests"),
    )

    summary = dict(summary)
    source_status = summary.pop("data_status", None)
    if source_status in {"collection_error", "unavailable"}:
        data_status = source_status
    if not summary.get("total_requests"):
        summary["success_rate"] = None
    return UsageSummaryResponse(
        **summary,
        data_status=data_status,
        data_freshness_minutes=freshness_minutes,
        last_ingested_at=last_ingested_at.isoformat() if last_ingested_at else None,
    )


@router.get("/breakdown", response_model=UsageBreakdownResponse)
async def get_usage_breakdown(
    request: Request,
    dimension: str = Query(
        "model", description="Breakdown dimension: model, user, assistant, service"
    ),
    start_date: date | None = Query(None, description="Start date (YYYY-MM-DD)"),
    end_date: date | None = Query(None, description="End date (YYYY-MM-DD)"),
    user_id: str | None = Query(None, description="Filter by user ID"),
    service_id: str | None = Query(None, description="Filter by service ID"),
    limit: int = Query(20, ge=1, le=100, description="Maximum items to return"),
    auth: AuthContext = Depends(get_auth_context),
) -> UsageBreakdownResponse:
    """
    Get usage breakdown by dimension.

    Breaks down usage by model, user, assistant, or service with cost and percentage.
    """
    _require_usage_read(request, auth)
    user_id = _scope_usage_user_id(request, auth, user_id)
    recorder = get_usage_recorder()

    # Validate dimension
    valid_dimensions = {"model", "user", "assistant", "service", "provider"}
    if dimension not in valid_dimensions:
        raise HTTPException(
            status_code=400,
            detail=f"Invalid dimension. Must be one of: {', '.join(valid_dimensions)}",
        )

    # Default to last 7 days
    if not end_date:
        end_date = date.today()
    if not start_date:
        start_date = end_date - timedelta(days=7)

    items = await recorder.get_usage_breakdown(
        tenant_id=auth.tenant_id,
        dimension=dimension,
        start_date=start_date,
        end_date=end_date,
        user_id=user_id,
        service_id=service_id,
        limit=limit,
    )

    total_cost = sum(item.get("cost_usd", 0) for item in items)
    total_requests = sum(item.get("requests", 0) for item in items)
    last_ingested_at = await recorder.get_last_ingested_at(
        tenant_id=auth.tenant_id,
        start_date=start_date,
        end_date=end_date,
        granularity="day",
    )
    data_status, freshness_minutes = compute_data_status(
        last_ingested_at,
        total_requests=total_requests,
    )

    return UsageBreakdownResponse(
        dimension=dimension,
        items=items,
        start_date=start_date.isoformat(),
        end_date=end_date.isoformat(),
        total_cost_usd=round(total_cost, 4),
        data_status=data_status,
        data_freshness_minutes=freshness_minutes,
        last_ingested_at=last_ingested_at.isoformat() if last_ingested_at else None,
    )


@router.get("/timeseries", response_model=UsageTimeSeriesResponse)
async def get_usage_timeseries(
    request: Request,
    start_date: date | None = Query(None, description="Start date (YYYY-MM-DD)"),
    end_date: date | None = Query(None, description="End date (YYYY-MM-DD)"),
    user_id: str | None = Query(None, description="Filter by user ID"),
    model: str | None = Query(None, description="Filter by model"),
    service_id: str | None = Query(None, description="Filter by service ID"),
    provider: str | None = Query(None, description="Filter by provider"),
    granularity: str = Query("day", description="Granularity: hour, day"),
    auth: AuthContext = Depends(get_auth_context),
) -> UsageTimeSeriesResponse:
    """
    Get daily usage time series.

    Returns daily usage data for charts and trend analysis.
    """
    _require_usage_read(request, auth)
    user_id = _scope_usage_user_id(request, auth, user_id)
    recorder = get_usage_recorder()

    # Default to last 30 days
    if not end_date:
        end_date = date.today()
    if not start_date:
        start_date = end_date - timedelta(days=30)

    data = await recorder.get_usage_timeseries(
        tenant_id=auth.tenant_id,
        start_date=start_date,
        end_date=end_date,
        user_id=user_id,
        model=model,
        service_id=service_id,
        provider=provider,
        granularity=granularity,
    )

    total_requests = sum(point.get("requests", 0) for point in data)
    last_ingested_at = await recorder.get_last_ingested_at(
        tenant_id=auth.tenant_id,
        start_date=start_date,
        end_date=end_date,
        granularity=granularity,
    )
    data_status, freshness_minutes = compute_data_status(
        last_ingested_at,
        total_requests=total_requests,
    )

    return UsageTimeSeriesResponse(
        data=[UsageTimeSeriesPoint(**d) for d in data],
        start_date=start_date.isoformat(),
        end_date=end_date.isoformat(),
        granularity=granularity,
        data_status=data_status,
        data_freshness_minutes=freshness_minutes,
        last_ingested_at=last_ingested_at.isoformat() if last_ingested_at else None,
    )


@router.get("/performance-breakdown", response_model=LatencyBreakdownResponse)
async def get_performance_breakdown(
    request: Request,
    start_date: date | None = Query(None, description="Start date (YYYY-MM-DD)"),
    end_date: date | None = Query(None, description="End date (YYYY-MM-DD)"),
    user_id: str | None = Query(None, description="Filter by user ID"),
    model: str | None = Query(None, description="Filter by model"),
    service_id: str | None = Query(None, description="Filter by service ID"),
    provider: str | None = Query(None, description="Filter by provider"),
    granularity: str = Query("day", description="Granularity: hour, day"),
    auth: AuthContext = Depends(get_auth_context),
) -> LatencyBreakdownResponse:
    """Get detailed latency phase breakdown (TTFB/LLM/Retrieval/Tool/Overhead)."""
    _require_usage_read(request, auth)
    user_id = _scope_usage_user_id(request, auth, user_id)
    recorder = get_usage_recorder()

    if not end_date:
        end_date = date.today()
    if not start_date:
        start_date = end_date - timedelta(days=7)

    data = await recorder.get_latency_breakdown_timeseries(
        tenant_id=auth.tenant_id,
        start_date=start_date,
        end_date=end_date,
        user_id=user_id,
        model=model,
        service_id=service_id,
        provider=provider,
        granularity=granularity,
    )

    total_requests = sum(point.get("request_count", 0) for point in data)
    last_ingested_at = await recorder.get_last_ingested_at(
        tenant_id=auth.tenant_id,
        start_date=start_date,
        end_date=end_date,
        granularity=granularity,
    )
    data_status, freshness_minutes = compute_data_status(
        last_ingested_at,
        total_requests=total_requests,
    )

    return LatencyBreakdownResponse(
        data=[LatencyBreakdownPoint(**point) for point in data],
        start_date=start_date.isoformat(),
        end_date=end_date.isoformat(),
        granularity=granularity,
        data_status=data_status,
        data_freshness_minutes=freshness_minutes,
        last_ingested_at=last_ingested_at.isoformat() if last_ingested_at else None,
    )


@router.get("/failure-breakdown", response_model=FailureBreakdownResponse)
async def get_failure_breakdown(
    request: Request,
    dimension: str = Query("service", description="service | model | provider | user"),
    start_date: date | None = Query(None, description="Start date (YYYY-MM-DD)"),
    end_date: date | None = Query(None, description="End date (YYYY-MM-DD)"),
    user_id: str | None = Query(None, description="Filter by user ID"),
    model: str | None = Query(None, description="Filter by model"),
    service_id: str | None = Query(None, description="Filter by service ID"),
    provider: str | None = Query(None, description="Filter by provider"),
    limit: int = Query(50, ge=1, le=200),
    auth: AuthContext = Depends(get_auth_context),
) -> FailureBreakdownResponse:
    """Get failure classification breakdown for operational analysis."""
    _require_usage_read(request, auth)
    user_id = _scope_usage_user_id(request, auth, user_id)
    recorder = get_usage_recorder()
    if dimension not in {"service", "model", "provider", "user"}:
        raise HTTPException(status_code=400, detail="Invalid dimension")

    if not end_date:
        end_date = date.today()
    if not start_date:
        start_date = end_date - timedelta(days=7)

    items = await recorder.get_failure_breakdown(
        tenant_id=auth.tenant_id,
        start_date=start_date,
        end_date=end_date,
        dimension=dimension,
        user_id=user_id,
        model=model,
        service_id=service_id,
        provider=provider,
        limit=limit,
    )
    return FailureBreakdownResponse(
        dimension=dimension,
        items=[FailureBreakdownItem(**item) for item in items],
        start_date=start_date.isoformat(),
        end_date=end_date.isoformat(),
    )


@router.get("/traces", response_model=list[RequestTraceResponse])
async def list_request_traces(
    request: Request,
    start_date: date | None = Query(None, description="Start date (YYYY-MM-DD)"),
    end_date: date | None = Query(None, description="End date (YYYY-MM-DD)"),
    service_id: str | None = Query(None, description="Filter by service ID"),
    user_id: str | None = Query(None, description="Filter by user ID"),
    provider: str | None = Query(None, description="Filter by provider"),
    model: str | None = Query(None, description="Filter by model"),
    status: str | None = Query(None, description="Filter by request status"),
    error_type: str | None = Query(None, description="Filter by error type"),
    limit: int = Query(100, ge=1, le=500),
    auth: AuthContext = Depends(get_auth_context),
) -> list[RequestTraceResponse]:
    """List sampled traces."""
    _require_usage_read(request, auth)
    user_id = _scope_usage_user_id(request, auth, user_id)
    recorder = get_usage_recorder()
    if not end_date:
        end_date = date.today()
    if not start_date:
        start_date = end_date - timedelta(days=7)

    rows = await recorder.list_request_traces(
        tenant_id=auth.tenant_id,
        start_date=start_date,
        end_date=end_date,
        service_id=service_id,
        user_id=user_id,
        provider=provider,
        model=model,
        status=status,
        error_type=error_type,
        limit=limit,
    )
    return [RequestTraceResponse(**row) for row in rows]


@router.get("/traces/{request_id}", response_model=RequestTraceResponse)
async def get_request_trace(
    request_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
) -> RequestTraceResponse:
    """Get latest sampled trace for a request."""
    _require_usage_read(request, auth)
    _require_usage_tenant_scope(request, auth)
    recorder = get_usage_recorder()
    trace = await recorder.get_request_trace_by_id(auth.tenant_id, request_id)
    if not trace:
        raise HTTPException(status_code=404, detail="Trace not found")
    return RequestTraceResponse(**trace)


@router.get("/user/{user_id}", response_model=UserUsageResponse)
async def get_user_usage(
    user_id: str,
    request: Request,
    start_date: date | None = Query(None, description="Start date (YYYY-MM-DD)"),
    end_date: date | None = Query(None, description="End date (YYYY-MM-DD)"),
    auth: AuthContext = Depends(get_auth_context),
) -> UserUsageResponse:
    """
    Get usage details for a specific user.

    Includes summary, top models, and daily trend.
    """
    _require_usage_read(request, auth)
    user_id = _scope_usage_user_id(request, auth, user_id)
    recorder = get_usage_recorder()

    # Default to last 7 days
    if not end_date:
        end_date = date.today()
    if not start_date:
        start_date = end_date - timedelta(days=7)

    # Get summary
    summary = await recorder.get_usage_summary(
        tenant_id=auth.tenant_id,
        start_date=start_date,
        end_date=end_date,
        user_id=user_id,
    )
    last_ingested_at = await recorder.get_last_ingested_at(
        tenant_id=auth.tenant_id,
        start_date=start_date,
        end_date=end_date,
        granularity="day",
    )
    data_status, freshness_minutes = compute_data_status(
        last_ingested_at,
        total_requests=summary.get("total_requests"),
    )

    # Get top models for this user
    top_models = await recorder.get_usage_breakdown(
        tenant_id=auth.tenant_id,
        dimension="model",
        start_date=start_date,
        end_date=end_date,
        limit=5,
    )

    # Get daily trend
    daily_trend = await recorder.get_usage_timeseries(
        tenant_id=auth.tenant_id,
        start_date=start_date,
        end_date=end_date,
        user_id=user_id,
    )

    return UserUsageResponse(
        user_id=user_id,
        summary=UsageSummaryResponse(
            **summary,
            data_status=data_status,
            data_freshness_minutes=freshness_minutes,
            last_ingested_at=last_ingested_at.isoformat() if last_ingested_at else None,
        ),
        top_models=top_models,
        daily_trend=[UsageTimeSeriesPoint(**d) for d in daily_trend],
    )


@router.get("/export")
async def export_usage(
    request: Request,
    format: str = Query("csv", description="Export format: csv or json"),
    start_date: date | None = Query(None, description="Start date (YYYY-MM-DD)"),
    end_date: date | None = Query(None, description="End date (YYYY-MM-DD)"),
    dimension: str = Query("model", description="Breakdown dimension for export"),
    auth: AuthContext = Depends(get_auth_context),
):
    """
    Export usage data.

    Supports CSV and JSON formats for download.
    """
    _require_usage_read(request, auth)
    _require_usage_tenant_scope(request, auth)
    recorder = get_usage_recorder()

    # Default to last 30 days
    if not end_date:
        end_date = date.today()
    if not start_date:
        start_date = end_date - timedelta(days=30)

    # Get time series data
    timeseries = await recorder.get_usage_timeseries(
        tenant_id=auth.tenant_id,
        start_date=start_date,
        end_date=end_date,
    )

    # Get breakdown data
    breakdown = await recorder.get_usage_breakdown(
        tenant_id=auth.tenant_id,
        dimension=dimension,
        start_date=start_date,
        end_date=end_date,
        limit=100,
    )

    if format.lower() == "json":
        return {
            "period": {
                "start": start_date.isoformat(),
                "end": end_date.isoformat(),
            },
            "timeseries": timeseries,
            "breakdown": {
                "dimension": dimension,
                "items": breakdown,
            },
            "exported_at": datetime.now().isoformat(),
        }

    # CSV export
    output = StringIO()
    writer = csv.writer(output)

    # Write time series section
    writer.writerow(["=== Daily Usage ==="])
    writer.writerow(
        [
            "Date",
            "Requests",
            "Input Tokens",
            "Output Tokens",
            "Total Tokens",
            "Cost (USD)",
            "Avg Latency (ms)",
        ]
    )
    for row in timeseries:
        writer.writerow(
            [
                row["date"],
                row["requests"],
                row["input_tokens"],
                row["output_tokens"],
                row["total_tokens"],
                row["cost_usd"],
                row["avg_latency_ms"],
            ]
        )

    writer.writerow([])
    writer.writerow([f"=== Breakdown by {dimension.title()} ==="])
    writer.writerow(
        [
            dimension.title(),
            "Requests",
            "Input Tokens",
            "Output Tokens",
            "Total Tokens",
            "Cost (USD)",
            "Percentage",
        ]
    )
    for row in breakdown:
        writer.writerow(
            [
                row.get(dimension, "Unknown"),
                row.get("requests", 0),
                row.get("input_tokens", 0),
                row.get("output_tokens", 0),
                row.get("total_tokens", 0),
                row.get("cost_usd", 0),
                row.get("percentage", 0),
            ]
        )

    output.seek(0)
    filename = f"usage_export_{start_date}_{end_date}.csv"

    return StreamingResponse(
        iter([output.getvalue()]),
        media_type="text/csv",
        headers={"Content-Disposition": f"attachment; filename={filename}"},
    )


@router.get("/models")
async def get_model_usage(
    request: Request,
    start_date: date | None = Query(None, description="Start date (YYYY-MM-DD)"),
    end_date: date | None = Query(None, description="End date (YYYY-MM-DD)"),
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    """
    Get usage statistics by model.

    Provides detailed breakdown of usage per model including token counts and costs.
    """
    _require_usage_read(request, auth)
    _require_usage_tenant_scope(request, auth)
    recorder = get_usage_recorder()

    # Default to last 7 days
    if not end_date:
        end_date = date.today()
    if not start_date:
        start_date = end_date - timedelta(days=7)

    breakdown = await recorder.get_usage_breakdown(
        tenant_id=auth.tenant_id,
        dimension="model",
        start_date=start_date,
        end_date=end_date,
        limit=50,
    )

    total_cost = sum(item.get("cost_usd", 0) for item in breakdown)
    total_tokens = sum(item.get("total_tokens", 0) for item in breakdown)
    total_requests = sum(item.get("requests", 0) for item in breakdown)

    return {
        "period": {
            "start": start_date.isoformat(),
            "end": end_date.isoformat(),
        },
        "totals": {
            "requests": total_requests,
            "tokens": total_tokens,
            "cost_usd": round(total_cost, 4),
        },
        "models": breakdown,
    }
