from __future__ import annotations

from types import SimpleNamespace

import pytest
from fastapi import HTTPException

from src.api.v1.proxy import check_proxy_rate_limit
from src.core.auth.user_resolver import UserContext
from src.core.gateway.multi_dimension_rate_limiter import RateLimitResult
from src.core.gateway.rate_policy import RatePolicy
from src.proxy.config_loader import ProxyServiceConfig


class _FakeRateLimiter:
    def __init__(self, *, custom_result: RateLimitResult, global_result: RateLimitResult):
        self.custom_result = custom_result
        self.global_result = global_result
        self.custom_calls = []
        self.global_calls = []

    async def check_custom_limit(self, **kwargs):
        self.custom_calls.append(kwargs)
        return self.custom_result

    async def check(self, context):
        self.global_calls.append(context)
        return self.global_result


def _make_user() -> UserContext:
    return UserContext(
        user_id="user_1",
        tenant_id="tenant_1",
        tier="normal",
        is_authenticated=True,
        roles=["user"],
        ip="127.0.0.1",
    )


def _make_service_config(enabled: bool) -> ProxyServiceConfig:
    return ProxyServiceConfig(
        service_id="agent",
        service_name="agent",
        upstream_url="http://localhost:2024",
        rate_limit_enabled=enabled,
        rate_limit_requests=5,
        rate_limit_window=60,
    )


@pytest.mark.asyncio
async def test_service_rate_limit_override_takes_precedence() -> None:
    limiter = _FakeRateLimiter(
        custom_result=RateLimitResult(allowed=True, dimension="service:agent"),
        global_result=RateLimitResult(
            allowed=True,
            dimension="user",
            limit=60,
            remaining=59,
            reset_at=9999999999,
        ),
    )
    headers = await check_proxy_rate_limit(
        user=_make_user(),
        rate_limiter=limiter,
        service_name="agent",
        operation="run_wait",
        service_config=_make_service_config(True),
    )
    assert len(limiter.custom_calls) == 1
    assert len(limiter.global_calls) == 0
    assert "tenant_1" in limiter.custom_calls[0]["key"]
    assert headers["X-RateLimit-Dimension"] == "service:agent"


@pytest.mark.asyncio
async def test_global_rate_limit_used_when_service_override_disabled() -> None:
    limiter = _FakeRateLimiter(
        custom_result=RateLimitResult(allowed=True, dimension="service:agent"),
        global_result=RateLimitResult(
            allowed=True,
            dimension="user",
            limit=60,
            remaining=59,
            reset_at=9999999999,
        ),
    )
    headers = await check_proxy_rate_limit(
        user=_make_user(),
        rate_limiter=limiter,
        service_name="agent",
        operation="run_wait",
        service_config=_make_service_config(False),
    )
    assert len(limiter.custom_calls) == 0
    assert len(limiter.global_calls) == 1
    assert headers["X-RateLimit-Dimension"] == "user"


@pytest.mark.asyncio
async def test_no_limiter_marks_rate_limit_exempt() -> None:
    headers = await check_proxy_rate_limit(
        user=_make_user(),
        rate_limiter=None,
        service_name="agent",
        operation="run_wait",
        service_config=_make_service_config(True),
    )
    assert headers == {"X-Gateway-Policy-Exempt": "rate_limit"}


@pytest.mark.asyncio
async def test_service_rate_limit_rejects_with_429() -> None:
    limiter = _FakeRateLimiter(
        custom_result=RateLimitResult(
            allowed=False,
            dimension="service:agent",
            limit=5,
            remaining=0,
            reset_at=9999999999,
            retry_after=30,
        ),
        global_result=RateLimitResult(allowed=True),
    )
    with pytest.raises(HTTPException) as exc:
        await check_proxy_rate_limit(
            user=_make_user(),
            rate_limiter=limiter,
            service_name="agent",
            operation="run_wait",
            service_config=_make_service_config(True),
        )
    assert exc.value.status_code == 429
    assert exc.value.headers["Retry-After"] == "30"
    assert exc.value.headers["X-RateLimit-Limit"] == "5"
    assert exc.value.detail["error"]["code"] == "RATE_LIMIT_EXCEEDED"


@pytest.mark.asyncio
async def test_dynamic_policy_batches_preserve_noncontiguous_window_order() -> None:
    policies = [
        RatePolicy(key="a", dimension="global", requests=100, window=60),
        RatePolicy(key="b", dimension="tenant", requests=50, window=10),
        RatePolicy(key="c", dimension="user", requests=25, window=60),
    ]

    class Resolver:
        async def resolve(self, **_kwargs):
            return policies

    class Limiter:
        def __init__(self) -> None:
            self.batch_calls: list[list[RatePolicy]] = []

        async def check_custom_limits(self, *, policies):
            self.batch_calls.append(list(policies))
            return [
                RateLimitResult(
                    allowed=True,
                    dimension=policy.dimension,
                    limit=policy.requests,
                    remaining=policy.requests - 1,
                )
                for policy in policies
            ]

    limiter = Limiter()
    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(rate_policy_resolver=Resolver()))
    )

    headers = await check_proxy_rate_limit(
        user=_make_user(),
        rate_limiter=limiter,
        service_name="agent",
        operation="run_wait",
        service_config=_make_service_config(False),
        request=request,
    )

    assert len(limiter.batch_calls) == 3
    assert [[policy.key for policy in batch] for batch in limiter.batch_calls] == [
        ["a"],
        ["b"],
        ["c"],
    ]
    assert headers["X-RateLimit-Dimension"] == "user"


@pytest.mark.asyncio
async def test_dynamic_policy_rejection_stops_at_original_priority_position() -> None:
    policies = [
        RatePolicy(key="a", dimension="global", requests=100, window=60),
        RatePolicy(key="b", dimension="tenant", requests=1, window=10),
        RatePolicy(key="c", dimension="user", requests=25, window=60),
    ]

    class Resolver:
        async def resolve(self, **_kwargs):
            return policies

    class Limiter:
        def __init__(self) -> None:
            self.checked: list[str] = []

        async def check_custom_limits(self, *, policies):
            policy = policies[0]
            self.checked.append(policy.key)
            return [
                RateLimitResult(
                    allowed=policy.key != "b",
                    dimension=policy.dimension,
                    limit=policy.requests,
                    remaining=0,
                    retry_after=10 if policy.key == "b" else 0,
                )
            ]

    limiter = Limiter()
    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(rate_policy_resolver=Resolver()))
    )

    with pytest.raises(HTTPException) as exc:
        await check_proxy_rate_limit(
            user=_make_user(),
            rate_limiter=limiter,
            service_name="agent",
            operation="run_wait",
            service_config=_make_service_config(False),
            request=request,
        )

    assert exc.value.status_code == 429
    assert limiter.checked == ["a", "b"]
