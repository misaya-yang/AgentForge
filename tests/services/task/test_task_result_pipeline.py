"""Controlled production pipeline and SQL-shape checks; no PostgreSQL acceptance."""
from __future__ import annotations

import json
import re
from copy import deepcopy
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from ai_gateway_core.enums import ContentType
from ai_gateway_core.persistence.database import DatabaseStorage
from fastapi import FastAPI

from src.api.schemas.response import UnifiedResponseSchema
from src.api.schemas.task import TaskSchema
from src.api.v1 import tasks
from src.core.auth.user_resolver import UserContext
from src.models.request import ContentItem, UnifiedRequest
from src.models.response import UnifiedResponse
from src.models.task import TaskStatus
from src.services.task.database_task_storage import DatabaseTaskStorage
from src.services.task.task_manager import TaskManager
from src.services.task.task_queue import MemoryTaskQueue
from src.services.task.task_result_codec import (
    RESULT_FORMAT_KEY,
    TaskResultCodecError,
    encode_task_result,
    project_task_result,
)
from src.services.task.task_worker import TaskWorker
from tests.services.task.test_database_task_transitions import fixture


def response():
    return UnifiedResponse("r", "success", [
        ContentItem(ContentType.IMAGE, b"\x00\xff", mime_type="image/png", metadata={"item": 1}),
        ContentItem(ContentType.TEXT, [0, 255]),
    ], session_id="session", task_id="id", usage={"count": 1}, metadata={"response": True},
        timestamp=datetime(2026, 10, 12, tzinfo=timezone.utc))


def with_legacy_io(pool):
    """Extend the recording seam for legacy writes/read projections, not SQL execution."""
    async def execute(sql, *params):
        pool.calls.append((sql, params))
        if "INSERT INTO tasks" in sql:
            columns = re.search(r"INSERT INTO tasks\s*\((.*?)\)\s*VALUES", sql, re.S).group(1)
            values = dict(zip([column.strip() for column in columns.split(",")], params, strict=True))
            # Save's SQL retains current identity on conflict, including created_at.
            identity = {key: pool.row[key] for key in ("task_id", "request_id", "service_id", "user_id", "tenant_id", "created_at")}
            pool.row.update(values)
            pool.row.update(identity)
            return "INSERT 0 1"
        assignments = sql.split(" SET ", 1)[1].split(" WHERE ", 1)[0]
        for assignment in re.split(r", (?=\w+ = )", assignments):
            key, value = assignment.split(" = ", 1)
            if key == "metadata":
                metadata = json.loads(pool.row["metadata"]) if isinstance(pool.row.get("metadata"), str) else deepcopy(pool.row.get("metadata"))
                if "jsonb_build_object" in value:
                    metadata = metadata if isinstance(metadata, dict) else {}
                    index = int(re.findall(r"\$(\d+)", value)[-1])
                    metadata[RESULT_FORMAT_KEY] = json.loads(params[index - 1])
                elif isinstance(metadata, dict):
                    metadata.pop(RESULT_FORMAT_KEY, None)
                pool.row[key] = json.dumps(metadata) if metadata is not None else None
            else:
                pool.row[key] = datetime(2026, 10, 12) if value == "NOW()" else params[int(value[1:].split(":")[0]) - 1]
        return "UPDATE 1"

    async def fetch(sql, *params):
        pool.calls.append((sql, params))
        assert "result::text AS result" in sql and "metadata::text AS metadata" in sql
        return [deepcopy(pool.row)]

    pool.execute = execute
    pool.fetch = fetch
    return pool


