from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from fastapi import HTTPException

from src.api.deps import AuthContext
from src.api.v1.services import register_service, update_service
from src.config.settings import Settings
from src.core.auth.rbac import RBAC
from src.services.registry.service_registry import MemoryRegistryStorage, ServiceRegistry


class FakeProviderService:
    def __init__(self, providers: dict[str, dict[str, Any]] | None = None):
        self.providers = providers or {}

    async def get_provider(self, tenant_id: str, provider_id: str) -> dict[str, Any] | None:
        assert tenant_id == "tenant-a"
        return self.providers.get(provider_id)

    def allows_environment_credentials(self, provider: dict[str, Any]) -> bool:
        return bool(
            provider.get("allow_environment_credentials")
            or provider.get("uses_environment_credentials")
        )


class FakeModelService:
    def __init__(self, models: dict[tuple[str, str], dict[str, Any]] | None = None):
        self.models = models or {}

    async def get_provider_model(
        self,
        tenant_id: str,
        provider_id: str,
        model_id: str,
    ) -> dict[str, Any] | None:
        assert tenant_id == "tenant-a"
        return self.models.get((provider_id, model_id))


def _auth() -> AuthContext:
    return AuthContext(
        user_id="admin-1",
        tenant_id="tenant-a",
        roles=["platform_admin"],
        permissions=["admin:*"],
    )


def _tenant_admin_auth() -> AuthContext:
    return AuthContext(
        user_id="tenant-admin-1",
        tenant_id="tenant-a",
        roles=["admin"],
        permissions=["admin:*"],
    )


def _request(
    *,
    provider_service: FakeProviderService | None = None,
    model_service: FakeModelService | None = None,
) -> SimpleNamespace:
    settings = Settings()
    request = SimpleNamespace()
    request.app = SimpleNamespace()
    request.app.state = SimpleNamespace()
    request.app.state.dispatcher = SimpleNamespace(
        rbac=RBAC(role_permissions=settings.rbac.roles)
    )
    if provider_service is not None:
        request.app.state.provider_service = provider_service
    if model_service is not None:
        request.app.state.model_service = model_service
    request.state = SimpleNamespace(request_id="req-model-override")
    return request


def _provider(
    provider_id: str = "dashscope-prod",
    *,
    enabled: bool = True,
    has_api_key: bool = True,
) -> dict[str, Any]:
    return {
        "provider_id": provider_id,
        "api_type": "dashscope",
        "base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1",
        "is_enabled": enabled,
        "has_api_key": has_api_key,
    }


def _model(
    provider_id: str = "dashscope-prod",
    model_id: str = "qwen-max",
    *,
    enabled: bool = True,
) -> dict[str, Any]:
    return {
        "provider_id": provider_id,
        "model_id": model_id,
        "is_enabled": enabled,
    }


def _definition(model_override: dict[str, Any]) -> dict[str, Any]:
    return {
        "service_id": "langgraph-agent",
        "name": "Agent Agent",
        "service_type": "langgraph",
        "supported_modes": ["sync", "stream"],
        "connector_type": "http",
        "connector_config": {
            "base_url": "http://langgraph-agent:8000",
            "graph_id": "Agent",
            "model_override": model_override,
        },
        "accepted_content_types": ["text"],
        "output_content_types": ["text"],
        "metadata": {"adapter_type": "langgraph"},
    }


@pytest.mark.asyncio
async def test_register_service_rejects_tenant_admin() -> None:
    registry = ServiceRegistry(MemoryRegistryStorage())
    request = _request()

    with pytest.raises(HTTPException) as exc:
        await register_service(
            request=request,
            definition=_definition({"enabled": False}),
            registry=registry,
            auth=_tenant_admin_auth(),
        )

    assert exc.value.status_code == 403
    assert exc.value.detail["reason"] == "platform_admin_required"
    assert await registry.get("langgraph-agent") is None


