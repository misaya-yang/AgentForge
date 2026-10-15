from __future__ import annotations

import json
import re
from collections.abc import Callable
from datetime import datetime, timezone
from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_core.security import decrypt_value, is_encrypted
from fastapi import HTTPException
from pydantic import ValidationError

from src.api.deps import AuthContext
from src.api.schemas.connectors import (
    ConnectorProviderCreate,
    ConnectorProviderResponse,
    ConnectorProviderUpdate,
    ConnectorToggleRequest,
)
from src.api.v1.connector_admin import (
    _row_to_response,
    create_connector_config,
    delete_connector_config,
    list_connector_configs,
    toggle_connector_config,
    update_connector_config,
)
from src.config.settings import Settings
from src.core.auth.user_resolver import UserContext

_INSERT_RE = re.compile(
    r"INSERT INTO connector_configs \((?P<cols>[^)]+)\) VALUES \((?P<ph>[^)]+)\)"
)
_UPDATE_CONFIG_RE = re.compile(
    r"UPDATE connector_configs SET (?P<sets>.+?),[ \t\n]*updated_at = NOW\(\)\s+"
    r"WHERE tenant_id = \$(?P<tid_idx>\d+) AND provider = \$(?P<pid_idx>\d+)"
)
_SELECT_ONE_RE = re.compile(
    r"SELECT \* FROM connector_configs\s+WHERE tenant_id = \$1 AND provider = \$2"
)
class _FakeDb:
    """In-memory connector_configs/user_connectors store driven by query text."""

    def __init__(self) -> None:
        self.configs: list[dict[str, Any]] = []
        self.user_connectors: list[dict[str, Any]] = []
        self.enabled = True

    # -- helpers ---------------------------------------------------------

    def _visible(self, tenant_id: str) -> list[dict[str, Any]]:
        return [
            row for row in self.configs if row.get("tenant_id") in {tenant_id, ""}
        ]

    def _row(self, tenant_id: str, provider: str) -> dict[str, Any] | None:
        return next(
            (
                row
                for row in self.configs
                if row.get("provider") == provider and row.get("tenant_id") == tenant_id
            ),
            None,
        )

    # -- async DB surface -------------------------------------------------

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        if "SELECT 1 FROM connector_configs" in query:
            if "tenant_id = ''" in query:
                provider = args[0]
                return (
                    {"present": True}
                    if any(
                        row.get("tenant_id") == "" and row.get("provider") == provider
                        for row in self.configs
                    )
                    else None
                )
            tenant_id, provider = args
            return (
                {"present": True}
                if any(
                    row.get("tenant_id") == tenant_id and row.get("provider") == provider
                    for row in self.configs
                )
                else None
            )
        if _SELECT_ONE_RE.search(query):
            tenant_id, provider = args
            return next(
                (
                    dict(row)
                    for row in self.configs
                    if row.get("tenant_id") == tenant_id and row.get("provider") == provider
                ),
                None,
            )
        raise AssertionError(f"unhandled fetchrow query: {query}")

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        if "SELECT 1 FROM user_connectors" in query:
            if "tenant_id = $1" in query:
                tenant_id, provider = args
                return [
                    row
                    for row in self.user_connectors
                    if row.get("tenant_id") == tenant_id and row.get("provider") == provider
                ]
            provider = args[0]
            return [row for row in self.user_connectors if row.get("provider") == provider]
        if "FROM connector_configs" in query and "WHERE" in query:
            effective: dict[str, dict[str, Any]] = {}
            for row in self._visible(args[0]):
                provider = str(row.get("provider") or "")
                current = effective.get(provider)
                if current is None or row.get("tenant_id") == args[0]:
                    effective[provider] = row
            rows = sorted(
                effective.values(),
                key=lambda r: (str(r.get("display_name") or ""), str(r.get("provider") or "")),
            )
            return [dict(row) for row in rows]
        raise AssertionError(f"unhandled fetch query: {query}")

    async def execute(self, query: str, *args: Any) -> None:
        match = _INSERT_RE.search(query)
        if match:
            columns = [col.strip() for col in match.group("cols").split(",")]
            values = list(args)
            row = dict(zip(columns, values, strict=True))
            if "tenant_id" not in row:
                raise AssertionError("insert missing tenant_id")
            if any(
                r.get("tenant_id") == row["tenant_id"] and r.get("provider") == row["provider"]
                for r in self.configs
            ):
                raise AssertionError("duplicate (tenant_id, provider) insert")
            self.configs.append(row)
            return
        match = _UPDATE_CONFIG_RE.search(query)
        if match:
            tid_idx = int(match.group("tid_idx"))
            pid_idx = int(match.group("pid_idx"))
            tenant_id = args[tid_idx - 1]
            provider = args[pid_idx - 1]
            row = self._row(tenant_id, provider)
            if row is None:
                raise AssertionError("update on missing row")
            assignments = match.group("sets").split(",")
            for index, assignment in enumerate(assignments):
                column = assignment.strip().split("=")[0].strip()
                if column == "extra_config":
                    # COALESCE(extra_config, '{}'::jsonb) || $1::jsonb
                    json_value = args[index]
                    existing = row.get("extra_config") or {}
                    merged = dict(existing)
                    merged.update(json.loads(json_value))
                    row["extra_config"] = merged
                else:
                    row[column] = args[index]
            return
        if query.startswith("DELETE FROM connector_configs"):
            tenant_id, provider = args
            row = self._row(tenant_id, provider)
            if row is None:
                raise AssertionError("delete on missing row")
            self.configs.remove(row)
            return
        raise AssertionError(f"unhandled execute query: {query}")


