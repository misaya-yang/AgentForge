"""Contract tests for the Gateway-owned Confluence credential broker."""

from __future__ import annotations

from types import SimpleNamespace

import pytest
from ai_gateway_contracts.capability_proof import canonical_body_hash
from ai_gateway_core.persistence.repositories.mcp_repository import MCPAuthorizationError
from fastapi import HTTPException
from pydantic import ValidationError

from src.api.internal.confluence_capabilities import (
    ConfluenceConnectorBinding,
    ConfluenceReadEnvelope,
    ConfluenceReadRequest,
    ConfluenceWriteBinding,
    ConfluenceWriteEnvelope,
    ConfluenceWriteRequest,
    _confluence_api_root,
    _connection,
    _durable_confluence_result,
    _html_text,
    _PinnedResolver,
    _read_bounded_body,
    _search_cql,
    _storage_text,
    write_confluence,
)


def test_confluence_envelope_requires_exact_connector_identity() -> None:
    envelope = ConfluenceReadEnvelope.model_validate(
        {
            "arguments": {"action": "search", "query": "roadmap"},
            "binding": {
                "binding_type": "grant",
                "provider": "confluence",
                "tool_name": "confluence_read",
                "principal_type": "service_account",
                "grant_id": "00000000-0000-0000-0000-000000000001",
                "channel": "api",
            },
        }
    )
    assert envelope.binding.channel == "api"
    with pytest.raises(ValidationError):
        ConfluenceConnectorBinding(
            binding_type="catalog",
            provider="confluence",
            tool_name="confluence_read",
            grant_id="00000000-0000-0000-0000-000000000001",
            channel="api",
        )


def test_confluence_request_preserves_meta_tool_schema_and_rejects_credentials() -> None:
    request = ConfluenceReadRequest(action="search", query="roadmap")
    assert request.model_dump(exclude_none=True) == {
        "action": "search",
        "query": "roadmap",
    }
    with pytest.raises(ValidationError):
        ConfluenceReadRequest.model_validate(
            {"action": "search", "query": "roadmap", "api_token": "never"}
        )


def test_confluence_proof_body_preserves_omissions_and_explicit_nulls() -> None:
    read_body = {
        "arguments": {"action": "search", "query": "roadmap"},
        "binding": {
            "binding_type": "catalog",
            "provider": "confluence",
            "tool_name": "confluence_read",
            "principal_type": None,
            "grant_id": None,
            "channel": "api",
        },
    }
    read = ConfluenceReadEnvelope.model_validate(read_body)
    assert read.model_dump(mode="json", exclude_unset=True) == read_body

    arguments = {"action": "delete_page", "page_id": "42"}
    write_body = {
        "arguments": arguments,
        "arguments_hash": f"sha256:{canonical_body_hash(arguments)}",
        "binding": {
            "binding_type": "catalog",
            "provider": "confluence",
            "tool_name": "confluence_write",
            "principal_type": None,
            "grant_id": None,
            "channel": "api",
        },
    }
    write = ConfluenceWriteEnvelope.model_validate(write_body)
    assert write.model_dump(mode="json", exclude_unset=True) == write_body


def test_confluence_write_envelope_is_strict_and_has_no_secret_fields() -> None:
    arguments = {"action": "comment", "page_id": "42", "body": "LGTM"}
    envelope = ConfluenceWriteEnvelope.model_validate(
        {
            "arguments": arguments,
            "arguments_hash": f"sha256:{canonical_body_hash(arguments)}",
            "binding": {
                "binding_type": "catalog",
                "provider": "confluence",
                "tool_name": "confluence_write",
                "channel": "api",
            },
        }
    )
    assert envelope.binding.tool_name == "confluence_write"
    with pytest.raises(ValidationError):
        ConfluenceWriteBinding(
            binding_type="catalog",
            provider="confluence",
            tool_name="confluence_write",
            channel="api",
            grant_id="00000000-0000-0000-0000-000000000001",
        )
    with pytest.raises(ValidationError):
        ConfluenceWriteRequest.model_validate(
            {"action": "delete_page", "page_id": "42", "api_token": "secret"}
        )


