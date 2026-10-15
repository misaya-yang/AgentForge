from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge import embedding_manager as embedding_manager_module
from knowledge_service.services.knowledge import retrieval_service as retrieval_service_module
from knowledge_service.services.knowledge.embedding_manager import EmbeddingManager
from knowledge_service.services.knowledge.knowledge_service import KnowledgeService
from knowledge_service.services.knowledge.retrieval_service import RetrievalService


def _embedding_settings() -> SimpleNamespace:
    return SimpleNamespace(
        knowledge=SimpleNamespace(
            dashscope=SimpleNamespace(api_key="server-dashscope-key"),
            gemini=SimpleNamespace(api_key="server-gemini-key"),
            siliconflow=SimpleNamespace(
                api_key="server-siliconflow-key",
                base_url="https://server-owned.invalid/v1",
            ),
            text_embedding_dimension=1024,
            multimodal_embedding_model="tongyi-embedding-vision-plus",
            multimodal_embedding_max_concurrent=2,
        )
    )


@pytest.mark.parametrize(
    "legacy_config",
    [
        {"api_key": "row-key"},
        {"key": "row-key"},
        {"base_url": "http://127.0.0.1:8092/internal"},
        {"endpoint": "http://169.254.169.254/latest/meta-data"},
        {"endpointUrl": "https://attacker.invalid/embeddings"},
        {"nested": [{"api_base": "https://attacker.invalid/v1"}]},
        {"provider": {"openai_api_key": "row-key"}},
        {"request_headers": {"X-Token": "row-token"}},
        {"provider": {"access_token": "row-token"}},
        {"credentials": {"token": "row-token"}},
    ],
)
async def test_legacy_embedding_config_is_rejected_before_factory(
    monkeypatch: pytest.MonkeyPatch,
    legacy_config: dict[str, Any],
) -> None:
    factory_calls = 0

    def forbidden_factory(*_args: Any, **_kwargs: Any) -> None:
        nonlocal factory_calls
        factory_calls += 1
        raise AssertionError("embedding factory must not receive legacy row config")

    monkeypatch.setattr(embedding_manager_module, "create_embedding", forbidden_factory)
    manager = EmbeddingManager(_embedding_settings())
    dataset = {
        "embedding_provider": "dashscope",
        "embedding_model": "text-embedding-v4",
        "embedding_dimension": 1024,
        "embedding_config": legacy_config,
    }

    with pytest.raises(ValidationFailedError, match="legacy credential or endpoint"):
        await manager.get_text_embedder(dataset)

    assert factory_calls == 0


