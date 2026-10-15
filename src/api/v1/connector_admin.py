"""Connector catalog admin API — manage connector_configs provider definitions.

Admin CRUD for the connector catalog surfaced at ``/settings/connectors`` in the
console. ``client_secret`` is write-only: it is accepted on create/update but
never serialized in any response, and validation errors never echo submitted
values (RedactedValidationRoute).

Reads require ``console:settings:view``. Mutations require both
``console:settings:edit`` and a tenant-administrator role.

DELETE refuses while user_connectors rows still reference the provider, so a
definition cannot be removed from under connected users.
"""

from __future__ import annotations

import asyncio
import json
import os
from collections.abc import Mapping
from typing import Any
from urllib.parse import urlsplit

from ai_gateway_core.security import encrypt_value, is_encrypted, is_safe_destination
from fastapi import APIRouter, Depends, HTTPException, Request

from ...core.auth.permissions import Capability
from ...core.auth.user_resolver import UserContext
from ..deps import AuthContext, get_auth_context, get_user_context, require_gateway_capability
from ..redacted_validation_route import RedactedValidationRoute
from ..schemas.connectors import (
    ConnectorProviderCreate,
    ConnectorProviderResponse,
    ConnectorProviderUpdate,
    ConnectorToggleRequest,
)

router = APIRouter(
    prefix="/connectors/admin",
    tags=["Connectors"],
    route_class=RedactedValidationRoute,
)

_MCP_TOOLS_EXTRA_KEY = "mcp_tools"
_TENANT_ADMIN_ROLES = {"admin", "tenant_admin", "superadmin", "super_admin"}


def _get_db(request: Request):
    return getattr(request.app.state, "database", None)


def _require_db(request: Request):
    db = _get_db(request)
    if not db:
        raise HTTPException(500, "Database not available")
    return db


def _authorize_read(request: Request, auth: AuthContext, user: UserContext) -> None:
    _require_tenant(auth, user)
    require_gateway_capability(request, auth, Capability.GATEWAY_CONNECTOR_CONFIG_READ)


def _authorize_write(request: Request, auth: AuthContext, user: UserContext) -> None:
    _require_tenant(auth, user)
    require_gateway_capability(request, auth, Capability.GATEWAY_CONNECTOR_CONFIG_WRITE)
    roles = {str(role).lower() for role in (auth.roles or [])}
    if not roles.intersection(_TENANT_ADMIN_ROLES):
        raise HTTPException(403, "Tenant administrator access is required")


def _require_tenant(auth: AuthContext, user: UserContext) -> None:
    if (
        not auth.is_authenticated
        or not user.is_authenticated
        or not user.tenant_id
        or user.tenant_id == "public"
        or auth.tenant_id != user.tenant_id
    ):
        raise HTTPException(403, "Authenticated tenant context is required")


def _connector_encryption_key(request: Request) -> str:
    injected = getattr(request.app.state, "connector_encryption_key", None)
    if isinstance(injected, str):
        return injected
    return os.environ.get("GATEWAY_ENCRYPTION_KEY", "")


def _encrypt_client_secret(request: Request, secret: str) -> str:
    key = _connector_encryption_key(request)
    if not key:
        raise HTTPException(503, "Connector secret encryption is not configured")
    encrypted = encrypt_value(secret, key)
    if not is_encrypted(encrypted):
        raise HTTPException(500, "Connector secret encryption failed")
    return encrypted


async def _validate_persisted_destination(request: Request, url: str) -> None:
    """Resolve OAuth endpoints before persisting them; runtime pins DNS again."""

    if not url:
        return
    parsed = urlsplit(url)
    hostname = parsed.hostname or ""
    port = parsed.port or 443
    validator = getattr(
        request.app.state,
        "connector_destination_validator",
        is_safe_destination,
    )
    try:
        allowed, detail = await asyncio.to_thread(validator, hostname, port)
    except Exception as exc:
        raise HTTPException(422, "OAuth destination cannot be resolved") from exc
    if allowed:
        return
    if detail and "DNS" in str(detail):
        raise HTTPException(422, "OAuth destination cannot be resolved")
    raise HTTPException(422, "OAuth destination is not permitted")


