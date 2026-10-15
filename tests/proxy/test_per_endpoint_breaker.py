"""Per-endpoint circuit breaker integration tests.

Phase 5e contract: a flaky route on a service must NOT trip the
breaker for sibling routes on the same service. Two failure modes
this guards against:

1. ``/chat/stream`` flakes for 3 requests → ``/chat/stream`` opens, but
   ``/health`` on the same ``ServiceProxy`` still serves traffic.
2. Half-open probe is per-route — recovery on one route does not
   accidentally reset another route's failure counter.

The tests drive ``ServiceProxy.forward`` through a mocked httpx
client so we exercise the real gating + counter wiring, not just the
``_breaker_for`` helper.
"""
from __future__ import annotations

from collections.abc import AsyncIterator
from typing import Any
from unittest.mock import AsyncMock, MagicMock

import httpx
import pytest
from fastapi import HTTPException
from starlette.requests import Request

from ai_gateway_core.proxy.base import (
    CircuitBreakerState,
    ServiceProxy,
    ServiceProxyConfig,
)


# --------------------------------------------------------------------
# Fixtures
# --------------------------------------------------------------------


def _make_request(method: str = "GET", query: str = "") -> Request:
    """Build a minimal Starlette ``Request`` good enough for ``forward``.

    ``forward`` reads: ``method``, ``url.query``, ``headers``, ``state``,
    and (on POST/PUT/PATCH/DELETE) ``request.body()``. We give it a
    plain ASGI scope and an empty receive callable.
    """
    scope = {
        "type": "http",
        "method": method,
        "headers": [],
        "query_string": query.encode(),
        "path": "/",
        "raw_path": b"/",
        "scheme": "http",
        "server": ("test", 80),
    }

    async def _receive() -> dict[str, Any]:
        return {"type": "http.request", "body": b"", "more_body": False}

    return Request(scope, _receive)


class _MockHttpxResponse:
    """Stand-in for ``httpx.Response`` (stream=True) — yields a tiny
    JSON body and short-circuits ``aiter_bytes``.

    We only need the attributes ``ServiceProxy._do`` reads:
    ``status_code``, ``headers``, ``aiter_bytes()``, ``aread()``,
    ``aclose()``.
    """

    def __init__(self, status_code: int = 200, body: bytes = b'{"ok":true}') -> None:
        self.status_code = status_code
        self._body = body
        self.headers = httpx.Headers(
            {
                "content-type": "application/json",
                "content-length": str(len(body)),
            }
        )

    async def aread(self) -> bytes:
        return self._body

    async def aclose(self) -> None:
        return None

    async def aiter_bytes(self) -> AsyncIterator[bytes]:
        yield self._body


def _install_send(proxy: ServiceProxy, behaviour: list[Any]) -> MagicMock:
    """Patch the proxy's underlying httpx client so ``send()`` returns
    or raises items from ``behaviour`` in order. Each entry is either
    an ``Exception`` instance (raised) or a ``_MockHttpxResponse``
    (returned). After the list is drained, the last entry repeats.
    """
    fake_client = MagicMock(spec=httpx.AsyncClient)

    def _build_request(*_: Any, **__: Any) -> MagicMock:
        return MagicMock()

    fake_client.build_request = _build_request

    idx = {"i": 0}

    async def _send(*_: Any, **__: Any) -> Any:
        i = idx["i"]
        item = behaviour[i] if i < len(behaviour) else behaviour[-1]
        idx["i"] = i + 1
        if isinstance(item, Exception):
            raise item
        return item

    fake_client.send = _send

    async def _get_client() -> httpx.AsyncClient:
        return fake_client  # type: ignore[return-value]

    proxy._get_client = _get_client  # type: ignore[method-assign]

    async def _reset_client() -> None:
        return None

    proxy._reset_client = _reset_client  # type: ignore[method-assign]

    return fake_client


