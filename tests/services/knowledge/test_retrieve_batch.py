from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest
from knowledge_service.core.exceptions import PermissionDeniedError, ValidationFailedError
from knowledge_service.services.knowledge.knowledge_service import KnowledgeService
from knowledge_service.services.knowledge.retrieval import MMRPick
from knowledge_service.services.knowledge.retrieval import (
    reciprocal_rank_fusion as real_reciprocal_rank_fusion,
)
from knowledge_service.services.knowledge.retrieval_service import RetrievalService
from knowledge_service.services.knowledge.text_reranker import RerankResult
from knowledge_service.services.knowledge.vector_store import CollectionReadAuthorityError

from tests.services.knowledge.retrieve_batch_support import (
    FakeDatabase as FakeDatabase,
)
from tests.services.knowledge.retrieve_batch_support import (
    make_bm25_service as _make_bm25_service,
)


def _mock_result(query: str, metadata: dict | None = None) -> SimpleNamespace:
    return SimpleNamespace(
        segment_id=f"seg-{query}",
        document_id=f"doc-{query}",
        score=0.9,
        text=f"result for {query}",
        metadata=metadata or {},
        content_type="text",
        image_url=None,
        vlm_description=None,
    )


def _readable_dataset(dataset_id: str) -> dict:
    return {
        "dataset_id": dataset_id,
        "tenant_id": "tenant-a",
        "collection_name": "kb-demo-collection",
        "index_config": {},
    }


class ReadableVectorStore:
    async def require_collection_readable(self, *_args, **_kwargs):
        return {"tenant_id": "tenant-a", "dataset_id": "kb-demo"}


class RejectedReadAuthorityStore:
    def __init__(self, reason: str):
        self.reason = reason

    async def require_collection_readable(self, *_args, **_kwargs):
        raise CollectionReadAuthorityError(self.reason)


class RecallProbe:
    def __init__(self):
        self.bm25_started = asyncio.Event()
        self.dense_started = asyncio.Event()
        self.access_calls = 0


class FakeHybridDatabase:
    def __init__(self, probe: RecallProbe):
        self.probe = probe
        self.calls = []

    async def search_segments_text(self, **kwargs):
        self.calls.append(kwargs)
        self.probe.bm25_started.set()
        await asyncio.wait_for(self.probe.dense_started.wait(), timeout=1)
        is_rewrite = "rewrite" in kwargs["terms"]
        query_word = "rewrite" if is_rewrite else "original"
        unique_suffix = "rewrite" if is_rewrite else "original"
        return [
            {
                "segment_id": "shared",
                "dataset_id": "kb-demo",
                "document_id": "doc-shared",
                "text": f"{query_word} shared answer",
                "metadata": {"kind": "shared"},
            },
            {
                "segment_id": f"bm25-{unique_suffix}",
                "dataset_id": "kb-demo",
                "document_id": f"doc-bm25-{unique_suffix}",
                "text": f"{query_word} lexical answer",
                "metadata": {"kind": "lexical"},
            },
        ]

    async def filter_active_segment_ids(self, *, segment_ids, **_kwargs):
        return set(segment_ids)


class FakeHybridVectorStore:
    def __init__(self, probe: RecallProbe):
        self.probe = probe
        self.calls = []
        self.native_calls = []
        self.retrieve_vectors_calls = 0
        self.url = "memory://qdrant"

    async def ping(self, timeout_seconds=1.0):
        _ = timeout_seconds
        return True

    async def require_collection_readable(self, *_args, **_kwargs):
        return {"tenant_id": "tenant-a", "dataset_id": "kb-demo"}

    async def search(self, **kwargs):
        self.calls.append(kwargs)
        self.probe.dense_started.set()
        await asyncio.wait_for(self.probe.bm25_started.wait(), timeout=1)
        is_rewrite = kwargs["query_vector"] == [2.0, 0.0]
        suffix = "rewrite" if is_rewrite else "original"
        return [
            SimpleNamespace(
                point_id="shared",
                score=0.95,
                payload={
                    "segment_id": "shared",
                    "document_id": "doc-shared",
                    "text": "shared answer",
                    "metadata": {"kind": "shared"},
                },
            ),
            SimpleNamespace(
                point_id=f"dense-{suffix}",
                score=0.8,
                payload={
                    "segment_id": f"dense-{suffix}",
                    "document_id": f"doc-dense-{suffix}",
                    "text": f"{suffix} dense exclusive answer",
                    "metadata": {"kind": "dense", "tags": [suffix]},
                },
            ),
        ]

    async def hybrid_search_multi_native(self, **kwargs):
        self.native_calls.append(kwargs)
        raise RuntimeError("native RRF unavailable in fallback test")

    async def retrieve_vectors(
        self,
        *,
        collection_name,
        point_ids,
        tenant_id,
        dataset_id,
    ):
        _ = collection_name, tenant_id, dataset_id
        self.retrieve_vectors_calls += 1
        return {
            point_id: [float(index + 1), 1.0]
            for index, point_id in enumerate(point_ids)
        }


