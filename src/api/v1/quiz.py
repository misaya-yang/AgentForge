"""
Quiz API — compatibility routes for quizzes created by the Agent Runtime's
``generate_quiz`` capability, documented by the ai-quiz plugin manifest.

Endpoints (load-bearing only):
- GET  /assistant/quiz/{quiz_id}         — Get quiz details (no answers)
- POST /assistant/quiz/{quiz_id}/submit  — Submit answers for grading
- GET  /assistant/quiz/{quiz_id}/attempts — List attempts for a quiz
- DELETE /assistant/quiz/{quiz_id}       — Delete a quiz
- GET  /quiz/shared/{share_code}         — Audience-checked share (alias over artifact_shares)
- POST /quiz/shared/{share_code}/submit  — Audience-checked submit (alias over artifact_shares)

Share creation/revocation moved to POST/DELETE /api/v1/artifact-shares
(src/api/v1/artifact_shares.py); quiz generation moved to the in-chat tool.
"""

from __future__ import annotations

import logging
import random
import uuid
from datetime import datetime, timezone
from typing import Any, Literal

from ai_gateway_core.quiz import QuizAccessService, QuizGrader
from ai_gateway_core.quiz.quiz_access_service import QuizAttemptConflictError
from ai_gateway_core.sharing import ArtifactShareManager
from ai_gateway_core.sharing.artifact_share_manager import (
    ArtifactShareError,
    AttemptConflictError,
    AttemptInputError,
    AttemptLimitReachedError,
    ShareUnavailableError,
)
from fastapi import APIRouter, Depends, HTTPException, Query, Request, Response
from pydantic import BaseModel, Field

from ...core.auth.user_resolver import UserContext
from ...core.client_ip import get_client_ip_from_request
from ...services.assistant_entry.source_access import (
    quiz_source_scope,
    require_public_quiz_source_access,
    source_scope_allowed,
)
from ..deps import enforce_rate_limit, get_user_context
from ._internal_share_scope import require_internal_share_access

router = APIRouter(prefix="/assistant/quiz", tags=["quiz"])
logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Schemas
# ---------------------------------------------------------------------------


class QuizSubmitRequest(BaseModel):
    answers: dict[str, str] = Field(..., description="question_id → selected option label")
    attempt_id: uuid.UUID | None = Field(None, description="Stable id for retrying this answer submission")


class PublicQuizSubmitRequest(BaseModel):
    answers: dict[str, str] = Field(..., description="question_id → selected option label")
    display_name: str | None = Field(None, description="Anonymous user's name")
    attempt_token: str | None = Field(
        None,
        description="Opaque token returned by the public attempt-start endpoint",
    )


class PublicQuizAttemptStartResponse(BaseModel):
    attempt_token: str
    started_at: datetime
    expires_at: datetime


class PublicQuizAttemptResultRequest(BaseModel):
    attempt_token: str = Field(min_length=1, max_length=200)


class QuizAttemptResponse(BaseModel):
    attempt_id: uuid.UUID
    cached: bool = False
    total_score: float
    correct_count: int
    total_count: int
    per_question: list[dict[str, Any]]


class QuizQuestionResponse(BaseModel):
    id: uuid.UUID
    question_num: int
    question_type: str
    question_text: str
    options: list[Any]


class QuizResponse(BaseModel):
    quiz_id: uuid.UUID
    title: str
    description: str | None
    topic: str | None
    difficulty: str
    question_count: int
    status: str
    created_at: datetime | None
    questions: list[QuizQuestionResponse]


class QuizAttemptListItem(BaseModel):
    attempt_id: uuid.UUID
    user_id: str | None
    display_name: str | None
    total_score: float | None
    correct_count: int | None
    total_count: int | None
    started_at: datetime | None
    completed_at: datetime | None
    status: str


class QuizAttemptListResponse(BaseModel):
    attempts: list[QuizAttemptListItem]
    total: int


class QuizDeleteResponse(BaseModel):
    deleted: bool


class PublicQuizResponse(BaseModel):
    share_code: str
    kind: str
    title: str
    require_name: bool
    time_limit_minutes: int | None
    quiz_id: uuid.UUID | None
    description: str | None = None
    question_count: int
    difficulty: str
    questions: list[QuizQuestionResponse]
    audience: Literal["public", "internal"] = "public"


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _get_quiz_service(request: Request) -> QuizAccessService:
    """Build the shared read/grade/delete service from app state."""
    db = getattr(request.app.state, "database", None)
    if db is None:
        raise HTTPException(503, "Database not available")
    return QuizAccessService(db=db, grader=QuizGrader())


