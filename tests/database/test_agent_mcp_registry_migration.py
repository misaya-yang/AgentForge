from __future__ import annotations

import uuid
from collections.abc import AsyncIterator
from pathlib import Path
from types import SimpleNamespace

import asyncpg
import pytest
import pytest_asyncio
from ai_gateway_core.persistence.repositories.agent_repository import (
    DatabaseAgentRepository,
)
from ai_gateway_core.persistence.repositories.mcp_repository import (
    DatabaseMCPRepository,
    MCPAuthorizationError,
    MCPNotFoundError,
    MCPValidationError,
)

from tests.database.test_agent_studio_migrations import _postgres_config

ROOT = Path(__file__).resolve().parents[2]
DOMAIN_MIGRATION = ROOT / "database" / "migrations" / "071_agent_studio_domain.sql"
MCP_MIGRATION = ROOT / "database" / "migrations" / "074_agent_mcp_registry.sql"
MCP_TABLES = {
    "mcp_servers",
    "mcp_connections",
    "mcp_tools",
    "mcp_tool_snapshots",
    "mcp_schema_diffs",
    "mcp_channel_grants",
    "connector_credential_principals",
}


@pytest_asyncio.fixture
async def mcp_pool() -> AsyncIterator[asyncpg.Pool]:
    config = _postgres_config()
    schema_name = f"agent_mcp_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE SCHEMA "{schema_name}"')
    await admin.close()
    pool = await asyncpg.create_pool(
        **config,
        min_size=1,
        max_size=2,
        server_settings={"search_path": f'"{schema_name}",public'},
    )
    try:
        async with pool.acquire() as connection:
            await connection.execute(
                """
                CREATE TABLE datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL DEFAULT '',
                    name VARCHAR(255) NOT NULL DEFAULT ''
                );
                CREATE TABLE users (
                    user_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    status VARCHAR(50) NOT NULL DEFAULT 'active'
                );
                CREATE TABLE audit_logs (
                    id BIGSERIAL PRIMARY KEY,
                    event_type VARCHAR(100) NOT NULL,
                    user_id VARCHAR(255),
                    tenant_id VARCHAR(255),
                    resource_type VARCHAR(100),
                    resource_id VARCHAR(255),
                    action VARCHAR(50) NOT NULL,
                    request_summary JSONB,
                    status VARCHAR(50) NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                """
            )
            await connection.execute(DOMAIN_MIGRATION.read_text(encoding="utf-8"))
            mcp_sql = MCP_MIGRATION.read_text(encoding="utf-8")
            await connection.execute(mcp_sql)
            await connection.execute(mcp_sql)
            await connection.executemany(
                "INSERT INTO users (user_id, tenant_id) VALUES ($1, $2)",
                [("admin-a", "tenant-a"), ("user-a", "tenant-a"), ("admin-b", "tenant-b")],
            )
        yield pool
    finally:
        await pool.close()
        admin = await asyncpg.connect(**config)
        await admin.execute(f'DROP SCHEMA "{schema_name}" CASCADE')
        await admin.close()


def _repository(pool: asyncpg.Pool) -> DatabaseMCPRepository:
    return DatabaseMCPRepository(SimpleNamespace(enabled=True, _pool=pool))


async def _server_and_connection(
    repository: DatabaseMCPRepository,
    *,
    tenant_id: str = "tenant-a",
) -> tuple[dict, dict]:
    server = await repository.create_server(
        tenant_id=tenant_id,
        user_id="admin-a" if tenant_id == "tenant-a" else "admin-b",
        name=f"MCP {uuid.uuid4().hex[:8]}",
        description="",
        base_url="https://mcp.example",
        auth_method="none",
        oauth_metadata_url=None,
        oauth_resource=None,
        oauth_audience=None,
        allowed_origins=["https://studio.example"],
        timeout_ms=1000,
        max_concurrency=2,
        response_limit_bytes=65536,
    )
    connection = await repository.create_connection(
        tenant_id=tenant_id,
        server_id=str(server["server_id"]),
        user_id="admin-a" if tenant_id == "tenant-a" else "admin-b",
        principal_type="service_account",
        owner_user_id=None,
        secret_ref=None,
        scopes=[],
        audience=None,
        expires_at=None,
    )
    return server, connection


