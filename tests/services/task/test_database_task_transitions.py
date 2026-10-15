"""Actual repository/adapter against a recording SQL interpreter, not live PostgreSQL."""
from __future__ import annotations

import asyncio
import json
import re
from copy import deepcopy
from datetime import datetime
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from ai_gateway_core.persistence.database import DatabaseStorage

from src.models.task import Task, TaskStatus
from src.services.task.database_task_storage import DatabaseTaskStorage
from src.services.task.task_manager import TaskManager, TaskStateConflictError
from src.services.task.task_queue import MemoryTaskQueue


class RecordingPool:
    def __init__(self, row):
        self.row = row
        self.calls = []
        self.acquires = 0
        self.lock = asyncio.Lock()

    def acquire(self):
        self.acquires += 1
        return self

    async def __aenter__(self):
        return self

    async def __aexit__(self, *args):
        return None

    async def fetchrow(self, sql, *params):
        self.calls.append((sql, params))
        async with self.lock:
            if sql.startswith("SELECT"):
                return deepcopy(self.row) if self.row["task_id"] == params[0] else None
            assert sql.startswith("UPDATE tasks SET ")
            assignments, where = sql.split(" SET ", 1)[1].split(" WHERE ", 1)
            predicates = re.findall(r"(\w+) = \$(\d+)", where.split(" RETURNING ", 1)[0])
            assert {key for key, _ in predicates} == {
                "task_id", "request_id", "service_id", "user_id", "tenant_id", "status",
            }
            if any(self.row[key] != params[int(index) - 1] for key, index in predicates):
                return None
            now = datetime.utcnow()
            for assignment in re.split(r", (?=\w+ = )", assignments):
                key, value = assignment.split(" = ", 1)
                if key == "metadata":
                    metadata = json.loads(self.row["metadata"]) if isinstance(self.row.get("metadata"), str) else deepcopy(self.row.get("metadata"))
                    if "jsonb_build_object" in value:
                        metadata = metadata if isinstance(metadata, dict) else {}
                        index = int(re.findall(r"\$(\d+)", value)[-1])
                        metadata["gateway.native_task_result.v1"] = json.loads(params[index - 1])
                    elif isinstance(metadata, dict):
                        metadata.pop("gateway.native_task_result.v1", None)
                    self.row[key] = json.dumps(metadata) if metadata is not None else None
                else:
                    self.row[key] = now if value == "NOW()" else params[int(value[1:].split(":")[0]) - 1]
            return deepcopy(self.row)


def fixture():
    row = {
        "task_id": "id", "request_id": "r", "service_id": "s", "user_id": "u", "tenant_id": "t",
        "status": "pending", "created_at": datetime(2026, 10, 12), "updated_at": None,
        "result": None, "metadata": '{"nested":[1]}', "error": None, "progress": 0,
    }
    pool = RecordingPool(row)
    database = DatabaseStorage("postgresql://fixture.invalid/testing", enabled=True, auto_init=False)
    database.enabled = True
    database._pool = pool
    redis = SimpleNamespace(enabled=True, get=AsyncMock(side_effect=AssertionError("cache read")),
                            save=AsyncMock(side_effect=AssertionError("cache write")),
                            delete=AsyncMock(side_effect=AssertionError("cache delete")))
    adapter = DatabaseTaskStorage(database, redis)
    return pool, database, adapter


async def test_wired_repository_authority_and_ignored_cache_failures():
    pool, database, adapter = fixture()
    adapter._memory_cache["id"] = Task(task_id="id", request_id="wrong", service_id="s", status=TaskStatus.COMPLETED)
    database.get_task = AsyncMock(side_effect=AssertionError("generic decoder must not run"))
    task = await adapter.get("id")
    assert task.status == TaskStatus.PENDING
    assert task.metadata == {"nested": [1]}
    claimed = await adapter.compare_status(task, TaskStatus.PROCESSING)
    assert claimed.status == TaskStatus.PROCESSING
    assert await adapter.compare_status(task, TaskStatus.PROCESSING) is None
    assert pool.acquires == 3
    assert len(pool.calls) == 3
    assert "result::text AS result" in pool.calls[0][0]
    assert "RETURNING task_id, request_id" in pool.calls[1][0]
    assert "result::text AS result" in pool.calls[1][0]
    assert claimed.started_at == claimed.updated_at
    adapter.redis.get.assert_not_awaited()
    adapter.redis.save.assert_not_awaited()
    adapter.redis.delete.assert_not_awaited()


@pytest.mark.parametrize("field", ["task_id", "request_id", "service_id", "user_id", "tenant_id"])
async def test_real_repository_predicate_rejects_each_identity_mismatch(field):
    pool, _, adapter = fixture()
    task = await adapter.get("id")
    setattr(task, field, "wrong")
    assert await adapter.compare_status(task, TaskStatus.PROCESSING) is None
    assert pool.row["status"] == "pending"