@pytest.fixture
def proxy() -> ServiceProxy:
    return ServiceProxy(
        ServiceProxyConfig(
            name="test-service",
            base_url="http://upstream",
            breaker_threshold=3,
        )
    )


# --------------------------------------------------------------------
# Tests
# --------------------------------------------------------------------


def test_breaker_for_returns_distinct_per_pattern(proxy: ServiceProxy) -> None:
    cb_a = proxy._breaker_for("/chat/stream")
    cb_b = proxy._breaker_for("/health")
    assert cb_a is not cb_b
    assert cb_a.name.endswith(":/chat/stream")
    assert cb_b.name.endswith(":/health")


def test_breaker_for_caches(proxy: ServiceProxy) -> None:
    """Repeated lookups for the same pattern return the same instance —
    otherwise failure counters reset between requests."""
    cb1 = proxy._breaker_for("/chat/stream")
    cb2 = proxy._breaker_for("/chat/stream")
    assert cb1 is cb2


def test_global_breaker_alias_still_present(proxy: ServiceProxy) -> None:
    """The ``proxy.breaker`` property must keep working for legacy
    callers — no AttributeError after the per-route refactor."""
    assert proxy.breaker is proxy._global_breaker
    # The global breaker is independent of any per-route breaker.
    cb_route = proxy._breaker_for("/chat/stream")
    assert cb_route is not proxy.breaker


@pytest.mark.asyncio
async def test_failures_on_route_a_do_not_trip_route_b(
    proxy: ServiceProxy,
) -> None:
    """The headline guarantee. Route A flakes 3 times, route B should
    still serve traffic."""
    # Three transport failures on route A, then a 200 on route B.
    _install_send(
        proxy,
        [
            httpx.ConnectError("boom"),
            httpx.ConnectError("boom"),
            httpx.ConnectError("boom"),
            httpx.ConnectError("boom"),  # retry of failure #3
            httpx.ConnectError("boom"),
            httpx.ConnectError("boom"),
            _MockHttpxResponse(200),  # route B's first request
        ],
    )

    route_a = "/chat/stream"
    route_b = "/health"

    # Three failed attempts on A trip its breaker.
    for _ in range(3):
        with pytest.raises(HTTPException) as exc:
            await proxy.forward(
                _make_request("GET"),
                user_headers={},
                upstream_path=route_a,
            )
        assert exc.value.status_code == 502

    cb_a = proxy._breaker_for(route_a)
    cb_b = proxy._breaker_for(route_b)
    assert cb_a.state == CircuitBreakerState.OPEN
    assert cb_b.state == CircuitBreakerState.CLOSED

    # Route B request should pass straight through — no 503 from a
    # poisoned global breaker.
    resp = await proxy.forward(
        _make_request("GET"), user_headers={}, upstream_path=route_b
    )
    assert resp.status_code == 200

    # Global breaker also stayed closed (per-route keying isolates it).
    assert proxy._global_breaker.state == CircuitBreakerState.CLOSED


@pytest.mark.asyncio
async def test_open_route_blocks_subsequent_requests_with_503(
    proxy: ServiceProxy,
) -> None:
    """Once route A is OPEN, the next request grabs the half-open probe
    slot and the *one after that* is rejected with 503 — sibling routes
    are unaffected."""
    _install_send(
        proxy,
        [
            httpx.ConnectError("boom"),
            httpx.ConnectError("boom"),
            httpx.ConnectError("boom"),
            httpx.ConnectError("boom"),
            httpx.ConnectError("boom"),
            httpx.ConnectError("boom"),
        ],
    )

    route_a = "/chat/stream"

    for _ in range(3):
        with pytest.raises(HTTPException):
            await proxy.forward(
                _make_request("GET"),
                user_headers={},
                upstream_path=route_a,
            )

    cb_a = proxy._breaker_for(route_a)
    assert cb_a.state == CircuitBreakerState.OPEN

    # The breaker now has a probe slot. First request post-OPEN will
    # claim it and try to forward (failing again on our mocked send),
    # the second concurrent request would 503 — we simulate by directly
    # checking the gate:
    cb_a.gate()  # claim probe slot
    with pytest.raises(HTTPException) as exc:
        cb_a.gate()  # second caller while probe in flight
    assert exc.value.status_code == 503


