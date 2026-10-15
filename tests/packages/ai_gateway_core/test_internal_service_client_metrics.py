from __future__ import annotations

import httpx
import pytest
from ai_gateway_core.comm import client as client_module
from ai_gateway_core.comm.client import (
    InternalServiceClient,
    InternalServiceClientConfig,
    configure_service_metrics,
)

from src.core.observability.metrics import get_metrics
from src.services.metrics.collector import get_service_metrics


@pytest.fixture(autouse=True)
def isolated_metric_binding(monkeypatch):
    monkeypatch.setattr(client_module, "_SERVICE_METRICS", None)
    monkeypatch.setattr(client_module, "_SERVICE_METRICS_FACTORY", None)


@pytest.mark.asyncio
async def test_internal_service_client_records_low_cardinality_metrics() -> None:
    configure_service_metrics(get_service_metrics)

    async def handler(_request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json={"ok": True})

    service_name = "metrics-test-service"
    client = InternalServiceClient(
        InternalServiceClientConfig(
            name=service_name,
            base_url="http://metrics.test",
        ),
        transport=httpx.MockTransport(handler),
    )

    try:
        await client.request_json("GET", "/health")
    finally:
        await client.close()

    metrics = get_metrics()
    counter = metrics._counters["service_call_total"]
    assert counter.get(service=service_name, method="GET", status="200") >= 1
    assert "path" not in counter.label_names
    assert metrics._gauges["service_call_inflight"].get(service=service_name) == 0


@pytest.mark.asyncio
@pytest.mark.parametrize("broken_factory", [False, True])
async def test_internal_service_transport_does_not_require_a_gateway_collector(broken_factory):
    if broken_factory:

        def unavailable():
            raise RuntimeError("collector is unavailable")

        configure_service_metrics(unavailable)
    client = InternalServiceClient(
        InternalServiceClientConfig(name="unobserved-test", base_url="http://metrics.test"),
        transport=httpx.MockTransport(lambda _request: httpx.Response(200, json={"ok": True})),
    )
    try:
        assert await client.request_json("GET", "/health") == {"ok": True}
    finally:
        await client.close()


def test_disabling_metrics_resets_an_existing_binding():
    configure_service_metrics(get_service_metrics)
    assert client_module._service_metrics()
    configure_service_metrics(None)
    assert client_module._service_metrics() is None
