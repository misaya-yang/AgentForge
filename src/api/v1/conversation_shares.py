"""Conversation Share API — frozen public or access-controlled internal snapshots."""

from __future__ import annotations

import hashlib
import json
import secrets
import string
import uuid
from contextlib import suppress
from datetime import datetime, timedelta, timezone
from typing import Any, Literal

from ai_gateway_core.logging import get_logger
from ai_gateway_core.quiz import QuizGrader
from ai_gateway_core.quiz.public_projection import safe_quiz_options
from fastapi import APIRouter, Depends, HTTPException, Query, Request
from fastapi.responses import JSONResponse, StreamingResponse
from pydantic import BaseModel, Field

from ...core.auth.user_resolver import UserContext
from ...core.client_ip import get_client_ip_from_request
from ...services.agent_runtime.thread_store import AgentThreadStore
from ...services.assistant_entry.source_access import (
    conversation_sources,
    quiz_source_scope,
    source_scope_allowed,
)
from ..deps import enforce_rate_limit, get_user_context
from ._artifact_headers import attachment_content_disposition
from ._internal_share_scope import (
    freeze_source_scope,
    require_active_internal_user,
    require_internal_share_access,
)

logger = get_logger(__name__)
router = APIRouter(prefix="/assistant", tags=["conversation-shares"])

# ── Models ───────────────────────────────────────────────────────────


class CreateShareRequest(BaseModel):
    expires_days: int | None = Field(None, ge=1, le=365)
    include_artifacts: bool = True
    preview_hash: str | None = Field(None, pattern=r"^[0-9a-f]{64}$")
    audience: Literal["public", "internal"] = "public"


class ShareResponse(BaseModel):
    share_code: str
    share_url: str
    title: str | None
    message_count: int
    artifact_count: int
    created_at: str
    expires_at: str | None
    audience: Literal["public", "internal"]


# ── Helpers ──────────────────────────────────────────────────────────


def _generate_share_code(length: int = 8) -> str:
    alphabet = string.ascii_lowercase + string.digits
    return "".join(secrets.choice(alphabet) for _ in range(length))


def _get_db(request: Request):
    db = getattr(request.app.state, "database", None)
    if db is None:
        raise HTTPException(503, "Database not available")
    return db


def _get_artifact_storage(request: Request):
    return getattr(request.app.state, "artifact_storage", None)


