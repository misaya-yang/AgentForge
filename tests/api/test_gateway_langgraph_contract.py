from __future__ import annotations

import hashlib
import json
from types import SimpleNamespace
from typing import Any

import pytest
from fastapi import HTTPException

from src.api.deps import AuthContext
from src.proxy.config_loader import ProxyServiceConfig
from src.proxy.context_injector import ContextInjector, RequestContext
from src.proxy.langgraph_run_body import (
    clear_runtime_model_override_cache,
    inject_langgraph_gateway_configurable_bytes,
    inject_langgraph_model_override_bytes,
)


@pytest.fixture(autouse=True)
def _reset_runtime_model_override_cache() -> None:
    clear_runtime_model_override_cache()
    yield
    clear_runtime_model_override_cache()


class FakeProviderService:
    def __init__(self, *, with_key: bool = True):
        self.with_key = with_key

    async def get_runtime_provider_config(self, tenant_id: str, provider_id: str) -> dict[str, Any]:
        assert tenant_id == "tenant-a"
        if provider_id != "dashscope-prod":
            raise ValueError(provider_id)
        provider = {
            "is_enabled": True,
            "runtime_provider": "dashscope",
            "runtime_base_url": "https://dashscope.aliyuncs.com/compatible-mode/v1",
        }
        if self.with_key:
            provider["api_key"] = "gateway-runtime-secret"
        return provider


class FakeModelService:
    async def get_provider_model(
        self,
        tenant_id: str,
        provider_id: str,
        model_id: str,
    ) -> dict[str, Any] | None:
        assert tenant_id == "tenant-a"
        if (provider_id, model_id) == ("dashscope-prod", "qwen-max"):
            return {"is_enabled": True}
        return None


def _request(*, provider_has_key: bool = True) -> SimpleNamespace:
    return SimpleNamespace(
        app=SimpleNamespace(
            state=SimpleNamespace(
                provider_service=FakeProviderService(with_key=provider_has_key),
                model_service=FakeModelService(),
            )
        ),
        state=SimpleNamespace(request_id="req-p5", trace_id="trace-p5"),
    )


def _user() -> SimpleNamespace:
    return SimpleNamespace(
        user_id="gateway-user",
        tenant_id="tenant-a",
        tier="premium",
        is_authenticated=True,
        roles=["user"],
    )


def _auth() -> AuthContext:
    return AuthContext(
        user_id="gateway-user",
        tenant_id="tenant-a",
        roles=["user"],
        is_authenticated=True,
    )


def _service(model_override: dict[str, Any]) -> ProxyServiceConfig:
    return ProxyServiceConfig(
        service_id="local-2024-agent",
        service_name="LangGraph Agent",
        upstream_url="http://langgraph-agent:8000",
        graph_id="Agent",
        metadata={"adapter_type": "langgraph"},
        model_override=model_override,
    )


def _override(enabled: bool = True) -> dict[str, Any]:
    return {
        "enabled": enabled,
        "provider_id": "dashscope-prod",
        "model_id": "qwen-max",
        "temperature": 0.1,
        "cache_epoch": 12,
    }


def test_gateway_configurable_overwrites_identity_and_scrubs_browser_secrets() -> None:
    body = json.dumps(
        {
            "input": {"messages": [{"role": "user", "content": "hello"}]},
            "config": {
                "metadata": {"client": "web"},
                "configurable": {
                    "user_id": "attacker-user",
                    "tenant_id": "attacker-tenant",
                    "checkpoint_ns": "attacker-ns",
                    "gateway_model": {"_api_key": "browser-secret"},
                    "provider_api_key": "nested-secret",
                    "locale": "en",
                },
            },
        }
    ).encode("utf-8")

    updated = inject_langgraph_gateway_configurable_bytes(
        request=_request(),
        body=body,
        method="POST",
        path="threads/thread-123/runs/stream",
        user=_user(),
        auth=_auth(),
    )

    payload = json.loads((updated or b"{}").decode("utf-8"))
    configurable = payload["config"]["configurable"]
    metadata = payload["config"]["metadata"]
    assert configurable["user_id"] == "gateway-user"
    assert configurable["tenant_id"] == "tenant-a"
    assert configurable["checkpoint_ns"] == "tenant-a"
    assert configurable["thread_id"] == "thread-123"
    assert configurable["locale"] == "en"
    assert "gateway_model" not in configurable
    assert "provider_api_key" not in configurable
    assert "browser-secret" not in json.dumps(payload)
    assert "nested-secret" not in json.dumps(payload)
    assert metadata["gateway_request_id"] == "req-p5"
    assert metadata["gateway_trace_id"] == "trace-p5"


