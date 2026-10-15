"""Tenant administration regressions against real FastAPI routers.

Only identity resolution and database/cache/storage collaborators are replaced.
No application startup, external service, credential, or real file deletion is used.
"""

from types import SimpleNamespace
from unittest.mock import AsyncMock, Mock

import pytest
import pytest_asyncio
from fastapi import FastAPI
from httpx import ASGITransport, AsyncClient

from src.api.v1 import files, tenant_policies
from src.core.auth.user_resolver import UserContext

PLATFORM_ROLES = ("platform_admin", "superadmin", "super_admin", " SUPER_ADMIN ")
POLICY_ADMINS = ("admin", "admin-tier", *PLATFORM_ROLES)
FILE_ADMINS = ("admin", "file_admin", "configured", *PLATFORM_ROLES)


def actor(identity="admin", tenant="tenant-a"):
    return UserContext(
        user_id="configured-user" if identity == "configured" else "operator",
        tenant_id=tenant,
        roles=[] if identity in ("admin-tier", "configured", "ordinary") else [identity],
        tier="AdMiN" if identity == "admin-tier" else "normal",
        is_authenticated=True,
    )


class Database:
    def __init__(self):
        self.calls = []

    async def fetch(self, sql, *args):
        self.calls.append((sql, args))
        return [{"tenant_id": args[0] if args else "global"}]

    async def fetchrow(self, sql, *args):
        self.calls.append((sql, args))
        return {"cnt": 1} if "COUNT(*)" in sql else {"tenant_id": args[0]}

    async def execute(self, sql, *args):
        self.calls.append((sql, args))


@pytest_asyncio.fixture
async def boundary(monkeypatch):
    state = SimpleNamespace(user=actor(), database=Database())
    app = FastAPI()
    app.state.database = state.database
    state.policy_cache = app.state.tenant_tool_policy = Mock()
    state.mcp_cache = app.state.tenant_mcp_config = Mock()
    app.dependency_overrides[files.get_user_context] = lambda: state.user
    app.include_router(files.router, prefix="/api/v1")
    app.include_router(tenant_policies.router, prefix="/api/v1")
    monkeypatch.setattr(files, "ADMIN_USER_IDS", {"configured-user"})
    state.file = Mock()
    state.file.is_file.return_value = True
    state.file.stat.return_value = SimpleNamespace(st_size=7)
    state.directory = Mock()
    state.directory.exists.return_value = True
    state.directory.iterdir.return_value = [state.file]
    state.path = Mock(return_value=state.directory)
    monkeypatch.setattr(files, "get_user_uploads_path", state.path)
    state.cleanup = SimpleNamespace(
        get_storage_stats=AsyncMock(return_value={"total_files": 1}),
        cleanup_user=AsyncMock(return_value={"files_deleted": 1}),
        run_cleanup=AsyncMock(
            return_value=SimpleNamespace(
                files_deleted=1,
                bytes_freed=7,
                users_cleaned=1,
                errors=0,
                duration_ms=1,
            )
        ),
    )
    state.cleanup_factory = Mock(return_value=state.cleanup)
    monkeypatch.setattr(files, "get_cleanup_service", state.cleanup_factory)
    async with AsyncClient(transport=ASGITransport(app=app), base_url="http://test") as client:
        state.client = client
        yield state


def assert_no_policy_access(state):
    assert state.database.calls == []
    state.policy_cache.invalidate.assert_not_called()
    state.mcp_cache.invalidate.assert_not_called()


@pytest.mark.parametrize("identity", (*FILE_ADMINS, "admin-tier", "ordinary"))
@pytest.mark.parametrize("target", (None, "", "tenant-a", "tenant-b"))
async def test_file_delete_is_tenant_scoped(boundary, identity, target):
    boundary.user = actor(identity)
    params = {} if target is None else {"target_tenant_id": target}
    response = await boundary.client.delete("/api/v1/files/admin/user/victim", params=params)
    allowed = identity in PLATFORM_ROLES or (
        identity in FILE_ADMINS and target in (None, "", "tenant-a")
    )
    assert response.status_code == (200 if allowed else 403)
    if allowed:
        boundary.path.assert_called_once_with("victim", target or "tenant-a")
        boundary.file.unlink.assert_called_once_with()
        assert response.json()["files_deleted"] == 1
        assert response.json()["bytes_freed"] == 7
    else:
        boundary.path.assert_not_called()
        boundary.file.unlink.assert_not_called()


@pytest.mark.parametrize("identity", ("admin", "file_admin", "configured"))
@pytest.mark.parametrize("tenant", ("", None))
@pytest.mark.parametrize("target", (None, "", "tenant-a", "tenant-b"))
async def test_file_delete_missing_actor_tenant_fails_closed(boundary, identity, tenant, target):
    boundary.user = actor(identity, tenant)
    params = {} if target is None else {"target_tenant_id": target}
    response = await boundary.client.delete("/api/v1/files/admin/user/victim", params=params)
    assert response.status_code == 403
    boundary.path.assert_not_called()
    boundary.file.unlink.assert_not_called()


