from __future__ import annotations

import contextlib
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import FastAPI, HTTPException
from fastapi.testclient import TestClient

from src.api.deps import AuthContext, get_auth_context, get_user_context
from src.api.v1 import dashboard as dashboard_routes
from src.api.v1 import mcp as mcp_routes
from src.api.v1 import providers as provider_routes
from src.api.v1 import skills as skill_routes
from src.core.auth.user_resolver import UserContext


def _auth(*permissions: str) -> AuthContext:
    return AuthContext(
        user_id="user-security",
        tenant_id="tenant-security",
        roles=["user"],
        permissions=list(permissions),
        is_authenticated=True,
    )


def _user(*permissions: str) -> UserContext:
    return UserContext(
        user_id="user-security",
        tenant_id="tenant-security",
        tier="normal",
        is_authenticated=True,
        roles=["user", *permissions],
        ip="127.0.0.1",
    )


def _auth_without_metrics() -> AuthContext:
    return AuthContext(
        user_id="user-security",
        tenant_id="tenant-security",
        roles=["no_dashboard"],
        permissions=[],
        is_authenticated=True,
    )


def _request() -> SimpleNamespace:
    request = SimpleNamespace()
    request.app = SimpleNamespace(state=SimpleNamespace())
    request.state = SimpleNamespace(request_id="req-security-authz")
    request.headers = {}
    request.client = SimpleNamespace(host="127.0.0.1")
    return request


def _client(
    router, *, auth: AuthContext | None = None, user: UserContext | None = None
) -> TestClient:
    app = FastAPI()
    app.include_router(router)
    app.dependency_overrides[get_auth_context] = lambda: auth or _auth()
    app.dependency_overrides[get_user_context] = lambda: user or _user()
    app.state.database = None
    app.state.mcp_manager = SimpleNamespace(get_servers_status=lambda: [])
    app.state.tool_registry = SimpleNamespace(list_tools=lambda: [])
    app.state.provider_service = SimpleNamespace(list_providers=AsyncMock(return_value=[]))
    return TestClient(app)


def test_skills_list_rejects_user_without_skill_read_permission() -> None:
    response = _client(skill_routes.router).get("/skills")

    assert response.status_code == 403


def test_skills_upload_rejects_user_without_skill_write_permission() -> None:
    skill_md = b"---\nname: secure-skill\ndescription: Test skill\n---\n# Secure Skill\n"

    response = _client(skill_routes.router).post(
        "/skills/upload",
        files={"file": ("SKILL.md", skill_md, "text/markdown")},
    )

    assert response.status_code == 403


def test_mcp_inventory_rejects_user_without_mcp_read_permission() -> None:
    client = _client(mcp_routes.legacy_router)

    assert client.get("/assistant/mcp/servers").status_code == 403
    assert client.get("/assistant/mcp/tools").status_code == 403


def test_provider_inventory_rejects_user_without_provider_read_permission() -> None:
    response = _client(provider_routes.router).get("/providers")

    assert response.status_code == 403


def test_provider_templates_rejects_user_without_provider_read_permission() -> None:
    response = _client(provider_routes.router).get("/provider-templates")

    assert response.status_code == 403


def test_provider_get_rejects_user_without_provider_read_permission() -> None:
    client = _client(provider_routes.router)
    client.app.state.provider_service = SimpleNamespace(
        get_provider=AsyncMock(return_value=None)
    )

    response = client.get("/providers/prov-security")

    assert response.status_code == 403


def test_skills_get_rejects_user_without_skill_read_permission() -> None:
    response = _client(skill_routes.router).get("/skills/secure-skill")

    assert response.status_code == 403


def test_dashboard_timeseries_rejects_user_without_metrics_permission() -> None:
    response = _client(
        dashboard_routes.router,
        auth=_auth_without_metrics(),
    ).get("/dashboard/timeseries/requests")

    assert response.status_code == 403


