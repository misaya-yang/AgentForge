"""
服务配置加载器

从数据库或内存缓存中加载代理服务配置。
支持：
- 动态获取 upstream URL 和 assistant ID
- 配置缓存和自动刷新
- 多实例负载均衡配置
"""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass, field
from typing import Any

from ai_gateway_core.logging import get_logger

logger = get_logger(__name__)


def _normalize_url(url: Any) -> str:
    """Normalize URL values for consistent routing decisions."""
    if url is None:
        return ""
    value = str(url).strip()
    if not value:
        return ""
    return value.rstrip("/")


@dataclass
class ProxyServiceConfig:
    """代理服务配置"""

    service_id: str
    service_name: str

    # 上游配置
    upstream_url: str  # e.g., http://langgraph:8123
    upstream_urls: list[str] = field(default_factory=list)  # 多实例 URL 列表

    # 可选配置
    assistant_id: str | None = None  # LangGraph assistant_id
    graph_id: str | None = None  # LangGraph graph_id (graph name)
    default_model: str | None = None
    default_provider: str | None = None
    model_override: dict[str, Any] | None = None
    path_rewrite: str | None = None  # 路径重写前缀，如 /api/v1
    strip_prefix: bool = True  # 是否去除 /proxy/{service_name} 前缀

    # 超时配置
    timeout_connect: float = 5.0
    timeout_read: float = 300.0
    timeout_write: float = 60.0
    timeout_pool: float = 60.0

    # 认证配置
    auth_token: str | None = None  # 内部认证 token
    forward_auth: bool = True  # 是否转发原始 Authorization 头

    # 限流配置
    rate_limit_enabled: bool = False
    rate_limit_requests: int = 100
    rate_limit_window: int = 60

    # 响应缓存配置
    cache_enabled: bool = False
    cache_ttl: int = 300

    # 负载均衡
    load_balance_strategy: str = "round_robin"  # round_robin | least_connections | random
    concurrency_limit: int = 32
    streaming_concurrency_limit: int | None = None
    non_streaming_concurrency_limit: int | None = None
    concurrency_queue_timeout: float | None = None
    capacity_config: dict[str, Any] = field(default_factory=dict)
    max_connections: int | None = None
    streaming_max_connections: int | None = None
    max_keepalive_connections: int | None = None
    keepalive_expiry: float | None = None
    health_check_timeout: float = 5.0

    # 元数据
    metadata: dict[str, Any] = field(default_factory=dict)

    # 状态
    enabled: bool = True
    availability_status: str = "unknown"
    last_health_check_at: float | None = None
    last_health_error: str | None = None

    def get_upstream_urls(self) -> list[str]:
        """获取所有上游 URL"""
        urls = list(self.upstream_urls) if self.upstream_urls else []
        if self.upstream_url and self.upstream_url not in urls:
            urls.insert(0, self.upstream_url)
        return urls


@dataclass
class CachedConfig:
    """缓存的配置项"""

    config: ProxyServiceConfig
    cached_at: float
    ttl: float = 60.0  # 缓存 TTL（秒）

    def is_expired(self) -> bool:
        return time.time() - self.cached_at > self.ttl


