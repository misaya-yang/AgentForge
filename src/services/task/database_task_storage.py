"""
数据库任务存储

基于 PostgreSQL 的 TaskStorage 实现，支持任务持久化
"""

from __future__ import annotations

import builtins
from datetime import datetime
from typing import TYPE_CHECKING, Any

from ai_gateway_core.exceptions import TaskNotFoundError

from ...models.task import Task, TaskAuthority, TaskStatus
from .task_manager import TaskStateConflictError
from .task_result_codec import (
    RESULT_FORMAT_ABSENT,
    RESULT_FORMAT_KEY,
    decode_task_result,
    encode_task_result,
)

if TYPE_CHECKING:
    from ...persistence.database import DatabaseStorage
    from ...persistence.redis import RedisStorage


class DatabaseTaskStorage:
    """基于数据库的任务存储"""

    def __init__(
        self,
        database: DatabaseStorage,
        redis: RedisStorage | None = None,
        cache_ttl: int = 600,  # Retained constructor compatibility; lifecycle reads bypass caches.
    ):
        self.database = database
        self.redis = redis
        self.cache_ttl = cache_ttl

        # Legacy cache helpers are retained, but never consulted by lifecycle reads.
        self._memory_cache: dict[str, Task] = {}

    def _require_authority(self) -> None:
        if not self.database.repos["tasks"].enabled:
            raise RuntimeError("task storage unavailable")

    async def save(self, task: Task) -> None:
        """Low-level admission/compatibility write; never a lifecycle transition."""
        self._require_authority()
        task_dict = self._task_to_dict(task)
        metadata = task_dict["metadata"]
        result_format = metadata.get(RESULT_FORMAT_KEY) if isinstance(metadata, dict) else None
        await self.database.save_task(task_dict, result_format=result_format)

    async def get(self, task_id: str) -> Task:
        # Task identity and status are authority, not a cacheable hint. A stale
        # read-fill can outlive invalidation, including in another process.
        self._require_authority()
        task_dict = await self.database.repos["tasks"].get_authoritative(task_id)
        if not task_dict:
            raise TaskNotFoundError(task_id)
        return self._dict_to_task(task_dict)

    async def get_authority(self, task_id: str) -> TaskAuthority:
        self._require_authority()
        row = await self.database.repos["tasks"].get_authority(task_id)
        if row is None:
            raise TaskNotFoundError(task_id)
        return TaskAuthority(
            tuple(row[key] for key in ("task_id", "request_id", "service_id", "user_id", "tenant_id")),
            TaskStatus(row["status"]),
        )

    async def compare_status(
        self, expected: Task, status: TaskStatus, *, result: Any = None,
        error: str | None = None,
    ) -> Task | None:
        if not expected.status.can_transition_to(status) or not all(expected.identity):
            raise TaskStateConflictError()
        self._require_authority()
        value, result_format = encode_task_result(result)
        row = await self.database.repos["tasks"].compare_status(
            dict(zip(("task_id", "request_id", "service_id", "user_id", "tenant_id"), expected.identity, strict=True)),
            expected.status.value, status.value, result=value, error=error,
            result_format=result_format,
        )
        return self._dict_to_task(row) if row is not None else None

    async def list(
        self,
        user_id: str | None = None,
        tenant_id: str | None = None,
        service_id: str | None = None,
        status: str | None = None,
        limit: int = 100,
        offset: int = 0,
    ) -> builtins.list[Task]:
        """获取任务列表"""
        self._require_authority()
        tasks_dict = await self.database.list_tasks(
            user_id=user_id,
            tenant_id=tenant_id,
            service_id=service_id,
            status=status,
            limit=limit,
            offset=offset,
        )
        return [self._dict_to_task(t) for t in tasks_dict]

    async def update_status(
        self,
        task_id: str,
        status: TaskStatus,
        progress: float = None,
        result: Any = None,
        error: str = None,
    ) -> None:
        """Legacy direct update; not lifecycle authority. None remains omission."""
        self._require_authority()
        value, result_format = encode_task_result(result)
        await self.database.update_task_status(
            task_id=task_id,
            status=status.value,
            progress=progress,
            result=value,
            error=error,
            result_format=result_format,
        )


    async def get_pending_tasks(self, limit: int = 10) -> builtins.list[Task]:
        """获取待处理任务"""
        self._require_authority()
        tasks_dict = await self.database.get_pending_tasks(limit)
        return [self._dict_to_task(t) for t in tasks_dict]

    async def mark_callback_sent(self, task_id: str) -> None:
        """标记回调已发送"""
        await self.database.mark_callback_sent(task_id)


    # =========================================================================
    # 私有方法
    # =========================================================================

    async def _cache_task(self, task: Task) -> None:
        """缓存任务"""
        if self.redis and self.redis.enabled:
            task_dict = self._task_to_dict(task)
            await self.redis.save(f"task:{task.task_id}", task_dict, self.cache_ttl)
        else:
            self._memory_cache[task.task_id] = task

    async def _get_from_cache(self, task_id: str) -> Task | None:
        """从缓存获取任务"""
        if self.redis and self.redis.enabled:
            cached = await self.redis.get(f"task:{task_id}")
            if cached:
                return self._dict_to_task(cached)
            return None
        else:
            return self._memory_cache.get(task_id)

    async def _remove_from_cache(self, task_id: str) -> None:
        """从缓存移除任务"""
        if self.redis and self.redis.enabled:
            await self.redis.delete(f"task:{task_id}")
        else:
            self._memory_cache.pop(task_id, None)

    def _task_to_dict(self, task: Task) -> dict[str, Any]:
        """Detached task JSON; preserve private type evidence only from the codec."""
        value, result_format = encode_task_result(task.result)
        metadata = dict(task.metadata) if isinstance(task.metadata, dict) else task.metadata
        if isinstance(metadata, dict):
            metadata.pop(RESULT_FORMAT_KEY, None)
        if result_format is not None:
            metadata = metadata if isinstance(metadata, dict) else {}
            metadata[RESULT_FORMAT_KEY] = result_format
        return {
            "task_id": task.task_id,
            "request_id": task.request_id,
            "service_id": task.service_id,
            "user_id": task.user_id,
            "tenant_id": task.tenant_id,
            "status": task.status.value if isinstance(task.status, TaskStatus) else task.status,
            "progress": getattr(task, "progress", 0),
            "request_data": getattr(task, "request_data", None),
            "result": value,
            "error": task.error,
            "callback_url": task.callback_url,
            "callback_sent": getattr(task, "callback_sent", False),
            "priority": getattr(task, "priority", 0),
            "retry_count": getattr(task, "retry_count", 0),
            "max_retries": getattr(task, "max_retries", 3),
            "metadata": metadata,
            "started_at": getattr(task, "started_at", None),
            "completed_at": getattr(task, "completed_at", None),
        }

    def _dict_to_task(self, data: dict[str, Any]) -> Task:
        """将字典转换为 Task"""
        status = TaskStatus(data["status"])

        created_at = data.get("created_at")
        if isinstance(created_at, str):
            created_at = datetime.fromisoformat(created_at.replace("Z", "+00:00"))

        updated_at = data.get("updated_at")
        if isinstance(updated_at, str):
            updated_at = datetime.fromisoformat(updated_at.replace("Z", "+00:00"))

        started_at = data.get("started_at")
        if isinstance(started_at, str):
            started_at = datetime.fromisoformat(started_at.replace("Z", "+00:00"))

        completed_at = data.get("completed_at")
        if isinstance(completed_at, str):
            completed_at = datetime.fromisoformat(completed_at.replace("Z", "+00:00"))

        # Repository JSON is already decoded; a JSON string is a string, not
        # another serialized document. Only trusted bookkeeping carries the tag.
        metadata = data.get("metadata", {})
        result_format = metadata.get(RESULT_FORMAT_KEY, RESULT_FORMAT_ABSENT) if isinstance(metadata, dict) else RESULT_FORMAT_ABSENT
        result = decode_task_result(data.get("result"), result_format)

        task = Task(
            task_id=data.get("task_id"),
            request_id=data.get("request_id"),
            service_id=data.get("service_id"),
            status=status,
            created_at=created_at or datetime.utcnow(),
            updated_at=updated_at,
            user_id=data.get("user_id"),
            tenant_id=data.get("tenant_id"),
            result=result,
            error=data.get("error"),
            callback_url=data.get("callback_url"),
        )

        # 添加额外属性
        task.progress = data.get("progress", 0)
        task.request_data = data.get("request_data")
        task.callback_sent = data.get("callback_sent", False)
        task.priority = data.get("priority", 0)
        task.retry_count = data.get("retry_count", 0)
        task.max_retries = data.get("max_retries", 3)
        task.metadata = metadata
        task.started_at = started_at
        task.completed_at = completed_at

        return task
