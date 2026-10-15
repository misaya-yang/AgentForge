"""Envelope round-trip + discriminator tests.

What we're locking in:

- ``EventEnvelope[T]`` JSON round-trips losslessly (UUID, datetime,
  payload, traceparent).
- ``parse_envelope`` dispatches on ``event_type`` to the right concrete
  payload model (``UsageRecordedV1`` here).
- An unknown ``event_type`` raises ``EventDeserializationError`` so the
  consumer can route to DLQ.
- Default factories (``event_id``, ``occurred_at``) actually run.
"""

from __future__ import annotations

from datetime import datetime, timezone
from uuid import UUID

import pytest
from ai_gateway_contracts.event_envelope import (
    EventEnvelope,
    UsageRecordedV1,
    parse_envelope,
)
from ai_gateway_contracts.event_errors import (
    EventDeserializationError,
)


def _make_envelope(usage_payload_dict: dict) -> EventEnvelope[UsageRecordedV1]:
    return EventEnvelope[UsageRecordedV1](
        event_type="usage.recorded.v1",
        producer="ai-gateway",
        tenant_id="t1",
        request_id="req-abc",
        traceparent="00-aabb-ccdd-01",
        payload=UsageRecordedV1(**usage_payload_dict),
    )


def test_envelope_default_factories_populate_id_and_timestamp(
    usage_payload_dict: dict,
):
    env = _make_envelope(usage_payload_dict)

    # event_id is a real UUID4
    assert isinstance(env.event_id, UUID)
    assert env.event_id.version == 4

    # occurred_at is timezone-aware UTC
    assert isinstance(env.occurred_at, datetime)
    assert env.occurred_at.tzinfo is not None
    assert env.occurred_at.tzinfo.utcoffset(env.occurred_at) == timezone.utc.utcoffset(
        env.occurred_at
    )


def test_envelope_round_trip_via_parse_envelope(usage_payload_dict: dict):
    """publish-side .json() → consumer-side parse_envelope() yields equal data."""
    original = _make_envelope(usage_payload_dict)
    raw = original.model_dump_json()

    parsed = parse_envelope(raw)

    # envelope-level fields preserved
    assert str(parsed.event_id) == str(original.event_id)
    assert parsed.event_type == original.event_type
    assert parsed.producer == original.producer
    assert parsed.tenant_id == original.tenant_id
    assert parsed.request_id == original.request_id
    assert parsed.traceparent == original.traceparent
    assert parsed.schema_version == 1

    # payload validated as UsageRecordedV1
    assert isinstance(parsed.payload, UsageRecordedV1)
    assert parsed.payload.input_tokens == 100
    assert parsed.payload.output_tokens == 200
    assert parsed.payload.metadata == {"k": "v"}
    assert parsed.payload.timestamp == 1714200000.0


def test_parse_envelope_rejects_unknown_event_type():
    """Discriminator: unknown event_type → EventDeserializationError.

    The consumer relies on this to send poison messages to DLQ instead
    of crashing the worker.
    """
    raw = (
        '{"event_id":"00000000-0000-4000-8000-000000000000",'
        '"event_type":"unknown.future.v9",'
        '"schema_version":1,'
        '"occurred_at":"2025-01-01T00:00:00+00:00",'
        '"producer":"x","tenant_id":"t","request_id":"r",'
        '"traceparent":null,'
        '"payload":{"foo":"bar"}}'
    )
    with pytest.raises(EventDeserializationError) as exc_info:
        parse_envelope(raw)
    assert "unknown.future.v9" in str(exc_info.value)


def test_parse_envelope_rejects_malformed_json():
    """Non-JSON / missing required envelope fields → DeserializationError."""
    with pytest.raises(EventDeserializationError):
        parse_envelope("{not-json")
    with pytest.raises(EventDeserializationError):
        # missing required ``event_type``
        parse_envelope(
            '{"producer":"x","tenant_id":"t","request_id":"r",'
            '"payload":{}}'
        )
