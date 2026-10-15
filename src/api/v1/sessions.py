from __future__ import annotations

from typing import Any
from uuid import UUID

from ai_gateway_core.exceptions import SessionAlreadyExistsError
from fastapi import APIRouter, Body, Depends, HTTPException, Query, Request
from pydantic import BaseModel, field_validator

from ...core.auth.user_resolver import UserContext
from ...services.session.session_manager import SessionManager
from ..deps import get_session_manager, get_user_context

router = APIRouter()


class SessionCreate(BaseModel):
    session_id: str | None = None
    service_id: str | None = None
    metadata: dict[str, Any] | None = None
    config: dict[str, Any] | None = None  # 会话配置（知识库、模型等）

    @field_validator("session_id")
    @classmethod
    def _normalize_session_id(cls, value: str | None) -> str | None:
        if value is None or not str(value).strip():
            return None
        return str(UUID(str(value).strip()))


class SessionUpdate(BaseModel):
    metadata: dict[str, Any] | None = None
    config: dict[str, Any] | None = None  # 会话配置（知识库、模型等）


class SessionMessageCreate(BaseModel):
    role: str
    content: Any
    metadata: dict[str, Any] | None = None


async def _list_assistant_sessions_for_service_id(
    session_manager: SessionManager,
    user: UserContext,
    *,
    limit: int,
    service_id: str,
):
    """Backward-compatible assistant session listing helper."""
    if service_id != "__builtin_assistant__":
        return await session_manager.list_sessions(
            user_id=user.user_id,
            tenant_id=user.tenant_id,
            service_id=service_id,
            limit=limit,
            status="active",
        )

    sessions = await session_manager.list_sessions(
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        service_id=None,
        limit=limit,
        status="active",
    )
    allowed = {"__builtin_assistant__", "assistant", None, ""}
    filtered = [session for session in sessions if getattr(session, "service_id", None) in allowed]
    filtered.sort(key=lambda item: getattr(item, "updated_at", None), reverse=True)
    return filtered[:limit]


@router.get("/sessions")
async def list_sessions(
    service_id: str | None = Query(default=None),
    limit: int = Query(100, ge=1, le=500),
    session_manager: SessionManager = Depends(get_session_manager),
    user: UserContext = Depends(get_user_context),
):
    # Use lightweight summary query — skips history/state JSONB, slims metadata
    if service_id == "__builtin_assistant__":
        # Assistant historically had sessions with NULL/"" service_id; include them
        summaries = await session_manager.list_session_summaries(
            user_id=user.user_id,
            tenant_id=user.tenant_id,
            service_ids=["__builtin_assistant__", "assistant"],
            include_null_service_id=True,
            limit=limit,
        )
    elif service_id:
        # Strict match — do NOT include NULL service_id sessions (avoids pollution from other services)
        summaries = await session_manager.list_session_summaries(
            user_id=user.user_id,
            tenant_id=user.tenant_id,
            service_ids=[service_id],
            include_null_service_id=False,
            limit=limit,
        )
    else:
        summaries = await session_manager.list_session_summaries(
            user_id=user.user_id,
            tenant_id=user.tenant_id,
            limit=limit,
        )
    return [
        {
            "session_id": s.get("session_id"),
            "user_id": s.get("user_id"),
            "tenant_id": s.get("tenant_id"),
            "service_id": s.get("service_id"),
            "created_at": s.get("created_at"),
            "updated_at": s.get("updated_at"),
            "metadata": s.get("metadata"),
            "config": s.get("config"),
        }
        for s in summaries
    ]


@router.post("/sessions")
async def create_session(
    body: SessionCreate = Body(default_factory=SessionCreate),
    session_manager: SessionManager = Depends(get_session_manager),
    user: UserContext = Depends(get_user_context),
):
    # DatabaseSessionManager 支持 config 参数，内存版本不支持
    create_kwargs = {
        "user_id": user.user_id,
        "tenant_id": user.tenant_id,
        "service_id": body.service_id,
        "session_id": body.session_id,
        "metadata": body.metadata,
    }
    if body.config:
        create_kwargs["config"] = body.config
    if body.session_id:
        create_kwargs["fail_if_exists"] = True

    try:
        session = await session_manager.create(**create_kwargs)
    except SessionAlreadyExistsError as exc:
        raise HTTPException(status_code=409, detail="session already exists") from exc
    return {"session_id": session.session_id}


