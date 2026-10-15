"""Gateway Dataset deletion preflight: KS authority and Agent ACL projection."""

from __future__ import annotations

import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from ai_gateway_core.auth.gateway_secret import GatewaySecret
from ai_gateway_core.comm.client import InternalServiceClient, InternalServiceClientConfig
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentRepositoryError,
    DatabaseAgentRepository,
)
from fastapi import HTTPException

from src.api.v1 import knowledge
from src.core.auth.user_resolver import UserContext
from src.services.knowledge_authz import (
    DatasetImpactAuthorityError,
    KnowledgeServiceAgentKnowledgeResolver,
)

TEST_SECRET = "dataset-impact-test-secret-0123456789"


def _user() -> UserContext:
    return UserContext(
        user_id="user-a", tenant_id="tenant-a", is_authenticated=True, roles=["user"]
    )


def _request(*, scopes: list[str], authority: object, repository: object) -> SimpleNamespace:
    return SimpleNamespace(
        method="GET",
        state=SimpleNamespace(api_key_info={"scopes": scopes}),
        app=SimpleNamespace(
            state=SimpleNamespace(
                agent_runtime_knowledge_resolver=authority,
                agent_repository=repository,
            )
        ),
    )


def _attach_transport(resolver: KnowledgeServiceAgentKnowledgeResolver, handler) -> None:
    resolver._service_client = InternalServiceClient(  # noqa: SLF001 - test transport
        InternalServiceClientConfig(
            name="knowledge-service",
            base_url="http://kb.test",
            gateway_secret=GatewaySecret(
                secret=TEST_SECRET,
                caller_service="gateway",
                audience="knowledge-service",
                allowed_path_prefixes=("/api/v1",),
            ),
        ),
        transport=httpx.MockTransport(handler),
    )


@pytest.mark.asyncio
async def test_dataset_owner_check_uses_signed_ks_detail(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", TEST_SECRET)
    seen: dict[str, str | None] = {}

    async def handler(request: httpx.Request) -> httpx.Response:
        GatewaySecret(
            secret=TEST_SECRET,
            caller_service="gateway",
            audience="knowledge-service",
            allowed_path_prefixes=("/api/v1",),
        ).verify(
            request.headers.get("X-Gateway-Secret"),
            method="GET",
            path=request.url.path,
            identity_headers=request.headers,
        )
        seen.update(
            method=request.method,
            path=request.url.path,
            tenant=request.headers.get("X-Tenant-Id"),
            user=request.headers.get("X-User-Id"),
            roles=request.headers.get("X-User-Roles"),
            tier=request.headers.get("X-User-Tier"),
            signature=request.headers.get("X-Gateway-Secret"),
        )
        return httpx.Response(
            200,
            json={
                "dataset_id": "dataset-a",
                "tenant_id": "tenant-a",
                "my_permission": "owner",
            },
        )

    resolver = KnowledgeServiceAgentKnowledgeResolver()
    _attach_transport(resolver, handler)
    await resolver.require_dataset_owner(
        dataset_id="dataset-a",
        tenant_id="tenant-a",
        user_id="user-a",
        is_tenant_admin=True,
        roles=["admin"],
    )

    assert seen["method"] == "GET"
    assert seen["path"] == "/api/v1/knowledge/datasets/dataset-a"
    assert seen["tenant"] == "tenant-a"
    assert seen["user"] == "user-a"
    assert seen["roles"] == "admin"
    assert seen["tier"] == "admin"
    assert seen["signature"]
    await resolver.close()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("payload", "status"),
    [
        ({"dataset_id": "dataset-a", "tenant_id": "tenant-a", "my_permission": "viewer"}, 403),
        ({"dataset_id": "dataset-a", "tenant_id": "other", "my_permission": "owner"}, 403),
        ({"dataset_id": "other", "tenant_id": "tenant-a", "my_permission": "owner"}, 503),
        ({"dataset_id": "dataset-a", "tenant_id": "tenant-a"}, 403),
        ([], 503),
    ],
)
async def test_dataset_owner_check_fails_closed_on_inconsistent_detail(
    monkeypatch, payload, status
) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", TEST_SECRET)
    resolver = KnowledgeServiceAgentKnowledgeResolver()
    _attach_transport(resolver, lambda _request: httpx.Response(200, json=payload))

    with pytest.raises(DatasetImpactAuthorityError) as exc_info:
        await resolver.require_dataset_owner(
            dataset_id="dataset-a", tenant_id="tenant-a", user_id="user-a"
        )
    assert exc_info.value.status_code == status
    await resolver.close()


@pytest.mark.asyncio
@pytest.mark.parametrize(("upstream_status", "expected_status"), [(403, 403), (404, 404), (500, 503)])
async def test_dataset_owner_check_maps_ks_failure(monkeypatch, upstream_status, expected_status) -> None:
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", TEST_SECRET)
    resolver = KnowledgeServiceAgentKnowledgeResolver()
    _attach_transport(resolver, lambda _request: httpx.Response(upstream_status))

    with pytest.raises(DatasetImpactAuthorityError) as exc_info:
        await resolver.require_dataset_owner(
            dataset_id="dataset-a", tenant_id="tenant-a", user_id="user-a"
        )
    assert exc_info.value.status_code == expected_status
    await resolver.close()