def test_confluence_write_plain_replace_escapes_storage_markup() -> None:
    assert _storage_text("A & <B>") == "<p>A &amp; &lt;B&gt;</p>"
    assert _storage_text("<strong>ok</strong>", raw_html=True) == "<strong>ok</strong>"


def test_confluence_durable_receipt_requires_exact_capability_result() -> None:
    result = {
        "receipt_id": "00000000-0000-7000-8000-000000000001",
        "capability_id": "confluence_write",
        "external_id": "42",
        "external_url": "https://docs.example/wiki/42",
        "artifacts": [],
    }
    receipt = {
        "schema_version": "ai-platform/durable-capability-receipt/v1",
        "capability_id": "confluence_write",
        "result": result,
    }
    assert _durable_confluence_result(receipt) == result
    receipt["capability_id"] = "generate_image"
    assert _durable_confluence_result(receipt) is None


def _write_envelope(arguments: ConfluenceWriteRequest) -> ConfluenceWriteEnvelope:
    raw = arguments.model_dump(mode="json", exclude_none=True)
    return ConfluenceWriteEnvelope(
        arguments=arguments,
        arguments_hash=f"sha256:{canonical_body_hash(raw)}",
        binding=ConfluenceWriteBinding(
            binding_type="catalog",
            provider="confluence",
            tool_name="confluence_write",
            channel="api",
        ),
    )


def _patch_write_execution(monkeypatch: pytest.MonkeyPatch) -> None:
    async def state(*_args, **_kwargs):
        return None

    async def noop(*_args, **_kwargs):
        return None

    monkeypatch.setattr(
        "src.api.internal.confluence_capabilities._confluence_execution_state", state
    )
    monkeypatch.setattr(
        "src.api.internal.confluence_capabilities._claim_confluence_execution", noop
    )
    monkeypatch.setattr(
        "src.api.internal.confluence_capabilities._clear_confluence_claim", noop
    )
    monkeypatch.setattr(
        "src.api.internal.confluence_capabilities._store_confluence_receipt", noop
    )


def test_confluence_search_cql_is_single_line_and_parameterized() -> None:
    cql, strategy = _search_cql(
        ConfluenceReadRequest(action="search", query='Q4 "roadmap"', space_key="SALES")
    )
    assert strategy == "title+text"
    assert "space=SALES" in cql
    assert "\n" not in cql
    with pytest.raises(Exception, match="single-line"):
        _search_cql(ConfluenceReadRequest(action="search", cql="type=page\nOR 1=1"))


def test_confluence_html_extraction_is_bounded_and_ignores_active_content() -> None:
    value = _html_text("<h1>Roadmap</h1><script>secret()</script><p>A &amp; B</p>")
    assert value == "Roadmap\n\nA & B"
    assert "secret" not in value
    assert len(_html_text("x" * 30_000)) <= 20_032


def test_cloud_oauth_api_root_preserves_prefix_and_wiki_path() -> None:
    assert _confluence_api_root("api.atlassian.com/ex/confluence/cloud-123") == (
        "api.atlassian.com",
        "https://api.atlassian.com/ex/confluence/cloud-123/wiki/",
    )


@pytest.mark.asyncio
async def test_pinned_resolver_returns_only_validated_addresses() -> None:
    resolver = _PinnedResolver("docs.example", [("93.184.216.34", 443)])
    records = await resolver.resolve("docs.example", 443)
    assert [record["host"] for record in records] == ["93.184.216.34"]
    with pytest.raises(OSError):
        await resolver.resolve("attacker.example", 443)


@pytest.mark.asyncio
async def test_streaming_body_reader_rejects_over_limit_without_aread() -> None:
    class Content:
        async def iter_chunked(self, _size: int):
            yield b"x" * (2_000_000 + 1)

    with pytest.raises(Exception, match="response too large"):
        await _read_bounded_body(SimpleNamespace(content=Content()))