class FailingDenseVectorStore(FakeHybridVectorStore):
    def __init__(self, probe: RecallProbe, *, wait_for_bm25: bool):
        super().__init__(probe)
        self.wait_for_bm25 = wait_for_bm25

    async def search(self, **kwargs):
        self.calls.append(kwargs)
        self.probe.dense_started.set()
        if self.wait_for_bm25:
            await asyncio.wait_for(self.probe.bm25_started.wait(), timeout=1)
        raise RuntimeError("simulated dense recall failure")


class FakeEmbedder:
    dimension = 2

    async def embed_query(self, query):
        return [2.0, 0.0] if "rewrite" in query else [1.0, 0.0]


class FakeNativeHybridVectorStore(FakeHybridVectorStore):
    async def hybrid_search_multi_native(self, **kwargs):
        self.native_calls.append(kwargs)
        return [
            [
                SimpleNamespace(
                    point_id="shared",
                    score=0.6,
                    payload={
                        "segment_id": "shared",
                        "document_id": "doc-shared",
                        "text": "shared answer",
                        "metadata": {"kind": "shared"},
                    },
                ),
                SimpleNamespace(
                    point_id="lexical-original",
                    score=0.55,
                    payload={
                        "segment_id": "lexical-original",
                        "document_id": "doc-lexical-original",
                        "text": "original lexical candidate",
                        "metadata": {"kind": "lexical"},
                    },
                ),
            ],
            [
                SimpleNamespace(
                    point_id="shared",
                    score=0.9,
                    payload={
                        "segment_id": "shared",
                        "document_id": "doc-shared",
                        "text": "shared answer",
                        "metadata": {"kind": "shared"},
                    },
                ),
                SimpleNamespace(
                    point_id="dense-rewrite",
                    score=0.8,
                    payload={
                        "segment_id": "dense-rewrite",
                        "document_id": "doc-dense-rewrite",
                        "text": "rewrite dense exclusive answer",
                        "metadata": {"kind": "dense", "tags": ["rewrite"]},
                    },
                ),
            ],
        ]

    async def search(self, **kwargs):
        pytest.fail(f"fallback dense search must not run: {kwargs}")


def _make_hybrid_service(probe: RecallProbe):
    async def _require_dataset_access(user, dataset_id, required="viewer"):
        probe.access_calls += 1
        return {
            "dataset_id": dataset_id,
            "tenant_id": "tenant-a",
            "collection_name": "kb-demo-collection",
            "index_config": {},
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_dimension": 2,
        }

    async def _get_presigned_image_url(_raw_url, _segment_id):
        return None

    database = FakeHybridDatabase(probe)
    vector_store = FakeHybridVectorStore(probe)
    svc = RetrievalService(
        SimpleNamespace(
            knowledge=SimpleNamespace(
                retrieval_query_max_concurrency=4,
                dashscope=SimpleNamespace(api_key=None),
            )
        ),
        database,
    )
    svc.vector_store = vector_store
    svc._ks = SimpleNamespace(
        require_dataset_access=_require_dataset_access,
        _resolve_fusion_config=lambda **kwargs: {
            "method": "rrf",
            "dense_weight": kwargs.get("dense_weight")
            if kwargs.get("dense_weight") is not None
            else 0.5,
            "bm25_weight": kwargs.get("bm25_weight")
            if kwargs.get("bm25_weight") is not None
            else 0.5,
            "rrf_k": 60,
        },
        _is_multimodal_dataset=lambda _dataset: False,
        _resolve_embedding_config=lambda **_kwargs: {},
        _should_apply_score_threshold=lambda _mode: False,
        _filter_candidates_by_metadata=lambda candidates, source_type, language, metadata: (
            KnowledgeService._filter_candidates_by_metadata(
                None, candidates, source_type, language, metadata
            )
        ),
        _get_presigned_image_url=_get_presigned_image_url,
        _normalize_local_image_url=lambda raw_url, _segment_id: raw_url,
    )
    return svc, database, vector_store


@pytest.mark.asyncio
async def test_hybrid_retrieval_keeps_bm25_results_when_dense_recall_fails(
    monkeypatch,
):
    probe = RecallProbe()
    svc, _, _ = _make_hybrid_service(probe)
    svc.vector_store = FailingDenseVectorStore(probe, wait_for_bm25=True)

    async def _get_cached_embedder(_config, dimension=None):
        return FakeEmbedder()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.get_cached_embedder",
        _get_cached_embedder,
    )

    results, meta = await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="original full question",
        top_k=5,
        mode="hybrid",
        rerank=False,
        mmr=False,
    )

    assert {result.segment_id for result in results} == {"shared", "bm25-original"}
    assert meta["dense_hits_raw_count"] == 0
    assert meta["bm25_hits_raw_count"] == 2


