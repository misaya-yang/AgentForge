"""Stable facade for the Agent Studio Runtime surface (ARC-01B).

The handlers live in ``src/api/v1/_agent_runtime_routes`` split by use case
(core plumbing / rate limiting / resolution / snapshot / attachments /
streaming startup / Studio preview / published API).  This module keeps the
public router, registers every route in the pre-split order, defines the three
chat-stream handlers the single-kernel gate reads from this file
(``scripts/harness/agent_runtime_single_kernel_gate.py``), and carries
time-limited compatibility re-exports.  It must not grow handler logic again.

Contract baseline: ``tmp/assistant-api-routes-before.json`` — the split is
verified zero-drift for paths, methods, operation ids and status codes.  All
eight route paths are pairwise non-overlapping (distinct literal segments or
segment counts, all POST), so grouping registration across sub-modules cannot
change matching behaviour; the original registration order is preserved anyway.
"""

from __future__ import annotations

import contextlib
import uuid
from typing import Any

from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentNotFoundError,
    AgentRepositoryError,
)
from fastapi import APIRouter, Depends, Request
from fastapi.responses import StreamingResponse

from ...core.auth.user_resolver import UserContext
from ..deps import get_user_context
from ..schemas.agent_runtime import (
    AgentPreviewChatRequest,
    AgentPublishedChatRequest,
    AgentRuntimeAttachmentUploadResponse,
    AgentRuntimeFeedbackResponse,
    AgentRuntimeSessionResponse,
    AgentVersionPreviewChatRequest,
)
from ._agent_runtime_headers import reject_client_agent_forgery
from ._agent_runtime_routes.attachments import _file_storage as _file_storage
from ._agent_runtime_routes.attachments import (
    _resolve_runtime_attachments as _resolve_runtime_attachments,
)
from ._agent_runtime_routes.attachments import (
    _store_runtime_attachment as _store_runtime_attachment,
)
from ._agent_runtime_routes.attachments import (
    upload_published_attachment as upload_published_attachment,
)
from ._agent_runtime_routes.core import _bearer_token as _bearer_token
from ._agent_runtime_routes.core import _is_tenant_admin as _is_tenant_admin
from ._agent_runtime_routes.core import _map_repository_error as _map_repository_error
from ._agent_runtime_routes.core import _prefixed_hash as _prefixed_hash
from ._agent_runtime_routes.core import _raise_runtime_error as _raise_runtime_error
from ._agent_runtime_routes.core import _repository as _repository
from ._agent_runtime_routes.core import _request_id as _request_id
from ._agent_runtime_routes.core import _require_actor as _require_actor
from ._agent_runtime_routes.core import _resolve_api_caller as _resolve_api_caller
from ._agent_runtime_routes.core import _runtime_body as _runtime_body
from ._agent_runtime_routes.core import _runtime_enabled as _runtime_enabled
from ._agent_runtime_routes.core import _session_manager as _session_manager
from ._agent_runtime_routes.core import _token_user as _token_user
from ._agent_runtime_routes.preview import (
    PreviewSessionRecoveryResponse as PreviewSessionRecoveryResponse,
)
from ._agent_runtime_routes.preview import (
    create_preview_session as create_preview_session,
)
from ._agent_runtime_routes.preview import (
    create_version_preview_session as create_version_preview_session,
)
from ._agent_runtime_routes.preview import get_preview_session as get_preview_session
from ._agent_runtime_routes.published import (
    create_published_session as create_published_session,
)
from ._agent_runtime_routes.published import published_feedback as published_feedback
from ._agent_runtime_routes.rate_limit import (
    RedisAgentChannelLimiter as RedisAgentChannelLimiter,
)
from ._agent_runtime_routes.rate_limit import _bounded_policy_int as _bounded_policy_int
from ._agent_runtime_routes.rate_limit import _enforce_channel_limits as _enforce_channel_limits
from ._agent_runtime_routes.resolution import _channel_policy as _channel_policy
from ._agent_runtime_routes.resolution import (
    _confirmation_stamp as _confirmation_stamp,
)
from ._agent_runtime_routes.resolution import (
    _effective_capabilities as _effective_capabilities,
)
from ._agent_runtime_routes.resolution import _effective_knowledge as _effective_knowledge
from ._agent_runtime_routes.resolution import (
    _public_effective_native_capabilities as _public_effective_native_capabilities,
)
from ._agent_runtime_routes.resolution import _resolved_model as _resolved_model
from ._agent_runtime_routes.resolution import (
    _runtime_knowledge_config as _runtime_knowledge_config,
)
from ._agent_runtime_routes.snapshot import (
    _UNPINNED_WARNING_CAP as _UNPINNED_WARNING_CAP,
)
from ._agent_runtime_routes.snapshot import (
    _assert_attachments_allowed as _assert_attachments_allowed,
)
from ._agent_runtime_routes.snapshot import _build_snapshot as _build_snapshot
from ._agent_runtime_routes.snapshot import (
    _unpinned_high_risk_platform as _unpinned_high_risk_platform,
)
from ._agent_runtime_routes.streaming import _assert_existing_pin as _assert_existing_pin
from ._agent_runtime_routes.streaming import _bind_session as _bind_session
from ._agent_runtime_routes.streaming import _existing_session as _existing_session
from ._agent_runtime_routes.streaming import (
    _idempotency_replay_response as _idempotency_replay_response,
)
from ._agent_runtime_routes.streaming import (
    _record_idempotent_stream as _record_idempotent_stream,
)
from ._agent_runtime_routes.streaming import _start_runtime_stream as _start_runtime_stream