def _public_destination(_host: str, _port: int) -> tuple[bool, str]:
    return True, "93.184.216.34"


def _request(
    db: _FakeDb,
    *,
    destination_validator: Callable[[str, int], tuple[bool, str]] = _public_destination,
    encryption_key: str = "connector-unit-test-key",
) -> SimpleNamespace:
    return SimpleNamespace(
        state=SimpleNamespace(),
        app=SimpleNamespace(
            state=SimpleNamespace(
                settings=Settings(),
                database=db,
                connector_destination_validator=destination_validator,
                connector_encryption_key=encryption_key,
            )
        ),
    )


def _read_auth() -> AuthContext:
    return AuthContext(
        user_id="admin",
        tenant_id="tenant-a",
        roles=["developer"],
        permissions=["console:settings:view"],
        is_authenticated=True,
    )


def _write_auth(**changes: Any) -> AuthContext:
    values: dict[str, Any] = {
        "user_id": "admin",
        "tenant_id": "tenant-a",
        "roles": ["tenant_admin"],
        "permissions": ["console:settings:edit"],
        "is_authenticated": True,
    }
    values.update(changes)
    return AuthContext(**values)


def _user() -> UserContext:
    return UserContext(
        user_id="admin",
        tenant_id="tenant-a",
        tier="enterprise",
        is_authenticated=True,
        roles=["tenant_admin"],
        ip="127.0.0.1",
    )


def _config_row(**changes: Any) -> dict[str, Any]:
    row: dict[str, Any] = {
        "tenant_id": "tenant-a",
        "provider": "confluence",
        "display_name": "Confluence",
        "description": None,
        "icon_url": None,
        "client_id": "client-123",
        "client_secret": "super-secret",
        "auth_url": "https://auth.example.com/authorize",
        "token_url": "https://auth.example.com/token",
        "scopes": "read write",
        "redirect_uri": None,
        "enabled": True,
        "supports_sync": False,
        "supports_search": True,
        "mode": "live",
        "extra_config": {},
        "created_at": datetime(2026, 8, 1, tzinfo=timezone.utc),
        "updated_at": datetime(2026, 8, 1, tzinfo=timezone.utc),
    }
    row.update(changes)
    return row


def _create_payload(**changes: Any) -> ConnectorProviderCreate:
    data: dict[str, Any] = {
        "provider": "jira",
        "display_name": "Jira",
        "description": "Issue tracker",
        "mode": "live",
        "auth": {
            "client_id": "cid",
            "client_secret": "csecret",
            "auth_url": "https://auth.example.com/authorize",
            "token_url": "https://auth.example.com/token",
            "scopes": "read",
        },
        "mcp_tools": [{"name": "jira_search", "description": "Search issues"}],
    }
    data.update(changes)
    return ConnectorProviderCreate(**data)


# ─── Tests ───────────────────────────────────────────────────────────────


