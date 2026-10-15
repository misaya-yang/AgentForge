"""Shared ``ServiceProxy`` + ``CircuitBreaker`` used by Gateway microservice proxies.

Design reference: ``plans/SystemDesign-Assistant-Service-True-Extraction-Phase5-2026-04-23.md`` §5.1.

Semantics locked by the Phase-5 audit (Findings M-1..M-5):
- Half-open probe state machine, no wall-clock dead-zone.
- 4xx is NOT counted as success — only 2xx closes the breaker.
- Stream-mid-failure is counted as a breaker failure via ``_stream_wrapped``.
- ``content-type: text/event-stream`` FORCES streaming even when
  ``content-length`` is present (some misconfigured upstreams advertise it).
- Retry path calls ``_on_failure()`` at most once per external request.

Worker-scope caveat: ``InMemoryCounter`` is process-local. With
``uvicorn --workers N > 1`` each worker owns its own breaker state; the
module docstring on the concrete counter explains how to swap in a Redis
counter for multi-worker deployments.
"""
from __future__ import annotations

import asyncio
import contextlib
import logging
import os
import threading
import time
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass, field
from enum import Enum
from typing import Protocol

import httpx
from fastapi import HTTPException, Request
from fastapi.responses import StreamingResponse
from starlette.responses import Response

from ai_gateway_core.comm.retry import RetryBudget, RetryPolicy
from ai_gateway_core.logging import record_internal_exception

from .route_pattern import extract_route_pattern

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Circuit breaker
# ---------------------------------------------------------------------------


class CircuitBreakerState(str, Enum):
    CLOSED = "closed"
    OPEN = "open"


class CounterStore(Protocol):
    """Contract for breaker state storage.

    ``InMemoryCounter`` is process-local; a Redis-backed impl can be
    dropped in for multi-worker deployments without touching the proxy.
    """

    def get(self) -> tuple[int, bool]:  # (fail_count, probe_in_flight)
        ...

    def set_probe(self, in_flight: bool) -> None:
        ...

    def on_success(self) -> None:
        ...

    def on_failure(self) -> int:  # returns new fail count
        ...


class InMemoryCounter:
    """Process-local breaker counter.

    ⚠️  Single-worker only. With ``uvicorn --workers N > 1`` each worker
    maintains independent ``fails`` / ``probe_in_flight`` state, so the
    "one probe" guarantee is per-worker (aggregate probes = N). Swap
    to a Redis-backed store for multi-worker deployments.
    """

    __slots__ = ("_fails", "_probe_in_flight", "_probe_started_at", "_opened_at", "_lock")

    def __init__(self) -> None:
        self._fails = 0
        self._probe_in_flight = False
        self._probe_started_at = 0.0
        self._opened_at = 0.0
        self._lock = threading.Lock()

    def get(self) -> tuple[int, bool]:
        with self._lock:
            return self._fails, self._probe_in_flight

    def set_probe(self, in_flight: bool) -> None:
        with self._lock:
            self._probe_in_flight = in_flight
            self._probe_started_at = time.monotonic() if in_flight else 0.0

    def on_success(self) -> None:
        with self._lock:
            self._fails = 0
            self._probe_in_flight = False
            self._probe_started_at = 0.0
            self._opened_at = 0.0

    def on_failure(self) -> int:
        with self._lock:
            self._fails += 1
            self._probe_in_flight = False
            self._probe_started_at = 0.0
            return self._fails

    def probe_started_at(self) -> float:
        with self._lock:
            return self._probe_started_at

    def opened_at(self) -> float:
        with self._lock:
            return self._opened_at

    def set_opened_at(self, opened_at: float) -> None:
        with self._lock:
            if self._opened_at <= 0:
                self._opened_at = opened_at


