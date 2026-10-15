"""End-to-end smoke test: publisher → bus → consumer → handler.

This test stitches every piece together against ``fakeredis``:

- Build an ``EventBus`` and an ``EventConsumer`` against the same
  fakeredis client.
- Publish a real ``UsageRecordedV1`` envelope.
- Confirm the handler is invoked with a concrete ``UsageRecordedV1``
  payload whose fields match the publisher.
- Confirm the consumer ACK'd the message (pending = 0).

If this test breaks, the wire format between publish and consume is
broken — the most likely place for a regression.
"""

from __future__ import annotations

import asyncio

import pytest
from ai_gateway_contracts.event_envelope import EventEnvelope, UsageRecordedV1
from ai_gateway_core.events.bus import EventBus
from ai_gateway_core.events.consumer import EventConsumer
from ai_gateway_core.events.registry import STREAM_NAMES


@pytest.mark.asyncio
async def test_publish_consume_round_trip(fake_redis, usage_payload_dict: dict):
    bus = EventBus("redis://unused", client=fake_redis)

    envelope = EventEnvelope[UsageRecordedV1](
        event_type="usage.recorded.v1",
        producer="ai-gateway",
        tenant_id="tenant-42",
        request_id="req-xyz",
        traceparent="00-aabbccdd-11223344-01",
        payload=UsageRecordedV1(**usage_payload_dict),
    )
    published_id = await bus.publish(envelope)
    assert published_id

    received: list[EventEnvelope] = []

    async def handler(env: EventEnvelope) -> None:
        received.append(env)

    stream = STREAM_NAMES["usage.recorded.v1"]
    consumer = EventConsumer(
        "redis://unused",
        stream=stream,
        group="e2e-group",
        consumer_name="e2e-worker",
        handler=handler,
        client=fake_redis,
    )

    task = asyncio.create_task(consumer.start())
    # One BLOCK round (5s) is enough to pick up the entry; we shut
    # down well before the second iteration's full block elapses.
    await asyncio.sleep(0.3)
    await consumer.stop()
    await asyncio.wait_for(task, timeout=10)

    assert len(received) == 1
    got = received[0]

    # Envelope round-trip
    assert str(got.event_id) == str(envelope.event_id)
    assert got.event_type == "usage.recorded.v1"
    assert got.tenant_id == "tenant-42"
    assert got.request_id == "req-xyz"
    assert got.traceparent == "00-aabbccdd-11223344-01"

    # Payload typed correctly + values intact
    assert isinstance(got.payload, UsageRecordedV1)
    assert got.payload.tenant_id == "t1"  # from usage_payload_dict
    assert got.payload.user_id == "u1"
    assert got.payload.input_tokens == 100
    assert got.payload.output_tokens == 200
    assert got.payload.metadata == {"k": "v"}

    # Consumer ACK'd the message.
    pending_summary = await fake_redis.xpending(stream, "e2e-group")
    pending_count = (
        pending_summary["pending"]
        if isinstance(pending_summary, dict)
        else pending_summary[0]
    )
    assert pending_count == 0

    await bus.close()