@pytest.mark.asyncio
async def test_list_configs_includes_effective_rows_without_client_secret() -> None:
    db = _FakeDb()
    db.configs = [
        _config_row(tenant_id="", provider="github"),
        _config_row(tenant_id="tenant-a", provider="confluence"),
    ]
    request = _request(db)

    result = await list_connector_configs(request=request, user=_user(), auth=_read_auth())

    providers = [item["provider"] for item in result]
    assert providers == ["confluence", "github"]
    for item in result:
        assert "client_secret" not in item["auth"]
        assert item["auth"]["client_id"] == "client-123"
        assert item["mode"] == "live"


@pytest.mark.asyncio
async def test_list_prefers_tenant_override_over_global_template() -> None:
    db = _FakeDb()
    db.configs = [
        _config_row(tenant_id="", display_name="Global"),
        _config_row(tenant_id="tenant-a", display_name="Tenant"),
    ]

    result = await list_connector_configs(
        request=_request(db), user=_user(), auth=_read_auth()
    )

    assert len(result) == 1
    assert result[0]["display_name"] == "Tenant"
    assert result[0]["tenant_id"] == "tenant-a"


@pytest.mark.asyncio
async def test_create_config_persists_secret_but_never_echoes_it() -> None:
    db = _FakeDb()
    request = _request(db)

    result = await create_connector_config(
        payload=_create_payload(), request=request, user=_user(), auth=_write_auth()
    )

    assert result["provider"] == "jira"
    assert "client_secret" not in result["auth"]
    stored = db._row("tenant-a", "jira")
    assert stored is not None
    assert is_encrypted(stored["client_secret"])
    assert decrypt_value(stored["client_secret"], "connector-unit-test-key") == "csecret"
    assert stored["extra_config"]["mcp_tools"] == [
        {"name": "jira_search", "description": "Search issues"}
    ]


@pytest.mark.asyncio
async def test_create_config_conflicts_on_existing_provider() -> None:
    db = _FakeDb()
    db.configs = [_config_row(tenant_id="tenant-a", provider="jira")]
    request = _request(db)

    with pytest.raises(HTTPException) as exc:
        await create_connector_config(
            payload=_create_payload(), request=request, user=_user(), auth=_write_auth()
        )

    assert exc.value.status_code == 409


@pytest.mark.asyncio
async def test_update_config_keeps_existing_secret_when_empty() -> None:
    db = _FakeDb()
    db.configs = [_config_row(tenant_id="tenant-a", provider="confluence")]
    request = _request(db)

    result = await update_connector_config(
        provider="confluence",
        payload=ConnectorProviderUpdate(
            display_name="Confluence Cloud",
            mode="both",
            auth={"client_id": "new-cid", "client_secret": None},
        ),
        request=request,
        user=_user(),
        auth=_write_auth(),
    )

    assert result["display_name"] == "Confluence Cloud"
    assert result["mode"] == "both"
    stored = db._row("tenant-a", "confluence")
    assert stored["client_secret"] == "super-secret"
    assert stored["client_id"] == "new-cid"


@pytest.mark.asyncio
async def test_update_config_replaces_secret_when_provided() -> None:
    db = _FakeDb()
    db.configs = [_config_row(tenant_id="tenant-a", provider="confluence")]
    request = _request(db)

    result = await update_connector_config(
        provider="confluence",
        payload=ConnectorProviderUpdate(
            auth={"client_secret": "rotated-secret"},
        ),
        request=request,
        user=_user(),
        auth=_write_auth(),
    )

    assert "client_secret" not in result["auth"]
    stored_secret = db._row("tenant-a", "confluence")["client_secret"]
    assert is_encrypted(stored_secret)
    assert decrypt_value(stored_secret, "connector-unit-test-key") == "rotated-secret"


@pytest.mark.asyncio
async def test_update_config_extra_only_does_not_emit_empty_set_clause() -> None:
    db = _FakeDb()
    db.configs = [_config_row()]

    result = await update_connector_config(
        provider="confluence",
        payload=ConnectorProviderUpdate(
            mcp_tools=[{"name": "confluence_read", "description": "Read pages"}],
        ),
        request=_request(db),
        user=_user(),
        auth=_write_auth(),
    )

    assert result["mcp_tools"] == [
        {"name": "confluence_read", "description": "Read pages"}
    ]
    assert db._row("tenant-a", "confluence")["display_name"] == "Confluence"


