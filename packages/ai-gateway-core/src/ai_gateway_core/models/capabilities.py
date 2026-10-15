"""Provider-neutral model capability profiles.

Model identifiers and per-model budgets belong to the declarative catalog,
not to request-building code.  Gateway and Assistant both import this module
so the management UI, persisted rows and provider adapters share one contract.
"""

from __future__ import annotations

import copy
import functools
import json
import re
from collections.abc import Mapping
from dataclasses import dataclass
from importlib.resources import files
from typing import Any

PROFILE_SCHEMA_VERSION = 1
CHAT_COMPLETIONS_WIRE_PROTOCOL = "chat_completions"
RESPONSES_V1_WIRE_PROTOCOL = "responses_v1"
SUPPORTED_WIRE_PROTOCOLS = frozenset({CHAT_COMPLETIONS_WIRE_PROTOCOL, RESPONSES_V1_WIRE_PROTOCOL})
_OPTION_ID_RE = re.compile(r"^[a-z][a-z0-9_-]{0,31}$")
#: ``auto`` is reserved: it always resolves to the profile's default option.
RESERVED_OPTION_IDS = frozenset({"auto"})
_CANONICAL_EFFORTS = frozenset(
    {"none", "minimal", "low", "medium", "high", "xhigh", "adaptive", "max", "ultra"}
)


class ModelCapabilityError(ValueError):
    """A persisted or requested model capability profile is invalid."""


@dataclass(frozen=True)
class ResolvedReasoningOption:
    requested: str
    effective: str
    adapter_id: str
    canonical_effort: str | None
    settings: dict[str, Any]
    fallback_reason: str | None = None


