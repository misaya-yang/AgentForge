"""Tenant-scoped persistence and policy queries for Agent Studio MCP.

Server definitions, credential connections and immutable tool snapshots are
separate resources. Public repository reads redact Secret Store references;
runtime authorization uses exact tenant, principal, connection, channel and
schema-hash predicates and fails closed on every uncertainty.
"""

from __future__ import annotations

import hashlib
import json
import re
import uuid
from datetime import datetime, timezone
from typing import Any, Final

from .base import BaseRepository

MCP_TRANSPORT: Final = "streamable_http"
MCP_PRINCIPALS: Final = frozenset({"service_account", "user_delegated"})
MCP_CHANNELS: Final = frozenset({"preview", "hosted_private", "hosted_public", "embed", "api"})
MCP_PUBLIC_CHANNELS: Final = frozenset({"hosted_public", "embed"})
_SECRET_REF_RE: Final = re.compile(r"^[a-z][a-z0-9+.-]*://[^\s]+$")
_SCHEMA_ANNOTATION_KEYS: Final = frozenset(
    {
        "$comment",
        "default",
        "deprecated",
        "description",
        "examples",
        "readOnly",
        "title",
        "writeOnly",
    }
)


class MCPRepositoryError(RuntimeError):
    """Base class for stable MCP persistence failures."""


class MCPNotFoundError(MCPRepositoryError):
    """A tenant-owned MCP resource is absent or deliberately hidden."""


class MCPConflictError(MCPRepositoryError):
    """A mutation conflicts with an existing active resource."""


class MCPValidationError(MCPRepositoryError):
    """A requested MCP configuration violates the closed contract."""

    def __init__(self, code: str):
        self.code = code
        super().__init__(code)


class MCPAuthorizationError(MCPRepositoryError):
    """Runtime authorization failed closed with a stable non-secret code."""

    def __init__(self, code: str):
        self.code = code
        super().__init__(code)


