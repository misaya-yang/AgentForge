"""Concrete ``UserContext`` dataclass shared by platform services.

Only the pure-data record lives here. Request-resolution machinery
(JWT decoding, API-key lookup, the ``UserResolver`` class) stays in the
gateway at ``src.core.auth.user_resolver``. Internal services trust only
Gateway-bound identity headers and scoped leases.
"""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass
class UserContext:
    """Per-request authenticated-user context.

    Fields align with the Protocol in ``ai_gateway_core.auth.UserContextLike``
    so concrete instances structurally satisfy the protocol without any
    explicit registration.

    ``user_type`` / ``email`` / ``name`` populate the matching X-User-*
    identity headers used on internal calls. They're optional
    defaults so existing callers continue to construct UserContext with
    just ``user_id`` + ``tenant_id``.
    """

    user_id: str
    tenant_id: str = ""
    tier: str = "anonymous"  # anonymous | normal | premium | enterprise | admin
    is_authenticated: bool = False
    ip: str = ""
    roles: list[str] = field(default_factory=list)
    user_type: str = "user"  # user | admin | service
    email: str = ""
    name: str = ""


__all__ = ["UserContext"]
