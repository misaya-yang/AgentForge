"""Gateway session binding shared by the Assistant chat and Responses edges.

Moved verbatim from ``src/api/v1/assistant.py`` by ARC-01.  Session history
remains a read-only projection of the Agent Runtime thread store; nothing here
may turn ``sessions.history`` back into a write authority.
"""

from __future__ import annotations

import logging
from typing import Any

from ai_gateway_core.exceptions import SessionAlreadyExistsError
from fastapi import HTTPException, Request

from ...core.auth.user_resolver import UserContext

logger = logging.getLogger(__name__)

# Service ids that identify Assistant-owned sessions.  ``__builtin_assistant__``
# is the reserved id used when Gateway creates sessions itself; ``assistant``
# is the legacy id kept for backwards compatibility.
ASSISTANT_SERVICE_IDS: tuple[str, ...] = ("__builtin_assistant__", "assistant")


def get_session_manager(request: Request) -> Any:
    """Get session manager from app state."""
    manager = getattr(request.app.state, "session_manager", None)
    if manager is None:
        raise HTTPException(
            status_code=503,
            detail="Session manager is not initialized. Database may not be configured.",
        )
    return manager


async def validate_chat_session_access(
    request: Request,
    user: UserContext,
    session_id: str,
) -> None:
    """Return 404 when client-provided session is not accessible by current user."""
    if not session_id:
        return

    session_manager = get_session_manager(request)
    session = await session_manager.get(session_id)
    if not session:
        return

    if session.user_id != user.user_id or session.tenant_id != user.tenant_id:
        raise HTTPException(status_code=404, detail="Session not found")

    if session.service_id and session.service_id not in set(ASSISTANT_SERVICE_IDS):
        raise HTTPException(status_code=404, detail="Session not found")


async def session_runtime_assignment(
    request: Request,
    user: UserContext,
    session_id: str | None,
) -> Any | None:
    """Never let one session fall through to a different Agent kernel."""

    if not session_id:
        return None
    assignment_store = getattr(request.app.state, "assistant_runtime_assignments", None)
    if assignment_store is None:
        return None
    assignment = await assignment_store.resolve(
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        session_id=session_id,
    )
    if assignment is not None and assignment.runtime_owner != "agent_runtime":
        raise HTTPException(
            status_code=409,
            detail={
                "code": "AGENT_RUNTIME_ASSIGNMENT_INVALID",
                "message": "The session is not owned by the Agent Runtime",
            },
        )
    return assignment


async def ensure_agent_runtime_session(
    request: Request,
    user: UserContext,
    session_id: str,
) -> Any:
    """Create/bind a session before every V1 turn; no Python fallback exists."""
    session_manager = get_session_manager(request)
    session = await session_manager.get(session_id)
    if session is not None:
        if session.user_id != user.user_id or session.tenant_id != user.tenant_id:
            raise HTTPException(status_code=404, detail="Session not found")
    else:
        try:
            await session_manager.create(
                user_id=user.user_id,
                tenant_id=user.tenant_id,
                service_id="__builtin_assistant__",
                session_id=session_id,
                fail_if_exists=True,
            )
        except SessionAlreadyExistsError:
            session = await session_manager.get(session_id)
            if (
                session is None
                or session.user_id != user.user_id
                or session.tenant_id != user.tenant_id
            ):
                raise HTTPException(status_code=404, detail="Session not found") from None

    assignment_store = getattr(request.app.state, "assistant_runtime_assignments", None)
    if assignment_store is None:
        raise HTTPException(status_code=503, detail="Agent Runtime ownership is unavailable")
    assignment = await assignment_store.resolve(
        tenant_id=user.tenant_id, user_id=user.user_id, session_id=session_id
    )
    if assignment is None:
        policy = getattr(request.app.state, "assistant_runtime_assignment_policy", None)
        if policy is not None and hasattr(assignment_store, "bind_new_session"):
            assignment = await assignment_store.bind_new_session(
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                session_id=session_id,
                policy=policy,
            )
        else:
            assignment = await assignment_store.bind(
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                session_id=session_id,
                runtime_owner="agent_runtime",
                kernel_revision=getattr(
                    request.app.state, "assistant_runtime_kernel_revision", None
                ),
                assignment_reason="single_kernel",
            )
    if assignment.runtime_owner != "agent_runtime":
        raise HTTPException(status_code=409, detail="Invalid Agent Runtime ownership")
    return assignment