@pytest.mark.asyncio
async def test_migration_is_idempotent_additive_and_secret_ref_only(
    mcp_pool: asyncpg.Pool,
) -> None:
    sql = MCP_MIGRATION.read_text(encoding="utf-8")
    upper = sql.upper()
    assert "DROP TABLE" not in upper
    assert "TRUNCATE" not in upper
    assert "ACCESS_TOKEN" not in upper
    assert "REFRESH_TOKEN" not in upper
    assert "CLIENT_SECRET" not in upper

    async with mcp_pool.acquire() as connection:
        tables = await connection.fetch(
            """
            SELECT table_name FROM information_schema.tables
            WHERE table_schema = current_schema()
              AND table_name = ANY($1::text[])
            """,
            list(MCP_TABLES),
        )
        assert {row["table_name"] for row in tables} == MCP_TABLES
        columns = await connection.fetch(
            """
            SELECT table_name FROM information_schema.columns
            WHERE table_schema = current_schema()
              AND column_name = 'tenant_id'
              AND table_name = ANY($1::text[])
            """,
            list(MCP_TABLES),
        )
        assert {row["table_name"] for row in columns} == MCP_TABLES

        with pytest.raises(asyncpg.CheckViolationError):
            async with connection.transaction():
                await connection.execute(
                    """
                    INSERT INTO mcp_servers (
                        tenant_id, name, base_url, created_by, updated_by
                    ) VALUES ('tenant-a', 'unsafe', 'http://127.0.0.1', 'admin-a', 'admin-a')
                    """
                )


@pytest.mark.asyncio
async def test_discovery_snapshots_are_immutable_versioned_and_tenant_scoped(
    mcp_pool: asyncpg.Pool,
) -> None:
    repository = _repository(mcp_pool)
    server, connection = await _server_and_connection(repository)
    server_id = str(server["server_id"])
    first = await repository.record_discovery(
        tenant_id="tenant-a",
        server_id=server_id,
        tools=[
            {
                "name": "search",
                "description": "Search",
                "inputSchema": {
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                },
                "risk_level": "low",
                "read_only": True,
            }
        ],
    )
    repeated = await repository.record_discovery(
        tenant_id="tenant-a",
        server_id=server_id,
        tools=[
            {
                "name": "search",
                "description": "Search",
                "inputSchema": {
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                },
                "risk_level": "low",
                "read_only": True,
            }
        ],
    )
    agent_repository = DatabaseAgentRepository(SimpleNamespace(enabled=True, _pool=mcp_pool))
    spec = {
        "schema_version": "agent-spec/v1",
        "identity": {},
        "instructions": "Use only the bound MCP search tool.",
        "model": {"model_id": "qwen3.7-plus"},
        "capabilities": [
            {
                "type": "mcp",
                "resource_id": first["changed"][0]["runtime_name"],
                "resource_version": "1",
                "schema_hash": first["changed"][0]["schema_hash"],
                "config": {
                    "connection_id": str(connection["connection_id"]),
                    "principal_type": "service_account",
                    "risk": "medium",
                },
            }
        ],
        "knowledge": [],
        "memory": {},
    }
    agent = await agent_repository.create_agent(
        tenant_id="tenant-a",
        user_id="admin-a",
        name=f"MCP Agent {uuid.uuid4().hex[:8]}",
        slug=None,
        description="",
        spec=spec,
    )
    version = await agent_repository.create_version(
        tenant_id="tenant-a",
        agent_id=agent["agent_id"],
        user_id="admin-a",
        is_tenant_admin=False,
        expected_revision=1,
    )
    changed = await repository.record_discovery(
        tenant_id="tenant-a",
        server_id=server_id,
        tools=[
            {
                "name": "search",
                "description": "Search",
                "inputSchema": {
                    "type": "object",
                    "properties": {"query": {"type": "integer"}},
                    "required": ["query"],
                },
                "risk_level": "low",
                "read_only": True,
            }
        ],
    )
    classification_spoof = await repository.record_discovery(
        tenant_id="tenant-a",
        server_id=server_id,
        tools=[
            {
                "name": "search",
                "description": "Search",
                "inputSchema": {
                    "type": "object",
                    "properties": {"query": {"type": "integer"}},
                    "required": ["query"],
                },
                "risk_level": "critical",
                "read_only": True,
            }
        ],
    )

    assert first["changed"][0]["schema_version"] == 1
    assert repeated["unchanged"][0]["tool_id"] == first["changed"][0]["tool_id"]
    assert changed["changed"][0]["schema_version"] == 2
    assert changed["changed"][0]["breaking"] is True
    assert changed["changed"][0]["runtime_name"] == first["changed"][0]["runtime_name"]
    assert classification_spoof["unchanged"][0]["schema_version"] == 2

    async with mcp_pool.acquire() as db:
        sealed = await db.fetchrow(
            """
            SELECT schema_hash, config FROM agent_version_capabilities
            WHERE tenant_id = 'tenant-a' AND agent_version_id = $1
            """,
            uuid.UUID(version["agent_version_id"]),
        )
        assert sealed is not None
        assert sealed["schema_hash"] == first["changed"][0]["schema_hash"]
        assert first["changed"][0]["schema_hash"] != changed["changed"][0]["schema_hash"]

    tools = await repository.list_tools(tenant_id="tenant-a", server_id=server_id)
    assert len(tools) == 1
    assert tools[0]["risk_level"] == "medium"
    assert tools[0]["read_only"] is False
    assert await repository.list_tools(tenant_id="tenant-b", server_id=server_id) == []
    with pytest.raises(MCPNotFoundError):
        await repository.get_server(tenant_id="tenant-b", server_id=server_id)

    async with mcp_pool.acquire() as db:
        with pytest.raises(asyncpg.PostgresError, match="immutable"):
            async with db.transaction():
                await db.execute(
                    """
                    UPDATE mcp_tool_snapshots SET description = 'mutated'
                    WHERE tenant_id = 'tenant-a' AND tool_id = $1
                    """,
                    tools[0]["tool_id"],
                )

    with pytest.raises(MCPAuthorizationError):
        await repository.authorize_mcp_tool(
            tenant_id="tenant-b",
            user_id="admin-b",
            authenticated=True,
            runtime_name=tools[0]["runtime_name"],
            schema_hash=tools[0]["schema_hash"],
            risk_level="medium",
            connection_id=str(connection["connection_id"]),
            principal_type="service_account",
            channel="preview",
        )


