"""Closed public schemas for the tenant MCP registry."""

from __future__ import annotations

import ipaddress
import re
from datetime import datetime
from typing import Literal
from urllib.parse import urlsplit
from uuid import UUID

from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator

MCPAuthMethod = Literal["none", "bearer", "oauth"]
MCPPrincipalType = Literal["service_account", "user_delegated"]
MCPChannel = Literal["preview", "hosted_private", "hosted_public", "embed", "api"]
_SECRET_REF_RE = re.compile(r"^[a-z][a-z0-9+.-]*://[^\s]+$")


def _validate_https_url(value: str | None, *, field: str) -> str | None:
    if value is None:
        return None
    parsed = urlsplit(value)
    if (
        parsed.scheme.lower() != "https"
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.fragment
    ):
        raise ValueError(f"{field} must be an https URL without userinfo or fragment")
    try:
        literal = ipaddress.ip_address(parsed.hostname)
    except ValueError:
        literal = None
    if literal is not None:
        if isinstance(literal, ipaddress.IPv6Address) and literal.ipv4_mapped:
            literal = literal.ipv4_mapped
        if not literal.is_global:
            raise ValueError(f"{field} must not target a private network address")
    return value.rstrip("/")


def _normalize_origins(values: list[str]) -> list[str]:
    result: list[str] = []
    for value in values:
        parsed = urlsplit(value)
        if (
            parsed.scheme.lower() != "https"
            or not parsed.hostname
            or parsed.username is not None
            or parsed.password is not None
            or parsed.path not in {"", "/"}
            or parsed.query
            or parsed.fragment
        ):
            raise ValueError("allowed_origins must contain exact https origins")
        origin = f"https://{parsed.netloc}"
        if origin not in result:
            result.append(origin)
    return result


def _validate_secret_ref_value(value: str | None) -> str | None:
    if value is None:
        return None
    if (
        not _SECRET_REF_RE.fullmatch(value)
        or value.lower().startswith(("http://", "https://"))
    ):
        raise ValueError("secret_ref must be an opaque Secret Store reference")
    return value


class MCPServerCreate(BaseModel):
    model_config = ConfigDict(extra="forbid")

    name: str = Field(min_length=1, max_length=128, pattern=r"^[A-Za-z0-9][A-Za-z0-9 _.-]*$")
    description: str = Field(default="", max_length=2000)
    base_url: str = Field(min_length=1, max_length=2048)
    transport: Literal["streamable_http"] = "streamable_http"
    auth_method: MCPAuthMethod = "none"
    oauth_metadata_url: str | None = Field(default=None, max_length=2048)
    oauth_resource: str | None = Field(default=None, max_length=2048)
    oauth_audience: str | None = Field(default=None, max_length=2048)
    allowed_origins: list[str] = Field(min_length=1, max_length=64)
    timeout_ms: int = Field(default=30000, ge=100, le=120000)
    max_concurrency: int = Field(default=5, ge=1, le=32)
    response_limit_bytes: int = Field(default=1048576, ge=1024, le=8388608)

    @field_validator("base_url")
    @classmethod
    def validate_base_url(cls, value: str) -> str:
        return str(_validate_https_url(value, field="base_url"))

    @field_validator("oauth_metadata_url")
    @classmethod
    def validate_metadata_url(cls, value: str | None) -> str | None:
        return _validate_https_url(value, field="oauth_metadata_url")

    @field_validator("oauth_resource")
    @classmethod
    def validate_oauth_resource(cls, value: str | None) -> str | None:
        return _validate_https_url(value, field="oauth_resource")

    @field_validator("allowed_origins")
    @classmethod
    def validate_origins(cls, values: list[str]) -> list[str]:
        return _normalize_origins(values)

    @model_validator(mode="after")
    def validate_oauth(self) -> MCPServerCreate:
        oauth_values = (
            self.oauth_metadata_url,
            self.oauth_resource,
            self.oauth_audience,
        )
        if self.auth_method == "oauth" and not all(oauth_values):
            raise ValueError(
                "oauth_metadata_url, oauth_resource and oauth_audience are required for OAuth"
            )
        if self.auth_method != "oauth" and any(oauth_values):
            raise ValueError("OAuth metadata is only valid when auth_method is oauth")
        return self


