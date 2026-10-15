"""Assistant chat routes: Gateway edge checks + Agent Runtime control plane.

ARC-01 split of ``src/api/v1/assistant.py``.  These routes perform only the
Gateway-owned edge work — authentication, anti-forgery, rate limiting, model
authorization, session binding — and then hand the turn to
``AgentRuntimeControlPlane``.  Model routing, tool execution and the run loop
live in the Rust Runtime; no model/tool loop may ever be reintroduced here.
"""

from __future__ import annotations

import json
import logging
import time
import uuid
from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import StreamingResponse
from pydantic import ValidationError

from ....core.auth.user_resolver import UserContext
from ....services.agent_runtime import AgentRuntimeControlError
from ....services.assistant_entry.chat_citations import ChatCitations
from ....services.assistant_entry.launch_resolution import (
    AgentLaunchResolutionError,
    resolve_agent_launch,
)
from ....services.assistant_entry.memory_controls import effective_assistant_memory_mode
from ....services.assistant_entry.model_access import (
    assistant_model_service,
    check_model_permission,
    effective_chat_model_id,
)
from ....services.assistant_entry.run_queries import agent_runtime_control
from ....services.assistant_entry.session_binding import (
    ensure_agent_runtime_session,
    validate_chat_session_access,
)
from ....services.assistant_entry.source_access import (
    guarded_conversation_frames,
    require_conversation_source_access,
)
from ...deps import get_user_context
from ...schemas.assistant import AssistantChatRequest, AssistantChatResponse
from .._agent_runtime_headers import reject_client_agent_forgery

router = APIRouter()
logger = logging.getLogger(__name__)


def _require_agent_runtime_request(body: AssistantChatRequest) -> None:
    """Fail closed on controls the Runtime has no contract for.

    ``system_prompt`` is forwarded as style guidance. ``os_agent_enabled`` and
    the ``local_node_*`` fields are accepted but carry no turn-level binding:
    the worker resolves the device from the tenant grant or the tool arguments,
    so a Local Node request is answered by the capability, not by this edge.
    """

    unsupported = bool(
        body.execution_profile != "safe"
        or body.enable_task_planning
        or body.confirm_plan
        or body.resume_run_id
        or body.resume_approval_id
    )
    if unsupported:
        raise HTTPException(
            status_code=409,
            detail={
                "code": "AGENT_RUNTIME_CAPABILITY_NOT_MIGRATED",
                "message": "This capability is not available on the Agent Runtime yet",
            },
        )


def _agent_runtime_readonly_capabilities(
    body: AssistantChatRequest, *, attachment_refs: list[str] | None = None
) -> dict[str, Any]:
    """Build explicit read-only references for the Agent Runtime boundary."""

    return {
        "knowledge": {
            "dataset_ids": list(body.kb_dataset_ids),
            "mode": body.kb_mode,
            "top_k": body.kb_top_k,
            "score_threshold": body.kb_score_threshold,
        },
        "attachments": {"refs": list(attachment_refs if attachment_refs is not None else body.file_paths)},
        "web_search": {
            "enabled": body.web_search_enabled,
            "max_results": body.web_search_max_results,
        },
    }


