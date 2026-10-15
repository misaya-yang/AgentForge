"""
服务注册表测试

测试内容：
- 服务注册
- 服务查询
- 服务配置解析
"""

import pytest
from ai_gateway_core.enums import (
    ConnectorType,
    ContentType,
    InvocationMode,
    ServiceType,
    TransportType,
)

from src.services.registry.service_registry import MemoryRegistryStorage, ServiceRegistry


def test_connector_type_remains_a_transport_type_alias() -> None:
    assert ConnectorType is TransportType


class TestServiceRegistry:
    """服务注册表测试"""

    @pytest.fixture
    def registry(self):
        """服务注册表实例"""
        storage = MemoryRegistryStorage()
        return ServiceRegistry(storage)

    @pytest.fixture
    def sample_service_data(self):
        """样本服务数据"""
        return {
            "service_id": "test-svc",
            "name": "Test",
            "service_type": "processing",
            "supported_modes": ["sync", "async"],
            "connector_type": "http",
            "connector_config": {"base_url": "http://example"},
            "accepted_content_types": ["text"],
            "output_content_types": ["text"],
            "metadata": {"adapter_type": "generic_rest"},
        }

    @pytest.mark.asyncio
    async def test_service_from_dict(self, registry, sample_service_data):
        """测试从字典创建服务"""
        service = registry._service_from_dict(sample_service_data)

        assert service.service_id == "test-svc"
        assert service.service_type == ServiceType.PROCESSING
        assert service.supported_modes == [InvocationMode.SYNC, InvocationMode.ASYNC]
        assert service.connector_type == TransportType.HTTP
        assert service.accepted_content_types == [ContentType.TEXT]

    @pytest.mark.asyncio
    async def test_register_and_get_service(self, registry, sample_service_data):
        """测试注册和获取服务"""
        service = registry._service_from_dict(sample_service_data)
        await registry.register(service)

        got = await registry.get("test-svc")
        assert got is not None
        assert got.service_id == "test-svc"
        assert got.name == "Test"

    @pytest.mark.asyncio
    async def test_get_nonexistent_service(self, registry):
        """测试获取不存在的服务"""
        got = await registry.get("nonexistent")
        assert got is None

    @pytest.mark.asyncio
    async def test_list_services(self, registry, sample_service_data):
        """测试列出服务"""
        service = registry._service_from_dict(sample_service_data)
        await registry.register(service)

        services = await registry.list()
        assert len(services) >= 1
        assert any(s.service_id == "test-svc" for s in services)
