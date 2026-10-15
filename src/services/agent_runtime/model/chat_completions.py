"""Chat Completions compatibility wire and model-stream dispatch."""

from __future__ import annotations

import contextlib
import json
import logging
from collections.abc import AsyncIterator, Mapping
from typing import Any

from ai_gateway_core.models import ReasoningWireError

from ..timing import ModelPlaneTiming
from .authorization import (
    KERNEL_TOOL_TRANSCRIPT_NAMES,
    AgentModelPlaneError,
    _AuthorizedCall,
)
from .request_builder import (
    _content_text,
    _tool_output_text,
    _validate_tool_transcript,
    restrict_snapshot_tools,
)
from .stream_limits import provider_lines

logger = logging.getLogger("src.services.agent_runtime.model_plane")

def _validated_chat_tools(
    raw_tools: Any,
    profile: Mapping[str, Any],
    *,
    allowed_tool_names: set[str] | None,
    _helpers: Any,
):
    # Chat has no namespace containers or hosted search. Use the same
    # reversible aliases as Responses without mutating the pinned profile.
    compatible_tools = (
        [tool for tool in raw_tools if not isinstance(tool, Mapping) or tool.get("type") != "web_search"]
        if isinstance(raw_tools, list) else raw_tools
    )
    tool_profile = profile.get("tools")
    if not isinstance(tool_profile, Mapping):
        raise AgentModelPlaneError("RUNTIME_TOOL_CAPABILITY_INVALID", status_code=422)
    validated = _helpers._validated_native_tools(
        compatible_tools,
        {**profile, "tools": {**tool_profile, "namespace_wire": "flatten"}},
    )
    chat_tools = [
        {"type": "function", "function": {key: value for key, value in tool.items() if key != "type"}}
        for tool in validated.tools
        if allowed_tool_names is None
        or validated.aliases.get(str(tool["name"]), ("", str(tool["name"])))[1] in allowed_tool_names
    ]
    return chat_tools, validated


def _chat_tools_from_runtime(
    raw_tools: Any,
    profile: Mapping[str, Any],
    *,
    allowed_tool_names: set[str] | None,
    _helpers: Any,
) -> list[dict[str, Any]]:
    return _validated_chat_tools(
        raw_tools, profile, allowed_tool_names=allowed_tool_names, _helpers=_helpers,
    )[0]