async def _require_quiz_source_access(quiz_id: uuid.UUID, request: Request, user: UserContext) -> None:
    """Check both direct bindings and knowledge inherited by the creating run."""
    sources, documents, versions = await quiz_source_scope(request, quiz_id, user.tenant_id)
    if not await source_scope_allowed(
        request, user, sources, documents, versioned_refs=versions,
    ):
        raise HTTPException(403, detail={"code": "ASSISTANT_SOURCE_ACCESS_REVOKED"})


def _get_share_manager(request: Request) -> ArtifactShareManager:
    db = getattr(request.app.state, "database", None)
    if db is None:
        raise HTTPException(503, "Database not available")
    return ArtifactShareManager(db=db)


def _shuffle_options(questions: list[dict]) -> list[dict]:
    """Shuffle option display order per question. Labels stay attached to their text."""
    shuffled = []
    for q in questions:
        opts = list(q.get("options", []))
        random.shuffle(opts)
        shuffled.append({**q, "options": opts})
    return shuffled


def _share_http_error(error: ArtifactShareError) -> HTTPException:
    status_code = 400
    if isinstance(error, ShareUnavailableError):
        status_code = 404
        message = "Quiz not found or expired"
    elif isinstance(error, AttemptLimitReachedError):
        status_code = 429
        message = "Maximum attempts reached"
    elif isinstance(error, AttemptConflictError):
        status_code = 409
        message = str(error)
    elif isinstance(error, AttemptInputError):
        message = str(error)
    else:
        message = "Quiz attempt could not be submitted"
    return HTTPException(
        status_code=status_code,
        detail={"code": error.code, "message": message},
    )


async def _require_shared_quiz_access(
    request: Request, share_code: str,
) -> tuple[str, str | None]:
    """Select the share audience before any Quiz payload or attempt is read."""
    db = getattr(request.app.state, "database", None)
    if db is None:
        raise HTTPException(503, "Database not available")
    row = await db.fetchrow(
        "SELECT audience, source_scope, tenant_id, is_active, expires_at "
        "FROM assistant.artifact_shares WHERE share_code = $1 AND kind = 'quiz'",
        share_code,
    )
    unavailable = HTTPException(
        404, detail={"code": "share_unavailable", "message": "Quiz not found or expired"},
    )
    if not row or not row["is_active"] or (
        row["expires_at"] and row["expires_at"] <= datetime.now(timezone.utc)
    ):
        raise unavailable
    audience = row.get("audience") or "public"
    if audience == "internal":
        user = await require_internal_share_access(request, row)
        return audience, user.user_id
    elif audience == "public":
        await require_public_quiz_source_access(request, share_code)
    else:
        raise unavailable
    return audience, None


# ---------------------------------------------------------------------------
# Authenticated endpoints
# ---------------------------------------------------------------------------


