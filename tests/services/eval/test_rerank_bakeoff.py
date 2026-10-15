"""Pure-unit tests for the bilingual reranker bake-off harness (PRD T2-#2).

No network, no FlagEmbedding, no serving imports on the adapter path under
test (RerankerAdapter gets an injected fake reranker so its lazy
create_reranker import never fires).
"""

from __future__ import annotations

import importlib.util
import json
import math
from pathlib import Path
from types import SimpleNamespace

import pytest

from src.services.eval.rerank_bakeoff import (
    BakeoffCase,
    IdentityAdapter,
    RerankerAdapter,
    bake_off,
    gate_decision,
    load_cases,
    ndcg_at_k,
    recall_at_k,
    reciprocal_rank,
    render_markdown,
    run_adapter,
)

REPO_ROOT = Path(__file__).resolve().parents[3]


def _case(case_id: str, language: str, ids_grades, extra=None) -> BakeoffCase:
    relevance = {seg: grade for seg, grade in ids_grades.items() if grade > 0}
    return BakeoffCase(
        case_id=case_id,
        query=f"query {case_id}",
        language=language,
        cross_lingual=bool((extra or {}).get("cross_lingual", False)),
        candidates=tuple((seg, f"text {seg}") for seg in ids_grades),
        relevance=relevance,
    )


class FakeAdapter:
    """Pre-computed scores per case; optionally raises on chosen cases."""

    def __init__(self, name: str, scores: dict[str, list[float]], fail_on=()):
        self.name = name
        self._scores = scores
        self._fail_on = set(fail_on)

    async def score(self, case: BakeoffCase) -> list[float]:
        if case.case_id in self._fail_on:
            raise RuntimeError(f"boom {case.case_id}")
        return self._scores[case.case_id]


# ---------------------------------------------------------------------------
# load_cases: parse contract + fail-closed validation
# ---------------------------------------------------------------------------


def _write_cases(tmp_path: Path, rows: list[dict]) -> Path:
    path = tmp_path / "cases.jsonl"
    path.write_text("\n".join(json.dumps(r, ensure_ascii=False) for r in rows) + "\n", "utf-8")
    return path


def test_load_cases_parses_and_drops_zero_grades(tmp_path: Path) -> None:
    path = _write_cases(
        tmp_path,
        [
            {
                "case_id": "zh-1",
                "query": "报销额度",
                "language": "zh",
                "cross_lingual": True,
                "relevance": {"seg-a": 2, "seg-b": 0},
                "candidates": [
                    {"id": "seg-a", "text": "甲"},
                    {"id": "seg-b", "text": "乙"},
                ],
            }
        ],
    )
    cases = load_cases(path)
    assert len(cases) == 1
    case = cases[0]
    assert case.language == "zh" and case.cross_lingual is True
    assert case.candidates == (("seg-a", "甲"), ("seg-b", "乙"))
    assert case.relevance == {"seg-a": 2}  # grade 0 entries are dropped
    assert case.grade("seg-b") == 0


@pytest.mark.parametrize(
    "mutate, match",
    [
        (lambda r: r.update(case_id=""), "case_id missing"),
        (lambda r: r.update(candidates=[]), "candidates must be non-empty"),
        (lambda r: r.update(relevance={}), "relevance must be a non-empty"),
        (lambda r: r.pop("relevance"), "relevance must be a non-empty"),
    ],
)
def test_load_cases_fail_closed(tmp_path: Path, mutate, match: str) -> None:
    row = {
        "case_id": "c1",
        "query": "q",
        "language": "en",
        "relevance": {"a": 1},
        "candidates": [{"id": "a", "text": "t"}],
    }
    mutate(row)
    with pytest.raises(ValueError, match=match):
        load_cases(_write_cases(tmp_path, [row]))


