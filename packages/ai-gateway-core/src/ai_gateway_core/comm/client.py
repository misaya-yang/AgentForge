"""Shared outbound HTTP client for internal service calls."""

from __future__ import annotations

import asyncio
import json as jsonlib
import logging
import os
import time
import uuid
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any

import httpx

from ai_gateway_core.auth.gateway_secret import GatewaySecret

from .retry import RetryBudget, RetryPolicy, UpstreamOutcomeUnknown, outcome_is_unknown

logger = logging.getLogger(__name__)


class InternalServiceHTTPError(Exception):
    """Raised when an internal service returns an error response."""

    def __init__(self, status_code: int, text: str, *, service: str) -> None:
        super().__init__(f"{service} returned HTTP {status_code}")
        self.status_code = status_code
        self.text = text
        self.service = service


class TokenBucketRateLimiter:
    """Async token bucket used for internal service-to-service calls."""

    def __init__(self, *, rate: float, burst: int | None = None) -> None:
        self.rate = max(float(rate), 0.001)
        self.burst = max(int(burst if burst is not None else rate), 1)
        self._tokens = float(self.burst)
        self._updated_at = time.monotonic()
        self._lock = asyncio.Lock()

    async def acquire(self) -> None:
        while True:
            async with self._lock:
                now = time.monotonic()
                elapsed = max(now - self._updated_at, 0.0)
                self._tokens = min(self.burst, self._tokens + elapsed * self.rate)
                self._updated_at = now
                if self._tokens >= 1:
                    self._tokens -= 1
                    return
                wait_seconds = (1 - self._tokens) / self.rate
            await asyncio.sleep(wait_seconds)


@dataclass
class InternalServiceClientConfig:
    name: str
    base_url: str
    timeout: httpx.Timeout = field(
        default_factory=lambda: httpx.Timeout(
            connect=5.0,
            read=30.0,
            write=10.0,
            pool=10.0,
        )
    )
    limits: httpx.Limits = field(
        default_factory=lambda: httpx.Limits(
            max_connections=50,
            max_keepalive_connections=10,
        )
    )
    retry_policy: RetryPolicy = field(default_factory=RetryPolicy)
    retry_budget: RetryBudget = field(default_factory=RetryBudget)
    gateway_secret: GatewaySecret | None = None
    rate_limiter: TokenBucketRateLimiter | None = None
    auto_idempotency: bool = True


