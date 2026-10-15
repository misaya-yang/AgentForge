from __future__ import annotations

from datetime import date
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException

from src.api.deps import AuthContext
from src.api.v1 import quota as quota_routes
from src.api.v1 import usage as usage_routes
from src.api.v1.models import list_models
from src.api.v1.providers import list_providers
from src.api.v1.proxy import _enforce_model_allowlist, check_service_authorization
from src.config.settings import Settings
from src.core.auth.rbac import RBAC
from src.core.auth.user_resolver import UserContext


def _request() -> SimpleNamespace:
    settings = Settings()
    request = SimpleNamespace()
    request.app = SimpleNamespace()
    request.app.state = SimpleNamespace()
    request.app.state.dispatcher = SimpleNamespace(rbac=RBAC(role_permissions=settings.rbac.roles))
    request.app.state.registry = SimpleNamespace(
        get=AsyncMock(return_value=None),
        list=AsyncMock(return_value=[]),
    )
    request.app.state.database = SimpleNamespace(enabled=False)
    request.app.state.settings = settings
    request.state = SimpleNamespace(request_id="req-tenant-isolation", api_key_info=None)
    request.headers = {}
    return request


def _auth(
    *,
    user_id: str = "user-a",
    tenant_id: str = "tenant-a",
    roles: list[str] | None = None,
    permissions: list[str] | None = None,
) -> AuthContext:
    return AuthContext(
        user_id=user_id,
        tenant_id=tenant_id,
        roles=roles or ["user"],
        permissions=permissions or [],
        is_authenticated=True,
    )


def _user(user_id: str = "user-a", tenant_id: str = "tenant-a") -> UserContext:
    return UserContext(
        user_id=user_id,
        tenant_id=tenant_id,
        tier="normal",
        is_authenticated=True,
        roles=["user"],
        ip="127.0.0.1",
    )


@pytest.mark.asyncio
async def test_non_admin_usage_query_is_forced_to_authenticated_user(monkeypatch) -> None:
    observed: dict[str, object] = {}

    async def fake_summary(**kwargs):
        observed.update(kwargs)
        return {
            "total_requests": 0,
            "success_rate": 100.0,
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "total_tokens": 0,
            "total_cost_usd": 0.0,
            "avg_latency_ms": 0,
            "start_date": "2026-05-01",
            "end_date": "2026-05-26",
        }

    recorder = SimpleNamespace(
        get_usage_summary=AsyncMock(side_effect=fake_summary),
        get_last_ingested_at=AsyncMock(return_value=None),
    )
    monkeypatch.setattr(usage_routes, "get_usage_recorder", lambda: recorder)

    await usage_routes.get_usage_summary(
        request=_request(),
        start_date=date(2026, 5, 1),
        end_date=date(2026, 5, 26),
        user_id="user-b",
        auth=_auth(permissions=["console:usage:view"]),
    )

    assert observed["tenant_id"] == "tenant-a"
    assert observed["user_id"] == "user-a"


@pytest.mark.asyncio
async def test_admin_usage_query_keeps_explicit_user_with_tenant_scope(monkeypatch) -> None:
    observed: dict[str, object] = {}

    async def fake_summary(**kwargs):
        observed.update(kwargs)
        return {
            "total_requests": 0,
            "success_rate": 100.0,
            "total_input_tokens": 0,
            "total_output_tokens": 0,
            "total_tokens": 0,
            "total_cost_usd": 0.0,
            "avg_latency_ms": 0,
            "start_date": "2026-05-01",
            "end_date": "2026-05-26",
        }

    recorder = SimpleNamespace(
        get_usage_summary=AsyncMock(side_effect=fake_summary),
        get_last_ingested_at=AsyncMock(return_value=None),
    )
    monkeypatch.setattr(usage_routes, "get_usage_recorder", lambda: recorder)

    await usage_routes.get_usage_summary(
        request=_request(),
        start_date=date(2026, 5, 1),
        end_date=date(2026, 5, 26),
        user_id="user-b",
        auth=_auth(roles=["admin"], permissions=["console:usage:view"]),
    )

    assert observed["tenant_id"] == "tenant-a"
    assert observed["user_id"] == "user-b"