@pytest.mark.asyncio
async def test_dense_retrieval_propagates_dense_recall_failure(monkeypatch):
    probe = RecallProbe()
    svc, _, _ = _make_hybrid_service(probe)
    svc.vector_store = FailingDenseVectorStore(probe, wait_for_bm25=False)

    async def _get_cached_embedder(_config, dimension=None):
        return FakeEmbedder()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.get_cached_embedder",
        _get_cached_embedder,
    )

    with pytest.raises(ValidationFailedError, match="simulated dense recall failure"):
        await svc.retrieve(
            user=SimpleNamespace(),
            dataset_id="kb-demo",
            query="original full question",
            top_k=5,
            mode="dense",
            rerank=False,
            mmr=False,
        )


@pytest.mark.asyncio
async def test_mmr_collection_authority_race_fails_closed_without_ranked_fallback(
    monkeypatch,
):
    probe = RecallProbe()
    svc, _, _ = _make_hybrid_service(probe)

    class MmrAuthorityRaceStore(FakeHybridVectorStore):
        async def retrieve_vectors(self, **_kwargs):
            raise CollectionReadAuthorityError("scope changed before MMR vector fetch")

    svc.vector_store = MmrAuthorityRaceStore(probe)

    async def _get_cached_embedder(_config, dimension=None):
        return FakeEmbedder()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.get_cached_embedder",
        _get_cached_embedder,
    )

    with pytest.raises(ValidationFailedError, match="scope changed before MMR"):
        await svc.retrieve(
            user=SimpleNamespace(),
            dataset_id="kb-demo",
            query="original full question",
            top_k=1,
            mode="hybrid",
            rerank=False,
            mmr=True,
        )


@pytest.mark.asyncio
async def test_bm25_retrieval_uses_postgres_fts_candidates():
    svc, database = _make_bm25_service(
        [
            {
                "segment_id": "seg-1",
                "dataset_id": "kb-demo",
                "document_id": "doc-1",
                "position": 0,
                "text": "alpha alpha beta",
                "token_count": 3,
                "metadata": {"section": "one"},
                "source_type": "manual",
                "language": "en",
            },
            {
                "segment_id": "seg-2",
                "dataset_id": "kb-demo",
                "document_id": "doc-2",
                "position": 0,
                "text": "alpha gamma",
                "token_count": 2,
                "metadata": {"section": "two"},
                "source_type": "manual",
                "language": "en",
            },
        ]
    )

    results, meta = await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=2,
        mode="bm25",
        rerank=False,
        mmr=False,
    )

    assert [result.segment_id for result in results] == ["seg-1", "seg-2"]
    assert [result.metadata["_bm25_score"] for result in results] == [
        results[0].metadata["_bm25_score"],
        results[0].metadata["_bm25_score"],
    ]
    assert database.calls[0]["dataset_id"] == "kb-demo"
    assert "alpha" in database.calls[0]["terms"]
    assert meta["bm25_hits_raw_count"] == 2
    assert set(meta["timings_ms"]) == {
        "dense_prepare_ms",
        "dense_search_ms",
        "bm25_search_ms",
        "filter_ms",
        "rerank_ms",
        "mmr_ms",
        "total_ms",
    }
    assert all(value >= 0 for value in meta["timings_ms"].values())


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "reason",
    ["stored bm25_v2 active", "malformed immutable scope"],
)
async def test_pg_only_bm25_rejects_unsafe_collection_before_fts(reason):
    svc, database = _make_bm25_service(
        [
            {
                "segment_id": "must-not-return",
                "dataset_id": "kb-demo",
                "document_id": "doc-a",
                "text": "unsafe fallback",
                "metadata": {},
            }
        ]
    )
    svc.vector_store = RejectedReadAuthorityStore(reason)

    with pytest.raises(ValidationFailedError, match=reason):
        await svc.retrieve(
            user=SimpleNamespace(),
            dataset_id="kb-demo",
            query="unsafe fallback",
            mode="bm25",
            rerank=False,
            mmr=False,
        )
    assert database.calls == []


@pytest.mark.asyncio
async def test_native_authority_race_rejects_hybrid_without_pg_fallback(monkeypatch):
    probe = RecallProbe()
    svc, database, _ = _make_hybrid_service(probe)

    class NativeAuthorityRaceStore(FakeHybridVectorStore):
        async def hybrid_search_multi_native(self, **kwargs):
            self.native_calls.append(kwargs)
            raise CollectionReadAuthorityError("scope changed after preflight")

    store = NativeAuthorityRaceStore(probe)
    svc.vector_store = store

    async def _get_cached_embedder(_config, dimension=None):
        return FakeEmbedder()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.get_cached_embedder",
        _get_cached_embedder,
    )

    with pytest.raises(ValidationFailedError, match="scope changed after preflight"):
        await svc.retrieve(
            user=SimpleNamespace(),
            dataset_id="kb-demo",
            query="original full question",
            mode="hybrid",
            rerank=False,
            mmr=False,
        )
    assert len(store.native_calls) == 1
    assert database.calls == []
    assert store.calls == []


