"""
Streaming rate limit middleware tests.

Ensures the pure ASGI streaming rate limiter enforces limits for non-streaming paths.
"""

from fastapi import FastAPI
from fastapi.testclient import TestClient

from src.core.middleware.streaming import (
    StreamingAnonymousConfig,
    StreamingAnonymousMiddleware,
    StreamingAuthConfig,
    StreamingAuthMiddleware,
    StreamingRateLimitConfig,
    StreamingRateLimitMiddleware,
)


def _build_app(config: StreamingRateLimitConfig) -> FastAPI:
    app = FastAPI()
    app.add_middleware(StreamingRateLimitMiddleware, config=config)

    @app.get("/ping")
    async def ping():
        return {"ok": True}

    return app


def _build_app_with_auth_and_rate_limit(
    rate_config: StreamingRateLimitConfig,
    auth_config: StreamingAuthConfig,
    stream_path: str = "/ping",
) -> FastAPI:
    app = FastAPI()
    # Auth must execute before rate limiting to enforce user/guest limits.
    app.add_middleware(StreamingAnonymousMiddleware, config=StreamingAnonymousConfig())
    app.add_middleware(StreamingRateLimitMiddleware, config=rate_config)
    app.add_middleware(StreamingAuthMiddleware, config=auth_config)

    @app.get("/ping")
    async def ping():
        return {"ok": True}

    @app.get(stream_path)
    async def stream_ping():
        return {"ok": True}

    return app


def test_streaming_rate_limit_blocks_after_limit():
    config = StreamingRateLimitConfig(
        enabled=True,
        global_limit=2,
        global_window=60,
        user_limit=1000,
        guest_limit=1000,
        ip_limit=1000,
        whitelist_paths=[],
    )
    app = _build_app(config)

    client = TestClient(app)
    assert client.get("/ping").status_code == 200
    assert client.get("/ping").status_code == 200

    response = client.get("/ping")
    assert response.status_code == 429
    payload = response.json()
    assert payload["error"]["dimension"] == "global"


def test_rate_limit_uses_user_dimension_when_api_key_present():
    rate_config = StreamingRateLimitConfig(
        enabled=True,
        global_limit=1000,
        global_window=60,
        user_limit=1,
        user_window=60,
        guest_limit=1000,
        guest_window=60,
        ip_limit=1000,
        ip_window=60,
        whitelist_paths=[],
    )
    auth_config = StreamingAuthConfig(
        jwt_enabled=False,
        api_key_enabled=True,
        api_keys=["test-key"],
    )
    app = _build_app_with_auth_and_rate_limit(rate_config, auth_config)
    client = TestClient(app)

    headers = {"X-API-Key": "test-key"}
    assert client.get("/ping", headers=headers).status_code == 200

    response = client.get("/ping", headers=headers)
    assert response.status_code == 429
    payload = response.json()
    assert payload["error"]["dimension"] == "user"


def test_unconfigured_api_key_is_treated_as_guest_for_rate_limits():
    rate_config = StreamingRateLimitConfig(
        enabled=True,
        global_limit=1000,
        global_window=60,
        user_limit=1000,
        user_window=60,
        guest_limit=1,
        guest_window=60,
        ip_limit=1000,
        ip_window=60,
        whitelist_paths=[],
    )
    auth_config = StreamingAuthConfig(
        jwt_enabled=False,
        api_key_enabled=True,
        api_keys=[],
    )
    app = _build_app_with_auth_and_rate_limit(rate_config, auth_config)
    client = TestClient(app)

    headers = {"X-API-Key": "attacker-supplied-key"}
    assert client.get("/ping", headers=headers).status_code == 200

    response = client.get("/ping", headers=headers)
    assert response.status_code == 429
    payload = response.json()
    assert payload["error"]["dimension"] == "guest"


def test_streaming_paths_are_rate_limited():
    rate_config = StreamingRateLimitConfig(
        enabled=True,
        global_limit=1,
        global_window=60,
        user_limit=1000,
        user_window=60,
        guest_limit=1000,
        guest_window=60,
        ip_limit=1000,
        ip_window=60,
        whitelist_paths=[],
    )
    auth_config = StreamingAuthConfig(jwt_enabled=False, api_key_enabled=False)
    app = _build_app_with_auth_and_rate_limit(
        rate_config,
        auth_config,
        stream_path="/api/v1/proxy/langgraph/runs/stream",
    )
    client = TestClient(app)

    assert client.get("/api/v1/proxy/langgraph/runs/stream").status_code == 200

    response = client.get("/api/v1/proxy/langgraph/runs/stream")
    assert response.status_code == 429
    payload = response.json()
    assert payload["error"]["dimension"] == "global"
