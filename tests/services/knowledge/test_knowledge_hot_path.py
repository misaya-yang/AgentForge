from __future__ import annotations

import asyncio
import copy
import time
from types import SimpleNamespace

import pytest
from knowledge_service.persistence.database import DatabaseStorage
from knowledge_service.services.knowledge.cache_manager import CacheManager
from knowledge_service.services.knowledge.ingestion_service import IngestionService
from knowledge_service.services.knowledge.retrieval_service import RetrievalService
from knowledge_service.services.knowledge.vector_store import VectorStore, VectorStoreError


def _dataset() -> dict:
    return {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "content_revision": 7,
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 3,
        "collection_name": "kb_dataset-a_3",
        "index_config": {},
    }


def _interactive_store(*, deadline: float, health_ttl: float = 2.0) -> VectorStore:
    store = object.__new__(VectorStore)
    store.url = "http://qdrant"
    store.timeout_seconds = 30.0
    store.max_retries = 5
    store.retry_base_delay = 0.001
    store.interactive_deadline_seconds = deadline
    store.interactive_max_retries = 2
    store.health_receipt_ttl_seconds = health_ttl
    store._health_success_until = 0.0
    store._health_failure_until = 0.0
    return store


@pytest.mark.asyncio
async def test_standard_retrieve_cache_hit_skips_recall_pipeline() -> None:
    dataset = _dataset()
    cache: dict[str, tuple[list, dict]] = {}
    recall_calls = 0
    collection_checks = 0

    async def require_dataset_access(*_args, **_kwargs):
        return copy.deepcopy(dataset)

    async def get_cached(key):
        return copy.deepcopy(cache.get(key))

    async def set_cached(key, results, meta):
        cache[key] = (copy.deepcopy(results), copy.deepcopy(meta))

    async def retrieve_queries(**_kwargs):
        nonlocal recall_calls
        recall_calls += 1
        return [], {"recall_calls": recall_calls}

    async def require_collection_readable(*_args, **_kwargs):
        nonlocal collection_checks
        collection_checks += 1
        return {"tenant_id": "tenant-a", "dataset_id": "dataset-a"}

    service = object.__new__(RetrievalService)
    service._ks = SimpleNamespace(
        require_dataset_access=require_dataset_access,
        _compute_retrieval_query_fingerprint=CacheManager.compute_fingerprint,
        _get_cached_retrieval=get_cached,
        _set_cached_retrieval=set_cached,
    )
    service.vector_store = SimpleNamespace(
        require_collection_readable=require_collection_readable,
    )
    service._retrieve_queries = retrieve_queries

    user = SimpleNamespace(user_id="user-a")
    _, first_meta = await service.retrieve(user, "dataset-a", "cache me")
    _, second_meta = await service.retrieve(user, "dataset-a", "cache me")

    assert recall_calls == 1
    assert collection_checks == 2
    assert first_meta["retrieval_cache_hit"] is False
    assert second_meta["retrieval_cache_hit"] is True

    dataset["content_revision"] += 1
    _, changed_meta = await service.retrieve(user, "dataset-a", "cache me")
    assert recall_calls == 2
    assert changed_meta["retrieval_cache_hit"] is False


@pytest.mark.asyncio
async def test_interactive_qdrant_retries_one_transient_failure() -> None:
    store = _interactive_store(deadline=1.0)
    attempts = 0

    async def call():
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            raise ConnectionError("temporary outage")
        return "ok"

    assert await store._call(call, interactive=True) == "ok"
    assert attempts == 2


@pytest.mark.asyncio
async def test_interactive_qdrant_deadline_opens_short_circuit() -> None:
    store = _interactive_store(deadline=0.02, health_ttl=1.0)
    attempts = 0

    async def hang():
        nonlocal attempts
        attempts += 1
        await asyncio.sleep(1.0)

    started = time.monotonic()
    with pytest.raises(VectorStoreError, match="timed out|deadline"):
        await store._call(hang, interactive=True)
    assert time.monotonic() - started < 0.2
    attempts_after_failure = attempts

    with pytest.raises(VectorStoreError, match="circuit is open"):
        await store._call(hang, interactive=True)
    assert attempts == attempts_after_failure


@pytest.mark.asyncio
async def test_non_transient_tenant_error_does_not_poison_endpoint_breaker() -> None:
    store = _interactive_store(deadline=1.0, health_ttl=1.0)

    class TenantFilterError(RuntimeError):
        status_code = 400

    async def reject_tenant_a():
        raise TenantFilterError("invalid tenant filter")

    with pytest.raises(VectorStoreError, match="invalid tenant filter"):
        await store._call(reject_tenant_a, interactive=True)

    assert await store._call(lambda: asyncio.sleep(0, result="tenant-b"), interactive=True) == (
        "tenant-b"
    )


@pytest.mark.asyncio
async def test_nested_qdrant_reads_share_one_absolute_deadline() -> None:
    store = _interactive_store(deadline=0.03, health_ttl=0.0)
    token = store.begin_interactive_budget()
    started = time.monotonic()
    try:
        await store._call(lambda: asyncio.sleep(0.015), interactive=True)
        with pytest.raises(VectorStoreError, match="timed out|deadline"):
            await store._call(lambda: asyncio.sleep(1.0), interactive=True)
    finally:
        store.end_interactive_budget(token)

    assert time.monotonic() - started < 0.06


class _FetchErrorPool:
    def __init__(self, error: Exception) -> None:
        self.error = error

    def acquire(self):
        pool = self

        class _Acquire:
            async def __aenter__(self):
                return self

            async def __aexit__(self, *_args):
                return False

            async def fetch(self, *_args):
                raise pool.error

        return _Acquire()


class _UndefinedTextSearchColumnError(RuntimeError):
    sqlstate = "42703"
    column_name = "text_search"


@pytest.mark.asyncio
async def test_fts_only_falls_back_for_verified_legacy_column() -> None:
    storage = object.__new__(DatabaseStorage)
    storage._pool = _FetchErrorPool(_UndefinedTextSearchColumnError("missing text_search"))

    assert (
        await storage._search_segments_fts(
            "dataset-a",
            "tenant-a",
            ["query"],
            None,
            None,
            None,
            5,
            None,
        )
        is None
    )

    storage._pool = _FetchErrorPool(RuntimeError("database unavailable"))
    with pytest.raises(RuntimeError, match="database unavailable"):
        await storage._search_segments_fts(
            "dataset-a",
            "tenant-a",
            ["query"],
            None,
            None,
            None,
            5,
            None,
        )


@pytest.mark.asyncio
async def test_ingestion_cpu_work_uses_dedicated_bounded_executor() -> None:
    settings = SimpleNamespace(
        knowledge=SimpleNamespace(worker_concurrency=1),
    )
    service = IngestionService(settings, SimpleNamespace(), SimpleNamespace())
    try:
        thread_name = await service._run_cpu(
            lambda: __import__("threading").current_thread().name
        )
        assert thread_name.startswith("knowledge-ingestion-cpu")
    finally:
        await service.close()