async def test_actual_worker_db_reread_callback_and_http_binary_result(monkeypatch):
    pool, database, storage = fixture()
    with_legacy_io(pool)
    pool.row["callback_url"] = "https://fixture.invalid/callback"
    queue = MemoryTaskQueue()
    manager = TaskManager(storage, queue, callback_retries=1)
    dispatcher = SimpleNamespace(invoke=AsyncMock(return_value=response()))
    worker = TaskWorker(queue, storage, dispatcher, manager)
    callbacks = []

    async def offline_callback(url, *, json):
        request = httpx.Request("POST", url, json=json)
        callbacks.append(request)
        return httpx.Response(200, request=request)

    monkeypatch.setattr("ai_gateway_core.security.safe_callback_post", offline_callback)
    request = UnifiedRequest("r", "s", [], user_id="u", tenant_id="t")
    await worker._process("id", request, [], None)
    await worker._process("id", request, [], None)
    assert dispatcher.invoke.await_count == 1 and len(callbacks) == 1
    fresh_storage = DatabaseTaskStorage(database)
    fresh_manager = TaskManager(fresh_storage, queue)
    typed = await fresh_manager.get_result("id")
    assert typed.outputs[0].data == b"\x00\xff" and typed.outputs[1].data == [0, 255]
    assert (await fresh_storage.get("id")).status == TaskStatus.COMPLETED
    expected = UnifiedResponseSchema.from_domain(response()).model_dump(mode="json")
    assert json.loads(callbacks[0].content)["result"] == expected
    assert json.loads(pool.row["result"])["outputs"][0]["data"] == [0, 255]
    assert json.loads(pool.row["metadata"])[RESULT_FORMAT_KEY] == {"output_data_types": ["bytes", "json"]}
    app = FastAPI()
    app.include_router(tasks.router, prefix="/api/v1")
    app.dependency_overrides[tasks.get_task_manager] = lambda: fresh_manager
    app.dependency_overrides[tasks.get_user_context] = lambda: UserContext(user_id="u", tenant_id="t", is_authenticated=True)
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://fixture") as client:
        detail = await client.get("/api/v1/tasks/id")
        result = await client.get("/api/v1/tasks/id/result")
        listing = await client.get("/api/v1/tasks")
        assert detail.status_code == result.status_code == listing.status_code == 200
        assert detail.json()["result"] == result.json() == listing.json()[0]["result"] == expected
        assert (await client.delete("/api/v1/tasks/id")).status_code == 409
    assert isinstance((await fresh_storage.get_pending_tasks())[0].result, UnifiedResponse)


@pytest.mark.parametrize("metadata", [None, "null", "[]", '"scalar"', '{"concurrent":true}'])
async def test_cas_typed_marker_handles_sql_null_json_null_and_nonobjects(metadata):
    pool, _, storage = fixture()
    claimed = await storage.compare_status(await storage.get("id"), TaskStatus.PROCESSING)
    pool.row["metadata"] = metadata
    done = await storage.compare_status(claimed, TaskStatus.COMPLETED, result=response())
    expected = {RESULT_FORMAT_KEY: {"output_data_types": ["bytes", "json"]}}
    if metadata == '{"concurrent":true}':
        expected["concurrent"] = True
    assert done.metadata == expected
    sql, params = pool.calls[-1]
    assert "CASE WHEN jsonb_typeof(metadata) = 'object' THEN metadata ELSE '{}'::jsonb END" in sql
    assert "jsonb_build_object('gateway.native_task_result.v1'," in sql
    assert "result = $2::jsonb" in sql and params[0] == "completed"


@pytest.mark.parametrize("replacement", [False, 0, [], {}, "null", "[1]", {"request_id": "fake", "outputs": []}])
async def test_cas_non_none_raw_replacement_clears_marker_and_preserves_metadata(replacement):
    pool, _, storage = fixture()
    stored, marker = encode_task_result(response())
    pool.row["result"] = json.dumps(stored)
    pool.row["metadata"] = json.dumps({RESULT_FORMAT_KEY: marker, "keep": [1]})
    claimed = await storage.compare_status(await storage.get("id"), TaskStatus.PROCESSING)
    assert isinstance(claimed.result, UnifiedResponse)
    pool.row["metadata"] = json.dumps({RESULT_FORMAT_KEY: marker, "keep": [1], "concurrent": 2})
    done = await storage.compare_status(claimed, TaskStatus.COMPLETED, result=replacement)
    assert type(done.result) is type(replacement) and done.result == replacement
    assert done.metadata == {"keep": [1], "concurrent": 2}
    assert "metadata - 'gateway.native_task_result.v1'" in pool.calls[-1][0]


async def test_none_omission_retains_result_marker_and_public_projection():
    pool, _, storage = fixture()
    stored, marker = encode_task_result(response())
    pool.row["result"] = json.dumps(stored)
    pool.row["metadata"] = json.dumps({RESULT_FORMAT_KEY: marker, "keep": 1})
    cancelled = await storage.compare_status(await storage.get("id"), TaskStatus.CANCELLED, result=None)
    assert project_task_result(cancelled.result) == project_task_result(response())
    assert cancelled.metadata[RESULT_FORMAT_KEY] == marker
    assert "metadata = " not in pool.calls[-1][0] and "result = " not in pool.calls[-1][0]


