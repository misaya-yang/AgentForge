"""Tests for gateway circuit breaker."""

import asyncio
import time
from unittest.mock import AsyncMock, patch

import pytest

from ai_gateway_core.exceptions import CircuitBreakerOpenError
from src.core.gateway.circuit_breaker import CircuitBreaker


@pytest.fixture
def breaker():
    return CircuitBreaker(
        service_id="test-svc",
        failure_threshold=3,
        success_threshold=2,
        timeout=1,  # 1 second for fast tests
    )


class TestCircuitBreakerStates:
    async def test_starts_closed(self, breaker):
        assert breaker.state == "CLOSED"

    async def test_opens_after_threshold_failures(self, breaker):
        for _ in range(3):
            await breaker._on_failure(RuntimeError("fail"))
        assert breaker.state == "OPEN"

    async def test_stays_closed_below_threshold(self, breaker):
        for _ in range(2):
            await breaker._on_failure(RuntimeError("fail"))
        assert breaker.state == "CLOSED"

    async def test_open_rejects_calls(self, breaker):
        for _ in range(3):
            await breaker._on_failure(RuntimeError("fail"))
        with pytest.raises(CircuitBreakerOpenError):
            await breaker.allow()

    async def test_half_open_after_timeout(self, breaker):
        for _ in range(3):
            await breaker._on_failure(RuntimeError("fail"))
        assert breaker.state == "OPEN"
        # Simulate timeout elapsing
        breaker.last_failure_time = time.time() - 2
        await breaker.allow()
        assert breaker.state == "HALF_OPEN"

    async def test_closes_after_success_threshold_in_half_open(self, breaker):
        # Open the breaker
        for _ in range(3):
            await breaker._on_failure(RuntimeError("fail"))
        breaker.last_failure_time = time.time() - 2
        await breaker.allow()
        assert breaker.state == "HALF_OPEN"

        # Successes in HALF_OPEN
        await breaker._on_success()
        assert breaker.state == "HALF_OPEN"
        await breaker._on_success()
        assert breaker.state == "CLOSED"
        assert breaker.failure_count == 0

    async def test_success_resets_failure_count_in_closed(self, breaker):
        await breaker._on_failure(RuntimeError("fail"))
        assert breaker.failure_count == 1
        await breaker._on_success()
        assert breaker.failure_count == 0


class TestCircuitBreakerExcludedExceptions:
    async def test_excluded_exception_not_counted(self):
        breaker = CircuitBreaker(
            service_id="test-svc",
            failure_threshold=2,
            excluded_exceptions=[ValueError],
        )
        await breaker._on_failure(ValueError("ignored"))
        assert breaker.failure_count == 0
        assert breaker.state == "CLOSED"

    async def test_non_excluded_exception_counted(self):
        breaker = CircuitBreaker(
            service_id="test-svc",
            failure_threshold=2,
            excluded_exceptions=[ValueError],
        )
        await breaker._on_failure(RuntimeError("counted"))
        assert breaker.failure_count == 1


class TestCircuitBreakerCall:
    async def test_successful_call(self, breaker):
        func = AsyncMock(return_value="result")
        result = await breaker.call(func, "arg1", key="val")
        assert result == "result"
        func.assert_awaited_once_with("arg1", key="val")

    async def test_failed_call_propagates_exception(self, breaker):
        func = AsyncMock(side_effect=RuntimeError("boom"))
        with pytest.raises(RuntimeError, match="boom"):
            await breaker.call(func)
        assert breaker.failure_count == 1

    async def test_call_rejected_when_open(self, breaker):
        for _ in range(3):
            await breaker._on_failure(RuntimeError("fail"))
        func = AsyncMock()
        with pytest.raises(CircuitBreakerOpenError):
            await breaker.call(func)
        func.assert_not_awaited()