@pytest.mark.asyncio
async def test_dense_candidates_are_filtered_by_postgres_active_authority(monkeypatch):
    probe = RecallProbe()
    probe.bm25_started.set()
    svc, database, _ = _make_hybrid_service(probe)
    authority_calls = []

    async def filter_active(*, dataset_id, tenant_id, segment_ids):
        authority_calls.append((dataset_id, tenant_id, list(segment_ids)))
        return {"dense-original"}

    database.filter_active_segment_ids = filter_active

    async def _get_cached_embedder(_config, dimension=None):
        return FakeEmbedder()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.get_cached_embedder",
        _get_cached_embedder,
    )

    results, meta = await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="original full question",
        top_k=5,
        mode="dense",
        rerank=False,
        mmr=False,
    )

    assert [result.segment_id for result in results] == ["dense-original"]
    assert authority_calls == [
        ("kb-demo", "tenant-a", ["shared", "dense-original"])
    ]
    assert meta["inactive_candidates_filtered"] == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("failure_mode", ["error", "unexpected"])
async def test_dense_active_authority_failure_is_fail_closed(monkeypatch, failure_mode):
    probe = RecallProbe()
    probe.bm25_started.set()
    svc, database, _ = _make_hybrid_service(probe)

    async def filter_active(**_kwargs):
        if failure_mode == "error":
            raise RuntimeError("postgres unavailable")
        return {"foreign-segment"}

    database.filter_active_segment_ids = filter_active

    async def _get_cached_embedder(_config, dimension=None):
        return FakeEmbedder()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.get_cached_embedder",
        _get_cached_embedder,
    )

    expected = "authority failed" if failure_mode == "error" else "unexpected segment"
    with pytest.raises(ValidationFailedError, match=expected):
        await svc.retrieve(
            user=SimpleNamespace(),
            dataset_id="kb-demo",
            query="original full question",
            mode="dense",
            rerank=False,
            mmr=False,
        )


@pytest.mark.asyncio
async def test_native_hybrid_candidates_share_postgres_active_authority(monkeypatch):
    probe = RecallProbe()
    svc, database, _ = _make_hybrid_service(probe)
    store = FakeNativeHybridVectorStore(probe)
    svc.vector_store = store
    authority_calls = []

    async def filter_active(*, dataset_id, tenant_id, segment_ids):
        authority_calls.append((dataset_id, tenant_id, set(segment_ids)))
        return {"dense-rewrite"}

    database.filter_active_segment_ids = filter_active

    async def _get_cached_embedder(_config, dimension=None):
        return FakeEmbedder()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.get_cached_embedder",
        _get_cached_embedder,
    )

    batch_results, _ = await svc.retrieve_batch(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        queries=["original full question", "rewrite query"],
        top_k=5,
        mode="hybrid",
        rerank=False,
        mmr=False,
    )

    results = batch_results[0]["results"]
    assert [result["segment_id"] for result in results] == ["dense-rewrite"]
    assert authority_calls == [
        (
            "kb-demo",
            "tenant-a",
            {"shared", "lexical-original", "dense-rewrite"},
        )
    ]
    assert batch_results[0]["meta"]["inactive_candidates_filtered"] == 2
    assert not database.calls


@pytest.mark.asyncio
async def test_metadata_filter_refills_top_k_from_nested_metadata():
    svc, database = _make_bm25_service(
        [
            {
                "segment_id": "seg-wrong",
                "dataset_id": "kb-demo",
                "document_id": "doc-1",
                "text": "alpha alpha",
                "metadata": {"section": "wrong"},
            },
            {
                "segment_id": "seg-target",
                "dataset_id": "kb-demo",
                "document_id": "doc-2",
                "text": "alpha",
                "metadata": {"section": "target"},
            },
        ]
    )

    results, meta = await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=1,
        mode="bm25",
        rerank=False,
        mmr=False,
        metadata_filter={"section": "target"},
    )

    assert [result.segment_id for result in results] == ["seg-target"]
    assert database.calls[0]["metadata_filter"] == {"section": "target"}
    assert "Metadata filter: filtered 1 candidates" in meta["pipeline_stages"]


@pytest.mark.asyncio
async def test_rerank_order_is_not_mixed_with_fusion_scores(monkeypatch):
    svc, _ = _make_bm25_service(
        [
            {
                "segment_id": "seg-a",
                "dataset_id": "kb-demo",
                "document_id": "doc-a",
                "text": "alpha alpha alpha",
                "metadata": {},
            },
            {
                "segment_id": "seg-b",
                "dataset_id": "kb-demo",
                "document_id": "doc-b",
                "text": "alpha alpha",
                "metadata": {},
            },
            {
                "segment_id": "seg-c",
                "dataset_id": "kb-demo",
                "document_id": "doc-c",
                "text": "alpha",
                "metadata": {},
            },
        ]
    )

    class FakeReranker:
        async def rerank(self, **_kwargs):
            return [
                RerankResult(index=1, relevance_score=0.2),
                RerankResult(index=2, relevance_score=0.1),
            ]

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker.create_reranker",
        lambda **_kwargs: FakeReranker(),
    )

    results, meta = await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=3,
        mode="bm25",
        rerank=True,
        rerank_model="bge-reranker-v2-m3",
        rerank_top_n=2,
        mmr=False,
    )

    assert [result.segment_id for result in results] == ["seg-b", "seg-c", "seg-a"]
    assert meta["timings_ms"]["rerank_ms"] >= 0


