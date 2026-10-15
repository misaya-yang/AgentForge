"""Observed-terminal cancellation contract; no concurrency or remote-stop claim."""
from datetime import datetime
from unittest.mock import AsyncMock

import httpx
import pytest
from ai_gateway_core.enums import ContentType
from fastapi import FastAPI

from src.api.v1 import tasks
from src.core.auth.user_resolver import UserContext
from src.models.request import ContentItem, UnifiedRequest
from src.models.response import UnifiedResponse
from src.models.task import TaskStatus
from src.services.task.task_manager import MemoryTaskStorage, TaskManager
from src.services.task.task_queue import MemoryTaskQueue


def client_for(manager, user):
    app = FastAPI()
    app.include_router(tasks.router, prefix="/api/v1")
    app.dependency_overrides[tasks.get_task_manager] = lambda: manager
    app.dependency_overrides[tasks.get_user_context] = lambda: user
    return httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://fixture")


async def admitted(status):
    storage = MemoryTaskStorage()
    manager = TaskManager(storage, MemoryTaskQueue())
    task = await manager.create_task(UnifiedRequest(
        request_id="fixture-request", service_id="fixture-service", inputs=[],
        user_id="owner", tenant_id="tenant",
    ), roles=[], client_ip=None)
    task.status = status
    task.updated_at = datetime(2026, 10, 12, 10)
    if status == TaskStatus.COMPLETED:
        task.result = UnifiedResponse(request_id=task.request_id, status="success", outputs=[
            ContentItem(type=ContentType.TEXT, data="Finished output"),
        ])
    elif status == TaskStatus.FAILED:
        task.error = "fixture_failure"
    await storage.save(task)
    storage.save = AsyncMock(wraps=storage.save)
    storage.compare_status = AsyncMock(wraps=storage.compare_status)
    return manager, storage, task


@pytest.mark.parametrize("status", [TaskStatus.COMPLETED, TaskStatus.FAILED])
async def test_terminal_cancel_returns_conflict_without_rewriting_receipt(status):
    manager, storage, task = await admitted(status)
    user = UserContext(user_id="owner", tenant_id="tenant", is_authenticated=True)
    async with client_for(manager, user) as client:
        path = f"/api/v1/tasks/{task.task_id}"
        before = await client.get(path)
        cancelled = await client.delete(path)
        after = await client.get(path)
        assert cancelled.status_code == 409
        assert cancelled.json() == {"detail": "task_state_conflict"}
        assert after.status_code == before.status_code == 200
        assert after.json() == before.json()
        storage.save.assert_not_awaited()
        if status == TaskStatus.COMPLETED:
            result = await client.get(path + "/result")
            assert result.status_code == 200
            assert result.json()["outputs"][0]["data"] == "Finished output"


@pytest.mark.parametrize("status", [TaskStatus.PENDING, TaskStatus.PROCESSING, TaskStatus.CANCELLED])
async def test_active_cancel_and_repeated_cancel_keep_success_contract(status):
    manager, storage, task = await admitted(status)
    user = UserContext(user_id="owner", tenant_id="tenant", is_authenticated=True)
    original_updated = task.updated_at
    async with client_for(manager, user) as client:
        path = f"/api/v1/tasks/{task.task_id}"
        response = await client.delete(path)
        assert response.status_code == 200
        assert response.json() == {"task_id": task.task_id, "status": "cancelled"}
        snapshot = (await client.get(path)).json()
        assert snapshot["status"] == "cancelled"
        repeat = await client.delete(path)
        assert repeat.status_code == 200
        assert (await client.get(path)).json() == snapshot
        storage.save.assert_not_awaited()
        assert storage.compare_status.await_count == (0 if status == TaskStatus.CANCELLED else 1)
        if status == TaskStatus.CANCELLED:
            assert task.updated_at == original_updated


@pytest.mark.parametrize("user_id,tenant_id,roles,expected", [
    ("other", "tenant", [], 404),
    ("owner", "other-tenant", ["admin"], 404),
    ("owner", "", ["admin"], 404),
    ("other", "tenant", ["platform_admin"], 404),
    ("other", "tenant", ["admin"], 409),
])
async def test_terminal_conflict_is_only_visible_after_existing_authorization(user_id, tenant_id, roles, expected):
    manager, storage, task = await admitted(TaskStatus.COMPLETED)
    manager.cancel = AsyncMock(wraps=manager.cancel)
    user = UserContext(user_id=user_id, tenant_id=tenant_id, roles=roles, is_authenticated=True)
    async with client_for(manager, user) as client:
        response = await client.delete(f"/api/v1/tasks/{task.task_id}")
        assert response.status_code == expected
        assert manager.cancel.await_count == (1 if expected == 409 else 0)
        assert task.status == TaskStatus.COMPLETED
        storage.save.assert_not_awaited()


async def test_http_cancel_race_keeps_completed_receipt_and_typed_result():
    import asyncio
    manager, storage, task = await admitted(TaskStatus.PROCESSING)
    entered, release = asyncio.Event(), asyncio.Event()
    compare = storage.compare_status
    async def hold_cancel(expected, status, **kwargs):
        if status == TaskStatus.CANCELLED:
            entered.set()
            await release.wait()
        return await compare(expected, status, **kwargs)
    storage.compare_status = hold_cancel
    response = UnifiedResponse(request_id=task.request_id, status="success", outputs=[
        ContentItem(type=ContentType.TEXT, data="Race winner"),
    ])
    user = UserContext(user_id="owner", tenant_id="tenant", is_authenticated=True)
    async with client_for(manager, user) as client:
        path = f"/api/v1/tasks/{task.task_id}"
        cancelling = asyncio.create_task(client.delete(path))
        await entered.wait()
        await manager.transition_task(await storage.get(task.task_id), TaskStatus.COMPLETED, result=response)
        release.set()
        receipt = await cancelling
        assert receipt.status_code == 409
        assert receipt.json() == {"detail": "task_state_conflict"}
        assert (await client.get(path)).json()["status"] == "completed"
        assert (await client.get(path + "/result")).json()["outputs"][0]["data"] == "Race winner"
