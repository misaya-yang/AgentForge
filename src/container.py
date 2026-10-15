"""
依赖注入容器

集中管理所有组件的创建和生命周期，提供：
- 类型安全的依赖获取
- 基于配置的实现选择（内存 vs 数据库）
- 组件生命周期管理（初始化、关闭）
- 测试友好的依赖替换
"""

from __future__ import annotations

import asyncio
import os
from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any, Generic, TypeVar

from ai_gateway_core.logging import get_logger

from .config.settings import Settings

logger = get_logger(__name__)

T = TypeVar("T")


class Provider(Generic[T]):
    """
    依赖提供者

    延迟创建依赖实例，支持单例模式。
    """

    def __init__(
        self,
        factory: Callable[..., T],
        singleton: bool = True,
    ):
        self.factory = factory
        self.singleton = singleton
        self._instance: T | None = None
        self._lock = asyncio.Lock()

    async def get(self, **kwargs) -> T:
        """获取实例"""
        if self.singleton:
            if self._instance is None:
                async with self._lock:
                    if self._instance is None:
                        result = self.factory(**kwargs)
                        if asyncio.iscoroutine(result):
                            result = await result
                        self._instance = result
            return self._instance
        else:
            result = self.factory(**kwargs)
            if asyncio.iscoroutine(result):
                result = await result
            return result

    def get_sync(self, **kwargs) -> T:
        """同步获取实例（用于非异步上下文）"""
        if self.singleton and self._instance is not None:
            return self._instance

        result = self.factory(**kwargs)
        if asyncio.iscoroutine(result):
            raise RuntimeError("Cannot use get_sync with async factory")

        if self.singleton:
            self._instance = result

        return result

    def override(self, instance: T) -> None:
        """覆盖实例（用于测试）"""
        self._instance = instance

    def reset(self) -> None:
        """重置实例"""
        self._instance = None


