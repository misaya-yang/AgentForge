from datetime import datetime, timedelta, timezone
from types import SimpleNamespace

import jwt
import pytest
from fastapi import HTTPException

from src.api.deps import get_auth_context
from src.api.v1.users import ProfileUpdate, update_user_profile
from src.config.settings import (
    AuthAPIKeySettings,
    AuthenticationSettings,
    AuthJWTSettings,
    Settings,
)
from src.core.auth.rbac import RBAC
from src.core.auth.user_resolver import UserContext
from ai_gateway_core.exceptions import AuthError

TEST_JWT_SECRET = "test-secret-key"
TEST_JWT_ALGORITHM = "HS256"


def _make_token(extra_payload: dict | None = None) -> str:
    payload = {
        "sub": "user_1",
        "user_id": "user_1",
        "roles": ["user"],
        "exp": datetime.now(timezone.utc) + timedelta(hours=1),
        "iat": datetime.now(timezone.utc),
    }
    if extra_payload:
        payload.update(extra_payload)
    return jwt.encode(payload, TEST_JWT_SECRET, algorithm=TEST_JWT_ALGORITHM)


def _make_settings() -> Settings:
    settings = Settings()
    settings.authentication = AuthenticationSettings(
        jwt=AuthJWTSettings(
            enabled=True,
            secret=TEST_JWT_SECRET,
            algorithms=[TEST_JWT_ALGORITHM],
        )
    )
    return settings


def _make_api_key_settings() -> Settings:
    settings = Settings()
    settings.authentication = AuthenticationSettings(
        jwt=AuthJWTSettings(enabled=False, secret=TEST_JWT_SECRET, algorithms=[TEST_JWT_ALGORITHM]),
        api_key=AuthAPIKeySettings(enabled=True, header_name="X-API-Key"),
    )
    return settings


def _make_request(token: str, db=None, redis=None):
    request = SimpleNamespace()
    request.headers = {"Authorization": f"Bearer {token}"}
    request.app = SimpleNamespace()
    request.app.state = SimpleNamespace(database=db, redis=redis)
    request.state = SimpleNamespace()
    return request


def test_rbac_permission_in_roles_allows():
    rbac = RBAC(role_permissions={"user": ["console:dashboard:view"]})
    roles = ["user", "user:list"]
    assert rbac.has_permission(roles, "user:list") is True


def test_rbac_wildcard_allows():
    rbac = RBAC(role_permissions={"user": []})
    roles = ["user", "console:*"]
    assert rbac.has_permission(roles, "console:services:view") is True


@pytest.mark.asyncio
async def test_auth_context_merges_db_permissions():
    class FakeDB:
        enabled = True

        async def get_user_permissions(self, user_id: str):
            return ["user:list"]

    token = _make_token()
    request = _make_request(token, db=FakeDB())
    settings = _make_settings()

    ctx = await get_auth_context(request, settings)

    assert "user:list" in ctx.permissions
    assert "user:list" in ctx.roles


@pytest.mark.asyncio
async def test_auth_context_revoked_token_denied():
    class FakeRedis:
        enabled = True

        async def validate_token(self, token_id: str) -> bool:
            return False

    token = _make_token({"jti": "revoked-token-id"})
    request = _make_request(token, redis=FakeRedis())
    settings = _make_settings()

    with pytest.raises(AuthError):
        await get_auth_context(request, settings)


@pytest.mark.asyncio
async def test_auth_context_api_key_merges_db_permissions():
    class FakeDB:
        enabled = True

        async def get_api_key(self, key_hash: str):
            return {
                "key_hash": key_hash,
                "user_id": "api_user_1",
                "tenant_id": "tenant_1",
                "roles": ["user"],
            }

        async def get_user_permissions(self, user_id: str):
            return ["conversation:playground:access"]

    request = SimpleNamespace()
    request.headers = {"X-API-Key": "gw_test_key"}
    request.app = SimpleNamespace()
    request.app.state = SimpleNamespace(database=FakeDB(), redis=None)
    request.state = SimpleNamespace(api_key_info=None, api_key_hash=None)

    ctx = await get_auth_context(request, _make_api_key_settings())
    assert "conversation:playground:access" in ctx.permissions
    assert "conversation:playground:access" in ctx.roles


@pytest.mark.asyncio
async def test_profile_update_rejects_other_user():
    class FakeDB:
        enabled = True

        async def get_user(self, user_id: str):
            return {"user_id": user_id, "email": "u@example.com"}

        async def update_user(self, user_id: str, updates: dict):
            return None

        async def get_user_extra_permissions(self, user_id: str):
            return []

    request = SimpleNamespace()
    request.app = SimpleNamespace()
    request.app.state = SimpleNamespace(database=FakeDB())

    current_user = UserContext(
        user_id="user_a",
        tenant_id="default",
        tier="normal",
        is_authenticated=True,
        roles=["user"],
    )

    with pytest.raises(HTTPException) as exc:
        await update_user_profile(
            "user_b",
            ProfileUpdate(display_name="x"),
            request,
            current_user,
        )
    assert exc.value.status_code == 403