router = APIRouter(tags=["Agent Studio Runtime"])


async def preview_chat_stream(
    agent_id: str,
    payload: AgentPreviewChatRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> Any:
    _require_actor(request, user)
    reject_client_agent_forgery(request)
    session_id = payload.session_id or str(uuid.uuid4())
    existing = await _existing_session(request, payload.session_id)
    if existing:
        _assert_existing_pin(
            request,
            user,
            existing,
            agent_id=agent_id,
            agent_version_id=None,
            publication_id=None,
            channel="preview",
            draft_revision=payload.draft_revision,
        )
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
        _assert_attachments_allowed(request, snapshot, payload.attachments)
        await _bind_session(
            request,
            user,
            session_id=session_id,
            snapshot=snapshot,
            draft_revision=payload.draft_revision,
        )
    except (AgentRepositoryError, AgentNotFoundError) as exc:
        _map_repository_error(request, exc)
    body = _runtime_body(
        message=payload.message,
        session_id=session_id,
        attachments=[item.model_dump(mode="python") for item in payload.attachments],
        resume_run_id=payload.resume_run_id,
        resume_approval_id=payload.resume_approval_id,
    )
    return await _start_runtime_stream(
        request,
        user,
        body=body,
        snapshot=snapshot,
        draft_revision=payload.draft_revision,
    )


async def version_preview_chat_stream(
    agent_id: str,
    agent_version_id: str,
    payload: AgentVersionPreviewChatRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> Any:
    _require_actor(request, user)
    reject_client_agent_forgery(request)
    session_id = payload.session_id or str(uuid.uuid4())
    existing = await _existing_session(request, payload.session_id)
    if existing:
        _assert_existing_pin(
            request,
            user,
            existing,
            agent_id=agent_id,
            agent_version_id=agent_version_id,
            publication_id=None,
            channel="preview",
            draft_revision=None,
        )
    try:
        resolution = await _repository(request).resolve_version_runtime(
            tenant_id=user.tenant_id,
            agent_id=agent_id,
            agent_version_id=agent_version_id,
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
        snapshot = await _build_snapshot(request, resolution, user, channel="preview")
        _assert_attachments_allowed(request, snapshot, payload.attachments)
        await _bind_session(
            request,
            user,
            session_id=session_id,
            snapshot=snapshot,
            draft_revision=None,
        )
    except (AgentRepositoryError, AgentNotFoundError) as exc:
        _map_repository_error(request, exc)
    body = _runtime_body(
        message=payload.message,
        session_id=session_id,
        attachments=[item.model_dump(mode="python") for item in payload.attachments],
        resume_run_id=payload.resume_run_id,
        resume_approval_id=payload.resume_approval_id,
    )
    return await _start_runtime_stream(
        request,
        user,
        body=body,
        snapshot=snapshot,
    )


async def published_chat_stream(
    publication_id: str,
    payload: AgentPublishedChatRequest,
    request: Request,
) -> Any:
    reject_client_agent_forgery(request)
    provisional_session_id = payload.session_id or str(uuid.uuid4())
    required_scopes = ["chat:write"]
    if payload.attachments:
        required_scopes.append("attachments:write")
    # First resolution authenticates the token. Existing-session pinning is
    # then used for a second resolution only when a rollback happened.
    resolution, user = await _resolve_api_caller(
        request,
        publication_id=publication_id,
        required_scopes=required_scopes,
    )
    existing = await _existing_session(request, payload.session_id)
    if existing:
        _assert_existing_pin(
            request,
            user,
            existing,
            agent_id=None,
            agent_version_id=None,
            publication_id=publication_id,
            channel="api",
            draft_revision=None,
        )
        if existing.agent_version_id != resolution["version"]["agent_version_id"]:
            resolution, user = await _resolve_api_caller(
                request,
                publication_id=publication_id,
                required_scopes=required_scopes,
                pinned_version_id=existing.agent_version_id,
            )
    repository = _repository(request)
    idempotency_key = request.headers.get("Idempotency-Key", "").strip()
    request_hash = runtime_sha256(payload.model_dump(mode="json"))[7:]
    session_id = provisional_session_id
    reservation: dict[str, Any] | None = None
    reservation_key: dict[str, str] | None = None
    if idempotency_key:
        if len(idempotency_key) > 255:
            _raise_runtime_error(
                request,
                422,
                "AGENT_RUNTIME_IDEMPOTENCY_KEY_INVALID",
                "Invalid Idempotency-Key",
            )
        try:
            reservation = await repository.reserve_runtime_idempotency(
                tenant_id=user.tenant_id,
                publication_id=publication_id,
                principal_id=user.user_id,
                idempotency_key=idempotency_key,
                request_hash=request_hash,
                session_id=session_id,
            )
        except AgentRepositoryError as exc:
            _map_repository_error(request, exc)
        session_id = str(reservation["session_id"])
        if not reservation.get("created"):
            return _idempotency_replay_response(request, reservation)
        reservation_key = {
            "tenant_id": user.tenant_id,
            "publication_id": publication_id,
            "principal_id": user.user_id,
            "idempotency_key": idempotency_key,
            "request_hash": request_hash,
        }
    try:
        await _enforce_channel_limits(
            request,
            publication=resolution["publication"],
            principal_id=user.user_id,
        )
        snapshot = await _build_snapshot(request, resolution, user, channel="api")
        _assert_attachments_allowed(request, snapshot, payload.attachments)
        resolved_attachments = await _resolve_runtime_attachments(
            request,
            user,
            publication_id=publication_id,
            channel="api",
            attachments=payload.attachments,
        )
        if existing and existing.agent_id != snapshot["agent_id"]:
            _raise_runtime_error(
                request,
                404,
                "AGENT_RUNTIME_SESSION_NOT_FOUND",
                "Agent runtime session not found",
            )
        await _bind_session(
            request,
            user,
            session_id=session_id,
            snapshot=snapshot,
            draft_revision=None,
        )
        body = _runtime_body(
            message=payload.message,
            session_id=session_id,
            attachments=resolved_attachments,
            resume_run_id=payload.resume_run_id,
            resume_approval_id=payload.resume_approval_id,
        )
        response = await _start_runtime_stream(
            request,
            user,
            body=body,
            snapshot=snapshot,
        )
        if reservation_key:
            if not isinstance(response, StreamingResponse) or not 200 <= response.status_code < 300:
                await repository.fail_runtime_idempotency(**reservation_key)
                return response
            return _record_idempotent_stream(
                response,
                repository=repository,
                reservation_key=reservation_key,
            )
        return response
    except BaseException:
        if reservation_key:
            with contextlib.suppress(Exception):
                await repository.fail_runtime_idempotency(**reservation_key)
        raise


# Registration preserves the pre-split route order exactly.  Handlers defined
# in the use-case modules keep their function names, so FastAPI's generated
# operation ids are identical to the pre-split decorators.
router.add_api_route(
    "/agents/{agent_id}/preview/sessions",
    create_preview_session,
    methods={"POST"},
    response_model=AgentRuntimeSessionResponse,
    status_code=201,
)
router.add_api_route(
    "/agents/{agent_id}/preview/sessions/{session_id}",
    get_preview_session,
    methods={"GET"},
    response_model=PreviewSessionRecoveryResponse,
)
router.add_api_route(
    "/agents/{agent_id}/preview/chat/stream",
    preview_chat_stream,
    methods={"POST"},
)
router.add_api_route(
    "/agents/{agent_id}/versions/{agent_version_id}/preview/sessions",
    create_version_preview_session,
    methods={"POST"},
    response_model=AgentRuntimeSessionResponse,
    status_code=201,
)
router.add_api_route(
    "/agents/{agent_id}/versions/{agent_version_id}/preview/chat/stream",
    version_preview_chat_stream,
    methods={"POST"},
)
router.add_api_route(
    "/agent-runtime/{publication_id}/sessions",
    create_published_session,
    methods={"POST"},
    response_model=AgentRuntimeSessionResponse,
    status_code=201,
)
router.add_api_route(
    "/agent-runtime/{publication_id}/attachments",
    upload_published_attachment,
    methods={"POST"},
    response_model=AgentRuntimeAttachmentUploadResponse,
    status_code=201,
)
router.add_api_route(
    "/agent-runtime/{publication_id}/chat/stream",
    published_chat_stream,
    methods={"POST"},
)
router.add_api_route(
    "/agent-runtime/{publication_id}/feedback",
    published_feedback,
    methods={"POST"},
    response_model=AgentRuntimeFeedbackResponse,
)

# ---------------------------------------------------------------------------
# Time-limited compatibility surface (ARC-01B).
#
# The imports at the top of this module double as re-exports for pre-split
# import paths.  Removal condition: delete after ARC-08 once an import-scan
# gate (`rg "from src.api.v1.agent_runtime import" / "from .agent_runtime
# import"` across src/, tests/, scripts/, apps/, sdk/) shows zero hits for
# these names outside this file.  Do not add new consumers — import from the
# real home in ``src/api/v1/_agent_runtime_routes/*``.
#
# Known consumers at split time:
#   - router: src/api/router.py (permanent, not compat)
#   - agent_public.py (src/api/v1): _assert_attachments_allowed,
#     _assert_existing_pin, _bind_session, _build_snapshot,
#     _enforce_channel_limits, _existing_session, _is_tenant_admin,
#     _map_repository_error, _raise_runtime_error, _repository, _request_id,
#     _resolve_runtime_attachments, _runtime_body, _start_runtime_stream,
#     _store_runtime_attachment
#   - agents.py (src/api/v1): function-local ``from .agent_runtime import
#     _build_snapshot`` (also the patch seam of
#     tests/api/test_agent_publish_api.py, which must keep working)
#   - tests/api/test_agent_runtime_api.py: router + module attribute reads
#     (_confirmation_stamp, _public_effective_native_capabilities)
#   - tests/api/test_agent_runtime_envelope.py: _assert_attachments_allowed,
#     _assert_existing_pin, _build_snapshot, _runtime_enabled
#   - tests/api/test_agent_runtime_cutover.py: _start_runtime_stream
#   - tests/api/test_agents_api.py: router
#   - tests/services/test_knowledge_authz.py: _repository
#   - tests/security/test_agent_channel_security.py: RedisAgentChannelLimiter
# ---------------------------------------------------------------------------
__all__ = [
    "router",
    # Handlers (home: src/api/v1/_agent_runtime_routes/* unless noted)
    "create_preview_session",
    "create_version_preview_session",
    "create_published_session",
    "upload_published_attachment",
    "published_feedback",
    "preview_chat_stream",  # defined in this facade (single-kernel gate)
    "version_preview_chat_stream",  # defined in this facade (single-kernel gate)
    "published_chat_stream",  # defined in this facade (single-kernel gate)
    # Core plumbing (home: _agent_runtime_routes/core.py)
    "RedisAgentChannelLimiter",  # home: _agent_runtime_routes/rate_limit.py
    "_bearer_token",
    "_is_tenant_admin",
    "_map_repository_error",
    "_prefixed_hash",
    "_raise_runtime_error",
    "_repository",
    "_request_id",
    "_require_actor",
    "_resolve_api_caller",
    "_runtime_body",
    "_runtime_enabled",
    "_session_manager",
    "_token_user",
    # Rate limiting (home: _agent_runtime_routes/rate_limit.py)
    "_bounded_policy_int",
    "_enforce_channel_limits",
    # Resolution (home: _agent_runtime_routes/resolution.py)
    "_channel_policy",
    "_confirmation_stamp",
    "_effective_capabilities",
    "_effective_knowledge",
    "_public_effective_native_capabilities",
    "_resolved_model",
    "_runtime_knowledge_config",
    # Snapshot (home: _agent_runtime_routes/snapshot.py)
    "_UNPINNED_WARNING_CAP",
    "_assert_attachments_allowed",
    "_build_snapshot",
    "_unpinned_high_risk_platform",
    # Attachments (home: _agent_runtime_routes/attachments.py)
    "_file_storage",
    "_resolve_runtime_attachments",
    "_store_runtime_attachment",
    # Streaming startup (home: _agent_runtime_routes/streaming.py)
    "_assert_existing_pin",
    "_bind_session",
    "_existing_session",
    "_idempotency_replay_response",
    "_record_idempotent_stream",
    "_start_runtime_stream",
]