@pytest.mark.asyncio
async def test_register_service_rejects_browser_supplied_api_key():
    registry = ServiceRegistry(MemoryRegistryStorage())
    request = _request(
        provider_service=FakeProviderService(),
        model_service=FakeModelService(),
    )

    with pytest.raises(HTTPException) as exc:
        await register_service(
            request=request,
            definition=_definition(
                {
                    "enabled": True,
                    "provider_id": "dashscope-prod",
                    "model_id": "qwen-max",
                    "_api_key": "browser-secret",
                }
            ),
            registry=registry,
            auth=_auth(),
        )

    assert exc.value.status_code == 422
    assert exc.value.detail == "MODEL_OVERRIDE_API_KEY_FORBIDDEN"


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("providers", "models", "expected_detail"),
    [
        ({}, {}, "MODEL_OVERRIDE_PROVIDER_NOT_FOUND"),
        (
            {"dashscope-prod": _provider(enabled=False)},
            {},
            "MODEL_OVERRIDE_PROVIDER_DISABLED",
        ),
        (
            {"dashscope-prod": _provider(has_api_key=False)},
            {},
            "MODEL_OVERRIDE_API_KEY_MISSING",
        ),
        (
            {"dashscope-prod": _provider()},
            {("other-provider", "qwen-max"): _model("other-provider", "qwen-max")},
            "MODEL_OVERRIDE_MODEL_NOT_FOUND",
        ),
        (
            {"dashscope-prod": _provider()},
            {("dashscope-prod", "qwen-max"): _model(enabled=False)},
            "MODEL_OVERRIDE_MODEL_DISABLED",
        ),
    ],
)
async def test_register_service_validates_provider_model_and_key(
    providers: dict[str, dict[str, Any]],
    models: dict[tuple[str, str], dict[str, Any]],
    expected_detail: str,
):
    registry = ServiceRegistry(MemoryRegistryStorage())
    request = _request(
        provider_service=FakeProviderService(providers),
        model_service=FakeModelService(models),
    )

    with pytest.raises(HTTPException) as exc:
        await register_service(
            request=request,
            definition=_definition(
                {
                    "enabled": True,
                    "provider_id": "dashscope-prod",
                    "model_id": "qwen-max",
                }
            ),
            registry=registry,
            auth=_auth(),
        )

    assert exc.value.status_code == 422
    assert exc.value.detail == expected_detail


@pytest.mark.asyncio
async def test_register_service_stores_valid_override_without_secret():
    registry = ServiceRegistry(MemoryRegistryStorage())
    request = _request(
        provider_service=FakeProviderService({"dashscope-prod": _provider()}),
        model_service=FakeModelService({("dashscope-prod", "qwen-max"): _model()}),
    )

    result = await register_service(
        request=request,
        definition=_definition(
            {
                "enabled": "true",
                "provider_id": "dashscope-prod",
                "model_id": "qwen-max",
                "temperature": "0.3",
            }
        ),
        registry=registry,
        auth=_auth(),
    )
    stored = await registry.get("langgraph-agent")
    model_override = stored.connector_config["model_override"]

    assert result == {"service_id": "langgraph-agent", "status": "registered"}
    assert model_override["enabled"] is True
    assert model_override["temperature"] == 0.3
    assert model_override["cache_epoch"] == 1
    assert "_api_key" not in model_override
    assert "api_key" not in model_override


@pytest.mark.asyncio
async def test_disabled_override_can_be_saved_without_provider_lookup():
    registry = ServiceRegistry(MemoryRegistryStorage())
    request = _request()

    await register_service(
        request=request,
        definition=_definition(
            {
                "enabled": False,
                "provider_id": "missing-provider",
                "model_id": "missing-model",
            }
        ),
        registry=registry,
        auth=_auth(),
    )
    stored = await registry.get("langgraph-agent")
    model_override = stored.connector_config["model_override"]

    assert model_override["enabled"] is False
    assert model_override["cache_epoch"] == 1


@pytest.mark.asyncio
async def test_update_service_rejects_failover_candidate_secret_fields():
    registry = ServiceRegistry(MemoryRegistryStorage())
    initial = registry._service_from_dict(
        _definition(
            {
                "enabled": True,
                "provider_id": "dashscope-prod",
                "model_id": "qwen-max",
                "cache_epoch": 1,
            }
        )
    )
    await registry.register(initial)
    request = _request(
        provider_service=FakeProviderService({"dashscope-prod": _provider()}),
        model_service=FakeModelService({("dashscope-prod", "qwen-max"): _model()}),
    )

    with pytest.raises(HTTPException) as exc:
        await update_service(
            service_id="langgraph-agent",
            request=request,
            patch={
                "connector_config": {
                    "base_url": "http://langgraph-agent:8000",
                    "graph_id": "Agent",
                    "model_override": {
                        "enabled": True,
                        "provider_id": "dashscope-prod",
                        "model_id": "qwen-max",
                        "failover": {
                            "enabled": True,
                            "candidates": [
                                {
                                    "provider_id": "dashscope-prod-2",
                                    "model_id": "qwen-max",
                                    "_api_key": "browser-secret",
                                }
                            ],
                        },
                    },
                }
            },
            registry=registry,
            auth=_auth(),
        )

    assert exc.value.status_code == 422
    assert exc.value.detail == "MODEL_OVERRIDE_API_KEY_FORBIDDEN"


