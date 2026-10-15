"""Gateway-owned Capability V2 catalog service and client adapters.

The service is the single authority for Worker descriptor validation plus
tenant/RBAC/policy filtering.  Both the internal HTTP route and the Runtime
control plane call this service.  The default control-plane adapter is local;
the HTTP adapter exists only for a future physical split and rejects Gateway
or loopback targets so it cannot recreate Gateway-to-Gateway self-HTTP.
"""

from __future__ import annotations

import copy
import json
import re
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from typing import Any, Protocol
from urllib.parse import urlsplit

import httpx
from ai_gateway_core.tracing import internal_http_headers

from ...core.assistant_capability_catalog import load_assistant_capability_catalog
from .capability_leases import (
    CAPABILITY_CATALOG_SCHEMA_VERSION,
    CAPABILITY_DESCRIPTOR_SCHEMA_VERSION,
    canonical_json_hash,
)

RUNTIME_CATALOG_SCHEMA_VERSION = "agent-capability-catalog/v1"
MAX_CATALOG_BYTES = 4 * 1024 * 1024
MAX_CATALOG_ENTRIES = 256
_DEFAULT_HIDDEN_CAPABILITIES = frozenset({"todo_read", "todo_write"})

_DESCRIPTOR_KEYS = frozenset(
    {
        "schema_version",
        "id",
        "name",
        "version",
        "description",
        "schema_hash",
        "input_schema",
        "output_schema",
        "effect",
        "approval_policy",
        "execution_mode",
        "timeout_ms",
        "tags",
        "protocol",
        "connector_binding",
    }
)
_REQUIRED_DESCRIPTOR_KEYS = _DESCRIPTOR_KEYS - {"connector_binding"}
_HASH_RE = re.compile(r"^sha256:[0-9a-f]{64}$")
_IDENTIFIER_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,159}$")
_KINDS = frozenset(
    {
        "tool",
        "knowledge",
        "mcp",
        "connector",
        "office_read",
        "platform_tool_discovery",
    }
)
_SENSITIVE_KEYS = frozenset(
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


class _HTTPResponse(Protocol):
    status_code: int
    content: bytes

    def json(self) -> Any: ...


class CapabilityCatalogError(RuntimeError):
    """Stable domain error without FastAPI or transport coupling."""

    def __init__(self, code: str, detail: str, *, status_code: int = 503) -> None:
        self.code = code
        self.detail = detail
        self.status_code = status_code
        super().__init__(code)


def _scope_value(value: Any, field: str) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value) > 255
        or any(ord(character) < 32 for character in value)
    ):
        raise CapabilityCatalogError(
            "CAPABILITY_CATALOG_SCOPE_INVALID",
            f"{field} scope is invalid",
            status_code=403,
        )
    return value


@dataclass(frozen=True, slots=True)
class CapabilityCatalogQuery:
    tenant_id: str
    user_id: str
    session_id: str
    model_id: str
    capability_revision: int
    capability_allowlist: tuple[dict[str, Any], ...] | None = None

    @classmethod
    def create(
        cls,
        *,
        tenant_id: str,
        user_id: str,
        session_id: str,
        model_id: str,
        capability_revision: int,
        capability_allowlist: list[dict[str, Any]] | None = None,
    ) -> CapabilityCatalogQuery:
        tenant_id = _scope_value(tenant_id, "tenant")
        user_id = _scope_value(user_id, "user")
        session_id = _scope_value(session_id, "session")
        model_id = _scope_value(model_id, "model")
        if (
            isinstance(capability_revision, bool)
            or not isinstance(capability_revision, int)
            or capability_revision < 1
        ):
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_SCOPE_INVALID",
                "capability revision is invalid",
                status_code=403,
            )
        if capability_allowlist is not None and (
            not isinstance(capability_allowlist, list)
            or any(not isinstance(item, dict) for item in capability_allowlist)
        ):
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_SCOPE_INVALID",
                "capability allowlist is invalid",
                status_code=403,
            )
        frozen_allowlist = (
            tuple(copy.deepcopy(capability_allowlist))
            if capability_allowlist is not None
            else None
        )
        return cls(
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            model_id=model_id,
            capability_revision=capability_revision,
            capability_allowlist=frozen_allowlist,
        )

    def request_body(self, *, include_model: bool) -> dict[str, Any]:
        body: dict[str, Any] = {
            "tenant_id": self.tenant_id,
            "user_id": self.user_id,
            "session_id": self.session_id,
            "capability_revision": self.capability_revision,
        }
        if include_model:
            body["model_id"] = self.model_id
            if self.capability_allowlist is not None:
                body["capability_allowlist"] = copy.deepcopy(
                    list(self.capability_allowlist)
                )
        else:
            body["schema_version"] = CAPABILITY_CATALOG_SCHEMA_VERSION
        return body