class ProxyConfigLoader:
    """
    代理配置加载器

    从数据库加载服务配置，支持缓存和自动刷新。
    """

    # 默认缓存 TTL
    DEFAULT_CACHE_TTL = 60.0

    def __init__(
        self,
        database=None,
        redis=None,
        cache_ttl: float = DEFAULT_CACHE_TTL,
    ):
        self.database = database
        self.redis = redis
        self.cache_ttl = cache_ttl

        # 本地缓存
        self._cache: dict[str, CachedConfig] = {}
        self._lock = asyncio.Lock()

    async def get_config(self, service_name: str) -> ProxyServiceConfig | None:
        """
        获取服务配置

        Args:
            service_name: 服务名称（用于路由匹配）

        Returns:
            ProxyServiceConfig 或 None（服务不存在）
        """
        # 检查缓存
        cached = self._cache.get(service_name)
        if cached and not cached.is_expired():
            return cached.config

        # 从数据库加载
        config = await self._load_from_database(service_name)

        if config:
            # 更新缓存
            self._cache[service_name] = CachedConfig(
                config=config,
                cached_at=time.time(),
                ttl=self.cache_ttl,
            )

        return config

    async def _load_from_database(self, service_name: str) -> ProxyServiceConfig | None:
        """从数据库加载配置"""
        if not self.database or not getattr(self.database, "enabled", False):
            logger.debug(f"Database not available, cannot load config for {service_name}")
            return None

        try:
            # 查询 services 表
            # service_name 可以匹配 service_id 或 name
            query = """
                SELECT
                    service_id, name, connector_config, service_config,
                    timeout, status, metadata
                FROM services
                WHERE (service_id = $1 OR name = $1)
                  AND status = 'active'
                LIMIT 1
            """

            row = await self.database.fetchrow(query, service_name)

            if not row:
                logger.debug(f"Service not found: {service_name}")
                return None

            return self._parse_service_row(row)

        except Exception as e:
            logger.error(f"Failed to load service config for {service_name}: {e}")
            return None

    def _parse_service_row(self, row) -> ProxyServiceConfig:
        """解析数据库行为配置对象"""
        # 将 asyncpg.Record 转换为 dict
        row_dict = dict(row) if hasattr(row, "keys") else row

        # 获取 JSONB 字段，处理可能的字符串情况
        def get_json_field(field_name: str) -> dict:
            value = row_dict.get(field_name)
            if value is None:
                return {}
            if isinstance(value, str):
                import json

                try:
                    return json.loads(value)
                except (json.JSONDecodeError, TypeError):
                    return {}
            return value if isinstance(value, dict) else {}

        connector_config = get_json_field("connector_config")
        service_config = get_json_field("service_config")
        metadata = get_json_field("metadata")

        # 提取并规范化上游 URL
        base_url = _normalize_url(connector_config.get("base_url"))
        upstream_url = _normalize_url(connector_config.get("upstream_url"))
        proxy_mode = str(connector_config.get("proxy_mode") or "")

        # 修复历史不一致数据：base_url 与 upstream_url 偏离导致代理走错目标。
        if base_url and upstream_url and base_url != upstream_url and proxy_mode == "transparent":
            logger.warning(
                "Detected mismatched LangGraph proxy URLs for service %s; "
                "using base_url as canonical target (base=%s, upstream=%s)",
                row_dict.get("service_id", ""),
                base_url,
                upstream_url,
            )
            upstream_url = base_url

        upstream_url = upstream_url or base_url

        # 提取多实例 URL
        upstream_urls = connector_config.get("upstream_urls") or []
        if connector_config.get("instance_urls"):
            # 兼容旧格式
            urls_str = connector_config["instance_urls"]
            if isinstance(urls_str, str):
                upstream_urls = [u.strip() for u in urls_str.split(",") if u.strip()]
        normalized_urls: list[str] = []
        for url in upstream_urls:
            normalized = _normalize_url(url)
            if normalized:
                normalized_urls.append(normalized)
        upstream_urls = normalized_urls

        # 提取限流配置
        rate_limit = service_config.get("rate_limit") or {}
        cache_config = service_config.get("cache") or {}
        proxy_runtime = service_config.get("proxy_runtime") or {}
        capacity_config = service_config.get("capacity") or metadata.get("capacity") or {}
        if not isinstance(capacity_config, dict):
            capacity_config = {}

        def _pick_numeric(default: Any, *values: Any) -> Any:
            for value in values:
                if value is None:
                    continue
                return value
            return default

        def _pick_identity(*keys: str) -> str | None:
            for key in keys:
                value = connector_config.get(key)
                if value is None:
                    value = metadata.get(key)
                if value is None:
                    continue
                normalized = str(value).strip()
                if normalized:
                    return normalized
            return None

        default_model = _pick_identity("default_model", "model", "model_id", "llm_model")
        default_provider = _pick_identity(
            "default_provider",
            "provider",
            "provider_id",
            "llm_provider",
            "vendor",
        )

        return ProxyServiceConfig(
            service_id=row_dict.get("service_id", ""),
            service_name=row_dict.get("name", ""),
            upstream_url=upstream_url,
            upstream_urls=upstream_urls,
            assistant_id=connector_config.get("assistant_id"),
            graph_id=connector_config.get("graph_id"),
            default_model=default_model,
            default_provider=default_provider,
            model_override=(
                connector_config.get("model_override")
                if isinstance(connector_config.get("model_override"), dict)
                else None
            ),
            path_rewrite=connector_config.get("path_rewrite"),
            strip_prefix=connector_config.get("strip_prefix", True),
            timeout_connect=connector_config.get("timeout_connect", 5.0),
            timeout_read=connector_config.get("timeout_read", 300.0),
            timeout_write=connector_config.get("timeout_write", 60.0),
            timeout_pool=connector_config.get("timeout_pool", 60.0),
            auth_token=connector_config.get("auth_token"),
            forward_auth=connector_config.get("forward_auth", True),
            rate_limit_enabled=rate_limit.get("enabled", False),
            rate_limit_requests=rate_limit.get("requests", 100),
            rate_limit_window=rate_limit.get("window", 60),
            cache_enabled=cache_config.get("enabled", False),
            cache_ttl=cache_config.get("ttl", 300),
            load_balance_strategy=connector_config.get("load_balance_strategy", "round_robin"),
            concurrency_limit=int(
                _pick_numeric(32, proxy_runtime.get("concurrency_limit"), connector_config.get("concurrency_limit"))
            ),
            streaming_concurrency_limit=proxy_runtime.get(
                "streaming_concurrency_limit",
                connector_config.get("streaming_concurrency_limit"),
            ),
            non_streaming_concurrency_limit=proxy_runtime.get(
                "non_streaming_concurrency_limit",
                connector_config.get("non_streaming_concurrency_limit"),
            ),
            concurrency_queue_timeout=_pick_numeric(
                None,
                proxy_runtime.get("concurrency_queue_timeout"),
                connector_config.get("concurrency_queue_timeout"),
            ),
            capacity_config=capacity_config,
            max_connections=proxy_runtime.get("max_connections", connector_config.get("max_connections")),
            streaming_max_connections=proxy_runtime.get(
                "streaming_max_connections",
                connector_config.get("streaming_max_connections"),
            ),
            max_keepalive_connections=proxy_runtime.get(
                "max_keepalive_connections",
                connector_config.get("max_keepalive_connections"),
            ),
            keepalive_expiry=proxy_runtime.get("keepalive_expiry", connector_config.get("keepalive_expiry")),
            health_check_timeout=float(
                _pick_numeric(5.0, proxy_runtime.get("health_check_timeout"), connector_config.get("health_check_timeout"))
            ),
            metadata=metadata,
            enabled=row_dict.get("status") == "active",
        )

    def set_config(self, service_name: str, config: ProxyServiceConfig) -> None:
        """
        手动设置配置（用于测试或静态配置）

        Args:
            service_name: 服务名称
            config: 服务配置
        """
        self._cache[service_name] = CachedConfig(
            config=config,
            cached_at=time.time(),
            ttl=float("inf"),  # 永不过期
        )

    def invalidate(self, service_name: str | None = None) -> None:
        """
        使缓存失效

        Args:
            service_name: 服务名称，为 None 时清除所有缓存
        """
        if service_name:
            self._cache.pop(service_name, None)
        else:
            self._cache.clear()

    async def list_services(self) -> list[ProxyServiceConfig]:
        """列出所有启用透明代理的服务"""
        if not self.database or not getattr(self.database, "enabled", False):
            return [c.config for c in self._cache.values()]

        try:
            query = """
                SELECT
                    service_id, name, connector_config, service_config,
                    timeout, status, metadata
                FROM services
                WHERE status = 'active'
                  AND connector_config->>'proxy_mode' = 'transparent'
            """

            rows = await self.database.fetch(query)
            return [self._parse_service_row(row) for row in rows]

        except Exception as e:
            logger.error(f"Failed to list proxy services: {e}")
            return []
