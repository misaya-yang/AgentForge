"""Provider-native Responses request and stream authority."""

from __future__ import annotations

import copy
import logging
from collections.abc import AsyncIterator, Mapping
from dataclasses import dataclass
from hashlib import sha256
from typing import Any

from ai_gateway_core.models import ReasoningWireError, apply_reasoning_wire

from ..timing import ModelPlaneTiming
from .authorization import (
    _TOOL_NAME_RE,
    AgentModelPlaneError,
    _AuthorizedCall,
)
from .request_builder import restrict_snapshot_tools
from .stream_limits import provider_lines

logger = logging.getLogger("src.services.agent_runtime.model_plane")


@dataclass(frozen=True, slots=True)
class _ValidatedNativeTools:
    tools: list[dict[str, Any]]
    aliases: dict[str, tuple[str, str]]
    wire_aliases: dict[tuple[str, str], str]


def _function_tool(
    raw: Mapping[str, Any],
    *,
    wire_name: str | None = None,
    description_prefix: str = "",
) -> dict[str, Any]:
    name = raw.get("name")
    description = raw.get("description")
    parameters = raw.get("parameters")
    if (
        not isinstance(name, str)
        or not _TOOL_NAME_RE.fullmatch(name)
        or not isinstance(description, str)
        or len(description) > 16_384
        or not isinstance(parameters, Mapping)
    ):
        raise AgentModelPlaneError("RUNTIME_TOOL_SCHEMA_INVALID", status_code=422)
    tool = {
        "type": "function",
        "name": wire_name or name,
        "description": f"{description_prefix}{description}",
        "parameters": copy.deepcopy(dict(parameters)),
    }
    if isinstance(raw.get("strict"), bool):
        tool["strict"] = raw["strict"]
    return tool


def _namespace_alias(namespace: str, name: str) -> str:
    digest = sha256(f"{namespace}\0{name}".encode()).hexdigest()[:10]
    prefix = f"ns_{digest}_"
    return f"{prefix}{name[: 64 - len(prefix)]}"


def _validated_native_tools(
    value: Any, profile: Mapping[str, Any], *, _helpers: Any
) -> _ValidatedNativeTools:
    if value in (None, []):
        value = []
    if not isinstance(value, list) or len(value) > 256:
        raise AgentModelPlaneError("RUNTIME_TOOL_SCHEMA_INVALID", status_code=422)
    tool_capabilities = profile.get("tools")
    if not isinstance(tool_capabilities, Mapping):
        raise AgentModelPlaneError("RUNTIME_TOOL_CAPABILITY_INVALID", status_code=422)
    namespace_wire = tool_capabilities.get("namespace_wire")
    web_search_wire = tool_capabilities.get("web_search_wire")
    tools: list[dict[str, Any]] = []
    wire_names: set[str] = set()
    aliases: dict[str, tuple[str, str]] = {}
    wire_aliases: dict[tuple[str, str], str] = {}
    bare_namespace_candidates: dict[str, list[tuple[str, str]]] = {}
    for raw in value:
        if not isinstance(raw, Mapping):
            raise AgentModelPlaneError("RUNTIME_TOOL_SCHEMA_INVALID", status_code=422)
        tool_type = raw.get("type")
        if tool_type == "web_search":
            if web_search_wire != "native":
                raise AgentModelPlaneError("RUNTIME_TOOL_SCHEMA_UNSUPPORTED", status_code=422)
            tools.append({"type": "web_search"})
            continue
        if tool_type == "namespace":
            if namespace_wire != "flatten":
                raise AgentModelPlaneError("RUNTIME_TOOL_SCHEMA_UNSUPPORTED", status_code=422)
            namespace = raw.get("name")
            children = raw.get("tools")
            if (
                not isinstance(namespace, str)
                or not _TOOL_NAME_RE.fullmatch(namespace)
                or not isinstance(children, list)
                or len(children) > 256
            ):
                raise AgentModelPlaneError("RUNTIME_TOOL_SCHEMA_INVALID", status_code=422)
            for child in children:
                if not isinstance(child, Mapping) or child.get("type") != "function":
                    raise AgentModelPlaneError(
                        "RUNTIME_TOOL_SCHEMA_UNSUPPORTED",
                        status_code=422,
                    )
                child_name = child.get("name")
                if not isinstance(child_name, str):
                    raise AgentModelPlaneError("RUNTIME_TOOL_SCHEMA_INVALID", status_code=422)
                alias = _helpers._namespace_alias(namespace, child_name)
                if alias in wire_names:
                    raise AgentModelPlaneError("RUNTIME_TOOL_SCHEMA_INVALID", status_code=422)
                wire_names.add(alias)
                identity = (namespace, child_name)
                aliases[alias] = identity
                wire_aliases[identity] = alias
                bare_namespace_candidates.setdefault(child_name, []).append(identity)
                tools.append(
                    _helpers._function_tool(
                        child,
                        wire_name=alias,
                        description_prefix=f"[{namespace}] ",
                    )
                )
            continue
        if tool_type != "function":
            raise AgentModelPlaneError("RUNTIME_TOOL_SCHEMA_UNSUPPORTED", status_code=422)
        name = raw.get("name")
        if not isinstance(name, str) or name in wire_names:
            raise AgentModelPlaneError("RUNTIME_TOOL_SCHEMA_INVALID", status_code=422)
        wire_names.add(name)
        tools.append(_helpers._function_tool(raw))
    if len(tools) > 256:
        raise AgentModelPlaneError("RUNTIME_TOOL_SCHEMA_INVALID", status_code=422)
    # Some OpenAI-compatible Responses providers return the namespace child
    # name instead of the serialized wire alias. Restore it only when the
    # child is unique and cannot collide with a direct function tool.
    for child_name, identities in bare_namespace_candidates.items():
        if len(identities) == 1 and child_name not in wire_names:
            aliases[child_name] = identities[0]
    return _ValidatedNativeTools(
        tools=tools,
        aliases=aliases,
        wire_aliases=wire_aliases,
    )


