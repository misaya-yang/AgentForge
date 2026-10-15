#!/usr/bin/env python3
"""T2-1 shadow: Chinese lexical tokenization offline evaluation (PRD T2-#1).

Decision to inform: should the BM25 lexical leg split Chinese text into word
tokens (jieba pre-tokenization) or rely on learned-sparse (BGE-M3), versus the
two behaviours we can run today -- PostgreSQL FTS 'simple' (one lexeme per
unbroken CJK run, which is why retrieval_service.py:279 documents the ILIKE
fallback) and a character-bigram split approximating the Qdrant ``multilingual``
tokenizer (the real bm25_v2 tokenizers run server-side in Qdrant and are
T6-owned; this offline proxy cannot change them -- and does not try to).

It is read-only by construction: the corpus and graded queries come from the
bake-off case JSONL derived from the T0 golden set (``load_cases``), and it
writes evidence under reports/ only. The learned-sparse leg is reported as
skipped unless FlagEmbedding/torch are importable (they are absent here -- the
PRD scopes BGE-M3 to the separate inference container).

Usage:

    uv run --with jieba --all-packages python scripts/chinese_lexical_shadow_eval.py \
        --cases tests/fixtures/eval/rag/bakeoff/rerank_bakeoff_v1.jsonl \
        --out reports/chinese-lexical-shadow/2026-08-29-run1

Every leg is BM25 (k1=1.2, b=0.75) over the whole corpus; only the tokenizer
differs, so any recall difference is attributable to tokenization alone.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import sys
from collections.abc import Callable
from datetime import UTC, datetime
from itertools import pairwise
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from src.services.eval.rerank_bakeoff import BakeoffCase, load_cases  # noqa: E402

# Same CJK class retrieval_service.py:288 uses for the B6 fallback decision.
_CJK_RE = re.compile(r"[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\uac00-\ud7af]")
_LATIN_TOKEN_RE = re.compile(r"[a-z0-9]+")


def _split_runs(text: str) -> list[tuple[bool, str]]:
    """Split text into (is_cjk, run) pairs, Latin runs kept whole for word split."""
    runs: list[tuple[bool, str]] = []
    for part in re.split(
        r"([\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\uac00-\ud7af]+)", text
    ):
        if not part:
            continue
        # The split pattern is a single-char class; classify by first char —
        # captured runs are pure CJK, the rest are not.
        is_cjk = bool(_CJK_RE.match(part))
        runs.append((is_cjk, part))
    return runs


def tokenize_pg_simple(text: str) -> list[str]:
    """Emulate PostgreSQL FTS ``'simple'`` config: ONE lexeme per unbroken
    CJK run plus lowercased Latin word tokens -- the documented reason a
    Chinese tsquery term can never match (see B6 comment in retrieval_service)."""
    tokens: list[str] = []
    for is_cjk, run in _split_runs(text):
        if is_cjk:
            tokens.append(run)
        else:
            tokens.extend(_LATIN_TOKEN_RE.findall(run.lower()))
    return tokens


def tokenize_bigram(text: str) -> list[str]:
    """Character-bigram split of CJK runs (single chars when a run has length
    1), Latin words unchanged -- the offline proxy for Qdrant's multilingual
    tokenizer family."""
    tokens: list[str] = []
    for is_cjk, run in _split_runs(text):
        if not is_cjk:
            tokens.extend(_LATIN_TOKEN_RE.findall(run.lower()))
            continue
        chars = list(run)
        if len(chars) == 1:
            tokens.append(chars[0])
        else:
            tokens.extend(a + b for a, b in pairwise(chars))
    return tokens


def make_jieba_tokenizer() -> Callable[[str], list[str]] | str:
    """Return a jieba tokenizer, or a skip-reason string when unavailable."""
    try:
        import jieba
    except ImportError:
        return "jieba is not installed in this environment (run via `uv run --with jieba`)"

    def tokenize(text: str) -> list[str]:
        tokens: list[str] = []
        for is_cjk, run in _split_runs(text):
            if is_cjk:
                tokens.extend(t for t in jieba.cut(run) if t.strip())
            else:
                tokens.extend(_LATIN_TOKEN_RE.findall(run.lower()))
        return tokens

    return tokenize


def _try_learned_sparse() -> Callable[[str], list[str]] | str:
    try:
        import FlagEmbedding  # noqa: F401
    except ImportError:
        return (
            "BGE-M3 learned-sparse needs FlagEmbedding/torch (absent here); the PRD "
            "scopes it to the separate inference container -- deferred to the live phase"
        )
    return "learned sparse requires the inference container; not runnable offline"


class BM25:
    """Plain Okapi BM25 over a fixed corpus of token lists."""

    def __init__(self, docs: list[list[str]], *, k1: float = 1.2, b: float = 0.75):
        self.k1 = k1
        self.b = b
        self.n = len(docs)
        self.doc_len = [len(d) for d in docs]
        self.avg_len = sum(self.doc_len) / self.n if self.n else 0.0
        self.tf: list[dict[str, int]] = []
        df: dict[str, int] = {}
        for doc in docs:
            counts: dict[str, int] = {}
            for tok in doc:
                counts[tok] = counts.get(tok, 0) + 1
            self.tf.append(counts)
            for tok in counts:
                df[tok] = df.get(tok, 0) + 1
        self.idf = {tok: math.log(1.0 + (self.n - d + 0.5) / (d + 0.5)) for tok, d in df.items()}

    def scores(self, query_tokens: list[str]) -> list[float]:
        out = [0.0] * self.n
        for tok in set(query_tokens):
            idf = self.idf.get(tok)
            if idf is None:
                continue
            for i, counts in enumerate(self.tf):
                f = counts.get(tok, 0)
                if not f:
                    continue
                denom = f + self.k1 * (1 - self.b + self.b * self.doc_len[i] / (self.avg_len or 1))
                out[i] += idf * f * (self.k1 + 1) / denom
        return out


def dcg(grades_in_order: list[int]) -> float:
    return sum((2**g - 1) / math.log2(i + 2) for i, g in enumerate(grades_in_order))


def ndcg_at_k(ranked_grades: list[int], ideal_grades: list[int], k: int) -> float:
    ideal = dcg(sorted(ideal_grades, reverse=True)[:k])
    if ideal == 0.0:
        return 0.0
    return dcg(ranked_grades[:k]) / ideal


def run_leg(
    leg_name: str,
    tokenizer: Callable[[str], list[str]],
    cases: list[BakeoffCase],
    corpus: dict[str, str],
    *,
    k: int,
    recall_cut: int,
) -> dict[str, Any]:
    ids = sorted(corpus)
    docs = [tokenizer(corpus[sid]) for sid in ids]
    index = BM25(docs)
    per_case: list[dict[str, Any]] = []
    for case in cases:
        query_tokens = tokenizer(case.query)
        scores = index.scores(query_tokens)
        order = sorted(range(len(ids)), key=lambda i: (-scores[i], ids[i]))
        ranked = [ids[i] for i in order]
        grades = [case.grade(sid) for sid in ranked]
        top = ranked[:recall_cut]
        relevant = {sid for sid, g in case.relevance.items() if g > 0}
        hits = [i for i, g in enumerate(grades) if g > 0]
        rr = 1.0 / (hits[0] + 1) if hits else 0.0
        per_case.append(
            {
                "case_id": case.case_id,
                "language": case.language,
                "recall_at_k": (
                    len(relevant.intersection(top)) / len(relevant) if relevant else 0.0
                ),
                "mrr": rr,
                "ndcg_at_k": ndcg_at_k(grades, list(case.relevance.values()), k),
                "top_ids": top[:5],
                "query_token_count": len(query_tokens),
            }
        )

    def _slice(pred: Callable[[dict[str, Any]], bool]) -> dict[str, float]:
        rows = [r for r in per_case if pred(r)]
        if not rows:
            return {"cases": 0}
        return {
            "cases": len(rows),
            "recall_at_k": sum(r["recall_at_k"] for r in rows) / len(rows),
            "mrr": sum(r["mrr"] for r in rows) / len(rows),
            "ndcg_at_k": sum(r["ndcg_at_k"] for r in rows) / len(rows),
        }

    return {
        "leg": leg_name,
        "eligible": True,
        "slices": {
            "overall": _slice(lambda _: True),
            "language:zh": _slice(lambda r: r["language"] == "zh"),
            "language:en": _slice(lambda r: r["language"] == "en"),
        },
        "per_case": per_case,
    }


def skipped_leg(name: str, reason: str) -> dict[str, Any]:
    return {"leg": name, "eligible": False, "skip_reason": reason}


def build_report(
    cases: list[BakeoffCase],
    corpus: dict[str, str],
    *,
    k: int,
    recall_cut: int,
) -> dict[str, Any]:
    legs: list[dict[str, Any]] = []
    legs.append(
        run_leg(
            "pg_simple_emulation",
            tokenize_pg_simple,
            cases,
            corpus,
            k=k,
            recall_cut=recall_cut,
        )
    )
    legs.append(
        run_leg("cjk_bigram_proxy", tokenize_bigram, cases, corpus, k=k, recall_cut=recall_cut)
    )
    jieba_or_skip = make_jieba_tokenizer()
    if callable(jieba_or_skip):
        legs.append(
            run_leg("jieba_words", jieba_or_skip, cases, corpus, k=k, recall_cut=recall_cut)
        )
    else:
        legs.append(skipped_leg("jieba_words", jieba_or_skip))
    legs.append(skipped_leg("bge_m3_learned_sparse", _try_learned_sparse()))
    return {
        "schema": "chinese-lexical-shadow-v1",
        "generated_at": datetime.now(UTC).isoformat(timespec="seconds"),
        "params": {
            "k": k,
            "recall_cut": recall_cut,
            "corpus_size": len(corpus),
            "cases": len(cases),
        },
        "legs": legs,
    }


def recommendation(report: dict[str, Any]) -> str:
    legs = {entry["leg"]: entry for entry in report["legs"]}
    zh = {
        name: (
            entry["slices"]["language:zh"].get("recall_at_k", 0.0) if entry["eligible"] else None
        )
        for name, entry in legs.items()
    }
    lines = ["## Recommendation", ""]
    pg = zh.get("pg_simple_emulation")
    bigram = zh.get("cjk_bigram_proxy")
    jieba = zh.get("jieba_words")
    lines.append(
        f"- zh recall@{report['params']['recall_cut']} — pg_simple_emulation "
        f"{_fmt(pg)}, cjk_bigram_proxy {_fmt(bigram)}, jieba_words {_fmt(jieba)}, "
        "bge_m3_learned_sparse not evaluable offline (inference container only)."
    )
    pg_entry = legs.get("pg_simple_emulation", {})
    zh_single_lexeme_collapse = all(
        row["query_token_count"] == 1
        for row in pg_entry.get("per_case", [])
        if row["language"] == "zh"
    ) and bool(pg_entry.get("per_case"))
    if zh_single_lexeme_collapse and pg is not None and bigram is not None and bigram > pg:
        lines.append(
            "- Every Chinese query collapses to a single 'simple' lexeme matching no "
            "segment (all pg leg zh scores are zero; any recall@k it shows is the "
            "alphabetical id tie-break). One lexeme per unbroken run can never match "
            "shorter query runs — the B6 mismatch made numeric."
        )
    runnable = {n: v for n, v in zh.items() if v is not None and n != "pg_simple_emulation"}
    if runnable:
        better = max(runnable, key=lambda n: runnable[n])
        lines.append(
            f"- Among runnable legs, {better} gives the best Chinese lexical recall "
            f"({runnable[better]:.3f}); "
            "recommended next step is a shadow comparison of the same split wired "
            "through the T6 bm25_v2 tokenizer lifecycle (the real Qdrant tokenizer "
            "cannot be substituted offline), with learned-sparse BGE-M3 evaluated "
            "in the inference container before any default change."
        )
    lines.append("- No serving default changes on this evidence alone (report-only contract).")
    return "\n".join(lines)


def _fmt(v: float | None) -> str:
    return "n/a" if v is None else f"{v:.3f}"


def render_markdown(report: dict[str, Any]) -> str:
    lines = [
        "# Chinese lexical shadow eval (PRD T2-1)",
        "",
        f"Generated: {report['generated_at']} | corpus={report['params']['corpus_size']} "
        f"segments | cases={report['params']['cases']} | "
        f"recall cut={report['params']['recall_cut']} | nDCG@{report['params']['k']}",
        "",
        "| leg | eligible | slice | cases | recall | mrr | ndcg |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]
    for leg in report["legs"]:
        if not leg["eligible"]:
            lines.append(f"| {leg['leg']} | False | — | — | — | — | — |")
            continue
        for s, m in leg["slices"].items():
            if not m.get("cases"):
                continue
            lines.append(
                f"| {leg['leg']} | True | {s} | {m['cases']} "
                f"| {m['recall_at_k']:.3f} | {m['mrr']:.3f} | {m['ndcg_at_k']:.3f} |"
            )
    skipped = [entry for entry in report["legs"] if not entry["eligible"]]
    if skipped:
        lines += ["", "## Skipped legs", ""]
        lines += [f"- **{entry['leg']}**: {entry['skip_reason']}" for entry in skipped]
    lines += ["", recommendation(report), ""]
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--cases",
        default="tests/fixtures/eval/rag/bakeoff/rerank_bakeoff_v1.jsonl",
    )
    parser.add_argument("--out", required=True, help="output path stem (.json/.md)")
    parser.add_argument("--top-k", type=int, default=10)
    parser.add_argument("--recall-cut", type=int, default=5)
    args = parser.parse_args(argv)

    cases = load_cases(args.cases)
    corpus: dict[str, str] = {}
    for case in cases:
        for sid, text in case.candidates:
            if sid in corpus and corpus[sid] != text:
                raise SystemExit(f"corpus conflict on {sid}")
            corpus[sid] = text
    report = build_report(cases, corpus, k=args.top_k, recall_cut=args.recall_cut)

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.with_suffix(".json").write_text(json.dumps(report, ensure_ascii=False, indent=2), "utf-8")
    out.with_suffix(".md").write_text(render_markdown(report), "utf-8")
    for leg in report["legs"]:
        zh = leg.get("slices", {}).get("language:zh", {})
        state = "skip" if not leg["eligible"] else f"zh recall={zh.get('recall_at_k', 0.0):.3f}"
        print(f"{leg['leg']}: {state}")
    print(f"report: {out.with_suffix('.json')} / {out.with_suffix('.md')}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
