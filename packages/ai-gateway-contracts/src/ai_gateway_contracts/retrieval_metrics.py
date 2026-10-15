"""Retrieval quality metrics (IR ranking metrics) for KB evaluation.

RAGAS-style LLM metrics (faithfulness / context precision-recall / answer
relevancy) evaluate the *generation* side and are LLM-judged. They do NOT
measure whether the retriever actually surfaced the correct chunks. Following
the research finding that "answer quality can mask retrieval failures" and that
RAGAS never shipped IR-ranking metrics, this module provides the classical,
deterministic, order-sensitive retrieval metrics:

- Hit Rate@K  (a.k.a. Recall@K as a binary "any relevant in top-K")
- Precision@K
- Recall@K
- MRR         (Mean Reciprocal Rank — rank of the first relevant item)
- nDCG@K      (Normalized Discounted Cumulative Gain — graded, rank-aware)
- MAP         (Mean Average Precision)

These are computed deterministically from a ranked list of retrieved segment IDs
and a ground-truth relevance judgement, with no LLM calls — so they are cheap,
reproducible, and safe to gate on.

Relevance judgement model
-------------------------
Two input shapes are supported:

1. **Binary**: ``relevant`` is a ``set[str]`` of relevant segment IDs. nDCG then
   uses binary relevance (1 for relevant, 0 otherwise).
2. **Graded**: ``relevance`` is a ``dict[str, float]`` mapping segment ID to a
   relevance grade in ``[0, 1]`` (or ``[0, 5]`` etc.; nDCG normalizes). Missing
   IDs are grade 0. Binary sets are promoted to grade 1.0 automatically.
"""

from __future__ import annotations

import math
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from typing import Any

__all__ = [
    "QueryRetrievalJudgement",
    "RetrievalMetrics",
    "RetrievalEvalReport",
    "hit_rate_at_k",
    "precision_at_k",
    "recall_at_k",
    "reciprocal_rank",
    "ndcg_at_k",
    "average_precision",
    "score_single_query",
    "evaluate_retrieval",
]


def _as_grade_map(relevance: Mapping[str, float] | Iterable[str] | None) -> dict[str, float]:
    """Normalize a relevance judgement to a {segment_id: grade} map.

    Accepts either a graded mapping or a binary iterable of relevant IDs.
    """
    if relevance is None:
        return {}
    if isinstance(relevance, Mapping):
        grades: dict[str, float] = {}
        for raw_segment_id, raw_grade in relevance.items():
            segment_id = str(raw_segment_id)
            grade = float(raw_grade)
            if not math.isfinite(grade) or grade < 0:
                raise ValueError("relevance grades must be finite and non-negative")
            grades[segment_id] = grade
        return grades
    return {str(sid): 1.0 for sid in relevance}


def _ranked_with_credit(retrieved: Iterable[str]) -> list[tuple[str, bool]]:
    """Return every rank slot and whether that slot may receive credit.

    Provider/fusion regressions can emit duplicate IDs. A duplicate must still
    occupy its original rank slot, otherwise a later relevant item is promoted
    and ranking quality is overstated. Only the first occurrence of an ID may
    receive relevance credit.
    """

    ranked: list[tuple[str, bool]] = []
    seen: set[str] = set()
    for raw_segment_id in retrieved:
        segment_id = str(raw_segment_id)
        receives_credit = segment_id not in seen
        if receives_credit:
            seen.add(segment_id)
        ranked.append((segment_id, receives_credit))
    return ranked


def hit_rate_at_k(
    retrieved: list[str], relevant: Mapping[str, float] | Iterable[str], k: int
) -> float:
    """1.0 if at least one relevant item appears in the top-k, else 0.0."""
    grades = _as_grade_map(relevant)
    if not grades or k <= 0:
        return 0.0
    for segment_id, receives_credit in _ranked_with_credit(retrieved)[:k]:
        if receives_credit and grades.get(segment_id, 0.0) > 0:
            return 1.0
    return 0.0


def precision_at_k(
    retrieved: list[str], relevant: Mapping[str, float] | Iterable[str], k: int
) -> float:
    """Fraction of the K ranking slots that contain a relevant item."""
    grades = _as_grade_map(relevant)
    if k <= 0:
        return 0.0
    top = _ranked_with_credit(retrieved)[:k]
    hits = sum(
        1
        for segment_id, receives_credit in top
        if receives_credit and grades.get(segment_id, 0.0) > 0
    )
    return hits / k


