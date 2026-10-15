"""Thread and turn lifecycle: ensure/resume/verify, fingerprint CAS, turn issue.

ARC-02 split of ``control_plane.py``.  These functions take the control plane
as their first argument and are bound onto ``AgentRuntimeControlPlane`` by the
facade; ``start_turn`` stays the single orchestrator that resolves the Agent
spec, pins the snapshot, signs the model lease, and starts the Runtime turn.
"""

from __future__ import annotations

import asyncio
import logging
import uuid
from contextlib import asynccontextmanager
from typing import TYPE_CHECKING, Any
from urllib.parse import quote

from ai_gateway_contracts.agent_launch import ResolvedAgentLaunchV1

from .http_headers import runtime_headers
from .tool_policy import runtime_tool_policy
from .types import (
    BASE_AGENT_INSTRUCTIONS_V1,
    GENERIC_AGENT_INSTRUCTIONS_V1,
    AgentRuntimeControlError,
    AgentTurn,
)

if TYPE_CHECKING:
    from ..control_plane import AgentRuntimeControlPlane

logger = logging.getLogger(__name__)


@asynccontextmanager
async def _thread_creation_lock(
    plane: AgentRuntimeControlPlane,
    key: tuple[str, str, str],
):
    """Serialize one session and retire the lock after its last user exits."""

    lock = plane._thread_locks.setdefault(key, asyncio.Lock())
    plane._thread_lock_users[lock] = plane._thread_lock_users.get(lock, 0) + 1
    try:
        async with lock:
            yield
    finally:
        remaining = plane._thread_lock_users.get(lock, 1) - 1
        if remaining > 0:
            plane._thread_lock_users[lock] = remaining
        else:
            plane._thread_lock_users.pop(lock, None)
            if plane._thread_locks.get(key) is lock:
                plane._thread_locks.pop(key, None)


async def cleanup_session(
    plane: AgentRuntimeControlPlane,
    *,
    tenant_id: str,
    user_id: str,
    session_id: str,
) -> bool:
    """Tombstone one Runtime-owned session without mutating its item log."""

    if (
        not session_id
        or len(session_id) > 255
        or any(ord(character) < 32 for character in session_id)
    ):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_SESSION_ID_INVALID", status_code=400
        )
    response = await plane.http_client.post(
        f"{plane.runtime_url}/internal/v1/sessions/{quote(session_id, safe='')}/cleanup",
        headers=runtime_headers(
            plane,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
        ),
        json={},
    )
    if response.status_code == 404:
        return False
    if response.status_code >= 400:
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_SESSION_CLEANUP_FAILED",
            status_code=503 if response.status_code >= 500 else 409,
        )
    payload = response.json()
    if not isinstance(payload, dict) or payload.get("session_id") != session_id:
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_SESSION_CLEANUP_INVALID", status_code=503
        )
    # Runtime returns 200/not_found for an absent or already-tombstoned thread.
    # Do not treat arbitrary successful payloads as idempotent absence.
    status = payload.get("status")
    if status == "deleted":
        return True
    if status == "not_found":
        return False
    raise AgentRuntimeControlError(
        "AI_PLATFORM_AGENT_RUNTIME_SESSION_CLEANUP_INVALID", status_code=503
    )


async def assignment(plane: AgentRuntimeControlPlane, tenant_id: str, user_id: str, session_id: str):
    resolved = await plane.assignment_store.resolve(
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
    )
    if resolved is None or resolved.runtime_owner != "agent_runtime":
        raise AgentRuntimeControlError("AGENT_RUNTIME_ASSIGNMENT_MISMATCH", status_code=403)
    return resolved


async def existing_thread(
    plane: AgentRuntimeControlPlane,
    tenant_id: str,
    user_id: str,
    session_id: str,
) -> dict[str, Any] | None:
    row = await plane.database.fetchrow(
        """
        SELECT runtime_thread_id, last_sequence, dynamic_tool_fingerprint
          FROM assistant_runtime_threads
         WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
           AND deleted_at IS NULL
        """,
        tenant_id,
        user_id,
        session_id,
    )
    return dict(row) if row else None


def assert_dynamic_tool_fingerprint(
    existing: dict[str, Any], requested_fingerprint: str
) -> None:
    stored_fingerprint = str(existing.get("dynamic_tool_fingerprint") or "")
    if stored_fingerprint != requested_fingerprint:
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_THREAD_RECREATE_REQUIRED", status_code=409
        )


