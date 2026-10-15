"""T9 (PRD): unit coverage for the parent-child / summary-index /
structural-routing modules in isolation from the retrieval pipeline.

Pins the mechanism the Dify addendum requires:
  * parent score = ``max(child scores)`` and one row per parent;
  * summary merge = ``max(block, summary)`` — the summary never *overrides*
    the block score (the Dify coverage bug must not reappear);
  * invalid stored dataset configs fail before anything is recalled;
  * the fan-out widening keeps ``parents >= top_k`` headroom;
  * structural routing boosts inside the fusion score space only.
"""

from __future__ import annotations

import asyncio

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.parent_child_retrieval import (
    DEFAULT_FANOUT_TOP_K,
    apply_recall_fanout,
    fold_candidates_to_parents,
    merge_summary_siblings,
    parse_parent_child_settings,
    parse_summary_index_settings,
)
from knowledge_service.services.knowledge.structural_routing import (
    apply_structural_routing,
    extract_breadcrumb,
    parse_structural_settings,
)

# ---------------------------------------------------------------------------
# fakes
# ---------------------------------------------------------------------------


class FakeParentDb:
    """get_segment_scoped authority fake: rows map, raise set, cancel set."""

    def __init__(self, rows=None, *, raises=(), wrong_id=(), cancel=()):
        self.rows = rows or {}
        self.raises = set(raises)
        self.wrong_id = set(wrong_id)
        self.cancel = set(cancel)
        self.calls = []

    async def get_segment_scoped(self, *, segment_id, dataset_id, tenant_id):
        self.calls.append(
            {"segment_id": segment_id, "dataset_id": dataset_id, "tenant_id": tenant_id}
        )
        if segment_id in self.cancel:
            raise asyncio.CancelledError()
        if segment_id in self.raises:
            raise RuntimeError(f"db exploded for {segment_id}")
        row = self.rows.get(segment_id)
        if row is None:
            return None
        if segment_id in self.wrong_id:
            # Guard against the authority returning a different row than asked.
            return {**row, "segment_id": "someone-elses-parent"}
        return dict(row)


def _child(seg_id, parent_id=None, *, score=0.5, text=None, extra_meta=None):
    candidate = {
        "segment_id": seg_id,
        "document_id": "doc-1",
        "text": text if text is not None else f"text of {seg_id}",
        "metadata": {"metadata": {"parent_segment_id": parent_id}} if parent_id else {},
        "_final_score": score,
        "_fusion_score": score,
        "_sources": {"bm25"},
    }
    if extra_meta:
        candidate["metadata"].update(extra_meta)
    return candidate


def _parent_row(parent_id, *, text=None, content_type="text", document_id="doc-1"):
    return {
        "segment_id": parent_id,
        "document_id": document_id,
        "text": text if text is not None else f"PARENT TEXT for {parent_id}",
        "content_type": content_type,
        "metadata": {"chunk_level": 1},
    }


def _settings(mode="parent", fanout=DEFAULT_FANOUT_TOP_K):
    from knowledge_service.services.knowledge.parent_child_retrieval import (
        ParentChildSettings,
    )

    return ParentChildSettings(return_mode=mode, fanout_top_k=fanout, fanout_source="config")


def _summary_settings(prepend=True):
    from knowledge_service.services.knowledge.parent_child_retrieval import (
        SummaryIndexSettings,
    )

    return SummaryIndexSettings(prepend_summary=prepend)


def _structural_settings(boost=0.15, min_affinity=0.25, mode="heading_priority"):
    from knowledge_service.services.knowledge.structural_routing import (
        StructuralRoutingSettings,
    )

    return StructuralRoutingSettings(mode=mode, boost=boost, min_affinity=min_affinity)


# ---------------------------------------------------------------------------
# config parsing
# ---------------------------------------------------------------------------


