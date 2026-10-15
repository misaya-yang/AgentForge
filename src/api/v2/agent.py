"""Native V2 Thread/Turn/Item boundary.

V1 remains a compatibility projection.  V2 is intentionally thin: ownership,
assignment, legacy import, and durable cursor reads live in the Gateway while
the Agent Runtime remains the only Agent loop for every session.
"""

from __future__ import annotations

import json
import logging
from collections.abc import AsyncIterator
from datetime import datetime, timezone
from typing import Any
from uuid import UUID, uuid4

from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_core.exceptions import PermissionDeniedError, SessionAlreadyExistsError
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentNotFoundError,
    AgentRepositoryError,
)
from fastapi import APIRouter, Depends, HTTPException, Query, Request
from fastapi.responses import StreamingResponse
from pydantic import BaseModel, ConfigDict, Field, model_validator

from ...core.auth.user_resolver import UserContext
from ...services.agent_runtime.control.snapshot_builder import snapshot_capability_allowlist
from ...services.agent_runtime.control_plane import AgentRuntimeControlError
from ...services.agent_runtime.thread_store import (
    AgentThreadStore,
    RuntimeThread,
)
from ...services.assistant_entry.approval_preview import owner_approval_preview
from ...services.assistant_entry.launch_resolution import (
    AgentLaunchResolutionError,
    resolve_agent_launch,
)
from ...services.assistant_entry.memory_controls import effective_assistant_memory_mode
from ...services.assistant_entry.model_access import assistant_model_service
from ...services.assistant_entry.source_access import (
    ConversationEventGuard,
    conversation_sources,
    require_conversation_source_access,
    visible_dataset_names,
    visible_document_keys,
    visible_source_version_keys,
)
from ..deps import get_user_context
from ..v1._assistant_routes.attachment_refs import (
    bind_assistant_attachment_refs,
    selected_image_inputs,
)
from ..v1.agent_runtime import (
    _build_snapshot,
    _is_tenant_admin,
    _map_repository_error,
    _repository,
)

router = APIRouter(prefix="/agent", tags=["Agent Runtime V2"])
logger = logging.getLogger(__name__)


class ThreadCreateRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    session_id: str | None = Field(default=None, min_length=1, max_length=255)
    model_id: str | None = Field(default=None, min_length=1, max_length=255)
    provider_id: str | None = Field(default=None, min_length=1, max_length=255)
    agent_id: UUID | None = None
    agent_version_id: UUID | None = None
    expected_tenant_id: str | None = Field(
        default=None,
        min_length=1,
        max_length=255,
        description="Optional tenant precondition; never an authentication source.",
    )

    @model_validator(mode="after")
    def require_complete_version_target(self) -> ThreadCreateRequest:
        if (self.agent_id is None) != (self.agent_version_id is None):
            raise ValueError("agent_id and agent_version_id must be provided together")
        if self.agent_id is not None and (self.model_id is not None or self.provider_id is not None):
            raise ValueError("model_id and provider_id cannot override a fixed Agent Version")
        return self


class TurnCreateRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    message: str = Field(min_length=1, max_length=200_000)
    model_id: str | None = Field(default=None, min_length=1, max_length=255)
    provider_id: str | None = Field(default=None, min_length=1, max_length=255)
    expected_model_ref: dict[str, Any] | None = None
    reasoning_option: str | None = Field(default=None, max_length=100)
    thinking_level: str | None = Field(default=None, max_length=100)
    temperature: float | None = Field(default=None, ge=0, le=2)
    execution_profile: str = Field(default="safe", max_length=32)
    memory_mode: str = Field(default="auto", max_length=32)
    system_prompt: str | None = Field(default=None, max_length=100_000)
    os_agent_enabled: bool = False
    local_node_device_id: str | None = Field(default=None, max_length=128)
    local_node_grant_ids: list[str] = Field(default_factory=list, max_length=100)
    resume_run_id: str | None = Field(default=None, max_length=255)
    resume_approval_id: str | None = Field(default=None, max_length=255)
    max_tokens: int | None = Field(default=None, ge=1, le=1_000_000)
    kb_dataset_ids: list[str] = Field(default_factory=list, max_length=100)
    kb_mode: str = Field(default="off", max_length=20)
    kb_top_k: int = Field(default=5, ge=1, le=20)
    kb_score_threshold: float = Field(default=0.4, ge=0, le=1)
    web_search_enabled: bool = False
    web_search_max_results: int = Field(default=5, ge=1, le=20)
    file_paths: list[str] = Field(default_factory=list, max_length=20)

    @model_validator(mode="after")
    def reject_ambiguous_reasoning(self) -> TurnCreateRequest:
        if self.reasoning_option and self.thinking_level:
            raise ValueError("reasoning_option and thinking_level are mutually exclusive")
        if self.kb_mode not in {"auto", "tool", "off"}:
            raise ValueError("unsupported knowledge mode")
        return self


class InterruptRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    reason: str = Field(default="client_interrupt", min_length=1, max_length=100)


class ApprovalDecisionRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    approved: bool
    reason: str | None = Field(default=None, max_length=500)


