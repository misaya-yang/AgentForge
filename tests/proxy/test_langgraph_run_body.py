from __future__ import annotations

import asyncio
import json
from types import SimpleNamespace

import pytest

from src.api.deps import AuthContext
from src.proxy.config_loader import ProxyServiceConfig
from src.proxy.langgraph_run_body import (
    apply_langgraph_model_override,
    apply_quota_model_downgrade,
    billing_request_snapshot,
    clear_runtime_model_override_cache,
    inject_resolved_model_override,
    prepare_langgraph_run_body,
    resolve_langgraph_model_override,
    should_prepare_langgraph_run_body,
)


class FakeProviderService:
    def __init__(self) -> None:
        self.calls = 0

    async def get_runtime_provider_config(self, tenant_id: str, _provider_id: str) -> dict:
        self.calls += 1
        assert tenant_id == "tenant-a"
        return {
            "is_enabled": True,
            "runtime_provider": "dashscope",
            "runtime_base_url": "https://example.com",
            "api_key": "secret",
        }


class FakeModelService:
    def __init__(self) -> None:
        self.calls = 0

    async def get_provider_model(self, tenant_id: str, _provider_id: str, _model_id: str) -> dict:
        self.calls += 1
        assert tenant_id == "tenant-a"
        return {"is_enabled": True}


def _request() -> SimpleNamespace:
    return SimpleNamespace(
        app=SimpleNamespace(
            state=SimpleNamespace(
                provider_service=FakeProviderService(),
                model_service=FakeModelService(),
            )
        ),
        state=SimpleNamespace(request_id="req-1", trace_id="trace-1"),
    )


def _user() -> SimpleNamespace:
    return SimpleNamespace(user_id="gateway-user", tenant_id="tenant-a")


def _auth() -> AuthContext:
    return AuthContext(
        user_id="gateway-user",
        tenant_id="tenant-a",
        roles=["user"],
        is_authenticated=True,
    )


def _service() -> ProxyServiceConfig:
    return ProxyServiceConfig(
        service_id="local-agent",
        service_name="LangGraph Agent",
        upstream_url="http://langgraph:8000",
        assistant_id="assistant-1",
        metadata={"adapter_type": "langgraph", "domain_policy": "agent"},
        model_override={
            "enabled": True,
            "provider_id": "dashscope",
            "model_id": "qwen-max",
            "cache_epoch": 3,
        },
    )


def test_should_prepare_skips_run_control_endpoints() -> None:
    service = _service()
    assert should_prepare_langgraph_run_body("POST", "runs", service)
    assert should_prepare_langgraph_run_body("POST", "runs/wait", service)
    assert should_prepare_langgraph_run_body("POST", "threads/t1/runs", service)
    assert should_prepare_langgraph_run_body("POST", "threads/t1/runs/stream", service)
    assert not should_prepare_langgraph_run_body("POST", "threads/t1/runs/run-9/cancel", service)
    assert not should_prepare_langgraph_run_body("POST", "threads/t1/runs/run-9/join", service)
    assert not should_prepare_langgraph_run_body("GET", "threads/t1/runs", service)


def test_prepare_langgraph_run_body_applies_all_sync_mutations_once() -> None:
    body = json.dumps(
        {
            "input": {"messages": [{"role": "user", "content": "hello"}]},
            "config": {
                "configurable": {
                    "gateway_model": {"_api_key": "browser-secret"},
                    "user_id": "attacker",
                }
            },
        }
    ).encode("utf-8")

    updated_body, payload, changed = prepare_langgraph_run_body(
        body=body,
        method="POST",
        path="threads/thread-9/runs/stream",
        request=_request(),
        user=_user(),
        auth=_auth(),
        service_config=_service(),
    )

    assert changed is True
    assert updated_body is not None
    assert payload is not None
    assert payload["assistant_id"] == "assistant-1"
    assert payload["stream_mode"] == ["messages", "updates", "custom"]
    assert payload["stream_subgraphs"] is True
    assert payload["metadata"]["gateway"]["domain_policy"] == "agent"
    configurable = payload["config"]["configurable"]
    assert configurable["user_id"] == "gateway-user"
    assert configurable["thread_id"] == "thread-9"
    assert "gateway_model" not in configurable
    assert json.loads(updated_body.decode("utf-8")) == payload