async def bind_dynamic_tool_fingerprint(
    plane: AgentRuntimeControlPlane,
    *,
    existing: dict[str, Any],
    fingerprint: str,
    tenant_id: str,
    user_id: str,
    session_id: str,
    created_by_this_request: bool = False,
) -> dict[str, Any]:
    """Persist a newly created Thread fingerprint with a null-only CAS.

    Runtime thread creation and Gateway mapping are separate writes. A
    concurrent creator can briefly expose a mapped thread before its
    fingerprint is written, so readers wait for the creator. An older
    unbound Thread is never adopted because its kernel-side tool catalog
    cannot be proven equal to the current catalog.
    """
    stored_fingerprint = str(existing.get("dynamic_tool_fingerprint") or "")
    if stored_fingerprint:
        plane._assert_dynamic_tool_fingerprint(existing, fingerprint)
    if stored_fingerprint:
        return existing
    if not created_by_this_request:
        for _ in range(5):
            await asyncio.sleep(0.05)
            refreshed = await plane._existing_thread(tenant_id, user_id, session_id)
            if refreshed is not None and refreshed.get("dynamic_tool_fingerprint"):
                plane._assert_dynamic_tool_fingerprint(refreshed, fingerprint)
                return refreshed
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_THREAD_RECREATE_REQUIRED", status_code=409
        )
    last_error: Exception | None = None
    for attempt in range(3):
        try:
            result = await plane.database.execute(
                """
                UPDATE assistant_runtime_threads
                   SET dynamic_tool_fingerprint = $1, updated_at = NOW()
                 WHERE runtime_thread_id = $2 AND tenant_id = $3
                   AND user_id = $4 AND session_id = $5
                   AND deleted_at IS NULL
                   AND dynamic_tool_fingerprint IS NULL
                """,
                fingerprint,
                uuid.UUID(str(existing["runtime_thread_id"])),
                tenant_id,
                user_id,
                session_id,
            )
            if str(result).endswith(" 1"):
                return {
                    **existing,
                    "dynamic_tool_fingerprint": fingerprint,
                }
        except Exception as exc:  # retry a transient mapping write
            last_error = exc
        refreshed = await plane._existing_thread(tenant_id, user_id, session_id)
        if refreshed is not None:
            plane._assert_dynamic_tool_fingerprint(refreshed, fingerprint)
            if refreshed.get("dynamic_tool_fingerprint"):
                return refreshed
        if attempt < 2:
            await asyncio.sleep(0.05)
    error = AgentRuntimeControlError(
        "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_BIND_FAILED", status_code=503
    )
    if last_error is not None:
        raise error from last_error
    raise error


