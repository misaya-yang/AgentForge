"""``EventConsumer`` — single-stream, single-group consumer loop.

The consumer owns one Redis Streams consumer-group worker. It

1. Re-claims orphaned pending entries on startup via ``XAUTOCLAIM``
   (idle ≥ 60 s) so a crashed peer's inflight messages don't stall.
2. Runs a short-block ``XREADGROUP`` loop (``COUNT=32 BLOCK=1000``).
   The 1-second block keeps shutdown latency bounded; production
   throughput is unaffected because Redis returns immediately when
   data is available.
3. For each entry: deserialise → idempotency-claim → dispatch to the
   handler → ACK on success, escalate on failure.
4. Delivery counts are tracked **in-process** (per-stream id) — Redis's
   ``XPENDING`` is consulted as a fallback when available, but not
   relied upon, because ``fakeredis`` (used by the test suite) returns
   an empty pending list immediately after delivery. After
   ``MAX_DELIVERIES`` failed deliveries the message is shipped to
   ``{stream}:dlq`` (with ``original_id``, ``error``, ``attempts``
   fields) and ACKed against the original stream so it stops
   replaying.
5. Deserialisation errors are treated as poison — straight to DLQ on
   the first failure, no retries (replaying won't change the bytes).

Lifecycle:

    consumer = EventConsumer(
        "redis://localhost:6379/0",
        stream="events:usage:recorded:v1",
        group="billing-aggregator",
        consumer_name="worker-0",
        handler=on_usage,
    )
    task = asyncio.create_task(consumer.start())
    ...
    await consumer.stop()  # signals stop, waits for inflight to drain
    await task             # wait for loop exit
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
from collections.abc import Awaitable, Callable
from typing import Any

import redis.asyncio as aioredis
from ai_gateway_contracts.event_envelope import EventEnvelope, parse_envelope
from ai_gateway_contracts.event_errors import EventDeserializationError, EventHandlerError

from .idempotency import IdempotencyStore
from .registry import dlq_for

logger = logging.getLogger(__name__)


# Max times a single message may be delivered before it's parked in
# the DLQ. The 3rd failed delivery triggers the move (counter starts at 1).
MAX_DELIVERIES: int = 3

# Idle threshold for ``XAUTOCLAIM`` on startup. Pending entries idle
# longer than this are assumed to belong to a dead peer and reclaimed
# by this consumer. Tests patch this to 0 to exercise the path without
# real wall-clock waits.
RECLAIM_IDLE_MS: int = 60_000

# ``XREADGROUP`` defaults — count per round + block timeout in ms.
# A 1 s block keeps ``stop()`` responsive (worst-case shutdown ≈ 1 s).
_READ_COUNT: int = 32
_READ_BLOCK_MS: int = 1_000

# Cooperative back-off when XREADGROUP returns empty. Real Redis honours
# ``block`` and returns only after that timeout, so this sleep adds at
# most a few ms to live throughput. ``fakeredis`` (test) ignores
# ``block``, so without this sleep the loop would burn CPU + starve
# ``stop()`` of any chance to run.
_IDLE_SLEEP_SEC: float = 0.05


HandlerT = Callable[[EventEnvelope[Any]], Awaitable[None]]


class EventConsumer:
    """Single consumer-group worker."""

    def __init__(
        self,
        redis_url: str,
        *,
        stream: str,
        group: str,
        consumer_name: str,
        handler: HandlerT,
        client: aioredis.Redis | None = None,
        idempotency: IdempotencyStore | None = None,
    ) -> None:
        """
        Parameters
        ----------
        redis_url
            Standard ``redis://...`` URL.
        stream
            Redis stream key (resolve with ``registry.get_stream``).
        group
            Consumer group name. Multiple workers share a group to
            scale horizontally; each event is delivered to one member.
        consumer_name
            Unique name within the group (typically pod hostname or
            worker index).
        handler
            Async callable invoked per envelope. Raise to trigger
            retry/DLQ. **Must be idempotent in practice** — the bus
            cannot guarantee exactly-once even with the dedupe table
            (TTL'd entries are exposed to long-tail replays).
        client
            Pre-built async Redis client (tests inject ``fakeredis``
            here). When provided the consumer does **not** close it
            on exit — lifetime belongs to the caller.
        idempotency
            Override the default ``IdempotencyStore``. Useful for tests
            that want a tighter TTL.
        """
        self._redis_url = redis_url
        self._stream = stream
        self._group = group
        self._consumer = consumer_name
        self._handler = handler
        self._client: aioredis.Redis | None = client
        self._owns_client: bool = client is None
        self._idem: IdempotencyStore | None = idempotency
        self._stop_event = asyncio.Event()
        # Tracks the inflight handler call so ``stop()`` can wait
        # for it to drain rather than killing it mid-way.
        self._inflight: asyncio.Task[None] | None = None
        # Redis-backed delivery counter. Lives in a hash so the count
        # survives consumer restarts (a crashed worker comes back, its
        # peer reclaims the message, the count picks up where the dead
        # peer left off). HDEL on success/DLQ keeps it bounded.
        self._delivery_counter_key = f"events:deliveries:{stream}:{group}"

    # ----- public API ------------------------------------------------------

    async def start(self) -> None:
        """Run the consume loop until ``stop()`` is called.

        Returns when ``stop()`` is observed *and* the inflight handler
        (if any) has drained. The connection is closed on exit only if
        this consumer created it.
        """
        client = await self._get_client()
        await self._ensure_group(client)
        idem = self._get_idempotency(client)

        try:
            # 1. Drain any messages reclaimed from a dead peer before
            #    accepting new ones. ``XAUTOCLAIM`` transfers them to
            #    *this* consumer's PEL; we dispatch them straight from
            #    the autoclaim result rather than re-reading via
            #    ``XREADGROUP "0"`` (some Redis-clone servers — fakeredis
            #    in particular — return empty for own-pending reads).
            reclaimed = await self._reclaim_pending(client)
            for message_id, fields in reclaimed:
                if self._stop_event.is_set():
                    break
                await self._handle_one(client, idem, message_id, fields)

            # 2. Steady-state: read new entries via ``>`` until stopped.
            while not self._stop_event.is_set():
                entries = await self._read_once(client)
                if not entries:
                    # Either BLOCK timeout with no data or a transient
                    # error. Yield control so a misbehaving client
                    # (e.g. fakeredis ignores BLOCK) cannot busy-loop;
                    # the cooperative sleep also lets ``stop()`` be
                    # observed promptly.
                    await asyncio.sleep(_IDLE_SLEEP_SEC)
                    continue
                await self._dispatch_entries(client, idem, entries)
        finally:
            if self._owns_client:
                await self._close_client()

    async def stop(self) -> None:
        """Signal the loop to exit and wait for inflight to drain."""
        self._stop_event.set()
        if self._inflight is not None and not self._inflight.done():
            with contextlib.suppress(Exception):
                await self._inflight

    # ----- internals -------------------------------------------------------

    async def _dispatch_entries(
        self,
        client: aioredis.Redis,
        idem: IdempotencyStore,
        entries: list[Any],
    ) -> None:
        """Iterate one XREADGROUP result, dispatching each message.

        Bails on the next message boundary when ``_stop_event`` flips
        — never abandons an in-flight handler mid-execution.
        """
        for _stream_name, messages in entries:
            if self._stop_event.is_set():
                return
            for message_id, fields in messages:
                if self._stop_event.is_set():
                    return
                await self._handle_one(client, idem, message_id, fields)

    async def _read_once(
        self, client: aioredis.Redis
    ) -> list[Any]:
        """Single ``XREADGROUP`` call. Returns [] on block timeout."""
        try:
            res = await client.xreadgroup(
                groupname=self._group,
                consumername=self._consumer,
                streams={self._stream: ">"},
                count=_READ_COUNT,
                block=_READ_BLOCK_MS,
            )
        except aioredis.ResponseError as exc:
            # If the group was deleted out from under us, recreate and
            # try again next tick. Surfaces as an empty read.
            logger.warning(
                "XREADGROUP failed on %s/%s: %s; recreating group",
                self._stream,
                self._group,
                exc,
            )
            await self._ensure_group(client)
            return []
        return res or []

    async def _handle_one(
        self,
        client: aioredis.Redis,
        idem: IdempotencyStore,
        message_id: bytes | str,
        fields: dict[Any, Any],
    ) -> None:
        """Process one entry end-to-end with retry/DLQ semantics."""
        msg_id_str = (
            message_id.decode() if isinstance(message_id, bytes) else str(message_id)
        )

        # XADD with bytes payload may return either bytes or str keys
        # depending on the redis-py version / decode_responses setting.
        raw = fields.get(b"payload") or fields.get("payload")
        if raw is None:
            await self._send_to_dlq(
                client, msg_id_str, error="missing payload field", attempts=1
            )
            await client.xack(self._stream, self._group, message_id)
            await client.hdel(self._delivery_counter_key, msg_id_str)
            return

        # Step 1: deserialise. Bad bytes are poison — straight to DLQ.
        try:
            envelope = parse_envelope(raw)
        except EventDeserializationError as exc:
            await self._send_to_dlq(
                client, msg_id_str, error=f"deserialize: {exc}", attempts=1
            )
            await client.xack(self._stream, self._group, message_id)
            await client.hdel(self._delivery_counter_key, msg_id_str)
            return

        # Step 2: idempotency claim. Already-seen → ACK and skip handler.
        first_time = await idem.claim(self._consumer, str(envelope.event_id))
        if not first_time:
            logger.debug(
                "skip duplicate event %s on %s/%s",
                envelope.event_id,
                self._stream,
                self._consumer,
            )
            await client.xack(self._stream, self._group, message_id)
            await client.hdel(self._delivery_counter_key, msg_id_str)
            return

        # Bookkeeping for retry/DLQ. ``HINCRBY`` is atomic so concurrent
        # workers in the same group cannot under-count.
        attempts = int(
            await client.hincrby(self._delivery_counter_key, msg_id_str, 1)
        )

        # Step 3: dispatch. Track as inflight so ``stop()`` can drain.
        try:
            self._inflight = asyncio.create_task(self._handler(envelope))
            await self._inflight
        except Exception as exc:  # noqa: BLE001 — handler may raise anything
            # The handler did not complete, so the idempotency reservation
            # must not convert the later reclaimed delivery into a false
            # success. A crash remains conservatively deduplicated; this
            # release only covers a failure we observed directly.
            await idem.release(self._consumer, str(envelope.event_id))
            await self._on_handler_failure(client, message_id, msg_id_str, exc, attempts)
            return
        finally:
            self._inflight = None

        # Step 4: success → ACK + clear counter.
        await client.xack(self._stream, self._group, message_id)
        await client.hdel(self._delivery_counter_key, msg_id_str)

    async def _on_handler_failure(
        self,
        client: aioredis.Redis,
        message_id: bytes | str,
        msg_id_str: str,
        exc: BaseException,
        attempts: int,
    ) -> None:
        """Decide between retry (let it stay pending) and DLQ.

        Wraps the user's exception in :class:`EventHandlerError` so the
        bus's contract stays narrow even though we don't surface the
        wrapped error to callers (it would hide the original cause).
        """
        wrapped = EventHandlerError(str(exc))
        wrapped.__cause__ = exc

        if attempts >= MAX_DELIVERIES:
            # We've now failed MAX_DELIVERIES times — park.
            await self._send_to_dlq(
                client,
                msg_id_str,
                error=f"{type(exc).__name__}: {exc}",
                attempts=attempts,
            )
            await client.xack(self._stream, self._group, message_id)
            await client.hdel(self._delivery_counter_key, msg_id_str)
            logger.warning(
                "event %s on %s exhausted %d attempts; moved to DLQ",
                msg_id_str,
                self._stream,
                attempts,
            )
            return

        # Otherwise leave pending — Redis will redeliver on next read or
        # when XAUTOCLAIM fires after the idle threshold.
        logger.warning(
            "handler failed for %s on %s (attempt %d/%d): %s",
            msg_id_str,
            self._stream,
            attempts,
            MAX_DELIVERIES,
            exc,
        )

    async def _send_to_dlq(
        self,
        client: aioredis.Redis,
        original_id: str,
        *,
        error: str,
        attempts: int,
    ) -> None:
        """Append a DLQ entry describing a poisoned/failed message."""
        dlq_stream = dlq_for(self._stream)
        body = {
            "original_id": original_id,
            "error": error,
            "attempts": str(attempts),
            "stream": self._stream,
            "group": self._group,
            "consumer": self._consumer,
        }
        await client.xadd(
            dlq_stream,
            {k: v.encode() if isinstance(v, str) else v for k, v in body.items()},
            maxlen=10_000,
            approximate=True,
        )

    async def _reclaim_pending(
        self, client: aioredis.Redis
    ) -> list[tuple[bytes | str, dict[Any, Any]]]:
        """Reclaim entries pending on dead peers via ``XAUTOCLAIM``.

        Walks the cursor at most ``_RECLAIM_MAX_PASSES`` times and
        returns the aggregated list of (message_id, fields) tuples for
        the caller to dispatch. Returning the entries (rather than
        relying on ``XREADGROUP "0"`` to surface them later) is required
        because ``fakeredis`` returns empty for own-pending reads — and
        also more efficient on real Redis (one round-trip instead of
        autoclaim + reread).
        """
        cursor: bytes | str = b"0-0"
        out: list[tuple[bytes | str, dict[Any, Any]]] = []
        seen_ids: set[bytes] = set()
        for _ in range(_RECLAIM_MAX_PASSES):
            try:
                result = await client.xautoclaim(
                    name=self._stream,
                    groupname=self._group,
                    consumername=self._consumer,
                    min_idle_time=RECLAIM_IDLE_MS,
                    start_id=cursor,
                    count=_READ_COUNT,
                )
            except aioredis.ResponseError as exc:
                logger.debug("XAUTOCLAIM not supported / failed: %s", exc)
                return out
            if not result:
                return out
            # redis-py returns (next_cursor, claimed_messages[, deleted_ids]).
            next_cursor = result[0]
            claimed = result[1] if len(result) >= 2 else []
            new_in_batch = 0
            for message_id, fields in claimed:
                # Dedupe: fakeredis can return the same id on consecutive
                # passes when ``start_id`` equals an existing entry.
                key = message_id if isinstance(message_id, bytes) else str(message_id).encode()
                if key in seen_ids:
                    continue
                seen_ids.add(key)
                out.append((message_id, fields))
                new_in_batch += 1
            if new_in_batch == 0:
                return out
            if next_cursor in (b"0-0", "0-0", 0):
                return out
            # Defensive: if the cursor didn't advance, stop to avoid an
            # infinite loop on a misbehaving server.
            if next_cursor == cursor:
                return out
            cursor = next_cursor
        return out

    async def _ensure_group(self, client: aioredis.Redis) -> None:
        """Create the consumer group if it doesn't exist (idempotent).

        ``id="0"`` makes the group deliver every entry already in the
        stream when it's first created. ``"$"`` would skip historical
        events — wrong for an event bus where the producer might publish
        before the first consumer subscribes (very common during cold
        starts and during tests).
        """
        try:
            await client.xgroup_create(
                name=self._stream,
                groupname=self._group,
                id="0",
                mkstream=True,
            )
        except aioredis.ResponseError as exc:
            # BUSYGROUP means it already exists — fine.
            if "BUSYGROUP" not in str(exc):
                raise

    async def _get_client(self) -> aioredis.Redis:
        if self._client is None:
            self._client = aioredis.from_url(
                self._redis_url,
                decode_responses=False,
            )
            self._owns_client = True
        return self._client

    def _get_idempotency(self, client: aioredis.Redis) -> IdempotencyStore:
        if self._idem is None:
            self._idem = IdempotencyStore(client)
        return self._idem

    async def _close_client(self) -> None:
        client, self._client = self._client, None
        if client is not None:
            with contextlib.suppress(Exception):
                await client.aclose()


# Hard cap on XAUTOCLAIM passes — if Redis keeps returning a non-zero
# cursor for some pathological reason we don't want to spin forever.
_RECLAIM_MAX_PASSES: int = 10


__all__ = ["EventConsumer", "MAX_DELIVERIES", "RECLAIM_IDLE_MS"]
