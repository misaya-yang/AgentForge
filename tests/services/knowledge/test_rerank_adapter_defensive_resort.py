"""Addendum §1-T2-3 pin: reranker adapters defensively re-sort by returned score.

The addendum's bake-off requirement ("重排返回后防御性重施 … provider 插件过滤
行为不一致") is satisfied today by defense-in-depth: the pipeline re-applies
threshold + top_k, and every shipped adapter re-sorts provider output by the
returned relevance score descending before it reaches the pipeline (provider
plugins must not be trusted to return ranked order). These tests pin the
adapter half of that contract against deliberately UNSORTED provider
responses so a refactor cannot silently drop the sort.
"""

import httpx
import pytest
from knowledge_service.services.knowledge.text_reranker import (
    AsyncTextReranker,
    _provider_failure_state,
    _provider_success_state,
    _rerank_cache,
)


def _install_response(monkeypatch: pytest.MonkeyPatch, results: list[dict]):
    class _Client:
        async def post(self, url, **_kwargs):
            return httpx.Response(
                200,
                request=httpx.Request("POST", url),
                json={"results": results},
            )

    async def _client():
        return _Client()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker._get_http_client",
        _client,
    )


@pytest.mark.asyncio
async def test_dashscope_legacy_provider_unsorted_output_is_resorted(
    monkeypatch: pytest.MonkeyPatch,
):
    """A legacy-schema provider returning results in ascending score order must
    still yield descending-ranked adapter output (score space is trusted only
    for ordering, never for arrival order)."""
    _rerank_cache.clear()
    _provider_failure_state.clear()
    _provider_success_state.clear()
    _install_response(
        monkeypatch,
        [
            {"index": 0, "relevance_score": 0.11},
            {"index": 2, "relevance_score": 0.97},
            {"index": 1, "relevance_score": 0.55},
        ],
    )
    reranker = AsyncTextReranker(
        api_key="test-key",
        model="gte-rerank-v2",
        base_url=(
            "https://dashscope.aliyuncs.com/"
            "api/v1/services/rerank/text-rerank/text-rerank"
        ),
    )

    results = await reranker.rerank("query", ["a", "b", "c"])

    assert [r.index for r in results] == [2, 1, 0]
    scores = [r.relevance_score for r in results]
    assert scores == sorted(scores, reverse=True)


@pytest.mark.asyncio
async def test_dashscope_flat_provider_unsorted_output_is_resorted(
    monkeypatch: pytest.MonkeyPatch,
):
    """Same contract for the flat (qwen3) response schema."""
    _rerank_cache.clear()
    _provider_failure_state.clear()
    _provider_success_state.clear()
    _install_response(
        monkeypatch,
        [
            {"index": 3, "relevance_score": 0.02},
            {"index": 1, "relevance_score": 0.44},
            {"index": 0, "relevance_score": 0.88},
            {"index": 2, "relevance_score": 0.66},
        ],
    )
    reranker = AsyncTextReranker(
        api_key="test-key",
        model="qwen3-rerank",
        base_url=(
            "https://workspace.ap-southeast-1.maas.aliyuncs.com/"
            "compatible-api/v1/reranks"
        ),
    )

    results = await reranker.rerank("query", ["a", "b", "c", "d"])

    assert [r.index for r in results] == [0, 2, 1, 3]
    scores = [r.relevance_score for r in results]
    assert scores == sorted(scores, reverse=True)


@pytest.mark.asyncio
async def test_resorted_order_survives_the_rerank_cache(
    monkeypatch: pytest.MonkeyPatch,
):
    """The cache stores the defensively sorted list, so a cache hit returns
    the same ranking as the live call (addendum: order must not depend on
    provider arrival order on either path)."""
    _rerank_cache.clear()
    _provider_failure_state.clear()
    _provider_success_state.clear()
    _install_response(
        monkeypatch,
        [
            {"index": 0, "relevance_score": 0.30},
            {"index": 1, "relevance_score": 0.75},
        ],
    )
    reranker = AsyncTextReranker(
        api_key="test-key",
        model="gte-rerank-v2",
        base_url=(
            "https://dashscope.aliyuncs.com/"
            "api/v1/services/rerank/text-rerank/text-rerank"
        ),
    )

    live = await reranker.rerank("query", ["a", "b"])
    cached = await reranker.rerank("query", ["a", "b"])

    assert [r.index for r in live] == [1, 0]
    assert [(r.index, r.relevance_score) for r in cached] == [
        (r.index, r.relevance_score) for r in live
    ]
