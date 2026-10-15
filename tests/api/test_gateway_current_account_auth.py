"""Current local account admission; no live database or provider is used."""
from __future__ import annotations

from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import jwt
import pytest
from ai_gateway_core.exceptions import AuthError
from fastapi import FastAPI
from fastapi.responses import JSONResponse

from src.api.deps import get_auth_context, get_settings, get_user_context
from src.api.v1.providers import router as provider_router
from src.api.v1.sessions import router as session_router
from src.config.settings import (
    AuthAPIKeySettings,
    AuthenticationSettings,
    AuthJWTSettings,
    Settings,
)

SECRET = "synthetic-current-account-auth-test-secret"


def _database(*, profile=True, roles=None, permissions=None):
    return SimpleNamespace(
        enabled=True,
        get_user=AsyncMock(return_value={
            "user_id": "actor", "tenant_id": "tenant-a", "status": "active",
            "roles": ["admin"], "tier": "normal",
        } if profile else None),
        get_user_roles=AsyncMock(return_value=roles if roles is not None else ["user"]),
        get_user_permissions=AsyncMock(return_value=permissions if permissions is not None else []),
        get_api_key=AsyncMock(return_value={
            "user_id": "actor", "tenant_id": "tenant-a", "roles": ["user"],
            "permissions": ["conversation:playground:access"], "tier": "normal",
        }),
    )


def _settings():
    settings = Settings()
    settings.authentication = AuthenticationSettings(
        jwt=AuthJWTSettings(enabled=True, secret=SECRET, algorithms=["HS256"]),
        api_key=AuthAPIKeySettings(enabled=True, keys=["synthetic-static-key"]),
    )
    return settings


def _headers(mode="jwt"):
    if mode == "key":
        return {"X-API-Key": "synthetic-dynamic-key"}
    return {"Authorization": "Bearer " + jwt.encode({
        "sub": "actor", "tenant_id": "tenant-a", "roles": ["admin"],
        "permissions": ["admin:*"], "tier": "admin",
        "iat": datetime.now(timezone.utc),
        "exp": datetime.now(timezone.utc) + timedelta(minutes=5),
    }, SECRET, algorithm="HS256")}


def _request(db, mode="jwt"):
    return SimpleNamespace(
        headers=_headers(mode), client=SimpleNamespace(host="127.0.0.1"),
        state=SimpleNamespace(), app=SimpleNamespace(state=SimpleNamespace(database=db, redis=None)),
    )


def _app(db):
    app = FastAPI()
    app.include_router(session_router, prefix="/api/v1")
    app.include_router(provider_router, prefix="/api/v1")
    app.state.database = db
    app.state.redis = None
    app.state.session_manager = SimpleNamespace(get=AsyncMock(return_value=SimpleNamespace(
        user_id="actor", tenant_id="tenant-a", session_id="owned", service_id="assistant",
        created_at=None, updated_at=None, metadata={}, config={},
    )))
    app.dependency_overrides[get_settings] = _settings

    @app.exception_handler(AuthError)
    async def auth_error(_request, _exc):
        return JSONResponse({"detail": "authentication denied"}, status_code=401)

    return app


@pytest.mark.parametrize("mode", ["jwt", "key"])
@pytest.mark.parametrize("change", ["disabled", "tenant", "identity"])
async def test_existing_account_invalidity_rejects_before_session_read(mode, change):
    db = _database()
    field, value = {
        "disabled": ("status", "disabled"),
        "tenant": ("tenant_id", "tenant-b"),
        "identity": ("user_id", "someone-else"),
    }[change]
    db.get_user.return_value[field] = value
    app = _app(db)
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app), base_url="http://gateway") as client:
        response = await client.get("/api/v1/sessions/owned", headers=_headers(mode))
    assert response.status_code == 401
    app.state.session_manager.get.assert_not_awaited()
    db.get_user_roles.assert_not_awaited()


@pytest.mark.parametrize("mode", ["jwt", "key"])
@pytest.mark.parametrize("operation", ["get_user", "get_user_roles", "get_user_permissions"])
async def test_current_account_database_failures_are_closed_and_sanitized(mode, operation):
    db = _database()
    getattr(db, operation).side_effect = RuntimeError("private-storage-error-must-not-leak")
    app = _app(db)
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app), base_url="http://gateway") as client:
        response = await client.get("/api/v1/sessions/owned", headers=_headers(mode))
    assert response.status_code == 503
    assert "private-storage" not in response.text
    app.state.session_manager.get.assert_not_awaited()


@pytest.mark.parametrize("roles", [[], ["user"]], ids=["empty", "user"])
async def test_old_admin_claims_cannot_read_provider_management_after_demotion(roles):
    db = _database(roles=roles)
    app = _app(db)
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app), base_url="http://gateway") as client:
        response = await client.get("/api/v1/provider-templates", headers=_headers())
    assert response.status_code == 403
    db.get_user_permissions.assert_awaited_once_with("actor", require_current=True)


async def test_current_admin_retains_provider_management_access():
    app = _app(_database(roles=["admin"]))
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app), base_url="http://gateway") as client:
        response = await client.get("/api/v1/provider-templates", headers=_headers())
    assert response.status_code == 200
    assert isinstance(response.json(), list)


async def test_local_jwt_uses_only_current_grants_and_request_cache():
    db = _database(roles=[], permissions=["console:usage:view"])
    request = _request(db)
    user = await get_user_context(request, _settings())
    auth = await get_auth_context(request, _settings())
    assert user.roles == auth.roles == ["console:usage:view"]
    assert auth.permissions == ["console:usage:view"]
    assert user.tier == "normal"
    db.get_user.assert_awaited_once_with("actor")
    db.get_user_roles.assert_awaited_once_with("actor")
    db.get_user_permissions.assert_awaited_once_with("actor", require_current=True)


async def test_owned_key_keeps_key_roles_without_inheriting_owner_admin_role_or_tier():
    db = _database(roles=["admin"])
    db.get_user.return_value["tier"] = "admin"
    request = _request(db, "key")
    user = await get_user_context(request, _settings())
    auth = await get_auth_context(request, _settings())
    assert "admin" not in user.roles
    assert user.tier == "normal"
    assert auth.permissions == ["conversation:playground:access"]
    db.get_api_key.assert_awaited_once()


@pytest.mark.parametrize("mode", ["jwt", "key"])
async def test_external_identity_without_local_profile_retains_existing_authority(mode):
    db = _database(profile=False)
    request = _request(db, mode)
    user = await get_user_context(request, _settings())
    assert user.is_authenticated
    assert user.user_id == "actor" and user.tenant_id == "tenant-a"
    assert ("admin" in user.roles) == (mode == "jwt")
    db.get_user_roles.assert_not_awaited()


async def test_static_key_remains_separate_from_account_jwt_admission():
    db = _database()
    db.get_api_key.return_value = None
    request = _request(db)
    request.headers = {"X-API-Key": "synthetic-static-key"}
    user = await get_user_context(request, _settings())
    assert user.is_authenticated and user.user_id.startswith("apikey:")
    db.get_user.assert_not_awaited()


async def test_database_disabled_retains_existing_jwt_contract():
    db = _database()
    db.enabled = False
    user = await get_user_context(_request(db), _settings())
    assert user.roles == ["admin"] and user.tier == "admin"
    db.get_user.assert_not_awaited()


async def test_current_account_explicit_admin_tier_remains_authoritative():
    db = _database(roles=["user"])
    db.get_user.return_value["tier"] = "admin"
    user = await get_user_context(_request(db), _settings())
    assert user.roles == ["user"] and user.tier == "admin"
