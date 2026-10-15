"""
User Management API - CRUD operations for users

Provides endpoints for managing user accounts.
"""

from __future__ import annotations

from datetime import datetime
from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from pydantic import BaseModel, Field, validator

from ...core.auth.password import (
    ALLOWED_EMAIL_DOMAIN,
    DEFAULT_PASSWORD,
    extract_username_from_email,
    hash_password,
    validate_email,
)
from ...core.auth.service_access_resolver import clear_service_access_constraint_cache
from ...core.auth.user_resolver import UserContext
from ..deps import AuthContext, get_auth_context, get_dispatcher, get_user_context

router = APIRouter(prefix="/users", tags=["users"])


def _configured_default_password() -> str:
    if not DEFAULT_PASSWORD:
        raise HTTPException(
            status_code=503,
            detail="DEFAULT_USER_PASSWORD is not configured",
        )
    return DEFAULT_PASSWORD


# ============================================================
# Request/Response Models
# ============================================================


class UserCreate(BaseModel):
    """Create user request."""

    email: str
    display_name: str = Field(..., min_length=1, max_length=255)
    department: str | None = Field(
        None, max_length=50, description="Department: cs, sales, tech, admin"
    )
    roles: list[str] = Field(default=["user"])

    @validator("email")
    def normalize_email_domain(cls, v):
        raw = (v or "").strip()
        if not raw:
            raise ValueError("Email is required")
        if "@" not in raw:
            raw = f"{raw}@{ALLOWED_EMAIL_DOMAIN}"
        errors = validate_email(raw)
        if errors:
            raise ValueError("; ".join(errors))
        return raw.lower()

    @validator("department")
    def validate_department(cls, v):
        if v is not None:
            allowed = ["cs", "sales", "tech", "admin"]
            if v not in allowed:
                raise ValueError(f"Department must be one of: {', '.join(allowed)}")
        return v


class UserUpdate(BaseModel):
    """Update user request."""

    display_name: str | None = Field(None, min_length=1, max_length=255)
    department: str | None = Field(
        None, max_length=50, description="Department: cs, sales, tech, admin"
    )
    roles: list[str] | None = None
    extra_permissions: list[str] | None = Field(
        None, description="Extra permissions (directly assigned, not through roles)"
    )
    service_access_mode: str | None = Field(
        None,
        description="Service access mode: all | allowlist",
    )
    allowed_services: list[str] | None = Field(
        None,
        description="Allowed service IDs/names when allowlist mode is enabled",
    )
    denied_services: list[str] | None = Field(
        None,
        description="Denied service IDs/names (takes precedence)",
    )
    status: str | None = None
    tier: str | None = None

    @validator("department")
    def validate_department(cls, v):
        if v is not None:
            allowed = ["cs", "sales", "tech", "admin"]
            if v not in allowed:
                raise ValueError(f"Department must be one of: {', '.join(allowed)}")
        return v

    @validator("service_access_mode")
    def validate_service_access_mode(cls, v):
        if v is None:
            return v
        normalized = str(v).strip().lower()
        if normalized not in {"all", "allowlist"}:
            raise ValueError("service_access_mode must be one of: all, allowlist")
        return normalized


class ProfileUpdate(BaseModel):
    """Update own profile request (limited fields - no roles/permissions)."""

    display_name: str | None = Field(None, min_length=1, max_length=255)


class UserResponse(BaseModel):
    """User response model."""

    user_id: str
    email: str | None = None
    display_name: str | None = None
    username: str | None = None
    department: str | None = None
    roles: list[str] = []
    extra_permissions: list[str] = []
    service_access_mode: str = "all"
    allowed_services: list[str] = []
    denied_services: list[str] = []
    status: str = "active"
    tier: str = "normal"
    force_password_change: bool = True
    last_login_at: str | None = None
    created_at: str | None = None


class UserListResponse(BaseModel):
    """User list response with pagination."""

    users: list[UserResponse]
    total: int
    page: int
    page_size: int


def _normalize_service_list(values: list[str] | None) -> list[str]:
    normalized: list[str] = []
    if values is None:
        return normalized
    for value in values:
        token = str(value or "").strip()
        if token and token not in normalized:
            normalized.append(token)
    return normalized


