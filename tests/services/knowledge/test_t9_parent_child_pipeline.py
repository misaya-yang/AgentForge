"""T9 (PRD): pipeline-level activation of parent-child retrieval, summary
indexing, and structural routing through ``RetrievalService.retrieve``.

These run on the fake-store/fake-database harness from
``test_retrieve_batch._make_bm25_service`` (bm25-only leg, no vectors, no
rerank), so they pin exactly the retrieval-flow wiring PRD T9 adds:

  * the dormant switches leave the pipeline byte-for-byte unchanged when
    disabled (and when nothing carries a parent link);
  * when enabled, child matches fold to parents after scoring and before
    the top_k cut, so parents returned >= top_k whenever the corpus has
    that many distinct parents (the fan-out headroom requirement);
  * the scoped ``get_segment_scoped`` authority is the parent source;
  * invalid stored configs fail before any recall runs;
  * each stage reports through ``meta``.
"""

from __future__ import annotations

import copy
from types import SimpleNamespace

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.dataset_service import (
    _dataset_revision_fingerprint,
    _retrieval_effective_dataset_config,
)

from tests.services.knowledge.test_retrieve_batch import FakeDatabase, _make_bm25_service


def _row(segment_id, text, *, metadata=None):
    return {
        "segment_id": segment_id,
        "dataset_id": "kb-demo",
        "document_id": "doc-1",
        "position": 0,
        "text": text,
        "token_count": len(text.split()),
        "metadata": metadata or {},
        "source_type": "manual",
        "language": "en",
    }


def _child_row(segment_id, text, parent_id):
    # The hierarchical indexer keeps the parent link in the segment
    # ``metadata`` column; the FTS leg then nests that column under
    # ``payload["metadata"]`` (chunking_manager._segment_vector_payload), so
    # the candidate the fold sees carries it one level down.
    return _row(segment_id, text, metadata={"parent_segment_id": parent_id})


class _ParentDb(FakeDatabase):
    """FakeDatabase plus the scoped parent authority used by the fold."""

    def __init__(self, rows, parents=None, *, scoped=True):
        super().__init__(rows)
        self.parents = parents or {}
        self.scoped = scoped
        self.scoped_calls = []

    async def get_segment_scoped(self, *, segment_id, dataset_id, tenant_id):
        self.scoped_calls.append(
            {"segment_id": segment_id, "dataset_id": dataset_id, "tenant_id": tenant_id}
        )
        if not self.scoped:
            return None
        row = self.parents.get(segment_id)
        return dict(row) if row else None


def _service_with_config(rows, retrieval, *, parents=None, scoped=True):
    svc, _database = _make_bm25_service(rows)
    database = _ParentDb(rows, parents=parents, scoped=scoped)
    svc.db = database

    original = svc._ks.require_dataset_access

    async def _access(user, dataset_id, required="viewer"):
        dataset = await original(user, dataset_id, required=required)
        dataset["index_config"] = {"retrieval": copy.deepcopy(retrieval)}
        return dataset

    svc._ks.require_dataset_access = _access
    return svc, database


async def _retrieve(svc, query="alpha beta", top_k=3):
    return await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query=query,
        top_k=top_k,
        mode="bm25",
        rerank=False,
    )


_PARENTS = {
    "p-1": {
        "segment_id": "p-1",
        "document_id": "doc-1",
        "text": "PARENT ONE full block",
        "content_type": "text",
        "metadata": {"chunk_level": 1},
    },
    "p-2": {
        "segment_id": "p-2",
        "document_id": "doc-2",
        "text": "PARENT TWO full block",
        "content_type": "text",
        "metadata": {"chunk_level": 1},
    },
}


def _child_corpus():
    return [
        _child_row("c-1", "alpha beta one", "p-1"),
        _child_row("c-2", "alpha beta two", "p-1"),
        _child_row("c-3", "alpha beta three", "p-2"),
    ]


# ---------------------------------------------------------------------------
# activation
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_disabled_switch_leaves_pipeline_unchanged():
    baseline, _ = _service_with_config(_child_corpus(), {})
    base_results, base_meta = await _retrieve(baseline, top_k=3)

    dormant, _ = _service_with_config(
        _child_corpus(), {"parent_child": {"enabled": False}}, parents=_PARENTS
    )
    results, meta = await _retrieve(dormant, top_k=3)

    assert [r.segment_id for r in results] == [r.segment_id for r in base_results]
    assert "parent_child" not in meta
    assert "summary_index" not in meta
    assert "structural_routing" not in meta