async def _start_agent_runtime_turn(
    request: Request,
    user: UserContext,
    body: AssistantChatRequest,
    *,
    session_id: str,
    model_id: str,
) -> Any:
    _require_agent_runtime_request(body)
    await require_conversation_source_access(request, user, session_id, for_execution=True)
    control = agent_runtime_control(request)
    try:
        memory_mode = await effective_assistant_memory_mode(
            request, user.tenant_id, user.user_id, body.memory_mode,
        )
        style_guidance = str(body.system_prompt or "").strip() or None
        from .attachment_refs import bind_assistant_attachment_refs, selected_image_inputs

        bound_refs = await bind_assistant_attachment_refs(
            request, user, session_id=session_id, model_id=model_id, refs=body.file_paths,
        )
        image_inputs = await selected_image_inputs(user, session_id=session_id, refs=bound_refs) if bound_refs else []
        readonly = _agent_runtime_readonly_capabilities(body, attachment_refs=bound_refs)
        launch = await resolve_agent_launch(
            entrypoint="assistant",
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=session_id,
            model_id=model_id,
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
            memory_profile=body.memory_profile,
            enable_dynamic_tools=True,
        )
        return await control.start_turn(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=session_id,
            message=body.message,
            image_inputs=image_inputs,
            model_id=model_id,
            reasoning_option=body.reasoning_option,
            legacy_thinking_level=body.thinking_level,
            max_tokens=body.max_tokens,
            temperature=body.temperature,
            memory_mode=memory_mode,
            memory_profile=body.memory_profile,
            readonly_capabilities=readonly,
            resolved_agent_launch=launch,
            style_guidance=style_guidance,
        )
    except AgentLaunchResolutionError as exc:
        raise HTTPException(
            status_code=exc.status_code,
            detail={"code": exc.code, "message": "Agent Runtime rejected the turn"},
        ) from None
    except AgentRuntimeControlError as exc:
        raise HTTPException(
            status_code=exc.status_code,
            detail={"code": exc.code, "message": "Agent Runtime rejected the turn"},
        ) from None


def _chat_outcome_error(code: str, session_id: str, run_id: str, status: int = 502):
    return HTTPException(status_code=status, detail={
        "code": code,
        "message": "Chat did not complete successfully; inspect the existing run before retrying",
        "session_id": session_id,
        "run_id": run_id,
    })


