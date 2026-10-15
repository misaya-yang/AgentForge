"""Integration test for the KB retrieval evaluation endpoint.

Calls the ``retrieve_evaluate`` route handler directly with a fake
KnowledgeService whose ``retrieve`` returns deterministic ranked lists, and
asserts the aggregated IR metrics are computed and wired correctly. This is the
backend primitive behind the KB retrieval evaluation workbench (A/B of two
retrieval configs on the same labelled queries).
"""

from __future__ import annotations

import asyncio
import math
from types import SimpleNamespace

import pytest
from fastapi import HTTPException
from knowledge_service.api.routes import knowledge as knowledge_routes
from knowledge_service.api.routes.knowledge import (
    list_retrieval_presets,
    retrieve,
    retrieve_evaluate,
)
from knowledge_service.api.schemas.knowledge import (
    RetrievalEvalCaseSchema,
    RetrievalEvalRequestSchema,
    RetrieveRequestSchema,
)
from knowledge_service.core.exceptions import PermissionDeniedError
from knowledge_service.services.knowledge.retrieval_config import DEFAULT_CONFIGS
from pydantic import ValidationError


def _authenticated_user(**overrides):
    values = {
        "user_id": "u1",
        "tenant_id": "t1",
        "is_authenticated": True,
        "is_anonymous": False,
        "user_type": "user",
        "roles": ["admin"],
    }
    values.update(overrides)
    return SimpleNamespace(**values)


def _result(
    sid: str,
    score: float,
    *,
    metadata: dict | None = None,
    content_type: str = "text",
) -> SimpleNamespace:
    return SimpleNamespace(
        segment_id=sid,
        document_id=f"doc-{sid}",
        score=score,
        text=f"text for {sid}",
        metadata=metadata or {},
        content_type=content_type,
    )


class FakeKnowledgeService:
    """Returns a canned ranked list per query, mirroring svc.retrieve's shape."""

    def __init__(
        self,
        ranked_by_query: dict[str, list[tuple[str, float]]],
        *,
        retrieval_metadata: dict | None = None,
    ):
        self.ranked_by_query = ranked_by_query
        self.retrieval_metadata = retrieval_metadata or {}
        self.calls: list[dict] = []
        self.access_calls: list[tuple] = []

    async def require_dataset_access(self, user, dataset_id, required="viewer"):
        self.access_calls.append((user, dataset_id, required))
        return {"dataset_id": dataset_id}

    async def retrieve(self, **kwargs):
        self.calls.append(kwargs)
        ranked = self.ranked_by_query[kwargs["query"]]
        metadata = {"mode": kwargs.get("mode"), **self.retrieval_metadata}
        return [_result(sid, score) for sid, score in ranked], metadata


async def test_retrieve_evaluate_computes_metrics():
    svc = FakeKnowledgeService(
        {
            "q1": [("a", 0.9), ("c", 0.7), ("b", 0.5)],  # a,c relevant -> strong
            "q2": [("x", 0.9), ("y", 0.5)],  # a relevant but missed
        }
    )
    payload = RetrievalEvalRequestSchema(
        mode="hybrid",
        rerank=True,
        cases=[
            RetrievalEvalCaseSchema(query="q1", case_id="c1", relevant_segment_ids=["a", "c"]),
            RetrievalEvalCaseSchema(query="q2", case_id="c2", relevant_segment_ids=["a"]),
        ],
        k_values=[1, 3],
    )

    user = _authenticated_user()
    resp = await retrieve_evaluate("ds1", payload, svc, user)

    # Retrieval config is threaded through to every case (A/B comparable).
    assert len(svc.calls) == 2
    assert all(call["mode"] == "hybrid" and call["rerank"] is True for call in svc.calls)
    # Fetch depth is max(max(k_values), top_k) = 5 by default top_k.
    assert all(call["top_k"] == 5 for call in svc.calls)
    assert svc.access_calls == [(user, "ds1", "editor")]

    assert resp["dataset_id"] == "ds1"
    assert resp["num_cases"] == 2
    assert resp["k_values"] == [1, 3]

    # hit_rate@1: q1 top="a" relevant ->1.0; q2 top="x" not ->0.0 => 0.5
    assert resp["metrics"]["1"]["hit_rate"] == 0.5
    # recall@3: q1 finds {a,c} ->1.0; q2 finds none ->0.0 => 0.5
    assert resp["metrics"]["3"]["recall_at_k"] == 0.5
    # MRR: q1 rr=1.0 (a at rank1), q2 rr=0.0 => 0.5
    assert resp["primary_metrics"]["mrr"] == 0.5
    # nDCG@3 for q1 with a,c at ranks 1,2 is perfect => 1.0; q2 => 0.0 => 0.5
    assert resp["metrics"]["3"]["ndcg_at_k"] == 0.5

    # Per-case detail: ranked lists with relevance flags.
    assert len(resp["cases"]) == 2
    case1 = resp["cases"][0]
    assert case1["case_id"] == "c1"
    assert case1["retrieved"][0]["segment_id"] == "a"
    assert case1["retrieved"][0]["relevant"] is True
    assert case1["retrieved"][2]["relevant"] is False  # "b" not relevant

    # Per-query metric breakdown present.
    assert "c1" in resp["per_query"]
    assert resp["per_query"]["c1"]["reciprocal_rank"] == 1.0


