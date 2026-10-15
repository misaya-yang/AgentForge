"""Failing handler → after MAX_DELIVERIES tries the message lands in :dlq.

Scenario:

1. Publish one envelope.
2. Run a consumer whose handler always raises.
3. Restart the consumer enough times to push the delivery counter past
   ``MAX_DELIVERIES`` (3). Each restart's ``XAUTOCLAIM`` reclaims the
   pending entry with idle=0 (we lower ``RECLAIM_IDLE_MS`` for the test).
4. After the third failure the message must:
   - be ack'd against the original stream (no longer pending), and
   - appear in ``{stream}:dlq`` with ``original_id`` + ``error`` +
     ``attempts`` fields.

We patch ``RECLAIM_IDLE_MS`` to 0 so reclaiming is immediate. Retries use
the same consumer name: the consumer must release its failed idempotency
claim, otherwise the second delivery would be skipped and ACKed.
"""

from __future__ import annotations

import asyncio
from unittest.mock import patch

import pytest
from ai_gateway_contracts.event_envelope import EventEnvelope, UsageRecordedV1
from ai_gateway_core.events import consumer as consumer_module
from ai_gateway_core.events.bus import EventBus
from ai_gateway_core.events.consumer import EventConsumer
from ai_gateway_core.events.idempotency import IdempotencyStore
from ai_gateway_core.events.registry import DLQ_SUFFIX, STREAM_NAMES


class _BoomError(RuntimeError):
    """Sentinel raised by the always-failing handler."""


@pytest.mark.asyncio
async def test_handler_failures_route_to_dlq(
    fake_redis, usage_payload_dict: dict
):
    # Reclaim idle = 0 so retries flow without sleeping for a minute.
    with patch.object(consumer_module, "RECLAIM_IDLE_MS", 0):
        bus = EventBus("redis://unused", client=fake_redis)

        envelope = EventEnvelope[UsageRecordedV1](
            event_type="usage.recorded.v1",
            producer="ai-gateway",
            tenant_id="t1",
            request_id="req-1",
            payload=UsageRecordedV1(**usage_payload_dict),
        )
        await bus.publish(envelope)

        async def boom(env: EventEnvelope) -> None:  # noqa: ARG001
            raise _BoomError("nope")

        stream = STREAM_NAMES["usage.recorded.v1"]
        # Reuse the same consumer identity to reproduce a normal worker
        # restart. The failed handler must release the idempotency claim so
        # the reclaimed message is actually retried.
        for _attempt in range(3):
            consumer = EventConsumer(
                "redis://unused",
                stream=stream,
                group="dlq-group",
                consumer_name="worker-0",
                handler=boom,
                client=fake_redis,
                idempotency=IdempotencyStore(fake_redis, prefix="t-idem"),
            )
            task = asyncio.create_task(consumer.start())
            # Each pass: reclaim → read → fail → leave pending (or DLQ on 3rd).
            await asyncio.sleep(0.3)
            await consumer.stop()
            await asyncio.wait_for(task, timeout=10)

        # After three failures the original entry must be ack'd (gone
        # from pending) and a DLQ entry must exist.
        pending_summary = await fake_redis.xpending(stream, "dlq-group")
        pending_count = (
            pending_summary["pending"]
            if isinstance(pending_summary, dict)
            else pending_summary[0]
        )
        assert pending_count == 0, "original message should be ack'd after DLQ"

        dlq_entries = await fake_redis.xrange(stream + DLQ_SUFFIX)
        assert len(dlq_entries) >= 1, "expected DLQ entry"
        _dlq_id, dlq_fields = dlq_entries[-1]
        assert b"original_id" in dlq_fields
        assert b"error" in dlq_fields
        assert b"attempts" in dlq_fields
        # Error string must surface the handler's exception type.
        assert b"_BoomError" in dlq_fields[b"error"]
        # Attempts field is at least MAX_DELIVERIES.
        assert int(dlq_fields[b"attempts"]) >= 3

        await bus.close()
