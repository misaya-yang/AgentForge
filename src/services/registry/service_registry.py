from __future__ import annotations

import builtins
import json
import os
from pathlib import Path
from typing import Any

import yaml
from ai_gateway_core.enums import ContentType, InvocationMode, ServiceType, TransportType
from ai_gateway_core.exceptions import AdapterNotFoundError, ValidationFailedError

from ...adapters.base import BulkheadAdapter, ProtocolAdapter
from ...models.service import (
    ServiceAuthConfig,
    ServiceCacheConfig,
    ServiceCapacityConfig,
    ServiceConfig,
    ServiceDefinition,
    ServicePriorityConfig,
    ServiceRateLimitConfig,
)


class RegistryStorage:
    async def save(self, service: ServiceDefinition) -> None:
        raise NotImplementedError

    async def get(self, service_id: str) -> ServiceDefinition | None:
        raise NotImplementedError

    async def list(self) -> builtins.list[ServiceDefinition]:
        raise NotImplementedError

    async def delete(self, service_id: str) -> bool:
        raise NotImplementedError


class MemoryRegistryStorage(RegistryStorage):
    def __init__(self):
        self._services: dict[str, ServiceDefinition] = {}

    async def save(self, service: ServiceDefinition) -> None:
        self._services[service.service_id] = service

    async def get(self, service_id: str) -> ServiceDefinition | None:
        return self._services.get(service_id)

    async def list(self) -> builtins.list[ServiceDefinition]:
        return list(self._services.values())

    async def delete(self, service_id: str) -> bool:
        return self._services.pop(service_id, None) is not None


