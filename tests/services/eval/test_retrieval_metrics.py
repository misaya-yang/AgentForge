"""Unit tests for deterministic IR retrieval metrics (services/eval/retrieval_metrics.py).

These metrics underpin the KB retrieval evaluation workbench. They must be
correct and reproducible because they are used for A/B comparison and gating.
"""

from __future__ import annotations

import math

import pytest
from knowledge_service.services.eval.retrieval_metrics import (
    QueryRetrievalJudgement,
    average_precision,
    evaluate_retrieval,
    hit_rate_at_k,
    ndcg_at_k,
    precision_at_k,
    recall_at_k,
    reciprocal_rank,
    score_single_query,
)

RETRIEVED = ["a", "b", "c", "d"]
RELEVANT = {"a", "c"}


def test_hit_rate_at_k():
    assert hit_rate_at_k(RETRIEVED, RELEVANT, 1) == 1.0  # "a" is relevant
    assert hit_rate_at_k(["x", "a"], RELEVANT, 1) == 0.0
    assert hit_rate_at_k(["x", "a"], RELEVANT, 2) == 1.0
    assert hit_rate_at_k(["x", "y"], RELEVANT, 5) == 0.0
    assert hit_rate_at_k(RETRIEVED, set(), 5) == 0.0  # no ground truth


def test_precision_at_k():
    assert precision_at_k(RETRIEVED, RELEVANT, 2) == pytest.approx(0.5)  # a of {a,b}
    assert precision_at_k(RETRIEVED, RELEVANT, 4) == pytest.approx(0.5)  # a,c of 4
    assert precision_at_k(RETRIEVED, RELEVANT, 1) == pytest.approx(1.0)
    assert precision_at_k([], RELEVANT, 5) == 0.0
    # Precision@K uses the requested cutoff, including unfilled ranking slots.
    assert precision_at_k(["a"], RELEVANT, 10) == pytest.approx(0.1)


def test_recall_at_k():
    assert recall_at_k(RETRIEVED, RELEVANT, 4) == pytest.approx(1.0)  # both a,c found
    assert recall_at_k(RETRIEVED, RELEVANT, 1) == pytest.approx(0.5)  # only a
    assert recall_at_k(["x", "y"], RELEVANT, 2) == 0.0


def test_reciprocal_rank():
    assert reciprocal_rank(RETRIEVED, RELEVANT) == pytest.approx(1.0)  # a at rank 1
    assert reciprocal_rank(["x", "c", "a"], RELEVANT) == pytest.approx(0.5)  # c at rank 2
    assert reciprocal_rank(["x", "y", "z"], RELEVANT) == 0.0


def test_average_precision():
    # relevant {a,c}: rank1 hit prec=1.0, rank3 hit prec=2/3 -> AP=(1 + 2/3)/2
    assert average_precision(RETRIEVED, RELEVANT) == pytest.approx((1.0 + 2 / 3) / 2)
    assert average_precision(["x", "y"], RELEVANT) == 0.0


def test_ndcg_at_k_binary():
    # grades in order [1,0,1,0]; DCG = 1/log2(2) + 1/log2(4) = 1.5
    # IDCG (both relevant on top): 1/log2(2) + 1/log2(3)
    dcg = 1.0 / math.log2(2) + 1.0 / math.log2(4)
    idcg = 1.0 / math.log2(2) + 1.0 / math.log2(3)
    assert ndcg_at_k(RETRIEVED, RELEVANT, 4) == pytest.approx(dcg / idcg)
    # Perfect ranking -> nDCG = 1.0
    assert ndcg_at_k(["a", "c", "b", "d"], RELEVANT, 4) == pytest.approx(1.0)
    assert ndcg_at_k(["x", "y"], RELEVANT, 2) == 0.0


def test_ndcg_at_k_graded():
    relevance = {"a": 3.0, "b": 2.0, "c": 3.0}
    retrieved = ["a", "b"]
    dcg = 3.0 / math.log2(2) + 2.0 / math.log2(3)
    idcg = 3.0 / math.log2(2) + 3.0 / math.log2(3)  # ideal top-2 grades: 3,3
    assert ndcg_at_k(retrieved, relevance, 2) == pytest.approx(dcg / idcg)


def test_score_single_query_shape():
    detail = score_single_query(RETRIEVED, RELEVANT, [1, 5])
    assert detail["reciprocal_rank"] == pytest.approx(1.0)
    assert detail["average_precision"] == pytest.approx((1.0 + 2 / 3) / 2)
    assert detail["by_k"]["1"]["hit_rate"] == 1.0
    assert detail["by_k"]["5"]["recall_at_k"] == pytest.approx(1.0)


