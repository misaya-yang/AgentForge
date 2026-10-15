"""Provider stream validation and Responses SSE projection."""

from __future__ import annotations

import json
import logging
import time
import uuid
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

from .authorization import AgentModelPlaneError

logger = logging.getLogger("src.services.agent_runtime.model_plane")

class _ResponsesProjector:
    def __init__(
        self, *, model_id: str, estimated_input_tokens: int,
        tool_aliases: Mapping[str, tuple[str, str]] | None = None,
        allowed_tool_names: set[str] | None = None,
    ) -> None:
        self.model_id = model_id
        self.tool_aliases = tool_aliases or {}
        self.allowed_tool_names = allowed_tool_names
        self.tool_argument_bytes = 0
        self.estimated_input_tokens = estimated_input_tokens
        self.response_id = f"resp_{uuid.uuid4().hex}"
        self.reasoning_id = f"rs_{uuid.uuid4().hex}"
        self.message_id = f"msg_{uuid.uuid4().hex}"
        self.sequence = 0
        self.reasoning = ""
        self.text = ""
        self.text_bytes = 0
        self.reasoning_bytes = 0
        self.reasoning_open = False
        self.reasoning_closed = False
        self.message_open = False
        self.message_closed = False
        self.tool_calls: dict[int, dict[str, Any]] = {}
        self.tool_call_items_added: set[int] = set()
        self.output: list[dict[str, Any]] = []
        self.usage: dict[str, int] | None = None

    def _event(self, event_type: str, **payload: Any) -> bytes:
        event = {"type": event_type, "sequence_number": self.sequence, **payload}
        self.sequence += 1
        encoded = json.dumps(event, ensure_ascii=False, separators=(",", ":"))
        return f"event: {event_type}\ndata: {encoded}\n\n".encode()

    def created(self) -> bytes:
        return self._event(
            "response.created",
            response={
                "id": self.response_id,
                "object": "response",
                "created_at": int(time.time()),
                "status": "in_progress",
                "model": self.model_id,
                "output": [],
            },
        )

    def reasoning_delta(self, delta: str) -> list[bytes]:
        events: list[bytes] = []
        if not self.reasoning_open:
            self.reasoning_open = True
            events.extend(
                [
                    self._event(
                        "response.output_item.added",
                        output_index=len(self.output),
                        item={
                            "id": self.reasoning_id,
                            "type": "reasoning",
                            "status": "in_progress",
                            "summary": [],
                        },
                    ),
                    self._event(
                        "response.reasoning_summary_part.added",
                        item_id=self.reasoning_id,
                        output_index=len(self.output),
                        summary_index=0,
                        part={"type": "summary_text", "text": ""},
                    ),
                ]
            )
        self.reasoning_bytes += len(delta.encode("utf-8"))
        if self.reasoning_bytes > 8 * 1024 * 1024:
            raise AgentModelPlaneError("RUNTIME_PROVIDER_TEXT_LIMIT", status_code=502)
        self.reasoning += delta
        events.append(
            self._event(
                "response.reasoning_summary_text.delta",
                item_id=self.reasoning_id,
                output_index=len(self.output),
                summary_index=0,
                delta=delta,
            )
        )
        return events

    def close_reasoning(self) -> list[bytes]:
        if not self.reasoning_open or self.reasoning_closed:
            return []
        index = len(self.output)
        item = {
            "id": self.reasoning_id,
            "type": "reasoning",
            "status": "completed",
            "summary": [{"type": "summary_text", "text": self.reasoning}],
        }
        self.output.append(item)
        self.reasoning_closed = True
        return [
            self._event(
                "response.reasoning_summary_text.done",
                item_id=self.reasoning_id,
                output_index=index,
                summary_index=0,
                text=self.reasoning,
            ),
            self._event(
                "response.reasoning_summary_part.done",
                item_id=self.reasoning_id,
                output_index=index,
                summary_index=0,
                part={"type": "summary_text", "text": self.reasoning},
            ),
            self._event("response.output_item.done", output_index=index, item=item),
        ]

    def text_delta(self, delta: str) -> list[bytes]:
        events = self.close_reasoning()
        if not self.message_open:
            self.message_open = True
            index = len(self.output)
            events.extend(
                [
                    self._event(
                        "response.output_item.added",
                        output_index=index,
                        item={
                            "id": self.message_id,
                            "type": "message",
                            "status": "in_progress",
                            "role": "assistant",
                            "content": [],
                        },
                    ),
                    self._event(
                        "response.content_part.added",
                        item_id=self.message_id,
                        output_index=index,
                        content_index=0,
                        part={"type": "output_text", "text": "", "annotations": []},
                    ),
                ]
            )
        self.text_bytes += len(delta.encode("utf-8"))
        if self.text_bytes > 8 * 1024 * 1024:
            raise AgentModelPlaneError("RUNTIME_PROVIDER_TEXT_LIMIT", status_code=502)
        self.text += delta
        events.append(
            self._event(
                "response.output_text.delta",
                item_id=self.message_id,
                output_index=len(self.output),
                content_index=0,
                delta=delta,
                logprobs=[],
            )
        )
        return events

    def close_message(self) -> list[bytes]:
        if not self.message_open or self.message_closed:
            return []
        index = len(self.output)
        part = {"type": "output_text", "text": self.text, "annotations": []}
        item = {
            "id": self.message_id,
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [part],
        }
        self.output.append(item)
        self.message_closed = True
        return [
            self._event(
                "response.output_text.done",
                item_id=self.message_id,
                output_index=index,
                content_index=0,
                text=self.text,
                logprobs=[],
            ),
            self._event(
                "response.content_part.done",
                item_id=self.message_id,
                output_index=index,
                content_index=0,
                part=part,
            ),
            self._event("response.output_item.done", output_index=index, item=item),
        ]

    def tool_call_delta(self, raw_calls: Any) -> list[bytes]:
        if not isinstance(raw_calls, list):
            raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502)
        events = self.close_reasoning()
        for raw in raw_calls:
            if not isinstance(raw, Mapping):
                raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502)
            index = raw.get("index", 0)
            if isinstance(index, bool) or not isinstance(index, int) or index < 0:
                raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502)
            function = raw.get("function")
            if not isinstance(function, Mapping):
                raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502)
            if index not in self.tool_calls and len(self.tool_calls) >= 256:
                raise AgentModelPlaneError("RUNTIME_PROVIDER_TOOL_LIMIT", status_code=502)
            call = self.tool_calls.setdefault(
                index,
                {
                    "id": "",
                    "type": "function_call",
                    "status": "in_progress",
                    "call_id": "",
                    "name": "",
                    "arguments": "",
                },
            )
            call_id = raw.get("id")
            if call_id is not None:
                if (not isinstance(call_id, str) or not call_id or len(call_id) > 255
                        or (call["call_id"] and call["call_id"] != call_id)
                        or any(other is not call and other["call_id"] == call_id for other in self.tool_calls.values())):
                    raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502)
                call["id"] = call["call_id"] = call_id
            name = function.get("name")
            if name is not None:
                from .. import model_plane as facade

                if not isinstance(name, str):
                    raise AgentModelPlaneError("RUNTIME_TOOL_SCHEMA_INVALID", status_code=422)
                previous_name = call.get("_wire_name", "")
                wire_name = previous_name if name == previous_name else previous_name + name
                if not facade._TOOL_NAME_RE.fullmatch(wire_name):
                    raise AgentModelPlaneError("RUNTIME_TOOL_SCHEMA_INVALID", status_code=422)
                if index in self.tool_call_items_added and wire_name != previous_name:
                    raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502)
                if self.allowed_tool_names is not None and not any(allowed.startswith(wire_name) for allowed in self.allowed_tool_names):
                    raise AgentModelPlaneError("RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH", status_code=502)
                call["_wire_name"] = wire_name
            arguments = function.get("arguments", "")
            if arguments:
                if not isinstance(arguments, str):
                    raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502)
                size = len(arguments.encode("utf-8"))
                call_size = int(call.get("_argument_bytes", 0)) + size
                if call_size > 1024 * 1024 or self.tool_argument_bytes + size > 4 * 1024 * 1024:
                    raise AgentModelPlaneError("RUNTIME_PROVIDER_TOOL_ARGUMENTS_LIMIT", status_code=502)
                call["_argument_bytes"] = call_size
                self.tool_argument_bytes += size
                call["arguments"] += arguments
                call["_pending_arguments"] = call.get("_pending_arguments", "") + arguments
            if call["call_id"] and call.get("_wire_name") and call.get("_pending_arguments"):
                events.extend(self._open_tool_call(index, call))
                events.append(self._event(
                    "response.function_call_arguments.delta", item_id=call["call_id"],
                    output_index=len(self.output) + index, delta=call.pop("_pending_arguments"),
                ))
        return events

    def _open_tool_call(self, index: int, call: dict[str, Any]) -> list[bytes]:
        if index in self.tool_call_items_added:
            return []
        name = call.get("_wire_name")
        if not call["call_id"] or not name:
            raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502)
        if self.allowed_tool_names is not None and name not in self.allowed_tool_names:
            raise AgentModelPlaneError("RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH", status_code=502)
        identity = self.tool_aliases.get(name)
        call["name"] = identity[1] if identity else name
        if identity:
            call["namespace"] = identity[0]
        self.tool_call_items_added.add(index)
        item = {key: value for key, value in call.items() if not key.startswith("_")}
        item["arguments"] = ""
        return [self._event("response.output_item.added", output_index=len(self.output) + index, item=item)]

    def close_tool_calls(self) -> list[bytes]:
        events: list[bytes] = []
        base_index = len(self.output)
        for index, call in sorted(self.tool_calls.items()):
            events.extend(self._open_tool_call(index, call))
            output_index = base_index + index
            call = {key: value for key, value in call.items() if not key.startswith("_")}
            events.extend(
                [
                    self._event(
                        "response.function_call_arguments.done",
                        item_id=call["call_id"],
                        output_index=output_index,
                        name=call["name"],
                        arguments=call["arguments"],
                    ),
                    self._event(
                        "response.output_item.done",
                        output_index=output_index,
                        item={**call, "status": "completed"},
                    ),
                ]
            )
            self.output.append({**call, "status": "completed"})
        return events

    def set_usage(self, raw: Any) -> None:
        if not isinstance(raw, Mapping):
            return
        input_tokens = raw.get("prompt_tokens", raw.get("input_tokens"))
        output_tokens = raw.get("completion_tokens", raw.get("output_tokens"))
        if (
            isinstance(input_tokens, int)
            and not isinstance(input_tokens, bool)
            and input_tokens >= 0
            and isinstance(output_tokens, int)
            and not isinstance(output_tokens, bool)
            and output_tokens >= 0
        ):
            self.usage = {"input_tokens": input_tokens, "output_tokens": output_tokens}

    def complete(self) -> list[bytes]:
        from .. import model_plane as facade

        events = self.close_reasoning()
        events.extend(self.close_tool_calls())
        events.extend(self.close_message())
        if not self.text and not self.tool_calls:
            raise AgentModelPlaneError("RUNTIME_PROVIDER_EMPTY_RESPONSE", status_code=502)
        usage = self.usage or {
            "input_tokens": self.estimated_input_tokens,
            "output_tokens": facade._estimate_tokens(self.text),
        }
        usage = {
            **usage,
            "total_tokens": usage["input_tokens"] + usage["output_tokens"],
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens_details": {"reasoning_tokens": 0},
        }
        self.usage = usage
        events.append(
            self._event(
                "response.completed",
                response={
                    "id": self.response_id,
                    "object": "response",
                    "created_at": int(time.time()),
                    "status": "completed",
                    "error": None,
                    "incomplete_details": None,
                    "model": self.model_id,
                    "output": self.output,
                    "usage": usage,
                },
            )
        )
        events.append(b"data: [DONE]\n\n")
        return events