async def _collect_quiz_payloads(
    request: Request,
    db,
    messages: list[dict[str, Any]],
    *,
    tenant_id: str,
    user_id: str,
    audience: Literal["public", "internal"],
) -> tuple[dict[str, dict[str, Any]], dict[str, dict[str, Any]], tuple[set[str], set[tuple[str, str]], set[tuple[str, str, int, str]]]]:
    """Freeze quiz content referenced by ``metadata.quiz_id`` on assistant messages.

    Returns public quiz data, private grading keys, and source references:

    * ``public_quizzes`` — ``quiz_id → quiz payload`` safe to expose to anonymous
      viewers (questions + options, no answers). Embedded onto the snapshot
      messages so the share page can render the quiz even if the original quiz
      row is later deleted.
    * ``answer_keys`` — ``quiz_id → {questions: [...]}`` containing
      ``correct_answer`` + ``explanation``. Stored separately in the snapshot;
      **never returned by the public GET**. Used purely for grading anon
      submissions server-side.
    * source references — kept outside the snapshot for internal ACL checks.
    """
    quiz_ids: list[str] = []
    seen: set[str] = set()
    for msg in messages:
        if not isinstance(msg, dict):
            continue
        meta = msg.get("metadata")
        if not isinstance(meta, dict):
            continue
        qid = meta.get("quiz_id")
        if qid and isinstance(qid, str) and qid not in seen:
            seen.add(qid)
            quiz_ids.append(qid)

    public_quizzes: dict[str, dict[str, Any]] = {}
    answer_keys: dict[str, dict[str, Any]] = {}
    quiz_datasets: set[str] = set()
    quiz_documents: set[tuple[str, str]] = set()
    quiz_versions: set[tuple[str, str, int, str]] = set()

    for quiz_id in quiz_ids:
        try:
            quiz_uuid = uuid.UUID(quiz_id)
        except (ValueError, TypeError):
            raise HTTPException(409, "A referenced quiz cannot be verified for sharing") from None
        quiz_row = await db.fetchrow(
            "SELECT id, title, description, topic, difficulty, question_count, dataset_ids "
            "FROM quizzes WHERE id = $1 AND tenant_id = $2 AND created_by = $3",
            quiz_uuid,
            tenant_id,
            user_id,
        )
        if not quiz_row:
            raise HTTPException(409, "A referenced quiz cannot be verified for sharing")
        dataset_ids = quiz_row["dataset_ids"] or []
        if isinstance(dataset_ids, str):
            dataset_ids = json.loads(dataset_ids)
        if not isinstance(dataset_ids, list) or any(
            not isinstance(item, str) or not item for item in dataset_ids
        ):
            raise HTTPException(409, "Quiz source rights cannot be verified for sharing")
        if dataset_ids and audience == "public":
            raise HTTPException(409, "Quiz content derived from private knowledge cannot be shared anonymously")
        try:
            inherited_datasets, inherited_documents, inherited_versions = await quiz_source_scope(
                request, quiz_uuid, tenant_id, require_origin=True,
            )
        except HTTPException as exc:
            raise HTTPException(409, "Quiz source rights cannot be verified for sharing") from exc
        if audience == "public" and (inherited_datasets or inherited_documents or inherited_versions):
            raise HTTPException(409, "Quiz content derived from knowledge cannot be shared anonymously")
        quiz_datasets.update(str(item) for item in dataset_ids or [])
        quiz_datasets.update(inherited_datasets)
        quiz_documents.update(inherited_documents)
        quiz_versions.update(inherited_versions)
        q_rows = await db.fetch(
            "SELECT id, question_num, question_type, question_text, options, correct_answer, explanation "
            "FROM quiz_questions WHERE quiz_id = $1 ORDER BY question_num",
            quiz_uuid,
        )
        if not q_rows:
            raise HTTPException(409, "A referenced quiz has no verifiable questions")

        public_questions: list[dict[str, Any]] = []
        grading_questions: list[dict[str, Any]] = []
        for qr in q_rows:
            options = qr["options"]
            if isinstance(options, str):
                try:
                    options = json.loads(options)
                except (ValueError, TypeError):
                    options = []
            correct = qr["correct_answer"]
            if isinstance(correct, str):
                try:
                    correct = json.loads(correct)
                except (ValueError, TypeError):
                    correct = []
            qid_str = str(qr["id"])
            public_questions.append(
                {
                    "id": qid_str,
                    "question_num": qr["question_num"],
                    "question_type": qr["question_type"],
                    "question_text": qr["question_text"],
                    "options": safe_quiz_options(options),
                }
            )
            grading_questions.append(
                {
                    "id": qid_str,
                    "question_num": qr["question_num"],
                    "question_type": qr["question_type"],
                    "correct_answer": correct,
                    "explanation": qr["explanation"] or "",
                }
            )

        public_quizzes[quiz_id] = {
            "quiz_id": quiz_id,
            "title": quiz_row["title"],
            "description": quiz_row["description"],
            "topic": quiz_row["topic"],
            "difficulty": quiz_row["difficulty"],
            "question_count": quiz_row["question_count"],
            "questions": public_questions,
        }
        answer_keys[quiz_id] = {"questions": grading_questions}

    return public_quizzes, answer_keys, (quiz_datasets, quiz_documents, quiz_versions)


def _strip_snapshot_for_public(snapshot: dict[str, Any]) -> dict[str, Any]:
    """Return a copy of ``snapshot`` safe for anonymous GET.

    Specifically removes the ``quiz_answer_keys`` field which contains grading
    secrets. The ``messages[].quiz_data`` embedded at snapshot time has already
    been stripped of answers, so it is safe to return as-is.
    """
    if not isinstance(snapshot, dict):
        return snapshot
    public = dict(snapshot)
    public.pop("quiz_answer_keys", None)
    return public


def _json_value(value: Any) -> Any:
    return json.loads(value) if isinstance(value, str) else value


def _snapshot_hash(
    snapshot: dict[str, Any], *, include_artifacts: bool, expires_days: int | None,
    audience: str = "public", source_scope: dict[str, Any] | None = None,
) -> str:
    payload = {
        "snapshot": snapshot,
        "include_artifacts": include_artifacts,
        "expires_days": expires_days,
        "audience": audience,
        "source_scope": source_scope,
    }
    return hashlib.sha256(
        json.dumps(payload, sort_keys=True, ensure_ascii=False, default=str).encode("utf-8")
    ).hexdigest()


