from __future__ import annotations

import asyncio
import json
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock
from urllib.parse import parse_qs, urlsplit

import pytest
from ai_gateway_core.security import encrypt_value
from fastapi import HTTPException

from src.api.v1.connectors import (
    OAUTH_STATE_TTL_SECONDS,
    _consume_oauth_state,
    _decrypt_connector_secret,
    _oauth_console_redirect,
    _refresh_token_if_needed,
    _store_oauth_state,
    initiate_oauth,
    oauth_callback,
)
from src.core.auth.user_resolver import UserContext


class _FakeRedis:
    def __init__(self) -> None:
        self.saved: dict[str, tuple[dict, int | None]] = {}
        self.deleted: list[str] = []

    async def save(self, key: str, value: dict, ttl: int | None = None) -> None:
        self.saved[key] = (value, ttl)

    async def getdel(self, key: str):
        self.deleted.append(key)
        item = self.saved.pop(key, None)
        return item[0] if item else None


def _request(
    redis=None,
    database=None,
    encryption_key: str = "connector-oauth-test-key",
) -> SimpleNamespace:
    return SimpleNamespace(
        app=SimpleNamespace(
            state=SimpleNamespace(
                redis=redis,
                database=database,
                connector_encryption_key=encryption_key,
            )
        ),
        base_url="http://test/",
    )


def _user(user_id: str = "user", tenant_id: str = "tenant") -> UserContext:
    return UserContext(
        user_id=user_id,
        tenant_id=tenant_id,
        is_authenticated=True,
    )


def test_oauth_console_redirect_uses_explicit_console_origin(monkeypatch) -> None:
    monkeypatch.setenv(
        "CORS_ALLOWED_ORIGINS",
        "https://wrong-allowed.example,https://console.example",
    )
    monkeypatch.setenv("CONSOLE_PUBLIC_BASE_URL", "https://console.example/")

    response = _oauth_console_redirect("google drive", status="connected")

    assert response.headers["location"] == (
        "https://console.example/connections?connected=google%20drive&status=connected"
    )


def test_oauth_console_redirect_is_relative_without_valid_explicit_origin(monkeypatch) -> None:
    monkeypatch.setenv("CORS_ALLOWED_ORIGINS", "https://wrong-allowed.example")
    monkeypatch.setenv("CONSOLE_PUBLIC_BASE_URL", "https://user:secret@example.com/path")

    response = _oauth_console_redirect("github")

    assert response.headers["location"] == "/connections?connected=github&status=connected"


@pytest.mark.asyncio
async def test_oauth_callback_rejects_unissued_state_before_token_exchange() -> None:
    with pytest.raises(HTTPException) as exc_info:
        await oauth_callback(
            provider="github",
            code="attacker-code",
            state="tenant:user:github:forgednonce",
            request=_request(),
        )

    assert exc_info.value.status_code == 400
    assert "state" in str(exc_info.value.detail).lower()


@pytest.mark.asyncio
async def test_oauth_state_is_stored_in_redis_with_ttl_and_consumed_once() -> None:
    redis = _FakeRedis()
    request = _request(redis)
    state = "tenant:user:github:nonce"

    await _store_oauth_state(
        request,
        state=state,
        tenant_id="tenant",
        user_id="user",
        provider="github",
        nonce="nonce",
    )

    assert not hasattr(request.app.state, "oauth_states")
    assert len(redis.saved) == 1
    stored_key, (stored_value, ttl) = next(iter(redis.saved.items()))
    assert stored_key.endswith(state)
    assert ttl == OAUTH_STATE_TTL_SECONDS
    assert stored_value["tenant_id"] == "tenant"

    consumed = await _consume_oauth_state(request, state)

    assert consumed["user_id"] == "user"
    assert redis.deleted == [stored_key]


@pytest.mark.asyncio
async def test_oauth_callback_rejects_state_provider_mismatch_without_user(monkeypatch) -> None:
    token_post = AsyncMock()
    monkeypatch.setattr("src.api.v1.connectors.httpx.AsyncClient.post", token_post)
    redis = _FakeRedis()
    request = _request(redis)
    state = "tenant:alice:github:nonce"
    await _store_oauth_state(
        request,
        state=state,
        tenant_id="tenant",
        user_id="alice",
        provider="github",
        nonce="nonce",
    )

    with pytest.raises(HTTPException) as exc_info:
        await oauth_callback(
            provider="slack",
            code="provider-code",
            state=state,
            request=request,
        )

    assert exc_info.value.status_code == 400
    token_post.assert_not_called()


