from __future__ import annotations

import hashlib
import json
from types import SimpleNamespace
from typing import Any

import pytest

from src.api.v1.proxy import _safe_model_override_debug
from src.proxy.config_loader import ProxyServiceConfig
from src.proxy.langgraph_run_body import (
    clear_runtime_model_override_cache,
    inject_langgraph_model_override_bytes,
)


@pytest.fixture(autouse=True)
def _reset_runtime_model_override_cache() -> None:
    clear_runtime_model_override_cache()
    yield
    clear_runtime_model_override_cache()


class FakeProviderService:
    providers = {
        "dashscope": {
            "is_enabled": True,
            "runtime_provider": "dashscope",
            "runtime_base_url": "https://dashscope-intl.aliyuncs.com/compatible-mode",
            "api_key": "gateway-runtime-secret",
        },
        "google-ai-studio": {
            "is_enabled": True,
            "runtime_provider": "gemini",
            "runtime_base_url": "https://generativelanguage.googleapis.com",
            "api_key": "gemini-runtime-secret",
        },
    }

    async def get_runtime_provider_config(self, tenant_id: str, provider_id: str) -> dict[str, Any]:
        assert tenant_id == "tenant-a"
        provider = self.providers.get(provider_id)
        if not provider:
            raise ValueError(provider_id)
        return provider


class FakeModelService:
    models = {
        ("dashscope", "qwen3.6-plus"): {"is_enabled": True},
        ("google-ai-studio", "gemini-3.5-flash"): {"is_enabled": True},
    }

    async def get_provider_model(
        self,
        tenant_id: str,
        provider_id: str,
        model_id: str,
    ) -> dict[str, Any]:
        assert tenant_id == "tenant-a"
        return self.models.get((provider_id, model_id))


def _request() -> SimpleNamespace:
    return SimpleNamespace(
        app=SimpleNamespace(
            state=SimpleNamespace(
                provider_service=FakeProviderService(),
                model_service=FakeModelService(),
            )
        )
    )


def _config(model_override: dict[str, Any]) -> ProxyServiceConfig:
    return ProxyServiceConfig(
        service_id="local-2024-agent",
        service_name="LangGraph Agent",
        upstream_url="http://langgraph-agent:8000",
        assistant_id="Agent",
        metadata={"adapter_type": "langgraph"},
        model_override=model_override,
    )


def test_safe_model_override_debug_exposes_no_secret_fields() -> None:
    debug = _safe_model_override_debug(
        {
            "enabled": True,
            "provider_id": "dashscope-intl",
            "model_id": "qwen3.6-plus",
            "temperature": 0.2,
            "cache_epoch": 9,
            "_api_key": "gateway-runtime-secret",
            "api_key": "also-secret",
            "base_url": "https://dashscope-intl.aliyuncs.com/compatible-mode",
        }
    )

    assert debug == {
        "enabled": True,
        "provider_id": "dashscope-intl",
        "model_id": "qwen3.6-plus",
        "temperature": 0.2,
        "cache_epoch": 9,
    }
    assert "secret" not in json.dumps(debug)
    assert "api_key" not in debug
    assert "_api_key" not in debug