def test_load_cases_rejects_duplicate_ids_and_empty_file(tmp_path: Path) -> None:
    row = {
        "case_id": "c1",
        "query": "q",
        "relevance": {"a": 1},
        "candidates": [{"id": "a", "text": "t"}],
    }
    with pytest.raises(ValueError, match="duplicated"):
        load_cases(_write_cases(tmp_path, [row, dict(row)]))
    empty = tmp_path / "empty.jsonl"
    empty.write_text("\n\n", "utf-8")
    with pytest.raises(ValueError, match="no cases"):
        load_cases(empty)


# ---------------------------------------------------------------------------
# Metrics: hand-computed values
# ---------------------------------------------------------------------------


def test_ndcg_hand_computed() -> None:
    case = _case("m1", "en", {"A": 0, "B": 1})
    # k=1 with the relevant doc second: no gain inside the cut.
    assert ndcg_at_k([5.0, 1.0], case, 1) == pytest.approx(0.0)
    # k=2 with the relevant doc second: full gain discounted to rank 2.
    assert ndcg_at_k([5.0, 1.0], case, 2) == pytest.approx(1.0 / math.log2(3))
    # Relevant doc first, any k: perfect.
    assert ndcg_at_k([1.0, 5.0], case, 1) == pytest.approx(1.0)


def test_ndcg_ties_keep_baseline_order() -> None:
    case = _case("m2", "en", {"A": 0, "B": 1})
    # Equal scores must resolve by input index (stable sort), not flip.
    assert ndcg_at_k([0.0, 0.0], case, 2) == pytest.approx(1.0 / math.log2(3))


def test_recall_and_mrr() -> None:
    case = _case("m3", "en", {"A": 1, "B": 2, "C": 0})
    scores = [3.0, 2.0, 1.0]  # order A, B, C
    assert recall_at_k(scores, case, 1) == pytest.approx(0.5)
    assert recall_at_k(scores, case, 3) == pytest.approx(1.0)
    assert reciprocal_rank(scores, case) == pytest.approx(1.0)
    # C first (grade 0) pushes the first relevant doc (A) to rank 2.
    assert reciprocal_rank([1.0, 0.0, 9.0], case) == pytest.approx(0.5)


@pytest.mark.asyncio
async def test_identity_adapter_is_strictly_descending() -> None:
    case = _case("m4", "en", {"A": 1, "B": 0, "C": 0})
    values = await IdentityAdapter().score(case)
    assert values == sorted(values, reverse=True)
    assert ndcg_at_k(values, case, 1) == pytest.approx(1.0)


# ---------------------------------------------------------------------------
# RerankerAdapter: lazy reranker + -inf tail for unreturned candidates
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_reranker_adapter_tail_is_minus_inf() -> None:
    class FakeReranker:
        async def rerank(self, _query, _texts, top_n=None):
            assert top_n == 3
            return [SimpleNamespace(index=2, relevance_score=0.9)]

    adapter = RerankerAdapter(provider="bge", model="m")
    adapter._reranker = FakeReranker()  # bypass the lazy create_reranker import
    case = _case("r1", "zh", {"A": 1, "B": 2, "C": 0})
    scores = await adapter.score(case)
    assert scores[2] == pytest.approx(0.9)
    assert scores[0] == float("-inf") and scores[1] == float("-inf")
    # Unreturned candidates keep baseline relative order below the real score.
    order = sorted(range(3), key=lambda i: (-scores[i], i))
    assert order == [2, 0, 1]


# ---------------------------------------------------------------------------
# run_adapter: slices, errors disqualify
# ---------------------------------------------------------------------------


def _gate_cases() -> list[BakeoffCase]:
    return [
        _case("zh-1", "zh", {"a": 0, "b": 2}),
        _case("zh-2", "zh", {"c": 0, "d": 1}),
        _case("en-1", "en", {"e": 3, "f": 0}),
    ]


def _identity_scores() -> dict[str, list[float]]:
    return {
        "zh-1": [2.0, 1.0],
        "zh-2": [2.0, 1.0],
        "en-1": [2.0, 1.0],
    }


@pytest.mark.asyncio
async def test_run_adapter_slices_and_eligibility() -> None:
    cases = _gate_cases()
    result = await run_adapter(IdentityAdapter(), cases, k=2)
    assert result["eligible"] is True and result["errors"] == []
    assert set(result["slices"]) == {"overall", "language:zh", "language:en"}
    assert result["slices"]["language:zh"]["cases"] == 2


