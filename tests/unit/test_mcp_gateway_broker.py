from __future__ import annotations

from types import SimpleNamespace

import pytest
from ai_gateway_core.persistence.repositories.mcp_repository import MCPAuthorizationError

from src.services.agent_runtime import mcp_gateway_broker as broker_module
from src.services.agent_runtime.mcp_gateway_broker import MCPGatewayBroker


class _Secrets:
    async def resolve(self, reference: str) -> str:
        assert reference == "vault://mcp"
        return "ephemeral-token"


class _Repository:
    def __init__(self, *, read_only: bool = True, protocol: str = "2025-11-25") -> None:
        self.read_only = read_only
        self.protocol = protocol
        self.discovered: list[dict] = []

    async def resolve_discovery_connection(self, **kwargs):
        assert kwargs["tenant_id"] == "tenant-a"
        return self._item()

    async def record_discovery(self, **kwargs):
        self.discovered.append(kwargs)
        return {"changed": [], "removed": [], "breaking": False}

    async def authorize_mcp_tool(self, **kwargs):
        return self._item() | {
            "runtime_name": kwargs["runtime_name"],
            "upstream_name": "upstream_search",
            "schema_hash": kwargs["schema_hash"],
            "read_only": self.read_only,
        }

    def _item(self) -> dict:
        return {
            "tenant_id": "tenant-a",
            "server_id": "server-a",
            "connection_id": "connection-a",
            "base_url": "https://mcp.example.test",
            "endpoint_path": "/mcp",
            "auth_method": "oauth",
            "secret_ref": "vault://mcp",
            "credential_updated_at": "revision-a",
            "response_limit_bytes": 1024,
            "timeout_ms": 3000,
            "protocol": self.protocol,
        }


class _FakeClient:
    instances: list[_FakeClient] = []

    def __init__(self, config) -> None:
        self.config = config
        self.initialize_calls = 0
        self.__class__.instances.append(self)

    async def initialize(self):
        self.initialize_calls += 1
        return {"protocolVersion": "2025-11-25"}

    async def list_tools(self):
        return [
            SimpleNamespace(
                upstream_name="upstream_search",
                description="search",
                input_schema={"type": "object"},
            )
        ]

    async def call_tool(self, tool_name, arguments, *, invocation_policy):
        assert tool_name == "upstream_search"
        assert invocation_policy.max_attempts == 1
        return SimpleNamespace(content=[{"type": "text", "text": arguments["q"]}], is_error=False)

    async def close(self):
        return None


@pytest.mark.asyncio
async def test_discovery_reuses_mature_client_without_network(monkeypatch) -> None:
    _FakeClient.instances.clear()
    monkeypatch.setattr(broker_module, "MCPClient", _FakeClient)
    repo = _Repository()
    broker = MCPGatewayBroker(repository=repo, secret_resolver=_Secrets())
    first = await broker.discover(
        tenant_id="tenant-a", user_id="user-a", server_id="server-a", connection_id="connection-a", principal_type="user_delegated"
    )
    second = await broker.discover(
        tenant_id="tenant-a", user_id="user-a", server_id="server-a", connection_id="connection-a", principal_type="user_delegated"
    )
    assert first["protocol_version"] == second["protocol_version"]
    assert len(_FakeClient.instances) == 1
    assert _FakeClient.instances[0].initialize_calls == 1
    assert _FakeClient.instances[0].config.api_key == "ephemeral-token"
    assert repo.discovered[0]["tools"][0]["name"] == "upstream_search"


@pytest.mark.asyncio
async def test_readonly_invocation_uses_upstream_name_and_single_attempt(monkeypatch) -> None:
    _FakeClient.instances.clear()
    monkeypatch.setattr(broker_module, "MCPClient", _FakeClient)
    result = await MCPGatewayBroker(
        repository=_Repository(), secret_resolver=_Secrets()
    ).invoke_read_only(
        tenant_id="tenant-a", user_id="user-a", authenticated=True, channel="assistant",
        runtime_name="search", schema_hash="sha256:" + "a" * 64, risk_level="low",
        connection_id="connection-a", principal_type="user_delegated", arguments={"q": "hello"},
    )
    assert result["content"][0]["text"] == "hello"


@pytest.mark.asyncio
async def test_write_grant_is_rejected_before_any_client_or_network(monkeypatch) -> None:
    _FakeClient.instances.clear()
    monkeypatch.setattr(broker_module, "MCPClient", _FakeClient)
    with pytest.raises(MCPAuthorizationError, match="CAPABILITY_EXECUTION"):
        await MCPGatewayBroker(
            repository=_Repository(read_only=False), secret_resolver=_Secrets()
        ).invoke_read_only(
            tenant_id="tenant-a", user_id="user-a", authenticated=True, channel="assistant",
            runtime_name="write", schema_hash="sha256:" + "a" * 64, risk_level="high",
            connection_id="connection-a", principal_type="user_delegated", arguments={},
        )
    assert not _FakeClient.instances


@pytest.mark.asyncio
async def test_protocol_mismatch_is_rejected(monkeypatch) -> None:
    class MismatchClient(_FakeClient):
        async def initialize(self):
            return {"protocolVersion": "old"}

    monkeypatch.setattr(broker_module, "MCPClient", MismatchClient)
    with pytest.raises(broker_module.MCPGatewayBrokerError) as error:
        await MCPGatewayBroker(
            repository=_Repository(), secret_resolver=_Secrets()
        ).discover(
            tenant_id="tenant-a", user_id="user-a", server_id="server-a", connection_id="connection-a", principal_type="user_delegated"
        )
    assert error.value.code == "MCP_PROTOCOL_VERSION_MISMATCH"