_ADAPTER_SPECS: tuple[dict[str, Any], ...] = (
    {
        "id": "reasoning/none-v1",
        "kind": "reasoning",
        "label": "No reasoning control",
        "settings_schema": {"type": "object", "additionalProperties": False},
    },
    {
        "id": "reasoning/dashscope-thinking-v1",
        "kind": "reasoning",
        "label": "DashScope thinking toggle and budget",
        "settings_schema": {
            "type": "object",
            "properties": {
                "enabled": {"type": "boolean"},
                "budget_tokens": {"type": "integer", "minimum": 1, "maximum": 262144},
            },
            "required": ["enabled"],
            "additionalProperties": False,
        },
    },
    {
        "id": "reasoning/dashscope-responses-effort-v1",
        "kind": "reasoning",
        "label": "DashScope Responses reasoning effort",
        "settings_schema": {
            "type": "object",
            "properties": {
                "effort": {
                    "type": "string",
                    "enum": ["none", "minimal", "medium", "high"],
                },
                "chat_enabled": {"type": "boolean"},
                "chat_budget_tokens": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 262144,
                },
            },
            "required": ["effort", "chat_enabled"],
            "additionalProperties": False,
        },
    },
    {
        "id": "reasoning/deepseek-thinking-effort-v1",
        "kind": "reasoning",
        "label": "DeepSeek thinking toggle and effort",
        "settings_schema": {
            "type": "object",
            "properties": {
                "enabled": {"type": "boolean"},
                "effort": {"type": "string", "enum": ["high", "max"]},
            },
            "required": ["enabled"],
            "additionalProperties": False,
        },
    },
    {
        "id": "reasoning/anthropic-adaptive-v1",
        "kind": "reasoning",
        "label": "Anthropic adaptive thinking",
        "settings_schema": {
            "type": "object",
            "properties": {
                "enabled": {"type": "boolean"},
                "effort": {
                    "type": "string",
                    "enum": ["low", "medium", "high", "xhigh", "max"],
                },
            },
            "required": ["enabled"],
            "additionalProperties": False,
        },
    },
    {
        "id": "reasoning/anthropic-budget-v1",
        "kind": "reasoning",
        "label": "Anthropic fixed thinking budget",
        "settings_schema": {
            "type": "object",
            "properties": {
                "enabled": {"type": "boolean"},
                "budget_tokens": {"type": "integer", "minimum": 1024, "maximum": 65536},
            },
            "required": ["enabled"],
            "additionalProperties": False,
        },
    },
    {
        "id": "reasoning/openai-responses-effort-v1",
        "kind": "reasoning",
        "label": "OpenAI Responses reasoning effort",
        "settings_schema": {
            "type": "object",
            "properties": {
                "effort": {
                    "type": "string",
                    "enum": ["none", "minimal", "low", "medium", "high", "xhigh", "max"],
                }
            },
            "required": ["effort"],
            "additionalProperties": False,
        },
    },
    {
        "id": "reasoning/gemini-thinking-v1",
        "kind": "reasoning",
        "label": "Gemini thinking level or budget",
        "settings_schema": {
            "type": "object",
            "properties": {
                "level": {"type": "string", "enum": ["minimal", "low", "medium", "high"]},
                "budget_tokens": {"type": "integer", "minimum": -1, "maximum": 24576},
                "include_thoughts": {"type": "boolean"},
            },
            "additionalProperties": False,
        },
    },
    {
        "id": "reasoning/xai-effort-v1",
        "kind": "reasoning",
        "label": "xAI reasoning effort",
        "settings_schema": {
            "type": "object",
            "properties": {
                "effort": {"type": "string", "enum": ["none", "low", "medium", "high", "xhigh"]}
            },
            "required": ["effort"],
            "additionalProperties": False,
        },
    },
    {
        "id": "reasoning/openai-compatible-binary-v1",
        "kind": "reasoning",
        "label": "OpenAI-compatible binary thinking",
        "settings_schema": {
            "type": "object",
            "properties": {"enabled": {"type": "boolean"}},
            "required": ["enabled"],
            "additionalProperties": False,
        },
    },
    {
        "id": "cache/none-v1",
        "kind": "prompt_cache",
        "label": "No prompt cache controls",
        "settings_schema": {"type": "object", "additionalProperties": False},
    },
    {
        "id": "cache/automatic-v1",
        "kind": "prompt_cache",
        "label": "Provider automatic prefix cache",
        "settings_schema": {"type": "object", "additionalProperties": False},
    },
    {
        "id": "cache/dashscope-session-v1",
        "kind": "prompt_cache",
        "label": "DashScope Responses session cache",
        "settings_schema": {
            "type": "object",
            "properties": {"enabled": {"type": "boolean"}},
            "required": ["enabled"],
            "additionalProperties": False,
        },
    },
    {
        "id": "cache/anthropic-breakpoint-v1",
        "kind": "prompt_cache",
        "label": "Anthropic explicit cache breakpoints",
        "settings_schema": {"type": "object", "additionalProperties": False},
    },
    {
        "id": "cache/openai-content-breakpoint-v1",
        "kind": "prompt_cache",
        "label": "OpenAI-compatible content cache breakpoint",
        "settings_schema": {"type": "object", "additionalProperties": False},
    },
    {
        "id": "search/none-v1",
        "kind": "native_search",
        "label": "No native search",
        "settings_schema": {"type": "object", "additionalProperties": False},
    },
    {
        "id": "search/openai-web-search-v1",
        "kind": "native_search",
        "label": "OpenAI web search",
        "settings_schema": {"type": "object", "additionalProperties": False},
    },
    {
        "id": "search/anthropic-server-tool-v1",
        "kind": "native_search",
        "label": "Anthropic web search server tool",
        "settings_schema": {
            "type": "object",
            "properties": {"max_uses": {"type": "integer", "minimum": 1, "maximum": 20}},
            "additionalProperties": False,
        },
    },
    {
        "id": "search/gemini-google-search-v1",
        "kind": "native_search",
        "label": "Gemini Google Search",
        "settings_schema": {"type": "object", "additionalProperties": False},
    },
    {
        "id": "search/dashscope-native-v1",
        "kind": "native_search",
        "label": "DashScope native search",
        "settings_schema": {"type": "object", "additionalProperties": False},
    },
)

_ADAPTERS = {str(spec["id"]): spec for spec in _ADAPTER_SPECS}


def list_model_capability_adapters() -> list[dict[str, Any]]:
    return copy.deepcopy(list(_ADAPTER_SPECS))


def get_model_capability_adapter(adapter_id: str) -> dict[str, Any] | None:
    spec = _ADAPTERS.get(str(adapter_id or ""))
    return copy.deepcopy(spec) if spec is not None else None