@pytest.mark.asyncio
async def test_update_service_rejects_unknown_failover_model():
    registry = ServiceRegistry(MemoryRegistryStorage())
    initial = registry._service_from_dict(
        _definition(
            {
                "enabled": True,
                "provider_id": "dashscope-prod",
                "model_id": "qwen-max",
                "cache_epoch": 1,
            }
        )
    )
    await registry.register(initial)
    request = _request(
        provider_service=FakeProviderService(
            {
                "dashscope-prod": _provider(),
                "dashscope-intl": _provider("dashscope-intl"),
            }
        ),
        model_service=FakeModelService({("dashscope-prod", "qwen-max"): _model()}),
    )

    with pytest.raises(HTTPException) as exc:
        await update_service(
            service_id="langgraph-agent",
            request=request,
            patch={
                "connector_config": {
                    "base_url": "http://langgraph-agent:8000",
                    "graph_id": "Agent",
                    "model_override": {
                        "enabled": True,
                        "provider_id": "dashscope-prod",
                        "model_id": "qwen-max",
                        "failover": {
                            "enabled": True,
                            "candidates": [
                                {"provider_id": "dashscope-intl", "model_id": "qwen-max"}
                            ],
                        },
                    },
                }
            },
            registry=registry,
            auth=_auth(),
        )

    assert exc.value.status_code == 422
    assert exc.value.detail == "MODEL_OVERRIDE_FAILOVER_MODEL_NOT_FOUND"


@pytest.mark.asyncio
async def test_update_service_auto_seeds_default_failover_candidates():
    registry = ServiceRegistry(MemoryRegistryStorage())
    initial = registry._service_from_dict(
        _definition(
            {
                "enabled": True,
                "provider_id": "dashscope-prod",
                "model_id": "qwen-old",
                "cache_epoch": 4,
            }
        )
    )
    await registry.register(initial)
    request = _request(
        provider_service=FakeProviderService(
            {
                "google-vertex": _provider("google-vertex"),
                "google": _provider("google"),
                "dashscope": _provider("dashscope"),
            }
        ),
        model_service=FakeModelService(
            {
                ("google-vertex", "gemini-3-flash-preview-vertex"): _model(
                    "google-vertex",
                    "gemini-3-flash-preview-vertex",
                ),
                ("google", "gemini-3.5-flash"): {
                    **_model("google", "gemini-3.5-flash"),
                    "display_name": "Gemini 3.5 Flash",
                    "sort_order": 120,
                },
                ("dashscope", "qwen3.6-plus"): {
                    **_model("dashscope", "qwen3.6-plus"),
                    "display_name": "Qwen 3.6 Plus",
                    "sort_order": 100,
                },
                ("dashscope", "qwen3.7-max"): {
                    **_model("dashscope", "qwen3.7-max"),
                    "display_name": "Qwen 3.7 Max",
                    "sort_order": 120,
                },
            }
        ),
    )

    await update_service(
        service_id="langgraph-agent",
        request=request,
        patch={
            "connector_config": {
                "base_url": "http://langgraph-agent:8000",
                "graph_id": "Agent",
                "model_override": {
                    "enabled": True,
                    "provider_id": "google-vertex",
                    "model_id": "gemini-3-flash-preview-vertex",
                    "failover": {"enabled": False, "candidates": []},
                },
            }
        },
        registry=registry,
        auth=_auth(),
    )
    stored = await registry.get("langgraph-agent")
    model_override = stored.connector_config["model_override"]

    assert model_override["cache_epoch"] == 5
    assert model_override["failover"]["enabled"] is True
    assert model_override["failover"]["candidates"] == [
        {"provider_id": "google", "model_id": "gemini-3.5-flash"},
        {"provider_id": "dashscope", "model_id": "qwen3.7-max"},
    ]