@pytest.mark.parametrize("value", [False, 0, [], [1], {}, "null", "[1]", "123", "", None])
async def test_task_read_boundaries_parse_json_exactly_once(value):
    pool, database, storage = fixture()
    with_legacy_io(pool)
    pool.row["result"] = json.dumps(value)
    for result in [(await storage.get("id")).result, (await storage.list())[0].result,
                   (await storage.get_pending_tasks())[0].result,
                   (await database.get_task("id"))["result"], (await database.list_tasks())[0]["result"],
                   (await database.get_pending_tasks())[0]["result"]]:
        assert type(result) is type(value) and result == value


@pytest.mark.parametrize("method", ["save", "update"])
async def test_adapter_legacy_typed_writes_and_facade_raw_writes_remove_marker(method):
    pool, database, storage = fixture()
    with_legacy_io(pool)
    task = await storage.get("id")
    task.result = response()
    if method == "save":
        await storage.save(task)
    else:
        await storage.update_status("id", TaskStatus.COMPLETED, result=task.result)
    assert isinstance((await storage.get("id")).result, UnifiedResponse)
    pool.row["metadata"] = json.dumps({RESULT_FORMAT_KEY: {"output_data_types": ["bytes", "json"]}, "keep": 1})
    await database.update_task_status("id", "completed", result={"raw": True})
    reread = await storage.get("id")
    assert reread.result == {"raw": True} and reread.metadata == {"keep": 1}


@pytest.mark.parametrize("value", [False, 0, [], {}, "", None])
async def test_legacy_save_retains_historical_falsy_to_sql_null(value):
    pool, database, storage = fixture()
    with_legacy_io(pool)
    task = await storage.get("id")
    raw = storage._task_to_dict(task)
    raw["result"] = value
    raw["metadata"] = {RESULT_FORMAT_KEY: {"output_data_types": []}, "keep": 1}
    await database.save_task(raw)
    assert pool.row["result"] is None
    assert json.loads(pool.row["metadata"]) == {"keep": 1}


async def test_legacy_none_update_preserves_existing_marker():
    pool, database, storage = fixture()
    with_legacy_io(pool)
    task = await storage.get("id")
    task.result = response()
    await storage.save(task)
    before = pool.row["result"], pool.row["metadata"]
    await database.update_task_status("id", "processing", result=None)
    assert (pool.row["result"], pool.row["metadata"]) == before


async def test_facade_disabled_absence_semantics_and_adapter_fail_closed():
    database = DatabaseStorage("postgresql://fixture.invalid/testing", enabled=False, auto_init=False)
    await database.save_task({"result": object()})
    await database.update_task_status("id", "completed", result=object())
    assert await database.get_task("id") is None
    assert await database.list_tasks() == await database.get_pending_tasks() == []
    with pytest.raises(RuntimeError, match="task storage unavailable"):
        await DatabaseTaskStorage(database).get_pending_tasks()


async def test_delegated_list_and_pending_sql_preserve_filters_order_and_limits():
    pool, database, _ = fixture()
    with_legacy_io(pool)
    await database.list_tasks("u", "t", "s", "pending", 17, 9)
    sql, params = pool.calls[-1]
    assert params == ("u", "t", "s", "pending", 17, 9)
    assert "WHERE 1=1 AND user_id = $1 AND tenant_id = $2 AND service_id = $3 AND status = $4" in sql
    assert sql.endswith("ORDER BY created_at DESC LIMIT $5 OFFSET $6")
    await database.get_pending_tasks(11)
    sql, params = pool.calls[-1]
    assert params == (11,) and "ORDER BY priority DESC, created_at ASC" in sql and "WHERE status = 'pending'" in sql


