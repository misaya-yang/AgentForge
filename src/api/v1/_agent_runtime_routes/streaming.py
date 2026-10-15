"""Session binding, idempotency and streaming startup for Agent Runtime.

ARC-01B split of ``src/api/v1/agent_runtime.py``.  Moved verbatim; the facade
keeps time-limited re-exports for pre-split import paths.
"""

from __future__ import annotations

import contextlib
import os
from collections.abc import AsyncIterator
from typing import Any

from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_core.exceptions import PermissionDeniedError
from fastapi import HTTPException, Request
from fastapi.responses import Response, StreamingResponse

from ....core.auth.user_resolver import UserContext
from ....services.assistant_entry.launch_resolution import (
    AgentLaunchResolutionError,
    resolve_agent_launch,
)
from ....services.assistant_entry.model_access import assistant_model_service
from ....services.eval.assistant_trace_capture import capture_assistant_runtime_stream
from .core import (
    _raise_runtime_error,
    _session_manager,
)


def _idempotency_replay_response(
    request: Request,
    reservation: dict[str, Any],
) -> Response:
    status = str(reservation.get("status") or "pending")
    if status != "completed" or reservation.get("response_body") is None:
        code = (
            "AGENT_RUNTIME_IDEMPOTENCY_IN_PROGRESS"
            if status == "pending"
            else "AGENT_RUNTIME_IDEMPOTENCY_EXECUTION_FAILED"
        )
        _raise_runtime_error(
            request,
            409,
            code,
            "The idempotent request has already been attempted",
        )
    response = Response(
        content=bytes(reservation["response_body"]),
        status_code=int(reservation.get("response_status_code") or 200),
        media_type=str(reservation.get("response_media_type") or "text/event-stream"),
    )
    response.headers["Cache-Control"] = "no-cache"
    response.headers["X-Session-Id"] = str(reservation["session_id"])
    response.headers["X-Idempotent-Replay"] = "true"
    return response


def _record_idempotent_stream(
    response: Any,
    *,
    repository: Any,
    reservation_key: dict[str, str],
) -> Any:
    """Capture the terminal SSE body while forwarding it exactly once."""

    if not isinstance(response, StreamingResponse) or not 200 <= response.status_code < 300:
        return response
    source = response.body_iterator
    max_bytes = int(os.getenv("AGENT_RUNTIME_IDEMPOTENCY_MAX_RESPONSE_BYTES", "8388608"))

    async def recorded() -> AsyncIterator[bytes | str]:
        chunks: list[bytes] = []
        size = 0
        overflow = False
        try:
            async for chunk in source:
                encoded = chunk.encode("utf-8") if isinstance(chunk, str) else bytes(chunk)
                size += len(encoded)
                if size <= max_bytes:
                    chunks.append(encoded)
                else:
                    overflow = True
                yield chunk
        except BaseException:
            await repository.fail_runtime_idempotency(**reservation_key)
            raise
        if overflow:
            await repository.fail_runtime_idempotency(**reservation_key)
            return
        await repository.complete_runtime_idempotency(
            **reservation_key,
            response_body=b"".join(chunks),
            response_media_type=response.media_type or "text/event-stream",
            response_status_code=response.status_code,
        )

    response.body_iterator = recorded()
    return response


async def _bind_session(
    request: Request,
    user: UserContext,
    *,
    session_id: str,
    snapshot: dict[str, Any],
    draft_revision: int | None,
) -> Any:
    publication = snapshot["publication"]
    assignment_store = getattr(request.app.state, "assistant_runtime_assignments", None)
    if assignment_store is None:
        _raise_runtime_error(
            request,
            503,
            "AGENT_RUNTIME_ASSIGNMENT_UNAVAILABLE",
            "Agent Runtime ownership is unavailable",
        )
    session_manager = _session_manager(request)
    existing_before = await session_manager.get(session_id)
    try:
        bound_session = await session_manager.bind_agent_runtime(
            session_id=session_id,
            user_id=user.user_id,
            tenant_id=user.tenant_id,
            agent_id=snapshot["agent_id"],
            agent_version_id=snapshot["agent_version_id"],
            agent_draft_revision=draft_revision,
            publication_id=publication["id"],
            channel=publication["channel"],
            runtime_fingerprint=runtime_sha256(snapshot),
            agent_spec_hash=snapshot["fingerprints"]["spec"],
        )
    except PermissionDeniedError:
        _raise_runtime_error(
            request,
            404,
            "AGENT_RUNTIME_SESSION_NOT_FOUND",
            "Agent runtime session not found",
        )
    try:
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
                    request.app.state,
                    "assistant_runtime_kernel_revision",
                    None,
                ),
                assignment_reason="single_kernel_agent_channel",
            )
        if assignment.runtime_owner != "agent_runtime":
            _raise_runtime_error(
                request,
                409,
                "AGENT_RUNTIME_ASSIGNMENT_MISMATCH",
                "Agent Runtime session ownership is invalid",
            )
    except BaseException:
        if existing_before is None:
            with contextlib.suppress(Exception):
                await session_manager.delete(session_id)
        raise
    return bound_session


async def _existing_session(request: Request, session_id: str | None) -> Any | None:
    if not session_id:
        return None
    return await _session_manager(request).get(session_id)


