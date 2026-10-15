"""
Authentication configuration and header management.

The ``AuthManager`` produces the standard header set expected by the
AI Gateway on every request:

    X-API-Key, X-Tenant-Id, X-User-Id, X-SDK-Version, X-Request-Id, Content-Type
"""

from __future__ import annotations

import uuid
from dataclasses import dataclass

from ai_assistant import __version__


@dataclass(slots=True)
class AuthConfig:
    """Credentials and connection settings."""

    api_key: str
    """API key issued by the gateway."""

    tenant_id: str = ""
    """Multi-tenant identifier.  Empty string = default tenant."""

    base_url: str = "http://localhost:8080"
    """Gateway base URL (no trailing slash)."""

    user_id: str | None = None
    """Optional user identifier for per-user session scoping."""

    timeout: float = 30.0
    """Default HTTP timeout in seconds."""

    max_retries: int = 3
    """Max automatic retries for read-only GET/HEAD requests on transient failures."""


class AuthManager:
    """Generates per-request headers from a frozen ``AuthConfig``."""

    def __init__(self, config: AuthConfig) -> None:
        self.config = config

    # -- public API ----------------------------------------------------------

    @property
    def base_url(self) -> str:
        return self.config.base_url.rstrip("/")

    def get_headers(self) -> dict[str, str]:
        """Build the header dict for a single outgoing request.

        A fresh ``X-Request-Id`` (UUID-4) is generated on every call so that
        each request is independently traceable in gateway logs.
        """
        headers: dict[str, str] = {
            "Content-Type": "application/json",
            "X-API-Key": self.config.api_key,
            "X-Tenant-Id": self.config.tenant_id,
            "X-SDK-Version": f"python/{__version__}",
            "X-Request-Id": uuid.uuid4().hex,
        }
        if self.config.user_id:
            headers["X-User-Id"] = self.config.user_id
        return headers