@pytest.mark.asyncio
async def test_default_rrf_keeps_legacy_unweighted_fallback_contract(monkeypatch):
    probe = RecallProbe()
    svc, _, _ = _make_hybrid_service(probe)
    rrf_calls = []

    async def _get_cached_embedder(_config, dimension=None):
        return FakeEmbedder()

    def _rrf(ranked_lists, **kwargs):
        rrf_calls.append({"ranked_lists": ranked_lists, "kwargs": kwargs})
        return real_reciprocal_rank_fusion(ranked_lists, **kwargs)

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.get_cached_embedder",
        _get_cached_embedder,
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.reciprocal_rank_fusion",
        _rrf,
    )

    batch_results, _ = await svc.retrieve_batch(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        queries=["original full question", "rewrite query"],
        top_k=3,
        mode="hybrid",
        rerank=False,
        mmr=False,
    )

    assert len(rrf_calls) == 2
    assert [call["kwargs"] for call in rrf_calls] == [{"k": 60}, {"k": 60}]
    assert batch_results[0]["meta"]["fusion_semantics"] == "legacy_unweighted_rrf_v1"


@pytest.mark.asyncio
async def test_retrieve_batch_runs_per_query_rrf_then_one_global_rerank_and_mmr(monkeypatch):
    probe = RecallProbe()
    svc, database, vector_store = _make_hybrid_service(probe)
    rrf_calls = []
    rerank_calls = []
    mmr_calls = []

    async def _get_cached_embedder(_config, dimension=None):
        return FakeEmbedder()

    def _rrf(ranked_lists, **kwargs):
        rrf_calls.append({"ranked_lists": ranked_lists, "kwargs": kwargs})
        return real_reciprocal_rank_fusion(ranked_lists, **kwargs)

    class FakeReranker:
        async def rerank(self, *, query, documents, top_n):
            rerank_calls.append({"query": query, "documents": documents, "top_n": top_n})
            ordered = sorted(
                range(len(documents)),
                key=lambda index: "rewrite dense exclusive" in documents[index],
                reverse=True,
            )
            return [
                RerankResult(index=index, relevance_score=1.0 - rank * 0.05)
                for rank, index in enumerate(ordered[:top_n])
            ]

    def _mmr(candidates, relevance, vectors, *, top_k, **_kwargs):
        mmr_calls.append(list(candidates))
        selected = candidates[:top_k]
        return selected, {
            segment_id: MMRPick(
                item_id=segment_id,
                mmr_score=relevance[segment_id],
                relevance=relevance[segment_id],
                max_sim_to_selected=0.0,
            )
            for segment_id in selected
        }

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.get_cached_embedder",
        _get_cached_embedder,
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.reciprocal_rank_fusion",
        _rrf,
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker.create_reranker",
        lambda **_kwargs: FakeReranker(),
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.mmr_select",
        _mmr,
    )

    batch_results, meta = await svc.retrieve_batch(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        queries=["知识库问题 original", "rewrite query"],
        top_k=2,
        mode="hybrid",
        dense_weight=0.7,
        bm25_weight=0.3,
        rerank=True,
        rerank_model="bge-reranker-v2-m3",
        mmr=True,
        max_parallel=10,
    )

    results = batch_results[0]["results"]
    result_ids = [result["segment_id"] for result in results]
    assert probe.bm25_started.is_set() and probe.dense_started.is_set()
    # Initial authorization plus inner and final generation validation.
    assert probe.access_calls == 3
    assert len(database.calls) == 2
    assert len(vector_store.calls) == 2
    assert len(rrf_calls) == 2
    assert len(rerank_calls) == len(mmr_calls) == 1
    assert all(len(call["ranked_lists"]) == 2 for call in rrf_calls)
    assert [call["kwargs"]["weights"] for call in rrf_calls] == [
        {"dense": 0.75, "bm25": 0.25},
        {"dense": 0.75, "bm25": 0.25},
    ]
    assert all(call["kwargs"]["qdrant_weighted"] is True for call in rrf_calls)
    assert all(
        sum("shared" in ranked_ids for ranked_ids in ranked_lists.values()) == 2
        for ranked_lists in (call["ranked_lists"] for call in rrf_calls)
    )
    assert rerank_calls[0]["query"] == "知识库问题 original"
    assert vector_store.retrieve_vectors_calls == 1
    assert "dense-rewrite" in result_ids
    assert len(result_ids) == len(set(result_ids)) <= 2
    assert meta["total_results"] == len(results)
    assert meta["max_parallel"] == 4
    assert batch_results[0]["meta"]["rrf_ranked_list_count"] == 4
    assert batch_results[0]["meta"]["rrf_query_count"] == 2
    assert batch_results[0]["meta"]["cross_query_fusion"] == "max"
    assert batch_results[0]["meta"]["dense_weight"] == 0.75
    assert batch_results[0]["meta"]["bm25_weight"] == 0.25
    assert batch_results[0]["meta"]["fusion_semantics"] == "qdrant_weighted_rrf_v1"
    shared = next(result for result in results if result["segment_id"] == "shared")
    expected_shared_score = max(
        real_reciprocal_rank_fusion(
            call["ranked_lists"],
            **call["kwargs"],
        )["shared"]
        for call in rrf_calls
    )
    assert shared["metadata"]["_rrf_score_raw"] == round(expected_shared_score, 6)


