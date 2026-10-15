"""``EventBus`` — async publisher for the Redis Streams event bus.

Responsibilities (and *only* these):

- Maintain a single lazy ``redis.asyncio.Redis`` connection.
- Resolve the destination stream key from the envelope's ``event_type``.
- ``XADD`` the envelope as a single JSON blob in the field
  ``"payload"`` with an approximate ``MAXLEN ~ {trim_max_len}``
  (``~`` = approximate trim, lets Redis pick efficient trim points).
- Return the auto-generated stream id so the caller can correlate.

Things this module deliberately does NOT do:

- No retries / backoff — that belongs in the calling code or a higher
  layer. Redis is fast enough that a transient blip is the user's
  problem to handle.
- No batching — single-event publish keeps the contract simple.
  Aggregation happens upstream (the usage recorder already buffers).
- No serialization fallbacks — anything that isn't a valid
  ``EventEnvelope`` is a programming bug, not a bus concern.

Wire format:

    XADD <stream> MAXLEN ~ <N> *  payload "<envelope-json>"

The single-field "payload" form keeps decoder code trivial: read the
``payload`` field bytes, ``parse_envelope`` it, done.
"""

from __future__ import annotations

import asyncio
import logging
from typing import Any

import redis.asyncio as aioredis
from ai_gateway_contracts.event_envelope import EventEnvelope
from ai_gateway_contracts.event_errors import EventBusError

from ..logging import record_internal_exception
from .registry import get_stream

logger = logging.getLogger(__name__)


# Default approximate stream length. Redis will trim to roughly this
# many entries (the ``~`` modifier permits over-trim for efficiency).
# 50_000 entries × ~1 KB envelopes = ~50 MB per stream — comfortable
# for a single-shard Redis with multiple streams.
_DEFAULT_MAX_LEN: int = 50_000


class EventBus:
    """Lazy-connected async publisher.

    Lifecycle:

        bus = EventBus("redis://localhost:6379/0")
        await bus.publish(envelope)
        ...
        await bus.close()  # idempotent

    The first ``publish`` triggers the connection; subsequent calls
    reuse the pool. ``close()`` is safe to call multiple times.
    """

    def __init__(
        self,
        redis_url: str,
        *,
        trim_max_len: int = _DEFAULT_MAX_LEN,
        client: aioredis.Redis | None = None,
    ) -> None:
        """
        Parameters
        ----------
        redis_url
            Standard ``redis://...`` URL. DB index in the URL is honoured.
        trim_max_len
            Approximate ``MAXLEN`` cap per stream. Override per-instance
            if a particular service produces vastly more events.
        client
            Pre-built async Redis client. **Tests use this** to inject
            ``fakeredis.aioredis.FakeRedis``. When supplied,
            ``redis_url`` is stored only for diagnostics; the existing
            client is reused as-is and ``close()`` will close it.
        """
        self._redis_url = redis_url
        self._trim_max_len = trim_max_len
        self._client: aioredis.Redis | None = client
        # Track whether we own the connection so ``close()`` doesn't
        # tear down a caller-injected client (tests share a fakeredis
        # client between bus + consumer; double-close is bad news).
        self._owns_client: bool = client is None
        self._connect_lock = asyncio.Lock()

    # ----- public API ------------------------------------------------------

    async def publish(self, envelope: EventEnvelope[Any]) -> str:
        """Publish ``envelope`` and return the assigned stream id.

        The stream key is resolved from ``envelope.event_type`` via
        ``registry.get_stream``. Unknown event types raise
        ``EventBusError`` so misconfigured producers fail loudly.
        """
        try:
            stream = get_stream(envelope.event_type)
        except KeyError as exc:
            raise EventBusError(str(exc)) from exc

        client = await self._get_client()
        # ``model_dump_json`` produces the canonical wire format. Encoding
        # to bytes lets us bypass redis-py's str→bytes conversion and
        # makes the on-wire payload independent of client encoding flags.
        payload_bytes = envelope.model_dump_json().encode("utf-8")

        # ``approximate=True`` -> XADD ... MAXLEN ~ N — Redis is allowed
        # to over-trim a little for efficiency. Exact trims would force
        # O(log N) work on every write.
        message_id: bytes = await client.xadd(  # type: ignore[assignment]
            stream,
            {"payload": payload_bytes},
            maxlen=self._trim_max_len,
            approximate=True,
        )
        # redis-py returns ``bytes`` when ``decode_responses=False`` and
        # ``str`` otherwise. Normalise so callers always see ``str``.
        return message_id.decode() if isinstance(message_id, bytes) else message_id

    async def close(self) -> None:
        """Close the underlying connection. Safe to call multiple times.

        If the bus was constructed with an injected ``client`` (tests),
        the client is *not* closed here — its lifetime belongs to the
        caller. Only client instances minted by ``_get_client`` are
        torn down.
        """
        client, self._client = self._client, None
        if client is not None and self._owns_client:
            try:
                await client.aclose()
            except Exception as exc:  # noqa: BLE001 — best-effort teardown
                record_internal_exception(
                    logger,
                    "assistant.events.client_teardown_failed",
                    exc,
                    level=logging.DEBUG,
                )
        self._owns_client = False

    # ----- internals -------------------------------------------------------

    async def _get_client(self) -> aioredis.Redis:
        if self._client is not None:
            return self._client
        async with self._connect_lock:
            if self._client is None:
                self._client = aioredis.from_url(
                    self._redis_url,
                    decode_responses=False,
                )
                self._owns_client = True
            return self._client


__all__ = ["EventBus"]