async def _validate_auth_destinations(request: Request, auth: Any) -> None:
    if auth is None:
        return
    await _validate_persisted_destination(request, str(auth.auth_url or ""))
    await _validate_persisted_destination(request, str(auth.token_url or ""))


async def _mutable_tenant_row(db: Any, *, tenant_id: str, provider: str) -> dict[str, Any]:
    """Return only the caller tenant's row; global templates are read-only."""

    row = await db.fetchrow(
        "SELECT * FROM connector_configs WHERE tenant_id = $1 AND provider = $2",
        tenant_id,
        provider,
    )
    if row:
        return dict(row)
    global_row = await db.fetchrow(
        "SELECT 1 FROM connector_configs WHERE tenant_id = '' AND provider = $1",
        provider,
    )
    if global_row:
        raise HTTPException(
            403,
            "Global connector templates are read-only; create a tenant override",
        )
    raise HTTPException(404, f"Connector not configured: {provider}")


def _row_to_response(row: dict[str, Any]) -> dict[str, Any]:
    """Project a connector_configs row onto the public read shape.

    client_secret is write-only — it is dropped here and never re-added.
    """
    raw_extra = row.get("extra_config")
    if isinstance(raw_extra, str):
        try:
            raw_extra = json.loads(raw_extra)
        except (TypeError, json.JSONDecodeError):
            raw_extra = None
    extra = dict(raw_extra) if isinstance(raw_extra, Mapping) else {}
    mcp_tools = extra.pop(_MCP_TOOLS_EXTRA_KEY, None) or []
    auth = None
    if any(
        row.get(key)
        for key in ("client_id", "auth_url", "token_url", "scopes", "redirect_uri")
    ):
        auth = {
            "client_id": row.get("client_id") or "",
            "auth_url": row.get("auth_url") or "",
            "token_url": row.get("token_url") or "",
            "scopes": row.get("scopes") or "",
            "redirect_uri": row.get("redirect_uri"),
        }
    return {
        "provider": row["provider"],
        "display_name": row["display_name"],
        "description": row.get("description"),
        "icon_url": row.get("icon_url"),
        "mode": row.get("mode") or "live",
        "enabled": bool(row.get("enabled", True)),
        "supports_sync": bool(row.get("supports_sync", False)),
        "supports_search": bool(row.get("supports_search", True)),
        "auth": auth,
        "mcp_tools": mcp_tools,
        "extra_config": extra,
        "tenant_id": row.get("tenant_id") or "",
        "created_at": row.get("created_at"),
        "updated_at": row.get("updated_at"),
    }


def _column_values(payload: ConnectorProviderCreate) -> tuple[dict[str, Any], dict[str, Any]]:
    """Split a create payload into direct columns and extra_config content."""
    body = payload.model_dump(exclude_unset=True)
    auth = body.pop("auth", None) or {}
    mcp_tools = body.pop("mcp_tools", None) or []
    extra = dict(body.pop("extra_config", None) or {})
    if mcp_tools:
        extra[_MCP_TOOLS_EXTRA_KEY] = [
            {"name": tool.get("name") or "", "description": tool.get("description") or ""}
            for tool in mcp_tools
        ]
    columns: dict[str, Any] = {}
    for key, column in (
        ("provider", "provider"),
        ("display_name", "display_name"),
        ("description", "description"),
        ("icon_url", "icon_url"),
        ("mode", "mode"),
        ("enabled", "enabled"),
        ("supports_sync", "supports_sync"),
        ("supports_search", "supports_search"),
    ):
        if key in body:
            columns[column] = body[key]
    for key, column in (
        ("client_id", "client_id"),
        ("auth_url", "auth_url"),
        ("token_url", "token_url"),
        ("scopes", "scopes"),
        ("redirect_uri", "redirect_uri"),
    ):
        if key in auth:
            columns[column] = auth[key]
    return columns, extra