@pytest.mark.asyncio
async def test_run_adapter_error_disqualifies_but_keeps_scored() -> None:
    cases = _gate_cases()
    scores = _identity_scores()
    adapter = FakeAdapter("broken", scores, fail_on=["zh-2"])
    result = await run_adapter(adapter, cases, k=2)
    assert result["eligible"] is False
    assert result["errors"][0]["case_id"] == "zh-2"
    assert result["slices"]["language:zh"]["cases"] == 1  # only the scored one


@pytest.mark.asyncio
async def test_run_adapter_score_length_mismatch_is_error() -> None:
    adapter = FakeAdapter("short", {"zh-1": [1.0]})
    result = await run_adapter(adapter, [_case("zh-1", "zh", {"a": 1, "b": 0})], k=1)
    assert result["eligible"] is False
    assert "1 scores for 2 candidates" in result["errors"][0]["error"]


# ---------------------------------------------------------------------------
# gate_decision: zh net win + en no-regression
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_gate_promotes_zh_winner_without_en_regression() -> None:
    ident = await run_adapter(IdentityAdapter(), _gate_cases(), k=2)
    winner = FakeAdapter(
        "dashscope:qwen3-reranker",
        {"zh-1": [1.0, 2.0], "zh-2": [1.0, 2.0], "en-1": [2.0, 1.0]},
    )
    chal = await run_adapter(winner, _gate_cases(), k=2)
    decision = gate_decision([ident, chal])
    assert decision["promotable"] is True
    assert decision["winner"] == "dashscope:qwen3-reranker"
    assert decision["zh_net_win"] is True and decision["en_no_regression"] is True


@pytest.mark.asyncio
async def test_gate_rejects_when_zh_does_not_strictly_win() -> None:
    ident = await run_adapter(IdentityAdapter(), _gate_cases(), k=2)
    # Same zh order as identity, better only on the EN case → no zh win.
    tie = FakeAdapter("bge:tie", {"zh-1": [2.0, 1.0], "zh-2": [2.0, 1.0], "en-1": [2.0, 1.0]})
    chal = await run_adapter(tie, _gate_cases(), k=2)
    decision = gate_decision([ident, chal])
    assert decision["promotable"] is False
    assert decision["zh_net_win"] is False


@pytest.mark.asyncio
async def test_gate_rejects_on_en_regression() -> None:
    ident = await run_adapter(IdentityAdapter(), _gate_cases(), k=2)
    regress = FakeAdapter(
        "jina:regress",
        {"zh-1": [1.0, 2.0], "zh-2": [1.0, 2.0], "en-1": [1.0, 2.0]},
    )
    chal = await run_adapter(regress, _gate_cases(), k=2)
    decision = gate_decision([ident, chal])
    assert decision["promotable"] is False
    assert decision["zh_net_win"] is True and decision["en_no_regression"] is False


@pytest.mark.asyncio
async def test_gate_errors_disqualify_challenger() -> None:
    ident = await run_adapter(IdentityAdapter(), _gate_cases(), k=2)
    broken = FakeAdapter(
        "x:broken",
        {"zh-1": [1.0, 2.0], "zh-2": [1.0, 2.0], "en-1": [2.0, 1.0]},
        fail_on=["en-1"],
    )
    chal = await run_adapter(broken, _gate_cases(), k=2)
    decision = gate_decision([ident, chal])
    assert decision["promotable"] is False
    assert decision["winner"] is None
    assert "no eligible challenger" in decision["reason"]


def test_gate_requires_identity_and_zh_slice() -> None:
    fake_result = {
        "adapter": "someone",
        "eligible": True,
        "errors": [],
        "slices": {"overall": {"ndcg_at_k": 0.5}},
    }
    assert "identity baseline missing" in gate_decision([fake_result])["reason"]
    no_zh = {
        "adapter": "identity",
        "eligible": True,
        "errors": [],
        "slices": {"overall": {"ndcg_at_k": 0.5}},
    }
    decision = gate_decision([no_zh, fake_result])
    assert "no language:zh cases" in decision["reason"]