async def _collect_chat_text(request, user, session_id, turn, control) -> tuple[str, list[dict[str, Any]]]:
    """Collect only this run's authorized text and require its successful terminal.

    Runtime owns execution. EOF, tool completion and another run's terminal are
    never evidence that this turn succeeded. No event payload is used as an error
    message, and no retry or additional turn is started here.
    """
    parts: list[str] = []
    citations = ChatCitations()
    terminal: str | None = None
    revoked = False
    unknown = False
    frames = control.stream_events(
        turn=turn, tenant_id=user.tenant_id, user_id=user.user_id, session_id=session_id,
    )
    guarded = guarded_conversation_frames(request, user, session_id, frames)
    body_failed = False
    cleanup_failed = False
    try:
        async for frame in guarded:
            for line in frame.decode("utf-8").splitlines():
                if not line.startswith("data:"):
                    continue
                event = json.loads(line[5:].strip())
                if not isinstance(event, dict) or not isinstance(event.get("data"), dict):
                    unknown = True
                    continue
                data = event["data"]
                # The source guard's denial is sticky, including redacted frames.
                if data.get("source_access_revoked") is True:
                    revoked = True
                    parts.clear()
                if data.get("run_id") != turn.run_id:
                    continue
                kind = event.get("event_type")
                if kind not in {
                    "text_delta", "run_finished", "run_error", "cancelled",
                    "side_effect_unknown", "error", "context_retrieved",
                }:
                    continue
                if any(data.get(key) != expected for key, expected in (
                    ("session_id", session_id), ("thread_id", turn.runtime_thread_id),
                )):
                    unknown = True
                    continue
                if kind == "context_retrieved" and not revoked:
                    citations.add(data)
                if kind == "text_delta" and isinstance(data.get("content"), str) and not revoked:
                    parts.append(data["content"])
                envelope = data.get("terminal_envelope")
                if kind in {"run_finished", "run_error", "cancelled", "side_effect_unknown", "error"}:
                    if "terminal_envelope" in data and (
                        not isinstance(envelope, dict)
                        or envelope.get("run_id") != turn.run_id
                        or envelope.get("session_id") != session_id
                        or envelope.get("thread_id") != turn.runtime_thread_id
                        or envelope.get("status") != data.get("status")
                        or envelope.get("exit_reason") != data.get("status")
                    ):
                        unknown = True
                    if kind in {"side_effect_unknown", "error"} or (
                        isinstance(envelope, dict)
                        and envelope.get("exit_reason") == "side_effect_unknown"
                    ):
                        unknown = True
                    if (
                        kind == "run_finished" and data.get("status") == "succeeded"
                        and "terminal_envelope" not in data
                        and data.get("recovery") != "original_turn_reconciled"
                    ):
                        unknown = True
                    if data.get("durable") is False:
                        unknown = True
                    status = data.get("status")
                    observed = (
                        "succeeded" if kind == "run_finished" and status == "succeeded"
                        else "cancelled" if kind == "cancelled" or status == "cancelled"
                        else "failed" if kind == "run_error" and status == "failed"
                        else "blocked" if status == "blocked"
                        else None
                    )
                    if observed is None or (terminal is not None and terminal != observed):
                        unknown = True
                    if observed is not None:
                        terminal = observed
    except BaseException as exc:
        body_failed = True
        if not isinstance(exc, Exception):
            raise
        if revoked:
            raise _chat_outcome_error(
                "ASSISTANT_SOURCE_ACCESS_REVOKED", session_id, turn.run_id, 403,
            ) from None
        raise _chat_outcome_error("ASSISTANT_CHAT_OUTCOME_UNKNOWN", session_id, turn.run_id) from None
    finally:
        # Retire both layers on early failure/cancellation. Preserve the body
        # exception if cleanup also fails; cleanup alone cannot certify success.
        for iterator in (guarded, frames):
            close = getattr(iterator, "aclose", None)
            if close is not None:
                try:
                    await close()
                except BaseException as exc:
                    if not body_failed:
                        if not isinstance(exc, Exception):
                            raise
                        cleanup_failed = True
    if revoked:
        raise _chat_outcome_error("ASSISTANT_SOURCE_ACCESS_REVOKED", session_id, turn.run_id, 403)
    if unknown or cleanup_failed or terminal is None:
        raise _chat_outcome_error("ASSISTANT_CHAT_OUTCOME_UNKNOWN", session_id, turn.run_id)
    if terminal != "succeeded":
        raise _chat_outcome_error(
            f"ASSISTANT_CHAT_{terminal.upper()}", session_id, turn.run_id, 409,
        )
    try:
        contexts = await citations.project(request, user)
    except Exception:
        raise _chat_outcome_error("ASSISTANT_SOURCE_CHECK_UNAVAILABLE", session_id, turn.run_id, 503) from None
    if contexts is None:
        raise _chat_outcome_error("ASSISTANT_SOURCE_ACCESS_REVOKED", session_id, turn.run_id, 403)
    return "".join(parts), contexts


@router.post("/chat", response_model=AssistantChatResponse)
async def chat(
    body: AssistantChatRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> AssistantChatResponse:
    """
    Non-streaming chat completion through the Gateway-owned Agent Runtime.

    Gateway responsibilities:
      - per-user rate limit
      - model-permission check
      - session-ownership check

    Model routing, tool execution, and persistence remain Runtime-owned.
    """
    # ``get_user_context`` is a *scoping* dependency: with no credentials it
    # mints an anonymous guest on tenant ``public`` rather than rejecting the
    # call.  Every route below assumes a real actor, so the V1 chat edge has to
    # say so itself — exactly like ``/tools`` and ``/policies`` and V2's
    # ``_require_actor``.  Without it an anonymous caller reaches model
    # resolution and is turned away only because tenant ``public`` happens to
    # own no ``llm_models`` row, which leaks the configured default model name
    # as ``400 Unknown model`` and would start a real turn the moment that
    # tenant ever gained one.
    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="authentication required")

    from ...deps import enforce_rate_limit

    try:
        raw_body = await request.json()
    except (json.JSONDecodeError, ValueError):
        raw_body = {}
    reject_client_agent_forgery(
        request,
        raw_body if isinstance(raw_body, dict) else {},
    )
    await enforce_rate_limit(request, user, operation="assistant_chat")

    # Model-permission authz is enforced at the Gateway edge before Runtime.
    model_id = effective_chat_model_id(request, body.model_id)
    model_meta = getattr(request.app.state, "model_meta", None)
    if model_meta:
        await check_model_permission(user, model_id, model_meta)

    session_id = body.session_id or str(uuid.uuid4())
    await validate_chat_session_access(request=request, user=user, session_id=session_id)
    await ensure_agent_runtime_session(request, user, session_id)
    started_at = time.perf_counter()
    turn = await _start_agent_runtime_turn(
        request,
        user,
        body,
        session_id=session_id,
        model_id=model_id,
    )
    control = agent_runtime_control(request)
    content, contexts = await _collect_chat_text(
        request, user, session_id, turn, control,
    )
    return AssistantChatResponse(
        content=content,
        usage={},
        contexts=contexts,
        duration_ms=(time.perf_counter() - started_at) * 1000,
        model_id=model_id,
        session_id=session_id,
        run_id=turn.run_id,
    )


