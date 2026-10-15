"""
Artifact share API — kind-generic public and internal sharing of agent artifacts.

Replaces the quiz-specific share endpoints (product-convergence PC-03).
kind='quiz' freezes a snapshot of the quiz payload + answer keys; the public
routes in src/api/v1/quiz.py read the same artifact_shares rows, so legacy
/quiz/shared/{code} links stay valid.
"""

from __future__ import annotations

import json
import logging
import uuid
from datetime import datetime
from typing import Any, Literal

from ai_gateway_core.quiz.public_projection import safe_quiz_options
from ai_gateway_core.sharing import ArtifactShareManager
from fastapi import APIRouter, Depends, HTTPException, Query, Request
from pydantic import BaseModel, Field

from ...core.auth.user_resolver import UserContext
from ...services.assistant_entry.source_access import quiz_source_scope, source_scope_allowed
from ..deps import get_user_context
from ._internal_share_scope import freeze_source_scope, require_active_internal_user

router = APIRouter(prefix="/artifact-shares", tags=["artifact-shares"])
logger = logging.getLogger(__name__)


class ArtifactShareCreateRequest(BaseModel):
    kind: str = Field("quiz", description="Artifact kind; only 'quiz' is supported today")
    quiz_id: uuid.UUID | None = Field(None, description="Source quiz id (kind='quiz')")
    expires_hours: int | None = Field(None, ge=1, description="Hours until expiry (None = never)")
    max_attempts: int | None = Field(None, ge=1, description="Max attempts (None = unlimited)")
    require_name: bool = Field(True, description="Require name before taking")
    time_limit_minutes: int | None = Field(None, ge=1, description="Time limit per attempt in minutes (None = unlimited)")
    audience: Literal["public", "internal"] = "public"


class ArtifactShareCreateResponse(BaseModel):
    share_id: uuid.UUID
    share_code: str
    kind: str
    title: str
    expires_at: datetime | None
    require_name: bool
    max_attempts: int | None
    time_limit_minutes: int | None
    quiz_id: uuid.UUID
    quiz_title: str
    audience: Literal["public", "internal"]


class ArtifactShareRevokeResponse(BaseModel):
    revoked: bool


class ArtifactShareSummary(BaseModel):
    share_id: uuid.UUID
    share_code: str
    is_active: bool
    expired: bool
    created_at: datetime
    expires_at: datetime | None
    revoked_at: datetime | None
    require_name: bool
    audience: Literal["public", "internal"] = "public"


@router.get("", response_model=list[ArtifactShareSummary])
async def list_artifact_shares(
    request: Request, quiz_id: uuid.UUID, limit: int = Query(50, ge=1, le=200),
    user: UserContext = Depends(get_user_context),
):
    """Owner-only management metadata; never expose grading keys or payload.

    Listing/revoking remains available when source rights prohibit publishing.
    """
    db = getattr(request.app.state, "database", None)
    if db is None:
        raise HTTPException(503, "Database not available")
    rows = await db.fetch(
        """
        SELECT id AS share_id, share_code, is_active, created_at, expires_at, audience,
               revoked_at, require_name,
               (expires_at IS NOT NULL AND expires_at <= NOW()) AS expired
          FROM assistant.artifact_shares
         WHERE tenant_id = $1 AND created_by = $2 AND kind = 'quiz'
           AND payload ->> 'quiz_id' = $3
         ORDER BY created_at DESC, id DESC LIMIT $4
        """,
        user.tenant_id, user.user_id, str(quiz_id), limit,
    )
    return [dict(row) for row in rows]


@router.post("", response_model=ArtifactShareCreateResponse)
async def create_artifact_share(
    body: ArtifactShareCreateRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Create a frozen artifact snapshot under the selected audience policy."""
    db = getattr(request.app.state, "database", None)
    if db is None:
        raise HTTPException(503, "Database not available")
    mgr = ArtifactShareManager(db=db)

    if body.kind != "quiz":
        raise HTTPException(400, f"Unsupported artifact kind: {body.kind}")
    if not body.quiz_id:
        raise HTTPException(400, "quiz_id is required for kind='quiz'")

    # Verify quiz exists and belongs to the caller.
    quiz_row = await db.fetchrow(
        "SELECT id, tenant_id, title, description, question_count, difficulty, dataset_ids "
        "FROM quizzes WHERE id = $1 AND tenant_id = $2 AND created_by = $3",
        body.quiz_id,
        user.tenant_id,
        user.user_id,
    )
    if not quiz_row:
        raise HTTPException(404, "Quiz not found or not authorized")
    datasets, documents, versions = await quiz_source_scope(
        request, body.quiz_id, user.tenant_id, require_origin=True,
    )
    source_scope = None
    if body.audience == "public":
        if datasets or documents or versions:
            raise HTTPException(409, "Private knowledge content cannot be shared anonymously")
    else:
        await require_active_internal_user(request, user, user.tenant_id or "")
        source_scope = freeze_source_scope(datasets, documents, versions)
        if not await source_scope_allowed(request, user, datasets, documents, versioned_refs=versions):
            raise HTTPException(403, "Quiz source access has been revoked")

    # Freeze a snapshot: public questions + grading answer keys.
    q_rows = await db.fetch(
        "SELECT id, question_num, question_type, question_text, options, "
        "correct_answer, explanation FROM quiz_questions "
        "WHERE quiz_id = $1 ORDER BY question_num",
        body.quiz_id,
    )
    questions: list[dict[str, Any]] = []
    answer_keys: list[dict[str, Any]] = []
    for qr in q_rows:
        options = qr["options"]
        if isinstance(options, str):
            options = json.loads(options)
        correct = qr["correct_answer"]
        if isinstance(correct, str):
            correct = json.loads(correct)
        questions.append({
            "id": str(qr["id"]),
            "question_num": qr["question_num"],
            "question_type": qr["question_type"],
            "question_text": qr["question_text"],
            "options": safe_quiz_options(options),
        })
        answer_keys.append({
            "id": str(qr["id"]),
            "question_num": qr["question_num"],
            "question_type": qr["question_type"],
            "correct_answer": correct,
            "explanation": qr["explanation"],
        })

    payload = {
        "quiz_id": str(body.quiz_id),
        "description": quiz_row["description"],
        "question_count": quiz_row["question_count"],
        "difficulty": quiz_row["difficulty"],
        "questions": questions,
    }

    share = await mgr.create_share(
        kind="quiz",
        title=quiz_row["title"],
        payload=payload,
        answer_keys=answer_keys,
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        expires_hours=body.expires_hours,
        max_attempts=body.max_attempts,
        require_name=body.require_name,
        time_limit_minutes=body.time_limit_minutes,
        audience=body.audience,
        source_scope=source_scope,
    )
    return {**share, "quiz_id": body.quiz_id, "quiz_title": quiz_row["title"]}


@router.delete("/{share_id}", response_model=ArtifactShareRevokeResponse)
async def revoke_artifact_share(
    share_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Revoke a share link (creator only)."""
    db = getattr(request.app.state, "database", None)
    if db is None:
        raise HTTPException(503, "Database not available")
    mgr = ArtifactShareManager(db=db)
    revoked = await mgr.revoke_share(
        share_id,
        user_id=user.user_id,
        tenant_id=user.tenant_id,
    )
    if not revoked:
        raise HTTPException(404, "Share link not found or not authorized")
    return {"revoked": True}
