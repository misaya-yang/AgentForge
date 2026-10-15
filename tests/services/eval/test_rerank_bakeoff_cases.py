"""Contract tests for the committed bilingual bake-off case set (PRD T2-2c)."""

from __future__ import annotations

import asyncio
import importlib.util
import json
from pathlib import Path

from src.services.eval.rerank_bakeoff import IdentityAdapter, load_cases, ndcg_at_k

REPO_ROOT = Path(__file__).resolve().parents[3]
FIXTURE = REPO_ROOT / "tests/fixtures/eval/rag/bakeoff/rerank_bakeoff_v1.jsonl"


def _builder():
    spec = importlib.util.spec_from_file_location(
        "build_rerank_bakeoff_cases",
        REPO_ROOT / "scripts/build_rerank_bakeoff_cases.py",
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_fixture_loads_with_expected_slices() -> None:
    cases = load_cases(FIXTURE)
    assert len(cases) == 12
    zh = [c for c in cases if c.language == "zh"]
    en = [c for c in cases if c.language == "en"]
    assert len(zh) == 6 and len(en) == 6
    # zh slice mirrors the seeded golden: zh queries are cross-lingual.
    assert all(c.cross_lingual for c in zh)
    assert not any(c.cross_lingual for c in en)
    assert all(c.candidates and c.relevance for c in cases)


def test_grade3_target_always_present_in_candidates() -> None:
    for case in load_cases(FIXTURE):
        ids = {seg for seg, _ in case.candidates}
        top = {seg for seg, grade in case.relevance.items() if grade >= 3}
        assert top, case.case_id
        assert top <= ids, f"{case.case_id}: graded ids missing from candidates"


def test_identity_baseline_has_zh_headroom() -> None:
    # The gate is only meaningful if the fusion-order baseline is imperfect
    # on the Chinese slice — a reranker must be able to strictly beat it.
    cases = [c for c in load_cases(FIXTURE) if c.language == "zh"]
    scores_identity = {c.case_id: IdentityAdapter().score(c) for c in cases}
    import asyncio

    values = {cid: asyncio.run(fut) for cid, fut in scores_identity.items()}
    zh_ndcg = sum(ndcg_at_k(values[c.case_id], c, 10) for c in cases) / len(cases)
    assert zh_ndcg < 0.9, "baseline too perfect: bake-off gate cannot fire"
    assert zh_ndcg > 0.0, "baseline broken: no relevance signal in cases"


def test_oracle_ranking_would_pass_the_gate() -> None:
    # End-to-end sanity: a perfect ranker over these cases satisfies the
    # promotion gate (zh strict win, en no regression). If this ever fails,
    # the case set itself — not the reranker — is broken.
    from src.services.eval.rerank_bakeoff import bake_off

    class OracleAdapter:
        name = "oracle"

        async def score(self, case):
            return [float(case.grade(seg)) for seg, _ in case.candidates]

    report = asyncio.run(bake_off(load_cases(FIXTURE), [IdentityAdapter(), OracleAdapter()], k=10))
    decision = report["gate"]
    assert decision["promotable"] is True, decision
    assert decision["winner"] == "oracle"


def test_fixture_is_byte_deterministic() -> None:
    builder = _builder()
    rendered = "\n".join(
        json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
        for row in builder.build_rows()
    )
    assert rendered + "\n" == FIXTURE.read_text("utf-8")
