"""
Authentication API - Login, Logout, Password Management

Provides endpoints for user authentication and password management.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Request
from pydantic import BaseModel, Field, validator

from ...config.settings import Settings
from ...core.auth.jwt_config import get_jwt_algorithms, get_jwt_secret
from ...core.auth.password import (
    ALLOWED_EMAIL_DOMAIN,
    create_access_token,
    get_token_id,
    hash_password,
    is_account_locked,
    should_lock_account,
    validate_email,
    validate_password_strength,
    verify_password,
)
from ...core.auth.user_resolver import UserContext
from ...core.client_ip import get_client_ip_from_request
from ..deps import get_settings, get_user_context

# Token expiration time (3 hours)
TOKEN_EXPIRATION_HOURS = 3
TOKEN_EXPIRATION_SECONDS = TOKEN_EXPIRATION_HOURS * 3600


router = APIRouter(prefix="/auth", tags=["auth"])


# ============================================================
# Request/Response Models
# ============================================================


class LoginRequest(BaseModel):
    """Login request with email and password."""

    email: str
    password: str = Field(..., min_length=1)

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


class LoginResponse(BaseModel):
    """Login response with access token and user info."""

    access_token: str
    token_type: str = "bearer"
    expires_in: int
    user: dict[str, Any]
    force_password_change: bool


class PasswordChangeRequest(BaseModel):
    """Password change request."""

    current_password: str = Field(..., min_length=1)
    new_password: str = Field(..., min_length=8)
    confirm_password: str = Field(..., min_length=8)

    @validator("confirm_password")
    def passwords_match(cls, v, values):
        if "new_password" in values and v != values["new_password"]:
            raise ValueError("Passwords do not match")
        return v

    @validator("new_password")
    def validate_new_password(cls, v, values):
        errors = validate_password_strength(v)
        if errors:
            raise ValueError("; ".join(errors))
        # Check that new password is different from current
        if "current_password" in values and v == values["current_password"]:
            raise ValueError("New password must be different from current password")
        return v


class CurrentUserResponse(BaseModel):
    """Current user information response."""

    user_id: str
    tenant_id: str
    email: str | None
    display_name: str | None
    department: str | None
    roles: list[str]
    permissions: list[str]
    effective_permissions: list[str] = Field(default_factory=list)
    tier: str
    force_password_change: bool


# ============================================================
# Helper Functions
# ============================================================


def _get_client_ip(request: Request) -> str:
    """Extract client IP from request headers."""
    return get_client_ip_from_request(request)


def _compute_effective_permissions(
    request: Request,
    roles: list[str] | None,
    explicit_permissions: list[str] | None,
) -> list[str]:
    """Build a de-duplicated permission view for frontend governance UI."""
    effective: list[str] = []

    for perm in explicit_permissions or []:
        value = str(perm or "").strip()
        if value and value not in effective:
            effective.append(value)

    for role in roles or []:
        token = str(role or "").strip()
        if ":" in token and token not in effective:
            effective.append(token)

    dispatcher = getattr(request.app.state, "dispatcher", None)
    rbac = getattr(dispatcher, "rbac", None)
    if rbac and hasattr(rbac, "permissions_for_roles"):
        try:
            for perm in rbac.permissions_for_roles(roles or []):
                if perm not in effective:
                    effective.append(perm)
        except Exception:
            pass

    return effective


# ============================================================
# API Endpoints
# ============================================================


@router.post("/login", response_model=LoginResponse)
async def login(
    body: LoginRequest,
    request: Request,
    settings: Settings = Depends(get_settings),
):
    """
    User login with email and password.

    - Validates the configured email domain
    - Checks account lock status
    - Verifies password
    - Returns JWT token and user info
    - Indicates if password change is required
    """
    db = getattr(request.app.state, "database", None)
    if not db or not getattr(db, "enabled", False):
        raise HTTPException(status_code=503, detail="Database not available")

    client_ip = _get_client_ip(request)
    user_agent = request.headers.get("User-Agent", "")

    # Get user by email (or fallback to user_id)
    login_email = body.email
    login_prefix = login_email.split("@")[0] if login_email else ""
    user = await db.get_user_by_email(login_email)
    if not user and login_prefix:
        user = await db.get_user(login_prefix)

    auto_created = False

    if not user:
        # Log failed attempt
        await db.log_login_audit(
            user_id=None,
            email=login_email,
            action="login_failed",
            ip_address=client_ip,
            user_agent=user_agent,
            details={"reason": "user_not_found"},
        )
        raise HTTPException(status_code=401, detail="Invalid email or password")

    user_id = user.get("user_id")

    # Check if account is locked
    locked_until = user.get("locked_until")
    if is_account_locked(locked_until):
        # Normalise string-cached timestamps to a naive UTC datetime before
        # arithmetic — same defensive parse as is_account_locked. Otherwise a
        # Redis-cached user row would crash this branch with a TypeError even
        # though the lock check above passed.
        if isinstance(locked_until, str):
            try:
                locked_until = datetime.fromisoformat(locked_until.replace("Z", "+00:00"))
            except ValueError:
                locked_until = datetime.utcnow()
        if locked_until.tzinfo is not None:
            locked_until = locked_until.astimezone(timezone.utc).replace(tzinfo=None)
        remaining = max(0, (locked_until - datetime.utcnow()).seconds // 60 + 1)
        raise HTTPException(
            status_code=423, detail=f"Account is locked. Please try again in {remaining} minutes."
        )

    # Check if account is active
    if user.get("status") != "active":
        await db.log_login_audit(
            user_id=user_id,
            email=body.email,
            action="login_failed",
            ip_address=client_ip,
            user_agent=user_agent,
            details={"reason": "account_disabled"},
        )
        raise HTTPException(status_code=403, detail="Account is disabled")

    # Verify password
    password_hash = user.get("password_hash", "")
    if not verify_password(body.password, password_hash):
        # Increment failed attempts
        current_attempts = user.get("login_attempts", 0) + 1
        await db.increment_login_attempts(user_id)

        await db.log_login_audit(
            user_id=user_id,
            email=body.email,
            action="login_failed",
            ip_address=client_ip,
            user_agent=user_agent,
            details={"reason": "invalid_password", "attempts": current_attempts},
        )

        # Lock account if too many failed attempts
        if should_lock_account(current_attempts):
            await db.lock_user_account(user_id, minutes=30)
            raise HTTPException(
                status_code=423,
                detail="Account locked due to too many failed attempts. Try again in 30 minutes.",
            )

        raise HTTPException(status_code=401, detail="Invalid email or password")

    # Successful login - reset attempts and update last login
    await db.reset_login_attempts(user_id)
    await db.update_last_login(user_id, client_ip)

    await db.log_login_audit(
        user_id=user_id,
        email=body.email,
        action="login_success",
        ip_address=client_ip,
        user_agent=user_agent,
        details={"auto_created": auto_created} if auto_created else {},
    )

    # Get user permissions
    permissions = await db.get_user_permissions(user_id)

    # Generate JWT token
    roles = user.get("roles", ["user"])
    tier = user.get("tier", "normal")
    if "admin" in roles:
        tier = "admin"

    token_data = {
        "sub": user_id,
        "user_id": user_id,
        "email": user.get("email"),
        "tenant_id": user.get("tenant_id", "default"),
        "roles": roles,
        "tier": tier,
        "permissions": permissions,
    }

    expires_delta = timedelta(hours=TOKEN_EXPIRATION_HOURS)
    jwt_secret = get_jwt_secret(settings.authentication.jwt.secret)
    jwt_algorithms = get_jwt_algorithms(settings.authentication.jwt.algorithms)
    access_token, token_id = create_access_token(
        data=token_data,
        secret=jwt_secret,
        expires_delta=expires_delta,
        algorithm=jwt_algorithms[0],
    )

    # Store token in Redis for validation and revocation
    redis = getattr(request.app.state, "redis", None)
    if redis and getattr(redis, "enabled", False):
        await redis.save_token(
            token_id=token_id,
            user_id=user_id,
            data={
                "user_id": user_id,
                "email": user.get("email"),
                "roles": roles,
                "tier": tier,
                "ip": client_ip,
            },
            ttl=TOKEN_EXPIRATION_SECONDS,
        )

    return LoginResponse(
        access_token=access_token,
        expires_in=int(expires_delta.total_seconds()),
        user={
            "user_id": user_id,
            "email": user.get("email"),
            "display_name": user.get("display_name"),
            "department": user.get("department"),
            "roles": roles,
            "permissions": permissions,
            "tier": tier,
        },
        force_password_change=user.get("force_password_change", True),
    )


@router.post("/logout")
async def logout(
    request: Request,
    settings: Settings = Depends(get_settings),
    user: UserContext = Depends(get_user_context),
):
    """
    User logout.

    - Revokes the current token from Redis
    - Logs the logout event for audit purposes
    """
    if not user.is_authenticated:
        return {"status": "success", "message": "Not logged in"}

    # Revoke token from Redis
    auth_header = request.headers.get("Authorization", "")
    if auth_header.startswith("Bearer "):
        token = auth_header[7:]
        jwt_secret = get_jwt_secret(settings.authentication.jwt.secret)
        jwt_algorithms = get_jwt_algorithms(settings.authentication.jwt.algorithms)
        token_id = get_token_id(token, jwt_secret, algorithm=jwt_algorithms[0])
        if token_id:
            redis = getattr(request.app.state, "redis", None)
            if redis and getattr(redis, "enabled", False):
                await redis.revoke_token(token_id, user.user_id)

    db = getattr(request.app.state, "database", None)
    if db and getattr(db, "enabled", False):
        client_ip = _get_client_ip(request)
        user_agent = request.headers.get("User-Agent", "")
        await db.log_login_audit(
            user_id=user.user_id,
            email=None,
            action="logout",
            ip_address=client_ip,
            user_agent=user_agent,
            details={},
        )

    return {"status": "success", "message": "Logged out successfully"}


@router.post("/change-password")
async def change_password(
    body: PasswordChangeRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """
    Change user password.

    - Requires authentication
    - Verifies current password
    - Validates new password strength
    - Updates password and clears force_password_change flag
    """
    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="Authentication required")

    db = getattr(request.app.state, "database", None)
    if not db or not getattr(db, "enabled", False):
        raise HTTPException(status_code=503, detail="Database not available")

    # Get current user from database
    current_user = await db.get_user(user.user_id)
    if not current_user:
        raise HTTPException(status_code=404, detail="User not found")

    # Verify current password
    current_hash = current_user.get("password_hash", "")
    if not verify_password(body.current_password, current_hash):
        raise HTTPException(status_code=400, detail="Current password is incorrect")

    # Hash and save new password
    new_hash = hash_password(body.new_password)
    await db.update_user_password(user.user_id, new_hash)

    # Log password change
    client_ip = _get_client_ip(request)
    user_agent = request.headers.get("User-Agent", "")
    await db.log_login_audit(
        user_id=user.user_id,
        email=current_user.get("email"),
        action="password_change",
        ip_address=client_ip,
        user_agent=user_agent,
        details={},
    )

    return {"status": "success", "message": "Password changed successfully"}


@router.get("/me", response_model=CurrentUserResponse)
async def get_current_user(
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """
    Get current user information.

    Returns the authenticated user's profile, roles, and permissions.
    """
    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="Not authenticated")

    db = getattr(request.app.state, "database", None)
    if db and getattr(db, "enabled", False):
        user_data = await db.get_user(user.user_id)
        if user_data:
            permissions = await db.get_user_permissions(user.user_id)
            roles = user_data.get("roles", user.roles or [])
            effective_permissions = _compute_effective_permissions(
                request=request,
                roles=roles,
                explicit_permissions=permissions,
            )
            return CurrentUserResponse(
                user_id=user_data.get("user_id", user.user_id),
                tenant_id=user.tenant_id,
                email=user_data.get("email"),
                display_name=user_data.get("display_name"),
                department=user_data.get("department"),
                roles=roles,
                permissions=permissions,
                effective_permissions=effective_permissions,
                tier=user_data.get("tier", user.tier),
                force_password_change=user_data.get("force_password_change", False),
            )

    # Fallback to context info if DB unavailable
    fallback_permissions: list[str] = []
    fallback_roles = user.roles or []
    for token in fallback_roles:
        value = str(token or "").strip()
        if ":" in value and value not in fallback_permissions:
            fallback_permissions.append(value)

    return CurrentUserResponse(
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        email=None,
        display_name=None,
        department=None,
        roles=fallback_roles,
        permissions=fallback_permissions,
        effective_permissions=_compute_effective_permissions(
            request=request,
            roles=fallback_roles,
            explicit_permissions=fallback_permissions,
        ),
        tier=user.tier,
        force_password_change=False,
    )


@router.post("/validate-token")
async def validate_token(
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    """
    Validate current JWT token.

    Returns user info if token is valid, error otherwise.
    """
    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="Invalid or expired token")

    return {
        "valid": True,
        "user_id": user.user_id,
        "roles": user.roles,
        "tier": user.tier,
    }
