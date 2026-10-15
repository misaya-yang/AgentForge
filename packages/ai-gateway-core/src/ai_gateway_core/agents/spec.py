"""Pure Agent Spec normalization, validation, hashing, and redaction helpers."""

from __future__ import annotations

import copy
import hashlib
import json
import re
from typing import Any, Final

AGENT_SPEC_SCHEMA_VERSION: Final = "agent-spec/v1"

_AGENT_SPEC_ROOT_KEYS: Final = frozenset(
    {
        "schema_version",
        "identity",
        "outcome",
        "instructions",
        "model",
        "capabilities",
        "knowledge",
        "memory",
    }
)
_AGENT_IDENTITY_KEYS: Final = frozenset(
    {"icon_url", "theme_color", "welcome_message", "suggested_prompts"}
)
_AGENT_OUTCOME_KEYS: Final = frozenset(
    {"goal", "triggers", "inputs", "human_boundaries", "success_criteria"}
)
_AGENT_MODEL_KEYS: Final = frozenset(
    {"model_id", "provider_id", "temperature", "max_tokens", "thinking_mode"}
)
_AGENT_CAPABILITY_KEYS: Final = frozenset(
    {"type", "resource_id", "resource_version", "schema_hash", "config"}
)
_AGENT_KNOWLEDGE_KEYS: Final = frozenset({"dataset_id", "retrieval_config"})
_AGENT_KNOWLEDGE_RETRIEVAL_KEYS: Final = frozenset(
    {"mode", "top_k", "threshold", "score_threshold", "include_images"}
)
_SENSITIVE_CANONICAL_KEYS: Final = frozenset(
    {
        "accesskey",
        "accesstoken",
        "apikey",
        "apitoken",
        "auth",
        "authentication",
        "authorization",
        "bearer",
        "bearertoken",
        "clientsecret",
        "credential",
        "credentials",
        "credentialref",
        "credentialvalue",
        "oauthrefreshtoken",
        "oauthtoken",
        "password",
        "passwordhash",
        "privatekey",
        "refreshtoken",
        "secret",
        "secretref",
        "secrets",
        "tokenhash",
        "tokenref",
    }
)
_SENSITIVE_CANONICAL_MARKERS: Final = (
    "apikey",
    "apitoken",
    "accesstoken",
    "authorization",
    "bearertoken",
    "clientsecret",
    "password",
    "privatekey",
    "refreshtoken",
    "secret",
    "tokenhash",
)


