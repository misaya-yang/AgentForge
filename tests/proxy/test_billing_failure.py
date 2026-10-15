"""Phase 0 hotfix — billing flush failure handling.

Covers:
- Per-stage classification via ``gateway_billing_flush_failures_total``.
- Retry with exponential backoff.
- Redis DLQ (``metrics:billing:dead_letter``) as terminal sink.
- ``gateway_billing_records_dropped_total`` increments on exhausted retries.
"""

from __future__ import annotations

import json
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from src.core.observability.metrics import MetricsCollector
from src.proxy.billing_interceptor import BillingInterceptor, UsageData


@pytest.fixture(autouse=True)
def reset_singleton_metrics():
    """Ensure counter state is isolated between tests."""
    metrics = MetricsCollector()
    for counter in (
        metrics.request_metrics.billing_flush_failures_total,
        metrics.request_metrics.billing_records_dropped_total,
    ):
        counter._values.clear()
    yield


@pytest.fixture
def fast_backoffs(monkeypatch):
    """Shrink retry backoffs so tests run quickly."""
    from src.proxy import billing_interceptor as bi

    monkeypatch.setattr(bi, "_FLUSH_RETRY_BACKOFFS", (0.0, 0.0, 0.0))


def _make_usage(**overrides) -> UsageData:
    base = {
        "input_tokens": 10,
        "output_tokens": 5,
        "request_id": "req-1",
        "service_id": "svc-1",
        "user_id": "u1",
        "tenant_id": "t1",
        "model": "gpt-4",
    }
    base.update(overrides)
    return UsageData(**base)


@pytest.mark.usefixtures("fast_backoffs")
class TestDatabaseRetryAndDLQ:
    @pytest.mark.asyncio
    async def test_db_retry_then_dlq(self):
        """DB write always fails → counter counted (initial + 3 retries),
        DLQ receives record, dropped-counter increments."""
        redis = AsyncMock()
        redis.lpush = AsyncMock()
        redis.ltrim = AsyncMock()

        interceptor = BillingInterceptor(redis_client=redis)

        # Force DB recorder to exist but always fail
        fake_recorder = MagicMock()
        fake_recorder.record_usage = AsyncMock(side_effect=ConnectionError("db down"))

        with patch(
            "src.services.metrics.get_usage_recorder", return_value=fake_recorder
        ):
            await interceptor._push_usage(_make_usage())

        metrics = MetricsCollector().request_metrics

        # 1 initial + 3 retries = 4 failure samples recorded for stage=database
        assert (
            metrics.billing_flush_failures_total.get(
                stage="database", error_type="ConnectionError"
            )
            == 4
        )

        # DLQ received exactly 1 LPUSH
        assert redis.lpush.await_count == 1
        dlq_key, payload = redis.lpush.call_args.args
        assert dlq_key == "metrics:billing:dead_letter"
        decoded = json.loads(payload)
        assert decoded["stage"] == "database"
        assert decoded["usage"]["request_id"] == "req-1"

        # Cap enforced
        redis.ltrim.assert_awaited_once_with("metrics:billing:dead_letter", 0, 9999)

        # Dropped counter increments once
        assert (
            metrics.billing_records_dropped_total.get(reason="max_retries_exceeded") == 1
        )

    @pytest.mark.asyncio
    async def test_db_retry_succeeds_on_second_attempt(self):
        redis = AsyncMock()
        redis.lpush = AsyncMock()
        redis.ltrim = AsyncMock()

        interceptor = BillingInterceptor(redis_client=redis)

        # Recorder fails once, then succeeds
        fake_recorder = MagicMock()
        fake_recorder.record_usage = AsyncMock(
            side_effect=[ConnectionError("transient"), None]
        )

        with patch(
            "src.services.metrics.get_usage_recorder", return_value=fake_recorder
        ):
            await interceptor._push_usage(_make_usage())

        metrics = MetricsCollector().request_metrics

        # 1 initial failure recorded, then success — no retry failures
        assert (
            metrics.billing_flush_failures_total.get(
                stage="database", error_type="ConnectionError"
            )
            == 1
        )
        # No DLQ write, no dropped count
        assert redis.lpush.await_count == 0
        assert (
            metrics.billing_records_dropped_total.get(reason="max_retries_exceeded") == 0
        )
        # Durable success → stats updated
        assert interceptor._total_events == 1


