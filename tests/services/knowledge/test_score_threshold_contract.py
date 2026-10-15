"""Tenant score-threshold placement (PRD T2-4, runbook §4.6-6, Dify #35233).

The threshold is calibrated against absolute scores (dense cosine or rerank
relevance). It must never pre-filter the legs, must govern final results
post-rerank once a rerank is served, and must be SKIPPED (explicitly, with a
machine-readable reason) when final scores are only on the relative fusion
scale — the old code silently zeroed it for every non-dense mode, so hybrid
tenants never got their threshold honored even after rerank.
"""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest
from knowledge_service.services.knowledge.text_reranker import RerankResult

from tests.services.knowledge.test_rerank_budget_contract import (
    _BM25_ROWS,
    _install_reranker,
    _patch_budget,
)
from tests.services.knowledge.test_retrieve_batch import _make_bm25_service


async def _retrieve(svc, **overrides):
    kwargs = {
        "user": SimpleNamespace(),
        "dataset_id": "kb-demo",
        "query": "alpha",
        "top_k": 2,
        "mode": "bm25",
        "mmr": False,
        "score_threshold": 0.5,
    }
    kwargs.update(overrides)
    return await svc.retrieve(**kwargs)


@pytest.mark.asyncio
async def test_rerank_scored_finals_are_thresholded_post_rerank(
    monkeypatch: pytest.MonkeyPatch,
):
    svc, _database = _make_bm25_service(_BM25_ROWS)
    _patch_budget(monkeypatch, 5.0)

    async def _reranker(**_kwargs):
        return [
            RerankResult(index=0, relevance_score=0.9),
            RerankResult(index=1, relevance_score=0.3),
        ]

    _install_reranker(monkeypatch, _reranker)

    results, meta = await _retrieve(svc, rerank=True, rerank_model="bge-reranker-v2-m3")

    assert [result.segment_id for result in results] == ["seg-a"]
    assert meta["score_threshold"] == 0.5
    assert "score_threshold_skipped" not in meta


@pytest.mark.asyncio
async def test_untouched_rerank_tail_is_exempt_from_the_rerank_scale_filter(
    monkeypatch: pytest.MonkeyPatch,
):
    svc, _database = _make_bm25_service(_BM25_ROWS)
    _patch_budget(monkeypatch, 5.0)

    async def _reranker(**_kwargs):
        # Provider returns fewer than asked: seg-b keeps its fusion-scale
        # final score and must not be judged on the rerank scale.
        return [RerankResult(index=0, relevance_score=0.9)]

    _install_reranker(monkeypatch, _reranker)

    results, meta = await _retrieve(svc, rerank=True, rerank_model="bge-reranker-v2-m3")

    assert [result.segment_id for result in results] == ["seg-a", "seg-b"]
    assert meta["score_threshold"] == 0.5


@pytest.mark.asyncio
async def test_degraded_rerank_skips_threshold_with_machine_readable_reason(
    monkeypatch: pytest.MonkeyPatch,
):
    svc, _database = _make_bm25_service(_BM25_ROWS)
    _patch_budget(monkeypatch, 0.3)

    async def _slow_reranker(**_kwargs):
        await asyncio.sleep(5)
        return []

    _install_reranker(monkeypatch, _slow_reranker)

    results, meta = await _retrieve(svc, rerank=True, rerank_model="bge-reranker-v2-m3")

    assert meta["rerank_degraded"] == "timeout"
    # Fusion-scale finals: the absolute threshold is undefined, skip + say so.
    assert meta["score_threshold_skipped"] == "uncalibrated_final_score"
    assert meta.get("score_threshold") is None
    # Fusion order keeps serving, nothing silently filtered away.
    assert [result.segment_id for result in results] == ["seg-a", "seg-b"]


@pytest.mark.asyncio
async def test_hybrid_without_rerank_skips_threshold_explicitly(
    monkeypatch: pytest.MonkeyPatch,
):
    svc, _database = _make_bm25_service(_BM25_ROWS)
    _patch_budget(monkeypatch, 5.0)

    results, meta = await _retrieve(svc, rerank=False)

    assert meta["score_threshold_skipped"] == "uncalibrated_final_score"
    assert "rerank_degraded" not in meta
    assert [result.segment_id for result in results] == ["seg-a", "seg-b"]


@pytest.mark.asyncio
async def test_no_tenant_threshold_records_neither_application_nor_skip(
    monkeypatch: pytest.MonkeyPatch,
):
    svc, _database = _make_bm25_service(_BM25_ROWS)

    _results, meta = await _retrieve(svc, rerank=False, score_threshold=None)

    assert "score_threshold_skipped" not in meta
    assert meta.get("score_threshold") is None
