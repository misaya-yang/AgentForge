from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass, field
from datetime import datetime

from ...models.service import ServiceDefinition
from .service_registry import ServiceRegistry


@dataclass
class HealthStatus:
    service_id: str
    status: str
    latency: float = 0.0
    last_check: datetime = field(default_factory=datetime.utcnow)
    error: str | None = None


class HealthMonitor:
    def __init__(self, registry: ServiceRegistry, check_interval: int = 30):
        self.registry = registry
        self.check_interval = check_interval
        self._health_status: dict[str, HealthStatus] = {}
        self._task: asyncio.Task | None = None

    async def start(self) -> None:
        if self._task is None:
            self._task = asyncio.create_task(self._background_check())

    async def stop(self) -> None:
        if self._task:
            self._task.cancel()
            self._task = None

    async def _background_check(self) -> None:
        while True:
            services = await self.registry.list()
            for service in services:
                status = await self.check_service(service)
                self._health_status[service.service_id] = status
            await asyncio.sleep(self.check_interval)

    async def check_service(self, service: ServiceDefinition) -> HealthStatus:
        start = time.time()
        try:
            adapter = self.registry.get_adapter(service)
            ok = await adapter.health_check()
            latency = time.time() - start
            return HealthStatus(
                service_id=service.service_id,
                status="healthy" if ok else "unhealthy",
                latency=latency,
                last_check=datetime.utcnow(),
            )
        except Exception as exc:
            latency = time.time() - start
            return HealthStatus(
                service_id=service.service_id,
                status="error",
                latency=latency,
                last_check=datetime.utcnow(),
                error=str(exc),
            )

    def get_status(self, service_id: str) -> HealthStatus | None:
        return self._health_status.get(service_id)

    def all_status(self) -> dict[str, HealthStatus]:
        return dict(self._health_status)