@pytest.mark.usefixtures("fast_backoffs")
class TestRedisStageClassification:
    @pytest.mark.asyncio
    async def test_redis_storage_wrapper_uses_native_client(self):
        native = AsyncMock()
        native.publish = AsyncMock(return_value=None)
        native.lpush = AsyncMock()
        native.ltrim = AsyncMock()

        storage = MagicMock()
        storage.get_native_client = MagicMock(return_value=native)

        interceptor = BillingInterceptor(redis_client=storage)

        fake_recorder = MagicMock()
        fake_recorder.record_usage = AsyncMock(return_value=None)

        with patch(
            "src.services.metrics.get_usage_recorder", return_value=fake_recorder
        ):
            await interceptor._push_usage(_make_usage())

        native.publish.assert_awaited_once()
        assert native.lpush.await_count == 0

    @pytest.mark.asyncio
    async def test_redis_publish_failure_classified_and_dlq(self):
        redis = AsyncMock()
        redis.publish = AsyncMock(side_effect=RuntimeError("publish boom"))
        redis.lpush = AsyncMock()
        redis.ltrim = AsyncMock()

        interceptor = BillingInterceptor(redis_client=redis)

        # DB succeeds to isolate redis stage failure
        fake_recorder = MagicMock()
        fake_recorder.record_usage = AsyncMock(return_value=None)

        with patch(
            "src.services.metrics.get_usage_recorder", return_value=fake_recorder
        ):
            await interceptor._push_usage(_make_usage())

        metrics = MetricsCollector().request_metrics

        # Redis stage: 1 initial + 3 retries = 4 classified failures
        assert (
            metrics.billing_flush_failures_total.get(
                stage="redis", error_type="RuntimeError"
            )
            == 4
        )
        # Database stage: 0 failures
        assert (
            metrics.billing_flush_failures_total.get(
                stage="database", error_type=""
            )
            == 0
        )
        # DLQ was written for the redis failure
        assert redis.lpush.await_count == 1


@pytest.mark.usefixtures("fast_backoffs")
class TestCallbackStageClassification:
    @pytest.mark.asyncio
    async def test_callback_failure_classified(self):
        redis = AsyncMock()
        redis.publish = AsyncMock()
        redis.lpush = AsyncMock()
        redis.ltrim = AsyncMock()

        async def failing_cb(_u):
            raise ValueError("cb boom")

        interceptor = BillingInterceptor(
            callback=failing_cb,
            redis_client=redis,
        )

        # DB succeeds to keep focus on callback
        fake_recorder = MagicMock()
        fake_recorder.record_usage = AsyncMock(return_value=None)

        with patch(
            "src.services.metrics.get_usage_recorder", return_value=fake_recorder
        ):
            await interceptor._push_usage(_make_usage())

        metrics = MetricsCollector().request_metrics
        assert (
            metrics.billing_flush_failures_total.get(
                stage="callback", error_type="ValueError"
            )
            == 4
        )


@pytest.mark.usefixtures("fast_backoffs")
class TestDLQFullFallback:
    @pytest.mark.asyncio
    async def test_dlq_push_itself_fails_increments_full_counter(self):
        """If LPUSH to DLQ also fails, dead_letter_full counter bumps."""
        redis = AsyncMock()
        redis.publish = AsyncMock()
        redis.lpush = AsyncMock(side_effect=ConnectionError("redis dead"))
        redis.ltrim = AsyncMock()

        interceptor = BillingInterceptor(redis_client=redis)

        fake_recorder = MagicMock()
        fake_recorder.record_usage = AsyncMock(
            side_effect=ConnectionError("db also dead")
        )

        with patch(
            "src.services.metrics.get_usage_recorder", return_value=fake_recorder
        ):
            await interceptor._push_usage(_make_usage())

        metrics = MetricsCollector().request_metrics
        assert (
            metrics.billing_records_dropped_total.get(reason="dead_letter_full") == 1
        )


@pytest.mark.usefixtures("fast_backoffs")
class TestDLQReplay:
    @pytest.mark.asyncio
    async def test_dlq_item_replays_to_database_and_audits_success(self):
        usage = _make_usage(request_id="req-replay")
        payload = json.dumps(
            {
                "stage": "database",
                "usage": usage.__dict__,
                "dropped_at": 123.0,
            }
        )
        redis = AsyncMock()
        redis.rpop = AsyncMock(side_effect=[payload, None])
        redis.lpush = AsyncMock()
        redis.ltrim = AsyncMock()

        interceptor = BillingInterceptor(redis_client=redis)
        fake_recorder = MagicMock()
        fake_recorder.record_usage = AsyncMock(return_value=None)

        with patch(
            "src.services.metrics.get_usage_recorder", return_value=fake_recorder
        ):
            result = await interceptor.replay_dead_letter(limit=10)

        assert result == {"attempted": 1, "replayed": 1, "failed": 0}
        fake_recorder.record_usage.assert_awaited_once()
        audit_key, audit_payload = redis.lpush.await_args.args
        assert audit_key == "metrics:billing:dead_letter:replayed"
        decoded = json.loads(audit_payload)
        assert decoded["usage"]["request_id"] == "req-replay"
        assert decoded["replayed_at"] > 0
