"""Bilingual reranker bake-off scoring (PRD T2-#2).

Scores candidate reranker backends (incumbent BGE-v2-m3, Qwen3-Reranker,
jina-reranker-v3, …) against graded-relevance cases whose candidate lists
were fixed by the retrieval baseline, so the comparison isolates reranking
quality from recall. Report-only: nothing here changes serving config — the
winner is promoted through the T0 gate afterwards ("只跑评测，不切默认").

Gate semantics (PRD: 中文切片净胜为门禁): the challenger must STRICTLY beat
the identity baseline (fusion order, i.e. no rerank) on the Chinese slice's
nDCG@k, and must not regress the English slice beyond ``eps``. Any adapter
that errored on any case is disqualified (fail-closed) — partial scores are
not a win.

Input JSONL (one case per line, ``load_cases`` contract, validated):
    {"case_id": "...", "query": "...", "language": "zh|en",
     "cross_lingual": false, "relevance": {"seg-id": 3, "seg2": 1},
     "candidates": [{"id": "seg-id", "text": "..."}]}
``relevance`` grades are integers >= 0; candidates not listed are grade 0.
"""

from __future__ import annotations

import json
import math
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Protocol


@dataclass(frozen=True)
class BakeoffCase:
    case_id: str
    query: str
    language: str
    cross_lingual: bool
    candidates: tuple[tuple[str, str], ...]  # (segment_id, text) in baseline order
    relevance: dict[str, int]

    def grade(self, segment_id: str) -> int:
        return int(self.relevance.get(segment_id, 0))


def load_cases(path: str | Path) -> list[BakeoffCase]:
    """Parse and validate the bake-off case JSONL. Raises on any violation."""
    cases: list[BakeoffCase] = []
    seen: set[str] = set()
    for line_no, line in enumerate(Path(path).read_text("utf-8").splitlines(), 1):
        if not line.strip():
            continue
        row = json.loads(line)  # caller-facing fail-closed on malformed JSONL
        if not isinstance(row, dict):
            raise ValueError(f"case line {line_no} must be an object")
        case_id = str(row.get("case_id") or "").strip()
        query = str(row.get("query") or "").strip()
        if not case_id or case_id in seen:
            raise ValueError(f"case line {line_no}: case_id missing or duplicated")
        seen.add(case_id)
        candidates = tuple((str(c["id"]), str(c["text"])) for c in row.get("candidates") or [])
        if not candidates:
            raise ValueError(f"case {case_id}: candidates must be non-empty")
        relevance = row.get("relevance")
        if not isinstance(relevance, dict) or not relevance:
            raise ValueError(f"case {case_id}: relevance must be a non-empty object")
        normalized = {str(seg): int(grade) for seg, grade in relevance.items() if int(grade) > 0}
        if any(grade < 0 for grade in normalized.values()):
            raise ValueError(f"case {case_id}: relevance grades must be >= 0")
        cases.append(
            BakeoffCase(
                case_id=case_id,
                query=query,
                language=str(row.get("language") or "en"),
                cross_lingual=bool(row.get("cross_lingual") or False),
                candidates=candidates,
                relevance=normalized,
            )
        )
    if not cases:
        raise ValueError(f"{path}: no cases")
    return cases


class RerankAdapter(Protocol):
    """One bake-off contestant: scores candidate texts, preserving no order."""

    name: str

    async def score(self, case: BakeoffCase) -> list[float]:
        """Return one score per candidate, input order preserved."""
        ...


class IdentityAdapter:
    """Baseline: keep the retrieval-baseline order (fusion order, no rerank)."""

    name = "identity"

    async def score(self, case: BakeoffCase) -> list[float]:
        count = len(case.candidates)
        return [float(count - i) for i in range(count)]


@dataclass
class RerankerAdapter:
    """Contestant using a factory injected by the standalone runner."""

    provider: str
    model: str | None = None
    api_key: str | None = None
    _reranker: Any = field(default=None, repr=False)
    reranker_factory: Any = field(default=None, repr=False)

    @property
    def name(self) -> str:
        return f"{self.provider}:{self.model or 'default'}"

    async def score(self, case: BakeoffCase) -> list[float]:
        if self._reranker is None:
            if not callable(self.reranker_factory):
                raise ValueError("a reranker factory must be supplied by the runner")
            self._reranker = self.reranker_factory(
                provider=self.provider, model=self.model, api_key=self.api_key
            )
        results = await self._reranker.rerank(
            case.query,
            [text for _seg, text in case.candidates],
            top_n=len(case.candidates),
        )
        # Providers may return fewer than asked; unreturned candidates fall
        # below every real relevance score, keeping baseline order among them.
        scores = [float("-inf") for _ in case.candidates]
        for hit in results or []:
            if 0 <= int(hit.index) < len(scores):
                scores[int(hit.index)] = float(hit.relevance_score)
        return scores


