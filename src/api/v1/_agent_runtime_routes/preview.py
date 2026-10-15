"""Agent Studio preview session handlers (draft and version previews).

ARC-01B split of ``src/api/v1/agent_runtime.py``.  Moved verbatim; route
registration stays in the facade.  The preview *chat stream* handlers remain
in the facade because the single-kernel gate reads them from
``src/api/v1/agent_runtime.py`` (scripts/harness/agent_runtime_single_kernel_gate.py).
"""

from __future__ import annotations

import uuid
from typing import Literal

from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentNotFoundError,
    AgentRepositoryError,
)
from fastapi import Depends, Request
from pydantic import BaseModel, ConfigDict

from ....core.auth.user_resolver import UserContext
from ...deps import get_user_context
from ...schemas.agent_runtime import (
    AgentPreviewSessionRequest,
    AgentRuntimeSessionResponse,
    AgentVersionPreviewSessionRequest,
)
from .._agent_runtime_headers import reject_client_agent_forgery
from .core import (
    _is_tenant_admin,
    _map_repository_error,
    _raise_runtime_error,
    _repository,
    _request_id,
    _require_actor,
    _session_manager,
)
from .resolution import _public_effective_native_capabilities
from .snapshot import _build_snapshot
from .streaming import _assert_existing_pin, _bind_session


class PreviewSessionRecoveryResponse(BaseModel):
    model_config = ConfigDict(extra="forbid")

    session_id: str
    agent_id: str
    agent_version_id: str | None
    draft_revision: int | None
    channel: Literal["preview"]
    request_id: str


async def get_preview_session(
    agent_id: str,
    session_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> PreviewSessionRecoveryResponse:
    """Read an owned Preview pin before restoring history or Runtime controls."""
    _require_actor(request, user)
    existing = await _session_manager(request).get(session_id)
    if existing is None or (
        (existing.agent_version_id is None) == (existing.agent_draft_revision is None)
    ):
        _raise_runtime_error(
            request, 404, "AGENT_RUNTIME_SESSION_NOT_FOUND", "Agent runtime session not found"
        )
    _assert_existing_pin(
        request,
        user,
        existing,
        agent_id=agent_id,
        agent_version_id=existing.agent_version_id,
        publication_id=None,
        channel="preview",
        draft_revision=existing.agent_draft_revision,
    )
    return PreviewSessionRecoveryResponse(
        session_id=session_id,
        agent_id=agent_id,
        agent_version_id=existing.agent_version_id,
        draft_revision=existing.agent_draft_revision,
        channel="preview",
        request_id=_request_id(request),
    )


async def create_preview_session(
    agent_id: str,
    payload: AgentPreviewSessionRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentRuntimeSessionResponse:
    _require_actor(request, user)
    reject_client_agent_forgery(request)
    try:
        resolution = await _repository(request).resolve_preview_runtime(
            tenant_id=user.tenant_id,
            agent_id=agent_id,
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
            draft_revision=payload.draft_revision,
        )
        snapshot = await _build_snapshot(
            request,
            resolution,
            user,
            channel="preview",
        )
        session_id = str(uuid.uuid4())
        await _bind_session(
            request,
            user,
            session_id=session_id,
            snapshot=snapshot,
            draft_revision=payload.draft_revision,
        )
    except (AgentRepositoryError, AgentNotFoundError) as exc:
        _map_repository_error(request, exc)
    return AgentRuntimeSessionResponse(
        session_id=session_id,
        agent_id=snapshot["agent_id"],
        agent_version_id=None,
        draft_revision=payload.draft_revision,
        publication_id=None,
        channel="preview",
        runtime_fingerprint=runtime_sha256(snapshot),
        effective_capabilities=_public_effective_native_capabilities(snapshot),
        request_id=_request_id(request),
    )


async def create_version_preview_session(
    agent_id: str,
    agent_version_id: str,
    _payload: AgentVersionPreviewSessionRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AgentRuntimeSessionResponse:
    _require_actor(request, user)
    reject_client_agent_forgery(request)
    try:
        resolution = await _repository(request).resolve_version_runtime(
            tenant_id=user.tenant_id,
            agent_id=agent_id,
            agent_version_id=agent_version_id,
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
        snapshot = await _build_snapshot(request, resolution, user, channel="preview")
        session_id = str(uuid.uuid4())
        await _bind_session(
            request,
            user,
            session_id=session_id,
            snapshot=snapshot,
            draft_revision=None,
        )
    except (AgentRepositoryError, AgentNotFoundError) as exc:
        _map_repository_error(request, exc)
    return AgentRuntimeSessionResponse(
        session_id=session_id,
        agent_id=snapshot["agent_id"],
        agent_version_id=snapshot["agent_version_id"],
        draft_revision=None,
        publication_id=None,
        channel="preview",
        runtime_fingerprint=runtime_sha256(snapshot),
        effective_capabilities=_public_effective_native_capabilities(snapshot),
        request_id=_request_id(request),
    )
