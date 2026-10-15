"""Tests for ``OTelInboundMiddleware``.

Uses a real ``TracerProvider`` with ``InMemorySpanExporter`` so we can
inspect span attributes / parent context without spinning up a network
collector.
"""

from __future__ import annotations

import pytest
from fastapi import FastAPI, Request
from fastapi.testclient import TestClient

from ai_gateway_core.tracing import OTelInboundMiddleware
from ai_gateway_core.tracing.init import _reset_for_tests


def _install_in_memory_provider():
    """Replace the global tracer provider with an in-memory one.

    Returns the ``InMemorySpanExporter`` so the test can read finished
    spans. The opentelemetry SDK lets you set the global provider once
    per process (subsequent calls log a warning and are ignored) — we
    work around that by always creating a fresh provider through
    ``_force_set_provider`` (private, but it's the same trick the SDK's
    own tests use).
    """
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
    # Force-replace the global provider. ``set_tracer_provider`` warns
    # if called twice, but we don't actually care about the warning in
    # tests; we just want the provider to be ours.
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


def _make_app() -> FastAPI:
    app = FastAPI()
    # Mirror the production stack — RequestID first (executes innermost),
    # OTel second (executes outermost) so OTel sees request_id.
    from ai_gateway_core.proxy import RequestIDMiddleware

    app.add_middleware(OTelInboundMiddleware)
    app.add_middleware(RequestIDMiddleware)

    @app.get("/echo")
    async def echo(request: Request):
        return {
            "traceparent": getattr(request.state, "traceparent", None),
            "request_id": getattr(request.state, "request_id", None),
        }

    @app.get("/boom")
    async def boom():
        raise RuntimeError("kaboom")

    return app


def test_middleware_creates_span_for_request(memory_exporter) -> None:
    app = _make_app()
    client = TestClient(app, raise_server_exceptions=False)

    resp = client.get("/echo")
    assert resp.status_code == 200

    spans = memory_exporter.get_finished_spans()
    assert len(spans) >= 1, "Expected at least one server span"
    span = next((s for s in spans if s.name.startswith("GET")), None)
    assert span is not None
    assert span.name == "GET /echo"
    assert span.attributes.get("http.method") == "GET"
    assert span.attributes.get("http.target") == "/echo"
    # Status is OK for 2xx responses
    assert int(span.attributes.get("http.status_code")) == 200


def test_middleware_extracts_inbound_traceparent(memory_exporter) -> None:
    """A request that arrives with a traceparent must be picked up as
    the parent of the new server span."""
    app = _make_app()
    client = TestClient(app)

    # W3C-compliant traceparent: version-trace_id-parent_id-flags
    inbound_trace_id = "0af7651916cd43dd8448eb211c80319c"
    inbound_span_id = "b7ad6b7169203331"
    traceparent = f"00-{inbound_trace_id}-{inbound_span_id}-01"

    resp = client.get("/echo", headers={"traceparent": traceparent})
    assert resp.status_code == 200

    body = resp.json()
    # The middleware writes the active traceparent to request.state so
    # downstream code can forward it. It must reference the same trace_id
    # we sent in (parent linkage). The span_id portion will be the new
    # server span (so it's different from inbound_span_id).
    assert body["traceparent"] is not None
    assert inbound_trace_id in body["traceparent"]

    spans = memory_exporter.get_finished_spans()
    server_span = next((s for s in spans if s.name == "GET /echo"), None)
    assert server_span is not None
    # The inbound trace_id must match the server span's trace_id (parent
    # context extraction worked).
    actual_trace_id_hex = format(server_span.context.trace_id, "032x")
    assert actual_trace_id_hex == inbound_trace_id


def test_middleware_records_request_id_attribute(memory_exporter) -> None:
    """``request.id`` attribute on the span lets log↔trace correlation
    queries match by the same key."""
    app = _make_app()
    client = TestClient(app)

    resp = client.get("/echo", headers={"x-request-id": "test-req-12345"})
    assert resp.status_code == 200

    spans = memory_exporter.get_finished_spans()
    server_span = next((s for s in spans if s.name == "GET /echo"), None)
    assert server_span is not None
    assert server_span.attributes.get("request.id") == "test-req-12345"


def test_middleware_marks_5xx_as_error(memory_exporter) -> None:
    """A 500-class response must mark the span status ERROR."""
    app = FastAPI()
    app.add_middleware(OTelInboundMiddleware)

    @app.get("/fivehundred")
    async def fivehundred():
        from fastapi.responses import JSONResponse
        return JSONResponse({"error": "boom"}, status_code=500)

    client = TestClient(app)
    resp = client.get("/fivehundred")
    assert resp.status_code == 500

    spans = memory_exporter.get_finished_spans()
    span = next((s for s in spans if s.name.startswith("GET")), None)
    assert span is not None
    from opentelemetry.trace import StatusCode
    assert span.status.status_code == StatusCode.ERROR


def test_middleware_records_exception(memory_exporter) -> None:
    """An uncaught exception inside the handler must be recorded on the
    span and the span status must be ERROR."""
    app = _make_app()
    client = TestClient(app, raise_server_exceptions=False)

    resp = client.get("/boom")
    assert resp.status_code == 500

    spans = memory_exporter.get_finished_spans()
    span = next((s for s in spans if s.name == "GET /boom"), None)
    assert span is not None

    from opentelemetry.trace import StatusCode
    assert span.status.status_code == StatusCode.ERROR
    # The exception should be recorded as an event on the span
    event_names = [e.name for e in span.events]
    assert "exception" in event_names