def _require_safe_share_snapshot(snapshot: Any, audience: str = "public") -> dict[str, Any]:
    expected = (
        (3, "verified_internal_source_scope") if audience == "internal"
        else (2, "verified_no_private_knowledge")
    )
    if (
        not isinstance(snapshot, dict)
        or (snapshot.get("share_snapshot_version"), snapshot.get("source_policy")) != expected
    ):
        raise HTTPException(410, "This older share needs a new source-rights review")
    return snapshot


async def _build_share_snapshot(
    request: Request,
    *,
    session_id: str,
    user: UserContext,
    include_artifacts: bool,
    audience: Literal["public", "internal"] = "public",
) -> tuple[dict[str, Any], str, dict[str, Any] | None]:
    """Build the exact visitor snapshot from owner-scoped, source-checked facts."""
    db = _get_db(request)
    session = await db.fetchrow(
        "SELECT session_id, history, metadata, user_id, tenant_id "
        "FROM assistant.sessions WHERE session_id = $1 AND user_id = $2 AND tenant_id = $3",
        session_id,
        user.user_id,
        user.tenant_id or "",
    )
    if not session:
        raise HTTPException(404, "Session not found")
    legacy_raw = _json_value(session["history"]) or []
    legacy = legacy_raw.get("messages", []) if isinstance(legacy_raw, dict) else legacy_raw
    if not isinstance(legacy, list) or len(legacy) > 1000:
        raise HTTPException(409, "Conversation history cannot be verified for sharing")
    # Legacy image turns have an explicit no-knowledge origin; older generic
    # assistant turns do not carry sufficient per-turn provenance.
    for message in legacy:
        if not isinstance(message, dict):
            raise HTTPException(409, "Conversation history cannot be verified for sharing")
        if message.get("role") == "assistant":
            meta = message.get("metadata")
            if not isinstance(meta, dict) or meta.get("source_kind") != "image_generation":
                raise HTTPException(409, "Older assistant messages have unverified source rights")

    sources = await conversation_sources(request, user, session_id)
    if audience == "public" and (sources.dataset_ids or sources.document_ids or sources.source_versions):
        raise HTTPException(409, "Knowledge-derived content cannot be shared anonymously")

    store = getattr(request.app.state, "agent_thread_store", None) or AgentThreadStore(db)
    thread = await store.get_for_session(
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        session_id=session_id,
    )
    runtime_messages: list[dict[str, Any]] = []
    if thread is not None:
        runtime_messages, total = await store.history_messages(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            runtime_thread_id=thread.runtime_thread_id,
            limit=1000,
        )
        if total > len(runtime_messages):
            raise HTTPException(409, "Conversation is too long to preview completely")
        rows = await db.fetch(
            "SELECT run_id, snapshot FROM assistant_runtime_snapshots "
            "WHERE session_id = $1 AND tenant_id = $2 AND user_id = $3",
            session_id,
            user.tenant_id,
            user.user_id,
        )
        checked_runs: set[str] = set()
        for row in rows:
            payload = _json_value(row["snapshot"])
            readonly = payload.get("readonly_capabilities") if isinstance(payload, dict) else None
            items = readonly.get("items") if isinstance(readonly, dict) else None
            if not isinstance(items, list):
                raise HTTPException(409, "Conversation source rights cannot be verified")
            if audience == "public" and any(isinstance(item, dict) and item.get("kind") == "knowledge" for item in items):
                raise HTTPException(409, "Private knowledge content cannot be shared anonymously")
            checked_runs.add(str(row["run_id"]))
        for message in runtime_messages:
            run_id = (message.get("metadata") or {}).get("runtime_run_id")
            if not run_id or str(run_id) not in checked_runs:
                raise HTTPException(409, "Conversation source rights cannot be verified")

    history = legacy + runtime_messages
    if not history:
        raise HTTPException(400, "Session has no messages")

    artifacts_data: list[dict[str, Any]] = []
    if include_artifacts:
        rows = await db.fetch(
            "SELECT artifact_id, type, format, title, filename, size_bytes, mime_type, source "
            "FROM assistant.artifacts WHERE session_id = $1 AND tenant_id = $2 AND user_id = $3 "
            "AND source <> 'user' AND size_bytes > 0 AND variant = 'raw' "
            "ORDER BY created_at, artifact_id",
            session_id,
            user.tenant_id,
            user.user_id,
        )
        artifacts_data = [dict(row) for row in rows]
    artifact_ids = {str(row["artifact_id"]) for row in artifacts_data}
    public_messages: list[dict[str, Any]] = []
    for message in history:
        if not isinstance(message, dict) or message.get("role") not in {"user", "assistant"}:
            continue
        content = message.get("content")
        if not isinstance(content, str):
            continue
        public: dict[str, Any] = {"role": message["role"], "content": content}
        timestamp = message.get("timestamp")
        if isinstance(timestamp, str):
            public["timestamp"] = timestamp
        metadata = message.get("metadata")
        if isinstance(metadata, dict) and message["role"] == "assistant":
            safe_meta: dict[str, Any] = {}
            quiz_id = metadata.get("quiz_id")
            if isinstance(quiz_id, str):
                safe_meta["quiz_id"] = quiz_id
            linked = metadata.get("artifact_ids")
            if isinstance(linked, list):
                safe_meta["artifact_ids"] = [
                    item for item in linked if isinstance(item, str) and item in artifact_ids
                ]
            if safe_meta:
                public["metadata"] = safe_meta
        public_messages.append(public)

    public_quizzes, answer_keys, quiz_scope = await _collect_quiz_payloads(
        request,
        db, public_messages, tenant_id=user.tenant_id or "", user_id=user.user_id,
        audience=audience,
    )
    for message in public_messages:
        quiz_id = (message.get("metadata") or {}).get("quiz_id")
        if quiz_id in public_quizzes:
            message["quiz_data"] = public_quizzes[quiz_id]
    raw_meta = _json_value(session["metadata"]) or {}
    title = raw_meta.get("title", "") if isinstance(raw_meta, dict) else ""
    snapshot: dict[str, Any] = {
        "share_snapshot_version": 3 if audience == "internal" else 2,
        "source_policy": "verified_internal_source_scope" if audience == "internal" else "verified_no_private_knowledge",
        "messages": public_messages,
        "artifacts": artifacts_data,
    }
    if answer_keys:
        snapshot["quiz_answer_keys"] = answer_keys
    source_scope = None
    if audience == "internal":
        await require_active_internal_user(request, user, user.tenant_id or "")
        dataset_ids = frozenset(set(sources.dataset_ids) | quiz_scope[0])
        document_ids = frozenset(set(sources.document_ids) | quiz_scope[1])
        versions = frozenset(set(sources.source_versions) | quiz_scope[2])
        source_scope = freeze_source_scope(dataset_ids, document_ids, versions)
        if not await source_scope_allowed(request, user, dataset_ids, document_ids, versioned_refs=versions):
            raise HTTPException(403, "Share source access has been revoked")
    return snapshot, title, source_scope