async def test_legacy_multimodal_config_is_rejected_before_server_resolver(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    resolver_calls = 0

    def forbidden_resolver(_domain: str) -> tuple[str, str]:
        nonlocal resolver_calls
        resolver_calls += 1
        raise AssertionError("server resolver must not run for a rejected dataset row")

    monkeypatch.setattr("ai_gateway_core.config.resolve_dashscope", forbidden_resolver)
    manager = EmbeddingManager(_embedding_settings())

    with pytest.raises(ValidationFailedError, match="legacy credential or endpoint"):
        await manager.get_unified_multimodal_embedder(
            {
                "embedding_provider": "unified_multimodal",
                "embedding_model": "tongyi-embedding-vision-plus",
                "embedding_config": {
                    "baseUrl": "http://127.0.0.1:8092/private",
                    "apiKey": "row-key",
                },
            }
        )

    assert resolver_calls == 0


class ProbeDatabase:
    def __init__(self, rows: list[dict[str, Any]] | None = None) -> None:
        self.rows = rows or []
        self.search_calls = 0
        self.association_calls = 0

    async def search_segments_text(self, **_kwargs: Any) -> list[dict[str, Any]]:
        self.search_calls += 1
        return list(self.rows)

    async def filter_active_segment_ids(
        self,
        *,
        segment_ids: list[str],
        **_kwargs: Any,
    ) -> set[str]:
        return set(segment_ids)

    async def get_segment_associations_batch(
        self,
        _segment_ids: list[str],
        **_kwargs: Any,
    ) -> dict[str, list[dict[str, Any]]]:
        self.association_calls += 1
        return {}


class ProbeVectorStore:
    def __init__(self) -> None:
        self.authority_calls = 0

    async def require_collection_readable(self, *_args: Any, **_kwargs: Any) -> dict[str, str]:
        self.authority_calls += 1
        return {"tenant_id": "tenant-a", "dataset_id": "dataset-a"}


def _retrieval_service(
    *,
    index_config: dict[str, Any],
    rows: list[dict[str, Any]] | None = None,
    is_multimodal: bool = False,
) -> tuple[RetrievalService, ProbeDatabase, ProbeVectorStore]:
    dataset = {
        "dataset_id": "dataset-a",
        "tenant_id": "tenant-a",
        "collection_name": "collection-a",
        "content_revision": 1,
        "embedding_provider": "local",
        "embedding_model": "hash-384",
        "embedding_dimension": 384,
        "embedding_config": {},
        "index_config": index_config,
    }

    async def require_dataset_access(
        _user: Any,
        _dataset_id: str,
        *,
        required: str = "viewer",
    ) -> dict[str, Any]:
        assert required == "viewer"
        return dict(dataset)

    async def get_presigned_image_url(_raw_url: str, _segment_id: str) -> None:
        return None

    database = ProbeDatabase(rows)
    vector_store = ProbeVectorStore()
    service = RetrievalService(
        SimpleNamespace(
            knowledge=SimpleNamespace(
                retrieval_query_max_concurrency=2,
                dashscope=SimpleNamespace(api_key="server-rerank-key"),
            )
        ),
        database,  # type: ignore[arg-type]
    )
    service.vector_store = vector_store  # type: ignore[assignment]
    service._ks = SimpleNamespace(
        require_dataset_access=require_dataset_access,
        _resolve_fusion_config=lambda **kwargs: {
            "method": "rrf",
            "dense_weight": kwargs.get("dense_weight") or 0.5,
            "bm25_weight": kwargs.get("bm25_weight") or 0.5,
            "rrf_k": 60,
        },
        _is_multimodal_dataset=lambda _dataset: is_multimodal,
        _should_apply_score_threshold=lambda _mode: False,
        _filter_candidates_by_metadata=lambda candidates, source_type, language, metadata: (
            KnowledgeService._filter_candidates_by_metadata(
                None,
                candidates,
                source_type,
                language,
                metadata,
            )
        ),
        _get_presigned_image_url=get_presigned_image_url,
        _normalize_local_image_url=lambda raw_url, _segment_id: raw_url,
    )
    return service, database, vector_store


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "legacy_rerank",
    [
        {"enabled": True, "api_key": "row-key"},
        {"enabled": True, "key": "row-key"},
        {"enabled": True, "base_url": "http://127.0.0.1:8092/private"},
        {"enabled": True, "endpoint": "http://169.254.169.254/latest/meta-data"},
        {"enabled": True, "nested": [{"api_base": "https://attacker.invalid/v1"}]},
        {"enabled": True, "provider_config": {"openai_api_key": "row-key"}},
        {"enabled": True, "request_headers": {"X-Token": "row-token"}},
        {"enabled": True, "provider_config": {"access_token": "row-token"}},
        {"enabled": True, "credentials": {"token": "row-token"}},
    ],
)
async def test_legacy_rerank_config_is_rejected_before_authority_or_factory(
    monkeypatch: pytest.MonkeyPatch,
    legacy_rerank: dict[str, Any],
) -> None:
    factory_calls = 0

    def forbidden_factory(*_args: Any, **_kwargs: Any) -> None:
        nonlocal factory_calls
        factory_calls += 1
        raise AssertionError("reranker factory must not receive legacy row config")

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker.create_reranker",
        forbidden_factory,
    )
    service, database, vector_store = _retrieval_service(
        index_config={"retrieval": {"mode": "bm25", "rerank": legacy_rerank}}
    )

    with pytest.raises(ValidationFailedError, match="legacy credential or endpoint"):
        await service.retrieve(
            user=SimpleNamespace(),
            dataset_id="dataset-a",
            query="query",
            mode="bm25",
            mmr=False,
        )

    assert factory_calls == 0
    assert database.search_calls == 0
    assert vector_store.authority_calls == 0


