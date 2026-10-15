"""Published Agent Runtime session and feedback handlers.

ARC-01B split of ``src/api/v1/agent_runtime.py``.  Moved verbatim; route
registration stays in the facade.  The published *chat stream* handler remains
in the facade because the single-kernel gate reads it from
``src/api/v1/agent_runtime.py`` (scripts/harness/agent_runtime_single_kernel_gate.py).
"""

from __future__ import annotations

import uuid

from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentRepositoryError,
)
from fastapi import Request

from ...schemas.agent_runtime import (
    AgentRuntimeFeedbackRequest,
    AgentRuntimeFeedbackResponse,
    AgentRuntimeSessionResponse,
)
from .._agent_runtime_headers import reject_client_agent_forgery
from .core import (
    _map_repository_error,
    _raise_runtime_error,
    _repository,
    _request_id,
    _resolve_api_caller,
)
from .rate_limit import _enforce_channel_limits
from .snapshot import _build_snapshot
from .streaming import _assert_existing_pin, _bind_session, _existing_session


async def create_published_session(
    publication_id: str,
    request: Request,
) -> AgentRuntimeSessionResponse:
    reject_client_agent_forgery(request)
    resolution, user = await _resolve_api_caller(
        request,
        publication_id=publication_id,
        required_scopes=["sessions:write"],
    )
    await _enforce_channel_limits(
        request,
        publication=resolution["publication"],
        principal_id=user.user_id,
    )
    snapshot = await _build_snapshot(request, resolution, user, channel="api")
    session_id = str(uuid.uuid4())
    await _bind_session(
        request,
        user,
        session_id=session_id,
        snapshot=snapshot,
        draft_revision=None,
    )
    return AgentRuntimeSessionResponse(
        session_id=session_id,
        agent_id=snapshot["agent_id"],
        agent_version_id=snapshot["agent_version_id"],
        publication_id=publication_id,
        channel="api",
        runtime_fingerprint=runtime_sha256(snapshot),
        request_id=_request_id(request),
    )


async def published_feedback(
    publication_id: str,
    payload: AgentRuntimeFeedbackRequest,
    request: Request,
) -> AgentRuntimeFeedbackResponse:
    reject_client_agent_forgery(request)
    resolution, user = await _resolve_api_caller(
        request,
        publication_id=publication_id,
        required_scopes=["feedback:write"],
    )
    existing = await _existing_session(request, payload.session_id)
    if not existing:
        _raise_runtime_error(
            request,
            404,
            "AGENT_RUNTIME_SESSION_NOT_FOUND",
            "Agent runtime session not found",
        )
    _assert_existing_pin(
        request,
        user,
        existing,
        agent_id=str(resolution["agent"]["agent_id"]),
        agent_version_id=existing.agent_version_id,
        publication_id=publication_id,
        channel="api",
        draft_revision=None,
    )
    try:
        row = await _repository(request).record_runtime_feedback(
            tenant_id=user.tenant_id,
            publication_id=publication_id,
            agent_version_id=existing.agent_version_id,
            session_id=payload.session_id,
            principal_id=user.user_id,
            channel="api",
            rating=payload.rating,
            comment=payload.comment,
        )
    except AgentRepositoryError as exc:
        _map_repository_error(request, exc)
    return AgentRuntimeFeedbackResponse(
        feedback_id=str(row["feedback_id"]),
        session_id=payload.session_id,
        rating=payload.rating,
        request_id=_request_id(request),
    )