@pytest.mark.parametrize("identity", (*FILE_ADMINS, "admin-tier", "ordinary"))
@pytest.mark.parametrize("operation", ("stats", "cleanup-all", "cleanup-user"))
async def test_global_file_operations_require_explicit_platform_role(boundary, identity, operation):
    boundary.user = actor(identity)
    if operation == "stats":
        response = await boundary.client.get("/api/v1/files/admin/stats")
    else:
        params = {"user_id": "victim"} if operation == "cleanup-user" else {}
        response = await boundary.client.post("/api/v1/files/admin/cleanup", params=params)
    assert response.status_code == (200 if identity in PLATFORM_ROLES else 403)
    if identity not in PLATFORM_ROLES:
        boundary.cleanup_factory.assert_not_called()
        for method in vars(boundary.cleanup).values():
            method.assert_not_awaited()
    else:
        boundary.cleanup_factory.assert_called_once_with()
        method = {
            "stats": "get_storage_stats",
            "cleanup-all": "run_cleanup",
            "cleanup-user": "cleanup_user",
        }[operation]
        getattr(boundary.cleanup, method).assert_awaited_once_with(
            *(("victim",) if operation == "cleanup-user" else ())
        )


@pytest.mark.parametrize("kind", ("tool-policies", "mcp-configs"))
@pytest.mark.parametrize("method", ("get", "put", "delete"))
@pytest.mark.parametrize("identity", (*POLICY_ADMINS, "file_admin", "ordinary"))
@pytest.mark.parametrize("target", ("tenant-a", "tenant-b"))
async def test_policy_crud_authorizes_tenant_before_database_or_cache(
    boundary, kind, method, identity, target
):
    boundary.user = actor(identity)
    path = f"/api/v1/admin/tenant-policies/{kind}"
    kwargs = {}
    if method == "put":
        kwargs["json"] = {"tenant_id": target}
        if kind == "mcp-configs":
            kwargs["json"]["allowed_servers"] = []
    else:
        path += f"/{target}"
    response = await boundary.client.request(method, path, **kwargs)
    allowed = identity in PLATFORM_ROLES or (identity in POLICY_ADMINS and target == "tenant-a")
    assert response.status_code == (200 if allowed else 403)
    if not allowed:
        assert_no_policy_access(boundary)
        return
    assert len(boundary.database.calls) == 1
    sql, args = boundary.database.calls[0]
    assert args[0] == target
    if method in ("get", "delete"):
        assert "tenant_id = $1" in sql
    cache = boundary.policy_cache if kind == "tool-policies" else boundary.mcp_cache
    if method == "get":
        cache.invalidate.assert_not_called()
    else:
        cache.invalidate.assert_called_once_with(target)


@pytest.mark.parametrize("identity", POLICY_ADMINS)
@pytest.mark.parametrize("kind", ("tool-policies", "mcp-configs"))
async def test_policy_list_filters_ordinary_admins(boundary, identity, kind):
    boundary.user = actor(identity)
    response = await boundary.client.get(f"/api/v1/admin/tenant-policies/{kind}")
    assert response.status_code == 200
    assert len(boundary.database.calls) == 1
    sql, args = boundary.database.calls[0]
    if identity in PLATFORM_ROLES:
        assert "WHERE" not in sql
        assert args == ()
    else:
        assert "WHERE tenant_id = $1" in sql
        assert args == ("tenant-a",)


@pytest.mark.parametrize("identity", (*POLICY_ADMINS, "ordinary"))
@pytest.mark.parametrize("target", (None, "", "tenant-a", "tenant-b"))
async def test_audit_rows_and_count_share_authorized_scope(boundary, identity, target):
    boundary.user = actor(identity)
    params = {"user_id": "victim", "tool_name": "a%_\\b", "limit": 3, "offset": 2}
    if target is not None:
        params["tenant_id"] = target
    response = await boundary.client.get("/api/v1/admin/tenant-policies/audit-log", params=params)
    allowed = identity in PLATFORM_ROLES or (
        identity in POLICY_ADMINS and target in (None, "", "tenant-a")
    )
    assert response.status_code == (200 if allowed else 403)
    if not allowed:
        assert_no_policy_access(boundary)
        return
    assert len(boundary.database.calls) == 2
    (row_sql, row_args), (count_sql, count_args) = boundary.database.calls
    scope = target if identity in PLATFORM_ROLES else "tenant-a"
    expected = ((scope,) if scope else ()) + ("victim", "%a\\%\\_\\\\b%")
    assert row_args == (*expected, 3, 2)
    assert count_args == expected
    assert ("tenant_id = $1" in row_sql) == bool(scope)
    assert ("tenant_id = $1" in count_sql) == bool(scope)
    assert "COUNT(*)" in count_sql
    assert response.json()["total"] == 1


@pytest.mark.parametrize(
    "identity,tenant",
    (("admin", ""), ("admin", None), ("admin-tier", ""), ("ordinary", "tenant-a")),
)
@pytest.mark.parametrize(
    "operation",
    (
        "list-tools",
        "list-mcp",
        "audit",
        "get-tools",
        "get-mcp",
        "put-tools",
        "put-mcp",
        "delete-tools",
        "delete-mcp",
    ),
)
async def test_policy_missing_tenant_or_non_admin_has_no_backend_access(
    boundary, identity, tenant, operation
):
    boundary.user = actor(identity, tenant)
    verb, kind = operation.split("-", 1) if "-" in operation else ("get", "audit")
    kind = {"tools": "tool-policies", "mcp": "mcp-configs", "audit": "audit-log"}[kind]
    path = f"/api/v1/admin/tenant-policies/{kind}"
    kwargs = {}
    if verb in ("get", "delete") and kind != "audit-log":
        path += "/tenant-a"
    elif verb == "put":
        kwargs["json"] = {"tenant_id": "tenant-a", "allowed_servers": []}
    response = await boundary.client.request("get" if verb == "list" else verb, path, **kwargs)
    assert response.status_code == 403
    assert_no_policy_access(boundary)
