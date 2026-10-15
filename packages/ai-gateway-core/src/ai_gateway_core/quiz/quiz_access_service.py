"""Shared quiz persistence boundary for Gateway and capability execution.

Quiz generation is worker-owned. Reading, grading submissions,
listing attempts, and deletion are also used by the gateway's compatibility
API, so those operations must live in the shared package rather than making
the gateway import an application service.
"""

from __future__ import annotations

import json
import logging
import uuid
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from ai_gateway_core.persistence import DatabaseStorageLike

from .public_projection import safe_quiz_options
from .quiz_grader import QuizGrader

logger = logging.getLogger(__name__)


class QuizAttemptConflictError(ValueError):
    """The stable attempt id was already used or cannot be safely replayed."""


def _quiz_uuid(quiz_id: str | uuid.UUID) -> uuid.UUID:
    return quiz_id if isinstance(quiz_id, uuid.UUID) else uuid.UUID(quiz_id)


class QuizAccessService:
    """Quiz operations shared by the gateway API and assistant runtime."""

    def __init__(
        self,
        db: DatabaseStorageLike,
        grader: QuizGrader | None = None,
    ) -> None:
        self.db = db
        self.grader = grader or QuizGrader()

    async def get_quiz(
        self,
        quiz_id: str | uuid.UUID,
        tenant_id: str,
        include_answers: bool = False,
    ) -> dict | None:
        """Fetch a tenant-scoped quiz with its questions."""
        qid = _quiz_uuid(quiz_id)
        row = await self.db.fetchrow(
            "SELECT * FROM assistant.quizzes WHERE id = $1 AND tenant_id = $2",
            qid,
            tenant_id,
        )
        if not row:
            return None

        q_rows = await self.db.fetch(
            "SELECT * FROM assistant.quiz_questions WHERE quiz_id = $1 ORDER BY question_num",
            qid,
        )

        questions = []
        for qr in q_rows:
            raw_options = qr["options"]
            question: dict[str, Any] = {
                "id": str(qr["id"]),
                "question_num": qr["question_num"],
                "question_type": qr["question_type"],
                "question_text": qr["question_text"],
                "options": safe_quiz_options(raw_options),
            }
            if include_answers:
                raw_answer = qr["correct_answer"]
                question["correct_answer"] = (
                    raw_answer
                    if isinstance(raw_answer, list)
                    else json.loads(raw_answer or "[]")
                )
                question["explanation"] = qr["explanation"]
            questions.append(question)

        return {
            "quiz_id": str(row["id"]),
            "title": row["title"],
            "description": row["description"],
            "topic": row["topic"],
            "difficulty": row["difficulty"],
            "question_count": row["question_count"],
            "status": row["status"],
            "created_at": row["created_at"].isoformat() if row["created_at"] else None,
            "questions": questions,
        }

    async def list_quizzes(
        self,
        tenant_id: str,
        user_id: str,
        limit: int = 20,
        offset: int = 0,
    ) -> tuple[list[dict], int]:
        """List quizzes created by one user in one tenant."""
        total_row = await self.db.fetchrow(
            "SELECT count(*) AS cnt FROM assistant.quizzes WHERE tenant_id = $1 AND created_by = $2",
            tenant_id,
            user_id,
        )
        total = total_row["cnt"] if total_row else 0

        rows = await self.db.fetch(
            """
            SELECT id, title, description, topic, difficulty, question_count,
                   status, created_at
            FROM assistant.quizzes
            WHERE tenant_id = $1 AND created_by = $2
            ORDER BY created_at DESC
            LIMIT $3 OFFSET $4
            """,
            tenant_id,
            user_id,
            limit,
            offset,
        )
        quizzes = [
            {
                "quiz_id": str(row["id"]),
                "title": row["title"],
                "description": row["description"],
                "topic": row["topic"],
                "difficulty": row["difficulty"],
                "question_count": row["question_count"],
                "status": row["status"],
                "created_at": (
                    row["created_at"].isoformat() if row["created_at"] else None
                ),
            }
            for row in rows
        ]
        return quizzes, total

    async def list_attempts(
        self,
        quiz_id: str | uuid.UUID,
        tenant_id: str,
        user_id: str,
        limit: int = 100,
        offset: int = 0,
    ) -> dict:
        """List attempts; creators see all, other users only see their own."""
        qid = _quiz_uuid(quiz_id)
        quiz_row = await self.db.fetchrow(
            "SELECT created_by FROM assistant.quizzes WHERE id = $1 AND tenant_id = $2",
            qid,
            tenant_id,
        )
        if not quiz_row:
            return {"attempts": [], "total": 0}

        if quiz_row["created_by"] == user_id:
            count_row = await self.db.fetchrow(
                "SELECT count(*) AS cnt FROM assistant.quiz_attempts WHERE quiz_id = $1",
                qid,
            )
            rows = await self.db.fetch(
                """
                SELECT id, user_id, display_name, total_score, correct_count,
                       total_count, started_at, completed_at, status
                FROM assistant.quiz_attempts WHERE quiz_id = $1
                ORDER BY started_at DESC
                LIMIT $2 OFFSET $3
                """,
                qid,
                limit,
                offset,
            )
        else:
            count_row = await self.db.fetchrow(
                "SELECT count(*) AS cnt FROM assistant.quiz_attempts "
                "WHERE quiz_id = $1 AND user_id = $2",
                qid,
                user_id,
            )
            rows = await self.db.fetch(
                """
                SELECT id, user_id, display_name, total_score, correct_count,
                       total_count, started_at, completed_at, status
                FROM assistant.quiz_attempts WHERE quiz_id = $1 AND user_id = $2
                ORDER BY started_at DESC
                LIMIT $3 OFFSET $4
                """,
                qid,
                user_id,
                limit,
                offset,
            )

        total = count_row["cnt"] if count_row else 0
        attempts = [
            {
                "attempt_id": str(row["id"]),
                "user_id": row["user_id"],
                "display_name": row["display_name"],
                "total_score": (
                    float(row["total_score"])
                    if row["total_score"] is not None
                    else None
                ),
                "correct_count": row["correct_count"],
                "total_count": row["total_count"],
                "started_at": (
                    row["started_at"].isoformat() if row["started_at"] else None
                ),
                "completed_at": (
                    row["completed_at"].isoformat()
                    if row["completed_at"]
                    else None
                ),
                "status": row["status"],
            }
            for row in rows
        ]
        return {"attempts": attempts, "total": total}

    async def submit_attempt(
        self,
        quiz_id: str | uuid.UUID,
        tenant_id: str,
        user_id: str,
        answers: dict[str, str],
        attempt_id: str | uuid.UUID | None = None,
    ) -> dict:
        """Grade a submission and persist the attempt."""
        qid = _quiz_uuid(quiz_id)
        aid = _quiz_uuid(attempt_id) if attempt_id is not None else uuid.uuid4()

        async def replay() -> dict | None:
            prior = await self.db.fetchrow(
                "SELECT a.quiz_id, a.user_id, a.answers, a.result_payload "
                "FROM assistant.quiz_attempts AS a "
                "JOIN assistant.quizzes AS q ON q.id = a.quiz_id "
                "WHERE a.id = $1 AND q.tenant_id = $2",
                aid,
                tenant_id,
            )
            if not prior:
                return None
            old_answers = prior["answers"]
            if isinstance(old_answers, str):
                old_answers = json.loads(old_answers)
            if (
                str(prior["quiz_id"]) != str(qid)
                or prior["user_id"] != user_id
                or old_answers != answers
            ):
                raise QuizAttemptConflictError("Attempt id belongs to a different submission")
            saved = prior["result_payload"]
            if isinstance(saved, str):
                saved = json.loads(saved)
            if not isinstance(saved, dict):
                raise QuizAttemptConflictError("Attempt result is unavailable")
            return {**saved, "attempt_id": str(aid), "cached": True}

        existing = await replay()
        if existing is not None:
            return existing
        quiz = await self.get_quiz(qid, tenant_id, include_answers=True)
        if not quiz:
            raise ValueError(f"Quiz {qid} not found")

        has_short_answer = any(
            question.get("question_type") == "short_answer"
            for question in quiz["questions"]
        )
        if has_short_answer and hasattr(self.grader, "grade_async"):
            result = await self.grader.grade_async(quiz["questions"], answers)
        else:
            result = self.grader.grade(quiz["questions"], answers)

        now = datetime.now(timezone.utc)
        payload = {"attempt_id": str(aid), **result}
        inserted = await self.db.fetchrow(
            """
            INSERT INTO assistant.quiz_attempts (id, quiz_id, user_id, answers,
                                       total_score, correct_count, total_count,
                                       started_at, completed_at, status, result_payload)
            VALUES ($1, $2, $3, $4::jsonb, $5, $6, $7, $8, $9, $10, $11::jsonb)
            ON CONFLICT (id) DO NOTHING
            RETURNING id
            """,
            aid,
            qid,
            user_id,
            json.dumps(answers),
            result["total_score"],
            result["correct_count"],
            result["total_count"],
            now,
            now,
            "completed",
            json.dumps(payload, default=str),
        )
        if not inserted:
            raced = await replay()
            if raced is not None:
                return raced
            raise QuizAttemptConflictError("Attempt result is unavailable")
        logger.info(
            "Quiz attempt %s: %s/%s (%.0f%%)",
            aid,
            result["correct_count"],
            result["total_count"],
            result["total_score"] * 100,
        )
        return payload

    async def get_attempt_result(
        self,
        quiz_id: str | uuid.UUID,
        tenant_id: str,
        user_id: str,
        attempt_id: str | uuid.UUID,
    ) -> dict | None:
        row = await self.db.fetchrow(
            "SELECT a.result_payload FROM assistant.quiz_attempts AS a "
            "JOIN assistant.quizzes AS q ON q.id = a.quiz_id "
            "WHERE a.id = $1 AND a.quiz_id = $2 AND a.user_id = $3 AND q.tenant_id = $4",
            _quiz_uuid(attempt_id), _quiz_uuid(quiz_id), user_id, tenant_id,
        )
        if not row or row["result_payload"] is None:
            return None
        payload = row["result_payload"]
        return json.loads(payload) if isinstance(payload, str) else payload

    async def delete_quiz(
        self,
        quiz_id: str | uuid.UUID,
        tenant_id: str,
        user_id: str,
    ) -> bool:
        """Delete a tenant-scoped quiz, restricted to its creator."""
        qid = _quiz_uuid(quiz_id)
        result = await self.db.execute(
            "DELETE FROM assistant.quizzes WHERE id = $1 AND tenant_id = $2 AND created_by = $3",
            qid,
            tenant_id,
            user_id,
        )
        deleted = "DELETE 1" in result
        if deleted:
            logger.info("Deleted quiz %s", qid)
        return deleted