@pytest.mark.asyncio
async def test_retrieve_batch_uses_one_native_batch_then_one_global_rerank_and_mmr(monkeypatch):
    probe = RecallProbe()
    svc, database, _ = _make_hybrid_service(probe)
    vector_store = FakeNativeHybridVectorStore(probe)
    svc.vector_store = vector_store
    rerank_calls = []
    mmr_calls = []

    async def _get_cached_embedder(_config, dimension=None):
        return FakeEmbedder()

    def _python_rrf_must_not_run(*args, **kwargs):
        pytest.fail(f"Python RRF must not run: {args}, {kwargs}")

    class FakeReranker:
        async def rerank(self, *, query, documents, top_n):
            rerank_calls.append({"query": query, "documents": documents, "top_n": top_n})
            return [
                RerankResult(index=index, relevance_score=1.0 - index * 0.1)
                for index in range(min(len(documents), top_n))
            ]

    def _mmr(candidates, relevance, vectors, *, top_k, **_kwargs):
        mmr_calls.append(list(candidates))
        selected = candidates[:top_k]
        return selected, {
            segment_id: MMRPick(
                item_id=segment_id,
                mmr_score=relevance[segment_id],
                relevance=relevance[segment_id],
                max_sim_to_selected=0.0,
            )
            for segment_id in selected
        }

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.get_cached_embedder",
        _get_cached_embedder,
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.reciprocal_rank_fusion",
        _python_rrf_must_not_run,
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker.create_reranker",
        lambda **_kwargs: FakeReranker(),
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.mmr_select",
        _mmr,
    )

    batch_results, meta = await svc.retrieve_batch(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        queries=["知识库问题 original", "rewrite query"],
        top_k=2,
        mode="hybrid",
        dense_weight=0.7,
        bm25_weight=0.3,
        rerank=True,
        rerank_model="bge-reranker-v2-m3",
        mmr=True,
    )

    results = batch_results[0]["results"]
    pipeline_meta = batch_results[0]["meta"]
    assert len(vector_store.native_calls) == 1
    assert len(vector_store.native_calls[0]["routes"]) == 2
    assert vector_store.native_calls[0]["rrf_k"] == 60
    assert vector_store.native_calls[0]["tenant_id"] == "tenant-a"
    assert vector_store.native_calls[0]["dataset_id"] == "kb-demo"
    assert vector_store.native_calls[0]["dense_weight"] == 0.75
    assert vector_store.native_calls[0]["sparse_weight"] == 0.25
    assert not vector_store.calls and not database.calls
    assert len(rerank_calls) == len(mmr_calls) == 1
    assert pipeline_meta["native_hybrid"] is True
    assert pipeline_meta["native_prefetch_count"] == 4
    assert pipeline_meta["rrf_ranked_list_count"] == 4
    assert pipeline_meta["rrf_query_count"] == 2
    assert pipeline_meta["cross_query_fusion"] == "max"
    assert pipeline_meta["native_batch_request_count"] == 1
    assert pipeline_meta["fusion_applied_by"] == "qdrant"
    assert pipeline_meta["fusion_semantics"] == "qdrant_weighted_rrf_v1"
    assert pipeline_meta["dense_weight"] == 0.75
    assert pipeline_meta["bm25_weight"] == 0.25
    assert [item["metadata"]["global_rank"] for item in results] == [1, 2]
    assert results[0]["segment_id"] == "shared"
    assert results[0]["metadata"]["_rrf_score_raw"] == 0.9
    assert len(results) == meta["total_results"] == 2


