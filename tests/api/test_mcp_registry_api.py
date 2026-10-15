from __future__ import annotations

import copy
import ipaddress
import uuid
from datetime import datetime, timezone
from typing import Any

from ai_gateway_core.persistence.repositories.mcp_repository import MCPNotFoundError
from fastapi import FastAPI
from fastapi.testclient import TestClient

from src.api.deps import AuthContext, get_auth_context, get_user_context
from src.api.v1.connectors import router as connectors_router
from src.api.v1.mcp import legacy_router, router
from src.core.auth.user_resolver import UserContext


def _now() -> datetime:
    return datetime.now(timezone.utc)


def _user(tenant_id: str = "tenant-a", user_id: str = "admin-a") -> UserContext:
    return UserContext(
        tenant_id=tenant_id,
        user_id=user_id,
        is_authenticated=True,
        roles=["admin"],
    )


class _Registry:
    def __init__(self) -> None:
        self.servers: dict[tuple[str, str], dict[str, Any]] = {}
        self.connections: dict[tuple[str, str], dict[str, Any]] = {}
        self.connector_principals: dict[tuple[str, str], dict[str, Any]] = {}
        self.grants: list[dict[str, Any]] = []

    async def create_server(self, **values: Any) -> dict[str, Any]:
        server_id = uuid.uuid4()
        timestamp = _now()
        row = {
            **values,
            "server_id": server_id,
            "transport": "streamable_http",
            "enabled": True,
            "health_status": "unknown",
            "circuit_state": "closed",
            "consecutive_failures": 0,
            "last_health_at": None,
            "last_error_code": None,
            "created_at": timestamp,
            "updated_at": timestamp,
        }
        row.pop("tenant_id")
        row.pop("user_id")
        self.servers[(values["tenant_id"], str(server_id))] = copy.deepcopy(row)
        return row

    async def list_servers(self, *, tenant_id: str) -> list[dict[str, Any]]:
        return [
            copy.deepcopy(row)
            for (tenant, _), row in self.servers.items()
            if tenant == tenant_id and not row.get("deleted")
        ]

    async def get_server(self, *, tenant_id: str, server_id: str) -> dict[str, Any]:
        row = self.servers.get((tenant_id, str(server_id)))
        if not row or row.get("deleted"):
            raise MCPNotFoundError("MCP_SERVER_NOT_FOUND")
        return copy.deepcopy(row)

    async def update_server(self, **values: Any) -> dict[str, Any]:
        row = await self.get_server(
            tenant_id=values["tenant_id"], server_id=values["server_id"]
        )
        row.update(values["changes"])
        row["updated_at"] = _now()
        self.servers[(values["tenant_id"], str(values["server_id"]))] = copy.deepcopy(row)
        return row

    async def delete_server(self, **values: Any) -> None:
        row = await self.get_server(
            tenant_id=values["tenant_id"], server_id=values["server_id"]
        )
        row["deleted"] = True
        row["enabled"] = False
        self.servers[(values["tenant_id"], str(values["server_id"]))] = row

    async def create_connection(self, **values: Any) -> dict[str, Any]:
        await self.get_server(
            tenant_id=values["tenant_id"], server_id=values["server_id"]
        )
        connection_id = uuid.uuid4()
        timestamp = _now()
        internal = {
            **values,
            "connection_id": connection_id,
            "enabled": True,
            "revoked_at": None,
            "created_at": timestamp,
            "updated_at": timestamp,
        }
        self.connections[(values["tenant_id"], str(connection_id))] = internal
        return self._redacted_connection(internal)

    @staticmethod
    def _redacted_connection(row: dict[str, Any]) -> dict[str, Any]:
        return {
            key: row.get(key)
            for key in (
                "connection_id",
                "server_id",
                "principal_type",
                "owner_user_id",
                "scopes",
                "audience",
                "expires_at",
                "revoked_at",
                "enabled",
                "created_at",
                "updated_at",
            )
        } | {"credential_configured": bool(row.get("secret_ref"))}

    async def list_connections(
        self, *, tenant_id: str, server_id: str
    ) -> list[dict[str, Any]]:
        return [
            self._redacted_connection(row)
            for (tenant, _), row in self.connections.items()
            if tenant == tenant_id and str(row["server_id"]) == str(server_id)
        ]

    async def revoke_connection(self, **values: Any) -> None:
        key = (values["tenant_id"], str(values["connection_id"]))
        row = self.connections.get(key)
        if not row:
            raise MCPNotFoundError("MCP_CONNECTION_NOT_FOUND")
        row["enabled"] = False
        row["revoked_at"] = _now()

    async def grant_channel(self, **values: Any) -> dict[str, Any]:
        self.grants.append(copy.deepcopy(values))
        return values

    async def list_tools(self, **_values: Any) -> list[dict[str, Any]]:
        return []

    async def create_connector_principal(self, **values: Any) -> dict[str, Any]:
        grant_id = uuid.uuid4()
        timestamp = _now()
        internal = {
            **values,
            "grant_id": grant_id,
            "enabled": True,
            "revoked_at": None,
            "created_at": timestamp,
            "updated_at": timestamp,
        }
        self.connector_principals[(values["tenant_id"], str(grant_id))] = internal
        return self._redacted_connector(internal)

    @staticmethod
    def _redacted_connector(row: dict[str, Any]) -> dict[str, Any]:
        return {
            key: copy.deepcopy(row.get(key))
            for key in (
                "grant_id",
                "provider",
                "principal_type",
                "owner_user_id",
                "scopes",
                "audience",
                "connection_metadata",
                "allowed_channels",
                "expires_at",
                "revoked_at",
                "enabled",
                "created_at",
                "updated_at",
            )
        } | {"credential_configured": bool(row.get("secret_ref"))}

    async def list_connector_principals(self, **values: Any) -> list[dict[str, Any]]:
        return [
            self._redacted_connector(row)
            for (tenant, _), row in self.connector_principals.items()
            if tenant == values["tenant_id"] and row["provider"] == values["provider"]
        ]

    async def revoke_connector_principal(self, **values: Any) -> None:
        key = (values["tenant_id"], str(values["grant_id"]))
        row = self.connector_principals.get(key)
        if not row:
            raise MCPNotFoundError("CONNECTOR_PRINCIPAL_NOT_FOUND")
        row["enabled"] = False
        row["revoked_at"] = _now()


