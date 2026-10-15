"""Tenant-scoped Agent Studio MCP registry and compatibility routes."""

from __future__ import annotations

import asyncio
import json
import uuid
from typing import Any, Literal

from ai_gateway_core.persistence.repositories.mcp_repository import (
    DatabaseMCPRepository,
    MCPAuthorizationError,
    MCPConflictError,
    MCPNotFoundError,
    MCPRepositoryError,
    MCPValidationError,
)
from ai_gateway_core.security import is_safe_destination
from fastapi import APIRouter, Depends, HTTPException, Query, Request

from ...core.auth.permissions import Capability
from ...core.auth.user_resolver import UserContext
from ..deps import AuthContext, get_auth_context, get_user_context, require_gateway_capability
from ..redacted_validation_route import RedactedValidationRoute
from ..schemas.mcp import (
    MCPChannelGrantRequest,
    MCPConnectionCreate,
    MCPConnectionListResponse,
    MCPConnectionMutationResponse,
    MCPConnectionResponse,
    MCPDiscoveryRequest,
    MCPDiscoveryResponse,
    MCPMutationResponse,
    MCPServerCreate,
    MCPServerListResponse,
    MCPServerMutationResponse,
    MCPServerResponse,
    MCPServerUpdate,
    MCPToolListResponse,
)

router = APIRouter(
    prefix="/mcp",
    tags=["Agent Studio MCP"],
    route_class=RedactedValidationRoute,
)
legacy_router = APIRouter(prefix="/assistant/mcp", tags=["mcp-compat"])


def _request_id(request: Request) -> str:
    value = str(
        getattr(request.state, "request_id", "")
        or getattr(request.state, "trace_id", "")
        or uuid.uuid4()
    )
    request.state.request_id = value
    return value


def _raise_mcp_error(
    request: Request,
    status_code: int,
    code: str,
    message: str,
) -> None:
    raise HTTPException(
        status_code=status_code,
        detail={"code": code, "message": message, "request_id": _request_id(request)},
    )


def _require_tenant(request: Request, user: UserContext) -> None:
    if not user.is_authenticated or not user.user_id:
        _raise_mcp_error(request, 401, "AUTHENTICATION_REQUIRED", "Authentication required")
    if not user.tenant_id or user.tenant_id == "public":
        _raise_mcp_error(request, 403, "TENANT_REQUIRED", "Tenant identity required")


def _get_repository(request: Request) -> Any:
    if getattr(request.app.state, "agent_studio_mcp_enabled", True) is False:
        _raise_mcp_error(request, 503, "MCP_DISABLED", "MCP registry is disabled")
    repository = getattr(request.app.state, "mcp_repository", None)
    if repository is not None:
        return repository
    database = getattr(request.app.state, "database", None)
    if database is None:
        _raise_mcp_error(
            request,
            503,
            "MCP_STORAGE_UNAVAILABLE",
            "MCP registry storage unavailable",
        )
    repository = DatabaseMCPRepository(database)
    request.app.state.mcp_repository = repository
    return repository


def _map_repository_error(request: Request, exc: Exception) -> None:
    from ...services.agent_runtime.mcp_gateway_broker import MCPGatewayBrokerError

    if isinstance(exc, MCPGatewayBrokerError):
        _raise_mcp_error(request, 502, exc.code, "MCP server unavailable")
    if isinstance(exc, MCPNotFoundError):
        _raise_mcp_error(request, 404, str(exc), "MCP resource not found")
    if isinstance(exc, MCPConflictError):
        _raise_mcp_error(request, 409, str(exc), "MCP resource conflicts with existing state")
    if isinstance(exc, MCPValidationError):
        _raise_mcp_error(request, 422, exc.code, "MCP configuration is invalid")
    if isinstance(exc, MCPAuthorizationError):
        _raise_mcp_error(request, 403, exc.code, "MCP operation is not authorized")
    if isinstance(exc, MCPRepositoryError):
        _raise_mcp_error(
            request,
            503,
            "MCP_STORAGE_UNAVAILABLE",
            "MCP registry storage unavailable",
        )
    if getattr(exc, "sqlstate", None) == "23505":
        _raise_mcp_error(request, 409, "MCP_RESOURCE_CONFLICT", "MCP resource conflict")
    raise exc


