"""
Tests for QuizGrader objective scoring (shared ai-gateway-core package).
"""

from __future__ import annotations

from ai_gateway_core.quiz.quiz_grader import QuizGrader


def _q(qid: str, qtype: str, correct):
    return {
        "id": qid,
        "question_num": 1,
        "question_type": qtype,
        "question_text": "Q?",
        "correct_answer": correct,
        "explanation": "",
    }


def test_mc_single_correct_answer_B_scores_B_correct():
    grader = QuizGrader()
    result = grader.grade([_q("q1", "mc_single", ["B"])], {"q1": "B"})
    assert result["correct_count"] == 1
    assert result["per_question"][0]["correct"] is True


def test_mc_single_multiple_labels_accepts_any():
    grader = QuizGrader()
    q = _q("q1", "mc_single", ["B", "C"])
    assert grader.grade([q], {"q1": "B"})["per_question"][0]["correct"] is True
    assert grader.grade([q], {"q1": "C"})["per_question"][0]["correct"] is True
    assert grader.grade([q], {"q1": "A"})["per_question"][0]["correct"] is False