def _extract_service_access_metadata(metadata: Any) -> dict[str, Any]:
    payload = metadata if isinstance(metadata, dict) else {}
    service_access = payload.get("service_access")
    service_access = service_access if isinstance(service_access, dict) else {}

    mode = str(service_access.get("mode") or "").strip().lower()
    if mode not in {"all", "allowlist"}:
        mode = "allowlist" if service_access.get("allowed_services") else "all"

    allowed_services = _normalize_service_list(service_access.get("allowed_services"))
    denied_services = _normalize_service_list(service_access.get("denied_services"))

    return {
        "service_access_mode": mode,
        "allowed_services": allowed_services,
        "denied_services": denied_services,
    }


def _build_user_response_payload(
    user: dict[str, Any],
    *,
    extra_permissions: list[str] | None = None,
) -> dict[str, Any]:
    payload = {k: v for k, v in user.items() if k in UserResponse.__fields__}
    payload["extra_permissions"] = extra_permissions or []
    payload.update(_extract_service_access_metadata(user.get("metadata")))
    return payload


def _tenant_scope(identity: AuthContext | UserContext) -> str:
    """Return the caller's tenant, preserving the legacy default tenant."""
    return str(identity.tenant_id or "default")


# ============================================================
# API Endpoints
# ============================================================


@router.get("", response_model=UserListResponse)
async def list_users(
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
    page: int = Query(1, ge=1),
    page_size: int = Query(20, ge=1, le=100),
    status: str | None = Query(None),
    search: str | None = Query(None),
):
    """
    List all users with pagination.

    Requires: user:list permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "user:list")

    db = getattr(request.app.state, "database", None)
    if not db or not getattr(db, "enabled", False):
        raise HTTPException(status_code=503, detail="Database not available")

    offset = (page - 1) * page_size
    users, total = await db.list_users_paginated(
        status=status,
        search=search,
        tenant_id=_tenant_scope(auth),
        limit=page_size,
        offset=offset,
    )

    return UserListResponse(
        users=[UserResponse(**_build_user_response_payload(u)) for u in users],
        total=total,
        page=page,
        page_size=page_size,
    )


@router.post("", response_model=UserResponse)
async def create_user(
    body: UserCreate,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
):
    """
    Create a new user.

    - User is created with the configured default password
    - User must change password on first login

    Requires: user:create permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "user:create")

    db = getattr(request.app.state, "database", None)
    if not db or not getattr(db, "enabled", False):
        raise HTTPException(status_code=503, detail="Database not available")

    # Check if email already exists
    existing = await db.get_user_by_email(body.email)
    if existing:
        raise HTTPException(status_code=400, detail="Email already registered")

    # Generate user_id from email
    user_id = extract_username_from_email(body.email)

    # Check if user_id already exists
    existing_id = await db.get_user(user_id)
    if existing_id:
        # Add suffix to make unique
        import uuid

        user_id = f"{user_id}_{uuid.uuid4().hex[:6]}"

    # Create user with the configured default password (env-only; no source fallback).
    password_hash = hash_password(_configured_default_password())

    user_data = {
        "user_id": user_id,
        "email": body.email,
        "display_name": body.display_name,
        "department": body.department,
        "username": user_id,
        "roles": body.roles,
        "password_hash": password_hash,
        "force_password_change": True,
        "status": "active",
        "tier": "normal",
        "tenant_id": _tenant_scope(auth),
        "created_by": auth.user_id,
    }

    await db.save_user_with_password(user_data)

    # Assign roles in user_roles table
    for role in body.roles:
        await db.assign_user_role(user_id, role, auth.user_id)

    return UserResponse(
        user_id=user_id,
        email=body.email,
        display_name=body.display_name,
        department=body.department,
        username=user_id,
        roles=body.roles,
        service_access_mode="all",
        allowed_services=[],
        denied_services=[],
        status="active",
        tier="normal",
        force_password_change=True,
        last_login_at=None,
        created_at=datetime.utcnow().isoformat(),
    )