def _assert_existing_pin(
    request: Request,
    user: UserContext,
    existing: Any,
    *,
    agent_id: str | None,
    agent_version_id: str | None,
    publication_id: str | None,
    channel: str,
    draft_revision: int | None,
) -> None:
    if (
        existing.user_id != user.user_id
        or existing.tenant_id != user.tenant_id
        or existing.channel != channel
        or (agent_id is not None and existing.agent_id != agent_id)
        or (
            agent_version_id is not None
            and existing.agent_version_id != agent_version_id
        )
        or existing.publication_id != publication_id
        or existing.agent_draft_revision != draft_revision
    ):
        _raise_runtime_error(
            request,
            404,
            "AGENT_RUNTIME_SESSION_NOT_FOUND",
            "Agent runtime session not found",
        )


async def _start_runtime_stream(
    request: Request,
    user: UserContext,
    *,
    body: dict[str, Any],
    snapshot: dict[str, Any],
    draft_revision: int | None = None,
) -> Any:
    if body.get("resume_run_id") or body.get("resume_approval_id"):
        _raise_runtime_error(
            request,
            409,
            "AGENT_RUNTIME_RESUME_NOT_AVAILABLE",
            "This Runtime turn cannot resume an approval from the retired Agent loop",
        )
    control = getattr(request.app.state, "agent_runtime_control", None)
    if control is None or not hasattr(control, "start_turn"):
        _raise_runtime_error(
            request,
            503,
            "AGENT_RUNTIME_UNAVAILABLE",
            "Agent Runtime control plane is unavailable",
        )
    model = snapshot.get("model") if isinstance(snapshot.get("model"), dict) else {}
    parameters = model.get("parameters") if isinstance(model.get("parameters"), dict) else {}
    knowledge = snapshot.get("knowledge") if isinstance(snapshot.get("knowledge"), dict) else {}
    retrieval = knowledge.get("retrieval") if isinstance(knowledge.get("retrieval"), dict) else {}
    attachments = body.get("attachments") if isinstance(body.get("attachments"), list) else []
    readonly = {
        "knowledge": {
            "dataset_ids": list(knowledge.get("datasets") or []),
            "mode": str(retrieval.get("mode") or "off"),
            "top_k": int(retrieval.get("top_k") or 5),
            "score_threshold": float(retrieval.get("threshold") or 0.4),
        },
        "attachments": {
            "refs": [
                str(item.get("file_path") or item.get("artifact_id"))
                for item in attachments
                if isinstance(item, dict) and (item.get("file_path") or item.get("artifact_id"))
            ]
        },
    }
    try:
        publication = (
            snapshot.get("publication")
            if isinstance(snapshot.get("publication"), dict)
            else {}
        )
        launch = await resolve_agent_launch(
            entrypoint=(
                "studio_preview"
                if publication.get("channel") == "preview"
                else "published_agent"
            ),
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=str(body["session_id"]),
            model_id=str(model.get("id") or ""),
            model_service=(
                assistant_model_service(request)
                or getattr(control, "model_service", None)
            ),
            readonly_capabilities=readonly,
            legacy_thinking_level=str(parameters.get("thinking_mode") or "") or None,
            max_tokens=(
                int(parameters["max_tokens"])
                if isinstance(parameters.get("max_tokens"), int)
                and not isinstance(parameters.get("max_tokens"), bool)
                else None
            ),
            temperature=(
                float(parameters["temperature"])
                if isinstance(parameters.get("temperature"), int | float)
                and not isinstance(parameters.get("temperature"), bool)
                else None
            ),
            legacy_snapshot=snapshot,
            draft_revision=draft_revision,
        )
        turn = await control.start_turn(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=str(body["session_id"]),
            message=str(body["message"]),
            model_id=str(model.get("id") or ""),
            reasoning_option=None,
            legacy_thinking_level=str(parameters.get("thinking_mode") or "") or None,
            max_tokens=(
                int(parameters["max_tokens"])
                if isinstance(parameters.get("max_tokens"), int)
                and not isinstance(parameters.get("max_tokens"), bool)
                else None
            ),
            temperature=(
                float(parameters["temperature"])
                if isinstance(parameters.get("temperature"), int | float)
                and not isinstance(parameters.get("temperature"), bool)
                else None
            ),
            resolved_agent_launch=launch,
        )
    except Exception as exc:
        from ....services.agent_runtime import AgentRuntimeControlError

        if isinstance(exc, AgentLaunchResolutionError):
            raise HTTPException(
                status_code=exc.status_code,
                detail={"code": exc.code, "message": "Agent Runtime rejected the turn"},
            ) from None
        if isinstance(exc, AgentRuntimeControlError):
            raise HTTPException(
                status_code=exc.status_code,
                detail={"code": exc.code, "message": "Agent Runtime rejected the turn"},
            ) from None
        raise
    stream = capture_assistant_runtime_stream(
        control.stream_events(
            turn=turn,
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=str(body["session_id"]),
        ),
        database=getattr(request.app.state, "database", None),
        run_id=str(turn.run_id),
        request_id=str(
            getattr(request.state, "request_id", "")
            or getattr(request.state, "trace_id", "")
        ),
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        session_id=str(body["session_id"]),
        message=str(body["message"]),
        snapshot=snapshot,
        terminal_replay_source=lambda: control.stream_events(
            turn=turn,
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=str(body["session_id"]),
        ),
    )
    return StreamingResponse(
        stream,
        media_type="text/event-stream",
        headers={
            "cache-control": "no-cache",
            "x-accel-buffering": "no",
            "x-ai-agent-kernel": "agent_runtime",
            "x-session-id": str(body["session_id"]),
            "x-run-id": str(turn.run_id),
        },
    )