def _store(request: Request) -> AgentThreadStore:
    database = getattr(request.app.state, "database", None)
    if database is None:
        raise HTTPException(status_code=503, detail={"code": "AGENT_RUNTIME_STORAGE_UNAVAILABLE"})
    value = getattr(request.app.state, "agent_thread_store", None)
    if value is None:
        value = AgentThreadStore(database)
        request.app.state.agent_thread_store = value
    return value


def _require_actor(user: UserContext) -> None:
    if not user.is_authenticated or not user.user_id:
        raise HTTPException(status_code=401, detail={"code": "AUTHENTICATION_REQUIRED"})
    if not user.tenant_id or user.tenant_id == "public":
        raise HTTPException(status_code=403, detail={"code": "TENANT_REQUIRED"})


def _reject_unmigrated_turn_capabilities(body: TurnCreateRequest) -> None:
    """Keep V2 fail-closed until these controls have a runtime contract.

    Mirrors ``_require_agent_runtime_request``: ``system_prompt`` becomes style
    guidance, while ``os_agent_enabled`` and the ``local_node_*`` fields are
    accepted without a turn-level binding and resolved by the capability worker.
    """

    unsupported = (
        body.execution_profile != "safe"
        or body.memory_mode not in {"auto", "strict", "off"}
        or body.resume_run_id is not None
        or body.resume_approval_id is not None
    )
    if unsupported:
        raise HTTPException(
            status_code=409,
            detail={
                "code": "AGENT_RUNTIME_CAPABILITY_NOT_MIGRATED",
                "message": "This capability is not available on the Agent Runtime yet",
            },
        )


async def _version_snapshot(
    request: Request, user: UserContext, *, agent_id: str, agent_version_id: str
) -> dict[str, Any]:
    """Recheck viewer ACL and materialize the exact saved Version for Preview."""

    try:
        resolution = await _repository(request).resolve_version_runtime(
            tenant_id=user.tenant_id,
            agent_id=agent_id,
            agent_version_id=agent_version_id,
            user_id=user.user_id,
            is_tenant_admin=_is_tenant_admin(user),
        )
    except (AgentRepositoryError, AgentNotFoundError) as exc:
        _map_repository_error(request, exc)
        raise AssertionError("unreachable") from exc
    snapshot = await _build_snapshot(request, resolution, user, channel="preview")
    expected_capabilities = sum(
        str(item.get("capability_type") or item.get("type") or "") != "knowledge"
        for item in resolution.get("capabilities") or []
        if isinstance(item, dict)
    )
    if len(snapshot.get("capabilities") or []) != expected_capabilities:
        raise HTTPException(
            status_code=409,
            detail={"code": "AGENT_RUNTIME_CAPABILITY_UNAVAILABLE"},
        )
    if (
        str(snapshot.get("agent_id") or "") != agent_id
        or str(snapshot.get("agent_version_id") or "") != agent_version_id
    ):
        raise HTTPException(status_code=409, detail={"code": "AGENT_RUNTIME_VERSION_MISMATCH"})
    return snapshot


async def _pinned_version_session(
    request: Request, user: UserContext, session_id: str, *, allow_channel_pin: bool = False,
) -> Any | None:
    manager = getattr(request.app.state, "session_manager", None)
    if manager is None:
        raise HTTPException(status_code=503, detail={"code": "SESSION_STORAGE_UNAVAILABLE"})
    session = await manager.get(session_id)
    if session is None or session.user_id != user.user_id or session.tenant_id != user.tenant_id:
        raise HTTPException(status_code=404, detail={"code": "SESSION_NOT_FOUND"})
    agent_id = getattr(session, "agent_id", None)
    agent_version_id = getattr(session, "agent_version_id", None)
    channel = getattr(session, "channel", None)
    publication_id = getattr(session, "publication_id", None)
    draft_revision = getattr(session, "agent_draft_revision", None)
    if not any((agent_id, agent_version_id, channel, publication_id, draft_revision)):
        return None
    if allow_channel_pin and agent_id and (
        (channel == "preview" and agent_version_id is None and publication_id is None
         and isinstance(draft_revision, int) and not isinstance(draft_revision, bool)
         and draft_revision > 0)
        or (channel in {"hosted", "embed", "api"} and agent_version_id
            and publication_id and draft_revision is None)
    ):
        # Channel entrypoints already bound these sessions. V2 may observe or
        # control their original turn, but may not start an unbound builtin turn.
        return session
    if (
        not agent_id
        or not agent_version_id
        or channel != "preview"
        or publication_id is not None
        or draft_revision is not None
    ):
        raise HTTPException(status_code=409, detail={"code": "AGENT_RUNTIME_PIN_INVALID"})
    return session


