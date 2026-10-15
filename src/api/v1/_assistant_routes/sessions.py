"""Assistant session CRUD + history routes.

ARC-01 split of ``src/api/v1/assistant.py``.  Session rows stay in the
Gateway-owned session store; history stays a *read-only projection* of the
Agent Runtime thread (``AgentThreadStore``) appended after any legacy
messages — this surface must never become the write authority for turn
state again.
"""

from __future__ import annotations

import logging

from ai_gateway_core.logging import record_internal_exception
from fastapi import APIRouter, Depends, HTTPException, Query, Request

from ....core.auth.user_resolver import UserContext
from ....services.agent_runtime.thread_store import AgentThreadStore
from ....services.assistant_entry.run_queries import agent_runtime_control
from ....services.assistant_entry.session_binding import (
    ASSISTANT_SERVICE_IDS,
    get_session_manager,
)
from ....services.assistant_entry.source_access import (
    conversation_sources,
    visible_dataset_names,
    visible_document_keys,
    visible_source_version_keys,
)
from ....services.images.history import image_history
from ...deps import get_user_context
from .schemas import (
    SessionCreateRequest,
    SessionHistoryMessage,
    SessionHistoryResponse,
    SessionListResponse,
    SessionResponse,
)

router = APIRouter()
logger = logging.getLogger(__name__)


async def _list_assistant_session_summaries(session_manager, user: UserContext, limit: int):
    """List assistant session summaries (lightweight, no history)."""
    return await session_manager.list_session_summaries(
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        service_ids=list(ASSISTANT_SERVICE_IDS),
        include_null_service_id=True,
        limit=limit,
    )


async def _list_assistant_sessions(session_manager, user: UserContext, limit: int):
    """Backward-compatible full session listing for assistant sessions."""
    sessions = []
    for service_id in ASSISTANT_SERVICE_IDS:
        sessions.extend(
            await session_manager.list_sessions(
                user_id=user.user_id,
                tenant_id=user.tenant_id,
                service_id=service_id,
                limit=limit,
                status="active",
            )
        )
    sessions.sort(key=lambda item: getattr(item, "updated_at", None), reverse=True)
    return sessions[:limit]


@router.post("/sessions", response_model=SessionResponse)
async def create_session(
    body: SessionCreateRequest = None,
    user: UserContext = Depends(get_user_context),
    request: Request = None,
) -> SessionResponse:
    """
    Create a new assistant conversation session.

    Creates a persistent session for storing conversation history.
    Sessions are isolated by user and tenant.
    """
    session_manager = get_session_manager(request)

    try:
        session = await session_manager.create(
            user_id=user.user_id,
            tenant_id=user.tenant_id,
            service_id="__builtin_assistant__",  # 保留的 service_id，避免与用户注册的服务冲突
            metadata=body.metadata if body else None,
        )
        assignment_store = getattr(request.app.state, "assistant_runtime_assignments", None)
        if assignment_store is not None:
            try:
                policy = getattr(request.app.state, "assistant_runtime_assignment_policy", None)
                if policy is not None and hasattr(assignment_store, "bind_new_session"):
                    await assignment_store.bind_new_session(
                        tenant_id=user.tenant_id,
                        user_id=user.user_id,
                        session_id=session.session_id,
                        policy=policy,
                    )
                else:
                    await assignment_store.bind(
                        tenant_id=user.tenant_id,
                        user_id=user.user_id,
                        session_id=session.session_id,
                        runtime_owner=request.app.state.assistant_runtime_default_owner,
                        kernel_revision=request.app.state.assistant_runtime_kernel_revision,
                    )
            except Exception:
                await session_manager.delete(session.session_id)
                raise

        return SessionResponse(
            session_id=session.session_id,
            user_id=session.user_id,
            tenant_id=session.tenant_id,
            service_id=session.service_id,
            created_at=session.created_at.isoformat() if session.created_at else None,
            updated_at=session.updated_at.isoformat() if session.updated_at else None,
            metadata=session.metadata,
            message_count=len(session.history) if session.history else 0,
        )
    except Exception as exc:
        record_internal_exception(logger, "assistant.gateway.session.create_failure", exc)
        raise HTTPException(status_code=500, detail="Failed to create session") from None


