"""Authoritative tenant introspection and conditional thread creation."""

from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import jwt
import pytest
from fastapi import FastAPI

from src.api.deps import get_settings
from src.api.v1.auth import router as auth_router
from src.api.v2.agent import router as agent_router
from src.config.settings import (
    AuthAPIKeySettings,
    AuthenticationSettings,
    AuthJWTSettings,
    Settings,
)

TEST_SECRET = "phase-one-auth-test-secret-not-a-real-credential"


def _app(*, mode: str, profile: bool, tenant: str = "tenant-a") -> FastAPI:
    settings = Settings()
    settings.authentication = AuthenticationSettings(
        jwt=AuthJWTSettings(enabled=mode == "jwt", secret=TEST_SECRET, algorithms=["HS256"]),
        api_key=AuthAPIKeySettings(enabled=mode == "api_key", keys=[]),
    )
    app = FastAPI()
    app.include_router(auth_router, prefix="/api/v1")
    app.include_router(agent_router, prefix="/api/v2")
    app.dependency_overrides[get_settings] = lambda: settings
    app.state.database = SimpleNamespace(
        enabled=True,
        get_api_key=AsyncMock(return_value={
            "user_id": "eval-user", "tenant_id": tenant, "roles": ["user"],
        }),
        get_user=AsyncMock(return_value={
            "user_id": "eval-user", "tenant_id": tenant, "status": "active",
            "roles": ["user"], "tier": "normal",
        } if profile else None),
        get_user_roles=AsyncMock(return_value=["user"]),
        get_user_permissions=AsyncMock(return_value=[]),
    )
    app.state.session_manager = SimpleNamespace(
        get=AsyncMock(), create=AsyncMock(),
    )
    return app


def _headers(mode: str, *, tenant: str = "tenant-a") -> dict[str, str]:
    if mode == "api_key":
        auth = {"X-API-Key": "phase-one-test-api-key"}
    else:
        token = jwt.encode({
            "sub": "eval-user", "tenant_id": tenant, "roles": ["user"],
            "exp": datetime.now(timezone.utc) + timedelta(minutes=5),
        }, TEST_SECRET, algorithm="HS256")
        auth = {"Authorization": f"Bearer {token}"}
    return {**auth, "X-Tenant-Id": "forged-tenant", "X-User-Id": "forged-user"}


@pytest.mark.parametrize("mode", ["jwt", "api_key"])
@pytest.mark.parametrize("profile", [True, False])
async def test_auth_me_returns_tenant_from_verified_credentials(mode: str, profile: bool) -> None:
    app = _app(mode=mode, profile=profile)
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app), base_url="http://gateway") as client:
        response = await client.get("/api/v1/auth/me", headers=_headers(mode))

    assert response.status_code == 200
    assert response.json()["tenant_id"] == "tenant-a"
    assert response.json()["user_id"] == "eval-user"


@pytest.mark.parametrize("mode", ["jwt", "api_key"])
async def test_thread_expected_tenant_fails_before_any_session_access(mode: str) -> None:
    # Models a key whose binding changed after an earlier tenant-a introspection.
    app = _app(mode=mode, profile=False, tenant="tenant-b")
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app), base_url="http://gateway") as client:
        response = await client.post(
            "/api/v2/agent/threads",
            headers=_headers(mode, tenant="tenant-b"),
            json={"session_id": "eval-case", "expected_tenant_id": "tenant-a"},
        )

    assert response.status_code == 409
    assert response.json()["detail"]["code"] == "AGENT_RUNTIME_TENANT_PRECONDITION_FAILED"
    app.state.session_manager.get.assert_not_awaited()
    app.state.session_manager.create.assert_not_awaited()
