"""PRD T3 item 1: the retrieval query boundary refuses mixed-model queries.

The serving binding (dataset_collection_bindings) is the authoritative
indirection layer for which embedding generation serves a dataset. The query
embedder is resolved from the datasets row, and cutover flips row + binding
atomically — so a divergence means the query would embed with a different
generation than the one indexed in the serving collection, which returns
noise, not answers. These offline tests pin:

* a known identity mismatch (provider, model, or dimension) is refused with
  ValidationFailedError before any embed/search call happens;
* a matching binding, a legacy unbound dataset, and a version-store outage
  all keep retrieving (the guard degrades open; only a *known* mismatch is
  refused — retrieval availability beats versioning telemetry);
* a binding whose recorded identity is empty (pre-T3 registration) rejects
  nothing;
* when the binding's collection name diverges from the row, the query
  follows the binding — the indirection layer is what actually serves.
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge import retrieval_service
from knowledge_service.services.knowledge.retrieval_service import RetrievalService

ROW_IDENTITY = {
    "embedding_provider": "local",
    "embedding_model": "hash-384",
    "embedding_dimension": 3,
}


class FakeEmbedder:
    dimension = 3

    def __init__(self) -> None:
        self.embed_calls = 0

    async def embed_query(self, _query: str) -> list[float]:
        self.embed_calls += 1
        return [0.1, 0.2, 0.3]


class FakeVersionStore:
    def __init__(self, binding: dict[str, Any] | None = None, *, fail: bool = False) -> None:
        self.binding = binding
        self.fail = fail
        self.lookups: list[str] = []

    async def get_serving_binding(self, dataset_id: str) -> dict[str, Any] | None:
        self.lookups.append(dataset_id)
        if self.fail:
            raise RuntimeError("pg pool is down")
        return self.binding


class FakeVectorStore:
    def __init__(self) -> None:
        self.search_calls: list[dict[str, Any]] = []

    async def ping(self, **_kwargs: Any) -> bool:
        return True

    async def require_collection_readable(self, *_args: Any, **_kwargs: Any) -> dict[str, Any]:
        return {"tenant_id": "tenant-a", "dataset_id": "kb-demo"}

    async def ensure_collection(self, **_kwargs: Any) -> str:
        return "kb_created"

    async def search(self, **kwargs: Any) -> list[Any]:
        self.search_calls.append(kwargs)
        return [
            SimpleNamespace(
                point_id="seg-1",
                score=0.9,
                payload={
                    "segment_id": "seg-1",
                    "document_id": "doc-1",
                    "text": "dense result",
                },
            )
        ]


def _dataset() -> dict[str, Any]:
    return {
        "dataset_id": "kb-demo",
        "tenant_id": "tenant-a",
        "collection_name": "kb_existing",
        "index_config": {},
        **ROW_IDENTITY,
    }


def _service(
    monkeypatch: pytest.MonkeyPatch,
    *,
    binding: dict[str, Any] | None = None,
    store_fail: bool = False,
    migration_service: Any | None = "auto",
) -> tuple[RetrievalService, FakeVectorStore, FakeEmbedder]:
    embedder = FakeEmbedder()

    async def get_embedder(_config: Any, dimension: int | None = None) -> FakeEmbedder:
        return embedder

    monkeypatch.setattr(retrieval_service, "get_cached_embedder", get_embedder)

    async def require_dataset_access(_user: Any, dataset_id: str, required: str = "viewer") -> dict[str, Any]:
        return _dataset()

    async def filter_active_segment_ids(*, segment_ids: Any, **_kwargs: Any) -> Any:
        return set(segment_ids)

    vector_store = FakeVectorStore()
    service = RetrievalService(
        SimpleNamespace(
            knowledge=SimpleNamespace(
                retrieval_query_max_concurrency=2,
                dashscope=SimpleNamespace(api_key=None),
            )
        ),
        SimpleNamespace(filter_active_segment_ids=filter_active_segment_ids),
    )
    service.vector_store = vector_store

    if migration_service == "auto":
        store = FakeVersionStore(binding, fail=store_fail)
        migration_service = SimpleNamespace(store=store)
    ks = SimpleNamespace(
        require_dataset_access=require_dataset_access,
        _resolve_fusion_config=lambda **_kwargs: {
            "method": "rrf",
            "dense_weight": 0.5,
            "bm25_weight": 0.5,
            "rrf_k": 60,
        },
        _is_multimodal_dataset=lambda _dataset: False,
        _resolve_embedding_config=lambda **_kwargs: SimpleNamespace(),
        _should_apply_score_threshold=lambda _mode: False,
        _filter_candidates_by_metadata=lambda candidates, *_args: candidates,
        _get_presigned_image_url=lambda *_args: None,
        _normalize_local_image_url=lambda raw_url, _segment_id: raw_url,
    )
    # Legacy KnowledgeService instances have no version store at all; the
    # guard must treat that exactly like an unbound dataset.
    if migration_service is not None:
        ks.embedding_migration_service = migration_service
    service._ks = ks
    return service, vector_store, embedder


async def _retrieve(service: RetrievalService, mode: str = "dense"):
    return await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="query",
        mode=mode,
        rerank=False,
        mmr=False,
    )


@pytest.mark.asyncio
async def test_matching_binding_retrieves_normally(monkeypatch: pytest.MonkeyPatch) -> None:
    binding = {"collection_name": "kb_existing", **ROW_IDENTITY}
    service, vector_store, embedder = _service(monkeypatch, binding=binding)
    results, meta = await _retrieve(service)
    assert [result.segment_id for result in results] == ["seg-1"]
    assert meta["collection_name"] == "kb_existing"
    assert embedder.embed_calls == 1
    assert vector_store.search_calls


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "binding_identity",
    [
        # provider mismatch
        {"embedding_provider": "dashscope", "embedding_model": "hash-384", "embedding_dimension": 3},
        # model mismatch
        {"embedding_provider": "local", "embedding_model": "text-embedding-v4", "embedding_dimension": 3},
        # dimension mismatch
        {"embedding_provider": "local", "embedding_model": "hash-384", "embedding_dimension": 1024},
        # model-version mismatch
        {
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_model_version": "generation-2",
            "embedding_dimension": 3,
        },
    ],
)
async def test_mismatched_binding_refuses_before_any_embed_or_search(
    monkeypatch: pytest.MonkeyPatch, binding_identity: dict[str, Any]
) -> None:
    binding = {"collection_name": "kb_existing", **binding_identity}
    service, vector_store, embedder = _service(monkeypatch, binding=binding)
    with pytest.raises(ValidationFailedError, match="does not match"):
        await _retrieve(service)
    # Nothing measurable may run against the wrong generation.
    assert embedder.embed_calls == 0
    assert vector_store.search_calls == []


@pytest.mark.asyncio
async def test_mismatch_refused_for_pure_bm25_too(monkeypatch: pytest.MonkeyPatch) -> None:
    # A row/binding divergence is a corrupted serving state; refusing every
    # mode keeps the anomaly loud instead of half-serving stale lexical index.
    binding = {
        "collection_name": "kb_existing",
        "embedding_provider": "dashscope",
        "embedding_model": "text-embedding-v4",
        "embedding_dimension": 1024,
    }
    service, _vector_store, _embedder = _service(monkeypatch, binding=binding)
    with pytest.raises(ValidationFailedError, match="does not match"):
        await _retrieve(service, mode="bm25")


@pytest.mark.asyncio
async def test_legacy_unbound_dataset_retrieves_without_guard(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, _vector_store, embedder = _service(monkeypatch, binding=None)
    results, _meta = await _retrieve(service)
    assert [result.segment_id for result in results] == ["seg-1"]
    assert embedder.embed_calls == 1


@pytest.mark.asyncio
async def test_no_migration_service_is_legacy_passthrough(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service, _vector_store, _embedder = _service(
        monkeypatch, migration_service=None
    )
    results, _meta = await _retrieve(service)
    assert [result.segment_id for result in results] == ["seg-1"]


@pytest.mark.asyncio
async def test_version_store_outage_degrades_open(monkeypatch: pytest.MonkeyPatch) -> None:
    # Retrieval availability beats versioning telemetry: a down store logs
    # and falls back to the datasets row instead of failing every query.
    service, _vector_store, embedder = _service(
        monkeypatch, binding=None, store_fail=True
    )
    results, _meta = await _retrieve(service)
    assert [result.segment_id for result in results] == ["seg-1"]
    assert embedder.embed_calls == 1


@pytest.mark.asyncio
async def test_binding_without_recorded_identity_rejects_nothing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # Pre-T3 registrations carry no generation identity; the guard tolerates
    # them until the next migration stamps one.
    binding = {"collection_name": "kb_existing"}
    service, _vector_store, _embedder = _service(monkeypatch, binding=binding)
    results, _meta = await _retrieve(service)
    assert [result.segment_id for result in results] == ["seg-1"]


@pytest.mark.asyncio
async def test_divergent_binding_collection_wins_over_row(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # Same identity, different pointer (a mid-cutover race): the binding is
    # the indirection layer, so the query follows it.
    binding = {"collection_name": "kb_new_generation", **ROW_IDENTITY}
    service, vector_store, _embedder = _service(monkeypatch, binding=binding)
    results, meta = await _retrieve(service)
    assert [result.segment_id for result in results] == ["seg-1"]
    assert meta["collection_name"] == "kb_new_generation"
    assert vector_store.search_calls
    assert all(
        call.get("collection_name") == "kb_new_generation"
        for call in vector_store.search_calls
    )


@pytest.mark.asyncio
async def test_cutover_between_dataset_and_binding_reads_retries_new_generation(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Old dataset row + new binding is a transient cutover snapshot.

    The mixed-model guard stays strict, but the expanded generation fence
    recognizes the committed pointer move and restarts the whole entrypoint;
    no embed/search runs for the split snapshot.
    """

    new_identity = {
        "embedding_provider": "local",
        "embedding_model": "hash-768",
        "embedding_model_version": "generation-2",
        "embedding_dimension": 3,
    }
    new_binding = {
        "collection_name": "kb_new_generation",
        **new_identity,
    }
    service, vector_store, embedder = _service(
        monkeypatch,
        binding=new_binding,
    )
    old_dataset = _dataset()
    new_dataset = {
        **old_dataset,
        "collection_name": "kb_new_generation",
        **new_identity,
    }
    access_calls = 0

    async def require_dataset_access(
        _user: Any,
        _dataset_id: str,
        required: str = "viewer",
    ) -> dict[str, Any]:
        nonlocal access_calls
        assert required == "viewer"
        access_calls += 1
        return old_dataset if access_calls == 1 else new_dataset

    service._ks.require_dataset_access = require_dataset_access
    results, meta = await _retrieve(service)

    assert [result.segment_id for result in results] == ["seg-1"]
    assert meta["collection_name"] == "kb_new_generation"
    assert embedder.embed_calls == 1
    assert [call["collection_name"] for call in vector_store.search_calls] == [
        "kb_new_generation"
    ]
    assert access_calls >= 3
