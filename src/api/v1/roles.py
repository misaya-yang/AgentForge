"""
Role and Permission Management API

Provides endpoints for managing roles and permissions.
"""

from __future__ import annotations

from datetime import datetime

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from pydantic import BaseModel, Field

from ..deps import (
    AuthContext,
    _normalize_permissions,
    _normalize_roles,
    get_auth_context,
    get_dispatcher,
)

router = APIRouter(prefix="/roles", tags=["roles"])


async def _require_role_account(
    request: Request, auth: AuthContext, permission: str, *, mutate: bool = False
):
    """Shared roles require a current account; global writes require platform authority."""
    if not auth.is_authenticated or not auth.user_id.strip() or not auth.tenant_id.strip():
        raise HTTPException(status_code=403, detail="Active tenant account required")
    if mutate and "platform_admin" not in auth.roles:
        raise HTTPException(status_code=403, detail="Only platform administrators can change shared roles")

    db = getattr(request.app.state, "database", None)
    if not db or not getattr(db, "enabled", False):
        raise HTTPException(status_code=503, detail="Database not available")
    account = await db.get_user_for_tenant(auth.user_id, auth.tenant_id)
    tenant = await db.get_tenant(auth.tenant_id)
    if (
        not account
        or account.get("status") != "active"
        or str(account.get("tenant_id") or "") != auth.tenant_id
        or not tenant
        or tenant.get("status") != "active"
    ):
        raise HTTPException(status_code=403, detail="Active tenant account required")
    # JWT claims and the general permission cache may outlive revoked grants.
    # Bound API keys retain their own roles/grants; owner roles cannot promote them.
    permissions = await db.get_user_permissions(auth.user_id, require_current=True)
    resolution = getattr(request.state, "_auth_resolution", {})
    if resolution.get("auth_type") == "api_key":
        key = getattr(request.state, "api_key_info", None)
        if (
            not isinstance(key, dict)
            or key.get("user_id") != auth.user_id
            or key.get("tenant_id") != auth.tenant_id
        ):
            raise HTTPException(status_code=403, detail="Active tenant account required")
        subjects = [*_normalize_roles(key.get("roles")), *_normalize_permissions(key.get("permissions"))]
    else:
        subjects = list(account.get("roles") or [])
    get_dispatcher(request).rbac.require([*subjects, *permissions], permission)

    if mutate and (
        "platform_admin" not in (account.get("roles") or [])
        or "platform_admin" not in await db.get_user_roles(auth.user_id)
    ):
        raise HTTPException(status_code=403, detail="Current platform-administrator authority required")
    return db


# ============================================================
# Request/Response Models
# ============================================================


class PermissionResponse(BaseModel):
    """Permission response model."""

    permission_code: str
    name: str
    description: str | None = None
    category: str
    resource: str
    action: str
    is_system: bool = False


class RoleCreate(BaseModel):
    """Create role request."""

    role_name: str = Field(..., min_length=1, max_length=100, pattern=r"^[a-z_]+$")
    description: str = Field(..., min_length=1, max_length=255)
    permissions: list[str] = Field(default=[])


class RoleUpdate(BaseModel):
    """Update role request."""

    description: str | None = Field(None, min_length=1, max_length=255)
    permissions: list[str] | None = None


class RoleResponse(BaseModel):
    """Role response model."""

    role_name: str
    description: str | None = None
    permissions: list[str] = []
    is_system: bool = False
    user_count: int | None = None
    created_at: str | None = None
    updated_at: str | None = None


class RoleListResponse(BaseModel):
    """Role list response."""

    roles: list[RoleResponse]
    total: int


class PermissionListResponse(BaseModel):
    """Permission list response."""

    permissions: list[PermissionResponse]
    total: int


class UserRoleAssignment(BaseModel):
    """User role assignment request."""

    user_id: str
    role_name: str


# ============================================================
# API Endpoints
# ============================================================


@router.get("", response_model=RoleListResponse)
async def list_roles(
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
):
    """
    List all roles.

    Requires: role:list permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "role:list")

    db = await _require_role_account(request, auth, "role:list")

    roles = await db.list_roles()

    # Get user count for each role
    role_responses = []
    for role in roles:
        user_count = await db.get_tenant_role_user_count(role.get("role_name", ""), auth.tenant_id)
        role_responses.append(
            RoleResponse(
                role_name=role.get("role_name", ""),
                description=role.get("description"),
                permissions=role.get("permissions", []),
                is_system=role.get("is_system", False),
                user_count=user_count,
                created_at=str(role.get("created_at")) if role.get("created_at") else None,
                updated_at=str(role.get("updated_at")) if role.get("updated_at") else None,
            )
        )

    return RoleListResponse(roles=role_responses, total=len(role_responses))


@router.post("", response_model=RoleResponse)
async def create_role(
    body: RoleCreate,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
):
    """
    Create a new role.

    Requires: role:create permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "role:create")

    db = await _require_role_account(request, auth, "role:create", mutate=True)

    # Check if role already exists
    existing = await db.get_role(body.role_name)
    if existing:
        raise HTTPException(status_code=400, detail="Role already exists")

    # Validate permissions exist
    all_permissions = await db.list_permissions()
    valid_codes = {p.get("permission_code") for p in all_permissions}
    for perm in body.permissions:
        if perm not in valid_codes and perm != "admin:*":
            raise HTTPException(status_code=400, detail=f"Invalid permission: {perm}")

    await db.create_role(body.role_name, body.description, body.permissions)

    return RoleResponse(
        role_name=body.role_name,
        description=body.description,
        permissions=body.permissions,
        is_system=False,
        user_count=0,
        created_at=datetime.utcnow().isoformat(),
    )