# ---------------------------------------------------------------------------
# bake_off end-to-end (offline) + report shape + markdown
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_bake_off_report_and_markdown() -> None:
    winner = FakeAdapter(
        "bge:challenger",
        {"zh-1": [1.0, 2.0], "zh-2": [1.0, 2.0], "en-1": [2.0, 1.0]},
    )
    report = await bake_off(_gate_cases(), [IdentityAdapter(), winner], k=2)
    assert report["schema"] == "rerank-bakeoff-v1"
    assert report["params"] == {"k": 2, "total_cases": 3}
    assert [a["adapter"] for a in report["adapters"]] == [
        "identity",
        "bge:challenger",
    ]
    assert report["gate"]["promotable"] is True
    json.dumps(report, ensure_ascii=False)  # must be serializable
    md = render_markdown(report)
    assert "# Reranker bake-off report" in md
    assert "bge:challenger" in md
    assert "| identity | True | language:zh" in md


# ---------------------------------------------------------------------------
# CLI wiring (scripts/reranker_bakeoff.py), parsed offline
# ---------------------------------------------------------------------------


def _load_cli():
    spec = importlib.util.spec_from_file_location(
        "reranker_bakeoff_cli", REPO_ROOT / "scripts/reranker_bakeoff.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_cli_unknown_adapter_spec() -> None:
    cli = _load_cli()
    with pytest.raises(ValueError, match="unknown adapter provider"):
        cli._parse_adapter("nope:model")
    adapter = cli._parse_adapter("dashscope:qwen3-rerank")
    assert adapter.provider == "dashscope" and adapter.model == "qwen3-rerank"
    jina = cli._parse_adapter("jina")
    assert jina.model == cli.JINA_DEFAULT_MODEL == "jina-reranker-v3"
    assert jina.url == "https://api.jina.ai/v1/rerank"


def test_cli_live_requires_flag_and_key(monkeypatch, tmp_path) -> None:
    cli = _load_cli()
    adapter = cli._parse_adapter("cohere")
    monkeypatch.delenv("COHERE_API_KEY", raising=False)
    with pytest.raises(SystemExit, match="--allow-live"):
        cli._require_live_keys([adapter], ["cohere"], allow_live=False)
    with pytest.raises(SystemExit, match="COHERE_API_KEY is not set"):
        cli._require_live_keys([adapter], ["cohere"], allow_live=True)
    monkeypatch.setenv("COHERE_API_KEY", "k")
    cli._require_live_keys([adapter], ["cohere"], allow_live=True)
    assert adapter.api_key == "k"


def test_cli_main_identity_only_writes_reports(tmp_path) -> None:
    cli = _load_cli()
    cases = _write_cases(
        tmp_path,
        [
            {
                "case_id": "zh-1",
                "query": "问题",
                "language": "zh",
                "relevance": {"b": 2},
                "candidates": [{"id": "a", "text": "甲"}, {"id": "b", "text": "乙"}],
            }
        ],
    )
    out = tmp_path / "run/report"
    assert cli.main(["--cases", str(cases), "--adapters", "identity", "--out", str(out)]) == 0
    report = json.loads(out.with_suffix(".json").read_text("utf-8"))
    assert report["schema"] == "rerank-bakeoff-v1"
    assert report["gate"]["promotable"] is False  # no challenger ran
    assert out.with_suffix(".md").exists()


def test_cli_main_requires_identity_baseline(tmp_path) -> None:
    cli = _load_cli()
    cases = _write_cases(
        tmp_path,
        [
            {
                "case_id": "c1",
                "query": "q",
                "relevance": {"a": 1},
                "candidates": [{"id": "a", "text": "t"}],
            }
        ],
    )
    with pytest.raises(SystemExit, match="identity baseline"):
        cli.main(
            [
                "--cases",
                str(cases),
                "--adapters",
                "bge",
                "--out",
                str(tmp_path / "x"),
            ]
        )
