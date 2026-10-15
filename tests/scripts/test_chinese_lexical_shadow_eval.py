"""T2-1 shadow eval: pure-unit coverage of tokenizers, BM25, and metrics.

The evidence claim these tests protect is narrow and mechanical: the three
offline legs differ ONLY in tokenization, pg_simple must collapse on Chinese
(one lexeme per unbroken run), and the bigram proxy must recover the recall
that collapse loses. jieba is not a project dependency, so its leg is tested
under ``importorskip`` (the evidence run uses ``uv run --with jieba``).
"""

from __future__ import annotations

import importlib.util
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]

_spec = importlib.util.spec_from_file_location(
    "chinese_lexical_shadow_eval", REPO_ROOT / "scripts/chinese_lexical_shadow_eval.py"
)
sh = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sh)


# ---------------------------------------------------------------------------
# tokenizers
# ---------------------------------------------------------------------------


def test_pg_simple_emits_one_lexeme_per_cjk_run():
    tokens = sh.tokenize_pg_simple("年付订阅如何申请退款。Annual refunds follow.")
    assert "年付订阅如何申请退款" in tokens  # whole run, punctuation cut
    assert "annual" in tokens and "refunds" in tokens
    assert "退款" not in tokens  # no sub-run tokenization — the B6 mismatch


def test_pg_simple_query_shorter_than_segment_run_never_matches():
    query = sh.tokenize_pg_simple("年付订阅如何申请退款")
    doc = sh.tokenize_pg_simple("年付订阅的退款资格以公示的退款窗口期为准。")
    assert set(query).isdisjoint(doc)


def test_bigram_splits_cjk_runs_and_keeps_latin_words():
    tokens = sh.tokenize_bigram("退款窗口 opens 7 days")
    assert "退" + "款" in tokens
    assert "款" + "窗" in tokens
    assert "opens" in tokens and "7" in tokens and "days" in tokens


def test_bigram_single_char_run_emits_unigram():
    assert sh.tokenize_bigram("税") == ["税"]


def test_jieba_leg_when_available():
    pytest.importorskip("jieba")
    tokenize = sh.make_jieba_tokenizer()
    assert callable(tokenize)
    tokens = tokenize("年付订阅如何申请退款")
    assert "退款" in tokens and "订阅" in tokens
    assert len(tokens) > 1  # split into words, not one run


def test_jieba_skip_reason_when_absent(monkeypatch):
    import builtins

    real_import = builtins.__import__

    def _no_jieba(name, *args, **kwargs):
        if name == "jieba":
            raise ImportError("blocked for test")
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", _no_jieba)
    result = sh.make_jieba_tokenizer()
    assert isinstance(result, str) and "jieba" in result


# ---------------------------------------------------------------------------
# BM25 + metrics
# ---------------------------------------------------------------------------


def test_bm25_prefers_matching_doc():
    docs = [["alpha", "beta"], ["gamma"], ["alpha", "alpha"]]
    index = sh.BM25(docs)
    scores = index.scores(["alpha"])
    assert scores[2] > scores[0] > 0.0
    assert scores[1] == 0.0


def test_bm25_idf_penalizes_common_token():
    docs = [["common", "rare"], ["common"], ["common"], ["common"]]
    index = sh.BM25(docs)
    scores = index.scores(["rare"])
    assert scores[0] > 0.0 and scores[1:] == [0.0, 0.0, 0.0]


def test_ndcg_at_k_hand_computed():
    # grades [0,3] vs ideal [3]: DCG = 0 + 7/log2(3), IDCG = 7/log2(2)
    assert sh.ndcg_at_k([0, 3], [3], 2) == pytest.approx(1 / 1.584962500721156)
    assert sh.ndcg_at_k([3], [3], 1) == pytest.approx(1.0)
    assert sh.ndcg_at_k([0, 0], [0], 2) == 0.0


# ---------------------------------------------------------------------------
# end-to-end over the fixture (offline legs only, deterministic)
# ---------------------------------------------------------------------------


def test_shadow_report_collapses_pg_and_recovers_bigram_on_zh():
    from src.services.eval.rerank_bakeoff import load_cases

    cases = load_cases(REPO_ROOT / "tests/fixtures/eval/rag/bakeoff/rerank_bakeoff_v1.jsonl")
    corpus: dict[str, str] = {}
    for case in cases:
        for sid, text in case.candidates:
            corpus.setdefault(sid, text)
    assert len(corpus) == 30  # 6 families x 5 segments

    report = sh.build_report(cases, corpus, k=10, recall_cut=5)
    legs = {entry["leg"]: entry for entry in report["legs"]}
    assert legs["bge_m3_learned_sparse"]["eligible"] is False

    pg = legs["pg_simple_emulation"]
    pg_zh_rows = [r for r in pg["per_case"] if r["language"] == "zh"]
    # Every Chinese query collapses to ONE lexeme that matches no document:
    # all scores are zero, so every zh case ranks by the alphabetical id
    # tie-break — any nonzero recall@5 is luck of the id sort, not signal.
    assert all(r["query_token_count"] == 1 for r in pg_zh_rows)
    assert len({tuple(r["top_ids"]) for r in pg_zh_rows}) == 1
    assert pg_zh_rows[0]["top_ids"] == sorted(corpus)[:5]

    pg_zh = pg["slices"]["language:zh"]
    bigram_zh = legs["cjk_bigram_proxy"]["slices"]["language:zh"]
    assert bigram_zh["recall_at_k"] >= 5 / 6
    assert bigram_zh["mrr"] > pg_zh["mrr"]
    # English uses the same Latin tokens in both legs; only doc-length
    # normalization differs, so recall must be identical.
    pg_en = legs["pg_simple_emulation"]["slices"]["language:en"]["recall_at_k"]
    bigram_en = legs["cjk_bigram_proxy"]["slices"]["language:en"]["recall_at_k"]
    assert pg_en == bigram_en
    assert "Recommendation" in sh.render_markdown(report)