@pytest.mark.asyncio
async def test_child_hits_fold_to_parents_with_parent_text_and_provenance():
    svc, db = _service_with_config(
        _child_corpus(), {"parent_child": {"enabled": True}}, parents=_PARENTS
    )
    results, meta = await _retrieve(svc, top_k=3)

    assert sorted(r.segment_id for r in results) == ["p-1", "p-2"]
    texts = {r.segment_id: r.text for r in results}
    assert texts["p-1"] == "PARENT ONE full block"
    assert texts["p-2"] == "PARENT TWO full block"
    # Parent text is fetched through the scoped dataset/tenant authority.
    assert {c["dataset_id"] for c in db.scoped_calls} == {"kb-demo"}
    assert {c["tenant_id"] for c in db.scoped_calls} == {"tenant-a"}

    p1 = next(r for r in results if r.segment_id == "p-1")
    prov = p1.metadata["_parent_child"]
    assert prov["mode"] == "parent"
    assert prov["status"] == "resolved"
    assert {c["segment_id"] for c in prov["children"]} == {"c-1", "c-2"}
    assert meta["parent_child"]["fold"]["child_hits"] == 3
    assert meta["parent_child"]["fold"]["parents"] == 2
    assert meta["parent_child"]["fold"]["collapsed_children"] == 1
    assert any("Parent-child fold" in stage for stage in meta["pipeline_stages"])


@pytest.mark.asyncio
async def test_fold_happens_before_top_k_cut_not_after():
    # Three children under two parents, top_k=2. A CHILD-level truncation
    # before the fold (the Dify bug this design rejects) could cut at the two
    # best children of p-1 and return ONE parent. Folding first and cutting at
    # parent level must return BOTH parents.
    svc, _db = _service_with_config(
        _child_corpus(),
        {"parent_child": {"enabled": True, "fanout_top_k": 48}},
        parents=_PARENTS,
    )
    results, meta = await _retrieve(svc, top_k=2)

    assert len(results) == 2
    assert {r.segment_id for r in results} == {"p-1", "p-2"}
    # The fan-out widened recall so the parent-level cut has headroom.
    assert meta["parent_child"]["fanout"]["top_k"] == 48
    assert meta["parent_child"]["fanout"]["source"] == "config"


@pytest.mark.asyncio
async def test_parent_count_reaches_top_k_when_corpus_supports_it():
    # 8 children under 8 parents, top_k=6 -> the fold must not lose parents
    # to truncation: 6 distinct parent rows returned (parents < top_k only
    # when the corpus lacks distinct parents, which it does not here).
    rows = [
        _child_row(f"c-{i}", f"alpha beta child {i}", f"p-{i}") for i in range(1, 9)
    ]
    parents = {
        f"p-{i}": {
            "segment_id": f"p-{i}",
            "document_id": "doc-1",
            "text": f"PARENT {i}",
            "content_type": "text",
            "metadata": {},
        }
        for i in range(1, 9)
    }
    svc, _db = _service_with_config(
        rows, {"parent_child": {"enabled": True}}, parents=parents
    )
    results, _meta = await _retrieve(svc, top_k=6)
    assert len(results) == 6
    assert len({r.segment_id for r in results}) == 6


@pytest.mark.asyncio
async def test_mode_context_keeps_child_text_and_attaches_parent_context():
    svc, _db = _service_with_config(
        _child_corpus(),
        {"parent_child": {"enabled": True, "return_mode": "context"}},
        parents=_PARENTS,
    )
    results, _meta = await _retrieve(svc, top_k=3)

    p1 = next(r for r in results if r.segment_id == "c-1" or r.segment_id == "c-2")
    assert p1.metadata["parent_context"] == "PARENT ONE full block"
    assert p1.metadata["_parent_child"]["mode"] == "context"
    # context mode keeps best-child identity, parent link stays in provenance
    assert p1.metadata["_parent_child"]["parent_segment_id"] in {"p-1", "p-2"}


