from __future__ import annotations

import asyncio

import pytest
from ai_gateway_core.enums import ContentType, TransportType
from ai_gateway_core.exceptions import GatewayError

from src.adapters.base import ProtocolAdapter
from src.models.request import ContentItem, UnifiedRequest
from src.models.response import UnifiedResponse
from src.models.service import ServiceDefinition
from src.services.registry.service_registry import MemoryRegistryStorage, ServiceRegistry


class SlowAdapter(ProtocolAdapter):
    gate: asyncio.Event

    async def invoke(self, request: UnifiedRequest) -> UnifiedResponse:
        await self.gate.wait()
        return UnifiedResponse(
            request_id=request.request_id,
            status="success",
            outputs=[],
        )


@pytest.mark.asyncio
async def test_service_registry_wraps_adapter_with_configured_bulkhead() -> None:
    registry = ServiceRegistry(MemoryRegistryStorage())
    registry.register_adapter("slow", SlowAdapter)
    service = ServiceDefinition(
        service_id="slow-service",
        name="Slow Service",
        connector_type=TransportType.IN_PROCESS,
        connector_config={
            "module": "src.main",
            "callable": "create_app",
        },
        metadata={
            "adapter_type": "slow",
            "bulkhead": {"concurrency_limit": 1, "queue_timeout_seconds": 0.01},
        },
    )
    await registry.register(service)

    adapter = registry.get_adapter(service)
    adapter.gate = asyncio.Event()

    first = asyncio.create_task(
        adapter.invoke(
            UnifiedRequest(
                request_id="req-1",
                service_id="slow-service",
                inputs=[ContentItem(type=ContentType.TEXT, data="one")],
            )
        )
    )
    await asyncio.sleep(0.01)
    with pytest.raises(GatewayError):
        await adapter.invoke(
            UnifiedRequest(
                request_id="req-2",
                service_id="slow-service",
                inputs=[ContentItem(type=ContentType.TEXT, data="two")],
            )
        )
    adapter.gate.set()
    await first