@pytest.mark.asyncio
async def test_usage_query_without_tenant_scope_is_rejected(monkeypatch) -> None:
    recorder = SimpleNamespace(
        get_usage_summary=AsyncMock(
            return_value={
                "total_requests": 0,
                "success_rate": 100.0,
                "total_input_tokens": 0,
                "total_output_tokens": 0,
                "total_tokens": 0,
                "total_cost_usd": 0.0,
                "avg_latency_ms": 0,
                "start_date": "2026-05-01",
                "end_date": "2026-05-26",
            }
        ),
        get_last_ingested_at=AsyncMock(return_value=None),
    )
    monkeypatch.setattr(usage_routes, "get_usage_recorder", lambda: recorder)

    with pytest.raises(HTTPException) as exc:
        await usage_routes.get_usage_summary(
            request=_request(),
            start_date=date(2026, 5, 1),
            end_date=date(2026, 5, 26),
            auth=_auth(tenant_id="", roles=["admin"], permissions=["console:usage:view"]),
        )

    assert exc.value.status_code == 403
    assert "tenant" in str(exc.value.detail).lower()


@pytest.mark.asyncio
async def test_non_admin_cannot_read_another_users_quota(monkeypatch) -> None:
    quota_service = SimpleNamespace(get_user_quota=AsyncMock(return_value=None))
    monkeypatch.setattr(quota_routes, "get_quota_service", lambda: quota_service)

    with pytest.raises(HTTPException) as exc:
        await quota_routes.get_user_quota(
            user_id="user-b",
            request=_request(),
            auth=_auth(permissions=["console:quota:view"]),
        )

    assert exc.value.status_code == 403
    assert exc.value.detail["required_capability"] == "GatewayQuotaRead"
    quota_service.get_user_quota.assert_not_awaited()


@pytest.mark.asyncio
async def test_api_key_allowed_services_rejects_service_alias_outside_allowlist() -> None:
    request = _request()
    request.state.api_key_info = {"allowed_services": ["agent"]}

    with pytest.raises(HTTPException) as exc:
        await check_service_authorization(
            request=request,
            service_name="assistant",
            user=_user(),
            auth=_auth(permissions=["conversation:playground:access"]),
        )

    assert exc.value.status_code == 403
    assert "not in allowed services" in str(exc.value.detail)


@pytest.mark.asyncio
async def test_api_key_allowed_models_rejects_model_outside_allowlist() -> None:
    request = _request()
    request.state.api_key_info = {"allowed_models": ["gemini-3-flash-preview"]}

    with pytest.raises(HTTPException) as exc:
        await _enforce_model_allowlist(
            request=request,
            service_name="agent",
            user=_user(),
            auth=_auth(),
            model="qwen-plus",
        )

    assert exc.value.status_code == 403
    assert "not allowed" in str(exc.value.detail)


@pytest.mark.asyncio
async def test_provider_and_model_lists_use_authenticated_tenant() -> None:
    provider_service = SimpleNamespace(list_providers=AsyncMock(return_value=[]))
    model_service = SimpleNamespace(list_models=AsyncMock(return_value=[]))
    user = _user(tenant_id="tenant-a")
    user.roles = ["admin"]

    await list_providers(provider_service=provider_service, user=user)
    await list_models(model_service=model_service, user=user)

    provider_service.list_providers.assert_awaited_once()
    model_service.list_models.assert_awaited_once()
    assert provider_service.list_providers.await_args.kwargs["tenant_id"] == "tenant-a"
    assert model_service.list_models.await_args.kwargs["tenant_id"] == "tenant-a"