async def _upsert_extra_config(db, *, tenant_id: str, provider: str, extra: dict[str, Any]) -> None:
    """Merge catalog metadata into the provider's existing extra_config JSONB."""
    await db.execute(
        """UPDATE connector_configs SET extra_config = COALESCE(extra_config, '{}'::jsonb) || $1::jsonb,
           updated_at = NOW()
           WHERE tenant_id = $2 AND provider = $3""",
        json.dumps(extra, ensure_ascii=False), tenant_id, provider,
    )


@router.get("/configs", response_model=list[ConnectorProviderResponse])
async def list_connector_configs(
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
):
    """List catalog definitions visible to the caller's tenant (global rows included)."""
    _authorize_read(request, auth, user)
    db = _require_db(request)
    rows = await db.fetch(
        """SELECT * FROM (
               SELECT DISTINCT ON (provider) * FROM connector_configs
               WHERE tenant_id = $1 OR tenant_id = ''
               ORDER BY provider, (tenant_id = $1) DESC
           ) AS effective
           ORDER BY display_name, provider""",
        user.tenant_id,
    )
    return [_row_to_response(dict(row)) for row in rows]


@router.post("/configs", response_model=ConnectorProviderResponse, status_code=201)
async def create_connector_config(
    payload: ConnectorProviderCreate,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
):
    """Create a tenant-scoped catalog definition (unique per provider)."""
    _authorize_write(request, auth, user)
    await _validate_auth_destinations(request, payload.auth)
    db = _require_db(request)
    existing = await db.fetchrow(
        "SELECT 1 FROM connector_configs WHERE tenant_id = $1 AND provider = $2",
        user.tenant_id, payload.provider,
    )
    if existing:
        raise HTTPException(409, f"Connector already configured: {payload.provider}")
    columns, extra = _column_values(payload)
    if payload.auth and payload.auth.client_secret:
        columns["client_secret"] = _encrypt_client_secret(
            request, payload.auth.client_secret
        )
    columns.setdefault("tenant_id", user.tenant_id)
    column_names = ", ".join(columns)
    placeholders = ", ".join(f"${index}" for index in range(1, len(columns) + 1))
    await db.execute(
        f"INSERT INTO connector_configs ({column_names}) VALUES ({placeholders})",
        *columns.values(),
    )
    if extra:
        await _upsert_extra_config(db, tenant_id=user.tenant_id, provider=payload.provider, extra=extra)
    row = await db.fetchrow(
        """SELECT * FROM connector_configs WHERE tenant_id = $1 AND provider = $2""",
        user.tenant_id, payload.provider,
    )
    if not row:
        raise HTTPException(500, "Failed to persist connector config")
    return _row_to_response(dict(row))


