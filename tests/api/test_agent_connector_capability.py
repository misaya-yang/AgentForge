"""Connector capability binding validation (catalog model) — AS-02 gate.

Catalog-model connector bindings (config carries provider + tool_name but no
credential grant) are effective only when the provider has an enabled
connector_configs row visible to the tenant AND the calling user holds a
user_connectors row with status 'connected'. Grant-based bindings keep the
full credential-principal authorization path.
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_core.persistence.repositories.mcp_repository import (
    DatabaseMCPAgentCapabilityResolver,
    DatabaseMCPRepository,
    MCPAuthorizationError,
    MCPValidationError,
)


class _CatalogRepository(DatabaseMCPRepository):
    def __init__(
        self,
        *,
        config_row: dict[str, Any] | None,
        connection_row: dict[str, Any] | None,
        grant_row: dict[str, Any] | None = None,
    ):
        super().__init__(SimpleNamespace(enabled=True, _pool=object()))
        self.config_row = config_row
        self.connection_row = connection_row
        self.grant_row = grant_row
        self.catalog_calls: list[tuple[Any, ...]] = []
        self.grant_calls: list[tuple[Any, ...]] = []

    async def fetchrow(self, query: str, *_args: Any) -> dict[str, Any] | None:
        if "FROM connector_configs" in query:
            return dict(self.config_row) if self.config_row is not None else None
        if "FROM user_connectors" in query:
            return dict(self.connection_row) if self.connection_row is not None else None
        if "FROM connector_credential_principals" in query:
            return dict(self.grant_row) if self.grant_row is not None else None
        raise AssertionError(f"unhandled fetchrow query: {query}")

    async def authorize_connector_catalog(self, **kwargs: Any) -> dict[str, Any]:
        self.catalog_calls.append((kwargs.get("provider"), kwargs.get("user_id")))
        return await super().authorize_connector_catalog(**kwargs)

    async def authorize_connector_tool(self, **kwargs: Any) -> dict[str, Any]:
        self.grant_calls.append((kwargs.get("provider"), kwargs.get("grant_id")))
        return await super().authorize_connector_tool(**kwargs)


def _config(**changes: Any) -> dict[str, Any]:
    row: dict[str, Any] = {"provider": "jira", "mode": "live", "enabled": True}
    row.update(changes)
    return row


def _connection(**changes: Any) -> dict[str, Any]:
    row: dict[str, Any] = {
        "user_id": "user-a",
        "provider": "jira",
        "status": "connected",
    }
    row.update(changes)
    return row


def _catalog_binding(provider: str = "jira", tool: str = "jira_search") -> dict[str, Any]:
    return {
        "capability_type": "connector",
        "resource_id": tool,
        "config": {"provider": provider, "tool_name": tool},
    }


def _grant_binding() -> dict[str, Any]:
    return {
        "capability_type": "connector",
        "resource_id": "confluence_read",
        "config": {
            "provider": "confluence",
            "principal_type": "user_delegated",
            "grant_id": "11111111-1111-4111-8111-111111111111",
            "tool_name": "confluence_read",
        },
    }


def _resolver(repo: DatabaseMCPRepository) -> DatabaseMCPAgentCapabilityResolver:
    return DatabaseMCPAgentCapabilityResolver(repo)


def _grant() -> dict[str, Any]:
    return {
        "grant_id": "11111111-1111-4111-8111-111111111111",
        "provider": "confluence",
        "principal_type": "user_delegated",
        "owner_user_id": "user-a",
        "scopes": ["read:confluence-content.all"],
        "audience": "confluence",
        "connection_metadata": {},
        "allowed_channels": ["preview", "hosted_private"],
        "enabled": True,
        "revoked_at": None,
    }


@pytest.mark.asyncio
async def test_catalog_binding_allowed_when_enabled_and_connected() -> None:
    repo = _CatalogRepository(config_row=_config(), connection_row=_connection())
    allowed = await _resolver(repo).resolve(
        tenant_id="tenant-a",
        agent_id="agent-1",
        bindings=[_catalog_binding()],
        channel="preview",
        channel_policy={},
        user_id="user-a",
        authenticated=True,
    )

    assert len(allowed) == 1
    assert allowed[0]["resource_id"] == "jira_search"
    assert repo.catalog_calls == [("jira", "user-a")]


@pytest.mark.asyncio
async def test_catalog_binding_stripped_when_provider_not_in_catalog() -> None:
    repo = _CatalogRepository(config_row=None, connection_row=None)
    allowed = await _resolver(repo).resolve(
        tenant_id="tenant-a",
        agent_id="agent-1",
        bindings=[_catalog_binding()],
        channel="preview",
        channel_policy={},
        user_id="user-a",
        authenticated=True,
    )

    assert allowed == []


@pytest.mark.asyncio
async def test_catalog_binding_stripped_when_provider_disabled() -> None:
    repo = _CatalogRepository(config_row=_config(enabled=False), connection_row=_connection())
    allowed = await _resolver(repo).resolve(
        tenant_id="tenant-a",
        agent_id="agent-1",
        bindings=[_catalog_binding()],
        channel="preview",
        channel_policy={},
        user_id="user-a",
        authenticated=True,
    )

    assert allowed == []


@pytest.mark.asyncio
async def test_catalog_binding_stripped_for_ingest_only_mode() -> None:
    repo = _CatalogRepository(config_row=_config(mode="ingest"), connection_row=_connection())
    allowed = await _resolver(repo).resolve(
        tenant_id="tenant-a",
        agent_id="agent-1",
        bindings=[_catalog_binding()],
        channel="preview",
        channel_policy={},
        user_id="user-a",
        authenticated=True,
    )

    assert allowed == []


@pytest.mark.asyncio
async def test_catalog_binding_stripped_for_unauthenticated_caller() -> None:
    repo = _CatalogRepository(config_row=_config(), connection_row=_connection())
    allowed = await _resolver(repo).resolve(
        tenant_id="tenant-a",
        agent_id="agent-1",
        bindings=[_catalog_binding()],
        channel="preview",
        channel_policy={},
        user_id="",
        authenticated=False,
    )

    assert allowed == []


@pytest.mark.asyncio
async def test_catalog_binding_stripped_on_public_channel() -> None:
    repo = _CatalogRepository(config_row=_config(), connection_row=_connection())
    allowed = await _resolver(repo).resolve(
        tenant_id="tenant-a",
        agent_id="agent-1",
        bindings=[_catalog_binding()],
        channel="embed",
        channel_policy={},
        user_id="user-a",
        authenticated=True,
    )

    assert allowed == []


@pytest.mark.asyncio
async def test_catalog_binding_stripped_when_user_not_connected() -> None:
    repo = _CatalogRepository(config_row=_config(), connection_row=None)
    allowed = await _resolver(repo).resolve(
        tenant_id="tenant-a",
        agent_id="agent-1",
        bindings=[_catalog_binding()],
        channel="preview",
        channel_policy={},
        user_id="user-a",
        authenticated=True,
    )

    assert allowed == []


@pytest.mark.asyncio
async def test_catalog_binding_stripped_when_connection_revoked() -> None:
    repo = _CatalogRepository(
        config_row=_config(), connection_row=_connection(status="revoked")
    )
    allowed = await _resolver(repo).resolve(
        tenant_id="tenant-a",
        agent_id="agent-1",
        bindings=[_catalog_binding()],
        channel="preview",
        channel_policy={},
        user_id="user-a",
        authenticated=True,
    )

    assert allowed == []


@pytest.mark.asyncio
async def test_grant_binding_keeps_credential_principal_path() -> None:
    repo = _CatalogRepository(config_row=None, connection_row=None, grant_row=_grant())
    allowed = await _resolver(repo).resolve(
        tenant_id="tenant-a",
        agent_id="agent-1",
        bindings=[_grant_binding()],
        channel="preview",
        channel_policy={},
        user_id="user-a",
        authenticated=True,
    )

    assert len(allowed) == 1
    assert repo.catalog_calls == []
    assert repo.grant_calls == [
        ("confluence", "11111111-1111-4111-8111-111111111111")
    ]


@pytest.mark.asyncio
async def test_grant_binding_stripped_when_grant_missing() -> None:
    repo = _CatalogRepository(config_row=_config(), connection_row=_connection(), grant_row=None)
    allowed = await _resolver(repo).resolve(
        tenant_id="tenant-a",
        agent_id="agent-1",
        bindings=[_grant_binding()],
        channel="preview",
        channel_policy={},
        user_id="user-a",
        authenticated=True,
    )

    assert allowed == []


@pytest.mark.asyncio
async def test_catalog_lookup_is_tenant_aware() -> None:
    """The config lookup must pass tenant_id so a tenant-scoped row wins over global."""

    class _ScopedRepository(_CatalogRepository):
        def __init__(self):
            super().__init__(config_row=_config(), connection_row=_connection())
            self.seen_args: list[Any] = []

        async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
            if "FROM connector_configs" in query:
                self.seen_args = list(args)
            return await super().fetchrow(query, *args)

    scoped = _ScopedRepository()
    allowed = await _resolver(scoped).resolve(
        tenant_id="tenant-a",
        agent_id="agent-1",
        bindings=[_catalog_binding()],
        channel="preview",
        channel_policy={},
        user_id="user-a",
        authenticated=True,
    )

    assert len(allowed) == 1
    assert scoped.seen_args == ["jira", "tenant-a"]


@pytest.mark.asyncio
async def test_repository_error_codes_surface() -> None:
    cases: list[tuple[dict[str, Any] | None, dict[str, Any] | None, str, bool, str, str]] = [
        # config_row, connection_row, channel, authenticated, user_id, expected code
        (None, None, "preview", True, "user-a", "CONNECTOR_CATALOG_UNAVAILABLE"),
        (_config(), None, "preview", True, "user-a", "CONNECTOR_CATALOG_NOT_CONNECTED"),
        (_config(), _connection(status="error"), "preview", True, "user-a", "CONNECTOR_CATALOG_NOT_CONNECTED"),
        (_config(mode="ingest"), _connection(), "preview", True, "user-a", "CONNECTOR_CATALOG_INGEST_ONLY"),
        (_config(), _connection(), "preview", False, "", "CONNECTOR_CATALOG_PRINCIPAL_DENIED"),
        (_config(), _connection(), "hosted_public", True, "user-a", "CONNECTOR_CATALOG_PRINCIPAL_DENIED"),
    ]
    for config_row, connection_row, channel, authenticated, user_id, expected in cases:
        repo = _CatalogRepository(config_row=config_row, connection_row=connection_row)
        with pytest.raises(MCPAuthorizationError, match=expected):
            await repo.authorize_connector_catalog(
                tenant_id="tenant-a",
                user_id=user_id,
                authenticated=authenticated,
                provider="jira",
                tool_name="jira_search",
                channel=channel,
            )


@pytest.mark.asyncio
async def test_repository_allows_connected_user() -> None:
    repo = _CatalogRepository(config_row=_config(), connection_row=_connection())
    result = await repo.authorize_connector_catalog(
        tenant_id="tenant-a",
        user_id="user-a",
        authenticated=True,
        provider="jira",
        tool_name="jira_search",
        channel="preview",
    )

    assert result == {"provider": "jira", "tool_name": "jira_search", "user_id": "user-a"}


@pytest.mark.asyncio
async def test_publish_validation_allows_live_grantless_catalog_binding() -> None:
    repo = _CatalogRepository(config_row=_config(), connection_row=None)

    await repo.validate_version_binding(
        tenant_id="tenant-a",
        capability_type="connector",
        resource_id="jira_search",
        schema_hash=None,
        risk_level="low",
        config={"provider": "jira", "tool_name": "jira_search"},
    )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("config_row", "error_code"),
    [
        (None, "CONNECTOR_CATALOG_UNAVAILABLE"),
        (_config(enabled=False), "CONNECTOR_CATALOG_UNAVAILABLE"),
        (_config(mode="ingest"), "CONNECTOR_CATALOG_INGEST_ONLY"),
    ],
)
async def test_publish_validation_rejects_unavailable_catalog_binding(
    config_row: dict[str, Any] | None,
    error_code: str,
) -> None:
    repo = _CatalogRepository(config_row=config_row, connection_row=None)

    with pytest.raises(MCPValidationError, match=error_code):
        await repo.validate_version_binding(
            tenant_id="tenant-a",
            capability_type="connector",
            resource_id="jira_search",
            schema_hash=None,
            risk_level="low",
            config={"provider": "jira", "tool_name": "jira_search"},
        )


@pytest.mark.asyncio
async def test_publish_validation_preserves_legacy_confluence_grant_semantics() -> None:
    repo = _CatalogRepository(config_row=None, connection_row=None, grant_row=_grant())

    await repo.validate_version_binding(
        tenant_id="tenant-a",
        capability_type="connector",
        resource_id="confluence_read",
        schema_hash=None,
        risk_level="low",
        config=_grant_binding()["config"],
    )


@pytest.mark.asyncio
async def test_partial_grant_binding_never_falls_through_to_catalog() -> None:
    repo = _CatalogRepository(config_row=_config(), connection_row=_connection())
    binding = _catalog_binding(provider="confluence", tool="confluence_read")
    binding["config"]["grant_id"] = "11111111-1111-4111-8111-111111111111"

    allowed = await _resolver(repo).resolve(
        tenant_id="tenant-a",
        agent_id="agent-1",
        bindings=[binding],
        channel="preview",
        channel_policy={},
        user_id="user-a",
        authenticated=True,
    )

    assert allowed == []
    assert repo.catalog_calls == []