@router.get("/{quiz_id}", response_model=QuizResponse)
async def get_quiz(
    quiz_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Get quiz details (questions without answers)."""
    await _require_quiz_source_access(quiz_id, request, user)
    svc = _get_quiz_service(request)
    quiz = await svc.get_quiz(quiz_id, user.tenant_id, include_answers=False)
    if not quiz:
        raise HTTPException(404, "Quiz not found")
    return quiz


@router.post("/{quiz_id}/submit", response_model=QuizAttemptResponse)
async def submit_quiz(
    quiz_id: uuid.UUID,
    body: QuizSubmitRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Submit quiz answers and receive grading results."""
    await _require_quiz_source_access(quiz_id, request, user)
    svc = _get_quiz_service(request)
    try:
        result = await svc.submit_attempt(
            quiz_id=quiz_id,
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            answers=body.answers,
            attempt_id=body.attempt_id,
        )
    except QuizAttemptConflictError as e:
        raise HTTPException(409, str(e)) from None
    except ValueError as e:
        raise HTTPException(404, str(e))

    return result


@router.get("/{quiz_id}/attempts/{attempt_id}", response_model=QuizAttemptResponse)
async def get_attempt_result(
    quiz_id: uuid.UUID,
    attempt_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    await _require_quiz_source_access(quiz_id, request, user)
    result = await _get_quiz_service(request).get_attempt_result(
        quiz_id, user.tenant_id, user.user_id, attempt_id,
    )
    if result is None:
        raise HTTPException(404, "Attempt not found")
    return result


@router.get("/{quiz_id}/attempts", response_model=QuizAttemptListResponse)
async def list_attempts(
    quiz_id: uuid.UUID,
    request: Request,
    limit: int = Query(100, ge=1, le=1000),
    offset: int = Query(0, ge=0),
    user: UserContext = Depends(get_user_context),
):
    """List all attempts for a quiz (creator sees all, others see own). Paginated."""
    await _require_quiz_source_access(quiz_id, request, user)
    svc = _get_quiz_service(request)
    return await svc.list_attempts(
        quiz_id, user.tenant_id, user.user_id, limit=limit, offset=offset,
    )


@router.delete("/{quiz_id}", response_model=QuizDeleteResponse)
async def delete_quiz(
    quiz_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Delete a quiz (only by creator)."""
    svc = _get_quiz_service(request)
    deleted = await svc.delete_quiz(quiz_id, user.tenant_id, user.user_id)
    if not deleted:
        raise HTTPException(404, "Quiz not found or not authorized to delete")
    return {"deleted": True}


# ---------------------------------------------------------------------------
# Share endpoints — public links stay anonymous; internal links require access
# ---------------------------------------------------------------------------

public_router = APIRouter(prefix="/quiz", tags=["quiz-public"])


@public_router.get("/shared/{share_code}", response_model=PublicQuizResponse)
async def get_shared_quiz(share_code: str, request: Request, response: Response):
    """Get a shared quiz after audience and current source-right checks."""
    audience, _user_id = await _require_shared_quiz_access(request, share_code)
    mgr = _get_share_manager(request)
    artifact = await mgr.get_public_artifact(share_code, audience=audience)
    if not artifact:
        raise HTTPException(404, "Quiz not found, expired, or max attempts reached")
    # Option display order is shuffled per viewer; answer keys stay server-side.
    questions = artifact.get("questions", [])
    artifact["questions"] = _shuffle_options(questions)
    if audience == "internal":
        response.headers["Cache-Control"] = "no-store"
    return artifact


@public_router.post(
    "/public/{share_code}/attempts/start",
    response_model=PublicQuizAttemptStartResponse,
)
async def start_shared_quiz_attempt(share_code: str, request: Request, response: Response):
    """Start the per-attempt clock and return a single-use opaque token."""
    await enforce_rate_limit(request, user=None, operation="quiz_attempt_start_public")
    audience, user_id = await _require_shared_quiz_access(request, share_code)
    if audience == "internal":
        response.headers["Cache-Control"] = "no-store"
    try:
        return await _get_share_manager(request).start_attempt(
            share_code, audience=audience, user_id=user_id,
        )
    except ArtifactShareError as exc:
        raise _share_http_error(exc) from exc


@public_router.post("/public/{share_code}/attempts/result", response_model=QuizAttemptResponse)
async def get_shared_quiz_attempt_result(
    share_code: str,
    body: PublicQuizAttemptResultRequest,
    request: Request,
    response: Response,
):
    await enforce_rate_limit(request, user=None, operation="quiz_submit_public")
    audience, user_id = await _require_shared_quiz_access(request, share_code)
    if audience == "internal":
        response.headers["Cache-Control"] = "no-store"
    try:
        result = await _get_share_manager(request).get_attempt_result(
            share_code, body.attempt_token, audience=audience, user_id=user_id,
        )
    except ArtifactShareError as exc:
        raise _share_http_error(exc) from exc
    if result is None:
        raise HTTPException(404, "Attempt not found")
    return result


@public_router.post("/shared/{share_code}/submit", response_model=QuizAttemptResponse)
async def submit_shared_quiz(
    share_code: str,
    body: PublicQuizSubmitRequest,
    request: Request,
    response: Response,
):
    """Submit answers only while this viewer can read the shared quiz."""
    # Anonymous endpoint: IP-only rate limit to prevent submission spam
    await enforce_rate_limit(request, user=None, operation="quiz_submit_public")
    audience, user_id = await _require_shared_quiz_access(request, share_code)
    if audience == "internal":
        response.headers["Cache-Control"] = "no-store"

    mgr = _get_share_manager(request)
    client_ip = get_client_ip_from_request(request)
    try:
        result = await mgr.submit_attempt(
            share_code=share_code,
            answers=body.answers,
            display_name=body.display_name,
            client_ip=client_ip,
            attempt_token=body.attempt_token,
            audience=audience,
            user_id=user_id,
        )
    except ArtifactShareError as exc:
        raise _share_http_error(exc) from exc
    return result