class RedisCounterStore:
    """Redis-backed circuit-breaker state store.

    The store keeps the same sync ``CounterStore`` contract as
    ``InMemoryCounter`` so it can be swapped in without changing breaker logic.
    """

    def __init__(self, redis_client, *, key: str, ttl_seconds: int = 300) -> None:
        self._redis = redis_client
        self._key = key
        self._ttl_seconds = ttl_seconds

    @classmethod
    def from_url(
        cls,
        redis_url: str,
        *,
        key: str,
        ttl_seconds: int = 300,
    ) -> RedisCounterStore:
        try:
            import redis
        except ImportError as exc:  # pragma: no cover - dependency declared by package
            raise RuntimeError("redis package is required for RedisCounterStore") from exc
        return cls(
            redis.Redis.from_url(redis_url, decode_responses=True),
            key=key,
            ttl_seconds=ttl_seconds,
        )

    def get(self) -> tuple[int, bool]:
        try:
            data = self._redis.hgetall(self._key) or {}
        except Exception as exc:  # noqa: BLE001
            raise RuntimeError("redis counter store unavailable") from exc
        fails = int(_redis_value(data.get("fails"), "0") or "0")
        probe = (_redis_value(data.get("probe_in_flight"), "0") or "0") == "1"
        return fails, probe

    def set_probe(self, in_flight: bool) -> None:
        try:
            self._redis.hset(
                self._key,
                mapping={
                    "probe_in_flight": "1" if in_flight else "0",
                    "probe_started_at": str(time.monotonic() if in_flight else 0.0),
                },
            )
            self._redis.expire(self._key, self._ttl_seconds)
        except Exception as exc:  # noqa: BLE001
            raise RuntimeError("redis counter store unavailable") from exc

    def on_success(self) -> None:
        try:
            self._redis.delete(self._key)
        except Exception as exc:  # noqa: BLE001
            raise RuntimeError("redis counter store unavailable") from exc

    def on_failure(self) -> int:
        try:
            new_value = int(self._redis.hincrby(self._key, "fails", 1))
            self._redis.hset(
                self._key,
                mapping={
                    "probe_in_flight": "0",
                    "probe_started_at": "0",
                },
            )
            self._redis.expire(self._key, self._ttl_seconds)
            return new_value
        except Exception as exc:  # noqa: BLE001
            raise RuntimeError("redis counter store unavailable") from exc

    def probe_started_at(self) -> float:
        data = self._redis.hgetall(self._key) or {}
        return float(_redis_value(data.get("probe_started_at"), "0") or "0")

    def opened_at(self) -> float:
        data = self._redis.hgetall(self._key) or {}
        return float(_redis_value(data.get("opened_at"), "0") or "0")

    def set_opened_at(self, opened_at: float) -> None:
        try:
            if self.opened_at() <= 0:
                self._redis.hset(self._key, mapping={"opened_at": str(opened_at)})
                self._redis.expire(self._key, self._ttl_seconds)
        except Exception as exc:  # noqa: BLE001
            raise RuntimeError("redis counter store unavailable") from exc


@dataclass
class CircuitBreaker:
    """Half-open probe breaker. Threshold = 3 consecutive failures.

    State machine:
        CLOSED ──(N failures)──▶ OPEN
        OPEN    one probe slot; the next request takes it and tries the
                upstream. Success → CLOSED. Failure → stay OPEN, slot
                released so a later request can retry.

    No wall-clock recovery window — the old 30s dead-zone blocked every
    request for 30s even after the upstream came back healthy.
    """

    name: str
    threshold: int = 3
    store: CounterStore = field(default_factory=InMemoryCounter)
    recovery_timeout: float = field(
        default_factory=lambda: _get_float_env(
            "SERVICE_BREAKER_RECOVERY_TIMEOUT_SECONDS",
            30.0,
        )
    )

    def gate(self) -> None:
        """Allow or block the incoming request.

        Raises ``HTTPException(503)`` if the breaker is OPEN and a
        probe is already in flight. Claims the probe slot otherwise.
        """
        fails, probe = self.store.get()
        if fails < self.threshold:
            return
        if probe:
            started_at = _store_float(self.store, "probe_started_at")
            if (
                self.recovery_timeout > 0
                and started_at > 0
                and time.monotonic() - started_at >= self.recovery_timeout
            ):
                self.store.set_probe(True)
                return
            raise HTTPException(
                status_code=503,
                detail=f"{self.name} recovering — probe in flight",
                headers={"Retry-After": "2"},
            )
        self.store.set_probe(True)

    def on_response(self, status_code: int) -> None:
        """Feedback from a completed upstream response.

        Finding M-5: only 2xx closes the breaker. 3xx/4xx leaves state
        as-is — "200 means upstream is fine", "400 means configuration
        is broken but not a transport failure".
        """
        if 200 <= status_code < 300:
            self.store.on_success()
        elif status_code >= 500:
            new_fails = self.store.on_failure()
            if new_fails >= self.threshold:
                _set_store_opened_at(self.store, time.monotonic())
            if new_fails == self.threshold:
                logger.warning(
                    "%s circuit breaker OPEN after %d failures",
                    self.name,
                    new_fails,
                )
        else:
            # 3xx / 4xx: release the probe slot but do not change fail count.
            _, probe = self.store.get()
            if probe:
                self.store.set_probe(False)

    def on_failure(self) -> None:
        """Transport/stream failure (connect error, timeout, mid-stream abort)."""
        new_fails = self.store.on_failure()
        if new_fails >= self.threshold:
            _set_store_opened_at(self.store, time.monotonic())
        if new_fails == self.threshold:
            logger.warning(
                "%s circuit breaker OPEN after %d failures",
                self.name,
                new_fails,
            )

    @property
    def state(self) -> CircuitBreakerState:
        fails, _ = self.store.get()
        return (
            CircuitBreakerState.OPEN
            if fails >= self.threshold
            else CircuitBreakerState.CLOSED
        )


