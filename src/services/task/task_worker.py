from __future__ import annotations

import asyncio
import logging
from copy import deepcopy

from ...core.gateway.admission import _finish_cleanup
from ...core.gateway.dispatcher import GatewayDispatcher
from ...models.request import UnifiedRequest
from ...models.response import UnifiedResponse
from ...models.task import Task, TaskStatus
from .task_manager import (
    TaskClaimReadCancelled,
    TaskClaimReadUnavailable,
    TaskManager,
    TaskStorage,
)
from .task_queue import TaskQueue

logger = logging.getLogger(__name__)


class _TaskObservationError(RuntimeError):
    """Execution authority could not be observed; do not project a failure."""


async def _join_tasks(tasks: list[asyncio.Task]) -> None:
    await asyncio.gather(*tasks, return_exceptions=True)


class TaskWorker:
    def __init__(
        self,
        queue: TaskQueue,
        storage: TaskStorage,
        dispatcher: GatewayDispatcher,
        task_manager: TaskManager,
    ):
        self.queue = queue
        self.storage = storage
        self.dispatcher = dispatcher
        self.task_manager = task_manager
        self._workers: list[asyncio.Task] = []
        self._running = False
        self._stop_task: asyncio.Task | None = None

    async def start(self, concurrency: int = 1) -> None:
        if self._running or self._workers:
            return
        self._running = True
        self._stop_task = None
        for _ in range(concurrency):
            self._workers.append(asyncio.create_task(self._run()))

    async def stop(self) -> None:
        self._running = False
        if self._stop_task is None:
            for task in self._workers:
                task.cancel()
            self._stop_task = asyncio.create_task(_join_tasks(self._workers))
        # Retain the same cleanup across concurrent/repeated stop callers. Do
        # not abandon an invocation's admission cleanup on caller cancellation.
        stop_task = self._stop_task
        try:
            await _finish_cleanup(stop_task)
        finally:
            if self._stop_task is stop_task and stop_task.done():
                self._workers = []

    async def _run(self) -> None:
        while self._running:
            task_id, request, roles, client_ip = await self.queue.dequeue()
            try:
                await self._process(task_id, request, roles, client_ip)
            except Exception:
                # An unavailable/uncertain authority must never cause invocation
                # or replay. Isolate this item without exposing backend details.
                logger.warning("Task processing stopped without a confirmed transition")

    async def _process(
        self, task_id: str, request: UnifiedRequest, roles: list[str], client_ip: str | None,
    ) -> None:
        # A memory queue can retain caller-owned nested inputs. Verify and invoke
        # the same detached envelope even if the original mutates during CAS.
        request = deepcopy(request)
        roles = list(roles)
        claimed = await self._claim_before_dispatch(task_id, request, roles, client_ip)
        if claimed is None:
            return
        try:
            response = await self._invoke_while_owned(claimed, request, roles, client_ip)
        except _TaskObservationError:
            raise
        except Exception as exc:
            await self.task_manager.transition_task(claimed, TaskStatus.FAILED, error=str(exc))
            return
        if response is None:
            return
        try:
            await self.task_manager.transition_task(claimed, TaskStatus.COMPLETED, result=response)
        except Exception:
            # A completion COMMIT may already have won. Only the still-processing
            # original identity can accept a failure projection; never re-invoke.
            await self.task_manager.transition_task(
                claimed, TaskStatus.FAILED, error="task_completion_persistence_failed",
            )

    async def _claim_before_dispatch(
        self, task_id: str, request: UnifiedRequest, roles: list[str], client_ip: str | None,
    ) -> Task | None:
        failures = 0
        while True:
            try:
                return await self.task_manager.claim_task(task_id, request)
            except TaskClaimReadCancelled:
                await self._return_unclaimed(task_id, request, roles, client_ip)
                raise
            except TaskClaimReadUnavailable:
                failures += 1
                if failures == 1 or failures % 60 == 0:
                    logger.warning("Task claim read unavailable; retaining undispatched envelope")
                # One held envelope per lane: no hot loop, unbounded retry queue,
                # new identity or invocation. Cancellation is still responsive.
                try:
                    await asyncio.sleep(1.0)
                except asyncio.CancelledError:
                    await self._return_unclaimed(task_id, request, roles, client_ip)
                    raise

    async def _return_unclaimed(
        self, task_id: str, request: UnifiedRequest, roles: list[str], client_ip: str | None,
    ) -> None:
        # Only definitely pre-claim cancellation reaches here. Reinsert for a
        # same-process worker restart; never replay a possibly committed claim.
        await _finish_cleanup(asyncio.create_task(
            self.queue.enqueue(task_id, request, roles, client_ip),
        ))

    async def _still_processing(self, claimed: Task) -> bool:
        try:
            current = await self.task_manager.storage.get_authority(claimed.task_id)
        except Exception as exc:
            raise _TaskObservationError("Task execution authority unavailable") from exc
        return current.identity == claimed.identity and current.status == TaskStatus.PROCESSING

    async def _observe_ownership(self, claimed: Task) -> None:
        while True:
            await asyncio.sleep(1.0)
            if not await self._still_processing(claimed):
                return

    async def _invoke_while_owned(
        self, claimed: Task, request: UnifiedRequest, roles: list[str], client_ip: str | None,
    ) -> UnifiedResponse | None:
        # One initial read, then at most one read per second per active worker
        # lane. Queued tasks create no observers. Both children remain owned.
        if not await self._still_processing(claimed):
            return None
        invoke = asyncio.create_task(
            self.dispatcher.invoke(request, roles=roles, client_ip=client_ip),
        )
        observe = asyncio.create_task(self._observe_ownership(claimed))
        try:
            done, _ = await asyncio.wait({invoke, observe}, return_when=asyncio.FIRST_COMPLETED)
            if observe in done:
                observe.result()
                return None
            # Independent adapter cancellation is not worker shutdown. Leave
            # its outcome unconfirmed, without replay or callback.
            return None if invoke.cancelled() else invoke.result()
        finally:
            for child in (invoke, observe):
                if not child.done():
                    child.cancel()
            await _finish_cleanup(asyncio.create_task(_join_tasks([invoke, observe])))