@pytest.mark.asyncio
async def test_half_open_probe_is_per_route(proxy: ServiceProxy) -> None:
    """Probe success on route A must not reset route B's fail counter
    (and vice versa)."""
    cb_a = proxy._breaker_for("/chat/stream")
    cb_b = proxy._breaker_for("/health")

    # Trip A.
    cb_a.on_failure()
    cb_a.on_failure()
    cb_a.on_failure()
    assert cb_a.state == CircuitBreakerState.OPEN

    # Pile up 2 fails on B (just below threshold).
    cb_b.on_failure()
    cb_b.on_failure()
    assert cb_b.state == CircuitBreakerState.CLOSED
    assert cb_b.store.get() == (2, False)

    # Successful probe on A.
    cb_a.gate()
    cb_a.on_response(200)
    assert cb_a.state == CircuitBreakerState.CLOSED

    # B's counter is unchanged.
    assert cb_b.store.get() == (2, False)


@pytest.mark.asyncio
async def test_route_b_recovery_does_not_close_route_a(
    proxy: ServiceProxy,
) -> None:
    """Reverse direction: route B success must not close route A's
    open breaker. Each route owns its own state machine."""
    cb_a = proxy._breaker_for("/chat/stream")
    cb_b = proxy._breaker_for("/health")

    cb_a.on_failure()
    cb_a.on_failure()
    cb_a.on_failure()
    assert cb_a.state == CircuitBreakerState.OPEN

    # Some 200s on B.
    cb_b.on_response(200)
    cb_b.on_response(200)

    assert cb_a.state == CircuitBreakerState.OPEN


@pytest.mark.asyncio
async def test_uuid_paths_share_breaker_via_template(
    proxy: ServiceProxy,
) -> None:
    """Two requests to ``/sessions/<uuid1>`` and ``/sessions/<uuid2>``
    must share the SAME breaker (both collapse to ``/sessions/{id}``).
    Otherwise the breaker dict explodes per UUID and never trips."""
    path1 = "/api/v1/assistant/sessions/abc12345-1234-1234-1234-1234567890ab"
    path2 = "/api/v1/assistant/sessions/deadbeef-cafe-babe-face-01234567890a"

    cb1 = proxy._breaker_for(_extract(path1))
    cb2 = proxy._breaker_for(_extract(path2))
    assert cb1 is cb2


def _extract(path: str) -> str:
    """Helper that mirrors what ``forward`` does internally."""
    from ai_gateway_core.proxy.route_pattern import extract_route_pattern

    return extract_route_pattern(path)


@pytest.mark.asyncio
async def test_successful_response_closes_only_that_routes_breaker(
    proxy: ServiceProxy,
) -> None:
    """A 200 response for route A must close A's breaker but leave
    route B's breaker exactly as it was."""
    # Pre-pile 2 fails on each.
    cb_a = proxy._breaker_for("/chat/stream")
    cb_b = proxy._breaker_for("/health")
    cb_a.on_failure()
    cb_a.on_failure()
    cb_b.on_failure()
    cb_b.on_failure()
    assert cb_a.store.get() == (2, False)
    assert cb_b.store.get() == (2, False)

    # Now a successful request to A.
    _install_send(proxy, [_MockHttpxResponse(200)])
    resp = await proxy.forward(
        _make_request("GET"),
        user_headers={},
        upstream_path="/chat/stream",
    )
    assert resp.status_code == 200

    # A reset, B unchanged.
    assert cb_a.store.get() == (0, False)
    assert cb_b.store.get() == (2, False)
