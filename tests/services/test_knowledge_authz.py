"""Unit tests for the KS-backed agent knowledge resolver (PRD T8.2).

The resolver must send the authenticated identity to knowledge-service's
internal authorization endpoint and fail closed (raise
``AgentKnowledgeAuthorizationError``) on any uncertainty: transport errors,
non-2xx, malformed responses, partial denial, or a missing internal token.
"""

from __future__ import annotations

import json
from types import SimpleNamespace
from typing import Any

import httpx
import pytest
from ai_gateway_core.auth.gateway_secret import GatewaySecret
from ai_gateway_core.comm.client import InternalServiceClient, InternalServiceClientConfig
from ai_gateway_core.persistence.repositories.agent_repository import DatabaseAgentRepository
from ai_gateway_core.persistence.repositories.agent_resource_resolver import (
    authorized_dataset_ids,
)

from src.api.v1.agent_runtime import _repository as _runtime_repository
from src.api.v1.agents import _get_repository as _authoring_repository
from src.services.knowledge_authz import (
    AUTHORIZE_DATASETS_PATH,
    AgentKnowledgeAuthorizationError,
    KnowledgeServiceAgentKnowledgeResolver,
)

TEST_SECRET = "t8-test-internal-secret-0123456789"


class RecordingResolver:
    def __init__(self, *, error: Exception | None = None) -> None:
        self.error = error
        self.calls: list[dict[str, Any]] = []

    async def resolve(self, **kwargs: Any) -> list[dict[str, Any]]:
        self.calls.append(kwargs)
        if self.error is not None:
            raise self.error
        return list(kwargs["bindings"])


class RoleConnection:
    async def fetchval(self, query: str, *args: str) -> list[str]:
        assert "FROM users" in query
        assert args == ("tenant-a", "user-a")
        return ["user", "knowledge-editor"]


@pytest.fixture(autouse=True)
def _internal_token(monkeypatch):
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", TEST_SECRET)
    monkeypatch.setenv("KB_SERVICE_URL", "http://kb.test")


def _attach_mock_transport(
    resolver: KnowledgeServiceAgentKnowledgeResolver,
    handler,
) -> KnowledgeServiceAgentKnowledgeResolver:
    resolver._service_client = InternalServiceClient(  # noqa: SLF001 - test setup
        InternalServiceClientConfig(
            name="knowledge-service",
            base_url="http://kb.test",
            gateway_secret=GatewaySecret(secret=TEST_SECRET),
        ),
        transport=httpx.MockTransport(handler),
    )
    return resolver


@pytest.mark.asyncio
async def test_resolve_allows_full_grant_and_normalizes_bindings():
    seen: dict = {}

    async def handler(request: httpx.Request) -> httpx.Response:
        seen["path"] = request.url.path
        seen["method"] = request.method
        seen["tenant"] = request.headers.get("X-Tenant-Id")
        seen["user"] = request.headers.get("X-User-Id")
        seen["roles"] = request.headers.get("X-User-Roles")
        seen["tier"] = request.headers.get("X-User-Tier")
        seen["body"] = json.loads(request.content.decode("utf-8"))
        return httpx.Response(200, json={"allowed_dataset_ids": ["kb-a", "kb-b", "kb-extra"]})

    resolver = _attach_mock_transport(KnowledgeServiceAgentKnowledgeResolver(), handler)
    bindings = [
        {"dataset_id": "kb-a", "retrieval_config": {"top_k": 5}},
        {"dataset_id": "kb-b"},
        {"dataset_id": ""},  # malformed binding rows never reach KS
        "not-a-dict",
    ]
    result = await resolver.resolve(
        tenant_id="tenant-a",
        user_id="user-a",
        bindings=bindings,
        roles=["admin"],
        is_tenant_admin=False,
        agent_id="agent-1",  # absorbed kwargs from agent_runtime call sites
        channel="preview",
        authenticated=True,
    )

    assert [binding["dataset_id"] for binding in result] == ["kb-a", "kb-b"]
    assert seen["method"] == "POST"
    assert seen["path"] == AUTHORIZE_DATASETS_PATH
    assert seen["tenant"] == "tenant-a"
    assert seen["user"] == "user-a"
    assert seen["roles"] == "admin"
    # is_tenant_admin=False must not smuggle an admin tier to KS.
    assert seen["tier"] is None
    assert seen["body"] == {"dataset_ids": ["kb-a", "kb-b"], "is_tenant_admin": False}
    await resolver.close()