@router.get("/sessions", response_model=SessionListResponse)
async def list_sessions(
    limit: int = Query(50, ge=1, le=200),
    user: UserContext = Depends(get_user_context),
    request: Request = None,
) -> SessionListResponse:
    """
    List user's assistant conversation sessions.

    Returns sessions sorted by most recently updated.
    Sessions are filtered by user and tenant for isolation.
    """
    session_manager = get_session_manager(request)

    try:
        summaries = await _list_assistant_session_summaries(session_manager, user, limit)

        return SessionListResponse(
            sessions=[
                SessionResponse(
                    session_id=s.get("session_id"),
                    user_id=s.get("user_id"),
                    tenant_id=s.get("tenant_id"),
                    service_id=s.get("service_id"),
                    created_at=s.get("created_at"),
                    updated_at=s.get("updated_at"),
                    metadata=s.get("metadata"),
                    message_count=0,  # not computed in summary path; use /history if needed
                )
                for s in summaries
            ],
            total=len(summaries),
        )
    except Exception as exc:
        record_internal_exception(logger, "assistant.gateway.session.list_failure", exc)
        raise HTTPException(status_code=500, detail="Failed to list sessions") from None


@router.get("/sessions/{session_id}", response_model=SessionResponse)
async def get_session(
    session_id: str,
    user: UserContext = Depends(get_user_context),
    request: Request = None,
) -> SessionResponse:
    """
    Get assistant session details.

    Returns session metadata and message count.
    User isolation is enforced.
    """
    session_manager = get_session_manager(request)

    try:
        session = await session_manager.get(session_id)
        if not session:
            raise HTTPException(status_code=404, detail="Session not found")

        # Enforce user isolation
        if session.user_id != user.user_id or session.tenant_id != user.tenant_id:
            raise HTTPException(status_code=404, detail="Session not found")

        return SessionResponse(
            session_id=session.session_id,
            user_id=session.user_id,
            tenant_id=session.tenant_id,
            service_id=session.service_id,
            created_at=session.created_at.isoformat() if session.created_at else None,
            updated_at=session.updated_at.isoformat() if session.updated_at else None,
            metadata=session.metadata,
            message_count=len(session.history) if session.history else 0,
        )
    except HTTPException:
        raise
    except Exception as exc:
        record_internal_exception(logger, "assistant.gateway.session.get_failure", exc)
        raise HTTPException(status_code=500, detail="Failed to get session") from None


@router.delete("/sessions/{session_id}")
async def delete_session(
    session_id: str,
    user: UserContext = Depends(get_user_context),
    request: Request = None,
):
    """
    Delete an assistant session.

    Permanently deletes the session and all its message history.
    User isolation is enforced.
    """
    session_manager = get_session_manager(request)

    try:
        # Verify ownership against the Gateway's canonical session row before
        # tombstoning the authoritative Agent Runtime thread.
        session = await session_manager.get(session_id)
        if not session:
            raise HTTPException(status_code=404, detail="Session not found")

        if session.user_id != user.user_id or session.tenant_id != user.tenant_id:
            raise HTTPException(status_code=404, detail="Session not found")

        control = agent_runtime_control(request)
        await control.cleanup_session(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=session_id,
        )

        await session_manager.delete(session_id)
        if await session_manager.get(session_id) is not None:
            raise HTTPException(status_code=503, detail="Session deletion was not completed")

        return {"session_id": session_id, "status": "deleted"}
    except HTTPException:
        raise
    except Exception as exc:
        record_internal_exception(logger, "assistant.gateway.session.delete_failure", exc)
        raise HTTPException(status_code=500, detail="Failed to delete session") from None


