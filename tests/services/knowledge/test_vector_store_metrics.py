"""SPO-00 knowledge vector store counter tests.

Drives the shipped ``VectorStore.search`` path with a fake Qdrant client and
asserts the exercisable ``vector_store_metrics.get_collection_calls`` counter.
The same counter backs the SPO-04 gate (≤ 1 get_collection per interactive
retrieve without rerank).
"""

from __future__ import annotations

from types import SimpleNamespace

import pytest
from knowledge_service.services.knowledge import vector_store
from knowledge_service.services.knowledge.vector_store import VectorStore
from knowledge_service.services.knowledge.vector_store_metrics import vector_store_metrics


class _DummyClient:
    def __init__(self) -> None:
        self.get_collection_calls = 0

    async def get_collection(self, _collection_name):
        self.get_collection_calls += 1
        return SimpleNamespace(
            config=SimpleNamespace(strict_mode_config=None, metadata={}),
            payload_schema={},
        )

    async def query_points(self, **_kwargs):
        return SimpleNamespace(points=[])

    async def close(self):
        return None


@pytest.fixture(autouse=True)
def _reset_metrics() -> None:
    vector_store_metrics.reset()
    yield
    vector_store_metrics.reset()


@pytest.mark.asyncio
async def test_search_counts_one_get_collection_per_retrieve(monkeypatch) -> None:
    client = _DummyClient()
    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: client)
    store = VectorStore(url="http://localhost:6333")

    await store.search(
        collection_name="kb_ds_3",
        query_vector=[0.1, 0.2, 0.3],
        top_k=5,
        tenant_id="tenant-a",
        dataset_id="ds",
    )

    assert client.get_collection_calls == 1
    assert vector_store_metrics.get_collection_calls == 1


@pytest.mark.asyncio
async def test_two_retrieves_count_two_get_collection_calls(monkeypatch) -> None:
    client = _DummyClient()
    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: client)
    store = VectorStore(url="http://localhost:6333")

    for _ in range(2):
        await store.search(
            collection_name="kb_ds_3",
            query_vector=[0.1, 0.2, 0.3],
            top_k=5,
            tenant_id="tenant-a",
            dataset_id="ds",
        )

    # SPO-04 / K1: the collection metadata cache serves the second retrieve,
    # so the wire only sees ONE get_collection (≤1 per retrieve).
    assert client.get_collection_calls == 1
    assert vector_store_metrics.get_collection_calls == 1


@pytest.mark.asyncio
async def test_collection_cache_invalidated_by_metadata_write(monkeypatch) -> None:
    """K1: a metadata-bearing write invalidates the cached collection info."""
    client = _DummyClient()
    monkeypatch.setattr(vector_store, "AsyncQdrantClient", lambda **_kwargs: client)
    store = VectorStore(url="http://localhost:6333")

    for _ in range(2):
        await store.search(
            collection_name="kb_ds_3",
            query_vector=[0.1, 0.2, 0.3],
            top_k=5,
            tenant_id="tenant-a",
            dataset_id="ds",
        )
    assert client.get_collection_calls == 1

    # A collection metadata write invalidates; the next retrieve re-reads.
    store._invalidate_collection_info("kb_ds_3")
    await store.search(
        collection_name="kb_ds_3",
        query_vector=[0.1, 0.2, 0.3],
        top_k=5,
        tenant_id="tenant-a",
        dataset_id="ds",
    )

    assert client.get_collection_calls == 2
    assert vector_store_metrics.get_collection_calls == 2
