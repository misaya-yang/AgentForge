"""Actual Roles router and authentication dependency; no live services or sockets."""
from __future__ import annotations

from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import jwt
import pytest
from ai_gateway_core.exceptions import AuthError, PermissionDeniedError
from fastapi import FastAPI
from fastapi.responses import JSONResponse
from httpx import ASGITransport, AsyncClient

from src.api.v1.roles import router
from src.config.settings import (
    AuthAPIKeySettings,
    AuthenticationSettings,
    AuthJWTSettings,
    Settings,
)
from src.core.auth.rbac import RBAC

SECRET = "synthetic-roles-contract-secret-only"
OPERATIONS = [
    ("GET", "/roles", None),
    ("GET", "/roles/permissions", None),
    ("GET", "/roles/custom", None),
    ("GET", "/roles/custom/users", None),
    ("POST", "/roles", {"role_name": "new_role", "description": "Shared role"}),
    ("PUT", "/roles/custom", {"description": "Updated role"}),
    ("DELETE", "/roles/custom", None),
]


@pytest.fixture
def app(monkeypatch):
    monkeypatch.delenv("JWT_SECRET", raising=False)
    monkeypatch.delenv("GATEWAY_JWT_SECRET", raising=False)
    settings = Settings()
    settings.authentication = AuthenticationSettings(
        jwt=AuthJWTSettings(enabled=True, secret=SECRET, algorithms=["HS256"]),
        api_key=AuthAPIKeySettings(enabled=False),
    )
    db = SimpleNamespace(
        enabled=True,
        get_user_permissions=AsyncMock(return_value=[]),
        get_user_for_tenant=AsyncMock(return_value={
            "user_id": "actor", "tenant_id": "tenant-a", "status": "active",
            "roles": ["platform_admin"],
        }),
        get_tenant=AsyncMock(return_value={"tenant_id": "tenant-a", "status": "active"}),
        get_user_roles=AsyncMock(return_value=["platform_admin"]),
        list_roles=AsyncMock(return_value=[{"role_name": "custom", "permissions": []}]),
        get_role=AsyncMock(return_value={"role_name": "custom", "permissions": []}),
        list_permissions=AsyncMock(return_value=[]),
        get_tenant_role_user_count=AsyncMock(return_value=1),
        get_tenant_users_by_role=AsyncMock(return_value=[{"user_id": "own-member"}]),
        get_role_user_count=AsyncMock(return_value=0),
        get_users_by_role=AsyncMock(side_effect=AssertionError("Global member read forbidden")),
        create_role=AsyncMock(), update_role=AsyncMock(), delete_role=AsyncMock(),
        get_api_key=AsyncMock(return_value=None),
    )
    application = FastAPI()
    application.state.settings = settings
    application.state.database = db
    # Tenant admin has all role permissions: explicit identity must still guard writes.
    application.state.dispatcher = SimpleNamespace(rbac=RBAC({
        "platform_admin": ["role:*"], "admin": ["role:*"], "user": ["role:list"],
        "guest": ["role:list"],
    }))
    application.state.redis = SimpleNamespace(enabled=True, validate_token=AsyncMock(return_value=True))
    application.include_router(router)

    @application.exception_handler(AuthError)
    async def auth_error(_request, _error):
        return JSONResponse(status_code=401, content={"detail": "Authentication failed"})

    @application.exception_handler(PermissionDeniedError)
    async def permission_error(_request, _error):
        return JSONResponse(status_code=403, content={"detail": "Permission denied"})

    return application


def headers(**changes):
    claims = {"sub": "actor", "tenant_id": "tenant-a", "roles": ["platform_admin"],
              "jti": "synthetic-session", "exp": datetime.now(timezone.utc) + timedelta(minutes=5)}
    claims.update(changes)
    return {"Authorization": "Bearer " + jwt.encode(claims, SECRET, algorithm="HS256")}


async def send(app, operation, authorization=None):
    method, path, body = operation
    async with AsyncClient(transport=ASGITransport(app=app), base_url="http://test") as client:
        return await client.request(method, path, json=body, headers=authorization or headers())


def assert_no_mutations(app):
    for name in ("create_role", "update_role", "delete_role"):
        getattr(app.state.database, name).assert_not_awaited()


@pytest.mark.parametrize("operation", OPERATIONS)
@pytest.mark.parametrize("identity", [{"tenant_id": ""}, {"tenant_id": " "}, {"sub": " "}])
async def test_unbound_identity_fails_closed(app, operation, identity):
    response = await send(app, operation, headers(**identity))
    assert response.status_code == 403
    assert_no_mutations(app)
    app.state.database.get_tenant_users_by_role.assert_not_awaited()