class _Discovery:
    def __init__(self) -> None:
        self.calls: list[dict[str, Any]] = []

    async def discover(self, **values: Any) -> dict[str, Any]:
        self.calls.append(values)
        return {
            "server_id": values["server_id"],
            "changed": [],
            "unchanged": [],
            "removed": [],
            "breaking": False,
        }


def _client(
    repository: _Registry | None = None,
    *,
    user: UserContext | None = None,
    discovery: _Discovery | None = None,
    auth_roles: list[str] | None = None,
) -> tuple[TestClient, _Registry]:
    app = FastAPI()
    app.include_router(router)
    app.include_router(legacy_router)
    app.include_router(connectors_router)
    app.state.mcp_repository = repository or _Registry()

    def validate_test_destination(host: str, _port: int) -> tuple[bool, str]:
        if host == "private-resolve.example":
            return False, "resolves to disallowed address"
        try:
            address = ipaddress.ip_address(host)
        except ValueError:
            return True, "93.184.216.34"
        return address.is_global, str(address)

    app.state.mcp_destination_validator = validate_test_destination
    if discovery is not None:
        app.state.mcp_discovery_service = discovery
    actor = user or _user()
    auth = AuthContext(
        user_id=actor.user_id,
        tenant_id=actor.tenant_id,
        roles=auth_roles or ["admin"],
        permissions=["console:mcp:view", "console:mcp:edit"],
        is_authenticated=True,
    )
    app.dependency_overrides[get_user_context] = lambda: actor
    app.dependency_overrides[get_auth_context] = lambda: auth
    return TestClient(app), app.state.mcp_repository