def user_has_permissions(
    roles: set[str], permissions: set[str], required: list[str]
) -> bool:
    """Apply the existing catalog permission lattice."""

    if not required:
        return True
    subjects = roles | permissions
    if "admin" in subjects:
        return True
    tier_order = {
        "anonymous": 0,
        "normal": 1,
        "premium": 2,
        "enterprise": 3,
        "admin": 4,
    }
    tier = next(
        (
            subject.split(":", 1)[1].lower()
            for subject in subjects
            if subject.startswith("tier:")
        ),
        "normal",
    )
    for permission in required:
        if permission.startswith("role:"):
            if permission.split(":", 1)[1].strip() not in subjects:
                return False
        elif permission.startswith("tier:"):
            required_tier = permission.split(":", 1)[1].strip().lower()
            if required_tier not in tier_order or tier not in tier_order:
                return False
            if tier_order[tier] < tier_order[required_tier]:
                return False
        elif permission not in subjects:
            return False
    return True


def _canonical_key(value: Any) -> str:
    return re.sub(r"[^a-z0-9]", "", str(value).lower())


def _reject_sensitive_keys(value: Any) -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            if _canonical_key(key) in _SENSITIVE_KEYS:
                raise CapabilityCatalogError(
                    "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
                )
            _reject_sensitive_keys(child)
    elif isinstance(value, list):
        for child in value:
            _reject_sensitive_keys(child)


def descriptor_kind(
    descriptor: Mapping[str, Any], record: Mapping[str, Any] | None
) -> str:
    if record is not None:
        kind = record.get("kind")
        if kind in _KINDS:
            return str(kind)
    tags = descriptor.get("tags")
    if not isinstance(tags, list):
        raise CapabilityCatalogError(
            "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
        )
    for kind in (
        "knowledge",
        "mcp",
        "connector",
        "office_read",
        "platform_tool_discovery",
        "tool",
    ):
        if f"kind:{kind}" in tags:
            return kind
    if "fixture" in tags:
        return "tool"
    raise CapabilityCatalogError(
        "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
    )


@dataclass(frozen=True, slots=True)
class CapabilityDescriptorV2:
    """Strict canonical Worker descriptor plus Gateway policy metadata."""

    _canonical_payload: str
    kind: str
    category: str
    required_permissions: tuple[str, ...]

    @classmethod
    def parse(
        cls,
        value: Mapping[str, Any],
        *,
        record: Mapping[str, Any] | None,
    ) -> CapabilityDescriptorV2:
        raw = copy.deepcopy(dict(value))
        if (
            set(raw) - _DESCRIPTOR_KEYS
            or _REQUIRED_DESCRIPTOR_KEYS - set(raw)
            or raw.get("schema_version") != CAPABILITY_DESCRIPTOR_SCHEMA_VERSION
        ):
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
            )
        name = raw.get("name")
        version = raw.get("version")
        description = raw.get("description")
        schema = raw.get("input_schema")
        output_schema = raw.get("output_schema")
        schema_hash = raw.get("schema_hash")
        tags = raw.get("tags")
        protocol = raw.get("protocol")
        timeout_ms = raw.get("timeout_ms")
        if (
            not isinstance(name, str)
            or _IDENTIFIER_RE.fullmatch(name) is None
            or raw.get("id") != name
            or not isinstance(version, str)
            or not version
            or len(version) > 128
            or not isinstance(description, str)
            or not description
            or len(description) > 20_000
            or any(ord(character) < 32 and character != "\n" for character in description)
            or not isinstance(schema, dict)
            or not isinstance(output_schema, dict)
            or not isinstance(schema_hash, str)
            or _HASH_RE.fullmatch(schema_hash) is None
            or canonical_json_hash(schema) != schema_hash
            or raw.get("effect") not in {"read", "write", "unknown"}
            or raw.get("approval_policy") not in {"never", "on_request", "always"}
            or raw.get("execution_mode") not in {"inline", "stream", "job"}
            or isinstance(timeout_ms, bool)
            or not isinstance(timeout_ms, int)
            or not 0 < timeout_ms <= 300_000
            or not isinstance(tags, list)
            or len(tags) > 128
            or any(
                not isinstance(tag, str)
                or not tag
                or len(tag) > 160
                or any(ord(character) < 32 for character in tag)
                for tag in tags
            )
            or len(set(tags)) != len(tags)
            or not isinstance(protocol, str)
            or not protocol
            or len(protocol) > 128
        ):
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
            )
        if (raw["effect"] == "read") != (raw["approval_policy"] == "never"):
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
            )
        connector_binding = raw.get("connector_binding")
        if connector_binding is not None and not isinstance(connector_binding, dict):
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
            )
        _reject_sensitive_keys(raw)
        kind = descriptor_kind(raw, record)
        category = ""
        required: list[str] = []
        if record is not None:
            expected_version = (
                "null" if record.get("version") is None else str(record.get("version"))
            )
            if (
                version != expected_version
                or description != record.get("description")
                or schema != record.get("input_schema")
                or schema_hash != record.get("schema_hash")
                or raw["effect"] != record.get("effect")
                or raw["approval_policy"] != record.get("approval")
                or timeout_ms != record.get("timeout_ms")
                or protocol != record.get("protocol")
                or f"kind:{record.get('kind')}" not in tags
            ):
                raise CapabilityCatalogError(
                    "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
                )
            category = str(record.get("category") or "")
            required.extend(str(item) for item in (record.get("required_permissions") or []))
        required.extend(str(tag)[11:] for tag in tags if str(tag).startswith("permission:"))
        for tag in tags:
            if tag.startswith("category:"):
                category = tag[9:]
        canonical = json.dumps(
            raw,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        )
        return cls(
            _canonical_payload=canonical,
            kind=kind,
            category=category,
            required_permissions=tuple(dict.fromkeys(required)),
        )

    def to_dict(self) -> dict[str, Any]:
        return json.loads(self._canonical_payload)