@pytest.mark.parametrize("operation", OPERATIONS)
@pytest.mark.parametrize("invalid", ["missing-user", "disabled-user", "moved-user", "missing-tenant", "disabled-tenant"])
async def test_current_account_and_tenant_required(app, operation, invalid):
    db = app.state.database
    if invalid == "missing-user":
        db.get_user_for_tenant.return_value = None
    elif invalid == "disabled-user":
        db.get_user_for_tenant.return_value["status"] = "disabled"
    elif invalid == "moved-user":
        db.get_user_for_tenant.return_value["tenant_id"] = "tenant-b"
    elif invalid == "missing-tenant":
        db.get_tenant.return_value = None
    else:
        db.get_tenant.return_value["status"] = "disabled"
    assert (await send(app, operation)).status_code == 403
    assert_no_mutations(app)
    db.list_roles.assert_not_awaited()
    db.get_role.assert_not_awaited()


@pytest.mark.parametrize("operation", OPERATIONS[4:])
@pytest.mark.parametrize("roles", [["admin"], ["role:*"], ["superadmin", "role:*"], ["admin:*"]])
async def test_global_mutation_requires_exact_platform_role(app, operation, roles):
    assert (await send(app, operation, headers(roles=roles))).status_code == 403
    assert_no_mutations(app)


@pytest.mark.parametrize("operation", OPERATIONS[4:])
@pytest.mark.parametrize("revoked_source", ["account", "assignment"])
async def test_stale_platform_claim_is_insufficient(app, operation, revoked_source):
    db = app.state.database
    if revoked_source == "account":
        db.get_user_for_tenant.return_value["roles"] = ["admin"]
    else:
        db.get_user_roles.return_value = []
    assert (await send(app, operation)).status_code == 403
    assert_no_mutations(app)


@pytest.mark.parametrize("operation", OPERATIONS)
async def test_revoked_token_rejected_by_real_dependency(app, operation):
    app.state.redis.validate_token.return_value = False
    assert (await send(app, operation)).status_code == 401
    assert_no_mutations(app)
    app.state.database.get_user_for_tenant.assert_not_awaited()


@pytest.mark.parametrize("role", ["admin", "platform_admin"])
@pytest.mark.parametrize("operation", [OPERATIONS[0], OPERATIONS[2], OPERATIONS[3]])
async def test_read_scope_is_own_tenant_including_platform_admin(app, role, operation):
    response = await send(app, operation, headers(roles=[role]))
    assert response.status_code == 200
    db = app.state.database
    db.get_user_for_tenant.assert_awaited_once_with("actor", "tenant-a")
    if operation[1].endswith("/users"):
        db.get_tenant_users_by_role.assert_awaited_once_with("custom", "tenant-a")
        assert response.json() == {"role_name": "custom", "users": [{"user_id": "own-member"}], "total": 1}
    else:
        db.get_tenant_role_user_count.assert_awaited_once_with("custom", "tenant-a")
    db.get_role_user_count.assert_not_awaited()
    db.get_users_by_role.assert_not_awaited()


async def test_create_update_and_delete_preserve_success_contract(app):
    db = app.state.database
    db.get_role.return_value = None
    response = await send(app, OPERATIONS[4])
    assert response.status_code == 200
    assert response.json()["role_name"] == "new_role"
    db.create_role.assert_awaited_once_with("new_role", "Shared role", [])
    db.get_role.return_value = {"role_name": "custom", "description": "Updated role", "permissions": []}
    response = await send(app, OPERATIONS[5])
    assert response.status_code == 200 and response.json()["user_count"] == 1
    db.get_tenant_role_user_count.assert_awaited_once_with("custom", "tenant-a")
    response = await send(app, OPERATIONS[6])
    assert response.json() == {"status": "success", "message": "Role custom deleted"}
    db.delete_role.assert_awaited_once_with("custom")


async def test_delete_uses_global_in_use_guard_even_with_zero_local_members(app):
    db = app.state.database
    db.get_tenant_role_user_count.return_value = 0
    db.get_role_user_count.return_value = 3
    response = await send(app, OPERATIONS[6])
    assert response.status_code == 400 and "3 assigned" in response.json()["detail"]
    db.get_role_user_count.assert_awaited_once_with("custom")
    db.get_tenant_role_user_count.assert_not_awaited()
    db.delete_role.assert_not_awaited()


@pytest.mark.parametrize("operation", OPERATIONS)
async def test_static_key_is_not_a_tenant_account(app, operation):
    app.state.settings.authentication = AuthenticationSettings(
        jwt=AuthJWTSettings(enabled=False),
        api_key=AuthAPIKeySettings(enabled=True, keys=["synthetic-static-key"]),
    )
    assert (await send(app, operation, {"X-API-Key": "synthetic-static-key"})).status_code == 403
    assert_no_mutations(app)


@pytest.mark.parametrize("operation", OPERATIONS)
async def test_disabled_auth_does_not_grant_catalog_access(app, operation):
    app.state.settings.authentication = AuthenticationSettings(
        jwt=AuthJWTSettings(enabled=False), api_key=AuthAPIKeySettings(enabled=False),
    )
    assert (await send(app, operation)).status_code == 403
    assert_no_mutations(app)


@pytest.mark.parametrize("operation", OPERATIONS)
async def test_permission_gate_remains_required(app, operation):
    assert (await send(app, operation, headers(roles=["no_role"]))).status_code == 403
    assert_no_mutations(app)


