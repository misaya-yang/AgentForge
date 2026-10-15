"""Actual task routes; identity/storage doubles isolate resource authorization."""
from __future__ import annotations

from unittest.mock import AsyncMock

import httpx
import pytest
from ai_gateway_core.enums import ContentType
from ai_gateway_core.exceptions import TaskNotFoundError
from fastapi import FastAPI

from src.api.deps import get_task_manager, get_user_context
from src.api.v1 import tasks as tasks_api
from src.core.auth.user_resolver import UserContext
from src.models.request import ContentItem
from src.models.response import UnifiedResponse
from src.models.task import Task, TaskStatus

ROUTES = [("GET", "/task-1"), ("GET", "/task-1/result"), ("DELETE", "/task-1")]


def _fixture(user: UserContext, task_user: str = "other", task_tenant: str | None = "tenant-a"):
    manager = AsyncMock()
    manager.get_task.return_value = Task(
        task_id="task-1", request_id="request-1", service_id="service-1",
        status=TaskStatus.COMPLETED, user_id=task_user, tenant_id=task_tenant,
        result={"secret_result": "synthetic-private-result"},
    )
    manager.get_result.return_value = UnifiedResponse(
        request_id="request-1", status="completed",
        outputs=[ContentItem(type=ContentType.TEXT, data="synthetic-private-result")],
    )
    manager.list_tasks.return_value = []
    app = FastAPI()
    app.include_router(tasks_api.router, prefix="/api/v1")
    app.dependency_overrides[get_task_manager] = lambda: manager
    app.dependency_overrides[get_user_context] = lambda: user
    return app, manager


@pytest.mark.parametrize("method,path", ROUTES)
@pytest.mark.parametrize(
    "roles,actor,actor_tenant,task_user,task_tenant",
    [
        (["admin"], "actor", "tenant-a", "other", "tenant-b"),
        (["admin"], "actor", "tenant-a", "actor", "tenant-b"),
        (["admin", "platform_admin"], "actor", "tenant-a", "other", "tenant-b"),
        (["platform_admin"], "actor", "tenant-a", "other", "tenant-b"),
        (["platform_admin"], "actor", "tenant-a", "other", "tenant-a"),
        (["Admin"], "actor", "tenant-a", "other", "tenant-a"),
        (["admin "], "actor", "tenant-a", "other", "tenant-a"),
        ([], "actor", "tenant-a", "other", "tenant-a"),
        ([], "actor", "tenant-a", "actor", "tenant-b"),
        (["admin"], "actor", "tenant-a", "actor", ""),
        (["admin"], "actor", "tenant-a", "actor", None),
        (["admin"], "actor", "", "actor", ""),
        (["admin"], "actor", "", "actor", None),
    ],
)
async def test_task_routes_conceal_denied_resources_before_result_or_cancel(
    method, path, roles, actor, actor_tenant, task_user, task_tenant,
) -> None:
    user = UserContext(
        user_id=actor, tenant_id=actor_tenant, roles=roles,
        tier="admin", is_authenticated=True,
    )
    app, manager = _fixture(user, task_user, task_tenant)
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://test") as client:
        response = await client.request(method, "/api/v1/tasks" + path)
    assert response.status_code == 404
    assert response.json() == {"detail": "task not found"}
    assert "synthetic-private-result" not in response.text
    manager.get_task.assert_awaited_once_with("task-1")
    manager.get_result.assert_not_awaited()
    manager.cancel.assert_not_awaited()


@pytest.mark.parametrize("method,path", ROUTES)
@pytest.mark.parametrize(
    "roles,task_user",
    [([], "actor"), (["admin"], "actor"), (["admin"], "other"),
     (["admin", "platform_admin"], "other"), (["platform_admin"], "actor")],
)
async def test_task_routes_preserve_owner_and_same_tenant_admin(method, path, roles, task_user) -> None:
    user = UserContext(user_id="actor", tenant_id="tenant-a", roles=roles, is_authenticated=True)
    app, manager = _fixture(user, task_user)
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://test") as client:
        response = await client.request(method, "/api/v1/tasks" + path)
    assert response.status_code == 200
    manager.get_task.assert_awaited_once_with("task-1")
    if method == "DELETE":
        assert response.json() == {"task_id": "task-1", "status": "cancelled"}
        manager.cancel.assert_awaited_once_with("task-1", expected=manager.get_task.return_value)
        manager.get_result.assert_not_awaited()
    elif path.endswith("/result"):
        assert response.json()["outputs"][0]["data"] == "synthetic-private-result"
        manager.get_result.assert_awaited_once_with("task-1")
        manager.cancel.assert_not_awaited()
    else:
        assert response.json()["task_id"] == "task-1"
        manager.get_result.assert_not_awaited()
        manager.cancel.assert_not_awaited()


@pytest.mark.parametrize("roles", [[], ["admin"], ["platform_admin"], ["admin", "platform_admin"]])
async def test_task_list_stays_owner_and_tenant_scoped_for_every_role(roles) -> None:
    user = UserContext(user_id="actor", tenant_id="tenant-a", roles=roles, is_authenticated=True)
    app, manager = _fixture(user)
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://test") as client:
        response = await client.get("/api/v1/tasks?limit=7&offset=3&status=completed")
    assert response.status_code == 200
    manager.list_tasks.assert_awaited_once_with(
        user_id="actor", tenant_id="tenant-a", status="completed", limit=7, offset=3,
    )


@pytest.mark.parametrize("method,path", ROUTES)
async def test_task_routes_keep_missing_resource_404_without_side_effects(method, path) -> None:
    user = UserContext(user_id="actor", tenant_id="tenant-a", roles=["admin"], is_authenticated=True)
    app, manager = _fixture(user)
    manager.get_task.side_effect = TaskNotFoundError("task-1")
    async with httpx.AsyncClient(transport=httpx.ASGITransport(app=app), base_url="http://test") as client:
        response = await client.request(method, "/api/v1/tasks" + path)
    assert response.status_code == 404
    manager.get_result.assert_not_awaited()
    manager.cancel.assert_not_awaited()
