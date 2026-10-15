"""
服务仓库

提供服务定义的数据访问接口
"""

from __future__ import annotations

import builtins
import json
import logging
from abc import ABC, abstractmethod
from datetime import datetime
from typing import TYPE_CHECKING, Any

from .base import BaseRepository

if TYPE_CHECKING:
    from ..redis import RedisStorage

logger = logging.getLogger(__name__)


def build_service_query(
    status: str | None = None,
    service_type: str | None = None,
    tags: builtins.list[str] | None = None,
) -> tuple[str, builtins.list[Any]]:
    """Build service query with safe parameterization.

    Uses len(params) for parameter indexing to avoid off-by-one errors.
    This is safer than manually tracking param_idx.

    Args:
        status: Filter by service status
        service_type: Filter by service type
        tags: Filter by tags (array overlap)

    Returns:
        Tuple of (query_string, params_list)
    """
    query_parts = ["SELECT * FROM services WHERE 1=1"]
    params: builtins.list[Any] = []

    if status:
        params.append(status)
        query_parts.append(f"AND status = ${len(params)}")

    if service_type:
        params.append(service_type)
        query_parts.append(f"AND service_type = ${len(params)}")

    if tags:
        params.append(tags)
        query_parts.append(f"AND tags && ${len(params)}")

    query_parts.append("ORDER BY created_at DESC")

    return " ".join(query_parts), params


def _row_to_dict(row: Any) -> dict[str, Any]:
    """Convert a database row to a dict, handling JSON and datetime fields."""
    if not row:
        return {}
    result = dict(row)

    # JSON fields that should be parsed as dicts
    json_dict_fields = {"metadata", "connector_config", "input_schema", "output_schema",
                        "rate_limit", "service_config", "async_config"}

    for key, value in result.items():
        if isinstance(value, datetime):
            result[key] = value.isoformat()
        elif key in json_dict_fields and value is not None:
            if isinstance(value, str):
                try:
                    result[key] = json.loads(value)
                except (json.JSONDecodeError, TypeError):
                    result[key] = {}
            elif not isinstance(value, dict):
                result[key] = {}
    return result


class ServiceRepository(ABC):
    """服务仓库抽象基类"""

    @abstractmethod
    async def save(self, service: dict[str, Any]) -> None:
        """保存服务"""
        pass

    @abstractmethod
    async def get(self, service_id: str) -> dict[str, Any] | None:
        """获取服务"""
        pass

    @abstractmethod
    async def list(
        self,
        status: str | None = None,
        service_type: str | None = None,
        tags: builtins.list[str] | None = None,
    ) -> builtins.list[dict[str, Any]]:
        """获取服务列表"""
        pass

    @abstractmethod
    async def delete(self, service_id: str) -> bool:
        """删除服务"""
        pass

    @abstractmethod
    async def update_status(self, service_id: str, status: str) -> None:
        """更新服务状态"""
        pass


