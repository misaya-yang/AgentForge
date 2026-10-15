"""Addendum §1-T2-4 numeric pin: weighted fusion consumes normalized legs.

The addendum accepts a weighted-fusion option only under two conditions:
per-request per-leg normalization and server-side weight-sum validation
(Dify's unweighted mix of ts_rank ~0.1 with cosine ~0.8 into one weighted
sum is the rejected shape). The weight-sum validation half is pinned by
test_retrieval_fusion_validation.py; until now the *numeric consumption*
half was only pinned as plumbing (test_active_read_routes.py checks the
fusion_method kwarg reaches the service, not what fusion does with it).

These tests drive the real pipeline with weighted fusion enabled and assert
that every returned ``_fusion_score`` equals the re-normalized weighted sum
of the request-local per-leg normalized scores — never the raw engine
scores — including the documented missing-leg penalty.
"""

from __future__ import annotations

import math
from types import SimpleNamespace

import pytest

from tests.services.knowledge.test_relevance_contracts import _make_hybrid_service


def _enable_weighted_fusion(service, *, dense_weight: float, bm25_weight: float):
    service._ks._resolve_fusion_config = lambda **_kwargs: {
        "method": "weighted",
        "dense_weight": dense_weight,
        "bm25_weight": bm25_weight,
        "rrf_k": 60,
    }


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("dense_weight", "bm25_weight"),
    [
        (0.7, 0.3),  # already sums to 1
        (3.0, 1.0),  # server re-normalizes to 0.75 / 0.25
        (2.0, 2.0),  # server re-normalizes to 0.5 / 0.5
    ],
)
async def test_weighted_fusion_is_the_renormalized_sum_of_normalized_legs(
    monkeypatch, dense_weight, bm25_weight
) -> None:
    # Addendum §1-T2-4: raw ts_rank-scale and cosine-scale scores must never
    # enter the same weighted sum; fusion = w_d*norm_d + w_b*norm_b with the
    # weights re-normalized to sum 1 per request.
    service, _database, _store = _make_hybrid_service(monkeypatch)
    _enable_weighted_fusion(
        service, dense_weight=dense_weight, bm25_weight=bm25_weight
    )

    results, meta = await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=10,
        mode="hybrid",
        rerank=False,
        mmr=False,
    )

    assert meta["fusion_method"] == "weighted"
    total = dense_weight + bm25_weight
    w_dense = dense_weight / total
    w_bm25 = bm25_weight / total

    def _leg_norm(payload: dict, leg: str) -> float:
        # The payload serializes an absent leg's normalized score as "N/A";
        # the fusion itself must treat that leg as contributing 0.0.
        value = payload.get(f"_{leg}_score_norm")
        return 0.0 if value in (None, "N/A") else float(value)

    assert results, "weighted hybrid must still serve the union"
    for result in results:
        payload = result.metadata
        d_norm = _leg_norm(payload, "dense")
        b_norm = _leg_norm(payload, "bm25")
        # Normalization is per leg and maps into [0, 1] (addendum: not raw).
        assert 0.0 <= d_norm <= 1.0
        assert 0.0 <= b_norm <= 1.0
        expected = w_dense * d_norm + w_bm25 * b_norm
        assert math.isclose(
            float(payload["_fusion_score"]), expected, rel_tol=1e-6, abs_tol=1e-9
        )

    # Both-legs hits outrank single-leg hits: a shared candidate gets
    # contributions from every leg, so it must lead the ranking.
    ids = [r.segment_id for r in results]
    assert ids[0] == "shared"
    shared_payload = next(r.metadata for r in results if r.segment_id == "shared")
    assert sorted(shared_payload["_sources"]) == ["bm25", "dense"]


@pytest.mark.asyncio
async def test_weighted_fusion_penalizes_missing_legs_instead_of_defaulting(
    monkeypatch,
) -> None:
    # A candidate recalled by only one leg keeps that leg's normalized score
    # scaled by its weight only — the missing leg contributes 0 (documented
    # penalty), it must not silently fall back to the raw mixed score.
    service, _database, _store = _make_hybrid_service(monkeypatch)
    _enable_weighted_fusion(service, dense_weight=0.5, bm25_weight=0.5)

    results, _meta = await service.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=10,
        mode="hybrid",
        rerank=False,
        mmr=False,
    )

    single_leg = {
        r.segment_id: r.metadata
        for r in results
        if sorted(r.metadata.get("_sources", [])) in (["bm25"], ["dense"])
    }
    assert {"bm25-only", "dense-only"} <= set(single_leg)
    for _segment_id, payload in single_leg.items():
        present = "dense" if "dense" in payload["_sources"] else "bm25"
        norm = float(payload[f"_{present}_score_norm"])
        assert math.isclose(
            float(payload["_fusion_score"]), 0.5 * norm, rel_tol=1e-6, abs_tol=1e-9
        )
        missing = "bm25" if present == "dense" else "dense"
        assert payload.get(f"_{missing}_score_norm") in (None, "N/A")