# ---------------------------------------------------------------------------
# Service proxy
# ---------------------------------------------------------------------------


# These are stripped from incoming client requests before we inject the
# trusted values. Case-insensitive; we compare against ``.lower()``.
_DEFAULT_INJECTED_IDENTITY_HEADERS = frozenset(
    {
        "x-user-id",
        "x-tenant-id",
        "x-user-tier",
        "x-user-type",
        "x-user-roles",
        "x-user-email",
        "x-user-name",
    }
)
# Headers the gateway injects via the optional ``signer`` callback.
# These too must be stripped from inbound requests — otherwise a
# public client could smuggle ``X-Gateway-Secret: <forged>``, which
# (in the absence of strip) would ride along next to the gateway's
# own real signature since Python dicts are case-sensitive.
_DEFAULT_INJECTED_AUTH_HEADERS = frozenset({"x-gateway-secret"})
_DEFAULT_STRIP_REQ = (
    frozenset({"host", "connection", "transfer-encoding", "content-length"})
    | _DEFAULT_INJECTED_IDENTITY_HEADERS
    | _DEFAULT_INJECTED_AUTH_HEADERS
)
_DEFAULT_STRIP_RESP = frozenset(
    {"transfer-encoding", "connection", "content-encoding"}
)


class ProxyError(Exception):
    """Raised for local proxy misconfiguration (not upstream errors)."""


@dataclass
class ServiceProxyConfig:
    name: str                                    # e.g. "knowledge-service"
    base_url: str                                 # e.g. "http://knowledge-service:8092"
    timeout: httpx.Timeout = field(
        default_factory=lambda: httpx.Timeout(
            connect=5.0, read=600.0, write=120.0, pool=30.0
        )
    )
    limits: httpx.Limits = field(
        default_factory=lambda: httpx.Limits(
            max_connections=50, max_keepalive_connections=10
        )
    )
    breaker_threshold: int = field(
        default_factory=lambda: _get_int_env("SERVICE_BREAKER_THRESHOLD", 3)
    )
    buffer_threshold_bytes: int = 256 * 1024
    strip_req: frozenset[str] = _DEFAULT_STRIP_REQ
    strip_resp: frozenset[str] = _DEFAULT_STRIP_RESP
    injected_identity_headers: frozenset[str] = _DEFAULT_INJECTED_IDENTITY_HEADERS
    retry_policy: RetryPolicy = field(default_factory=lambda: _retry_policy_from_env())
    retry_budget: RetryBudget = field(default_factory=lambda: _retry_budget_from_env())


