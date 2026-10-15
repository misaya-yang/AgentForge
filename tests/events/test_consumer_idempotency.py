"""Idempotency: same ``event_id`` → handler invoked once, both ack'd.

Scenario:

1. Publish two envelopes that share an ``event_id`` (simulating a redelivery
   from a producer-side retry).
2. Run the consumer.
3. Assert the handler was called exactly once *and* both stream entries
   were ack'd (no pending backlog).

This exercises ``IdempotencyStore.claim`` end-to-end inside the consumer
loop.
"""

from __future__ import annotations

import asyncio

import pytest
from ai_gateway_contracts.event_envelope import EventEnvelope, UsageRecordedV1
from ai_gateway_core.events.bus import EventBus
from ai_gateway_core.events.consumer import EventConsumer
from ai_gateway_core.events.registry import STREAM_NAMES


@pytest.mark.asyncio
async def test_duplicate_event_id_calls_handler_once(
    fake_redis, usage_payload_dict: dict
):
    bus = EventBus("redis://unused", client=fake_redis)

    payload = UsageRecordedV1(**usage_payload_dict)
    envelope = EventEnvelope[UsageRecordedV1](
        event_type="usage.recorded.v1",
        producer="ai-gateway",
        tenant_id="t1",
        request_id="req-1",
        payload=payload,
    )
    # Publish the *same* envelope (same event_id) twice.
    await bus.publish(envelope)
    await bus.publish(envelope)

    received: list[EventEnvelope] = []

    async def handler(env: EventEnvelope) -> None:
        received.append(env)

    stream = STREAM_NAMES["usage.recorded.v1"]
    consumer = EventConsumer(
        "redis://unused",
        stream=stream,
        group="test-group",
        consumer_name="worker-0",
        handler=handler,
        client=fake_redis,
    )

    task = asyncio.create_task(consumer.start())
    # Give the consumer enough time to drain both messages. The
    # ``BLOCK=5000`` round will return early on data; the second loop
    # iteration will block fully — we cancel via ``stop()`` before it
    # times out.
    await asyncio.sleep(0.5)
    await consumer.stop()
    await asyncio.wait_for(task, timeout=10)

    # Handler ran exactly once — second delivery deduped.
    assert len(received) == 1
    assert str(received[0].event_id) == str(envelope.event_id)

    # Both entries ack'd: no pending entries left in the group.
    pending_summary = await fake_redis.xpending(stream, "test-group")
    # ``xpending`` returns either dict (modern) or tuple (legacy) summary.
    pending_count = (
        pending_summary["pending"]
        if isinstance(pending_summary, dict)
        else pending_summary[0]
    )
    assert pending_count == 0

    await bus.close()