@pytest.mark.asyncio
async def test_retrieve_batch_applies_complex_per_query_filters_after_dense_recall(
    monkeypatch,
):
    probe = RecallProbe()
    probe.bm25_started.set()
    svc, _, _ = _make_hybrid_service(probe)

    async def _get_cached_embedder(_config, dimension=None):
        return FakeEmbedder()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.retrieval_service.get_cached_embedder",
        _get_cached_embedder,
    )

    batch_results, _ = await svc.retrieve_batch(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        queries=[
            {
                "query": "original full question",
                "mode": "dense",
                "metadata_filter": {"tags": ["original"]},
            },
            {
                "query": "rewrite query",
                "mode": "dense",
                "metadata_filter": {"tags": ["not-rewrite"]},
            },
        ],
        top_k=5,
        mode="dense",
        rerank=False,
        mmr=False,
    )

    assert [item["segment_id"] for item in batch_results[0]["results"]] == [
        "dense-original"
    ]


@pytest.mark.asyncio
async def test_multimodal_cache_rechecks_dataset_access():
    svc = object.__new__(RetrievalService)

    async def _deny_access(*_args, **_kwargs):
        raise PermissionDeniedError("access revoked")

    async def _unexpected_cache_read(*_args, **_kwargs):
        pytest.fail("cache must not be read before authorization")

    svc._ks = SimpleNamespace(
        require_dataset_access=_deny_access,
        _compute_retrieval_query_fingerprint=lambda _payload: "fingerprint",
        _get_cached_retrieval=_unexpected_cache_read,
    )

    with pytest.raises(PermissionDeniedError, match="access revoked"):
        await svc.retrieve_with_images_v2(
            user=SimpleNamespace(user_id="user-1"),
            dataset_id="kb-demo",
            query="cached query",
            include_images=False,
            vlm_rerank=False,
        )


@pytest.mark.asyncio
async def test_retrieve_batch_supports_per_query_overrides():
    svc = object.__new__(RetrievalService)

    async def _require_dataset_access(user, dataset_id, required="viewer"):
        return _readable_dataset(dataset_id)

    calls = []

    async def _retrieve(**kwargs):
        calls.append(kwargs)
        return [_mock_result(kwargs["query"], {"source_type": kwargs.get("source_type_filter")})], {
            "ok": True
        }

    svc._ks = SimpleNamespace(require_dataset_access=_require_dataset_access)
    svc.vector_store = ReadableVectorStore()
    svc._retrieve_queries = _retrieve

    batch_results, meta = await RetrievalService.retrieve_batch(
        svc,
        user=SimpleNamespace(),
        dataset_id="kb_demo",
        queries=[
            {
                "query": "release rollback",
                "source_type_filter": "runbook",
                "metadata_filter": {"team": "platform"},
            },
            "deployment health checks",
        ],
        top_k=4,
        max_parallel=2,
    )

    assert len(calls) == 1
    assert calls[0]["query"] == "release rollback"
    assert calls[0]["_query_specs"][0]["source_type_filter"] == "runbook"
    assert calls[0]["_query_specs"][0]["metadata_filter"] == {"team": "platform"}
    assert calls[0]["_query_specs"][1]["query"] == "deployment health checks"
    assert len(batch_results) == 1
    assert batch_results[0]["meta"]["queue_wait_ms"] >= 0
    assert batch_results[0]["meta"]["retrieve_time_ms"] >= 0
    assert meta["total_queries"] == 2
    assert meta["total_results"] == 1
    assert meta["avg_queue_wait_ms"] >= 0
    assert meta["max_queue_wait_ms"] >= 0


@pytest.mark.asyncio
async def test_retrieve_batch_deduplicates_queries_before_recall():
    svc = object.__new__(RetrievalService)
    calls = []

    async def _require_dataset_access(user, dataset_id, required="viewer"):
        return _readable_dataset(dataset_id)

    async def _retrieve(**kwargs):
        calls.append(kwargs)
        return [_mock_result(kwargs["query"])], {}

    svc._ks = SimpleNamespace(require_dataset_access=_require_dataset_access)
    svc.vector_store = ReadableVectorStore()
    svc._retrieve_queries = _retrieve

    batch_results, meta = await RetrievalService.retrieve_batch(
        svc,
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        queries=["same query", "same query", "rewrite query"],
        top_k=1,
    )

    assert len(calls) == 1
    assert [spec["query"] for spec in calls[0]["_query_specs"]] == [
        "same query",
        "rewrite query",
    ]
    assert len(batch_results[0]["results"]) == 1
    assert meta["total_queries"] == 3
    assert meta["unique_queries"] == 2


