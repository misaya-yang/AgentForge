"""``IdempotencyStore`` — per-consumer dedupe of ``event_id``.

Why this exists: Redis Streams give *at-least-once* delivery. A
consumer crash between "handler returned" and "XACK" replays the
same message after restart. Most handlers are idempotent in spirit
but not in implementation (e.g. inserting a row, charging a credit
counter), so we put a small lookaside in front of the handler.

Algorithm: ``SET key NX EX ttl`` where the key is
``{prefix}:{consumer}:{event_id}``.

- If the SET succeeds (NX), this is the first time we've seen the
  ``event_id`` *for this consumer* — return ``True``, run the handler.
- If the SET fails, another worker (or a previous incarnation of this
  worker) already claimed it — return ``False``, the consumer skips
  the handler call but still ACKs.

Per-consumer scoping (``{consumer}`` in the key) is important: the
same event may legitimately be processed by multiple consumer groups
(billing aggregator AND observability), and each must see it exactly
once *for itself*.

TTL: events older than ``ttl_seconds`` (default 24 h) are unlikely to
replay because Redis will have trimmed them from the stream by then.
A short TTL keeps the lookaside table from growing without bound.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    import redis.asyncio as aioredis


class IdempotencyStore:
    """Lookaside dedupe table backed by Redis SET-NX-EX."""

    def __init__(
        self,
        redis: aioredis.Redis,
        *,
        prefix: str = "events:idem",
        ttl_seconds: int = 86_400,
    ) -> None:
        """
        Parameters
        ----------
        redis
            Async Redis client. The store does **not** own this client —
            the caller (typically ``EventConsumer``) is responsible for
            its lifetime. This avoids double-close in setups where the
            bus and consumer share a connection pool.
        prefix
            Key prefix; useful in shared Redis deployments. Joined with
            consumer + event_id by ``:``.
        ttl_seconds
            Expiration window. Should comfortably exceed the longest
            stream retention so a true replay still hits the cache.
        """
        self._redis = redis
        self._prefix = prefix
        self._ttl = ttl_seconds

    async def claim(self, consumer: str, event_id: str) -> bool:
        """Atomically claim ``event_id`` for ``consumer``.

        Returns
        -------
        bool
            ``True`` if this is the first time ``consumer`` has seen
            ``event_id`` (caller should run the handler). ``False`` if
            already claimed (caller should skip but still ACK).
        """
        key = self._key(consumer, event_id)
        # ``set(..., nx=True, ex=ttl)`` returns ``True`` on first set,
        # ``None`` (== falsy) when the key already existed.
        result = await self._redis.set(key, b"1", nx=True, ex=self._ttl)
        return bool(result)

    async def release(self, consumer: str, event_id: str) -> None:
        """Release a claim when the handler did not complete successfully.

        A claim protects the ACK-after-success path from duplicate delivery.
        Retaining it after a known handler failure would instead turn a
        redelivery into a skipped-and-ACKed message, preventing retries and
        the DLQ policy from ever running.
        """
        await self._redis.delete(self._key(consumer, event_id))

    def _key(self, consumer: str, event_id: str) -> str:
        return f"{self._prefix}:{consumer}:{event_id}"


__all__ = ["IdempotencyStore"]