@pytest.mark.asyncio
async def test_update_config_rejects_empty_payload() -> None:
    db = _FakeDb()
    db.configs = [_config_row(tenant_id="tenant-a", provider="confluence")]
    request = _request(db)

    with pytest.raises(HTTPException) as exc:
        await update_connector_config(
            provider="confluence",
            payload=ConnectorProviderUpdate(),
            request=request,
            user=_user(),
            auth=_write_auth(),
        )

    assert exc.value.status_code == 422


@pytest.mark.asyncio
async def test_toggle_enabled_flips_the_flag() -> None:
    db = _FakeDb()
    db.configs = [_config_row(tenant_id="tenant-a", provider="confluence")]
    request = _request(db)

    result = await toggle_connector_config(
        provider="confluence",
        payload=ConnectorToggleRequest(enabled=False),
        request=request,
        user=_user(),
        auth=_write_auth(),
    )

    assert result["enabled"] is False
    assert db._row("tenant-a", "confluence")["enabled"] is False


@pytest.mark.asyncio
async def test_delete_config_refused_while_user_connectors_exist() -> None:
    db = _FakeDb()
    db.configs = [_config_row(tenant_id="tenant-a", provider="confluence")]
    db.user_connectors = [
        {
            "tenant_id": "tenant-a",
            "user_id": "user-1",
            "provider": "confluence",
            "status": "connected",
        }
    ]
    request = _request(db)

    with pytest.raises(HTTPException) as exc:
        await delete_connector_config(
            provider="confluence", request=request, user=_user(), auth=_write_auth()
        )

    assert exc.value.status_code == 409
    assert db._row("tenant-a", "confluence") is not None


@pytest.mark.asyncio
async def test_delete_config_succeeds_when_no_connections() -> None:
    db = _FakeDb()
    db.configs = [_config_row(tenant_id="tenant-a", provider="confluence")]
    request = _request(db)

    result = await delete_connector_config(
        provider="confluence", request=request, user=_user(), auth=_write_auth()
    )

    assert result["status"] == "deleted"
    assert db._row("tenant-a", "confluence") is None


@pytest.mark.asyncio
async def test_global_config_is_read_only_for_tenant_admin() -> None:
    db = _FakeDb()
    db.configs = [_config_row(tenant_id="", provider="github")]
    request = _request(db)

    with pytest.raises(HTTPException) as exc:
        await delete_connector_config(
            provider="github", request=request, user=_user(), auth=_write_auth()
        )

    assert exc.value.status_code == 403
    assert db._row("", "github") is not None


@pytest.mark.asyncio
async def test_missing_provider_returns_404() -> None:
    db = _FakeDb()
    request = _request(db)

    with pytest.raises(HTTPException) as exc:
        await delete_connector_config(
            provider="nope", request=request, user=_user(), auth=_write_auth()
        )

    assert exc.value.status_code == 404


@pytest.mark.asyncio
async def test_endpoints_deny_callers_without_settings_permission() -> None:
    db = _FakeDb()
    db.configs = [_config_row(tenant_id="tenant-a", provider="confluence")]
    request = _request(db)
    auth = AuthContext(
        user_id="user-1",
        tenant_id="tenant-a",
        roles=["user"],
        permissions=[],
        is_authenticated=True,
    )

    with pytest.raises(HTTPException) as exc:
        await list_connector_configs(request=request, user=_user(), auth=auth)

    assert exc.value.status_code == 403


@pytest.mark.asyncio
async def test_read_permission_cannot_mutate_connector_config() -> None:
    db = _FakeDb()
    db.configs = [_config_row()]

    with pytest.raises(HTTPException) as exc:
        await toggle_connector_config(
            provider="confluence",
            payload=ConnectorToggleRequest(enabled=False),
            request=_request(db),
            user=_user(),
            auth=_write_auth(permissions=["console:settings:view"]),
        )

    assert exc.value.status_code == 403
    assert db._row("tenant-a", "confluence")["enabled"] is True


@pytest.mark.asyncio
async def test_edit_permission_without_tenant_admin_role_cannot_mutate() -> None:
    db = _FakeDb()
    db.configs = [_config_row()]

    with pytest.raises(HTTPException) as exc:
        await update_connector_config(
            provider="confluence",
            payload=ConnectorProviderUpdate(display_name="Denied"),
            request=_request(db),
            user=_user(),
            auth=_write_auth(roles=["developer"]),
        )

    assert exc.value.status_code == 403
    assert db._row("tenant-a", "confluence")["display_name"] == "Confluence"