@router.put("/configs/{provider}", response_model=ConnectorProviderResponse)
async def update_connector_config(
    provider: str,
    payload: ConnectorProviderUpdate,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
):
    """Update a catalog definition; empty client_secret keeps the stored one."""
    _authorize_write(request, auth, user)
    db = _require_db(request)
    await _mutable_tenant_row(db, tenant_id=user.tenant_id, provider=provider)
    await _validate_auth_destinations(request, payload.auth)
    tenant_id = user.tenant_id

    body = payload.model_dump(exclude_unset=True)
    auth_body = body.pop("auth", None) or {}
    mcp_tools = body.pop("mcp_tools", None)
    extra_update = body.pop("extra_config", None)

    sets: list[str] = []
    values: list[Any] = []
    for key, column in (
        ("display_name", "display_name"),
        ("description", "description"),
        ("icon_url", "icon_url"),
        ("mode", "mode"),
        ("enabled", "enabled"),
        ("supports_sync", "supports_sync"),
        ("supports_search", "supports_search"),
    ):
        if key in body:
            sets.append(f"{column} = ${len(values) + 1}")
            values.append(body[key])
    for key, column in (
        ("client_id", "client_id"),
        ("auth_url", "auth_url"),
        ("token_url", "token_url"),
        ("scopes", "scopes"),
        ("redirect_uri", "redirect_uri"),
    ):
        if key in auth_body and auth_body[key] is not None:
            sets.append(f"{column} = ${len(values) + 1}")
            values.append(auth_body[key])
    secret = auth_body.get("client_secret")
    if secret:
        sets.append(f"client_secret = ${len(values) + 1}")
        values.append(_encrypt_client_secret(request, secret))
    if not sets and mcp_tools is None and extra_update is None:
        raise HTTPException(422, "No mutable fields provided")
    # Extra-only updates (mcp_tools / extra_config) merge into JSONB below;
    # skip the column UPDATE entirely so `SET , updated_at = NOW()` can never
    # be assembled from an empty set list.
    if sets:
        values.extend((tenant_id, provider))
        await db.execute(
            f"""UPDATE connector_configs SET {', '.join(sets)}, updated_at = NOW()
                WHERE tenant_id = ${len(values) - 1} AND provider = ${len(values)}""",
            *values,
        )

    if mcp_tools is not None:
        extra_merge: dict[str, Any] = {
            _MCP_TOOLS_EXTRA_KEY: [
                {"name": tool.get("name") or "", "description": tool.get("description") or ""}
                for tool in mcp_tools
            ]
        }
        await _upsert_extra_config(db, tenant_id=tenant_id, provider=provider, extra=extra_merge)
    if extra_update is not None:
        extra_update = dict(extra_update)
        extra_update.pop(_MCP_TOOLS_EXTRA_KEY, None)
        if extra_update:
            await _upsert_extra_config(db, tenant_id=tenant_id, provider=provider, extra=extra_update)

    row = await db.fetchrow(
        """SELECT * FROM connector_configs WHERE tenant_id = $1 AND provider = $2""",
        tenant_id, provider,
    )
    if not row:
        raise HTTPException(500, "Failed to persist connector config")
    return _row_to_response(dict(row))


@router.patch("/configs/{provider}/enabled", response_model=ConnectorProviderResponse)
async def toggle_connector_config(
    provider: str,
    payload: ConnectorToggleRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
):
    """Enable or disable a catalog definition without a full update."""
    _authorize_write(request, auth, user)
    db = _require_db(request)
    await _mutable_tenant_row(db, tenant_id=user.tenant_id, provider=provider)
    tenant_id = user.tenant_id
    await db.execute(
        "UPDATE connector_configs SET enabled = $1, updated_at = NOW() WHERE tenant_id = $2 AND provider = $3",
        payload.enabled, tenant_id, provider,
    )
    row = await db.fetchrow(
        """SELECT * FROM connector_configs WHERE tenant_id = $1 AND provider = $2""",
        tenant_id, provider,
    )
    if not row:
        raise HTTPException(500, "Failed to persist connector config")
    return _row_to_response(dict(row))


@router.delete("/configs/{provider}")
async def delete_connector_config(
    provider: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
):
    """Delete a catalog definition; refused while user_connectors rows exist."""
    _authorize_write(request, auth, user)
    db = _require_db(request)
    await _mutable_tenant_row(db, tenant_id=user.tenant_id, provider=provider)
    tenant_id = user.tenant_id
    user_rows = await db.fetch(
        "SELECT 1 FROM user_connectors WHERE tenant_id = $1 AND provider = $2 LIMIT 1",
        tenant_id, provider,
    )
    if user_rows:
        raise HTTPException(
            409,
            f"Connector '{provider}' has connected users; disconnect them before deleting",
        )
    await db.execute(
        "DELETE FROM connector_configs WHERE tenant_id = $1 AND provider = $2",
        tenant_id, provider,
    )
    return {"status": "deleted", "provider": provider}
