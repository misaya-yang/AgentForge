from __future__ import annotations

from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import jwt
import pytest
from ai_gateway_core.exceptions import AuthError

from src.api.deps import get_auth_context, get_user_context
from src.config.settings import (
    AuthAPIKeySettings,
    AuthenticationSettings,
    AuthJWTSettings,
    Settings,
)

TEST_JWT_SECRET = "test-secret-key-for-gateway-auth-contract"
TEST_JWT_ALGORITHM = "HS256"


class RecordingSecurityRecorder:
    def __init__(self) -> None:
        self.events: list[dict] = []

    async def record_event(self, **kwargs) -> None:
        self.events.append(kwargs)


def _jwt_settings() -> Settings:
    settings = Settings()
    settings.authentication = AuthenticationSettings(
        jwt=AuthJWTSettings(
            enabled=True,
            secret=TEST_JWT_SECRET,
            algorithms=[TEST_JWT_ALGORITHM],
        ),
        api_key=AuthAPIKeySettings(enabled=False),
    )
    return settings


def _api_key_settings() -> Settings:
    settings = Settings()
    settings.authentication = AuthenticationSettings(
        jwt=AuthJWTSettings(enabled=False),
        api_key=AuthAPIKeySettings(enabled=True, header_name="X-API-Key", keys=["valid-static"]),
    )
    return settings


def _token(payload: dict) -> str:
    base = {
        "exp": datetime.now(timezone.utc) + timedelta(hours=1),
        "iat": datetime.now(timezone.utc),
    }
    base.update(payload)
    return jwt.encode(base, TEST_JWT_SECRET, algorithm=TEST_JWT_ALGORITHM)


def _request(headers: dict[str, str] | None = None, database=None, redis=None) -> SimpleNamespace:
    request = SimpleNamespace()
    request.headers = headers or {}
    request.client = SimpleNamespace(host="127.0.0.1")
    request.url = SimpleNamespace(path="/api/v1/proxy/agent/assistants/search")
    request.app = SimpleNamespace()
    request.app.state = SimpleNamespace(database=database, redis=redis)
    request.state = SimpleNamespace()
    return request


@pytest.mark.asyncio
async def test_jwt_without_subject_is_rejected_and_records_one_auth_failure(monkeypatch) -> None:
    recorder = RecordingSecurityRecorder()
    monkeypatch.setattr(
        "src.services.metrics.get_security_event_recorder",
        lambda: recorder,
    )
    token = _token(
        {
            "roles": ["user"],
            "tenant_id": "tenant-a",
        }
    )
    request = _request({"Authorization": f"Bearer {token}"})

    with pytest.raises(AuthError):
        await get_auth_context(request, _jwt_settings())

    assert len(recorder.events) == 1
    assert recorder.events[0]["event_type"] == "auth_failed"
    assert recorder.events[0]["tenant_id"] == "tenant-a"
    assert recorder.events[0]["user_id"] is None
    assert not hasattr(request.state, "_auth_resolution")


@pytest.mark.asyncio
async def test_static_api_key_failure_is_auth_error_not_uninitialized_tenant(monkeypatch) -> None:
    recorder = RecordingSecurityRecorder()
    monkeypatch.setattr(
        "src.services.metrics.get_security_event_recorder",
        lambda: recorder,
    )
    request = _request({"X-API-Key": "invalid-static"})

    with pytest.raises(AuthError, match="Invalid API key"):
        await get_auth_context(request, _api_key_settings())

    assert len(recorder.events) == 1
    assert recorder.events[0]["event_type"] == "auth_failed"
    assert recorder.events[0]["tenant_id"] == "public"


@pytest.mark.asyncio
async def test_api_key_info_is_cached_between_user_and_auth_dependencies() -> None:
    database = SimpleNamespace(
        enabled=True,
        get_user=AsyncMock(return_value=None),
        get_api_key=AsyncMock(
            return_value={
                "tenant_id": "tenant-a",
                "user_id": "api-user",
                "roles": ["user"],
                "permissions": ["conversation:playground:access"],
                "tier": "normal",
            }
        ),
        get_user_permissions=AsyncMock(return_value=["console:services:view"]),
    )
    request = _request({"X-API-Key": "dynamic-key"}, database=database)
    settings = _api_key_settings()

    user = await get_user_context(request, settings)
    auth = await get_auth_context(request, settings)

    assert user.user_id == auth.user_id == "api-user"
    assert user.tenant_id == auth.tenant_id == "tenant-a"
    assert "conversation:playground:access" in auth.permissions
    assert "console:services:view" in auth.permissions
    assert database.get_api_key.await_count == 1


@pytest.mark.asyncio
async def test_user_and_auth_context_match_for_jwt_identity() -> None:
    database = SimpleNamespace(
        enabled=True,
        get_user=AsyncMock(return_value=None),
        get_user_permissions=AsyncMock(return_value=["console:usage:view"]),
    )
    token = _token(
        {
            "sub": "user-a",
            "tenant_id": "tenant-a",
            "roles": ["operator"],
            "permissions": ["conversation:playground:access"],
        }
    )
    request = _request({"Authorization": f"Bearer {token}"}, database=database)
    settings = _jwt_settings()

    user = await get_user_context(request, settings)
    auth = await get_auth_context(request, settings)

    assert user.user_id == auth.user_id == "user-a"
    assert user.tenant_id == auth.tenant_id == "tenant-a"
    assert auth.roles == user.roles
    assert "conversation:playground:access" in auth.permissions
    assert "console:usage:view" in auth.permissions


@pytest.mark.asyncio
async def test_anonymous_context_is_public_guest_and_not_authenticated() -> None:
    request = _request()
    settings = _jwt_settings()

    user = await get_user_context(request, settings)
    auth = await get_auth_context(request, settings)

    assert user.user_id.startswith("anon:")
    assert user.tenant_id == "public"
    assert user.roles == ["guest"]
    assert user.is_authenticated is False
    assert auth.user_id == user.user_id
    assert auth.tenant_id == "public"
    assert auth.roles == ["guest"]
    assert auth.is_authenticated is False
