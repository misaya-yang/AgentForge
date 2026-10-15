"""Canonical, secret-free configuration handed from Gateway to Agent Runtime.

The Gateway resolves AgentSpec and tenant policy.  Runtime receives the
resulting immutable projection only; this module deliberately contains no
provider lookup, user-text routing, or capability execution code.
"""

from __future__ import annotations

import hashlib
import re
from copy import deepcopy
from typing import Any

from ai_gateway_contracts.agent_runtime import canonical_runtime_json

PLATFORM_CONFIG_SCHEMA_VERSION = "agent-runtime-platform-config/v1"
_HASH_RE = re.compile(r"^(?:sha256:)?[0-9a-f]{64}$")
_SAFE_KEY_RE = re.compile(r"^[A-Za-z][A-Za-z0-9_.:-]{0,127}$")
_SECRET_KEY_RE = re.compile(
    r"(?:token|secret|password|api[_-]?key|credential|private[_-]?key|authorization)",
    re.IGNORECASE,
)


class RuntimePlatformConfigError(ValueError):
    """Raised when an Agent snapshot cannot be projected safely."""


def _hash(value: Any) -> str:
    return "sha256:" + hashlib.sha256(canonical_runtime_json(value).encode()).hexdigest()


def _text(value: Any, *, field: str, required: bool = False, limit: int = 1024) -> str | None:
    if value is None and not required:
        return None
    if not isinstance(value, str) or not value.strip() or len(value) > limit:
        raise RuntimePlatformConfigError(f"{field} is invalid")
    if any(ord(char) < 32 and char not in "\r\n\t" for char in value):
        raise RuntimePlatformConfigError(f"{field} is invalid")
    return value.strip()


def _safe_hash(value: Any, *, field: str, required: bool = False) -> str | None:
    result = _text(value, field=field, required=required, limit=80)
    if result is None:
        return None
    if not _HASH_RE.fullmatch(result):
        raise RuntimePlatformConfigError(f"{field} is invalid")
    return result if result.startswith("sha256:") else f"sha256:{result}"


def _safe_metadata(value: Any, *, field: str, limit: int = 64) -> dict[str, Any]:
    if value is None:
        return {}
    if not isinstance(value, dict) or len(value) > limit:
        raise RuntimePlatformConfigError(f"{field} is invalid")
    output: dict[str, Any] = {}
    for key, item in value.items():
        if not isinstance(key, str) or not _SAFE_KEY_RE.fullmatch(key) or _SECRET_KEY_RE.search(key):
            raise RuntimePlatformConfigError(f"{field} contains an unsafe key")
        if isinstance(item, (dict, list)):
            # Metadata is identity only. Nested free-form payloads are a
            # prompt/data injection surface and do not cross this boundary.
            raise RuntimePlatformConfigError(f"{field} contains nested data")
        if not isinstance(item, (str, int, float, bool)) and item is not None:
            raise RuntimePlatformConfigError(f"{field} contains an invalid value")
        output[key] = item
    return output


def _capability_id(binding: dict[str, Any]) -> str:
    value = binding.get("id") or binding.get("resource_id")
    return _text(value, field="capability.id", required=True, limit=255) or ""


def _skill(binding: dict[str, Any]) -> dict[str, Any]:
    config = binding.get("config") if isinstance(binding.get("config"), dict) else {}
    skill_id = _capability_id(binding)
    version = _text(
        binding.get("version") or binding.get("resource_version") or config.get("version"),
        field="skill.version",
        required=True,
        limit=255,
    )
    content_hash = _safe_hash(
        binding.get("content_hash")
        or binding.get("content_sha256")
        or config.get("content_hash")
        or config.get("content_sha256")
        or binding.get("schema_hash"),
        field="skill.content_hash",
        required=True,
    )
    schema_hash = _safe_hash(binding.get("schema_hash"), field="skill.schema_hash")
    raw_permissions = config.get("permissions", config.get("required_permissions", []))
    if not isinstance(raw_permissions, list) or len(raw_permissions) > 32:
        raise RuntimePlatformConfigError("skill.permissions is invalid")
    permissions = sorted(
        {
            _text(permission, field="skill.permission", required=True, limit=128) or ""
            for permission in raw_permissions
        }
    )
    manifest = config.get("manifest")
    if manifest is not None and not isinstance(manifest, dict):
        raise RuntimePlatformConfigError("skill.manifest is invalid")
    manifest_identity = _safe_metadata(
        {
            key: manifest[key]
            for key in ("name", "title", "description", "entrypoint")
            if isinstance(manifest, dict) and key in manifest
        },
        field="skill.manifest",
    )
    return {
        "id": skill_id,
        "version": version,
        "content_hash": content_hash,
        "schema_hash": schema_hash,
        "permissions": permissions,
        "manifest": manifest_identity,
    }


