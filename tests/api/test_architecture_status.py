from __future__ import annotations

from types import SimpleNamespace

from fastapi import FastAPI
from fastapi.testclient import TestClient

from src.api.deps import AuthContext, get_auth_context, get_health_monitor
from src.api.v1.architecture_status import router
from src.config.settings import Settings


def _client(*, platform_admin: bool) -> TestClient:
    app = FastAPI()
    app.include_router(router)
    app.state.settings = Settings(_env_file=None)
    app.state.gateway_health_probe = _snapshot
    app.dependency_overrides[get_auth_context] = lambda: AuthContext(
        tenant_id="tenant",
        user_id="admin",
        is_authenticated=True,
        roles=["platform_admin" if platform_admin else "user"],
        permissions=["console:services:view"],
    )
    monitor = SimpleNamespace(
        all_status=lambda: {
            "frontend": SimpleNamespace(status="healthy", last_check="http://internal.invalid"),
            "qdrant": SimpleNamespace(status="degraded", last_check=None),
        }
    )
    app.dependency_overrides[get_health_monitor] = lambda: monitor
    return TestClient(app)


async def _snapshot() -> dict:
    return {
        "core_ready": True,
        "core": {
            "database": "healthy",
            "redis": "healthy",
            "agent_runtime": "healthy",
        },
        "capabilities": {
            "knowledge_service": "degraded",
            "capability_worker": "healthy",
        },
    }


def test_architecture_status_is_platform_admin_only() -> None:
    assert _client(platform_admin=False).get("/admin/architecture-status").status_code == 403


def test_architecture_status_is_grouped_sanitized_and_marks_jobs() -> None:
    response = _client(platform_admin=True).get("/admin/architecture-status")
    assert response.status_code == 200
    body = response.json()
    assert [group["display_name"] for group in body["groups"]] == [
        "Gateway Control",
        "Agent Execution",
        "Knowledge",
        "Infrastructure",
    ]
    services = {
        service["service_id"]: service for group in body["groups"] for service in group["services"]
    }
    assert services["gateway"]["replicas"] == 1
    assert services["agent-runtime"]["replicas"] == 1
    assert services["migrate"]["status"] == "one-shot"
    assert services["gateway-init"]["lifecycle"] == "one-shot"
    assert services["knowledge-service"]["degraded_reasons"]
    assert services["knowledge-service"]["status"] == "degraded"
    assert services["gateway"]["status"] == "degraded"
    assert services["agent-capability-worker"]["status"] == "degraded"
    assert services["agent-capability-worker"]["degraded_reasons"] == [
        "optional_dependency_degraded"
    ]
    assert services["qdrant"]["status"] == "degraded"
    assert services["qdrant"]["degraded_reasons"] == ["health_contract_degraded"]
    rendered = response.text.lower()
    for forbidden in ("postgresql://", "redis://", "http://", "/users/", "traceback"):
        assert forbidden not in rendered


def test_architecture_status_falls_back_safely_for_invalid_mode(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_TOPOLOGY_MODE", "invalid")

    body = _client(platform_admin=True).get("/admin/architecture-status").json()

    assert body["mode"] == "full"
    assert body["mode_configuration_valid"] is False
    services = {
        service["service_id"]: service for group in body["groups"] for service in group["services"]
    }
    assert services["knowledge-worker"]["active_in_mode"] is True