class TestParentChildParsing:
    def test_absent_and_false_are_disabled(self):
        assert parse_parent_child_settings({}) is None
        assert parse_parent_child_settings({"parent_child": False}) is None
        assert parse_parent_child_settings({"parent_child": {"enabled": False}}) is None

    def test_boolean_shorthand_uses_defaults(self):
        settings = parse_parent_child_settings({"parent_child": True})
        assert settings is not None
        assert settings.return_mode == "parent"
        assert settings.fanout_top_k == DEFAULT_FANOUT_TOP_K
        assert settings.fanout_source == "default"

    def test_dict_form_with_explicit_values(self):
        settings = parse_parent_child_settings(
            {"parent_child": {"enabled": True, "return_mode": "CONTEXT", "fanout_top_k": 12}}
        )
        assert settings.return_mode == "context"
        assert settings.fanout_top_k == 12
        assert settings.fanout_source == "config"

    @pytest.mark.parametrize(
        "raw",
        [
            {"parent_child": "yes"},
            {"parent_child": {"return_mode": "hybrid"}},
            {"parent_child": {"fanout_top_k": 0}},
            {"parent_child": {"fanout_top_k": -3}},
            {"parent_child": {"fanout_top_k": 2001}},
            {"parent_child": {"fanout_top_k": "48"}},
            {"parent_child": {"fanout_top_k": True}},
        ],
    )
    def test_invalid_stored_configs_raise(self, raw):
        with pytest.raises(ValidationFailedError):
            parse_parent_child_settings(raw)


class TestSummaryIndexParsing:
    def test_disabled_shapes(self):
        assert parse_summary_index_settings({}) is None
        assert parse_summary_index_settings({"summary_index": False}) is None
        assert parse_summary_index_settings({"summary_index": {"enabled": False}}) is None

    def test_enabled_shapes(self):
        assert parse_summary_index_settings({"summary_index": True}).prepend_summary is True
        settings = parse_summary_index_settings(
            {"summary_index": {"enabled": True, "prepend_summary": False}}
        )
        assert settings.prepend_summary is False

    @pytest.mark.parametrize(
        "raw", [{"summary_index": 1}, {"summary_index": {"prepend_summary": "yes"}}]
    )
    def test_invalid_raise(self, raw):
        with pytest.raises(ValidationFailedError):
            parse_summary_index_settings(raw)


class TestStructuralParsing:
    def test_disabled_shapes(self):
        assert parse_structural_settings({}) is None
        assert parse_structural_settings({"structural_routing": False}) is None
        assert parse_structural_settings({"structural_routing": {"enabled": False}}) is None

    def test_defaults_and_custom(self):
        default = parse_structural_settings({"structural_routing": True})
        assert default.mode == "heading_priority"
        assert default.boost == pytest.approx(0.15)
        assert default.min_affinity == pytest.approx(0.25)
        custom = parse_structural_settings(
            {
                "structural_routing": {
                    "enabled": True,
                    "mode": "heading_priority",
                    "boost": 0.4,
                    "min_affinity": 0.0,
                }
            }
        )
        assert custom.boost == pytest.approx(0.4)

    @pytest.mark.parametrize(
        "raw",
        [
            {"structural_routing": []},
            {"structural_routing": {"mode": "document_first"}},
            {"structural_routing": {"boost": 1.5}},
            {"structural_routing": {"boost": -0.1}},
            {"structural_routing": {"boost": True}},
            {"structural_routing": {"min_affinity": "wide"}},
        ],
    )
    def test_invalid_raise(self, raw):
        with pytest.raises(ValidationFailedError):
            parse_structural_settings(raw)


# ---------------------------------------------------------------------------
# recall fan-out
# ---------------------------------------------------------------------------


class TestRecallFanout:
    def test_widens_all_legs_to_fanout(self):
        vk, kk, ck, kpk, report = apply_recall_fanout(
            _settings(fanout=48),
            vector_k=10,
            keyword_k=10,
            candidate_k=30,
            keyword_pool_k=50,
            top_k=5,
        )
        assert (vk, kk, ck, kpk) == (48, 48, 48, 50)
        assert report["fanout"]["top_k"] == 48
        assert report["fanout"]["source"] == "config"
        assert report["return_mode"] == "parent"

    def test_higher_existing_k_never_shrinks(self):
        vk, kk, ck, kpk, _ = apply_recall_fanout(
            _settings(fanout=8),
            vector_k=100,
            keyword_k=60,
            candidate_k=200,
            keyword_pool_k=300,
            top_k=5,
        )
        assert (vk, kk, ck, kpk) == (100, 60, 200, 300)

    def test_top_k_floor_and_leg_caps(self):
        # fanout below top_k raises to top_k; per-leg ceilings hold.
        vk, kk, ck, kpk, report = apply_recall_fanout(
            _settings(fanout=3),
            vector_k=1,
            keyword_k=1,
            candidate_k=1,
            keyword_pool_k=1,
            top_k=10,
        )
        assert report["fanout"]["top_k"] == 10
        assert (vk, kk) == (10, 10)
        # A fanout above a leg cap widens that leg only up to its ceiling.
        _vk, _kk, _ck, kpk, _r = apply_recall_fanout(
            _settings(fanout=2000),
            vector_k=1,
            keyword_k=1,
            candidate_k=1,
            keyword_pool_k=1,
            top_k=1,
        )
        assert kpk == 500  # _KEYWORD_POOL_UPPER_BOUND