@router.get("/{user_id}", response_model=UserResponse)
async def get_user(
    user_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
):
    """
    Get user details.

    Requires: user:list permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "user:list")

    db = getattr(request.app.state, "database", None)
    if not db or not getattr(db, "enabled", False):
        raise HTTPException(status_code=503, detail="Database not available")

    user = await db.get_user_for_tenant(user_id, _tenant_scope(auth))
    if not user:
        raise HTTPException(status_code=404, detail="User not found")

    # Get extra permissions
    extra_perms = await db.get_user_extra_permissions(user_id)
    extra_perm_codes = [p.get("permission_code") for p in extra_perms]

    response_data = _build_user_response_payload(user, extra_permissions=extra_perm_codes)
    return UserResponse(**response_data)


@router.put("/{user_id}", response_model=UserResponse)
async def update_user(
    user_id: str,
    body: UserUpdate,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
):
    """
    Update user details.

    Requires: user:edit permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "user:edit")

    db = getattr(request.app.state, "database", None)
    if not db or not getattr(db, "enabled", False):
        raise HTTPException(status_code=503, detail="Database not available")

    tenant_id = _tenant_scope(auth)
    user = await db.get_user_for_tenant(user_id, tenant_id)
    if not user:
        raise HTTPException(status_code=404, detail="User not found")

    # Build update dict (exclude extra_permissions + service access + roles as they are handled separately)
    update_data = body.dict(
        exclude_unset=True,
        exclude={"extra_permissions", "service_access_mode", "allowed_services", "denied_services", "roles"},
    )

    # === Mass-Assignment Protection ===
    # roles, tier, status are privileged fields — only admin can change them.
    # Without this gate, any user with user:edit could self-promote to admin.
    admin_field_changes = {
        field
        for field in ("roles", "tier", "status")
        if getattr(body, field) is not None
    }
    if admin_field_changes and "admin" not in auth.roles:
        raise HTTPException(
            status_code=403,
            detail=f"Only admins can modify: {', '.join(sorted(admin_field_changes))}",
        )

    if body.extra_permissions is not None and "admin" not in auth.roles:
        raise HTTPException(
            status_code=403,
            detail="Only admins can modify extra permissions",
        )

    service_policy_updated = (
        body.service_access_mode is not None
        or body.allowed_services is not None
        or body.denied_services is not None
    )
    if service_policy_updated:
        if "admin" not in auth.roles:
            raise HTTPException(
                status_code=403,
                detail="Only admins can modify service access policy",
            )

        metadata = user.get("metadata")
        metadata = dict(metadata) if isinstance(metadata, dict) else {}
        service_access = metadata.get("service_access")
        service_access = dict(service_access) if isinstance(service_access, dict) else {}

        if body.service_access_mode is not None:
            service_access["mode"] = body.service_access_mode
        if body.allowed_services is not None:
            service_access["allowed_services"] = _normalize_service_list(body.allowed_services)
        if body.denied_services is not None:
            service_access["denied_services"] = _normalize_service_list(body.denied_services)

        if "mode" not in service_access:
            service_access["mode"] = (
                "allowlist" if service_access.get("allowed_services") else "all"
            )

        metadata["service_access"] = service_access
        update_data["metadata"] = metadata

    if update_data or body.roles is not None or body.extra_permissions is not None:
        updated = await db.update_user_for_tenant(
            user_id,
            tenant_id,
            update_data,
            roles=body.roles,
            extra_permissions=body.extra_permissions,
            granted_by=auth.user_id,
        )
        if not updated:
            raise HTTPException(status_code=404, detail="User not found")
        if service_policy_updated:
            clear_service_access_constraint_cache()

    updated_user = await db.get_user_for_tenant(user_id, tenant_id)
    if not updated_user:
        raise HTTPException(status_code=404, detail="User not found")

    # Get extra permissions
    extra_perms = await db.get_user_extra_permissions(user_id)
    extra_perm_codes = [p.get("permission_code") for p in extra_perms]

    response_data = _build_user_response_payload(
        updated_user,
        extra_permissions=extra_perm_codes,
    )
    return UserResponse(**response_data)


@router.delete("/{user_id}")
async def delete_user(
    user_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
):
    """
    Delete user.

    Cannot delete self or system admin.

    Requires: user:delete permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "user:delete")

    # Cannot delete self
    if user_id == auth.user_id:
        raise HTTPException(status_code=400, detail="Cannot delete your own account")

    db = getattr(request.app.state, "database", None)
    if not db or not getattr(db, "enabled", False):
        raise HTTPException(status_code=503, detail="Database not available")

    tenant_id = _tenant_scope(auth)
    user = await db.get_user_for_tenant(user_id, tenant_id)
    if not user:
        raise HTTPException(status_code=404, detail="User not found")

    # Only the system administrator in its own tenant is protected specially.
    if user_id == "admin":
        raise HTTPException(status_code=400, detail="Cannot delete system admin")

    deleted = await db.delete_user_for_tenant(user_id, tenant_id)
    if not deleted:
        raise HTTPException(status_code=404, detail="User not found")
    clear_service_access_constraint_cache()
    return {"status": "success", "message": f"User {user_id} deleted"}


@router.post("/{user_id}/reset-password")
async def reset_user_password(
    user_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
):
    """
    Reset user password to the configured default.

    User will be required to change password on next login.

    Requires: user:edit permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "user:edit")

    db = getattr(request.app.state, "database", None)
    if not db or not getattr(db, "enabled", False):
        raise HTTPException(status_code=503, detail="Database not available")

    tenant_id = _tenant_scope(auth)
    user = await db.get_user_for_tenant(user_id, tenant_id)
    if not user:
        raise HTTPException(status_code=404, detail="User not found")

    password_hash = hash_password(_configured_default_password())
    reset = await db.reset_user_password_for_tenant(user_id, tenant_id, password_hash)
    if not reset:
        raise HTTPException(status_code=404, detail="User not found")

    return {"status": "success", "message": f"Password reset to default for user {user_id}"}


