"""Generic provider request normalization and transcript validation."""

from __future__ import annotations

import copy
import logging
from collections.abc import Mapping
from typing import Any
from urllib.parse import urlsplit, urlunsplit

from .authorization import _TOOL_NAME_RE, AgentModelPlaneError, _is_allowed_tool_identity

logger = logging.getLogger("src.services.agent_runtime.model_plane")


def _chat_completions_url(base_url: str) -> str:
    parsed = urlsplit(base_url.rstrip("/"))
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise AgentModelPlaneError("RUNTIME_PROVIDER_ENDPOINT_INVALID", status_code=503)
    path = parsed.path.rstrip("/")
    if path.endswith("/chat/completions"):
        return urlunsplit(parsed)
    return urlunsplit(parsed._replace(path=f"{path}/chat/completions"))


def _responses_url(base_url: str) -> str:
    parsed = urlsplit(base_url.rstrip("/"))
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise AgentModelPlaneError("RUNTIME_PROVIDER_ENDPOINT_INVALID", status_code=503)
    path = parsed.path.rstrip("/")
    if path.endswith("/responses"):
        return urlunsplit(parsed)
    if path.endswith("/v1"):
        return urlunsplit(parsed._replace(path=f"{path}/responses"))
    return urlunsplit(parsed._replace(path=f"{path}/v1/responses"))


def _validate_phase2_responses_input(
    body: Mapping[str, Any], *, allow_tool_transcript: bool = False
) -> None:
    raw_input = body.get("input")
    if isinstance(raw_input, str):
        if not raw_input:
            raise AgentModelPlaneError("RUNTIME_RESPONSES_INPUT_INVALID")
        return
    if not isinstance(raw_input, list) or not raw_input:
        raise AgentModelPlaneError("RUNTIME_RESPONSES_INPUT_INVALID")
    for item in raw_input:
        if not isinstance(item, Mapping):
            raise AgentModelPlaneError("RUNTIME_RESPONSES_INPUT_INVALID")
        item_type = item.get("type")
        if item_type in {"additional_tools", "configuration_update"}:
            # Responses Lite and reasoning overrides need a dedicated adapter;
            # embedded controls cannot bypass the immutable turn snapshot.
            raise AgentModelPlaneError("RUNTIME_INPUT_CONTROL_UNSUPPORTED", status_code=422)
        if item_type in {"function_call", "function_call_output"} and not allow_tool_transcript:
            raise AgentModelPlaneError("RUNTIME_TOOLS_NOT_ENABLED_FOR_PHASE", status_code=422)


def _provider_headers(profile: Mapping[str, Any], api_key: str) -> dict[str, str]:
    headers = {"Authorization": f"Bearer {api_key}", "content-type": "application/json"}
    prompt_cache = profile.get("prompt_cache")
    if isinstance(prompt_cache, Mapping):
        adapter_id = prompt_cache.get("adapter_id")
        config = prompt_cache.get("config")
        if (
            adapter_id == "cache/dashscope-session-v1"
            and isinstance(config, Mapping)
            and config.get("enabled") is True
        ):
            headers["x-dashscope-session-cache"] = "enable"
    return headers


def _content_text(value: Any) -> str:
    if isinstance(value, str):
        return value
    if not isinstance(value, list):
        return ""
    parts: list[str] = []
    for part in value:
        if not isinstance(part, Mapping):
            continue
        part_type = part.get("type")
        if part_type in {"input_text", "output_text", "text", "encrypted_content"}:
            text = (
                part.get("encrypted_content")
                if part_type == "encrypted_content"
                else part.get("text")
            )
            if isinstance(text, str):
                parts.append(text)
    return "".join(parts)


def _native_responses_input(value: Any) -> Any:
    """Expose internal collaboration payloads on non-OpenAI Responses wires."""

    if not isinstance(value, list):
        return copy.deepcopy(value)
    normalized: list[Any] = []
    for raw_item in value:
        item = copy.deepcopy(raw_item)
        if not isinstance(item, dict) or not isinstance(item.get("content"), list):
            normalized.append(item)
            continue
        content: list[Any] = []
        for raw_part in item["content"]:
            if (
                isinstance(raw_part, Mapping)
                and raw_part.get("type") == "encrypted_content"
                and isinstance(raw_part.get("encrypted_content"), str)
            ):
                content.append({"type": "input_text", "text": raw_part["encrypted_content"]})
            else:
                content.append(raw_part)
        item["content"] = content
        # Collaboration messages are kernel-internal input items.  A provider
        # Responses endpoint does not understand that item type, so expose the
        # already-authorized payload as an ordinary user message on the wire.
        if item.get("type") == "agent_message":
            item = {"type": "message", "role": "user", "content": content}
        normalized.append(item)
    return normalized


def _tool_output_text(value: Any) -> str | None:
    if isinstance(value, str):
        return value
    if isinstance(value, list) and all(
        isinstance(part, Mapping) and part.get("type") in {"input_text", "output_text", "text"}
        and isinstance(part.get("text"), str) for part in value
    ):
        return "".join(part["text"] for part in value)
    return None


