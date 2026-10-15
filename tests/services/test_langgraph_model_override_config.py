from __future__ import annotations

import hashlib
from typing import Any

import pytest
from ai_gateway_core.enums import ContentType, InvocationMode, ServiceType, TransportType
from ai_gateway_core.exceptions import ValidationFailedError

from src.adapters.base import ProtocolAdapter
from src.adapters.langgraph import LangGraphAdapter, _scrub_sensitive_text
from src.models.request import ContentItem, UnifiedRequest
from src.models.response import UnifiedResponse
from src.models.service import ServiceDefinition
from src.services.registry.service_registry import MemoryRegistryStorage, ServiceRegistry


class FakeProviderService:
    providers = {
        "dashscope-prod": {
            "provider_id": "dashscope-prod",
            "is_enabled": True,
            "runtime_provider": "dashscope",
            "runtime_base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "api_key": "unit-test-runtime-secret",
            "allow_environment_credentials": False,
        },
        "google-ai-studio": {
            "provider_id": "google-ai-studio",
            "is_enabled": True,
            "runtime_provider": "gemini",
            "runtime_base_url": "https://generativelanguage.googleapis.com",
            "api_key": "unit-test-gemini-secret",
            "allow_environment_credentials": False,
        },
    }

    async def get_runtime_provider_config(
        self,
        tenant_id: str,
        provider_id: str,
    ) -> dict[str, Any]:
        assert tenant_id == "tenant-a"
        provider = self.providers.get(provider_id)
        if not provider:
            raise ValueError(provider_id)
        return provider


class FakeModelService:
    models = {
        ("dashscope-prod", "qwen-max"): {
            "provider_id": "dashscope-prod",
            "model_id": "qwen-max",
            "is_enabled": True,
        },
        ("google-ai-studio", "gemini-3.5-flash"): {
            "provider_id": "google-ai-studio",
            "model_id": "gemini-3.5-flash",
            "is_enabled": True,
        },
    }

    async def get_provider_model(
        self,
        tenant_id: str,
        provider_id: str,
        model_id: str,
    ) -> dict[str, Any]:
        assert tenant_id == "tenant-a"
        return self.models.get((provider_id, model_id))


class DummyAdapter(ProtocolAdapter):
    async def invoke(self, request: UnifiedRequest) -> UnifiedResponse:
        raise NotImplementedError


class FakeErrorConnector:
    async def post(self, _endpoint: str, **_: object) -> dict[str, Any]:
        return {
            "__error__": {
                "error": "RuntimeError",
                "message": 'provider failed with "_api_key":"browser-secret"',
            }
        }

    async def close(self) -> None:
        return None


@pytest.fixture(autouse=True)
def reset_langgraph_adapter_control_plane():
    LangGraphAdapter.configure_model_control_plane(None, None)
    yield
    LangGraphAdapter.configure_model_control_plane(None, None)


def _service(model_override: dict[str, Any] | None = None) -> ServiceDefinition:
    connector_config: dict[str, Any] = {
        "base_url": "http://langgraph-agent:8000",
        "graph_id": "Agent",
    }
    if model_override is not None:
        connector_config["model_override"] = model_override

    return ServiceDefinition(
        service_id="langgraph-agent",
        name="Agent Agent",
        service_type=ServiceType.LANGGRAPH,
        supported_modes=[InvocationMode.SYNC, InvocationMode.STREAM],
        connector_type=TransportType.HTTP,
        connector_config=connector_config,
        accepted_content_types=[ContentType.TEXT],
        output_content_types=[ContentType.TEXT],
        metadata={"adapter_type": "langgraph"},
    )


def _request(
    *,
    context: dict[str, Any] | None = None,
    parameters: dict[str, Any] | None = None,
) -> UnifiedRequest:
    return UnifiedRequest(
        request_id="req-1",
        service_id="langgraph-agent",
        inputs=[ContentItem(type=ContentType.TEXT, data="hello")],
        session_id="session-1",
        context=context,
        parameters=parameters,
        user_id="user-a",
        tenant_id="tenant-a",
    )


@pytest.mark.asyncio
async def test_run_config_injects_gateway_resolved_gateway_model():
    LangGraphAdapter.configure_model_control_plane(FakeProviderService(), FakeModelService())
    adapter = LangGraphAdapter(
        _service(
            {
                "enabled": True,
                "provider_id": "dashscope-prod",
                "model_id": "qwen-max",
                "temperature": 0.2,
                "cache_epoch": 7,
            }
        )
    )

    config = await adapter._build_run_config(
        _request(
            parameters={
                "config": {
                    "configurable": {
                        "gateway_model": {"_api_key": "browser-secret"},
                        "locale": "en",
                    }
                }
            }
        )
    )
    gateway_model = config["configurable"]["gateway_model"]

    assert gateway_model["tenant_id"] == "tenant-a"
    assert gateway_model["provider_id"] == "dashscope-prod"
    assert gateway_model["provider"] == "dashscope"
    assert gateway_model["model"] == "qwen-max"
    assert gateway_model["base_url"] == "https://dashscope.aliyuncs.com/compatible-mode/v1"
    assert gateway_model["_api_key"] == "unit-test-runtime-secret"
    assert gateway_model["api_key_fingerprint"] == hashlib.sha256(
        b"unit-test-runtime-secret"
    ).hexdigest()[:16]
    assert gateway_model["cache_epoch"] == "7"
    assert config["configurable"]["locale"] == "en"
    assert "browser-secret" not in str(config)


