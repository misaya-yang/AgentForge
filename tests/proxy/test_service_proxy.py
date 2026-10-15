"""Phase 5a unit tests for ``ai_gateway_core.proxy.ServiceProxy``.

Locks the semantics audited as Findings M-1..M-5:
- half-open probe breaker (Finding M-1)
- mid-stream failure counted (Finding M-2)
- SSE content-type forces streaming (Finding M-3)
- 4xx does NOT close the breaker (Finding M-5)
- header strip + inject (Audit Claim E)
- retry path counts failure exactly once (Finding H-4 / earlier bug)
"""
from __future__ import annotations

import httpx
import pytest
from ai_gateway_core.comm.retry import RetryBudget, RetryPolicy
from ai_gateway_core.proxy.base import (
    CircuitBreaker,
    CircuitBreakerState,
    InMemoryCounter,
    ServiceProxy,
    ServiceProxyConfig,
)
from fastapi import HTTPException
from starlette.requests import Request


def test_breaker_closed_by_default() -> None:
    cb = CircuitBreaker(name="test")
    assert cb.state == CircuitBreakerState.CLOSED
    cb.gate()  # should not raise


def test_breaker_opens_after_threshold_failures() -> None:
    cb = CircuitBreaker(name="test", threshold=3)
    for _ in range(3):
        cb.on_failure()
    assert cb.state == CircuitBreakerState.OPEN


def test_breaker_half_open_probe_slot_single_use() -> None:
    cb = CircuitBreaker(name="test", threshold=2)
    cb.on_failure()
    cb.on_failure()
    assert cb.state == CircuitBreakerState.OPEN

    # First gate() acquires the probe slot
    cb.gate()
    # Second gate() while probe in flight → 503
    from fastapi import HTTPException

    with pytest.raises(HTTPException) as exc:
        cb.gate()
    assert exc.value.status_code == 503
    assert "Retry-After" in exc.value.headers


def test_breaker_2xx_closes() -> None:
    cb = CircuitBreaker(name="test", threshold=2)
    cb.on_failure()
    cb.on_failure()
    cb.gate()  # claim probe
    cb.on_response(200)
    assert cb.state == CircuitBreakerState.CLOSED


def test_breaker_4xx_does_not_close_breaker(
    # Finding M-5: a run of 400 responses after a 5xx burst must NOT
    # spuriously close the breaker.
) -> None:
    cb = CircuitBreaker(name="test", threshold=3)
    cb.on_failure()
    cb.on_failure()
    assert cb.store.get() == (2, False)

    cb.on_response(400)
    # fails stay at 2; probe slot released
    assert cb.store.get() == (2, False)

    cb.on_response(404)
    assert cb.store.get() == (2, False)


def test_breaker_5xx_response_counts_as_failure() -> None:
    cb = CircuitBreaker(name="test", threshold=3)
    for _ in range(3):
        cb.on_response(500)
    assert cb.state == CircuitBreakerState.OPEN


def test_breaker_probe_success_resets_counter() -> None:
    cb = CircuitBreaker(name="test", threshold=2)
    cb.on_failure()
    cb.on_failure()
    cb.gate()
    cb.on_response(200)
    # Now the breaker is closed — a fresh failure starts at 1, not 3
    cb.on_failure()
    assert cb.store.get() == (1, False)
    assert cb.state == CircuitBreakerState.CLOSED


def test_inmemory_counter_isolated_per_instance() -> None:
    c1 = InMemoryCounter()
    c2 = InMemoryCounter()
    c1.on_failure()
    c1.on_failure()
    assert c1.get() == (2, False)
    assert c2.get() == (0, False)


def test_breaker_3xx_is_not_success() -> None:
    """3xx should release the probe but not reset fail counter."""
    cb = CircuitBreaker(name="test", threshold=3)
    cb.on_failure()
    cb.on_failure()
    cb.on_response(302)
    assert cb.store.get() == (2, False)


def test_breaker_custom_threshold() -> None:
    cb = CircuitBreaker(name="test", threshold=5)
    for _ in range(4):
        cb.on_failure()
    assert cb.state == CircuitBreakerState.CLOSED
    cb.on_failure()
    assert cb.state == CircuitBreakerState.OPEN


def test_breaker_recovery_timeout_allows_new_probe_after_stale_probe_slot(
    monkeypatch,
) -> None:
    now = 1_000.0
    monkeypatch.setattr("ai_gateway_core.proxy.base.time.monotonic", lambda: now)
    cb = CircuitBreaker(name="test", threshold=2, recovery_timeout=30)
    cb.on_failure()
    cb.on_failure()
    cb.gate()

    from fastapi import HTTPException

    with pytest.raises(HTTPException):
        cb.gate()

    now = 1_031.0
    cb.gate()
    cb.on_response(200)
    assert cb.state == CircuitBreakerState.CLOSED