def project_runtime_descriptor(
    descriptor: CapabilityDescriptorV2,
    *,
    tenant_id: str,
    capability_revision: int,
) -> dict[str, Any]:
    """The single Capability V2 to Runtime catalog projection."""

    value = descriptor.to_dict()
    version = value["version"]
    if version == "null":
        version = None
    return {
        "name": value["name"],
        "id": value["id"],
        "version": version,
        "schema_hash": value["schema_hash"],
        "description": value["description"],
        "schema": value["input_schema"],
        "output_schema": value["output_schema"],
        "source": "capability_worker",
        "kind": descriptor.kind,
        "read_only": value["effect"] == "read",
        "effect": value["effect"],
        "approval_policy": value["approval_policy"],
        "execution_mode": value["execution_mode"],
        "timeout_ms": value["timeout_ms"],
        "tags": value["tags"],
        "protocol": value["protocol"],
        "tenant_id": tenant_id,
        "capability_revision": capability_revision,
        **(
            {"connector_binding": value["connector_binding"]}
            if isinstance(value.get("connector_binding"), dict)
            else {}
        ),
    }


def _policy_set(value: Any) -> set[str]:
    if value is None:
        return set()
    if not isinstance(value, list) or len(value) > MAX_CATALOG_ENTRIES:
        raise CapabilityCatalogError(
            "CAPABILITY_CATALOG_POLICY_UNAVAILABLE", "catalog policy unavailable"
        )
    if any(
        not isinstance(item, str)
        or not item
        or len(item) > 160
        or any(ord(character) < 32 for character in item)
        for item in value
    ):
        raise CapabilityCatalogError(
            "CAPABILITY_CATALOG_POLICY_UNAVAILABLE", "catalog policy unavailable"
        )
    return set(value)