class DatabaseServiceRepository(ServiceRepository, BaseRepository):
    """基于 PostgreSQL 的服务仓库实现"""

    def __init__(
        self, pool_holder: Any, redis: RedisStorage | None = None, cache_ttl: int = 300
    ):
        BaseRepository.__init__(self, pool_holder)
        self.redis = redis
        self.cache_ttl = cache_ttl

    async def save(self, service: dict[str, Any]) -> None:
        """保存或更新服务"""
        await self.execute(
            """
            INSERT INTO services (
                service_id, name, description, version, service_type,
                connector_type, connector_config, supported_modes,
                accepted_content_types, output_content_types,
                input_schema, output_schema, session_enabled, session_adapter,
                timeout, max_retries, retry_delay, circuit_breaker_enabled,
                failure_threshold, recovery_timeout, rate_limit, concurrency_limit,
                service_config, async_config, status, tags, metadata
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                $11, $12, $13, $14, $15, $16, $17, $18, $19, $20,
                $21, $22, $23, $24, $25, $26, $27
            )
            ON CONFLICT (service_id) DO UPDATE SET
                name = EXCLUDED.name,
                description = EXCLUDED.description,
                version = EXCLUDED.version,
                service_type = EXCLUDED.service_type,
                connector_type = EXCLUDED.connector_type,
                connector_config = EXCLUDED.connector_config,
                supported_modes = EXCLUDED.supported_modes,
                accepted_content_types = EXCLUDED.accepted_content_types,
                output_content_types = EXCLUDED.output_content_types,
                input_schema = EXCLUDED.input_schema,
                output_schema = EXCLUDED.output_schema,
                session_enabled = EXCLUDED.session_enabled,
                session_adapter = EXCLUDED.session_adapter,
                timeout = EXCLUDED.timeout,
                max_retries = EXCLUDED.max_retries,
                retry_delay = EXCLUDED.retry_delay,
                circuit_breaker_enabled = EXCLUDED.circuit_breaker_enabled,
                failure_threshold = EXCLUDED.failure_threshold,
                recovery_timeout = EXCLUDED.recovery_timeout,
                rate_limit = EXCLUDED.rate_limit,
                concurrency_limit = EXCLUDED.concurrency_limit,
                service_config = EXCLUDED.service_config,
                async_config = EXCLUDED.async_config,
                status = EXCLUDED.status,
                tags = EXCLUDED.tags,
                metadata = EXCLUDED.metadata,
                updated_at = NOW()
            """,
            service.get("service_id"),
            service.get("name"),
            service.get("description"),
            service.get("version", "1.0.0"),
            service.get("service_type", "custom"),
            service.get("connector_type", "http"),
            json.dumps(service.get("connector_config", {})),
            service.get("supported_modes", ["sync", "stream"]),
            service.get("accepted_content_types", ["text"]),
            service.get("output_content_types", ["text"]),
            json.dumps(service.get("input_schema", {})),
            json.dumps(service.get("output_schema", {})),
            service.get("session_enabled", False),
            service.get("session_adapter"),
            service.get("timeout", 60),
            service.get("max_retries", 3),
            service.get("retry_delay", 1.0),
            service.get("circuit_breaker_enabled", True),
            service.get("failure_threshold", 5),
            service.get("recovery_timeout", 30),
            json.dumps(service.get("rate_limit")) if service.get("rate_limit") else None,
            service.get("concurrency_limit"),
            json.dumps(service.get("service_config", {})),
            json.dumps(service.get("async_config")) if service.get("async_config") else None,
            service.get("status", "active"),
            service.get("tags", []),
            json.dumps(service.get("metadata", {})),
        )
        # 更新缓存
        if self.redis and self.redis.enabled:
            service_id = service.get("service_id")
            await self.redis.save(f"service:{service_id}", service, self.cache_ttl)
            # 使列表缓存失效
            await self.redis.delete("services:list")

    async def get(self, service_id: str) -> dict[str, Any] | None:
        """获取服务，优先从缓存读取"""
        # 尝试从缓存获取
        if self.redis and self.redis.enabled:
            cached = await self.redis.get(f"service:{service_id}")
            if cached:
                return cached

        # 从数据库获取
        if not self.enabled:
            return None
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow("SELECT * FROM services WHERE service_id = $1", service_id)
            service = _row_to_dict(row) if row else None

        # 写入缓存
        if service and self.redis and self.redis.enabled:
            await self.redis.save(f"service:{service_id}", service, self.cache_ttl)

        return service

    async def list(
        self,
        status: str | None = None,
        service_type: str | None = None,
        tags: builtins.list[str] | None = None,
    ) -> builtins.list[dict[str, Any]]:
        """获取服务列表

        Uses build_service_query() for safe parameterization.
        """
        if not self.enabled:
            return []

        query, params = build_service_query(status=status, service_type=service_type, tags=tags)

        async with self._pool.acquire() as conn:
            rows = await conn.fetch(query, *params)
            return [_row_to_dict(row) for row in rows]

    async def delete(self, service_id: str) -> bool:
        """删除服务"""
        result = await self.execute("DELETE FROM services WHERE service_id = $1", service_id)
        # 清除缓存
        if self.redis and self.redis.enabled:
            await self.redis.delete(f"service:{service_id}")
            await self.redis.delete("services:list")
        return result == "DELETE 1"

    async def update_status(self, service_id: str, status: str) -> None:
        """更新服务状态"""
        await self.execute(
            "UPDATE services SET status = $1, updated_at = NOW() WHERE service_id = $2",
            status,
            service_id,
        )
        # 使缓存失效
        if self.redis and self.redis.enabled:
            await self.redis.delete(f"service:{service_id}")
