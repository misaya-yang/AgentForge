"""Tenant Tool Policy & MCP Config Admin API — ADR-002 Phase 1."""

from __future__ import annotations

import json
from typing import Any

from ai_gateway_core.logging import get_logger
from fastapi import APIRouter, Depends, HTTPException, Query, Request
from pydantic import BaseModel, Field, field_validator

from ...core.auth.user_resolver import UserContext
from ..deps import get_user_context

logger = get_logger(__name__)
router = APIRouter(prefix="/admin/tenant-policies", tags=["tenant-policies"])


# ── Pydantic Models ──────────────────────────────────────────────────


def _check_tenant_id(v: str) -> str:
    if not v.replace("-", "").replace("_", "").isalnum():
        raise ValueError("tenant_id must be alphanumeric (hyphens/underscores allowed)")
    return v


class ToolPolicyPayload(BaseModel):
    tenant_id: str = Field(..., min_length=1, max_length=64)
    allowed_tools: list[str] | None = None
    blocked_tools: list[str] | None = None
    allowed_categories: list[str] | None = None
    max_calls_per_minute: int = Field(20, ge=1, le=10000)

    _validate_tenant_id = field_validator("tenant_id")(_check_tenant_id)


class MCPConfigPayload(BaseModel):
    tenant_id: str = Field(..., min_length=1, max_length=64)
    allowed_servers: list[str]
    server_overrides: dict[str, Any] | None = None
    max_connections: int = Field(5, ge=1, le=100)

    _validate_tenant_id = field_validator("tenant_id")(_check_tenant_id)


# ── Helpers ──────────────────────────────────────────────────────────


def _is_platform_admin(user: UserContext) -> bool:
    return bool({str(role).strip().lower() for role in (user.roles or [])}
                & {"platform_admin", "superadmin", "super_admin"})


def _require_tenant_scope(user: UserContext, tenant_id: str) -> None:
    if not _is_platform_admin(user) and (not user.tenant_id or tenant_id != user.tenant_id):
        raise HTTPException(403, "Cross-tenant policy administration denied")


def _require_admin(user: UserContext) -> None:
    if _is_platform_admin(user):
        return
    roles = set(user.roles or [])
    tier = (user.tier or "").lower()
    if "admin" not in roles and tier != "admin":
        raise HTTPException(403, "Admin access required")


def _get_db(request: Request):
    db = getattr(request.app.state, "database", None)
    if db is None:
        raise HTTPException(503, "Database not available")
    return db


# ── Tool Policy CRUD ─────────────────────────────────────────────────


