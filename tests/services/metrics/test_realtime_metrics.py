"""Tests for RealtimeMetricsService.

Phase 0 hotfix: verify record_token_usage is a Redis no-op to prevent
double-incrementing daily token counters (previous bug inflated numbers 2x
because both MetricsRecorder.record_tokens AND this method wrote the same
``metrics:tokens:input|output:{today}`` keys).
"""

from unittest.mock import AsyncMock, MagicMock

import pytest

from src.services.metrics.realtime_metrics import RealtimeMetricsService


class TestRecordTokenUsageNoOp:
    """record_token_usage must NOT write to Redis — single-writer invariant."""

    @pytest.fixture
    def redis_mock(self):
        """RedisStorage-like mock exposing ._client with a pipeline()."""
        pipeline = MagicMock()
        pipeline.incrby = MagicMock()
        pipeline.expire = MagicMock()
        pipeline.execute = AsyncMock(return_value=[])

        client = MagicMock()
        client.pipeline = MagicMock(return_value=pipeline)

        storage = MagicMock()
        storage._client = client
        # expose pipeline via attribute for test assertions
        storage._pipeline = pipeline
        return storage

    @pytest.mark.asyncio
    async def test_record_token_usage_does_not_touch_redis(self, redis_mock):
        service = RealtimeMetricsService(redis=redis_mock)

        await service.record_token_usage(input_tokens=1000, output_tokens=500)

        # Core invariant: no pipeline is ever created, no incrby/expire/execute
        assert redis_mock._client.pipeline.call_count == 0
        assert redis_mock._pipeline.incrby.call_count == 0
        assert redis_mock._pipeline.expire.call_count == 0
        assert redis_mock._pipeline.execute.await_count == 0

    @pytest.mark.asyncio
    async def test_record_token_usage_returns_none(self, redis_mock):
        service = RealtimeMetricsService(redis=redis_mock)
        result = await service.record_token_usage(123, 456)
        assert result is None

    @pytest.mark.asyncio
    async def test_record_token_usage_safe_without_redis(self):
        """Still safe to call when redis is not configured."""
        service = RealtimeMetricsService(redis=None)
        # Must not raise
        await service.record_token_usage(10, 20)