@router.get("/sessions/{session_id}/history", response_model=SessionHistoryResponse)
async def get_session_history(
    session_id: str,
    limit: int = Query(100, ge=1, le=500),
    user: UserContext = Depends(get_user_context),
    request: Request = None,
) -> SessionHistoryResponse:
    """
    Get session message history.

    Returns the conversation messages for resuming a chat.
    Messages are returned in chronological order.
    """
    # Runtime turns are read from the Agent Runtime thread projection; this
    # route never writes turn state back into the session store.
    session_manager = get_session_manager(request)

    try:
        session = await session_manager.get(session_id)
        if not session:
            raise HTTPException(status_code=404, detail="Session not found")

        # Enforce user isolation
        if session.user_id != user.user_id or session.tenant_id != user.tenant_id:
            raise HTTPException(status_code=404, detail="Session not found")

        visible_datasets = await visible_dataset_names(request, user)
        database = getattr(request.app.state, "database", None)
        sources = await conversation_sources(request, user, session_id) if database is not None else None
        visible_documents = await visible_document_keys(request, user, sources.document_ids) if sources else set()
        visible_versions = await visible_source_version_keys(request, user, sources.source_versions) if sources else set()
        restricted_runs = {
            run_id for run_id, dataset_ids in sources.inherited_by_run.items()
            if not dataset_ids <= visible_datasets.keys()
            or not (sources.documents_by_run or {}).get(run_id, frozenset()) <= visible_documents
            or not (sources.versions_by_run or {}).get(run_id, frozenset()) <= visible_versions
        } if sources else set()
        legacy_restricted = bool(sources and (
            not sources.legacy_dataset_ids <= visible_datasets.keys()
            or not sources.legacy_document_ids <= visible_documents
            or not sources.legacy_source_versions <= visible_versions
        ))

        def safe_history_metadata(raw: dict | None) -> dict | None:
            if not isinstance(raw, dict):
                return raw
            metadata = dict(raw)
            contexts = metadata.get("contexts")
            if isinstance(contexts, list):
                revoked = any(
                    isinstance(context, dict)
                    and isinstance(context.get("dataset_id"), str)
                    and context["dataset_id"] not in visible_datasets
                    for context in contexts
                )
                if revoked:
                    return {"source_access_revoked": True}
                metadata["contexts"] = [
                    context for context in contexts
                    if isinstance(context, dict)
                    and str(context.get("dataset_id") or "") in visible_datasets
                ]
            metadata.pop("rag_evaluation", None)
            return metadata

        legacy_messages: list[SessionHistoryMessage] = []
        for message in session.history or []:
            metadata = safe_history_metadata(message.metadata)
            restricted = (
                message.role == "assistant"
                and (legacy_restricted or (
                    isinstance(metadata, dict)
                    and metadata.get("source_access_revoked") is True
                ))
            )
            if restricted:
                metadata = {"source_access_revoked": True}
            legacy_messages.append(SessionHistoryMessage(
                role=message.role,
                content="" if restricted else message.content,
                timestamp=message.timestamp.isoformat() if message.timestamp else None,
                metadata=metadata,
            ))
        runtime_messages: list[SessionHistoryMessage] = []
        runtime_total = 0
        if database is not None:
            store = getattr(request.app.state, "agent_thread_store", None)
            if store is None:
                store = AgentThreadStore(database)
                request.app.state.agent_thread_store = store
            thread = await store.get_for_session(
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                session_id=session_id,
            )
            if thread is not None:
                projected, runtime_total = await store.history_messages(
                    tenant_id=user.tenant_id,
                    user_id=user.user_id,
                    runtime_thread_id=thread.runtime_thread_id,
                    limit=limit,
                    allowed_datasets=visible_datasets,
                    restricted_run_ids=restricted_runs,
                )
                runtime_messages = [SessionHistoryMessage(**message) for message in projected]
        combined = await image_history(
            request, user, session_id,
            [m.model_dump() for m in legacy_messages + runtime_messages],
            sources, visible_datasets, limit=limit,
            identity_metadata=[m.metadata for m in session.history or []],
        )
        messages = [SessionHistoryMessage(**m) for m in combined[-limit:]]

        return SessionHistoryResponse(
            session_id=session_id,
            messages=messages,
            total=len(combined) + max(0, runtime_total - len(runtime_messages)),
        )
    except HTTPException:
        raise
    except Exception as exc:
        record_internal_exception(logger, "assistant.gateway.session.history_failure", exc)
        raise HTTPException(status_code=500, detail="Failed to get session history") from None
