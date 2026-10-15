"""Pure, versioned resolved launch contract for every Agent entry point."""

from __future__ import annotations

import copy
import json
import math
import re
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Final

from .agent_runtime import canonical_runtime_json

RESOLVED_AGENT_LAUNCH_SCHEMA_VERSION: Final = "resolved-agent-launch/v1"

_TOP_KEYS: Final = frozenset(
    {
        "schema_version",
        "identity",
        "agent_spec",
        "model",
        "model_profile",
        "capability_bindings",
        "knowledge_bindings",
        "memory_policy",
        "channel_policy",
        "runtime_inputs",
        "turn_policy",
        "fingerprints",
    }
)
_IDENTITY_KEYS: Final = frozenset(
    {
        "tenant_id",
        "user_id",
        "session_id",
        "agent_id",
        "agent_version_id",
        "draft_revision",
        "publication_id",
        "channel",
        "entrypoint",
        "auth_mode",
    }
)
_AGENT_SPEC_KEYS: Final = frozenset(
    {
        "agentId",
        "agentVersionId",
        "channel",
        "developerInstructions",
        "model",
        "knowledge",
        "capabilities",
        "memory",
    }
)
_MODEL_KEYS: Final = frozenset({"id", "provider", "parameters"})
_CAPABILITY_KEYS: Final = frozenset(
    {"type", "id", "version", "schema_hash", "risk", "config"}
)
_KNOWLEDGE_KEYS: Final = frozenset({"datasets", "retrieval"})
_MEMORY_POLICY_KEYS: Final = frozenset({"mode", "profile"})
_CHANNEL_POLICY_KEYS: Final = frozenset(
    {"attachments", "high_risk_tools", "allowed_origins"}
)
_RUNTIME_INPUT_KEYS: Final = frozenset({"readonly_capabilities"})
_TURN_POLICY_KEYS: Final = frozenset(
    {
        "reasoning_option",
        "legacy_thinking_level",
        "max_tokens",
        "temperature",
        "style_guidance",
        "memory_mode",
        "memory_profile",
        "enable_dynamic_tools",
    }
)
_FINGERPRINT_KEYS: Final = frozenset(
    {"spec", "tool_schema", "skills", "knowledge_revision"}
)
_ENTRYPOINTS: Final = frozenset(
    {"assistant", "responses", "studio_preview", "published_agent"}
)
_CHANNELS: Final = frozenset({"preview", "hosted", "embed", "api", "builtin"})
_AUTH_MODES: Final = frozenset({"private", "tenant", "public", "token"})
_CAPABILITY_TYPES: Final = frozenset({"platform", "mcp", "skill", "connector"})
_RISKS: Final = frozenset({"low", "medium", "high", "critical"})
_AGENT_MEMORY_MODES: Final = frozenset({"off", "session", "user"})
_TURN_MEMORY_MODES: Final = frozenset({"off", "session", "user", "auto", "strict"})
_MEMORY_PROFILES: Final = frozenset({"off", "basic", "hybrid"})
_ID_RE: Final = re.compile(r"^[^\x00-\x1f\x7f]{1,512}$")
_HASH_RE: Final = re.compile(r"^sha256:[a-f0-9]{64}$")
_SENSITIVE_KEYS: Final = frozenset(
    {
        "apikey",
        "apitoken",
        "authorization",
        "bearertoken",
        "clientsecret",
        "credential",
        "credentialref",
        "dsn",
        "password",
        "privatekey",
        "refreshtoken",
        "secret",
        "secretref",
        "token",
        "tokenref",
    }
)


class ResolvedAgentLaunchError(ValueError):
    """Stable fail-closed error for malformed or scope-mismatched launches."""

    def __init__(self, code: str) -> None:
        self.code = code
        super().__init__(code)