@pytest.mark.asyncio
async def test_retrieve_batch_single_query_uses_one_global_pipeline():
    svc = object.__new__(RetrievalService)
    calls = []

    async def _require_dataset_access(user, dataset_id, required="viewer"):
        return _readable_dataset(dataset_id)

    async def _retrieve(**kwargs):
        calls.append(kwargs)
        return [_mock_result(kwargs["query"])], {}

    svc._ks = SimpleNamespace(require_dataset_access=_require_dataset_access)
    svc.vector_store = ReadableVectorStore()
    svc._retrieve_queries = _retrieve

    batch_results, meta = await RetrievalService.retrieve_batch(
        svc,
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        queries=["only query"],
        top_k=3,
    )

    assert len(calls) == 1
    assert calls[0]["_query_specs"] == [{"query": "only query"}]
    assert batch_results[0]["query"] == "only query"
    assert meta["total_queries"] == meta["unique_queries"] == 1


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("query_count", "expected_top_k"),
    [(1, 5), (2, 6), (3, 8), (4, 9), (5, 10)],
)
async def test_retrieve_batch_default_top_k_follows_unique_query_count(query_count, expected_top_k):
    svc = object.__new__(RetrievalService)
    calls = []

    async def _require_dataset_access(user, dataset_id, required="viewer"):
        return _readable_dataset(dataset_id)

    async def _retrieve(**kwargs):
        calls.append(kwargs)
        return [], {}

    svc._ks = SimpleNamespace(require_dataset_access=_require_dataset_access)
    svc.vector_store = ReadableVectorStore()
    svc._retrieve_queries = _retrieve

    _, meta = await RetrievalService.retrieve_batch(
        svc,
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        queries=[f"query-{index}" for index in range(query_count)],
    )

    assert calls[0]["top_k"] == expected_top_k
    assert meta["final_top_k"] == expected_top_k


@pytest.mark.asyncio
async def test_retrieve_batch_explicit_top_k_overrides_dynamic_default():
    svc = object.__new__(RetrievalService)
    calls = []

    async def _require_dataset_access(user, dataset_id, required="viewer"):
        return _readable_dataset(dataset_id)

    async def _retrieve(**kwargs):
        calls.append(kwargs)
        return [], {}

    svc._ks = SimpleNamespace(require_dataset_access=_require_dataset_access)
    svc.vector_store = ReadableVectorStore()
    svc._retrieve_queries = _retrieve

    _, meta = await RetrievalService.retrieve_batch(
        svc,
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        queries=["one", "two", "three"],
        top_k=4,
    )

    assert calls[0]["top_k"] == 4
    assert meta["final_top_k"] == 4


@pytest.mark.asyncio
async def test_retrieve_batch_keeps_successful_candidates_after_partial_failure():
    svc, _ = _make_bm25_service([])

    class PartialDatabase:
        async def filter_active_segment_ids(self, *, segment_ids, **_kwargs):
            return set(segment_ids)

        async def search_segments_text(self, **kwargs):
            if "broken" in kwargs["terms"]:
                raise RuntimeError("simulated recall failure")
            return [
                {
                    "segment_id": "good-segment",
                    "dataset_id": "kb-demo",
                    "document_id": "good-document",
                    "text": "good answer",
                    "metadata": {},
                }
            ]

    svc.db = PartialDatabase()

    batch_results, meta = await svc.retrieve_batch(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        queries=["good", "broken"],
        top_k=5,
        mode="bm25",
        rerank=False,
        mmr=False,
        max_parallel=2,
    )

    assert [result["segment_id"] for result in batch_results[0]["results"]] == [
        "good-segment"
    ]
    assert "broken" in batch_results[0]["meta"]["recall_errors"]
    assert meta["total_results"] == 1


@pytest.mark.asyncio
async def test_retrieve_batch_does_not_turn_pipeline_failure_into_empty_success():
    svc = object.__new__(RetrievalService)

    async def _require_dataset_access(_user, dataset_id, required="viewer"):
        assert required == "viewer"
        return _readable_dataset(dataset_id)

    async def _failed_retrieval(**_kwargs):
        raise RuntimeError("vector dependency unavailable")

    svc._ks = SimpleNamespace(require_dataset_access=_require_dataset_access)
    svc.vector_store = ReadableVectorStore()
    svc._retrieve_queries = _failed_retrieval

    with pytest.raises(RuntimeError, match="knowledge batch retrieval failed"):
        await RetrievalService.retrieve_batch(
            svc,
            user=SimpleNamespace(),
            dataset_id="kb-demo",
            queries=["real query"],
        )


@pytest.mark.asyncio
async def test_retrieve_batch_fails_when_every_recall_path_errors():
    svc, _ = _make_bm25_service([])

    class FailedDatabase:
        async def search_segments_text(self, **_kwargs):
            raise RuntimeError("postgres unavailable")

    svc.db = FailedDatabase()

    with pytest.raises(RuntimeError, match="knowledge batch retrieval failed"):
        await svc.retrieve_batch(
            user=SimpleNamespace(),
            dataset_id="kb-demo",
            queries=["real query"],
            top_k=5,
            mode="bm25",
            rerank=False,
            mmr=False,
        )


@pytest.mark.asyncio
async def test_retrieve_batch_handles_empty_query_list():
    svc = object.__new__(RetrievalService)

    with pytest.raises(ValidationFailedError, match=r"query\[0\]"):
        await RetrievalService.retrieve_batch(
            svc,
            user=SimpleNamespace(),
            dataset_id="kb-demo",
            queries=["", "   ", None],
        )
