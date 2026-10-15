"""B5 (PRD T1-8): MMR fill-remaining must not resurrect diversity-rejected candidates.

Covers the ``mmr_select`` rewrite:
  * the strict/fill ``fill_policy`` contract (rejected stays rejected);
  * selection parity with the pre-B5 brute-force O(k^2 * n * d) reference
    (the incremental O(k * n * d) loop must pick the same order and the
    same MMRPick scores);
  * ordering sanity on small vector sets;
plus the pipeline wiring in ``retrieval_service`` (index_config
``mmr.fill_policy`` / ``mmr.strict_diversity``, meta reporting, and the
removal of the old caller-side fill-remaining loop).
"""

from __future__ import annotations

from types import SimpleNamespace

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.retrieval import cosine_similarity, mmr_select

from tests.services.knowledge.test_retrieve_batch import _make_bm25_service


# Reference: the literal pre-B5 loop, kept here so the rewritten
# implementation is pinned against the algorithm it replaced.
def _brute_force_mmr(
    candidates, relevance, vectors, *, top_k, lambda_mult=0.5, similarity_threshold=None
):
    lam = max(0.0, min(1.0, lambda_mult))
    threshold = similarity_threshold
    remaining = [c for c in candidates if c]
    selected = []

    def max_sim(cid):
        if not selected:
            return 0.0
        v = vectors.get(cid)
        if v is None:
            return 0.0
        best = 0.0
        for sid in selected:
            sv = vectors.get(sid)
            if sv is None:
                continue
            best = max(best, cosine_similarity(v, sv))
        return float(best)

    while remaining and len(selected) < int(top_k):
        best_id, best_mmr = None, -1e30
        for cid in remaining:
            rel = float(relevance.get(cid, 0.0))
            sim = max_sim(cid)
            if threshold is not None and selected and sim >= threshold:
                continue
            mmr = lam * rel - (1.0 - lam) * sim
            if mmr > best_mmr:
                best_id, best_mmr = cid, mmr
        if best_id is None:
            break
        selected.append(best_id)
        remaining = [c for c in remaining if c != best_id]
    return selected


class TestMMRFillPolicyContract:
    def test_strict_policy_never_resurrects_rejected(self):
        vectors = {
            "a": [1.0, 0.0],
            "b": [0.99999, 0.0045],   # ~1.0 cosine to a -> rejected by threshold
            "c": [0.99998, 0.0063],   # ditto
            "d": [0.0, 1.0],          # orthogonal -> survives
        }
        relevance = {"a": 1.0, "b": 0.95, "c": 0.9, "d": 0.1}

        selected, picks = mmr_select(
            ["a", "b", "c", "d"],
            relevance,
            vectors,
            top_k=4,
            lambda_mult=0.5,
            similarity_threshold=0.99,
            fill_policy="strict",
        )

        # b and c were rejected by the diversity pass; strict must return
        # SHORT rather than pad back to top_k with them.
        assert selected == ["a", "d"]
        assert set(picks) == {"a", "d"}

    def test_fill_policy_restores_legacy_append(self):
        vectors = {
            "a": [1.0, 0.0],
            "b": [0.99999, 0.0045],
            "c": [0.99998, 0.0063],
            "d": [0.0, 1.0],
        }
        relevance = {"a": 1.0, "b": 0.95, "c": 0.9, "d": 0.1}

        selected, picks = mmr_select(
            ["a", "b", "c", "d"],
            relevance,
            vectors,
            top_k=4,
            lambda_mult=0.5,
            similarity_threshold=0.99,
            fill_policy="fill",
        )

        # Legacy behaviour: the pre-B5 caller-side fill loop appended the
        # rejected candidates in relevance (candidate) order.
        assert selected == ["a", "d", "b", "c"]
        # Appended rows carry no pick info — same as the legacy caller path.
        assert set(picks) == {"a", "d"}

    def test_default_policy_is_strict(self):
        vectors = {
            "a": [1.0, 0.0],
            "b": [0.99999, 0.0045],
            "d": [0.0, 1.0],
        }
        relevance = {"a": 1.0, "b": 0.9, "d": 0.1}
        selected, _ = mmr_select(
            ["a", "b", "d"], relevance, vectors, top_k=3, similarity_threshold=0.99
        )
        assert selected == ["a", "d"]

    def test_no_threshold_makes_policies_identical(self):
        vectors = {
            "a": [1.0, 0.0],
            "b": [0.8, 0.6],
            "c": [0.0, 1.0],
            "d": [0.2, 0.98],
        }
        relevance = {"a": 1.0, "b": 0.9, "c": 0.8, "d": 0.7}
        strict, _ = mmr_select(
            ["a", "b", "c", "d"], relevance, vectors, top_k=3, fill_policy="strict"
        )
        fill, _ = mmr_select(
            ["a", "b", "c", "d"], relevance, vectors, top_k=3, fill_policy="fill"
        )
        assert strict == fill == ["a", "c", "b"]

    @pytest.mark.parametrize(
        "policy", ["", "anything", "FILL-REJECTED", "strictly"]
    )
    def test_unknown_policy_rejected(self, policy):
        with pytest.raises(ValueError, match="fill_policy"):
            mmr_select(
                ["a", "b"], {"a": 1.0, "b": 0.5}, {"a": [1, 0], "b": [0, 1]},
                top_k=1, fill_policy=policy,
            )

    def test_empty_and_degenerate_inputs(self):
        assert mmr_select([], {"a": 1.0}, {}, top_k=3) == ([], {})
        assert mmr_select(["a"], {"a": 1.0}, {"a": [1, 0]}, top_k=0) == ([], {})
        # Only empty-string candidates.
        assert mmr_select(["", ""], {}, {}, top_k=2) == ([], {})


