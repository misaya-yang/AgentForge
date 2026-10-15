"""KB Tools API — streaming proxy to KB Service microservice.

These endpoints were previously served by the Gateway's internal KB.
Now forwarded to the KB Service with streaming and auto-reconnect.
"""

from __future__ import annotations

from fastapi import APIRouter, Depends, HTTPException, Request
from starlette.responses import Response

from ...core.auth.user_resolver import UserContext
from ...core.gateway.multi_dimension_rate_limiter import MultiDimensionRateLimiter, RateLimitContext
from ..deps import get_rate_limiter, get_user_context
from ._proxy_utils import enforce_knowledge_scope, proxy_to_kb_service

router = APIRouter(prefix="/kb-tools", tags=["KB Tools"])


@router.api_route(
    "/{path:path}",
    methods=["GET", "POST", "PUT", "PATCH", "DELETE"],
    summary="Proxy KB Tools to KB Service",
)
async def proxy_kb_tools(
    path: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
    rate_limiter: MultiDimensionRateLimiter | None = Depends(get_rate_limiter),
) -> Response:
    """Forward /kb-tools/* requests with the standard knowledge rate limit."""
    enforce_knowledge_scope(request, path=path)
    if rate_limiter is not None:
        ctx = RateLimitContext.from_user_context(user)
        result = await rate_limiter.check(ctx)
        if not result.allowed:
            raise HTTPException(429, "Rate limit exceeded", headers={
                "X-RateLimit-Limit": str(result.limit),
                "X-RateLimit-Remaining": str(result.remaining),
                "Retry-After": str(result.retry_after),
            })
    return await proxy_to_kb_service(request, user, path=path)