@pytest.mark.asyncio
async def test_update_service_increments_cache_epoch_when_failover_order_changes():
    registry = ServiceRegistry(MemoryRegistryStorage())
    initial = registry._service_from_dict(
        _definition(
            {
                "enabled": True,
                "provider_id": "dashscope-prod",
                "model_id": "qwen-max",
                "cache_epoch": 4,
                "failover": {
                    "enabled": True,
                    "max_attempts": 3,
                    "candidates": [
                        {"provider_id": "dashscope-intl", "model_id": "qwen-max"},
                        {"provider_id": "google-ai-studio", "model_id": "gemini-3.5-flash"},
                    ],
                },
            }
        )
    )
    await registry.register(initial)
    request = _request(
        provider_service=FakeProviderService(
            {
                "dashscope-prod": _provider(),
                "dashscope-intl": _provider("dashscope-intl"),
                "google-ai-studio": _provider("google-ai-studio"),
            }
        ),
        model_service=FakeModelService(
            {
                ("dashscope-prod", "qwen-max"): _model(),
                ("dashscope-intl", "qwen-max"): _model("dashscope-intl", "qwen-max"),
                ("google-ai-studio", "gemini-3.5-flash"): _model(
                    "google-ai-studio",
                    "gemini-3.5-flash",
                ),
            }
        ),
    )

    await update_service(
        service_id="langgraph-agent",
        request=request,
        patch={
            "connector_config": {
                "base_url": "http://langgraph-agent:8000",
                "graph_id": "Agent",
                "model_override": {
                    "enabled": True,
                    "provider_id": "dashscope-prod",
                    "model_id": "qwen-max",
                    "failover": {
                        "enabled": True,
                        "max_attempts": 3,
                        "candidates": [
                            {"provider_id": "google-ai-studio", "model_id": "gemini-3.5-flash"},
                            {"provider_id": "dashscope-intl", "model_id": "qwen-max"},
                        ],
                    },
                },
            }
        },
        registry=registry,
        auth=_auth(),
    )
    stored = await registry.get("langgraph-agent")
    model_override = stored.connector_config["model_override"]

    assert model_override["cache_epoch"] == 5
    assert model_override["failover"]["candidates"] == [
        {"provider_id": "google-ai-studio", "model_id": "gemini-3.5-flash"},
        {"provider_id": "dashscope-intl", "model_id": "qwen-max"},
    ]


@pytest.mark.asyncio
async def test_update_service_clamps_failover_attempts_to_reach_fallback():
    registry = ServiceRegistry(MemoryRegistryStorage())
    request = _request(
        provider_service=FakeProviderService(
            {
                "dashscope-prod": _provider(),
                "google-ai-studio": _provider("google-ai-studio"),
            }
        ),
        model_service=FakeModelService(
            {
                ("dashscope-prod", "qwen-max"): _model(),
                ("google-ai-studio", "gemini-3.5-flash"): _model(
                    "google-ai-studio",
                    "gemini-3.5-flash",
                ),
            }
        ),
    )

    await register_service(
        request=request,
        definition=_definition(
            {
                "enabled": True,
                "provider_id": "dashscope-prod",
                "model_id": "qwen-max",
                "failover": {
                    "enabled": True,
                    "max_attempts": 1,
                    "candidates": [
                        {"provider_id": "google-ai-studio", "model_id": "gemini-3.5-flash"}
                    ],
                },
            }
        ),
        registry=registry,
        auth=_auth(),
    )
    stored = await registry.get("langgraph-agent")
    model_override = stored.connector_config["model_override"]

    assert model_override["failover"]["max_attempts"] == 2


@pytest.mark.asyncio
async def test_disabled_failover_can_save_primary_only():
    registry = ServiceRegistry(MemoryRegistryStorage())
    request = _request(
        provider_service=FakeProviderService({"dashscope-prod": _provider()}),
        model_service=FakeModelService({("dashscope-prod", "qwen-max"): _model()}),
    )

    await register_service(
        request=request,
        definition=_definition(
            {
                "enabled": True,
                "provider_id": "dashscope-prod",
                "model_id": "qwen-max",
                "failover": {
                    "enabled": False,
                    "candidates": [
                        {"provider_id": "missing-provider", "model_id": "missing-model"}
                    ],
                },
            }
        ),
        registry=registry,
        auth=_auth(),
    )
    stored = await registry.get("langgraph-agent")
    model_override = stored.connector_config["model_override"]

    assert model_override["enabled"] is True
    assert model_override["failover"]["enabled"] is False
    assert model_override["failover"]["candidates"] == []


@pytest.mark.asyncio
async def test_update_service_increments_cache_epoch_when_override_changes():
    registry = ServiceRegistry(MemoryRegistryStorage())
    initial = registry._service_from_dict(
        _definition(
            {
                "enabled": True,
                "provider_id": "dashscope-prod",
                "model_id": "qwen-old",
                "cache_epoch": 4,
            }
        )
    )
    await registry.register(initial)
    request = _request(
        provider_service=FakeProviderService({"dashscope-prod": _provider()}),
        model_service=FakeModelService(
            {
                ("dashscope-prod", "qwen-new"): _model(model_id="qwen-new"),
            }
        ),
    )

    result = await update_service(
        service_id="langgraph-agent",
        request=request,
        patch={
            "connector_config": {
                "base_url": "http://langgraph-agent:8000",
                "graph_id": "Agent",
                "model_override": {
                    "enabled": True,
                    "provider_id": "dashscope-prod",
                    "model_id": "qwen-new",
                    "cache_epoch": 999,
                },
            }
        },
        registry=registry,
        auth=_auth(),
    )
    stored = await registry.get("langgraph-agent")
    model_override = stored.connector_config["model_override"]

    assert result == {"status": "success", "service_id": "langgraph-agent"}
    assert model_override["model_id"] == "qwen-new"
    assert model_override["cache_epoch"] == 5