@dataclass
class Container:
    """
    依赖注入容器

    管理所有应用组件的创建和生命周期。

    Usage:
        container = Container(settings)
        await container.initialize()

        registry = container.service_registry
        dispatcher = container.dispatcher

        await container.shutdown()
    """

    settings: Settings
    _providers: dict[str, Provider] = field(default_factory=dict, init=False)
    _initialized: bool = field(default=False, init=False)

    def __post_init__(self):
        """注册所有组件提供者"""
        self._providers = {}
        self._register_providers()

    def _register_providers(self) -> None:
        """注册组件提供者"""

        # ========== 基础设施层 ==========

        # 数据库存储
        self._providers["database"] = Provider(self._create_database, singleton=True)

        # Redis 缓存
        self._providers["redis"] = Provider(self._create_redis, singleton=True)
        self._providers["capacity_resolver"] = Provider(self._create_capacity_resolver, singleton=True)
        self._providers["admission_controller"] = Provider(self._create_admission_controller, singleton=True)

        # ========== 存储层 ==========

        # 注册存储
        self._providers["registry_storage"] = Provider(
            self._create_registry_storage, singleton=True
        )

        # 任务存储
        self._providers["task_storage"] = Provider(self._create_task_storage, singleton=True)

        # ========== 服务层 ==========

        # 服务注册表
        self._providers["service_registry"] = Provider(
            self._create_service_registry, singleton=True
        )

        # 会话管理器
        self._providers["session_manager"] = Provider(self._create_session_manager, singleton=True)

        # 任务管理器
        self._providers["task_manager"] = Provider(self._create_task_manager, singleton=True)

        # 任务队列
        self._providers["task_queue"] = Provider(self._create_task_queue, singleton=True)

        # 任务工作者
        self._providers["task_worker"] = Provider(self._create_task_worker, singleton=True)

        # 记忆服务
        self._providers["memory_service"] = Provider(self._create_memory_service, singleton=True)

        # ========== 核心层 ==========

        # RBAC
        self._providers["rbac"] = Provider(self._create_rbac, singleton=True)

        # 请求验证器
        self._providers["validator"] = Provider(self._create_validator, singleton=True)

        # 限流器
        self._providers["rate_limiter"] = Provider(self._create_rate_limiter, singleton=True)

        # 网关调度器
        self._providers["dispatcher"] = Provider(self._create_dispatcher, singleton=True)

        # 健康监控
        self._providers["health_monitor"] = Provider(self._create_health_monitor, singleton=True)

        # 负载均衡器
        self._providers["load_balancer"] = Provider(self._create_load_balancer, singleton=True)

        # ========== LangGraph 相关 ==========

        # LangGraph 代理
        self._providers["langgraph_proxy"] = Provider(self._create_langgraph_proxy, singleton=True)

        # 多维度限流器
        self._providers["multi_rate_limiter"] = Provider(
            self._create_multi_rate_limiter, singleton=True
        )

        # 用户解析器
        self._providers["user_resolver"] = Provider(self._create_user_resolver, singleton=True)

        # ========== 透明代理相关 ==========

        # 代理配置加载器
        self._providers["proxy_config_loader"] = Provider(
            self._create_proxy_config_loader, singleton=True
        )

        # 计费拦截器
        self._providers["billing_interceptor"] = Provider(
            self._create_billing_interceptor, singleton=True
        )

        # 上下文注入器
        self._providers["context_injector"] = Provider(
            self._create_context_injector, singleton=True
        )

        # 透明代理
        self._providers["transparent_proxy"] = Provider(
            self._create_transparent_proxy, singleton=True
        )

    # ========== 工厂方法 ==========

    def _create_database(self):
        """创建数据库存储"""
        from .persistence.database import DatabaseStorage

        auto_init = getattr(self.settings.database, "auto_init", False)
        bootstrap_admin_password_hash = None
        bootstrap_admin_password = os.environ.get("DEFAULT_USER_PASSWORD", "")
        if auto_init:
            raise RuntimeError(
                "GATEWAY_DATABASE__AUTO_INIT is retired; run "
                "`python -m database.authority migrate` before Gateway startup"
            )
        if bootstrap_admin_password:
            # Hash in the application process so the plaintext bootstrap
            # password never crosses the database connection or appears in SQL.
            from .core.auth.password import hash_password

            bootstrap_admin_password_hash = hash_password(bootstrap_admin_password)

        return DatabaseStorage(
            dsn=self.settings.database.dsn,
            enabled=self.settings.database.enabled,
            auto_init=False,
            bootstrap_admin_password_hash=bootstrap_admin_password_hash,
            permission_cache_ttl_seconds=getattr(
                self.settings.database, "permission_cache_ttl_seconds", 60
            ),
            pool_min_size=getattr(self.settings.database, "pool_min_size", 2),
            pool_max_size=getattr(self.settings.database, "pool_max_size", 10),
            api_key_usage_flush_interval_seconds=getattr(
                self.settings.database, "api_key_usage_flush_interval_seconds", 2
            ),
            api_key_usage_flush_batch_size=getattr(
                self.settings.database, "api_key_usage_flush_batch_size", 100
            ),
        )

    def _create_redis(self):
        """创建 Redis 存储"""
        from .persistence.redis import RedisStorage

        return RedisStorage(
            url=self.settings.redis.url,
            enabled=self.settings.redis.enabled,
        )

    def _create_capacity_resolver(self):
        from .core.gateway.capacity import CapacityResolver
        return CapacityResolver()

    def _create_admission_controller(self):
        from .core.gateway.admission import CapacityAdmissionController
        from .proxy.transparent_proxy import _load_shedder_from_env

        return CapacityAdmissionController(
            redis_client=self.redis.get_native_client(), load_shedder=_load_shedder_from_env(),
        )

    def _create_registry_storage(self):
        """创建注册存储"""
        if self.settings.database.enabled:
            from .services.registry.database_storage import DatabaseRegistryStorage

            database = self._providers["database"].get_sync()
            redis = self._providers["redis"].get_sync()
            logger.info("使用 PostgreSQL 作为服务注册存储后端")
            return DatabaseRegistryStorage(database, redis)
        else:
            from .services.registry.service_registry import MemoryRegistryStorage

            logger.info("使用内存作为服务注册存储后端")
            return MemoryRegistryStorage()

    def _create_task_storage(self):
        """创建任务存储"""
        if self.settings.database.enabled:
            from .services.task.database_task_storage import DatabaseTaskStorage

            database = self._providers["database"].get_sync()
            redis = self._providers["redis"].get_sync()
            logger.info("使用 PostgreSQL 作为任务存储后端")
            return DatabaseTaskStorage(database, redis)
        else:
            from .services.task.task_manager import MemoryTaskStorage

            logger.info("使用内存作为任务存储后端")
            return MemoryTaskStorage()

    def _create_service_registry(self):
        """创建服务注册表"""
        from .adapters.comfyui import ComfyUIAdapter
        from .adapters.dify import DifyAdapter
        from .adapters.generic_rest import GenericRESTAdapter
        from .adapters.langgraph import LangGraphAdapter
        from .adapters.openai import OpenAIAdapter
        from .adapters.stable_diffusion import Text2ImageAdapter
        from .adapters.tts import TTSAdapter
        from .adapters.whisper import WhisperAdapter
        from .services.registry.service_registry import ServiceRegistry

        storage = self._providers["registry_storage"].get_sync()
        registry = ServiceRegistry(storage)

        # 注册所有适配器
        adapters = [
            ("langgraph", LangGraphAdapter),
            ("openai", OpenAIAdapter),
            ("stable_diffusion", Text2ImageAdapter),
            ("comfyui", ComfyUIAdapter),
            ("whisper", WhisperAdapter),
            ("tts", TTSAdapter),
            ("dify", DifyAdapter),
            ("generic_rest", GenericRESTAdapter),
        ]

        for name, adapter_cls in adapters:
            registry.register_adapter(name, adapter_cls)

        logger.info(f"已注册 {len(adapters)} 个适配器")
        return registry

    def _create_session_manager(self):
        """创建会话管理器"""
        if self.settings.database.enabled:
            from .services.session.database_session_manager import DatabaseSessionManager

            database = self._providers["database"].get_sync()
            redis = self._providers["redis"].get_sync()
            logger.info("使用 PostgreSQL 作为会话存储后端")
            return DatabaseSessionManager(
                database,
                redis,
                default_session_ttl=self.settings.session.authenticated_ttl_seconds,
                anonymous_session_ttl=self.settings.session.anonymous_ttl_seconds,
            )
        else:
            from .services.session.session_manager import SessionManager

            logger.info("使用内存作为会话存储后端")
            return SessionManager(
                default_session_ttl=self.settings.session.authenticated_ttl_seconds,
                anonymous_session_ttl=self.settings.session.anonymous_ttl_seconds,
            )

    def _create_task_queue(self):
        """创建任务队列"""
        from .services.task.task_queue import MemoryTaskQueue

        return MemoryTaskQueue()

    def _create_task_manager(self):
        """创建任务管理器"""
        from .services.task.task_manager import TaskManager

        storage = self._providers["task_storage"].get_sync()
        queue = self._providers["task_queue"].get_sync()
        return TaskManager(
            storage=storage,
            queue=queue,
            callback_timeout=30.0,
            callback_retries=3,
            callback_retry_delay=1.0,
        )

    def _create_task_worker(self):
        """创建任务工作者"""
        from .services.task.task_worker import TaskWorker

        queue = self._providers["task_queue"].get_sync()
        storage = self._providers["task_storage"].get_sync()
        dispatcher = self._providers["dispatcher"].get_sync()
        task_manager = self._providers["task_manager"].get_sync()
        return TaskWorker(queue, storage, dispatcher, task_manager)

    def _create_memory_service(self):
        """创建记忆服务"""
        from ai_gateway_core.memory import MemoryService

        database = self._providers["database"].get_sync()
        return MemoryService(database)

    def _create_rbac(self):
        """创建 RBAC"""
        from .core.auth.rbac import RBAC

        return RBAC(self.settings.rbac.roles)

    def _create_validator(self):
        """创建请求验证器"""
        from .core.gateway.validator import RequestValidator

        rbac = self._providers["rbac"].get_sync()
        return RequestValidator(rbac)

    def _create_rate_limiter(self):
        """创建限流器"""
        from .core.gateway.rate_limiter import RateLimit, RateLimitConfig, RateLimiter

        def parse_rate_limit(data):
            if not data:
                return None
            return RateLimit(**data)

        rl = self.settings.rate_limits
        config = RateLimitConfig(
            global_limit=parse_rate_limit(rl.global_limit),
            tenant_limits={k: parse_rate_limit(v) for k, v in rl.tenant_limits.items()}
            if rl.tenant_limits
            else None,
            user_limits={k: parse_rate_limit(v) for k, v in rl.user_limits.items()}
            if rl.user_limits
            else None,
            service_limits={k: parse_rate_limit(v) for k, v in rl.service_limits.items()}
            if rl.service_limits
            else None,
            ip_limits=parse_rate_limit(rl.ip_limits),
        )
        return RateLimiter(config)

    def _create_dispatcher(self):
        """创建网关调度器"""
        from .core.gateway.dispatcher import GatewayDispatcher

        return GatewayDispatcher(
            admission_controller=self.admission_controller,
            capacity_resolver=self.capacity_resolver,
            registry=self._providers["service_registry"].get_sync(),
            validator=self._providers["validator"].get_sync(),
            rate_limiter=self._providers["rate_limiter"].get_sync(),
            rbac=self._providers["rbac"].get_sync(),
            task_manager=self._providers["task_manager"].get_sync(),
            session_manager=self._providers["session_manager"].get_sync(),
        )

    def _create_health_monitor(self):
        """创建健康监控"""
        from .services.registry.health_monitor import HealthMonitor

        registry = self._providers["service_registry"].get_sync()
        return HealthMonitor(
            registry=registry,
            check_interval=self.settings.health_check_interval,
        )

    def _create_load_balancer(self):
        """创建负载均衡器"""
        from .services.registry.load_balancer import LoadBalancer

        return LoadBalancer(strategy=self.settings.load_balancer.strategy)

    def _create_langgraph_proxy(self):
        """创建 LangGraph 代理"""
        if not self.settings.langgraph.enabled:
            return None

        from .adapters.langgraph_proxy import (
            LangGraphInstance,
            LangGraphLoadBalancer,
            LangGraphProxy,
            LoadBalancerConfig,
        )

        # 解析实例
        instances = []

        # 从配置文件加载
        for inst_cfg in self.settings.langgraph.instances:
            instances.append(
                LangGraphInstance(
                    instance_id=inst_cfg.instance_id,
                    url=inst_cfg.url,
                    weight=inst_cfg.weight,
                )
            )

        # 从环境变量加载
        if self.settings.langgraph.instance_urls:
            for idx, url in enumerate(self.settings.langgraph.instance_urls.split(",")):
                url = url.strip()
                if url:
                    instances.append(
                        LangGraphInstance(
                            instance_id=f"langgraph-{idx}",
                            url=url,
                            weight=1,
                        )
                    )

        if not instances:
            return None

        # 创建负载均衡器
        lb_config = LoadBalancerConfig(
            strategy=self.settings.langgraph.load_balancer_strategy,
            health_check_interval=self.settings.langgraph.health_check_interval,
            instances=instances,
        )
        load_balancer = LangGraphLoadBalancer(lb_config)

        # 创建代理
        redis = self._providers["redis"].get_sync()
        auth_token = self.settings.langgraph.auth_token

        if not auth_token:
            auth_token = os.environ.get("LANGGRAPH_AUTH_TOKEN", "")
        if not auth_token:
            logger.warning("LANGGRAPH_AUTH_TOKEN not configured — LangGraph proxy may fail authentication")

        proxy = LangGraphProxy(
            load_balancer=load_balancer,
            redis_client=redis if redis.enabled else None,
            auth_token=auth_token,
        )

        logger.info(f"LangGraph 代理已初始化，{len(instances)} 个实例")
        return proxy

    def _create_multi_rate_limiter(self):
        """创建多维度限流器 — always enabled for AOP enforcement"""

        from .core.gateway.multi_dimension_rate_limiter import (
            MultiDimensionRateLimiter,
            TierLimit,
            create_rate_limit_config,
        )

        tier_limits = {}
        lg_tier_limits = getattr(self.settings.langgraph, "tier_limits", {}) if self.settings.langgraph else {}
        for tier, limits in lg_tier_limits.items():
            tier_limits[tier] = TierLimit(
                requests=limits.get("requests", 10),
                window=limits.get("window", 60),
                burst=limits.get("burst", 0),
            )

        redis = self._providers["redis"].get_sync()
        # MultiDimensionRateLimiter needs the native redis client, not the RedisStorage wrapper
        redis_client = redis.get_native_client() if redis else None
        return MultiDimensionRateLimiter(
            config=create_rate_limit_config(user_tier_overrides=tier_limits),
            redis_client=redis_client,
        )

    def _create_user_resolver(self):
        """创建用户解析器"""
        from .core.auth.user_resolver import UserResolver, UserResolverConfig

        return UserResolver(
            UserResolverConfig(
                jwt_enabled=self.settings.authentication.jwt.enabled,
                jwt_secret=self.settings.authentication.jwt.secret,
                jwt_algorithms=self.settings.authentication.jwt.algorithms,
                jwt_audience=self.settings.authentication.jwt.audience,
                jwt_issuer=self.settings.authentication.jwt.issuer,
                api_key_enabled=self.settings.authentication.api_key.enabled,
                api_key_header=self.settings.authentication.api_key.header_name,
            )
        )

    # ========== 透明代理工厂方法 ==========

    def _create_proxy_config_loader(self):
        """创建代理配置加载器"""
        from .proxy.config_loader import ProxyConfigLoader

        database = self._providers["database"].get_sync()
        redis = self._providers["redis"].get_sync()

        return ProxyConfigLoader(
            database=database if database.enabled else None,
            redis=redis if redis.enabled else None,
            cache_ttl=self.settings.proxy.config_cache_ttl,
        )

    def _create_billing_interceptor(self):
        """创建计费拦截器"""
        if not self.settings.proxy.billing_enabled:
            return None

        from .proxy.billing_interceptor import BillingInterceptor

        redis = self._providers["redis"].get_sync()

        # 注意：realtime_metrics 使用延迟获取（在 BillingInterceptor 内部动态获取）
        # 因为它在 main.py 启动过程中稍后初始化
        return BillingInterceptor(
            redis_client=redis if redis.enabled else None,
            realtime_metrics=None,  # 使用延迟获取，见 BillingInterceptor
            buffer_size=self.settings.proxy.billing_buffer_size,
            flush_interval=self.settings.proxy.billing_flush_interval,
        )

    def _create_context_injector(self):
        """创建上下文注入器"""
        from .proxy.context_injector import ContextInjector

        return ContextInjector(
            inject_user_info=self.settings.proxy.inject_user_info,
            inject_request_info=self.settings.proxy.inject_request_info,
            forward_auth=self.settings.proxy.forward_auth,
            forward_all_headers=self.settings.proxy.forward_all_headers,
            custom_headers=self.settings.proxy.custom_headers,
        )

    def _create_transparent_proxy(self):
        """创建透明代理"""
        if not self.settings.proxy.enabled:
            return None

        from .proxy.transparent_proxy import TransparentProxy

        config_loader = self._providers["proxy_config_loader"].get_sync()
        context_injector = self._providers["context_injector"].get_sync()
        billing_interceptor = self._providers["billing_interceptor"].get_sync()

        proxy = TransparentProxy(
            admission_controller=self.admission_controller,
            capacity_resolver=self.capacity_resolver,
            config_loader=config_loader,
            context_injector=context_injector,
            billing_interceptor=billing_interceptor,
            default_timeout=self.settings.proxy.timeout_read,
            health_check_timeout=self.settings.proxy.health_check_timeout,
            availability_cache_ttl=self.settings.proxy.availability_cache_ttl_seconds,
            default_concurrency_limit=self.settings.proxy.default_concurrency_limit,
            default_streaming_concurrency_limit=(
                self.settings.proxy.default_streaming_concurrency_limit
            ),
            default_non_streaming_concurrency_limit=(
                self.settings.proxy.default_non_streaming_concurrency_limit
            ),
            default_concurrency_queue_timeout=(
                self.settings.proxy.default_concurrency_queue_timeout
            ),
            client_max_connections=self.settings.proxy.client_max_connections,
            client_max_keepalive_connections=self.settings.proxy.client_max_keepalive_connections,
            client_keepalive_expiry=self.settings.proxy.client_keepalive_expiry,
        )

        logger.info("透明代理已初始化")
        return proxy

    # ========== 生命周期管理 ==========

    async def initialize(self) -> None:
        """
        初始化容器

        连接数据库、Redis 等外部资源。
        """
        if self._initialized:
            return

        logger.info("正在初始化容器...")

        # 连接数据库
        if self.settings.database.enabled:
            from .persistence.schema_compatibility import require_compatible_schema

            database = await self._providers["database"].get()
            try:
                # Validate this Gateway's window before the shared storage can
                # open a pool, start its usage flusher, or bootstrap a password.
                await require_compatible_schema(self.settings.database.dsn)
                await database.connect()
                logger.info("PostgreSQL 数据库已连接")
            except BaseException:
                # Startup exceptions (including cancellation) must not leave a
                # partially connected pool that SELECT 1 could mark ready.
                await database.close()
                raise

        # 连接 Redis
        if self.settings.redis.enabled:
            redis = await self._providers["redis"].get()
            try:
                await redis.connect()
                if await redis.ping():
                    logger.info("Redis 已连接")
                else:
                    logger.warning("Redis 连接失败：ping 无响应")
            except Exception as e:
                logger.error(f"Redis 连接失败: {e}")

        if self.settings.proxy.enabled:
            try:
                transparent_proxy = await self._providers["transparent_proxy"].get()
                if transparent_proxy:
                    await transparent_proxy.refresh_all_service_health()
                    logger.info("透明代理上游健康状态已预热")
            except Exception as e:
                logger.warning(f"透明代理健康预热失败: {e}")

        self._initialized = True
        logger.info("容器初始化完成")

    async def shutdown(self) -> None:
        """
        关闭容器

        断开所有外部连接，清理资源。
        """
        if not self._initialized:
            return

        logger.info("正在关闭容器...")

        # 关闭任务管理器（释放 HTTP client 连接）
        task_manager = self._providers.get("task_manager")
        if task_manager and task_manager._instance:
            await task_manager._instance.close()

        # 停止任务工作者
        task_worker = self._providers.get("task_worker")
        if task_worker and task_worker._instance:
            await task_worker._instance.stop()

        # 停止健康监控
        health_monitor = self._providers.get("health_monitor")
        if health_monitor and health_monitor._instance:
            await health_monitor._instance.stop()

        # 关闭 LangGraph 代理
        langgraph_proxy = self._providers.get("langgraph_proxy")
        if langgraph_proxy and langgraph_proxy._instance:
            await langgraph_proxy._instance.close()

        # 关闭透明代理
        transparent_proxy = self._providers.get("transparent_proxy")
        if transparent_proxy and transparent_proxy._instance:
            await transparent_proxy._instance.close()

        # 停止计费拦截器
        billing_interceptor = self._providers.get("billing_interceptor")
        if billing_interceptor and billing_interceptor._instance:
            await billing_interceptor._instance.stop()

        # 关闭数据库连接
        database = self._providers.get("database")
        if database and database._instance:
            await database._instance.close()

        # 关闭 Redis 连接
        redis = self._providers.get("redis")
        if redis and redis._instance:
            await redis._instance.close()

        self._initialized = False
        logger.info("容器已关闭")

    # ========== 便捷访问属性 ==========

    @property
    def database(self):
        """获取数据库存储"""
        return self._providers["database"].get_sync()

    @property
    def redis(self):
        """获取 Redis 存储"""
        return self._providers["redis"].get_sync()

    @property
    def admission_controller(self):
        return self._providers["admission_controller"].get_sync()

    @property
    def capacity_resolver(self):
        return self._providers["capacity_resolver"].get_sync()

    @property
    def service_registry(self):
        """获取服务注册表"""
        return self._providers["service_registry"].get_sync()

    @property
    def session_manager(self):
        """获取会话管理器"""
        return self._providers["session_manager"].get_sync()

    @property
    def task_manager(self):
        """获取任务管理器"""
        return self._providers["task_manager"].get_sync()

    @property
    def dispatcher(self):
        """获取网关调度器"""
        return self._providers["dispatcher"].get_sync()

    @property
    def health_monitor(self):
        """获取健康监控"""
        return self._providers["health_monitor"].get_sync()

    @property
    def task_worker(self):
        """获取任务工作者"""
        return self._providers["task_worker"].get_sync()

    @property
    def memory_service(self):
        """获取记忆服务"""
        return self._providers["memory_service"].get_sync()

    @property
    def load_balancer(self):
        """获取负载均衡器"""
        return self._providers["load_balancer"].get_sync()

    @property
    def langgraph_proxy(self):
        """获取 LangGraph 代理"""
        return self._providers["langgraph_proxy"].get_sync()

    @property
    def multi_rate_limiter(self):
        """获取多维度限流器"""
        return self._providers["multi_rate_limiter"].get_sync()

    @property
    def user_resolver(self):
        """获取用户解析器"""
        return self._providers["user_resolver"].get_sync()

    @property
    def proxy_config_loader(self):
        """获取代理配置加载器"""
        return self._providers["proxy_config_loader"].get_sync()

    @property
    def billing_interceptor(self):
        """获取计费拦截器"""
        return self._providers["billing_interceptor"].get_sync()

    @property
    def context_injector(self):
        """获取上下文注入器"""
        return self._providers["context_injector"].get_sync()

    @property
    def transparent_proxy(self):
        """获取透明代理"""
        return self._providers["transparent_proxy"].get_sync()

    # ========== 测试支持 ==========

    def override(self, name: str, instance: Any) -> None:
        """
        覆盖组件实例（用于测试）

        Args:
            name: 组件名称
            instance: 替换的实例
        """
        if name in self._providers:
            self._providers[name].override(instance)

    def reset(self, name: str | None = None) -> None:
        """
        重置组件实例

        Args:
            name: 组件名称，为 None 时重置所有组件
        """
        if name:
            if name in self._providers:
                self._providers[name].reset()
        else:
            for provider in self._providers.values():
                provider.reset()


# 全局容器实例
_container: Container | None = None


def get_container() -> Container:
    """获取全局容器实例"""
    global _container
    if _container is None:
        raise RuntimeError("Container not initialized. Call create_container() first.")
    return _container


def create_container(settings: Settings) -> Container:
    """创建并设置全局容器实例"""
    global _container
    _container = Container(settings)
    return _container
