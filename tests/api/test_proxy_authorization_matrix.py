from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException

from src.api.deps import AuthContext
from src.api.v1.proxy import check_service_authorization
from src.config.settings import Settings
from src.core.auth.rbac import RBAC
from src.core.auth.user_resolver import UserContext


def _make_request() -> SimpleNamespace:
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
    request.state = SimpleNamespace(request_id="req-test-001", api_key_info=None)
    request.headers = {}
    return request


def _make_user(role: str) -> UserContext:
    return UserContext(
        user_id=f"user_{role}",
        tenant_id="tenant_test",
        tier="normal",
        is_authenticated=role != "guest",
        roles=[role] if role != "guest" else [],
        ip="127.0.0.1",
    )


def _make_auth(role: str) -> AuthContext:
    if role == "guest":
        return AuthContext(user_id="", tenant_id="tenant_test", roles=["guest"], permissions=[])
    return AuthContext(
        user_id=f"user_{role}",
        tenant_id="tenant_test",
        roles=[role],
        permissions=[],
    )


def _enable_db_constraints(
    request: SimpleNamespace,
    *,
    tenant_allowed: list[str] | None = None,
    user_policy: dict | None = None,
) -> None:
    tenant_payload = {"allowed_services": tenant_allowed or []}
    user_metadata = {"service_access": user_policy or {}}
    request.app.state.database = SimpleNamespace(
        enabled=True,
        get_tenant=AsyncMock(return_value=tenant_payload),
        get_user=AsyncMock(return_value={"metadata": user_metadata}),
    )


@pytest.mark.asyncio
@pytest.mark.parametrize("role", ["user", "manager", "cs_staff", "sales_staff", "admin"])
async def test_proxy_authorization_accepts_expected_roles(role: str) -> None:
    request = _make_request()
    await check_service_authorization(
        request=request,
        service_name="agent",
        user=_make_user(role),
        auth=_make_auth(role),
    )


@pytest.mark.asyncio
async def test_proxy_authorization_rejects_guest_without_capability() -> None:
    request = _make_request()
    with pytest.raises(HTTPException) as exc:
        await check_service_authorization(
            request=request,
            service_name="agent",
            user=_make_user("guest"),
            auth=_make_auth("guest"),
        )
    assert exc.value.status_code == 403
    assert exc.value.detail["required_capability"] == "AgentInvoke"
    assert exc.value.detail["required_permission"] == "conversation:playground:access"


@pytest.mark.asyncio
async def test_proxy_authorization_respects_user_allowlist_policy() -> None:
    request = _make_request()
    _enable_db_constraints(
        request,
        user_policy={"mode": "allowlist", "allowed_services": ["flash"]},
    )
    with pytest.raises(HTTPException) as exc:
        await check_service_authorization(
            request=request,
            service_name="agent",
            user=_make_user("user"),
            auth=_make_auth("user"),
        )
    assert exc.value.status_code == 403
    assert "blocked by user policy" in str(exc.value.detail)


@pytest.mark.asyncio
async def test_proxy_authorization_user_allowlist_allows_matched_service() -> None:
    request = _make_request()
    _enable_db_constraints(
        request,
        user_policy={"mode": "allowlist", "allowed_services": ["agent"]},
    )
    await check_service_authorization(
        request=request,
        service_name="agent",
        user=_make_user("user"),
        auth=_make_auth("user"),
    )


@pytest.mark.asyncio
async def test_proxy_authorization_user_denylist_takes_precedence() -> None:
    request = _make_request()
    _enable_db_constraints(
        request,
        user_policy={
            "mode": "all",
            "denied_services": ["agent"],
        },
    )
    with pytest.raises(HTTPException) as exc:
        await check_service_authorization(
            request=request,
            service_name="agent",
            user=_make_user("user"),
            auth=_make_auth("user"),
        )
    assert exc.value.status_code == 403
    assert "blocked by user policy" in str(exc.value.detail)


@pytest.mark.asyncio
async def test_proxy_authorization_respects_api_key_allowed_services() -> None:
    request = _make_request()
    _enable_db_constraints(request)
    request.state.api_key_info = {"allowed_services": ["flash"]}

    with pytest.raises(HTTPException) as exc:
        await check_service_authorization(
            request=request,
            service_name="agent",
            user=_make_user("user"),
            auth=_make_auth("user"),
        )
    assert exc.value.status_code == 403
    assert "not in allowed services" in str(exc.value.detail)