class _ConnectorDb:
    def __init__(self, config: dict | None = None) -> None:
        self.config = config
        self.executions: list[tuple[str, tuple]] = []

    async def fetchrow(self, query: str, *_args):
        if "FROM connector_configs" in query:
            return self.config
        raise AssertionError(f"unexpected query: {query}")

    async def execute(self, query: str, *args) -> None:
        self.executions.append((query, args))


@pytest.mark.asyncio
async def test_oauth_init_decodes_json_string_extra_config() -> None:
    db = _ConnectorDb(
        {
            "tenant_id": "tenant",
            "provider": "confluence",
            "client_id": "client-id",
            "auth_url": "https://oauth.example/authorize",
            "token_url": "https://oauth.example/token",
            "redirect_uri": None,
            "scopes": "read",
            "enabled": True,
            "extra_config": '{"audience":"custom-audience"}',
        }
    )
    request = _request(redis=_FakeRedis(), database=db)

    result = await initiate_oauth("confluence", request, _user())

    params = parse_qs(urlsplit(result["auth_url"]).query)
    assert params["audience"] == ["custom-audience"]


@pytest.mark.asyncio
async def test_oauth_callback_decrypts_secret_and_uses_safe_form_post(monkeypatch) -> None:
    key = "connector-oauth-test-key"
    db = _ConnectorDb(
        {
            "tenant_id": "tenant",
            "provider": "github",
            "client_id": "client-id",
            "client_secret": encrypt_value("synthetic-client-secret", key),
            "auth_url": "https://oauth.example/authorize",
            "token_url": "https://oauth.example/token",
            "redirect_uri": None,
            "scopes": "repo:read",
            "enabled": True,
            "extra_config": {},
        }
    )
    redis = _FakeRedis()
    request = _request(redis=redis, database=db, encryption_key=key)
    state = "tenant:user:github:nonce"
    await _store_oauth_state(
        request,
        state=state,
        tenant_id="tenant",
        user_id="user",
        provider="github",
        nonce="nonce",
    )
    response = SimpleNamespace(
        status_code=200,
        json=lambda: {
            "access_token": "synthetic-access-token",
            "refresh_token": "synthetic-refresh-token",
            "expires_in": 3600,
        },
    )
    safe_post = AsyncMock(return_value=response)
    monkeypatch.setattr("src.api.v1.connectors.safe_form_post", safe_post)

    result = await oauth_callback(
        provider="github",
        code="provider-code",
        state=state,
        request=request,
    )

    assert result.status_code == 302
    assert "connected=github" in result.headers["location"]
    call = safe_post.await_args
    assert call.args == ("https://oauth.example/token",)
    assert call.kwargs["data"]["client_secret"] == "synthetic-client-secret"
    assert call.kwargs["headers"] == {"Accept": "application/json"}
    assert db.executions


@pytest.mark.asyncio
async def test_refresh_uses_safe_form_post_with_decrypted_secret(monkeypatch) -> None:
    key = "connector-oauth-test-key"
    request = _request(encryption_key=key)
    config = _decrypt_connector_secret(
        request,
        {
            "client_id": "client-id",
            "client_secret": encrypt_value("synthetic-client-secret", key),
            "token_url": "https://oauth.example/token",
        },
    )
    db = _ConnectorDb()
    response = SimpleNamespace(
        status_code=200,
        json=lambda: {"access_token": "new-access-token", "expires_in": 3600},
    )
    safe_post = AsyncMock(return_value=response)
    monkeypatch.setattr("src.api.v1.connectors.safe_form_post", safe_post)

    token = await _refresh_token_if_needed(
        db,
        request,
        {
            "id": "connector-id",
            "access_token": "expired-token",
            "refresh_token": "synthetic-refresh-token",
            "token_expires_at": datetime.now(timezone.utc) - timedelta(seconds=1),
        },
        config,
    )

    assert token == "new-access-token"
    call = safe_post.await_args
    assert call.args == ("https://oauth.example/token",)
    assert call.kwargs["data"]["client_secret"] == "synthetic-client-secret"
    assert db.executions