class ServiceProxy:
    """Shared HTTP streaming proxy.

    Owns one ``httpx.AsyncClient`` with auto-reconnect on transport
    failure. The client is lazily constructed and reset on connection
    errors (remember: pooled connections can go stale).

    Behavior contract (matches Phase-5 design §5.1 + audit findings):
      1. SSE streams: ``content-type == text/event-stream`` → always stream
         chunk-by-chunk. Never buffer even if a rogue upstream sent
         ``content-length`` (Finding M-3).
      2. Small JSON: ``content-length`` present AND < buffer_threshold AND
         content-type is not SSE → buffer to a plain ``Response``.
      3. Everything else: stream via ``aiter_bytes()`` (no chunk_size
         argument — passing one forces httpx's ``ByteChunker`` to buffer
         until the threshold is hit; fatal for SSE).
      4. Circuit breaker: half-open probe, only 2xx closes the breaker,
         mid-stream failures are counted (Findings M-1, M-2, M-5).
      5. Retry: one reconnect on ``ConnectError`` / ``RemoteProtocolError``
         / ``PoolTimeout``. If the retry also fails, ``on_failure()``
         fires exactly once (never double-counted).
    """

    def __init__(
        self,
        config: ServiceProxyConfig,
        *,
        breaker: CircuitBreaker | None = None,
        signer: Callable[..., tuple[str, str] | None] | None = None,
    ) -> None:
        """
        Parameters
        ----------
        config
            Immutable proxy config (base url, timeouts, strip lists).
        breaker
            Injectable circuit breaker. Defaults to a fresh
            ``CircuitBreaker(name=config.name, threshold=config.breaker_threshold)``.
            Inject explicitly to share state with tests or swap in a
            Redis-backed counter.
        signer
            Optional callable that returns ``(header_name, header_value)``
            for a per-request HMAC signature (``X-Gateway-Secret`` etc).
            Return ``None`` to skip signing for a given request.
        """
        self._cfg = config
        # Legacy / fallback "global" breaker. Kept so external callers
        # that share state via the ``breaker=`` constructor arg continue
        # to work (e.g. injected for test isolation), and as a safety
        # net for code paths that haven't been routed through
        # ``_breaker_for`` yet. Per-route breakers in ``self._breakers``
        # are the hot-path keying.
        self._global_breaker = breaker or CircuitBreaker(
            name=config.name,
            threshold=config.breaker_threshold,
            store=_counter_store_for(config.name),
        )
        # Lazily-populated per-route breakers, keyed by canonical
        # route pattern (output of ``extract_route_pattern``). Created
        # once per pattern; kept in a plain dict because the GIL is
        # sufficient for the ``setdefault``-style write we use below.
        self._breakers: dict[str, CircuitBreaker] = {}
        self._breakers_lock = threading.Lock()
        self._signer = signer
        self._client: httpx.AsyncClient | None = None
        self._client_lock = asyncio.Lock()

    # ----- public API -----

    @property
    def breaker(self) -> CircuitBreaker:
        """Backward-compat alias for the legacy single-breaker accessor.

        New code should use :meth:`_breaker_for` (per-route keying).
        Returns the global breaker — same instance an injected
        ``breaker=`` constructor argument provides.
        """
        return self._global_breaker

    def _breaker_for(self, route_pattern: str) -> CircuitBreaker:
        """Return (lazily creating) the per-pattern breaker.

        The breaker name is ``"{service_name}:{route_pattern}"`` so logs
        and metrics distinguish ``Assistant Service:/chat/stream`` from
        ``Assistant Service:/health``. Threshold inherits the proxy
        config — same trip behaviour as the legacy global breaker, just
        scoped per route.
        """
        existing = self._breakers.get(route_pattern)
        if existing is not None:
            return existing
        with self._breakers_lock:
            existing = self._breakers.get(route_pattern)
            if existing is not None:
                return existing
            cb = CircuitBreaker(
                name=f"{self._cfg.name}:{route_pattern}",
                threshold=self._cfg.breaker_threshold,
                store=_counter_store_for(f"{self._cfg.name}:{route_pattern}"),
            )
            self._breakers[route_pattern] = cb
            return cb

    async def forward(
        self,
        request: Request,
        user_headers: dict[str, str],
        *,
        upstream_path: str,
        body: bytes | None = None,
        error_prefix: str | None = None,
    ) -> Response:
        """Forward a request to the upstream service.

        Parameters
        ----------
        request
            The inbound FastAPI/Starlette ``Request``. The proxy reads
            the method, URL query string, and — unless ``body`` is
            passed — the request stream.
        user_headers
            The trusted identity headers to inject (caller builds these
            from a validated ``UserContext``).
        upstream_path
            Path segment appended to ``config.base_url``. The caller
            is responsible for including the API prefix
            (e.g. ``"/api/v1/assistant/chat/stream"``).
        body
            Pre-read request bytes. Required when the caller has
            already consumed ``request.stream()`` (e.g. for authz
            parsing) — starlette's request stream is single-use.
        error_prefix
            Human-readable prefix for HTTPException details. Defaults
            to the service name.
        """
        # Per-route breaker keying (Phase 5e). A flaky ``/chat/stream``
        # must not poison ``/health`` on the same upstream. Pattern is
        # extracted from the upstream path; the breaker is created
        # lazily on first use.
        route_pattern = extract_route_pattern(upstream_path)
        breaker = self._breaker_for(route_pattern)
        breaker.gate()

        url = upstream_path
        if request.url.query:
            url = f"{upstream_path}?{request.url.query}"

        # Body caching — audit H-1. If the inbound request carries a
        # body (POST/PUT/PATCH/DELETE) and the caller didn't pre-read,
        # pre-read it here so the retry branch below can replay the
        # same bytes. Without this cache, a transient
        # ``RemoteProtocolError`` / ``PoolTimeout`` on the first
        # attempt consumes ``request.stream()`` and the retry gets an
        # empty body — silently 400s upstream document uploads on a
        # flaky KB hop. GET / HEAD / OPTIONS skip this cache so
        # body-less requests stay stream-through.
        if body is None and request.method.upper() in {"POST", "PUT", "PATCH", "DELETE"}:
            body = await request.body()

        headers = self._build_headers(
            request,
            user_headers,
            upstream_path=upstream_path,
            body=body,
        )

        prefix = error_prefix or self._cfg.name
        policy = self._cfg.retry_policy
        budget = self._cfg.retry_budget
        budget.record_original()
        method = request.method.upper()
        body_replayable = body is not None
        idempotency_key = any(
            key.lower() == "idempotency-key" and bool(value.strip())
            for key, value in headers.items()
        )
        attempt = 1

        while True:
            try:
                return await self._do(request, url, headers, body, breaker)
            except (
                httpx.ConnectError,
                httpx.RemoteProtocolError,
                httpx.PoolTimeout,
            ) as exc:
                can_retry = (
                    attempt < policy.max_attempts
                    and policy.can_retry_exception(
                        exc,
                        method=method,
                        body_replayable=body_replayable,
                        idempotency_key=idempotency_key,
                    )
                    and budget.try_acquire_retry()
                )
                logger.warning(
                    "%s proxy connection error%s: %s",
                    self._cfg.name,
                    ", retrying" if can_retry else "",
                    exc,
                )
                await self._reset_client()
                if can_retry:
                    delay = policy.delay_seconds(attempt)
                    if delay > 0:
                        await asyncio.sleep(delay)
                    attempt += 1
                    continue
                breaker.on_failure()
                raise HTTPException(502, f"{prefix} unavailable") from exc
            except httpx.TimeoutException as exc:
                breaker.on_failure()
                raise HTTPException(504, f"{prefix} timeout") from exc
            except HTTPException:
                # Propagate upstream-produced HTTPExceptions without treating
                # them as transport failures.
                raise
            except Exception as exc:
                breaker.on_failure()
                raise HTTPException(502, f"{prefix} error") from exc

    async def aclose(self) -> None:
        await self._reset_client()

    # ----- internals -----

    async def _get_client(self) -> httpx.AsyncClient:
        if self._client is not None:
            return self._client
        async with self._client_lock:
            if self._client is None:
                self._client = httpx.AsyncClient(
                    base_url=self._cfg.base_url,
                    timeout=self._cfg.timeout,
                    limits=self._cfg.limits,
                    transport=httpx.AsyncHTTPTransport(retries=0),
                )
                # Attach OTel CLIENT-span instrumentation so outbound
                # gateway → microservice hops show up in the trace tree.
                # ``instrument_httpx_client`` is graceful — it catches
                # ImportError so a stripped-down container without the
                # OTel httpx instrumentor still proxies fine, just
                # without per-hop CLIENT spans.
                try:
                    from ai_gateway_core.tracing import instrument_httpx_client

                    instrument_httpx_client(self._client)
                except Exception as exc:  # noqa: BLE001
                    logger.warning(
                        "ServiceProxy: OTel httpx instrumentation skipped: %s",
                        exc,
                    )
            return self._client

    async def _reset_client(self) -> None:
        old: httpx.AsyncClient | None
        async with self._client_lock:
            old, self._client = self._client, None
        if old is not None:
            with contextlib.suppress(Exception):
                await old.aclose()

    def _build_headers(
        self,
        request: Request,
        user_headers: dict[str, str],
        *,
        upstream_path: str,
        body: bytes | None,
    ) -> dict[str, str]:
        strip = self._cfg.strip_req
        out = {k: v for k, v in request.headers.items() if k.lower() not in strip}
        out.update(user_headers)

        # Forward the gateway's request_id so upstream microservices can
        # correlate logs end-to-end. Middleware sets ``request.state.request_id``
        # by either preserving the inbound X-Request-Id or minting a fresh
        # UUID. If the client also sent X-Request-Id, the request.headers
        # copy above already kept it — skip overriding.
        state_request_id = getattr(
            getattr(request, "state", None), "request_id", None,
        )
        if state_request_id and not any(k.lower() == "x-request-id" for k in out):
            out["X-Request-Id"] = state_request_id

        # Forward W3C Trace Context. ``OTelInboundMiddleware`` injected
        # the active context into ``request.state.traceparent`` /
        # ``request.state.tracestate`` so we don't rebuild them from
        # OTel here — that keeps the proxy free of an OTel hard-dep
        # and avoids a double-extract round trip. The httpx
        # instrumentor adds them again on the client side, but having
        # the explicit forward here means that even when the
        # instrumentor isn't loaded the chain stays intact.
        request_state = getattr(request, "state", None)
        traceparent = getattr(request_state, "traceparent", None) if request_state else None
        if traceparent and not any(k.lower() == "traceparent" for k in out):
            out["traceparent"] = traceparent
        tracestate = getattr(request_state, "tracestate", None) if request_state else None
        if tracestate and not any(k.lower() == "tracestate" for k in out):
            out["tracestate"] = tracestate

        if self._signer is not None:
            try:
                sig = self._signer(
                    request,
                    upstream_path=upstream_path,
                    body=body,
                    identity_headers=user_headers,
                )
            except TypeError:
                # Preserve compatibility with older custom signers while all
                # built-in signers receive the trusted identity envelope.
                sig = self._signer(request, upstream_path=upstream_path, body=body)
            if sig is not None:
                name, value = sig
                out[name] = value
        return out

    def _clean_response_headers(self, h: httpx.Headers) -> dict[str, str]:
        strip = self._cfg.strip_resp
        return {k: v for k, v in h.items() if k.lower() not in strip}

    async def _do(
        self,
        request: Request,
        url: str,
        headers: dict[str, str],
        body: bytes | None,
        breaker: CircuitBreaker,
    ) -> Response:
        client = await self._get_client()
        request_content = body
        if body is None and request.method.upper() not in {"GET", "HEAD", "OPTIONS"}:
            request_content = request.stream()
        req = client.build_request(
            method=request.method,
            url=url,
            headers=headers,
            content=request_content,
        )
        resp = await client.send(req, stream=True)

        # Breaker feedback on the response status. Mid-stream failures
        # are handled separately by ``_stream_wrapped``.
        breaker.on_response(resp.status_code)

        rh = self._clean_response_headers(resp.headers)
        mt = resp.headers.get("content-type")
        cl = resp.headers.get("content-length")
        is_sse = mt is not None and mt.lower().startswith("text/event-stream")

        # Small-response buffer path — only if we have a plausible length,
        # the body is small, AND it is NOT an SSE stream. ``text/event-stream``
        # with content-length is still streamed chunk-by-chunk.
        if (
            not is_sse
            and cl is not None
            and cl.isdigit()
            and int(cl) < self._cfg.buffer_threshold_bytes
        ):
            buffered = await resp.aread()
            await resp.aclose()
            return Response(
                content=buffered,
                status_code=resp.status_code,
                headers=rh,
                media_type=mt,
            )

        return StreamingResponse(
            content=self._stream_wrapped(resp, breaker),
            status_code=resp.status_code,
            headers=rh,
            media_type=mt,
        )

    async def _stream_wrapped(
        self, resp: httpx.Response, breaker: CircuitBreaker
    ) -> AsyncIterator[bytes]:
        """Stream chunks and count mid-stream failures as breaker failures.

        ``aiter_bytes()`` WITHOUT a chunk_size yields each network read
        directly. Passing a size wraps the stream in httpx's ``ByteChunker``
        which BUFFERS until the threshold is hit — fatal for SSE text_delta
        latency.
        """
        try:
            async for chunk in resp.aiter_bytes():
                yield chunk
        except Exception:
            # Finding M-2: a "handshake 200 + stream truncated" upstream
            # previously flew under the breaker's radar.
            breaker.on_failure()
            raise
        finally:
            await resp.aclose()


