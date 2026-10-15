from __future__ import annotations

from collections.abc import Iterable
from typing import Any

import pytest
from ai_gateway_core.image.image_state import reserve_scoped_image_task


class _AsyncContext:
    def __init__(self, value: Any) -> None:
        self.value = value

    async def __aenter__(self) -> Any:
        return self.value

    async def __aexit__(self, *_args: Any) -> None:
        return None


class _Connection:
    def __init__(self, rows: Iterable[dict[str, Any] | None]) -> None:
        self.rows = iter(rows)
        self.calls: list[tuple[str, tuple[Any, ...]]] = []

    def transaction(self) -> _AsyncContext:
        return _AsyncContext(None)

    async def fetchrow(self, sql: str, *args: Any) -> dict[str, Any] | None:
        self.calls.append((sql, args))
        return next(self.rows)


class _Pool:
    def __init__(self, connection: _Connection) -> None:
        self.connection = connection

    def acquire(self) -> _AsyncContext:
        return _AsyncContext(self.connection)


def _values() -> dict[str, Any]:
    return {
        "task_id": "imt_task",
        "tenant_id": "tenant-a",
        "user_id": "user-a",
        "owner_scope": "owner-a",
        "status": "pending",
        "prompt": "draw",
        "model_id": "image-model",
        "request_payload": {"prompt": "draw"},
        "turn_id": "itn_turn",
        "session_id": "img_session",
        "parent_artifact_id": None,
        "client_request_id": "client-a",
        "request_hash": "a" * 64,
    }


@pytest.mark.asyncio
async def test_reservation_and_task_insert_share_one_transaction() -> None:
    connection = _Connection(
        [
            {"request_hash": "a" * 64, "task_id": "imt_task"},
            {"task_id": "imt_task"},
        ]
    )

    result = await reserve_scoped_image_task(_Pool(connection), **_values())

    assert result == {"state": "reserved", "task_id": "imt_task"}
    assert len(connection.calls) == 2
    task_sql, task_args = connection.calls[1]
    assert "runtime_scope_version" in task_sql
    assert task_args[1:3] == ("tenant-a", "user-a")


@pytest.mark.asyncio
async def test_existing_idempotency_key_does_not_create_another_task() -> None:
    connection = _Connection(
        [None, {"request_hash": "a" * 64, "task_id": "imt_original"}]
    )

    result = await reserve_scoped_image_task(_Pool(connection), **_values())

    assert result == {"state": "existing", "task_id": "imt_original"}
    assert len(connection.calls) == 2
    assert all("INSERT INTO assistant.image_tasks" not in sql for sql, _ in connection.calls)


@pytest.mark.asyncio
async def test_existing_idempotency_key_rejects_different_request_hash() -> None:
    connection = _Connection(
        [None, {"request_hash": "b" * 64, "task_id": "imt_original"}]
    )

    result = await reserve_scoped_image_task(_Pool(connection), **_values())

    assert result == {"state": "conflict", "task_id": "imt_original"}
