from __future__ import annotations

from types import SimpleNamespace

import pytest
from fastapi import HTTPException

import src.api.v1.users as users_module
from src.api.deps import AuthContext
from src.api.v1.users import UserUpdate, update_user


class _RBAC:
    def require(self, _roles: list[str], permission: str) -> None:
        assert permission == "user:edit"


class _DB:
    enabled = True

    def __init__(self) -> None:
        self.updated = False
        self.extra_permissions_updated = False

    async def get_user_for_tenant(self, user_id: str, _tenant_id: str) -> dict:
        return {"user_id": user_id, "metadata": {}}

    async def update_user_for_tenant(
        self,
        _user_id: str,
        _tenant_id: str,
        _updates: dict,
        **_kwargs,
    ) -> bool:
        self.updated = True
        return True

    async def get_user_extra_permissions(self, _user_id: str) -> list[dict]:
        return []

    async def update_user_extra_permissions(
        self,
        _user_id: str,
        _permissions: list[str],
        _granted_by: str,
    ) -> None:
        self.extra_permissions_updated = True


def _request(db: _DB) -> SimpleNamespace:
    return SimpleNamespace(
        app=SimpleNamespace(
            state=SimpleNamespace(
                dispatcher=SimpleNamespace(rbac=_RBAC()),
                database=db,
            )
        )
    )


@pytest.mark.asyncio
async def test_non_admin_cannot_modify_user_service_access_policy():
    db = _DB()

    with pytest.raises(HTTPException) as exc:
        await update_user(
            user_id="target-user",
            body=UserUpdate(service_access_mode="allowlist", allowed_services=["agent"]),
            request=_request(db),
            auth=AuthContext(user_id="editor", roles=["user:edit"], is_authenticated=True),
        )

    assert exc.value.status_code == 403
    assert db.updated is False


@pytest.mark.asyncio
async def test_service_access_update_clears_constraint_cache(monkeypatch):
    db = _DB()
    clear_calls = 0

    def fake_clear_cache() -> None:
        nonlocal clear_calls
        clear_calls += 1

    monkeypatch.setattr(users_module, "clear_service_access_constraint_cache", fake_clear_cache)

    await update_user(
        user_id="target-user",
        body=UserUpdate(service_access_mode="allowlist", allowed_services=["agent"]),
        request=_request(db),
        auth=AuthContext(user_id="admin", roles=["admin"], is_authenticated=True),
    )

    assert db.updated is True
    assert clear_calls == 1


@pytest.mark.asyncio
async def test_non_admin_cannot_modify_user_extra_permissions():
    db = _DB()

    with pytest.raises(HTTPException) as exc:
        await update_user(
            user_id="target-user",
            body=UserUpdate(extra_permissions=["admin:*"]),
            request=_request(db),
            auth=AuthContext(user_id="editor", roles=["user:edit"], is_authenticated=True),
        )

    assert exc.value.status_code == 403
    assert db.extra_permissions_updated is False


@pytest.mark.asyncio
async def test_non_admin_cannot_modify_user_roles():
    db = _DB()

    with pytest.raises(HTTPException) as exc:
        await update_user(
            user_id="target-user",
            body=UserUpdate(display_name="Escalated", roles=["admin"]),
            request=_request(db),
            auth=AuthContext(user_id="editor", roles=["user:edit"], is_authenticated=True),
        )

    assert exc.value.status_code == 403
    assert db.updated is False
