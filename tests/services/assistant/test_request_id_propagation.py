"""End-to-end ``X-Request-Id`` propagation tests.

Locks the contract:

1. Gateway middleware preserves a valid inbound ``X-Request-Id`` instead of
   minting a fresh UUID.
2. Gateway middleware rejects malformed inbound IDs (too long / unsafe
   chars) and falls back to a fresh UUID.
3. ``ServiceProxy._build_headers`` injects the gateway's
   ``request.state.request_id`` into the upstream request when no inbound
   ``X-Request-Id`` was sent.
4. ``RequestIDMiddleware`` (used by AS / KS) reads the incoming header,
   binds it to ``request.state`` + ``REQUEST_ID_CTX``, and echoes it back
   on the response.
5. When called with no incoming ID, ``RequestIDMiddleware`` mints
   ``svc-<uuid>`` so log aggregators can tell direct calls from
   gateway-fronted ones.
"""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest
from fastapi import FastAPI, Request
from fastapi.testclient import TestClient

from ai_gateway_core.proxy import REQUEST_ID_CTX, RequestIDMiddleware
from ai_gateway_core.proxy.base import ServiceProxy, ServiceProxyConfig


# ---------------------------------------------------------------------------
# RequestIDMiddleware (ai_gateway_core)
# ---------------------------------------------------------------------------


def _make_app_with_middleware() -> FastAPI:
    app = FastAPI()
    app.add_middleware(RequestIDMiddleware)

    @app.get("/probe")
    async def probe(req: Request):
        return {
            "state_request_id": getattr(req.state, "request_id", None),
            "ctx_request_id": REQUEST_ID_CTX.get(),
        }

    return app


def test_middleware_preserves_inbound_request_id():
    client = TestClient(_make_app_with_middleware())
    resp = client.get("/probe", headers={"X-Request-Id": "abc-123_test"})
    assert resp.status_code == 200, f"unexpected status {resp.status_code}: {resp.text}"
    assert resp.json()["state_request_id"] == "abc-123_test"
    assert resp.json()["ctx_request_id"] == "abc-123_test"
    assert resp.headers["X-Request-Id"] == "abc-123_test"


def test_middleware_mints_svc_prefix_when_no_inbound_header():
    client = TestClient(_make_app_with_middleware())
    resp = client.get("/probe")
    assert resp.status_code == 200
    minted = resp.json()["state_request_id"]
    assert minted.startswith("svc-"), (
        "Direct (non-gateway) calls must mint svc-<uuid> so they're "
        "distinguishable in log aggregations from gateway-fronted ones."
    )
    assert resp.headers["X-Request-Id"] == minted


@pytest.mark.parametrize("malformed", [
    "x" * 65,  # too long
    "has spaces",
    "has;semi",
    "has<script>",
    "",
])
def test_middleware_rejects_malformed_inbound_id_and_mints_fresh(malformed):
    client = TestClient(_make_app_with_middleware())
    resp = client.get("/probe", headers={"X-Request-Id": malformed})
    assert resp.status_code == 200
    out_id = resp.json()["state_request_id"]
    assert out_id != malformed, (
        f"Malformed inbound ID {malformed!r} must be rejected and "
        "replaced with a freshly-minted ID."
    )
    assert out_id.startswith("svc-")


def test_middleware_resets_contextvar_after_request():
    """ContextVar must NOT leak between requests in the same worker."""
    client = TestClient(_make_app_with_middleware())
    client.get("/probe", headers={"X-Request-Id": "first-call"})
    # Outside any request, contextvar default should be empty
    assert REQUEST_ID_CTX.get() == ""


# ---------------------------------------------------------------------------
# ServiceProxy._build_headers — gateway → upstream
# ---------------------------------------------------------------------------


def _make_proxy() -> ServiceProxy:
    return ServiceProxy(
        ServiceProxyConfig(
            name="test-svc",
            base_url="http://test-svc:9999",
        ),
    )


def test_proxy_injects_gateway_request_id_when_client_didnt_send_one():
    """Gateway middleware sets request.state.request_id; if the inbound
    HTTP request didn't carry X-Request-Id, the proxy must still forward
    the gateway's own ID upstream so logs correlate."""
    proxy = _make_proxy()

    request = MagicMock()
    request.headers = {"content-type": "application/json"}  # NO X-Request-Id
    request.state = SimpleNamespace(request_id="gw-generated-uuid-1234")

    headers = proxy._build_headers(
        request,
        user_headers={"X-User-Id": "u1"},
        upstream_path="/probe",
        body=b"",
    )

    # Case-insensitive lookup since dict-keys preserve case
    keys_lower = {k.lower(): v for k, v in headers.items()}
    assert keys_lower.get("x-request-id") == "gw-generated-uuid-1234"
    assert keys_lower.get("x-user-id") == "u1"


def test_proxy_preserves_inbound_request_id_when_client_sent_one():
    """If the client also sent X-Request-Id (e.g. a service-to-service
    call going through the gateway), the original value rides through —
    the proxy doesn't override with state.request_id."""
    proxy = _make_proxy()

    request = MagicMock()
    request.headers = {
        "content-type": "application/json",
        "x-request-id": "client-sent-id-9999",
    }
    request.state = SimpleNamespace(request_id="gw-generated-uuid-1234")

    headers = proxy._build_headers(
        request,
        user_headers={"X-User-Id": "u1"},
        upstream_path="/probe",
        body=b"",
    )

    keys_lower = {k.lower(): v for k, v in headers.items()}
    # Inbound wins because request.headers is copied first
    assert keys_lower.get("x-request-id") == "client-sent-id-9999"


def test_proxy_no_request_id_when_neither_inbound_nor_state():
    """If neither path provided one (legacy path or pre-middleware test),
    the proxy doesn't fabricate one. Downstream middleware will mint
    its own svc-<uuid>."""
    proxy = _make_proxy()

    request = MagicMock()
    request.headers = {"content-type": "application/json"}
    # No request.state.request_id — Mock without the attr would still answer
    # truthy via spec-less Mock; explicitly set to None:
    request.state = SimpleNamespace()  # no request_id attr at all

    headers = proxy._build_headers(
        request,
        user_headers={"X-User-Id": "u1"},
        upstream_path="/probe",
        body=b"",
    )

    keys_lower = {k.lower(): v for k, v in headers.items()}
    assert "x-request-id" not in keys_lower