def _responses_input_to_messages(
    body: Mapping[str, Any],
    *,
    allowed_tool_names: set[str] | None = None,
    _logger: logging.Logger = logger,
    _helpers: Any = None,
) -> list[dict[str, Any]]:
    messages: list[dict[str, Any]] = []
    instructions = body.get("instructions")
    if isinstance(instructions, str) and instructions:
        messages.append({"role": "system", "content": instructions})
    raw_input = body.get("input")
    if isinstance(raw_input, str):
        messages.append({"role": "user", "content": raw_input})
        return messages
    if not isinstance(raw_input, list):
        raise AgentModelPlaneError("RUNTIME_RESPONSES_INPUT_INVALID")

    transcript_tool_names = (
        (allowed_tool_names | KERNEL_TOOL_TRANSCRIPT_NAMES)
        if allowed_tool_names is not None
        else None
    )
    if _helpers is not None:
        _helpers._validate_tool_transcript(
            raw_input,
            allowed_tool_names=transcript_tool_names,
        )
    else:
        _validate_tool_transcript(
            raw_input,
            allowed_tool_names=transcript_tool_names,
            _logger=_logger,
        )

    pending_calls: dict[str, str] = {}
    for item in raw_input:
        if not isinstance(item, Mapping):
            raise AgentModelPlaneError("RUNTIME_RESPONSES_INPUT_INVALID")
        item_type = item.get("type")
        if item_type in {None, "message", "agent_message"}:
            role = "user" if item_type == "agent_message" else item.get("role")
            if role not in {"user", "assistant", "developer", "system"}:
                continue
            raw_content = item.get("content")
            text = (
                _helpers._content_text(raw_content)
                if _helpers is not None
                else _content_text(raw_content)
            )
            images = [
                part.get("image_url")
                for part in raw_content
                if isinstance(part, Mapping) and part.get("type") == "input_image"
            ] if isinstance(raw_content, list) else []
            if images and (
                role != "user"
                or any(not isinstance(url, str) or not url.startswith(("data:image/", "https://")) for url in images)
            ):
                raise AgentModelPlaneError("RUNTIME_IMAGE_INPUT_INVALID")
            if text or images:
                content: str | list[dict[str, Any]] = text
                if images:
                    content = [
                        *([{"type": "text", "text": text}] if text else []),
                        *[{"type": "image_url", "image_url": {"url": url}} for url in images],
                    ]
                messages.append(
                    {"role": "system" if role == "developer" else role, "content": content}
                )
            continue
        # Reasoning items are provider-owned opaque state. They are never
        # converted into model-visible text on a compatibility wire.
        if item_type == "reasoning":
            continue
        if item_type == "function_call":
            call_id = str(item.get("call_id") or "")
            name = str(item.get("name") or "")
            arguments = str(item.get("arguments") or "")
            if not call_id or not name:
                raise AgentModelPlaneError("RUNTIME_TOOL_TRANSCRIPT_INVALID")
            if call_id in pending_calls:
                raise AgentModelPlaneError("RUNTIME_TOOL_TRANSCRIPT_INVALID")
            pending_calls[call_id] = name
            tool_call = {
                "id": call_id, "type": "function",
                "function": {"name": name, "arguments": arguments},
            }
            if messages and messages[-1].get("tool_calls"):
                messages[-1]["tool_calls"].append(tool_call)
            else:
                messages.append({"role": "assistant", "content": "", "tool_calls": [tool_call]})
            continue
        if item_type == "function_call_output":
            call_id = str(item.get("call_id") or "")
            if call_id not in pending_calls:
                raise AgentModelPlaneError("RUNTIME_TOOL_TRANSCRIPT_INVALID")
            messages.append(
                {
                    "role": "tool",
                    "tool_call_id": call_id,
                    "name": pending_calls.pop(call_id),
                    "content": _tool_output_text(item.get("output")),
                }
            )
            continue
    if pending_calls:
        raise AgentModelPlaneError("RUNTIME_TOOL_TRANSCRIPT_INVALID")
    if not messages:
        raise AgentModelPlaneError("RUNTIME_RESPONSES_INPUT_INVALID")
    return messages