class CapabilityCatalogService:
    """Resolve one tenant-scoped Worker V2 catalog without Gateway self-HTTP."""

    def __init__(
        self,
        *,
        database: Any,
        worker_url: str,
        internal_token: str,
        worker_client: Any | None = None,
        catalog_loader: Callable[[], tuple[int, tuple[dict[str, Any], ...]]] = (
            load_assistant_capability_catalog
        ),
        web_search_configured: bool = False,
    ) -> None:
        self.database = database
        self.worker_url = worker_url.strip().rstrip("/")
        self.internal_token = internal_token.strip()
        self.catalog_loader = catalog_loader
        self.web_search_configured = bool(web_search_configured)
        self._worker_client = worker_client or httpx.AsyncClient(
            timeout=httpx.Timeout(connect=2.0, read=10.0, write=5.0, pool=2.0),
            limits=httpx.Limits(max_connections=4, max_keepalive_connections=2),
            trust_env=False,
        )
        self._owns_worker_client = worker_client is None

    async def close(self) -> None:
        if self._owns_worker_client:
            await self._worker_client.aclose()

    def _worker_endpoint(self) -> str:
        try:
            parsed = urlsplit(self.worker_url)
            valid = (
                parsed.scheme in {"http", "https"}
                and bool(parsed.hostname)
                and parsed.username is None
                and parsed.password is None
                and not parsed.query
                and not parsed.fragment
                and parsed.path in {"", "/"}
            )
        except ValueError:
            valid = False
        if not valid or not self.internal_token:
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_WORKER_UNAVAILABLE",
                "capability worker unavailable",
            )
        return f"{self.worker_url}/internal/v2/capabilities/catalog"

    async def _identity_policy(
        self, query: CapabilityCatalogQuery
    ) -> tuple[set[str], set[str], dict[str, Any]]:
        if self.database is None or getattr(self.database, "enabled", True) is False:
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_IDENTITY_UNAVAILABLE",
                "catalog identity unavailable",
            )
        get_user = getattr(self.database, "get_user_for_tenant", None)
        if not callable(get_user):
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_IDENTITY_UNAVAILABLE",
                "catalog identity unavailable",
            )
        try:
            user = await get_user(query.user_id, query.tenant_id)
        except Exception as exc:
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_IDENTITY_UNAVAILABLE",
                "catalog identity unavailable",
            ) from exc
        if not user:
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_SCOPE_UNAUTHORIZED",
                "catalog scope is not authorized",
                status_code=403,
            )
        roles = {str(role) for role in (user.get("roles") or [])}
        permissions = {str(permission) for permission in (user.get("permissions") or [])}
        for method_name, target in (
            ("get_user_roles", roles),
            ("get_user_permissions", permissions),
        ):
            method = getattr(self.database, method_name, None)
            if callable(method):
                try:
                    target.update(str(value) for value in await method(query.user_id))
                except Exception as exc:
                    raise CapabilityCatalogError(
                        "CAPABILITY_CATALOG_IDENTITY_UNAVAILABLE",
                        "catalog identity unavailable",
                    ) from exc
        policy: dict[str, Any] = {}
        fetchrow = getattr(self.database, "fetchrow", None)
        if callable(fetchrow):
            try:
                row = await fetchrow(
                    "SELECT allowed_tools, blocked_tools, allowed_categories "
                    "FROM tenant_tool_policies WHERE tenant_id = $1",
                    query.tenant_id,
                )
                policy = dict(row) if row else {}
            except Exception as exc:
                raise CapabilityCatalogError(
                    "CAPABILITY_CATALOG_POLICY_UNAVAILABLE",
                    "catalog policy unavailable",
                ) from exc
        return roles, permissions, policy

    async def resolve(
        self,
        query: CapabilityCatalogQuery,
        *,
        worker_client: Any | None = None,
    ) -> dict[str, Any]:
        roles, permissions, policy = await self._identity_policy(query)
        try:
            _, records = self.catalog_loader()
        except Exception as exc:
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_SOURCE_UNAVAILABLE",
                "capability catalog unavailable",
            ) from exc
        record_by_id = {str(record["id"]): record for record in records}
        client = worker_client or self._worker_client
        try:
            response: _HTTPResponse = await client.post(
                self._worker_endpoint(),
                headers=internal_http_headers(
                    {
                        "x-ai-platform-internal-token": self.internal_token,
                        "x-ai-tenant-id": query.tenant_id,
                        "x-ai-user-id": query.user_id,
                        "x-ai-session-id": query.session_id,
                    }
                ),
                json=query.request_body(include_model=False),
            )
        except (httpx.HTTPError, ValueError) as exc:
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_WORKER_UNAVAILABLE",
                "capability worker unavailable",
            ) from exc
        if response.status_code >= 400 or len(response.content) > MAX_CATALOG_BYTES:
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_WORKER_UNAVAILABLE",
                "capability worker unavailable",
            )
        try:
            envelope = response.json()
        except (ValueError, json.JSONDecodeError) as exc:
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
            ) from exc
        if (
            not isinstance(envelope, dict)
            or set(envelope)
            != {"schema_version", "capability_revision", "capabilities"}
            or envelope.get("schema_version") != CAPABILITY_CATALOG_SCHEMA_VERSION
            or envelope.get("capability_revision") != query.capability_revision
            or not isinstance(envelope.get("capabilities"), list)
            or len(envelope["capabilities"]) > MAX_CATALOG_ENTRIES
        ):
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
            )
        allowed_tools = _policy_set(policy.get("allowed_tools"))
        blocked_tools = _policy_set(policy.get("blocked_tools"))
        allowed_categories = _policy_set(policy.get("allowed_categories"))
        projected = {"tools": [], "mcp": [], "deferred": []}
        seen: set[str] = set()
        for raw in envelope["capabilities"]:
            if not isinstance(raw, dict):
                raise CapabilityCatalogError(
                    "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
                )
            descriptor = CapabilityDescriptorV2.parse(
                raw, record=record_by_id.get(str(raw.get("id") or ""))
            )
            value = descriptor.to_dict()
            name = value["name"]
            if name in seen:
                raise CapabilityCatalogError(
                    "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
                )
            seen.add(name)
            # The generic Web Assistant has no durable task-list UI. Exposing
            # the internal planner there makes ordinary answers stop for an
            # approval on every progress update. Signed AgentSpecs can still
            # opt into these capabilities through an explicit allowlist.
            if (
                query.capability_allowlist is None
                and name in _DEFAULT_HIDDEN_CAPABILITIES
            ):
                continue
            if name == "search_web" and not self.web_search_configured:
                continue
            if not user_has_permissions(
                roles, permissions, list(descriptor.required_permissions)
            ):
                continue
            if allowed_tools and name not in allowed_tools:
                continue
            if name in blocked_tools or (
                allowed_categories and descriptor.category not in allowed_categories
            ):
                continue
            runtime = project_runtime_descriptor(
                descriptor,
                tenant_id=query.tenant_id,
                capability_revision=query.capability_revision,
            )
            if runtime["read_only"]:
                key = "mcp" if runtime["kind"] == "mcp" else "tools"
            else:
                key = "deferred"
            projected[key].append(runtime)
        return {
            "schema_version": RUNTIME_CATALOG_SCHEMA_VERSION,
            "capability_revision": query.capability_revision,
            **projected,
        }