def canonical_schema(schema: dict[str, Any]) -> str:
    return json.dumps(schema, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def hash_tool_schema(schema: dict[str, Any]) -> str:
    return hashlib.sha256(canonical_schema(schema).encode("utf-8")).hexdigest()


def hash_tool_contract(
    schema: dict[str, Any],
    *,
    risk_level: str,
    read_only: bool,
) -> str:
    return hash_tool_schema(
        {
            "input_schema": schema,
            "risk_level": risk_level,
            "read_only": read_only,
        }
    )


def mcp_runtime_name(server_id: str | uuid.UUID, tool_id: str | uuid.UUID) -> str:
    server = uuid.UUID(str(server_id)).hex
    tool = uuid.UUID(str(tool_id)).hex
    return f"mcp_{server}__{tool}"


def _schema_validation_view(value: Any) -> Any:
    """Remove annotation-only JSON-Schema keywords at every nesting level."""

    if isinstance(value, dict):
        return {
            key: _schema_validation_view(item)
            for key, item in value.items()
            if key not in _SCHEMA_ANNOTATION_KEYS
        }
    if isinstance(value, list):
        return [_schema_validation_view(item) for item in value]
    return value


def _schema_is_backward_compatible(previous: Any, current: Any) -> bool:
    """Conservatively prove that every old input remains accepted.

    Only annotation edits and required-property removals are accepted. Existing
    properties and array ``items`` are checked recursively. Added properties
    are conservatively breaking because an old schema may have accepted the
    same name through ``additionalProperties`` or ``patternProperties``. Every
    other changed or unknown validation keyword is likewise treated as
    breaking instead of guessing at JSON-Schema semantics.
    """

    if _schema_validation_view(previous) == _schema_validation_view(current):
        return True
    if not isinstance(previous, dict) or not isinstance(current, dict):
        return False
    if previous.get("type") != current.get("type"):
        return False

    before_properties = previous.get("properties", {})
    after_properties = current.get("properties", {})
    if not isinstance(before_properties, dict) or not isinstance(after_properties, dict):
        return False
    if set(before_properties) != set(after_properties):
        return False

    before_required_raw = previous.get("required", [])
    after_required_raw = current.get("required", [])
    if not isinstance(before_required_raw, list) or not isinstance(after_required_raw, list):
        return False
    if not all(isinstance(name, str) for name in before_required_raw) or not all(
        isinstance(name, str) for name in after_required_raw
    ):
        return False
    if set(after_required_raw) - set(before_required_raw):
        return False
    if any(
        not _schema_is_backward_compatible(before_properties[name], after_properties[name])
        for name in before_properties
    ):
        return False

    before_remaining = {
        key: value
        for key, value in previous.items()
        if key not in _SCHEMA_ANNOTATION_KEYS | {"properties", "required", "items"}
    }
    after_remaining = {
        key: value
        for key, value in current.items()
        if key not in _SCHEMA_ANNOTATION_KEYS | {"properties", "required", "items"}
    }
    if _schema_validation_view(before_remaining) != _schema_validation_view(after_remaining):
        return False

    before_has_items = "items" in previous
    after_has_items = "items" in current
    if before_has_items != after_has_items:
        return False
    return not before_has_items or _schema_is_backward_compatible(
        previous["items"], current["items"]
    )


def schema_diff(
    previous: dict[str, Any] | None,
    current: dict[str, Any],
) -> dict[str, Any]:
    """Return a deterministic, conservative JSON-Schema compatibility diff."""

    current_properties = current.get("properties")
    if not isinstance(current_properties, dict):
        current_properties = {}
    if previous is None:
        return {
            "breaking": False,
            "added_properties": sorted(current_properties),
            "removed_properties": [],
            "required_added": [],
            "required_removed": [],
            "type_changes": {},
        }

    before_properties = previous.get("properties")
    if not isinstance(before_properties, dict):
        before_properties = {}
    after_properties = current_properties
    before_required = set(previous.get("required") or [])
    after_required = set(current.get("required") or [])
    shared = set(before_properties).intersection(after_properties)
    type_changes = {
        name: {
            "from": before_properties[name].get("type"),
            "to": after_properties[name].get("type"),
        }
        for name in sorted(shared)
        if before_properties[name].get("type") != after_properties[name].get("type")
    }
    removed = sorted(set(before_properties) - set(after_properties))
    required_added = sorted(after_required - before_required)
    result = {
        "added_properties": sorted(set(after_properties) - set(before_properties)),
        "removed_properties": removed,
        "required_added": required_added,
        "required_removed": sorted(before_required - after_required),
        "type_changes": type_changes,
    }
    result["breaking"] = not _schema_is_backward_compatible(previous, current)
    return result


def _row_dict(row: Any) -> dict[str, Any]:
    return dict(row) if row is not None else {}


def _json_object(value: Any) -> dict[str, Any]:
    if isinstance(value, dict):
        return value
    if isinstance(value, str):
        try:
            parsed = json.loads(value)
        except json.JSONDecodeError:
            return {}
        return parsed if isinstance(parsed, dict) else {}
    return {}


def _validate_secret_ref(secret_ref: str | None) -> None:
    if secret_ref is None:
        return
    if not _SECRET_REF_RE.fullmatch(secret_ref) or secret_ref.lower().startswith(
        ("http://", "https://")
    ):
        raise MCPValidationError("MCP_SECRET_REF_INVALID")


def _redact_server(row: Any) -> dict[str, Any]:
    item = _row_dict(row)
    return {
        key: item.get(key)
        for key in (
            "server_id",
            "name",
            "description",
            "base_url",
            "transport",
            "auth_method",
            "oauth_metadata_url",
            "oauth_resource",
            "oauth_audience",
            "allowed_origins",
            "timeout_ms",
            "max_concurrency",
            "response_limit_bytes",
            "enabled",
            "health_status",
            "circuit_state",
            "consecutive_failures",
            "last_health_at",
            "last_error_code",
            "created_at",
            "updated_at",
        )
    }


def _redact_connection(row: Any) -> dict[str, Any]:
    item = _row_dict(row)
    return {
        "connection_id": item.get("connection_id"),
        "server_id": item.get("server_id"),
        "principal_type": item.get("principal_type"),
        "owner_user_id": item.get("owner_user_id"),
        "scopes": item.get("scopes") or [],
        "audience": item.get("audience"),
        "expires_at": item.get("expires_at"),
        "revoked_at": item.get("revoked_at"),
        "enabled": item.get("enabled"),
        "credential_configured": bool(item.get("secret_ref")),
        "created_at": item.get("created_at"),
        "updated_at": item.get("updated_at"),
    }


class DatabaseMCPRepository(BaseRepository):
    """PostgreSQL MCP registry with tenant predicates on every operation."""

    def _require_enabled(self) -> None:
        if not self.enabled:
            raise MCPRepositoryError("MCP_STORAGE_UNAVAILABLE")

    async def create_server(
        self,
        *,
        tenant_id: str,
        user_id: str,
        name: str,
        description: str,
        base_url: str,
        auth_method: str,
        oauth_metadata_url: str | None,
        oauth_resource: str | None,
        oauth_audience: str | None,
        allowed_origins: list[str],
        timeout_ms: int,
        max_concurrency: int,
        response_limit_bytes: int,
    ) -> dict[str, Any]:
        self._require_enabled()
        if not 1 <= len(allowed_origins) <= 64:
            raise MCPValidationError("MCP_ALLOWED_ORIGIN_REQUIRED")
        oauth_values = (oauth_metadata_url, oauth_resource, oauth_audience)
        if auth_method == "oauth" and not all(oauth_values):
            raise MCPValidationError("MCP_OAUTH_CONFIG_INCOMPLETE")
        if auth_method != "oauth" and any(oauth_values):
            raise MCPValidationError("MCP_OAUTH_CONFIG_NOT_ALLOWED")
        try:
            row = await self.fetchrow(
                """
                INSERT INTO mcp_servers (
                    tenant_id, name, description, base_url, transport,
                    auth_method, oauth_metadata_url, oauth_resource,
                    oauth_audience, allowed_origins, timeout_ms,
                    max_concurrency, response_limit_bytes, created_by, updated_by
                ) VALUES (
                    $1, $2, $3, $4, 'streamable_http', $5, $6, $7, $8,
                    $9::text[], $10, $11, $12, $13, $13
                )
                RETURNING *
                """,
                tenant_id,
                name,
                description,
                base_url,
                auth_method,
                oauth_metadata_url,
                oauth_resource,
                oauth_audience,
                allowed_origins,
                timeout_ms,
                max_concurrency,
                response_limit_bytes,
                user_id,
            )
        except Exception as exc:
            if getattr(exc, "sqlstate", None) == "23505":
                raise MCPConflictError("MCP_SERVER_NAME_CONFLICT") from exc
            raise
        return _redact_server(row)

    async def list_servers(self, *, tenant_id: str) -> list[dict[str, Any]]:
        self._require_enabled()
        rows = await self.fetch(
            """
            SELECT * FROM mcp_servers
            WHERE tenant_id = $1 AND deleted_at IS NULL
            ORDER BY updated_at DESC, server_id
            """,
            tenant_id,
        )
        return [_redact_server(row) for row in rows]

    async def get_server(
        self,
        *,
        tenant_id: str,
        server_id: str,
        include_deleted: bool = False,
    ) -> dict[str, Any]:
        self._require_enabled()
        deleted_clause = "" if include_deleted else "AND deleted_at IS NULL"
        row = await self.fetchrow(
            f"""
            SELECT * FROM mcp_servers
            WHERE tenant_id = $1 AND server_id = $2::uuid {deleted_clause}
            """,
            tenant_id,
            server_id,
        )
        if not row:
            raise MCPNotFoundError("MCP_SERVER_NOT_FOUND")
        return _redact_server(row)

    async def update_server(
        self,
        *,
        tenant_id: str,
        server_id: str,
        user_id: str,
        changes: dict[str, Any],
    ) -> dict[str, Any]:
        self._require_enabled()
        allowed = {
            "name",
            "description",
            "base_url",
            "auth_method",
            "oauth_metadata_url",
            "oauth_resource",
            "oauth_audience",
            "allowed_origins",
            "timeout_ms",
            "max_concurrency",
            "response_limit_bytes",
            "enabled",
        }
        items = [(key, value) for key, value in changes.items() if key in allowed]
        if not items:
            return await self.get_server(tenant_id=tenant_id, server_id=server_id)
        current = await self.fetchrow(
            """
            SELECT * FROM mcp_servers
            WHERE tenant_id = $1 AND server_id = $2::uuid AND deleted_at IS NULL
            """,
            tenant_id,
            server_id,
        )
        if not current:
            raise MCPNotFoundError("MCP_SERVER_NOT_FOUND")
        merged = {**_row_dict(current), **dict(items)}
        if not 1 <= len(merged.get("allowed_origins") or []) <= 64:
            raise MCPValidationError("MCP_ALLOWED_ORIGIN_REQUIRED")
        oauth_values = (
            merged.get("oauth_metadata_url"),
            merged.get("oauth_resource"),
            merged.get("oauth_audience"),
        )
        if merged.get("auth_method") == "oauth" and not all(oauth_values):
            raise MCPValidationError("MCP_OAUTH_CONFIG_INCOMPLETE")
        if merged.get("auth_method") != "oauth" and any(oauth_values):
            raise MCPValidationError("MCP_OAUTH_CONFIG_NOT_ALLOWED")
        auth_fields = {
            "auth_method",
            "oauth_metadata_url",
            "oauth_resource",
            "oauth_audience",
        }
        if auth_fields.intersection(changes):
            active_connections = await self.fetchrow(
                """
                SELECT COUNT(*) AS count FROM mcp_connections
                WHERE tenant_id = $1 AND server_id = $2::uuid
                  AND enabled = TRUE AND revoked_at IS NULL
                """,
                tenant_id,
                server_id,
            )
            if int((active_connections or {}).get("count") or 0) > 0:
                raise MCPValidationError("MCP_CONNECTION_REBIND_REQUIRED")
        assignments: list[str] = []
        values: list[Any] = [tenant_id, server_id]
        for key, value in items:
            values.append(value)
            cast = "::text[]" if key == "allowed_origins" else ""
            assignments.append(f"{key} = ${len(values)}{cast}")
        values.append(user_id)
        assignments.extend([f"updated_by = ${len(values)}", "updated_at = NOW()"])
        try:
            row = await self.fetchrow(
                f"""
                UPDATE mcp_servers SET {", ".join(assignments)}
                WHERE tenant_id = $1 AND server_id = $2::uuid AND deleted_at IS NULL
                RETURNING *
                """,
                *values,
            )
        except Exception as exc:
            if getattr(exc, "sqlstate", None) == "23505":
                raise MCPConflictError("MCP_SERVER_NAME_CONFLICT") from exc
            raise
        if not row:
            raise MCPNotFoundError("MCP_SERVER_NOT_FOUND")
        return _redact_server(row)

    async def delete_server(
        self,
        *,
        tenant_id: str,
        server_id: str,
        user_id: str,
    ) -> None:
        self._require_enabled()
        row = await self.fetchrow(
            """
            UPDATE mcp_servers
            SET enabled = FALSE, deleted_at = NOW(), updated_at = NOW(), updated_by = $3
            WHERE tenant_id = $1 AND server_id = $2::uuid AND deleted_at IS NULL
            RETURNING server_id
            """,
            tenant_id,
            server_id,
            user_id,
        )
        if not row:
            raise MCPNotFoundError("MCP_SERVER_NOT_FOUND")
        await self.execute(
            """
            UPDATE mcp_connections
            SET enabled = FALSE, revoked_at = COALESCE(revoked_at, NOW()),
                updated_at = NOW(), updated_by = $3
            WHERE tenant_id = $1 AND server_id = $2::uuid
              AND revoked_at IS NULL
            """,
            tenant_id,
            server_id,
            user_id,
        )

    async def create_connection(
        self,
        *,
        tenant_id: str,
        server_id: str,
        user_id: str,
        principal_type: str,
        owner_user_id: str | None,
        secret_ref: str | None,
        scopes: list[str],
        audience: str | None,
        expires_at: datetime | None,
    ) -> dict[str, Any]:
        self._require_enabled()
        if principal_type not in MCP_PRINCIPALS:
            raise MCPValidationError("MCP_PRINCIPAL_TYPE_INVALID")
        if principal_type == "service_account" and owner_user_id is not None:
            raise MCPValidationError("MCP_PRINCIPAL_OWNER_INVALID")
        if principal_type == "user_delegated" and not owner_user_id:
            raise MCPValidationError("MCP_PRINCIPAL_OWNER_REQUIRED")
        _validate_secret_ref(secret_ref)
        raw_server = await self.fetchrow(
            """
            SELECT auth_method, oauth_audience FROM mcp_servers
            WHERE tenant_id = $1 AND server_id = $2::uuid
              AND enabled = TRUE AND deleted_at IS NULL
            """,
            tenant_id,
            server_id,
        )
        if not raw_server:
            raise MCPNotFoundError("MCP_SERVER_NOT_FOUND")
        auth_method = raw_server["auth_method"]
        if auth_method == "none" and secret_ref is not None:
            raise MCPValidationError("MCP_SECRET_NOT_ALLOWED")
        if auth_method in {"bearer", "oauth"} and secret_ref is None:
            raise MCPValidationError("MCP_SECRET_REF_REQUIRED")
        expected_audience = raw_server.get("oauth_audience")
        if auth_method == "oauth" and (
            not audience or not expected_audience or audience != expected_audience
        ):
            raise MCPValidationError("MCP_OAUTH_AUDIENCE_MISMATCH")
        if auth_method == "oauth" and not scopes:
            raise MCPValidationError("MCP_OAUTH_SCOPES_REQUIRED")
        try:
            row = await self.fetchrow(
                """
                INSERT INTO mcp_connections (
                    tenant_id, server_id, principal_type, owner_user_id,
                    secret_ref, scopes, audience, expires_at, created_by, updated_by
                ) VALUES (
                    $1, $2::uuid, $3, $4, $5, $6::text[], $7, $8, $9, $9
                )
                RETURNING *
                """,
                tenant_id,
                server_id,
                principal_type,
                owner_user_id,
                secret_ref,
                scopes,
                audience,
                expires_at,
                user_id,
            )
        except Exception as exc:
            if getattr(exc, "sqlstate", None) == "23505":
                raise MCPConflictError("MCP_CONNECTION_CONFLICT") from exc
            raise
        return _redact_connection(row)

    async def list_connections(
        self,
        *,
        tenant_id: str,
        server_id: str,
    ) -> list[dict[str, Any]]:
        self._require_enabled()
        rows = await self.fetch(
            """
            SELECT * FROM mcp_connections
            WHERE tenant_id = $1 AND server_id = $2::uuid
            ORDER BY created_at DESC, connection_id
            """,
            tenant_id,
            server_id,
        )
        return [_redact_connection(row) for row in rows]

    async def resolve_discovery_connection(
        self,
        *,
        tenant_id: str,
        user_id: str,
        authenticated: bool,
        server_id: str,
        connection_id: str,
        principal_type: str,
    ) -> dict[str, Any]:
        """Resolve one explicit Admin-selected connection for discovery."""

        self._require_enabled()
        if principal_type not in MCP_PRINCIPALS:
            raise MCPAuthorizationError("MCP_PRINCIPAL_POLICY_DENIED")
        row = await self.fetchrow(
            """
            SELECT srv.server_id, srv.name, srv.base_url, srv.auth_method,
                   srv.oauth_resource, srv.oauth_audience, srv.allowed_origins,
                   srv.timeout_ms, srv.max_concurrency,
                   srv.response_limit_bytes, srv.enabled,
                   c.connection_id, c.principal_type, c.owner_user_id,
                   c.secret_ref, c.scopes, c.audience, c.expires_at
            FROM mcp_servers srv
            JOIN mcp_connections c
              ON c.tenant_id = srv.tenant_id AND c.server_id = srv.server_id
            WHERE srv.tenant_id = $1 AND srv.server_id = $2::uuid
              AND c.connection_id = $3::uuid AND c.principal_type = $4
              AND srv.enabled = TRUE AND srv.deleted_at IS NULL
              AND c.enabled = TRUE AND c.revoked_at IS NULL
              AND (c.expires_at IS NULL OR c.expires_at > NOW())
            """,
            tenant_id,
            server_id,
            connection_id,
            principal_type,
        )
        if not row:
            raise MCPAuthorizationError("MCP_CONNECTION_UNAVAILABLE")
        item = _row_dict(row)
        if principal_type == "user_delegated" and (
            not authenticated or item.get("owner_user_id") != user_id
        ):
            raise MCPAuthorizationError("MCP_DELEGATED_PRINCIPAL_DENIED")
        if item["auth_method"] == "oauth" and (
            not item.get("oauth_audience") or item.get("audience") != item.get("oauth_audience")
        ):
            raise MCPAuthorizationError("MCP_OAUTH_AUDIENCE_MISMATCH")
        return item

    async def revoke_connection(
        self,
        *,
        tenant_id: str,
        connection_id: str,
        user_id: str,
    ) -> None:
        self._require_enabled()
        row = await self.fetchrow(
            """
            UPDATE mcp_connections
            SET enabled = FALSE, revoked_at = COALESCE(revoked_at, NOW()),
                updated_at = NOW(), updated_by = $3
            WHERE tenant_id = $1 AND connection_id = $2::uuid
            RETURNING connection_id
            """,
            tenant_id,
            connection_id,
            user_id,
        )
        if not row:
            raise MCPNotFoundError("MCP_CONNECTION_NOT_FOUND")

    async def grant_channel(
        self,
        *,
        tenant_id: str,
        connection_id: str,
        tool_id: str,
        channel: str,
        read_only_only: bool,
        user_id: str,
    ) -> dict[str, Any]:
        self._require_enabled()
        if channel not in MCP_CHANNELS:
            raise MCPValidationError("MCP_CHANNEL_INVALID")
        target = await self.fetchrow(
            """
            SELECT c.principal_type, c.server_id AS connection_server_id,
                   t.server_id AS tool_server_id, s.schema_hash
            FROM mcp_connections c
            JOIN mcp_tools t
              ON t.tenant_id = c.tenant_id AND t.tool_id = $3::uuid
            JOIN mcp_tool_snapshots s
              ON s.tenant_id = t.tenant_id
             AND s.snapshot_id = t.current_snapshot_id
            WHERE c.tenant_id = $1 AND c.connection_id = $2::uuid
              AND c.enabled = TRUE AND c.revoked_at IS NULL
              AND t.enabled = TRUE
            """,
            tenant_id,
            connection_id,
            tool_id,
        )
        if not target or target["connection_server_id"] != target["tool_server_id"]:
            raise MCPNotFoundError("MCP_CHANNEL_GRANT_TARGET_NOT_FOUND")
        if channel in MCP_PUBLIC_CHANNELS and (
            target["principal_type"] != "service_account" or not read_only_only
        ):
            raise MCPValidationError("MCP_PUBLIC_CHANNEL_DENIED")
        row = await self.fetchrow(
            """
            INSERT INTO mcp_channel_grants (
                tenant_id, connection_id, tool_id, channel,
                read_only_only, approved_schema_hash, authorized_by
            ) VALUES ($1, $2::uuid, $3::uuid, $4, $5, $6, $7)
            ON CONFLICT (tenant_id, connection_id, tool_id, channel)
            DO UPDATE SET read_only_only = EXCLUDED.read_only_only,
                          approved_schema_hash = EXCLUDED.approved_schema_hash,
                          enabled = TRUE,
                          authorized_by = EXCLUDED.authorized_by,
                          updated_at = NOW()
            RETURNING connection_id, tool_id, channel, read_only_only,
                      approved_schema_hash, enabled, authorized_by,
                      created_at, updated_at
            """,
            tenant_id,
            connection_id,
            tool_id,
            channel,
            read_only_only,
            target["schema_hash"],
            user_id,
        )
        if not row:
            raise MCPNotFoundError("MCP_CHANNEL_GRANT_TARGET_NOT_FOUND")
        return _row_dict(row)

    async def record_discovery(
        self,
        *,
        tenant_id: str,
        server_id: str,
        tools: list[dict[str, Any]],
    ) -> dict[str, Any]:
        """Insert immutable snapshots and move only mutable current pointers."""

        self._require_enabled()
        normalized_names = [str(tool.get("name") or "") for tool in tools]
        if any(not name or len(name) > 255 for name in normalized_names):
            raise MCPValidationError("MCP_TOOL_NAME_INVALID")
        if len(set(normalized_names)) != len(normalized_names):
            raise MCPValidationError("MCP_TOOL_NAME_DUPLICATE")

        changed: list[dict[str, Any]] = []
        unchanged: list[dict[str, Any]] = []
        async with self._pool.acquire() as conn, conn.transaction():
            server = await conn.fetchrow(
                """
                SELECT server_id FROM mcp_servers
                WHERE tenant_id = $1 AND server_id = $2::uuid
                  AND enabled = TRUE AND deleted_at IS NULL
                FOR UPDATE
                """,
                tenant_id,
                server_id,
            )
            if not server:
                raise MCPNotFoundError("MCP_SERVER_NOT_FOUND")

            existing_rows = await conn.fetch(
                """
                SELECT t.tool_id, t.upstream_name, t.runtime_name,
                       t.current_snapshot_id, s.schema_version, s.schema_hash,
                       s.contract_hash, s.input_schema, s.read_only, s.risk_level
                FROM mcp_tools t
                LEFT JOIN mcp_tool_snapshots s
                  ON s.tenant_id = t.tenant_id
                 AND s.snapshot_id = t.current_snapshot_id
                WHERE t.tenant_id = $1 AND t.server_id = $2::uuid
                FOR UPDATE OF t
                """,
                tenant_id,
                server_id,
            )
            existing = {str(row["upstream_name"]): row for row in existing_rows}

            for raw in tools:
                name = str(raw["name"])
                description = str(raw.get("description") or "")[:2000]
                input_schema = raw.get("inputSchema") or raw.get("input_schema") or {}
                if not isinstance(input_schema, dict):
                    raise MCPValidationError("MCP_TOOL_SCHEMA_INVALID")
                # Tenant MCP catalog annotations are controlled by the remote
                # server, so they can never lower platform risk or establish a
                # public-channel read-only guarantee. An exact-schema Tenant
                # Admin channel grant is the separate trusted assertion.
                risk_level = "medium"
                read_only = False
                schema_hash = hash_tool_schema(input_schema)
                contract_hash = hash_tool_contract(
                    input_schema,
                    risk_level=risk_level,
                    read_only=read_only,
                )
                old = existing.get(name)
                if old is None:
                    tool_id = uuid.uuid4()
                    runtime_name = mcp_runtime_name(server_id, tool_id)
                    await conn.execute(
                        """
                        INSERT INTO mcp_tools (
                            tenant_id, tool_id, server_id, upstream_name, runtime_name
                        ) VALUES ($1, $2, $3::uuid, $4, $5)
                        """,
                        tenant_id,
                        tool_id,
                        server_id,
                        name,
                        runtime_name,
                    )
                    previous_schema = None
                    previous_snapshot_id = None
                    schema_version = 1
                else:
                    tool_id = old["tool_id"]
                    runtime_name = str(old["runtime_name"])
                    if str(old.get("contract_hash") or "") == contract_hash:
                        await conn.execute(
                            """
                            UPDATE mcp_tools SET enabled = TRUE, updated_at = NOW()
                            WHERE tenant_id = $1 AND tool_id = $2
                            """,
                            tenant_id,
                            tool_id,
                        )
                        unchanged.append(
                            {
                                "tool_id": tool_id,
                                "runtime_name": runtime_name,
                                "schema_hash": schema_hash,
                                "schema_version": old["schema_version"],
                            }
                        )
                        continue
                    previous_schema = _json_object(old.get("input_schema"))
                    previous_snapshot_id = old.get("current_snapshot_id")
                    schema_version = int(old.get("schema_version") or 0) + 1

                diff = schema_diff(previous_schema, input_schema)
                previous_risk = str(old.get("risk_level") or "") if old else ""
                previous_read_only = bool(old.get("read_only")) if old else read_only
                diff["risk_change"] = (
                    {"from": previous_risk, "to": risk_level}
                    if previous_risk and previous_risk != risk_level
                    else None
                )
                diff["read_only_change"] = (
                    {"from": previous_read_only, "to": read_only}
                    if old is not None and previous_read_only != read_only
                    else None
                )
                if diff["risk_change"] or (previous_read_only and not read_only):
                    diff["breaking"] = True
                snapshot_id = uuid.uuid4()
                await conn.execute(
                    """
                    INSERT INTO mcp_tool_snapshots (
                        tenant_id, snapshot_id, server_id, tool_id,
                        schema_version, schema_hash, description, input_schema,
                        contract_hash, risk_level, read_only
                    ) VALUES (
                        $1, $2, $3::uuid, $4, $5, $6, $7, $8::jsonb, $9, $10, $11
                    )
                    """,
                    tenant_id,
                    snapshot_id,
                    server_id,
                    tool_id,
                    schema_version,
                    schema_hash,
                    description,
                    json.dumps(input_schema, sort_keys=True),
                    contract_hash,
                    risk_level,
                    read_only,
                )
                await conn.execute(
                    """
                    INSERT INTO mcp_schema_diffs (
                        tenant_id, server_id, tool_id, from_snapshot_id,
                        to_snapshot_id, breaking, diff
                    ) VALUES ($1, $2::uuid, $3, $4, $5, $6, $7::jsonb)
                    """,
                    tenant_id,
                    server_id,
                    tool_id,
                    previous_snapshot_id,
                    snapshot_id,
                    bool(diff["breaking"]),
                    json.dumps(diff, sort_keys=True),
                )
                await conn.execute(
                    """
                    UPDATE mcp_tools
                    SET current_snapshot_id = $3, enabled = TRUE, updated_at = NOW()
                    WHERE tenant_id = $1 AND tool_id = $2
                    """,
                    tenant_id,
                    tool_id,
                    snapshot_id,
                )
                changed.append(
                    {
                        "tool_id": tool_id,
                        "runtime_name": runtime_name,
                        "schema_hash": schema_hash,
                        "schema_version": schema_version,
                        "breaking": bool(diff["breaking"]),
                        "diff": diff,
                    }
                )

            removed_names = sorted(set(existing) - set(normalized_names))
            if removed_names:
                await conn.execute(
                    """
                    UPDATE mcp_tools SET enabled = FALSE, updated_at = NOW()
                    WHERE tenant_id = $1 AND server_id = $2::uuid
                      AND upstream_name = ANY($3::text[])
                    """,
                    tenant_id,
                    server_id,
                    removed_names,
                )
            await conn.execute(
                """
                UPDATE mcp_servers
                SET health_status = 'healthy', circuit_state = 'closed',
                    consecutive_failures = 0, circuit_open_until = NULL,
                    last_health_at = NOW(), last_error_code = NULL,
                    updated_at = NOW()
                WHERE tenant_id = $1 AND server_id = $2::uuid
                """,
                tenant_id,
                server_id,
            )

        return {
            "server_id": server_id,
            "changed": changed,
            "unchanged": unchanged,
            "removed": removed_names,
            "breaking": any(item["breaking"] for item in changed) or bool(removed_names),
        }

    async def list_tools(
        self,
        *,
        tenant_id: str,
        server_id: str,
    ) -> list[dict[str, Any]]:
        self._require_enabled()
        rows = await self.fetch(
            """
            SELECT t.tool_id, t.server_id, t.upstream_name, t.runtime_name,
                   t.enabled, s.snapshot_id, s.schema_version, s.schema_hash,
                   s.description, s.input_schema, s.risk_level, s.read_only,
                   s.discovered_at
            FROM mcp_tools t
            JOIN mcp_tool_snapshots s
              ON s.tenant_id = t.tenant_id
             AND s.snapshot_id = t.current_snapshot_id
            WHERE t.tenant_id = $1 AND t.server_id = $2::uuid
            ORDER BY t.upstream_name
            """,
            tenant_id,
            server_id,
        )
        result: list[dict[str, Any]] = []
        for row in rows:
            item = _row_dict(row)
            item["input_schema"] = _json_object(item.get("input_schema"))
            result.append(item)
        return result

    async def authorize_mcp_tool(
        self,
        *,
        tenant_id: str,
        user_id: str,
        authenticated: bool,
        runtime_name: str,
        schema_hash: str,
        risk_level: str,
        connection_id: str,
        principal_type: str,
        channel: str,
    ) -> dict[str, Any]:
        """Resolve one exact tool/credential principal or deny without fallback."""

        self._require_enabled()
        if principal_type not in MCP_PRINCIPALS or channel not in MCP_CHANNELS:
            raise MCPAuthorizationError("MCP_PRINCIPAL_POLICY_DENIED")
        normalized_hash = schema_hash.removeprefix("sha256:")
        row = await self.fetchrow(
            """
            SELECT t.tool_id, t.server_id, t.upstream_name, t.runtime_name,
                   s.schema_version, s.schema_hash, s.description,
                   s.input_schema, s.risk_level, s.read_only,
                   srv.base_url, srv.auth_method, srv.oauth_resource,
                   srv.oauth_audience, srv.allowed_origins, srv.timeout_ms,
                   srv.max_concurrency, srv.response_limit_bytes,
                   srv.health_status, srv.circuit_state, srv.circuit_open_until,
                   srv.updated_at AS server_updated_at,
                   c.connection_id, c.principal_type, c.owner_user_id,
                   c.secret_ref, c.scopes, c.audience, c.expires_at,
                   c.updated_at AS credential_updated_at
            FROM mcp_tools t
            JOIN mcp_tool_snapshots s
              ON s.tenant_id = t.tenant_id
             AND s.snapshot_id = t.current_snapshot_id
            JOIN mcp_servers srv
              ON srv.tenant_id = t.tenant_id AND srv.server_id = t.server_id
            JOIN mcp_connections c
              ON c.tenant_id = t.tenant_id AND c.server_id = t.server_id
            WHERE t.tenant_id = $1
              AND t.runtime_name = $2
              AND s.schema_hash = $3
              AND c.connection_id = $4::uuid
              AND c.principal_type = $5
              AND t.enabled = TRUE
              AND srv.enabled = TRUE AND srv.deleted_at IS NULL
              AND c.enabled = TRUE AND c.revoked_at IS NULL
              AND (c.expires_at IS NULL OR c.expires_at > NOW())
            """,
            tenant_id,
            runtime_name,
            normalized_hash,
            connection_id,
            principal_type,
        )
        if not row:
            raise MCPAuthorizationError("MCP_CAPABILITY_UNAVAILABLE")
        item = _row_dict(row)
        item["input_schema"] = _json_object(item.get("input_schema"))
        if str(item.get("risk_level") or "") != risk_level:
            raise MCPAuthorizationError("MCP_RISK_CHANGED")
        if item["circuit_state"] == "open" and (
            item.get("circuit_open_until") is None
            or item["circuit_open_until"] > datetime.now(timezone.utc)
        ):
            raise MCPAuthorizationError("MCP_CIRCUIT_OPEN")
        if item["auth_method"] == "oauth" and (
            not item.get("oauth_audience") or item.get("audience") != item.get("oauth_audience")
        ):
            raise MCPAuthorizationError("MCP_OAUTH_AUDIENCE_MISMATCH")
        if principal_type == "user_delegated":
            if not authenticated or not user_id or item.get("owner_user_id") != user_id:
                raise MCPAuthorizationError("MCP_DELEGATED_PRINCIPAL_DENIED")
            if channel in MCP_PUBLIC_CHANNELS:
                raise MCPAuthorizationError("MCP_PUBLIC_CHANNEL_DENIED")
        elif not authenticated and channel not in MCP_PUBLIC_CHANNELS:
            raise MCPAuthorizationError("MCP_SERVICE_PRINCIPAL_DENIED")

        if channel in MCP_PUBLIC_CHANNELS:
            grant = await self.fetchrow(
                """
                SELECT read_only_only, approved_schema_hash
                FROM mcp_channel_grants
                WHERE tenant_id = $1 AND connection_id = $2::uuid
                  AND tool_id = $3 AND channel = $4 AND enabled = TRUE
                  AND approved_schema_hash = $5
                """,
                tenant_id,
                connection_id,
                item["tool_id"],
                channel,
                item["schema_hash"],
            )
            if not grant or not bool(grant["read_only_only"]):
                raise MCPAuthorizationError("MCP_PUBLIC_CHANNEL_DENIED")
            item["read_only"] = True
            item["admin_read_only_approved"] = True
        return item

    async def validate_version_binding(
        self,
        *,
        tenant_id: str,
        capability_type: str,
        resource_id: str,
        schema_hash: str | None,
        risk_level: str | None,
        config: dict[str, Any],
        connection: Any | None = None,
    ) -> None:
        """Reject publish-time drift or implicit credential principal config."""

        self._require_enabled()
        fetchrow = connection.fetchrow if connection is not None else self.fetchrow
        if capability_type == "mcp":
            principal_type = str(config.get("principal_type") or "")
            connection_id = str(config.get("connection_id") or "")
            if principal_type not in MCP_PRINCIPALS or not connection_id or not schema_hash:
                raise MCPValidationError("MCP_BINDING_INCOMPLETE")
            tool = await fetchrow(
                """
                SELECT s.schema_hash, s.risk_level, srv.auth_method,
                       srv.oauth_audience, c.audience, c.scopes
                FROM mcp_tools t
                JOIN mcp_tool_snapshots s
                  ON s.tenant_id = t.tenant_id
                 AND s.snapshot_id = t.current_snapshot_id
                JOIN mcp_servers srv
                  ON srv.tenant_id = t.tenant_id AND srv.server_id = t.server_id
                JOIN mcp_connections c
                  ON c.tenant_id = t.tenant_id AND c.server_id = t.server_id
                WHERE t.tenant_id = $1 AND t.runtime_name = $2
                  AND c.connection_id = $3::uuid
                  AND c.principal_type = $4
                  AND t.enabled = TRUE AND srv.enabled = TRUE
                  AND srv.deleted_at IS NULL
                  AND c.enabled = TRUE AND c.revoked_at IS NULL
                  AND (c.expires_at IS NULL OR c.expires_at > NOW())
                """,
                tenant_id,
                resource_id,
                connection_id,
                principal_type,
            )
            if not tool:
                raise MCPValidationError("MCP_CAPABILITY_UNAVAILABLE")
            if str(tool["schema_hash"]) != schema_hash.removeprefix("sha256:"):
                raise MCPValidationError("MCP_SCHEMA_CHANGED")
            if not risk_level or str(tool["risk_level"]) != risk_level:
                raise MCPValidationError("MCP_RISK_CHANGED")
            if tool["auth_method"] == "oauth" and (
                not tool.get("oauth_audience")
                or tool.get("audience") != tool.get("oauth_audience")
                or not (tool.get("scopes") or [])
            ):
                raise MCPValidationError("MCP_OAUTH_BINDING_INVALID")
            return
        if capability_type == "connector":
            provider = str(config.get("provider") or "")
            principal_type = str(config.get("principal_type") or "")
            grant_id = str(config.get("grant_id") or "")
            tool_name = str(config.get("tool_name") or "")
            if not provider or tool_name != resource_id:
                raise MCPValidationError("CONNECTOR_BINDING_INCOMPLETE")

            # Credential-principal bindings remain the legacy Confluence
            # contract. A partially supplied principal must never fall
            # through to the grantless catalog path.
            if principal_type or grant_id:
                if (
                    provider != "confluence"
                    or principal_type not in MCP_PRINCIPALS
                    or not grant_id
                    or tool_name not in {"confluence_read", "confluence_write"}
                ):
                    raise MCPValidationError("CONNECTOR_BINDING_INCOMPLETE")
                expected_risk = "low" if tool_name == "confluence_read" else "high"
                if risk_level != expected_risk:
                    raise MCPValidationError("CONNECTOR_RISK_INVALID")
                grant = await fetchrow(
                    """
                    SELECT grant_id, scopes FROM connector_credential_principals
                    WHERE tenant_id = $1 AND grant_id = $2::uuid
                      AND provider = $3 AND principal_type = $4
                      AND enabled = TRUE AND revoked_at IS NULL
                      AND (expires_at IS NULL OR expires_at > NOW())
                    """,
                    tenant_id,
                    grant_id,
                    provider,
                    principal_type,
                )
                if not grant:
                    raise MCPValidationError("CONNECTOR_CAPABILITY_UNAVAILABLE")
                scopes = {str(scope).lower() for scope in (grant.get("scopes") or [])}
                if tool_name == "confluence_read" and not any(
                    scope == "read" or scope.startswith(("read:", "write:"))
                    for scope in scopes
                ):
                    raise MCPValidationError("CONNECTOR_SCOPE_DENIED")
                if tool_name == "confluence_write" and not any(
                    scope == "write" or scope.startswith("write:") for scope in scopes
                ):
                    raise MCPValidationError("CONNECTOR_SCOPE_DENIED")
                return

            catalog = await fetchrow(
                """
                SELECT provider, mode, enabled FROM connector_configs
                WHERE provider = $1 AND (tenant_id = $2 OR tenant_id = '')
                ORDER BY tenant_id DESC LIMIT 1
                """,
                provider,
                tenant_id,
            )
            if not catalog or not catalog.get("enabled"):
                raise MCPValidationError("CONNECTOR_CATALOG_UNAVAILABLE")
            if str(catalog.get("mode") or "live") == "ingest":
                raise MCPValidationError("CONNECTOR_CATALOG_INGEST_ONLY")
            return

    async def authorize_connector_tool(
        self,
        *,
        tenant_id: str,
        user_id: str,
        authenticated: bool,
        provider: str,
        tool_name: str,
        principal_type: str,
        grant_id: str,
        channel: str,
    ) -> dict[str, Any]:
        self._require_enabled()
        if (
            provider != "confluence"
            or tool_name not in {"confluence_read", "confluence_write"}
            or principal_type not in MCP_PRINCIPALS
        ):
            raise MCPAuthorizationError("CONNECTOR_PRINCIPAL_POLICY_DENIED")
        row = await self.fetchrow(
            """
            SELECT * FROM connector_credential_principals
            WHERE tenant_id = $1 AND grant_id = $2::uuid
              AND provider = $3 AND principal_type = $4
              AND enabled = TRUE AND revoked_at IS NULL
              AND (expires_at IS NULL OR expires_at > NOW())
            """,
            tenant_id,
            grant_id,
            provider,
            principal_type,
        )
        if not row:
            raise MCPAuthorizationError("CONNECTOR_CAPABILITY_UNAVAILABLE")
        item = _row_dict(row)
        item["connection_metadata"] = _json_object(item.get("connection_metadata"))
        if principal_type == "user_delegated" and (
            not authenticated
            or not user_id
            or item.get("owner_user_id") != user_id
            or channel in MCP_PUBLIC_CHANNELS
        ):
            raise MCPAuthorizationError("CONNECTOR_DELEGATED_PRINCIPAL_DENIED")
        if (
            principal_type == "service_account"
            and not authenticated
            and channel not in MCP_PUBLIC_CHANNELS
        ):
            raise MCPAuthorizationError("CONNECTOR_SERVICE_PRINCIPAL_DENIED")
        if channel not in set(item.get("allowed_channels") or []):
            raise MCPAuthorizationError("CONNECTOR_CHANNEL_DENIED")
        if channel in MCP_PUBLIC_CHANNELS and principal_type != "service_account":
            raise MCPAuthorizationError("CONNECTOR_PUBLIC_CHANNEL_DENIED")
        if channel in MCP_PUBLIC_CHANNELS and tool_name != "confluence_read":
            raise MCPAuthorizationError("CONNECTOR_PUBLIC_CHANNEL_DENIED")
        scopes = {str(scope).lower() for scope in (item.get("scopes") or [])}
        if tool_name == "confluence_read" and not any(
            scope == "read" or scope.startswith(("read:", "write:")) for scope in scopes
        ):
            raise MCPAuthorizationError("CONNECTOR_SCOPE_DENIED")
        if tool_name == "confluence_write" and not any(
            scope == "write" or scope.startswith("write:") for scope in scopes
        ):
            raise MCPAuthorizationError("CONNECTOR_SCOPE_DENIED")
        return item

    async def authorize_connector_catalog(
        self,
        *,
        tenant_id: str,
        user_id: str,
        authenticated: bool,
        provider: str,
        tool_name: str,
        channel: str,
    ) -> dict[str, Any]:
        """Authorize a catalog-model connector binding (no credential grant).

        Catalog bindings carry only ``provider`` + ``tool_name`` and are
        effective when (a) the provider has an enabled ``connector_configs``
        row visible to the tenant (tenant-scoped row wins over global, mirror
        of the OAuth flow lookup), (b) the provider's mode allows live tool
        use (``live`` or ``both``, not ``ingest``), and (c) the calling user
        holds a ``user_connectors`` row with status ``connected``. Catalog
        connections are inherently user-delegated, so unauthenticated callers
        and public channels are denied, mirroring the grant-based path.
        """
        self._require_enabled()
        if not provider or not tool_name:
            raise MCPAuthorizationError("CONNECTOR_CATALOG_INCOMPLETE")
        config = await self.fetchrow(
            """
            SELECT provider, mode, enabled FROM connector_configs
            WHERE provider = $1 AND (tenant_id = $2 OR tenant_id = '')
            ORDER BY tenant_id DESC LIMIT 1
            """,
            provider,
            tenant_id,
        )
        if not config or not config.get("enabled"):
            raise MCPAuthorizationError("CONNECTOR_CATALOG_UNAVAILABLE")
        if str(config.get("mode") or "live") == "ingest":
            raise MCPAuthorizationError("CONNECTOR_CATALOG_INGEST_ONLY")
        if channel in MCP_PUBLIC_CHANNELS or not authenticated or not user_id:
            raise MCPAuthorizationError("CONNECTOR_CATALOG_PRINCIPAL_DENIED")
        connection = await self.fetchrow(
            """
            SELECT user_id, provider, status FROM user_connectors
            WHERE tenant_id = $1 AND user_id = $2 AND provider = $3
            """,
            tenant_id,
            user_id,
            provider,
        )
        if not connection or str(connection.get("status") or "") != "connected":
            raise MCPAuthorizationError("CONNECTOR_CATALOG_NOT_CONNECTED")
        return {
            "provider": provider,
            "tool_name": tool_name,
            "user_id": user_id,
        }

    async def create_connector_principal(
        self,
        *,
        tenant_id: str,
        user_id: str,
        provider: str,
        principal_type: str,
        owner_user_id: str | None,
        secret_ref: str,
        scopes: list[str],
        audience: str | None,
        connection_metadata: dict[str, Any],
        allowed_channels: list[str],
        expires_at: datetime | None,
    ) -> dict[str, Any]:
        self._require_enabled()
        if provider != "confluence" or principal_type not in MCP_PRINCIPALS:
            raise MCPValidationError("CONNECTOR_PRINCIPAL_INVALID")
        if principal_type == "service_account" and owner_user_id is not None:
            raise MCPValidationError("CONNECTOR_PRINCIPAL_OWNER_INVALID")
        if principal_type == "user_delegated" and not owner_user_id:
            raise MCPValidationError("CONNECTOR_PRINCIPAL_OWNER_REQUIRED")
        if not set(allowed_channels).issubset(MCP_CHANNELS):
            raise MCPValidationError("CONNECTOR_CHANNEL_INVALID")
        if principal_type == "user_delegated" and set(allowed_channels).intersection(
            MCP_PUBLIC_CHANNELS
        ):
            raise MCPValidationError("CONNECTOR_PUBLIC_CHANNEL_DENIED")
        if not scopes:
            raise MCPValidationError("CONNECTOR_SCOPES_REQUIRED")
        _validate_secret_ref(secret_ref)
        allowed_metadata = {"domain", "email", "site_name", "cloud_id"}
        if set(connection_metadata) - allowed_metadata:
            raise MCPValidationError("CONNECTOR_METADATA_INVALID")
        try:
            row = await self.fetchrow(
                """
                INSERT INTO connector_credential_principals (
                    tenant_id, provider, principal_type, owner_user_id,
                    secret_ref, scopes, audience, connection_metadata,
                    allowed_channels, expires_at, created_by, updated_by
                ) VALUES (
                    $1, $2, $3, $4, $5, $6::text[], $7, $8::jsonb,
                    $9::text[], $10, $11, $11
                )
                RETURNING grant_id, provider, principal_type, owner_user_id,
                          scopes, audience, connection_metadata, allowed_channels,
                          expires_at, revoked_at, enabled, created_at, updated_at,
                          TRUE AS credential_configured
                """,
                tenant_id,
                provider,
                principal_type,
                owner_user_id,
                secret_ref,
                scopes,
                audience,
                json.dumps(connection_metadata, sort_keys=True),
                allowed_channels,
                expires_at,
                user_id,
            )
        except Exception as exc:
            if getattr(exc, "sqlstate", None) == "23505":
                raise MCPConflictError("CONNECTOR_PRINCIPAL_CONFLICT") from exc
            raise
        item = _row_dict(row)
        item["connection_metadata"] = _json_object(item.get("connection_metadata"))
        return item

    async def list_connector_principals(
        self,
        *,
        tenant_id: str,
        provider: str,
    ) -> list[dict[str, Any]]:
        self._require_enabled()
        rows = await self.fetch(
            """
            SELECT grant_id, provider, principal_type, owner_user_id,
                   scopes, audience, connection_metadata, allowed_channels,
                   expires_at, revoked_at, enabled, created_at, updated_at,
                   (secret_ref IS NOT NULL) AS credential_configured
            FROM connector_credential_principals
            WHERE tenant_id = $1 AND provider = $2
            ORDER BY created_at DESC, grant_id
            """,
            tenant_id,
            provider,
        )
        result: list[dict[str, Any]] = []
        for row in rows:
            item = _row_dict(row)
            item["connection_metadata"] = _json_object(item.get("connection_metadata"))
            result.append(item)
        return result

    async def revoke_connector_principal(
        self,
        *,
        tenant_id: str,
        grant_id: str,
        user_id: str,
    ) -> None:
        self._require_enabled()
        row = await self.fetchrow(
            """
            UPDATE connector_credential_principals
            SET enabled = FALSE, revoked_at = COALESCE(revoked_at, NOW()),
                updated_at = NOW(), updated_by = $3
            WHERE tenant_id = $1 AND grant_id = $2::uuid
            RETURNING grant_id
            """,
            tenant_id,
            grant_id,
            user_id,
        )
        if not row:
            raise MCPNotFoundError("CONNECTOR_PRINCIPAL_NOT_FOUND")

    async def record_runtime_result(
        self,
        *,
        tenant_id: str,
        server_id: str,
        success: bool,
        error_code: str | None = None,
        counts_toward_circuit: bool = True,
        failure_threshold: int = 3,
        recovery_seconds: int = 30,
    ) -> None:
        self._require_enabled()
        if success:
            await self.execute(
                """
                UPDATE mcp_servers
                SET health_status = 'healthy', circuit_state = 'closed',
                    consecutive_failures = 0, circuit_open_until = NULL,
                    last_health_at = NOW(), last_error_code = NULL,
                    updated_at = NOW()
                WHERE tenant_id = $1 AND server_id = $2::uuid
                """,
                tenant_id,
                server_id,
            )
            return
        if not counts_toward_circuit:
            await self.execute(
                """
                UPDATE mcp_servers
                SET last_health_at = NOW(), last_error_code = $3,
                    updated_at = NOW()
                WHERE tenant_id = $1 AND server_id = $2::uuid
                """,
                tenant_id,
                server_id,
                (error_code or "MCP_OPERATION_NOT_COUNTED")[:64],
            )
            return
        await self.execute(
            """
            UPDATE mcp_servers
            SET consecutive_failures = consecutive_failures + 1,
                health_status = CASE
                    WHEN consecutive_failures + 1 >= $3 THEN 'unavailable'
                    ELSE 'degraded'
                END,
                circuit_state = CASE
                    WHEN consecutive_failures + 1 >= $3 THEN 'open'
                    ELSE circuit_state
                END,
                circuit_open_until = CASE
                    WHEN consecutive_failures + 1 >= $3
                    THEN NOW() + ($4::integer * INTERVAL '1 second')
                    ELSE circuit_open_until
                END,
                last_health_at = NOW(), last_error_code = $5,
                updated_at = NOW()
            WHERE tenant_id = $1 AND server_id = $2::uuid
            """,
            tenant_id,
            server_id,
            max(1, failure_threshold),
            max(1, recovery_seconds),
            (error_code or "MCP_UPSTREAM_UNAVAILABLE")[:64],
        )


class DatabaseMCPAgentCapabilityResolver:
    """Gateway adapter: authorize only exact Version-bound MCP/Connector IDs."""

    def __init__(
        self,
        repository: DatabaseMCPRepository,
        *,
        mcp_enabled: bool = True,
        skill_repository: Any | None = None,
    ):
        self._repository = repository
        self._mcp_enabled = mcp_enabled
        self._skill_repository = skill_repository

    async def resolve(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        bindings: list[dict[str, Any]],
        channel: str,
        channel_policy: dict[str, Any],
        user_id: str = "",
        authenticated: bool = False,
        is_tenant_admin: bool = False,
        **_kwargs: Any,
    ) -> list[dict[str, Any]]:
        del agent_id, channel_policy, is_tenant_admin
        allowed: list[dict[str, Any]] = []
        for binding in bindings:
            capability_type = str(binding.get("capability_type") or binding.get("type") or "")
            resource_id = str(binding.get("resource_id") or binding.get("id") or "")
            if capability_type in {"native", "model_native"} and resource_id:
                allowed.append(binding)
                continue
            config = binding.get("config")
            config = config if isinstance(config, dict) else {}
            try:
                if capability_type == "skill":
                    if self._skill_repository is None:
                        continue
                    version_id = str(binding.get("resource_version") or "")
                    if not resource_id or not version_id:
                        continue
                    artifact = await self._skill_repository.authorize_version(
                        tenant_id=tenant_id,
                        user_id=user_id,
                        version_id=version_id,
                        allow_tenant_admin=False,
                    )
                    if (
                        str(artifact.get("name") or "") != resource_id
                        or str(artifact.get("version_id") or "") != version_id
                    ):
                        continue
                    allowed.append(binding)
                elif not self._mcp_enabled:
                    continue
                elif capability_type == "mcp":
                    schema_hash = str(binding.get("schema_hash") or "")
                    connection_id = str(config.get("connection_id") or "")
                    principal_type = str(config.get("principal_type") or "")
                    if not all((resource_id, schema_hash, connection_id, principal_type)):
                        continue
                    await self._repository.authorize_mcp_tool(
                        tenant_id=tenant_id,
                        user_id=user_id,
                        authenticated=authenticated,
                        runtime_name=resource_id,
                        schema_hash=schema_hash,
                        risk_level=str(config.get("risk") or binding.get("risk") or ""),
                        connection_id=connection_id,
                        principal_type=principal_type,
                        channel=channel,
                    )
                    allowed.append(binding)
                elif capability_type == "connector":
                    provider = str(config.get("provider") or "")
                    principal_type = str(config.get("principal_type") or "")
                    grant_id = str(config.get("grant_id") or "")
                    tool_name = str(config.get("tool_name") or resource_id)
                    if tool_name != resource_id or not provider or not resource_id:
                        continue
                    if grant_id or principal_type:
                        if not grant_id or not principal_type:
                            continue
                        # Grant-based binding: full credential-principal check.
                        await self._repository.authorize_connector_tool(
                            tenant_id=tenant_id,
                            user_id=user_id,
                            authenticated=authenticated,
                            provider=provider,
                            tool_name=resource_id,
                            principal_type=principal_type,
                            grant_id=grant_id,
                            channel=channel,
                        )
                    else:
                        # Catalog binding: enabled connector_configs row plus a
                        # connected user_connectors row for the calling user.
                        await self._repository.authorize_connector_catalog(
                            tenant_id=tenant_id,
                            user_id=user_id,
                            authenticated=authenticated,
                            provider=provider,
                            tool_name=resource_id,
                            channel=channel,
                        )
                    allowed.append(binding)
            except (MCPAuthorizationError, MCPRepositoryError):
                continue
            except Exception:  # Skill authorization is also fail-closed.
                if capability_type != "skill":
                    raise
                continue
        return allowed


__all__ = [
    "DatabaseMCPRepository",
    "DatabaseMCPAgentCapabilityResolver",
    "MCPAuthorizationError",
    "MCPConflictError",
    "MCPNotFoundError",
    "MCPRepositoryError",
    "MCPValidationError",
    "canonical_schema",
    "hash_tool_schema",
    "hash_tool_contract",
    "mcp_runtime_name",
    "schema_diff",
]