def _native_tool_transcript(
    value: Any,
    *,
    aliases: Mapping[str, tuple[str, str]],
    wire_aliases: Mapping[tuple[str, str], str],
    _helpers: Any,
) -> Any:
    """Serialize restored namespace calls back to their provider wire names."""

    normalized = _helpers._native_responses_input(value)
    if not isinstance(normalized, list):
        return normalized
    for item in normalized:
        if not isinstance(item, dict) or item.get("type") not in {"function_call", "function_call_output"}:
            continue
        name = item.get("name")
        namespace = item.get("namespace")
        identity: tuple[str, str] | None = None
        if isinstance(namespace, str) and isinstance(name, str):
            identity = (namespace, name)
        elif isinstance(name, str):
            identity = aliases.get(name)
        wire_name = wire_aliases.get(identity) if identity is not None else None
        if wire_name is not None:
            item["name"] = wire_name
            item.pop("namespace", None)
    return normalized


def _native_responses_body(
    body: Mapping[str, Any],
    *,
    model_id: str,
    max_output_tokens: int,
    profile: Mapping[str, Any],
    reasoning_option: str,
    allowed_tool_names: set[str] | None = None,
    tool_choice: str | dict[str, str] = "auto",
    parallel_tool_calls: bool = True,
    native_search_authorized: bool = False,
    _apply_reasoning_wire: Any = apply_reasoning_wire,
    _helpers: Any,
) -> tuple[dict[str, Any], dict[str, tuple[str, str]]]:
    validated_tools = _helpers._validated_native_tools(body.get("tools"), profile)
    serialized_tools = list(validated_tools.tools)
    native_search = profile.get("native_search")
    tool_capabilities = profile.get("tools")
    if (
        native_search_authorized
        and tool_choice != "none"
        and isinstance(native_search, Mapping)
        and native_search.get("enabled") is True
        and isinstance(tool_capabilities, Mapping)
        and tool_capabilities.get("web_search_wire") == "native"
        and not any(tool.get("type") == "web_search" for tool in serialized_tools)
    ):
        # Hosted tools are immutable profile data. Inject them in the one
        # Responses serialization stage so a Thread resume cannot
        # accidentally drop a Provider capability from the outbound request.
        serialized_tools.append({"type": "web_search"})
    raw_input = body.get("input")
    has_tool_transcript = isinstance(raw_input, list) and any(
        isinstance(item, Mapping) and item.get("type") in {"function_call", "function_call_output"}
        for item in raw_input
    )
    if not native_search_authorized:
        serialized_tools = [tool for tool in serialized_tools if tool.get("type") != "web_search"]
    function_tool_names = {
        str(tool["name"]) for tool in serialized_tools if tool.get("type") == "function"
    }
    wire_input = _helpers._native_tool_transcript(
        raw_input,
        aliases=validated_tools.aliases,
        wire_aliases=validated_tools.wire_aliases,
    )
    transcript_prevalidated = False
    allow_function_transcript = bool(function_tool_names)
    if has_tool_transcript and not allow_function_transcript:
        try:
            _helpers._validate_tool_transcript(
                wire_input,
                allowed_tool_names=_helpers.KERNEL_TOOL_TRANSCRIPT_NAMES,
                allowed_namespaced_tools=set(validated_tools.aliases.values()),
            )
        except AgentModelPlaneError:
            pass
        else:
            transcript_prevalidated = True
            allow_function_transcript = True
    _helpers._validate_phase2_responses_input(
        body,
        allow_tool_transcript=allow_function_transcript,
    )
    if has_tool_transcript and not transcript_prevalidated:
        _helpers._validate_tool_transcript(
            wire_input,
            allowed_tool_names=function_tool_names | _helpers.KERNEL_TOOL_TRANSCRIPT_NAMES,
            allowed_namespaced_tools=set(validated_tools.aliases.values()),
        )
    # A required/specific choice applies to the initial model call only. Once
    # the kernel supplies a completed tool transcript, forcing it again would
    # create an unbounded post-tool loop.
    if allowed_tool_names is not None:
        serialized_tools = [
            tool for tool in serialized_tools if tool.get("type") != "function"
            or validated_tools.aliases.get(str(tool["name"]), ("", str(tool["name"])))[1] in allowed_tool_names
        ]
    function_tool_names = {str(tool["name"]) for tool in serialized_tools if tool.get("type") == "function"}
    effective_tool_choice = "auto" if has_tool_transcript and tool_choice != "none" else tool_choice
    effective_parallel_tool_calls = parallel_tool_calls
    if isinstance(effective_tool_choice, dict):
        identity = validated_tools.aliases.get(effective_tool_choice["name"])
        if identity is not None:
            effective_tool_choice = {**effective_tool_choice, "name": validated_tools.wire_aliases[identity]}
    if tool_choice == "none":
        serialized_tools = []
    if (
        isinstance(effective_tool_choice, dict)
        and effective_tool_choice["name"] not in function_tool_names
    ):
        raise AgentModelPlaneError("RUNTIME_TOOL_CHOICE_INVALID", status_code=422)
    if effective_tool_choice == "required" and not function_tool_names:
        raise AgentModelPlaneError("RUNTIME_TOOL_CHOICE_INVALID", status_code=422)
    result: dict[str, Any] = {
        "model": model_id,
        "input": wire_input,
        "stream": True,
        "store": False,
        "max_output_tokens": max_output_tokens,
    }
    instructions = body.get("instructions")
    if isinstance(instructions, str) and instructions:
        result["instructions"] = instructions
    if serialized_tools:
        result["tools"] = serialized_tools
        result["tool_choice"] = effective_tool_choice
        result["parallel_tool_calls"] = effective_parallel_tool_calls
    for key in ("temperature", "top_p"):
        value = body.get(key)
        if isinstance(value, int | float) and not isinstance(value, bool):
            result[key] = value
    _apply_reasoning_wire(result, profile, reasoning_option)
    return result, validated_tools.aliases