@pytest.mark.asyncio
async def test_stream_and_wait_paths_receive_same_gateway_model_override() -> None:
    body = json.dumps(
        {
            "input": {"messages": [{"role": "user", "content": "hello"}]},
            "config": {"configurable": {"user_id": "attacker", "tenant_id": "attacker"}},
        }
    ).encode("utf-8")
    service = _service(_override())
    results: list[dict[str, Any]] = []

    for path in ("threads/thread-1/runs/stream", "threads/thread-1/runs/wait"):
        gateway_body = inject_langgraph_gateway_configurable_bytes(
            request=_request(),
            body=body,
            method="POST",
            path=path,
            user=_user(),
            auth=_auth(),
        )
        updated = await inject_langgraph_model_override_bytes(
            request=_request(),
            body=gateway_body,
            method="POST",
            path=path,
            service_config=service,
            tenant_id="tenant-a",
        )
        payload = json.loads((updated or b"{}").decode("utf-8"))
        results.append(payload["config"]["configurable"])

    stream_config, wait_config = results
    assert stream_config["user_id"] == "gateway-user"
    assert wait_config["tenant_id"] == "tenant-a"
    assert stream_config["gateway_model"] == wait_config["gateway_model"]
    gateway_model = stream_config["gateway_model"]
    assert gateway_model["provider_id"] == "dashscope-prod"
    assert gateway_model["model_id"] == "qwen-max"
    assert gateway_model["api_key_fingerprint"] == hashlib.sha256(
        b"gateway-runtime-secret"
    ).hexdigest()[:16]


@pytest.mark.asyncio
async def test_disabled_override_does_not_restore_browser_gateway_model() -> None:
    body = json.dumps(
        {
            "input": {"messages": [{"role": "user", "content": "hello"}]},
            "config": {"configurable": {"gateway_model": {"_api_key": "browser-secret"}}},
        }
    ).encode("utf-8")

    gateway_body = inject_langgraph_gateway_configurable_bytes(
        request=_request(),
        body=body,
        method="POST",
        path="runs/stream",
        user=_user(),
        auth=_auth(),
    )
    updated = await inject_langgraph_model_override_bytes(
        request=_request(),
        body=gateway_body,
        method="POST",
        path="runs/stream",
        service_config=_service(_override(enabled=False)),
        tenant_id="tenant-a",
    )

    payload = json.loads((updated or b"{}").decode("utf-8"))
    assert "gateway_model" not in payload["config"]["configurable"]
    assert "browser-secret" not in json.dumps(payload)


@pytest.mark.asyncio
async def test_missing_provider_key_fails_before_upstream_call() -> None:
    body = json.dumps({"input": {}, "config": {"configurable": {}}}).encode("utf-8")

    with pytest.raises(HTTPException) as exc_info:
        await inject_langgraph_model_override_bytes(
            request=_request(provider_has_key=False),
            body=body,
            method="POST",
            path="runs/wait",
            service_config=_service(_override()),
            tenant_id="tenant-a",
        )

    assert exc_info.value.status_code == 422
    assert exc_info.value.detail == "MODEL_OVERRIDE_API_KEY_MISSING"


def test_internal_service_auth_replaces_client_authorization_and_spoofed_identity() -> None:
    injector = ContextInjector(inject_user_info=True, inject_request_info=True)
    context = RequestContext(
        user_id="gateway-user",
        tenant_id="tenant-a",
        user_tier="premium",
        is_authenticated=True,
        roles=["user"],
        original_headers={
            "Authorization": "Bearer client-token",
            "X-User-Id": "attacker-user",
            "X-Tenant-Id": "attacker-tenant",
        },
    )

    headers = injector.build_headers(
        context,
        service_auth_token="Bearer internal-service-token",
    )

    assert headers["Authorization"] == "Bearer internal-service-token"
    assert headers["X-User-Id"] == "gateway-user"
    assert headers["X-Tenant-Id"] == "tenant-a"
    assert "client-token" not in headers.values()
    assert "attacker-user" not in headers.values()