class MCPServerUpdate(BaseModel):
    model_config = ConfigDict(extra="forbid")

    name: str | None = Field(
        default=None,
        min_length=1,
        max_length=128,
        pattern=r"^[A-Za-z0-9][A-Za-z0-9 _.-]*$",
    )
    description: str | None = Field(default=None, max_length=2000)
    base_url: str | None = Field(default=None, min_length=1, max_length=2048)
    auth_method: MCPAuthMethod | None = None
    oauth_metadata_url: str | None = Field(default=None, max_length=2048)
    oauth_resource: str | None = Field(default=None, max_length=2048)
    oauth_audience: str | None = Field(default=None, max_length=2048)
    allowed_origins: list[str] | None = Field(
        default=None, min_length=1, max_length=64
    )
    timeout_ms: int | None = Field(default=None, ge=100, le=120000)
    max_concurrency: int | None = Field(default=None, ge=1, le=32)
    response_limit_bytes: int | None = Field(default=None, ge=1024, le=8388608)
    enabled: bool | None = None

    @field_validator("base_url")
    @classmethod
    def validate_base_url(cls, value: str | None) -> str | None:
        return _validate_https_url(value, field="base_url")

    @field_validator("oauth_metadata_url")
    @classmethod
    def validate_metadata_url(cls, value: str | None) -> str | None:
        return _validate_https_url(value, field="oauth_metadata_url")

    @field_validator("oauth_resource")
    @classmethod
    def validate_oauth_resource(cls, value: str | None) -> str | None:
        return _validate_https_url(value, field="oauth_resource")

    @field_validator("allowed_origins")
    @classmethod
    def validate_origins(cls, values: list[str] | None) -> list[str] | None:
        if values is None:
            return None
        return _normalize_origins(values)


class MCPServerResponse(BaseModel):
    model_config = ConfigDict(extra="forbid")

    server_id: UUID
    name: str
    description: str
    base_url: str
    transport: Literal["streamable_http"]
    auth_method: MCPAuthMethod
    oauth_metadata_url: str | None = None
    oauth_resource: str | None = None
    oauth_audience: str | None = None
    allowed_origins: list[str] = Field(default_factory=list)
    timeout_ms: int
    max_concurrency: int
    response_limit_bytes: int
    enabled: bool
    health_status: str
    circuit_state: str
    consecutive_failures: int
    last_health_at: datetime | None = None
    last_error_code: str | None = None
    created_at: datetime
    updated_at: datetime


class MCPServerListResponse(BaseModel):
    servers: list[MCPServerResponse]
    total: int


class MCPServerMutationResponse(BaseModel):
    server: MCPServerResponse
    request_id: str
    audit_ref: str


class MCPConnectionCreate(BaseModel):
    model_config = ConfigDict(extra="forbid")

    principal_type: MCPPrincipalType
    owner_user_id: str | None = Field(default=None, min_length=1, max_length=255)
    secret_ref: str | None = Field(default=None, min_length=1, max_length=2048)
    scopes: list[str] = Field(default_factory=list, max_length=128)
    audience: str | None = Field(default=None, max_length=2048)
    expires_at: datetime | None = None

    @field_validator("secret_ref")
    @classmethod
    def validate_secret_ref(cls, value: str | None) -> str | None:
        return _validate_secret_ref_value(value)

    @field_validator("scopes")
    @classmethod
    def normalize_scopes(cls, values: list[str]) -> list[str]:
        return sorted({value.strip() for value in values if value.strip()})

    @model_validator(mode="after")
    def validate_owner(self) -> MCPConnectionCreate:
        if self.principal_type == "service_account" and self.owner_user_id is not None:
            raise ValueError("service_account connections cannot have owner_user_id")
        if self.principal_type == "user_delegated" and not self.owner_user_id:
            raise ValueError("user_delegated connections require owner_user_id")
        return self


class MCPConnectionResponse(BaseModel):
    model_config = ConfigDict(extra="forbid")

    connection_id: UUID
    server_id: UUID
    principal_type: MCPPrincipalType
    owner_user_id: str | None = None
    scopes: list[str] = Field(default_factory=list)
    audience: str | None = None
    expires_at: datetime | None = None
    revoked_at: datetime | None = None
    enabled: bool
    credential_configured: bool
    created_at: datetime
    updated_at: datetime


class MCPConnectionListResponse(BaseModel):
    connections: list[MCPConnectionResponse]
    total: int


class MCPConnectionMutationResponse(BaseModel):
    connection: MCPConnectionResponse
    request_id: str
    audit_ref: str


