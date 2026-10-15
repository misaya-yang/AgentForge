"""Pydantic envelope + first-party payload schemas for the event bus.

Every event published on Redis Streams is wrapped in an
``EventEnvelope[T]`` so producers and consumers share a single
serialisation contract:

- The envelope owns *transport metadata* — id, timestamp, producer,
  tenant, request correlation, traceparent.
- The payload ``T`` carries the *business data* and is opaque to the
  bus.

Why generics? It lets handlers say ``handler(env: EventEnvelope[
UsageRecordedV1])`` and get full IDE/type-check on ``env.payload``
without losing the shared envelope fields.

Why not put ``event_type`` inside the payload? Routing decisions
(stream key, group name, DLQ) need ``event_type`` *before* we attempt
to deserialise the payload, so it lives on the envelope.

Discriminator pattern: ``payload_for(event_type)`` returns the
registered Pydantic model class for an event type, so consumers can
parse ``payload_dict`` into the right concrete model without each
caller re-implementing a switch. Unknown ``event_type`` raises
``EventDeserializationError`` — see the test for the contract.
"""

from __future__ import annotations

from datetime import datetime, timezone
from typing import Any, ClassVar, Generic, TypeVar
from uuid import UUID, uuid4

from pydantic import BaseModel, ConfigDict, Field

from .event_errors import EventDeserializationError

# --- generic payload type ---------------------------------------------------

PayloadT = TypeVar("PayloadT", bound=BaseModel)


def _utcnow() -> datetime:
    """Tz-aware UTC ``datetime`` for envelope timestamps."""
    return datetime.now(timezone.utc)


class EventEnvelope(BaseModel, Generic[PayloadT]):
    """Universal wrapper for any event published on the bus.

    The envelope is intentionally light — the only heavyweight field is
    ``payload``. All other fields are either auto-generated
    (``event_id``, ``occurred_at``) or producer-controlled context
    (``tenant_id``, ``request_id``, ``traceparent``).

    Serialisation contract:

    - ``EventBus.publish`` writes the JSON-encoded envelope into a
      single XADD field named ``"payload"``. Multi-field XADD entries
      are avoided so the bytes-on-the-wire format is independent of
      Redis client quirks.
    - ``model_dump_json()`` is the canonical wire format. Use
      ``EventEnvelope[YourPayload].model_validate_json(bytes)`` on the
      consume side once you know the payload type.
    """

    model_config = ConfigDict(
        # Strict enough to catch typos, lenient enough to allow new
        # optional fields on the producer without breaking old consumers.
        extra="ignore",
        # Datetime is encoded as ISO-8601 in JSON.
        ser_json_timedelta="iso8601",
    )

    event_id: UUID = Field(default_factory=uuid4)
    event_type: str
    schema_version: int = 1
    occurred_at: datetime = Field(default_factory=_utcnow)
    producer: str
    tenant_id: str
    request_id: str
    traceparent: str | None = None
    payload: PayloadT


# --- concrete payload schemas ----------------------------------------------


class UsageRecordedV1(BaseModel):
    """Payload for a single usage record (token counts, latency, status).

    Mirrors the in-memory ``UsageRecord`` dataclass used by the gateway's
    ``UsageRecorder``, but trimmed to the fields actually needed by
    downstream consumers (billing aggregates, observability sinks). New
    consumers should add fields here, not in the envelope.

    ``EVENT_TYPE`` is the canonical string for routing this payload type
    through the bus — producers and consumers reference it via the class
    attribute so a renamed event surfaces as a type error, not a runtime
    misroute.
    """

    EVENT_TYPE: ClassVar[str] = "usage.recorded.v1"

    model_config = ConfigDict(extra="ignore")

    tenant_id: str
    user_id: str
    model: str
    provider: str = ""
    service_id: str = ""
    assistant_id: str = ""
    input_tokens: int = 0
    output_tokens: int = 0
    latency_ms: int = 0
    first_token_ms: int = 0
    request_total_duration_ms: int = 0
    error_type: str | None = None
    status: str = "success"
    request_type: str = "chat"
    timestamp: float
    metadata: dict[str, Any] = Field(default_factory=dict)


# --- discriminator registry -------------------------------------------------


# Maps ``event_type`` -> concrete payload model class. Kept on the module
# rather than ``registry.py`` because ``registry.py`` is the *transport*
# routing table (stream key) and this is the *schema* table (model class);
# they evolve independently.
_PAYLOAD_MODELS: dict[str, type[BaseModel]] = {
    UsageRecordedV1.EVENT_TYPE: UsageRecordedV1,
}


def payload_for(event_type: str) -> type[BaseModel]:
    """Return the registered Pydantic model class for ``event_type``.

    Raises
    ------
    EventDeserializationError
        ``event_type`` is not in the registry. Treated as a poison
        message by the consumer — see ``EventConsumer._dispatch``.
    """
    try:
        return _PAYLOAD_MODELS[event_type]
    except KeyError as exc:
        raise EventDeserializationError(
            f"no payload schema registered for event_type {event_type!r}"
        ) from exc


def parse_envelope(raw_json: str | bytes) -> EventEnvelope[BaseModel]:
    """Parse a JSON envelope, dispatching on ``event_type``.

    Two-step parse:

    1. Decode as a generic envelope with a ``dict`` payload to learn
       ``event_type``.
    2. Re-validate with the concrete payload model from
       ``payload_for(event_type)`` to enforce schema.

    Raises
    ------
    EventDeserializationError
        Bad JSON, missing required envelope field, or unknown
        ``event_type``.
    """
    try:
        # Step 1: loose parse to read event_type.
        loose = EventEnvelope[_OpaquePayload].model_validate_json(raw_json)
    except Exception as exc:  # noqa: BLE001 — Pydantic raises a custom type
        raise EventDeserializationError(f"malformed envelope: {exc}") from exc

    payload_cls = payload_for(loose.event_type)
    # Step 2: rebuild with the concrete payload class so tests/consumers
    # can use ``env.payload.field`` with full typing.
    return EventEnvelope[payload_cls].model_validate_json(raw_json)  # type: ignore[valid-type]


class _OpaquePayload(BaseModel):
    """Loose payload used during the first parse pass of ``parse_envelope``.

    Accepts any field set so step-1 succeeds for every registered event
    type; step-2 then enforces the strict schema.
    """

    model_config = ConfigDict(extra="allow")

    # No declared fields — all data lands in ``__pydantic_extra__``.
    # ``ClassVar`` placeholder to keep the linter happy.
    _placeholder: ClassVar[None] = None


__all__ = [
    "EventEnvelope",
    "UsageRecordedV1",
    "parse_envelope",
    "payload_for",
]