@pytest.mark.asyncio
async def test_proxy_run_injects_gateway_resolved_gateway_model() -> None:
    body = json.dumps(
        {
            "input": {"messages": [{"role": "user", "content": "hello"}]},
            "config": {
                "configurable": {
                    "thread_id": "t1",
                    "gateway_model": {"_api_key": "browser-secret"},
                }
            },
        }
    ).encode("utf-8")

    updated = await inject_langgraph_model_override_bytes(
        request=_request(),
        body=body,
        method="POST",
        path="threads/t1/runs/stream",
        service_config=_config(
            {
                "enabled": True,
                "provider_id": "dashscope",
                "model_id": "qwen3.6-plus",
                "temperature": 0.2,
                "cache_epoch": 7,
            }
        ),
        tenant_id="tenant-a",
    )

    payload = json.loads((updated or b"{}").decode("utf-8"))
    gateway_model = payload["config"]["configurable"]["gateway_model"]
    assert gateway_model["tenant_id"] == "tenant-a"
    assert gateway_model["provider_id"] == "dashscope"
    assert gateway_model["provider"] == "dashscope"
    assert gateway_model["model_id"] == "qwen3.6-plus"
    assert gateway_model["model"] == "qwen3.6-plus"
    assert gateway_model["temperature"] == 0.2
    assert gateway_model["cache_epoch"] == "7"
    assert gateway_model["_api_key"] == "gateway-runtime-secret"
    assert gateway_model["api_key_fingerprint"] == hashlib.sha256(
        b"gateway-runtime-secret"
    ).hexdigest()[:16]
    assert "browser-secret" not in json.dumps(payload)


@pytest.mark.asyncio
async def test_proxy_run_injects_gateway_model_when_config_is_absent() -> None:
    body = json.dumps(
        {
            "input": {"messages": [{"role": "user", "content": "hello"}]},
        }
    ).encode("utf-8")

    updated = await inject_langgraph_model_override_bytes(
        request=_request(),
        body=body,
        method="POST",
        path="threads/t1/runs/stream",
        service_config=_config(
            {
                "enabled": True,
                "provider_id": "dashscope",
                "model_id": "qwen3.6-plus",
                "temperature": 0.2,
            }
        ),
        tenant_id="tenant-a",
    )

    payload = json.loads((updated or b"{}").decode("utf-8"))
    gateway_model = payload["config"]["configurable"]["gateway_model"]
    assert gateway_model["provider_id"] == "dashscope"
    assert gateway_model["model_id"] == "qwen3.6-plus"
    assert gateway_model["_api_key"] == "gateway-runtime-secret"


@pytest.mark.asyncio
async def test_proxy_injects_primary_and_fallback_candidates_without_browser_key() -> None:
    body = json.dumps(
        {
            "input": {"messages": [{"role": "user", "content": "hello"}]},
            "config": {"configurable": {"gateway_model": {"api_key": "browser-secret"}}},
        }
    ).encode("utf-8")

    updated = await inject_langgraph_model_override_bytes(
        request=_request(),
        body=body,
        method="POST",
        path="threads/t1/runs/stream",
        service_config=_config(
            {
                "enabled": True,
                "provider_id": "dashscope",
                "model_id": "qwen3.6-plus",
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
        ),
        tenant_id="tenant-a",
    )

    payload = json.loads((updated or b"{}").decode("utf-8"))
    gateway_model = payload["config"]["configurable"]["gateway_model"]
    candidates = gateway_model["failover"]["candidates"]

    assert [(c["provider_id"], c["model_id"]) for c in candidates] == [
        ("dashscope", "qwen3.6-plus"),
        ("google-ai-studio", "gemini-3.5-flash"),
    ]
    assert candidates[0]["_api_key"] == "gateway-runtime-secret"
    assert candidates[1]["_api_key"] == "gemini-runtime-secret"
    assert "browser-secret" not in json.dumps(payload)


@pytest.mark.asyncio
async def test_proxy_run_scrubs_browser_gateway_model_when_override_disabled() -> None:
    body = json.dumps(
        {
            "input": {"messages": [{"role": "user", "content": "hello"}]},
            "config": {"configurable": {"gateway_model": {"_api_key": "browser-secret"}}},
        }
    ).encode("utf-8")

    updated = await inject_langgraph_model_override_bytes(
        request=_request(),
        body=body,
        method="POST",
        path="runs/stream",
        service_config=_config({"enabled": False}),
        tenant_id="tenant-a",
    )

    payload = json.loads((updated or b"{}").decode("utf-8"))
    assert "gateway_model" not in payload["config"]["configurable"]
    assert "browser-secret" not in json.dumps(payload)
