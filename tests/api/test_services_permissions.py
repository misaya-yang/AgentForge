from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import HTTPException

from src.api.deps import AuthContext
from src.api.v1 import services as services_api
from src.api.v1.services import list_services, register_service
from src.config.settings import Settings
from src.core.auth.rbac import RBAC
from src.core.auth.user_resolver import UserContext


def _make_request(request_id: str = "req-services-001") -> SimpleNamespace:
    settings = Settings()
    request = SimpleNamespace()
    request.app = SimpleNamespace()
    request.app.state = SimpleNamespace()
    request.app.state.dispatcher = SimpleNamespace(rbac=RBAC(role_permissions=settings.rbac.roles))
    request.state = SimpleNamespace(request_id=request_id)
    return request


def test_services_router_contract_stays_on_facade() -> None:
    actual = {
        (method, route.path): route.endpoint
        for route in services_api.router.routes
        for method in route.methods or set()
    }
    assert actual == {
        ("POST", "/services"): services_api.register_service,
        ("GET", "/services"): services_api.list_services,
        ("GET", "/services/{service_id}"): services_api.get_service,
        ("PUT", "/services/{service_id}"): services_api.update_service,
        ("DELETE", "/services/{service_id}"): services_api.delete_service,
        ("GET", "/services/{service_id}/schema"): services_api.get_service_schema,
    }


@pytest.mark.asyncio
async def test_list_services_requires_service_list_read_capability() -> None:
    request = _make_request()
    registry = SimpleNamespace(list=AsyncMock(return_value=[]))

    with pytest.raises(HTTPException) as exc:
        await list_services(
            request=request,
            service_type=None,
            tags=None,
            registry=registry,
            auth=AuthContext(user_id="u1", tenant_id="t1", roles=["user"], permissions=[]),
            user=UserContext(
                user_id="u1",
                tenant_id="t1",
                tier="normal",
                is_authenticated=True,
                roles=["user"],
                ip="127.0.0.1",
            ),
        )

    assert exc.value.status_code == 403
    assert exc.value.detail["required_capability"] == "ServiceListRead"
    assert exc.value.detail["required_permission"] == "console:services:view"


@pytest.mark.asyncio
async def test_list_services_accepts_manager_role() -> None:
    request = _make_request()
    registry = SimpleNamespace(list=AsyncMock(return_value=[]))

    result = await list_services(
        request=request,
        service_type=None,
        tags=None,
        registry=registry,
        auth=AuthContext(user_id="manager_1", tenant_id="t1", roles=["manager"], permissions=[]),
        user=UserContext(
            user_id="manager_1",
            tenant_id="t1",
            tier="normal",
            is_authenticated=True,
            roles=["manager"],
            ip="127.0.0.1",
        ),
    )

    assert isinstance(result, list)
    assert any(item.get("service_id") == "assistant" for item in result)


@pytest.mark.asyncio
async def test_list_services_uses_runtime_and_worker_health() -> None:
    request = _make_request()
    registry = SimpleNamespace(list=AsyncMock(return_value=[]))
    request.app.state.agent_runtime_control = object()
    request.app.state.image_task_worker = SimpleNamespace(
        _loop_task=SimpleNamespace(done=lambda: False)
    )

    result = await list_services(
        request=request,
        service_type=None,
        tags=None,
        registry=registry,
        auth=AuthContext(user_id="manager_1", tenant_id="t1", roles=["manager"], permissions=[]),
        user=UserContext(
            user_id="manager_1",
            tenant_id="t1",
            tier="normal",
            is_authenticated=True,
            roles=["manager"],
            ip="127.0.0.1",
        ),
    )

    assistant = next(item for item in result if item.get("service_id") == "assistant")
    assert assistant["status"] == "active"


@pytest.mark.asyncio
async def test_register_service_rejects_tenant_service_manage_alias() -> None:
    request = _make_request()
    registry = MagicMock()
    registry._service_from_dict.return_value = SimpleNamespace(service_id="svc_legacy")
    registry.register = AsyncMock()

    with pytest.raises(HTTPException) as exc:
        await register_service(
            request=request,
            definition={"service_id": "svc_legacy", "name": "Legacy Service"},
            registry=registry,
            auth=AuthContext(
                user_id="dev_1",
                tenant_id="t1",
                roles=["service:manage"],
                permissions=[],
            ),
        )

    assert exc.value.status_code == 403
    registry.register.assert_not_awaited()