__all__ = [
    "CircuitBreaker",
    "CircuitBreakerState",
    "CounterStore",
    "InMemoryCounter",
    "RedisCounterStore",
    "ProxyError",
    "ServiceProxy",
    "ServiceProxyConfig",
]


def _redis_value(value, default: str) -> str:
    if value is None:
        return default
    if isinstance(value, bytes):
        return value.decode("utf-8")
    return str(value)


def _counter_store_for(name: str) -> CounterStore:
    backend = os.getenv("INTERNAL_COMM_STATE_BACKEND", "memory").strip().lower()
    if backend == "redis":
        redis_url = os.getenv("INTERNAL_COMM_REDIS_URL", "").strip()
        if not redis_url:
            logger.warning(
                "INTERNAL_COMM_STATE_BACKEND=redis but INTERNAL_COMM_REDIS_URL is unset; "
                "falling back to in-memory circuit breaker state"
            )
            return InMemoryCounter()
        safe_name = name.replace(" ", "_").replace("/", ":")
        return RedisCounterStore.from_url(
            redis_url,
            key=f"ai-gateway:internal:breaker:{safe_name}",
        )
    return InMemoryCounter()


def _store_float(store: CounterStore, method_name: str) -> float:
    method = getattr(store, method_name, None)
    if not callable(method):
        return 0.0
    try:
        return float(method() or 0.0)
    except Exception:  # noqa: BLE001
        return 0.0


