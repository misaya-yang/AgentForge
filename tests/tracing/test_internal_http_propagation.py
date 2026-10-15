from __future__ import annotations

from collections.abc import Awaitable, Callable

import pytest
from ai_gateway_core.proxy.request_id_middleware import REQUEST_ID_CTX
from ai_gateway_core.tracing import internal_http_headers

from src.core.middleware.streaming import RequestContextBridgeMiddleware


def test_internal_headers_propagate_trace_and_bounded_correlation(monkeypatch) -> None:
    def inject(carrier: dict[str, str]) -> None:
        carrier["traceparent"] = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01"
        carrier["tracestate"] = "vendor=value"

    monkeypatch.setattr("opentelemetry.propagate.inject", inject)
    token = REQUEST_ID_CTX.set("request-a")
    try:
        headers = internal_http_headers(
            {"x-service-auth": "opaque"},
            run_id="run-a",
            turn_id="turn-a",
            execution_id="execution-a",
        )
    finally:
        REQUEST_ID_CTX.reset(token)

    assert headers == {
        "x-service-auth": "opaque",
        "x-request-id": "request-a",
        "traceparent": "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
        "tracestate": "vendor=value",
        "x-ai-run-id": "run-a",
        "x-ai-turn-id": "turn-a",
        "x-ai-execution-id": "execution-a",
    }


def test_internal_headers_reject_log_injection_in_correlation_ids() -> None:
    with pytest.raises(ValueError, match="malformed"):
        internal_http_headers(run_id="run-a\nsecret=value")


@pytest.mark.asyncio
async def test_request_context_bridge_preserves_id_for_internal_hop_and_resets() -> None:
    observed: dict[str, str] = {}

    async def app(
        _scope: dict,
        _receive: Callable[[], Awaitable[dict]],
        send: Callable[[dict], Awaitable[None]],
    ) -> None:
        observed.update(internal_http_headers())
        await send({"type": "http.response.start", "status": 200, "headers": []})
        await send({"type": "http.response.body", "body": b""})

    async def receive() -> dict:
        return {"type": "http.request", "body": b"", "more_body": False}

    async def send(_message: dict) -> None:
        return None

    middleware = RequestContextBridgeMiddleware(app)
    await middleware(
        {
            "type": "http",
            "state": {"request_id": "gateway-request-a"},
            "headers": [],
        },
        receive,
        send,
    )

    assert observed["x-request-id"] == "gateway-request-a"
    assert REQUEST_ID_CTX.get() == ""


def test_internal_headers_mint_safe_request_id_outside_request_context() -> None:
    headers = internal_http_headers()
    assert headers["x-request-id"].startswith("svc-")
    assert " " not in headers["x-request-id"]
