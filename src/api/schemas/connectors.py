"""Typed API contracts for the connector catalog (admin-managed provider definitions).

The catalog maps onto the existing ``connector_configs`` columns; ``mode``
(live | ingest | both) is a product-level view of the same row.
"""

from __future__ import annotations

import ipaddress
from datetime import datetime
from typing import Any, Literal
from urllib.parse import urlsplit

from pydantic import BaseModel, ConfigDict, Field, field_validator

ConnectorMode = Literal["live", "ingest", "both"]


def _validate_oauth_endpoint(value: str, *, field: str) -> str:
    """Reject unsafe OAuth endpoints before they reach persistence checks."""

    if not value:
        return value
    try:
        parsed = urlsplit(value)
        port = parsed.port
    except ValueError as exc:
        raise ValueError(f"{field} must be a valid https URL") from exc
    if (
        parsed.scheme.lower() != "https"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.fragment
    ):
        raise ValueError(f"{field} must be an https URL without userinfo or fragment")
    if port is not None and not (1 <= port <= 65535):
        raise ValueError(f"{field} contains an invalid port")
    try:
        literal = ipaddress.ip_address(parsed.hostname)
    except ValueError:
        literal = None
    if literal is not None:
        if isinstance(literal, ipaddress.IPv6Address) and literal.ipv4_mapped:
            literal = literal.ipv4_mapped
        if not literal.is_global:
            raise ValueError(f"{field} must not target a private network address")
    return value


class ConnectorAuthConfig(BaseModel):
    """OAuth app credentials/endpoints the admin manages for a provider."""

    model_config = ConfigDict(extra="forbid")

    client_id: str = Field("", description="OAuth client ID")
    client_secret: str | None = Field(
        None, description="OAuth client secret — write-only, never echoed back"
    )
    auth_url: str = Field("", description="Authorization endpoint URL")
    token_url: str = Field("", description="Token endpoint URL")
    scopes: str = Field("", description="Space-separated default scopes")
    redirect_uri: str | None = Field(None, description="OAuth redirect URI override")

    @field_validator("auth_url", "token_url")
    @classmethod
    def validate_oauth_endpoint(cls, value: str, info) -> str:
        return _validate_oauth_endpoint(value, field=info.field_name)


class ConnectorAuthResponse(BaseModel):
    """Public OAuth metadata. The write-only client secret is not a field."""

    client_id: str = ""
    auth_url: str = ""
    token_url: str = ""
    scopes: str = ""
    redirect_uri: str | None = None


class ConnectorMcpToolInfo(BaseModel):
    """Informational MCP tool surfaced by a connector provider."""

    name: str = Field("", description="Stable tool name, e.g. confluence_read")
    description: str = Field("", description="Short human-readable description")


class ConnectorProviderBase(BaseModel):
    """Non-secret connector provider fields shared by write/read contracts."""

    model_config = ConfigDict(extra="forbid")

    provider: str = Field(..., min_length=1, max_length=64)
    display_name: str = Field(..., min_length=1, max_length=256)
    description: str | None = Field(None, max_length=4000)
    icon_url: str | None = Field(None, max_length=512)
    mode: ConnectorMode = "live"
    enabled: bool = True
    supports_sync: bool = False
    supports_search: bool = True
    mcp_tools: list[ConnectorMcpToolInfo] = Field(default_factory=list, max_length=64)
    extra_config: dict[str, Any] = Field(default_factory=dict)


class ConnectorProviderDefinition(ConnectorProviderBase):
    """Admin-managed connector provider definition (connector_configs row)."""

    auth: ConnectorAuthConfig | None = None


class ConnectorProviderCreate(ConnectorProviderDefinition):
    """Create payload — same shape as the definition (client_secret write-only)."""


class ConnectorProviderUpdate(BaseModel):
    """Update payload — every field optional; absent/empty secret keeps current."""

    model_config = ConfigDict(extra="forbid")

    display_name: str | None = Field(None, min_length=1, max_length=256)
    description: str | None = Field(None, max_length=4000)
    icon_url: str | None = Field(None, max_length=512)
    mode: ConnectorMode | None = None
    enabled: bool | None = None
    supports_sync: bool | None = None
    supports_search: bool | None = None
    auth: ConnectorAuthConfig | None = None
    mcp_tools: list[ConnectorMcpToolInfo] | None = Field(None, max_length=64)
    extra_config: dict[str, Any] | None = None


class ConnectorToggleRequest(BaseModel):
    """PATCH body for enabling/disabling a provider without a full update."""

    enabled: bool


class ConnectorProviderResponse(ConnectorProviderBase):
    """Read model with no client-secret field in its schema."""

    auth: ConnectorAuthResponse | None = None
    tenant_id: str = ""
    created_at: datetime | None = None
    updated_at: datetime | None = None