@pytest.mark.asyncio
async def test_unresolved_parent_degrades_to_child_not_dropped():
    svc, _db = _service_with_config(
        _child_corpus(), {"parent_child": {"enabled": True}}, parents={}, scoped=False
    )
    results, meta = await _retrieve(svc, top_k=3)

    # Each parent group still folds to ONE representative (its best child) —
    # the collapse is per-parent regardless of resolution, so siblings of the
    # same parent never duplicate; nothing vanishes silently.
    ids = {r.segment_id for r in results}
    assert len(results) == 2
    assert "c-3" in ids
    assert len(ids & {"c-1", "c-2"}) == 1
    fold = meta["parent_child"]["fold"]
    assert fold["unresolved_parents"] == 2
    assert fold["child_hits"] == 3
    assert fold["parents"] == 2


@pytest.mark.asyncio
async def test_children_without_links_are_passthrough():
    rows = [_row("plain-1", "alpha beta plain"), _child_row("c-1", "alpha beta kid", "p-1")]
    svc, _db = _service_with_config(rows, {"parent_child": True}, parents=_PARENTS)
    results, meta = await _retrieve(svc, top_k=3)

    ids = {r.segment_id for r in results}
    assert "plain-1" in ids
    assert "p-1" in ids
    assert meta["parent_child"]["fold"]["child_hits"] == 1


@pytest.mark.asyncio
async def test_invalid_parent_child_config_fails_before_any_recall():
    rows = _child_corpus()
    svc, db = _service_with_config(rows, {"parent_child": {"return_mode": "sideways"}})
    with pytest.raises(ValidationFailedError, match="parent_child.return_mode"):
        await _retrieve(svc)
    assert db.calls == []  # no search_segments_text ran
    assert db.scoped_calls == []


# ---------------------------------------------------------------------------
# summary index layer
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_summary_hit_returns_block_not_summary_with_max_score():
    rows = [
        _row("b-1", "alpha beta block body"),
        _row(
            "s-1",
            "alpha beta summary",
            metadata={"is_summary": True, "original_chunk_id": "b-1"},
        ),
    ]
    svc, _db = _service_with_config(rows, {"summary_index": {"enabled": True}})
    results, meta = await _retrieve(svc, top_k=2)

    ids = [r.segment_id for r in results]
    assert "s-1" not in ids
    assert "b-1" in ids
    block = next(r for r in results if r.segment_id == "b-1")
    assert block.metadata["_summary_hit"] is True
    assert block.metadata["_summary_prefix"] == "alpha beta summary"
    assert meta["summary_index"]["merged"] == 1
    assert meta["summary_index"]["unresolved"] == 0


@pytest.mark.asyncio
async def test_summary_index_disabled_keeps_summary_as_own_hit():
    rows = [
        _row("b-1", "alpha beta block body"),
        _row(
            "s-1",
            "alpha beta summary",
            metadata={"is_summary": True, "original_chunk_id": "b-1"},
        ),
    ]
    svc, _db = _service_with_config(rows, {"summary_index": {"enabled": False}})
    results, meta = await _retrieve(svc, top_k=2)

    assert {r.segment_id for r in results} == {"b-1", "s-1"}
    assert "summary_index" not in meta


# ---------------------------------------------------------------------------
# structural routing
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_structural_routing_promotes_affine_heading_first():
    rows = [
        _row("h-1", "alpha beta", metadata={"section_header": "Deployment Network"}),
        _row("h-2", "alpha beta", metadata={}),
    ]
    svc, _db = _service_with_config(
        rows, {"structural_routing": {"enabled": True, "boost": 0.4, "min_affinity": 0.25}}
    )
    results, meta = await _retrieve(svc, query="alpha beta deployment network", top_k=2)

    # Identical lexical scores: only the routing bonus breaks the tie.
    assert results[0].segment_id == "h-1"
    assert results[0].metadata["_structural_route"]["mode"] == "heading_priority"
    # affinity = breadcrumb-token coverage of the 5-token query
    # ("deployment network" over "alpha beta deployment network" = 2/5).
    assert results[0].metadata["_structural_route"]["affinity"] == pytest.approx(0.4)
    assert meta["structural_routing"]["boosted"] == 1
    assert meta["structural_routing"]["candidates_with_breadcrumb"] == 1


