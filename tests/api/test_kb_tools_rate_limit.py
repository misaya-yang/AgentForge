from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException
from starlette.responses import Response

from src.api.v1 import kb_tools
from src.core.auth.user_resolver import UserContext
from src.core.gateway.multi_dimension_rate_limiter import RateLimitResult


class _RateLimiter:
    def __init__(self, result: RateLimitResult) -> None:
        self.result = result
        self.contexts = []

    async def check(self, context):
        self.contexts.append(context)
        return self.result


def _user() -> UserContext:
    return UserContext(
        user_id="user-1",
        tenant_id="tenant-1",
        tier="normal",
        is_authenticated=True,
        ip="203.0.113.8",
    )


@pytest.mark.asyncio
async def test_proxy_kb_tools_forwards_when_rate_limit_allows(monkeypatch) -> None:
    request = SimpleNamespace()
    user = _user()
    limiter = _RateLimiter(RateLimitResult(allowed=True, limit=60, remaining=59))
    upstream_response = Response(status_code=204)
    upstream = AsyncMock(return_value=upstream_response)
    monkeypatch.setattr(kb_tools, "proxy_to_kb_service", upstream)

    response = await kb_tools.proxy_kb_tools(
        path="datasets",
        request=request,
        user=user,
        rate_limiter=limiter,
    )

    assert response is upstream_response
    assert len(limiter.contexts) == 1
    context = limiter.contexts[0]
    assert (context.user_id, context.tenant_id, context.user_tier, context.ip) == (
        user.user_id,
        user.tenant_id,
        user.tier,
        user.ip,
    )
    upstream.assert_awaited_once_with(request, user, path="datasets")


@pytest.mark.asyncio
async def test_proxy_kb_tools_rejects_before_proxying(monkeypatch) -> None:
    request = SimpleNamespace()
    user = _user()
    limiter = _RateLimiter(
        RateLimitResult(allowed=False, limit=60, remaining=0, retry_after=17)
    )
    upstream = AsyncMock()
    monkeypatch.setattr(kb_tools, "proxy_to_kb_service", upstream)

    with pytest.raises(HTTPException) as exc_info:
        await kb_tools.proxy_kb_tools(
            path="datasets",
            request=request,
            user=user,
            rate_limiter=limiter,
        )

    assert exc_info.value.status_code == 429
    assert exc_info.value.headers == {
        "X-RateLimit-Limit": "60",
        "X-RateLimit-Remaining": "0",
        "Retry-After": "17",
    }
    upstream.assert_not_awaited()


@pytest.mark.asyncio
async def test_proxy_kb_tools_forwards_when_limiter_is_unconfigured(monkeypatch) -> None:
    request = SimpleNamespace()
    user = _user()
    upstream_response = Response(status_code=204)
    upstream = AsyncMock(return_value=upstream_response)
    monkeypatch.setattr(kb_tools, "proxy_to_kb_service", upstream)

    response = await kb_tools.proxy_kb_tools(
        path="datasets",
        request=request,
        user=user,
        rate_limiter=None,
    )

    assert response is upstream_response
    upstream.assert_awaited_once_with(request, user, path="datasets")
