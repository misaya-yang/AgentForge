from __future__ import annotations

from typing import Any

import pytest

from src.persistence.database import DatabaseStorage


class _Transaction:
    async def __aenter__(self) -> None:
        return None

    async def __aexit__(self, *_args: Any) -> None:
        return None


class _Connection:
    def __init__(self, *, exists: int | None = 1) -> None:
        self.exists = exists
        self.calls: list[tuple[str, tuple[Any, ...]]] = []

    def transaction(self) -> _Transaction:
        return _Transaction()

    async def fetchval(self, query: str, *args: Any) -> int | None:
        self.calls.append((query, args))
        return self.exists

    async def execute(self, query: str, *args: Any) -> str:
        self.calls.append((query, args))
        if query.lstrip().startswith("DELETE FROM users"):
            return "DELETE 1"
        return "UPDATE 1"


class _Acquire:
    def __init__(self, connection: _Connection) -> None:
        self.connection = connection

    async def __aenter__(self) -> _Connection:
        return self.connection

    async def __aexit__(self, *_args: Any) -> None:
        return None


class _Pool:
    def __init__(self, connection: _Connection) -> None:
        self.connection = connection

    def acquire(self) -> _Acquire:
        return _Acquire(self.connection)


def _database(connection: _Connection) -> DatabaseStorage:
    database = DatabaseStorage(dsn="", enabled=False)
    database._pool = _Pool(connection)
    return database


@pytest.mark.asyncio
async def test_tenant_scoped_update_locks_and_writes_the_same_tenant() -> None:
    connection = _Connection()
    database = _database(connection)

    updated = await database.update_user_for_tenant(
        "member-b",
        "tenant-a",
        {"display_name": "Scoped update"},
    )

    assert updated is True
    lock_query, lock_args = connection.calls[0]
    assert "user_id = $1 AND tenant_id = $2 FOR UPDATE" in lock_query
    assert lock_args == ("member-b", "tenant-a")
    update_query, update_args = connection.calls[1]
    assert "WHERE user_id = $2 AND tenant_id = $3" in update_query
    assert update_args == ("Scoped update", "member-b", "tenant-a")


@pytest.mark.asyncio
async def test_tenant_scoped_delete_does_not_touch_grants_when_target_is_elsewhere() -> None:
    connection = _Connection(exists=None)
    database = _database(connection)

    deleted = await database.delete_user_for_tenant("member-b", "tenant-a")

    assert deleted is False
    assert len(connection.calls) == 1
    query, args = connection.calls[0]
    assert "user_id = $1 AND tenant_id = $2 FOR UPDATE" in query
    assert args == ("member-b", "tenant-a")


@pytest.mark.asyncio
async def test_tenant_scoped_password_reset_uses_tenant_in_write_predicate() -> None:
    connection = _Connection()
    database = _database(connection)

    reset = await database.reset_user_password_for_tenant(
        "member-b",
        "tenant-a",
        "hash",
    )

    assert reset is True
    query, args = connection.calls[0]
    assert "WHERE user_id = $2 AND tenant_id = $3" in query
    assert args == ("hash", "member-b", "tenant-a")
