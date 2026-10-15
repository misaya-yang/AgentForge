from __future__ import annotations

from typing import Annotated

from ai_gateway_core.exceptions import GatewayError, TaskNotFoundError
from fastapi import APIRouter, Depends, HTTPException, Query

from ...core.auth.user_resolver import UserContext
from ...models.task import Task
from ...services.task.task_manager import TaskManager, TaskStateConflictError
from ..deps import get_task_manager, get_user_context
from ..schemas.response import UnifiedResponseSchema
from ..schemas.task import TaskSchema

router = APIRouter()


def _require_task_access(task: Task, user: UserContext) -> None:
    # Generic task administration is tenant scoped. Neither a tier/permission
    # nor platform_admin introduces an unaudited cross-tenant exception here.
    if (
        not user.tenant_id
        or task.tenant_id != user.tenant_id
        or (task.user_id != user.user_id and "admin" not in (user.roles or []))
    ):
        raise HTTPException(status_code=404, detail="task not found")


@router.get("/tasks", response_model=list[TaskSchema])
async def list_tasks(
    status: str | None = Query(
        None,
        pattern=r"^(pending|processing|completed|failed|cancelled)$",
    ),
    limit: Annotated[int, Query(ge=1, le=100)] = 50,
    offset: Annotated[int, Query(ge=0)] = 0,
    task_manager: TaskManager = Depends(get_task_manager),
    user: UserContext = Depends(get_user_context),
):
    tasks = await task_manager.list_tasks(
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        status=status,
        limit=limit,
        offset=offset,
    )
    return [TaskSchema.from_domain(task) for task in tasks]


@router.get("/tasks/{task_id}", response_model=TaskSchema)
async def get_task(
    task_id: str,
    task_manager: TaskManager = Depends(get_task_manager),
    user: UserContext = Depends(get_user_context),
):
    try:
        task = await task_manager.get_task(task_id)
    except TaskNotFoundError as exc:
        raise HTTPException(status_code=404, detail=str(exc))

    _require_task_access(task, user)
    return TaskSchema.from_domain(task)


@router.delete("/tasks/{task_id}")
async def cancel_task(
    task_id: str,
    task_manager: TaskManager = Depends(get_task_manager),
    user: UserContext = Depends(get_user_context),
):
    try:
        task = await task_manager.get_task(task_id)
        _require_task_access(task, user)
        await task_manager.cancel(task_id, expected=task)
    except TaskStateConflictError as exc:
        raise HTTPException(status_code=409, detail=str(exc)) from exc
    except TaskNotFoundError as exc:
        raise HTTPException(status_code=404, detail=str(exc))
    return {"task_id": task_id, "status": "cancelled"}


@router.get("/tasks/{task_id}/result", response_model=UnifiedResponseSchema)
async def get_task_result(
    task_id: str,
    task_manager: TaskManager = Depends(get_task_manager),
    user: UserContext = Depends(get_user_context),
):
    try:
        task = await task_manager.get_task(task_id)
        _require_task_access(task, user)
        resp = await task_manager.get_result(task_id)
    except GatewayError as exc:
        raise HTTPException(status_code=404, detail=str(exc))
    return UnifiedResponseSchema.from_domain(resp)