def _create_server(client: TestClient, *, auth_method: str = "none") -> dict[str, Any]:
    payload: dict[str, Any] = {
        "name": "Research MCP",
        "base_url": "https://mcp.example",
        "auth_method": auth_method,
        "allowed_origins": ["https://studio.example"],
    }
    if auth_method == "oauth":
        payload.update(
            oauth_metadata_url="https://auth.example/.well-known/oauth",
            oauth_resource="https://mcp.example",
            oauth_audience="https://mcp.example",
        )
    response = client.post("/mcp/servers", json=payload)
    assert response.status_code == 201, response.text
    return response.json()["server"]


def test_server_crud_is_tenant_scoped_and_transport_is_closed() -> None:
    client, repository = _client()
    server = _create_server(client)

    listed = client.get("/mcp/servers")
    assert listed.status_code == 200
    assert listed.json()["servers"] == [server]
    assert server["transport"] == "streamable_http"

    other, _ = _client(repository, user=_user("tenant-b", "admin-b"))
    assert other.get("/mcp/servers").json() == {"servers": [], "total": 0}
    hidden = other.get(f"/mcp/servers/{server['server_id']}")
    assert hidden.status_code == 404

    updated = client.patch(
        f"/mcp/servers/{server['server_id']}", json={"name": "Renamed MCP"}
    )
    assert updated.status_code == 200
    assert updated.json()["server"]["name"] == "Renamed MCP"
    assert client.delete(f"/mcp/servers/{server['server_id']}").status_code == 200
    assert client.get(f"/mcp/servers/{server['server_id']}").status_code == 404

    stdio = client.post(
        "/mcp/servers",
        json={
            "name": "Unsafe",
            "base_url": "https://mcp.example",
            "transport": "stdio",
        },
    )
    assert stdio.status_code == 422
    assert client.post(
        "/mcp/servers",
        json={"name": "Unsafe", "base_url": "http://127.0.0.1:9000"},
    ).status_code == 422

    non_admin, _ = _client(repository, auth_roles=["developer"])
    denied = non_admin.post(
        "/mcp/servers",
        json={
            "name": "Denied",
            "base_url": "https://mcp.example",
            "allowed_origins": ["https://studio.example"],
        },
    )
    assert denied.status_code == 403
    assert denied.json()["detail"]["code"] == "MCP_ADMIN_REQUIRED"


def test_server_create_and_update_reject_private_destinations_before_save() -> None:
    client, repository = _client()
    blocked = (
        "https://127.0.0.1",
        "https://169.254.169.254",
        "https://private-resolve.example",
    )
    for index, target in enumerate(blocked):
        response = client.post(
            "/mcp/servers",
            json={
                "name": f"Blocked {index}",
                "base_url": target,
                "allowed_origins": ["https://studio.example"],
            },
        )
        assert response.status_code == 422
        assert target not in response.text
    assert repository.servers == {}

    server = _create_server(client)
    original_url = server["base_url"]
    for target in blocked:
        response = client.patch(
            f"/mcp/servers/{server['server_id']}",
            json={"base_url": target},
        )
        assert response.status_code == 422
        assert target not in response.text
        persisted = repository.servers[
            ("tenant-a", str(server["server_id"]))
        ]
        assert persisted["base_url"] == original_url

    oauth_metadata = client.post(
        "/mcp/servers",
        json={
            "name": "Blocked OAuth Metadata",
            "base_url": "https://mcp.example",
            "auth_method": "oauth",
            "oauth_metadata_url": "https://private-resolve.example/.well-known/oauth",
            "oauth_resource": "https://mcp.example",
            "oauth_audience": "https://mcp.example",
            "allowed_origins": ["https://studio.example"],
        },
    )
    assert oauth_metadata.status_code == 422
    assert "private-resolve.example" not in oauth_metadata.text