@router.get("/permissions", response_model=PermissionListResponse)
async def list_permissions(
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
    category: str | None = Query(None),
):
    """
    List all available permissions.

    Requires: role:list permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "role:list")

    db = await _require_role_account(request, auth, "role:list")

    permissions = await db.list_permissions(category=category)

    return PermissionListResponse(
        permissions=[
            PermissionResponse(**{k: v for k, v in p.items() if k in PermissionResponse.__fields__})
            for p in permissions
        ],
        total=len(permissions),
    )


@router.get("/{role_name}", response_model=RoleResponse)
async def get_role(
    role_name: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
):
    """
    Get role details.

    Requires: role:list permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "role:list")

    db = await _require_role_account(request, auth, "role:list")

    role = await db.get_role(role_name)
    if not role:
        raise HTTPException(status_code=404, detail="Role not found")

    user_count = await db.get_tenant_role_user_count(role_name, auth.tenant_id)

    return RoleResponse(
        role_name=role.get("role_name", ""),
        description=role.get("description"),
        permissions=role.get("permissions", []),
        is_system=role.get("is_system", False),
        user_count=user_count,
        created_at=str(role.get("created_at")) if role.get("created_at") else None,
        updated_at=str(role.get("updated_at")) if role.get("updated_at") else None,
    )


@router.put("/{role_name}", response_model=RoleResponse)
async def update_role(
    role_name: str,
    body: RoleUpdate,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
):
    """
    Update role details.

    Cannot modify system roles.

    Requires: role:edit permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "role:edit")

    db = await _require_role_account(request, auth, "role:edit", mutate=True)

    role = await db.get_role(role_name)
    if not role:
        raise HTTPException(status_code=404, detail="Role not found")

    # Cannot modify system roles
    if role.get("is_system", False):
        raise HTTPException(status_code=400, detail="Cannot modify system role")

    # Validate permissions if provided
    if body.permissions is not None:
        all_permissions = await db.list_permissions()
        valid_codes = {p.get("permission_code") for p in all_permissions}
        for perm in body.permissions:
            if perm not in valid_codes and perm != "admin:*":
                raise HTTPException(status_code=400, detail=f"Invalid permission: {perm}")

    await db.update_role(role_name, description=body.description, permissions=body.permissions)

    updated_role = await db.get_role(role_name)
    user_count = await db.get_tenant_role_user_count(role_name, auth.tenant_id)

    return RoleResponse(
        role_name=updated_role.get("role_name", ""),
        description=updated_role.get("description"),
        permissions=updated_role.get("permissions", []),
        is_system=updated_role.get("is_system", False),
        user_count=user_count,
        created_at=str(updated_role.get("created_at")) if updated_role.get("created_at") else None,
        updated_at=str(updated_role.get("updated_at")) if updated_role.get("updated_at") else None,
    )


@router.delete("/{role_name}")
async def delete_role(
    role_name: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
):
    """
    Delete role.

    Cannot delete system roles or roles with assigned users.

    Requires: role:delete permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "role:delete")

    db = await _require_role_account(request, auth, "role:delete", mutate=True)

    role = await db.get_role(role_name)
    if not role:
        raise HTTPException(status_code=404, detail="Role not found")

    # Cannot delete system roles
    if role.get("is_system", False):
        raise HTTPException(status_code=400, detail="Cannot delete system role")

    # Global deletion safety must include assignments in every tenant, even expired ones.
    user_count = await db.get_role_user_count(role_name)
    if user_count > 0:
        raise HTTPException(
            status_code=400,
            detail=f"Cannot delete role with {user_count} assigned user(s). Remove users first.",
        )

    await db.delete_role(role_name)
    return {"status": "success", "message": f"Role {role_name} deleted"}


@router.get("/{role_name}/users")
async def get_role_users(
    role_name: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
):
    """
    Get users assigned to a role.

    Requires: role:list permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "role:list")

    db = await _require_role_account(request, auth, "role:list")

    role = await db.get_role(role_name)
    if not role:
        raise HTTPException(status_code=404, detail="Role not found")

    users = await db.get_tenant_users_by_role(role_name, auth.tenant_id)

    return {"role_name": role_name, "users": users, "total": len(users)}
