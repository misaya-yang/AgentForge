"""A retried logged-in quiz submission keeps one graded attempt."""

from __future__ import annotations

import json
import uuid
from unittest.mock import AsyncMock

import pytest
from ai_gateway_core.quiz import QuizGrader
from ai_gateway_core.quiz.quiz_access_service import QuizAccessService, QuizAttemptConflictError


class _QuizDB:
    def __init__(self):
        self.rows: dict[uuid.UUID, dict] = {}
        self.insert_count = 0

    async def fetchrow(self, sql, *args):
        if "INSERT INTO assistant.quiz_attempts" in sql:
            attempt_id, quiz_id, user_id, answers, *_middle, payload = args
            if attempt_id in self.rows:
                return None
            self.rows[attempt_id] = {
                "quiz_id": quiz_id,
                "user_id": user_id,
                "answers": json.loads(answers),
                "result_payload": json.loads(payload),
            }
            self.insert_count += 1
            return {"id": attempt_id}
        if "JOIN assistant.quizzes" in sql:
            if "SELECT a.quiz_id" in sql:
                attempt_id, tenant_id = args
                return self.rows.get(attempt_id) if tenant_id == "tenant-a" else None
            attempt_id, quiz_id, user_id, tenant_id = args
            assert tenant_id == "tenant-a"
            row = self.rows.get(attempt_id)
            if row and row["quiz_id"] == quiz_id and row["user_id"] == user_id:
                return {"result_payload": row["result_payload"]}
            return None
        raise AssertionError(sql)


@pytest.mark.asyncio
async def test_quiz_retry_replays_one_result_and_rejects_changed_answers() -> None:
    db = _QuizDB()
    service = QuizAccessService(db=db, grader=QuizGrader())
    quiz_id = uuid.uuid4()
    attempt_id = uuid.uuid4()
    question_id = str(uuid.uuid4())
    service.get_quiz = AsyncMock(return_value={"questions": [{
        "id": question_id, "question_num": 1, "question_type": "mc_single",
        "question_text": "Q?", "correct_answer": ["A"], "explanation": "A",
    }]})

    first = await service.submit_attempt(quiz_id, "tenant-a", "user-a", {question_id: "A"}, attempt_id)
    retry = await service.submit_attempt(quiz_id, "tenant-a", "user-a", {question_id: "A"}, attempt_id)
    assert first["attempt_id"] == retry["attempt_id"]
    assert retry["cached"] is True
    assert db.insert_count == 1
    assert await service.get_attempt_result(quiz_id, "tenant-a", "user-a", attempt_id) == first
    assert await service.get_attempt_result(quiz_id, "tenant-a", "other-user", attempt_id) is None
    with pytest.raises(QuizAttemptConflictError):
        await service.submit_attempt(quiz_id, "tenant-a", "user-a", {question_id: "B"}, attempt_id)
    service.get_quiz = AsyncMock(return_value=None)
    with pytest.raises(ValueError, match="not found"):
        await service.submit_attempt(quiz_id, "tenant-b", "user-a", {question_id: "A"}, attempt_id)