async def test_invalid_codec_completion_does_not_callback_or_reinvoke(monkeypatch):
    pool, _, storage = fixture()
    pool.row["callback_url"] = "https://fixture.invalid/callback"
    queue = MemoryTaskQueue()
    manager = TaskManager(storage, queue, callback_retries=1)
    invalid = response()
    invalid.timestamp = datetime(2026, 10, 12, tzinfo=timezone(timedelta(microseconds=1)))
    dispatcher = SimpleNamespace(invoke=AsyncMock(return_value=invalid))
    callback = AsyncMock(return_value=httpx.Response(200, request=httpx.Request("POST", "https://fixture.invalid")))
    monkeypatch.setattr("ai_gateway_core.security.safe_callback_post", callback)
    worker = TaskWorker(queue, storage, dispatcher, manager)
    request = UnifiedRequest("r", "s", [], user_id="u", tenant_id="t")
    await worker._process("id", request, [], None)
    await worker._process("id", request, [], None)
    assert dispatcher.invoke.await_count == 1
    assert pool.row["status"] == "failed" and pool.row["error"] == "task_completion_persistence_failed"
    assert callback.await_count == 1  # The acknowledged failure winner, not completion.
    assert callback.call_args.kwargs["json"]["status"] == "failed"


async def test_callback_transport_failure_leaves_typed_completion(monkeypatch):
    pool, _, storage = fixture()
    pool.row["callback_url"] = "https://fixture.invalid/callback"
    manager = TaskManager(storage, MemoryTaskQueue(), callback_retries=1)
    monkeypatch.setattr("ai_gateway_core.security.safe_callback_post", AsyncMock(side_effect=httpx.TimeoutException("fixture")))
    claimed = await manager.transition_task(await storage.get("id"), TaskStatus.PROCESSING)
    done = await manager.transition_task(claimed, TaskStatus.COMPLETED, result=response())
    assert done.status == TaskStatus.COMPLETED and isinstance((await storage.get("id")).result, UnifiedResponse)


async def test_corrupt_marker_read_fails_closed_and_redacted():
    pool, _, storage = fixture()
    pool.row["result"] = json.dumps({"private": "do-not-leak"})
    pool.row["metadata"] = json.dumps({RESULT_FORMAT_KEY: None})
    with pytest.raises(TaskResultCodecError, match="^invalid_task_result$"):
        await storage.get("id")


def test_schema_projects_typed_binary_without_private_kind_field():
    from src.models.task import Task
    task = Task("id", "r", "s", TaskStatus.COMPLETED, result=response())
    assert TaskSchema.from_domain(task).model_dump(mode="json")["result"] == UnifiedResponseSchema.from_domain(response()).model_dump(mode="json")


async def test_raw_response_shaped_save_cannot_reuse_forged_metadata_marker():
    pool, database, storage = fixture()
    with_legacy_io(pool)
    task = await storage.get("id")
    raw = storage._task_to_dict(task)
    raw["result"], marker = encode_task_result(response())
    raw["metadata"] = {RESULT_FORMAT_KEY: marker, "keep": 1}
    await database.save_task(raw)
    reread = await storage.get("id")
    assert isinstance(reread.result, dict)
    assert reread.metadata == {"keep": 1}
    assert reread.result["outputs"][0]["data"] == [0, 255]


async def test_native_kind_provenance_survives_adapter_read_then_legacy_save():
    from pathlib import Path
    fixture_path = Path(__file__).parents[2] / "unit/fixtures/task_result_native_v1.json"
    native = json.loads(fixture_path.read_text())
    pool, _, storage = fixture()
    with_legacy_io(pool)
    pool.row["result"] = json.dumps(native["value"])
    pool.row["metadata"] = json.dumps({RESULT_FORMAT_KEY: native["format"], "keep": 1})
    task = await storage.get("id")
    await storage.save(task)
    assert json.loads(pool.row["result"]) == native["value"]
    assert json.loads(pool.row["metadata"]) == {RESULT_FORMAT_KEY: native["format"], "keep": 1}


async def test_corrupt_typed_row_prevents_dispatch_and_callback():
    pool, _, storage = fixture()
    pool.row["metadata"] = json.dumps({RESULT_FORMAT_KEY: {"output_data_types": []}})
    pool.row["result"] = json.dumps({"secret": "bad"})
    manager = TaskManager(storage, MemoryTaskQueue())
    manager._send_callback = AsyncMock()
    dispatcher = SimpleNamespace(invoke=AsyncMock())
    worker = TaskWorker(manager.queue, storage, dispatcher, manager)
    with pytest.raises(TaskResultCodecError, match="^invalid_task_result$"):
        await worker._process("id", UnifiedRequest("r", "s", [], user_id="u", tenant_id="t"), [], None)
    dispatcher.invoke.assert_not_awaited()
    manager._send_callback.assert_not_awaited()
    assert pool.row["status"] == "pending"