def recall_at_k(
    retrieved: list[str], relevant: Mapping[str, float] | Iterable[str], k: int
) -> float:
    """Fraction of all relevant items that appear in the top-k (binary)."""
    grades = _as_grade_map(relevant)
    relevant_ids = {sid for sid, g in grades.items() if g > 0}
    if not relevant_ids or k <= 0:
        return 0.0
    hits = sum(
        1
        for segment_id, receives_credit in _ranked_with_credit(retrieved)[:k]
        if receives_credit and segment_id in relevant_ids
    )
    return hits / len(relevant_ids)


def reciprocal_rank(
    retrieved: list[str],
    relevant: Mapping[str, float] | Iterable[str],
    k: int | None = None,
) -> float:
    """1/rank of the first relevant item, optionally truncated at K."""
    grades = _as_grade_map(relevant)
    ranked = _ranked_with_credit(retrieved)
    if k is not None:
        if k <= 0:
            return 0.0
        ranked = ranked[:k]
    for i, (segment_id, receives_credit) in enumerate(ranked, start=1):
        if receives_credit and grades.get(segment_id, 0.0) > 0:
            return 1.0 / i
    return 0.0


def _dcg(grades_in_rank_order: list[float], k: int) -> float:
    """Discounted Cumulative Gain at k using the log2(rank+1) discount."""
    dcg = 0.0
    for i, grade in enumerate(grades_in_rank_order[:k], start=1):
        if grade > 0:
            dcg += grade / math.log2(i + 1)
    return dcg


def ndcg_at_k(
    retrieved: list[str], relevance: Mapping[str, float] | Iterable[str], k: int
) -> float:
    """Normalized DCG at k. Supports graded relevance; binary sets use grade 1.

    nDCG = DCG(retrieved order) / IDCG(ideal order). Returns 0.0 when there is
    no relevant item (IDCG == 0).
    """
    grades = _as_grade_map(relevance)
    if k <= 0 or not grades:
        return 0.0

    retrieved_grades = [
        grades.get(segment_id, 0.0) if receives_credit else 0.0
        for segment_id, receives_credit in _ranked_with_credit(retrieved)[:k]
    ]
    dcg = _dcg(retrieved_grades, k)

    ideal_grades = sorted(grades.values(), reverse=True)
    idcg = _dcg(ideal_grades, k)
    if idcg <= 0:
        return 0.0
    return min(max(dcg / idcg, 0.0), 1.0)


def average_precision(
    retrieved: list[str],
    relevant: Mapping[str, float] | Iterable[str],
    k: int | None = None,
) -> float:
    """Average Precision for a single query, optionally truncated at K.

    AP = (1/R) * sum_{k} Precision@k * rel(k), where R is the number of relevant
    items. For AP@K, the denominator is ``min(R, K)``. Returns 0.0 if there are
    no relevant items.
    """
    grades = _as_grade_map(relevant)
    relevant_ids = {sid for sid, g in grades.items() if g > 0}
    if not relevant_ids or (k is not None and k <= 0):
        return 0.0
    ranked = _ranked_with_credit(retrieved)
    if k is not None:
        ranked = ranked[:k]
    hits = 0
    precision_sum = 0.0
    for i, (segment_id, receives_credit) in enumerate(ranked, start=1):
        if receives_credit and segment_id in relevant_ids:
            hits += 1
            precision_sum += hits / i
    denominator = min(len(relevant_ids), k) if k is not None else len(relevant_ids)
    return precision_sum / denominator


@dataclass
class QueryRetrievalJudgement:
    """One evaluation case: a query's ranked retrieval result + ground truth.

    Attributes:
        query_id: Stable identifier for the case (e.g. eval example id).
        retrieved: Segment IDs in rank order (best first).
        relevance: Ground-truth relevance — a set of relevant IDs (binary) or a
            mapping of ID -> grade in [0, 1] (graded, used by nDCG).
    """

    query_id: str
    retrieved: list[str]
    relevance: Mapping[str, float] | Iterable[str] = field(default_factory=set)

    def grade_map(self) -> dict[str, float]:
        return _as_grade_map(self.relevance)


@dataclass
class RetrievalMetrics:
    """Aggregated retrieval metrics across a set of queries, at one K."""

    k: int
    num_queries: int
    hit_rate: float
    precision: float
    recall: float
    mrr: float
    ndcg: float
    map_score: float

    def to_dict(self) -> dict[str, Any]:
        return {
            "k": self.k,
            "num_queries": self.num_queries,
            "hit_rate": self.hit_rate,
            "precision_at_k": self.precision,
            "recall_at_k": self.recall,
            "mrr": self.mrr,
            "ndcg_at_k": self.ndcg,
            "map": self.map_score,
        }


