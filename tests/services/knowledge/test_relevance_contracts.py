"""Relevance interface contracts (PRD addendum §1-T2 + "所有相关性接口测试齐备").

Pins the CURRENT retrieval relevance contract so the T2 changes (post-hoc
threshold rule, defensive rerank re-application, weighted-fusion
normalization) land on a verified baseline instead of shifting sand:

* retrieval legs never pre-filter by score — the tenant threshold is applied
  only after fusion/rerank (main PRD T2-4; addendum §1-T2-1);
* the threshold gate today is dense-only (post-hoc rule ships in Phase 2);
* fusion dedupes the union by stable segment id, keeps per-leg scores, and
  preserves first-seen order (addendum §1-T2-2 / T2-5);
* rerank results keep provider order, drop invalid indices, and top_k is
  re-applied after rerank (addendum §1-T2-3 baseline);
* chunk-search LIKE patterns escape metacharacters (addendum §1-T2-7).
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.persistence.database import (
    DatabaseStorage,
    _escape_like_pattern,
)
from knowledge_service.services.knowledge import retrieval_service as retrieval_module
from knowledge_service.services.knowledge.knowledge_service import KnowledgeService
from knowledge_service.services.knowledge.retrieval_service import RetrievalService
from knowledge_service.services.knowledge.text_reranker import RerankResult

from tests.services.knowledge.test_retrieve_batch import _make_bm25_service

_BM25_ROWS = [
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
]


# ---------------------------------------------------------------------------
# Dense harness
# ---------------------------------------------------------------------------


class _DenseEmbedder:
    dimension = 3

    async def embed_query(self, _query):
        return [0.1, 0.2, 0.3]


class _DenseVectorStore:
    def __init__(self, hits: list[Any]):
        self._hits = hits
        self.search_calls: list[dict[str, Any]] = []

    async def ping(self, **_kwargs):
        return True

    async def require_collection_readable(self, *_args, **_kwargs):
        return {"tenant_id": "tenant-a", "dataset_id": "kb-demo"}

    async def search(self, **kwargs):
        self.search_calls.append(kwargs)
        return list(self._hits)


def _dense_hit(segment_id: str, score: float, text: str = "dense answer") -> Any:
    return SimpleNamespace(
        point_id=segment_id,
        score=score,
        payload={
            "segment_id": segment_id,
            "document_id": f"doc-{segment_id}",
            "text": text,
        },
    )


def _make_dense_service(monkeypatch, hits: list[Any], *, threshold_gate: bool = True):
    async def _require_dataset_access(_user, dataset_id, required="viewer"):
        assert required == "viewer"
        return {
            "dataset_id": dataset_id,
            "tenant_id": "tenant-a",
            "collection_name": "kb-existing",
            "index_config": {},
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_dimension": 3,
        }

    async def _get_embedder(_config, dimension=None):
        return _DenseEmbedder()

    async def _filter_active(*, segment_ids, **_kwargs):
        return set(segment_ids)

    monkeypatch.setattr(retrieval_module, "get_cached_embedder", _get_embedder)

    vector_store = _DenseVectorStore(hits)
    service = RetrievalService(
        SimpleNamespace(
            knowledge=SimpleNamespace(
                retrieval_query_max_concurrency=2,
                dashscope=SimpleNamespace(api_key=None),
            )
        ),
        SimpleNamespace(filter_active_segment_ids=_filter_active),
    )
    service.vector_store = vector_store
    service._ks = SimpleNamespace(
        require_dataset_access=_require_dataset_access,
        _resolve_fusion_config=lambda **_kwargs: {
            "method": "rrf",
            "dense_weight": 0.5,
            "bm25_weight": 0.5,
            "rrf_k": 60,
        },
        _is_multimodal_dataset=lambda _dataset: False,
        _resolve_embedding_config=lambda **_kwargs: SimpleNamespace(),
        # Mirror the real KnowledgeService gate unless a test says otherwise.
        _should_apply_score_threshold=(
            (lambda mode: KnowledgeService._should_apply_score_threshold(None, mode))
            if threshold_gate
            else (lambda _mode: False)
        ),
        _filter_candidates_by_metadata=lambda candidates, *_args: candidates,
        _get_presigned_image_url=lambda *_args: None,
        _normalize_local_image_url=lambda raw_url, _segment_id: raw_url,
    )
    return service, vector_store


# ---------------------------------------------------------------------------
# Threshold contract
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("mode", "expected"),
    [
        ("dense", True),
        ("Dense", True),
        ("hybrid", False),
        ("bm25", False),
        ("keyword", False),
        ("", False),
        (None, False),
    ],
)
def test_threshold_gate_is_dense_only(mode, expected) -> None:
    # Production gate: tenant thresholds are only enforced on the dense path
    # today; the post-hoc rule for all modes is PRD T2-4 (Phase 2).
    assert KnowledgeService._should_apply_score_threshold(None, mode) is expected


@pytest.mark.asyncio
async def test_dense_leg_never_receives_a_score_threshold(monkeypatch) -> None:
    # Addendum §1-T2-1 / main PRD T2-4: legs recall unfiltered; the threshold
    # is a post-fusion/post-rerank concern. Even a request carrying a
    # threshold must not leak it into the vector leg query.
    service, vector_store = _make_dense_service(
        monkeypatch, [_dense_hit("seg-1", 0.9)], threshold_gate=False
    )

    results, _meta = await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="query",
        mode="dense",
        score_threshold=0.5,
        rerank=False,
        mmr=False,
    )

    assert [r.segment_id for r in results] == ["seg-1"]
    assert vector_store.search_calls, "dense leg must run"
    for call in vector_store.search_calls:
        assert call.get("score_threshold") is None, (
            "vector leg must recall unfiltered; threshold belongs after fusion"
        )


@pytest.mark.asyncio
async def test_threshold_filters_final_scores_after_the_pipeline(monkeypatch) -> None:
    # Two dense hits, raw 0.95 / 0.1 → robust-normalized 1.0 / 0.0.
    service, _store = _make_dense_service(
        monkeypatch,
        [_dense_hit("seg-top", 0.95), _dense_hit("seg-low", 0.1)],
    )

    results, meta = await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="query",
        mode="dense",
        score_threshold=1.0,
        top_k=10,
        rerank=False,
        mmr=False,
    )

    assert [r.segment_id for r in results] == ["seg-top"]
    assert meta["score_threshold"] == 1.0
    assert any(stage.startswith("Score threshold (1.0)") for stage in meta["pipeline_stages"])


@pytest.mark.asyncio
async def test_threshold_zero_is_a_noop(monkeypatch) -> None:
    service, _store = _make_dense_service(
        monkeypatch,
        [_dense_hit("seg-top", 0.95), _dense_hit("seg-low", 0.1)],
    )

    results, meta = await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="query",
        mode="dense",
        score_threshold=0.0,
        top_k=10,
        rerank=False,
        mmr=False,
    )

    assert [r.segment_id for r in results] == ["seg-top", "seg-low"]
    assert meta["score_threshold"] is None  # 0 = no filtering, not reported


@pytest.mark.asyncio
@pytest.mark.parametrize("bad_threshold", [2.0, -0.1, float("nan"), float("inf")])
async def test_out_of_range_request_threshold_is_rejected_fail_closed(
    monkeypatch, bad_threshold
) -> None:
    # Request-level thresholds outside [0, 1] are rejected before any recall
    # runs (bounded-request guard). Only dataset-configured defaults pass
    # through the pipeline's clamp. Fail-closed beats silent clamping.
    service, vector_store = _make_dense_service(
        monkeypatch, [_dense_hit("seg-mid", 0.9)]
    )

    with pytest.raises(ValidationFailedError, match="score_threshold"):
        await service.retrieve(
            user=SimpleNamespace(),
            dataset_id="kb-demo",
            query="query",
            mode="dense",
            score_threshold=bad_threshold,
            rerank=False,
            mmr=False,
        )

    assert vector_store.search_calls == [], "rejection must happen before recall"


@pytest.mark.asyncio
async def test_threshold_applies_to_rerank_scores_in_dense_mode(monkeypatch) -> None:
    # Addendum §1-T2-3: after rerank the threshold must be re-applied — on
    # the reranked scores, not the pre-rerank fusion scores (dense gate).
    service, _store = _make_dense_service(
        monkeypatch,
        [_dense_hit("seg-top", 0.95), _dense_hit("seg-low", 0.9)],
    )

    async def _fake_reranker(**_kwargs):
        # Provider flips the ranking: the lower-fusion candidate wins.
        return [
            RerankResult(index=1, relevance_score=0.9),
            RerankResult(index=0, relevance_score=0.2),
        ]

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker.create_reranker",
        lambda **_kwargs: SimpleNamespace(rerank=_fake_reranker),
    )

    results, meta = await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="query",
        mode="dense",
        score_threshold=0.5,
        rerank=True,
        rerank_model="bge-reranker-v2-m3",
        rerank_top_n=2,
        mmr=False,
    )

    assert "rerank_degraded" not in meta
    # Threshold cut the reranked score 0.2 < 0.5; the surviving hit is the
    # rerank winner, not the fusion winner.
    assert [r.segment_id for r in results] == ["seg-low"]


# ---------------------------------------------------------------------------
# Fusion union contract
# ---------------------------------------------------------------------------


class _HybridVectorStore:
    url = "memory://qdrant"

    def __init__(self):
        self.search_calls: list[dict[str, Any]] = []

    async def ping(self, timeout_seconds=1.0):
        _ = timeout_seconds
        return True

    async def require_collection_readable(self, *_args, **_kwargs):
        return {"tenant_id": "tenant-a", "dataset_id": "kb-demo"}

    async def search(self, **kwargs):
        self.search_calls.append(kwargs)
        return [
            SimpleNamespace(
                point_id="shared",
                score=0.95,
                payload={
                    "segment_id": "shared",
                    "document_id": "doc-shared",
                    "text": "alpha shared answer",
                },
            ),
            SimpleNamespace(
                point_id="dense-only",
                score=0.7,
                payload={
                    "segment_id": "dense-only",
                    "document_id": "doc-dense-only",
                    "text": "alpha dense exclusive",
                },
            ),
        ]

    async def hybrid_search_multi_native(self, **_kwargs):
        raise RuntimeError("native RRF unavailable in this test")

    async def retrieve_vectors(self, *, collection_name, point_ids, tenant_id, dataset_id):
        _ = collection_name, tenant_id, dataset_id
        return {point_id: [1.0, 0.0] for point_id in point_ids}


class _HybridDatabase:
    def __init__(self):
        self.calls: list[dict[str, Any]] = []

    async def search_segments_text(self, **kwargs):
        self.calls.append(kwargs)
        return [
            {
                "segment_id": "shared",
                "dataset_id": "kb-demo",
                "document_id": "doc-shared",
                "text": "alpha shared answer",
                "metadata": {"kind": "shared"},
            },
            {
                "segment_id": "bm25-only",
                "dataset_id": "kb-demo",
                "document_id": "doc-bm25-only",
                "text": "alpha lexical exclusive",
                "metadata": {"kind": "lexical"},
            },
        ]

    async def filter_active_segment_ids(self, *, segment_ids, **_kwargs):
        return set(segment_ids)


def _make_hybrid_service(monkeypatch):
    async def _require_dataset_access(_user, dataset_id, required="viewer"):
        return {
            "dataset_id": dataset_id,
            "tenant_id": "tenant-a",
            "collection_name": "kb-demo-collection",
            "index_config": {},
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_dimension": 2,
        }

    class _Embedder:
        dimension = 2

        async def embed_query(self, _query):
            return [1.0, 0.0]

    async def _get_embedder(_config, dimension=None):
        return _Embedder()

    monkeypatch.setattr(retrieval_module, "get_cached_embedder", _get_embedder)

    database = _HybridDatabase()
    vector_store = _HybridVectorStore()
    service = RetrievalService(
        SimpleNamespace(
            knowledge=SimpleNamespace(
                retrieval_query_max_concurrency=4,
                dashscope=SimpleNamespace(api_key=None),
            )
        ),
        database,
    )
    service.vector_store = vector_store
    service._ks = SimpleNamespace(
        require_dataset_access=_require_dataset_access,
        _resolve_fusion_config=lambda **_kwargs: {
            "method": "rrf",
            "dense_weight": 0.5,
            "bm25_weight": 0.5,
            "rrf_k": 60,
        },
        _is_multimodal_dataset=lambda _dataset: False,
        _resolve_embedding_config=lambda **_kwargs: {},
        _should_apply_score_threshold=lambda _mode: False,
        _filter_candidates_by_metadata=lambda candidates, *_args: candidates,
        _get_presigned_image_url=lambda *_args: None,
        _normalize_local_image_url=lambda raw_url, _segment_id: raw_url,
    )
    return service, database, vector_store


@pytest.mark.asyncio
async def test_hybrid_fusion_dedupes_the_union_by_segment_id(monkeypatch) -> None:
    # Addendum §1-T2-2/T2-5: union dedupe on the stable chunk id, per-leg
    # scores preserved (never a single mixed score field), first-seen order.
    service, _database, _store = _make_hybrid_service(monkeypatch)

    results, meta = await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=10,
        mode="hybrid",
        rerank=False,
        mmr=False,
    )

    ids = [r.segment_id for r in results]
    assert sorted(ids) == ["bm25-only", "dense-only", "shared"]
    assert len(ids) == len(set(ids)), "union fusion must not duplicate segments"
    # Present in both legs → ranks first under RRF.
    assert ids[0] == "shared"
    assert meta["total_candidates"] == 3

    shared = next(r for r in results if r.segment_id == "shared")
    payload = shared.metadata
    assert payload["_sources"] == ["bm25", "dense"]
    # Per-leg raw + normalized scores stay separate fields.
    assert isinstance(payload["_vector_score"], float)
    assert isinstance(payload["_keyword_score"], float)
    assert isinstance(payload["_dense_score_norm"], float)
    assert isinstance(payload["_bm25_score_norm"], float)
    assert isinstance(payload["_fusion_score"], float)
    assert payload["_rerank_score"] == "N/A"


@pytest.mark.asyncio
async def test_threshold_is_inert_in_hybrid_mode_today(monkeypatch) -> None:
    # Current contract (until PRD T2-4 ships in Phase 2): the tenant
    # threshold is enforced only on the dense path. A hybrid request with an
    # aggressive threshold must still serve its fused results.
    service, _database, _store = _make_hybrid_service(monkeypatch)

    results, meta = await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=10,
        mode="hybrid",
        score_threshold=0.99,
        rerank=False,
        mmr=False,
    )

    assert len(results) == 3
    assert meta["score_threshold"] is None


# ---------------------------------------------------------------------------
# Rerank return contract
# ---------------------------------------------------------------------------


def _install_reranker(monkeypatch, results: list[RerankResult]) -> None:
    async def _rerank(**_kwargs):
        return list(results)

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker.create_reranker",
        lambda **_kwargs: SimpleNamespace(rerank=_rerank),
    )


@pytest.mark.asyncio
async def test_rerank_keeps_provider_order_and_reapplies_top_k(monkeypatch) -> None:
    # Baseline for addendum §1-T2-3: today the pipeline trusts provider
    # order (no defensive re-sort yet) and re-applies top_k afterwards.
    svc, _database = _make_bm25_service(_BM25_ROWS)
    _install_reranker(
        monkeypatch,
        [
            RerankResult(index=1, relevance_score=0.4),
            RerankResult(index=0, relevance_score=0.95),
        ],
    )

    results, meta = await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=5,
        mode="bm25",
        rerank=True,
        rerank_model="bge-reranker-v2-m3",
        rerank_top_n=2,
        mmr=False,
    )

    assert "rerank_degraded" not in meta
    # Provider returned index 1 first; current contract preserves that order.
    assert [r.segment_id for r in results] == ["seg-b", "seg-a"]
    assert results[0].metadata["_rerank_score"] == 0.4
    assert results[1].metadata["_rerank_score"] == 0.95

    # top_k is re-applied after rerank: only the provider's first pick stays.
    svc, _database = _make_bm25_service(_BM25_ROWS)
    _install_reranker(
        monkeypatch,
        [
            RerankResult(index=1, relevance_score=0.4),
            RerankResult(index=0, relevance_score=0.95),
        ],
    )
    results, _meta = await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=1,
        mode="bm25",
        rerank=True,
        rerank_model="bge-reranker-v2-m3",
        rerank_top_n=2,
        mmr=False,
    )
    assert [r.segment_id for r in results] == ["seg-b"]


@pytest.mark.asyncio
async def test_rerank_out_of_range_indices_are_dropped_safely(monkeypatch) -> None:
    svc, _database = _make_bm25_service(_BM25_ROWS)
    _install_reranker(
        monkeypatch,
        [
            RerankResult(index=99, relevance_score=1.0),
            RerankResult(index=-1, relevance_score=1.0),
            RerankResult(index=0, relevance_score=0.8),
        ],
    )

    results, meta = await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=5,
        mode="bm25",
        rerank=True,
        rerank_model="bge-reranker-v2-m3",
        rerank_top_n=3,
        mmr=False,
    )

    assert "rerank_degraded" not in meta
    # Invalid indices never crash the pipeline; valid ones rerank, the rest
    # keep serving in fusion order behind them.
    assert [r.segment_id for r in results] == ["seg-a", "seg-b"]
    assert results[0].metadata["_rerank_score"] == 0.8
    assert results[1].metadata["_rerank_score"] == "N/A"


# ---------------------------------------------------------------------------
# Rerank API-key resolution (Phase 0 getenv consolidation compatibility):
# the legacy env chains must resolve exactly as the old os.getenv reads did.
# ---------------------------------------------------------------------------


def _install_reranker_capture(monkeypatch, captured: dict[str, Any]) -> None:
    async def _rerank(**_kwargs):
        return [RerankResult(index=0, relevance_score=0.9)]

    def _factory(**kwargs):
        captured.update(kwargs)
        return SimpleNamespace(rerank=_rerank)

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker.create_reranker",
        _factory,
    )


async def _rerank_retrieve(svc, rerank_model: str):
    return await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=5,
        mode="bm25",
        rerank=True,
        rerank_model=rerank_model,
        rerank_top_n=2,
        mmr=False,
    )


@pytest.mark.asyncio
async def test_dashscope_rerank_key_keeps_legacy_env_chain(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("DASHSCOPE_API_KEY", raising=False)
    monkeypatch.delenv("ALIYUN_KEY", raising=False)
    monkeypatch.delenv("Aliyun_KEY", raising=False)

    # Legacy fallback name alone resolves.
    svc, _database = _make_bm25_service(_BM25_ROWS)
    captured: dict[str, Any] = {}
    _install_reranker_capture(monkeypatch, captured)
    monkeypatch.setenv("ALIYUN_KEY", "legacy-key")
    await _rerank_retrieve(svc, "gte-rerank")
    assert captured["provider"] == "dashscope"
    assert captured["api_key"] == "legacy-key"

    # DASHSCOPE_API_KEY takes precedence over the legacy name.
    svc, _database = _make_bm25_service(_BM25_ROWS)
    captured.clear()
    _install_reranker_capture(monkeypatch, captured)
    monkeypatch.setenv("DASHSCOPE_API_KEY", "primary-key")
    await _rerank_retrieve(svc, "gte-rerank")
    assert captured["api_key"] == "primary-key"

    # The historical mixed-case spelling still resolves (case-insensitive).
    svc, _database = _make_bm25_service(_BM25_ROWS)
    captured.clear()
    _install_reranker_capture(monkeypatch, captured)
    monkeypatch.delenv("DASHSCOPE_API_KEY")
    monkeypatch.delenv("ALIYUN_KEY")
    monkeypatch.setenv("Aliyun_KEY", "mixed-case-key")
    await _rerank_retrieve(svc, "gte-rerank")
    assert captured["api_key"] == "mixed-case-key"


@pytest.mark.asyncio
async def test_dashscope_rerank_missing_key_degrades_instead_of_crashing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("DASHSCOPE_API_KEY", raising=False)
    monkeypatch.delenv("ALIYUN_KEY", raising=False)
    monkeypatch.delenv("Aliyun_KEY", raising=False)

    svc, _database = _make_bm25_service(_BM25_ROWS)
    captured: dict[str, Any] = {}
    _install_reranker_capture(monkeypatch, captured)

    results, meta = await _rerank_retrieve(svc, "gte-rerank")

    # Fail-closed for rerank, but the request still serves fusion order.
    assert captured == {}
    assert meta.get("rerank_degraded") == "error"
    assert "dashscope api_key is required" in str(meta.get("rerank_error"))
    assert [r.segment_id for r in results] == ["seg-a", "seg-b"]


@pytest.mark.asyncio
async def test_cohere_rerank_key_resolves_from_env(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    svc, _database = _make_bm25_service(_BM25_ROWS)
    captured: dict[str, Any] = {}
    _install_reranker_capture(monkeypatch, captured)
    monkeypatch.setenv("COHERE_API_KEY", "cohere-key")

    await _rerank_retrieve(svc, "rerank-multilingual-v3.0")

    assert captured["provider"] == "cohere"
    assert captured["api_key"] == "cohere-key"


# ---------------------------------------------------------------------------
# Chunk search LIKE-escape contract (addendum §1-T2-7)
# ---------------------------------------------------------------------------


def test_escape_like_pattern_neutralizes_metacharacters() -> None:
    assert _escape_like_pattern("plain") == "plain"
    assert _escape_like_pattern("50%") == "50\\%"
    assert _escape_like_pattern("a_b") == "a\\_b"
    assert _escape_like_pattern("c:\\path") == "c:\\\\path"
    # Escaping must be idempotent-safe in ordering: backslash first.
    assert _escape_like_pattern("%_\\") == "\\%\\_\\\\"
    # CJK text passes through untouched.
    assert _escape_like_pattern("知识库检索") == "知识库检索"


class _FetchConnection:
    def __init__(self) -> None:
        self.queries: list[tuple[str, tuple[Any, ...]]] = []

    async def fetch(self, query: str, *params: Any):
        self.queries.append((query, params))
        return []


class _FetchAcquire:
    def __init__(self, connection: _FetchConnection) -> None:
        self.connection = connection

    async def __aenter__(self) -> _FetchConnection:
        return self.connection

    async def __aexit__(self, *_exc: Any) -> bool:
        return False


class _FetchPool:
    def __init__(self, connection: _FetchConnection) -> None:
        self.connection = connection

    def acquire(self) -> _FetchAcquire:
        return _FetchAcquire(self.connection)


async def test_list_segments_escapes_like_metacharacters_in_user_input() -> None:
    connection = _FetchConnection()
    db = DatabaseStorage.__new__(DatabaseStorage)
    db._pool = _FetchPool(connection)

    await db.list_segments("dataset-a", query_text="50%_off")

    query, params = connection.queries[0]
    assert "text ILIKE" in query
    assert params[1] == "%50\\%\\_off%", (
        "LIKE metacharacters in chunk-search input must be escaped"
    )
