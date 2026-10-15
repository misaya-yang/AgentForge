from __future__ import annotations

import asyncio
import builtins
import logging
import uuid
from copy import deepcopy
from datetime import datetime
from typing import Any

import httpx
from ai_gateway_core.exceptions import GatewayError, TaskNotFoundError
from asyncpg.exceptions import (
    CannotConnectNowError,
    ConnectionDoesNotExistError,
    ConnectionFailureError,
    TooManyConnectionsError,
)

from ...models.request import UnifiedRequest
from ...models.response import UnifiedResponse
from ...models.task import Task, TaskAuthority, TaskStatus
from .task_queue import TaskQueue
from .task_result_codec import project_task_result

logger = logging.getLogger(__name__)


class TaskClaimReadUnavailable(RuntimeError):
    """A transient read failed before any claim write or dispatch was attempted."""


class TaskClaimReadCancelled(asyncio.CancelledError):
    """Cancellation interrupted only the read before the claim write."""


class TaskStateConflictError(GatewayError):
    """The expected task identity/status cannot make this transition."""

    def __init__(self) -> None:
        super().__init__("task_state_conflict")


class TaskStorage:
    async def save(self, task: Task) -> None:
        raise NotImplementedError

    async def get(self, task_id: str) -> Task:
        raise NotImplementedError

    async def get_authority(self, task_id: str) -> TaskAuthority:
        # Compatibility for existing subclasses; built-in stores avoid payload reads.
        task = await self.get(task_id)
        return TaskAuthority(task.identity, TaskStatus(task.status))

    async def compare_status(
        self, expected: Task, status: TaskStatus, *, result: Any = None,
        error: str | None = None,
    ) -> Task | None:
        """Return the acknowledged winner; None is a definitive conflict only."""
        raise NotImplementedError

    async def list(
        self,
        *,
        user_id: str | None = None,
        tenant_id: str | None = None,
        service_id: str | None = None,
        status: str | None = None,
        limit: int = 100,
        offset: int = 0,
    ) -> builtins.list[Task]:
        raise NotImplementedError


class MemoryTaskStorage(TaskStorage):
    def __init__(self):
        self._tasks: dict[str, Task] = {}
        self._lock = asyncio.Lock()

    async def save(self, task: Task) -> None:
        # Compatibility operation for admission/setup, never worker transitions.
        async with self._lock:
            self._tasks[task.task_id] = deepcopy(task)

    async def get(self, task_id: str) -> Task:
        async with self._lock:
            task = self._tasks.get(task_id)
            if task is None:
                raise TaskNotFoundError(task_id)
            return deepcopy(task)

    async def get_authority(self, task_id: str) -> TaskAuthority:
        async with self._lock:
            task = self._tasks.get(task_id)
            if task is None:
                raise TaskNotFoundError(task_id)
            return TaskAuthority(task.identity, TaskStatus(task.status))

    async def compare_status(
        self, expected: Task, status: TaskStatus, *, result: Any = None,
        error: str | None = None,
    ) -> Task | None:
        if not expected.status.can_transition_to(status) or not all(expected.identity):
            raise TaskStateConflictError()
        async with self._lock:
            current = self._tasks.get(expected.task_id)
            if current is None or current.identity != expected.identity or current.status != expected.status:
                return None
            winner = deepcopy(current)
            winner.status = status
            winner.updated_at = datetime.utcnow()
            if status == TaskStatus.PROCESSING:
                winner.started_at = winner.updated_at
            else:
                winner.completed_at = winner.updated_at
            if result is not None:
                winner.result = deepcopy(result)
            if error is not None:
                winner.error = error
            self._tasks[expected.task_id] = winner
            return deepcopy(winner)

    async def list(
        self,
        *,
        user_id: str | None = None,
        tenant_id: str | None = None,
        service_id: str | None = None,
        status: str | None = None,
        limit: int = 100,
        offset: int = 0,
    ) -> builtins.list[Task]:
        async with self._lock:
            tasks = list(self._tasks.values())
        if user_id:
            tasks = [task for task in tasks if task.user_id == user_id]
        if tenant_id:
            tasks = [task for task in tasks if task.tenant_id == tenant_id]
        if service_id:
            tasks = [task for task in tasks if task.service_id == service_id]
        if status:
            tasks = [task for task in tasks if task.status.value == status]
        tasks.sort(key=lambda task: (task.created_at, task.task_id), reverse=True)
        return deepcopy(tasks[offset : offset + limit])