@router.post("/{user_id}/enable")
async def enable_user(
    user_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
):
    """
    Enable a disabled user.

    Requires: user:edit permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "user:edit")

    db = getattr(request.app.state, "database", None)
    if not db or not getattr(db, "enabled", False):
        raise HTTPException(status_code=503, detail="Database not available")

    tenant_id = _tenant_scope(auth)
    user = await db.get_user_for_tenant(user_id, tenant_id)
    if not user:
        raise HTTPException(status_code=404, detail="User not found")

    updated = await db.update_user_for_tenant(user_id, tenant_id, {"status": "active"})
    if not updated:
        raise HTTPException(status_code=404, detail="User not found")
    return {"status": "success", "message": f"User {user_id} enabled"}


@router.post("/{user_id}/disable")
async def disable_user(
    user_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
):
    """
    Disable a user account.

    Cannot disable self or system admin.

    Requires: user:edit permission
    """
    dispatcher = get_dispatcher(request)
    dispatcher.rbac.require(auth.roles, "user:edit")

    # Cannot disable self
    if user_id == auth.user_id:
        raise HTTPException(status_code=400, detail="Cannot disable your own account")

    db = getattr(request.app.state, "database", None)
    if not db or not getattr(db, "enabled", False):
        raise HTTPException(status_code=503, detail="Database not available")

    tenant_id = _tenant_scope(auth)
    user = await db.get_user_for_tenant(user_id, tenant_id)
    if not user:
        raise HTTPException(status_code=404, detail="User not found")

    # Only the system administrator in its own tenant is protected specially.
    if user_id == "admin":
        raise HTTPException(status_code=400, detail="Cannot disable system admin")

    updated = await db.update_user_for_tenant(user_id, tenant_id, {"status": "disabled"})
    if not updated:
        raise HTTPException(status_code=404, detail="User not found")
    return {"status": "success", "message": f"User {user_id} disabled"}


@router.put("/{user_id}/profile", response_model=UserResponse)
async def update_user_profile(
    user_id: str,
    body: ProfileUpdate,
    request: Request,
    current_user: UserContext = Depends(get_user_context),
):
    """
    Update user's own profile (limited to display_name).

    Users can only update their own profile.
    For admin-level changes (roles, permissions), use PUT /{user_id} instead.
    """
    # Users can only update their own profile
    if current_user.user_id != user_id:
        raise HTTPException(status_code=403, detail="Can only update your own profile")

    db = getattr(request.app.state, "database", None)
    if not db or not getattr(db, "enabled", False):
        raise HTTPException(status_code=503, detail="Database not available")

    tenant_id = _tenant_scope(current_user)
    user = await db.get_user_for_tenant(user_id, tenant_id)
    if not user:
        raise HTTPException(status_code=404, detail="User not found")

    # Only update allowed fields
    update_data = body.dict(exclude_unset=True)
    if update_data:
        updated = await db.update_user_for_tenant(user_id, tenant_id, update_data)
        if not updated:
            raise HTTPException(status_code=404, detail="User not found")

    updated_user = await db.get_user_for_tenant(user_id, tenant_id)
    if not updated_user:
        raise HTTPException(status_code=404, detail="User not found")

    # Get extra permissions
    extra_perms = await db.get_user_extra_permissions(user_id)
    extra_perm_codes = [p.get("permission_code") for p in extra_perms]

    response_data = {k: v for k, v in updated_user.items() if k in UserResponse.__fields__}
    response_data["extra_permissions"] = extra_perm_codes
    return UserResponse(**response_data)