@router.post("/chat/stream")
async def chat_stream(
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> StreamingResponse:
    """Streaming chat completion (SSE) through the Gateway-owned Agent Runtime.

    Gateway responsibilities:
      - JWT authentication
      - per-user rate limiting
      - model-permission and session-ownership checks

    Model routing, tool execution, persistence, and SSE event projection remain Runtime-owned.
    """
    # ``get_user_context`` is a *scoping* dependency: with no credentials it
    # mints an anonymous guest on tenant ``public`` rather than rejecting the
    # call.  Every route below assumes a real actor, so the V1 chat edge has to
    # say so itself — exactly like ``/tools`` and ``/policies`` and V2's
    # ``_require_actor``.  Without it an anonymous caller reaches model
    # resolution and is turned away only because tenant ``public`` happens to
    # own no ``llm_models`` row, which leaks the configured default model name
    # as ``400 Unknown model`` and would start a real turn the moment that
    # tenant ever gained one.
    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="authentication required")

    from ...deps import enforce_rate_limit

    await enforce_rate_limit(request, user, operation="assistant_chat")

    # Read the body once; Starlette's Request.stream() is single-use.
    body_bytes = await request.body()
    try:
        body_json = json.loads(body_bytes) if body_bytes else {}
    except json.JSONDecodeError:
        raise HTTPException(status_code=400, detail="Invalid JSON body")

    # Keep the streaming edge contract identical to the typed non-streaming
    # route. Reject unmigrated fields before the Runtime control plane starts.
    try:
        validated_body = AssistantChatRequest.model_validate(body_json)
    except ValidationError as exc:
        raise RequestValidationError(exc.errors()) from exc

    reject_client_agent_forgery(
        request,
        body_json if isinstance(body_json, dict) else {},
    )

    model_id = effective_chat_model_id(request, validated_body.model_id)
    model_meta = getattr(request.app.state, "model_meta", None)
    if model_meta:
        await check_model_permission(user, model_id, model_meta)

    # Authz 2: every V1 stream is a projection of the single Agent Runtime.
    session_id = validated_body.session_id or str(uuid.uuid4())
    await validate_chat_session_access(request=request, user=user, session_id=session_id)
    await ensure_agent_runtime_session(request, user, session_id)
    turn = await _start_agent_runtime_turn(
        request,
        user,
        validated_body,
        session_id=session_id,
        model_id=model_id,
    )
    control = agent_runtime_control(request)
    return StreamingResponse(
        guarded_conversation_frames(request, user, session_id, control.stream_events(
            turn=turn,
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=session_id,
        )),
        media_type="text/event-stream",
        headers={
            "cache-control": "no-cache",
            "x-accel-buffering": "no",
            "x-ai-agent-kernel": "agent_runtime",
        },
    )
