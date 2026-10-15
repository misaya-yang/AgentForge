"""Static registry mapping event types to Redis Stream key names.

Centralising these strings keeps producers and consumers in lockstep:
when a new event is introduced you add one entry here and both ends
pick it up via ``get_stream(event_type)``.

Naming convention:

    Event type   :  ``<domain>.<verb>.v<schema_version>``
                    e.g. ``"usage.recorded.v1"``
    Stream key   :  ``events:<domain>:<verb>:v<schema_version>``
                    e.g. ``"events:usage:recorded:v1"``
    DLQ key      :  ``<stream_key>:dlq``
                    e.g. ``"events:usage:recorded:v1:dlq"``

Schema version is part of the event type AND the stream name on
purpose: a v2 schema gets its own stream so v1 consumers keep
working until they're migrated. Never reuse a stream key after a
schema break.
"""

from __future__ import annotations

# Suffix appended to every stream key to derive its dead-letter queue.
# A handler that fails ``MAX_DELIVERIES`` times has its message moved
# to ``f"{stream}{DLQ_SUFFIX}"`` and the original is ack'd.
DLQ_SUFFIX: str = ":dlq"


# Static map: event_type -> stream key.
# Add new entries here when new envelope payload types ship.
STREAM_NAMES: dict[str, str] = {
    "usage.recorded.v1": "events:usage:recorded:v1",
}


def get_stream(event_type: str) -> str:
    """Return the stream key for ``event_type``.

    Raises
    ------
    KeyError
        If ``event_type`` is not registered. The caller — typically a
        publisher — is expected to fail loudly rather than silently
        publish to the wrong stream.
    """
    try:
        return STREAM_NAMES[event_type]
    except KeyError as exc:
        raise KeyError(
            f"unknown event_type {event_type!r}; "
            f"register it in ai_gateway_core.events.registry.STREAM_NAMES"
        ) from exc


def dlq_for(stream: str) -> str:
    """Return the DLQ key for an arbitrary ``stream`` key.

    Useful for the consumer when it wants to publish a poisoned message
    without a round-trip through ``get_stream`` (it already knows which
    stream it was reading from).
    """
    return f"{stream}{DLQ_SUFFIX}"


__all__ = [
    "DLQ_SUFFIX",
    "STREAM_NAMES",
    "dlq_for",
    "get_stream",
]
