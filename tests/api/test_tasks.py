from __future__ import annotations

from datetime import datetime, timedelta
from unittest.mock import AsyncMock

import pytest

from src.api.v1 import tasks as tasks_api
from src.core.auth.user_resolver import UserContext
from src.models.task import Task, TaskStatus
from src.services.task.task_manager import MemoryTaskStorage


def _task(
    task_id: str,
    *,
    user_id: str,
    tenant_id: str,
    status: TaskStatus = TaskStatus.PENDING,
    created_at: datetime | None = None,
) -> Task:
    return Task(
        task_id=task_id,
        request_id=f"request-{task_id}",
        service_id="service-1",
        status=status,
        created_at=created_at or datetime.utcnow(),
        user_id=user_id,
        tenant_id=tenant_id,
    )


@pytest.mark.asyncio
async def test_list_tasks_route_is_scoped_to_current_tenant_and_user() -> None:
    manager = AsyncMock()
    manager.list_tasks.return_value = [
        _task("task-1", user_id="user-1", tenant_id="tenant-1", status=TaskStatus.PROCESSING)
    ]
    user = UserContext(user_id="user-1", tenant_id="tenant-1", is_authenticated=True)

    result = await tasks_api.list_tasks(
        status="processing",
        limit=25,
        offset=0,
        task_manager=manager,
        user=user,
    )

    manager.list_tasks.assert_awaited_once_with(
        user_id="user-1",
        tenant_id="tenant-1",
        status="processing",
        limit=25,
        offset=0,
    )
    assert [item.task_id for item in result] == ["task-1"]
    assert result[0].status is TaskStatus.PROCESSING


@pytest.mark.asyncio
async def test_memory_task_inbox_filters_and_orders_without_cross_tenant_leakage() -> None:
    storage = MemoryTaskStorage()
    now = datetime.utcnow()
    await storage.save(_task("older", user_id="user-1", tenant_id="tenant-1", created_at=now - timedelta(minutes=1)))
    await storage.save(_task("newer", user_id="user-1", tenant_id="tenant-1", created_at=now))
    await storage.save(_task("other-user", user_id="user-2", tenant_id="tenant-1"))
    await storage.save(_task("other-tenant", user_id="user-1", tenant_id="tenant-2"))

    tasks = await storage.list(user_id="user-1", tenant_id="tenant-1", limit=10)

    assert [task.task_id for task in tasks] == ["newer", "older"]
