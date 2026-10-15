from __future__ import annotations

import asyncio
from abc import ABC, abstractmethod
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager, suppress
from typing import Any

from ai_gateway_core.exceptions import GatewayError

from ..core.gateway.admission import _finish_cleanup
from ..models.request import UnifiedRequest
from ..models.response import StreamChunk, UnifiedResponse
from ..models.service import ServiceDefinition
from ..transports.base import create_connector


@asynccontextmanager
async def _owned_stream(iterator: AsyncIterator[Any]):
    """Join cleanup of an iterator created by this wrapper before returning."""
    async def close() -> None:
        aclose = getattr(iterator, "aclose", None)
        if aclose is not None:
            await aclose()

    try:
        yield iterator
    except BaseException:
        # Cleanup must not replace the producer/delivery failure or cancellation.
        with suppress(BaseException):
            await _finish_cleanup(asyncio.create_task(close()))
        raise
    else:
        await _finish_cleanup(asyncio.create_task(close()))


class ProtocolAdapter(ABC):
    def __init__(self, service: ServiceDefinition):
        self.service = service
        self.connector = create_connector(service)

    @abstractmethod
    async def invoke(self, request: UnifiedRequest) -> UnifiedResponse:
        raise NotImplementedError

    async def stream(self, request: UnifiedRequest) -> AsyncIterator[StreamChunk]:
        raise NotImplementedError

    async def submit(self, request: UnifiedRequest) -> str:
        raise NotImplementedError

    async def get_result(self, task_id: str) -> UnifiedResponse:
        raise NotImplementedError

    async def health_check(self) -> bool:
        return await self.connector.health_check()

    def transform_request(self, request: UnifiedRequest) -> Any:
        return request

    def transform_response(self, response: Any) -> UnifiedResponse:
        return response


class BulkheadAdapter:
    """Concurrency wrapper around a concrete protocol adapter."""

    def __init__(
        self,
        inner: ProtocolAdapter,
        *,
        concurrency_limit: int,
        queue_timeout_seconds: float = 1.0,
    ) -> None:
        object.__setattr__(self, "_inner", inner)
        object.__setattr__(self, "_semaphore", asyncio.Semaphore(max(int(concurrency_limit), 1)))
        object.__setattr__(self, "_queue_timeout_seconds", max(float(queue_timeout_seconds), 0.001))

    async def invoke(self, request: UnifiedRequest) -> UnifiedResponse:
        return await self._with_slot(lambda: self._inner.invoke(request))

    async def stream(self, request: UnifiedRequest) -> AsyncIterator[StreamChunk]:
        await self._acquire()
        try:
            async with _owned_stream(self._inner.stream(request)) as producer:
                async for chunk in producer:
                    yield chunk
        finally:
            self._semaphore.release()

    async def submit(self, request: UnifiedRequest) -> str:
        return await self._with_slot(lambda: self._inner.submit(request))

    async def get_result(self, task_id: str) -> UnifiedResponse:
        return await self._with_slot(lambda: self._inner.get_result(task_id))

    async def health_check(self) -> bool:
        return await self._inner.health_check()

    def transform_request(self, request: UnifiedRequest) -> Any:
        return self._inner.transform_request(request)

    def transform_response(self, response: Any) -> UnifiedResponse:
        return self._inner.transform_response(response)

    async def _with_slot(self, factory):
        await self._acquire()
        try:
            return await factory()
        finally:
            self._semaphore.release()

    async def _acquire(self) -> None:
        try:
            await asyncio.wait_for(
                self._semaphore.acquire(),
                timeout=self._queue_timeout_seconds,
            )
        except asyncio.TimeoutError as exc:
            service_id = getattr(getattr(self._inner, "service", None), "service_id", "")
            raise GatewayError(f"Adapter bulkhead exhausted for {service_id}") from exc

    def __getattr__(self, name: str) -> Any:
        return getattr(self._inner, name)

    def __setattr__(self, name: str, value: Any) -> None:
        if name.startswith("_"):
            object.__setattr__(self, name, value)
            return
        setattr(self._inner, name, value)
