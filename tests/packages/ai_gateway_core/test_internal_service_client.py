from __future__ import annotations

from types import SimpleNamespace

import httpx
import pytest
from ai_gateway_contracts.replay import InMemoryReplayStore
from ai_gateway_core.auth.gateway_secret import GatewaySecret
from ai_gateway_core.comm.client import (
    InternalServiceClient,
    InternalServiceClientConfig,
    InternalServiceHTTPError,
    TokenBucketRateLimiter,
)
from ai_gateway_core.comm.retry import RetryBudget, RetryPolicy
from ai_gateway_core.proxy.base import RedisCounterStore
from ai_gateway_core.proxy.request_id_middleware import REQUEST_ID_CTX


class _FakeRedisCounter:
    def __init__(self) -> None:
        self.hashes: dict[str, dict[str, str]] = {}
        self.expiry: dict[str, int] = {}

    def hgetall(self, key: str) -> dict[str, str]:
        return dict(self.hashes.get(key, {}))

    def hset(self, key: str, mapping: dict[str, str]) -> None:
        self.hashes.setdefault(key, {}).update(mapping)

    def hincrby(self, key: str, field: str, amount: int) -> int:
        current = int(self.hashes.setdefault(key, {}).get(field, "0"))
        current += amount
        self.hashes[key][field] = str(current)
        return current

    def delete(self, key: str) -> None:
        self.hashes.pop(key, None)

    def expire(self, key: str, seconds: int) -> None:
        self.expiry[key] = seconds


def test_redis_counter_store_tracks_failures_and_probe_state() -> None:
    redis = _FakeRedisCounter()
    store = RedisCounterStore(redis, key="svc:route", ttl_seconds=30)

    assert store.get() == (0, False)
    assert store.on_failure() == 1
    store.set_probe(True)
    assert store.get() == (1, True)
    store.on_success()
    assert store.get() == (0, False)
    assert redis.expiry["svc:route"] == 30


@pytest.mark.asyncio
async def test_internal_service_client_retries_transient_get_once() -> None:
    calls: list[str] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        calls.append(str(request.url))
        if len(calls) == 1:
            raise httpx.ConnectError("temporary connect failure", request=request)
        return httpx.Response(200, json={"ok": True})

    client = InternalServiceClient(
        InternalServiceClientConfig(
            name="knowledge-service",
            base_url="http://knowledge-service.test",
            retry_policy=RetryPolicy(max_attempts=2, base_delay_ms=0, max_delay_ms=0),
            retry_budget=RetryBudget(budget_ratio=1.0),
        ),
        transport=httpx.MockTransport(handler),
    )

    try:
        data = await client.request_json("GET", "/health")
    finally:
        await client.close()

    assert data == {"ok": True}
    assert len(calls) == 2


