from __future__ import annotations

from types import SimpleNamespace

import pytest
from fastapi import HTTPException

import src.api.v1.users as users_module
from src.api.deps import AuthContext
from src.api.v1.users import (
    UserCreate,
    UserUpdate,
    create_user,
    delete_user,
    disable_user,
    enable_user,
    list_users,
    reset_user_password,
    update_user,
)
from src.api.v1.users import (
    get_user as get_user_endpoint,
)


class _RBAC:
    def __init__(self) -> None:
        self.required: list[str] = []

    def require(self, _roles: list[str], permission: str) -> None:
        self.required.append(permission)


class _TenantDB:
    enabled = True

    def __init__(self) -> None:
        self.users = {
            ("tenant-a", "admin-a"): {
                "user_id": "admin-a",
                "display_name": "Tenant A admin",
                "roles": ["admin"],
                "metadata": {},
            },
            ("tenant-a", "member-a"): {
                "user_id": "member-a",
                "display_name": "Tenant A member",
                "roles": ["user"],
                "metadata": {},
            },
            ("tenant-b", "victim-b"): {
                "user_id": "victim-b",
                "display_name": "Tenant B member",
                "roles": ["user"],
                "metadata": {},
            },
        }
        self.lookup_calls: list[tuple[str, str]] = []
        self.list_calls: list[dict] = []
        self.mutations: list[tuple[str, str, str]] = []
        self.saved_user: dict | None = None
        self.assigned_roles: list[tuple[str, str, str]] = []

    async def list_users_paginated(self, **kwargs):
        self.list_calls.append(kwargs)
        tenant_id = kwargs["tenant_id"]
        users = [
            dict(user)
            for (tenant, _user_id), user in self.users.items()
            if tenant == tenant_id
        ]
        return users, len(users)

    async def get_user_for_tenant(self, user_id: str, tenant_id: str) -> dict | None:
        self.lookup_calls.append((user_id, tenant_id))
        user = self.users.get((tenant_id, user_id))
        return dict(user) if user else None

    async def update_user_for_tenant(
        self,
        user_id: str,
        tenant_id: str,
        updates: dict,
        **_kwargs,
    ) -> bool:
        key = (tenant_id, user_id)
        if key not in self.users:
            return False
        self.users[key].update(updates)
        self.mutations.append(("update", user_id, tenant_id))
        return True

    async def delete_user_for_tenant(self, user_id: str, tenant_id: str) -> bool:
        key = (tenant_id, user_id)
        if key not in self.users:
            return False
        del self.users[key]
        self.mutations.append(("delete", user_id, tenant_id))
        return True

    async def reset_user_password_for_tenant(
        self, user_id: str, tenant_id: str, _password_hash: str
    ) -> bool:
        if (tenant_id, user_id) not in self.users:
            return False
        self.mutations.append(("reset", user_id, tenant_id))
        return True

    async def get_user_extra_permissions(self, _user_id: str) -> list[dict]:
        return []

    async def get_user_by_email(self, _email: str) -> None:
        return None

    async def get_user(self, _user_id: str) -> None:
        return None

    async def save_user_with_password(self, user: dict) -> None:
        self.saved_user = dict(user)

    async def assign_user_role(self, user_id: str, role: str, granted_by: str) -> None:
        self.assigned_roles.append((user_id, role, granted_by))


def _request(db: _TenantDB, rbac: _RBAC | None = None) -> SimpleNamespace:
    return SimpleNamespace(
        app=SimpleNamespace(
            state=SimpleNamespace(
                dispatcher=SimpleNamespace(rbac=rbac or _RBAC()),
                database=db,
            )
        )
    )


def _tenant_admin() -> AuthContext:
    # The repository's Agent Studio contract treats ``admin`` as tenant-scoped.
    return AuthContext(
        user_id="admin-a",
        tenant_id="tenant-a",
        roles=["admin"],
        is_authenticated=True,
    )


@pytest.mark.asyncio
async def test_tenant_admin_list_is_pinned_to_its_tenant() -> None:
    db = _TenantDB()

    result = await list_users(
        request=_request(db),
        auth=_tenant_admin(),
        page=1,
        page_size=20,
        status=None,
        search=None,
    )

    assert db.list_calls == [
        {
            "status": None,
            "search": None,
            "tenant_id": "tenant-a",
            "limit": 20,
            "offset": 0,
        }
    ]
    assert {user.user_id for user in result.users} == {"admin-a", "member-a"}


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "operation",
    ["read", "update", "delete", "reset", "enable", "disable"],
)
async def test_tenant_admin_cannot_operate_on_a_user_in_another_tenant(
    operation: str,
) -> None:
    db = _TenantDB()
    request = _request(db)
    auth = _tenant_admin()

    with pytest.raises(HTTPException) as exc:
        if operation == "read":
            await get_user_endpoint("victim-b", request, auth)
        elif operation == "update":
            await update_user(
                "victim-b",
                UserUpdate(display_name="not allowed"),
                request,
                auth,
            )
        elif operation == "delete":
            await delete_user("victim-b", request, auth)
        elif operation == "reset":
            await reset_user_password("victim-b", request, auth)
        elif operation == "enable":
            await enable_user("victim-b", request, auth)
        else:
            await disable_user("victim-b", request, auth)

    assert exc.value.status_code == 404
    assert db.lookup_calls[-1] == ("victim-b", "tenant-a")
    assert db.mutations == []


@pytest.mark.asyncio
async def test_create_user_uses_callers_tenant(monkeypatch) -> None:
    db = _TenantDB()
    monkeypatch.setattr(users_module, "DEFAULT_PASSWORD", "test-bootstrap-password")
    monkeypatch.setattr(users_module, "hash_password", lambda _value: "test-hash")

    result = await create_user(
        UserCreate(email="new-user@example.com", display_name="New tenant user"),
        _request(db),
        _tenant_admin(),
    )

    assert result.user_id == "new-user"
    assert db.saved_user is not None
    assert db.saved_user["tenant_id"] == "tenant-a"
    assert db.assigned_roles == [("new-user", "user", "admin-a")]


@pytest.mark.asyncio
async def test_create_user_requires_configured_default_password(monkeypatch) -> None:
    db = _TenantDB()
    monkeypatch.setattr(users_module, "DEFAULT_PASSWORD", "")

    with pytest.raises(HTTPException) as exc:
        await create_user(
            UserCreate(email="new-user@example.com", display_name="New tenant user"),
            _request(db),
            _tenant_admin(),
        )

    assert exc.value.status_code == 503
    assert db.saved_user is None
