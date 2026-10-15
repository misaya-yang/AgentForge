"""Public ``POST /v1/responses`` boundary backed by the Agent Runtime.

The Gateway owns authentication, tenant/model authorization, request
validation, idempotency, and the Responses compatibility projection.
"""

from __future__ import annotations

import hashlib
import json
import logging
import re
import time
import uuid
from collections.abc import AsyncIterator
from typing import Any

from ai_gateway_core.comm.idempotency import (
    CachedResponse,
    InMemoryIdempotencyStore,
    RedisIdempotencyStore,
)
from fastapi import APIRouter, Depends, HTTPException, Request
from fastapi.responses import JSONResponse, Response, StreamingResponse

from ...core.auth.user_resolver import UserContext
from ...services.assistant_entry.launch_resolution import (
    AgentLaunchResolutionError,
    resolve_agent_launch,
)
from ...services.assistant_entry.model_access import (
    assistant_model_service,
    check_model_permission,
)
from ...services.assistant_entry.session_binding import ensure_agent_runtime_session
from ...services.llm.model_service import ModelProviderAmbiguous
from ..deps import enforce_rate_limit, get_user_context
from ._agent_runtime_headers import reject_client_agent_forgery
from .responses_usage import responses_usage

router = APIRouter(tags=["Responses"])

logger = logging.getLogger(__name__)
_RESPONSES_TOOL_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$")
_MAX_RESPONSES_TOOLS = 128


def _responses_tool_config(payload: dict[str, Any]) -> dict[str, Any]:
    """Validate the public tool controls and project them to Runtime metadata.

    The Runtime executes only descriptors from the tenant-scoped capability
    catalog.  Responses function definitions are therefore selectors, not an
    escape hatch for client-supplied executors or schemas.  The control plane
    resolves these names against its immutable catalog before creating the
    kernel thread.
    """

    raw_tools = payload.get("tools")
    if raw_tools is None:
        tool_names: list[str] = []
        public_tools: list[dict[str, Any]] = []
    else:
        if not isinstance(raw_tools, list) or len(raw_tools) > _MAX_RESPONSES_TOOLS:
            raise HTTPException(status_code=400, detail="tools must be an array of at most 128 items")
        tool_names = []
        public_tools = []
        for raw in raw_tools:
            if not isinstance(raw, dict) or raw.get("type") != "function":
                raise HTTPException(
                    status_code=409,
                    detail={"code": "responses_tool_type_not_migrated"},
                )
            function = raw.get("function") if isinstance(raw.get("function"), dict) else raw
            name = function.get("name")
            if not isinstance(name, str) or not _RESPONSES_TOOL_NAME.fullmatch(name):
                raise HTTPException(status_code=400, detail="function tool name is invalid")
            description = function.get("description", "")
            if not isinstance(description, str) or len(description) > 20_000:
                raise HTTPException(status_code=400, detail="function tool description is invalid")
            parameters = function.get("parameters", {"type": "object", "properties": {}})
            if not isinstance(parameters, dict):
                raise HTTPException(status_code=400, detail="function tool parameters must be an object")
            if name in tool_names:
                raise HTTPException(status_code=400, detail="function tool names must be unique")
            tool_names.append(name)
            # Keep the public shape for the compatibility response.  No
            # client-provided executable fields cross the Runtime boundary.
            public_tools.append(
                {
                    "type": "function",
                    "name": name,
                    "description": description,
                    "parameters": parameters,
                    **({"strict": function["strict"]} if isinstance(function.get("strict"), bool) else {}),
                }
            )

    raw_choice = payload.get("tool_choice", "auto")
    if raw_choice is None:
        raw_choice = "auto"
    if not isinstance(raw_choice, str) or raw_choice not in {"auto", "none", "required"}:
        if not isinstance(raw_choice, dict) or raw_choice.get("type") != "function":
            raise HTTPException(status_code=400, detail="tool_choice is invalid")
        choice_function = raw_choice.get("function")
        choice_name = (
            choice_function.get("name")
            if isinstance(choice_function, dict)
            else raw_choice.get("name")
        )
        if not isinstance(choice_name, str) or not _RESPONSES_TOOL_NAME.fullmatch(choice_name):
            raise HTTPException(status_code=400, detail="tool_choice function name is invalid")
        raw_choice = {"type": "function", "name": choice_name}
    if tool_names is not None:
        choice_name = raw_choice.get("name") if isinstance(raw_choice, dict) else None
        if choice_name and choice_name not in tool_names:
            raise HTTPException(status_code=400, detail="tool_choice references an unavailable tool")
        if raw_choice == "required" and not tool_names:
            raise HTTPException(status_code=400, detail="required tool_choice needs at least one tool")
    parallel = payload.get("parallel_tool_calls", True)
    if not isinstance(parallel, bool):
        raise HTTPException(status_code=400, detail="parallel_tool_calls must be a boolean")
    return {
        "tool_names": tool_names,
        "public_tools": public_tools,
        "tool_choice": raw_choice,
        "parallel_tool_calls": parallel,
    }