class TestMMRIncrementalParity:
    @pytest.mark.parametrize("seed", [11, 23, 42])
    @pytest.mark.parametrize("lambda_mult", [0.0, 0.35, 0.5, 1.0])
    def test_matches_brute_force_reference(self, seed, lambda_mult):
        import random

        rng = random.Random(seed)
        n = 24
        ids = [f"seg-{i}" for i in range(n)]
        vectors = {cid: [rng.uniform(-1, 1) for _ in range(8)] for cid in ids}
        # Inject a few near-duplicates so diversity pressure is real.
        vectors["seg-5"] = [v * 1.001 + 0.0001 for v in vectors["seg-4"]]
        relevance = {cid: rng.random() for cid in ids}
        order = sorted(ids, key=lambda x: relevance[x], reverse=True)

        got, picks = mmr_select(
            order, relevance, vectors, top_k=7, lambda_mult=lambda_mult
        )
        want = _brute_force_mmr(
            order, relevance, vectors, top_k=7, lambda_mult=lambda_mult
        )
        assert got == want
        for cid in want:
            assert picks[cid].relevance == pytest.approx(relevance[cid])
            # Reference mirrors the legacy floor at 0.0: max_sim starts at
            # 0.0 and only ever grows.
            assert picks[cid].max_sim_to_selected == pytest.approx(
                max(
                    [0.0]
                    + [
                        cosine_similarity(vectors[cid], vectors[sid])
                        for sid in want[: want.index(cid)]
                    ]
                ),
                abs=1e-9,
            )

    def test_missing_vectors_and_zero_norms_select_like_reference(self):
        vectors = {
            "a": [1.0, 0.0],
            "z": [0.0, 0.0],  # zero norm -> treated as similarity 0
            # "m" missing entirely
        }
        relevance = {"a": 0.5, "z": 0.4, "m": 1.0}
        order = ["m", "a", "z"]
        got, picks = mmr_select(order, relevance, vectors, top_k=3, lambda_mult=0.5)
        want = _brute_force_mmr(order, relevance, vectors, top_k=3, lambda_mult=0.5)
        assert got == want == ["m", "a", "z"]
        assert picks["m"].max_sim_to_selected == 0.0


class TestMMROrderingSanity:
    def test_lambda_one_is_pure_relevance(self):
        vectors = {
            "a": [1.0, 0.0],
            "b": [0.0, 1.0],
            "c": [1.0, 1.0],
        }
        relevance = {"a": 1.0, "b": 0.9, "c": 0.2}
        selected, _ = mmr_select(
            ["a", "b", "c"], relevance, vectors, top_k=2, lambda_mult=1.0
        )
        assert selected == ["a", "b"]

    def test_lambda_zero_is_pure_diversity(self):
        vectors = {
            "a": [1.0, 0.0],
            "b": [0.99, 0.14],
            "c": [0.0, 1.0],
        }
        relevance = {"a": 1.0, "b": 0.95, "c": 0.1}
        selected, _ = mmr_select(
            ["a", "b", "c"], relevance, vectors, top_k=2, lambda_mult=0.0
        )
        # First pick still max relevance (all sims are 0 before any pick);
        # second pick must be the most DIVERSIFIED one (c), not b.
        assert selected == ["a", "c"]


def _mmr_rows():
    return [
        {
            "segment_id": f"seg-{i}",
            "dataset_id": "kb-demo",
            "document_id": f"doc-{i}",
            "position": 0,
            "text": f"alpha {' ' * (0 if i == 1 else i)} beta{i}",
            "token_count": 3,
            "metadata": {},
            "source_type": "manual",
            "language": "en",
        }
        for i in range(1, 5)
    ]


class _MMRVectorStore:
    """retrieve_vectors fake: returns crafted vectors and records scope."""

    VECTORS = {
        "seg-1": [1.0, 0.0],
        "seg-2": [0.99999, 0.0045],
        "seg-3": [0.99998, 0.0063],
        "seg-4": [0.0, 1.0],
    }

    def __init__(self):
        self.calls = []

    async def require_collection_readable(self, *_args, **_kwargs):
        return {"tenant_id": "tenant-a", "dataset_id": "kb-demo"}

    async def retrieve_vectors(
        self, *, collection_name, point_ids, tenant_id, dataset_id
    ):
        self.calls.append(
            {
                "collection_name": collection_name,
                "point_ids": list(point_ids),
                "tenant_id": tenant_id,
                "dataset_id": dataset_id,
            }
        )
        return {pid: self.VECTORS[pid] for pid in point_ids if pid in self.VECTORS}