@pytest.mark.parametrize("operation", OPERATIONS[5:])
async def test_system_role_cannot_be_changed(app, operation):
    app.state.database.get_role.return_value["is_system"] = True
    assert (await send(app, operation)).status_code == 400
    assert_no_mutations(app)


@pytest.mark.parametrize("operation", OPERATIONS)
async def test_database_unavailable_does_not_admit_account(app, operation):
    app.state.database.enabled = False
    assert (await send(app, operation)).status_code == 503
    assert_no_mutations(app)


@pytest.mark.parametrize("operation", OPERATIONS[:4])
@pytest.mark.parametrize("stale", [["admin"], ["role:*"], ["platform_admin"]])
async def test_revoked_read_authority_not_restored_by_stale_jwt_or_cache(app, operation, stale):
    db = app.state.database
    db.get_user_for_tenant.return_value["roles"] = ["no_role"]

    async def permission_lookup(_user_id, *, require_current=False):
        return [] if require_current else ["role:*"]

    db.get_user_permissions.side_effect = permission_lookup
    assert (await send(app, operation, headers(roles=stale))).status_code == 403
    db.get_user_permissions.assert_any_await("actor", require_current=True)
    db.get_role.assert_not_awaited()
    db.list_roles.assert_not_awaited()
    db.list_permissions.assert_not_awaited()
    db.get_tenant_users_by_role.assert_not_awaited()


@pytest.mark.parametrize("operation", OPERATIONS[:4])
async def test_custom_or_direct_current_read_grant_is_preserved(app, operation):
    db = app.state.database
    db.get_user_for_tenant.return_value["roles"] = ["custom_reader"]
    db.get_user_permissions.return_value = ["role:list"]
    assert (await send(app, operation, headers(roles=["custom_reader"]))).status_code == 200
    db.get_user_permissions.assert_any_await("actor", require_current=True)


@pytest.mark.parametrize("operation", OPERATIONS[:4])
@pytest.mark.parametrize("key_grant", ["declared-permission", "declared-role", "current-owner-permission", "stale-owner-cache"])
async def test_bound_key_retains_restricted_authority(app, operation, key_grant):
    db = app.state.database
    db.get_user_for_tenant.return_value["roles"] = ["platform_admin"]
    db.get_api_key.return_value = {
        "user_id": "actor", "tenant_id": "tenant-a", "roles": ["no_role"], "permissions": [],
    }
    if key_grant == "declared-permission":
        db.get_api_key.return_value["permissions"] = ["role:list"]
    elif key_grant == "declared-role":
        db.get_api_key.return_value["roles"] = ["admin"]
    elif key_grant == "current-owner-permission":
        db.get_user_permissions.return_value = ["role:list"]
    else:
        async def stale_lookup(_user_id, *, require_current=False):
            return [] if require_current else ["role:list"]
        db.get_user_permissions.side_effect = stale_lookup
    app.state.settings.authentication = AuthenticationSettings(
        jwt=AuthJWTSettings(enabled=False),
        api_key=AuthAPIKeySettings(enabled=True),
    )
    response = await send(app, operation, {"X-API-Key": "synthetic-bound-key"})
    assert response.status_code == (403 if key_grant == "stale-owner-cache" else 200)
    db.get_user_permissions.assert_any_await("actor", require_current=True)


@pytest.mark.parametrize("operation", OPERATIONS)
async def test_current_permission_failure_prevents_data_access_and_mutation(app, operation):
    db = app.state.database

    async def failing_lookup(_user_id, *, require_current=False):
        if require_current:
            raise RuntimeError("synthetic permission storage failure")
        return []

    db.get_user_permissions.side_effect = failing_lookup
    async with AsyncClient(transport=ASGITransport(app=app, raise_app_exceptions=False), base_url="http://test") as client:
        method, path, body = operation
        response = await client.request(method, path, json=body, headers=headers())
    assert response.status_code == 500
    assert_no_mutations(app)
    db.get_role.assert_not_awaited()
    db.list_roles.assert_not_awaited()
    db.list_permissions.assert_not_awaited()


@pytest.mark.parametrize("operation", OPERATIONS)
async def test_missing_credentials_fail_closed(app, operation):
    async with AsyncClient(transport=ASGITransport(app=app), base_url="http://test") as client:
        method, path, body = operation
        response = await client.request(method, path, json=body)
    assert response.status_code == 403
    assert_no_mutations(app)
    app.state.database.get_user_for_tenant.assert_not_awaited()


@pytest.mark.parametrize("operation", OPERATIONS[:4])
async def test_empty_current_account_roles_never_synthesize_user_authority(app, operation):
    app.state.database.get_user_for_tenant.return_value["roles"] = []
    app.state.database.get_user_permissions.return_value = []
    # The configured default user has role:list, but this account has no roles.
    assert (await send(app, operation, headers(roles=["admin"]))).status_code == 403
    app.state.database.list_roles.assert_not_awaited()
    app.state.database.get_role.assert_not_awaited()