@pytest.mark.asyncio
async def test_cross_tenant_mutation_is_hidden_and_never_updates_other_row() -> None:
    db = _FakeDb()
    db.configs = [_config_row(tenant_id="tenant-b")]

    with pytest.raises(HTTPException) as exc:
        await update_connector_config(
            provider="confluence",
            payload=ConnectorProviderUpdate(display_name="Cross-tenant write"),
            request=_request(db),
            user=_user(),
            auth=_write_auth(),
        )

    assert exc.value.status_code == 404
    assert db._row("tenant-b", "confluence")["display_name"] == "Confluence"


@pytest.mark.asyncio
async def test_global_template_update_and_toggle_are_rejected() -> None:
    db = _FakeDb()
    db.configs = [_config_row(tenant_id="")]
    request = _request(db)

    with pytest.raises(HTTPException, match="Global connector templates") as update_exc:
        await update_connector_config(
            provider="confluence",
            payload=ConnectorProviderUpdate(display_name="Denied"),
            request=request,
            user=_user(),
            auth=_write_auth(),
        )
    with pytest.raises(HTTPException, match="Global connector templates") as toggle_exc:
        await toggle_connector_config(
            provider="confluence",
            payload=ConnectorToggleRequest(enabled=False),
            request=request,
            user=_user(),
            auth=_write_auth(),
        )

    assert update_exc.value.status_code == 403
    assert toggle_exc.value.status_code == 403
    assert db._row("", "confluence")["display_name"] == "Confluence"
    assert db._row("", "confluence")["enabled"] is True


@pytest.mark.parametrize(
    "unsafe_url",
    [
        "http://oauth.example/token",
        "https://127.0.0.1/token",
        "https://169.254.169.254/token",
        "https://[::1]/token",
        "https://user:password@oauth.example/token",
        "https://oauth.example/token#fragment",
    ],
)
def test_oauth_schema_rejects_unsafe_token_endpoints(unsafe_url: str) -> None:
    with pytest.raises(ValidationError):
        _create_payload(
            auth={
                "client_id": "cid",
                "auth_url": "https://oauth.example/authorize",
                "token_url": unsafe_url,
            }
        )


@pytest.mark.asyncio
async def test_oauth_hostname_must_resolve_publicly_before_persistence() -> None:
    db = _FakeDb()

    def private_destination(_host: str, _port: int) -> tuple[bool, str]:
        return False, "resolves to disallowed address"

    with pytest.raises(HTTPException) as exc:
        await create_connector_config(
            payload=_create_payload(),
            request=_request(db, destination_validator=private_destination),
            user=_user(),
            auth=_write_auth(),
        )

    assert exc.value.status_code == 422
    assert db.configs == []


@pytest.mark.asyncio
async def test_new_secret_requires_encryption_key() -> None:
    db = _FakeDb()

    with pytest.raises(HTTPException) as exc:
        await create_connector_config(
            payload=_create_payload(),
            request=_request(db, encryption_key=""),
            user=_user(),
            auth=_write_auth(),
        )

    assert exc.value.status_code == 503
    assert db.configs == []


def test_response_contract_uses_datetimes_and_has_no_secret_field() -> None:
    public = ConnectorProviderResponse.model_validate(_row_to_response(_config_row()))
    payload = public.model_dump(mode="json")

    assert payload["created_at"] == "2026-08-01T00:00:00Z"
    assert "client_secret" not in payload["auth"]
    auth_schema = ConnectorProviderResponse.model_json_schema()["$defs"][
        "ConnectorAuthResponse"
    ]
    assert "client_secret" not in auth_schema["properties"]


def test_response_contract_decodes_legacy_json_string_extra_config() -> None:
    public = ConnectorProviderResponse.model_validate(
        _row_to_response(
            _config_row(
                extra_config=(
                    '{"audience":"api.atlassian.com",'
                    '"mcp_tools":[{"name":"search","description":"Search"}]}'
                )
            )
        )
    )

    assert public.extra_config == {"audience": "api.atlassian.com"}
    assert [tool.name for tool in public.mcp_tools] == ["search"]