def test_prepare_overwrites_spoofed_assistant_id() -> None:
    body = json.dumps(
        {
            "assistant_id": "attacker-assistant",
            "input": {"messages": [{"role": "user", "content": "hello"}]},
        }
    ).encode("utf-8")

    _, payload, changed = prepare_langgraph_run_body(
        body=body,
        method="POST",
        path="threads/thread-9/runs/wait",
        request=_request(),
        user=_user(),
        auth=_auth(),
        service_config=_service(),
    )

    assert changed is True
    assert payload is not None
    assert payload["assistant_id"] == "assistant-1"


def test_prepare_unchanged_gateway_identity_skips_encode() -> None:
    body = json.dumps(
        {
            "assistant_id": "assistant-1",
            "input": {"messages": [{"role": "user", "content": "hello"}]},
            "metadata": {"gateway": {"domain_policy": "agent"}},
            "config": {
                "metadata": {
                    "gateway_request_id": "req-1",
                    "gateway_trace_id": "trace-1",
                },
                "configurable": {
                    "user_id": "gateway-user",
                    "tenant_id": "tenant-a",
                    "checkpoint_ns": "tenant-a",
                    "thread_id": "thread-9",
                },
            },
            "stream_mode": ["messages", "updates", "custom"],
            "stream_subgraphs": True,
        }
    ).encode("utf-8")

    updated_body, payload, changed = prepare_langgraph_run_body(
        body=body,
        method="POST",
        path="threads/thread-9/runs/stream",
        request=_request(),
        user=_user(),
        auth=_auth(),
        service_config=_service(),
    )

    assert changed is False
    assert updated_body == body
    assert payload is not None


def test_billing_request_snapshot_strips_gateway_model_secrets() -> None:
    payload = {
        "config": {
            "configurable": {
                "gateway_model": {
                    "_api_key": "secret",
                    "model_id": "qwen-max",
                    "provider_id": "dashscope",
                },
                "user_id": "gateway-user",
            }
        }
    }
    snapshot = billing_request_snapshot(payload)
    assert snapshot is not None
    hints = snapshot["config"]["configurable"]["gateway_model"]
    assert hints["model_id"] == "qwen-max"
    assert hints["provider_id"] == "dashscope"
    assert "_api_key" not in hints
    assert payload["config"]["configurable"]["gateway_model"]["_api_key"] == "secret"


@pytest.mark.asyncio
async def test_runtime_model_override_cache_avoids_repeat_control_plane_reads() -> None:
    clear_runtime_model_override_cache()
    request = _request()
    service = _service()
    payload = {"config": {"configurable": {}}}

    await apply_langgraph_model_override(
        request=request,
        payload=payload,
        service_config=service,
        tenant_id="tenant-a",
    )
    await apply_langgraph_model_override(
        request=request,
        payload={"config": {"configurable": {}}},
        service_config=service,
        tenant_id="tenant-a",
    )

    provider_service = request.app.state.provider_service
    model_service = request.app.state.model_service
    assert provider_service.calls == 1
    assert model_service.calls == 1
    assert payload["config"]["configurable"]["gateway_model"]["model_id"] == "qwen-max"


@pytest.mark.asyncio
async def test_runtime_override_singleflight_dedupes_concurrent_resolves() -> None:
    clear_runtime_model_override_cache()
    request = _request()
    service = _service()

    await asyncio.gather(
        *[
            resolve_langgraph_model_override(
                request=request,
                service_config=service,
                tenant_id="tenant-a",
            )
            for _ in range(8)
        ]
    )

    assert request.app.state.provider_service.calls == 1
    assert request.app.state.model_service.calls == 1


@pytest.mark.asyncio
async def test_resolve_then_inject_after_quota_style_mutation_preserves_override() -> None:
    clear_runtime_model_override_cache()
    request = _request()
    service = _service()
    payload = {
        "config": {"configurable": {"model": "premium-model"}},
    }

    runtime_config = await resolve_langgraph_model_override(
        request=request,
        service_config=service,
        tenant_id="tenant-a",
    )
    payload["config"]["configurable"]["model"] = "downgraded-model"
    assert runtime_config is not None
    inject_resolved_model_override(payload, runtime_config)
    assert apply_quota_model_downgrade(
        payload,
        downgraded_model="downgraded-model",
        requested_model="premium-model",
    )

    configurable = payload["config"]["configurable"]
    assert configurable["model"] == "downgraded-model"
    assert configurable["gateway_model"]["model_id"] == "downgraded-model"