def test_confluence_rejects_controls_in_cql_dimensions() -> None:
    with pytest.raises(Exception, match="control"):
        _search_cql(ConfluenceReadRequest(action="search", query="roadmap\nsecret"))
    with pytest.raises(Exception, match="control"):
        _search_cql(ConfluenceReadRequest(action="search", author="user\radmin"))


@pytest.mark.asyncio
@pytest.mark.parametrize("database", [None, SimpleNamespace(enabled=False, _pool=None)])
async def test_connection_fails_closed_when_database_is_disabled_or_poolless(database) -> None:
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(database=database)))
    with pytest.raises(Exception, match="connection unavailable"):
        await _connection(request, "tenant-a")


@pytest.mark.asyncio
async def test_connection_rechecks_grant_and_requires_secret_resolver(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class Repository:
        def __init__(self) -> None:
            self.calls: list[dict[str, str]] = []

        async def authorize_connector_tool(self, **kwargs):
            self.calls.append(kwargs)
            return {
                "secret_ref": "vault://confluence/grant",
                "connection_metadata": {"domain": "docs.example", "email": "agent@example.com"},
            }

    class Resolver:
        async def resolve(self, secret_ref: str) -> str:
            assert secret_ref == "vault://confluence/grant"
            return "opaque-token"

    repository = Repository()
    request = SimpleNamespace(
        app=SimpleNamespace(
            state=SimpleNamespace(
                database=SimpleNamespace(enabled=True, _pool=object()),
                mcp_repository=repository,
                mcp_secret_resolver=Resolver(),
            )
        )
    )

    async def public_addresses(_domain: str) -> list[tuple[str, int]]:
        return [("93.184.216.34", 443)]

    monkeypatch.setattr(
        "src.api.internal.confluence_capabilities._public_addresses",
        public_addresses,
    )
    binding = ConfluenceConnectorBinding(
        binding_type="grant",
        provider="confluence",
        tool_name="confluence_read",
        principal_type="service_account",
        grant_id="00000000-0000-0000-0000-000000000001",
        channel="api",
    )
    domain, auth, _ = await _connection(
        request,
        "tenant-a",
        user_id="user-a",
        binding=binding,
    )
    assert domain == "docs.example"
    assert auth.startswith("Basic ")
    assert repository.calls[0]["channel"] == "api"


def _grant_binding(**overrides: str) -> ConfluenceConnectorBinding:
    values = {
        "binding_type": "grant",
        "provider": "confluence",
        "tool_name": "confluence_read",
        "principal_type": "service_account",
        "grant_id": "00000000-0000-0000-0000-000000000001",
        "channel": "api",
    }
    values.update(overrides)
    return ConfluenceConnectorBinding.model_validate(values)


def _connection_request(repository, resolver) -> SimpleNamespace:
    return SimpleNamespace(
        app=SimpleNamespace(
            state=SimpleNamespace(
                database=SimpleNamespace(enabled=True, _pool=object()),
                mcp_repository=repository,
                mcp_secret_resolver=resolver,
            )
        )
    )


@pytest.mark.asyncio
async def test_connection_valid_grant_resolves_credential_only_after_authorization(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class Repository:
        async def authorize_connector_tool(self, **kwargs):
            assert kwargs == {
                "tenant_id": "tenant-a",
                "user_id": "user-a",
                "authenticated": True,
                "provider": "confluence",
                "tool_name": "confluence_read",
                "principal_type": "service_account",
                "grant_id": "00000000-0000-0000-0000-000000000001",
                "channel": "api",
            }
            return {
                "secret_ref": "vault://confluence/grant",
                "connection_metadata": {"domain": "docs.example", "email": "agent@example.com"},
            }

    class Resolver:
        async def resolve(self, secret_ref: str) -> str:
            assert secret_ref == "vault://confluence/grant"
            return "opaque-token"

    async def public_addresses(_domain: str) -> list[tuple[str, int]]:
        return [("93.184.216.34", 443)]

    monkeypatch.setattr(
        "src.api.internal.confluence_capabilities._public_addresses", public_addresses
    )
    domain, auth, _ = await _connection(
        _connection_request(Repository(), Resolver()),
        "tenant-a",
        user_id="user-a",
        binding=_grant_binding(),
    )
    assert domain == "docs.example"
    assert auth.startswith("Basic ")


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "error",
    [
        "CONNECTOR_TENANT_DENIED",
        "CONNECTOR_USER_DENIED",
        "CONNECTOR_GRANT_DENIED",
    ],
)
async def test_connection_fails_closed_for_cross_scope_grant_denial(error: str) -> None:
    class Repository:
        async def authorize_connector_tool(self, **_kwargs):
            raise MCPAuthorizationError(error)

    with pytest.raises(Exception) as caught:
        await _connection(
            _connection_request(Repository(), object()),
            "tenant-a",
            user_id="user-a",
            binding=_grant_binding(),
        )
    assert getattr(caught.value, "detail", "") == "confluence connector authorization denied"
    assert error not in str(caught.value)


@pytest.mark.asyncio
@pytest.mark.parametrize("row", [None, {"secret_ref": "", "connection_metadata": {}}])
async def test_connection_rejects_missing_or_revoked_credential(row) -> None:
    class Repository:
        async def authorize_connector_tool(self, **_kwargs):
            if row is None:
                raise MCPAuthorizationError("CONNECTOR_CAPABILITY_UNAVAILABLE")
            return row

    with pytest.raises(Exception) as caught:
        await _connection(
            _connection_request(Repository(), object()),
            "tenant-a",
            user_id="user-a",
            binding=_grant_binding(),
        )
    detail = getattr(caught.value, "detail", "")
    assert detail in {
        "confluence connector authorization denied",
        "confluence credential unavailable",
    }


@pytest.mark.asyncio
async def test_connection_never_exposes_secret_in_resolver_error() -> None:
    secret = "super-secret-token"

    class Repository:
        async def authorize_connector_tool(self, **_kwargs):
            return {
                "secret_ref": "vault://confluence/grant",
                "connection_metadata": {"domain": "docs.example", "email": "agent@example.com"},
            }

    class Resolver:
        async def resolve(self, _secret_ref: str) -> str:
            raise RuntimeError(secret)

    with pytest.raises(Exception) as caught:
        await _connection(
            _connection_request(Repository(), Resolver()),
            "tenant-a",
            user_id="user-a",
            binding=_grant_binding(),
        )
    detail = str(getattr(caught.value, "detail", caught.value))
    assert secret not in detail


@pytest.mark.asyncio
async def test_catalog_connection_uses_tenant_user_connector_and_bearer_auth(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class Repository:
        async def authorize_connector_catalog(self, **kwargs):
            assert kwargs == {
                "tenant_id": "tenant-a",
                "user_id": "user-a",
                "authenticated": True,
                "provider": "confluence",
                "tool_name": "confluence_read",
                "channel": "api",
            }
            return {"provider": "confluence", "tool_name": "confluence_read", "user_id": "user-a"}

    async def resolve_catalog(request, *, tenant_id: str, user_id: str, provider: str):
        assert tenant_id == "tenant-a"
        assert user_id == "user-a"
        assert provider == "confluence"
        return {
            "access_token": "oauth-access-token",
            "provider_metadata": {"cloud_id": "cloud-123"},
        }

    async def public_addresses(domain: str) -> list[tuple[str, int]]:
        assert domain == "api.atlassian.com"
        return [("104.18.32.47", 443)]

    monkeypatch.setattr(
        "src.api.v1.connectors.resolve_catalog_connector_credential", resolve_catalog
    )
    monkeypatch.setattr(
        "src.api.internal.confluence_capabilities._public_addresses", public_addresses
    )
    request = _connection_request(Repository(), object())
    domain, auth, _ = await _connection(
        request,
        "tenant-a",
        user_id="user-a",
        binding=ConfluenceConnectorBinding(
            binding_type="catalog",
            provider="confluence",
            tool_name="confluence_read",
            channel="api",
        ),
    )
    assert domain == "api.atlassian.com/ex/confluence/cloud-123"
    assert auth == "Bearer oauth-access-token"


@pytest.mark.asyncio
async def test_catalog_credential_failure_does_not_disclose_oauth_token(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    secret = "oauth-secret-token"

    class Repository:
        async def authorize_connector_catalog(self, **_kwargs):
            return {"provider": "confluence", "tool_name": "confluence_read", "user_id": "user-a"}

    async def resolve_catalog(*_args, **_kwargs):
        raise RuntimeError(secret)

    monkeypatch.setattr(
        "src.api.v1.connectors.resolve_catalog_connector_credential", resolve_catalog
    )
    with pytest.raises(Exception) as caught:
        await _connection(
            _connection_request(Repository(), object()),
            "tenant-a",
            user_id="user-a",
            binding=ConfluenceConnectorBinding(
                binding_type="catalog",
                provider="confluence",
                tool_name="confluence_read",
                channel="api",
            ),
        )
    assert getattr(caught.value, "detail", "") == "confluence credential unavailable"
    assert secret not in str(caught.value)


@pytest.mark.asyncio
async def test_write_broker_uses_fixed_path_and_single_upstream_request(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _patch_write_execution(monkeypatch)
    calls: list[tuple[str, str, dict]] = []

    async def authorize(*_args, **_kwargs):
        return "tenant-a", "user-a", "session-a"

    async def connection(*_args, **_kwargs):
        return "docs.example", "Basic opaque", [("93.184.216.34", 443)]

    async def upstream(_domain, _auth, _addresses, method, path, body=None):
        calls.append((method, path, body or {}))
        return 200, {"id": "9001", "_links": {"base": "https://docs.example", "webui": "/wiki/9001"}}

    monkeypatch.setattr("src.api.internal.confluence_capabilities._authorize", authorize)
    monkeypatch.setattr("src.api.internal.confluence_capabilities._connection", connection)
    monkeypatch.setattr("src.api.internal.confluence_capabilities._upstream_write", upstream)
    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(confluence_capability_enabled=True))
    )
    payload = _write_envelope(
        ConfluenceWriteRequest(
            action="create_page", space_key="ENG", title="Roadmap", content="A & B"
        )
    )
    execution_id = "00000000-0000-7000-8000-000000000001"
    result = await write_confluence(
        request,
        payload,
        x_ai_execution_id=execution_id,
        x_ai_run_id="00000000-0000-7000-8000-000000000101",
        x_ai_tool_call_id="call-1",
    )
    assert result["receipt_id"] == execution_id
    assert result["external_id"] == "9001"
    assert result["external_url"] == "https://docs.example/wiki/9001"
    assert result["artifacts"] == []
    assert len(calls) == 1
    assert calls[0][0:2] == ("POST", "content")
    assert calls[0][2]["body"]["storage"]["value"] == "<p>A &amp; B</p>"


@pytest.mark.asyncio
async def test_write_broker_preserves_definitive_4xx_and_unknown_5xx(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _patch_write_execution(monkeypatch)
    async def authorize(*_args, **_kwargs):
        return "tenant-a", "user-a", "session-a"

    async def connection(*_args, **_kwargs):
        return "docs.example", "Basic opaque", [("93.184.216.34", 443)]

    monkeypatch.setattr("src.api.internal.confluence_capabilities._authorize", authorize)
    monkeypatch.setattr("src.api.internal.confluence_capabilities._connection", connection)
    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(confluence_capability_enabled=True))
    )
    payload = _write_envelope(ConfluenceWriteRequest(action="delete_page", page_id="42"))

    async def fourxx(*_args, **_kwargs):
        raise HTTPException(status_code=409, detail="conflict")

    monkeypatch.setattr("src.api.internal.confluence_capabilities._upstream_write", fourxx)
    with pytest.raises(Exception) as caught:
        await write_confluence(
            request,
            payload,
            x_ai_execution_id="00000000-0000-7000-8000-000000000002",
            x_ai_run_id="00000000-0000-7000-8000-000000000102",
            x_ai_tool_call_id="call-2",
        )
    assert getattr(caught.value, "status_code", None) == 409

    async def fivexx(*_args, **_kwargs):
        raise HTTPException(status_code=502, detail="unknown")

    monkeypatch.setattr("src.api.internal.confluence_capabilities._upstream_write", fivexx)
    with pytest.raises(Exception) as caught:
        await write_confluence(
            request,
            payload,
            x_ai_execution_id="00000000-0000-7000-8000-000000000003",
            x_ai_run_id="00000000-0000-7000-8000-000000000103",
            x_ai_tool_call_id="call-3",
        )
    assert getattr(caught.value, "status_code", None) == 502


@pytest.mark.asyncio
async def test_write_broker_preflights_update_before_single_side_effect(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _patch_write_execution(monkeypatch)
    calls: list[tuple[str, str, dict | None, dict | None]] = []

    async def authorize(*_args, **_kwargs):
        return "tenant-a", "user-a", "session-a"

    async def connection(*_args, **_kwargs):
        return "docs.example", "Basic opaque", [("93.184.216.34", 443)]

    async def upstream(
        _domain,
        _auth,
        _addresses,
        method,
        path,
        body=None,
        *,
        params=None,
    ):
        calls.append((method, path, body, params))
        if method == "GET":
            return 200, {
                "id": "42",
                "title": "Old",
                "version": {"number": 7},
                "space": {"key": "ENG"},
            }
        return 200, {
            "id": "42",
            "_links": {"base": "https://docs.example", "webui": "/wiki/42"},
        }

    monkeypatch.setattr("src.api.internal.confluence_capabilities._authorize", authorize)
    monkeypatch.setattr("src.api.internal.confluence_capabilities._connection", connection)
    monkeypatch.setattr("src.api.internal.confluence_capabilities._upstream_write", upstream)
    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(confluence_capability_enabled=True))
    )
    payload = _write_envelope(
        ConfluenceWriteRequest(
            action="update_page", page_id="42", title="New", content="Hello"
        )
    )

    await write_confluence(
        request,
        payload,
        x_ai_execution_id="00000000-0000-7000-8000-000000000004",
        x_ai_run_id="00000000-0000-7000-8000-000000000104",
        x_ai_tool_call_id="call-4",
    )

    assert [call[0] for call in calls] == ["GET", "PUT"]
    assert calls[1][2]["version"]["number"] == 8
    assert calls[1][2]["body"]["storage"]["value"] == "<p>Hello</p>"


@pytest.mark.asyncio
async def test_write_broker_preflight_failure_is_definitive(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _patch_write_execution(monkeypatch)
    async def authorize(*_args, **_kwargs):
        return "tenant-a", "user-a", "session-a"

    async def connection(*_args, **_kwargs):
        return "docs.example", "Basic opaque", [("93.184.216.34", 443)]

    async def upstream(*_args, **_kwargs):
        raise HTTPException(status_code=502, detail="unavailable")

    monkeypatch.setattr("src.api.internal.confluence_capabilities._authorize", authorize)
    monkeypatch.setattr("src.api.internal.confluence_capabilities._connection", connection)
    monkeypatch.setattr("src.api.internal.confluence_capabilities._upstream_write", upstream)
    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(confluence_capability_enabled=True))
    )
    payload = _write_envelope(
        ConfluenceWriteRequest(action="update_page", page_id="42", content="Hello")
    )

    with pytest.raises(HTTPException) as caught:
        await write_confluence(
            request,
            payload,
            x_ai_execution_id="00000000-0000-7000-8000-000000000005",
            x_ai_run_id="00000000-0000-7000-8000-000000000105",
            x_ai_tool_call_id="call-5",
        )

    assert caught.value.status_code == 424
