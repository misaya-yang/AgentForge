"""
任务仓库

提供异步任务的数据访问接口
"""

from __future__ import annotations

import builtins
import json
import logging
from abc import ABC, abstractmethod
from datetime import datetime
from typing import Any

from .base import BaseRepository

logger = logging.getLogger(__name__)

_TASK_RETURNING = (
    "task_id, request_id, service_id, user_id, tenant_id, status, progress, request_data::text AS request_data, "
    "result::text AS result, error, callback_url, callback_sent, priority, retry_count, "
    "max_retries, metadata::text AS metadata, created_at, updated_at, started_at, completed_at"
)


class TaskRepository(ABC):
    """任务仓库抽象基类"""

    @abstractmethod
    async def save(self, task: dict[str, Any]) -> None:
        """保存任务"""
        pass

    @abstractmethod
    async def get(self, task_id: str) -> dict[str, Any] | None:
        """获取任务"""
        pass

    @abstractmethod
    async def list(
        self,
        user_id: str | None = None,
        tenant_id: str | None = None,
        service_id: str | None = None,
        status: str | None = None,
        limit: int = 100,
        offset: int = 0,
    ) -> builtins.list[dict[str, Any]]:
        """获取任务列表"""
        pass

    @abstractmethod
    async def update_status(
        self,
        task_id: str,
        status: str,
        progress: float = None,
        result: Any = None,
        error: str = None,
    ) -> None:
        """更新任务状态"""
        pass

    @abstractmethod
    async def get_pending_tasks(self, limit: int = 10) -> builtins.list[dict[str, Any]]:
        """获取待处理任务"""
        pass

    @abstractmethod
    async def mark_callback_sent(self, task_id: str) -> None:
        """标记回调已发送"""
        pass


