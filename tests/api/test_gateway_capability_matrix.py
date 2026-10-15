from __future__ import annotations

from datetime import date, datetime, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest
from fastapi import HTTPException

from src.api.deps import AuthContext
from src.api.schemas.providers import ModelCreate, ProviderCreate
from src.api.v1 import assistant as assistant_routes
from src.api.v1 import metrics as metrics_routes
from src.api.v1 import quota as quota_routes
from src.api.v1 import usage as usage_routes
from src.api.v1.config import (
    RateLimitRule,
    ServiceCapacityConfigUpdate,
    ServiceConfigUpdate,
    ServicePriorityConfigUpdate,
    create_rate_limit,
    update_service_config,
)
from src.api.v1.models import create_model
from src.api.v1.providers import create_provider
from src.config.settings import Settings
from src.core.auth.permissions import Capability, canonical_permission
from src.core.auth.rbac import RBAC
from src.core.auth.user_resolver import UserContext
from src.models.service import ServiceDefinition


def _request() -> SimpleNamespace:
    settings = Settings()
    request = SimpleNamespace()
    request.app = SimpleNamespace()
    request.app.state = SimpleNamespace()
    request.app.state.dispatcher = SimpleNamespace(rbac=RBAC(role_permissions=settings.rbac.roles))
    request.app.state.settings = settings
    request.app.state.database = None
    request.app.state.redis = None
    request.state = SimpleNamespace(request_id="req-capability-matrix")
    request.headers = {}
    request.client = SimpleNamespace(host="127.0.0.1")
    return request


def _auth(*permissions: str, roles: list[str] | None = None) -> AuthContext:
    return AuthContext(
        user_id="user-capability",
        tenant_id="tenant-a",
        roles=roles or ["user"],
        permissions=list(permissions),
        is_authenticated=True,
    )


def _user(*permissions: str) -> UserContext:
    return UserContext(
        user_id="user-capability",
        tenant_id="tenant-a",
        tier="normal",
        is_authenticated=True,
        roles=["user", *permissions],
        ip="127.0.0.1",
    )


@pytest.mark.parametrize(
    ("capability_name", "permission"),
    [
        ("GATEWAY_METRICS_READ", "console:metrics:view"),
        ("GATEWAY_USAGE_READ", "console:usage:view"),
        ("GATEWAY_QUOTA_READ", "console:quota:view"),
        ("GATEWAY_QUOTA_WRITE", "console:quota:edit"),
        ("GATEWAY_RATE_LIMIT_WRITE", "console:rate_limits:edit"),
        ("GATEWAY_PROVIDER_CONFIG_WRITE", "console:providers:edit"),
        ("GATEWAY_MODEL_CONFIG_WRITE", "console:models:edit"),
        ("GATEWAY_SERVICE_CONFIG_WRITE", "console:services:edit"),
    ],
)
def test_gateway_capabilities_map_to_canonical_permissions(
    capability_name: str,
    permission: str,
) -> None:
    capability = getattr(Capability, capability_name)

    assert canonical_permission(capability) == permission


@pytest.mark.asyncio
async def test_metrics_read_accepts_gateway_metrics_permission(monkeypatch) -> None:
    recorder = SimpleNamespace(
        get_today_summary=AsyncMock(
            return_value={
                "total_requests": 0,
                "success_rate": 100.0,
                "avg_latency_ms": 0,
                "requests_by_hour": [],
            }
        )
    )
    monkeypatch.setattr(metrics_routes, "get_metrics_recorder", lambda: recorder)

    result = await metrics_routes.get_metrics_summary(
        request=_request(),
        auth=_auth("console:metrics:view", roles=["platform_admin"]),
    )

    assert result.total_requests == 0


@pytest.mark.asyncio
async def test_legacy_assistant_resume_enforces_rate_limit_and_rejects_second_loop(
    monkeypatch,
) -> None:
    from src.api import deps

    calls: list[tuple[str, str]] = []

    async def fake_enforce_rate_limit(request, user, operation: str):
        calls.append((user.user_id, operation))

    monkeypatch.setattr(deps, "enforce_rate_limit", fake_enforce_rate_limit)

    with pytest.raises(HTTPException) as exc:
        await assistant_routes.prepare_run_resume(
            run_id="run-1",
            request=SimpleNamespace(),
            body=None,
            user=_user("conversation:playground:access"),
        )

    assert calls == [("user-capability", "assistant_resume")]
    assert exc.value.status_code == 409
    assert exc.value.detail["code"] == "AGENT_RUNTIME_ONLY"


@pytest.mark.asyncio
async def test_usage_read_rejects_missing_gateway_usage_permission(monkeypatch) -> None:
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
            auth=_auth(),
        )

    assert exc.value.status_code == 403
    assert exc.value.detail["required_capability"] == "GatewayUsageRead"
    assert exc.value.detail["required_permission"] == "console:usage:view"