def safe_model_capability_profile(
    *, supports_tools: bool = True, supports_vision: bool = False
) -> dict[str, Any]:
    inputs = ["text", *(["image"] if supports_vision else [])]
    return {
        "schema_version": PROFILE_SCHEMA_VERSION,
        "wire_protocols": {
            "preferred": CHAT_COMPLETIONS_WIRE_PROTOCOL,
            "supported": [CHAT_COMPLETIONS_WIRE_PROTOCOL],
        },
        "reasoning": {
            "adapter_id": "reasoning/none-v1",
            "default_option": "off",
            "options": [
                {
                    "id": "off",
                    "label": "Off",
                    "aliases": [],
                    "canonical_effort": "none",
                    "settings": {},
                }
            ],
            "visibility": "none",
            "replay_policy": "discard_after_turn",
        },
        "prompt_cache": {"adapter_id": "cache/none-v1", "config": {}},
        "native_search": {"adapter_id": "search/none-v1", "enabled": False, "config": {}},
        "tools": {
            "function_calling": bool(supports_tools),
            "parallel_calls": False,
            "strict_schema": False,
            "namespace_wire": "native",
            "web_search_wire": "disabled",
        },
        "modalities": {"input": inputs, "output": ["text"]},
        "streaming": {
            "text_deltas": True,
            "reasoning_deltas": False,
            "tool_call_deltas": bool(supports_tools),
        },
    }


def _deep_merge(base: Mapping[str, Any], override: Mapping[str, Any]) -> dict[str, Any]:
    result = copy.deepcopy(dict(base))
    for key, value in override.items():
        if isinstance(value, Mapping) and isinstance(result.get(key), Mapping):
            result[key] = _deep_merge(result[key], value)
        else:
            result[key] = copy.deepcopy(value)
    return result


def merge_model_capability_profiles(
    catalog: Mapping[str, Any] | None,
    overrides: Mapping[str, Any] | None,
    *,
    supports_tools: bool = True,
    supports_vision: bool = False,
) -> dict[str, Any]:
    merged = _deep_merge(
        safe_model_capability_profile(
            supports_tools=supports_tools,
            supports_vision=supports_vision,
        ),
        catalog or {},
    )
    merged = _deep_merge(merged, overrides or {})
    return validate_model_capability_profile(merged)


def _validate_settings(adapter_id: str, settings: Mapping[str, Any]) -> None:
    spec = _ADAPTERS.get(adapter_id)
    if spec is None:
        raise ModelCapabilityError(f"unknown capability adapter: {adapter_id}")
    schema = spec["settings_schema"]
    properties = schema.get("properties", {})
    unknown = set(settings) - set(properties)
    if schema.get("additionalProperties") is False and unknown:
        raise ModelCapabilityError(
            f"unsupported {adapter_id} settings: {', '.join(sorted(unknown))}"
        )
    missing = set(schema.get("required", [])) - set(settings)
    if missing:
        raise ModelCapabilityError(f"missing {adapter_id} settings: {', '.join(sorted(missing))}")
    for key, value in settings.items():
        rule = properties.get(key)
        if not rule:
            continue
        expected = rule.get("type")
        if expected == "boolean" and not isinstance(value, bool):
            raise ModelCapabilityError(f"{adapter_id}.{key} must be boolean")
        if expected == "string" and not isinstance(value, str):
            raise ModelCapabilityError(f"{adapter_id}.{key} must be string")
        if expected == "integer" and (not isinstance(value, int) or isinstance(value, bool)):
            raise ModelCapabilityError(f"{adapter_id}.{key} must be integer")
        if "enum" in rule and value not in rule["enum"]:
            raise ModelCapabilityError(f"{adapter_id}.{key} has an unsupported value")
        if (
            isinstance(value, int)
            and not isinstance(value, bool)
            and (value < rule.get("minimum", value) or value > rule.get("maximum", value))
        ):
            raise ModelCapabilityError(f"{adapter_id}.{key} is out of range")