class ConnectorPrincipalCreate(BaseModel):
    model_config = ConfigDict(extra="forbid")

    principal_type: MCPPrincipalType
    owner_user_id: str | None = Field(default=None, min_length=1, max_length=255)
    secret_ref: str = Field(min_length=1, max_length=2048)
    scopes: list[str] = Field(default_factory=list, max_length=128)
    audience: str | None = Field(default=None, max_length=2048)
    connection_metadata: dict = Field(default_factory=dict)
    allowed_channels: list[MCPChannel] = Field(
        default_factory=lambda: ["preview", "hosted_private"]
    )
    expires_at: datetime | None = None

    @field_validator("secret_ref")
    @classmethod
    def validate_secret_ref(cls, value: str) -> str:
        rendered = _validate_secret_ref_value(value)
        if rendered is None:
            raise ValueError("secret_ref is required")
        return rendered

    @model_validator(mode="after")
    def validate_principal(self) -> ConnectorPrincipalCreate:
        if self.principal_type == "service_account" and self.owner_user_id is not None:
            raise ValueError("service_account principals cannot have owner_user_id")
        if self.principal_type == "user_delegated" and not self.owner_user_id:
            raise ValueError("user_delegated principals require owner_user_id")
        if self.principal_type == "user_delegated" and set(
            self.allowed_channels
        ).intersection({"hosted_public", "embed"}):
            raise ValueError("delegated principals cannot be enabled for public channels")
        if set(self.connection_metadata) - {"domain", "email", "site_name", "cloud_id"}:
            raise ValueError("connection_metadata contains an unsupported field")
        return self


class ConnectorPrincipalResponse(BaseModel):
    model_config = ConfigDict(extra="forbid")

    grant_id: UUID
    provider: Literal["confluence"]
    principal_type: MCPPrincipalType
    owner_user_id: str | None = None
    scopes: list[str] = Field(default_factory=list)
    audience: str | None = None
    connection_metadata: dict = Field(default_factory=dict)
    allowed_channels: list[MCPChannel] = Field(default_factory=list)
    expires_at: datetime | None = None
    revoked_at: datetime | None = None
    enabled: bool
    credential_configured: bool
    created_at: datetime
    updated_at: datetime


class ConnectorPrincipalMutationResponse(BaseModel):
    principal: ConnectorPrincipalResponse
    request_id: str
    audit_ref: str


class ConnectorPrincipalListResponse(BaseModel):
    principals: list[ConnectorPrincipalResponse]
    total: int


class MCPChannelGrantRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    tool_id: UUID
    channel: MCPChannel
    read_only_only: Literal[True] = True


class MCPToolResponse(BaseModel):
    model_config = ConfigDict(extra="forbid")

    tool_id: UUID
    server_id: UUID
    upstream_name: str
    runtime_name: str
    enabled: bool
    snapshot_id: UUID
    schema_version: int
    schema_hash: str
    description: str
    input_schema: dict
    risk_level: Literal["low", "medium", "high", "critical"]
    read_only: bool
    discovered_at: datetime


class MCPToolListResponse(BaseModel):
    tools: list[MCPToolResponse]
    total: int


class MCPDiscoveryRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    connection_id: UUID
    principal_type: MCPPrincipalType


class MCPDiscoveryResponse(BaseModel):
    server_id: UUID
    changed: list[dict]
    unchanged: list[dict]
    removed: list[str]
    breaking: bool
    request_id: str
    audit_ref: str


class MCPMutationResponse(BaseModel):
    status: str
    request_id: str
    audit_ref: str


class MCPErrorResponse(BaseModel):
    detail: dict


__all__ = [
    "ConnectorPrincipalCreate",
    "ConnectorPrincipalListResponse",
    "ConnectorPrincipalMutationResponse",
    "ConnectorPrincipalResponse",
    "MCPChannelGrantRequest",
    "MCPConnectionCreate",
    "MCPConnectionListResponse",
    "MCPConnectionMutationResponse",
    "MCPConnectionResponse",
    "MCPDiscoveryResponse",
    "MCPDiscoveryRequest",
    "MCPErrorResponse",
    "MCPMutationResponse",
    "MCPServerCreate",
    "MCPServerListResponse",
    "MCPServerMutationResponse",
    "MCPServerResponse",
    "MCPServerUpdate",
    "MCPToolListResponse",
]
