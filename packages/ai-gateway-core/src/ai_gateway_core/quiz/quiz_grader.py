"""
Quiz Grader — Grade quiz submissions.

Supports:
- mc_single: single correct answer (deterministic)
- mc_multi: multiple correct answers (deterministic)
- true_false: true/false (deterministic)
- short_answer: AI-graded free text via LLM
"""

from __future__ import annotations

import json
import logging
from typing import Any

logger = logging.getLogger(__name__)

SHORT_ANSWER_GRADING_PROMPT = """\
You are grading a short-answer quiz question. Compare the student's answer \
against the correct answer.

Question: {question_text}
Correct answer: {correct_answer}
Student's answer: {user_answer}

Evaluate whether the student's answer is essentially correct. Be lenient with:
- Minor spelling/grammar differences
- Different wording that conveys the same meaning
- Partial answers that capture the key point

Respond with ONLY a JSON object (no markdown fences):
{{"correct": true/false, "feedback": "Brief explanation of why correct/incorrect"}}
"""


class QuizGrader:
    """Grade quiz submissions. Uses LLM for short_answer, deterministic for others."""

    def __init__(self, model_registry: Any | None = None) -> None:
        self.model_registry = model_registry

    def grade(
        self,
        questions: list[dict],
        answers: dict[str, str],
    ) -> dict:
        """Synchronous grading for objective types only."""
        per_question: list[dict] = []
        correct_count = 0
        total_count = len(questions)

        for q in questions:
            q_id = str(q["id"])
            q_type = q.get("question_type", "mc_single")
            user_answer = answers.get(q_id, "")
            correct_raw = q.get("correct_answer", [])

            if q_type == "short_answer":
                # Short answer needs async grading — mark as pending
                per_question.append({
                    "question_num": q["question_num"],
                    "question_id": q_id,
                    "correct": False,
                    "user_answer": user_answer,
                    "correct_answer": (
                        correct_raw[0]
                        if isinstance(correct_raw, list) and correct_raw
                        else str(correct_raw)
                    ),
                    "explanation": q.get("explanation", ""),
                    "needs_ai_grading": True,
                })
                continue

            is_correct = self._grade_objective(q_type, correct_raw, user_answer)

            if is_correct:
                correct_count += 1

            # Format correct_answer for display
            if isinstance(correct_raw, list):
                display_answer = ", ".join(str(a) for a in correct_raw)
            else:
                display_answer = str(correct_raw)

            per_question.append({
                "question_num": q["question_num"],
                "question_id": q_id,
                "correct": is_correct,
                "user_answer": user_answer,
                "correct_answer": display_answer,
                "explanation": q.get("explanation", ""),
            })

        total_score = correct_count / total_count if total_count > 0 else 0.0

        return {
            "total_score": round(total_score, 4),
            "correct_count": correct_count,
            "total_count": total_count,
            "per_question": per_question,
        }

    async def grade_async(
        self,
        questions: list[dict],
        answers: dict[str, str],
    ) -> dict:
        """Full grading including AI-graded short_answer questions."""
        result = self.grade(questions, answers)

        # Process short_answer questions with AI grading
        if self.model_registry:
            for pq in result["per_question"]:
                if pq.pop("needs_ai_grading", False):
                    q = next((q for q in questions if str(q["id"]) == pq["question_id"]), None)
                    if q and pq["user_answer"].strip():
                        ai_result = await self._grade_short_answer(
                            question_text=q["question_text"],
                            correct_answer=pq["correct_answer"],
                            user_answer=pq["user_answer"],
                        )
                        pq["correct"] = ai_result["correct"]
                        if ai_result.get("feedback"):
                            pq["explanation"] = ai_result["feedback"]
                        if ai_result["correct"]:
                            result["correct_count"] += 1

            # Recalculate score
            result["total_score"] = round(
                result["correct_count"] / result["total_count"], 4
            ) if result["total_count"] > 0 else 0.0

        return result

    def _grade_objective(
        self,
        question_type: str,
        correct_raw: Any,
        user_answer: str,
    ) -> bool:
        """Deterministic grading for mc_single, mc_multi, true_false."""
        if not isinstance(correct_raw, list):
            correct_raw = [correct_raw]

        if question_type == "true_false":
            correct_val = str(correct_raw[0]).lower().strip() if correct_raw else ""
            user_val = user_answer.lower().strip()
            return user_val == correct_val

        if question_type == "mc_multi":
            user_labels = sorted(
                label.strip().upper()
                for label in user_answer.split(",")
                if label.strip()
            )
            correct_labels = sorted(str(a).upper().strip() for a in correct_raw)
            return user_labels == correct_labels

        # mc_single: current quiz_tool emits one label, but legacy rows may
        # contain multiple labels that the UI highlights as correct.
        if not correct_raw:
            return False
        user_label = user_answer.upper().strip()
        return any(user_label == str(answer).upper().strip() for answer in correct_raw)

    async def _grade_short_answer(
        self,
        question_text: str,
        correct_answer: str,
        user_answer: str,
    ) -> dict:
        """Use LLM to grade a short answer question."""
        from ai_gateway_core.models import ChatMessage

        prompt = SHORT_ANSWER_GRADING_PROMPT.format(
            question_text=question_text,
            correct_answer=correct_answer,
            user_answer=user_answer,
        )

        try:
            content, _usage = await self.model_registry.chat(
                model_id="qwen3.7-plus",
                messages=[ChatMessage(role="user", content=prompt)],
                temperature=0.1,
                max_tokens=256,
            )

            # Parse JSON response
            text = content.strip()
            if text.startswith("```"):
                import re
                match = re.search(r"```(?:json)?\s*\n?(.*?)\n?```", text, re.DOTALL)
                if match:
                    text = match.group(1).strip()

            data = json.loads(text)
            return {
                "correct": bool(data.get("correct", False)),
                "feedback": data.get("feedback", ""),
            }
        except Exception as e:
            logger.warning(f"AI grading failed, defaulting to incorrect: {e}")
            return {"correct": False, "feedback": f"AI grading error: {e}"}
