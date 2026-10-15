"""Retrieval result caching with TTL and true LRU eviction.

Eviction is least-recently-*used*, not least-recently-*inserted*: a cache hit
refreshes the entry's recency (PRD T2-#7; the old FIFO pop order under high
query-concurrency evicted hot entries while cold ones survived).
"""

from __future__ import annotations

import asyncio
import copy
import hashlib
import json
import time
from collections import OrderedDict
from typing import Any

from .retrieval_service import RetrieveResult


class CacheManager:
    """In-memory cache for retrieval results with TTL + LRU eviction."""

    def __init__(self, ttl_seconds: int = 0, max_entries: int = 2000):
        self._ttl_seconds = max(ttl_seconds, 0)
        self._max_entries = max_entries
        self._cache: OrderedDict[str, tuple[float, list[RetrieveResult], dict[str, Any]]] = (
            OrderedDict()
        )
        self._lock = asyncio.Lock()

    @staticmethod
    def clone_results(results: list[RetrieveResult]) -> list[RetrieveResult]:
        """Deep-copy a list of RetrieveResult to prevent mutation of cached data."""
        cloned: list[RetrieveResult] = []
        for item in results:
            cloned.append(
                RetrieveResult(
                    segment_id=item.segment_id,
                    document_id=item.document_id,
                    score=item.score,
                    text=item.text,
                    metadata=copy.deepcopy(item.metadata or {}),
                    content_type=item.content_type,
                    image_url=item.image_url,
                    vlm_description=item.vlm_description,
                    associated_images=tuple(item.associated_images or ()),
                )
            )
        return cloned

    @staticmethod
    def compute_fingerprint(payload: dict[str, Any]) -> str:
        """Compute a short SHA-256 fingerprint of a retrieval query payload."""
        serialized = json.dumps(payload, ensure_ascii=False, sort_keys=True, default=str)
        return hashlib.sha256(serialized.encode("utf-8")).hexdigest()[:20]

    async def get(self, cache_key: str) -> tuple[list[RetrieveResult], dict[str, Any]] | None:
        """Return cached results if present and not expired, else None."""
        if self._ttl_seconds <= 0:
            return None
        now = time.monotonic()
        async with self._lock:
            cached = self._cache.get(cache_key)
            if not cached:
                return None
            expires_at, cached_results, cached_meta = cached
            if now >= expires_at:
                self._cache.pop(cache_key, None)
                return None
            # A hit is a use: keep the entry ahead of the eviction queue.
            self._cache.move_to_end(cache_key)
        return self.clone_results(cached_results), copy.deepcopy(cached_meta)

    async def set(
        self,
        cache_key: str,
        results: list[RetrieveResult],
        meta: dict[str, Any],
    ) -> None:
        """Store results in cache with TTL."""
        if self._ttl_seconds <= 0:
            return
        async with self._lock:
            if cache_key in self._cache:
                # Update in place: a rewrite is also a use, and it never
                # grows the cache, so it must not evict anything.
                self._cache.move_to_end(cache_key)
            else:
                while len(self._cache) >= self._max_entries:
                    self._cache.popitem(last=False)  # evict least recently used
            self._cache[cache_key] = (
                time.monotonic() + self._ttl_seconds,
                self.clone_results(results),
                copy.deepcopy(meta),
            )