# ---------------------------------------------------------------------------
# summary-sibling merge
# ---------------------------------------------------------------------------


def _summary_candidate(seg_id, original_id, *, score, text="summary text"):
    return {
        "segment_id": seg_id,
        "document_id": "doc-1",
        "text": text,
        "metadata": {"metadata": {"is_summary": True, "original_chunk_id": original_id}},
        "_final_score": score,
        "_fusion_score": score,
        "_sources": {"vector"},
    }


def _block_candidate(seg_id, *, score):
    return {
        "segment_id": seg_id,
        "document_id": "doc-1",
        "text": f"block body of {seg_id}",
        "metadata": {},
        "_final_score": score,
        "_fusion_score": score,
        "_sources": {"bm25"},
    }


class TestMergeSummarySiblings:
    def test_summary_hit_returns_block_and_score_is_max(self):
        block = _block_candidate("b-1", score=0.8)
        summary = _summary_candidate("s-1", "b-1", score=0.3)
        candidates = {"b-1": block, "s-1": summary}
        stats = merge_summary_siblings(candidates, settings=_summary_settings())
        assert stats == {"enabled": True, "merged": 1, "unresolved": 0}
        assert "s-1" not in candidates
        # Dify coverage-bug regression: block score must NOT be overwritten
        # by the lower summary score — combined is max().
        assert candidates["b-1"]["_final_score"] == pytest.approx(0.8)
        assert candidates["b-1"]["_fusion_score"] == pytest.approx(0.8)
        assert candidates["b-1"]["metadata"]["_summary_hit"] is True
        assert candidates["b-1"]["metadata"]["_summary_prefix"] == "summary text"
        assert candidates["b-1"]["metadata"]["_summary_segment_id"] == "s-1"

    def test_stronger_summary_raises_block_score(self):
        block = _block_candidate("b-1", score=0.2)
        summary = _summary_candidate("s-1", "b-1", score=0.9)
        candidates = {"b-1": block, "s-1": summary}
        merge_summary_siblings(candidates, settings=_summary_settings())
        assert candidates["b-1"]["_final_score"] == pytest.approx(0.9)
        assert candidates["b-1"]["_fusion_score"] == pytest.approx(0.9)
        assert set(candidates) == {"b-1"}

    def test_prepend_summary_false_skips_prefix(self):
        candidates = {
            "b-1": _block_candidate("b-1", score=0.5),
            "s-1": _summary_candidate("s-1", "b-1", score=0.4),
        }
        merge_summary_siblings(candidates, settings=_summary_settings(prepend=False))
        assert "_summary_prefix" not in candidates["b-1"]["metadata"]
        assert candidates["b-1"]["metadata"]["_summary_hit"] is True

    def test_unresolved_summary_passes_through(self):
        stray = _summary_candidate("s-9", "missing-block", score=0.7)
        candidates = {"s-9": stray}
        stats = merge_summary_siblings(candidates, settings=_summary_settings())
        assert stats["merged"] == 0
        assert stats["unresolved"] == 1
        assert candidates["s-9"] is stray

    def test_source_sets_union(self):
        candidates = {
            "b-1": _block_candidate("b-1", score=0.5),
            "s-1": _summary_candidate("s-1", "b-1", score=0.1),
        }
        merge_summary_siblings(candidates, settings=_summary_settings())
        assert candidates["b-1"]["_sources"] == {"bm25", "vector"}


# ---------------------------------------------------------------------------
# structural routing
# ---------------------------------------------------------------------------


