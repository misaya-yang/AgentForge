"""
Tests for anonymous quiz submission via conversation-share public endpoint.

Covers the Phase-1 quiz-on-share flow:
- Successful anon submit grades against the frozen answer key embedded at
  share-creation time and persists a row keyed by (share_code, anon_id, quiz_id).
- A second submission from the same anon_id replays the cached first result
  (cached=True) instead of re-grading.
- A share with no matching quiz_id returns 404.
"""

from __future__ import annotations

import asyncio
import json
import uuid
from types import SimpleNamespace

import pytest
from ai_gateway_core.quiz.public_projection import safe_quiz_options

from src.api.v1 import conversation_shares as cs


class FakeDB:
    """Minimal asyncpg-ish facade that records every INSERT into a dict."""

    def __init__(self, share_row: dict | None):
        self._share_row = share_row
        self._attempts: dict[tuple[str, str, str], dict] = {}

    async def fetchrow(self, sql: str, *args):
        sql_norm = " ".join(sql.split())
        # Share lookup by share_code (the only fetchrow path the endpoint uses
        # besides the attempt-dedup query).
        if "FROM conversation_shares WHERE share_code" in sql_norm:
            return self._share_row
        if "FROM conversation_share_quiz_attempts" in sql_norm:
            share_code, anon_id, quiz_id = args
            return self._attempts.get((share_code, anon_id, str(quiz_id)))
        return None

    async def execute(self, sql: str, *args):
        sql_norm = " ".join(sql.split())
        if "INSERT INTO conversation_share_quiz_attempts" in sql_norm:
            _id, share_code, anon_id, quiz_id, result_json = args
            key = (share_code, anon_id, str(quiz_id))
            if key in self._attempts:
                raise RuntimeError("unique violation (simulated)")
            self._attempts[key] = {"result": json.loads(result_json)}
            return "INSERT 1"
        return "OK"


def _make_request(
    db,
    *,
    anonymous_id: str | None = None,
    cookies: dict[str, str] | None = None,
    headers: dict[str, str] | None = None,
    client_host: str = "127.0.0.1",
):
    """Fake FastAPI Request exposing only what the endpoint reads."""
    app = SimpleNamespace(state=SimpleNamespace(database=db, multi_rate_limiter=None))
    return SimpleNamespace(
        app=app,
        cookies=cookies or {},
        headers=headers or {},
        state=SimpleNamespace(anonymous_id=anonymous_id) if anonymous_id else SimpleNamespace(),
        client=SimpleNamespace(host=client_host),
    )


def _build_snapshot(quiz_id: str, question_id: str) -> dict:
    """Snapshot containing one quiz with a single mc_single question (answer A)."""
    return {
        "share_snapshot_version": 2,
        "source_policy": "verified_no_private_knowledge",
        "messages": [
            {
                "role": "assistant",
                "content": "Quiz ready",
                "metadata": {"quiz_id": quiz_id},
                "quiz_data": {
                    "quiz_id": quiz_id,
                    "title": "T",
                    "question_count": 1,
                    "questions": [
                        {
                            "id": question_id,
                            "question_num": 1,
                            "question_type": "mc_single",
                            "question_text": "Q1?",
                            "options": [{"label": "A", "text": "Yes"}, {"label": "B", "text": "No"}],
                        }
                    ],
                },
            }
        ],
        "artifacts": [],
        "quiz_answer_keys": {
            quiz_id: {
                "questions": [
                    {
                        "id": question_id,
                        "question_num": 1,
                        "question_type": "mc_single",
                        "correct_answer": ["A"],
                        "explanation": "A is correct because.",
                    }
                ]
            }
        },
    }


def _share_row(snapshot: dict, *, is_active: bool = True, expires_at=None) -> dict:
    return {
        "snapshot": snapshot,
        "is_active": is_active,
        "expires_at": expires_at,
    }


def test_anon_submit_grades_and_persists():
    quiz_id = str(uuid.uuid4())
    question_id = str(uuid.uuid4())
    snapshot = _build_snapshot(quiz_id, question_id)
    db = FakeDB(_share_row(snapshot))
    anon_id = str(uuid.uuid4())
    request = _make_request(db, anonymous_id=anon_id)
    body = cs.SharedQuizSubmitRequest(answers={question_id: "A"})

    result = asyncio.run(
        cs.submit_shared_quiz(
            share_code="abc12345",
            quiz_id=quiz_id,
            body=body,
            request=request,
        )
    )

    assert result["correct_count"] == 1
    assert result["total_count"] == 1
    assert result["total_score"] == 1.0
    # Should have been persisted
    assert ("abc12345", anon_id, quiz_id) in db._attempts