def _error(
    *,
    status_code: int,
    code: str,
    message: str,
    param: str | None = None,
    error_type: str = "invalid_request_error",
    headers: dict[str, str] | None = None,
) -> JSONResponse:
    return JSONResponse(
        status_code=status_code,
        content={
            "error": {
                "message": message,
                "type": error_type,
                "param": param,
                "code": code,
            }
        },
        headers=headers,
    )


def _has_authenticated_tenant(user: UserContext) -> bool:
    return bool(
        user.is_authenticated
        and user.user_id
        and user.tenant_id
        and user.user_id != "anonymous"
        and user.tenant_id != "public"
    )


def _responses_message(payload: dict[str, Any]) -> str:
    value = payload.get("input")
    if isinstance(value, str) and value.strip():
        return value
    if not isinstance(value, list) or not value:
        raise HTTPException(status_code=400, detail="input must be a non-empty string or message list")
    parts: list[str] = []
    for item in value:
        if not isinstance(item, dict):
            raise HTTPException(status_code=400, detail="input items must be objects")
        item_type = item.get("type", "message")
        if item_type not in {"message", "input_text"}:
            raise HTTPException(
                status_code=409,
                detail={"code": "responses_input_item_not_migrated", "item_type": item_type},
            )
        content = item.get("content", item.get("text", ""))
        if isinstance(content, str):
            parts.append(content)
            continue
        if isinstance(content, list):
            for part in content:
                if not isinstance(part, dict) or part.get("type") not in {"input_text", "text"}:
                    raise HTTPException(
                        status_code=409,
                        detail={"code": "responses_input_item_not_migrated"},
                    )
                text = part.get("text")
                if not isinstance(text, str):
                    raise HTTPException(status_code=400, detail="input text must be a string")
                parts.append(text)
            continue
        raise HTTPException(status_code=400, detail="input content is invalid")
    message = "\n".join(part for part in parts if part)
    if not message:
        raise HTTPException(status_code=400, detail="input must contain text")
    return message


def _reject_unmigrated_fields(payload: dict[str, Any]) -> None:
    unsupported = {
        "previous_response_id",
        "attachments",
        "conversation",
        "store",
    }
    present = sorted(
        key
        for key in unsupported
        if key in payload
        and (
            payload[key] not in (None, False)
            if key == "store"
            else payload[key] not in (None, [], "")
        )
    )
    if present:
        raise HTTPException(
            status_code=409,
            detail={"code": "responses_fields_not_migrated", "fields": present},
        )


def _runtime_control(request: Request) -> Any:
    control = getattr(request.app.state, "agent_runtime_control", None)
    if control is None:
        raise HTTPException(
            status_code=503,
            detail={"code": "agent_runtime_unavailable"},
        )
    return control


async def _runtime_events(
    control: Any,
    *,
    turn: Any,
    user: UserContext,
    session_id: str,
) -> AsyncIterator[dict[str, Any]]:
    async for frame in control.stream_events(
        turn=turn,
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        session_id=session_id,
    ):
        for line in frame.decode("utf-8", errors="replace").splitlines():
            if not line.startswith("data:"):
                continue
            try:
                event = json.loads(line[5:].strip())
            except json.JSONDecodeError:
                continue
            if isinstance(event, dict):
                yield event


def _response_id(turn: Any) -> str:
    return f"resp_{turn.run_id.replace('-', '')}"


