"""Dual-write event-bus publishing in UsageRecorder.

Verifies the new ``_maybe_publish_usage_event`` path:
  * No-op when ``EVENT_BUS_REDIS_URL`` is unset (default).
  * Publishes a ``UsageRecordedV1`` event when env is set.
  * Bus failure is caught and latched — billing path unaffected.
"""

from __future__ import annotations

import time
from unittest.mock import AsyncMock, MagicMock

import pytest
from ai_gateway_contracts.event_envelope import EventEnvelope, UsageRecordedV1
from ai_gateway_core.metrics.usage_recorder import UsageRecord, UsageRecorder


@pytest.fixture
def base_record() -> UsageRecord:
    return UsageRecord(
        tenant_id="t-1",
        user_id="u-1",
        model="qwen3.5-plus",
        input_tokens=100,
        output_tokens=200,
        request_id="req-abc",
        provider="dashscope",
        latency_ms=1500,
        first_token_ms=300,
        request_total_duration_ms=1500,
        status="success",
        request_type="chat",
        metadata={"k": "v"},
        timestamp=time.time(),
    )


@pytest.fixture(autouse=True)
def _no_env(monkeypatch):
    monkeypatch.delenv("EVENT_BUS_REDIS_URL", raising=False)
    yield


async def test_no_op_when_env_unset(base_record):
    """Default path: env not set → bus stays None, latch True from start."""
    rec = UsageRecorder(database=None, buffer_size=10_000)
    # Constructor reads env once; absent env latches the publish flag
    # to True so the hot path can short-circuit on a single attr read.
    assert rec._event_bus_url == ""
    assert rec._event_bus_failed is True
    await rec._maybe_publish_usage_event(base_record)
    assert rec._event_bus is None


async def test_publishes_envelope_when_env_set(base_record, monkeypatch):
    """Env set → constructs EventBus once and publishes envelope."""
    monkeypatch.setenv("EVENT_BUS_REDIS_URL", "redis://fake:6379/0")

    rec = UsageRecorder(database=None, buffer_size=10_000)

    fake_bus = MagicMock()
    fake_bus.publish = AsyncMock(return_value="1234-0")

    # Inject without going through the EventBus constructor (which would
    # try to dial a real redis on first publish).
    rec._event_bus = fake_bus

    await rec._maybe_publish_usage_event(base_record)

    fake_bus.publish.assert_awaited_once()
    envelope = fake_bus.publish.call_args.args[0]
    assert isinstance(envelope, EventEnvelope)
    assert envelope.event_type == "usage.recorded.v1"
    assert envelope.tenant_id == "t-1"
    assert envelope.request_id == "req-abc"
    assert envelope.producer == "usage-recorder"
    assert isinstance(envelope.payload, UsageRecordedV1)
    p = envelope.payload
    assert p.tenant_id == "t-1"
    assert p.model == "qwen3.5-plus"
    assert p.input_tokens == 100
    assert p.output_tokens == 200
    assert p.latency_ms == 1500
    assert p.metadata == {"k": "v"}


async def test_bus_failure_is_latched_and_silent(base_record, monkeypatch, caplog):
    """A bus exception → ``_event_bus_failed`` flips True; no further attempts."""
    monkeypatch.setenv("EVENT_BUS_REDIS_URL", "redis://fake:6379/0")

    rec = UsageRecorder(database=None, buffer_size=10_000)
    bad_bus = MagicMock()
    bad_bus.publish = AsyncMock(side_effect=RuntimeError("boom"))
    rec._event_bus = bad_bus

    # First call: error caught, flag latched.
    await rec._maybe_publish_usage_event(base_record)
    assert rec._event_bus_failed is True
    assert bad_bus.publish.await_count == 1

    # Second call: skipped due to latch — no extra publish attempt.
    await rec._maybe_publish_usage_event(base_record)
    assert bad_bus.publish.await_count == 1


async def test_record_path_calls_publish(base_record, monkeypatch):
    """Full record() path: DB buffer append + publish are both invoked."""
    monkeypatch.setenv("EVENT_BUS_REDIS_URL", "redis://fake:6379/0")

    rec = UsageRecorder(database=None, buffer_size=10_000)
    fake_bus = MagicMock()
    fake_bus.publish = AsyncMock(return_value="x-0")
    rec._event_bus = fake_bus

    # Stub the cost calculator + dimension normaliser so we don't need a DB.
    async def _noop(*_args, **_kwargs):
        return None

    rec._normalize_record_dimensions = _noop  # type: ignore[assignment]
    rec._calculate_cost = _noop  # type: ignore[assignment]

    await rec.record(base_record)

    # DB buffer received the record AND the bus got the event.
    assert len(rec._buffer) == 1
    assert rec._buffer[0] is base_record
    fake_bus.publish.assert_awaited_once()