def test_evaluate_retrieval_aggregation():
    judgements = [
        # Perfect retrieval
        QueryRetrievalJudgement("q1", ["a", "c", "b"], {"a", "c"}),
        # Missed entirely
        QueryRetrievalJudgement("q2", ["x", "y", "z"], {"a"}),
    ]
    report = evaluate_retrieval(judgements, k_values=[1, 3])
    m1 = report.metrics_at_k[1]
    m3 = report.metrics_at_k[3]

    # hit_rate@1: q1=1 (a at top), q2=0 -> 0.5
    assert m1.hit_rate == pytest.approx(0.5)
    # MRR: q1 rr=1.0, q2 rr=0.0 -> 0.5
    assert m3.mrr == pytest.approx(0.5)
    # recall@3: q1=1.0 (a,c), q2=0.0 -> 0.5
    assert m3.recall == pytest.approx(0.5)
    # MAP: q1 AP=1.0, q2=0.0 -> 0.5
    assert m3.map_score == pytest.approx(0.5)
    assert m3.num_queries == 2

    # Primary = largest K
    assert report.primary() is m3

    # Serialization
    d = report.to_dict()
    assert d["k_values"] == [1, 3]
    assert "3" in d["metrics"]
    assert d["metrics"]["3"]["mrr"] == pytest.approx(0.5)
    assert "q1" in d["per_query"]


def test_evaluate_retrieval_empty():
    report = evaluate_retrieval([], k_values=[5])
    assert report.primary() is not None
    assert report.primary().num_queries == 0
    assert report.primary().mrr == 0.0


def test_duplicate_retrieval_ids_receive_relevance_credit_once():
    retrieved = ["a", "a", "a", "b"]
    relevant = {"a"}

    detail = score_single_query(retrieved, relevant, [1, 2, 4])

    assert detail["retrieved_count"] == 4
    assert detail["unique_retrieved_count"] == 2
    assert recall_at_k(retrieved, relevant, 4) == 1.0
    assert precision_at_k(retrieved, relevant, 4) == pytest.approx(0.25)
    assert average_precision(retrieved, relevant) == 1.0
    assert ndcg_at_k(retrieved, relevant, 4) == 1.0
    for by_k in detail["by_k"].values():
        assert all(0.0 <= value <= 1.0 for value in by_k.values())


def test_duplicate_slots_do_not_promote_later_relevant_results():
    retrieved = ["duplicate", "duplicate", "relevant-1", "relevant-1", "relevant-2"]
    relevant = {"relevant-1", "relevant-2"}

    assert hit_rate_at_k(retrieved, relevant, 2) == 0.0
    assert precision_at_k(retrieved, relevant, 2) == 0.0
    assert precision_at_k(retrieved, relevant, 3) == pytest.approx(1 / 3)
    assert precision_at_k(retrieved, relevant, 5) == pytest.approx(2 / 5)
    assert recall_at_k(retrieved, relevant, 2) == 0.0
    assert recall_at_k(retrieved, relevant, 3) == pytest.approx(1 / 2)
    assert recall_at_k(retrieved, relevant, 5) == 1.0
    assert reciprocal_rank(retrieved, relevant, k=2) == 0.0
    assert reciprocal_rank(retrieved, relevant) == pytest.approx(1 / 3)

    dcg = 1.0 / math.log2(4) + 1.0 / math.log2(6)
    idcg = 1.0 / math.log2(2) + 1.0 / math.log2(3)
    assert ndcg_at_k(retrieved, relevant, 2) == 0.0
    assert ndcg_at_k(retrieved, relevant, 5) == pytest.approx(dcg / idcg)
    assert average_precision(retrieved, relevant, k=2) == 0.0
    assert average_precision(retrieved, relevant) == pytest.approx(((1 / 3) + (2 / 5)) / 2)

    detail = score_single_query(retrieved, relevant, [2, 5])
    assert detail["retrieved_count"] == 5
    assert detail["unique_retrieved_count"] == 3
    assert detail["by_k"]["2"]["mrr"] == 0.0
    assert detail["by_k"]["5"]["mrr"] == pytest.approx(1 / 3)
    assert detail["by_k"]["5"]["map"] == pytest.approx(((1 / 3) + (2 / 5)) / 2)


def test_mrr_and_map_are_truncated_at_each_k():
    report = evaluate_retrieval(
        [QueryRetrievalJudgement("q", ["miss", "hit"], {"hit"})],
        k_values=[1, 2],
    )

    assert report.metrics_at_k[1].mrr == 0.0
    assert report.metrics_at_k[1].map_score == 0.0
    assert report.metrics_at_k[2].mrr == pytest.approx(0.5)
    assert report.metrics_at_k[2].map_score == pytest.approx(0.5)


@pytest.mark.parametrize("grade", [math.inf, -math.inf, math.nan, -0.1])
def test_invalid_relevance_grades_fail_closed(grade: float):
    with pytest.raises(ValueError, match="finite and non-negative"):
        score_single_query(["a"], {"a": grade}, [1])


@pytest.mark.parametrize("k_values", [[], [0], [-1], [101], [True], [1.5]])
def test_invalid_k_values_fail_closed(k_values):
    with pytest.raises(ValueError, match="k_values"):
        evaluate_retrieval([], k_values=k_values)


def test_duplicate_query_ids_fail_closed():
    with pytest.raises(ValueError, match="duplicate query_id"):
        evaluate_retrieval(
            [
                QueryRetrievalJudgement("same", ["a"], {"a"}),
                QueryRetrievalJudgement("same", ["b"], {"b"}),
            ],
            k_values=[1],
        )