@pytest.mark.parametrize("value", [[], [1, {"nested": False}], "123", "null", "[1]", "", 0, False, {}])
async def test_json_values_roundtrip_without_dictionary_or_string_coercion(value):
    pool, _, adapter = fixture()
    pending = await adapter.get("id")
    claimed = await adapter.compare_status(pending, TaskStatus.PROCESSING)
    pool.row["metadata"] = '{"nested":[1],"concurrent":true}'
    pool.row["progress"] = 37
    pool.row["callback_sent"] = True
    done = await adapter.compare_status(claimed, TaskStatus.COMPLETED, result=value)
    reread = await adapter.get("id")
    assert done.result == reread.result == value
    assert type(done.result) is type(value)
    assert done.metadata == {"nested": [1], "concurrent": True}
    assert done.progress == 37 and done.callback_sent
    assert done.started_at == claimed.started_at
    assert done.completed_at == done.updated_at


@pytest.mark.parametrize("cancel_first", [True, False])
async def test_two_adapters_terminal_conflict_preserves_winner(cancel_first):
    pool, database, first = fixture()
    second = DatabaseTaskStorage(database)
    claimed = await first.compare_status(await first.get("id"), TaskStatus.PROCESSING)
    before = await second.get("id")
    winner = TaskStatus.CANCELLED if cancel_first else TaskStatus.COMPLETED
    loser = TaskStatus.COMPLETED if cancel_first else TaskStatus.CANCELLED
    assert await first.compare_status(claimed, winner) is not None
    assert await second.compare_status(before, loser) is None
    assert pool.row["status"] == winner.value


async def test_unavailable_authority_is_not_a_conflict_or_successful_save():
    _, database, adapter = fixture()
    task = await adapter.get("id")
    database._pool = None
    for call in [lambda: adapter.get("id"), lambda: adapter.compare_status(task, TaskStatus.PROCESSING),
                 lambda: adapter.save(task), lambda: adapter.list(),
                 lambda: database.repos["tasks"].compare_status(
                     dict(zip(("task_id", "request_id", "service_id", "user_id", "tenant_id"), task.identity, strict=True)),
                     "pending", "processing")]:
        with pytest.raises(RuntimeError, match="task storage unavailable"):
            await call()


@pytest.mark.parametrize("status", [None, "unknown", "", 1])
async def test_invalid_status_cannot_be_claimed(status):
    pool, _, adapter = fixture()
    pool.row["status"] = status
    with pytest.raises(ValueError):
        await adapter.get("id")
    assert len(pool.calls) == 1


async def test_missing_status_cannot_become_pending():
    pool, _, adapter = fixture()
    del pool.row["status"]
    with pytest.raises(KeyError):
        await adapter.get("id")


async def test_omitted_result_preserves_existing_json_and_metadata():
    pool, _, adapter = fixture()
    pool.row["result"] = json.dumps({"preexisting": 1})
    task = await adapter.get("id")
    result = await adapter.compare_status(task, TaskStatus.CANCELLED)
    assert result.result == {"preexisting": 1}
    assert result.metadata == {"nested": [1]}


async def test_storage_and_repository_reject_invalid_edges_and_empty_identity():
    pool, database, adapter = fixture()
    task = await adapter.get("id")
    with pytest.raises(TaskStateConflictError):
        await adapter.compare_status(task, TaskStatus.COMPLETED)
    identity = dict(zip(("task_id", "request_id", "service_id", "user_id", "tenant_id"), task.identity, strict=True))
    with pytest.raises(ValueError):
        await database.repos["tasks"].compare_status(identity, "completed", "processing")
    identity["tenant_id"] = ""
    with pytest.raises(ValueError):
        await database.repos["tasks"].compare_status(identity, "pending", "processing")
    assert len(pool.calls) == 1


async def test_database_typed_codec_roundtrip_through_actual_repository():
    from src.models.response import UnifiedResponse
    pool, _, adapter = fixture()
    task = await adapter.compare_status(await adapter.get("id"), TaskStatus.PROCESSING)
    done = await adapter.compare_status(task, TaskStatus.COMPLETED,
                                        result=UnifiedResponse(request_id="r", status="success", outputs=[]))
    assert isinstance(done.result, UnifiedResponse)
    assert isinstance((await adapter.get("id")).result, UnifiedResponse)
    assert pool.row["status"] == "completed"


async def test_cache_disabled_adapter_manager_never_reports_stale_cancel_success():
    pool, database, adapter = fixture()
    manager = TaskManager(adapter, MemoryTaskQueue())
    pending = await adapter.get("id")
    await adapter.compare_status(pending, TaskStatus.PROCESSING)
    completed = await adapter.compare_status(await adapter.get("id"), TaskStatus.COMPLETED, result={"kept": 1})
    adapter._memory_cache["id"] = pending
    with pytest.raises(TaskStateConflictError):
        await manager.cancel("id", expected=pending)
    assert await adapter.get("id") == completed