async def ensure_thread(
    plane: AgentRuntimeControlPlane,
    *,
    tenant_id: str,
    user_id: str,
    session_id: str,
    model_id: str,
    provider_id: str | None = None,
    readonly_capabilities: dict[str, Any] | None = None,
    capability_allowlist: list[dict[str, Any]] | None = None,
    native_web_search_enabled: bool = False,
    developer_instructions: str | None = None,
) -> dict[str, Any]:
    if developer_instructions is not None and not isinstance(developer_instructions, str):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_AGENT_INSTRUCTIONS_INVALID", status_code=409
        )
    if developer_instructions is not None and not developer_instructions.strip():
        developer_instructions = GENERIC_AGENT_INSTRUCTIONS_V1
    if developer_instructions is not None and len(developer_instructions.encode("utf-8")) > 256 * 1024:
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_AGENT_INSTRUCTIONS_INVALID", status_code=409
        )
    if readonly_capabilities is None:
        model = await plane.model_service.get_model(
            tenant_id, model_id, **({"provider_id": provider_id} if provider_id else {})
        )
        if not model or not bool(model.get("is_enabled", True)):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_MODEL_NOT_FOUND", status_code=400
            )
        capability_revision = int(model.get("capability_revision") or 1)
        readonly_capabilities = plane._readonly_capability_payload(
            None,
            tenant_id=tenant_id,
            capability_revision=capability_revision,
        )
        await plane._fetch_capability_catalog(
            readonly_capabilities,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            model_id=model_id,
            capability_revision=capability_revision,
            capability_allowlist=capability_allowlist,
        )
    generic_product = capability_allowlist is None and readonly_capabilities.get("responses_tool_names") is None
    selected_capabilities = readonly_capabilities
    thread_capabilities = readonly_capabilities.pop("_thread_capabilities", None)
    if isinstance(thread_capabilities, dict):
        readonly_capabilities = thread_capabilities
    existing = await plane._existing_thread(tenant_id, user_id, session_id)
    if existing:
        fingerprint = plane._dynamic_tool_fingerprint(readonly_capabilities or {})
        return await plane._bind_dynamic_tool_fingerprint(
            existing=existing,
            fingerprint=fingerprint,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
        )
    key = (tenant_id, user_id, session_id)
    async with _thread_creation_lock(plane, key):
        existing = await plane._existing_thread(tenant_id, user_id, session_id)
        if existing:
            fingerprint = plane._dynamic_tool_fingerprint(readonly_capabilities or {})
            return await plane._bind_dynamic_tool_fingerprint(
                existing=existing,
                fingerprint=fingerprint,
                tenant_id=tenant_id,
                user_id=user_id,
                session_id=session_id,
                created_by_this_request=True,
            )
        start = {
            "model": model_id,
            "modelProvider": "ai-platform-gateway",
            "cwd": "/workspace",
            # Agent emits approval requests for write-capable built-ins;
            # the Runtime broker persists and scope-binds those requests
            # before any handler is allowed to dispatch.
            "approvalPolicy": "on-request",
            "sandbox": "read-only",
            "config": plane._runtime_model_config(
                model_id,
                native_web_search_enabled=native_web_search_enabled,
            ),
            "dynamicTools": plane._dynamic_tools(readonly_capabilities or {}),
        }
        if developer_instructions is not None:
            # A loaded kernel Thread can ignore later resume instruction
            # overrides. Bind the immutable Version instructions at creation.
            start["baseInstructions"] = BASE_AGENT_INSTRUCTIONS_V1
            start["developerInstructions"] = developer_instructions
        response = await plane.http_client.post(
            f"{plane.runtime_url}/internal/v1/threads",
            headers=runtime_headers(
                plane,
                tenant_id=tenant_id,
                user_id=user_id,
                session_id=session_id,
            ),
            json={
                "tenantId": tenant_id,
                "userId": user_id,
                "sessionId": session_id,
                "start": start,
                "toolPolicy": runtime_tool_policy(
                    readonly_capabilities if generic_product else selected_capabilities, startup=True,
                    native_search_allowed=(native_web_search_enabled or generic_product),
                    generic_product=generic_product,
                ),
            },
        )
        if response.status_code >= 400:
            # A concurrent process may have won the unique session scope.
            existing = await plane._existing_thread(tenant_id, user_id, session_id)
            if existing:
                return await plane._bind_dynamic_tool_fingerprint(
                    existing=existing,
                    fingerprint=plane._dynamic_tool_fingerprint(readonly_capabilities or {}),
                    tenant_id=tenant_id,
                    user_id=user_id,
                    session_id=session_id,
                )
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_THREAD_CREATE_FAILED",
                status_code=503,
            )
        payload = response.json()
        thread = payload.get("thread") if isinstance(payload, dict) else None
        thread_id = str((thread or {}).get("id") or "")
        if not thread_id:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_THREAD_CREATE_INVALID",
                status_code=503,
            )
        fingerprint = plane._dynamic_tool_fingerprint(readonly_capabilities or {})
        await plane._bind_dynamic_tool_fingerprint(
            existing={
                "runtime_thread_id": uuid.UUID(thread_id),
                "dynamic_tool_fingerprint": None,
            },
            fingerprint=fingerprint,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            created_by_this_request=True,
        )
        return {
            "runtime_thread_id": uuid.UUID(thread_id),
            "last_sequence": 0,
            "dynamic_tool_fingerprint": fingerprint,
        }


async def resume_thread(
    plane: AgentRuntimeControlPlane,
    *,
    runtime_thread_id: uuid.UUID,
    tenant_id: str,
    user_id: str,
    session_id: str,
    model_id: str | None,
    base_instructions: str | None = BASE_AGENT_INSTRUCTIONS_V1,
    developer_instructions: str | None = None,
    model_context_window: int | None = None,
    auto_compact_token_limit: int | None = None,
    native_web_search_enabled: bool | None = False,
    tool_policy: dict[str, Any] | None = None,
) -> None:
    if developer_instructions is not None and not developer_instructions.strip():
        developer_instructions = GENERIC_AGENT_INSTRUCTIONS_V1
    response = await plane.http_client.post(
        f"{plane.runtime_url}/internal/v1/threads/{runtime_thread_id}/resume",
        headers=runtime_headers(
            plane,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
        ),
        json={
            "model": model_id,
            "modelPlaneBaseUrl": plane.model_plane_base_url,
            "baseInstructions": base_instructions,
            "developerInstructions": developer_instructions,
            "modelContextWindow": model_context_window,
            "autoCompactTokenLimit": auto_compact_token_limit,
            **({"nativeWebSearchEnabled": native_web_search_enabled}
               if native_web_search_enabled is not None else {}),
            **({"toolPolicy": tool_policy} if tool_policy is not None else {}),
        } if model_id is not None else {},
    )
    if response.status_code >= 400:
        try:
            runtime_code = response.json().get("error")
        except (ValueError, AttributeError):
            runtime_code = None
        code = (
            "AI_PLATFORM_AGENT_RUNTIME_TURN_RECOVERING"
            if runtime_code == "runtime_turn_recovering" else
            "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_THREAD_RECREATE_REQUIRED"
            if response.status_code == 409 else "AI_PLATFORM_AGENT_RUNTIME_THREAD_RESUME_FAILED"
        )
        raise AgentRuntimeControlError(code, status_code=409 if response.status_code == 409 else 503)


