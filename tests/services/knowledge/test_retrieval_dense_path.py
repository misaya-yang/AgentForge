from types import SimpleNamespace

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.persistence.datasets import IndexLeaseUnavailableError
from knowledge_service.services.knowledge import retrieval_service
from knowledge_service.services.knowledge.retrieval_service import RetrievalService


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("collection_name", "expect_rejection"),
    [("kb_existing", False), ("", True)],
)
async def test_dense_retrieval_requires_persisted_authoritative_collection(
    monkeypatch,
    collection_name: str,
    expect_rejection: bool,
):
    class FakeEmbedder:
        dimension = 3

        async def embed_query(self, _query):
            return [0.1, 0.2, 0.3]

    class FakeVectorStore:
        def __init__(self):
            self.ensure_calls = 0

        async def ping(self, **_kwargs):
            return True

        async def require_collection_readable(self, *_args, **_kwargs):
            return {"tenant_id": "tenant-a", "dataset_id": "kb-demo"}

        async def ensure_collection(self, **_kwargs):
            self.ensure_calls += 1
            return "kb_created"

        async def search(self, **_kwargs):
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

    async def require_dataset_access(_user, dataset_id, required="viewer"):
        return {
            "dataset_id": dataset_id,
            "tenant_id": "tenant-a",
            "collection_name": collection_name,
            "index_config": {},
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_dimension": 3,
        }

    async def get_presigned_image_url(_raw_url, _segment_id):
        return None

    async def get_embedder(_config, dimension=None):
        return FakeEmbedder()

    async def filter_active_segment_ids(*, segment_ids, **_kwargs):
        return set(segment_ids)

    monkeypatch.setattr(retrieval_service, "get_cached_embedder", get_embedder)

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
    service._ks = SimpleNamespace(
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
        _get_presigned_image_url=get_presigned_image_url,
        _normalize_local_image_url=lambda raw_url, _segment_id: raw_url,
    )

    if expect_rejection:
        with pytest.raises(ValidationFailedError, match="persisted Qdrant collection"):
            await service.retrieve(
                user=SimpleNamespace(),
                dataset_id="kb-demo",
                query="query",
                mode="dense",
                rerank=False,
                mmr=False,
            )
        assert vector_store.ensure_calls == 0
        return

    results, meta = await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="query",
        mode="dense",
        rerank=False,
        mmr=False,
    )

    assert [result.segment_id for result in results] == ["seg-1"]
    assert vector_store.ensure_calls == 0
    assert meta["collection_name"] == collection_name
    assert meta["timings_ms"]["dense_prepare_ms"] >= 0
    assert meta["timings_ms"]["dense_search_ms"] >= 0


@pytest.mark.asyncio
@pytest.mark.parametrize("entrypoint", ["normal", "batch"])
async def test_retrieval_entrypoint_discards_deletion_generation_overlap(
    monkeypatch,
    entrypoint: str,
) -> None:
    authority_checks: list[tuple[str, str | None, str | None]] = []
    search_calls = 0
    dataset = {
        "dataset_id": "kb-demo",
        "tenant_id": "tenant-a",
        "content_revision": 5,
        "collection_name": "kb-existing",
        "index_config": {},
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 3,
    }

    class FakeEmbedder:
        dimension = 3

        async def embed_query(self, _query):
            return [0.1, 0.2, 0.3]

    class RacingVectorStore:
        async def ping(self, **_kwargs):
            return True

        async def require_collection_readable(
            self,
            collection_name: str,
            *,
            tenant_id: str | None,
            dataset_id: str | None,
        ):
            authority_checks.append((collection_name, tenant_id, dataset_id))
            return {"tenant_id": tenant_id or "", "dataset_id": dataset_id or ""}

        async def search(self, **_kwargs):
            nonlocal search_calls
            search_calls += 1
            dataset["content_revision"] += 2
            return []

    async def require_dataset_access(_user, _dataset_id, required="viewer"):
        assert required == "viewer"
        return dict(dataset)

    async def get_embedder(_config, dimension=None):
        return FakeEmbedder()

    monkeypatch.setattr(retrieval_service, "get_cached_embedder", get_embedder)
    service = RetrievalService(
        SimpleNamespace(
            knowledge=SimpleNamespace(
                retrieval_query_max_concurrency=2,
                dashscope=SimpleNamespace(api_key=None),
            )
        ),
        SimpleNamespace(),
    )
    service.vector_store = RacingVectorStore()
    service._ks = SimpleNamespace(
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

    with pytest.raises(IndexLeaseUnavailableError, match="publication is still in progress"):
        if entrypoint == "normal":
            await service.retrieve(
                user=SimpleNamespace(),
                dataset_id="kb-demo",
                query="query",
                mode="dense",
                rerank=False,
                mmr=False,
            )
        elif entrypoint == "batch":
            await service.retrieve_batch(
                user=SimpleNamespace(),
                dataset_id="kb-demo",
                queries=["query"],
                mode="dense",
                rerank=False,
                mmr=False,
            )
    assert authority_checks
    assert all(
        check == ("kb-existing", "tenant-a", "kb-demo")
        for check in authority_checks
    )
    assert search_calls >= 1
    assert dataset["content_revision"] > 5