def test_resubmit_returns_cached_result_without_regrading():
    quiz_id = str(uuid.uuid4())
    question_id = str(uuid.uuid4())
    snapshot = _build_snapshot(quiz_id, question_id)
    db = FakeDB(_share_row(snapshot))
    request = _make_request(db, anonymous_id=str(uuid.uuid4()))

    first = asyncio.run(
        cs.submit_shared_quiz(
            share_code="abc12345",
            quiz_id=quiz_id,
            body=cs.SharedQuizSubmitRequest(answers={question_id: "A"}),
            request=request,
        )
    )
    assert first["correct_count"] == 1

    # Second attempt with WRONG answer — must return the cached (correct)
    # result, not re-grade.
    second = asyncio.run(
        cs.submit_shared_quiz(
            share_code="abc12345",
            quiz_id=quiz_id,
            body=cs.SharedQuizSubmitRequest(answers={question_id: "B"}),
            request=request,
        )
    )
    assert second["cached"] is True
    assert second["correct_count"] == 1
    assert second["attempt_id"] == first["attempt_id"]


def test_unknown_share_returns_404():
    from fastapi import HTTPException

    db = FakeDB(share_row=None)
    request = _make_request(db)
    body = cs.SharedQuizSubmitRequest(answers={})

    with pytest.raises(HTTPException) as exc_info:
        asyncio.run(
            cs.submit_shared_quiz(
                share_code="does-not-exist",
                quiz_id=str(uuid.uuid4()),
                body=body,
                request=request,
            )
        )
    assert exc_info.value.status_code == 404


def test_share_without_matching_quiz_returns_404():
    """A valid share that does not contain the requested quiz_id — 404, not 500."""
    from fastapi import HTTPException

    quiz_id = str(uuid.uuid4())
    other_quiz_id = str(uuid.uuid4())
    question_id = str(uuid.uuid4())
    snapshot = _build_snapshot(quiz_id, question_id)
    db = FakeDB(_share_row(snapshot))
    request = _make_request(db, anonymous_id=str(uuid.uuid4()))
    body = cs.SharedQuizSubmitRequest(answers={})

    with pytest.raises(HTTPException) as exc_info:
        asyncio.run(
            cs.submit_shared_quiz(
                share_code="abc12345",
                quiz_id=other_quiz_id,
                body=body,
                request=request,
            )
        )
    assert exc_info.value.status_code == 404


def test_shared_quiz_attempt_key_uses_trusted_state_and_isolates_viewers():
    """Raw cookie/header values cannot select another viewer's attempt key."""
    quiz_id = str(uuid.uuid4())
    question_id = str(uuid.uuid4())
    snapshot = _build_snapshot(quiz_id, question_id)
    db = FakeDB(_share_row(snapshot))
    viewer_a = str(uuid.uuid4())
    viewer_b = str(uuid.uuid4())
    forged_cookies = {"ag_anon_id": "attacker-controlled-cookie"}
    forged_headers = {"x-anon-id": "attacker-controlled-header"}

    first = asyncio.run(
        cs.submit_shared_quiz(
            share_code="abc12345",
            quiz_id=quiz_id,
            body=cs.SharedQuizSubmitRequest(answers={question_id: "A"}),
            request=_make_request(
                db,
                anonymous_id=viewer_a,
                cookies=forged_cookies,
                headers=forged_headers,
            ),
        )
    )
    second = asyncio.run(
        cs.submit_shared_quiz(
            share_code="abc12345",
            quiz_id=quiz_id,
            body=cs.SharedQuizSubmitRequest(answers={question_id: "A"}),
            request=_make_request(
                db,
                anonymous_id=viewer_b,
                cookies=forged_cookies,
                headers=forged_headers,
            ),
        )
    )

    assert "cached" not in first
    assert "cached" not in second
    assert set(db._attempts) == {
        ("abc12345", viewer_a, quiz_id),
        ("abc12345", viewer_b, quiz_id),
    }


def test_anon_id_without_middleware_state_uses_client_ip_not_raw_client_ids():
    request = _make_request(
        None,
        cookies={"ag_anon_id": "attacker-controlled-cookie"},
        headers={"x-anon-id": "attacker-controlled-header"},
        client_host="198.51.100.10",
    )

    assert cs._resolve_anon_id(request) == "198.51.100.10"


def test_public_get_strips_answer_keys():
    """_strip_snapshot_for_public must remove grading secrets."""
    quiz_id = str(uuid.uuid4())
    question_id = str(uuid.uuid4())
    snapshot = _build_snapshot(quiz_id, question_id)

    public = cs._strip_snapshot_for_public(snapshot)

    assert "quiz_answer_keys" not in public
    # Public quiz_data on messages is intact and contains no correct_answer.
    assistant_msg = public["messages"][0]
    assert "quiz_data" in assistant_msg
    q = assistant_msg["quiz_data"]["questions"][0]
    assert "correct_answer" not in q
    assert "explanation" not in q


def test_pre_submission_options_drop_legacy_answer_fields() -> None:
    raw = [{
        "label": "A", "text": "Yes", "correct_answer": True,
        "explanation": "secret", "source_chunks": ["private"],
    }]
    assert safe_quiz_options(raw) == [{"label": "A", "text": "Yes"}]


def test_old_share_requires_new_source_review() -> None:
    from fastapi import HTTPException

    with pytest.raises(HTTPException) as exc:
        cs._require_safe_share_snapshot({"messages": [{"role": "assistant", "content": "legacy"}]})
    assert exc.value.status_code == 410