class DatabaseTaskRepository(TaskRepository, BaseRepository):
    """基于 PostgreSQL 的任务仓库实现"""

    def __init__(self, pool_holder: Any):
        BaseRepository.__init__(self, pool_holder)

    # ------------------------------------------------------------------
    # Row conversion helper
    # ------------------------------------------------------------------

    @staticmethod
    def _row_to_dict(row: Any) -> dict[str, Any]:
        """Decode JSON exactly once from this repository's explicit text casts."""
        result = dict(row)
        for key in ("result", "metadata", "request_data"):
            value = result.get(key)
            if isinstance(value, str):
                try:
                    result[key] = json.loads(value)
                except (json.JSONDecodeError, TypeError):
                    raise ValueError("invalid task storage JSON") from None
        for key, value in result.items():
            if isinstance(value, datetime):
                result[key] = value.isoformat()
        return result

    @staticmethod
    def _metadata_patch(result_format: dict[str, Any] | None, params: list[Any]) -> str:
        # Never merge a stale task snapshot. JSON null/non-objects are not SQL NULL.
        if result_format is None:
            return ("metadata = CASE WHEN jsonb_typeof(metadata) = 'object' "
                    "THEN metadata - 'gateway.native_task_result.v1' ELSE metadata END")
        params.append(json.dumps(result_format, allow_nan=False))
        return ("metadata = (CASE WHEN jsonb_typeof(metadata) = 'object' THEN metadata "
                "ELSE '{}'::jsonb END) || jsonb_build_object('gateway.native_task_result.v1', "
                f"${len(params)}::jsonb)")

    # ------------------------------------------------------------------
    # CRUD
    # ------------------------------------------------------------------

    async def save(self, task: dict[str, Any], *, result_format: dict[str, Any] | None = None) -> None:
        """Legacy save keeps its historical falsy-result-to-SQL-NULL behavior."""
        metadata = task.get("metadata", {})
        if isinstance(metadata, dict):
            metadata = dict(metadata)
            metadata.pop("gateway.native_task_result.v1", None)
        if result_format is not None:
            if not task.get("result"):
                raise ValueError("invalid task result format")
            metadata = metadata if isinstance(metadata, dict) else {}
            metadata["gateway.native_task_result.v1"] = result_format
        await self.execute(
            """
                INSERT INTO tasks (
                    task_id, request_id, service_id, user_id, tenant_id,
                    status, progress, request_data, result, error,
                    callback_url, callback_sent, priority, retry_count, max_retries,
                    metadata, started_at, completed_at
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                    $11, $12, $13, $14, $15, $16, $17, $18
                )
                ON CONFLICT (task_id) DO UPDATE SET
                    status = EXCLUDED.status,
                    progress = EXCLUDED.progress,
                    result = EXCLUDED.result,
                    error = EXCLUDED.error,
                    callback_sent = EXCLUDED.callback_sent,
                    retry_count = EXCLUDED.retry_count,
                    metadata = EXCLUDED.metadata,
                    started_at = EXCLUDED.started_at,
                    completed_at = EXCLUDED.completed_at,
                    updated_at = NOW()
            """,
            task.get("task_id"),
            task.get("request_id"),
            task.get("service_id"),
            task.get("user_id"),
            task.get("tenant_id"),
            task.get("status", "pending"),
            task.get("progress", 0),
            json.dumps(task.get("request_data")) if task.get("request_data") else None,
            json.dumps(task.get("result"), allow_nan=False) if task.get("result") else None,
            task.get("error"),
            task.get("callback_url"),
            task.get("callback_sent", False),
            task.get("priority", 0),
            task.get("retry_count", 0),
            task.get("max_retries", 3),
            json.dumps(metadata, allow_nan=False),
            task.get("started_at"),
            task.get("completed_at"),
        )

    async def get(self, task_id: str) -> dict[str, Any] | None:
        """获取任务"""
        row = await self.fetchrow(f"SELECT {_TASK_RETURNING} FROM tasks WHERE task_id = $1", task_id)
        return self._row_to_dict(row) if row else None

    async def list(
        self,
        user_id: str | None = None,
        tenant_id: str | None = None,
        service_id: str | None = None,
        status: str | None = None,
        limit: int = 100,
        offset: int = 0,
    ) -> builtins.list[dict[str, Any]]:
        """获取任务列表"""
        query = f"SELECT {_TASK_RETURNING} FROM tasks WHERE 1=1"
        params: builtins.list[Any] = []
        param_idx = 1

        if user_id:
            query += f" AND user_id = ${param_idx}"
            params.append(user_id)
            param_idx += 1

        if tenant_id:
            query += f" AND tenant_id = ${param_idx}"
            params.append(tenant_id)
            param_idx += 1

        if service_id:
            query += f" AND service_id = ${param_idx}"
            params.append(service_id)
            param_idx += 1

        if status:
            query += f" AND status = ${param_idx}"
            params.append(status)
            param_idx += 1

        query += f" ORDER BY created_at DESC LIMIT ${param_idx} OFFSET ${param_idx + 1}"
        params.extend([limit, offset])

        rows = await self.fetch(query, *params)
        return [self._row_to_dict(row) for row in rows]

    async def update_status(
        self,
        task_id: str,
        status: str,
        progress: float = None,
        result: Any = None,
        error: str = None,
        *, result_format: dict[str, Any] | None = None,
    ) -> None:
        """更新任务状态"""
        updates = ["status = $1", "updated_at = NOW()"]
        params: builtins.list[Any] = [status]
        param_idx = 2

        if progress is not None:
            updates.append(f"progress = ${param_idx}")
            params.append(progress)
            param_idx += 1

        if result is not None:
            updates.append(f"result = ${param_idx}")
            params.append(json.dumps(result, allow_nan=False))
            param_idx += 1
            updates.append(self._metadata_patch(result_format, params))
            param_idx = len(params) + 1

        if error is not None:
            updates.append(f"error = ${param_idx}")
            params.append(error)
            param_idx += 1

        if status == "processing":
            updates.append(f"started_at = ${param_idx}")
            params.append(datetime.utcnow())
            param_idx += 1
        elif status in ("completed", "failed", "cancelled"):
            updates.append(f"completed_at = ${param_idx}")
            params.append(datetime.utcnow())
            param_idx += 1

        params.append(task_id)
        from ..database import _build_safe_set_clause
        query = f"UPDATE tasks SET {_build_safe_set_clause(updates)} WHERE task_id = ${param_idx}"

        await self.execute(query, *params)

    async def get_authority(self, task_id: str) -> dict[str, Any] | None:
        """Read only lifecycle authority, without JSON/result decoding or caches."""
        if not self.enabled:
            raise RuntimeError("task storage unavailable")
        return await self.fetchrow(
            "SELECT task_id, request_id, service_id, user_id, tenant_id, status "
            "FROM tasks WHERE task_id = $1", task_id,
        )

    async def get_authoritative(self, task_id: str) -> dict[str, Any] | None:
        """Task-owned JSON text avoids the legacy generic dictionary coercion."""
        if not self.enabled:
            raise RuntimeError("task storage unavailable")
        row = await self.fetchrow(
            f"SELECT {_TASK_RETURNING} FROM tasks WHERE task_id = $1", task_id,
        )
        return self._row_to_dict(row) if row is not None else None

    async def compare_status(
        self, identity: dict[str, str], expected: str, status: str, *,
        result: Any = None, error: str | None = None,
        result_format: dict[str, Any] | None = None,
    ) -> dict[str, Any] | None:
        """One conditional statement; unavailable/uncertain writes never return success."""
        allowed = {"pending": {"processing", "cancelled"},
                   "processing": {"completed", "failed", "cancelled"}}
        keys = ("task_id", "request_id", "service_id", "user_id", "tenant_id")
        if status not in allowed.get(expected, set()) or not all(identity.get(key) for key in keys):
            raise ValueError("invalid task transition")
        if not self.enabled:
            raise RuntimeError("task storage unavailable")
        updates = ["status = $1", "updated_at = NOW()"]
        updates.append("started_at = NOW()" if status == "processing" else "completed_at = NOW()")
        params: list[Any] = [status]
        if result is not None:
            params.append(json.dumps(result, allow_nan=False))
            updates.append(f"result = ${len(params)}::jsonb")
            updates.append(self._metadata_patch(result_format, params))
        if error is not None:
            params.append(error)
            updates.append(f"error = ${len(params)}")
        predicates = []
        for key in keys:
            params.append(identity[key])
            predicates.append(f"{key} = ${len(params)}")
        params.append(expected)
        predicates.append(f"status = ${len(params)}")
        # Only lifecycle fields and the reserved result marker change.
        # Unrelated metadata/progress never come from the stale expected snapshot.
        row = await self.fetchrow(
            f"UPDATE tasks SET {', '.join(updates)} WHERE {' AND '.join(predicates)} "
            f"RETURNING {_TASK_RETURNING}",
            *params,
        )
        return self._row_to_dict(row) if row is not None else None

    async def mark_callback_sent(self, task_id: str) -> None:
        """标记回调已发送"""
        await self.execute(
            "UPDATE tasks SET callback_sent = TRUE, updated_at = NOW() WHERE task_id = $1",
            task_id,
        )

    async def get_pending_tasks(self, limit: int = 10) -> builtins.list[dict[str, Any]]:
        """获取待处理任务"""
        rows = await self.fetch(
            f"""
                SELECT {_TASK_RETURNING} FROM tasks
                WHERE status = 'pending'
                ORDER BY priority DESC, created_at ASC
                LIMIT $1
            """,
            limit,
        )
        return [self._row_to_dict(row) for row in rows]