def _make_mmr_service(mmr_cfg):
    svc, database = _make_bm25_service(_mmr_rows())
    store = _MMRVectorStore()
    svc.vector_store = store

    original = svc._ks.require_dataset_access

    async def _access(user, dataset_id, required="viewer"):
        dataset = await original(user, dataset_id, required=required)
        dataset["index_config"] = {"retrieval": {"mmr": mmr_cfg}}
        return dataset

    svc._ks.require_dataset_access = _access
    return svc, database, store


@pytest.mark.asyncio
async def test_pipeline_strict_returns_fewer_than_top_k_and_keeps_diversity():
    svc, _database, store = _make_mmr_service({"enabled": True, "threshold": 0.99})

    results, meta = await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=3,
        mode="bm25",
        rerank=False,
    )

    ids = [r.segment_id for r in results]
    # seg-1..seg-3 are mutual near-duplicates; strict policy must return at
    # most ONE of them plus the orthogonal seg-4, i.e. fewer than top_k.
    assert len(ids) < 3, f"fill-remaining resurrected rejected candidates: {ids}"
    assert "seg-4" in ids
    assert sum(1 for i in ids if i in {"seg-1", "seg-2", "seg-3"}) == 1
    assert meta["mmr"] is True
    assert meta["mmr_fill_policy"] == "strict"
    assert meta["mmr_diversity_shortfall"] == {
        "requested": 3,
        "selected": len(ids),
        "candidates": 4,
    }
    # Vectors were fetched under the same tenant/dataset authority scope.
    assert store.calls[0]["tenant_id"] == "tenant-a"
    assert store.calls[0]["dataset_id"] == "kb-demo"
    assert store.calls[0]["collection_name"] == "kb-demo-collection"


@pytest.mark.asyncio
async def test_pipeline_fill_policy_resurrects_rejected_as_legacy():
    svc, _database, _store = _make_mmr_service(
        {"enabled": True, "threshold": 0.99, "fill_policy": "fill"}
    )

    results, meta = await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=3,
        mode="bm25",
        rerank=False,
    )

    ids = [r.segment_id for r in results]
    assert len(ids) == 3  # legacy fill pads back up to top_k
    assert "seg-4" in ids
    assert "mmr_diversity_shortfall" not in meta
    assert meta["mmr_fill_policy"] == "fill"


@pytest.mark.asyncio
async def test_pipeline_strict_diversity_false_is_fill_shorthand():
    svc, _database, _store = _make_mmr_service(
        {"enabled": True, "threshold": 0.99, "strict_diversity": False}
    )
    results, meta = await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=3,
        mode="bm25",
        rerank=False,
    )
    assert meta["mmr_fill_policy"] == "fill"
    assert len(results) == 3


@pytest.mark.asyncio
async def test_pipeline_fill_policy_takes_precedence_over_shorthand():
    svc, _database, _store = _make_mmr_service(
        {
            "enabled": True,
            "threshold": 0.99,
            "strict_diversity": False,
            "fill_policy": "strict",
        }
    )
    results, meta = await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=3,
        mode="bm25",
        rerank=False,
    )
    assert meta["mmr_fill_policy"] == "strict"
    assert len(results) < 3


@pytest.mark.asyncio
@pytest.mark.parametrize("bad_policy", ["resurrect", "any", "True", 1])
async def test_pipeline_rejects_invalid_fill_policy_config(bad_policy):
    svc, _database, _store = _make_mmr_service(
        {"enabled": True, "threshold": 0.99, "fill_policy": bad_policy}
    )
    with pytest.raises(ValidationFailedError, match="mmr.fill_policy"):
        await svc.retrieve(
            user=SimpleNamespace(),
            dataset_id="kb-demo",
            query="alpha",
            top_k=3,
            mode="bm25",
            rerank=False,
        )


@pytest.mark.asyncio
async def test_pipeline_rejects_non_boolean_strict_diversity():
    svc, _database, _store = _make_mmr_service(
        {"enabled": True, "strict_diversity": "yes"}
    )
    with pytest.raises(ValidationFailedError, match="mmr.strict_diversity"):
        await svc.retrieve(
            user=SimpleNamespace(),
            dataset_id="kb-demo",
            query="alpha",
            top_k=3,
            mode="bm25",
            rerank=False,
        )


@pytest.mark.asyncio
async def test_pipeline_mmr_field_shape_unchanged():
    """Retrieval responses keep the existing _mmr_* payload field names."""
    svc, _database, _store = _make_mmr_service({"enabled": True})
    results, _meta = await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query="alpha",
        top_k=2,
        mode="bm25",
        rerank=False,
    )
    assert len(results) == 2
    for result in results:
        payload = result.metadata
        assert "_mmr_score" in payload
        assert "_mmr_relevance" in payload
        assert "_mmr_max_sim" in payload
        assert isinstance(payload["_mmr_score"], (int, float))