@dataclass
class HttpRerankAdapter:
    """Cohere-wire-compatible hosted rerank endpoint (e.g. jina-reranker-v3).

    Bake-off only: it deliberately bypasses the serving create_reranker stack
    so evaluating a new provider never touches production provider wiring.
    """

    name: str
    url: str
    model: str
    api_key: str

    async def score(self, case: BakeoffCase) -> list[float]:
        import httpx

        async with httpx.AsyncClient(timeout=60.0) as client:
            response = await client.post(
                self.url,
                json={
                    "model": self.model,
                    "query": case.query,
                    "documents": [text for _seg, text in case.candidates],
                    "top_n": len(case.candidates),
                },
                headers={
                    "Authorization": f"Bearer {self.api_key}",
                    "Content-Type": "application/json",
                },
            )
            response.raise_for_status()
            data = response.json()
        scores = [float("-inf") for _ in case.candidates]
        for hit in data.get("results") or []:
            index = int(hit.get("index", -1))
            if 0 <= index < len(scores):
                scores[index] = float(hit.get("relevance_score", 0.0))
        return scores


def _dcg(grades_in_rank_order: list[int], k: int) -> float:
    return sum(
        (2**grade - 1) / math.log2(i + 2) for i, grade in enumerate(grades_in_rank_order[:k])
    )


def ndcg_at_k(scores: list[float], case: BakeoffCase, k: int) -> float:
    # Stable descending sort on (-score, rank) mirrors pipeline tie handling.
    order = sorted(range(len(scores)), key=lambda i: (-scores[i], i))
    ranked_grades = [case.grade(case.candidates[i][0]) for i in order]
    ideal_grades = sorted((case.grade(seg) for seg, _ in case.candidates), reverse=True)
    idcg = _dcg(ideal_grades, k)
    if idcg == 0.0:
        return 0.0
    return _dcg(ranked_grades, k) / idcg


def recall_at_k(scores: list[float], case: BakeoffCase, k: int) -> float:
    relevant = {seg for seg, grade in case.relevance.items() if grade > 0}
    if not relevant:
        return 0.0
    order = sorted(range(len(scores)), key=lambda i: (-scores[i], i))
    hits = sum(1 for i in order[:k] if case.candidates[i][0] in relevant)
    return hits / len(relevant)


def reciprocal_rank(scores: list[float], case: BakeoffCase) -> float:
    order = sorted(range(len(scores)), key=lambda i: (-scores[i], i))
    for rank, i in enumerate(order, start=1):
        if case.grade(case.candidates[i][0]) > 0:
            return 1.0 / rank
    return 0.0


def _slice_metrics(
    cases: list[BakeoffCase], scores_by_case: dict[str, list[float]], k: int
) -> dict[str, float]:
    if not cases:
        return {"cases": 0, "ndcg_at_k": 0.0, "recall_at_k": 0.0, "mrr": 0.0}
    return {
        "cases": len(cases),
        "ndcg_at_k": round(
            sum(ndcg_at_k(scores_by_case[c.case_id], c, k) for c in cases) / len(cases),
            6,
        ),
        "recall_at_k": round(
            sum(recall_at_k(scores_by_case[c.case_id], c, k) for c in cases) / len(cases),
            6,
        ),
        "mrr": round(
            sum(reciprocal_rank(scores_by_case[c.case_id], c) for c in cases) / len(cases),
            6,
        ),
    }


async def run_adapter(adapter: RerankAdapter, cases: list[BakeoffCase], k: int) -> dict:
    """Score one contestant; per-case errors are collected, never swallowed."""
    scores_by_case: dict[str, list[float]] = {}
    errors: list[dict[str, str]] = []
    for case in cases:
        try:
            scores = await adapter.score(case)
        except Exception as exc:  # noqa: BLE001 - bake-off must report, not crash
            errors.append({"case_id": case.case_id, "error": str(exc)[:300]})
            continue
        if len(scores) != len(case.candidates):
            errors.append(
                {
                    "case_id": case.case_id,
                    "error": f"adapter returned {len(scores)} scores for "
                    f"{len(case.candidates)} candidates",
                }
            )
            continue
        scores_by_case[case.case_id] = scores
    scored = [c for c in cases if c.case_id in scores_by_case]
    slices: dict[str, dict[str, float]] = {
        "overall": _slice_metrics(scored, scores_by_case, k),
    }
    for language in sorted({c.language for c in scored}):
        slices[f"language:{language}"] = _slice_metrics(
            [c for c in scored if c.language == language], scores_by_case, k
        )
    cross = [c for c in scored if c.cross_lingual]
    if cross:
        slices["cross_lingual"] = _slice_metrics(cross, scores_by_case, k)
    return {
        "adapter": adapter.name,
        "eligible": not errors,
        "errors": errors,
        "slices": slices,
    }