def test_dashboard_summary_rejects_user_without_metrics_permission() -> None:
    response = _client(
        dashboard_routes.router,
        auth=_auth_without_metrics(),
    ).get("/dashboard/summary")

    assert response.status_code == 403


@pytest.mark.asyncio
async def test_dashboard_websocket_accepts_metrics_read_permission(monkeypatch) -> None:
    from src.config.settings import Settings

    websocket = AsyncMock()
    websocket.app = SimpleNamespace(state=SimpleNamespace(settings=Settings()))
    websocket.close = AsyncMock()
    manager = AsyncMock()
    manager.connect = AsyncMock()
    manager.disconnect = AsyncMock()
    monkeypatch.setattr(dashboard_routes, "manager", manager)

    async def _authenticated(_websocket):
        return AuthContext(
            user_id="platform-admin",
            tenant_id="platform",
            roles=["platform_admin"],
            permissions=["console:metrics:view"],
            is_authenticated=True,
        )

    monkeypatch.setattr(dashboard_routes, "authenticate_websocket", _authenticated)
    monkeypatch.setattr(
        dashboard_routes,
        "get_realtime_metrics",
        lambda: SimpleNamespace(
            get_realtime_snapshot=AsyncMock(
                return_value=SimpleNamespace(to_dict=lambda: {"rps": 0.0})
            )
        ),
    )
    monkeypatch.setattr(dashboard_routes, "_check_alerts", lambda _snapshot: [])

    websocket.receive_text = AsyncMock(side_effect=Exception("stop loop"))
    websocket.send_json = AsyncMock()

    with contextlib.suppress(Exception):
        await dashboard_routes.websocket_dashboard(websocket)

    websocket.close.assert_not_awaited()
    manager.connect.assert_awaited_once_with(websocket)


@pytest.mark.asyncio
async def test_dashboard_websocket_closes_without_metrics_permission(monkeypatch) -> None:
    websocket = AsyncMock()
    websocket.app = SimpleNamespace(state=SimpleNamespace())
    websocket.close = AsyncMock()

    async def _authenticated(_websocket):
        return _auth_without_metrics()

    monkeypatch.setattr(dashboard_routes, "authenticate_websocket", _authenticated)

    await dashboard_routes.websocket_dashboard(websocket)

    websocket.close.assert_awaited_once_with(
        code=4003,
        reason="Metrics permission required",
    )


def test_provider_create_rejects_user_without_provider_write_permission() -> None:
    response = _client(provider_routes.router).post(
        "/providers",
        json={"provider_id": "prov-security", "display_name": "Provider Security"},
    )

    assert response.status_code == 403


@pytest.mark.asyncio
async def test_dashboard_realtime_rejects_user_without_metrics_permission(monkeypatch) -> None:
    snapshot = SimpleNamespace(
        rps=0.0,
        rps_1m=0.0,
        rps_5m=0.0,
        latency_p50=0,
        latency_p95=0,
        latency_p99=0,
        latency_avg=0,
        error_rate=0.0,
        error_rate_4xx=0.0,
        error_rate_5xx=0.0,
        active_users=0,
        total_threads=0,
        threads_by_user={},
        queue_depth=0,
        concurrent_requests=0,
        max_concurrent=1,
        total_tokens=0,
        input_tokens=0,
        output_tokens=0,
        token_cost_usd=0.0,
        tokens_per_minute=0.0,
        total_runs=0,
        run_success_rate=100.0,
        avg_run_duration_ms=0,
        timestamp="2026-06-30T00:00:00",
    )
    monkeypatch.setattr(
        dashboard_routes,
        "get_realtime_metrics",
        lambda: SimpleNamespace(get_realtime_snapshot=AsyncMock(return_value=snapshot)),
    )

    with pytest.raises(HTTPException) as exc:
        await dashboard_routes.get_realtime_dashboard(
            request=_request(),
            auth=_auth_without_metrics(),
        )

    assert exc.value.status_code == 403