def validate_model_capability_profile(profile: Mapping[str, Any]) -> dict[str, Any]:
    value = copy.deepcopy(dict(profile))
    if value.get("schema_version") != PROFILE_SCHEMA_VERSION:
        raise ModelCapabilityError("unsupported model capability schema_version")

    wire_protocols = value.setdefault(
        "wire_protocols",
        {
            "preferred": CHAT_COMPLETIONS_WIRE_PROTOCOL,
            "supported": [CHAT_COMPLETIONS_WIRE_PROTOCOL],
        },
    )
    if not isinstance(wire_protocols, dict):
        raise ModelCapabilityError("wire_protocols profile must be an object")
    preferred_wire = wire_protocols.get("preferred")
    supported_wires = wire_protocols.get("supported")
    if (
        not isinstance(preferred_wire, str)
        or preferred_wire not in SUPPORTED_WIRE_PROTOCOLS
        or not isinstance(supported_wires, list)
        or not supported_wires
        or len(supported_wires) != len(set(supported_wires))
        or any(
            not isinstance(item, str) or item not in SUPPORTED_WIRE_PROTOCOLS
            for item in supported_wires
        )
        or preferred_wire not in supported_wires
    ):
        raise ModelCapabilityError("wire_protocols profile is invalid")

    reasoning = value.get("reasoning")
    if not isinstance(reasoning, dict):
        raise ModelCapabilityError("reasoning profile must be an object")
    adapter_id = str(reasoning.get("adapter_id") or "")
    adapter = _ADAPTERS.get(adapter_id)
    if adapter is None or adapter["kind"] != "reasoning":
        raise ModelCapabilityError("reasoning adapter is invalid")
    options = reasoning.get("options")
    if not isinstance(options, list) or not 1 <= len(options) <= 16:
        raise ModelCapabilityError("reasoning options must contain between 1 and 16 entries")
    # Collect every declared option id first so alias collisions are caught
    # regardless of declaration order (an alias may not shadow any option id).
    declared_ids: set[str] = set()
    for raw in options:
        if isinstance(raw, dict):
            candidate = str(raw.get("id") or "")
            if _OPTION_ID_RE.fullmatch(candidate):
                declared_ids.add(candidate)
    option_ids: set[str] = set()
    aliases: set[str] = set()
    for raw in options:
        if not isinstance(raw, dict):
            raise ModelCapabilityError("reasoning option must be an object")
        option_id = str(raw.get("id") or "")
        if (
            not _OPTION_ID_RE.fullmatch(option_id)
            or option_id in option_ids
            or option_id in RESERVED_OPTION_IDS
        ):
            raise ModelCapabilityError("reasoning option id is invalid, duplicated, or reserved")
        option_ids.add(option_id)
        label = raw.get("label")
        if not isinstance(label, str) or not 1 <= len(label.strip()) <= 64:
            raise ModelCapabilityError("reasoning option label is invalid")
        canonical = raw.get("canonical_effort")
        if canonical is not None and (
            not isinstance(canonical, str) or canonical not in _CANONICAL_EFFORTS
        ):
            raise ModelCapabilityError("canonical reasoning effort is invalid")
        option_aliases = raw.get("aliases", [])
        if not isinstance(option_aliases, list) or len(option_aliases) > 16:
            raise ModelCapabilityError("reasoning option aliases are invalid")
        for alias in option_aliases:
            if (
                not isinstance(alias, str)
                or not _OPTION_ID_RE.fullmatch(alias)
                or alias in RESERVED_OPTION_IDS
            ):
                raise ModelCapabilityError("reasoning option alias is invalid or reserved")
            if alias in aliases or alias in declared_ids:
                raise ModelCapabilityError("reasoning option alias is duplicated")
            aliases.add(alias)
        settings = raw.get("settings", {})
        if not isinstance(settings, dict):
            raise ModelCapabilityError("reasoning option settings must be an object")
        _validate_settings(adapter_id, settings)
    default_option = reasoning.get("default_option")
    if not isinstance(default_option, str) or default_option not in option_ids:
        raise ModelCapabilityError("reasoning default_option must name an option")
    visibility = reasoning.get("visibility")
    if not isinstance(visibility, str) or visibility not in {"none", "summary", "stream"}:
        raise ModelCapabilityError("reasoning visibility is invalid")
    replay_policy = reasoning.get("replay_policy")
    if not isinstance(replay_policy, str) or replay_policy not in {
        "discard_after_turn",
        "preserve_during_tool_turn",
        "preserve_session",
    }:
        raise ModelCapabilityError("reasoning replay_policy is invalid")

    for key, kind in (("prompt_cache", "prompt_cache"), ("native_search", "native_search")):
        section = value.get(key)
        if not isinstance(section, dict):
            raise ModelCapabilityError(f"{key} profile must be an object")
        section_adapter = str(section.get("adapter_id") or "")
        spec = _ADAPTERS.get(section_adapter)
        if spec is None or spec["kind"] != kind:
            raise ModelCapabilityError(f"{key} adapter is invalid")
        config = section.get("config", {})
        if not isinstance(config, dict):
            raise ModelCapabilityError(f"{key} config must be an object")
        _validate_settings(section_adapter, config)
    if not isinstance(value["native_search"].get("enabled"), bool):
        raise ModelCapabilityError("native_search.enabled must be boolean")

    tools = value.get("tools")
    if isinstance(tools, dict):
        tools.setdefault("namespace_wire", "native")
        tools.setdefault("web_search_wire", "disabled")
    if (
        not isinstance(tools, dict)
        or any(
            not isinstance(tools.get(key), bool)
            for key in ("function_calling", "parallel_calls", "strict_schema")
        )
        or tools.get("namespace_wire") not in {"native", "flatten"}
        or tools.get("web_search_wire") not in {"native", "disabled"}
    ):
        raise ModelCapabilityError("tools capability profile is invalid")
    if tools["web_search_wire"] == "native" and value["native_search"]["enabled"] is not True:
        raise ModelCapabilityError("tools.web_search_wire requires native_search.enabled")
    modalities = value.get("modalities")
    if not isinstance(modalities, dict):
        raise ModelCapabilityError("modalities capability profile is invalid")
    for key in ("input", "output"):
        items = modalities.get(key)
        if (
            not isinstance(items, list)
            or not items
            or any(not isinstance(item, str) for item in items)
        ):
            raise ModelCapabilityError(f"modalities.{key} is invalid")
    streaming = value.get("streaming")
    if not isinstance(streaming, dict) or any(
        not isinstance(streaming.get(key), bool)
        for key in ("text_deltas", "reasoning_deltas", "tool_call_deltas")
    ):
        raise ModelCapabilityError("streaming capability profile is invalid")
    return value