@pytest.mark.asyncio
async def test_structural_routing_no_breadcrumb_corpus_is_noop():
    rows = [_row("a", "alpha beta"), _row("b", "alpha beta gamma")]
    svc, _db = _service_with_config(rows, {"structural_routing": True})
    results, meta = await _retrieve(svc, query="alpha", top_k=2)
    assert meta["structural_routing"]["boosted"] == 0
    assert meta["structural_routing"]["candidates_with_breadcrumb"] == 0
    assert len(results) == 2


@pytest.mark.asyncio
async def test_invalid_structural_config_fails_before_recall():
    svc, db = _service_with_config(
        [_row("a", "alpha beta")], {"structural_routing": {"boost": 4}}
    )
    with pytest.raises(ValidationFailedError, match="structural_routing.boost"):
        await _retrieve(svc)
    assert db.calls == []


# ---------------------------------------------------------------------------
# retrieval-cache invalidation for the new switches
# ---------------------------------------------------------------------------


def _fingerprint_dataset(retrieval):
    return {
        "dataset_id": "dataset-a",
        "content_revision": 7,
        "index_config": {"retrieval": retrieval},
    }


def test_t9_switches_are_part_of_effective_retrieval_config():
    dataset = _fingerprint_dataset(
        {
            "parent_child": {"enabled": True, "return_mode": "parent", "fanout_top_k": 48},
            "summary_index": {"enabled": True, "prepend_summary": True},
            "structural_routing": {"enabled": True, "mode": "heading_priority"},
        }
    )
    projected = _retrieval_effective_dataset_config(dataset)["retrieval"]
    assert projected["parent_child"]["fanout_top_k"] == 48
    assert projected["summary_index"]["prepend_summary"] is True
    assert projected["structural_routing"]["mode"] == "heading_priority"


@pytest.mark.parametrize(
    "key, base_cfg, field, value",
    [
        (
            "parent_child",
            {"enabled": True, "return_mode": "parent", "fanout_top_k": 48},
            "return_mode",
            "context",
        ),
        (
            "parent_child",
            {"enabled": True, "return_mode": "parent", "fanout_top_k": 48},
            "fanout_top_k",
            96,
        ),
        (
            "parent_child",
            {"enabled": True, "return_mode": "parent", "fanout_top_k": 48},
            "enabled",
            False,
        ),
        ("summary_index", {"enabled": True, "prepend_summary": True}, "prepend_summary", False),
        (
            "structural_routing",
            {"enabled": True, "mode": "heading_priority", "boost": 0.15, "min_affinity": 0.25},
            "boost",
            0.5,
        ),
        (
            "structural_routing",
            {"enabled": True, "mode": "heading_priority", "boost": 0.15, "min_affinity": 0.25},
            "min_affinity",
            0.8,
        ),
    ],
)
def test_t9_switch_changes_invalidate_revision_fingerprint(key, base_cfg, field, value):
    base = _fingerprint_dataset({key: base_cfg})
    changed = copy.deepcopy(base)
    changed["index_config"]["retrieval"][key][field] = value
    assert _dataset_revision_fingerprint(base) != _dataset_revision_fingerprint(changed)


def test_boolean_shorthand_switches_also_fingerprint():
    off = _fingerprint_dataset({})
    on = _fingerprint_dataset({"parent_child": True})
    assert _dataset_revision_fingerprint(off) != _dataset_revision_fingerprint(on)
    # And the projection keeps the scalar shorthand.
    assert _retrieval_effective_dataset_config(on)["retrieval"]["parent_child"] is True


async def test_parent_fold_reuses_resolved_query_concurrency(monkeypatch):
    from knowledge_service.services.knowledge import retrieval_service

    limits = []
    original_fold = retrieval_service.fold_candidates_to_parents

    async def record_fold(*args, **kwargs):
        limits.append(kwargs["max_concurrency"])
        return await original_fold(*args, **kwargs)

    monkeypatch.setattr(retrieval_service, "fold_candidates_to_parents", record_fold)
    svc, _ = _service_with_config(
        _child_corpus(), {"parent_child": True}, parents=_PARENTS
    )
    await _retrieve(svc)
    assert limits == [svc.settings.knowledge.retrieval_query_max_concurrency]

    # The per-request batch limit is already capped by the service setting.
    await svc._retrieve_queries(
        user=SimpleNamespace(), dataset_id="kb-demo", query="alpha beta",
        top_k=3, mode="bm25", rerank=False, _recall_max_parallel=1,
    )
    assert limits[-1] == 1