@router.get("/tool-policies")
async def list_tool_policies(
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """List all tenant tool policies."""
    _require_admin(user)
    db = _get_db(request)
    if _is_platform_admin(user):
        rows = await db.fetch("SELECT * FROM tenant_tool_policies ORDER BY tenant_id")
    else:
        _require_tenant_scope(user, user.tenant_id)
        rows = await db.fetch("SELECT * FROM tenant_tool_policies WHERE tenant_id = $1 ORDER BY tenant_id", user.tenant_id)
    return {"policies": [dict(r) for r in rows]}


@router.get("/tool-policies/{tenant_id}")
async def get_tool_policy(
    tenant_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Get tool policy for a specific tenant."""
    _require_admin(user)
    _require_tenant_scope(user, tenant_id)
    db = _get_db(request)
    row = await db.fetchrow(
        "SELECT * FROM tenant_tool_policies WHERE tenant_id = $1", tenant_id
    )
    if not row:
        raise HTTPException(404, f"No policy for tenant {tenant_id}")
    return dict(row)


@router.put("/tool-policies")
async def upsert_tool_policy(
    payload: ToolPolicyPayload,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Create or update a tenant tool policy."""
    _require_admin(user)
    _require_tenant_scope(user, payload.tenant_id)
    db = _get_db(request)
    await db.execute(
        """
        INSERT INTO tenant_tool_policies
            (tenant_id, allowed_tools, blocked_tools, allowed_categories,
             max_calls_per_minute, updated_at)
        VALUES ($1, $2, $3, $4, $5, NOW())
        ON CONFLICT (tenant_id) DO UPDATE SET
            allowed_tools = EXCLUDED.allowed_tools,
            blocked_tools = EXCLUDED.blocked_tools,
            allowed_categories = EXCLUDED.allowed_categories,
            max_calls_per_minute = EXCLUDED.max_calls_per_minute,
            updated_at = NOW()
        """,
        payload.tenant_id,
        payload.allowed_tools,
        payload.blocked_tools,
        payload.allowed_categories,
        payload.max_calls_per_minute,
    )
    # Invalidate cache
    svc = getattr(request.app.state, "tenant_tool_policy", None)
    if svc:
        svc.invalidate(payload.tenant_id)
    return {"status": "ok", "tenant_id": payload.tenant_id}


@router.delete("/tool-policies/{tenant_id}")
async def delete_tool_policy(
    tenant_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Delete a tenant tool policy (reverts to defaults)."""
    _require_admin(user)
    _require_tenant_scope(user, tenant_id)
    db = _get_db(request)
    await db.execute(
        "DELETE FROM tenant_tool_policies WHERE tenant_id = $1", tenant_id
    )
    svc = getattr(request.app.state, "tenant_tool_policy", None)
    if svc:
        svc.invalidate(tenant_id)
    return {"status": "deleted", "tenant_id": tenant_id}


# ── MCP Config CRUD ──────────────────────────────────────────────────


@router.get("/mcp-configs")
async def list_mcp_configs(
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """List all tenant MCP configs."""
    _require_admin(user)
    db = _get_db(request)
    if _is_platform_admin(user):
        rows = await db.fetch("SELECT * FROM tenant_mcp_configs ORDER BY tenant_id")
    else:
        _require_tenant_scope(user, user.tenant_id)
        rows = await db.fetch("SELECT * FROM tenant_mcp_configs WHERE tenant_id = $1 ORDER BY tenant_id", user.tenant_id)
    return {"configs": [dict(r) for r in rows]}


@router.get("/mcp-configs/{tenant_id}")
async def get_mcp_config(
    tenant_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Get MCP config for a specific tenant."""
    _require_admin(user)
    _require_tenant_scope(user, tenant_id)
    db = _get_db(request)
    row = await db.fetchrow(
        "SELECT * FROM tenant_mcp_configs WHERE tenant_id = $1", tenant_id
    )
    if not row:
        raise HTTPException(404, f"No MCP config for tenant {tenant_id}")
    return dict(row)


@router.put("/mcp-configs")
async def upsert_mcp_config(
    payload: MCPConfigPayload,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Create or update a tenant MCP config."""
    _require_admin(user)
    _require_tenant_scope(user, payload.tenant_id)
    db = _get_db(request)
    await db.execute(
        """
        INSERT INTO tenant_mcp_configs
            (tenant_id, allowed_servers, server_overrides, max_connections, updated_at)
        VALUES ($1, $2, $3::jsonb, $4, NOW())
        ON CONFLICT (tenant_id) DO UPDATE SET
            allowed_servers = EXCLUDED.allowed_servers,
            server_overrides = EXCLUDED.server_overrides,
            max_connections = EXCLUDED.max_connections,
            updated_at = NOW()
        """,
        payload.tenant_id,
        payload.allowed_servers,
        json.dumps(payload.server_overrides) if payload.server_overrides else None,
        payload.max_connections,
    )
    svc = getattr(request.app.state, "tenant_mcp_config", None)
    if svc:
        svc.invalidate(payload.tenant_id)
    return {"status": "ok", "tenant_id": payload.tenant_id}


@router.delete("/mcp-configs/{tenant_id}")
async def delete_mcp_config(
    tenant_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """Delete a tenant MCP config (reverts to defaults)."""
    _require_admin(user)
    _require_tenant_scope(user, tenant_id)
    db = _get_db(request)
    await db.execute(
        "DELETE FROM tenant_mcp_configs WHERE tenant_id = $1", tenant_id
    )
    svc = getattr(request.app.state, "tenant_mcp_config", None)
    if svc:
        svc.invalidate(tenant_id)
    return {"status": "deleted", "tenant_id": tenant_id}


# ── Audit Log Read ───────────────────────────────────────────────────


@router.get("/audit-log")
async def query_audit_log(
    request: Request,
    tenant_id: str | None = None,
    user_id: str | None = None,
    tool_name: str | None = None,
    limit: int = Query(50, ge=1, le=1000),
    offset: int = Query(0, ge=0),
    user: UserContext = Depends(get_user_context),
):
    """Query tool audit log with optional filters."""
    _require_admin(user)
    if not _is_platform_admin(user):
        _require_tenant_scope(user, tenant_id or user.tenant_id)
        tenant_id = user.tenant_id
    db = _get_db(request)

    conditions = []
    params: list[Any] = []
    idx = 1
    if tenant_id:
        conditions.append(f"tenant_id = ${idx}")
        params.append(tenant_id)
        idx += 1
    if user_id:
        conditions.append(f"user_id = ${idx}")
        params.append(user_id)
        idx += 1
    if tool_name:
        # Escape LIKE metacharacters to prevent wildcard abuse
        safe_name = tool_name.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")
        conditions.append(f"tool_name LIKE ${idx} ESCAPE '\\'")
        params.append(f"%{safe_name}%")
        idx += 1

    where = f"WHERE {' AND '.join(conditions)}" if conditions else ""
    params.extend([limit, offset])

    rows = await db.fetch(
        f"SELECT * FROM tool_audit_log {where} ORDER BY created_at DESC LIMIT ${idx} OFFSET ${idx + 1}",
        *params,
    )
    count_row = await db.fetchrow(
        f"SELECT COUNT(*) AS cnt FROM tool_audit_log {where}",
        *params[:-2],
    )
    total = count_row["cnt"] if count_row else 0
    return {"total": total, "items": [dict(r) for r in rows]}