@router.get("/sessions/{session_id}")
async def get_session(
    session_id: str,
    session_manager: SessionManager = Depends(get_session_manager),
    user: UserContext = Depends(get_user_context),
):
    session = await session_manager.get(session_id)
    if not session or (session.user_id != user.user_id or session.tenant_id != user.tenant_id):
        raise HTTPException(status_code=404, detail="session not found")
    return {
        "session_id": session.session_id,
        "service_id": session.service_id,
        "user_id": session.user_id,
        "tenant_id": session.tenant_id,
        "created_at": session.created_at,
        "updated_at": session.updated_at,
        "metadata": session.metadata,
        "config": getattr(session, "config", None),
    }


@router.patch("/sessions/{session_id}")
async def update_session(
    session_id: str,
    body: SessionUpdate,
    session_manager: SessionManager = Depends(get_session_manager),
    user: UserContext = Depends(get_user_context),
):
    session = await session_manager.get(session_id)
    if not session or (session.user_id != user.user_id or session.tenant_id != user.tenant_id):
        raise HTTPException(status_code=404, detail="session not found")

    if body.metadata:
        # 支持 update_metadata 方法（DatabaseSessionManager）或直接更新（内存版本）
        if hasattr(session_manager, "update_metadata"):
            await session_manager.update_metadata(session_id, body.metadata)
        else:
            session.metadata = session.metadata or {}
            session.metadata.update(body.metadata)

    if body.config:
        # 支持 update_config 方法（DatabaseSessionManager）或直接更新（内存版本）
        if hasattr(session_manager, "update_config"):
            await session_manager.update_config(session_id, body.config)
        else:
            session.config = getattr(session, "config", {}) or {}
            session.config.update(body.config)

    return {"session_id": session_id, "status": "updated"}


@router.delete("/sessions/{session_id}")
async def delete_session(
    session_id: str,
    request: Request,
    session_manager: SessionManager = Depends(get_session_manager),
    user: UserContext = Depends(get_user_context),
):
    session = await session_manager.get(session_id)
    if not session or (session.user_id != user.user_id or session.tenant_id != user.tenant_id):
        raise HTTPException(status_code=404, detail="session not found")
    # ``assistant_runtime_threads`` references ``sessions`` ON DELETE RESTRICT,
    # so the Runtime thread has to be tombstoned first. /api/v1/assistant/
    # sessions/{id} already does this; without the same step here a session
    # that ever ran a turn fails the delete with a raw ForeignKeyViolation 500.
    control = getattr(request.app.state, "agent_runtime_control", None)
    if control is not None:
        from ...services.agent_runtime import AgentRuntimeControlError

        try:
            await control.cleanup_session(
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                session_id=session_id,
            )
        except AgentRuntimeControlError as exc:
            raise HTTPException(status_code=exc.status_code, detail=exc.code) from exc
    await session_manager.delete(session_id)
    return {"session_id": session_id, "status": "deleted"}


@router.post("/sessions/{session_id}/messages")
async def add_session_message(
    session_id: str,
    body: SessionMessageCreate,
    session_manager: SessionManager = Depends(get_session_manager),
    user: UserContext = Depends(get_user_context),
):
    session = await session_manager.get(session_id)
    if not session or (session.user_id != user.user_id or session.tenant_id != user.tenant_id):
        raise HTTPException(status_code=404, detail="session not found")

    await session_manager.add_message(
        session_id=session_id,
        role=body.role,
        content=body.content,
        metadata=body.metadata,
    )

    return {"session_id": session_id, "status": "added"}


@router.get("/sessions/{session_id}/history")
async def get_history(
    session_id: str,
    limit: int = Query(50, ge=1, le=500),
    session_manager: SessionManager = Depends(get_session_manager),
    user: UserContext = Depends(get_user_context),
):
    session = await session_manager.get(session_id)
    if not session or (session.user_id != user.user_id or session.tenant_id != user.tenant_id):
        raise HTTPException(status_code=404, detail="session not found")
    history = await session_manager.history(session_id, limit=limit)
    return [
        {
            "role": m.role,
            "content": m.content,
            "timestamp": m.timestamp,
            "metadata": m.metadata if hasattr(m, "metadata") and m.metadata else None,
        }
        for m in history
    ]
