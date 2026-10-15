"""End-to-end test: gateway-style FastAPI app + ServiceProxy must
forward W3C ``traceparent`` to the upstream so the chain stays unbroken.

Uses ``httpx.MockTransport`` to capture the request the proxy emits to
its upstream and assert the traceparent header is set + matches the
trace_id of the inbound server span.
"""

from __future__ import annotations

import pytest
from fastapi import FastAPI, Request
from fastapi.testclient import TestClient

from ai_gateway_core.proxy.base import ServiceProxy, ServiceProxyConfig
from ai_gateway_core.tracing import OTelInboundMiddleware
from ai_gateway_core.tracing.init import _reset_for_tests


def _install_in_memory_provider():
    from opentelemetry import trace
    from opentelemetry.sdk.resources import Resource, SERVICE_NAME
    from opentelemetry.sdk.trace import TracerProvider
    from opentelemetry.sdk.trace.export import SimpleSpanProcessor
    from opentelemetry.sdk.trace.export.in_memory_span_exporter import (
        InMemorySpanExporter,
    )

    exporter = InMemorySpanExporter()
    provider = TracerProvider(resource=Resource.create({SERVICE_NAME: "test-svc"}))
    provider.add_span_processor(SimpleSpanProcessor(exporter))
    trace._TRACER_PROVIDER_SET_ONCE._done = False  # type: ignore[attr-defined]
    trace._TRACER_PROVIDER = None  # type: ignore[attr-defined]
    trace.set_tracer_provider(provider)
    return exporter


@pytest.fixture(autouse=True)
def _reset_state():
    _reset_for_tests()
    yield
    _reset_for_tests()


@pytest.fixture
def memory_exporter():
    yield _install_in_memory_provider()


def test_proxy_forwards_traceparent(memory_exporter) -> None:
    """A request that lands on the proxied route must reach the upstream
    with a ``traceparent`` header tied to the inbound trace context."""
    import httpx

    captured: dict[str, str] = {}

    def upstream_handler(request: httpx.Request) -> httpx.Response:
        # Capture all headers on the upstream-bound request so we can
        # assert traceparent arrived.
        for k, v in request.headers.items():
            captured[k.lower()] = v
        return httpx.Response(200, json={"upstream": "ok"})

    transport = httpx.MockTransport(upstream_handler)
    proxy_cfg = ServiceProxyConfig(
        name="test-upstream",
        base_url="http://test-upstream:9999",
    )
    proxy = ServiceProxy(proxy_cfg)

    # Replace the lazily-built httpx client with one that uses our
    # MockTransport. ServiceProxy._client is None until first call;
    # we pre-populate it. The instrumentor still gets a chance to wrap
    # via ``instrument_httpx_client`` because we use a real client.
    pre_built = httpx.AsyncClient(
        base_url=proxy_cfg.base_url,
        transport=transport,
    )
    proxy._client = pre_built

    app = FastAPI()
    app.add_middleware(OTelInboundMiddleware)

    @app.get("/proxied")
    async def proxied(request: Request):
        return await proxy.forward(
            request, user_headers={}, upstream_path="/api/v1/test"
        )

    client = TestClient(app)

    inbound_trace_id = "4bf92f3577b34da6a3ce929d0e0e4736"
    inbound_span_id = "00f067aa0ba902b7"
    traceparent_in = f"00-{inbound_trace_id}-{inbound_span_id}-01"

    resp = client.get("/proxied", headers={"traceparent": traceparent_in})
    assert resp.status_code == 200, resp.text

    # The upstream call must have received a ``traceparent`` header.
    # ``traceparent`` may have been written by either the explicit
    # ``_build_headers`` forward path OR by the httpx instrumentor —
    # either is correct for end-to-end propagation. It must reference
    # the same trace_id we sent in.
    assert "traceparent" in captured
    forwarded_tp = captured["traceparent"]
    assert inbound_trace_id in forwarded_tp


def test_proxy_forwards_traceparent_without_inbound(memory_exporter) -> None:
    """Even without an inbound traceparent, the proxy must emit one
    based on the freshly-minted server span — otherwise upstream
    services can't link back to the gateway request."""
    import httpx

    captured: dict[str, str] = {}

    def upstream_handler(request: httpx.Request) -> httpx.Response:
        for k, v in request.headers.items():
            captured[k.lower()] = v
        return httpx.Response(200, json={"upstream": "ok"})

    transport = httpx.MockTransport(upstream_handler)
    proxy_cfg = ServiceProxyConfig(
        name="test-upstream",
        base_url="http://test-upstream:9999",
    )
    proxy = ServiceProxy(proxy_cfg)
    proxy._client = httpx.AsyncClient(
        base_url=proxy_cfg.base_url, transport=transport
    )

    app = FastAPI()
    app.add_middleware(OTelInboundMiddleware)

    @app.get("/proxied")
    async def proxied(request: Request):
        return await proxy.forward(
            request, user_headers={}, upstream_path="/api/v1/test"
        )

    client = TestClient(app)
    resp = client.get("/proxied")
    assert resp.status_code == 200

    assert "traceparent" in captured, (
        "Proxy must inject traceparent even when inbound request had none"
    )
