"""Seen-request-id replay protection protocol (I/O-free).

Signed-payload verification (capability proofs, Agent Runtime envelopes,
``X-Gateway-Secret``) needs a bounded replay window.  The ``ReplayStore``
protocol is the contract; ``InMemoryReplayStore`` is the process-local
default.  Multi-replica deployments swap in a shared backend (e.g. the
Redis-backed implementation that lives with the gateway-secret signer in
``ai_gateway_core.auth.gateway_secret``) — same protocol, no protocol code
in this package touches any backend itself.
"""

from __future__ import annotations

import threading
import time
from collections import OrderedDict
from typing import Protocol


def _epoch_ms() -> int:
    return int(time.time() * 1000)


class ReplayStore(Protocol):
    """Contract for a seen-request-id store with TTL."""

    def seen_or_record(self, request_id: str, ttl_ms: int) -> bool:
        """Record ``request_id``. Return True if it was already present."""
        ...


class InMemoryReplayStore:
    """LRU-bounded seen-ids store.

    Safe for concurrent access within a single process. Entries expire
    after their TTL; capacity is bounded to prevent unbounded growth.
    """

    __slots__ = ("_seen", "_lock", "_capacity")

    def __init__(self, capacity: int = 10_000) -> None:
        self._seen: OrderedDict[str, int] = OrderedDict()
        self._lock = threading.Lock()
        self._capacity = capacity

    def seen_or_record(self, request_id: str, ttl_ms: int) -> bool:
        now_ms = _epoch_ms()
        with self._lock:
            self._evict_expired(now_ms)
            existing = self._seen.get(request_id)
            if existing is not None and existing > now_ms:
                return True
            self._seen[request_id] = now_ms + ttl_ms
            self._seen.move_to_end(request_id)
            while len(self._seen) > self._capacity:
                self._seen.popitem(last=False)
            return False

    def _evict_expired(self, now_ms: int) -> None:
        stale: list[str] = []
        for rid, expires in self._seen.items():
            if expires <= now_ms:
                stale.append(rid)
            else:
                break
        for rid in stale:
            self._seen.pop(rid, None)


__all__ = [
    "InMemoryReplayStore",
    "ReplayStore",
]
