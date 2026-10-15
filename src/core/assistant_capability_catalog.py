"""Gateway-owned projection of the versioned Assistant capability catalog.

The Rust worker owns execution.  The Gateway only reads the checked-in
declarative catalog used by that worker and projects the public Assistant
shape. Keeping the reader here makes the control-plane route independent from
the worker process.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
from collections.abc import Mapping
from functools import lru_cache
from pathlib import Path
from typing import Any

from fastapi import HTTPException, Request

from .auth.user_resolver import UserContext

_CATALOG_SCHEMA_VERSION = "ai-platform-capability-catalog/v1"
_CATALOG_RELATIVE_PATH = (
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/"
    "platform_catalog_v1.json"
)
_NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$")
_ALLOWED_RECORD_KEYS = frozenset(
    {
        "name",
        "id",
        "version",
        "schema_hash",
        "description",
        "input_schema",
        "read_only",
        "effect",
        "risk",
        "protocol",
        "kind",
        "approval",
        "category",
        "when_to_use",
        "when_not_to_use",
        "requires_confirmation",
        "required_permissions",
        "timeout_ms",
        "implementation_owner",
        "connector_provider",
        "content_hash",
        "permissions",
        "plugin_id",
        "plugin_version",
        "plugin_metadata",
    }
)
_OPTIONAL_RECORD_KEYS = frozenset(
    {
        "connector_provider",
        "content_hash",
        "permissions",
        "plugin_id",
        "plugin_version",
        "plugin_metadata",
    }
)
_ALLOWED_ROOT_KEYS = frozenset(
    {"schema_version", "capability_revision", "gateway_policy", "capabilities"}
)
_TRUE_VALUES = frozenset({"1", "true", "yes", "on"})
_FALSE_VALUES = frozenset({"0", "false", "no", "off"})
_MAX_CATALOG_BYTES = 4 * 1024 * 1024
_ALLOWED_CATEGORIES = frozenset(
    {"retrieval", "generation", "analysis", "integration", "utility", "skill", "mcp", "local"}
)
_ALLOWED_PROTOCOLS = frozenset(
    {"internal", "mcp", "http", "local", "local-node-capability/v2"}
)
_PERMISSION_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9:._-]{0,127}$")


class AssistantCapabilityCatalogError(RuntimeError):
    """Raised when the shared catalog cannot be trusted."""


def _catalog_path() -> Path:
    configured = os.getenv("AI_PLATFORM_CAPABILITY_CATALOG_PATH", "").strip()
    if configured:
        return Path(configured).expanduser()
    packaged = Path(__file__).resolve().parent / "data" / "platform_catalog_v1.json"
    if packaged.is_file():
        return packaged
    # src/core/.. -> repository root.  This is the same source consumed by
    # the Rust worker. Wheels and OCI images project this exact file into the
    # packaged path above at build time; no second checked-in schema exists.
    return Path(__file__).resolve().parents[2] / _CATALOG_RELATIVE_PATH


def _canonical_json(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode(
        "utf-8"
    )


def _validate_record(record: Any) -> dict[str, Any]:
    if (
        not isinstance(record, dict)
        or set(record) - _ALLOWED_RECORD_KEYS
        or (_ALLOWED_RECORD_KEYS - _OPTIONAL_RECORD_KEYS) - set(record)
    ):
        raise AssistantCapabilityCatalogError("catalog capability record is malformed")
    name = record.get("name")
    if not isinstance(name, str) or not _NAME_RE.fullmatch(name) or record.get("id") != name:
        raise AssistantCapabilityCatalogError("catalog capability name binding is invalid")
    if not isinstance(record.get("description"), str) or not record["description"].strip():
        raise AssistantCapabilityCatalogError(f"catalog capability {name} has no description")
    if not isinstance(record.get("input_schema"), dict):
        raise AssistantCapabilityCatalogError(f"catalog capability {name} schema is invalid")
    expected_hash = "sha256:" + hashlib.sha256(_canonical_json(record["input_schema"])).hexdigest()
    if record.get("schema_hash") != expected_hash:
        raise AssistantCapabilityCatalogError(f"catalog capability {name} schema hash is invalid")
    if record.get("version") is not None and not isinstance(record.get("version"), str):
        raise AssistantCapabilityCatalogError(f"catalog capability {name} version is invalid")
    if not isinstance(record.get("read_only"), bool):
        raise AssistantCapabilityCatalogError(f"catalog capability {name} read_only is invalid")
    if record.get("effect") not in {"read", "write", "unknown"}:
        raise AssistantCapabilityCatalogError(f"catalog capability {name} effect is invalid")
    if record.get("risk") not in {"low", "medium", "high", "critical"}:
        raise AssistantCapabilityCatalogError(f"catalog capability {name} risk is invalid")
    if record.get("approval") not in {"never", "on_request", "always"}:
        raise AssistantCapabilityCatalogError(f"catalog capability {name} approval is invalid")
    if record.get("protocol") not in _ALLOWED_PROTOCOLS:
        raise AssistantCapabilityCatalogError(f"catalog capability {name} protocol is invalid")
    if record.get("kind") not in {
        "tool",
        "knowledge",
        "platform_tool_discovery",
        "skill",
        "mcp",
        "fixture",
    }:
        raise AssistantCapabilityCatalogError(f"catalog capability {name} kind is invalid")
    if record.get("category") not in _ALLOWED_CATEGORIES:
        raise AssistantCapabilityCatalogError(f"catalog capability {name} category is invalid")
    for field in ("when_to_use", "when_not_to_use"):
        value = record.get(field)
        if value is not None and (
            not isinstance(value, str)
            or not value.strip()
            or len(value) > 20_000
            or any(ord(char) < 32 and char not in "\r\n" for char in value)
        ):
            raise AssistantCapabilityCatalogError(f"catalog capability {name} {field} is invalid")
    if not isinstance(record.get("requires_confirmation"), bool):
        raise AssistantCapabilityCatalogError(
            f"catalog capability {name} confirmation metadata is invalid"
        )
    permissions = record.get("required_permissions")
    if (
        not isinstance(permissions, list)
        or len(permissions) > 32
        or any(
            not isinstance(permission, str) or not _PERMISSION_RE.fullmatch(permission)
            for permission in permissions
        )
        or len(set(permissions)) != len(permissions)
    ):
        raise AssistantCapabilityCatalogError(
            f"catalog capability {name} permissions metadata is invalid"
        )
    timeout_ms = record.get("timeout_ms")
    if (
        isinstance(timeout_ms, bool)
        or not isinstance(timeout_ms, int)
        or not 0 < timeout_ms <= 300_000
    ):
        raise AssistantCapabilityCatalogError(f"catalog capability {name} timeout is invalid")
    if record.get("implementation_owner") not in {"runtime", "worker"}:
        raise AssistantCapabilityCatalogError(f"catalog capability {name} owner is invalid")
    connector_provider = record.get("connector_provider")
    if connector_provider is not None and (
        not isinstance(connector_provider, str)
        or not _NAME_RE.fullmatch(connector_provider)
        or record["implementation_owner"] != "worker"
        or record["protocol"] != "internal"
        or record["kind"] != "tool"
    ):
        raise AssistantCapabilityCatalogError(
            f"catalog capability {name} connector provider is invalid"
        )
    content_hash = record.get("content_hash")
    if content_hash is not None and (
        not isinstance(content_hash, str)
        or not re.fullmatch(r"sha256:[0-9a-f]{64}", content_hash)
    ):
        raise AssistantCapabilityCatalogError(f"catalog capability {name} content hash is invalid")
    permissions = record.get("permissions")
    if permissions is not None and (
        not isinstance(permissions, list)
        or len(permissions) > 32
        or any(not isinstance(item, str) or not _PERMISSION_RE.fullmatch(item) for item in permissions)
        or len(set(permissions)) != len(permissions)
    ):
        raise AssistantCapabilityCatalogError(f"catalog capability {name} permissions are invalid")
    for field in ("plugin_id", "plugin_version"):
        value = record.get(field)
        if value is not None and (not isinstance(value, str) or not _NAME_RE.fullmatch(value)):
            raise AssistantCapabilityCatalogError(f"catalog capability {name} {field} is invalid")
    plugin_metadata = record.get("plugin_metadata")
    if plugin_metadata is not None and (
        not isinstance(plugin_metadata, dict)
        or any(
            not isinstance(key, str)
            or not _NAME_RE.fullmatch(key)
            or isinstance(value, (dict, list))
            for key, value in plugin_metadata.items()
        )
    ):
        raise AssistantCapabilityCatalogError(
            f"catalog capability {name} plugin metadata is invalid"
        )
    if record["effect"] == "read" and (not record["read_only"] or record["approval"] != "never"):
        raise AssistantCapabilityCatalogError(f"catalog capability {name} read policy is invalid")
    if record["effect"] != "read" and (record["read_only"] or record["approval"] == "never"):
        raise AssistantCapabilityCatalogError(f"catalog capability {name} write policy is invalid")
    return record


def _validate_gateway_policy(value: Any) -> dict[str, list[str]]:
    if not isinstance(value, dict) or set(value) != {"high_risk_tools", "medium_risk_tools"}:
        raise AssistantCapabilityCatalogError("gateway policy metadata is malformed")
    validated: dict[str, list[str]] = {}
    seen: set[str] = set()
    for field in ("high_risk_tools", "medium_risk_tools"):
        names = value[field]
        if not isinstance(names, list) or len(names) > 256:
            raise AssistantCapabilityCatalogError("gateway policy tool list is malformed")
        checked: list[str] = []
        for name in names:
            if not isinstance(name, str) or not _NAME_RE.fullmatch(name) or name in seen:
                raise AssistantCapabilityCatalogError("gateway policy tool name is malformed")
            seen.add(name)
            checked.append(name)
        validated[field] = checked
    return validated


@lru_cache(maxsize=8)
def _load_catalog_from_path(
    path_string: str,
) -> tuple[int, dict[str, list[str]], tuple[dict[str, Any], ...]]:
    path = Path(path_string)
    try:
        if path.stat().st_size > _MAX_CATALOG_BYTES:
            raise AssistantCapabilityCatalogError("capability catalog exceeds size limit")
        raw = json.loads(path.read_text(encoding="utf-8"))
    except AssistantCapabilityCatalogError:
        raise
    except (OSError, ValueError) as exc:
        raise AssistantCapabilityCatalogError("capability catalog is unavailable") from exc
    if not isinstance(raw, dict) or set(raw) != _ALLOWED_ROOT_KEYS:
        raise AssistantCapabilityCatalogError("capability catalog envelope is malformed")
    if raw.get("schema_version") != _CATALOG_SCHEMA_VERSION:
        raise AssistantCapabilityCatalogError("capability catalog version is unsupported")
    revision = raw.get("capability_revision")
    records = raw.get("capabilities")
    gateway_policy = _validate_gateway_policy(raw.get("gateway_policy"))
    if isinstance(revision, bool) or not isinstance(revision, int) or revision < 1:
        raise AssistantCapabilityCatalogError("capability catalog revision is invalid")
    if not isinstance(records, list) or not 1 <= len(records) <= 256:
        raise AssistantCapabilityCatalogError("capability catalog count is invalid")
    checked = tuple(_validate_record(record) for record in records)
    names = [record["name"] for record in checked]
    if len(set(names)) != len(names):
        raise AssistantCapabilityCatalogError("capability catalog contains duplicate names")
    return revision, gateway_policy, checked


def load_assistant_capability_catalog() -> tuple[int, tuple[dict[str, Any], ...]]:
    """Load and validate the shared catalog, failing closed on any damage."""

    revision, _, records = _load_catalog_from_path(str(_catalog_path()))
    return revision, records


def load_assistant_gateway_policy() -> dict[str, list[str]]:
    """Return policy metadata from the same validated cached snapshot."""

    _, policy, _ = _load_catalog_from_path(str(_catalog_path()))
    return {key: list(value) for key, value in policy.items()}


def clear_assistant_capability_catalog_cache() -> None:
    """Clear the bounded process cache for tests and explicit reloads."""

    _load_catalog_from_path.cache_clear()


def _category(record: Mapping[str, Any]) -> str:
    return str(record["category"])


def _user_has_required_permissions(user: UserContext, required: list[str]) -> bool:
    """Mirror the legacy ToolRegistry role/tier lattice exactly."""

    if not required:
        return True
    roles = set(user.roles or [])
    tier = str(user.tier or "anonymous").lower()
    tier_order = {"anonymous": 0, "normal": 1, "premium": 2, "enterprise": 3, "admin": 4}
    if tier not in tier_order:
        return False
    for permission in required:
        if permission.startswith("role:"):
            role = permission.split(":", 1)[1].strip()
            if role not in roles and "admin" not in roles:
                return False
        elif permission.startswith("tier:"):
            required_tier = permission.split(":", 1)[1].strip().lower()
            if required_tier not in tier_order or tier_order[tier] < tier_order[required_tier]:
                return False
        elif permission not in roles and "admin" not in roles:
            return False
    return True


def _normalise_policy_list(value: Any, field: str) -> list[str]:
    if value is None:
        return []
    if not isinstance(value, (list, tuple)) or len(value) > 256:
        raise AssistantCapabilityCatalogError(f"tenant policy {field} is malformed")
    items = []
    for item in value:
        if not isinstance(item, str) or not _NAME_RE.fullmatch(item.strip()):
            raise AssistantCapabilityCatalogError(f"tenant policy {field} is malformed")
        items.append(item.strip())
    return list(dict.fromkeys(items))


def project_assistant_tools(
    user: UserContext,
    *,
    tenant_policy: Mapping[str, Any] | None = None,
) -> list[dict[str, Any]]:
    """Project only tenant-authorized catalog entries into the public shape."""

    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="authentication required")
    _, records = load_assistant_capability_catalog()
    policy = tenant_policy or {}
    allowed = set(_normalise_policy_list(policy.get("allowed_tools"), "allowed_tools"))
    blocked = set(_normalise_policy_list(policy.get("blocked_tools"), "blocked_tools"))
    category_value = policy.get("allowed_tool_categories", policy.get("allowed_categories"))
    categories = set(_normalise_policy_list(category_value, "allowed_tool_categories"))
    result: list[dict[str, Any]] = []
    for record in records:
        name = record["name"]
        category = _category(record)
        if not _user_has_required_permissions(user, record["required_permissions"]):
            continue
        if allowed and name not in allowed:
            continue
        if name in blocked or (categories and category not in categories):
            continue
        result.append(
            {
                "name": name,
                "description": record["description"],
                "category": category,
                "risk_level": record["risk"],
                "when_to_use": record["when_to_use"],
                "when_not_to_use": record["when_not_to_use"],
            }
        )
    return result


def _env_bool(name: str, default: bool) -> bool:
    value = os.getenv(name)
    if value is None or not value.strip():
        return default
    lowered = value.strip().lower()
    if lowered in _TRUE_VALUES:
        return True
    if lowered in _FALSE_VALUES:
        return False
    raise AssistantCapabilityCatalogError(f"{name} is malformed")


def _env_text(name: str, default: str, allowed: set[str]) -> str:
    value = os.getenv(name, default).strip().lower()
    if value not in allowed:
        raise AssistantCapabilityCatalogError(f"{name} is malformed")
    return value


async def load_gateway_assistant_policies(
    request: Request,
    user: UserContext,
    records: tuple[dict[str, Any], ...],
) -> dict[str, Any]:
    """Return a Gateway-owned, tenant-scoped public policy snapshot."""

    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="authentication required")
    try:
        policy = None
        database = getattr(request.app.state, "database", None)
        if database is not None:
            # DatabaseStorage exposes both flags in production.  Tiny test
            # fakes may intentionally omit them, so only enforce attributes
            # that are actually present.  Never turn a known-disabled or
            # poolless store into an allow-all default.
            if hasattr(database, "enabled") and not bool(database.enabled):
                raise AssistantCapabilityCatalogError("tenant policy database is disabled")
            if hasattr(database, "_pool") and database._pool is None:
                raise AssistantCapabilityCatalogError("tenant policy database pool is unavailable")
            fetchrow = getattr(database, "fetchrow", None)
            if callable(fetchrow):
                policy = await fetchrow(
                    "SELECT allowed_tools, blocked_tools, allowed_categories, "
                    "max_calls_per_session, max_calls_per_minute "
                    "FROM tenant_tool_policies WHERE tenant_id = $1",
                    user.tenant_id or "default",
                )
            elif hasattr(database, "enabled") or hasattr(database, "_pool"):
                raise AssistantCapabilityCatalogError("tenant policy database is unavailable")
        policy = dict(policy) if policy is not None else {}
        allowed_tools = _normalise_policy_list(policy.get("allowed_tools"), "allowed_tools")
        blocked_tools = _normalise_policy_list(policy.get("blocked_tools"), "blocked_tools")
        allowed_categories = _normalise_policy_list(
            policy.get("allowed_categories"), "allowed_categories"
        )
        max_session = int(policy.get("max_calls_per_session", 100) or 100)
        max_minute = int(policy.get("max_calls_per_minute", 20) or 20)
        if not 1 <= max_session <= 100_000 or not 1 <= max_minute <= 10_000:
            raise AssistantCapabilityCatalogError("tenant policy limits are malformed")
        gateway_policy = load_assistant_gateway_policy()
        return {
            # The Runtime currently implements only the safe profile. An old
            # deployment setting must not be advertised as effective policy.
            "default_execution_profile": "safe",
            "supported_execution_profiles": ["safe"],
            "configured_execution_profile_supported": os.getenv(
                "ASSISTANT_DEFAULT_EXECUTION_PROFILE", "safe"
            ).strip().lower() == "safe",
            "default_memory_mode": _env_text(
                "ASSISTANT_DEFAULT_MEMORY_MODE", "auto", {"auto", "on", "off"}
            ),
            "os_agent_default_enabled": _env_bool("ASSISTANT_OS_AGENT_LITE", False),
            "high_risk_tools": gateway_policy["high_risk_tools"],
            "medium_risk_tools": gateway_policy["medium_risk_tools"],
            "gateway_enabled": _env_bool("ASSISTANT_GATEWAY_ENABLED", True),
            "allowed_tools": allowed_tools,
            "blocked_tools": blocked_tools,
            "allowed_tool_categories": allowed_categories,
            "max_calls_per_session": max_session,
            "max_calls_per_minute": max_minute,
        }
    except HTTPException:
        raise
    except Exception as exc:
        raise HTTPException(status_code=503, detail="Assistant policy is unavailable") from exc
