from __future__ import annotations

import asyncio
import time
from collections.abc import Awaitable, Callable
from typing import Any

from ai_gateway_core.exceptions import CircuitBreakerOpenError


class CircuitBreaker:
    states = ["CLOSED", "OPEN", "HALF_OPEN"]

    def __init__(
        self,
        service_id: str,
        failure_threshold: int = 5,
        success_threshold: int = 3,
        timeout: int = 30,
        excluded_exceptions: list[type[Exception]] | None = None,
    ):
        self.service_id = service_id
        self.failure_threshold = failure_threshold
        self.success_threshold = success_threshold
        self.timeout = timeout
        self.excluded_exceptions = excluded_exceptions or []

        self.state = "CLOSED"
        self.failure_count = 0
        self.success_count = 0
        self.last_failure_time: float | None = None
        self._lock = asyncio.Lock()

    def _should_attempt_reset(self) -> bool:
        if self.last_failure_time is None:
            return False
        return (time.time() - self.last_failure_time) >= self.timeout

    async def _on_success(self) -> None:
        async with self._lock:
            if self.state == "HALF_OPEN":
                self.success_count += 1
                if self.success_count >= self.success_threshold:
                    self.state = "CLOSED"
                    self.failure_count = 0
                    self.success_count = 0
            elif self.state == "CLOSED":
                self.failure_count = 0

    async def _on_failure(self, exc: Exception) -> None:
        if any(isinstance(exc, ex) for ex in self.excluded_exceptions):
            return
        async with self._lock:
            self.failure_count += 1
            self.last_failure_time = time.time()
            if self.failure_count >= self.failure_threshold:
                self.state = "OPEN"
                self.success_count = 0

    async def allow(self) -> None:
        async with self._lock:
            if self.state == "OPEN":
                if self._should_attempt_reset():
                    self.state = "HALF_OPEN"
                else:
                    raise CircuitBreakerOpenError(self.service_id)

    async def call(self, func: Callable[..., Awaitable[Any]], *args: Any, **kwargs: Any) -> Any:
        await self.allow()
        try:
            result = await func(*args, **kwargs)
        except Exception as exc:
            await self._on_failure(exc)
            raise
        await self._on_success()
        return result
