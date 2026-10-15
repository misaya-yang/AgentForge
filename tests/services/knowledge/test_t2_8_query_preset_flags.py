"""T2-8 (PRD): query_rewrite / multi_query_expansion / hyde are flag-only.

Two contracts, both pinned here as pure units plus one pipeline check on the
shared fake-store harness:

  * reject/accept parity — the dataset write-time validator and the retrieval
    read path share ``parse_query_preset_settings``, so a config that would
    fail a query can never be persisted, and a bad stored config fails the
    request before any recall runs;
  * inertness — enabling a flag changes no results, only adds the
    ``meta["query_presets"]`` echo (applied: false + reason), and the keys are
    registered in the cache fingerprint so promotion later invalidates caches.
"""

from __future__ import annotations

import copy
from types import SimpleNamespace

import pytest
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.dataset_service import (
    _dataset_revision_fingerprint,
    _require_bounded_persisted_retrieval_config,
)
from knowledge_service.services.knowledge.query_preset_config import (
    QUERY_REWRITE_PRESETS,
    parse_query_preset_settings,
)

from tests.services.knowledge.test_retrieve_batch import _make_bm25_service

# ---------------------------------------------------------------------------
# parser: accept / reject parity (pure unit)
# ---------------------------------------------------------------------------


def test_absent_or_disabled_is_not_configured():
    for retrieval in (
        {},
        {"query_rewrite": None},
        {"query_rewrite": False},
        {"multi_query_expansion": {"enabled": False}},
        {"hyde": {"enabled": False}},
    ):
        assert parse_query_preset_settings(retrieval) is None, retrieval


def test_boolean_shorthand_echoes_defaults_inert():
    report = parse_query_preset_settings({"query_rewrite": True, "hyde": True})
    assert report == {
        "query_rewrite": {
            "enabled": True,
            "applied": False,
            "reason": "eval_gate_pending",
            "preset": "pronoun_resolution",
        },
        "hyde": {"enabled": True, "applied": False, "reason": "flag_only"},
    }


def test_preset_echo_is_normalized():
    report = parse_query_preset_settings(
        {"multi_query_expansion": {"enabled": True, "preset": " LLM_Paraphrase "}}
    )
    assert report["multi_query_expansion"]["preset"] == "llm_paraphrase"


def test_every_shipped_preset_is_accepted():
    for preset in QUERY_REWRITE_PRESETS:
        report = parse_query_preset_settings({"query_rewrite": {"preset": preset}})
        assert report["query_rewrite"]["preset"] == preset


@pytest.mark.parametrize("key", ["query_rewrite", "multi_query_expansion"])
def test_unknown_preset_rejected(key):
    with pytest.raises(ValidationFailedError, match="preset"):
        parse_query_preset_settings({key: {"enabled": True, "preset": "gpt-forever"}})


def test_hyde_takes_no_preset():
    with pytest.raises(ValidationFailedError, match="flag-only"):
        parse_query_preset_settings({"hyde": {"enabled": True, "preset": "x"}})


@pytest.mark.parametrize(
    "retrieval",
    [
        {"query_rewrite": "yes"},  # not bool/dict
        {"query_rewrite": {"enabled": "true"}},  # enabled not bool
        {"hyde": [1]},  # list not accepted
        {"multi_query_expansion": {"enabled": 1}},  # int not bool
    ],
)
def test_bad_shapes_rejected(retrieval):
    with pytest.raises(ValidationFailedError):
        parse_query_preset_settings(retrieval)


# ---------------------------------------------------------------------------
# dataset write-time validator + cache fingerprint
# ---------------------------------------------------------------------------


def test_write_validator_accepts_valid_and_rejects_invalid():
    _require_bounded_persisted_retrieval_config(
        {"retrieval": {"query_rewrite": True, "hyde": {"enabled": True}}}
    )
    with pytest.raises(ValidationFailedError, match="query_rewrite"):
        _require_bounded_persisted_retrieval_config(
            {"retrieval": {"query_rewrite": {"preset": "nope"}}}
        )


def _dataset_with(retrieval):
    return {
        "dataset_id": "kb-demo",
        "content_revision": 7,
        "embedding_provider": "dashscope",
        "embedding_model": "text-embedding-v3",
        "embedding_dimension": 1024,
        "index_config": {"retrieval": copy.deepcopy(retrieval)},
    }


def test_flags_participate_in_cache_fingerprint():
    off = _dataset_revision_fingerprint(_dataset_with({}))
    on = _dataset_revision_fingerprint(
        _dataset_with({"query_rewrite": {"enabled": True, "preset": "multi_turn_merge"}})
    )
    other = _dataset_revision_fingerprint(
        _dataset_with({"query_rewrite": {"enabled": True, "preset": "pronoun_resolution"}})
    )
    assert off != on != other


def test_bool_shorthand_fingerprint_survives_scalar_branch():
    # ``query_rewrite: true`` must still move the fingerprint (T9 precedent).
    off = _dataset_revision_fingerprint(_dataset_with({}))
    on = _dataset_revision_fingerprint(_dataset_with({"hyde": True}))
    assert off != on


# ---------------------------------------------------------------------------
# pipeline: inert on results, fails closed, echoes in meta
# ---------------------------------------------------------------------------


def _rows():
    return [
        {
            "segment_id": f"s-{i}",
            "dataset_id": "kb-demo",
            "document_id": "doc-1",
            "position": i,
            "text": f"alpha beta {i} gamma" if i else "alpha beta",
            "token_count": 3,
            "metadata": {},
            "source_type": "manual",
            "language": "en",
        }
        for i in range(3)
    ]


def _service_with_config(retrieval):
    svc, _database = _make_bm25_service(_rows())
    original = svc._ks.require_dataset_access

    async def _access(user, dataset_id, required="viewer"):
        dataset = await original(user, dataset_id, required=required)
        dataset["index_config"] = {"retrieval": copy.deepcopy(retrieval)}
        return dataset

    svc._ks.require_dataset_access = _access
    return svc


async def _retrieve(svc, query="alpha beta", top_k=3):
    return await svc.retrieve(
        user=SimpleNamespace(),
        dataset_id="kb-demo",
        query=query,
        top_k=top_k,
        mode="bm25",
        rerank=False,
    )


@pytest.mark.asyncio
async def test_enabled_flag_is_inert_and_echoed():
    baseline, base_meta = await _retrieve(_service_with_config({}))
    flagged, meta = await _retrieve(
        _service_with_config({"query_rewrite": True, "hyde": {"enabled": True}})
    )
    assert [r.segment_id for r in flagged] == [r.segment_id for r in baseline]
    assert "query_presets" not in base_meta
    assert meta["query_presets"] == {
        "query_rewrite": {
            "enabled": True,
            "applied": False,
            "reason": "eval_gate_pending",
            "preset": "pronoun_resolution",
        },
        "hyde": {"enabled": True, "applied": False, "reason": "flag_only"},
    }


@pytest.mark.asyncio
async def test_bad_stored_config_fails_the_request():
    svc = _service_with_config({"multi_query_expansion": {"preset": "quantum"}})
    with pytest.raises(ValidationFailedError, match="preset"):
        await _retrieve(svc)