async def _audit_mutation(
    request: Request,
    user: UserContext,
    *,
    action: str,
    resource_type: str,
    resource_id: str | uuid.UUID,
    summary: dict[str, Any] | None = None,
) -> str:
    request_id = _request_id(request)
    database = getattr(request.app.state, "database", None)
    if database is None:
        return f"request:{request_id}"
    safe_summary = {
        key: value
        for key, value in (summary or {}).items()
        if key not in {"secret_ref", "token", "authorization", "credential"}
    }
    try:
        row = await database.fetchrow(
            """
            INSERT INTO audit_logs (
                event_type, user_id, tenant_id, resource_type, resource_id,
                action, request_summary, status
            ) VALUES ('mcp_registry', $1, $2, $3, $4, $5, $6::jsonb, 'success')
            RETURNING id
            """,
            user.user_id,
            user.tenant_id,
            resource_type,
            str(resource_id),
            action,
            json.dumps(safe_summary, sort_keys=True, default=str),
        )
        if row and row.get("id") is not None:
            return f"audit:{row['id']}"
    except Exception:
        # The mutation already succeeded. A stable request correlation remains
        # available without leaking the database failure or request content.
        return f"request:{request_id}"
    return f"request:{request_id}"


def _authorize_read(request: Request, auth: AuthContext, user: UserContext) -> None:
    _require_tenant(request, user)
    require_gateway_capability(request, auth, Capability.GATEWAY_MCP_READ)


def _authorize_write(request: Request, auth: AuthContext, user: UserContext) -> None:
    _require_tenant(request, user)
    require_gateway_capability(request, auth, Capability.GATEWAY_MCP_WRITE)
    roles = {str(role).lower() for role in (auth.roles or [])}
    if not roles.intersection({"admin", "tenant_admin", "superadmin", "super_admin"}):
        _raise_mcp_error(
            request,
            403,
            "MCP_ADMIN_REQUIRED",
            "Tenant administrator access is required",
        )


async def _validate_persisted_destination(request: Request, url: str) -> None:
    """Resolve a tenant MCP network target before it can be persisted.

    Literal private addresses are rejected by the closed request schema. This
    second boundary resolves hostnames and rejects any multi-record destination
    containing a non-global address. Runtime repeats the policy and pins the
    validated IP, so a later DNS change still fails closed before connection.
    """

    from urllib.parse import urlsplit

    try:
        parsed = urlsplit(url)
        hostname = parsed.hostname or ""
        port = parsed.port or 443
    except ValueError:
        _raise_mcp_error(request, 422, "MCP_URL_INVALID", "MCP destination is invalid")
        return

    validator = getattr(
        request.app.state,
        "mcp_destination_validator",
        is_safe_destination,
    )
    try:
        allowed, detail = await asyncio.to_thread(validator, hostname, port)
    except Exception:
        _raise_mcp_error(
            request,
            422,
            "MCP_DNS_UNAVAILABLE",
            "MCP destination cannot be resolved",
        )
        return
    if allowed:
        return
    code = "MCP_DNS_UNAVAILABLE" if detail and "DNS" in str(detail) else "MCP_SSRF_BLOCKED"
    message = (
        "MCP destination cannot be resolved"
        if code == "MCP_DNS_UNAVAILABLE"
        else "MCP destination is not permitted"
    )
    _raise_mcp_error(request, 422, code, message)