async def test_retrieve_evaluate_graded_relevance():
    svc = FakeKnowledgeService({"q": [("a", 0.9), ("b", 0.5)]})
    payload = RetrievalEvalRequestSchema(
        cases=[
            RetrievalEvalCaseSchema(
                query="q",
                case_id="g1",
                relevance={"a": 3.0, "b": 2.0, "c": 3.0},
            )
        ],
        k_values=[2],
        return_retrieved=False,
    )
    resp = await retrieve_evaluate("ds2", payload, svc, _authenticated_user())
    # nDCG@2 with grades a=3,b=2 retrieved vs ideal top2 (3,3) is < 1.
    ndcg = resp["metrics"]["2"]["ndcg_at_k"]
    assert 0.0 < ndcg < 1.0
    # return_retrieved=False omits the heavy per-case lists.
    assert "cases" not in resp
    assert resp["case_metadata"][0]["retrieval_metadata"]["pipeline"] == "standard"


async def test_preset_config_round_trips_into_executable_eval_request():
    presets = await list_retrieval_presets()
    balanced = next(item["config"] for item in presets["presets"] if item["name"] == "balanced")
    assert balanced == DEFAULT_CONFIGS["balanced"].to_dict()

    payload = RetrievalEvalRequestSchema.model_validate(
        {
            **balanced,
            "cases": [{"query": "q", "relevant_segment_ids": ["a"]}],
            "k_values": [5, 1, 5],
        }
    )

    assert payload.k_values == [1, 5]
    assert payload.fusion_method == "rrf"
    assert payload.rrf_k == 60
    assert payload.rrf_weights == {"vector": 1.0, "keyword": 1.0}
    assert payload.alpha == pytest.approx(0.6)
    assert payload.rerank is True
    assert payload.rerank_model == "qwen3-rerank"
    assert payload.mmr is False

    svc = FakeKnowledgeService({"q": [("a", 0.9)]})
    response = await retrieve_evaluate("ds-preset", payload, svc, _authenticated_user())

    assert response["case_metadata"][0]["retrieval_metadata"]["pipeline"] == "standard"
    assert svc.calls[0]["fusion_method"] == "rrf"
    assert svc.calls[0]["rrf_k"] == 60
    assert svc.calls[0]["rerank"] is True


@pytest.mark.parametrize("preset_name", sorted(DEFAULT_CONFIGS))
def test_every_builtin_preset_validates_as_an_eval_request(preset_name: str):
    payload = RetrievalEvalRequestSchema.model_validate(
        {
            **DEFAULT_CONFIGS[preset_name].to_dict(),
            "cases": [{"query": "q", "relevant_segment_ids": ["a"]}],
        }
    )

    assert payload.mode == DEFAULT_CONFIGS[preset_name].mode.value
    assert payload.top_k == DEFAULT_CONFIGS[preset_name].top_k


