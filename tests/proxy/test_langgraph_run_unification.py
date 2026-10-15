from __future__ import annotations

import json
from types import SimpleNamespace

import pytest

from src.api.deps import AuthContext
from src.proxy.config_loader import ProxyServiceConfig
from src.proxy.langgraph_run_body import (
    merge_gateway_domain_policy_metadata,
    prepare_and_finalize_langgraph_run_payload,
    prepare_langgraph_run_body_for_passthrough,
    prepare_langgraph_run_payload,
    resolve_domain_policy,
)


class FakeProviderService:
    async def get_runtime_provider_config(self, tenant_id: str, _provider_id: str) -> dict:
        return {
            "is_enabled": True,
            "runtime_provider": "dashscope",
            "runtime_base_url": "https://example.com",
            "api_key": "secret",
        }


class FakeModelService:
    async def get_provider_model(self, tenant_id: str, _provider_id: str, _model_id: str) -> dict:
        return {"is_enabled": True}


class FakeConfigLoader:
    async def get_config(self, service_name: str) -> ProxyServiceConfig | None:
        if service_name in {"langgraph", "langgraph-agent", "assistant-1", "local-agent"}:
            return _service()
        return None


def _http_request(*, with_config_loader: bool = False) -> SimpleNamespace:
    return SimpleNamespace(
        app=SimpleNamespace(
            state=SimpleNamespace(
                provider_service=FakeProviderService(),
                model_service=FakeModelService(),
                proxy_config_loader=FakeConfigLoader() if with_config_loader else None,
            )
        ),
        state=SimpleNamespace(request_id="req-1", trace_id="trace-1"),
    )


def _user() -> SimpleNamespace:
    return SimpleNamespace(user_id="gateway-user", tenant_id="tenant-a", tier="premium")


def _auth() -> AuthContext:
    return AuthContext(
        user_id="gateway-user",
        tenant_id="tenant-a",
        roles=["user"],
        is_authenticated=True,
    )


def _service(*, domain_policy: str = "agent") -> ProxyServiceConfig:
    return ProxyServiceConfig(
        service_id="local-agent",
        service_name="LangGraph Agent",
        upstream_url="http://langgraph:8000",
        assistant_id="assistant-1",
        metadata={"adapter_type": "langgraph", "domain_policy": domain_policy},
        model_override={
            "enabled": True,
            "provider_id": "dashscope",
            "model_id": "qwen-max",
            "cache_epoch": 3,
        },
    )


def test_resolve_domain_policy_prefers_service_config_over_assistant() -> None:
    policy = resolve_domain_policy(
        service_config=_service(domain_policy="service-policy"),
        assistant_payload={"metadata": {"domain_policy": "assistant-policy"}},
    )
    assert policy == "service-policy"


def test_merge_gateway_domain_policy_uses_assistant_fallback() -> None:
    merged = merge_gateway_domain_policy_metadata(
        metadata=None,
        service_config=ProxyServiceConfig(
            service_id="langgraph",
            service_name="langgraph",
            upstream_url="",
            metadata={"adapter_type": "langgraph"},
        ),
        assistant_payload={"metadata": {"domain_policy": "agent"}},
    )
    assert merged is not None
    assert merged["gateway"]["domain_policy"] == "agent"


def test_prepare_langgraph_run_payload_scrubs_and_injects_stream_defaults() -> None:
    payload = {
        "assistant_id": "assistant-1",
        "input": {"messages": [{"role": "user", "content": "hi"}]},
        "config": {
            "configurable": {
                "gateway_model": {"_api_key": "browser-secret"},
                "user_id": "attacker",
            }
        },
    }
    prepared, changed = prepare_langgraph_run_payload(
        payload,
        method="POST",
        path="threads/thread-9/runs/stream",
        request=_http_request(),
        user=_user(),
        auth=_auth(),
        service_config=_service(),
    )
    assert changed is True
    assert prepared["stream_mode"] == ["messages", "updates", "custom"]
    assert prepared["stream_subgraphs"] is True
    assert prepared["metadata"]["gateway"]["domain_policy"] == "agent"
    assert "gateway_model" not in prepared["config"]["configurable"]
    assert prepared["config"]["configurable"]["user_id"] == "gateway-user"
    assert prepared["config"]["configurable"]["user_tier"] == "premium"
    assert prepared["config"]["configurable"]["thread_id"] == "thread-9"


@pytest.mark.asyncio
async def test_prepare_and_finalize_injects_gateway_model() -> None:
    payload = {
        "assistant_id": "assistant-1",
        "input": {"messages": [{"role": "user", "content": "hi"}]},
        "config": {"configurable": {}},
    }
    prepared = await prepare_and_finalize_langgraph_run_payload(
        payload,
        method="POST",
        path="threads/thread-9/runs/wait",
        request=_http_request(),
        user=_user(),
        auth=_auth(),
        service_config=_service(),
    )
    gateway_model = prepared["config"]["configurable"]["gateway_model"]
    assert gateway_model["model_id"] == "qwen-max"


@pytest.mark.asyncio
async def test_prepare_run_payload_without_http_request_scrubs_gateway_model() -> None:
    from src.adapters.langgraph_proxy import LangGraphProxy

    proxy = LangGraphProxy.__new__(LangGraphProxy)
    payload = {
        "assistant_id": "assistant-1",
        "input": {"messages": [{"role": "user", "content": "hi"}]},
        "config": {
            "configurable": {
                "gateway_model": {"_api_key": "browser-secret"},
                "api_key": "leak",
            }
        },
    }
    prepared = await proxy._prepare_run_payload(
        payload=payload,
        path="threads/thread-9/runs/wait",
        user=_user(),
        http_request=None,
    )
    configurable = prepared["config"]["configurable"]
    assert "gateway_model" not in configurable
    assert "api_key" not in configurable
    assert configurable["user_id"] == "gateway-user"


@pytest.mark.asyncio
async def test_passthrough_preparer_reencodes_prepared_body() -> None:
    body = json.dumps(
        {
            "assistant_id": "assistant-1",
            "input": {"messages": [{"role": "user", "content": "hi"}]},
            "config": {"configurable": {"gateway_model": {"_api_key": "browser-secret"}}},
        }
    ).encode("utf-8")

    updated = await prepare_langgraph_run_body_for_passthrough(
        body,
        method="POST",
        path="threads/thread-9/runs/stream",
        request=_http_request(with_config_loader=True),
        user=_user(),
        auth=_auth(),
    )
    assert updated is not None
    parsed = json.loads(updated.decode("utf-8"))
    assert parsed["stream_subgraphs"] is True
    assert "gateway_model" in parsed["config"]["configurable"]
    assert parsed["config"]["configurable"]["gateway_model"]["model_id"] == "qwen-max"