@pytest.mark.asyncio
@pytest.mark.parametrize("origin,expected", [
    ("https://console.example/", "https://console.example"),
    ("https://user:secret@example.com/path", ""),
    ("", ""),
])
async def test_denial_mounted_callback_validates_state_and_never_exchanges(monkeypatch, origin, expected):
    import httpx
    from fastapi import FastAPI

    from src.api.v1.connectors import router

    monkeypatch.setenv("CONSOLE_PUBLIC_BASE_URL", origin)
    safe_post = AsyncMock(side_effect=AssertionError("denial must not exchange"))
    monkeypatch.setattr("src.api.v1.connectors.safe_form_post", safe_post)
    redis = _FakeRedis()
    request = _request(redis=redis)
    state = "tenant:user:github:nonce"
    await _store_oauth_state(request, state=state, tenant_id="tenant", user_id="user", provider="github", nonce="nonce")
    app = FastAPI()
    app.state.redis = redis
    app.include_router(router, prefix="/api/v1")
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://test") as client:
        response = await client.get("/api/v1/connectors/callback/github", params={
            "state": state, "error": "access_denied-SENSITIVE", "error_description": "SENSITIVE", "error_uri": "https://untrusted.example/SENSITIVE",
        })
        assert response.status_code == 302
        assert response.headers["location"] == expected + "/connections?status=authorization_failed"
        assert "SENSITIVE" not in str(response.headers) + response.text
        replay = await client.get("/api/v1/connectors/callback/github", params={"state": state, "error": "access_denied"})
        assert replay.status_code == 400
        assert "location" not in replay.headers
    safe_post.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("query,status", [
    ({}, 422),
    ({"error": ""}, 400),
    ({"code": "code", "error": "access_denied"}, 400),
    ({"code": "", "error": "access_denied"}, 400),
    ({"error": "access_denied", "state": "bad"}, 400),
    ({"error": "access_denied", "state": "tenant:user:other:nonce"}, 400),
    ({"error": "access_denied", "state": "tenant:user:github:unissued"}, 400),
])
async def test_malformed_denial_stays_http_error(monkeypatch, query, status):
    import httpx
    from fastapi import FastAPI

    from src.api.v1.connectors import router

    safe_post = AsyncMock(side_effect=AssertionError("must not exchange"))
    monkeypatch.setattr("src.api.v1.connectors.safe_form_post", safe_post)
    app = FastAPI()
    app.include_router(router, prefix="/api/v1")
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://test") as client:
        response = await client.get("/api/v1/connectors/callback/github", params=query)
    assert response.status_code == status
    assert "location" not in response.headers
    safe_post.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("field,value", [
    ("tenant_id", "other"), ("user_id", "other"), ("provider", "other"), ("nonce", "other"),
    ("expires_at", "2000-01-01T00:00:00+00:00"),
])
async def test_denial_rejects_expired_or_mismatched_issued_state(field, value):
    redis = _FakeRedis()
    request = _request(redis=redis)
    state = "tenant:user:github:nonce"
    await _store_oauth_state(request, state=state, tenant_id="tenant", user_id="user", provider="github", nonce="nonce")
    next(iter(redis.saved.values()))[0][field] = value
    with pytest.raises(HTTPException) as caught:
        await oauth_callback(provider="github", request=request, state=state, error="access_denied")
    assert caught.value.status_code == 400
    assert not redis.saved


def test_published_callback_query_contract_matches_production_route():
    import json
    from pathlib import Path

    from fastapi import FastAPI

    from src.api.v1.connectors import router

    app = FastAPI()
    app.include_router(router, prefix="/api/v1")
    path = "/api/v1/connectors/callback/{provider}"
    actual = app.openapi()["paths"][path]["get"]
    published = json.loads(Path("sdk/openapi.json").read_text())["paths"][path]["get"]
    assert actual["parameters"] == published["parameters"]
    assert actual["description"] == published["description"]


@pytest.mark.asyncio
async def test_denial_state_storage_failure_does_not_redirect():
    redis = _FakeRedis()
    request = _request(redis=redis)
    state = "tenant:user:github:nonce"
    await _store_oauth_state(request, state=state, tenant_id="tenant", user_id="user", provider="github", nonce="nonce")
    redis.getdel = AsyncMock(side_effect=RuntimeError("controlled storage failure"))
    with pytest.raises(HTTPException) as caught:
        await oauth_callback(provider="github", request=request, state=state, error="access_denied")
    assert caught.value.status_code == 503