@dataclass
class RetrievalEvalReport:
    """Full retrieval evaluation report across multiple K values."""

    metrics_at_k: dict[int, RetrievalMetrics]
    per_query: dict[str, dict[str, Any]]
    k_values: list[int]

    def to_dict(self) -> dict[str, Any]:
        return {
            "k_values": self.k_values,
            "metrics": {str(k): m.to_dict() for k, m in self.metrics_at_k.items()},
            "per_query": self.per_query,
        }

    def primary(self) -> RetrievalMetrics | None:
        """The metrics at the largest K (used as the headline number)."""
        if not self.metrics_at_k:
            return None
        return self.metrics_at_k[max(self.metrics_at_k)]


def score_single_query(
    retrieved: list[str],
    relevance: Mapping[str, float] | Iterable[str],
    k_values: Iterable[int],
) -> dict[str, Any]:
    """Compute all metrics for a single query across the given K values."""
    grades = _as_grade_map(relevance)
    ranked = [str(segment_id) for segment_id in retrieved]
    unique_retrieved_count = sum(
        1 for _segment_id, receives_credit in _ranked_with_credit(ranked) if receives_credit
    )
    rr = reciprocal_rank(ranked, grades)
    ap = average_precision(ranked, grades)
    per_k: dict[str, Any] = {}
    for k in k_values:
        per_k[str(k)] = {
            "hit_rate": hit_rate_at_k(ranked, grades, k),
            "precision_at_k": precision_at_k(ranked, grades, k),
            "recall_at_k": recall_at_k(ranked, grades, k),
            "mrr": reciprocal_rank(ranked, grades, k),
            "ndcg_at_k": ndcg_at_k(ranked, grades, k),
            "map": average_precision(ranked, grades, k),
        }
    return {
        "reciprocal_rank": rr,
        "average_precision": ap,
        "retrieved_count": len(retrieved),
        "unique_retrieved_count": unique_retrieved_count,
        "by_k": per_k,
    }


def evaluate_retrieval(
    judgements: list[QueryRetrievalJudgement],
    k_values: Iterable[int] = (1, 3, 5, 10),
) -> RetrievalEvalReport:
    """Aggregate retrieval metrics across a batch of query judgements.

    Args:
        judgements: One ``QueryRetrievalJudgement`` per evaluation case.
        k_values: The K cutoffs to report (e.g. 1/3/5/10).

    Returns:
        A ``RetrievalEvalReport`` with per-K aggregates and per-query detail.
        Queries with an empty relevance judgement are counted in ``num_queries``
        for MRR/MAP (contributing 0) but excluded from hit-rate/precision/recall/
        nDCG denominators only when they have no positive — here we keep the
        standard convention of averaging 0.0 for them so a missed query hurts.
    """
    raw_k_values = list(k_values)
    if not raw_k_values:
        raise ValueError("k_values must not be empty")
    if any(isinstance(k, bool) or not isinstance(k, int) for k in raw_k_values):
        raise ValueError("k_values must contain integers")
    if any(k < 1 or k > 100 for k in raw_k_values):
        raise ValueError("k_values must be between 1 and 100")
    ks = sorted(set(raw_k_values))
    per_query: dict[str, dict[str, Any]] = {}
    accum: dict[int, dict[str, float]] = {
        k: {
            "hit": 0.0,
            "prec": 0.0,
            "rec": 0.0,
            "mrr": 0.0,
            "ndcg": 0.0,
            "map": 0.0,
        }
        for k in ks
    }
    n = len(judgements)

    for j in judgements:
        if j.query_id in per_query:
            raise ValueError(f"duplicate query_id: {j.query_id}")
        grades = j.grade_map()
        detail = score_single_query(j.retrieved, grades, ks)
        per_query[j.query_id] = detail
        for k in ks:
            accum[k]["hit"] += detail["by_k"][str(k)]["hit_rate"]
            accum[k]["prec"] += detail["by_k"][str(k)]["precision_at_k"]
            accum[k]["rec"] += detail["by_k"][str(k)]["recall_at_k"]
            accum[k]["ndcg"] += detail["by_k"][str(k)]["ndcg_at_k"]
            accum[k]["mrr"] += detail["by_k"][str(k)]["mrr"]
            accum[k]["map"] += detail["by_k"][str(k)]["map"]

    metrics_at_k: dict[int, RetrievalMetrics] = {}
    denom = max(n, 1)
    for k in ks:
        metrics_at_k[k] = RetrievalMetrics(
            k=k,
            num_queries=n,
            hit_rate=accum[k]["hit"] / denom,
            precision=accum[k]["prec"] / denom,
            recall=accum[k]["rec"] / denom,
            mrr=accum[k]["mrr"] / denom,
            ndcg=accum[k]["ndcg"] / denom,
            map_score=accum[k]["map"] / denom,
        )

    return RetrievalEvalReport(metrics_at_k=metrics_at_k, per_query=per_query, k_values=ks)