@pytest.mark.asyncio
async def test_quota_write_rejects_missing_gateway_quota_write_permission(monkeypatch) -> None:
    quota_service = SimpleNamespace(
        set_user_quota=AsyncMock(return_value={}),
        get_user_quota=AsyncMock(return_value=None),
    )
    monkeypatch.setattr(quota_routes, "get_quota_service", lambda: quota_service)

    with pytest.raises(HTTPException) as exc:
        await quota_routes.set_user_quota(
            user_id="target-user",
            quota_request=quota_routes.SetQuotaRequest(daily_token_limit=10),
            request=_request(),
            auth=_auth("console:quota:view"),
        )

    assert exc.value.status_code == 403
    assert exc.value.detail["required_capability"] == "GatewayQuotaWrite"
    assert exc.value.detail["required_permission"] == "console:quota:edit"


@pytest.mark.asyncio
async def test_rate_limit_write_accepts_gateway_rate_limit_write_permission() -> None:
    result = await create_rate_limit(
        body=RateLimitRule(scope="global", requests=10, window=60, burst=0),
        request=_request(),
        auth=_auth("console:rate_limits:edit"),
    )

    assert result["status"] == "success"


@pytest.mark.asyncio
async def test_service_config_write_accepts_gateway_service_config_permission() -> None:
    request = _request()
    service = ServiceDefinition(service_id="svc-1", name="Service 1")
    request.app.state.registry = SimpleNamespace(
        get=AsyncMock(return_value=service),
        storage=SimpleNamespace(save=AsyncMock()),
        _cache={},
    )

    result = await update_service_config(
        service_id="svc-1",
        body=ServiceConfigUpdate(priority=ServicePriorityConfigUpdate(priority=9)),
        request=request,
        auth=_auth("console:services:edit"),
    )

    assert result["status"] == "success"
    assert service.get_service_config().priority.priority == 9


@pytest.mark.asyncio
async def test_service_capacity_config_update_uses_registry_persistence_without_legacy_db_method() -> None:
    request = _request()
    service = ServiceDefinition(service_id="svc-1", name="Service 1")
    request.app.state.registry = SimpleNamespace(
        get=AsyncMock(return_value=service),
        storage=SimpleNamespace(save=AsyncMock()),
        _cache={},
    )
    request.app.state.proxy_config_loader = SimpleNamespace(invalidate=Mock())
    request.app.state.database = SimpleNamespace(
        enabled=True,
        record_audit_event=AsyncMock(),
    )

    result = await update_service_config(
        service_id="svc-1",
        body=ServiceConfigUpdate(
            capacity=ServiceCapacityConfigUpdate(
                upstream_group="langgraph_agent",
                concurrency_limit=3,
                queue_max=0,
                queue_timeout_ms=1,
            )
        ),
        request=request,
        auth=_auth("console:services:edit"),
    )

    assert result["status"] == "success"
    request.app.state.registry.storage.save.assert_awaited_once_with(service)
    request.app.state.proxy_config_loader.invalidate.assert_any_call("svc-1")
    assert service.get_service_config().capacity.upstream_group == "langgraph_agent"
    assert service.get_service_config().capacity.concurrency_limit == 3


@pytest.mark.asyncio
async def test_provider_write_accepts_gateway_provider_config_permission() -> None:
    provider_service = SimpleNamespace(
        create_provider=AsyncMock(
            return_value={
                "tenant_id": "tenant-a",
                "provider_id": "prov-a",
                "display_name": "Provider A",
                "api_type": "openai",
                "base_url": None,
                "is_enabled": True,
                "has_api_key": False,
                "created_at": datetime.now(timezone.utc),
                "updated_at": datetime.now(timezone.utc),
            }
        )
    )

    result = await create_provider(
        body=ProviderCreate(provider_id="prov-a", display_name="Provider A"),
        provider_service=provider_service,
        user=_user("console:providers:edit"),
    )

    assert result["provider_id"] == "prov-a"


@pytest.mark.asyncio
async def test_model_write_accepts_gateway_model_config_permission() -> None:
    model_service = SimpleNamespace(
        create_model=AsyncMock(
            return_value={
                "tenant_id": "tenant-a",
                "model_id": "model-a",
                "provider_id": "prov-a",
                "display_name": "Model A",
                "context_window": 128000,
                "max_output_tokens": 4096,
                "supports_vision": False,
                "supports_tools": True,
                "input_price_per_1k": 0,
                "output_price_per_1k": 0,
                "access_level": "public",
                "is_enabled": True,
                "sort_order": 0,
                "created_at": datetime.now(timezone.utc),
                "updated_at": datetime.now(timezone.utc),
            }
        )
    )

    result = await create_model(
        body=ModelCreate(
            model_id="model-a",
            provider_id="prov-a",
            display_name="Model A",
        ),
        model_service=model_service,
        user=_user("console:models:edit"),
    )

    assert result["model_id"] == "model-a"