async def _pinned_snapshot(
    request: Request, user: UserContext, session: Any
) -> dict[str, Any]:
    if getattr(session, "agent_draft_revision", None) is not None:
        # Saving r2 cannot invalidate a parked r1 action. Recheck current Agent
        # access; the Runtime retains the original immutable turn/action hash.
        try:
            agent = await _repository(request).get_agent(
                tenant_id=user.tenant_id, agent_id=str(session.agent_id),
                user_id=user.user_id, is_tenant_admin=_is_tenant_admin(user),
            )
        except (AgentRepositoryError, AgentNotFoundError) as exc:
            _map_repository_error(request, exc)
            raise AssertionError("unreachable") from exc
        if agent.get("status") in {"archived", "deleted"}:
            raise HTTPException(status_code=409, detail={"code": "AGENT_RUNTIME_AGENT_UNAVAILABLE"})
        return {}
    if getattr(session, "publication_id", None) is not None:
        try:
            resolution = await _repository(request).resolve_publication_runtime(
                tenant_id=user.tenant_id, publication_id=str(session.publication_id),
                user_id=user.user_id, is_tenant_admin=_is_tenant_admin(user),
                pinned_version_id=str(session.agent_version_id),
            )
        except (AgentRepositoryError, AgentNotFoundError) as exc:
            _map_repository_error(request, exc)
            raise AssertionError("unreachable") from exc
        snapshot = await _build_snapshot(request, resolution, user, channel=str(session.channel))
    else:
        snapshot = await _version_snapshot(
            request,
            user,
            agent_id=str(session.agent_id),
            agent_version_id=str(session.agent_version_id),
        )
    if (
        str(session.agent_spec_hash) != str(snapshot["fingerprints"]["spec"])
        or str(session.runtime_fingerprint) != runtime_sha256(snapshot)
    ):
        raise HTTPException(status_code=409, detail={"code": "AGENT_RUNTIME_PIN_STALE"})
    return snapshot


def _reject_pinned_turn_overrides(body: TurnCreateRequest) -> None:
    if body.model_fields_set - {"message"}:
        raise HTTPException(
            status_code=422,
            detail={"code": "AGENT_RUNTIME_VERSION_OVERRIDES_FORBIDDEN"},
        )


async def _start_version_turn(
    request: Request,
    user: UserContext,
    *,
    session_id: str,
    body: TurnCreateRequest,
    snapshot: dict[str, Any],
    control: Any,
) -> Any:
    model = snapshot["model"]
    parameters = model.get("parameters") or {}
    knowledge = snapshot.get("knowledge") or {}
    retrieval = knowledge.get("retrieval") or {}
    readonly = {
        "knowledge": {
            "dataset_ids": list(knowledge.get("datasets") or []),
            "mode": str(retrieval.get("mode") or "off"),
            "top_k": int(retrieval.get("top_k") or 5),
            "score_threshold": float(retrieval.get("threshold") or 0.4),
        },
        "attachments": {"refs": []},
    }
    thinking_mode = str(parameters.get("thinking_mode") or "") or None
    max_tokens = parameters.get("max_tokens")
    temperature = parameters.get("temperature")
    memory_mode = str((snapshot.get("memory") or {}).get("mode") or "session")
    launch = await resolve_agent_launch(
        entrypoint="studio_preview",
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        session_id=session_id,
        model_id=str(model["id"]),
        model_service=assistant_model_service(request) or getattr(control, "model_service", None),
        readonly_capabilities=readonly,
        legacy_thinking_level=thinking_mode,
        max_tokens=max_tokens,
        temperature=temperature,
        memory_mode=memory_mode,
        legacy_snapshot=snapshot,
    )
    return await control.start_turn(
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        session_id=session_id,
        message=body.message,
        image_inputs=[],
        model_id=str(model["id"]),
        reasoning_option=None,
        legacy_thinking_level=thinking_mode,
        max_tokens=max_tokens,
        temperature=temperature,
        memory_mode=memory_mode,
        style_guidance=None,
        resolved_agent_launch=launch,
    )


async def _assignment(request: Request, user: UserContext, session_id: str) -> Any:
    assignments = getattr(request.app.state, "assistant_runtime_assignments", None)
    if assignments is None:
        raise HTTPException(status_code=503, detail={"code": "AGENT_RUNTIME_ASSIGNMENT_UNAVAILABLE"})
    assignment = await assignments.resolve(
        tenant_id=user.tenant_id, user_id=user.user_id, session_id=session_id
    )
    if assignment is None:
        raise HTTPException(status_code=404, detail={"code": "AGENT_RUNTIME_ASSIGNMENT_NOT_FOUND"})
    if assignment.runtime_owner != "agent_runtime":
        raise HTTPException(
            status_code=409,
            detail={"code": "AGENT_RUNTIME_NOT_ASSIGNED", "runtime_owner": assignment.runtime_owner},
        )
    return assignment


async def _bind_new_assignment(request: Request, user: UserContext, session_id: str) -> Any:
    assignments = getattr(request.app.state, "assistant_runtime_assignments", None)
    if assignments is None:
        raise HTTPException(status_code=503, detail={"code": "AGENT_RUNTIME_ASSIGNMENT_UNAVAILABLE"})
    policy = getattr(request.app.state, "assistant_runtime_assignment_policy", None)
    if policy is not None and hasattr(assignments, "bind_new_session"):
        return await assignments.bind_new_session(
            tenant_id=user.tenant_id, user_id=user.user_id,
            session_id=session_id, policy=policy,
        )
    return await assignments.bind(
        tenant_id=user.tenant_id, user_id=user.user_id, session_id=session_id,
        runtime_owner="agent_runtime",
        kernel_revision=getattr(request.app.state, "assistant_runtime_kernel_revision", None),
        assignment_reason="v2_thread_create",
    )


