from __future__ import annotations

from types import SimpleNamespace

import pytest
from fastapi import HTTPException

from src.api.v1.auth import LoginRequest, login


class _MissingUserDB:
    enabled = True

    def __init__(self) -> None:
        self.saved_user = False
        self.audit_events: list[dict] = []

    async def get_user_by_email(self, _email: str):
        return None

    async def get_user(self, _user_id: str):
        return None

    async def save_user_with_password(self, _user_data: dict):
        self.saved_user = True

    async def assign_user_role(self, user_id: str, role: str, created_by: str):
        pass

    async def log_login_audit(self, **kwargs):
        self.audit_events.append(kwargs)


@pytest.mark.asyncio
async def test_login_does_not_jit_create_missing_user_with_default_password() -> None:
    db = _MissingUserDB()
    request = SimpleNamespace(
        app=SimpleNamespace(state=SimpleNamespace(database=db)),
        headers={},
        client=SimpleNamespace(host="203.0.113.9"),
    )

    with pytest.raises(HTTPException) as exc_info:
        await login(
            LoginRequest(email="new.user@example.com", password="bootstrap-probe-password"),
            request,
            settings=SimpleNamespace(),
        )

    assert exc_info.value.status_code == 401
    assert db.saved_user is False
    assert db.audit_events[-1]["details"]["reason"] == "user_not_found"