def gate_decision(
    results: list[dict], *, zh_slice: str = "language:zh", eps: float = 1e-9
) -> dict[str, Any]:
    """Promotion gate: Chinese-slice net win vs identity, no EN regression."""
    by_name = {r["adapter"]: r for r in results}
    baseline = by_name.get(IdentityAdapter.name)
    eligible = [r for r in results if r["eligible"] and r["adapter"] != IdentityAdapter.name]
    decision: dict[str, Any] = {
        "promotable": False,
        "winner": None,
        "reason": "",
    }
    if baseline is None:
        decision["reason"] = "identity baseline missing from results"
        return decision
    if zh_slice not in baseline["slices"]:
        decision["reason"] = f"no {zh_slice} cases in the bake-off set"
        return decision
    if not eligible:
        decision["reason"] = "no eligible challenger (all errored or none ran)"
        return decision
    winner = max(eligible, key=lambda r: r["slices"]["overall"]["ndcg_at_k"])
    w_zh = winner["slices"].get(zh_slice, {"ndcg_at_k": 0.0})["ndcg_at_k"]
    zh_net_win = w_zh > baseline["slices"][zh_slice]["ndcg_at_k"] + eps
    en_key = "language:en"
    if en_key not in baseline["slices"]:
        en_no_regression = True
    else:
        w_en = winner["slices"].get(en_key, {"ndcg_at_k": 0.0})["ndcg_at_k"]
        en_no_regression = w_en >= baseline["slices"][en_key]["ndcg_at_k"] - eps
    decision["winner"] = winner["adapter"]
    decision["zh_net_win"] = zh_net_win
    decision["en_no_regression"] = en_no_regression
    decision["promotable"] = zh_net_win and en_no_regression
    if not decision["promotable"]:
        decision["reason"] = (
            f"challenger {winner['adapter']} failed "
            f"{'zh net win' if not zh_net_win else 'en no-regression'}"
        )
    return decision


def build_report(
    *, cases: list[BakeoffCase], results: list[dict], k: int, generated_at: str
) -> dict[str, Any]:
    return {
        "schema": "rerank-bakeoff-v1",
        "generated_at": generated_at,
        "params": {"k": k, "total_cases": len(cases)},
        "gate": gate_decision(results),
        "adapters": results,
    }


def render_markdown(report: dict[str, Any]) -> str:
    lines = [
        "# Reranker bake-off report",
        "",
        f"Generated: {report['generated_at']} | k={report['params']['k']} | "
        f"cases={report['params']['total_cases']}",
        "",
        "## Gate",
        "",
        "```json",
        json.dumps(report["gate"], ensure_ascii=False, indent=2),
        "```",
        "",
        "## Adapter slices (nDCG@k / recall@k / MRR)",
        "",
        "| adapter | eligible | slice | cases | ndcg | recall | mrr |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]
    for result in report["adapters"]:
        for slice_name, metrics in result["slices"].items():
            lines.append(
                f"| {result['adapter']} | {result['eligible']} | {slice_name} "
                f"| {metrics['cases']} | {metrics['ndcg_at_k']:.4f} "
                f"| {metrics['recall_at_k']:.4f} | {metrics['mrr']:.4f} |"
            )
    errored = [r for r in report["adapters"] if r["errors"]]
    if errored:
        lines += ["", "## Errors (adapters disqualified)", ""]
        for result in errored:
            for err in result["errors"]:
                lines.append(f"- `{result['adapter']}` case {err['case_id']}: {err['error']}")
    return "\n".join(lines) + "\n"


async def bake_off(
    cases: list[BakeoffCase], adapters: list[RerankAdapter], *, k: int = 10
) -> dict[str, Any]:
    results = []
    for adapter in adapters:
        results.append(await run_adapter(adapter, cases, k))
    from datetime import datetime, timezone

    return build_report(
        cases=cases,
        results=results,
        k=k,
        generated_at=datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    )