def test_connections_accept_only_secret_refs_and_never_return_them() -> None:
    client, _ = _client()
    server = _create_server(client, auth_method="oauth")
    secret_ref = "vault://tenants/tenant-a/mcp/research"
    created = client.post(
        f"/mcp/servers/{server['server_id']}/connections",
        json={
            "principal_type": "service_account",
            "secret_ref": secret_ref,
            "scopes": ["tools.read"],
            "audience": "https://mcp.example",
        },
    )
    assert created.status_code == 201, created.text
    body = created.json()
    assert body["connection"]["credential_configured"] is True
    assert "secret_ref" not in body["connection"]
    assert secret_ref not in created.text

    listed = client.get(f"/mcp/servers/{server['server_id']}/connections")
    assert listed.status_code == 200
    assert secret_ref not in listed.text
    assert "secret_ref" not in listed.json()["connections"][0]

    plaintext = client.post(
        f"/mcp/servers/{server['server_id']}/connections",
        json={
            "principal_type": "service_account",
            "secret_ref": "https://token.example/plaintext",
            "audience": "https://mcp.example",
        },
    )
    assert plaintext.status_code == 422
    assert "secret-value" not in plaintext.text
    extra_secret = client.post(
        f"/mcp/servers/{server['server_id']}/connections",
        json={
            "principal_type": "service_account",
            "access_token": "synthetic-token",
        },
    )
    assert extra_secret.status_code == 422
    assert "synthetic-token" not in extra_secret.text


def test_discovery_requires_an_explicit_connection_and_preserves_request_id() -> None:
    discovery = _Discovery()
    client, _ = _client(discovery=discovery)
    server = _create_server(client)
    connection_id = str(uuid.uuid4())

    response = client.post(
        f"/mcp/servers/{server['server_id']}/discover",
        json={
            "connection_id": connection_id,
            "principal_type": "service_account",
        },
    )
    assert response.status_code == 200, response.text
    assert response.json()["request_id"]
    assert response.json()["audit_ref"]
    assert str(discovery.calls[0]["connection_id"]) == connection_id

    legacy = client.post(f"/assistant/mcp/servers/{server['server_id']}/refresh")
    assert legacy.status_code == 422
    assert legacy.json()["detail"]["code"] == "MCP_CONNECTION_SELECTION_REQUIRED"


def test_oauth_shape_and_openapi_response_contract_are_redacted() -> None:
    client, _ = _client()
    incomplete = client.post(
        "/mcp/servers",
        json={
            "name": "Incomplete OAuth",
            "base_url": "https://mcp.example",
            "auth_method": "oauth",
        },
    )
    assert incomplete.status_code == 422

    schemas = client.get("/openapi.json").json()["components"]["schemas"]
    connection_response = schemas["MCPConnectionResponse"]["properties"]
    server_response = schemas["MCPServerResponse"]["properties"]
    discovery_response = schemas["MCPDiscoveryResponse"]["properties"]
    assert "secret_ref" not in connection_response
    assert "audit_ref" in discovery_response
    assert "access_token" not in connection_response
    assert "refresh_token" not in connection_response
    assert "resolved_ip" not in server_response
    assert "stack_trace" not in server_response


def test_connector_principal_api_uses_the_same_redacted_secret_ref_contract() -> None:
    client, _ = _client()
    secret_ref = "vault://tenant-a/connectors/confluence/service"
    created = client.post(
        "/connectors/confluence/principals",
        json={
            "principal_type": "service_account",
            "secret_ref": secret_ref,
            "scopes": ["read:confluence-content.all"],
            "connection_metadata": {
                "domain": "tenant-a.atlassian.net",
                "email": "service@example.com",
            },
            "allowed_channels": ["preview", "embed"],
        },
    )
    assert created.status_code == 201, created.text
    principal = created.json()["principal"]
    assert principal["credential_configured"] is True
    assert "secret_ref" not in principal
    assert secret_ref not in created.text
    assert client.get("/connectors/confluence/principals").json()["total"] == 1
    assert client.delete(
        f"/connectors/confluence/principals/{principal['grant_id']}"
    ).status_code == 200

    invalid = client.post(
        "/connectors/confluence/principals",
        json={
            "principal_type": "service_account",
            "secret_ref": "https://attacker.example/synthetic-connector-token",
        },
    )
    assert invalid.status_code == 422
    assert "synthetic-connector-token" not in invalid.text
