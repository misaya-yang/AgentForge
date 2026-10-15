"""Async Redis Streams event bus — public surface.

Sub-modules:

- ``envelope`` — ``EventEnvelope[T]`` + concrete payloads
  (``UsageRecordedV1``).
- ``registry`` — static ``STREAM_NAMES`` map +
  ``get_stream(event_type)`` helper.
- ``bus`` — ``EventBus.publish(envelope)`` writes to a stream.
- ``consumer`` — ``EventConsumer`` runs the XREADGROUP loop with
  idempotency + retry + DLQ.
- ``idempotency`` — ``IdempotencyStore.claim(consumer, event_id)``.
- ``errors`` — exception hierarchy.

Quick start (publisher)::

    from ai_gateway_core.events import EventBus, EventEnvelope, UsageRecordedV1

    bus = EventBus("redis://localhost:6379/0")
    await bus.publish(
        EventEnvelope[UsageRecordedV1](
            event_type="usage.recorded.v1",
            producer="ai-gateway",
            tenant_id="t1",
            request_id="req-abc",
            payload=UsageRecordedV1(
                tenant_id="t1", user_id="u1", model="qwen-3.6",
                timestamp=1234.5,
            ),
        )
    )
"""

from __future__ import annotations

from .bus import EventBus
from .consumer import EventConsumer
from .envelope import EventEnvelope, UsageRecordedV1, parse_envelope
from .errors import EventBusError, EventDeserializationError, EventHandlerError
from .idempotency import IdempotencyStore
from .registry import DLQ_SUFFIX, STREAM_NAMES, dlq_for, get_stream

__all__ = [
    "DLQ_SUFFIX",
    "EventBus",
    "EventBusError",
    "EventConsumer",
    "EventDeserializationError",
    "EventEnvelope",
    "EventHandlerError",
    "IdempotencyStore",
    "STREAM_NAMES",
    "UsageRecordedV1",
    "dlq_for",
    "get_stream",
    "parse_envelope",
]