@pytest.mark.asyncio
@pytest.mark.parametrize("entrypoint", ["standard", "batch", "v2"])
@pytest.mark.parametrize(
    "poisoned_retrieval",
    [
        pytest.param({"top_k": 1_000_000_000}, id="top-k-billion"),
        pytest.param({"vector_top_k": 1_000_000_000}, id="vector-top-k-billion"),
        pytest.param({"vector_top_k": "1000"}, id="vector-top-k-string"),
        pytest.param({"keyword_top_k": float("inf")}, id="keyword-top-k-inf"),
        pytest.param({"keyword_top_k": True}, id="keyword-top-k-bool"),
        pytest.param({"candidate_top_k": float("nan")}, id="candidate-top-k-nan"),
        pytest.param({"keyword_candidate_k": 501}, id="keyword-pool-over-limit"),
        pytest.param({"rerank_top_n": 1001}, id="rerank-top-n-over-limit"),
        pytest.param({"rrf_k": 10_001}, id="rrf-k-over-limit"),
        pytest.param({"vector": {"top_k": float("inf")}}, id="nested-vector-inf"),
        pytest.param(
            {"keyword": {"top_k": 1_000_000_000}},
            id="nested-keyword-billion",
        ),
        pytest.param(
            {"keyword": {"candidate_pool_size": 1_000_000_000}},
            id="nested-keyword-pool-billion",
        ),
        pytest.param({"fusion": {"rrf_k": float("nan")}}, id="nested-rrf-nan"),
        pytest.param({"rerank": {"top_n": float("inf")}}, id="nested-rerank-inf"),
        pytest.param({"alpha": float("nan")}, id="alpha-nan"),
        pytest.param({"alpha": "0.5"}, id="alpha-string"),
        pytest.param({"dense_weight": float("inf")}, id="dense-weight-inf"),
        pytest.param({"dense_weight": False}, id="dense-weight-bool"),
        pytest.param({"bm25_weight": -0.1}, id="bm25-weight-negative"),
        pytest.param({"score_threshold": float("inf")}, id="threshold-inf"),
        pytest.param({"mmr_lambda": float("nan")}, id="mmr-lambda-nan"),
        pytest.param(
            {"mmr": {"similarity_threshold": 1.1}},
            id="nested-mmr-threshold-over-limit",
        ),
        pytest.param(
            {"fusion": {"rrf_weights": {"vector": float("inf")}}},
            id="nested-rrf-weight-inf",
        ),
        pytest.param(
            {"rrf_weights": {"vector": 101.0}},
            id="rrf-weight-over-limit",
        ),
        pytest.param(
            {"rrf_weights": {"vector": "1.0"}},
            id="rrf-weight-string",
        ),
        pytest.param(
            {"fusion": {"dense_weight": 1.1}},
            id="nested-fusion-weight-over-limit",
        ),
        pytest.param(
            {"dense_weight": 0.0, "bm25_weight": 0.0},
            id="all-fusion-weights-zero",
        ),
    ],
)
async def test_legacy_retrieval_resource_poison_is_rejected_before_dependencies(
    monkeypatch: pytest.MonkeyPatch,
    poisoned_retrieval: dict[str, Any],
    entrypoint: str,
) -> None:
    embedding_factory_calls = 0
    reranker_factory_calls = 0

    async def forbidden_embedding_factory(*_args: Any, **_kwargs: Any) -> None:
        nonlocal embedding_factory_calls
        embedding_factory_calls += 1
        raise AssertionError("embedding factory must not receive persisted resource poison")

    def forbidden_reranker_factory(*_args: Any, **_kwargs: Any) -> None:
        nonlocal reranker_factory_calls
        reranker_factory_calls += 1
        raise AssertionError("reranker factory must not receive persisted resource poison")

    monkeypatch.setattr(
        retrieval_service_module,
        "get_cached_embedder",
        forbidden_embedding_factory,
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker.create_reranker",
        forbidden_reranker_factory,
    )
    service, database, vector_store = _retrieval_service(
        index_config={"retrieval": poisoned_retrieval}
    )
    user = SimpleNamespace(user_id="user-a")

    with pytest.raises(ValidationFailedError, match="stored retrieval config"):
        if entrypoint == "standard":
            await service.retrieve(
                user=user,
                dataset_id="dataset-a",
                query="query",
                mode="dense",
                rerank=True,
                mmr=False,
            )
        elif entrypoint == "batch":
            await service.retrieve_batch(
                user=user,
                dataset_id="dataset-a",
                queries=["query"],
                mode="dense",
                rerank=True,
                mmr=False,
            )
        else:
            await service.retrieve_with_images_v2(
                user=user,
                dataset_id="dataset-a",
                query="query",
                include_images=False,
                vlm_rerank=False,
                mode="dense",
                rerank=True,
                mmr=False,
            )

    assert embedding_factory_calls == 0
    assert reranker_factory_calls == 0
    assert database.search_calls == 0
    assert vector_store.authority_calls == 0


