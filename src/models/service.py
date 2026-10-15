from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from ai_gateway_core.enums import ContentType, InvocationMode, ServiceType, TransportType


@dataclass
class ServiceRateLimitConfig:
    """服务级别限流配置"""

    enabled: bool = False
    requests: int = 100  # 请求数量
    window: int = 60  # 时间窗口（秒）
    burst: int = 0  # 突发容量
    strategy: str = "sliding_window"  # sliding_window, fixed_window, token_bucket


@dataclass
class ServiceAuthConfig:
    """服务级别鉴权配置"""

    enabled: bool = False  # 是否启用服务级别鉴权
    require_auth: bool = True  # 是否必须鉴权
    allowed_roles: list[str] = field(default_factory=list)  # 允许的角色
    allowed_api_keys: list[str] = field(default_factory=list)  # 允许的 API Key 前缀
    public: bool = False  # 是否公开（无需鉴权）


@dataclass
class ServiceCacheConfig:
    """服务级别缓存配置"""

    enabled: bool = False
    ttl: int = 300  # 缓存过期时间（秒）
    semantic_cache: bool = False  # 是否启用语义缓存


@dataclass
class ServicePriorityConfig:
    """服务优先级与调度配置"""

    priority: int = 5  # 1-10，数字越大优先级越高
    weight: int = 1  # 负载均衡权重
    max_queue_size: int = 100  # 最大排队数量


@dataclass
class ServiceCapacityConfig:
    """服务容量与并发预算配置"""

    upstream_group: str | None = None
    concurrency_limit: int | None = None
    queue_max: int = 16
    queue_timeout_ms: int = 3000
    enforced: bool = True


@dataclass
class ServiceConfig:
    """服务级别综合配置"""

    rate_limit: ServiceRateLimitConfig = field(default_factory=ServiceRateLimitConfig)
    auth: ServiceAuthConfig = field(default_factory=ServiceAuthConfig)
    cache: ServiceCacheConfig = field(default_factory=ServiceCacheConfig)
    priority: ServicePriorityConfig = field(default_factory=ServicePriorityConfig)
    capacity: ServiceCapacityConfig = field(default_factory=ServiceCapacityConfig)


@dataclass
class ServiceDefinition:
    service_id: str
    name: str
    description: str = ""
    version: str = "1.0.0"

    service_type: ServiceType = ServiceType.CUSTOM
    supported_modes: list[InvocationMode] = field(default_factory=lambda: [InvocationMode.SYNC])

    connector_type: TransportType = TransportType.HTTP
    connector_config: dict[str, Any] = field(default_factory=dict)

    input_schema: dict[str, Any] = field(default_factory=dict)
    output_schema: dict[str, Any] = field(default_factory=dict)
    accepted_content_types: list[ContentType] = field(default_factory=list)
    output_content_types: list[ContentType] = field(default_factory=list)

    session_enabled: bool = False
    session_adapter: str | None = None

    timeout: int = 60
    max_retries: int = 3
    retry_delay: float = 1.0

    circuit_breaker_enabled: bool = True
    failure_threshold: int = 5
    recovery_timeout: int = 30

    # 保留原有字段兼容性
    rate_limit: dict[str, Any] | None = None
    concurrency_limit: int | None = None

    status: str = "active"
    tags: list[str] = field(default_factory=list)
    metadata: dict[str, Any] = field(default_factory=dict)

    async_config: dict[str, Any] | None = None

    # 新增：服务级别配置
    service_config: ServiceConfig | None = None

    def get_service_config(self) -> ServiceConfig:
        """获取服务配置，如果没有则返回默认配置"""
        if self.service_config is None:
            self.service_config = ServiceConfig()
        return self.service_config
