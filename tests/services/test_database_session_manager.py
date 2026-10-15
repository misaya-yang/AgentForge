from __future__ import annotations

from unittest.mock import AsyncMock

import ai_gateway_core.session.database_manager as database_manager_module
import pytest
from ai_gateway_core.persistence.database import DatabaseStorage

from src.models.session import Session
from src.services.session.database_session_manager import DatabaseSessionManager


def _build_manager() -> tuple[DatabaseSessionManager, AsyncMock]:
    db = AsyncMock()
    db.update_session_config = AsyncMock(return_value=True)
    db.update_session_metadata = AsyncMock(return_value=True)
    db.save_session = AsyncMock()
    db.create_session_if_absent = AsyncMock(return_value=True)
    manager = DatabaseSessionManager(database=db, redis=None)
    return manager, db


@pytest.mark.asyncio
async def test_client_selected_session_id_uses_atomic_insert() -> None:
    manager, db = _build_manager()

    session = await manager.create(
        user_id="user-1",
        tenant_id="tenant-1",
        session_id="11111111-1111-4111-8111-111111111111",
        fail_if_exists=True,
    )

    assert session.session_id == "11111111-1111-4111-8111-111111111111"
    db.create_session_if_absent.assert_awaited_once()
    db.save_session.assert_not_awaited()


@pytest.mark.asyncio
async def test_client_selected_session_id_defaults_to_atomic_insert() -> None:
    manager, db = _build_manager()

    await manager.create(
        user_id="user-1",
        tenant_id="tenant-1",
        session_id="11111111-1111-4111-8111-111111111111",
    )

    db.create_session_if_absent.assert_awaited_once()
    db.save_session.assert_not_awaited()


@pytest.mark.asyncio
async def test_client_selected_session_id_conflict_does_not_cache() -> None:
    from ai_gateway_core.exceptions import SessionAlreadyExistsError

    manager, db = _build_manager()
    db.create_session_if_absent.return_value = False

    with pytest.raises(SessionAlreadyExistsError):
        await manager.create(
            user_id="user-1",
            tenant_id="tenant-1",
            session_id="11111111-1111-4111-8111-111111111111",
            fail_if_exists=True,
        )

    assert manager._memory_cache == {}


@pytest.mark.asyncio
async def test_atomic_session_insert_handles_every_unique_constraint() -> None:
    class Connection:
        query = ""

        async def fetchrow(self, query: str, *args):
            del args
            self.query = query
            return None

    class Acquire:
        def __init__(self, connection: Connection) -> None:
            self.connection = connection

        async def __aenter__(self):
            return self.connection

        async def __aexit__(self, *_args):
            return False

    class Pool:
        def __init__(self, connection: Connection) -> None:
            self.connection = connection

        def acquire(self):
            return Acquire(self.connection)

    connection = Connection()
    database = object.__new__(DatabaseStorage)
    database._pool = Pool(connection)

    created = await database.create_session_if_absent(
        {
            "session_id": "session-1",
            "service_id": "__builtin_assistant__",
            "user_id": "user-1",
            "tenant_id": "tenant-1",
        }
    )

    assert created is False
    assert "ON CONFLICT DO NOTHING" in connection.query
    assert "ON CONFLICT (session_id)" not in connection.query


def _build_session(session_id: str = "session-1") -> Session:
    return Session(
        session_id=session_id,
        user_id="user-1",
        tenant_id="tenant-1",
        metadata={"title": "old"},
        history=[],
        state={},
        config={"model": "gemini"},
    )


@pytest.mark.asyncio
async def test_update_config_uses_atomic_patch_and_invalidates_cache():
    manager, db = _build_manager()
    session = _build_session()
    manager._memory_cache[session.session_id] = session

    result = await manager.update_config(session.session_id, {"selected_datasets": ["kb_1"]})

    assert result is True
    db.update_session_config.assert_awaited_once_with(
        session.session_id, {"selected_datasets": ["kb_1"]}
    )
    db.save_session.assert_not_awaited()
    assert session.session_id not in manager._memory_cache


@pytest.mark.asyncio
async def test_update_metadata_uses_atomic_patch_and_invalidates_cache():
    manager, db = _build_manager()
    session = _build_session("session-2")
    manager._memory_cache[session.session_id] = session

    result = await manager.update_metadata(session.session_id, {"agent_id": "agent"})

    assert result is True
    db.update_session_metadata.assert_awaited_once_with(session.session_id, {"agent_id": "agent"})
    db.save_session.assert_not_awaited()
    assert session.session_id not in manager._memory_cache


@pytest.mark.asyncio
async def test_memory_cache_is_lru_bounded_and_expires(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    now = [100.0]
    monkeypatch.setattr(database_manager_module.time, "monotonic", lambda: now[0])
    db = AsyncMock()
    manager = DatabaseSessionManager(
        database=db,
        redis=None,
        cache_ttl=5,
        memory_cache_max_entries=2,
    )
    first = _build_session("session-1")
    second = _build_session("session-2")
    third = _build_session("session-3")

    await manager._cache_session(first)
    await manager._cache_session(second)
    assert await manager._get_from_cache(first.session_id) is first

    await manager._cache_session(third)

    assert second.session_id not in manager._memory_cache
    assert list(manager._memory_cache) == [first.session_id, third.session_id]

    now[0] += 6
    assert await manager._get_from_cache(first.session_id) is None
    assert first.session_id not in manager._memory_cache


@pytest.mark.asyncio
async def test_update_state_invalidates_cache_instead_of_reinserting_stale_snapshot() -> None:
    manager, db = _build_manager()
    db.update_session_state = AsyncMock(return_value=True)
    session = _build_session("session-state")
    manager._memory_cache[session.session_id] = session

    result = await manager.update_state(session.session_id, {"phase": "new"})

    assert result is True
    db.update_session_state.assert_awaited_once_with(session.session_id, {"phase": "new"})
    assert session.session_id not in manager._memory_cache
