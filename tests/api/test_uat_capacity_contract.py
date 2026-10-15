from __future__ import annotations

import asyncio

import httpx
import pytest

from src.proxy.config_loader import ProxyServiceConfig
from src.proxy.context_injector import RequestContext
from src.proxy.transparent_proxy import ProxyRequest, TransparentProxy


@pytest.mark.asyncio
async def test_three_of_four_no_model_cost_requests_admit_with_capacity_headers(monkeypatch):
    monkeypatch.setenv("ADMISSION_TENANT_SHARE_RATIO", "1.0")
    config = ProxyServiceConfig(
        service_id="local-2024-agent",
        service_name="LangGraph Agent",
        upstream_url="http://langgraph-agent:8000",
        metadata={
            "capacity": {
                "upstream_group": "langgraph_agent",
                "concurrency_limit": 3,
                "queue_max": 0,
                "queue_timeout_ms": 100,
            }
        },
    )
    class Loader:
        database = None
        redis = None

        async def get_config(self, _name):
            return config

    loader = Loader()
    proxy = TransparentProxy(config_loader=loader)

    async def fake_request(self, *args, **kwargs):
        await asyncio.sleep(0.05)
        return httpx.Response(
            200,
            headers={"content-type": "application/json"},
            content=b'{"ok": true}',
            request=httpx.Request("POST", "http://langgraph-agent:8000/assistants/search"),
        )

    monkeypatch.setattr(httpx.AsyncClient, "request", fake_request)
    async def available(_config):
        return {
            "availability_status": "available",
            "available_upstreams": [config.upstream_url],
            "last_health_error": None,
        }

    monkeypatch.setattr(proxy, "get_service_availability", available)

    async def send(index: int):
        return await proxy.proxy(
            ProxyRequest(
                service_name="local-2024-agent",
                path="assistants/search",
                method="POST",
                body=b'{"limit": 5}',
                context=RequestContext(
                    user_id=f"user-{index}",
                    tenant_id="tenant-a",
                    request_id=f"req-{index}",
                ),
            )
        )

    responses = await asyncio.gather(*(send(index) for index in range(4)))
    status_codes = sorted(response.status_code for response in responses)
    assert status_codes == [200, 200, 200, 503]
    admitted = [response for response in responses if response.status_code == 200]
    denied = [response for response in responses if response.status_code == 503][0]
    assert all(response.headers["X-Gateway-Capacity-Key"] for response in admitted)
    assert denied.headers["X-Gateway-Capacity-Key"] == "upstream.langgraph_agent"
    assert b"GATEWAY_CAPACITY_EXHAUSTED" in (denied.body or b"")