# ── Create Share ─────────────────────────────────────────────────────


@router.get("/sessions/{session_id}/share-preview")
async def preview_share(
    session_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
    include_artifacts: bool = True,
    expires_days: int | None = Query(default=None, ge=1, le=365),
    audience: Literal["public", "internal"] = "public",
):
    """Show the owner exactly what a visitor will receive before sharing."""
    snapshot, title, source_scope = await _build_share_snapshot(
        request,
        session_id=session_id,
        user=user,
        include_artifacts=include_artifacts,
        audience=audience,
    )
    return {
        "title": title,
        "messages": _strip_snapshot_for_public(snapshot)["messages"],
        "artifacts": snapshot["artifacts"],
        "message_count": len(snapshot["messages"]),
        "artifact_count": len(snapshot["artifacts"]),
        "audience": audience,
        "expires_days": expires_days,
        "preview_hash": _snapshot_hash(
            snapshot,
            include_artifacts=include_artifacts,
            expires_days=expires_days,
            audience=audience,
            source_scope=source_scope,
        ),
    }


@router.post("/sessions/{session_id}/share")
async def create_share(
    session_id: str,
    body: CreateShareRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Create a frozen conversation link under the selected audience policy."""
    db = _get_db(request)
    snapshot, title, source_scope = await _build_share_snapshot(
        request,
        session_id=session_id,
        user=user,
        include_artifacts=body.include_artifacts,
        audience=body.audience,
    )
    expected_hash = _snapshot_hash(
        snapshot,
        include_artifacts=body.include_artifacts,
        expires_days=body.expires_days,
        audience=body.audience,
        source_scope=source_scope,
    )
    if body.preview_hash != expected_hash:
        raise HTTPException(409, "Share preview changed. Review it again before creating a link")
    snapshot["shared_at"] = datetime.now(timezone.utc).isoformat()
    history = snapshot["messages"]
    artifacts_data = snapshot["artifacts"]

    share_code = _generate_share_code()
    for _ in range(5):
        existing = await db.fetchrow(
            "SELECT id FROM conversation_shares WHERE share_code = $1", share_code
        )
        if not existing:
            break
        share_code = _generate_share_code()
    else:
        raise HTTPException(409, "Could not generate unique share code, try again")

    expires_at = None
    if body.expires_days:
        expires_at = datetime.now(timezone.utc) + timedelta(days=body.expires_days)

    await db.execute(
        """
        INSERT INTO conversation_shares
            (share_code, session_id, user_id, tenant_id, title, snapshot, message_count,
             artifact_count, expires_at, audience, source_scope)
        VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7, $8, $9, $10, $11::jsonb)
        """,
        share_code,
        session_id,
        user.user_id,
        user.tenant_id or "",
        title or f"Conversation ({len(history)} messages)",
        json.dumps(snapshot, ensure_ascii=False, default=str),
        len(history),
        len(artifacts_data),
        expires_at,
        body.audience,
        json.dumps(source_scope) if source_scope is not None else None,
    )

    share_url = f"/share/{share_code}"
    logger.info(
        f"Created share {share_code} for session {session_id} ({len(history)} msgs, {len(artifacts_data)} artifacts)"
    )

    return ShareResponse(
        share_code=share_code,
        share_url=share_url,
        title=title,
        message_count=len(history),
        artifact_count=len(artifacts_data),
        created_at=datetime.now(timezone.utc).isoformat(),
        expires_at=expires_at.isoformat() if expires_at else None,
        audience=body.audience,
    )


# ── Visitor: Get Shared Conversation ─────────────────────────────────


@router.get("/shares/{share_code}")
async def get_share(share_code: str, request: Request):
    """Return a share only after its audience and live source checks pass."""
    db = _get_db(request)
    row = await db.fetchrow(
        "SELECT * FROM conversation_shares WHERE share_code = $1 AND is_active = TRUE",
        share_code,
    )
    if not row or not row["is_active"]:
        raise HTTPException(404, "Share not found or expired")
    if row["expires_at"] and row["expires_at"] < datetime.now(timezone.utc):
        raise HTTPException(410, "Share has expired")

    audience = row.get("audience") or "public"
    if audience == "internal":
        await require_internal_share_access(request, row)
    elif audience != "public":
        raise HTTPException(410, "Share audience cannot be verified")

    with suppress(Exception):
        await db.execute(
            "UPDATE conversation_shares SET view_count = view_count + 1 WHERE share_code = $1",
            share_code,
        )

    snapshot = _require_safe_share_snapshot(_json_value(row["snapshot"]), audience)
    snapshot = _strip_snapshot_for_public(snapshot)

    result = {
        "share_code": share_code,
        "title": row["title"],
        "snapshot": snapshot,
        "message_count": row["message_count"],
        "artifact_count": row["artifact_count"],
        "view_count": (row["view_count"] or 0) + 1,
        "created_at": row["created_at"].isoformat() if row["created_at"] else None,
        "expires_at": row["expires_at"].isoformat() if row["expires_at"] else None,
        "audience": audience,
    }
    return JSONResponse(result, headers={"Cache-Control": "no-store"}) if audience == "internal" else result


# ── Visitor: Submit Shared Quiz Attempt ──────────────────────────────


class SharedQuizSubmitRequest(BaseModel):
    answers: dict[str, str] = Field(
        ...,
        description="question_id → selected option label (or free text for short_answer)",
    )


def _resolve_anon_id(request: Request) -> str:
    """Return the middleware-validated anonymous identity for a share viewer.

    ``StreamingAnonymousMiddleware`` validates or generates the stable ID and
    stores it on request state. Direct route invocations without that
    middleware retain a server-derived client-IP fallback; raw client-provided
    anonymous cookies and headers must not control quiz-attempt ownership.
    """
    anon_id = getattr(getattr(request, "state", None), "anonymous_id", None)
    if anon_id:
        return str(anon_id)[:128]
    return get_client_ip_from_request(request)[:128] or "anonymous"


@router.post("/shares/{share_code}/quiz/{quiz_id}/submit")
async def submit_shared_quiz(
    share_code: str,
    quiz_id: str,
    body: SharedQuizSubmitRequest,
    request: Request,
):
    """Grade a permitted viewer's answers against the frozen quiz snapshot.

    * Public links remain anonymous; internal links require current source access.
    * Keyed by ``(share_code, anon_id, quiz_id)`` using the trusted anonymous
      middleware identity — one attempt per viewer per quiz; resubmits return
      the cached result rather than re-grading.
    * Grades against the **frozen** answer key embedded in the share's
      snapshot, so even if the original quiz row is deleted the share still
      functions.
    """
    await enforce_rate_limit(request, user=None, operation="quiz_submit_public")
    db = _get_db(request)

    row = await db.fetchrow(
        "SELECT snapshot, expires_at, is_active, audience, source_scope, tenant_id "
        "FROM conversation_shares WHERE share_code = $1",
        share_code,
    )
    if not row or not row["is_active"]:
        raise HTTPException(404, "Share not found")
    if row["expires_at"] and row["expires_at"] < datetime.now(timezone.utc):
        raise HTTPException(410, "Share has expired")

    audience = row.get("audience") or "public"
    if audience == "internal":
        user = await require_internal_share_access(request, row)
    elif audience == "public":
        user = None
    else:
        raise HTTPException(410, "Share audience cannot be verified")
    snapshot = _require_safe_share_snapshot(_json_value(row["snapshot"]), audience)
    answer_keys = snapshot.get("quiz_answer_keys") if isinstance(snapshot, dict) else None
    if not isinstance(answer_keys, dict) or quiz_id not in answer_keys:
        raise HTTPException(404, "Quiz not found in this share")

    anon_id = f"user:{user.user_id}" if user else _resolve_anon_id(request)

    # Replay cached attempt if this anon viewer already submitted.
    try:
        quiz_uuid = uuid.UUID(quiz_id)
    except (ValueError, TypeError):
        raise HTTPException(404, "Quiz not found in this share")
    prior = await db.fetchrow(
        "SELECT result FROM conversation_share_quiz_attempts "
        "WHERE share_code = $1 AND anon_id = $2 AND quiz_id = $3",
        share_code,
        anon_id,
        quiz_uuid,
    )
    if prior:
        cached = (
            prior["result"] if isinstance(prior["result"], dict) else json.loads(prior["result"])
        )
        cached["cached"] = True
        return JSONResponse(cached, headers={"Cache-Control": "no-store"}) if user else cached

    grader = QuizGrader()
    grading_questions = answer_keys[quiz_id].get("questions", [])
    result = grader.grade(grading_questions, body.answers)

    attempt_id = str(uuid.uuid4())
    result_payload: dict[str, Any] = {"attempt_id": attempt_id, **result}

    try:
        await db.execute(
            """
            INSERT INTO conversation_share_quiz_attempts
                (id, share_code, anon_id, quiz_id, result)
            VALUES ($1, $2, $3, $4, $5::jsonb)
            """,
            uuid.UUID(attempt_id),
            share_code,
            anon_id,
            quiz_uuid,
            json.dumps(result_payload, default=str),
        )
    except Exception as e:
        # Likely a race — another parallel submit already inserted. Replay.
        logger.warning(f"Insert share quiz attempt failed ({e!r}); re-reading cached result")
        replay = await db.fetchrow(
            "SELECT result FROM conversation_share_quiz_attempts "
            "WHERE share_code = $1 AND anon_id = $2 AND quiz_id = $3",
            share_code,
            anon_id,
            quiz_uuid,
        )
        if replay:
            cached = (
                replay["result"]
                if isinstance(replay["result"], dict)
                else json.loads(replay["result"])
            )
            cached["cached"] = True
            return JSONResponse(cached, headers={"Cache-Control": "no-store"}) if user else cached
        raise HTTPException(500, "Failed to record attempt")

    return JSONResponse(result_payload, headers={"Cache-Control": "no-store"}) if user else result_payload


# ── Visitor: Download Shared Artifact ────────────────────────────────


@router.get("/shares/{share_code}/artifact/{artifact_id}")
async def download_shared_artifact(share_code: str, artifact_id: str, request: Request):
    """Download only after checking the share and current viewer rights."""
    db = _get_db(request)
    row = await db.fetchrow(
        "SELECT snapshot, expires_at, is_active, session_id, tenant_id, user_id, "
        "audience, source_scope "
        "FROM conversation_shares WHERE share_code = $1",
        share_code,
    )
    if not row or not row["is_active"]:
        raise HTTPException(404, "Share not found")
    if row["expires_at"] and row["expires_at"] < datetime.now(timezone.utc):
        raise HTTPException(410, "Share has expired")

    audience = row.get("audience") or "public"
    if audience == "internal":
        await require_internal_share_access(request, row)
    elif audience != "public":
        raise HTTPException(410, "Share audience cannot be verified")
    snapshot = _require_safe_share_snapshot(_json_value(row["snapshot"]), audience)
    artifact_ids = [a["artifact_id"] for a in snapshot.get("artifacts", [])]
    if artifact_id not in artifact_ids:
        raise HTTPException(404, "Artifact not in this share")

    artifact_storage = _get_artifact_storage(request)
    if not artifact_storage:
        raise HTTPException(503, "Artifact storage not available")

    try:
        artifact = await artifact_storage.get_artifact(artifact_id)
        if (
            not artifact
            or artifact.session_id != row["session_id"]
            or artifact.tenant_id != row["tenant_id"]
            or artifact.user_id != row["user_id"]
            or artifact.source == "user"
            or artifact.size_bytes <= 0
        ):
            raise HTTPException(404, "Artifact not found in storage")
        # Keep public reads behind the share check on every request. A
        # presigned storage URL would outlive share revocation or expiry.
        content = await artifact_storage.download_artifact(artifact_id)
        if content is None:
            raise HTTPException(404, "Artifact content not found")
        return StreamingResponse(
            iter([content]),
            media_type=artifact.mime_type or "application/octet-stream",
            headers={
                "Content-Disposition": attachment_content_disposition(artifact.filename),
                "Content-Length": str(len(content)),
                "Cache-Control": "no-store" if audience == "internal" else "private, max-age=0",
            },
        )
    except HTTPException:
        raise
    except Exception as e:
        logger.error(f"Failed to get artifact URL: {e}")
        raise HTTPException(404, "Artifact not found")


# ── Authenticated: List User's Shares ────────────────────────────────


@router.get("/shares")
async def list_shares(
    request: Request,
    user: UserContext = Depends(get_user_context),
    limit: int = Query(default=50, ge=1, le=200),
    session_id: str | None = None,
):
    """List shares created by the current user."""
    db = _get_db(request)
    if session_id:
        rows = await db.fetch(
            "SELECT share_code, session_id, title, message_count, artifact_count, view_count, "
            "is_active, created_at, expires_at, audience FROM conversation_shares "
            "WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3 "
            "ORDER BY created_at DESC LIMIT $4",
            user.tenant_id or "", user.user_id, session_id, limit,
        )
    else:
        rows = await db.fetch(
            "SELECT share_code, session_id, title, message_count, artifact_count, view_count, "
            "is_active, created_at, expires_at, audience FROM conversation_shares "
            "WHERE tenant_id = $1 AND user_id = $2 ORDER BY created_at DESC LIMIT $3",
            user.tenant_id or "", user.user_id, limit,
        )
    return {"shares": [dict(r) for r in rows]}


# ── Authenticated: Revoke Share ──────────────────────────────────────


@router.delete("/shares/{share_code}")
async def revoke_share(
    share_code: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Revoke (deactivate) a share link."""
    db = _get_db(request)
    result = await db.execute(
        "UPDATE conversation_shares SET is_active = FALSE "
        "WHERE share_code = $1 AND tenant_id = $2 AND user_id = $3 AND is_active = TRUE",
        share_code,
        user.tenant_id or "",
        user.user_id,
    )
    if not result or result.endswith(" 0"):
        raise HTTPException(status_code=404, detail="Share not found")
    return {"status": "revoked", "share_code": share_code}
