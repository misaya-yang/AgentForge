"""Event-bus exception hierarchy.

A small, deliberately flat tree:

- ``EventBusError`` is the root — catch this if you want to swallow any
  bus-related error without distinguishing the cause.
- ``EventDeserializationError`` — the raw stream entry could not be
  parsed back into a typed ``EventEnvelope`` (bad JSON, missing fields,
  schema mismatch). Surfaced by the consumer before the handler runs.
- ``EventHandlerError`` — the handler itself raised. The consumer
  re-wraps user exceptions in this type so the retry/DLQ logic can
  distinguish "we tried to deliver" from "we couldn't even deserialize".
"""

from __future__ import annotations


class EventBusError(Exception):
    """Base class for every error raised by the events subsystem."""


class EventDeserializationError(EventBusError):
    """Raised when a stream entry cannot be decoded into an envelope.

    The consumer treats this as poison: the message is shipped straight
    to the DLQ and ack'd, because retrying will not change the bytes.
    """


class EventHandlerError(EventBusError):
    """Raised by the consumer when the registered handler raises.

    Wraps the original exception in ``__cause__`` so the consumer's
    retry/DLQ machinery can count attempts without leaking the user's
    exception type into the bus's contract.
    """


__all__ = [
    "EventBusError",
    "EventDeserializationError",
    "EventHandlerError",
]