@pytest.mark.asyncio
async def test_internal_service_client_does_not_retry_429() -> None:
    calls = 0

    async def handler(_request: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        if calls == 1:
            return httpx.Response(429, json={"detail": "rate limited"})
        return httpx.Response(200, json={"ok": True})

    client = InternalServiceClient(
        InternalServiceClientConfig(
            name="knowledge-service",
            base_url="http://knowledge-service.test",
            retry_policy=RetryPolicy(max_attempts=2, base_delay_ms=0, max_delay_ms=0),
            retry_budget=RetryBudget(budget_ratio=1.0),
        ),
        transport=httpx.MockTransport(handler),
    )

    try:
        with pytest.raises(InternalServiceHTTPError) as exc:
            await client.request_json("GET", "/api/v1/knowledge/datasets")
    finally:
        await client.close()

    assert exc.value.status_code == 429
    assert calls == 1


@pytest.mark.asyncio
async def test_internal_service_client_signs_exact_v2_request_and_propagates_request_id() -> None:
    verifier = GatewaySecret(
        secret="shared-secret-for-tests",
        version="v2",
        key_id="local",
        keys={"local": "shared-secret-for-tests"},
        replay_store=InMemoryReplayStore(),
    )
    seen = SimpleNamespace(request_id="")

    async def handler(request: httpx.Request) -> httpx.Response:
        body = await request.aread()
        seen.request_id = request.headers["x-request-id"]
        verifier.verify(
            request.headers["x-gateway-secret"],
            method=request.method,
            path=request.url.path,
            query=request.url.query.decode(),
            body=body,
        )
        return httpx.Response(200, json={"accepted": True})

    signer = GatewaySecret(
        secret="shared-secret-for-tests",
        version="v2",
        key_id="local",
        keys={"local": "shared-secret-for-tests"},
        replay_store=InMemoryReplayStore(),
    )
    client = InternalServiceClient(
        InternalServiceClientConfig(
            name="knowledge-service",
            base_url="http://knowledge-service.test",
            gateway_secret=signer,
            retry_policy=RetryPolicy(max_attempts=1, base_delay_ms=0, max_delay_ms=0),
        ),
        transport=httpx.MockTransport(handler),
    )

    token = REQUEST_ID_CTX.set("req-v2-client")
    try:
        data = await client.request_json(
            "POST",
            "/api/v1/knowledge/demo/retrieve",
            query_params={"top_k": "5"},
            json={"query": "hello"},
        )
    finally:
        REQUEST_ID_CTX.reset(token)
        await client.close()

    assert data == {"accepted": True}
    assert seen.request_id == "req-v2-client"


@pytest.mark.asyncio
async def test_internal_service_client_generates_idempotency_key_for_post_body() -> None:
    seen_keys: list[str] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        seen_keys.append(request.headers["idempotency-key"])
        return httpx.Response(200, json={"ok": True})

    client = InternalServiceClient(
        InternalServiceClientConfig(
            name="knowledge-service",
            base_url="http://knowledge-service.test",
            retry_policy=RetryPolicy(max_attempts=1, base_delay_ms=0, max_delay_ms=0),
        ),
        transport=httpx.MockTransport(handler),
    )

    try:
        await client.request_json("POST", "/api/v1/knowledge/retrieve", json={"q": "hello"})
        await client.request_json("POST", "/api/v1/knowledge/retrieve", json={"q": "hello"})
    finally:
        await client.close()

    assert len(seen_keys) == 2
    assert seen_keys[0] != seen_keys[1]
    assert seen_keys[0].startswith("knowledge-service:")
    assert seen_keys[1].startswith("knowledge-service:")


@pytest.mark.asyncio
async def test_internal_service_client_reuses_idempotency_key_across_retries() -> None:
    calls = 0
    seen_keys: list[str] = []

    async def handler(request: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        seen_keys.append(request.headers["idempotency-key"])
        if calls == 1:
            return httpx.Response(503, json={"retry": True})
        return httpx.Response(200, json={"ok": True})

    client = InternalServiceClient(
        InternalServiceClientConfig(
            name="knowledge-service",
            base_url="http://knowledge-service.test",
            retry_policy=RetryPolicy(
                max_attempts=2,
                base_delay_ms=0,
                max_delay_ms=0,
                retry_status_codes=frozenset({503}),
            ),
        ),
        transport=httpx.MockTransport(handler),
    )

    try:
        response = await client.request_json(
            "POST",
            "/api/v1/knowledge/retrieve",
            json={"q": "hello"},
        )
    finally:
        await client.close()

    assert response == {"ok": True}
    assert calls == 2
    assert len(set(seen_keys)) == 1


@pytest.mark.asyncio
async def test_internal_service_client_does_not_retry_post_without_idempotency_key() -> None:
    calls = 0

    async def handler(_request: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        return httpx.Response(503, json={"retry": False})

    client = InternalServiceClient(
        InternalServiceClientConfig(
            name="knowledge-service",
            base_url="http://knowledge-service.test",
            auto_idempotency=False,
            retry_policy=RetryPolicy(
                max_attempts=2,
                base_delay_ms=0,
                max_delay_ms=0,
                retry_status_codes=frozenset({503}),
            ),
        ),
        transport=httpx.MockTransport(handler),
    )

    try:
        response = await client.request(
            "POST",
            "/api/v1/knowledge/retrieve",
            json={"q": "hello"},
        )
    finally:
        await client.close()

    assert response.status_code == 503
    assert calls == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("idempotency_key", ["", "   "])
async def test_internal_service_client_does_not_retry_post_with_blank_idempotency_key(
    idempotency_key: str,
) -> None:
    calls = 0

    async def handler(_request: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        return httpx.Response(503, json={"retry": False})

    client = InternalServiceClient(
        InternalServiceClientConfig(
            name="knowledge-service",
            base_url="http://knowledge-service.test",
            retry_policy=RetryPolicy(
                max_attempts=2,
                base_delay_ms=0,
                max_delay_ms=0,
                retry_status_codes=frozenset({503}),
            ),
        ),
        transport=httpx.MockTransport(handler),
    )

    try:
        response = await client.request(
            "POST",
            "/api/v1/knowledge/retrieve",
            headers={"Idempotency-Key": idempotency_key},
            json={"q": "hello"},
        )
    finally:
        await client.close()

    assert response.status_code == 503
    assert calls == 1


@pytest.mark.asyncio
async def test_internal_service_client_rate_limiter_applies_to_retry_attempts() -> None:
    calls = 0
    acquired = 0

    class CountingLimiter(TokenBucketRateLimiter):
        async def acquire(self) -> None:
            nonlocal acquired
            acquired += 1

    async def handler(request: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        if calls == 1:
            raise httpx.ConnectError("temporary connect failure", request=request)
        return httpx.Response(200, json={"ok": True})

    client = InternalServiceClient(
        InternalServiceClientConfig(
            name="knowledge-service",
            base_url="http://knowledge-service.test",
            retry_policy=RetryPolicy(max_attempts=2, base_delay_ms=0, max_delay_ms=0),
            retry_budget=RetryBudget(budget_ratio=1.0),
            rate_limiter=CountingLimiter(rate=1000, burst=1000),
        ),
        transport=httpx.MockTransport(handler),
    )

    try:
        await client.request_json("GET", "/health")
    finally:
        await client.close()

    assert calls == 2
    assert acquired == 2