def _closed(value: Any, keys: frozenset[str], code: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != set(keys):
        raise ResolvedAgentLaunchError(code)
    return value


def _string(value: Any, code: str, *, optional: bool = False) -> str | None:
    if value is None and optional:
        return None
    if not isinstance(value, str) or not _ID_RE.fullmatch(value):
        raise ResolvedAgentLaunchError(code)
    return value


def _canonical_key(value: Any) -> str:
    return re.sub(r"[^a-z0-9]", "", str(value).lower())


def _reject_sensitive_keys(value: Any) -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            if _canonical_key(key) in _SENSITIVE_KEYS:
                raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_SECRET_FORBIDDEN")
            _reject_sensitive_keys(child)
    elif isinstance(value, list):
        for child in value:
            _reject_sensitive_keys(child)


def _validate_model(value: Any) -> dict[str, Any]:
    model = _closed(value, _MODEL_KEYS, "RESOLVED_AGENT_LAUNCH_MODEL_INVALID")
    _string(model["id"], "RESOLVED_AGENT_LAUNCH_MODEL_INVALID")
    _string(model["provider"], "RESOLVED_AGENT_LAUNCH_MODEL_INVALID")
    parameters = model["parameters"]
    if not isinstance(parameters, dict) or set(parameters) - {
        "temperature",
        "max_tokens",
        "thinking_mode",
    }:
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_MODEL_INVALID")
    temperature = parameters.get("temperature")
    if temperature is not None and (
        isinstance(temperature, bool)
        or not isinstance(temperature, int | float)
        or not math.isfinite(float(temperature))
        or not 0 <= float(temperature) <= 2
    ):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_MODEL_INVALID")
    max_tokens = parameters.get("max_tokens")
    if max_tokens is not None and (
        isinstance(max_tokens, bool) or not isinstance(max_tokens, int) or max_tokens < 1
    ):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_MODEL_INVALID")
    thinking = parameters.get("thinking_mode")
    if thinking is not None and (
        not isinstance(thinking, str) or not thinking or len(thinking) > 100
    ):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_MODEL_INVALID")
    return model


def _validate_capabilities(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, list) or len(value) > 256:
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_CAPABILITIES_INVALID")
    seen: set[tuple[str, str]] = set()
    for raw in value:
        item = _closed(
            raw,
            _CAPABILITY_KEYS,
            "RESOLVED_AGENT_LAUNCH_CAPABILITIES_INVALID",
        )
        capability_type = item["type"]
        capability_id = _string(
            item["id"], "RESOLVED_AGENT_LAUNCH_CAPABILITIES_INVALID"
        )
        if capability_type not in _CAPABILITY_TYPES or item["risk"] not in _RISKS:
            raise ResolvedAgentLaunchError(
                "RESOLVED_AGENT_LAUNCH_CAPABILITIES_INVALID"
            )
        key = (str(capability_type), str(capability_id))
        if key in seen:
            raise ResolvedAgentLaunchError(
                "RESOLVED_AGENT_LAUNCH_CAPABILITIES_INVALID"
            )
        seen.add(key)
        _string(
            item["version"],
            "RESOLVED_AGENT_LAUNCH_CAPABILITIES_INVALID",
            optional=True,
        )
        schema_hash = _string(
            item["schema_hash"],
            "RESOLVED_AGENT_LAUNCH_CAPABILITIES_INVALID",
            optional=True,
        )
        if schema_hash is not None and not _HASH_RE.fullmatch(schema_hash):
            raise ResolvedAgentLaunchError(
                "RESOLVED_AGENT_LAUNCH_CAPABILITIES_INVALID"
            )
        if not isinstance(item["config"], dict):
            raise ResolvedAgentLaunchError(
                "RESOLVED_AGENT_LAUNCH_CAPABILITIES_INVALID"
            )
    return value


def _validate_knowledge(value: Any) -> dict[str, Any]:
    knowledge = _closed(
        value, _KNOWLEDGE_KEYS, "RESOLVED_AGENT_LAUNCH_KNOWLEDGE_INVALID"
    )
    datasets = knowledge["datasets"]
    if (
        not isinstance(datasets, list)
        or len(datasets) > 256
        or not all(isinstance(item, str) and _ID_RE.fullmatch(item) for item in datasets)
        or len(set(datasets)) != len(datasets)
        or not isinstance(knowledge["retrieval"], dict)
    ):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_KNOWLEDGE_INVALID")
    return knowledge


def _validate_identity(identity: dict[str, Any]) -> None:
    for key in ("tenant_id", "user_id", "session_id", "agent_id"):
        _string(identity[key], "RESOLVED_AGENT_LAUNCH_IDENTITY_INVALID")
    version_id = _string(
        identity["agent_version_id"],
        "RESOLVED_AGENT_LAUNCH_IDENTITY_INVALID",
        optional=True,
    )
    publication_id = _string(
        identity["publication_id"],
        "RESOLVED_AGENT_LAUNCH_IDENTITY_INVALID",
        optional=True,
    )
    draft_revision = identity["draft_revision"]
    if draft_revision is not None and (
        isinstance(draft_revision, bool)
        or not isinstance(draft_revision, int)
        or draft_revision < 1
    ):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_IDENTITY_INVALID")
    channel = identity["channel"]
    entrypoint = identity["entrypoint"]
    if (
        channel not in _CHANNELS
        or entrypoint not in _ENTRYPOINTS
        or identity["auth_mode"] not in _AUTH_MODES
    ):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_IDENTITY_INVALID")
    if entrypoint in {"assistant", "responses"}:
        if (
            channel != "builtin"
            or identity["agent_id"] != "__builtin_assistant__"
            or version_id is not None
            or publication_id is not None
            or draft_revision is not None
        ):
            raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_IDENTITY_INVALID")
    elif entrypoint == "studio_preview":
        if (
            channel != "preview"
            or publication_id is not None
            or (version_id is None) == (draft_revision is None)
        ):
            raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_IDENTITY_INVALID")
    elif (
        channel not in {"api", "hosted", "embed"}
        or version_id is None
        or publication_id is None
        or draft_revision is not None
    ):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_IDENTITY_INVALID")


def _validated_payload(value: Any) -> dict[str, Any]:
    payload = _closed(
        copy.deepcopy(value), _TOP_KEYS, "RESOLVED_AGENT_LAUNCH_INVALID"
    )
    if payload["schema_version"] != RESOLVED_AGENT_LAUNCH_SCHEMA_VERSION:
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_VERSION_UNSUPPORTED")
    identity = _closed(
        payload["identity"],
        _IDENTITY_KEYS,
        "RESOLVED_AGENT_LAUNCH_IDENTITY_INVALID",
    )
    _validate_identity(identity)
    model = _validate_model(payload["model"])
    if not isinstance(payload["model_profile"], dict):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_MODEL_INVALID")
    capabilities = _validate_capabilities(payload["capability_bindings"])
    knowledge = _validate_knowledge(payload["knowledge_bindings"])
    memory = _closed(
        payload["memory_policy"],
        _MEMORY_POLICY_KEYS,
        "RESOLVED_AGENT_LAUNCH_MEMORY_INVALID",
    )
    if memory["mode"] not in _TURN_MEMORY_MODES or memory["profile"] not in _MEMORY_PROFILES:
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_MEMORY_INVALID")
    channel_policy = _closed(
        payload["channel_policy"],
        _CHANNEL_POLICY_KEYS,
        "RESOLVED_AGENT_LAUNCH_CHANNEL_INVALID",
    )
    if (
        not isinstance(channel_policy["attachments"], bool)
        or not isinstance(channel_policy["high_risk_tools"], bool)
        or not isinstance(channel_policy["allowed_origins"], list)
        or not all(isinstance(item, str) for item in channel_policy["allowed_origins"])
    ):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_CHANNEL_INVALID")
    runtime_inputs = _closed(
        payload["runtime_inputs"],
        _RUNTIME_INPUT_KEYS,
        "RESOLVED_AGENT_LAUNCH_RUNTIME_INPUT_INVALID",
    )
    if not isinstance(runtime_inputs["readonly_capabilities"], dict):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_RUNTIME_INPUT_INVALID")
    turn_policy = _closed(
        payload["turn_policy"],
        _TURN_POLICY_KEYS,
        "RESOLVED_AGENT_LAUNCH_TURN_POLICY_INVALID",
    )
    for key in ("reasoning_option", "legacy_thinking_level", "style_guidance"):
        value = turn_policy[key]
        if value is not None and (not isinstance(value, str) or not value):
            raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_TURN_POLICY_INVALID")
    max_tokens = turn_policy["max_tokens"]
    if max_tokens is not None and (
        isinstance(max_tokens, bool) or not isinstance(max_tokens, int) or max_tokens < 1
    ):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_TURN_POLICY_INVALID")
    temperature = turn_policy["temperature"]
    if temperature is not None and (
        isinstance(temperature, bool)
        or not isinstance(temperature, int | float)
        or not math.isfinite(float(temperature))
        or not 0 <= float(temperature) <= 2
    ):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_TURN_POLICY_INVALID")
    if (
        turn_policy["memory_mode"] != memory["mode"]
        or turn_policy["memory_profile"] != memory["profile"]
        or not isinstance(turn_policy["enable_dynamic_tools"], bool)
    ):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_TURN_POLICY_INVALID")
    fingerprints = _closed(
        payload["fingerprints"],
        _FINGERPRINT_KEYS,
        "RESOLVED_AGENT_LAUNCH_FINGERPRINT_INVALID",
    )
    if not all(isinstance(item, str) and _HASH_RE.fullmatch(item) for item in fingerprints.values()):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_FINGERPRINT_INVALID")
    spec = _closed(
        payload["agent_spec"],
        _AGENT_SPEC_KEYS,
        "RESOLVED_AGENT_LAUNCH_AGENT_SPEC_INVALID",
    )
    spec_memory = _closed(
        spec["memory"], frozenset({"mode"}), "RESOLVED_AGENT_LAUNCH_AGENT_SPEC_INVALID"
    )
    if (
        spec["agentId"] != identity["agent_id"]
        or spec["agentVersionId"] != identity["agent_version_id"]
        or spec["channel"] != identity["channel"]
        or not isinstance(spec["developerInstructions"], str)
        or not spec["developerInstructions"].strip()
        or spec["model"] != model
        or spec["capabilities"] != capabilities
        or spec["knowledge"] != knowledge
        or spec_memory["mode"] not in _AGENT_MEMORY_MODES
    ):
        raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_AGENT_SPEC_INVALID")
    _reject_sensitive_keys(payload)
    canonical_runtime_json(payload)
    return payload


@dataclass(frozen=True, slots=True)
class ResolvedAgentLaunchV1:
    """Immutable canonical launch; accessors always return defensive copies."""

    _canonical_payload: str

    def __post_init__(self) -> None:
        """Reject forged instances that bypass :meth:`parse`."""

        if not isinstance(self._canonical_payload, str):
            raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_INVALID")
        try:
            raw = json.loads(self._canonical_payload)
        except (TypeError, json.JSONDecodeError) as exc:
            raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_INVALID") from exc
        payload = _validated_payload(raw)
        if canonical_runtime_json(payload) != self._canonical_payload:
            raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_INVALID")

    @classmethod
    def parse(cls, value: Mapping[str, Any]) -> ResolvedAgentLaunchV1:
        payload = _validated_payload(dict(value))
        return cls(canonical_runtime_json(payload))

    @classmethod
    def from_legacy_snapshot(
        cls,
        snapshot: Mapping[str, Any],
        *,
        user_id: str,
        session_id: str,
        entrypoint: str,
        model_profile: Mapping[str, Any],
        readonly_capabilities: Mapping[str, Any],
        turn_policy: Mapping[str, Any],
    ) -> ResolvedAgentLaunchV1:
        legacy = copy.deepcopy(dict(snapshot))
        channel_policy = legacy.get("channel_policy")
        if isinstance(channel_policy, dict):
            if set(channel_policy) - (_CHANNEL_POLICY_KEYS | {"expires_at"}):
                raise ResolvedAgentLaunchError("RESOLVED_AGENT_LAUNCH_CHANNEL_INVALID")
            # Expiry belongs to the Gateway publication access check and
            # release fingerprint. The established Runtime wire has three
            # permission fields and must not receive publication metadata.
            channel_policy = {key: value for key, value in channel_policy.items()
                              if key in _CHANNEL_POLICY_KEYS}
        identity = {
            "tenant_id": legacy.get("tenant_id"),
            "user_id": user_id,
            "session_id": session_id,
            "agent_id": legacy.get("agent_id"),
            "agent_version_id": legacy.get("agent_version_id"),
            "draft_revision": turn_policy.get("draft_revision"),
            "publication_id": (legacy.get("publication") or {}).get("id"),
            "channel": (legacy.get("publication") or {}).get("channel"),
            "entrypoint": entrypoint,
            "auth_mode": (legacy.get("publication") or {}).get("auth_mode"),
        }
        normalized_turn_policy = {
            key: copy.deepcopy(turn_policy.get(key)) for key in _TURN_POLICY_KEYS
        }
        normalized_turn_policy.setdefault("enable_dynamic_tools", True)
        payload = {
            "schema_version": RESOLVED_AGENT_LAUNCH_SCHEMA_VERSION,
            "identity": identity,
            "agent_spec": legacy.get("agent_spec"),
            "model": legacy.get("model"),
            "model_profile": copy.deepcopy(dict(model_profile)),
            "capability_bindings": legacy.get("capabilities"),
            "knowledge_bindings": legacy.get("knowledge"),
            "memory_policy": {
                "mode": normalized_turn_policy.get("memory_mode"),
                "profile": normalized_turn_policy.get("memory_profile"),
            },
            "channel_policy": channel_policy,
            "runtime_inputs": {
                "readonly_capabilities": copy.deepcopy(dict(readonly_capabilities))
            },
            "turn_policy": normalized_turn_policy,
            "fingerprints": legacy.get("fingerprints"),
        }
        return cls.parse(payload)

    def to_dict(self) -> dict[str, Any]:
        return json.loads(self._canonical_payload)

    @property
    def identity(self) -> dict[str, Any]:
        return self.to_dict()["identity"]

    @property
    def model(self) -> dict[str, Any]:
        return self.to_dict()["model"]

    @property
    def model_profile(self) -> dict[str, Any]:
        return self.to_dict()["model_profile"]

    @property
    def runtime_inputs(self) -> dict[str, Any]:
        return self.to_dict()["runtime_inputs"]

    @property
    def turn_policy(self) -> dict[str, Any]:
        return self.to_dict()["turn_policy"]

    def to_control_snapshot(self) -> dict[str, Any]:
        payload = self.to_dict()
        identity = payload["identity"]
        instructions = payload["agent_spec"]["developerInstructions"]
        return {
            "schema_version": "agent-runtime/v1",
            "tenant_id": identity["tenant_id"],
            "user_id": identity["user_id"],
            "session_id": identity["session_id"],
            "agent_id": identity["agent_id"],
            "agent_version_id": identity["agent_version_id"],
            "publication": {
                "id": identity["publication_id"],
                "channel": identity["channel"],
                "auth_mode": identity["auth_mode"],
            },
            "model": payload["model"],
            "model_profile": payload["model_profile"],
            "instructions": {
                "agent": instructions,
                "developerInstructions": instructions,
                "prompt_hash": payload["fingerprints"]["spec"],
            },
            "agent_spec": payload["agent_spec"],
            "capabilities": payload["capability_bindings"],
            "knowledge": payload["knowledge_bindings"],
            "memory": {"mode": payload["agent_spec"]["memory"]["mode"]},
            "channel_policy": payload["channel_policy"],
            "fingerprints": payload["fingerprints"],
        }


__all__ = [
    "RESOLVED_AGENT_LAUNCH_SCHEMA_VERSION",
    "ResolvedAgentLaunchError",
    "ResolvedAgentLaunchV1",
]