@pytest.mark.asyncio
async def test_persisted_retrieval_resource_limits_preserve_documented_boundaries() -> None:
    service, database, vector_store = _retrieval_service(
        index_config={
            "retrieval": {
                "top_k": 100,
                "vector_top_k": 1000,
                "keyword_top_k": 1000,
                "candidate_top_k": 2000,
                "keyword_candidate_k": 500,
                "rerank_top_n": 1000,
                "rrf_k": 10_000,
                "dense_weight": 1.0,
                "bm25_weight": 0.0,
                "alpha": 1.0,
                "score_threshold": 1.0,
                "mmr_lambda": 0.0,
                "mmr_threshold": 1.0,
                "rrf_weights": {"vector": 100.0, "keyword": 0.0},
                "vector": {"top_k": 1000, "score_threshold": 1.0},
                "keyword": {"top_k": 1000, "candidate_pool_size": 500},
                "fusion": {
                    "rrf_k": 10_000,
                    "dense_weight": 1.0,
                    "bm25_weight": 0.0,
                    "alpha": 1.0,
                    "rrf_weights": {"vector": 100.0, "keyword": 0.0},
                },
                "rerank": {"enabled": False, "top_n": 1000, "score_threshold": 1.0},
                "mmr": {
                    "enabled": False,
                    "lambda": 1.0,
                    "similarity_threshold": 0.0,
                },
            }
        },
        rows=[
            {
                "segment_id": "text-segment",
                "document_id": "document-a",
                "text": "query text",
                "metadata": {"content_type": "text"},
            }
        ],
    )

    results, _meta = await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="dataset-a",
        query="query",
        mode="bm25",
        rerank=False,
        mmr=False,
        top_k=5,
    )

    assert [result.segment_id for result in results] == ["text-segment"]
    assert database.search_calls == 1
    assert vector_store.authority_calls == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("entrypoint", ["standard", "batch", "v2"])