def _thread_payload(thread: RuntimeThread) -> dict[str, Any]:
    return {
        "schema_version": "agent-thread/v2",
        "id": thread.runtime_thread_id,
        "thread_id": thread.runtime_thread_id,
        "session_id": thread.session_id,
        "runtime": {"owner": thread.kernel_owner, "source": thread.source_kind},
        "import_status": thread.import_status,
        "last_sequence": thread.last_sequence,
    }


def _version_target_payload(snapshot: dict[str, Any]) -> dict[str, str]:
    return {
        "agent_id": str(snapshot["agent_id"]),
        "agent_version_id": str(snapshot["agent_version_id"]),
        "agent_spec_hash": str(snapshot["fingerprints"]["spec"]).removeprefix("sha256:"),
        "runtime_snapshot_hash": runtime_sha256(snapshot),
    }


def _pinned_target_payload(session: Any) -> dict[str, str]:
    return {
        "agent_id": str(session.agent_id),
        "agent_version_id": str(session.agent_version_id),
        "agent_spec_hash": str(session.agent_spec_hash).removeprefix("sha256:"),
        "runtime_snapshot_hash": str(session.runtime_fingerprint),
    }


@router.post("/threads", status_code=201)
async def create_thread(
    body: ThreadCreateRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> dict[str, Any]:
    _require_actor(user)
    if body.expected_tenant_id is not None and body.expected_tenant_id != user.tenant_id:
        raise HTTPException(
            status_code=409,
            detail={"code": "AGENT_RUNTIME_TENANT_PRECONDITION_FAILED"},
        )
    session_manager = getattr(request.app.state, "session_manager", None)
    if session_manager is None:
        raise HTTPException(status_code=503, detail={"code": "SESSION_STORAGE_UNAVAILABLE"})

    target_snapshot = (
        await _version_snapshot(
            request,
            user,
            agent_id=str(body.agent_id),
            agent_version_id=str(body.agent_version_id),
        )
        if body.agent_id is not None and body.agent_version_id is not None
        else None
    )
    session_id = body.session_id
    created_here = False
    if target_snapshot is not None:
        session_id = session_id or str(uuid4())
        existing_session = await session_manager.get(session_id)
        if existing_session and (
            existing_session.user_id != user.user_id
            or existing_session.tenant_id != user.tenant_id
        ):
            raise HTTPException(status_code=404, detail={"code": "SESSION_NOT_FOUND"})
        created_here = existing_session is None
        try:
            await session_manager.bind_agent_runtime(
                session_id=session_id,
                user_id=user.user_id,
                tenant_id=user.tenant_id,
                agent_id=str(target_snapshot["agent_id"]),
                agent_version_id=str(target_snapshot["agent_version_id"]),
                agent_draft_revision=None,
                publication_id=None,
                channel="preview",
                runtime_fingerprint=runtime_sha256(target_snapshot),
                agent_spec_hash=str(target_snapshot["fingerprints"]["spec"]),
            )
        except PermissionDeniedError as exc:
            raise HTTPException(
                status_code=409, detail={"code": "AGENT_RUNTIME_PIN_CONFLICT"}
            ) from exc
        try:
            assignment = await _bind_new_assignment(request, user, session_id)
            if assignment.runtime_owner != "agent_runtime":
                raise ValueError("Agent Runtime assignment owner mismatch")
        except Exception as exc:
            if created_here:
                await session_manager.delete(session_id)
            raise HTTPException(
                status_code=409,
                detail={"code": "AGENT_RUNTIME_ASSIGNMENT_CONFLICT"},
            ) from exc
    elif session_id:
        session = await session_manager.get(session_id)
        if session and (session.user_id != user.user_id or session.tenant_id != user.tenant_id):
            raise HTTPException(status_code=404, detail={"code": "SESSION_NOT_FOUND"})
        if session is None:
            # The Web client mints the session id before opening the stream.
            # Persist that id atomically; an existing owner is never adopted.
            try:
                session = await session_manager.create(
                    user_id=user.user_id,
                    tenant_id=user.tenant_id,
                    service_id="__builtin_assistant__",
                    session_id=session_id,
                    fail_if_exists=True,
                )
                created_here = True
            except SessionAlreadyExistsError:
                session = await session_manager.get(session_id)
                if not session or session.user_id != user.user_id or session.tenant_id != user.tenant_id:
                    raise HTTPException(status_code=404, detail={"code": "SESSION_NOT_FOUND"}) from None
        try:
            # The V1 session-create request and V2 thread-create request may
            # race. Bind even when the session already exists; otherwise an
            # unassigned session is incorrectly forced onto Python control.
            await _bind_new_assignment(request, user, session_id)
        except Exception as exc:
            if created_here:
                await session_manager.delete(session_id)
            raise HTTPException(
                status_code=409,
                detail={"code": "AGENT_RUNTIME_ASSIGNMENT_CONFLICT"},
            ) from exc
    else:
        session = await session_manager.create(
            user_id=user.user_id,
            tenant_id=user.tenant_id,
            service_id="__builtin_assistant__",
        )
        session_id = session.session_id
        created_here = True
        try:
            await _bind_new_assignment(request, user, session_id)
        except Exception:
            await session_manager.delete(session_id)
            raise
    assignments = getattr(request.app.state, "assistant_runtime_assignments", None)
    if assignments is None:
        raise HTTPException(status_code=503, detail={"code": "AGENT_RUNTIME_ASSIGNMENT_UNAVAILABLE"})
    assignment = await _assignment(request, user, session_id)
    del assignment
    store = _store(request)
    existing = await store.get_for_session(
        tenant_id=user.tenant_id, user_id=user.user_id, session_id=session_id
    )
    control = getattr(request.app.state, "agent_runtime_control", None)
    settings = getattr(request.app.state, "settings", None)
    model_id = (
        str(target_snapshot["model"]["id"])
        if target_snapshot is not None
        else body.model_id or str(getattr(settings, "default_model", "") or "").strip()
    )
    if not model_id:
        raise HTTPException(status_code=503, detail={"code": "AGENT_RUNTIME_MODEL_UNAVAILABLE"})
    if existing:
        if control is None:
            raise HTTPException(status_code=503, detail={"code": "AGENT_RUNTIME_UNAVAILABLE"})
        try:
            await control.verify_thread(
                runtime_thread_id=existing.runtime_thread_id,
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                session_id=session_id,
                model_id=model_id,
            )
        except AgentRuntimeControlError as exc:
            raise HTTPException(status_code=exc.status_code, detail={"code": exc.code}) from exc
        payload = _thread_payload(existing)
        if target_snapshot is not None:
            payload["agent_version_target"] = _version_target_payload(target_snapshot)
        return {"thread": payload}
    if control is None:
        raise HTTPException(status_code=503, detail={"code": "AGENT_RUNTIME_UNAVAILABLE"})
    try:
        thread_options = (
            {
                "capability_allowlist": snapshot_capability_allowlist(target_snapshot),
                "developer_instructions": target_snapshot["agent_spec"]["developerInstructions"],
            }
            if target_snapshot is not None
            else {}
        )
        provider_id = str(target_snapshot["model"]["provider"]) if target_snapshot is not None else body.provider_id
        if provider_id:
            thread_options["provider_id"] = provider_id
        runtime_thread = await control.ensure_thread(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=session_id,
            model_id=model_id,
            **thread_options,
        )
    except AgentRuntimeControlError as exc:
        raise HTTPException(
            status_code=exc.status_code,
            detail={"code": exc.code},
        ) from exc
    kernel_thread_id = str(runtime_thread["runtime_thread_id"])
    # Import legacy history against the Runtime-authorized root so resume
    # hydrates the real Agent ThreadStore instead of creating an orphan UUID.
    history = await session_manager.history(session_id, limit=1)
    if history:
        thread = await store.import_legacy(
            tenant_id=user.tenant_id, user_id=user.user_id, session_id=session_id,
            runtime_thread_id=kernel_thread_id,
        )
    else:
        thread = await store.ensure_native(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=session_id,
            runtime_thread_id=kernel_thread_id,
        )
    payload = _thread_payload(thread)
    if target_snapshot is not None:
        payload["agent_version_target"] = _version_target_payload(target_snapshot)
    return {"thread": payload}


async def _get_thread(request: Request, user: UserContext, thread_id: str) -> RuntimeThread:
    thread = await _store(request).get(
        tenant_id=user.tenant_id, user_id=user.user_id, runtime_thread_id=thread_id
    )
    if thread is None:
        raise HTTPException(status_code=404, detail={"code": "THREAD_NOT_FOUND"})
    await _assignment(request, user, thread.session_id)
    return thread


@router.get("/threads/{thread_id}")
async def get_thread(thread_id: str, request: Request, user: UserContext = Depends(get_user_context)) -> dict[str, Any]:
    _require_actor(user)
    thread = await _get_thread(request, user, thread_id)
    pin = await _pinned_version_session(request, user, thread.session_id, allow_channel_pin=True)
    sources = await conversation_sources(request, user, thread.session_id)
    visible = await visible_dataset_names(request, user) if sources.dataset_ids else {}
    visible_documents = await visible_document_keys(request, user, sources.document_ids)
    visible_versions = await visible_source_version_keys(request, user, sources.source_versions)
    restricted = [
        run_id for run_id, ids in sources.inherited_by_run.items()
        if not ids <= visible.keys()
        or not (sources.documents_by_run or {}).get(run_id, frozenset()) <= visible_documents
        or not (sources.versions_by_run or {}).get(run_id, frozenset()) <= visible_versions
    ]
    payload = {**_thread_payload(thread), "restricted_source_run_ids": restricted}
    if pin is not None and pin.agent_version_id and not getattr(pin, "publication_id", None):
        payload["agent_version_target"] = _pinned_target_payload(pin)
    return {"thread": payload}


@router.post("/threads/{thread_id}/turns", status_code=202)
async def create_turn(
    thread_id: str,
    body: TurnCreateRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> dict[str, Any]:
    _require_actor(user)
    _reject_unmigrated_turn_capabilities(body)
    thread = await _get_thread(request, user, thread_id)
    pin = await _pinned_version_session(request, user, thread.session_id)
    if pin is not None:
        _reject_pinned_turn_overrides(body)
    await require_conversation_source_access(request, user, thread.session_id, for_execution=True)
    control = getattr(request.app.state, "agent_runtime_control", None)
    if control is None:
        raise HTTPException(status_code=503, detail={"code": "AGENT_RUNTIME_UNAVAILABLE"})
    if pin is not None:
        try:
            snapshot = await _pinned_snapshot(request, user, pin)
            turn = await _start_version_turn(
                request,
                user,
                session_id=thread.session_id,
                body=body,
                snapshot=snapshot,
                control=control,
            )
        except AgentLaunchResolutionError as exc:
            raise HTTPException(status_code=exc.status_code, detail={"code": exc.code}) from exc
        except AgentRuntimeControlError as exc:
            raise HTTPException(status_code=exc.status_code, detail={"code": exc.code}) from exc
        return {
            "schema_version": "agent-turn/v2",
            "turn": {
                "id": turn.run_id,
                "thread_id": thread.runtime_thread_id,
                "status": "in_progress",
                "requested_reasoning_option": turn.requested_reasoning_option,
                "effective_reasoning_option": turn.effective_reasoning_option,
                "events_url": f"/api/v2/agent/threads/{thread.runtime_thread_id}/events?after_sequence={turn.after_sequence}&turn_id={turn.run_id}",
            },
        }
    settings = getattr(request.app.state, "settings", None)
    model_id = body.model_id or str(getattr(settings, "default_model", "") or "").strip()
    if not model_id:
        raise HTTPException(status_code=503, detail={"code": "AGENT_RUNTIME_MODEL_UNAVAILABLE"})
    try:
        memory_mode = await effective_assistant_memory_mode(
            request, user.tenant_id, user.user_id, body.memory_mode,
        )
        style_guidance = str(body.system_prompt or "").strip() or None
        bound_refs = await bind_assistant_attachment_refs(
            request, user, session_id=thread.session_id, model_id=model_id, refs=body.file_paths,
            provider_id=body.provider_id,
        )
        image_inputs = await selected_image_inputs(user, session_id=thread.session_id, refs=bound_refs) if bound_refs else []
        readonly = {
            "knowledge": {
                "dataset_ids": body.kb_dataset_ids,
                "mode": body.kb_mode,
                "top_k": body.kb_top_k,
                "score_threshold": body.kb_score_threshold,
            },
            "attachments": {"refs": bound_refs},
            "web_search": {
                "enabled": body.web_search_enabled,
                "max_results": body.web_search_max_results,
            },
        }
        launch = await resolve_agent_launch(
            entrypoint="assistant",
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=thread.session_id,
            model_id=model_id,
            provider_id=body.provider_id,
            expected_model_ref=body.expected_model_ref,
            model_service=(
                assistant_model_service(request)
                or getattr(control, "model_service", None)
            ),
            readonly_capabilities=readonly,
            reasoning_option=body.reasoning_option,
            legacy_thinking_level=body.thinking_level,
            max_tokens=body.max_tokens,
            temperature=body.temperature,
            style_guidance=style_guidance,
            memory_mode=memory_mode,
            memory_profile="basic",
        )
        turn = await control.start_turn(
            tenant_id=user.tenant_id, user_id=user.user_id, session_id=thread.session_id,
            message=body.message, image_inputs=image_inputs, model_id=model_id,
            reasoning_option=body.reasoning_option,
            legacy_thinking_level=body.thinking_level,
            max_tokens=body.max_tokens,
            temperature=body.temperature,
            memory_mode=memory_mode,
            style_guidance=style_guidance,
            resolved_agent_launch=launch,
        )
    except Exception as exc:
        if isinstance(exc, HTTPException):
            detail = exc.detail if isinstance(exc.detail, dict) else {}
            logger.warning(
                "Agent Runtime turn request rejected code=%s status=%s",
                detail.get("code", "HTTP_ERROR"),
                exc.status_code,
            )
            raise
        if isinstance(exc, AgentLaunchResolutionError):
            raise HTTPException(
                status_code=exc.status_code,
                detail={"code": exc.code},
            ) from exc
        if hasattr(exc, "code"):
            logger.warning(
                "Agent Runtime turn rejected code=%s status=%s",
                getattr(exc, "code", "AGENT_RUNTIME_ERROR"),
                int(getattr(exc, "status_code", 503)),
            )
            raise HTTPException(status_code=int(getattr(exc, "status_code", 503)), detail={"code": exc.code}) from exc
        raise
    return {
        "schema_version": "agent-turn/v2",
        "turn": {
            "id": turn.run_id,
            "thread_id": thread.runtime_thread_id,
            "status": "in_progress",
            "requested_reasoning_option": turn.requested_reasoning_option,
            "effective_reasoning_option": turn.effective_reasoning_option,
            "events_url": f"/api/v2/agent/threads/{thread.runtime_thread_id}/events?after_sequence={turn.after_sequence}&turn_id={turn.run_id}",
        },
    }


@router.post("/threads/{thread_id}/turns/{turn_id}:interrupt")
async def interrupt_turn(
    thread_id: str,
    turn_id: str,
    body: InterruptRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> dict[str, Any]:
    _require_actor(user)
    thread = await _get_thread(request, user, thread_id)
    control = getattr(request.app.state, "agent_runtime_control", None)
    interrupt = getattr(control, "interrupt_turn", None)
    if interrupt is None:
        raise HTTPException(status_code=501, detail={"code": "AGENT_RUNTIME_INTERRUPT_UNAVAILABLE"})
    try:
        await interrupt(
            runtime_thread_id=thread.runtime_thread_id,
            turn_id=turn_id,
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=thread.session_id,
            reason=body.reason,
        )
    except AgentRuntimeControlError as exc:
        raise HTTPException(
            status_code=exc.status_code,
            detail={"code": exc.code},
        ) from exc
    return {"schema_version": "agent-turn/v2", "turn_id": turn_id, "status": "interrupt_requested"}


@router.post("/threads/{thread_id}/turns/{turn_id}:recover")
async def recover_turn(thread_id: str, turn_id: str, request: Request, user: UserContext = Depends(get_user_context)) -> dict[str, Any]:
    _require_actor(user)
    thread = await _get_thread(request, user, thread_id)
    pin = await _pinned_version_session(request, user, thread.session_id, allow_channel_pin=True)
    if pin is not None:
        await _pinned_snapshot(request, user, pin)
    await require_conversation_source_access(request, user, thread.session_id, for_execution=True)
    control = getattr(request.app.state, "agent_runtime_control", None)
    recover = getattr(control, "recover_turn", None)
    if recover is None:
        raise HTTPException(status_code=501, detail={"code": "AGENT_RUNTIME_RECOVERY_UNAVAILABLE"})
    try:
        result = await recover(runtime_thread_id=thread.runtime_thread_id, turn_id=turn_id, tenant_id=user.tenant_id, user_id=user.user_id, session_id=thread.session_id)
    except AgentRuntimeControlError as exc:
        raise HTTPException(status_code=exc.status_code, detail={"code": exc.code}) from exc
    return {"schema_version": "agent-turn/v2", "turn_id": turn_id, "status": result.get("status", "recovery_requested")}


@router.get("/threads/{thread_id}/approvals/{approval_id}")
async def get_thread_approval(
    thread_id: str,
    approval_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> dict[str, Any]:
    """Read a pending approval through the owning Agent Runtime.

    The thread lookup is deliberately performed before forwarding the request;
    this keeps approval IDs from becoming a cross-tenant oracle and binds the
    Runtime scope to the session that owns the thread.
    """
    _require_actor(user)
    thread = await _get_thread(request, user, thread_id)
    source_revoked = False
    pin = await _pinned_version_session(request, user, thread.session_id, allow_channel_pin=True)
    if pin is not None:
        try:
            await _pinned_snapshot(request, user, pin)
        except HTTPException:
            source_revoked = True
    try:
        await require_conversation_source_access(request, user, thread.session_id)
    except HTTPException as exc:
        if not isinstance(exc.detail, dict) or exc.detail.get("code") != "ASSISTANT_SOURCE_ACCESS_REVOKED":
            raise
        source_revoked = True
    control = getattr(request.app.state, "agent_runtime_control", None)
    if control is None:
        raise HTTPException(status_code=503, detail={"code": "AGENT_RUNTIME_UNAVAILABLE"})
    try:
        approval = await control.get_approval(
            approval_id=approval_id,
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=thread.session_id,
        )
    except AgentRuntimeControlError as exc:
        raise HTTPException(status_code=exc.status_code, detail={"code": exc.code}) from exc
    if approval is None:
        raise HTTPException(status_code=404, detail={"code": "APPROVAL_NOT_FOUND"})
    if source_revoked:
        # A revoked source must not expose action arguments, but its owner can
        # still reject the parked action through the normal approval card.
        safe_approval = {key: approval[key] for key in (
            "approval_id", "status", "tool_name", "expires_at", "run_id",
        ) if key in approval}
        return {
            "schema_version": "agent-approval/v2",
            "approval": safe_approval,
            "preview": {"can_approve": False, "reason": "Knowledge source access is unavailable. Reject this action or stop the task."},
        }
    database = getattr(request.app.state, "database", None)
    preview = await owner_approval_preview(
        database,
        approval_id=approval_id,
        runtime_thread_id=thread.runtime_thread_id,
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        session_id=thread.session_id,
        runtime_summary=approval,
    ) if database is not None else {"can_approve": False, "reason": "Action details unavailable"}
    return {"schema_version": "agent-approval/v2", "approval": approval, "preview": preview}


@router.post("/threads/{thread_id}/approvals/{approval_id}/decision")
async def decide_thread_approval(
    thread_id: str,
    approval_id: str,
    body: ApprovalDecisionRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> dict[str, Any]:
    """Consume one Runtime approval decision, preserving tenant/thread scope."""
    _require_actor(user)
    thread = await _get_thread(request, user, thread_id)
    control = getattr(request.app.state, "agent_runtime_control", None)
    if control is None:
        raise HTTPException(status_code=503, detail={"code": "AGENT_RUNTIME_UNAVAILABLE"})
    try:
        if body.approved:
            pin = await _pinned_version_session(request, user, thread.session_id, allow_channel_pin=True)
            if pin is not None:
                await _pinned_snapshot(request, user, pin)
            await require_conversation_source_access(request, user, thread.session_id, for_execution=True)
            approval = await control.get_approval(
                approval_id=approval_id,
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                session_id=thread.session_id,
            )
            if approval is None:
                raise HTTPException(status_code=404, detail={"code": "APPROVAL_NOT_FOUND"})
            database = getattr(request.app.state, "database", None)
            preview = await owner_approval_preview(
                database,
                approval_id=approval_id,
                runtime_thread_id=thread.runtime_thread_id,
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                session_id=thread.session_id,
                runtime_summary=approval,
            ) if database is not None else {"can_approve": False}
            if not preview["can_approve"]:
                raise HTTPException(status_code=409, detail={"code": "APPROVAL_ACTION_UNVERIFIED"})
        result = await control.decide_approval(
            approval_id=approval_id,
            approved=body.approved,
            reason=body.reason,
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=thread.session_id,
        )
    except AgentRuntimeControlError as exc:
        raise HTTPException(status_code=exc.status_code, detail={"code": exc.code}) from exc
    return {
        "schema_version": "agent-approval/v2",
        "approval": {
            **result,
            "approval_id": result.get("approval_id", approval_id),
            "status": result.get("status", "approved" if body.approved else "rejected"),
            "approved": body.approved,
            "reason": body.reason,
        },
    }


@router.get("/threads/{thread_id}/events")
async def thread_events(
    thread_id: str,
    request: Request,
    after_sequence: int = Query(default=0, ge=0),
    limit: int = Query(default=100, ge=1, le=1000),
    turn_id: UUID | None = Query(default=None),
    user: UserContext = Depends(get_user_context),
) -> StreamingResponse:
    _require_actor(user)
    thread = await _get_thread(request, user, thread_id)
    source_guard = ConversationEventGuard(request, user, await conversation_sources(request, user, thread.session_id), session_id=thread.session_id)

    control = getattr(request.app.state, "agent_runtime_control", None)
    if control is None:
        raise HTTPException(status_code=503, detail={"code": "AGENT_RUNTIME_UNAVAILABLE"})
    turn_id_value = str(turn_id) if turn_id else None
    turn_metadata = (
        await _store(request).turn_metadata(
            tenant_id=user.tenant_id, user_id=user.user_id,
            session_id=thread.session_id, runtime_thread_id=thread.runtime_thread_id,
            turn_id=turn_id_value,
        )
        if turn_id_value
        else None
    )

    async def stream() -> AsyncIterator[bytes]:
        async for raw in control.stream_thread_events(
            runtime_thread_id=thread.runtime_thread_id,
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=thread.session_id,
            after_sequence=after_sequence,
            limit=limit,
            turn_id=turn_id_value,
        ):
            raw = await source_guard.project(raw)
            payload = raw.get("data") if isinstance(raw.get("data"), dict) else {}
            if raw.get("event_type") == "run_started" and turn_metadata and payload.get("source_access_revoked") is not True:
                payload = {**payload, **turn_metadata}
                raw = {**raw, "data": payload}
            sequence = int(raw.get("sequence") or after_sequence + 1)
            timestamp = raw.get("timestamp") or datetime.now(timezone.utc).isoformat()
            if isinstance(timestamp, (int, float)):
                timestamp = datetime.fromtimestamp(timestamp, timezone.utc).isoformat()
            event = {
                "schema_version": "agent-event/v2",
                "thread_id": thread.runtime_thread_id,
                "sequence": sequence,
                "event": {
                    "id": str(raw.get("event_id") or f"runtime:{sequence}"),
                    "key": str(raw.get("event_key") or f"runtime:{sequence}"),
                    "type": str(raw.get("event_type") or "item"),
                    "item_id": payload.get("item_id"),
                    "turn_id": payload.get("run_id") or turn_id_value,
                    "status": payload.get("status"),
                    "payload": raw,
                },
                "timestamp": str(timestamp),
            }
            yield f"id: {sequence}\nevent: item\ndata: {json.dumps(event, ensure_ascii=False, separators=(',', ':'))}\n\n".encode()

    return StreamingResponse(
        stream(), media_type="text/event-stream",
        headers={"cache-control": "no-cache", "x-accel-buffering": "no"},
    )


__all__ = ["router"]
