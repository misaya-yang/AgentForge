from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException
from knowledge_service.api.routes import knowledge as knowledge_routes
from knowledge_service.api.routes.knowledge import require_admin_user
from knowledge_service.api.schemas.knowledge import RetrieveRequestSchema
from knowledge_service.auth.user_context import UserContext
from knowledge_service.persistence import database as database_module


class _CaptureConn:
    def __init__(self) -> None:
        self.calls: list[tuple[str, tuple]] = []

    async def execute(self, query: str, *params):
        self.calls.append((query, params))
        return "UPDATE 1"


class _Acquire:
    def __init__(self, conn: _CaptureConn) -> None:
        self.conn = conn

    async def __aenter__(self):
        return self.conn

    async def __aexit__(self, exc_type, exc, tb):
        return False


class _Pool:
    def __init__(self, conn: _CaptureConn) -> None:
        self.conn = conn

    def acquire(self):
        return _Acquire(self.conn)


def _storage(conn: _CaptureConn):
    storage = database_module.DatabaseStorage()
    storage._pool = _Pool(conn)
    return storage


@pytest.mark.asyncio
async def test_lock_user_account_parameterizes_minutes() -> None:
    conn = _CaptureConn()
    storage = _storage(conn)

    await storage.lock_user_account("u1", minutes=30)

    query, params = conn.calls[-1]
    assert "INTERVAL '$" not in query
    assert "30 minutes" not in query
    assert params == ("u1", 30)


@pytest.mark.asyncio
async def test_lock_user_account_rejects_invalid_minutes() -> None:
    conn = _CaptureConn()
    storage = _storage(conn)

    with pytest.raises(ValueError):
        await storage.lock_user_account("u1", minutes="30; DROP TABLE users")  # type: ignore[arg-type]

    assert conn.calls == []


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("method_name", "record_id", "updates"),
    [
        ("update_confluence_connection", "conn1", {"status": "active"}),
        ("update_confluence_binding", "bind1", {"space_name": "Docs"}),
        ("update_confluence_sync_task", "task1", {"status": "completed"}),
        ("update_user", "user1", {"display_name": "Alice"}),
    ],
)
async def test_dynamic_updates_use_safe_set_clause(
    monkeypatch,
    method_name: str,
    record_id: str,
    updates: dict,
) -> None:
    conn = _CaptureConn()
    storage = _storage(conn)
    called = False

    def fake_safe_set_clause(parts: list[str]) -> str:
        nonlocal called
        called = True
        return ", ".join(parts)

    monkeypatch.setattr(database_module, "_build_safe_set_clause", fake_safe_set_clause)

    await getattr(storage, method_name)(record_id, updates)

    assert called is True


@pytest.mark.asyncio
async def test_require_admin_user_rejects_non_admin() -> None:
    user = UserContext(user_id="u1", tenant_id="t1", roles=["user"], user_tier="normal")

    with pytest.raises(HTTPException) as exc_info:
        await require_admin_user(user)

    assert exc_info.value.status_code == 403


@pytest.mark.asyncio
async def test_require_admin_user_allows_admin_role() -> None:
    user = UserContext(user_id="u1", tenant_id="t1", roles=["admin"], user_tier="normal")

    assert await require_admin_user(user) is user


@pytest.mark.asyncio
async def test_hit_test_fails_closed_on_unexpected_retrieval_error() -> None:
    user = UserContext(user_id="u1", tenant_id="tenant-a")
    svc = SimpleNamespace(
        retrieve=AsyncMock(side_effect=RuntimeError("vector backend unavailable")),
        settings=SimpleNamespace(log_level="DEBUG"),
    )

    with pytest.raises(HTTPException) as exc_info:
        await knowledge_routes.hit_test(
            "dataset-a",
            RetrieveRequestSchema(query="q"),
            svc=svc,
            user=user,
        )

    assert exc_info.value.status_code == 500
    assert "vector backend unavailable" not in str(exc_info.value.detail)