@pytest.mark.parametrize(
    "override",
    [
        {"top_k": 0},
        {"top_k": 101},
        {"k_values": [0]},
        {"k_values": [-1]},
        {"k_values": [101]},
        {"vector_top_k": -1},
        {"keyword_candidate_k": 501},
        {"alpha": math.inf},
        {"mmr_lambda": -0.1},
        {"image_boost": -1.0},
        {"rerank_model": "x" * 257},
        {"vector": {"top_k": 5, "unsupported": True}},
    ],
)
def test_eval_request_rejects_invalid_limits(override: dict):
    request = {
        "cases": [{"query": "q", "relevant_segment_ids": ["a"]}],
        **override,
    }
    with pytest.raises(ValidationError):
        RetrievalEvalRequestSchema.model_validate(request)


@pytest.mark.parametrize("query", ["", "   "])
def test_eval_request_rejects_blank_case_query(query: str):
    with pytest.raises(ValidationError):
        RetrievalEvalRequestSchema.model_validate({"cases": [{"query": query}]})


def test_eval_request_rejects_non_finite_grade_and_duplicate_case_ids():
    with pytest.raises(ValidationError):
        RetrievalEvalRequestSchema.model_validate(
            {"cases": [{"query": "q", "relevance": {"a": math.inf}}]}
        )
    with pytest.raises(ValidationError, match="duplicate case_id"):
        RetrievalEvalRequestSchema.model_validate(
            {
                "cases": [
                    {"query": "q1", "case_id": "same"},
                    {"query": "q2", "case_id": "same"},
                ]
            }
        )


def test_eval_request_caps_case_count_at_twenty():
    with pytest.raises(ValidationError):
        RetrievalEvalRequestSchema.model_validate(
            {"cases": [{"query": f"q-{index}"} for index in range(21)]}
        )


async def test_eval_requires_editor_permission():
    class DeniedService(FakeKnowledgeService):
        async def require_dataset_access(self, user, dataset_id, required="viewer"):
            _ = user, dataset_id, required
            raise PermissionDeniedError("editor required")

    svc = DeniedService({"q": [("a", 1.0)]})
    payload = RetrievalEvalRequestSchema.model_validate(
        {"cases": [{"query": "q", "relevant_segment_ids": ["a"]}]}
    )

    with pytest.raises(HTTPException) as exc_info:
        await retrieve_evaluate("ds", payload, svc, _authenticated_user())

    assert exc_info.value.status_code == 403
    assert svc.calls == []


async def test_eval_has_one_aggregate_service_deadline(monkeypatch: pytest.MonkeyPatch):
    class SlowService(FakeKnowledgeService):
        async def retrieve(self, **kwargs):
            self.calls.append(kwargs)
            await asyncio.sleep(0.05)
            return [], {}

    svc = SlowService({"q": []})
    payload = RetrievalEvalRequestSchema.model_validate(
        {"cases": [{"query": "q", "relevant_segment_ids": ["a"]}]}
    )
    monkeypatch.setattr(knowledge_routes, "RETRIEVAL_EVAL_TIMEOUT_SECONDS", 0.01)

    with pytest.raises(HTTPException) as exc_info:
        await retrieve_evaluate("ds", payload, svc, _authenticated_user())

    assert exc_info.value.status_code == 504
    assert len(svc.calls) == 1