class TestStructuralRouting:
    def test_breadcrumb_extraction_priority_and_dedupe(self):
        cand = {
            "metadata": {
                "section_header": "部署指南",
                "metadata": {
                    "breadcrumb": ["部署指南", "网络配置"],
                    "heading": "网络配置",
                },
            }
        }
        assert extract_breadcrumb(cand) == "部署指南 > 网络配置"

    def test_no_breadcrumb_fields_returns_empty(self):
        assert extract_breadcrumb({"metadata": {"content_type": "text"}}) == ""
        assert extract_breadcrumb({}) == ""

    def test_boost_only_for_affine_candidates(self):
        candidates = {
            "a": {
                "segment_id": "a",
                "metadata": {"section_header": "Deployment Network"},
                "_fusion_score": 0.5,
                "_final_score": 0.5,
            },
            "b": {
                "segment_id": "b",
                "metadata": {"section_header": "Billing FAQ"},
                "_fusion_score": 0.6,
                "_final_score": 0.6,
            },
            "c": {"segment_id": "c", "metadata": {}, "_fusion_score": 0.4, "_final_score": 0.4},
        }
        stats = apply_structural_routing(
            candidates,
            "deployment network",
            settings=_structural_settings(boost=0.4, min_affinity=0.25),
        )
        assert stats["candidates_with_breadcrumb"] == 2
        assert stats["boosted"] == 1
        # affinity = 2/2 = 1.0 -> +0.4
        assert candidates["a"]["_fusion_score"] == pytest.approx(0.9)
        assert candidates["a"]["_final_score"] == pytest.approx(0.9)
        assert candidates["a"]["metadata"]["_structural_route"]["affinity"] == pytest.approx(1.0)
        # Below-minimum affinity untouched.
        assert candidates["b"]["_fusion_score"] == pytest.approx(0.6)
        assert "_structural_route" not in candidates["b"]["metadata"]
        # No breadcrumb -> untouched.
        assert candidates["c"]["_fusion_score"] == pytest.approx(0.4)

    def test_boost_clamps_at_one(self):
        candidates = {
            "a": {
                "segment_id": "a",
                "metadata": {"section_header": "network"},
                "_fusion_score": 0.95,
                "_final_score": 0.95,
            }
        }
        apply_structural_routing(candidates, "network", settings=_structural_settings(boost=1.0))
        assert candidates["a"]["_fusion_score"] == pytest.approx(1.0)

    def test_empty_query_is_noop(self):
        candidates = {
            "a": {
                "segment_id": "a",
                "metadata": {"section_header": "network"},
                "_fusion_score": 0.5,
                "_final_score": 0.5,
            }
        }
        stats = apply_structural_routing(candidates, "   ", settings=_structural_settings())
        assert stats["boosted"] == 0
        assert candidates["a"]["_fusion_score"] == pytest.approx(0.5)