class TaskManager:
    """
    任务管理器，支持异步任务处理和回调。

    特性：
    - 回调重试机制（指数退避）
    - 连接复用和超时配置
    - 回调失败日志记录
    """

    def __init__(
        self,
        storage: TaskStorage,
        queue: TaskQueue,
        callback_timeout: float = 30.0,
        callback_retries: int = 3,
        callback_retry_delay: float = 1.0,
    ):
        self.storage = storage
        self.queue = queue
        self.callback_timeout = callback_timeout
        self.callback_retries = callback_retries
        self.callback_retry_delay = callback_retry_delay
        # 共享 HTTP client 以复用连接
        self._callback_client: httpx.AsyncClient | None = None
        # 保护 HTTP client 创建的锁
        self._client_lock = asyncio.Lock()

    async def _get_callback_client(self) -> httpx.AsyncClient:
        """获取或创建共享的 HTTP client"""
        if self._callback_client is None or self._callback_client.is_closed:
            async with self._client_lock:
                # 双重检查锁定模式，避免多个协程同时创建 client
                if self._callback_client is None or self._callback_client.is_closed:
                    # follow_redirects=False so a hostile callback server
                    # can't 302 us into the private network after URL
                    # validation passed. SSRF defense in depth.
                    self._callback_client = httpx.AsyncClient(
                        timeout=httpx.Timeout(self.callback_timeout),
                        limits=httpx.Limits(max_connections=20, max_keepalive_connections=10),
                        follow_redirects=False,
                    )
        return self._callback_client

    async def create_task(
        self,
        request: UnifiedRequest,
        roles: list[str],
        client_ip: str | None,
    ) -> Task:
        # Admission owns its envelope before the first storage await.
        request = deepcopy(request)
        roles = list(roles)
        task = Task(
            task_id=str(uuid.uuid4()),
            request_id=request.request_id,
            service_id=request.service_id,
            status=TaskStatus.PENDING,
            created_at=datetime.utcnow(),
            user_id=request.user_id,
            tenant_id=request.tenant_id,
            callback_url=request.callback_url,
        )
        await self.storage.save(task)
        await self.queue.enqueue(task.task_id, request, roles, client_ip)
        return task

    async def get_task(self, task_id: str) -> Task:
        return await self.storage.get(task_id)

    async def list_tasks(
        self,
        *,
        user_id: str,
        tenant_id: str,
        status: str | None = None,
        limit: int = 100,
        offset: int = 0,
    ) -> builtins.list[Task]:
        return await self.storage.list(
            user_id=user_id,
            tenant_id=tenant_id,
            status=status,
            limit=limit,
            offset=offset,
        )

    async def claim_task(self, task_id: str, request: UnifiedRequest) -> Task | None:
        try:
            task = await self.storage.get(task_id)
        except asyncio.CancelledError as exc:
            raise TaskClaimReadCancelled() from exc
        except (ConnectionError, TimeoutError, ConnectionDoesNotExistError, ConnectionFailureError,
                CannotConnectNowError, TooManyConnectionsError) as exc:
            # Only transport/availability failures are retryable. Missing rows,
            # malformed payloads, permissions and configuration fail closed.
            # Never include compare_status in this try: its COMMIT may be unknown.
            raise TaskClaimReadUnavailable("task_claim_read_unavailable") from exc
        identity = (task_id, request.request_id, request.service_id, request.user_id, request.tenant_id)
        if not all(identity) or task.identity != identity:
            raise TaskStateConflictError()
        return await self.transition_task(task, TaskStatus.PROCESSING)

    async def transition_task(
        self, expected: Task, status: TaskStatus, *, result: Any = None,
        error: str | None = None,
    ) -> Task | None:
        if not expected.status.can_transition_to(status):
            return None
        task = await self.storage.compare_status(expected, status, result=result, error=error)
        if task is not None and status in {TaskStatus.COMPLETED, TaskStatus.FAILED} and task.callback_url:
            # The terminal row already committed. Callback failure cannot change it.
            try:
                await self._send_callback(task)
            except Exception:
                logger.warning("Task callback failed after terminal commit")
        return task

    async def update_status(
        self, task_id: str, status: TaskStatus, result: Any = None,
        error: str | None = None,
    ) -> None:
        """Compatibility wrapper; workers use the identity-bound claimed snapshot."""
        task = await self.storage.get(task_id)
        if await self.transition_task(task, status, result=result, error=error) is None:
            raise TaskStateConflictError()

    async def cancel(self, task_id: str, *, expected: Task | None = None) -> None:
        task = expected if expected is not None else await self.storage.get(task_id)
        identity = task.identity
        if task.task_id != task_id:
            raise TaskStateConflictError()
        # At most pending -> processing can precede a terminal winner. Do not
        # spin forever if a compatibility writer violates the monotonic graph.
        for _ in range(3):
            if task.identity != identity:
                raise TaskStateConflictError()
            if task.status == TaskStatus.CANCELLED:
                return
            if task.status not in {TaskStatus.PENDING, TaskStatus.PROCESSING}:
                raise TaskStateConflictError()
            if await self.transition_task(task, TaskStatus.CANCELLED) is not None:
                return
            task = await self.storage.get(task_id)
        raise TaskStateConflictError()

    async def get_result(self, task_id: str) -> UnifiedResponse:
        task = await self.storage.get(task_id)
        if isinstance(task.result, UnifiedResponse):
            return task.result
        raise TaskNotFoundError(task_id)

    async def _send_callback(self, task: Task) -> None:
        """发送回调，带重试机制

        SSRF guard: ``task.callback_url`` is user-controlled. Validate
        before any network attempt to refuse private/loopback/non-http
        targets. The HTTP client is configured with ``follow_redirects=False``
        so a hostile callback server can't redirect us into the private
        network mid-transaction.
        """
        from ai_gateway_core.security import SafeFetchError, safe_callback_post

        payload = {
            "task_id": task.task_id,
            "status": task.status.value,
            "result": project_task_result(task.result),
            "error": task.error,
        }

        last_error = None
        for attempt in range(self.callback_retries):
            try:
                response = await safe_callback_post(task.callback_url, json=payload)
                response.raise_for_status()
                logger.info(
                    f"Callback sent successfully for task {task.task_id} "
                    f"to {task.callback_url} (attempt {attempt + 1})"
                )
                return
            except SafeFetchError as exc:
                logger.warning(
                    f"Refusing callback for task {task.task_id}: {exc}"
                )
                return
            except httpx.TimeoutException as e:
                last_error = e
                logger.warning(
                    f"Callback timeout for task {task.task_id} "
                    f"(attempt {attempt + 1}/{self.callback_retries})"
                )
            except httpx.HTTPStatusError as e:
                last_error = e
                logger.warning(
                    f"Callback failed for task {task.task_id} with status {e.response.status_code} "
                    f"(attempt {attempt + 1}/{self.callback_retries})"
                )
            except Exception as e:
                last_error = e
                logger.warning(
                    f"Callback error for task {task.task_id}: {e} "
                    f"(attempt {attempt + 1}/{self.callback_retries})"
                )

            # 指数退避重试
            if attempt < self.callback_retries - 1:
                delay = self.callback_retry_delay * (2**attempt)
                await asyncio.sleep(delay)

        # 所有重试都失败
        logger.error(
            f"Callback failed for task {task.task_id} after {self.callback_retries} attempts. "
            f"Last error: {last_error}"
        )

    async def close(self) -> None:
        """关闭资源"""
        if self._callback_client and not self._callback_client.is_closed:
            await self._callback_client.aclose()