async def test_eval_deduplicates_metrics_and_preserves_bounded_metadata():
    svc = FakeKnowledgeService(
        {"q": [("a", 0.9), ("a", 0.8)]},
        retrieval_metadata={
            "rerank_fallback": True,
            "rerank_error": "provider failed",
            "api_key": "must-not-leak",
        },
    )
    payload = RetrievalEvalRequestSchema.model_validate(
        {
            "cases": [{"query": "q", "relevant_segment_ids": ["a"]}],
            "k_values": [2],
        }
    )

    response = await retrieve_evaluate("ds", payload, svc, _authenticated_user())
    metrics = response["metrics"]["2"]
    assert all(0.0 <= value <= 1.0 for key, value in metrics.items() if key != "k")
    assert metrics["recall_at_k"] == 1.0
    assert metrics["precision_at_k"] == pytest.approx(0.5)

    evidence = response["case_metadata"][0]
    assert evidence["retrieved_count"] == 2
    assert evidence["unique_retrieved_count"] == 1
    assert evidence["duplicate_segment_ids"] == ["a"]
    assert evidence["retrieval_metadata"]["pipeline"] == "standard"
    assert evidence["retrieval_metadata"]["rerank_fallback"] is True
    assert evidence["retrieval_metadata"]["rerank_error"] == "provider failed"
    assert evidence["retrieval_metadata"]["api_key"] == "[redacted]"


async def test_eval_caps_provider_results_to_the_requested_window():
    svc = FakeKnowledgeService({"q": [(f"s{index}", 1.0 - index / 100) for index in range(8)]})
    payload = RetrievalEvalRequestSchema.model_validate(
        {
            "top_k": 1,
            "cases": [{"query": "q", "relevant_segment_ids": ["s0"]}],
            "k_values": [1],
        }
    )

    response = await retrieve_evaluate("ds", payload, svc, _authenticated_user())

    evidence = response["case_metadata"][0]
    assert evidence["provider_retrieved_count"] == 8
    assert evidence["retrieved_count"] == 1
    assert len(response["cases"][0]["retrieved"]) == 1
    assert evidence["retrieval_metadata"]["evaluation_window"] == {
        "requested_fetch_k": 1,
        "provider_retrieved_count": 8,
        "evaluated_retrieved_count": 1,
        "truncated": True,
    }


class FakeMultimodalKnowledgeService:
    def __init__(self):
        self.calls: list[dict] = []

    async def require_dataset_access(self, user, dataset_id, required="viewer"):
        _ = user, required
        return {"dataset_id": dataset_id}

    async def retrieve(self, **_kwargs):  # pragma: no cover - must not be selected
        raise AssertionError("standard retrieval branch selected")

    async def retrieve_with_images(self, **kwargs):
        self.calls.append(kwargs)
        return [_result("image-a", 0.8, content_type="image")], {"image_fallback": False}


async def test_eval_rejects_unreleased_multimodal_pipeline():
    svc = FakeMultimodalKnowledgeService()
    with pytest.raises(ValidationError, match="multimodal retrieval is not enabled"):
        RetrievalEvalRequestSchema.model_validate(
            {
                "include_associated_images": True,
                "include_images": True,
                "multimodal_rerank": True,
                "cases": [{"query": "q", "relevant_segment_ids": ["image-a"]}],
                "k_values": [1],
            }
        )

    assert svc.calls == []


class FakeHierarchicalKnowledgeService:
    class VectorStore:
        async def require_hierarchical_collections_readable(
            self,
            _collection_name,
            **_kwargs,
        ):
            return None

    class Database:
        def __init__(self):
            self.segment_filter_calls = 0

        async def filter_active_document_ids(self, *, document_ids, **_kwargs):
            return set(document_ids)

        async def filter_active_segment_ids(self, *, segment_ids, **_kwargs):
            self.segment_filter_calls += 1
            return set(segment_ids)

    vector_store = VectorStore()
    embedder = object()

    def __init__(self):
        self.access_calls: list[tuple] = []
        self.observation_calls: list[dict] = []
        self.db = self.Database()
        self.multimodal = False

    async def require_dataset_access(self, user, dataset_id, required="viewer"):
        self.access_calls.append((user, dataset_id, required))
        return {
            "dataset_id": dataset_id,
            "tenant_id": "t1",
            "content_revision": 1,
            "embedding_provider": "local",
            "embedding_model": "hash-384",
            "embedding_dimension": 384,
            "collection_name": "tenant_ds_384",
            "index_config": {},
        }

    def _is_multimodal_dataset(self, _dataset):
        return self.multimodal

    def _resolve_embedding_config(self, **_kwargs):
        return SimpleNamespace(provider="local", model="hash-384", extra={})

    def record_external_retrieval_observation(self, **kwargs):
        self.observation_calls.append(kwargs)
        return {
            **kwargs["meta"],
            "trace_id": "trace-test",
            "query_fingerprint": "fingerprint-test",
        }


