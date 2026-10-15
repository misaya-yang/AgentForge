from __future__ import annotations

from fastapi import FastAPI, Request
from fastapi.testclient import TestClient

from src.core.middleware.streaming import StreamingTracingConfig, StreamingTracingMiddleware
from src.proxy.context_injector import ContextInjector, RequestContext


def _app() -> FastAPI:
    app = FastAPI()
    app.add_middleware(
        StreamingTracingMiddleware,
        config=StreamingTracingConfig(log_requests=False, log_responses=False),
    )

    @app.get("/probe")
    async def probe(request: Request):
        return {
            "trace_id": getattr(request.state, "trace_id", ""),
            "span_id": getattr(request.state, "span_id", ""),
            "traceparent": getattr(request.state, "traceparent", ""),
        }

    return app


def test_trace_middleware_reuses_safe_traceparent_trace_id():
    client = TestClient(_app())
    trace_id = "4bf92f3577b34da6a3ce929d0e0e4736"
    traceparent = f"00-{trace_id}-00f067aa0ba902b7-01"

    response = client.get("/probe", headers={"traceparent": traceparent})

    assert response.status_code == 200
    body = response.json()
    assert body["trace_id"] == trace_id
    assert body["traceparent"].startswith(f"00-{trace_id}-")
    assert response.headers["traceparent"].startswith(f"00-{trace_id}-")


def test_trace_middleware_rejects_unsafe_trace_id_header():
    client = TestClient(_app())

    response = client.get("/probe", headers={"x-trace-id": "bad<script>"})

    assert response.status_code == 200
    body = response.json()
    assert body["trace_id"] != "bad<script>"
    assert len(body["trace_id"]) == 32
    int(body["trace_id"], 16)


def test_context_injector_forwards_traceparent_to_upstream():
    traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
    context = RequestContext(
        request_id="req-1",
        trace_id="4bf92f3577b34da6a3ce929d0e0e4736",
        span_id="00f067aa0ba902b7",
        traceparent=traceparent,
        original_headers={},
    )

    headers = ContextInjector().build_headers(context)

    assert headers["traceparent"] == traceparent
    assert headers["X-Trace-ID"] == context.trace_id