@router.post("/servers", response_model=MCPServerMutationResponse, status_code=201)
async def create_mcp_server(
    payload: MCPServerCreate,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> MCPServerMutationResponse:
    _authorize_write(request, auth, user)
    await _validate_persisted_destination(request, payload.base_url)
    if payload.oauth_metadata_url:
        await _validate_persisted_destination(request, payload.oauth_metadata_url)
    try:
        server = await _get_repository(request).create_server(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            **payload.model_dump(exclude={"transport"}),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    audit_ref = await _audit_mutation(
        request,
        user,
        action="create",
        resource_type="mcp_server",
        resource_id=str(server["server_id"]),
        summary={"name": payload.name, "auth_method": payload.auth_method},
    )
    return MCPServerMutationResponse(
        server=MCPServerResponse.model_validate(server),
        request_id=_request_id(request),
        audit_ref=audit_ref,
    )


@router.get("/servers", response_model=MCPServerListResponse)
async def list_mcp_servers(
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> MCPServerListResponse:
    _authorize_read(request, auth, user)
    try:
        servers = await _get_repository(request).list_servers(tenant_id=user.tenant_id)
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    rendered = [MCPServerResponse.model_validate(server) for server in servers]
    return MCPServerListResponse(servers=rendered, total=len(rendered))


@router.get("/servers/{server_id}", response_model=MCPServerResponse)
async def get_mcp_server(
    server_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> MCPServerResponse:
    _authorize_read(request, auth, user)
    try:
        server = await _get_repository(request).get_server(
            tenant_id=user.tenant_id,
            server_id=server_id,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    return MCPServerResponse.model_validate(server)


@router.patch("/servers/{server_id}", response_model=MCPServerMutationResponse)
async def update_mcp_server(
    server_id: uuid.UUID,
    payload: MCPServerUpdate,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> MCPServerMutationResponse:
    _authorize_write(request, auth, user)
    if payload.base_url is not None:
        await _validate_persisted_destination(request, payload.base_url)
    if payload.oauth_metadata_url is not None:
        await _validate_persisted_destination(request, payload.oauth_metadata_url)
    try:
        server = await _get_repository(request).update_server(
            tenant_id=user.tenant_id,
            server_id=server_id,
            user_id=user.user_id,
            changes=payload.model_dump(exclude_unset=True),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    audit_ref = await _audit_mutation(
        request,
        user,
        action="update",
        resource_type="mcp_server",
        resource_id=server_id,
        summary={"fields": sorted(payload.model_fields_set)},
    )
    return MCPServerMutationResponse(
        server=MCPServerResponse.model_validate(server),
        request_id=_request_id(request),
        audit_ref=audit_ref,
    )


@router.delete("/servers/{server_id}", response_model=MCPMutationResponse)
async def delete_mcp_server(
    server_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> MCPMutationResponse:
    _authorize_write(request, auth, user)
    try:
        await _get_repository(request).delete_server(
            tenant_id=user.tenant_id,
            server_id=server_id,
            user_id=user.user_id,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    audit_ref = await _audit_mutation(
        request,
        user,
        action="delete",
        resource_type="mcp_server",
        resource_id=server_id,
    )
    return MCPMutationResponse(
        status="deleted",
        request_id=_request_id(request),
        audit_ref=audit_ref,
    )


@router.post(
    "/servers/{server_id}/connections",
    response_model=MCPConnectionMutationResponse,
    status_code=201,
)
async def create_mcp_connection(
    server_id: uuid.UUID,
    payload: MCPConnectionCreate,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> MCPConnectionMutationResponse:
    _authorize_write(request, auth, user)
    try:
        connection = await _get_repository(request).create_connection(
            tenant_id=user.tenant_id,
            server_id=server_id,
            user_id=user.user_id,
            **payload.model_dump(),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    audit_ref = await _audit_mutation(
        request,
        user,
        action="create",
        resource_type="mcp_connection",
        resource_id=str(connection["connection_id"]),
        summary={
            "server_id": server_id,
            "principal_type": payload.principal_type,
            "owner_user_id": payload.owner_user_id,
        },
    )
    return MCPConnectionMutationResponse(
        connection=MCPConnectionResponse.model_validate(connection),
        request_id=_request_id(request),
        audit_ref=audit_ref,
    )


@router.get(
    "/servers/{server_id}/connections",
    response_model=MCPConnectionListResponse,
)
async def list_mcp_connections(
    server_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> MCPConnectionListResponse:
    _authorize_read(request, auth, user)
    try:
        connections = await _get_repository(request).list_connections(
            tenant_id=user.tenant_id,
            server_id=server_id,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    rendered = [MCPConnectionResponse.model_validate(item) for item in connections]
    return MCPConnectionListResponse(connections=rendered, total=len(rendered))


@router.delete("/connections/{connection_id}", response_model=MCPMutationResponse)
async def revoke_mcp_connection(
    connection_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> MCPMutationResponse:
    _authorize_write(request, auth, user)
    try:
        await _get_repository(request).revoke_connection(
            tenant_id=user.tenant_id,
            connection_id=connection_id,
            user_id=user.user_id,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    audit_ref = await _audit_mutation(
        request,
        user,
        action="revoke",
        resource_type="mcp_connection",
        resource_id=connection_id,
    )
    return MCPMutationResponse(
        status="revoked",
        request_id=_request_id(request),
        audit_ref=audit_ref,
    )


@router.put(
    "/connections/{connection_id}/channel-grants",
    response_model=MCPMutationResponse,
)
async def grant_mcp_channel(
    connection_id: uuid.UUID,
    payload: MCPChannelGrantRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> MCPMutationResponse:
    _authorize_write(request, auth, user)
    try:
        await _get_repository(request).grant_channel(
            tenant_id=user.tenant_id,
            connection_id=connection_id,
            tool_id=payload.tool_id,
            channel=payload.channel,
            read_only_only=payload.read_only_only,
            user_id=user.user_id,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    audit_ref = await _audit_mutation(
        request,
        user,
        action="grant_channel",
        resource_type="mcp_connection",
        resource_id=connection_id,
        summary={"tool_id": payload.tool_id, "channel": payload.channel},
    )
    return MCPMutationResponse(
        status="granted",
        request_id=_request_id(request),
        audit_ref=audit_ref,
    )


@router.get("/servers/{server_id}/tools", response_model=MCPToolListResponse)
async def list_mcp_tools(
    server_id: uuid.UUID,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> MCPToolListResponse:
    _authorize_read(request, auth, user)
    try:
        tools = await _get_repository(request).list_tools(
            tenant_id=user.tenant_id,
            server_id=server_id,
        )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    return MCPToolListResponse(tools=tools, total=len(tools))


@router.post("/servers/{server_id}/discover", response_model=MCPDiscoveryResponse)
async def discover_mcp_server(
    server_id: uuid.UUID,
    payload: MCPDiscoveryRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
):
    _authorize_write(request, auth, user)
    service = getattr(request.app.state, "mcp_gateway_broker", None)
    if service is None:
        service = getattr(request.app.state, "mcp_discovery_service", None)
    if service is None or not callable(getattr(service, "discover", None)):
        _raise_mcp_error(
            request,
            503,
            "MCP_DISCOVERY_UNAVAILABLE",
            "MCP discovery service unavailable",
        )
    try:
        result = await service.discover(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            server_id=server_id,
            connection_id=payload.connection_id,
            principal_type=payload.principal_type,
            repository=_get_repository(request),
        )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    audit_ref = await _audit_mutation(
        request,
        user,
        action="discover",
        resource_type="mcp_server",
        resource_id=server_id,
        summary={
            "changed": len(result.get("changed") or []),
            "removed": len(result.get("removed") or []),
            "breaking": bool(result.get("breaking")),
        },
    )
    return MCPDiscoveryResponse.model_validate(
        {
            **result,
            "request_id": _request_id(request),
            "audit_ref": audit_ref,
        }
    )


# Compatibility surface for the existing Assistant management callers. It now
# reads the tenant registry instead of the removed Gateway-global MCPManager.
@legacy_router.get("/servers")
async def list_legacy_mcp_servers(
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
):
    result = await list_mcp_servers(request=request, user=user, auth=auth)
    return {"servers": [item.model_dump(mode="json") for item in result.servers]}


@legacy_router.get("/tools")
async def list_legacy_mcp_tools(
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
):
    _authorize_read(request, auth, user)
    repository = _get_repository(request)
    try:
        servers = await repository.list_servers(tenant_id=user.tenant_id)
        tools: list[dict[str, Any]] = []
        for server in servers:
            tools.extend(
                await repository.list_tools(
                    tenant_id=user.tenant_id,
                    server_id=str(server["server_id"]),
                )
            )
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    return {"tools": tools, "total": len(tools)}


@legacy_router.post("/servers/{server_name}/refresh")
async def refresh_legacy_mcp_server(
    server_name: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
    connection_id: uuid.UUID | None = Query(default=None),
    principal_type: Literal["service_account", "user_delegated"] = Query(
        default="service_account"
    ),
):
    _authorize_write(request, auth, user)
    repository = _get_repository(request)
    try:
        servers = await repository.list_servers(tenant_id=user.tenant_id)
    except Exception as exc:
        _map_repository_error(request, exc)
        raise
    match = next(
        (
            server
            for server in servers
            if str(server["server_id"]) == server_name or server["name"] == server_name
        ),
        None,
    )
    if match is None:
        _raise_mcp_error(request, 404, "MCP_SERVER_NOT_FOUND", "MCP server not found")
    if connection_id is None:
        _raise_mcp_error(
            request,
            422,
            "MCP_CONNECTION_SELECTION_REQUIRED",
            "An explicit MCP credential connection is required",
        )
    return await discover_mcp_server(
        uuid.UUID(str(match["server_id"])),
        MCPDiscoveryRequest(
            connection_id=connection_id,
            principal_type=principal_type,
        ),
        request=request,
        user=user,
        auth=auth,
    )


__all__ = ["legacy_router", "router"]