@pytest.mark.asyncio
async def test_enabled_model_override_requires_control_plane():
    adapter = LangGraphAdapter(
        _service(
            {
                "enabled": True,
                "provider_id": "dashscope-prod",
                "model_id": "qwen-max",
            }
        )
    )

    with pytest.raises(ValidationFailedError, match="control plane is not initialized"):
        await adapter._build_run_config(_request())


@pytest.mark.asyncio
async def test_adapter_injects_failover_candidate_shape():
    LangGraphAdapter.configure_model_control_plane(FakeProviderService(), FakeModelService())
    adapter = LangGraphAdapter(
        _service(
            {
                "enabled": True,
                "provider_id": "dashscope-prod",
                "model_id": "qwen-max",
                "temperature": 0.2,
                "cache_epoch": 7,
                "failover": {
                    "enabled": True,
                    "max_attempts": 2,
                    "candidates": [
                        {
                            "provider_id": "google-ai-studio",
                            "model_id": "gemini-3.5-flash",
                        }
                    ],
                },
            }
        )
    )

    config = await adapter._build_run_config(_request())
    gateway_model = config["configurable"]["gateway_model"]
    candidates = gateway_model["failover"]["candidates"]

    assert gateway_model["provider_id"] == "dashscope-prod"
    assert [(c["provider_id"], c["model_id"]) for c in candidates] == [
        ("dashscope-prod", "qwen-max"),
        ("google-ai-studio", "gemini-3.5-flash"),
    ]
    assert candidates[0]["_api_key"] == "unit-test-runtime-secret"
    assert candidates[1]["_api_key"] == "unit-test-gemini-secret"


@pytest.mark.asyncio
async def test_run_config_ignores_caller_supplied_gateway_model_when_service_disabled():
    adapter = LangGraphAdapter(_service(model_override={"enabled": False}))

    config = await adapter._build_run_config(
        _request(
            context={
                "configurable": {
                    "gateway_model": {"_api_key": "browser-secret"},
                    "dataset_id": "dataset-a",
                }
            },
            parameters={
                "config": {
                    "configurable": {
                        "checkpoint_ns": "browser-tenant",
                    }
                }
            },
        )
    )

    assert "gateway_model" not in config["configurable"]
    assert config["configurable"]["dataset_id"] == "dataset-a"
    assert config["configurable"]["checkpoint_ns"] == "tenant-a"
    assert config["configurable"]["thread_id"] == "session-1"
    assert "browser-secret" not in str(config)


def test_scrub_sensitive_text_hides_api_keys_but_keeps_fingerprint():
    body = (
        '{"_api_key":"unit-test-runtime-secret",'
        '"api_key":"unit-test-provider-secret",'
        '"api_key_fingerprint":"abc123"}'
    )

    scrubbed = _scrub_sensitive_text(body)

    assert "unit-test-runtime-secret" not in scrubbed
    assert "unit-test-provider-secret" not in scrubbed
    assert '"_api_key":"***"' in scrubbed
    assert '"api_key":"***"' in scrubbed
    assert '"api_key_fingerprint":"abc123"' in scrubbed


@pytest.mark.asyncio
async def test_remote_wait_treats_langgraph_error_payload_as_failure():
    adapter = LangGraphAdapter(_service())
    adapter.connector = FakeErrorConnector()

    with pytest.raises(ValidationFailedError) as exc_info:
        await adapter._remote_wait(_request(), [{"role": "user", "content": "hello"}])

    message = str(exc_info.value)
    assert "LangGraph invoke failed at /runs/wait" in message
    assert "RuntimeError" in message
    assert "browser-secret" not in message
    assert '"_api_key":"***"' in message


@pytest.mark.asyncio
async def test_service_register_invalidates_cached_adapter():
    registry = ServiceRegistry(MemoryRegistryStorage())
    registry.register_adapter("dummy", DummyAdapter)
    service = _service()
    service.metadata = {"adapter_type": "dummy"}
    await registry.register(service)
    first_adapter = registry.get_adapter(service)

    updated = _service()
    updated.metadata = {"adapter_type": "dummy"}
    updated.connector_config["base_url"] = "http://langgraph-agent-new:8000"
    await registry.register(updated)
    second_adapter = registry.get_adapter(updated)

    try:
        assert second_adapter is not first_adapter
        assert second_adapter.service.connector_config["base_url"] == "http://langgraph-agent-new:8000"
    finally:
        await first_adapter.connector.close()
        await second_adapter.connector.close()
