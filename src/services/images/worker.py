"""Durable image task worker hook for Gateway startup wiring."""

from __future__ import annotations

import asyncio
import json
from collections.abc import Awaitable, Callable
from typing import Any

from ...api.schemas.assistant import AsyncImageGenerationRequest


def decode_image_task(task: dict[str, Any]) -> AsyncImageGenerationRequest:
    """Decode only the durable request payload from a claimed task row."""

    payload = task.get("request_payload")
    if isinstance(payload, str):
        payload = json.loads(payload)
    if not isinstance(payload, dict):
        raise ValueError("image task payload is invalid")
    return AsyncImageGenerationRequest.model_validate(payload)


class ImageTaskWorker:
    """Single-concurrency claim/dispatch loop; PostgreSQL is the authority."""

    def __init__(self, service: Any, decode: Callable[[dict[str, Any]], Any]):
        self.service = service
        self.decode = decode
        self._stop = asyncio.Event()
        self._finished = asyncio.Event()
        self._finished.set()
        self._drain = False
        self._running = False
        self._loop_task: asyncio.Task[None] | None = None
        self._execute: Callable[[Any, str], Awaitable[Any]] | None = None

    async def run_once(
        self, execute: Callable[[Any, str], Awaitable[Any]] | None = None
    ) -> int:
        if self._stop.is_set() or self._drain or self._running:
            return 0
        self._running = True
        self._finished.clear()
        try:
            claimed = await self.service.claim_pending(limit=1, visibility_seconds=300)
            if not claimed:
                return 0
            task = claimed[0]
            handler = execute or self._execute
            if handler is None:
                handler = self._default_execute
            try:
                await handler(self.decode(task), task["task_id"])
            except Exception:
                mark_unknown = getattr(self.service, "mark_unknown", None)
                if callable(mark_unknown):
                    await mark_unknown(task["task_id"], error="worker execution failed")
                raise
            return 1
        finally:
            self._running = False
            self._finished.set()

    async def _default_execute(self, body: Any, task_id: str) -> Any:
        return await self.service.execute_claimed(body, task_id)

    async def _run_loop(self, poll_interval: float) -> None:
        while not self._stop.is_set() and not self._drain:
            try:
                claimed = await self.run_once()
            except asyncio.CancelledError:
                raise
            except Exception:
                # The row has already been fenced by ``run_once``.  Keep the
                # worker alive for the next durable task without leaking the
                # provider error to the process supervisor.
                claimed = 0
            if claimed == 0 and not self._stop.is_set() and not self._drain:
                await asyncio.sleep(max(0.05, poll_interval))
        await self._finished.wait()

    async def start(
        self,
        execute: Callable[[Any, str], Awaitable[Any]] | None = None,
        *,
        poll_interval: float = 0.5,
    ) -> None:
        """Start the claim loop once; repeated startup is idempotent."""

        if self._loop_task is not None and not self._loop_task.done():
            return
        self._stop.clear()
        self._drain = False
        self._execute = execute
        self._loop_task = asyncio.create_task(self._run_loop(poll_interval))

    async def drain_and_wait(self) -> None:
        """Stop claiming and wait for the current provider call to finish."""

        self._drain = True
        if self._loop_task is not None:
            self._stop.set()
            await self._loop_task
        await self._finished.wait()

    async def shutdown(self) -> None:
        """Drain in-flight work and terminate the worker task."""

        await self.drain_and_wait()

    def stop(self) -> None:
        self._drain = True
        self._stop.set()

    def drain(self) -> None:
        self._drain = True

    async def wait_stopped(self) -> None:
        if self._loop_task is None:
            return
        await self._stop.wait()
        if self._loop_task is not None:
            await self._loop_task
        await self._finished.wait()


__all__ = ["ImageTaskWorker", "decode_image_task"]