@pytest.mark.asyncio
async def test_public_grant_exact_binding_revocation_and_circuit_are_fail_closed(
    mcp_pool: asyncpg.Pool,
) -> None:
    repository = _repository(mcp_pool)
    server, connection = await _server_and_connection(repository)
    server_id = str(server["server_id"])
    discovery = await repository.record_discovery(
        tenant_id="tenant-a",
        server_id=server_id,
        tools=[
            {
                "name": "read",
                "inputSchema": {"type": "object", "properties": {}},
                "risk_level": "low",
                "read_only": True,
            }
        ],
    )
    tool = discovery["changed"][0]
    discovered = await repository.list_tools(tenant_id="tenant-a", server_id=server_id)
    assert discovered[0]["read_only"] is False
    assert discovered[0]["risk_level"] == "medium"
    await repository.grant_channel(
        tenant_id="tenant-a",
        connection_id=str(connection["connection_id"]),
        tool_id=str(tool["tool_id"]),
        channel="embed",
        read_only_only=True,
        user_id="admin-a",
    )
    authorized = await repository.authorize_mcp_tool(
        tenant_id="tenant-a",
        user_id="",
        authenticated=False,
        runtime_name=tool["runtime_name"],
        schema_hash=tool["schema_hash"],
        risk_level="medium",
        connection_id=str(connection["connection_id"]),
        principal_type="service_account",
        channel="embed",
    )
    assert authorized["read_only"] is True
    assert authorized["admin_read_only_approved"] is True
    binding_config = {
        "connection_id": str(connection["connection_id"]),
        "principal_type": "service_account",
        "risk": "medium",
    }
    await repository.validate_version_binding(
        tenant_id="tenant-a",
        capability_type="mcp",
        resource_id=tool["runtime_name"],
        schema_hash=tool["schema_hash"],
        risk_level="medium",
        config=binding_config,
    )
    with pytest.raises(MCPValidationError, match="MCP_SCHEMA_CHANGED"):
        await repository.validate_version_binding(
            tenant_id="tenant-a",
            capability_type="mcp",
            resource_id=tool["runtime_name"],
            schema_hash="f" * 64,
            risk_level="medium",
            config=binding_config,
        )
    with pytest.raises(MCPValidationError, match="MCP_RISK_CHANGED"):
        await repository.validate_version_binding(
            tenant_id="tenant-a",
            capability_type="mcp",
            resource_id=tool["runtime_name"],
            schema_hash=tool["schema_hash"],
            risk_level="high",
            config={**binding_config, "risk": "high"},
        )

    drift = await repository.record_discovery(
        tenant_id="tenant-a",
        server_id=server_id,
        tools=[
            {
                "name": "read",
                "inputSchema": {
                    "type": "object",
                    "properties": {"limit": {"type": "integer"}},
                },
                "risk_level": "low",
                "read_only": True,
            }
        ],
    )
    current_tool = drift["changed"][0]
    assert current_tool["schema_hash"] != tool["schema_hash"]
    assert current_tool["breaking"] is True
    with pytest.raises(MCPAuthorizationError, match="MCP_PUBLIC_CHANNEL_DENIED"):
        await repository.authorize_mcp_tool(
            tenant_id="tenant-a",
            user_id="",
            authenticated=False,
            runtime_name=current_tool["runtime_name"],
            schema_hash=current_tool["schema_hash"],
            risk_level="medium",
            connection_id=str(connection["connection_id"]),
            principal_type="service_account",
            channel="embed",
        )
    renewed = await repository.grant_channel(
        tenant_id="tenant-a",
        connection_id=str(connection["connection_id"]),
        tool_id=str(current_tool["tool_id"]),
        channel="embed",
        read_only_only=True,
        user_id="admin-a",
    )
    assert renewed["approved_schema_hash"] == current_tool["schema_hash"]
    await repository.authorize_mcp_tool(
        tenant_id="tenant-a",
        user_id="",
        authenticated=False,
        runtime_name=current_tool["runtime_name"],
        schema_hash=current_tool["schema_hash"],
        risk_level="medium",
        connection_id=str(connection["connection_id"]),
        principal_type="service_account",
        channel="embed",
    )
    tool = current_tool

    for _ in range(3):
        await repository.record_runtime_result(
            tenant_id="tenant-a",
            server_id=server_id,
            success=False,
            error_code="MCP_TIMEOUT",
        )
    with pytest.raises(MCPAuthorizationError, match="MCP_CIRCUIT_OPEN"):
        await repository.authorize_mcp_tool(
            tenant_id="tenant-a",
            user_id="admin-a",
            authenticated=True,
            runtime_name=tool["runtime_name"],
            schema_hash=tool["schema_hash"],
            risk_level="medium",
            connection_id=str(connection["connection_id"]),
            principal_type="service_account",
            channel="preview",
        )

    await repository.record_runtime_result(tenant_id="tenant-a", server_id=server_id, success=True)
    await repository.revoke_connection(
        tenant_id="tenant-a",
        connection_id=str(connection["connection_id"]),
        user_id="admin-a",
    )
    with pytest.raises(MCPAuthorizationError, match="MCP_CAPABILITY_UNAVAILABLE"):
        await repository.authorize_mcp_tool(
            tenant_id="tenant-a",
            user_id="admin-a",
            authenticated=True,
            runtime_name=tool["runtime_name"],
            schema_hash=tool["schema_hash"],
            risk_level="medium",
            connection_id=str(connection["connection_id"]),
            principal_type="service_account",
            channel="preview",
        )

    other_server, other_connection = await _server_and_connection(repository)
    with pytest.raises(MCPNotFoundError):
        await repository.grant_channel(
            tenant_id="tenant-a",
            connection_id=str(other_connection["connection_id"]),
            tool_id=str(tool["tool_id"]),
            channel="embed",
            read_only_only=True,
            user_id="admin-a",
        )
    assert other_server["server_id"] != server["server_id"]

    with pytest.raises(MCPValidationError, match="MCP_BINDING_INCOMPLETE"):
        await repository.validate_version_binding(
            tenant_id="tenant-a",
            capability_type="mcp",
            resource_id=tool["runtime_name"],
            schema_hash=tool["schema_hash"],
            risk_level="medium",
            config={},
        )