async def test_eval_executes_hierarchical_pipeline_after_viewer_check(monkeypatch):
    from knowledge_service.services.knowledge import embedding, hierarchical_retriever

    calls: list[dict] = []

    async def fake_hierarchical_retrieve(**kwargs):
        calls.append(kwargs)
        result = _result("a", 0.9)
        # L1 summary IDs live in document_summaries, not segments. The route's
        # final lifecycle recheck must retain this result without consulting
        # segment authority.
        result.level = 1
        result.parent_context = "parent"
        result.document_summary = "summary"
        result.metadata = {
            "source_type": "file",
            "image_url": "https://private.invalid/image.png",
            "nested": {"associated_images": [{"storage_url": "s3://private"}]},
        }
        image_result = _result(
            "image-a",
            0.95,
            metadata={"content_type": "page_image", "image_url": "https://private.invalid"},
            content_type="image",
        )
        image_result.level = 1
        image_result.parent_context = None
        image_result.document_summary = None
        return [image_result, result], SimpleNamespace(
            strategy="cascade",
            l1_candidates=2,
            l2_candidates=1,
            l3_results=1,
            total_time_ms=3.5,
            filtered_documents=0,
        )

    async def fake_get_cached_embedder(_config, *, dimension=None):
        assert dimension == 384
        return svc.embedder

    monkeypatch.setattr(
        hierarchical_retriever,
        "hierarchical_retrieve",
        fake_hierarchical_retrieve,
    )
    svc = FakeHierarchicalKnowledgeService()
    monkeypatch.setattr(embedding, "get_cached_embedder", fake_get_cached_embedder)
    user = _authenticated_user()
    payload = RetrievalEvalRequestSchema.model_validate(
        {
            "hierarchical": True,
            "cases": [{"query": "q", "relevant_segment_ids": ["a"]}],
            "k_values": [1],
        }
    )

    response = await retrieve_evaluate("ds", payload, svc, user)

    assert svc.access_calls == [
        (user, "ds", "editor"),
        (user, "ds", "viewer"),
        (user, "ds", "viewer"),
    ]
    assert calls[0]["dataset_id"] == "ds"
    assert calls[0]["base_collection"] == "tenant_ds_384"
    assert response["case_metadata"][0]["retrieval_metadata"]["pipeline"] == "hierarchical"
    assert [item["segment_id"] for item in response["cases"][0]["retrieved"]] == ["a"]

    public_response = await retrieve(
        "ds",
        RetrieveRequestSchema(query="q", hierarchical=True, top_k=1),
        svc,
        user,
    )
    assert svc.access_calls == [
        (user, "ds", "editor"),
        (user, "ds", "viewer"),
        (user, "ds", "viewer"),
        (user, "ds", "viewer"),
        (user, "ds", "viewer"),
    ]
    assert public_response["results"][0]["segment_id"] == "a"
    assert public_response["results"][0]["metadata"] == {
        "source_type": "file",
        "nested": {},
    }
    assert public_response["metadata"]["strategy"] == "cascade"
    assert svc.observation_calls[0]["source"] == "hierarchical"
    assert svc.db.segment_filter_calls == 0


async def test_hierarchical_public_retrieval_rejects_legacy_multimodal_dataset():
    svc = FakeHierarchicalKnowledgeService()
    svc.multimodal = True
    user = _authenticated_user()

    with pytest.raises(HTTPException) as exc_info:
        await retrieve(
            "ds",
            RetrieveRequestSchema(query="q", hierarchical=True, top_k=1),
            svc,
            user,
        )

    assert exc_info.value.status_code == 400
    assert "multimodal dataset retrieval is not enabled" in str(exc_info.value.detail)
    assert svc.access_calls == [(user, "ds", "viewer")]
