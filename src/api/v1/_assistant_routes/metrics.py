"""Assistant context-metrics routes (observability).

ARC-01 split of ``src/api/v1/assistant.py``.  Read-only projections over the
Gateway-owned context metrics collector.
"""

from __future__ import annotations

import logging

from fastapi import APIRouter, Depends, HTTPException, Query, Request

from ....core.auth.user_resolver import UserContext
from ....services.assistant_entry.session_binding import get_session_manager
from ...deps import get_user_context
from .schemas import ContextMetricsResponse, TenantMetricsResponse

router = APIRouter()
logger = logging.getLogger(__name__)


@router.get(
    "/sessions/{session_id}/metrics",
    response_model=ContextMetricsResponse,
    summary="Get context metrics for a session",
    description="Returns aggregated context metrics for a specific session including token usage, compression, and cache performance.",
)
async def get_session_metrics(
    session_id: str,
    user: UserContext = Depends(get_user_context),
    request: Request = None,
    limit: int = Query(default=50, ge=1, le=200),
):
    """Get context metrics for a session."""
    from ai_gateway_core.metrics import get_context_metrics_collector

    # Verify session ownership (security: prevent access to other users' metrics)
    session_manager = get_session_manager(request)
    session = await session_manager.get(session_id)
    if not session:
        raise HTTPException(status_code=404, detail="Session not found")
    if session.user_id != user.user_id or session.tenant_id != user.tenant_id:
        raise HTTPException(status_code=404, detail="Session not found")

    collector = get_context_metrics_collector()
    stats = await collector.get_session_stats(session_id, limit=limit)

    return ContextMetricsResponse(
        session_id=session_id,
        request_count=stats.get("request_count", 0),
        avg_tokens=stats.get("avg_tokens", 0),
        avg_utilization=round(stats.get("avg_utilization", 0), 3),
        avg_compression_ratio=round(stats.get("avg_compression_ratio", 1.0), 2),
        avg_cache_hit_rate=round(stats.get("avg_cache_hit_rate", 0), 3),
        total_tokens_used=stats.get("total_tokens_used"),
    )


@router.get(
    "/metrics/tenant",
    response_model=TenantMetricsResponse,
    summary="Get aggregated metrics for tenant",
    description="Returns aggregated context metrics for the current tenant over a specified time window.",
)
async def get_tenant_metrics(
    user: UserContext = Depends(get_user_context),
    hours: int = Query(default=24, ge=1, le=168),
):
    """Get aggregated metrics for the current tenant."""
    from ai_gateway_core.metrics import get_context_metrics_collector

    collector = get_context_metrics_collector()
    stats = await collector.get_tenant_stats(user.tenant_id, hours=hours)

    return TenantMetricsResponse(
        tenant_id=user.tenant_id,
        hours=hours,
        request_count=stats.get("request_count", 0),
        unique_sessions=stats.get("unique_sessions", 0),
        total_tokens=stats.get("total_tokens", 0),
        avg_tokens_per_request=stats.get("avg_tokens_per_request"),
        avg_utilization=round(stats.get("avg_utilization", 0), 3)
        if stats.get("avg_utilization")
        else None,
    )