def _completed_response(
    *,
    response_id: str,
    model: str,
    text: str,
    input_value: Any = None,
    status: str = "completed",
    usage: dict[str, Any] | None = None,
    error: dict[str, Any] | None = None,
    tool_calls: list[dict[str, Any]] | None = None,
    requested_tools: list[dict[str, Any]] | None = None,
    tool_choice: Any = "auto",
    parallel_tool_calls: bool = True,
) -> dict[str, Any]:
    normalized_usage = responses_usage(
        usage,
        input_value=input_value,
        output_text=text,
    )
    output: list[dict[str, Any]] = list(tool_calls or [])
    output.append(
        {
            "id": f"msg_{response_id[5:]}",
            "type": "message",
            "role": "assistant",
            "status": "completed" if status == "completed" else "incomplete",
            "content": [{"type": "output_text", "text": text, "annotations": []}],
        }
    )
    return {
        "id": response_id,
        "object": "response",
        "created_at": int(time.time()),
        "status": status,
        "error": error,
        "incomplete_details": None,
        "model": model,
        "output": output,
        "output_text": text,
        "parallel_tool_calls": parallel_tool_calls,
        "tool_choice": tool_choice,
        "tools": requested_tools or [],
        "usage": normalized_usage,
    }


def _response_function_call(data: dict[str, Any]) -> dict[str, Any] | None:
    call_id = data.get("tool_call_id") or data.get("call_id")
    name = data.get("tool_name") or data.get("name")
    if not isinstance(call_id, str) or not call_id or not isinstance(name, str) or not name:
        return None
    arguments = data.get("arguments", "")
    if isinstance(arguments, (dict, list)):
        arguments = json.dumps(arguments, ensure_ascii=False, separators=(",", ":"))
    if not isinstance(arguments, str):
        arguments = str(arguments)
    return {
        "id": str(call_id),
        "type": "function_call",
        "status": "completed" if data.get("status") in {"completed", "succeeded"} else "in_progress",
        "call_id": str(call_id),
        "name": name,
        "arguments": arguments,
    }


def _sse(event_type: str, payload: dict[str, Any], sequence_number: int) -> bytes:
    event = {"type": event_type, "sequence_number": sequence_number, **payload}
    return (
        f"event: {event_type}\ndata: "
        f"{json.dumps(event, ensure_ascii=False, separators=(',', ':'))}\n\n"
    ).encode()


def _response_idempotency_store(request: Request):
    existing = getattr(request.app.state, "responses_idempotency_store", None)
    if existing is not None:
        return existing
    redis = getattr(request.app.state, "redis", None)
    native = None
    if redis is not None:
        getter = getattr(redis, "get_native_client", None)
        native = getter() if callable(getter) else redis
    store = (
        RedisIdempotencyStore(native, prefix="ai-gateway:responses:idem")
        if native is not None
        else InMemoryIdempotencyStore()
    )
    request.app.state.responses_idempotency_store = store
    return store


async def _begin_idempotent_response(
    request: Request,
    user: UserContext,
    body: bytes,
) -> tuple[Any, str, str] | Response | None:
    raw_key = request.headers.get("Idempotency-Key", "").strip()
    if not raw_key:
        return None
    if len(raw_key) > 255 or any(ord(char) < 0x21 or ord(char) > 0x7E for char in raw_key):
        return _error(
            status_code=400,
            code="invalid_idempotency_key",
            message="Idempotency-Key must contain 1-255 visible ASCII characters.",
        )
    digest = hashlib.sha256(body).hexdigest()
    scope_key = hashlib.sha256(
        "\n".join((user.tenant_id, user.user_id, "/v1/responses", raw_key)).encode()
    ).hexdigest()
    store = _response_idempotency_store(request)
    cached = await store.get_cached(scope_key)
    if cached is not None:
        if cached.request_body_digest != digest:
            return _error(
                status_code=409,
                code="idempotency_key_conflict",
                message="Idempotency-Key was already used with a different request body.",
            )
        headers = {
            name.decode("latin-1"): value.decode("latin-1")
            for name, value in cached.headers
            if name.lower() not in {b"content-length", b"transfer-encoding"}
        }
        headers["x-idempotency-replayed"] = "true"
        return Response(
            content=cached.body,
            status_code=cached.status_code,
            headers=headers,
            media_type="application/json",
        )
    if not await store.try_begin(scope_key, 86_400):
        cached = await store.wait_for_cached(
            scope_key,
            timeout_seconds=5.0,
            poll_seconds=0.05,
        )
        if cached is None:
            return _error(
                status_code=409,
                code="idempotency_request_in_progress",
                message="An identical request is still in progress.",
            )
        if cached.request_body_digest != digest:
            return _error(
                status_code=409,
                code="idempotency_key_conflict",
                message="Idempotency-Key was already used with a different request body.",
            )
        headers = {
            name.decode("latin-1"): value.decode("latin-1")
            for name, value in cached.headers
            if name.lower() not in {b"content-length", b"transfer-encoding"}
        }
        headers["x-idempotency-replayed"] = "true"
        return Response(
            content=cached.body,
            status_code=cached.status_code,
            headers=headers,
            media_type="application/json",
        )
    return store, scope_key, digest