def _grant(binding: dict[str, Any]) -> dict[str, Any] | None:
    config = binding.get("config") if isinstance(binding.get("config"), dict) else {}
    grant_id = binding.get("grant_id") or config.get("grant_id")
    connection_id = binding.get("connection_id") or config.get("connection_id")
    principal_type = binding.get("principal_type") or config.get("principal_type")
    if grant_id is None and connection_id is None and principal_type is None:
        return None
    result = {
        "capability_id": _capability_id(binding),
        "capability_type": str(binding.get("type") or binding.get("capability_type") or ""),
        "grant_id": _text(grant_id, field="tenant_grant.grant_id", limit=255),
        "connection_id": _text(connection_id, field="tenant_grant.connection_id", limit=255),
        "principal_type": _text(principal_type, field="tenant_grant.principal_type", limit=64),
        "provider": _text(
            binding.get("provider") or config.get("provider"),
            field="tenant_grant.provider",
            limit=128,
        ),
    }
    return {key: value for key, value in result.items() if value is not None}


def _plugin(binding: dict[str, Any]) -> dict[str, Any] | None:
    config = binding.get("config") if isinstance(binding.get("config"), dict) else {}
    plugin = config.get("plugin") if isinstance(config.get("plugin"), dict) else {}
    plugin_id = binding.get("plugin_id") or config.get("plugin_id") or plugin.get("id")
    if plugin_id is None and not plugin:
        return None
    result = {
        "id": _text(plugin_id, field="plugin.id", required=True, limit=255),
        "version": _text(
            binding.get("plugin_version") or config.get("plugin_version") or plugin.get("version"),
            field="plugin.version",
            required=True,
            limit=255,
        ),
        "capability_id": _capability_id(binding),
        "metadata": _safe_metadata(
            config.get("plugin_metadata") or plugin.get("metadata"),
            field="plugin.metadata",
        ),
    }
    return result


def build_runtime_platform_config(
    snapshot: dict[str, Any],
    *,
    tenant_id: str,
    user_id: str,
    session_id: str,
    attachment_refs: list[str] | None = None,
) -> dict[str, Any]:
    """Build the immutable Runtime projection from a resolved Agent snapshot."""

    if not isinstance(snapshot, dict):
        raise RuntimePlatformConfigError("agent snapshot is invalid")
    agent_spec = snapshot.get("agent_spec")
    if not isinstance(agent_spec, dict):
        raise RuntimePlatformConfigError("agent_spec is invalid")
    capabilities = snapshot.get("capabilities")
    if not isinstance(capabilities, list):
        raise RuntimePlatformConfigError("agent capabilities are invalid")
    skills: list[dict[str, Any]] = []
    grants: list[dict[str, Any]] = []
    plugins: list[dict[str, Any]] = []
    seen: set[tuple[str, str]] = set()
    for raw in capabilities:
        if not isinstance(raw, dict):
            raise RuntimePlatformConfigError("agent capability binding is invalid")
        raw_config = raw.get("config") if isinstance(raw.get("config"), dict) else {}
        if any(
            isinstance(key, str) and _SECRET_KEY_RE.search(key)
            for key in raw_config
        ):
            raise RuntimePlatformConfigError("capability config contains secret material")
        kind = str(raw.get("type") or raw.get("capability_type") or "")
        identity = (_capability_id(raw), kind)
        if identity in seen:
            raise RuntimePlatformConfigError("agent capability binding is duplicated")
        seen.add(identity)
        if kind == "skill":
            skills.append(_skill(raw))
        grant = _grant(raw)
        if grant is not None:
            grants.append(grant)
        plugin = _plugin(raw)
        if plugin is not None:
            plugins.append(plugin)
    refs = attachment_refs or []
    if not isinstance(refs, list) or any(not isinstance(ref, str) or not ref.strip() for ref in refs):
        raise RuntimePlatformConfigError("attachment refs are invalid")
    attachments = [
        {
            "ref": ref,
            "descriptor": "read_attachment",
            "version": "v1",
        }
        for ref in sorted(set(refs))
    ]
    instructions = agent_spec.get("developerInstructions")
    if not isinstance(instructions, str) or not instructions.strip():
        raise RuntimePlatformConfigError("agent instructions are invalid")
    config = {
        "schema_version": PLATFORM_CONFIG_SCHEMA_VERSION,
        "tenant_id": tenant_id,
        "user_id": user_id,
        "session_id": session_id,
        "agent_spec": deepcopy(agent_spec),
        "instructions": {
            "prompt_hash": _hash(instructions),
            "dynamic_data_after_instructions": True,
        },
        "tenant_grants": sorted(grants, key=lambda item: (item["capability_type"], item["capability_id"])),
        "skills": sorted(skills, key=lambda item: (item["id"], item["version"])),
        "plugins": sorted(plugins, key=lambda item: (item["id"], item["version"])),
        "attachments": attachments,
    }
    return config


def runtime_platform_config_hash(config: dict[str, Any]) -> str:
    canonical = dict(config)
    canonical.pop("config_hash", None)
    return _hash(canonical)


__all__ = [
    "PLATFORM_CONFIG_SCHEMA_VERSION",
    "RuntimePlatformConfigError",
    "build_runtime_platform_config",
    "runtime_platform_config_hash",
]
