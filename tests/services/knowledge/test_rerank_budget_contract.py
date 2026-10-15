"""Rerank budget coupling + explicit degrade flag (PRD T2-3, Phase 0 quick win).

The reranker's own ~30s HTTP timeout sits outside the 3s interactive budget.
These tests pin the coupling: every rerank call is capped by what remains of
the entrypoint budget, and degrade reasons are machine-readable
(``meta["rerank_degraded"]``) on top of the free-text ``rerank_error``.
Fusion order keeps serving whenever rerank degrades.
"""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest
from knowledge_service.services.knowledge import retrieval_service as retrieval_module
from knowledge_service.services.knowledge.retrieval_service import (
    _RERANK_MIN_BUDGET_SECONDS,
)
from knowledge_service.services.knowledge.text_reranker import RerankResult
from knowledge_service.services.knowledge.vector_store import (
    VectorStore,
    remaining_interactive_budget_seconds,
)

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


def _patch_budget(monkeypatch, remaining: float) -> None:
    monkeypatch.setattr(
        retrieval_module,
        "remaining_interactive_budget_seconds",
        lambda: remaining,
    )


def _install_reranker(monkeypatch, reranker) -> list[dict]:
    calls: list[dict] = []

    class _Probe:
        async def rerank(self, **kwargs):
            calls.append(kwargs)
            return await reranker(**kwargs)

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker.create_reranker",
        lambda **_kwargs: _Probe(),
    )
    return calls


async def _retrieve_with_rerank(svc) -> tuple[list, dict]:
    return await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=2,
        mode="bm25",
        rerank=True,
        rerank_model="bge-reranker-v2-m3",
        rerank_top_n=2,
        mmr=False,
    )


@pytest.mark.asyncio
async def test_rerank_timeout_within_budget_degrades_with_flag(monkeypatch):
    svc, _database = _make_bm25_service(_BM25_ROWS)
    _patch_budget(monkeypatch, 0.3)

    async def _slow_reranker(**_kwargs):
        await asyncio.sleep(5)
        return []

    calls = _install_reranker(monkeypatch, _slow_reranker)

    results, meta = await _retrieve_with_rerank(svc)

    assert len(calls) == 1  # rerank was attempted, then cut by the budget
    assert meta["rerank_degraded"] == "timeout"
    assert meta["rerank_error"]
    assert "rerank_applied_provider" not in meta
    # Fusion order still serves: no empty result on degrade.
    assert [result.segment_id for result in results] == ["seg-a", "seg-b"]
    assert meta["timings_ms"]["rerank_ms"] < 3000


@pytest.mark.asyncio
async def test_rerank_skipped_when_budget_already_exhausted(monkeypatch):
    svc, _database = _make_bm25_service(_BM25_ROWS)
    _patch_budget(monkeypatch, _RERANK_MIN_BUDGET_SECONDS / 2)

    async def _never_called(**_kwargs):
        raise AssertionError("rerank must not start without budget")

    calls = _install_reranker(monkeypatch, _never_called)

    results, meta = await _retrieve_with_rerank(svc)

    assert calls == []
    assert meta["rerank_degraded"] == "budget_exhausted"
    assert "only" in meta["rerank_error"]
    assert [result.segment_id for result in results] == ["seg-a", "seg-b"]


@pytest.mark.asyncio
async def test_rerank_provider_error_degrades_with_error_reason(monkeypatch):
    svc, _database = _make_bm25_service(_BM25_ROWS)
    _patch_budget(monkeypatch, 5.0)

    async def _exploding_reranker(**_kwargs):
        raise ValueError("provider exploded")

    _install_reranker(monkeypatch, _exploding_reranker)

    results, meta = await _retrieve_with_rerank(svc)

    assert meta["rerank_degraded"] == "error"
    assert "provider exploded" in meta["rerank_error"]
    assert [result.segment_id for result in results] == ["seg-a", "seg-b"]