@dataclass(frozen=True, slots=True)
class _NativeResponsesTerminal:
    event: bytes
    input_tokens: int
    output_tokens: int
    provider_request_id: str | None


class _NativeResponsesStreamValidator:
    """Validate provider-native Responses before exposing terminal state."""

    def __init__(
        self,
        tool_aliases: Mapping[str, tuple[str, str]] | None = None,
        *,
        reasoning_visibility: str = "none",
        allow_tools: bool = False,
        allowed_tool_identities: set[tuple[str | None, str]] | None = None,
        allow_native_search: bool = False,
    ) -> None:
        self.last_sequence = -1
        self.seen_created = False
        self.terminal: _NativeResponsesTerminal | None = None
        self.tool_aliases = dict(tool_aliases or {})
        self.reasoning_visibility = reasoning_visibility
        self.allow_tools = allow_tools
        self.allowed_tool_identities = allowed_tool_identities
        self.allow_native_search = allow_native_search
        self.argument_bytes: dict[str, int] = {}
        self.total_argument_bytes = 0

    def _normalize_reasoning_event(self, event: dict[str, Any]) -> None:
        event_type = event.get("type")
        if self.reasoning_visibility != "stream" or event_type not in {
            "response.reasoning_text.delta",
            "response.reasoning_text.done",
        }:
            return
        normalized = event_type.replace(
            "response.reasoning_text",
            "response.reasoning_summary_text",
        )
        event["type"] = normalized
        event.setdefault("summary_index", 0)

    def _restore_tool_namespace(self, item: Any) -> None:
        if not isinstance(item, dict) or item.get("type") != "function_call":
            return
        alias = item.get("name")
        resolved = self.tool_aliases.get(alias) if isinstance(alias, str) else None
        if resolved is not None:
            namespace, name = resolved
            if item.get("namespace") not in (None, "", namespace):
                raise AgentModelPlaneError("RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH", status_code=502)
            item["name"] = name
            item["namespace"] = namespace

    @staticmethod
    def _encoded(event_type: str, event: Mapping[str, Any]) -> bytes:
        payload = json.dumps(event, ensure_ascii=False, separators=(",", ":"))
        return f"event: {event_type}\ndata: {payload}\n\n".encode()

    @staticmethod
    def _usage(response: Mapping[str, Any]) -> tuple[int, int]:
        usage = response.get("usage")
        if not isinstance(usage, Mapping):
            raise AgentModelPlaneError("RUNTIME_PROVIDER_USAGE_INVALID", status_code=502)
        input_tokens = usage.get("input_tokens")
        output_tokens = usage.get("output_tokens")
        if any(
            isinstance(value, bool) or not isinstance(value, int) or value < 0
            for value in (input_tokens, output_tokens)
        ):
            raise AgentModelPlaneError("RUNTIME_PROVIDER_USAGE_INVALID", status_code=502)
        return input_tokens, output_tokens

    def _validate_tool_identity(self, item: Any) -> None:
        if not isinstance(item, Mapping) or self.allowed_tool_identities is None:
            return
        if item.get("type") == "function_call":
            arguments = item.get("arguments")
            if isinstance(arguments, str) and len(arguments.encode("utf-8")) > 1024 * 1024:
                raise AgentModelPlaneError("RUNTIME_PROVIDER_TOOL_ARGUMENTS_LIMIT", status_code=502)
            namespace = item.get("namespace") or None
            name = item.get("name")
            if (not isinstance(name, str) or (namespace is not None and not isinstance(namespace, str))
                    or (namespace, name) not in self.allowed_tool_identities):
                raise AgentModelPlaneError("RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH", status_code=502)
        elif item.get("type") == "web_search_call" and not self.allow_native_search:
            raise AgentModelPlaneError("RUNTIME_PROVIDER_TOOL_SCOPE_MISMATCH", status_code=502)

    def _reject_tool_item(self, event: Mapping[str, Any]) -> None:
        item = event.get("item")
        self._validate_tool_identity(item)
        allowed = {None, "message", "reasoning"}
        if self.allow_tools:
            allowed.update({"function_call", "web_search_call"})
        if isinstance(item, Mapping) and item.get("type") not in allowed:
            raise AgentModelPlaneError(
                "RUNTIME_TOOLS_NOT_ENABLED_FOR_PHASE",
                status_code=502,
            )

    def consume(self, payload: str) -> bytes | None:
        if payload == "[DONE]":
            return None
        try:
            event = json.loads(payload)
        except json.JSONDecodeError:
            raise AgentModelPlaneError(
                "RUNTIME_PROVIDER_STREAM_INVALID",
                status_code=502,
            ) from None
        if not isinstance(event, dict):
            raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502)
        self._normalize_reasoning_event(event)
        event_type = event.get("type")
        sequence = event.get("sequence_number")
        if (
            not isinstance(event_type, str)
            or isinstance(sequence, bool)
            or not isinstance(sequence, int)
            or sequence != self.last_sequence + 1
            or self.terminal is not None
        ):
            raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502)
        self.last_sequence = sequence
        if not self.seen_created:
            if event_type != "response.created":
                raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502)
            self.seen_created = True
        if event_type in {
            "error",
            "response.failed",
            "response.incomplete",
            "response.cancelled",
        }:
            error = event.get("error")
            if not isinstance(error, Mapping):
                response = event.get("response")
                error = response.get("error") if isinstance(response, Mapping) else None
            error = error if isinstance(error, Mapping) else {}
            from .. import model_plane as facade

            facade.logger.warning(
                "Agent provider stream rejected event=%s type=%s code=%s param=%s message=%s",
                event_type,
                facade.redact_sensitive_text(str(error.get("type") or ""))[:128],
                facade.redact_sensitive_text(str(error.get("code") or ""))[:128],
                facade.redact_sensitive_text(str(error.get("param") or ""))[:128],
                facade.redact_sensitive_text(str(error.get("message") or ""))[:512],
            )
            raise AgentModelPlaneError("RUNTIME_PROVIDER_REJECTED", status_code=502)
        if (
            any(marker in event_type for marker in ("function_call", "_search_call", "mcp_call"))
            and not self.allow_tools
        ):
            raise AgentModelPlaneError(
                "RUNTIME_TOOLS_NOT_ENABLED_FOR_PHASE",
                status_code=502,
            )
        if event_type == "response.function_call_arguments.delta":
            item_id, delta = event.get("item_id"), event.get("delta")
            if not isinstance(item_id, str) or not item_id or len(item_id) > 255 or not isinstance(delta, str):
                raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502)
            size = len(delta.encode("utf-8"))
            count = self.argument_bytes.get(item_id, 0) + size
            if count > 1024 * 1024 or self.total_argument_bytes + size > 4 * 1024 * 1024:
                raise AgentModelPlaneError("RUNTIME_PROVIDER_TOOL_ARGUMENTS_LIMIT", status_code=502)
            if item_id not in self.argument_bytes and len(self.argument_bytes) >= 256:
                raise AgentModelPlaneError("RUNTIME_PROVIDER_TOOL_LIMIT", status_code=502)
            self.argument_bytes[item_id] = count
            self.total_argument_bytes += size
        self._restore_tool_namespace(event.get("item"))
        self._reject_tool_item(event)
        encoded = self._encoded(event_type, event)
        if event_type != "response.completed":
            return encoded
        response = event.get("response")
        if not isinstance(response, Mapping) or response.get("status") != "completed":
            raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INVALID", status_code=502)
        output = response.get("output")
        if isinstance(output, list):
            for item in output:
                self._restore_tool_namespace(item)
                self._validate_tool_identity(item)
                allowed = {"message", "reasoning"}
                if self.allow_tools:
                    allowed.update({"function_call", "web_search_call"})
                if isinstance(item, Mapping) and item.get("type") not in allowed:
                    raise AgentModelPlaneError(
                        "RUNTIME_TOOLS_NOT_ENABLED_FOR_PHASE",
                        status_code=502,
                    )
        input_tokens, output_tokens = self._usage(response)
        response_id = response.get("id")
        self.terminal = _NativeResponsesTerminal(
            event=self._encoded(event_type, event),
            input_tokens=input_tokens,
            output_tokens=output_tokens,
            provider_request_id=response_id if isinstance(response_id, str) else None,
        )
        return None

    def finish(self) -> _NativeResponsesTerminal:
        if not self.seen_created or self.terminal is None:
            raise AgentModelPlaneError("RUNTIME_PROVIDER_STREAM_INCOMPLETE", status_code=502)
        return self.terminal