def _is_replayable_unsupported_tool_result(name: Any, namespace: Any, output: Any) -> bool:
    """Recognize the exact failure emitted by Codex for an unknown Web tool.

    The Web runtime must not gain permission to execute an unadvertised tool, but
    Codex records an attempted unknown tool plus a paired failure before asking
    the model to continue.  That completed failure is safe transcript history;
    accepting any other output would weaken the dynamic tool allowlist.
    """

    if not isinstance(name, str) or not isinstance(output, str):
        return False
    if namespace not in (None, ""):
        return False
    return output in (
        f"unsupported call: {name}",
        f"unsupported custom tool call: {name}",
    )


def _validate_tool_transcript(
    raw_input: list[Any],
    *,
    allowed_tool_names: set[str] | None = None,
    allowed_namespaced_tools: set[tuple[str, str]] | None = None,
    _logger: logging.Logger = logger,
    _helpers: Any = None,
) -> None:
    identity_allowed = (
        _helpers._is_allowed_tool_identity if _helpers is not None else _is_allowed_tool_identity
    )
    pending_calls: dict[str, tuple[str, Any, bool]] = {}
    completed_calls: set[str] = set()
    for item in raw_input:
        if not isinstance(item, Mapping):
            continue
        item_type = item.get("type")
        if item_type == "function_call":
            call_id = item.get("call_id")
            name = item.get("name")
            namespace = item.get("namespace")
            arguments = item.get("arguments")
            allowed_identity = identity_allowed(
                name,
                namespace,
                allowed_tool_names=allowed_tool_names,
                allowed_namespaced_tools=allowed_namespaced_tools,
            )
            if (
                not isinstance(call_id, str)
                or not call_id
                or not isinstance(name, str)
                or not _TOOL_NAME_RE.fullmatch(name)
                or not isinstance(arguments, str)
                or call_id in pending_calls
                or call_id in completed_calls
            ):
                # Names and identities only — arguments can carry user content.
                _logger.warning(
                    "Tool transcript rejected: function_call name=%s namespace=%s "
                    "identity_allowed=%s duplicate=%s arguments_str=%s",
                    str(name)[:64],
                    str(namespace)[:64],
                    allowed_identity,
                    call_id in pending_calls or call_id in completed_calls,
                    isinstance(arguments, str),
                )
                raise AgentModelPlaneError("RUNTIME_TOOL_TRANSCRIPT_INVALID", status_code=422)
            pending_calls[call_id] = (name, namespace, allowed_identity)
        elif item_type == "function_call_output":
            call_id = item.get("call_id")
            output = item.get("output")
            if (
                not isinstance(call_id, str)
                or call_id not in pending_calls
                or _tool_output_text(output) is None
            ):
                _logger.warning(
                    "Tool transcript rejected: function_call_output has_call=%s output_str=%s",
                    isinstance(call_id, str) and call_id in pending_calls,
                    isinstance(item.get("output"), str),
                )
                raise AgentModelPlaneError("RUNTIME_TOOL_TRANSCRIPT_INVALID", status_code=422)
            name, namespace, allowed_identity = pending_calls[call_id]
            if (item.get("name") is not None and item["name"] != name) or (
                item.get("namespace") is not None and item["namespace"] != namespace
            ):
                raise AgentModelPlaneError("RUNTIME_TOOL_TRANSCRIPT_INVALID", status_code=422)
            if not allowed_identity and not _is_replayable_unsupported_tool_result(
                name, namespace, output
            ):
                _logger.warning(
                    "Tool transcript rejected: unadvertised function_call name=%s "
                    "did not have the canonical unsupported result",
                    name[:64],
                )
                raise AgentModelPlaneError("RUNTIME_TOOL_TRANSCRIPT_INVALID", status_code=422)
            pending_calls.pop(call_id)
            completed_calls.add(call_id)
    if pending_calls:
        _logger.warning(
            "Tool transcript rejected: %d function_call item(s) without output (%s)",
            len(pending_calls),
            ",".join(sorted(call[0] for call in pending_calls.values()))[:200],
        )
        raise AgentModelPlaneError("RUNTIME_TOOL_TRANSCRIPT_INVALID", status_code=422)


def restrict_snapshot_tools(
    tools: list[dict[str, Any]], snapshot: Mapping[str, Any],
    aliases: Mapping[str, tuple[str, str]], *, chat: bool = False,
) -> list[dict[str, Any]]:
    """Apply the turn selection after transcript validation, before dispatch.

    History may contain completed calls from an earlier, broader turn. Their
    schemas can validate replay without granting any new call this turn.
    """
    policy = snapshot.get("tool_policy")
    if policy is None:  # Existing immutable snapshots predate the host ceiling.
        return tools
    entries = policy.get("allowedTools") if isinstance(policy, Mapping) else None
    if not isinstance(entries, list):
        raise AgentModelPlaneError("RUNTIME_MODEL_SNAPSHOT_INVALID", status_code=503)
    allowed: set[tuple[str | None, str]] = set()
    for entry in entries:
        if (not isinstance(entry, Mapping) or not isinstance(entry.get("name"), str)
                or (entry.get("namespace") is not None and not isinstance(entry["namespace"], str))):
            raise AgentModelPlaneError("RUNTIME_MODEL_SNAPSHOT_INVALID", status_code=503)
        namespace = entry.get("namespace")
        allowed.add((None if namespace in (None, "", "functions") else namespace, entry["name"]))
    result = []
    for tool in tools:
        definition = tool.get("function", {}) if chat else tool
        name = "web_search" if tool.get("type") == "web_search" else definition.get("name")
        identity = aliases.get(name, (None, name)) if isinstance(name, str) else None
        if identity in allowed:
            result.append(tool)
    return result