class InternalServiceClient:
    """Reusable non-proxy HTTP client for service-to-service calls."""

    def __init__(
        self,
        config: InternalServiceClientConfig,
        *,
        transport: httpx.AsyncBaseTransport | None = None,
    ) -> None:
        self._cfg = config
        self._transport = transport
        self._rate_limiter = config.rate_limiter or _rate_limiter_from_env(config.name)
        self._client: httpx.AsyncClient | None = None
        self._client_lock = asyncio.Lock()

    async def close(self) -> None:
        if self._client and not self._client.is_closed:
            await self._client.aclose()

    async def request_json(
        self,
        method: str,
        path: str,
        *,
        query_params: dict[str, str] | None = None,
        headers: dict[str, str] | None = None,
        json: Any | None = None,
        content: bytes | str | None = None,
    ) -> Any:
        response = await self.request(
            method,
            path,
            query_params=query_params,
            headers=headers,
            json=json,
            content=content,
        )
        if response.status_code >= 400:
            raise InternalServiceHTTPError(
                response.status_code,
                response.text,
                service=self._cfg.name,
            )
        if not response.content:
            return None
        return response.json()

    async def request(
        self,
        method: str,
        path: str,
        *,
        query_params: dict[str, str] | None = None,
        headers: dict[str, str] | None = None,
        json: Any | None = None,
        content: bytes | str | None = None,
    ) -> httpx.Response:
        method_upper = method.upper()
        body = _encode_body(json=json, content=content)
        merged_user_headers = dict(headers or {})
        if (
            self._cfg.auto_idempotency
            and _should_auto_idempotency(method_upper, merged_user_headers)
        ):
            merged_user_headers.setdefault(
                "Idempotency-Key",
                f"{self._cfg.name}:{uuid.uuid4().hex}",
            )
        merged_headers = self._headers(
            method=method_upper,
            path=path,
            query_params=query_params,
            body=body,
            headers=merged_user_headers,
            has_json=json is not None,
        )
        client = await self._get_client()
        policy = self._cfg.retry_policy
        budget = self._cfg.retry_budget
        budget.record_original()
        body_replayable = body is not None
        idempotency_key = any(
            key.lower() == "idempotency-key" and bool(value.strip())
            for key, value in merged_headers.items()
        )

        attempt = 1
        metrics = _service_metrics()
        started = time.perf_counter()
        _metrics_inflight(metrics, self._cfg.name, 1)
        try:
            while True:
                if self._rate_limiter is not None:
                    await self._rate_limiter.acquire()
                try:
                    response = await client.request(
                        method_upper,
                        path,
                        params=query_params,
                        headers=merged_headers,
                        content=body,
                    )
                except Exception as exc:
                    if (
                        attempt < policy.max_attempts
                        and policy.can_retry_exception(
                            exc,
                            method=method_upper,
                            body_replayable=body_replayable,
                            idempotency_key=idempotency_key,
                        )
                        and budget.try_acquire_retry()
                    ):
                        await _sleep(policy, attempt)
                        attempt += 1
                        continue
                    _metrics_record(
                        metrics,
                        service=self._cfg.name,
                        method=method_upper,
                        status=type(exc).__name__,
                        duration_seconds=time.perf_counter() - started,
                    )
                    if outcome_is_unknown(exc, method=method_upper):
                        raise UpstreamOutcomeUnknown("UPSTREAM_OUTCOME_UNKNOWN") from exc
                    raise

                if (
                    attempt < policy.max_attempts
                    and policy.can_retry_response(
                        response.status_code,
                        method=method_upper,
                        body_replayable=body_replayable,
                        idempotency_key=idempotency_key,
                    )
                    and budget.try_acquire_retry()
                ):
                    await response.aclose()
                    await _sleep(policy, attempt)
                    attempt += 1
                    continue
                _metrics_record(
                    metrics,
                    service=self._cfg.name,
                    method=method_upper,
                    status=str(response.status_code),
                    duration_seconds=time.perf_counter() - started,
                )
                return response
        finally:
            _metrics_inflight(metrics, self._cfg.name, -1)

    async def _get_client(self) -> httpx.AsyncClient:
        if self._client is not None and not self._client.is_closed:
            return self._client
        async with self._client_lock:
            if self._client is None or self._client.is_closed:
                transport = self._transport
                if transport is None:
                    transport = httpx.AsyncHTTPTransport(retries=0)
                self._client = httpx.AsyncClient(
                    base_url=self._cfg.base_url.rstrip("/"),
                    timeout=self._cfg.timeout,
                    limits=self._cfg.limits,
                    transport=transport,
                )
                try:
                    from ai_gateway_core.tracing import instrument_httpx_client

                    instrument_httpx_client(self._client)
                except Exception as exc:  # noqa: BLE001
                    logger.debug("InternalServiceClient instrumentation skipped: %s", exc)
            return self._client

    def _headers(
        self,
        *,
        method: str,
        path: str,
        query_params: dict[str, str] | None,
        body: bytes | None,
        headers: dict[str, str] | None,
        has_json: bool,
    ) -> dict[str, str]:
        from ai_gateway_core.tracing import internal_http_headers

        out = internal_http_headers(headers)
        if has_json and not any(k.lower() == "content-type" for k in out):
            out["Content-Type"] = "application/json"

        if self._cfg.gateway_secret is not None:
            query = str(httpx.QueryParams(query_params or {}))
            out[self._cfg.gateway_secret.header_name] = self._cfg.gateway_secret.sign(
                method=method,
                path=path,
                query=query,
                body=body,
                identity_headers=out,
            )
        return out


def _encode_body(*, json: Any | None, content: bytes | str | None) -> bytes | None:
    if json is not None and content is not None:
        raise ValueError("Pass either json or content, not both")
    if json is not None:
        return jsonlib.dumps(
            json,
            ensure_ascii=False,
            separators=(",", ":"),
        ).encode("utf-8")
    if isinstance(content, str):
        return content.encode("utf-8")
    return content


