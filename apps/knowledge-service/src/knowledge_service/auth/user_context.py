"""Lightweight auth that trusts headers forwarded by the API Gateway.

The Gateway authenticates users and forwards identity information as
``X-User-Id``, ``X-Tenant-Id``, ``X-User-Tier``, and ``X-User-Type`` headers.
This module extracts those headers into a :class:`UserContext` dataclass and
exposes a FastAPI dependency for convenient injection.

When ``allow_anonymous`` is enabled in settings (development / internal calls),
requests without any identity headers are accepted with a synthetic anonymous
context.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from fastapi import HTTPException, Request


@dataclass(frozen=True)
class UserContext:
    """Resolved identity from gateway-forwarded headers."""

    user_id: str
    tenant_id: str
    user_tier: str = "normal"
    user_type: str = "user"
    roles: list = field(default_factory=lambda: ["user"])
    ip: str = ""
    is_authenticated: bool = True

    def __post_init__(self) -> None:
        anonymous = (
            self.user_id.strip().lower() == "anonymous"
            or self.user_tier.strip().lower() == "anonymous"
            or self.user_type.strip().lower() in {"anonymous", "guest"}
            or "guest" in {str(role).strip().lower() for role in (self.roles or [])}
        )
        if anonymous:
            object.__setattr__(self, "is_authenticated", False)

    @property
    def tier(self) -> str:
        """Alias for user_tier — gateway code uses user.tier."""
        return self.user_tier

    @property
    def role(self) -> str:
        return self.roles[0] if self.roles else "user"

    @property
    def is_anonymous(self) -> bool:
        return not self.is_authenticated


# Sentinel used when anonymous access is allowed but no headers are present.
ANONYMOUS_CONTEXT = UserContext(
    user_id="anonymous",
    tenant_id="default",
    user_tier="anonymous",
    user_type="anonymous",
    roles=["guest"],
    is_authenticated=False,
)


async def get_user_context(request: Request) -> UserContext:
    """FastAPI dependency: resolve user context from forwarded headers.

    If ``settings.app.allow_anonymous`` is ``True`` and no identity headers
    are present, an anonymous context is returned.  Otherwise a 401 is raised.

    Roles come through ``X-User-Roles`` as a comma-separated list. The
    gateway's JWT layer produced this list; tool_registry / any KB
    admin-only endpoints consume it for role-based checks. Without
    this parse the KB side defaults to ``["user"]`` and silently denies
    any ``role:admin`` path (cross-service drift from Phase 5a H-2).
    """
    user_id = request.headers.get("X-User-Id", "").strip()
    tenant_id = request.headers.get("X-Tenant-Id", "").strip()
    user_tier = request.headers.get("X-User-Tier", "normal").strip()
    user_type = request.headers.get("X-User-Type", "user").strip()
    roles_raw = request.headers.get("X-User-Roles", "").strip()
    roles = (
        [r.strip() for r in roles_raw.split(",") if r.strip()]
        if roles_raw
        else ["user"]
    )

    if user_id and tenant_id:
        return UserContext(
            user_id=user_id,
            tenant_id=tenant_id,
            user_tier=user_tier,
            user_type=user_type,
            roles=roles,
            is_authenticated=user_type.lower() not in {"anonymous", "guest"},
        )

    # Check if anonymous access is allowed
    settings = request.app.state.settings
    if settings.app.allow_anonymous:
        return ANONYMOUS_CONTEXT

    raise HTTPException(
        status_code=401,
        detail="Missing required identity headers (X-User-Id, X-Tenant-Id). "
        "Requests must be routed through the API Gateway.",
    )
