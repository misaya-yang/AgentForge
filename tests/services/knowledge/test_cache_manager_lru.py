"""CacheManager eviction is true LRU (PRD T2-#7).

The old implementation's docstring said "LRU" but ``set()`` popped
``next(iter(self._cache))`` — the oldest *inserted* key — and ``get()`` never
refreshed recency. Under sustained query concurrency that evicted hot entries
while cold ones survived to TTL. These tests pin the recency semantics.
"""

from __future__ import annotations

from knowledge_service.services.knowledge.cache_manager import CacheManager
from knowledge_service.services.knowledge.retrieval_service import RetrieveResult


def _result(seg_id: str) -> RetrieveResult:
    return RetrieveResult(segment_id=seg_id, document_id="doc", score=1.0, text=seg_id, metadata={})


async def _put(cache: CacheManager, key: str, seg_id: str) -> None:
    await cache.set(key, [_result(seg_id)], {"tag": key})


async def test_hit_refreshes_recency_and_eviction_is_least_recently_used() -> None:
    cache = CacheManager(ttl_seconds=60, max_entries=2)
    await _put(cache, "a", "seg-a")
    await _put(cache, "b", "seg-b")

    assert await cache.get("a") is not None  # a becomes the most recently used

    await _put(cache, "c", "seg-c")  # must evict b, the least recently used

    assert await cache.get("a") is not None, "hit entry must survive eviction"
    assert await cache.get("c") is not None
    assert await cache.get("b") is None, "LRU entry must be the one evicted"


async def test_rewrite_of_existing_key_refreshes_recency_without_growing() -> None:
    cache = CacheManager(ttl_seconds=60, max_entries=2)
    await _put(cache, "a", "seg-a-v1")
    await _put(cache, "b", "seg-b")
    await _put(cache, "a", "seg-a-v2")  # update, not insert: b is now the LRU

    assert len(cache._cache) == 2  # noqa: SLF001 - eviction accounting is the subject

    await _put(cache, "c", "seg-c")

    assert await cache.get("a") is not None
    assert await cache.get("c") is not None
    assert await cache.get("b") is None
    results, _meta = await cache.get("a")
    assert str(results[0].segment_id) == "seg-a-v2"  # newest value kept


async def test_expired_entry_is_dropped_on_access() -> None:
    cache = CacheManager(ttl_seconds=1, max_entries=8)
    await _put(cache, "a", "seg-a")
    # Expire it deterministically instead of sleeping on the TTL.
    expires_at, results, meta = cache._cache["a"]  # noqa: SLF001
    cache._cache["a"] = (expires_at - 2, results, meta)  # noqa: SLF001

    assert await cache.get("a") is None
    assert "a" not in cache._cache  # noqa: SLF001


async def test_ttl_disabled_short_circuits_get_and_set() -> None:
    cache = CacheManager(ttl_seconds=0, max_entries=2)
    await _put(cache, "a", "seg-a")
    assert await cache.get("a") is None
    assert len(cache._cache) == 0  # noqa: SLF001