def _should_auto_idempotency(method: str, headers: dict[str, str]) -> bool:
    if method in {"GET", "HEAD", "OPTIONS"}:
        return False
    lowered = {key.lower(): value for key, value in headers.items()}
    if "idempotency-key" in lowered:
        return False
    accept = lowered.get("accept", "")
    content_type = lowered.get("content-type", "")
    return "text/event-stream" not in accept and "text/event-stream" not in content_type


def _rate_limiter_from_env(service_name: str) -> TokenBucketRateLimiter | None:
    raw = os.getenv("INTERNAL_SERVICE_RATE_LIMITS", "").strip()
    configured = _parse_rate_limits(raw)
    default_rates = {
        "knowledge-service": (100.0, 100),
        "agent-runtime": (50.0, 50),
        "agent-capability-worker": (50.0, 50),
    }
    rate_burst = configured.get(service_name) or default_rates.get(service_name)
    if rate_burst is None:
        return None
    rate, burst = rate_burst
    return TokenBucketRateLimiter(rate=rate, burst=burst)


def _parse_rate_limits(raw: str) -> dict[str, tuple[float, int]]:
    if not raw:
        return {}
    try:
        decoded = jsonlib.loads(raw)
    except jsonlib.JSONDecodeError:
        decoded = None
    result: dict[str, tuple[float, int]] = {}
    if isinstance(decoded, dict):
        for name, value in decoded.items():
            if isinstance(value, dict):
                rate = float(value.get("rate", 0))
                burst = int(value.get("burst", rate or 1))
            elif isinstance(value, (int, float)):
                rate = float(value)
                burst = int(value)
            else:
                continue
            if rate > 0:
                result[str(name)] = (rate, max(burst, 1))
        return result

    for item in raw.split(","):
        if not item.strip() or "=" not in item:
            continue
        name, value = item.split("=", 1)
        parts = [p.strip() for p in value.split(":") if p.strip()]
        try:
            rate = float(parts[0])
            burst = int(parts[1]) if len(parts) > 1 else int(rate)
        except (IndexError, ValueError):
            logger.warning("Invalid INTERNAL_SERVICE_RATE_LIMITS entry: %s", item)
            continue
        if rate > 0:
            result[name.strip()] = (rate, max(burst, 1))
    return result


_SERVICE_METRICS: dict[str, Any] | None = None
_SERVICE_METRICS_FACTORY: Callable[[], dict[str, Any]] | None = None


def configure_service_metrics(factory: Callable[[], dict[str, Any]] | None) -> None:
    """Inject a process-owned metrics sink; services without one remain uninstrumented."""
    global _SERVICE_METRICS, _SERVICE_METRICS_FACTORY
    _SERVICE_METRICS_FACTORY = factory
    _SERVICE_METRICS = None


def _service_metrics() -> dict[str, Any] | None:
    global _SERVICE_METRICS
    if _SERVICE_METRICS is not None:
        return _SERVICE_METRICS
    if _SERVICE_METRICS_FACTORY is None:
        return None
    try:
        _SERVICE_METRICS = _SERVICE_METRICS_FACTORY()
        return _SERVICE_METRICS
    except Exception:  # noqa: BLE001
        _SERVICE_METRICS = {}
        return None


def _metrics_record(
    metrics: dict[str, Any] | None,
    *,
    service: str,
    method: str,
    status: str,
    duration_seconds: float,
) -> None:
    if not metrics:
        return
    metrics["duration"].observe(
        duration_seconds,
        service=service,
        method=method,
        status=status,
    )
    metrics["total"].inc(service=service, method=method, status=status)


def _metrics_inflight(metrics: dict[str, Any] | None, service: str, delta: int) -> None:
    if not metrics:
        return
    if delta > 0:
        metrics["inflight"].inc(service=service)
    else:
        metrics["inflight"].dec(service=service)


async def _sleep(policy: RetryPolicy, attempt: int) -> None:
    delay = policy.delay_seconds(attempt)
    if delay > 0:
        await asyncio.sleep(delay)