class ServiceRegistry:
    def __init__(self, storage: RegistryStorage):
        self.storage = storage
        self._cache: dict[str, ServiceDefinition] = {}
        self._adapters: dict[str, type[ProtocolAdapter]] = {}
        self._adapter_cache: dict[str, ProtocolAdapter] = {}

    async def register(self, service: ServiceDefinition) -> None:
        self._validate_service(service)
        await self.storage.save(service)
        self._cache[service.service_id] = service
        self._adapter_cache.pop(service.service_id, None)

    async def register_from_config(self, config_path: str) -> None:
        path = Path(config_path)
        files: list[Path]
        if path.is_dir():
            files = [p for p in path.rglob("*") if p.suffix.lower() in {".yaml", ".yml", ".json"}]
        else:
            files = [path]

        for file_path in files:
            text = file_path.read_text(encoding="utf-8")
            data = json.loads(text) if file_path.suffix.lower() == ".json" else yaml.safe_load(text)
            if not data:
                continue
            service = self._service_from_dict(data)
            await self.register(service)

    async def get(self, service_id: str) -> ServiceDefinition | None:
        if service_id in self._cache:
            return self._cache[service_id]
        service = await self.storage.get(service_id)
        if service:
            self._cache[service_id] = service
        return service

    async def list(
        self,
        service_type: ServiceType | None = None,
        tags: builtins.list[str] | None = None,
        status: str = "active",
    ) -> builtins.list[ServiceDefinition]:
        # Prefer persistent storage, but always merge with in-memory cache so
        # newly registered services remain visible even if the DB backend is
        # temporarily unavailable (e.g. local dev without PostgreSQL).
        services = await self.storage.list()
        merged: dict[str, ServiceDefinition] = {s.service_id: s for s in services}
        merged.update(self._cache)

        result: list[ServiceDefinition] = []
        for service in merged.values():
            if status and service.status != status:
                continue
            if service_type and service.service_type != service_type:
                continue
            if tags and not set(tags).issubset(set(service.tags or [])):
                continue
            result.append(service)
        return result

    async def find_by_capability(
        self, content_types: builtins.list[ContentType]
    ) -> builtins.list[ServiceDefinition]:
        services = await self.storage.list()
        wanted = set(content_types)
        return [s for s in services if wanted.issubset(set(s.accepted_content_types or []))]

    def register_adapter(self, adapter_type: str, adapter_class: type[ProtocolAdapter]) -> None:
        self._adapters[adapter_type] = adapter_class

    def get_adapter(self, service: ServiceDefinition) -> ProtocolAdapter:
        # Cache adapters by service_id to avoid creating new instances per request
        cache_key = service.service_id
        if cache_key in self._adapter_cache:
            return self._adapter_cache[cache_key]

        adapter_type = (service.metadata or {}).get("adapter_type", "generic_rest")
        adapter_class = self._adapters.get(adapter_type)
        if not adapter_class:
            raise AdapterNotFoundError(adapter_type)
        adapter = adapter_class(service)
        adapter = self._wrap_adapter_bulkhead(service, adapter, adapter_type)
        self._adapter_cache[cache_key] = adapter
        return adapter

    def _wrap_adapter_bulkhead(
        self,
        service: ServiceDefinition,
        adapter: ProtocolAdapter,
        adapter_type: str,
    ):
        metadata = service.metadata or {}
        bulkhead = metadata.get("bulkhead") if isinstance(metadata.get("bulkhead"), dict) else {}
        limit = bulkhead.get("concurrency_limit")
        queue_timeout = bulkhead.get("queue_timeout_seconds", 1.0)
        if limit is None:
            limit = _adapter_bulkhead_limit_from_env(adapter_type)
        if limit is None:
            return adapter
        return BulkheadAdapter(
            adapter,
            concurrency_limit=int(limit),
            queue_timeout_seconds=float(queue_timeout),
        )

    def _validate_service(self, service: ServiceDefinition) -> None:
        if not service.service_id:
            raise ValidationFailedError("service_id is required")
        if not service.connector_type:
            raise ValidationFailedError("connector_type is required")

    def _service_from_dict(self, data: dict[str, Any]) -> ServiceDefinition:
        def as_enum(enum_cls, value, default):
            if value is None:
                return default
            return enum_cls(value)

        service_type = as_enum(ServiceType, data.get("service_type"), ServiceType.CUSTOM)
        supported_modes = [
            as_enum(InvocationMode, m, InvocationMode.SYNC)
            for m in data.get("supported_modes", ["sync"])
        ]
        connector_type = as_enum(TransportType, data.get("connector_type"), TransportType.HTTP)

        accepted = [
            as_enum(ContentType, t, ContentType.TEXT)
            for t in data.get("accepted_content_types", [])
        ]
        outputs = [
            as_enum(ContentType, t, ContentType.TEXT) for t in data.get("output_content_types", [])
        ]

        # 解析服务级别配置
        service_config = None
        if data.get("service_config"):
            sc = data["service_config"]
            service_config = ServiceConfig(
                rate_limit=ServiceRateLimitConfig(
                    enabled=sc.get("rate_limit", {}).get("enabled", False),
                    requests=sc.get("rate_limit", {}).get("requests", 100),
                    window=sc.get("rate_limit", {}).get("window", 60),
                    burst=sc.get("rate_limit", {}).get("burst", 0),
                    strategy=sc.get("rate_limit", {}).get("strategy", "sliding_window"),
                ),
                auth=ServiceAuthConfig(
                    enabled=sc.get("auth", {}).get("enabled", False),
                    require_auth=sc.get("auth", {}).get("require_auth", True),
                    allowed_roles=sc.get("auth", {}).get("allowed_roles", []),
                    allowed_api_keys=sc.get("auth", {}).get("allowed_api_keys", []),
                    public=sc.get("auth", {}).get("public", False),
                ),
                cache=ServiceCacheConfig(
                    enabled=sc.get("cache", {}).get("enabled", False),
                    ttl=sc.get("cache", {}).get("ttl", 300),
                    semantic_cache=sc.get("cache", {}).get("semantic_cache", False),
                ),
                priority=ServicePriorityConfig(
                    priority=sc.get("priority", {}).get("priority", 5),
                    weight=sc.get("priority", {}).get("weight", 1),
                    max_queue_size=sc.get("priority", {}).get("max_queue_size", 100),
                ),
                capacity=ServiceCapacityConfig(
                    upstream_group=sc.get("capacity", {}).get("upstream_group"),
                    concurrency_limit=sc.get("capacity", {}).get("concurrency_limit"),
                    queue_max=sc.get("capacity", {}).get("queue_max", 16),
                    queue_timeout_ms=sc.get("capacity", {}).get("queue_timeout_ms", 3000),
                    enforced=sc.get("capacity", {}).get("enforced", True),
                ),
            )

        return ServiceDefinition(
            service_id=data["service_id"],
            name=data.get("name", data["service_id"]),
            description=data.get("description", ""),
            version=data.get("version", "1.0.0"),
            service_type=service_type,
            supported_modes=supported_modes,
            connector_type=connector_type,
            connector_config=data.get("connector_config") or {},
            input_schema=data.get("input_schema") or {},
            output_schema=data.get("output_schema") or {},
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
            metadata=data.get("metadata") or {},
            async_config=data.get("async_config"),
            service_config=service_config,
        )


def _adapter_bulkhead_limit_from_env(adapter_type: str) -> int | None:
    raw = os.getenv("ADAPTER_BULKHEAD_LIMITS", "").strip()
    if not raw:
        return None
    for item in raw.split(","):
        if ":" not in item:
            continue
        name, value = item.split(":", 1)
        if name.strip() != adapter_type:
            continue
        try:
            parsed = int(value.strip())
        except ValueError:
            return None
        return parsed if parsed > 0 else None
    return None