@pytest.mark.asyncio
async def test_successful_rerank_sets_no_degrade_flag(monkeypatch):
    svc, _database = _make_bm25_service(_BM25_ROWS)
    _patch_budget(monkeypatch, 5.0)

    async def _fast_reranker(**_kwargs):
        return [RerankResult(index=1, relevance_score=0.9)]

    _install_reranker(monkeypatch, _fast_reranker)

    results, meta = await _retrieve_with_rerank(svc)

    assert "rerank_degraded" not in meta
    assert meta["rerank_applied_provider"]
    assert [result.segment_id for result in results][0] == "seg-b"


async def _slow_reranker(**_kwargs):
    await asyncio.sleep(5)
    return []


async def _never_called_reranker(**_kwargs):
    raise AssertionError("rerank must not start without budget")


async def _exploding_reranker(**_kwargs):
    raise ValueError("provider exploded")


class _RecordingMetrics:
    """Captures the degrade-counter wiring (review-metrics-v1 MAJOR #4).

    The pipeline calls ``_metrics.record_rerank_degraded`` at the same site
    that sets ``meta["rerank_degraded"]``; asserting only the meta left the
    metric side of that wiring unproven.
    """

    def __init__(self) -> None:
        self.degrades: list[object] = []
        self.retrievals: list[tuple[str, bool, float]] = []

    def record_retrieval(
        self, mode: object, *, cache_hit: bool, duration_seconds: float
    ) -> None:
        self.retrievals.append((str(mode), cache_hit, duration_seconds))

    def record_rerank_degraded(self, reason: object) -> None:
        self.degrades.append(reason)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("remaining", "reranker", "expected_reason"),
    [
        (0.3, _slow_reranker, "timeout"),
        (_RERANK_MIN_BUDGET_SECONDS / 2, _never_called_reranker, "budget_exhausted"),
        (5.0, _exploding_reranker, "error"),
    ],
)
async def test_rerank_degrade_fires_the_degraded_counter(
    monkeypatch: pytest.MonkeyPatch,
    remaining: float,
    reranker,
    expected_reason: str,
):
    svc, _database = _make_bm25_service(_BM25_ROWS)
    _patch_budget(monkeypatch, remaining)
    _install_reranker(monkeypatch, reranker)
    metrics = _RecordingMetrics()
    monkeypatch.setattr(retrieval_module, "_metrics", metrics)

    _results, meta = await _retrieve_with_rerank(svc)

    assert meta["rerank_degraded"] == expected_reason
    assert metrics.degrades == [expected_reason]


@pytest.mark.asyncio
async def test_successful_rerank_fires_no_degraded_counter(monkeypatch):
    svc, _database = _make_bm25_service(_BM25_ROWS)
    _patch_budget(monkeypatch, 5.0)

    async def _fast_reranker(**_kwargs):
        return [RerankResult(index=1, relevance_score=0.9)]

    _install_reranker(monkeypatch, _fast_reranker)
    metrics = _RecordingMetrics()
    monkeypatch.setattr(retrieval_module, "_metrics", metrics)

    _results, meta = await _retrieve_with_rerank(svc)

    assert "rerank_degraded" not in meta
    assert metrics.degrades == []
    # The served-retrieval counter keeps working through the same stub.
    assert len(metrics.retrievals) == 1
    assert metrics.retrievals[0][0] == "bm25"
    assert metrics.retrievals[0][1] is False
    assert metrics.retrievals[0][2] >= 0


def test_remaining_budget_helper_tracks_the_entrypoint_deadline():
    assert remaining_interactive_budget_seconds() is None

    store = object.__new__(VectorStore)
    store.interactive_deadline_seconds = 3.0
    token = store.begin_interactive_budget()
    try:
        remaining = remaining_interactive_budget_seconds()
        assert remaining is not None
        assert 2.0 < remaining <= 3.0
    finally:
        store.end_interactive_budget(token)

    assert remaining_interactive_budget_seconds() is None
