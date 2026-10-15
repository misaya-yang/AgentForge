"""Shared LangGraph / conversations route helpers."""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from typing import Any

from fastapi import HTTPException

from ...adapters.langgraph_proxy import (
    AssistantAccessDeniedError,
    AssistantNotFoundError,
    ForbiddenError,
    NoHealthyInstanceError,
    QuotaExceededError,
    ThreadNotFoundError,
)
from ...core.auth.user_resolver import UserContext
from ...core.gateway.multi_dimension_rate_limiter import (
    MultiDimensionRateLimiter,
    RateLimitContext,
    RateLimitHeaders,
)


def handle_langgraph_proxy_error(
    exc: Exception,
    *,
    not_found_detail: str = "resource not found",
) -> None:
    """Map LangGraph proxy exceptions to HTTP errors."""
    if isinstance(exc, HTTPException):
        raise exc
    if isinstance(exc, (ForbiddenError, AssistantAccessDeniedError)):
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    if isinstance(exc, QuotaExceededError):
        raise HTTPException(status_code=429, detail=str(exc)) from exc
    if isinstance(exc, ThreadNotFoundError):
        raise HTTPException(status_code=404, detail=not_found_detail) from exc
    if isinstance(exc, AssistantNotFoundError):
        raise HTTPException(status_code=404, detail=str(exc)) from exc
    if isinstance(exc, NoHealthyInstanceError):
        raise HTTPException(status_code=503, detail=str(exc)) from exc
    raise HTTPException(status_code=500, detail=str(exc)) from exc


async def check_langgraph_rate_limit(
    user: UserContext,
    rate_limiter: MultiDimensionRateLimiter | None,
    *,
    assistant_id: str | None = None,
    operation: str | None = None,
    service_id: str | None = None,
    on_denied: Callable[[], Awaitable[None]] | None = None,
) -> dict[str, str]:
    """Apply multi-dimension rate limiting for LangGraph and conversation routes."""
    if not rate_limiter:
        return {}

    context = RateLimitContext.from_user_context(
        user=user,
        assistant_id=assistant_id,
        operation=operation,
    )
    result = await rate_limiter.check(context)
    if not result.allowed:
        if on_denied is not None:
            await on_denied()
        raise HTTPException(
            status_code=429,
            detail=RateLimitHeaders.build_exceeded_response(result),
            headers=RateLimitHeaders.build(result),
        )
    return RateLimitHeaders.build(result)