@pytest.mark.asyncio
async def test_anonymous_agent_resolution_attests_reader_and_rejects_tenant_kb():
    seen: dict[str, str] = {}

    async def handler(request: httpx.Request) -> httpx.Response:
        seen["type"] = request.headers["X-User-Type"]
        seen["tier"] = request.headers["X-User-Tier"]
        seen["roles"] = request.headers["X-User-Roles"]
        seen["signature"] = request.headers["X-Gateway-Secret"]
        return httpx.Response(200, json={"allowed_dataset_ids": ["kb-public"]})

    resolver = _attach_mock_transport(KnowledgeServiceAgentKnowledgeResolver(), handler)
    with pytest.raises(AgentKnowledgeAuthorizationError):
        await resolver.resolve(
            tenant_id="tenant-a",
            user_id="agent-embed:opaque",
            bindings=[{"dataset_id": "kb-public"}, {"dataset_id": "kb-tenant"}],
            authenticated=False,
            roles=["admin"],
            is_tenant_admin=True,
        )
    assert seen["type"] == "anonymous"
    assert seen["tier"] == "anonymous"
    assert seen["roles"] == "guest"
    assert seen["signature"].startswith("v2:")
    await resolver.close()


@pytest.mark.asyncio
async def test_resolve_tenant_admin_sends_signature_bound_admin_tier():
    seen: dict = {}

    async def handler(request: httpx.Request) -> httpx.Response:
        seen["tier"] = request.headers.get("X-User-Tier")
        seen["body"] = json.loads(request.content.decode("utf-8"))
        return httpx.Response(200, json={"allowed_dataset_ids": ["kb-a"]})

    resolver = _attach_mock_transport(KnowledgeServiceAgentKnowledgeResolver(), handler)
    result = await resolver.resolve(
        tenant_id="tenant-a",
        user_id="admin-a",
        bindings=[{"dataset_id": "kb-a"}],
        is_tenant_admin=True,
    )
    assert [binding["dataset_id"] for binding in result] == ["kb-a"]
    assert seen["tier"] == "admin"
    assert seen["body"]["is_tenant_admin"] is True
    await resolver.close()


@pytest.mark.asyncio
async def test_resolve_denial_of_any_bound_dataset_fails_closed():
    async def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={"allowed_dataset_ids": ["kb-a"]})

    resolver = _attach_mock_transport(KnowledgeServiceAgentKnowledgeResolver(), handler)
    with pytest.raises(AgentKnowledgeAuthorizationError) as exc_info:
        await resolver.resolve(
            tenant_id="tenant-a",
            user_id="user-a",
            bindings=[{"dataset_id": "kb-a"}, {"dataset_id": "kb-denied"}],
        )
    assert exc_info.value.code == "AGENT_KNOWLEDGE_UNAVAILABLE"
    await resolver.close()


@pytest.mark.asyncio
async def test_resolve_http_error_fails_closed():
    async def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(500, json={"detail": "boom"})

    resolver = _attach_mock_transport(KnowledgeServiceAgentKnowledgeResolver(), handler)
    with pytest.raises(AgentKnowledgeAuthorizationError):
        await resolver.resolve(
            tenant_id="tenant-a",
            user_id="user-a",
            bindings=[{"dataset_id": "kb-a"}],
        )
    await resolver.close()


@pytest.mark.asyncio
async def test_resolve_invalid_json_fails_with_stable_authorization_error():
    async def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, content=b"not-json")

    resolver = _attach_mock_transport(KnowledgeServiceAgentKnowledgeResolver(), handler)
    with pytest.raises(AgentKnowledgeAuthorizationError):
        await resolver.resolve(
            tenant_id="tenant-a",
            user_id="user-a",
            bindings=[{"dataset_id": "kb-a"}],
        )
    await resolver.close()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "payload",
    [
        {},  # missing key
        {"allowed_dataset_ids": "kb-a"},  # not a list
        {"allowed_dataset_ids": [1, 2]},  # non-string entries
        [],  # not a dict
    ],
)
async def test_resolve_malformed_response_fails_closed(payload):
    async def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json=payload)

    resolver = _attach_mock_transport(KnowledgeServiceAgentKnowledgeResolver(), handler)
    with pytest.raises(AgentKnowledgeAuthorizationError):
        await resolver.resolve(
            tenant_id="tenant-a",
            user_id="user-a",
            bindings=[{"dataset_id": "kb-a"}],
        )
    await resolver.close()


