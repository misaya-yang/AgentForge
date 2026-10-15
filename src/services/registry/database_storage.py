"""
数据库服务注册存储

基于 PostgreSQL 的 ServiceRegistry 存储后端实现
"""

from __future__ import annotations

import builtins
import json
from typing import TYPE_CHECKING, Any

from ai_gateway_core.enums import ContentType, InvocationMode, ServiceType, TransportType

from ...models.service import (
    ServiceAuthConfig,
    ServiceCacheConfig,
    ServiceCapacityConfig,
    ServiceConfig,
    ServiceDefinition,
    ServicePriorityConfig,
    ServiceRateLimitConfig,
)
from .service_registry import RegistryStorage

if TYPE_CHECKING:
    from ...persistence.database import DatabaseStorage
    from ...persistence.redis import RedisStorage


class DatabaseRegistryStorage(RegistryStorage):
    """基于数据库的服务注册存储"""

    def __init__(
        self, database: DatabaseStorage, redis: RedisStorage | None = None, cache_ttl: int = 300
    ):
        self.database = database
        self.redis = redis
        self.cache_ttl = cache_ttl

    async def save(self, service: ServiceDefinition) -> None:
        """保存服务到数据库"""
        service_dict = self._service_to_dict(service)
        await self.database.save_service(service_dict)

        # 更新缓存
        if self.redis and self.redis.enabled:
            await self.redis.save(f"service:{service.service_id}", service_dict, self.cache_ttl)

    async def get(self, service_id: str) -> ServiceDefinition | None:
        """获取服务，优先从缓存读取"""
        # 尝试从缓存获取
        if self.redis and self.redis.enabled:
            cached = await self.redis.get(f"service:{service_id}")
            if cached:
                return self._dict_to_service(cached)

        # 从数据库获取
        service_dict = await self.database.get_service(service_id)
        if not service_dict:
            return None

        # 写入缓存
        if self.redis and self.redis.enabled:
            await self.redis.save(f"service:{service_id}", service_dict, self.cache_ttl)

        return self._dict_to_service(service_dict)

    async def list(self) -> builtins.list[ServiceDefinition]:
        """获取所有服务"""
        services_dict = await self.database.list_services()
        return [self._dict_to_service(s) for s in services_dict]

    async def delete(self, service_id: str) -> bool:
        """删除服务"""
        result = await self.database.delete_service(service_id)

        # 清除缓存
        if self.redis and self.redis.enabled:
            await self.redis.delete(f"service:{service_id}")

        return result

    def _service_to_dict(self, service: ServiceDefinition) -> dict[str, Any]:
        """将 ServiceDefinition 转换为字典"""
        result = {
            "service_id": service.service_id,
            "name": service.name,
            "description": service.description,
            "version": service.version,
            "service_type": service.service_type.value if service.service_type else "custom",
            "connector_type": service.connector_type.value if service.connector_type else "http",
            "connector_config": service.connector_config or {},
            "supported_modes": [m.value for m in (service.supported_modes or [])],
            "accepted_content_types": [t.value for t in (service.accepted_content_types or [])],
            "output_content_types": [t.value for t in (service.output_content_types or [])],
            "input_schema": service.input_schema or {},
            "output_schema": service.output_schema or {},
            "session_enabled": service.session_enabled,
            "session_adapter": service.session_adapter,
            "timeout": service.timeout,
            "max_retries": service.max_retries,
            "retry_delay": service.retry_delay,
            "circuit_breaker_enabled": service.circuit_breaker_enabled,
            "failure_threshold": service.failure_threshold,
            "recovery_timeout": service.recovery_timeout,
            "rate_limit": service.rate_limit,
            "concurrency_limit": service.concurrency_limit,
            "status": service.status,
            "tags": service.tags or [],
            "metadata": service.metadata or {},
            "async_config": service.async_config,
        }

        # 处理 service_config
        if service.service_config:
            sc = service.service_config
            result["service_config"] = {
                "rate_limit": {
                    "enabled": sc.rate_limit.enabled if sc.rate_limit else False,
                    "requests": sc.rate_limit.requests if sc.rate_limit else 100,
                    "window": sc.rate_limit.window if sc.rate_limit else 60,
                    "burst": sc.rate_limit.burst if sc.rate_limit else 0,
                    "strategy": sc.rate_limit.strategy if sc.rate_limit else "sliding_window",
                },
                "auth": {
                    "enabled": sc.auth.enabled if sc.auth else False,
                    "require_auth": sc.auth.require_auth if sc.auth else True,
                    "allowed_roles": sc.auth.allowed_roles if sc.auth else [],
                    "allowed_api_keys": sc.auth.allowed_api_keys if sc.auth else [],
                    "public": sc.auth.public if sc.auth else False,
                },
                "cache": {
                    "enabled": sc.cache.enabled if sc.cache else False,
                    "ttl": sc.cache.ttl if sc.cache else 300,
                    "semantic_cache": sc.cache.semantic_cache if sc.cache else False,
                },
                "priority": {
                    "priority": sc.priority.priority if sc.priority else 5,
                    "weight": sc.priority.weight if sc.priority else 1,
                    "max_queue_size": sc.priority.max_queue_size if sc.priority else 100,
                },
                "capacity": {
                    "upstream_group": sc.capacity.upstream_group if sc.capacity else None,
                    "concurrency_limit": sc.capacity.concurrency_limit if sc.capacity else None,
                    "queue_max": sc.capacity.queue_max if sc.capacity else 16,
                    "queue_timeout_ms": sc.capacity.queue_timeout_ms if sc.capacity else 3000,
                    "enforced": sc.capacity.enforced if sc.capacity else True,
                },
            }

        return result

    def _dict_to_service(self, data: dict[str, Any]) -> ServiceDefinition:
        """将字典转换为 ServiceDefinition"""

        def as_enum(enum_cls, value, default):
            if value is None:
                return default
            if isinstance(value, enum_cls):
                return value
            try:
                return enum_cls(value)
            except (ValueError, KeyError):
                return default

        service_type = as_enum(ServiceType, data.get("service_type"), ServiceType.CUSTOM)
        connector_type = as_enum(TransportType, data.get("connector_type"), TransportType.HTTP)

        supported_modes = []
        for m in data.get("supported_modes", ["sync"]):
            mode = as_enum(InvocationMode, m, InvocationMode.SYNC)
            supported_modes.append(mode)

        accepted = []
        for t in data.get("accepted_content_types", ["text"]):
            content_type = as_enum(ContentType, t, ContentType.TEXT)
            accepted.append(content_type)

        outputs = []
        for t in data.get("output_content_types", ["text"]):
            content_type = as_enum(ContentType, t, ContentType.TEXT)
            outputs.append(content_type)

        # 解析 connector_config
        connector_config = data.get("connector_config", {})
        if isinstance(connector_config, str):
            try:
                connector_config = json.loads(connector_config)
            except json.JSONDecodeError:
                connector_config = {}

        # 解析 service_config
        service_config = None
        sc_data = data.get("service_config")
        if sc_data:
            if isinstance(sc_data, str):
                try:
                    sc_data = json.loads(sc_data)
                except json.JSONDecodeError:
                    sc_data = {}

            if sc_data:
                service_config = ServiceConfig(
                    rate_limit=ServiceRateLimitConfig(
                        enabled=sc_data.get("rate_limit", {}).get("enabled", False),
                        requests=sc_data.get("rate_limit", {}).get("requests", 100),
                        window=sc_data.get("rate_limit", {}).get("window", 60),
                        burst=sc_data.get("rate_limit", {}).get("burst", 0),
                        strategy=sc_data.get("rate_limit", {}).get("strategy", "sliding_window"),
                    ),
                    auth=ServiceAuthConfig(
                        enabled=sc_data.get("auth", {}).get("enabled", False),
                        require_auth=sc_data.get("auth", {}).get("require_auth", True),
                        allowed_roles=sc_data.get("auth", {}).get("allowed_roles", []),
                        allowed_api_keys=sc_data.get("auth", {}).get("allowed_api_keys", []),
                        public=sc_data.get("auth", {}).get("public", False),
                    ),
                    cache=ServiceCacheConfig(
                        enabled=sc_data.get("cache", {}).get("enabled", False),
                        ttl=sc_data.get("cache", {}).get("ttl", 300),
                        semantic_cache=sc_data.get("cache", {}).get("semantic_cache", False),
                    ),
                    priority=ServicePriorityConfig(
                        priority=sc_data.get("priority", {}).get("priority", 5),
                        weight=sc_data.get("priority", {}).get("weight", 1),
                        max_queue_size=sc_data.get("priority", {}).get("max_queue_size", 100),
                    ),
                    capacity=ServiceCapacityConfig(
                        upstream_group=sc_data.get("capacity", {}).get("upstream_group"),
                        concurrency_limit=sc_data.get("capacity", {}).get("concurrency_limit"),
                        queue_max=sc_data.get("capacity", {}).get("queue_max", 16),
                        queue_timeout_ms=sc_data.get("capacity", {}).get("queue_timeout_ms", 3000),
                        enforced=sc_data.get("capacity", {}).get("enforced", True),
                    ),
                )

        # 解析 metadata
        metadata = data.get("metadata", {})
        if isinstance(metadata, str):
            try:
                metadata = json.loads(metadata)
            except json.JSONDecodeError:
                metadata = {}

        # 解析 input_schema
        input_schema = data.get("input_schema", {})
        if isinstance(input_schema, str):
            try:
                input_schema = json.loads(input_schema)
            except json.JSONDecodeError:
                input_schema = {}
        if not isinstance(input_schema, dict):
            input_schema = {}

        # 解析 output_schema
        output_schema = data.get("output_schema", {})
        if isinstance(output_schema, str):
            try:
                output_schema = json.loads(output_schema)
            except json.JSONDecodeError:
                output_schema = {}
        if not isinstance(output_schema, dict):
            output_schema = {}

        return ServiceDefinition(
            service_id=data["service_id"],
            name=data.get("name", data["service_id"]),
            description=data.get("description", ""),
            version=data.get("version", "1.0.0"),
            service_type=service_type,
            supported_modes=supported_modes,
            connector_type=connector_type,
            connector_config=connector_config,
            input_schema=input_schema,
            output_schema=output_schema,
            accepted_content_types=accepted,
            output_content_types=outputs,
            session_enabled=bool(data.get("session_enabled", False)),
            session_adapter=data.get("session_adapter"),
            timeout=int(data.get("timeout", 60)),
            max_retries=int(data.get("max_retries", 3)),
            retry_delay=float(data.get("retry_delay", 1.0)),
            circuit_breaker_enabled=bool(data.get("circuit_breaker_enabled", True)),
            failure_threshold=int(data.get("failure_threshold", 5)),
            recovery_timeout=int(data.get("recovery_timeout", 30)),
            rate_limit=data.get("rate_limit"),
            concurrency_limit=data.get("concurrency_limit"),
            status=data.get("status", "active"),
            tags=data.get("tags") or [],
            metadata=metadata,
            async_config=data.get("async_config"),
            service_config=service_config,
        )