@pytest.mark.asyncio
async def test_token_exchange_failure_stays_http_error_without_retry(monkeypatch):
    db = _ConnectorDb({"client_id": "fixture", "token_url": "https://fixture.example/token", "scopes": "read"})
    request = _request(redis=_FakeRedis(), database=db)
    state = "tenant:user:github:nonce"
    await _store_oauth_state(request, state=state, tenant_id="tenant", user_id="user", provider="github", nonce="nonce")
    safe_post = AsyncMock(return_value=SimpleNamespace(status_code=503))
    monkeypatch.setattr("src.api.v1.connectors.safe_form_post", safe_post)
    with pytest.raises(HTTPException) as caught:
        await oauth_callback(provider="github", request=request, state=state, code="code")
    assert caught.value.status_code == 400
    safe_post.assert_awaited_once()
    assert not db.executions


@pytest.mark.asyncio
@pytest.mark.parametrize("second_denial", [False, True])
async def test_concurrent_callbacks_consume_once(monkeypatch, second_denial):
    class ContendedRedis(_FakeRedis):
        def __init__(self):
            super().__init__()
            self.ready = asyncio.Event()
            self.arrivals = 0

        async def getdel(self, key):
            self.arrivals += 1
            if self.arrivals == 2:
                self.ready.set()
            await self.ready.wait()
            return await super().getdel(key)

    redis = ContendedRedis()
    db = _ConnectorDb({"client_id": "fixture", "token_url": "https://fixture.example/token", "scopes": "read"})
    request = _request(redis=redis, database=db)
    state = "tenant:user:github:nonce"
    await _store_oauth_state(request, state=state, tenant_id="tenant", user_id="user", provider="github", nonce="nonce")
    safe_post = AsyncMock(return_value=SimpleNamespace(status_code=200, json=lambda: {"access_token": "fixture"}))
    monkeypatch.setattr("src.api.v1.connectors.safe_form_post", safe_post)
    results = await asyncio.wait_for(asyncio.gather(
        oauth_callback(provider="github", request=request, state=state, code="code"),
        oauth_callback(provider="github", request=request, state=state, **({"error": "access_denied"} if second_denial else {"code": "code"})),
        return_exceptions=True,
    ), timeout=2)
    assert sorted(result.status_code for result in results) == [302, 400]
    winner = next(result for result in results if result.status_code == 302)
    expected_exchanges = int("connected=github" in winner.headers["location"])
    assert safe_post.await_count == expected_exchanges
    assert len(db.executions) == expected_exchanges
    assert len(redis.deleted) == 2
    assert not redis.saved


@pytest.mark.asyncio
async def test_nonatomic_legacy_adapter_is_rejected_without_using_it():
    legacy = SimpleNamespace(get=AsyncMock(), delete=AsyncMock())
    with pytest.raises(HTTPException) as caught:
        await _consume_oauth_state(_request(redis=legacy), "tenant:user:github:nonce")
    assert caught.value.status_code == 503
    legacy.get.assert_not_awaited()
    legacy.delete.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize("wrapped", [False, True])
async def test_production_redis_adapter_emits_one_getdel(wrapped):
    from ai_gateway_core.persistence.redis import RedisStorage
    from redis.asyncio import Redis
    from redis.exceptions import ResponseError

    state = "tenant:user:github:nonce"
    entry = {"tenant_id": "tenant", "user_id": "user", "provider": "github", "nonce": "nonce", "expires_at": (datetime.now(timezone.utc) + timedelta(minutes=5)).isoformat()}
    # Exercise installed redis-py GETDEL dispatch at a controlled command boundary.
    client = Redis()
    client.execute_command = AsyncMock(return_value=json.dumps(entry))
    adapter = client
    if wrapped:
        adapter = RedisStorage()
        adapter._client = client
    assert await _consume_oauth_state(_request(redis=adapter), state) == entry
    client.execute_command.assert_awaited_once_with("GETDEL", "oauth:state:" + state)
    client.execute_command.reset_mock()
    client.execute_command.side_effect = ResponseError("unknown command GETDEL")
    with pytest.raises(HTTPException) as caught:
        await _consume_oauth_state(_request(redis=adapter), state)
    assert caught.value.status_code == 503
    client.execute_command.assert_awaited_once_with("GETDEL", "oauth:state:" + state)


@pytest.mark.asyncio
async def test_malformed_record_is_burned_before_decode():
    redis = _FakeRedis()
    state = "tenant:user:github:nonce"
    redis.saved["oauth:state:" + state] = ("{broken", 600)
    with pytest.raises(HTTPException) as caught:
        await _consume_oauth_state(_request(redis=redis), state)
    assert caught.value.status_code == 503
    assert not redis.saved
    with pytest.raises(HTTPException) as replay:
        await _consume_oauth_state(_request(redis=redis), state)
    assert replay.value.status_code == 400