async def _stream_native_responses(
    self,
    *,
    call: _AuthorizedCall,
    timing: ModelPlaneTiming,
    body: dict[str, Any],
    profile: Mapping[str, Any],
    reasoning: Mapping[str, Any],
    api_key: str,
    base_url: str,
    allowed_tool_names: set[str] | None = None,
    tool_choice: str | dict[str, str] = "auto",
    parallel_tool_calls: bool = True,
    _helpers: Any,
) -> AsyncIterator[bytes]:
    try:
        provider_body, tool_aliases = _helpers._native_responses_body(
            body,
            model_id=call.model_id,
            max_output_tokens=call.reserved_output_tokens,
            profile=profile,
            reasoning_option=str(reasoning.get("effective_option") or "auto"),
            allowed_tool_names=allowed_tool_names,
            tool_choice=tool_choice,
            parallel_tool_calls=parallel_tool_calls,
            native_search_authorized=_helpers.native_web_search_authorized(call.snapshot),
        )
        provider_body["tools"] = restrict_snapshot_tools(
            provider_body.get("tools", []), call.snapshot, tool_aliases,
        )
        choice = provider_body.get("tool_choice")
        names = {tool.get("name") for tool in provider_body["tools"] if tool.get("type") == "function"}
        if (choice == "required" and not names) or (isinstance(choice, dict) and choice.get("name") not in names):
            raise AgentModelPlaneError("RUNTIME_TOOL_CHOICE_INVALID", status_code=422)
        if not provider_body["tools"]:
            provider_body.pop("tools")
            provider_body.pop("tool_choice", None)
            provider_body.pop("parallel_tool_calls", None)
    except ReasoningWireError:
        await self._fail_call(call.call_id, "reasoning_wire_invalid", dispatched=False)
        raise AgentModelPlaneError(
            "RUNTIME_REASONING_WIRE_INVALID",
            status_code=422,
        ) from None
    except AgentModelPlaneError:
        await self._fail_call(call.call_id, "responses_request_invalid", dispatched=False)
        raise

    _helpers.logger.info(
        "Agent provider dispatch wire=responses model=%s tool_types=%s",
        call.model_id,
        [
            str(tool.get("type") or "")
            for tool in provider_body.get("tools", [])
            if isinstance(tool, Mapping)
        ],
    )

    await self.database.execute(
        """
        UPDATE assistant_runtime_model_calls
           SET status = 'dispatched', dispatched_at = NOW(), updated_at = NOW()
         WHERE call_id = $1 AND status = 'reserved'
        """,
        call.call_id,
    )
    reasoning_profile = profile.get("reasoning")
    reasoning_visibility = (
        str(reasoning_profile.get("visibility") or "none")
        if isinstance(reasoning_profile, Mapping)
        else "none"
    )
    validator = _helpers._NativeResponsesStreamValidator(
        tool_aliases,
        reasoning_visibility=reasoning_visibility,
        allow_tools=any(isinstance(tool, Mapping) for tool in provider_body.get("tools", [])),
        allowed_tool_identities={
            tool_aliases.get(str(tool["name"]), (None, str(tool["name"])))
            for tool in provider_body.get("tools", []) if tool.get("type") == "function"
        },
        allow_native_search=any(tool.get("type") == "web_search" for tool in provider_body.get("tools", [])),
    )
    header_request_id: str | None = None
    # TTFT breakdown: provider connect/headers vs. first usable event. The
    # request-level middleware timing only reports total generation, which
    # hid where first-token latency actually goes.
    timing.note_dispatch()
    dispatch_started = self._clock()
    first_event_logged = False
    async with self.http_client.stream(
        "POST",
        _helpers._responses_url(base_url),
        headers=_helpers._provider_headers(profile, api_key),
        json=provider_body,
    ) as response:
        header_request_id = response.headers.get("x-request-id")
        headers_ms = (self._clock() - dispatch_started) * 1000
        if response.status_code >= 400:
            await response.aread()
            await self._fail_call(
                call.call_id,
                f"provider_http_{response.status_code}",
                dispatched=True,
            )
            raise AgentModelPlaneError("RUNTIME_PROVIDER_REJECTED", status_code=502)
        async for line in provider_lines(response):
            if not line.startswith("data:"):
                continue
            payload = line[5:].strip()
            if not payload:
                continue
            event = validator.consume(payload)
            if event is not None:
                timing.note_first_frame()
                # Match the re-encoded SSE event type line, not payload
                # substrings: a function_call_arguments.delta whose model-
                # generated arguments contain "reasoning" must not stamp
                # first-visible.
                event_type = event.split(b"\n", 1)[0]
                if timing.first_visible is None and event_type in (
                    b"event: response.output_text.delta",
                    b"event: response.reasoning_text.delta",
                    b"event: response.reasoning_summary_text.delta",
                    # A refusal delta is client-visible text on this
                    # wire (chat delivers refusals via delta.content);
                    # omitting it under-measures TTFT for refusal-only
                    # completions.
                    b"event: response.refusal.delta",
                ):
                    timing.note_first_visible()
                if not first_event_logged:
                    first_event_logged = True
                    _helpers.logger.info(
                        "Agent provider TTFT model=%s headers_ms=%.0f "
                        "first_event_ms=%.0f tools=%d payload_bytes=%d",
                        call.model_id,
                        headers_ms,
                        (self._clock() - dispatch_started) * 1000,
                        len(provider_body.get("tools") or []),
                        len(response.request.content),
                    )
                yield event
    terminal = validator.finish()
    await self._complete_call(
        call=call,
        input_tokens=terminal.input_tokens,
        output_tokens=terminal.output_tokens,
        provider_request_id=terminal.provider_request_id or header_request_id,
    )
    self._log_model_plane_timing("responses_v1", call, timing)
    yield terminal.event
    yield b"data: [DONE]\n\n"
