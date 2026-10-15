"""Deleting a session must not fail when the Runtime audit chain retains it.

``assistant_runtime_threads`` (and, behind it, items / runs / capability
executions / model leases) reference ``assistant.sessions`` with RESTRICT, and
the Runtime's session cleanup only tombstones the thread. A hard delete of a
session that ever ran a turn therefore raises ForeignKeyViolationError, which
surfaced to the user as a 500 from "delete conversation".
"""

from unittest.mock import AsyncMock, MagicMock

import asyncpg
import pytest
from ai_gateway_core.persistence.database import DatabaseStorage


def _database_with_conn(conn: AsyncMock) -> DatabaseStorage:
    database = DatabaseStorage.__new__(DatabaseStorage)
    pool = MagicMock()
    acquired = MagicMock()
    acquired.__aenter__ = AsyncMock(return_value=conn)
    acquired.__aexit__ = AsyncMock(return_value=False)
    pool.acquire = MagicMock(return_value=acquired)
    database._pool = pool
    return database


@pytest.mark.asyncio
async def test_delete_session_hard_deletes_when_nothing_references_it() -> None:
    conn = AsyncMock()
    conn.execute.return_value = "DELETE 1"
    database = _database_with_conn(conn)

    assert await database.delete_session("session-1") is True
    assert conn.execute.await_count == 1
    assert "DELETE FROM assistant.sessions" in conn.execute.await_args.args[0]


@pytest.mark.asyncio
async def test_delete_session_tombstones_when_the_audit_chain_restricts_it() -> None:
    conn = AsyncMock()
    conn.execute.side_effect = [
        asyncpg.exceptions.ForeignKeyViolationError("restricted"),
        "UPDATE 1",
    ]
    database = _database_with_conn(conn)

    assert await database.delete_session("session-1") is True
    assert conn.execute.await_count == 2
    fallback = conn.execute.await_args.args[0]
    assert "SET status = 'deleted'" in fallback


@pytest.mark.asyncio
async def test_get_session_hides_a_tombstoned_row() -> None:
    conn = AsyncMock()
    conn.fetchrow.return_value = None
    database = _database_with_conn(conn)

    assert await database.get_session("session-1") is None
    assert "status <> 'deleted'" in conn.fetchrow.await_args.args[0]