async def _finish_idempotent_response(
    state: tuple[Any, str, str] | None,
    response: JSONResponse,
    *,
    accepted_run: bool = False,
) -> None:
    if state is None:
        return
    store, scope_key, digest = state
    if response.status_code >= 500 and not accepted_run:
        await store.abort(scope_key)
        return
    await store.store_response(
        scope_key,
        CachedResponse(
            status_code=response.status_code,
            headers=list(response.raw_headers),
            body=bytes(response.body),
            request_body_digest=digest,
        ),
        86_400,
    )


async def _abort_idempotent_response(state: tuple[Any, str, str] | None) -> None:
    if state is None:
        return
    store, scope_key, _digest = state
    await store.abort(scope_key)


@router.post("/responses")
async def create_response(
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> Response:
    """Proxy one authenticated Responses request to the canonical Assistant runtime."""

    if not _has_authenticated_tenant(user):
        return _error(
            status_code=401,
            code="authentication_required",
            message="Authentication and tenant identity are required.",
            error_type="authentication_error",
        )
    if request.url.query:
        return _error(
            status_code=400,
            code="unsupported_query_parameters",
            message="Query parameters are not supported for this endpoint.",
        )
    try:
        await enforce_rate_limit(request, user, operation="assistant_chat")
    except HTTPException as exc:
        if exc.status_code != 429:
            raise
        return _error(
            status_code=429,
            code="rate_limit_exceeded",
            message="Rate limit exceeded.",
            error_type="rate_limit_error",
            headers=dict(exc.headers or {}),
        )

    body = await request.body()
    try:
        payload: Any = json.loads(body) if body else None
    except (json.JSONDecodeError, UnicodeDecodeError):
        return _error(status_code=400, code="invalid_json", message="Invalid JSON body.")
    if not isinstance(payload, dict):
        return _error(
            status_code=400,
            code="invalid_json_object",
            message="Request body must be a JSON object.",
        )
    try:
        reject_client_agent_forgery(request, payload)
    except HTTPException as exc:
        detail = exc.detail if isinstance(exc.detail, dict) else {}
        return _error(
            status_code=exc.status_code,
            code=str(detail.get("code") or "agent_runtime_field_forbidden").lower(),
            message="Client-supplied Agent runtime fields or headers are forbidden.",
            error_type="invalid_request_error",
        )

    model = payload.get("model")
    if not isinstance(model, str) or not model.strip() or len(model) > 255:
        return _error(
            status_code=400,
            code="invalid_model",
            message="model must be a non-empty string.",
            param="model",
        )
    expected_model_ref = payload.get("expected_model_ref")
    if expected_model_ref is not None and not isinstance(expected_model_ref, dict):
        return _error(status_code=400, code="invalid_expected_model_ref",
                      message="expected_model_ref must be an object.", param="expected_model_ref")
    provider_id = payload.get("provider_id")
    if provider_id is not None and (
        not isinstance(provider_id, str) or not provider_id.strip() or len(provider_id) > 255
    ):
        return _error(status_code=400, code="invalid_provider_id",
                      message="provider_id must be a non-empty string.", param="provider_id")
    provider_id = provider_id.strip() if provider_id else None
    model_meta = getattr(request.app.state, "model_meta", None)
    if model_meta is None:
        return _error(
            status_code=503,
            code="model_authorization_unavailable",
            message="Model authorization is temporarily unavailable.",
            param="model",
            error_type="server_error",
        )
    try:
        await check_model_permission(user, model, model_meta, provider_id=provider_id)
    except HTTPException as exc:
        if exc.status_code == 400:
            return _error(
                status_code=400,
                code="model_not_found",
                message="The requested model was not found.",
                param="model",
            )
        if exc.status_code == 403:
            return _error(
                status_code=403,
                code="model_access_denied",
                message="Access to the requested model is denied.",
                param="model",
                error_type="permission_error",
            )
        raise
    except ModelProviderAmbiguous:
        return _error(status_code=409, code="model_provider_required",
                      message="Specify provider_id for this model.", param="provider_id")
    except Exception as exc:
        logger.error(
            "Responses model authorization unavailable (exception_type=%s)",
            type(exc).__name__,
        )
        return _error(
            status_code=503,
            code="model_authorization_unavailable",
            message="Model authorization is temporarily unavailable.",
            param="model",
            error_type="server_error",
        )

    try:
        _reject_unmigrated_fields(payload)
    except HTTPException as exc:
        detail = exc.detail if isinstance(exc.detail, dict) else {}
        return _error(
            status_code=exc.status_code,
            code=str(detail.get("code") or "responses_fields_not_migrated"),
            message="Some Responses fields are not available on the Agent Runtime.",
        )
    try:
        tool_config = _responses_tool_config(payload)
    except HTTPException as exc:
        detail = exc.detail if isinstance(exc.detail, dict) else {}
        return _error(
            status_code=exc.status_code,
            code=str(detail.get("code") or "invalid_tools"),
            message="The requested tools are not available on the Agent Runtime.",
            param="tools" if "tool" in str(exc.detail) else None,
        )
    try:
        message = _responses_message(payload)
    except HTTPException as exc:
        if isinstance(exc.detail, dict):
            return _error(
                status_code=exc.status_code,
                code=str(exc.detail.get("code") or "invalid_input"),
                message="The Responses input is not supported by the Agent Runtime.",
            )
        return _error(status_code=exc.status_code, code="invalid_input", message=str(exc.detail))
    instructions = payload.get("instructions")
    if instructions is not None and (
        not isinstance(instructions, str) or not instructions.strip() or len(instructions) > 256 * 1024
    ):
        return _error(
            status_code=400,
            code="invalid_instructions",
            message="instructions must be a non-empty string",
            param="instructions",
        )
    model_id = model.strip()
    session_id = str(payload.get("session_id") or uuid.uuid4())
    reasoning = payload.get("reasoning")
    reasoning_option = None
    if isinstance(reasoning, dict):
        effort = reasoning.get("effort")
        if effort is not None and not isinstance(effort, str):
            return _error(status_code=400, code="invalid_reasoning", message="reasoning.effort must be a string")
        reasoning_option = effort
    elif reasoning is not None:
        return _error(status_code=400, code="invalid_reasoning", message="reasoning must be an object")
    max_output_tokens = payload.get("max_output_tokens")
    if max_output_tokens is not None and (
        isinstance(max_output_tokens, bool)
        or not isinstance(max_output_tokens, int)
        or max_output_tokens < 1
    ):
        return _error(
            status_code=400,
            code="invalid_max_output_tokens",
            message="max_output_tokens must be a positive integer",
            param="max_output_tokens",
        )
    temperature = payload.get("temperature")
    if temperature is not None and (
        isinstance(temperature, bool)
        or not isinstance(temperature, int | float)
        or not 0 <= float(temperature) <= 2
    ):
        return _error(
            status_code=400,
            code="invalid_temperature",
            message="temperature must be between 0 and 2",
            param="temperature",
        )
    stream = payload.get("stream", False)
    if not isinstance(stream, bool):
        return _error(
            status_code=400,
            code="invalid_stream",
            message="stream must be a boolean",
            param="stream",
        )
    if stream and request.headers.get("Idempotency-Key", "").strip():
        return _error(
            status_code=409,
            code="streaming_idempotency_not_supported",
            message="Idempotency-Key is currently supported only for non-streaming Responses.",
        )
    idempotency = await _begin_idempotent_response(request, user, body)
    if isinstance(idempotency, Response):
        return idempotency
    try:
        await ensure_agent_runtime_session(request, user, session_id)
    except Exception as exc:
        await _abort_idempotent_response(idempotency)
        logger.error(
            "Responses Runtime session preparation failed (exception_type=%s)",
            type(exc).__name__,
        )
        return _error(
            status_code=503,
            code="agent_runtime_session_unavailable",
            message="Agent Runtime session storage is temporarily unavailable.",
            error_type="server_error",
        )
    try:
        control = _runtime_control(request)
    except HTTPException as exc:
        await _abort_idempotent_response(idempotency)
        return _error(
            status_code=exc.status_code,
            code="agent_runtime_unavailable",
            message="Agent Runtime is temporarily unavailable.",
            error_type="server_error",
        )
    try:
        requested_tools = tool_config["tool_names"]
        readonly_capabilities: dict[str, Any] = {
            "responses_tool_choice": tool_config["tool_choice"] if requested_tools else "none",
            "responses_parallel_tool_calls": tool_config["parallel_tool_calls"],
        }
        if requested_tools:
            readonly_capabilities["responses_tool_names"] = requested_tools
        enable_dynamic_tools = bool(requested_tools) and tool_config["tool_choice"] != "none"
        launch = await resolve_agent_launch(
            entrypoint="responses",
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=session_id,
            model_id=model_id,
            provider_id=provider_id,
            expected_model_ref=expected_model_ref,
            model_service=(
                assistant_model_service(request)
                or getattr(control, "model_service", None)
            ),
            readonly_capabilities=readonly_capabilities,
            reasoning_option=reasoning_option,
            max_tokens=max_output_tokens,
            temperature=temperature,
            developer_instructions=instructions,
            memory_mode="off",
            memory_profile="off",
            enable_dynamic_tools=enable_dynamic_tools,
        )
        turn = await control.start_turn(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            session_id=session_id,
            message=message,
            model_id=model_id,
            reasoning_option=reasoning_option,
            legacy_thinking_level=None,
            max_tokens=max_output_tokens,
            temperature=temperature,
            readonly_capabilities=readonly_capabilities,
            resolved_agent_launch=launch,
            developer_instructions=instructions,
            memory_mode="off",
            # Responses tools must be caller-selected and tenant-catalog resolved;
            # platform tools are never exposed implicitly.
            enable_dynamic_tools=enable_dynamic_tools,
        )
    except HTTPException as exc:
        await _abort_idempotent_response(idempotency)
        return _error(
            status_code=exc.status_code,
            code="agent_runtime_rejected",
            message="Agent Runtime rejected the request.",
        )
    except Exception as exc:
        await _abort_idempotent_response(idempotency)
        from ...services.agent_runtime import AgentRuntimeControlError

        if isinstance(exc, AgentLaunchResolutionError):
            return _error(
                status_code=exc.status_code,
                code=exc.code.lower(),
                message="Agent Runtime rejected the request.",
                error_type=(
                    "server_error" if exc.status_code >= 500 else "invalid_request_error"
                ),
            )
        if isinstance(exc, AgentRuntimeControlError):
            return _error(
                status_code=exc.status_code,
                code=exc.code.lower(),
                message="Agent Runtime rejected the request.",
                error_type="server_error" if exc.status_code >= 500 else "invalid_request_error",
            )
        logger.error("Responses Agent Runtime start failed (exception_type=%s)", type(exc).__name__)
        return _error(
            status_code=503,
            code="agent_runtime_unavailable",
            message="Agent Runtime is temporarily unavailable.",
            error_type="server_error",
        )

    response_id = _response_id(turn)
    if not stream:
        text_parts: list[str] = []
        tool_calls: dict[str, dict[str, Any]] = {}
        terminal_status: str | None = None
        usage: dict[str, Any] | None = None
        try:
            async for event in _runtime_events(
                control, turn=turn, user=user, session_id=session_id
            ):
                event_type = str(event.get("event_type") or "")
                data = event.get("data") if isinstance(event.get("data"), dict) else {}
                if event_type == "text_delta" and isinstance(data.get("content"), str):
                    text_parts.append(data["content"])
                elif event_type == "tool_call_start":
                    call = _response_function_call(data)
                    if call is not None:
                        tool_calls[call["call_id"]] = call
                elif event_type == "tool_call_result":
                    call_id = data.get("tool_call_id") or data.get("call_id")
                    if isinstance(call_id, str) and call_id in tool_calls:
                        tool_calls[call_id]["status"] = (
                            "completed"
                            if data.get("status") in {"completed", "succeeded"}
                            else "failed"
                        )
                elif event_type == "run_error":
                    terminal_status = str(data.get("status") or "failed")
                    usage = data.get("usage") if isinstance(data.get("usage"), dict) else None
                    break
                elif event_type == "run_finished":
                    terminal_status = "completed"
                    usage = data.get("usage") if isinstance(data.get("usage"), dict) else None
                    break
        except Exception as exc:
            logger.error(
                "Responses Runtime event stream failed (exception_type=%s)",
                type(exc).__name__,
            )
            response = _error(
                status_code=503,
                code="agent_runtime_event_stream_failed",
                message="Agent Runtime event stream is temporarily unavailable.",
                error_type="server_error",
            )
            # Execution was accepted. Replaying an observer error must not
            # authorize a second turn under the same idempotency key.
            response.headers["x-run-id"] = str(turn.run_id)
            response.headers["x-session-id"] = session_id
            await _finish_idempotent_response(idempotency, response, accepted_run=True)
            return response
        if terminal_status is None:
            response = _error(
                status_code=502,
                code="agent_runtime_stream_incomplete",
                message="Agent Runtime ended without a terminal event.",
                error_type="server_error",
            )
        else:
            failed = terminal_status != "completed"
            response = JSONResponse(
                _completed_response(
                    response_id=response_id,
                    model=model_id,
                    text="".join(text_parts),
                    input_value=message,
                    status="failed" if failed else "completed",
                    usage=usage,
                    tool_calls=list(tool_calls.values()),
                    requested_tools=tool_config["public_tools"],
                    tool_choice=tool_config["tool_choice"],
                    parallel_tool_calls=tool_config["parallel_tool_calls"],
                    error=(
                        {
                            "code": terminal_status,
                            "message": "The response could not be completed.",
                            "type": "server_error",
                        }
                        if failed
                        else None
                    ),
                )
            )
        if response.status_code >= 500:
            response.headers["x-run-id"] = str(turn.run_id)
            response.headers["x-session-id"] = session_id
        await _finish_idempotent_response(idempotency, response, accepted_run=True)
        return response

    async def stream_events() -> AsyncIterator[bytes]:
        text_parts: list[str] = []
        tool_calls: dict[str, dict[str, Any]] = {}
        usage: dict[str, Any] | None = None
        sequence = 0
        item_id = f"msg_{response_id[5:]}"

        def emit(event_type: str, **payload_out: Any) -> bytes:
            nonlocal sequence
            encoded = _sse(event_type, payload_out, sequence)
            sequence += 1
            return encoded

        in_progress = {
            "id": response_id,
            "object": "response",
            "created_at": int(time.time()),
            "status": "in_progress",
            "model": model_id,
            "output": [],
            "parallel_tool_calls": tool_config["parallel_tool_calls"],
            "tool_choice": tool_config["tool_choice"],
            "tools": tool_config["public_tools"],
            "usage": None,
        }
        yield emit("response.created", response=in_progress)
        yield emit("response.in_progress", response=in_progress)
        yield emit(
            "response.output_item.added",
            output_index=0,
            item={
                "id": item_id,
                "type": "message",
                "role": "assistant",
                "status": "in_progress",
                "content": [],
            },
        )
        yield emit(
            "response.content_part.added",
            item_id=item_id,
            output_index=0,
            content_index=0,
            part={"type": "output_text", "text": "", "annotations": []},
        )
        async for event in _runtime_events(control, turn=turn, user=user, session_id=session_id):
            event_type = str(event.get("event_type") or "")
            data = event.get("data") if isinstance(event.get("data"), dict) else {}
            if event_type == "text_delta" and isinstance(data.get("content"), str):
                text_parts.append(data["content"])
                yield emit(
                    "response.output_text.delta",
                    item_id=item_id,
                    output_index=0,
                    content_index=0,
                    delta=data["content"],
                    logprobs=[],
                )
                continue
            if event_type == "thinking_delta" and isinstance(data.get("content"), str):
                yield emit(
                    "response.reasoning_summary_text.delta",
                    item_id=str(data.get("item_id") or f"reasoning_{response_id[5:]}"),
                    output_index=0,
                    summary_index=int(data.get("summary_index") or 0),
                    delta=data["content"],
                )
                continue
            if event_type == "tool_call_start":
                call = _response_function_call(data)
                if call is None:
                    continue
                call_id = call["call_id"]
                tool_calls[call_id] = call
                output_index = len(tool_calls)
                yield emit(
                    "response.output_item.added",
                    output_index=output_index,
                    item=call,
                )
                arguments = call["arguments"]
                if arguments:
                    yield emit(
                        "response.function_call_arguments.delta",
                        item_id=call_id,
                        output_index=output_index,
                        delta=arguments,
                    )
                yield emit(
                    "response.function_call_arguments.done",
                    item_id=call_id,
                    output_index=output_index,
                    name=call["name"],
                    arguments=arguments,
                )
                continue
            if event_type == "tool_call_result":
                call_id = data.get("tool_call_id") or data.get("call_id")
                if isinstance(call_id, str) and call_id in tool_calls:
                    tool_calls[call_id]["status"] = (
                        "completed"
                        if data.get("status") in {"completed", "succeeded"}
                        else "failed"
                    )
                continue
            if event_type == "tool_call_end":
                call_id = data.get("tool_call_id") or data.get("call_id")
                if isinstance(call_id, str) and call_id in tool_calls:
                    output_index = list(tool_calls).index(call_id) + 1
                    yield emit(
                        "response.output_item.done",
                        output_index=output_index,
                        item=tool_calls[call_id],
                    )
                continue
            if event_type not in {"run_finished", "run_error"}:
                continue
            usage = data.get("usage") if isinstance(data.get("usage"), dict) else None
            text = "".join(text_parts)
            failed = event_type == "run_error"
            item_status = "incomplete" if failed else "completed"
            part = {"type": "output_text", "text": text, "annotations": []}
            yield emit(
                "response.output_text.done",
                item_id=item_id,
                output_index=0,
                content_index=0,
                text=text,
                logprobs=[],
            )
            yield emit(
                "response.content_part.done",
                item_id=item_id,
                output_index=0,
                content_index=0,
                part=part,
            )
            yield emit(
                "response.output_item.done",
                output_index=0,
                item={
                    "id": item_id,
                    "type": "message",
                    "role": "assistant",
                    "status": item_status,
                    "content": [part],
                },
            )
            status = str(data.get("status") or "failed")
            response = _completed_response(
                response_id=response_id,
                model=model_id,
                text=text,
                input_value=message,
                status="failed" if failed else "completed",
                usage=usage,
                tool_calls=list(tool_calls.values()),
                requested_tools=tool_config["public_tools"],
                tool_choice=tool_config["tool_choice"],
                parallel_tool_calls=tool_config["parallel_tool_calls"],
                error=(
                    {
                        "code": status,
                        "message": "The response could not be completed.",
                        "type": "server_error",
                    }
                    if failed
                    else None
                ),
            )
            yield emit("response.failed" if failed else "response.completed", response=response)
            return
        response = _completed_response(
            response_id=response_id,
            model=model_id,
            text="".join(text_parts),
            input_value=message,
            status="failed",
            usage=usage,
            tool_calls=list(tool_calls.values()),
            requested_tools=tool_config["public_tools"],
            tool_choice=tool_config["tool_choice"],
            parallel_tool_calls=tool_config["parallel_tool_calls"],
            error={
                "code": "agent_runtime_stream_incomplete",
                "message": "Agent Runtime ended without a terminal event.",
                "type": "server_error",
            },
        )
        yield emit("response.failed", response=response)

    return StreamingResponse(
        stream_events(),
        media_type="text/event-stream",
        headers={"cache-control": "no-cache", "x-accel-buffering": "no"},
    )


__all__ = ["create_response", "router"]