def resolve_reasoning_option(
    profile: Mapping[str, Any], requested: str | None
) -> ResolvedReasoningOption:
    validated = validate_model_capability_profile(profile)
    reasoning = validated["reasoning"]
    requested_id = str(requested or "auto").strip().lower() or "auto"
    lookup: dict[str, dict[str, Any]] = {}
    for option in reasoning["options"]:
        lookup[option["id"]] = option
        for alias in option.get("aliases", []):
            lookup[alias] = option
    fallback_reason = None
    if requested_id == "auto":
        option = lookup[reasoning["default_option"]]
    else:
        option = lookup.get(requested_id)
        if option is None:
            option = lookup[reasoning["default_option"]]
            fallback_reason = "unsupported_reasoning_option"
    return ResolvedReasoningOption(
        requested=requested_id,
        effective=str(option["id"]),
        adapter_id=str(reasoning["adapter_id"]),
        canonical_effort=option.get("canonical_effort"),
        settings=copy.deepcopy(option.get("settings") or {}),
        fallback_reason=fallback_reason,
    )


@functools.lru_cache(maxsize=1)
def _load_catalog() -> tuple[dict[str, Any], ...]:
    # The catalog is immutable packaged data; parse it once.  All consumers
    # receive deep copies (via validate_model_capability_profile), so sharing
    # the parsed structure is safe.
    resource = files(__package__).joinpath("model_capability_catalog.json")
    payload = json.loads(resource.read_text(encoding="utf-8"))
    entries = payload.get("models", [])
    return tuple(entries) if isinstance(entries, list) else ()


def get_builtin_model_capabilities(provider_id: str, model_id: str) -> dict[str, Any] | None:
    provider_key = str(provider_id or "").strip().lower()
    model_key = str(model_id or "").strip().lower()
    for entry in _load_catalog():
        model_keys = {str(entry.get("model_id") or "").lower()}
        model_keys.update(str(item).lower() for item in entry.get("model_ids", []))
        if model_key not in model_keys:
            continue
        provider_keys = {str(item).lower() for item in entry.get("provider_keys", [])}
        if provider_key not in provider_keys:
            continue
        profile = entry.get("capabilities")
        if isinstance(profile, dict):
            return validate_model_capability_profile(profile)
    return None


__all__ = [
    "CHAT_COMPLETIONS_WIRE_PROTOCOL",
    "ModelCapabilityError",
    "RESERVED_OPTION_IDS",
    "RESPONSES_V1_WIRE_PROTOCOL",
    "ResolvedReasoningOption",
    "SUPPORTED_WIRE_PROTOCOLS",
    "get_builtin_model_capabilities",
    "get_model_capability_adapter",
    "list_model_capability_adapters",
    "merge_model_capability_profiles",
    "resolve_reasoning_option",
    "safe_model_capability_profile",
    "validate_model_capability_profile",
]