@pytest.mark.asyncio
async def test_resolve_empty_bindings_short_circuits_without_calling_ks(monkeypatch):
    called = False

    async def handler(request: httpx.Request) -> httpx.Response:
        nonlocal called
        called = True
        return httpx.Response(200, json={"allowed_dataset_ids": []})

    resolver = _attach_mock_transport(KnowledgeServiceAgentKnowledgeResolver(), handler)
    assert await resolver.resolve(
        tenant_id="tenant-a", user_id="user-a", bindings=[]
    ) == []
    assert not called

    # Even a missing token cannot fail an agent that has nothing to authorize.
    monkeypatch.delenv("AI_PLATFORM_INTERNAL_TOKEN")
    resolver2 = KnowledgeServiceAgentKnowledgeResolver()
    assert await resolver2.resolve(tenant_id="t", user_id="u", bindings=[{"dataset_id": ""}]) == []


@pytest.mark.asyncio
async def test_resolve_without_internal_token_fails_closed(monkeypatch):
    async def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={"allowed_dataset_ids": ["kb-a"]})

    monkeypatch.delenv("AI_PLATFORM_INTERNAL_TOKEN")
    resolver = _attach_mock_transport(KnowledgeServiceAgentKnowledgeResolver(), handler)
    with pytest.raises(AgentKnowledgeAuthorizationError):
        await resolver.resolve(
            tenant_id="tenant-a",
            user_id="user-a",
            bindings=[{"dataset_id": "kb-a"}],
        )


@pytest.mark.asyncio
async def test_resolve_sends_gateway_secret_signature_header():
    seen: dict = {}

    async def handler(request: httpx.Request) -> httpx.Response:
        seen["secret"] = request.headers.get("X-Gateway-Secret") or ""
        return httpx.Response(200, json={"allowed_dataset_ids": ["kb-a"]})

    resolver = _attach_mock_transport(KnowledgeServiceAgentKnowledgeResolver(), handler)
    await resolver.resolve(
        tenant_id="tenant-a",
        user_id="user-a",
        bindings=[{"dataset_id": "kb-a"}],
    )

    parts = seen["secret"].split(":")
    assert parts[0] == "v2", "identity must be attested over the HMAC v2 channel"
    assert len(parts) == 6  # v2:key_id:rid:ts:body_hash:hmac_hex
    await resolver.close()


@pytest.mark.asyncio
async def test_repository_authoring_uses_shared_resolver_and_signed_actor_roles():
    resolver = RecordingResolver()
    repository = DatabaseAgentRepository(
        SimpleNamespace(enabled=True, _pool=object()),
        knowledge_resolver=resolver,
    )

    allowed = await repository._authorized_knowledge_dataset_ids(  # noqa: SLF001
        RoleConnection(),
        tenant_id="tenant-a",
        user_id="user-a",
        dataset_ids=["kb-a", "kb-b"],
        is_tenant_admin=False,
    )

    assert allowed == {"kb-a", "kb-b"}
    assert resolver.calls[0]["roles"] == ["user", "knowledge-editor"]
    assert resolver.calls[0]["channel"] == "authoring"


@pytest.mark.asyncio
async def test_authoring_adapter_fails_closed_without_or_on_resolver_failure():
    kwargs = {
        "tenant_id": "tenant-a",
        "user_id": "user-a",
        "dataset_ids": ["kb-a"],
        "is_tenant_admin": False,
    }
    assert await authorized_dataset_ids(None, **kwargs) == set()
    assert await authorized_dataset_ids(
        RecordingResolver(error=RuntimeError("KS unavailable")),
        **kwargs,
    ) == set()


def test_authoring_and_runtime_repository_share_the_ks_resolver():
    resolver = RecordingResolver()
    state = SimpleNamespace(
        database=SimpleNamespace(enabled=True, _pool=object()),
        agent_runtime_knowledge_resolver=resolver,
    )
    request = SimpleNamespace(app=SimpleNamespace(state=state))

    authoring = _authoring_repository(request)
    runtime = _runtime_repository(request)

    assert authoring is runtime
    assert authoring._knowledge_resolver is resolver  # noqa: SLF001


@pytest.mark.asyncio
async def test_resolver_import_replaces_deleted_sql_resolver():
    # The Dataset ACL moved out of the gateway: the old SQL-backed resolver
    # must not be re-exported from the core repositories module.
    import ai_gateway_core.persistence.repositories.agent_resource_resolver as arr

    assert not hasattr(arr, "DatabaseAgentKnowledgeResolver")
    assert not hasattr(arr, "AgentKnowledgeAuthorizationError")
    assert hasattr(arr, "authorized_dataset_ids")
