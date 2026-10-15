from __future__ import annotations

import time

import pytest
from ai_gateway_core.comm.client import TokenBucketRateLimiter


@pytest.mark.asyncio
async def test_token_bucket_allows_burst_without_waiting() -> None:
    limiter = TokenBucketRateLimiter(rate=1.0, burst=2)

    started = time.perf_counter()
    await limiter.acquire()
    await limiter.acquire()
    elapsed = time.perf_counter() - started

    assert elapsed < 0.05


@pytest.mark.asyncio
async def test_token_bucket_waits_for_refill_after_burst_is_exhausted() -> None:
    limiter = TokenBucketRateLimiter(rate=20.0, burst=1)

    await limiter.acquire()
    started = time.perf_counter()
    await limiter.acquire()
    elapsed = time.perf_counter() - started

    assert elapsed >= 0.035