# ---------------------------------------------------------------------------
# parent-child fold
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
class TestFoldCandidatesToParents:
    async def test_no_candidates_no_children_passthrough(self):
        rows = [_child("x-1", None, score=0.9), _child("x-2", None, score=0.4)]
        db = FakeParentDb()
        folded, stats = await fold_candidates_to_parents(
            rows, settings=_settings(), db=db, dataset_id="kb", tenant_id="t"
        )
        assert folded == rows
        assert stats["parents"] == 0 and stats["child_hits"] == 0
        assert db.calls == []

    async def test_flat_and_nested_parent_links_both_fold(self):
        flat = {**_child("c-1", None, score=0.7), "metadata": {"parent_segment_id": "p-1"}}
        nested = _child("c-2", "p-1", score=0.9)
        db = FakeParentDb({"p-1": _parent_row("p-1")})
        folded, stats = await fold_candidates_to_parents(
            [flat, nested], settings=_settings(), db=db, dataset_id="kb", tenant_id="t"
        )
        assert stats["child_hits"] == 2
        assert stats["parents"] == 1
        assert stats["collapsed_children"] == 1
        assert len(folded) == 1
        assert folded[0]["segment_id"] == "p-1"
        # parent score = max(child scores), never a sum or the best child's
        # payload score alone.
        assert folded[0]["_final_score"] == pytest.approx(0.9)
        children = folded[0]["metadata"]["_parent_child"]["children"]
        assert {c["segment_id"] for c in children} == {"c-1", "c-2"}
        # scoped authority kwargs
        assert db.calls == [{"segment_id": "p-1", "dataset_id": "kb", "tenant_id": "t"}]

    async def test_ordering_emits_parent_at_best_child_slot(self):
        rows = [
            _child("c-a", "p-1", score=0.1),  # slot 0, weak child
            _child("c-b", "p-2", score=0.8),  # slot 1
            _child("c-c", "p-1", score=0.9),  # slot 2, best child of p-1
        ]
        db = FakeParentDb({"p-1": _parent_row("p-1"), "p-2": _parent_row("p-2")})
        folded, _stats = await fold_candidates_to_parents(
            rows, settings=_settings(), db=db, dataset_id="kb", tenant_id="t"
        )
        # p-2 (0.8) still precedes p-1, whose best child sat at slot 2.
        assert [r["segment_id"] for r in folded] == ["p-2", "p-1"]

    async def test_passthrough_children_keep_their_slots(self):
        rows = [
            _child("c-1", "p-1", score=0.9),
            _child("free-1", None, score=0.8),
        ]
        db = FakeParentDb({"p-1": _parent_row("p-1")})
        folded, _stats = await fold_candidates_to_parents(
            rows, settings=_settings(), db=db, dataset_id="kb", tenant_id="t"
        )
        assert [r["segment_id"] for r in folded] == ["p-1", "free-1"]

    async def test_mode_parent_replaces_text_and_identity(self):
        rows = [_child("c-1", "p-1", score=0.5, text="child fragment")]
        db = FakeParentDb({"p-1": _parent_row("p-1", text="the whole parent block")})
        folded, _stats = await fold_candidates_to_parents(
            rows, settings=_settings("parent"), db=db, dataset_id="kb", tenant_id="t"
        )
        assert folded[0]["text"] == "the whole parent block"
        assert folded[0]["segment_id"] == "p-1"
        assert folded[0]["metadata"]["content_type"] == "text"
        prov = folded[0]["metadata"]["_parent_child"]
        assert prov["mode"] == "parent" and prov["status"] == "resolved"

    async def test_mode_context_keeps_child_and_attaches_parent(self):
        rows = [_child("c-1", "p-1", score=0.5, text="child fragment")]
        db = FakeParentDb({"p-1": _parent_row("p-1", text="the whole parent block")})
        folded, _stats = await fold_candidates_to_parents(
            rows, settings=_settings("context"), db=db, dataset_id="kb", tenant_id="t"
        )
        assert folded[0]["segment_id"] == "c-1"
        assert folded[0]["text"] == "child fragment"
        assert folded[0]["metadata"]["parent_context"] == "the whole parent block"

    async def test_unresolved_parent_degrades_to_best_child(self):
        rows = [_child("c-1", "p-x", score=0.5), _child("c-2", "p-x", score=0.7)]
        db = FakeParentDb({})  # authority returns None for p-x
        folded, stats = await fold_candidates_to_parents(
            rows, settings=_settings(), db=db, dataset_id="kb", tenant_id="t"
        )
        assert stats["unresolved_parents"] == 1
        assert len(folded) == 1
        assert folded[0]["segment_id"] == "c-2"  # BEST child retained as evidence
        assert folded[0]["_final_score"] == pytest.approx(0.7)
        assert folded[0]["metadata"]["_parent_child"]["status"] == "parent_text_missing"

    async def test_per_parent_failure_does_not_break_the_fold(self):
        rows = [_child("c-1", "p-bad", score=0.5), _child("c-2", "p-ok", score=0.4)]
        db = FakeParentDb({"p-ok": _parent_row("p-ok")}, raises=["p-bad"])
        folded, stats = await fold_candidates_to_parents(
            rows, settings=_settings(), db=db, dataset_id="kb", tenant_id="t"
        )
        assert stats["unresolved_parents"] == 1
        ids = {r["segment_id"] for r in folded}
        assert ids == {"c-1", "p-ok"}

    async def test_authority_wrong_row_is_not_trusted(self):
        rows = [_child("c-1", "p-1", score=0.5)]
        db = FakeParentDb({"p-1": _parent_row("p-1")}, wrong_id=["p-1"])
        folded, stats = await fold_candidates_to_parents(
            rows, settings=_settings(), db=db, dataset_id="kb", tenant_id="t"
        )
        assert stats["unresolved_parents"] == 1
        assert folded[0]["segment_id"] == "c-1"

    async def test_cancellation_propagates(self):
        rows = [_child("c-1", "p-1", score=0.5)]
        db = FakeParentDb({"p-1": _parent_row("p-1")}, cancel=["p-1"])
        with pytest.raises(asyncio.CancelledError):
            await fold_candidates_to_parents(
                rows, settings=_settings(), db=db, dataset_id="kb", tenant_id="t"
            )

    async def test_db_without_scoped_authority_keeps_children(self):
        class _Bare:
            pass

        rows = [_child("c-1", "p-1", score=0.5)]
        folded, stats = await fold_candidates_to_parents(
            rows, settings=_settings(), db=_Bare(), dataset_id="kb", tenant_id="t"
        )
        assert stats["unresolved_parents"] == 1
        assert "authority" in stats
        assert folded[0]["segment_id"] == "c-1"

    async def test_document_id_prefers_parent_row(self):
        rows = [_child("c-1", "p-1", score=0.5)]
        db = FakeParentDb({"p-1": _parent_row("p-1", document_id="doc-parent")})
        folded, _stats = await fold_candidates_to_parents(
            rows, settings=_settings(), db=db, dataset_id="kb", tenant_id="t"
        )
        assert folded[0]["document_id"] == "doc-parent"