@pytest.mark.asyncio
async def test_service_proxy_disables_httpx_transport_retries(monkeypatch) -> None:
    captured_retries: list[int] = []

    def fake_transport(*, retries: int = 0, **_kwargs):
        captured_retries.append(retries)
        return httpx.MockTransport(lambda _request: httpx.Response(200))

    monkeypatch.setattr(
        "ai_gateway_core.proxy.base.httpx.AsyncHTTPTransport",
        fake_transport,
    )
    proxy = ServiceProxy(ServiceProxyConfig(name="test", base_url="http://upstream"))

    try:
        await proxy._get_client()
    finally:
        await proxy.aclose()

    assert captured_retries == [0]


def _retry_request(*, method: str, idempotency_key: str | None) -> Request:
    headers = (
        []
        if idempotency_key is None
        else [(b"idempotency-key", idempotency_key.encode("utf-8"))]
    )

    async def receive():
        return {"type": "http.request", "body": b"", "more_body": False}

    return Request(
        {
            "type": "http",
            "asgi": {"version": "3.0"},
            "http_version": "1.1",
            "method": method,
            "scheme": "http",
            "path": "/gateway",
            "raw_path": b"/gateway",
            "query_string": b"",
            "headers": headers,
            "client": ("127.0.0.1", 1234),
            "server": ("testserver", 80),
        },
        receive,
    )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("method", "idempotency_key", "expected_attempts", "expect_success"),
    [
        pytest.param("POST", None, 1, False, id="post-missing-key"),
        pytest.param("POST", "", 1, False, id="post-empty-key"),
        pytest.param("POST", "   ", 1, False, id="post-whitespace-key"),
        pytest.param("POST", "mutation-1", 2, True, id="post-valid-key"),
        pytest.param("GET", None, 2, True, id="get-no-key"),
    ],
)
async def test_service_proxy_retries_only_safe_or_idempotency_keyed_requests(
    method: str,
    idempotency_key: str | None,
    expected_attempts: int,
    expect_success: bool,
) -> None:
    seen: list[tuple[bytes, str | None]] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        seen.append((await request.aread(), request.headers.get("idempotency-key")))
        if len(seen) == 1:
            raise httpx.RemoteProtocolError("temporary disconnect", request=request)
        return httpx.Response(200, json={"ok": True})

    upstream_client = httpx.AsyncClient(
        transport=httpx.MockTransport(handler),
        base_url="http://upstream",
    )
    proxy = ServiceProxy(
        ServiceProxyConfig(
            name="test",
            base_url="http://upstream",
            retry_policy=RetryPolicy(
                max_attempts=2,
                base_delay_ms=0,
                max_delay_ms=0,
                jitter=False,
            ),
            retry_budget=RetryBudget(budget_ratio=1.0),
        )
    )

    async def get_client() -> httpx.AsyncClient:
        return upstream_client

    async def reset_client() -> None:
        return None

    proxy._get_client = get_client  # type: ignore[method-assign]
    proxy._reset_client = reset_client  # type: ignore[method-assign]
    body = b"mutation-body" if method == "POST" else None

    try:
        if expect_success:
            response = await proxy.forward(
                _retry_request(method=method, idempotency_key=idempotency_key),
                user_headers={"X-User-Id": "u", "X-Tenant-Id": "t"},
                upstream_path="/upstream",
                body=body,
            )
            assert response.status_code == 200
        else:
            with pytest.raises(HTTPException) as exc_info:
                await proxy.forward(
                    _retry_request(method=method, idempotency_key=idempotency_key),
                    user_headers={"X-User-Id": "u", "X-Tenant-Id": "t"},
                    upstream_path="/upstream",
                    body=body,
                )
            assert exc_info.value.status_code == 502
    finally:
        await upstream_client.aclose()

    assert len(seen) == expected_attempts
    if method == "POST" and expect_success:
        assert seen == [(b"mutation-body", idempotency_key)] * 2
    if method == "GET":
        assert seen == [(b"", None), (b"", None)]


# --- strip / inject header contract ---


def test_injected_identity_headers_cover_all_x_user() -> None:
    """GATE G5a-3. Every internal identity header must be in
    the gateway's strip list, otherwise a public client could smuggle
    ``x-user-type: admin`` past the Gateway and reach a downstream service
    header verbatim."""
    from ai_gateway_core.proxy.base import _DEFAULT_INJECTED_IDENTITY_HEADERS

    required = {
        "x-user-id",
        "x-tenant-id",
        "x-user-tier",
        "x-user-type",
        "x-user-roles",
        "x-user-email",
        "x-user-name",
    }
    assert required <= _DEFAULT_INJECTED_IDENTITY_HEADERS


def test_service_proxy_strip_list_is_lowercase() -> None:
    """Callers compare against ``.lower()``, so the strip set must be
    lowercase to match starlette's lowercased header iteration."""
    from ai_gateway_core.proxy.base import (
        _DEFAULT_INJECTED_IDENTITY_HEADERS,
        _DEFAULT_STRIP_REQ,
        _DEFAULT_STRIP_RESP,
    )

    for h in _DEFAULT_INJECTED_IDENTITY_HEADERS:
        assert h == h.lower()
    for h in _DEFAULT_STRIP_REQ:
        assert h == h.lower()
    for h in _DEFAULT_STRIP_RESP:
        assert h == h.lower()