def _set_store_opened_at(store: CounterStore, opened_at: float) -> None:
    method = getattr(store, "set_opened_at", None)
    if not callable(method):
        return
    try:
        method(opened_at)
    except Exception as exc:  # noqa: BLE001
        record_internal_exception(
            logger,
            "assistant.proxy.circuit_breaker_opened_at_failed",
            exc,
            level=logging.DEBUG,
        )


def _get_int_env(key: str, default: int) -> int:
    raw = os.getenv(key, "").strip()
    if not raw:
        return default
    try:
        return int(raw)
    except ValueError:
        logger.warning("Invalid %s=%r; using default %s", key, raw, default)
        return default


def _get_float_env(key: str, default: float) -> float:
    raw = os.getenv(key, "").strip()
    if not raw:
        return default
    try:
        return float(raw)
    except ValueError:
        logger.warning("Invalid %s=%r; using default %s", key, raw, default)
        return default


def _retry_policy_from_env() -> RetryPolicy:
    return RetryPolicy(
        max_attempts=_get_int_env("SERVICE_RETRY_MAX_ATTEMPTS", 2),
        base_delay_ms=_get_int_env("SERVICE_RETRY_BASE_DELAY_MS", 50),
        max_delay_ms=_get_int_env("SERVICE_RETRY_MAX_DELAY_MS", 500),
    )


def _retry_budget_from_env() -> RetryBudget:
    return RetryBudget(
        budget_ratio=_get_float_env("SERVICE_RETRY_BUDGET_RATIO", 0.1),
    )
