"""SPO-00 gateway hot-path counter tests.

Each test drives the shipped limiter code with a fake Redis and asserts the
exercisable ``gateway_hot_path_metrics`` counters — the same counters the
SPO-02 gate (warm chat/proxy path ≤ 4 Redis round-trips) relies on.
"""

from __future__ import annotations

from typing import Any

import pytest

from src.core.gateway.multi_dimension_rate_limiter import (
    MultiDimensionRateLimitConfig,
    MultiDimensionRateLimiter,
    RateLimitContext,
)
from src.core.hot_path_metrics import gateway_hot_path_metrics
from src.core.middleware.rate_limit_http import SlidingWindowRateLimiter


class _FakeRedis:
    """Fake redis simulating the SLIDING_WINDOW_CHECK_LUA contract.

    ``pre_add_counts`` maps key → zcard value observed by the script; the
    script then records the member and returns {-1, 0} when every key is
    under its limit, or {rejected_index, earliest_score} otherwise.
    """

    def __init__(self, pre_add_counts: dict[str, int]) -> None:
        self.pre_add_counts = pre_add_counts
        self.round_trips = 0

    async def eval(self, _script: str, numkeys: int, *keys_and_args: Any) -> list[Any]:
        self.round_trips += 1
        keys = [str(key) for key in keys_and_args[:numkeys]]
        args = [str(arg) for arg in keys_and_args[numkeys:]]
        window_start = float(args[1])
        for index, key in enumerate(keys):
            limit = int(args[4 + index])
            if self.pre_add_counts.get(key, 0) >= limit:
                return [index, window_start + 30.0]
        return [-1, 0]


@pytest.fixture(autouse=True)
def _reset_metrics() -> None:
    gateway_hot_path_metrics.reset()
    yield
    gateway_hot_path_metrics.reset()


@pytest.mark.asyncio
async def test_sliding_window_limiter_allowed_check_is_one_round_trip() -> None:
    fake = _FakeRedis(pre_add_counts={"ratelimit:global": 0})
    limiter = SlidingWindowRateLimiter(redis_client=fake)

    result = await limiter.check("ratelimit:global", limit=10, window=60)

    assert result.allowed
    assert fake.round_trips == 1
    assert gateway_hot_path_metrics.redis_round_trips == 1


@pytest.mark.asyncio
async def test_sliding_window_limiter_denied_check_is_one_atomic_round_trip() -> None:
    fake = _FakeRedis(pre_add_counts={"ratelimit:user:u1": 10})
    limiter = SlidingWindowRateLimiter(redis_client=fake)

    result = await limiter.check("ratelimit:user:u1", limit=10, window=60)

    assert not result.allowed
    # The retry_after data comes back inside the atomic EVAL result — no
    # follow-up ZRANGE round trip (SPO-02).
    assert fake.round_trips == 1
    assert result.retry_after == 31
    assert gateway_hot_path_metrics.redis_round_trips == 1


@pytest.mark.asyncio
async def test_multi_dimension_limiter_counts_one_round_trip_per_dimension() -> None:
    fake = _FakeRedis(pre_add_counts={})
    config = MultiDimensionRateLimitConfig(
        ip_enabled=False,
        tenant_enabled=False,
        assistant_enabled=False,
        global_limit=1000,
    )
    limiter = MultiDimensionRateLimiter(config, redis_client=fake)
    context = RateLimitContext(
        ip="127.0.0.1",
        user_id="user-a",
        user_tier="normal",
    )

    result = await limiter.check(context)

    assert result.allowed
    # global + user dimensions each run one atomic EVAL.
    assert fake.round_trips == 2
    assert gateway_hot_path_metrics.redis_round_trips == 2


@pytest.mark.asyncio
async def test_multi_dimension_limiter_denied_check_is_one_atomic_round_trip() -> None:
    fake = _FakeRedis(pre_add_counts={"ratelimit:global": 1001})
    config = MultiDimensionRateLimitConfig(
        ip_enabled=False,
        tenant_enabled=False,
        assistant_enabled=False,
        global_limit=1000,
    )
    limiter = MultiDimensionRateLimiter(config, redis_client=fake)
    context = RateLimitContext(ip="127.0.0.1")

    result = await limiter.check(context)

    assert not result.allowed
    assert fake.round_trips == 1
    assert gateway_hot_path_metrics.redis_round_trips == 1