@pytest.mark.parametrize(
    "request_poison",
    [
        pytest.param({"query": "q" * 4097}, id="query-too-long"),
        pytest.param({"query": 7}, id="query-not-string"),
        pytest.param({"top_k": 101}, id="top-k-over-limit"),
        pytest.param({"top_k": "5"}, id="top-k-string"),
        pytest.param({"top_k": True}, id="top-k-bool"),
        pytest.param({"vector_top_k": float("inf")}, id="vector-top-k-inf"),
        pytest.param({"keyword_top_k": 1_000_000_000}, id="keyword-top-k-billion"),
        pytest.param({"candidate_top_k": float("nan")}, id="candidate-top-k-nan"),
        pytest.param({"keyword_candidate_k": 501}, id="keyword-pool-over-limit"),
        pytest.param({"rrf_k": 10_001}, id="rrf-k-over-limit"),
        pytest.param({"rerank_top_n": 1001}, id="rerank-top-n-over-limit"),
        pytest.param({"alpha": "0.5"}, id="alpha-string"),
        pytest.param({"dense_weight": float("inf")}, id="dense-weight-inf"),
        pytest.param({"score_threshold": float("nan")}, id="threshold-nan"),
        pytest.param({"mmr_lambda": 1.1}, id="mmr-lambda-over-limit"),
        pytest.param(
            {"rrf_weights": {"vector": "1.0"}},
            id="rrf-weight-string",
        ),
    ],
)
async def test_direct_retrieval_resource_poison_is_rejected_before_dependencies(
    monkeypatch: pytest.MonkeyPatch,
    request_poison: dict[str, Any],
    entrypoint: str,
) -> None:
    dataset_access_calls = 0
    embedding_factory_calls = 0
    reranker_factory_calls = 0

    async def forbidden_dataset_access(*_args: Any, **_kwargs: Any) -> None:
        nonlocal dataset_access_calls
        dataset_access_calls += 1
        raise AssertionError("direct request validation must precede dataset access")

    async def forbidden_embedding_factory(*_args: Any, **_kwargs: Any) -> None:
        nonlocal embedding_factory_calls
        embedding_factory_calls += 1
        raise AssertionError("embedding factory must not receive request resource poison")

    def forbidden_reranker_factory(*_args: Any, **_kwargs: Any) -> None:
        nonlocal reranker_factory_calls
        reranker_factory_calls += 1
        raise AssertionError("reranker factory must not receive request resource poison")

    monkeypatch.setattr(
        retrieval_service_module,
        "get_cached_embedder",
        forbidden_embedding_factory,
    )
    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker.create_reranker",
        forbidden_reranker_factory,
    )
    service, database, vector_store = _retrieval_service(index_config={})
    service._ks.require_dataset_access = forbidden_dataset_access
    user = SimpleNamespace(user_id="user-a")
    request_kwargs = {"query": "query", "top_k": 5, **request_poison}

    with pytest.raises(ValidationFailedError, match="retrieval"):
        if entrypoint == "standard":
            await service.retrieve(
                user=user,
                dataset_id="dataset-a",
                mode="dense",
                rerank=True,
                mmr=False,
                **request_kwargs,
            )
        elif entrypoint == "batch":
            batch_query = request_kwargs.pop("query")
            batch_top_k = request_kwargs.pop("top_k")
            await service.retrieve_batch(
                user=user,
                dataset_id="dataset-a",
                queries=[batch_query],
                top_k=batch_top_k,
                mode="dense",
                rerank=True,
                mmr=False,
                **request_kwargs,
            )
        else:
            await service.retrieve_with_images_v2(
                user=user,
                dataset_id="dataset-a",
                include_images=False,
                vlm_rerank=False,
                mode="dense",
                rerank=True,
                mmr=False,
                **request_kwargs,
            )

    assert dataset_access_calls == 0
    assert embedding_factory_calls == 0
    assert reranker_factory_calls == 0
    assert database.search_calls == 0
    assert database.association_calls == 0
    assert vector_store.authority_calls == 0


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "batch_poison",
    [
        pytest.param({"queries": []}, id="empty-queries"),
        pytest.param({"queries": ["query"] * 21}, id="too-many-queries"),
        pytest.param({"queries": ["q" * 4097]}, id="query-too-long"),
        pytest.param(
            {"queries": [{"query": "query", "vector_top_k": 1_000_000_000}]},
            id="per-query-resource-poison",
        ),
        pytest.param({"max_parallel": 11}, id="parallel-over-limit"),
        pytest.param({"max_parallel": "10"}, id="parallel-string"),
        pytest.param({"max_parallel": True}, id="parallel-bool"),
    ],
)
async def test_direct_batch_shape_is_rejected_before_dependencies(
    batch_poison: dict[str, Any],
) -> None:
    dataset_access_calls = 0

    async def forbidden_dataset_access(*_args: Any, **_kwargs: Any) -> None:
        nonlocal dataset_access_calls
        dataset_access_calls += 1
        raise AssertionError("batch request validation must precede dataset access")

    service, database, vector_store = _retrieval_service(index_config={})
    service._ks.require_dataset_access = forbidden_dataset_access
    request_kwargs = {"queries": ["query"], "max_parallel": 10, **batch_poison}

    with pytest.raises(ValidationFailedError, match="retrieval batch"):
        await service.retrieve_batch(
            user=SimpleNamespace(user_id="user-a"),
            dataset_id="dataset-a",
            top_k=5,
            mode="dense",
            **request_kwargs,
        )

    assert dataset_access_calls == 0
    assert database.search_calls == 0
    assert database.association_calls == 0
    assert vector_store.authority_calls == 0


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("include_images", "vlm_rerank", "intent"),
    [
        (True, False, "general"),
        (False, True, "general"),
        (False, False, "find_image"),
    ],
)
async def test_v2_multimodal_options_are_rejected_before_dependencies(
    include_images: bool,
    vlm_rerank: bool,
    intent: str,
) -> None:
    dataset_access_calls = 0

    async def forbidden_dataset_access(*_args: Any, **_kwargs: Any) -> None:
        nonlocal dataset_access_calls
        dataset_access_calls += 1
        raise AssertionError("multimodal gate must precede dataset access")

    service, database, vector_store = _retrieval_service(index_config={})
    service._ks.require_dataset_access = forbidden_dataset_access

    with pytest.raises(ValidationFailedError, match="multimodal retrieval is unavailable"):
        await service.retrieve_with_images_v2(
            user=SimpleNamespace(user_id="user-a"),
            dataset_id="dataset-a",
            query="query",
            top_k=5,
            include_images=include_images,
            vlm_rerank=vlm_rerank,
            intent=intent,
        )

    assert dataset_access_calls == 0
    assert database.search_calls == 0
    assert database.association_calls == 0
    assert vector_store.authority_calls == 0