def canonical_spec(spec: dict[str, Any]) -> str:
    """Return the deterministic JSON representation used by Draft/Version hashes."""

    return json.dumps(spec, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def hash_agent_spec(spec: dict[str, Any]) -> str:
    return hashlib.sha256(canonical_spec(spec).encode("utf-8")).hexdigest()


def _canonical_hash(value: Any) -> str:
    encoded = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


def _canonical_spec_key(key: Any) -> str:
    return re.sub(r"[^a-z0-9]", "", str(key).lower())


def _is_sensitive_spec_key(key: Any) -> bool:
    canonical = _canonical_spec_key(key)
    return canonical in _SENSITIVE_CANONICAL_KEYS or any(
        marker in canonical for marker in _SENSITIVE_CANONICAL_MARKERS
    )


def unsafe_agent_spec_paths(value: Any, location: str = "spec") -> list[str]:
    """Return credential-shaped key paths using case/separator-insensitive matching."""

    paths: list[str] = []
    if isinstance(value, dict):
        for key, child in value.items():
            path = f"{location}.{key}"
            if _is_sensitive_spec_key(key):
                paths.append(path)
                continue
            paths.extend(unsafe_agent_spec_paths(child, path))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            paths.extend(unsafe_agent_spec_paths(child, f"{location}[{index}]"))
    return paths


def agent_spec_safety_errors(spec: dict[str, Any]) -> list[dict[str, str]]:
    """Validate the closed public shape and the no-Secret invariant."""

    errors: list[dict[str, str]] = []
    if not isinstance(spec, dict):
        return [
            {
                "field": "spec",
                "code": "AGENT_SPEC_TYPE_INVALID",
                "message": "spec must be an object",
            }
        ]

    def _reject_unknown(mapping: Any, allowed: frozenset[str], location: str) -> None:
        if not isinstance(mapping, dict):
            return
        for key in mapping:
            if key not in allowed:
                errors.append(
                    {
                        "field": f"{location}.{key}",
                        "code": "AGENT_SPEC_FIELD_FORBIDDEN",
                        "message": "field is not part of the public Agent Spec contract",
                    }
                )

    _reject_unknown(spec, _AGENT_SPEC_ROOT_KEYS, "spec")
    _reject_unknown(spec.get("identity"), _AGENT_IDENTITY_KEYS, "spec.identity")
    _reject_unknown(spec.get("outcome"), _AGENT_OUTCOME_KEYS, "spec.outcome")
    _reject_unknown(spec.get("model"), _AGENT_MODEL_KEYS, "spec.model")
    capabilities = spec.get("capabilities")
    if isinstance(capabilities, list):
        for index, binding in enumerate(capabilities):
            _reject_unknown(
                binding,
                _AGENT_CAPABILITY_KEYS,
                f"spec.capabilities[{index}]",
            )
    knowledge = spec.get("knowledge")
    if isinstance(knowledge, list):
        for index, binding in enumerate(knowledge):
            _reject_unknown(
                binding,
                _AGENT_KNOWLEDGE_KEYS,
                f"spec.knowledge[{index}]",
            )
            if isinstance(binding, dict):
                _reject_unknown(
                    binding.get("retrieval_config"),
                    _AGENT_KNOWLEDGE_RETRIEVAL_KEYS,
                    f"spec.knowledge[{index}].retrieval_config",
                )
    for path in unsafe_agent_spec_paths(spec):
        errors.append(
            {
                "field": path,
                "code": "AGENT_SPEC_SECRET_FORBIDDEN",
                "message": "credentials, Secret references, and token material are forbidden",
            }
        )
    return errors


def _redact_unstructured_spec_value(value: Any) -> Any:
    if isinstance(value, dict):
        return {
            key: _redact_unstructured_spec_value(child)
            for key, child in value.items()
            if not _is_sensitive_spec_key(key)
        }
    if isinstance(value, list):
        return [_redact_unstructured_spec_value(item) for item in value]
    return copy.deepcopy(value)


def _redact_structured_mapping(value: Any, allowed: frozenset[str]) -> dict[str, Any]:
    if not isinstance(value, dict):
        return {}
    return {
        key: _redact_unstructured_spec_value(child)
        for key, child in value.items()
        if key in allowed and not _is_sensitive_spec_key(key)
    }


def redact_agent_spec_for_read(spec: dict[str, Any]) -> dict[str, Any]:
    """Return only the closed public shape and redact unsafe legacy keys."""

    if not isinstance(spec, dict):
        return {}
    result: dict[str, Any] = {}
    if "schema_version" in spec:
        result["schema_version"] = copy.deepcopy(spec["schema_version"])
    if "identity" in spec:
        result["identity"] = _redact_structured_mapping(spec["identity"], _AGENT_IDENTITY_KEYS)
    if "outcome" in spec:
        result["outcome"] = _redact_structured_mapping(spec["outcome"], _AGENT_OUTCOME_KEYS)
    if "instructions" in spec:
        result["instructions"] = copy.deepcopy(spec["instructions"])
    if "model" in spec:
        result["model"] = _redact_structured_mapping(spec["model"], _AGENT_MODEL_KEYS)
    if isinstance(spec.get("capabilities"), list):
        result["capabilities"] = [
            _redact_structured_mapping(binding, _AGENT_CAPABILITY_KEYS)
            for binding in spec["capabilities"]
            if isinstance(binding, dict)
        ]
    if isinstance(spec.get("knowledge"), list):
        result["knowledge"] = [
            _redact_structured_mapping(binding, _AGENT_KNOWLEDGE_KEYS)
            for binding in spec["knowledge"]
            if isinstance(binding, dict)
        ]
    if isinstance(spec.get("memory"), dict):
        result["memory"] = _redact_unstructured_spec_value(spec["memory"])
    return result


def sanitize_agent_copy_spec(spec: dict[str, Any]) -> dict[str, Any]:
    """Allowlist presentation/instruction/model settings for a new Draft.

    AS-01 has no resource-authorization resolver, so it cannot prove that a
    source binding is accessible to the new Agent. Capabilities, Knowledge,
    memory, arbitrary legacy containers and credential-shaped fields are all
    dropped instead of being interpreted.
    """

    public = redact_agent_spec_for_read(spec)
    copied = {
        "schema_version": AGENT_SPEC_SCHEMA_VERSION,
        "identity": _redact_structured_mapping(public.get("identity"), _AGENT_IDENTITY_KEYS),
        "instructions": (
            public.get("instructions") if isinstance(public.get("instructions"), str) else ""
        ),
        "model": _redact_structured_mapping(public.get("model"), _AGENT_MODEL_KEYS),
        "capabilities": [],
        "knowledge": [],
        "memory": {},
    }
    if "outcome" in public:
        copied["outcome"] = _redact_structured_mapping(
            public.get("outcome"), _AGENT_OUTCOME_KEYS
        )
    return copied


def validate_agent_spec(spec: dict[str, Any]) -> list[dict[str, str]]:
    """Validate the AS-01 structural subset without resolving runtime resources."""

    errors = agent_spec_safety_errors(spec)
    if not isinstance(spec, dict):
        return errors
    if spec.get("schema_version") != AGENT_SPEC_SCHEMA_VERSION:
        errors.append(
            {
                "field": "schema_version",
                "code": "AGENT_SPEC_SCHEMA_UNSUPPORTED",
                "message": f"schema_version must be {AGENT_SPEC_SCHEMA_VERSION}",
            }
        )
    instructions = spec.get("instructions")
    if not isinstance(instructions, str) or not instructions.strip():
        errors.append(
            {
                "field": "instructions",
                "code": "AGENT_INSTRUCTIONS_REQUIRED",
                "message": "instructions must be a non-empty string",
            }
        )
    outcome = spec.get("outcome")
    if outcome is not None and not isinstance(outcome, dict):
        errors.append({
            "field": "outcome",
            "code": "AGENT_OUTCOME_INVALID",
            "message": "outcome must be an object",
        })
    if isinstance(outcome, dict):
        if not isinstance(outcome.get("goal", ""), str):
            errors.append({
                "field": "outcome.goal",
                "code": "AGENT_OUTCOME_GOAL_INVALID",
                "message": "outcome.goal must be a string",
            })
        for key in ("triggers", "inputs", "human_boundaries", "success_criteria"):
            values = outcome.get(key, [])
            if not isinstance(values, list) or any(
                not isinstance(value, str) or not value.strip() for value in values
            ):
                errors.append({
                    "field": f"outcome.{key}",
                    "code": "AGENT_OUTCOME_LIST_INVALID",
                    "message": f"outcome.{key} must contain non-empty strings",
                })
    model = spec.get("model")
    if (
        not isinstance(model, dict)
        or not isinstance(model.get("model_id"), str)
        or not model["model_id"].strip()
    ):
        errors.append(
            {
                "field": "model.model_id",
                "code": "AGENT_MODEL_REQUIRED",
                "message": "model.model_id must be configured",
            }
        )
    knowledge = spec.get("knowledge")
    seen_dataset_ids: set[str] = set()
    if knowledge is not None and not isinstance(knowledge, list):
        errors.append(
            {
                "field": "knowledge",
                "code": "AGENT_KNOWLEDGE_BINDING_INVALID",
                "message": "knowledge must be a list",
            }
        )
    if isinstance(knowledge, list):
        for index, binding in enumerate(knowledge):
            if not isinstance(binding, dict):
                errors.append(
                    {
                        "field": f"knowledge[{index}]",
                        "code": "AGENT_KNOWLEDGE_BINDING_INVALID",
                        "message": "Knowledge binding must be an object",
                    }
                )
                continue
            dataset_id = binding.get("dataset_id")
            if not isinstance(dataset_id, str) or not dataset_id.strip():
                errors.append(
                    {
                        "field": f"knowledge[{index}].dataset_id",
                        "code": "AGENT_KNOWLEDGE_DATASET_REQUIRED",
                        "message": "dataset_id must be a non-empty string",
                    }
                )
            elif dataset_id in seen_dataset_ids:
                errors.append(
                    {
                        "field": f"knowledge[{index}].dataset_id",
                        "code": "AGENT_KNOWLEDGE_DATASET_DUPLICATE",
                        "message": "dataset_id must be unique within an Agent spec",
                    }
                )
            else:
                seen_dataset_ids.add(dataset_id)
            location = f"knowledge[{index}].retrieval_config"
            config = binding.get("retrieval_config", {})
            if not isinstance(config, dict):
                errors.append(
                    {
                        "field": location,
                        "code": "AGENT_KNOWLEDGE_CONFIG_INVALID",
                        "message": "retrieval_config must be an object",
                    }
                )
                continue

            mode = config.get("mode", "auto")
            if not isinstance(mode, str) or mode not in {"auto", "tool", "off"}:
                errors.append(
                    {
                        "field": f"{location}.mode",
                        "code": "AGENT_KNOWLEDGE_MODE_INVALID",
                        "message": "mode must be auto, tool, or off",
                    }
                )
            top_k = config.get("top_k", 5)
            if isinstance(top_k, bool) or not isinstance(top_k, int) or not 1 <= top_k <= 20:
                errors.append(
                    {
                        "field": f"{location}.top_k",
                        "code": "AGENT_KNOWLEDGE_TOP_K_INVALID",
                        "message": "top_k must be an integer between 1 and 20",
                    }
                )

            threshold = config.get("threshold", config.get("score_threshold", 0.4))
            if (
                isinstance(threshold, bool)
                or not isinstance(threshold, (int, float))
                or not 0 <= float(threshold) <= 1
            ):
                errors.append(
                    {
                        "field": f"{location}.threshold",
                        "code": "AGENT_KNOWLEDGE_THRESHOLD_INVALID",
                        "message": "threshold must be a number between 0 and 1",
                    }
                )
            if (
                "threshold" in config
                and "score_threshold" in config
                and config["threshold"] != config["score_threshold"]
            ):
                errors.append(
                    {
                        "field": location,
                        "code": "AGENT_KNOWLEDGE_THRESHOLD_CONFLICT",
                        "message": "threshold and score_threshold must not conflict",
                    }
                )
            include_images = config.get("include_images", False)
            if not isinstance(include_images, bool):
                errors.append(
                    {
                        "field": f"{location}.include_images",
                        "code": "AGENT_KNOWLEDGE_IMAGES_INVALID",
                        "message": "include_images must be a boolean",
                    }
                )
    return errors


def render_agent_outcome_contract(spec: dict[str, Any]) -> str:
    """Render structured outcomes into a stable runtime instruction prefix."""

    outcome = spec.get("outcome")
    if not isinstance(outcome, dict):
        return ""
    goal = str(outcome.get("goal") or "").strip()
    sections: list[tuple[str, list[str]]] = []
    for label, key in (
        ("Triggers", "triggers"),
        ("Required inputs", "inputs"),
        ("Human boundaries", "human_boundaries"),
        ("Success criteria", "success_criteria"),
    ):
        raw = outcome.get(key)
        values = [str(value).strip() for value in raw] if isinstance(raw, list) else []
        values = [value for value in values if value]
        if values:
            sections.append((label, values))
    if not goal and not sections:
        return ""
    lines = ["[Agent outcome contract]"]
    if goal:
        lines.append(f"Goal: {goal}")
    for label, values in sections:
        lines.append(f"{label}:")
        lines.extend(f"- {value}" for value in values)
    return "\n".join(lines)


__all__ = [
    "AGENT_SPEC_SCHEMA_VERSION",
    "agent_spec_safety_errors",
    "canonical_spec",
    "hash_agent_spec",
    "redact_agent_spec_for_read",
    "render_agent_outcome_contract",
    "sanitize_agent_copy_spec",
    "unsafe_agent_spec_paths",
    "validate_agent_spec",
]