async def stream(
    self,
    *,
    body: dict[str, Any],
    turn_metadata: dict[str, Any],
    authorized_call: _AuthorizedCall | None = None,
    _helpers: Any,
) -> AsyncIterator[bytes]:
    call = authorized_call or await self.authorize_and_reserve(
        body=body,
        turn_metadata=turn_metadata,
    )
    # Gateway-owned additive timing (PPR-00): one monotonic clock domain,
    # internal observability only — nothing here enters the public SSE
    # envelope or any API contract. Surface: one structured log line per
    # completed call, keyed by call_id.
    timing = ModelPlaneTiming.start(self._clock)
    provider = await self.provider_service.get_runtime_provider_config(
        call.tenant_id,
        call.provider_id,
    )
    if _helpers._provider_revision(provider.get("updated_at")) != call.provider_revision:
        await self._fail_call(call.call_id, "provider_revision_changed", dispatched=False)
        raise AgentModelPlaneError("RUNTIME_PROVIDER_REVISION_CHANGED", status_code=409)
    api_key = str(provider.get("api_key") or "")
    base_url = str(provider.get("runtime_base_url") or "")
    if not api_key or not base_url:
        await self._fail_call(call.call_id, "provider_unavailable", dispatched=False)
        raise AgentModelPlaneError("RUNTIME_PROVIDER_UNAVAILABLE", status_code=503)
    snapshot_model = call.snapshot.get("model")
    profile = call.snapshot.get("capabilities")
    reasoning = call.snapshot.get("reasoning")
    if (
        not isinstance(snapshot_model, dict)
        or not isinstance(profile, dict)
        or not isinstance(reasoning, dict)
    ):
        await self._fail_call(call.call_id, "snapshot_invalid", dispatched=False)
        raise AgentModelPlaneError("RUNTIME_MODEL_SNAPSHOT_INVALID", status_code=503)
    snapshot_parameters = _helpers._snapshot_parameters(call.snapshot)
    allowed_tool_names, tool_choice, parallel_tool_calls = _helpers._snapshot_responses_tool_controls(
        call.snapshot
    )
    wire_protocol = str(snapshot_model.get("wire_protocol") or "")
    if wire_protocol == "responses_v1":
        try:
            chunks = self._stream_native_responses(
                call=call,
                timing=timing,
                body={**body, **snapshot_parameters},
                profile=profile,
                reasoning=reasoning,
                api_key=api_key,
                base_url=base_url,
                allowed_tool_names=allowed_tool_names,
                tool_choice=tool_choice,
                parallel_tool_calls=parallel_tool_calls,
            )
            async with contextlib.aclosing(chunks):
                async for chunk in chunks:
                    yield chunk
        except AgentModelPlaneError as exc:
            await self._fail_call(call.call_id, exc.code, dispatched=True)
            raise
        finally:
            api_key = ""
            await self._mark_unknown_if_dispatched(call.call_id)
        return
    if wire_protocol != "chat_completions":
        await self._fail_call(call.call_id, "wire_protocol_unsupported", dispatched=False)
        raise AgentModelPlaneError("RUNTIME_PROVIDER_WIRE_UNSUPPORTED", status_code=422)

    try:
        _helpers._validate_phase2_responses_input(body, allow_tool_transcript=True)
        chat_tools, validated_tools = _validated_chat_tools(
            body.get("tools"), profile, allowed_tool_names=allowed_tool_names, _helpers=_helpers,
        )
        wire_input = _helpers._native_tool_transcript(
            body.get("input"), aliases=validated_tools.aliases,
            wire_aliases=validated_tools.wire_aliases,
        )
        messages = _helpers._responses_input_to_messages(
            {**body, "input": wire_input},
            allowed_tool_names={tool["name"] for tool in validated_tools.tools},
        )
    except AgentModelPlaneError:
        await self._fail_call(call.call_id, "chat_request_invalid", dispatched=False)
        raise
    chat_body: dict[str, Any] = {
        "model": call.model_id,
        "messages": messages,
        "stream": True,
        "stream_options": {"include_usage": True},
        "max_tokens": call.reserved_output_tokens,
    }
    raw_input = body.get("input")
    has_tool_transcript = isinstance(raw_input, list) and any(
        isinstance(item, Mapping)
        and item.get("type") in {"function_call", "function_call_output"}
        for item in raw_input
    )
    effective_tool_choice = "auto" if has_tool_transcript and tool_choice != "none" else tool_choice
    effective_parallel_tool_calls = parallel_tool_calls
    if isinstance(effective_tool_choice, dict):
        name = effective_tool_choice["name"]
        identity = validated_tools.aliases.get(name)
        if identity is not None:
            effective_tool_choice = {**effective_tool_choice, "name": validated_tools.wire_aliases[identity]}
    try:
        chat_tools = restrict_snapshot_tools(chat_tools, call.snapshot, validated_tools.aliases, chat=True)
    except AgentModelPlaneError:
        await self._fail_call(call.call_id, "chat_request_invalid", dispatched=False)
        raise
    if effective_tool_choice == "none":
        chat_tools = []
    chat_names = {
        item["function"]["name"]
        for item in chat_tools
        if isinstance(item.get("function"), Mapping)
    }
    if (
        isinstance(effective_tool_choice, dict)
        and effective_tool_choice["name"] not in chat_names
    ):
        await self._fail_call(call.call_id, "chat_request_invalid", dispatched=False)
        raise AgentModelPlaneError("RUNTIME_TOOL_CHOICE_INVALID", status_code=422)
    if effective_tool_choice == "required" and not chat_tools:
        await self._fail_call(call.call_id, "chat_request_invalid", dispatched=False)
        raise AgentModelPlaneError("RUNTIME_TOOL_CHOICE_INVALID", status_code=422)
    if chat_tools:
        chat_body["tools"] = chat_tools
        chat_body["tool_choice"] = (
            {
                "type": "function",
                "function": {"name": effective_tool_choice["name"]},
            }
            if isinstance(effective_tool_choice, dict)
            else effective_tool_choice
        )
        chat_body["parallel_tool_calls"] = effective_parallel_tool_calls
    chat_body.update(snapshot_parameters)
    try:
        _helpers.apply_reasoning_wire(
            chat_body, profile, str(reasoning.get("effective_option") or "auto")
        )
    except ReasoningWireError:
        await self._fail_call(call.call_id, "reasoning_wire_invalid", dispatched=False)
        raise AgentModelPlaneError("RUNTIME_REASONING_WIRE_INVALID", status_code=422) from None

    await self.database.execute(
        """
        UPDATE assistant_runtime_model_calls
           SET status = 'dispatched', dispatched_at = NOW(), updated_at = NOW()
         WHERE call_id = $1 AND status = 'reserved'
        """,
        call.call_id,
    )
    projector = _helpers._ResponsesProjector(
        model_id=call.model_id,
        estimated_input_tokens=call.estimated_input_tokens,
        tool_aliases=validated_tools.aliases,
        allowed_tool_names=chat_names | {
            name for name, identity in validated_tools.aliases.items()
            if validated_tools.wire_aliases[identity] in chat_names
        },
    )
    provider_request_id: str | None = None
    terminal_received = False
    timing.note_dispatch()
    try:
        async with self.http_client.stream(
            "POST",
            _helpers._chat_completions_url(base_url),
            headers=_helpers._provider_headers(profile, api_key),
            json=chat_body,
        ) as response:
            provider_request_id = response.headers.get("x-request-id")
            if response.status_code >= 400:
                await response.aread()
                await self._fail_call(
                    call.call_id,
                    f"provider_http_{response.status_code}",
                    dispatched=True,
                )
                raise AgentModelPlaneError("RUNTIME_PROVIDER_REJECTED", status_code=502)
            yield projector.created()
            async for line in provider_lines(response):
                if not line.startswith("data:"):
                    continue
                payload = line[5:].strip()
                if not payload:
                    continue
                if payload == "[DONE]":
                    terminal_received = True
                    break
                try:
                    event = json.loads(payload)
                except json.JSONDecodeError:
                    raise AgentModelPlaneError(
                        "RUNTIME_PROVIDER_STREAM_INVALID", status_code=502
                    ) from None
                if not isinstance(event, dict):
                    raise AgentModelPlaneError(
                        "RUNTIME_PROVIDER_STREAM_INVALID", status_code=502
                    )
                timing.note_first_frame()
                projector.set_usage(event.get("usage"))
                choices = event.get("choices")
                if not isinstance(choices, list):
                    continue
                for choice in choices:
                    if not isinstance(choice, dict):
                        continue
                    if choice.get("finish_reason") is not None:
                        if choice["finish_reason"] not in {"stop", "tool_calls", "function_call"}:
                            raise AgentModelPlaneError("RUNTIME_PROVIDER_RESPONSE_INCOMPLETE", status_code=502)
                        terminal_received = True
                    delta = choice.get("delta")
                    if not isinstance(delta, dict):
                        continue
                    if delta.get("tool_calls"):
                        for chunk in projector.tool_call_delta(delta["tool_calls"]):
                            yield chunk
                    reasoning_delta = delta.get("reasoning_content")
                    if isinstance(reasoning_delta, str) and reasoning_delta:
                        for chunk in projector.reasoning_delta(reasoning_delta):
                            timing.note_first_visible()
                            yield chunk
                    text_delta = delta.get("content")
                    if isinstance(text_delta, str) and text_delta:
                        for chunk in projector.text_delta(text_delta):
                            timing.note_first_visible()
                            yield chunk
        if not terminal_received:
            raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INCOMPLETE", status_code=502)
        terminal_chunks = projector.complete()
        assert projector.usage is not None
        usage = projector.usage
        input_tokens = int(usage["input_tokens"])
        output_tokens = int(usage["output_tokens"])
        await self._complete_call(
            call=call,
            input_tokens=input_tokens,
            output_tokens=output_tokens,
            provider_request_id=provider_request_id,
        )
        self._log_model_plane_timing("chat_completions", call, timing)
        for chunk in terminal_chunks:
            yield chunk
    except AgentModelPlaneError as exc:
        await self._fail_call(call.call_id, exc.code, dispatched=True)
        raise
    finally:
        # Drop the local reference promptly; never retain tenant credentials
        # in caches, snapshots, exceptions, or telemetry.
        api_key = ""
        await self._mark_unknown_if_dispatched(call.call_id)