@pytest.mark.asyncio
async def test_v1_multimodal_entrypoint_is_dormant_before_dependencies() -> None:
    dataset_access_calls = 0

    async def forbidden_dataset_access(*_args: Any, **_kwargs: Any) -> None:
        nonlocal dataset_access_calls
        dataset_access_calls += 1
        raise AssertionError("multimodal gate must precede dataset access")

    service, database, vector_store = _retrieval_service(index_config={})
    service._ks.require_dataset_access = forbidden_dataset_access

    with pytest.raises(ValidationFailedError, match="multimodal retrieval is unavailable"):
        await service.retrieve_with_images(
            user=SimpleNamespace(user_id="user-a"),
            dataset_id="dataset-a",
            query="query",
            top_k=5,
        )

    assert dataset_access_calls == 0
    assert database.search_calls == 0
    assert database.association_calls == 0
    assert vector_store.authority_calls == 0


@pytest.mark.asyncio
@pytest.mark.parametrize("entrypoint", ["standard", "batch", "v2"])
async def test_legacy_multimodal_dataset_is_rejected_before_network_or_factory(
    entrypoint: str,
) -> None:
    factory_calls = 0

    def forbidden_factory(*_args: Any, **_kwargs: Any) -> None:
        nonlocal factory_calls
        factory_calls += 1
        raise AssertionError("multimodal factory must not run in this release")

    service, database, vector_store = _retrieval_service(
        index_config={},
        is_multimodal=True,
    )
    service._ks._get_unified_multimodal_embedder = forbidden_factory
    user = SimpleNamespace(user_id="user-a")

    with pytest.raises(ValidationFailedError, match="unavailable in this release"):
        if entrypoint == "standard":
            await service.retrieve(
                user=user,
                dataset_id="dataset-a",
                query="query",
                mode="bm25",
                rerank=False,
                mmr=False,
            )
        elif entrypoint == "batch":
            await service.retrieve_batch(
                user=user,
                dataset_id="dataset-a",
                queries=["query"],
                mode="bm25",
                rerank=False,
                mmr=False,
            )
        else:
            await service.retrieve_with_images_v2(
                user=user,
                dataset_id="dataset-a",
                query="query",
                include_images=False,
                vlm_rerank=False,
                mode="bm25",
                rerank=False,
                mmr=False,
            )

    assert factory_calls == 0
    assert database.search_calls == 0
    assert vector_store.authority_calls == 0


@pytest.mark.asyncio
async def test_text_profile_filters_legacy_image_points() -> None:
    service, database, _vector_store = _retrieval_service(
        index_config={},
        rows=[
            {
                "segment_id": "legacy-image",
                "document_id": "document-a",
                "text": "query image",
                "metadata": {
                    "content_type": "image",
                    "image_url": "http://127.0.0.1/private",
                },
            },
            {
                "segment_id": "legacy-page-image",
                "document_id": "document-a",
                "text": "query page image",
                "metadata": {"content_type": "page_image"},
            },
            {
                "segment_id": "legacy-mixed",
                "document_id": "document-a",
                "text": "query mixed",
                "metadata": {"metadata": {"content_type": "mixed"}},
            },
            {
                "segment_id": "text-segment",
                "document_id": "document-a",
                "text": "query text",
                "metadata": {"content_type": "text"},
            },
        ],
    )

    results, meta = await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="dataset-a",
        query="query",
        mode="bm25",
        rerank=False,
        mmr=False,
        top_k=5,
    )

    assert database.search_calls == 1
    assert [result.segment_id for result in results] == ["text-segment"]
    assert meta["legacy_image_candidates_filtered"] == 3