async def verify_thread(
    plane: AgentRuntimeControlPlane,
    *,
    runtime_thread_id: str,
    tenant_id: str,
    user_id: str,
    session_id: str,
    model_id: str,
) -> None:
    """Verify that the durable Gateway identity is backed by a live kernel thread."""
    # Cold verification must explicitly reconstruct the private provider route.
    # A configless resume can load upstream OpenAI defaults before the real turn
    # can override them. Preserve stored instructions and tool/search authority.
    await plane._resume_thread(
        runtime_thread_id=uuid.UUID(str(runtime_thread_id)),
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
        model_id=model_id,
        base_instructions=None,
        native_web_search_enabled=None,
    )


async def recover_turn(
    plane: AgentRuntimeControlPlane,
    *,
    runtime_thread_id: str,
    turn_id: str,
    tenant_id: str,
    user_id: str,
    session_id: str,
) -> dict[str, Any]:
    """Wake original Rust ownership reconciliation without issuing new authority."""
    response = await plane.http_client.post(
        f"{plane.runtime_url}/internal/v1/threads/{runtime_thread_id}/turns/{turn_id}/recover",
        headers=runtime_headers(plane, tenant_id=tenant_id, user_id=user_id, session_id=session_id),
        json={},
    )
    if response.status_code >= 400:
        raise AgentRuntimeControlError("AI_PLATFORM_AGENT_RUNTIME_RECOVERY_FAILED", status_code=503 if response.status_code >= 500 else response.status_code)
    return response.json()


async def interrupt_turn(
    plane: AgentRuntimeControlPlane,
    *,
    runtime_thread_id: str,
    turn_id: str,
    tenant_id: str,
    user_id: str,
    session_id: str,
    reason: str = "client_interrupt",
) -> None:
    """Request a native kernel interrupt without switching runtimes.

    The Runtime owns the active turn state.  Gateway only authenticates
    the scope and forwards the request; it never marks a turn terminal on
    a failed dispatch, which preserves the one-call/one-result contract.
    """
    del reason  # The strict Agent turn/interrupt wire body is intentionally empty.
    response = await plane.http_client.post(
        f"{plane.runtime_url}/internal/v1/threads/{runtime_thread_id}/turns/{turn_id}/interrupt",
        headers=runtime_headers(
            plane,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            run_id=turn_id,
            turn_id=turn_id,
        ),
        # Agent App Server's typed turn/interrupt request has an empty
        # body and only acknowledges after TurnAborted is emitted. The
        # public reason remains Gateway audit context; forwarding it would
        # make the strict Runtime decoder reject the interrupt.
        json={},
    )
    if response.status_code >= 400:
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_INTERRUPT_FAILED",
            status_code=503 if response.status_code >= 500 else 409,
        )
    await plane._complete_run(uuid.UUID(turn_id), "cancelled")


async def start_turn(
    plane: AgentRuntimeControlPlane,
    *,
    tenant_id: str,
    user_id: str,
    session_id: str,
    message: str,
    model_id: str,
    reasoning_option: str | None,
    legacy_thinking_level: str | None,
    max_tokens: int | None,
    temperature: float | None = None,
    readonly_capabilities: dict[str, Any] | None = None,
    resolved_agent_snapshot: dict[str, Any] | None = None,
    resolved_agent_launch: ResolvedAgentLaunchV1 | None = None,
    developer_instructions: str | None = None,
    style_guidance: str | None = None,
    memory_mode: str = "auto",
    memory_profile: str | None = None,
    enable_dynamic_tools: bool = True,
) -> AgentTurn:
    from .turn_start import start_turn as start_turn_impl

    return await start_turn_impl(
        plane,
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
        message=message,
        model_id=model_id,
        reasoning_option=reasoning_option,
        legacy_thinking_level=legacy_thinking_level,
        max_tokens=max_tokens,
        temperature=temperature,
        readonly_capabilities=readonly_capabilities,
        resolved_agent_snapshot=resolved_agent_snapshot,
        resolved_agent_launch=resolved_agent_launch,
        developer_instructions=developer_instructions,
        style_guidance=style_guidance,
        memory_mode=memory_mode,
        memory_profile=memory_profile,
        enable_dynamic_tools=enable_dynamic_tools,
    )


__all__ = [
    "assert_dynamic_tool_fingerprint",
    "assignment",
    "bind_dynamic_tool_fingerprint",
    "cleanup_session",
    "ensure_thread",
    "existing_thread",
    "interrupt_turn",
    "resume_thread",
    "start_turn",
    "verify_thread",
]