class CapabilityCatalogClient(Protocol):
    async def fetch_catalog(self, query: CapabilityCatalogQuery) -> dict[str, Any]: ...


class LocalCapabilityCatalogClient:
    """Default same-process adapter shared with the internal route."""

    def __init__(self, service: CapabilityCatalogService) -> None:
        self.service = service

    async def fetch_catalog(self, query: CapabilityCatalogQuery) -> dict[str, Any]:
        return await self.service.resolve(query)


class HttpCapabilityCatalogClient:
    """Opt-in adapter for a future non-Gateway physical catalog service."""

    def __init__(
        self,
        *,
        base_url: str,
        internal_token: str,
        http_client: Any,
    ) -> None:
        value = base_url.strip().rstrip("/")
        try:
            parsed = urlsplit(value)
            hostname = (parsed.hostname or "").lower()
            valid = (
                parsed.scheme in {"http", "https"}
                and bool(hostname)
                and hostname not in {"gateway", "localhost", "127.0.0.1", "::1"}
                and parsed.username is None
                and parsed.password is None
                and not parsed.query
                and not parsed.fragment
            )
        except ValueError:
            valid = False
        if not valid or not internal_token:
            raise ValueError("external capability catalog endpoint and token are required")
        self.base_url = value
        self.internal_token = internal_token
        self.http_client = http_client

    async def fetch_catalog(self, query: CapabilityCatalogQuery) -> dict[str, Any]:
        try:
            response: _HTTPResponse = await self.http_client.post(
                f"{self.base_url}/catalog",
                headers=internal_http_headers(
                    {
                        "x-ai-platform-internal-token": self.internal_token,
                        "x-ai-tenant-id": query.tenant_id,
                        "x-ai-user-id": query.user_id,
                        "x-ai-session-id": query.session_id,
                    }
                ),
                json=query.request_body(include_model=True),
            )
        except (httpx.HTTPError, ValueError) as exc:
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_UNAVAILABLE",
                "capability catalog unavailable",
            ) from exc
        if response.status_code >= 400 or len(response.content) > MAX_CATALOG_BYTES:
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_UNAVAILABLE",
                "capability catalog unavailable",
            )
        try:
            payload = response.json()
        except (ValueError, json.JSONDecodeError) as exc:
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
            ) from exc
        if not isinstance(payload, dict):
            raise CapabilityCatalogError(
                "CAPABILITY_CATALOG_INVALID", "capability catalog is invalid"
            )
        return payload


class UnavailableCapabilityCatalogClient:
    async def fetch_catalog(self, query: CapabilityCatalogQuery) -> dict[str, Any]:
        del query
        raise CapabilityCatalogError(
            "CAPABILITY_CATALOG_DEGRADED",
            "capability catalog service is not configured",
        )


__all__ = [
    "CapabilityCatalogClient",
    "CapabilityCatalogError",
    "CapabilityCatalogQuery",
    "CapabilityCatalogService",
    "CapabilityDescriptorV2",
    "HttpCapabilityCatalogClient",
    "LocalCapabilityCatalogClient",
    "UnavailableCapabilityCatalogClient",
    "descriptor_kind",
    "project_runtime_descriptor",
    "user_has_permissions",
]
