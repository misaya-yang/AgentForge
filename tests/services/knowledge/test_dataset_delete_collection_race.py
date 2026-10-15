"""Regression: dataset cleanup sweep must survive concurrently-removed collections.

E2E parallel workers intermittently hit DELETE /datasets/{id} -> 500 because
``delete_dataset_collections`` enumerated collections via ``get_collections``
and then per-collection ``get_collection``/``count`` calls 404'd when another
worker deleted the collection in between. A vanished collection is an
idempotent success for removal; only genuine faults must still propagate.
"""

from types import SimpleNamespace

import pytest
from knowledge_service.services.knowledge import vector_store as vs_module
from knowledge_service.services.knowledge.vector_store import (
    VectorStore,
    VectorStoreError,
)


class _QdrantStatusError(Exception):
    def __init__(self, status_code: int):
        super().__init__(f"status={status_code}")
        self.status_code = status_code


def _store(monkeypatch, client):
    monkeypatch.setattr(vs_module, "AsyncQdrantClient", lambda **_kwargs: client)
    return VectorStore(url="http://localhost:6333")


@pytest.mark.asyncio
async def test_sweep_skips_collections_that_vanish_mid_flight(monkeypatch):
    class DummyClient:
        async def get_collections(self):
            return SimpleNamespace(
                collections=[
                    SimpleNamespace(name="kb_other_gone"),
                    SimpleNamespace(name="kb_kb_target_384"),
                ]
            )

        async def get_collection(self, _collection_name):
            if _collection_name == "kb_other_gone":
                # Concurrency window: listed, then deleted before we inspected it.
                raise _QdrantStatusError(404)
            return SimpleNamespace(
                config=SimpleNamespace(strict_mode_config=None, metadata={}),
                payload_schema={},
            )

        async def count(self, *_args, **_kwargs):
            raise _QdrantStatusError(404)

        async def close(self):
            return None

    vs = _store(monkeypatch, DummyClient())
    touched = await vs.delete_dataset_collections(
        tenant_id="t1",
        dataset_id="target",
        authoritative_collection_names=("kb_kb_target_384",),
        lifecycle_lease_held=True,
    )
    assert touched == []


@pytest.mark.asyncio
async def test_sweep_still_propagates_non_404_failures(monkeypatch):
    class DummyClient:
        async def get_collections(self):
            return SimpleNamespace(collections=[SimpleNamespace(name="kb_kb_target_384")])

        async def get_collection(self, _collection_name):
            raise _QdrantStatusError(503)

        async def close(self):
            return None

    vs = _store(monkeypatch, DummyClient())
    with pytest.raises(VectorStoreError):
        await vs.delete_dataset_collections(
            tenant_id="t1",
            dataset_id="target",
            authoritative_collection_names=("kb_kb_target_384",),
            lifecycle_lease_held=True,
        )