@pytest.mark.asyncio
async def test_dataset_owner_check_requires_internal_token(monkeypatch) -> None:
    monkeypatch.delenv("AI_PLATFORM_INTERNAL_TOKEN", raising=False)
    resolver = KnowledgeServiceAgentKnowledgeResolver()
    with pytest.raises(DatasetImpactAuthorityError) as exc_info:
        await resolver.require_dataset_owner(
            dataset_id="dataset-a", tenant_id="tenant-a", user_id="user-a"
        )
    assert exc_info.value.status_code == 503


@pytest.mark.asyncio
async def test_impact_route_checks_scope_and_owner_before_repository() -> None:
    authority = SimpleNamespace(require_dataset_owner=AsyncMock())
    impact = {"dataset_id": "dataset-a", "visible_agents": [], "hidden_agent_count": 0}
    repository = SimpleNamespace(get_dataset_impact=AsyncMock(return_value=impact))
    request = _request(scopes=["knowledge:read"], authority=authority, repository=repository)

    assert await knowledge.get_dataset_impact(
        "dataset-a", request, user=_user(), rate_limiter=None
    ) == impact
    authority.require_dataset_owner.assert_awaited_once()
    repository.get_dataset_impact.assert_awaited_once_with(
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        user_id="user-a",
        is_tenant_admin=False,
    )

    authority.require_dataset_owner.reset_mock()
    repository.get_dataset_impact.reset_mock()
    request = _request(scopes=["knowledge:write"], authority=authority, repository=repository)
    with pytest.raises(HTTPException) as exc_info:
        await knowledge.get_dataset_impact("dataset-a", request, user=_user(), rate_limiter=None)
    assert exc_info.value.status_code == 403
    authority.require_dataset_owner.assert_not_awaited()
    repository.get_dataset_impact.assert_not_awaited()


@pytest.mark.asyncio
async def test_impact_route_never_reports_zero_when_authority_or_storage_fails() -> None:
    authority = SimpleNamespace(
        require_dataset_owner=AsyncMock(side_effect=DatasetImpactAuthorityError(503))
    )
    repository = SimpleNamespace(get_dataset_impact=AsyncMock(return_value={}))
    request = _request(scopes=["knowledge:read"], authority=authority, repository=repository)
    with pytest.raises(HTTPException) as exc_info:
        await knowledge.get_dataset_impact("dataset-a", request, user=_user(), rate_limiter=None)
    assert exc_info.value.status_code == 503
    repository.get_dataset_impact.assert_not_awaited()

    authority.require_dataset_owner.side_effect = None
    repository.get_dataset_impact.side_effect = RuntimeError("storage outage")
    with pytest.raises(HTTPException) as exc_info:
        await knowledge.get_dataset_impact("dataset-a", request, user=_user(), rate_limiter=None)
    assert exc_info.value.status_code == 503


def test_impact_route_precedes_catch_all_proxy() -> None:
    assert knowledge.router.routes[0].path == "/knowledge/datasets/{dataset_id}/impact"


@pytest.mark.asyncio
async def test_repository_projects_visible_names_and_hidden_counts(monkeypatch) -> None:
    repository = DatabaseAgentRepository(SimpleNamespace(_pool=object(), enabled=True))
    rows = [
        {
            "agent_id": "visible-a",
            "name": "Visible Agent",
            "visible": True,
            "current_draft": True,
            "active_publication": True,
            "historical_version_count": 2,
        },
        {
            "agent_id": "hidden-a",
            "name": "Secret Agent",
            "visible": False,
            "current_draft": True,
            "active_publication": False,
            "historical_version_count": 1,
        },
        {
            "agent_id": "hidden-b",
            "name": "Another Secret Agent",
            "visible": False,
            "current_draft": False,
            "active_publication": True,
            "historical_version_count": 0,
        },
    ]
    seen: dict[str, object] = {}

    async def fetch(query: str, *args):
        seen["query"] = query
        seen["args"] = args
        return rows

    monkeypatch.setattr(repository, "fetch", fetch)
    result = await repository.get_dataset_impact(
        tenant_id="tenant-a", dataset_id="dataset-a", user_id="user-a", is_tenant_admin=False
    )

    assert seen["args"] == ("tenant-a", "dataset-a", "user-a", False)
    query = str(seen["query"])
    assert "agent_draft_knowledge_bindings" in query
    assert "agent_version_knowledge_bindings" in query
    assert "p.status = 'active'" in query
    assert "a.deleted_at IS NULL" in query
    assert "current_draft.draft_id IS NOT NULL" in query
    assert "self_member.principal_id = $3" in query
    assert result == {
        "dataset_id": "dataset-a",
        "visible_agents": [
            {
                "agent_id": "visible-a",
                "name": "Visible Agent",
                "current_draft": True,
                "active_publication": True,
                "historical_version_count": 2,
            }
        ],
        "hidden_agent_count": 2,
        "counts": {
            "current_draft": {"visible": 1, "hidden": 1},
            "active_publication": {"visible": 1, "hidden": 1},
            "historical_version": {"visible": 1, "hidden": 1},
        },
    }
    assert "Secret Agent" not in json.dumps(result)


@pytest.mark.asyncio
async def test_repository_storage_unavailable_is_an_error() -> None:
    repository = DatabaseAgentRepository(SimpleNamespace(_pool=None, enabled=False))
    with pytest.raises(AgentRepositoryError, match="AGENT_STORAGE_UNAVAILABLE"):
        await repository.get_dataset_impact(
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            user_id="user-a",
            is_tenant_admin=False,
        )
