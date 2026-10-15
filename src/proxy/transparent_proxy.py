"""
透明代理核心

使用 httpx.AsyncClient 将原始请求透明转发至目标服务，支持：
- 动态路由转发
- 完美的 SSE 流式传输
- 请求/响应头处理
- 负载均衡
- LangGraph assistant_id 自动注入
- 上游错误透传（4xx/5xx）
"""

from __future__ import annotations

import asyncio
import json
import os
import random
import re
import time
from collections.abc import AsyncIterator, Awaitable, Callable
from dataclasses import dataclass, field
from typing import Any

import httpx
from ai_gateway_core.logging import get_logger

from ..core.gateway.admission import (
    AdaptiveLoadShedder,
    CapacityAdmissionController,
    CapacityRejected,
    _finish_cleanup,
)
from ..core.gateway.capacity import CapacityResolver, service_upstream_group
from ..services.metrics.observability import (
    classify_error_type,
    ensure_duration_breakdown,
    extract_duration_breakdown,
)
from ..services.metrics.redaction import redact_sensitive_text
from ..services.metrics.usage_parser import (
    extract_assistant_id,
    extract_model,
    extract_provider,
    extract_token_usage,
)
from .billing_interceptor import BillingInterceptor, StreamProcessor
from .config_loader import ProxyConfigLoader, ProxyServiceConfig
from .context_injector import ContextInjector, RequestContext
from .response_cache import ResponseCache

logger = get_logger(__name__)


class ProxyQueueTimeoutError(RuntimeError):
    """Raised when a proxy request waits too long for a service slot."""


class _OwnedProxyStream:
    """One demand-driven task owns admission across all downstream reads."""

    def __init__(self, source, close):
        self._source = source
        self._close = close
        self._task = None
        self._close_task = None
        self._ready = asyncio.Event()
        self._demand = asyncio.Event()
        self._result = None
        self._error = None
        self._closing = False

    async def start(self, lease):
        self._task = asyncio.create_task(self._run(lease))
        try:
            await self._ready.wait()
            if self._error is not None:
                raise self._error
        except BaseException:
            await self.aclose()
            raise

    async def _run(self, lease):
        try:
            if lease is not None:
                lease.bind_owner()
            self._ready.set()
            while True:
                await self._demand.wait()
                self._demand.clear()
                try:
                    chunk = await anext(self._source)
                except StopAsyncIteration:
                    return
                self._result.set_result(chunk)
        except BaseException as exc:
            if not (self._closing and isinstance(exc, asyncio.CancelledError)):
                self._error = exc
        finally:
            self._ready.set()
            try:
                try:
                    await self._source.aclose()
                finally:
                    await self._close()
            except BaseException as exc:
                if self._error is None:
                    self._error = exc
            finally:
                self._ready.set()

    def __aiter__(self):
        return self

    async def __anext__(self):
        if self._task.done():
            if self._error is not None:
                raise self._error
            raise StopAsyncIteration
        if self._result is not None:
            raise RuntimeError("Concurrent proxy stream reads are unsupported")
        self._result = asyncio.get_running_loop().create_future()
        self._demand.set()
        try:
            await asyncio.wait({self._result, self._task}, return_when=asyncio.FIRST_COMPLETED)
            if self._task.done():
                if self._error is not None:
                    raise self._error
                raise StopAsyncIteration
            return self._result.result()
        except BaseException:
            await self.aclose()
            raise
        finally:
            self._result = None

    async def wait_closed(self):
        await asyncio.shield(self._task)
        if self._error is not None:
            raise self._error

    async def aclose(self):
        if self._close_task is None:
            async def stop():
                # Let the owner enter its finally before it can be cancelled.
                await self._ready.wait()
                self._closing = True
                if not self._task.done():
                    self._task.cancel()
                await self._task
            self._close_task = asyncio.create_task(stop())
        try:
            await asyncio.shield(self._close_task)
        except asyncio.CancelledError:
            # Forward downstream cancellation to interrupt accounting, then wait
            # for the separately shielded transport/admission cleanup.
            if self._ready.is_set():
                self._task.cancel()
            await _finish_cleanup(self._close_task)
            raise
        if self._error is not None:
            raise self._error


# LangGraph 需要 assistant_id 的路径模式（正则匹配）
LANGGRAPH_ASSISTANT_PATHS = ("/runs", "/runs/stream", "/runs/wait", "/threads/")
_LANGGRAPH_RUN_RE = re.compile(
    r"/(runs|threads/[^/]+/runs)(/stream|/wait)?$", re.IGNORECASE
)

# LangGraph API 操作类型映射（用于限流）
LANGGRAPH_OPERATION_TYPES = {
    # Assistants API
    "GET /assistants": "assistant_list",
    "POST /assistants": "assistant_create",
    "POST /assistants/search": "assistant_list",
    "GET /assistants/": "assistant_read",
    "PATCH /assistants/": "assistant_update",
    "DELETE /assistants/": "assistant_delete",
    # Threads API
    "POST /threads": "thread_create",
    "POST /threads/search": "thread_search",
    "GET /threads/": "thread_read",
    "PATCH /threads/": "thread_update",
    "DELETE /threads/": "thread_delete",
    # Thread State API
    "GET /threads/*/state": "state_read",
    "POST /threads/*/state": "state_update",
    "GET /threads/*/history": "history_read",
    # Runs API
    "POST /threads/*/runs": "run_create",
    "POST /threads/*/runs/stream": "run_stream",
    "POST /threads/*/runs/wait": "run_wait",
    "GET /threads/*/runs": "run_list",
    "GET /threads/*/runs/": "run_read",
    "POST /threads/*/runs/*/cancel": "run_cancel",
    "POST /threads/*/runs/*/join": "run_join",
    # Stateless Runs API
    "POST /runs": "run_create",
    "POST /runs/stream": "run_stream",
    "POST /runs/wait": "run_wait",
    "GET /runs/": "run_read",
    # Store API
    "GET /store/items": "store_read",
    "POST /store/items": "store_write",
    "DELETE /store/items": "store_delete",
    "GET /store/namespaces": "store_list",
    "POST /store/namespaces": "store_list",
    # Crons API
    "GET /crons": "cron_list",
    "POST /crons": "cron_create",
    "GET /crons/": "cron_read",
    "PATCH /crons/": "cron_update",
    "DELETE /crons/": "cron_delete",
    "POST /crons/*/trigger": "cron_trigger",
}


@dataclass
class ProxyRequest:
    """代理请求"""

    # 目标服务
    service_name: str

    # 请求路径（不含 /proxy/{service_name} 前缀）
    path: str

    # HTTP 方法
    method: str = "GET"

    # 请求体
    body: bytes | None = None

    # 查询参数
    query_params: dict[str, Any] = field(default_factory=dict)

    # 请求上下文
    context: RequestContext | None = None

    # 是否期望流式响应
    stream: bool = False

    # Pre-parsed JSON body (avoids duplicate decode in core proxy path)
    parsed_body: dict[str, Any] | None = None

    # Route layer already applied LangGraph run body mutations
    langgraph_body_prepared: bool = False

    # Optional config snapshot from route layer (skips duplicate config load)
    preloaded_config: ProxyServiceConfig | None = None

    # Route-resolved billing hints (override extract_model on redacted snapshots)
    effective_model: str | None = None
    effective_provider: str | None = None


@dataclass
class ProxyResponse:
    """代理响应"""

    status_code: int
    headers: dict[str, str]
    body: bytes | None = None

    # 流式响应迭代器
    stream: AsyncIterator[bytes] | None = None

    # 是否是流式响应
    is_streaming: bool = False

    # 错误信息
    error: str | None = None


class TransparentProxy:
    """
    透明代理核心

    负责：
    - 从配置加载器获取目标服务信息
    - 构建上游请求
    - 转发请求并处理响应
    - 流式响应处理
    """

    def __init__(
        self,
        config_loader: ProxyConfigLoader,
        context_injector: ContextInjector | None = None,
        billing_interceptor: BillingInterceptor | None = None,
        response_cache: ResponseCache | None = None,
        default_timeout: float = 60.0,
        health_check_timeout: float = 5.0,
        availability_cache_ttl: float = 15.0,
        default_concurrency_limit: int = 32,
        default_streaming_concurrency_limit: int | None = None,
        default_non_streaming_concurrency_limit: int | None = None,
        default_concurrency_queue_timeout: float = 10.0,
        client_max_connections: int = 100,
        client_max_keepalive_connections: int = 20,
        client_keepalive_expiry: float = 30.0,
        capacity_resolver: CapacityResolver | None = None,
        admission_controller: CapacityAdmissionController | None = None,
    ):
        """
        初始化透明代理

        Args:
            config_loader: 服务配置加载器
            context_injector: 上下文注入器
            billing_interceptor: 计费拦截器
            default_timeout: 默认超时时间（秒）
        """
        self.config_loader = config_loader
        self.context_injector = context_injector or ContextInjector()
        self.billing_interceptor = billing_interceptor
        self.response_cache = response_cache or ResponseCache(database=config_loader.database)
        self.default_timeout = default_timeout
        self.health_check_timeout = health_check_timeout
        self.availability_cache_ttl = availability_cache_ttl
        self.default_concurrency_limit = default_concurrency_limit
        self.default_streaming_concurrency_limit = default_streaming_concurrency_limit
        self.default_non_streaming_concurrency_limit = default_non_streaming_concurrency_limit
        self.default_concurrency_queue_timeout = default_concurrency_queue_timeout
        self.client_max_connections = client_max_connections
        self.client_max_keepalive_connections = client_max_keepalive_connections
        self.client_keepalive_expiry = client_keepalive_expiry
        self.capacity_resolver = capacity_resolver or CapacityResolver()
        self.admission_controller = admission_controller or CapacityAdmissionController(
            redis_client=getattr(config_loader, "redis", None),
            load_shedder=_load_shedder_from_env(),
        )

        # HTTP 客户端池（按服务维护）
        self._clients: dict[tuple[str, str], httpx.AsyncClient] = {}
        self._client_lock = asyncio.Lock()

        # 负载均衡状态
        self._lb_counters: dict[str, int] = {}  # round-robin 计数器
        self._lb_connections: dict[str, dict[str, int]] = {}  # 连接计数
        self._service_semaphores: dict[tuple[str, str], asyncio.Semaphore] = {}
        self._availability: dict[str, dict[str, Any]] = {}
        self._availability_lock = asyncio.Lock()
        self._availability_refresh_inflight: dict[str, asyncio.Task[None]] = {}

    async def close(self) -> None:
        """关闭所有 HTTP 客户端"""
        for client in self._clients.values():
            await client.aclose()
        self._clients.clear()
        logger.info("Transparent proxy closed")

    async def _get_client(
        self,
        config: ProxyServiceConfig,
        *,
        slot_kind: str = "default",
    ) -> httpx.AsyncClient:
        """获取或创建 HTTP 客户端"""
        client_key = (config.service_id, slot_kind)

        if client_key not in self._clients:
            async with self._client_lock:
                if client_key not in self._clients:
                    timeout = httpx.Timeout(
                        connect=config.timeout_connect,
                        read=config.timeout_read,
                        write=config.timeout_write,
                        pool=config.timeout_pool,
                    )
                    max_connections = config.max_connections or self.client_max_connections
                    if slot_kind == "stream":
                        max_connections = (
                            config.streaming_max_connections
                            or _get_int_env("SERVICE_STREAMING_MAX_CONNECTIONS", 16)
                        )
                    limits = httpx.Limits(
                        max_connections=max_connections,
                        max_keepalive_connections=(
                            config.max_keepalive_connections
                            or self.client_max_keepalive_connections
                        ),
                        keepalive_expiry=(
                            config.keepalive_expiry
                            if config.keepalive_expiry is not None
                            else self.client_keepalive_expiry
                        ),
                    )
                    self._clients[client_key] = httpx.AsyncClient(
                        timeout=timeout,
                        follow_redirects=False,
                        http2=True,  # 启用 HTTP/2
                        limits=limits,
                    )

        return self._clients[client_key]

    def _resolve_concurrency_limit(
        self,
        config: ProxyServiceConfig,
        slot_kind: str,
    ) -> int:
        if slot_kind == "stream":
            configured = (
                config.streaming_concurrency_limit
                if config.streaming_concurrency_limit is not None
                else self.default_streaming_concurrency_limit
            )
        else:
            configured = (
                config.non_streaming_concurrency_limit
                if config.non_streaming_concurrency_limit is not None
                else self.default_non_streaming_concurrency_limit
            )

        limit = configured if configured is not None else config.concurrency_limit
        if limit is None:
            limit = self.default_concurrency_limit
        return max(1, int(limit))

    def _resolve_queue_timeout(self, config: ProxyServiceConfig) -> float:
        timeout = config.concurrency_queue_timeout
        if timeout is None:
            timeout = self.default_concurrency_queue_timeout
        return max(float(timeout or 0.0), 0.1)

    def _get_service_semaphore(
        self,
        config: ProxyServiceConfig,
        slot_kind: str,
    ) -> asyncio.Semaphore:
        service_key = (config.service_id, slot_kind)
        limit = self._resolve_concurrency_limit(config, slot_kind)
        semaphore = self._service_semaphores.get(service_key)
        if semaphore is None:
            semaphore = asyncio.Semaphore(limit)
            self._service_semaphores[service_key] = semaphore
        return semaphore

    @staticmethod
    def _request_class_for_capacity(request: ProxyRequest, slot_kind: str) -> str:
        path = (request.path or "").lower()
        if slot_kind == "stream" or "runs/stream" in path:
            return "stream"
        if "image" in path:
            return "image"
        if "retrieve" in path or "knowledge" in path:
            return "rag"
        if "tool" in path:
            return "tool"
        return "sync"

    @staticmethod
    def _provider_hint_for_capacity(
        body: bytes | None,
        config: ProxyServiceConfig,
        *,
        parsed_body: dict[str, Any] | None = None,
    ) -> str | None:
        request_data = parsed_body if parsed_body is not None else TransparentProxy._parse_json_body(body)
        return (
            extract_provider(request_data)
            or config.default_provider
            or (config.model_override or {}).get("provider_id")
        )

    @staticmethod
    def _capacity_config_for_proxy(config: ProxyServiceConfig) -> dict[str, Any]:
        capacity = dict(getattr(config, "capacity_config", None) or {})
        metadata = getattr(config, "metadata", None)
        if isinstance(metadata, dict):
            metadata_capacity = metadata.get("capacity")
            if isinstance(metadata_capacity, dict):
                capacity = {**metadata_capacity, **capacity}
        return {"capacity": capacity} if capacity else {}

    async def _acquire_capacity_lease(
        self,
        *,
        request: ProxyRequest,
        config: ProxyServiceConfig,
        slot_kind: str,
        body: bytes | None,
    ):
        context = request.context or RequestContext()
        request_class = self._request_class_for_capacity(request, slot_kind)
        capacity_config = self._capacity_config_for_proxy(config)
        configured_group = (capacity_config.get("capacity") or {}).get("upstream_group")
        budgets = await self.capacity_resolver.resolve(
            tenant_id=context.tenant_id or "public",
            service_id=config.service_id or request.service_name,
            request_class=request_class,
            upstream_group=service_upstream_group(config.service_id or request.service_name, configured_group),
            provider_id=self._provider_hint_for_capacity(
                body,
                config,
                parsed_body=request.parsed_body,
            ),
            service_config=capacity_config,
        )
        return await self.admission_controller.acquire(
            budgets=budgets,
            tenant_id=context.tenant_id or "public",
            user_id=context.user_id or "anonymous",
            service_id=config.service_id or request.service_name,
            request_class=request_class,
            request_id=context.request_id or f"{config.service_id}:{time.time_ns()}",
        )

    async def _acquire_request_slot(
        self,
        config: ProxyServiceConfig,
        upstream_base: str,
        slot_kind: str,
    ) -> tuple[Callable[[], Awaitable[None]], float]:
        semaphore = self._get_service_semaphore(config, slot_kind)
        queue_timeout = self._resolve_queue_timeout(config)
        wait_started = time.perf_counter()
        try:
            await asyncio.wait_for(semaphore.acquire(), timeout=queue_timeout)
        except asyncio.TimeoutError as exc:
            raise ProxyQueueTimeoutError(
                f"Service busy ({slot_kind} queue exceeded {queue_timeout:.1f}s)"
            ) from exc
        queue_wait_ms = (time.perf_counter() - wait_started) * 1000

        service_connections = self._lb_connections.setdefault(config.service_id, {})
        service_connections[upstream_base] = service_connections.get(upstream_base, 0) + 1
        released = False

        async def _release() -> None:
            nonlocal released
            if released:
                return
            released = True
            current = service_connections.get(upstream_base, 0)
            if current <= 1:
                service_connections.pop(upstream_base, None)
            else:
                service_connections[upstream_base] = current - 1
            semaphore.release()

        return _release, queue_wait_ms

    async def _set_service_availability(
        self,
        config: ProxyServiceConfig,
        *,
        status: str,
        healthy_urls: list[str] | None = None,
        error: str | None = None,
    ) -> dict[str, Any]:
        snapshot = {
            "availability_status": status,
            "available_upstreams": list(healthy_urls or []),
            "last_health_check_at": time.time(),
            "last_health_error": error,
        }
        config.availability_status = status
        config.last_health_check_at = snapshot["last_health_check_at"]
        config.last_health_error = error
        async with self._availability_lock:
            self._availability[config.service_id] = snapshot
        return snapshot

    async def _schedule_availability_refresh(self, config: ProxyServiceConfig) -> None:
        service_id = config.service_id
        async with self._availability_lock:
            inflight = self._availability_refresh_inflight.get(service_id)
            if inflight is not None and not inflight.done():
                return
            task = asyncio.create_task(self._run_availability_refresh(config))
            self._availability_refresh_inflight[service_id] = task

    async def _run_availability_refresh(self, config: ProxyServiceConfig) -> None:
        try:
            await self._refresh_service_availability(config)
        finally:
            async with self._availability_lock:
                current = self._availability_refresh_inflight.get(config.service_id)
                if current is asyncio.current_task():
                    self._availability_refresh_inflight.pop(config.service_id, None)

    async def get_service_availability(self, config: ProxyServiceConfig) -> dict[str, Any]:
        async with self._availability_lock:
            cached = self._availability.get(config.service_id)
        if cached:
            age = time.time() - float(cached.get("last_health_check_at") or 0.0)
            is_unavailable = cached.get("availability_status") == "unavailable"
            stale_ttl = self.availability_cache_ttl
            if is_unavailable:
                stale_ttl = min(stale_ttl, 5.0)
            if age > stale_ttl:
                if is_unavailable:
                    return await self._refresh_service_availability(config)
                await self._schedule_availability_refresh(config)
            return cached
        return await self._refresh_service_availability(config)

    async def _refresh_service_availability(self, config: ProxyServiceConfig) -> dict[str, Any]:
        client = await self._get_client(config, slot_kind="default")
        healthy_urls: list[str] = []
        failures: list[str] = []

        for upstream_base in config.get_upstream_urls():
            base = upstream_base.rstrip("/")
            probes: list[tuple[str, str, dict[str, Any] | None]] = [
                ("GET", f"{base}/health", None),
                ("GET", f"{base}/docs", None),
                ("POST", f"{base}/assistants/search", {}),
            ]

            upstream_ok = False
            last_error = ""
            for method, url, payload in probes:
                try:
                    if method == "POST":
                        response = await client.post(url, json=payload, timeout=config.health_check_timeout or self.health_check_timeout)
                    else:
                        response = await client.get(url, timeout=config.health_check_timeout or self.health_check_timeout)
                except Exception as exc:
                    last_error = str(exc)
                    continue

                if response.status_code < 400:
                    upstream_ok = True
                    break
                last_error = f"{response.status_code}"

            if upstream_ok:
                healthy_urls.append(upstream_base)
            else:
                failures.append(f"{upstream_base}: {last_error or 'unreachable'}")

        if healthy_urls and not failures:
            return await self._set_service_availability(config, status="available", healthy_urls=healthy_urls)
        if healthy_urls:
            return await self._set_service_availability(
                config,
                status="degraded",
                healthy_urls=healthy_urls,
                error="; ".join(failures),
            )
        return await self._set_service_availability(
            config,
            status="unavailable",
            healthy_urls=[],
            error="; ".join(failures) or "No healthy upstreams",
        )

    def _select_upstream(self, config: ProxyServiceConfig, urls: list[str] | None = None) -> str:
        """
        选择上游服务器（负载均衡）

        Args:
            config: 服务配置

        Returns:
            选中的上游 URL
        """
        urls = urls or config.get_upstream_urls()
        if not urls:
            raise ValueError(f"No upstream URLs configured for {config.service_name}")

        if len(urls) == 1:
            return urls[0]

        strategy = config.load_balance_strategy
        service_id = config.service_id

        if strategy == "round_robin":
            counter = self._lb_counters.get(service_id, -1)
            counter = (counter + 1) % len(urls)
            self._lb_counters[service_id] = counter
            return urls[counter]

        elif strategy == "least_connections":
            connections = self._lb_connections.get(service_id, {})
            min_conn = float("inf")
            selected = urls[0]
            for url in urls:
                conn = connections.get(url, 0)
                if conn < min_conn:
                    min_conn = conn
                    selected = url
            return selected

        elif strategy == "random":
            return random.choice(urls)

        else:
            # 默认 round_robin
            return urls[0]

    def _resolve_slot_kind(self, request: ProxyRequest) -> str:
        if request.stream or self._is_streaming_path(request.path):
            return "stream"
        return "default"

    def _build_upstream_url(
        self,
        config: ProxyServiceConfig,
        path: str,
        base_url: str,
    ) -> str:
        """
        构建上游请求 URL

        Args:
            config: 服务配置
            path: 请求路径
            base_url: 上游基础 URL

        Returns:
            完整的上游 URL
        """
        # 清理 base_url 尾部斜杠
        base_url = base_url.rstrip("/")

        # 路径重写
        if config.path_rewrite:
            path = config.path_rewrite.rstrip("/") + "/" + path.lstrip("/")

        # 确保路径以 / 开头
        if not path.startswith("/"):
            path = "/" + path

        return base_url + path

    async def proxy(self, request: ProxyRequest) -> ProxyResponse:
        """
        执行代理请求

        Args:
            request: 代理请求

        Returns:
            代理响应
        """
        start_time = time.perf_counter()
        t_config_start = start_time

        # 1. 获取服务配置
        config = request.preloaded_config or await self.config_loader.get_config(
            request.service_name
        )
        t_config_done = time.perf_counter()
        if not config:
            return ProxyResponse(
                status_code=404,
                headers={},
                error=f"Service not found: {request.service_name}",
            )

        if not config.enabled:
            return ProxyResponse(
                status_code=503,
                headers={},
                error=f"Service disabled: {request.service_name}",
            )

        availability = await self.get_service_availability(config)
        available_urls = availability.get("available_upstreams") or []
        availability_status = str(availability.get("availability_status") or "unknown")
        if not available_urls:
            error_message = str(availability.get("last_health_error") or "No healthy upstreams")
            logger.warning(
                "[Proxy] Fast-failing unavailable service %s status=%s error=%s",
                config.service_id,
                availability_status,
                error_message,
            )
            return ProxyResponse(
                status_code=503,
                headers={},
                error=f"Service unavailable: {error_message}",
            )

        # 2. 选择上游服务器
        try:
            upstream_base = self._select_upstream(config, urls=available_urls)
        except ValueError as e:
            return ProxyResponse(
                status_code=502,
                headers={},
                error=str(e),
            )

        # 3. 构建上游 URL
        upstream_url = self._build_upstream_url(config, request.path, upstream_base)

        # 4. 构建请求头
        context = request.context or RequestContext()
        headers = self.context_injector.build_headers(
            context=context,
            service_auth_token=config.auth_token if not config.forward_auth else None,
        )
        t_headers_done = time.perf_counter()

        # 5. 获取 HTTP 客户端
        slot_kind = self._resolve_slot_kind(request)
        client = await self._get_client(config, slot_kind=slot_kind)
        t_client_done = time.perf_counter()
        capacity_lease = None
        try:
            capacity_lease = await self._acquire_capacity_lease(
                request=request,
                config=config,
                slot_kind=slot_kind,
                body=request.body,
            )
            release_slot, queue_wait_ms = await self._acquire_request_slot(
                config,
                upstream_base,
                slot_kind,
            )
        except ProxyQueueTimeoutError as e:
            if capacity_lease is not None:
                await capacity_lease.release()
            duration = (time.time() - start_time) * 1000
            logger.warning(
                "[Proxy] Queue timeout after %.2fms service=%s slot=%s error=%s",
                duration,
                config.service_id,
                slot_kind,
                e,
            )
            return ProxyResponse(
                status_code=503,
                headers={},
                error=str(e),
            )
        except CapacityRejected as e:
            logger.warning(
                "gateway_capacity_decision result=rejected budget_key=%s service_id=%s "
                "request_id=%s code=%s",
                e.budget_key,
                config.service_id,
                getattr(request.context, "request_id", "") if request.context else "",
                e.code,
            )
            return ProxyResponse(
                status_code=e.status_code,
                headers={
                    **e.headers,
                    "content-type": "application/json",
                },
                body=json.dumps(
                    {
                        "detail": {
                            "code": e.code,
                            "message": e.message,
                            "budget_key": e.budget_key,
                        }
                    }
                ).encode("utf-8"),
                is_streaming=False,
            )

        except BaseException:
            if capacity_lease is not None:
                await capacity_lease.release()
            raise

        body = request.body
        t_body_done = time.perf_counter()
        capacity_headers = capacity_lease.headers if capacity_lease is not None else {}

        async def release_all() -> None:
            try:
                await release_slot()
            finally:
                if capacity_lease is not None:
                    await capacity_lease.release()

        # Performance logging
        logger.info(
            f"[Proxy][TIMING] {request.method} {request.service_name}/{request.path} prep: "
            f"config={((t_config_done - t_config_start) * 1000):.1f}ms "
            f"headers={((t_headers_done - t_config_done) * 1000):.1f}ms "
            f"client={((t_client_done - t_headers_done) * 1000):.1f}ms "
            f"body={((t_body_done - t_client_done) * 1000):.1f}ms "
            f"slot={slot_kind} "
            f"queue_wait={queue_wait_ms:.1f}ms "
            f"total_prep={((t_body_done - start_time) * 1000):.1f}ms"
        )

        # 7. 执行请求
        logger.info(
            f"[Proxy] {request.method} {request.service_name}/{request.path} -> {upstream_url}"
        )

        try:
            if request.stream or self._is_streaming_path(request.path):
                response = await self._proxy_streaming(
                    client=client,
                    method=request.method,
                    url=upstream_url,
                    headers=headers,
                    body=body,
                    params=request.query_params,
                    config=config,
                    context=context,
                    path=request.path,
                    release_slot=release_all,
                    parsed_body=request.parsed_body,
                    effective_model=request.effective_model,
                    effective_provider=request.effective_provider,
                )
                if response.stream is not None:
                    await response.stream.start(capacity_lease)
                response.headers.update(capacity_headers)
                return response
            else:
                try:
                    response = await self._proxy_normal(
                        client=client,
                        method=request.method,
                        url=upstream_url,
                        upstream_base=upstream_base,
                        headers=headers,
                        body=body,
                        params=request.query_params,
                        path=request.path,
                        config=config,
                        context=context,
                        parsed_body=request.parsed_body,
                        effective_model=request.effective_model,
                        effective_provider=request.effective_provider,
                    )
                    response.headers.update(capacity_headers)
                    return response
                finally:
                    await release_all()
        except httpx.TimeoutException as e:
            duration = (time.time() - start_time) * 1000
            logger.error(f"[Proxy] Timeout after {duration:.2f}ms: {e}")
            await release_all()
            return ProxyResponse(
                status_code=504,
                headers={},
                error=f"Upstream timeout: {e}",
            )
        except httpx.RequestError as e:
            duration = (time.time() - start_time) * 1000
            logger.error(f"[Proxy] Request error after {duration:.2f}ms: {e}")
            await release_all()
            return ProxyResponse(
                status_code=502,
                headers={},
                error=f"Upstream error: {e}",
            )
        except BaseException:
            await release_all()
            raise

    def _inject_assistant_id(
        self,
        body: bytes | None,
        path: str,
        assistant_id: str,
    ) -> bytes | None:
        """
        为 LangGraph 请求注入 assistant_id

        LangGraph API 的以下端点需要 assistant_id:
        - POST /runs
        - POST /runs/stream
        - POST /runs/wait
        - POST /threads/{thread_id}/runs
        - POST /threads/{thread_id}/runs/stream
        - POST /threads/{thread_id}/runs/wait

        如果请求体中已包含 assistant_id，则不覆盖。
        """
        if not body:
            return body

        # 检查是否是需要注入的路径
        path_lower = self._normalize_path(path).lower()
        needs_injection = bool(_LANGGRAPH_RUN_RE.search(path_lower))

        if not needs_injection:
            return body

        try:
            data = json.loads(body.decode("utf-8"))

            # 如果已有 assistant_id，不覆盖
            if isinstance(data, dict) and "assistant_id" not in data:
                data["assistant_id"] = assistant_id
                logger.debug(f"[Proxy] Injected assistant_id={assistant_id} into request body")
                return json.dumps(data).encode("utf-8")

            return body

        except (json.JSONDecodeError, UnicodeDecodeError):
            # 非 JSON 请求体，原样返回
            return body

    @staticmethod
    def _replace_assistant_id(body: bytes | None, assistant_id: str) -> bytes | None:
        """强制替换请求体中的 assistant_id（用于重试自愈）。"""
        if not body:
            return body
        try:
            data = json.loads(body.decode("utf-8"))
        except (json.JSONDecodeError, UnicodeDecodeError):
            return body
        if not isinstance(data, dict):
            return body
        data["assistant_id"] = assistant_id
        return json.dumps(data).encode("utf-8")

    @staticmethod
    def _normalize_path(path: str) -> str:
        normalized = (path or "").strip()
        if not normalized:
            return "/"
        if not normalized.startswith("/"):
            normalized = "/" + normalized
        return normalized

    @staticmethod
    def _stream_mode_wants_messages(stream_mode: Any) -> bool:
        """检查 stream_mode 是否包含 messages 模式"""
        if stream_mode is None:
            return False
        if isinstance(stream_mode, str):
            modes = [m.strip() for m in stream_mode.split(",") if m.strip()]
        elif isinstance(stream_mode, list):
            modes = [str(m).strip() for m in stream_mode if str(m).strip()]
        else:
            return False
        return any(m in ("messages", "messages-tuple") for m in modes)

    def _ensure_stream_defaults(self, body: bytes | None, path: str) -> bytes | None:
        """
        为 LangGraph 流式请求设置默认参数

        确保 /runs/stream 请求包含合理的默认值：
        - stream_mode: ["messages", "updates", "custom"] - 启用消息和自定义事件流
        - stream_subgraphs: true - 启用子图流式输出
        """
        if not body:
            return body
        normalized_path = self._normalize_path(path)
        if not self._is_streaming_path(normalized_path):
            return body
        if "/runs/stream" not in normalized_path.lower():
            return body

        try:
            data = json.loads(body.decode("utf-8"))
        except (json.JSONDecodeError, UnicodeDecodeError):
            return body

        if not isinstance(data, dict):
            return body

        changed = False

        # 设置默认 stream_mode
        if not data.get("stream_mode"):
            data["stream_mode"] = ["messages", "updates", "custom"]
            changed = True
            logger.debug("[Proxy] Set default stream_mode=['messages', 'updates', 'custom']")

        # 如果 stream_mode 包含 messages，自动启用 stream_subgraphs
        if "stream_subgraphs" not in data and self._stream_mode_wants_messages(
            data.get("stream_mode")
        ):
            data["stream_subgraphs"] = True
            changed = True
            logger.debug("[Proxy] Set stream_subgraphs=true for messages mode")

        if not changed:
            return body

        return json.dumps(data, ensure_ascii=False).encode("utf-8")

    @staticmethod
    def _parse_json_body(body: bytes | None) -> Any | None:
        if not body:
            return None
        try:
            return json.loads(body.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            return None

    @staticmethod
    def _is_run_operation_path(path: str) -> bool:
        normalized = TransparentProxy._normalize_path(path).lower()
        return "/runs" in normalized

    @staticmethod
    def _extract_error_detail(body: bytes) -> str:
        try:
            payload = json.loads(body.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            return body.decode("utf-8", errors="ignore")
        if isinstance(payload, dict):
            detail = payload.get("detail")
            if detail is not None:
                return str(detail)
        return str(payload)

    @staticmethod
    def _extract_assistant_records(payload: Any) -> list[dict[str, Any]]:
        if isinstance(payload, list):
            return [item for item in payload if isinstance(item, dict)]
        if isinstance(payload, dict):
            for key in ("items", "assistants", "data", "results"):
                value = payload.get(key)
                if isinstance(value, list):
                    return [item for item in value if isinstance(item, dict)]
        return []

    def _build_graph_candidates(self, config: ProxyServiceConfig) -> list[str]:
        candidates: list[str] = []

        def _add(value: str | None) -> None:
            if not value:
                return
            normalized = str(value).strip()
            if normalized and normalized not in candidates:
                candidates.append(normalized)

        _add(config.graph_id)
        _add(config.assistant_id)
        _add(config.service_name)
        _add(config.service_id)

        for token in re.split(r"[^A-Za-z0-9]+", config.service_id or ""):
            if not token or token.isdigit():
                continue
            _add(token)

        return candidates

    async def _resolve_assistant_retry_candidate(
        self,
        *,
        client: httpx.AsyncClient,
        config: ProxyServiceConfig,
        upstream_base: str,
        headers: dict[str, str],
    ) -> str | None:
        """
        基于上游 assistants/search 结果选择可用 assistant_id（只做无损自动修复）。
        """
        probe_url = self._build_upstream_url(config, "assistants/search", upstream_base)
        try:
            response = await client.post(
                probe_url,
                headers=headers,
                json={},
            )
        except Exception as exc:
            logger.debug(f"[Proxy] assistant retry probe failed: {exc}")
            return None

        if response.status_code >= 400:
            logger.debug(f"[Proxy] assistant retry probe status={response.status_code}")
            return None

        records = self._extract_assistant_records(self._parse_json_body(response.content))
        if not records:
            return None

        graph_map = {
            str(item.get("graph_id")).strip().lower(): str(item.get("graph_id")).strip()
            for item in records
            if item.get("graph_id")
        }
        assistant_map = {
            str(item.get("assistant_id")).strip().lower(): str(item.get("assistant_id")).strip()
            for item in records
            if item.get("assistant_id")
        }

        for candidate in self._build_graph_candidates(config):
            key = candidate.strip().lower()
            resolved = graph_map.get(key) or assistant_map.get(key)
            if resolved:
                return resolved

        return None

    async def _retry_on_invalid_assistant(
        self,
        *,
        response: httpx.Response,
        client: httpx.AsyncClient,
        method: str,
        url: str,
        headers: dict[str, str],
        body: bytes | None,
        params: dict[str, Any],
        path: str,
        config: ProxyServiceConfig,
        upstream_base: str,
    ) -> tuple[httpx.Response, bytes | None]:
        if response.status_code != 422 or not self._is_run_operation_path(path):
            return response, body

        detail = self._extract_error_detail(response.content)
        if "Invalid assistant" not in detail:
            return response, body

        retry_assistant = await self._resolve_assistant_retry_candidate(
            client=client,
            config=config,
            upstream_base=upstream_base,
            headers=headers,
        )
        if not retry_assistant:
            return response, body

        current_assistant = str(
            (self._parse_json_body(body) or {}).get("assistant_id") or config.assistant_id or ""
        ).strip()
        if retry_assistant == current_assistant:
            return response, body

        retry_body = self._replace_assistant_id(body, retry_assistant)
        if retry_body == body:
            return response, body

        logger.warning(
            "[Proxy] Auto-healing invalid assistant for service %s: %s -> %s",
            config.service_id,
            current_assistant or "<empty>",
            retry_assistant,
        )

        retry_response = await client.request(
            method=method,
            url=url,
            headers=headers,
            content=retry_body,
            params=params,
        )

        # Keep graph_id untouched; retry target can be either graph name or assistant UUID.
        config.assistant_id = retry_assistant

        return retry_response, retry_body

    @staticmethod
    def _extract_error_metadata(payload: Any) -> dict[str, str]:
        if not isinstance(payload, dict):
            return {}
        error_payload = payload.get("__error__") or payload.get("error")
        if not error_payload:
            return {}

        if isinstance(error_payload, dict):
            error_type = str(
                error_payload.get("error") or error_payload.get("type") or "upstream_error"
            )
            error_message = redact_sensitive_text(
                str(error_payload.get("message") or error_payload.get("detail") or "")
            )
        else:
            error_type = "upstream_error"
            error_message = redact_sensitive_text(str(error_payload))

        metadata: dict[str, str] = {"upstream_error_type": error_type[:128]}
        if error_message:
            metadata["upstream_error_message"] = error_message[:512]
        return metadata

    @staticmethod
    def _build_trace_steps(
        total_duration_ms: int, breakdown: dict[str, int], status: str
    ) -> list[dict[str, Any]]:
        steps: list[dict[str, Any]] = [
            {
                "name": "gateway.request_total",
                "start_offset_ms": 0,
                "duration_ms": max(int(total_duration_ms or 0), 0),
                "status": status,
            }
        ]

        offset = 0
        for name, key in (
            ("agent.retrieval", "retrieval_duration_ms"),
            ("agent.tool_calls", "tool_call_duration_ms"),
            ("llm.inference", "llm_inference_duration_ms"),
            ("agent.overhead", "agent_or_graph_overhead_ms"),
        ):
            duration = max(int(breakdown.get(key, 0) or 0), 0)
            if duration <= 0:
                continue
            steps.append(
                {
                    "name": name,
                    "start_offset_ms": offset,
                    "duration_ms": duration,
                    "status": "success",
                }
            )
            offset += duration
        return steps

    @staticmethod
    def _resolve_billing_hints(
        request_data: dict[str, Any] | None,
        *,
        config: ProxyServiceConfig,
        effective_model: str | None = None,
        effective_provider: str | None = None,
        is_run_operation: bool = False,
    ) -> tuple[str, str]:
        model = (
            str(effective_model or "").strip()
            or extract_model(request_data)
            or config.default_model
            or ("langgraph-agent" if is_run_operation else "")
        )
        provider = (
            str(effective_provider or "").strip()
            or extract_provider(request_data)
            or config.default_provider
            or ""
        )
        if not provider:
            model_override = config.model_override
            if isinstance(model_override, dict):
                provider = str(model_override.get("provider_id") or "").strip()
        return model, provider

    async def _record_non_stream_usage(
        self,
        response_body: bytes,
        response_content_type: str,
        request_body: bytes | None,
        config: ProxyServiceConfig,
        context: RequestContext,
        method: str,
        path: str,
        duration_ms: float,
        status_code: int,
        *,
        parsed_body: dict[str, Any] | None = None,
        effective_model: str | None = None,
        effective_provider: str | None = None,
    ) -> None:
        operation = self.detect_operation_type(method, path)
        is_run_operation = operation.startswith("run_")

        response_data = None
        if "application/json" in (response_content_type or "").lower():
            response_data = self._parse_json_body(response_body)

        # 非 run 请求仍保持「必须有 usage 才记录」策略，避免噪音数据。
        if response_data is None and not is_run_operation:
            return

        usage = extract_token_usage(response_data) if response_data is not None else None
        request_data = (
            parsed_body if parsed_body is not None else self._parse_json_body(request_body)
        )

        input_tokens = int((usage or {}).get("input_tokens") or 0)
        output_tokens = int((usage or {}).get("output_tokens") or 0)
        total_tokens = int((usage or {}).get("total_tokens") or (input_tokens + output_tokens))
        has_usage = total_tokens > 0 or input_tokens > 0 or output_tokens > 0
        if not has_usage and not is_run_operation:
            return

        response_error = bool(
            response_data
            and isinstance(response_data, dict)
            and ("__error__" in response_data or "error" in response_data)
        )
        status = "error" if status_code >= 400 or response_error else "success"

        assistant_id = (
            config.assistant_id
            or extract_assistant_id(request_data)
            or extract_assistant_id(response_data)
            or ""
        )
        response_model = extract_model(response_data)
        response_provider = extract_provider(response_data)
        model, provider = self._resolve_billing_hints(
            request_data if isinstance(request_data, dict) else None,
            config=config,
            effective_model=response_model or effective_model,
            effective_provider=response_provider or effective_provider,
            is_run_operation=is_run_operation,
        )

        request_type = f"proxy_{operation}"
        metadata: dict[str, Any] = {
            "source": "transparent_proxy_non_stream",
            "path": self._normalize_path(path),
            "http_status": status_code,
            "response_has_usage": bool(usage),
            "response_has_error": response_error,
            "token_source": "upstream"
            if usage
            else ("zero_on_failure" if status == "error" else "estimated"),
            "provider": provider,
            "model": model,
        }
        metadata.update(self._extract_error_metadata(response_data))

        extracted_breakdown: dict[str, Any] = {}
        if isinstance(request_data, dict):
            extracted_breakdown.update(extract_duration_breakdown(request_data))
        if isinstance(response_data, dict):
            extracted_breakdown.update(extract_duration_breakdown(response_data))

        breakdown = ensure_duration_breakdown(
            request_total_duration_ms=int(duration_ms),
            first_token_latency_ms=extracted_breakdown.get(
                "first_token_latency_ms", int(duration_ms)
            ),
            llm_inference_duration_ms=extracted_breakdown.get("llm_inference_duration_ms", 0),
            retrieval_duration_ms=extracted_breakdown.get("retrieval_duration_ms", 0),
            tool_call_duration_ms=extracted_breakdown.get("tool_call_duration_ms", 0),
            agent_or_graph_overhead_ms=extracted_breakdown.get("agent_or_graph_overhead_ms", 0),
        )
        tool_breakdown = extracted_breakdown.get("tool_call_breakdown", {})
        if not isinstance(tool_breakdown, dict):
            tool_breakdown = {}
        error_type = classify_error_type(
            status=status,
            status_code=status_code,
            upstream_error_type=str(metadata.get("upstream_error_type") or ""),
            upstream_error_message=str(metadata.get("upstream_error_message") or ""),
        )
        metadata["error_type"] = error_type
        metadata["duration_breakdown"] = breakdown
        metadata["tool_call_breakdown"] = tool_breakdown

        try:
            from ..services.metrics import get_usage_recorder

            recorder = get_usage_recorder()
            await recorder.record_usage(
                tenant_id=context.tenant_id or "default",
                user_id=context.user_id or "anonymous",
                model=model or "unknown",
                input_tokens=input_tokens,
                output_tokens=output_tokens,
                request_id=context.request_id,
                service_id=config.service_id,
                assistant_id=assistant_id,
                provider=provider,
                latency_ms=int(duration_ms),
                first_token_ms=breakdown["first_token_latency_ms"],
                request_total_duration_ms=breakdown["request_total_duration_ms"],
                llm_inference_duration_ms=breakdown["llm_inference_duration_ms"],
                retrieval_duration_ms=breakdown["retrieval_duration_ms"],
                tool_call_duration_ms=breakdown["tool_call_duration_ms"],
                agent_or_graph_overhead_ms=breakdown["agent_or_graph_overhead_ms"],
                tool_call_breakdown=tool_breakdown,
                error_type=error_type,
                status=status,
                request_type=request_type,
                metadata=metadata,
                trace_steps=self._build_trace_steps(
                    breakdown["request_total_duration_ms"],
                    breakdown,
                    status,
                ),
            )
        except Exception as e:
            logger.debug(f"[Proxy] Failed to record non-stream usage: {e}")

        try:
            if self.billing_interceptor and self.billing_interceptor.realtime_metrics:
                await self.billing_interceptor.realtime_metrics.record_token_usage(
                    input_tokens=input_tokens,
                    output_tokens=output_tokens,
                )
        except Exception as e:
            logger.debug(f"[Proxy] Failed to update realtime token metrics: {e}")

    async def _proxy_normal(
        self,
        client: httpx.AsyncClient,
        method: str,
        url: str,
        upstream_base: str,
        headers: dict[str, str],
        body: bytes | None,
        params: dict[str, Any],
        path: str,
        config: ProxyServiceConfig,
        context: RequestContext,
        *,
        parsed_body: dict[str, Any] | None = None,
        effective_model: str | None = None,
        effective_provider: str | None = None,
    ) -> ProxyResponse:
        """执行普通（非流式）代理请求"""
        request_started = time.perf_counter()

        cache_status = "BYPASS"
        cache_hash: str | None = None
        if self.response_cache:
            (
                cache_status,
                cache_hash,
                cached_response,
            ) = await self.response_cache.get_cached_response(
                config=config,
                context=context,
                method=method,
                path=path,
                body=body,
                query_params=params,
                stream=False,
                parsed_body=parsed_body,
            )
            if cached_response:
                cached_headers = self._filter_response_headers(dict(cached_response.headers))
                cached_headers["X-Gateway-Cache"] = "HIT"
                logger.info(
                    f"[ProxyCache] HIT service={config.service_id} path={self._normalize_path(path)} "
                    f"user={context.user_id or 'anonymous'}"
                )
                return ProxyResponse(
                    status_code=cached_response.status_code,
                    headers=cached_headers,
                    body=cached_response.body,
                    is_streaming=False,
                )

        response = await client.request(
            method=method,
            url=url,
            headers=headers,
            content=body,
            params=params,
        )
        response, body = await self._retry_on_invalid_assistant(
            response=response,
            client=client,
            method=method,
            url=url,
            headers=headers,
            body=body,
            params=params,
            path=path,
            config=config,
            upstream_base=upstream_base,
        )

        duration_ms = (time.perf_counter() - request_started) * 1000

        # 提取响应头（过滤 hop-by-hop 头）
        response_headers = self._filter_response_headers(dict(response.headers))
        response_headers["X-Gateway-Cache"] = cache_status

        # 非流式路径也需要做 usage 统计（例如 /runs/wait）。
        await self._record_non_stream_usage(
            response_body=response.content,
            response_content_type=response_headers.get("content-type", ""),
            request_body=body,
            config=config,
            context=context,
            method=method,
            path=path,
            duration_ms=duration_ms,
            status_code=response.status_code,
            parsed_body=parsed_body,
            effective_model=effective_model,
            effective_provider=effective_provider,
        )

        if self.response_cache and cache_status == "MISS":
            await self.response_cache.save_response(
                cache_hash=cache_hash,
                config=config,
                context=context,
                method=method,
                path=path,
                body=body,
                query_params=params,
                response_status=response.status_code,
                response_headers=response_headers,
                response_body=response.content,
                stream=False,
                parsed_body=parsed_body,
            )

        # 错误透传：原样返回上游的 4xx/5xx 响应，不用网关错误覆盖
        # 这样前端可以获取到原始的业务错误信息
        return ProxyResponse(
            status_code=response.status_code,
            headers=response_headers,
            body=response.content,
            is_streaming=False,
            # 不设置 error，让上游的原始响应透传
        )

    async def _proxy_streaming(
        self,
        client: httpx.AsyncClient,
        method: str,
        url: str,
        headers: dict[str, str],
        body: bytes | None,
        params: dict[str, Any],
        config: ProxyServiceConfig,
        context: RequestContext,
        path: str,
        release_slot,
        *,
        parsed_body: dict[str, Any] | None = None,
        effective_model: str | None = None,
        effective_provider: str | None = None,
    ) -> ProxyResponse:
        """
        执行流式代理请求

        支持：
        - 自动检测响应 Content-Type，如果不是 text/event-stream 则回退到普通响应
        - 错误透传：原样返回上游 4xx/5xx 错误
        - 计费拦截
        """
        request_started = time.perf_counter()
        request_data = parsed_body if parsed_body is not None else self._parse_json_body(body)

        # 创建流处理器（用于计费）
        stream_processor: StreamProcessor | None = None
        if self.billing_interceptor:
            assistant_id = config.assistant_id or extract_assistant_id(request_data) or ""
            operation = self.detect_operation_type(method, path)
            request_type = f"proxy_{operation}"
            model_hint, provider_hint = self._resolve_billing_hints(
                request_data if isinstance(request_data, dict) else None,
                config=config,
                effective_model=effective_model,
                effective_provider=effective_provider,
                is_run_operation=operation.startswith("run_"),
            )
            stream_processor = self.billing_interceptor.create_stream_processor(
                request_id=context.request_id,
                service_id=config.service_id,
                user_id=context.user_id,
                tenant_id=context.tenant_id,
                assistant_id=assistant_id,
                request_type=request_type,
                model_hint=model_hint or "langgraph-agent",
                provider_hint=provider_hint,
            )

        # 先获取响应头，判断是否真的是流式响应
        t_send_start = time.perf_counter()
        try:
            response = await client.send(
                client.build_request(
                    method=method,
                    url=url,
                    headers=headers,
                    content=body,
                    params=params,
                ),
                stream=True,
            )
            t_response_headers = time.perf_counter()
            logger.info(
                f"[Proxy][TIMING] streaming response_headers={((t_response_headers - t_send_start) * 1000):.1f}ms "
                f"status={response.status_code}"
            )
        except httpx.TimeoutException as e:
            logger.error(f"[Proxy] Streaming timeout: {e}")
            await release_slot()
            return ProxyResponse(
                status_code=504,
                headers={},
                error=f"Upstream timeout: {e}",
            )
        except httpx.RequestError as e:
            logger.error(f"[Proxy] Streaming request error: {e}")
            await release_slot()
            return ProxyResponse(
                status_code=502,
                headers={},
                error=f"Upstream error: {e}",
            )

        stream_owns_response = False
        try:
            response_content_type = response.headers.get("content-type", "")
            response_headers = self._filter_response_headers(dict(response.headers))
            response_headers["X-Gateway-Cache"] = "BYPASS"

            # 错误透传：4xx/5xx 错误原样返回，不覆盖
            if response.status_code >= 400:
                error_body = await response.aread()
                duration_ms = (time.perf_counter() - request_started) * 1000
                await self._record_non_stream_usage(
                    response_body=error_body,
                    response_content_type=response_content_type,
                    request_body=body,
                    config=config,
                    context=context,
                    method=method,
                    path=path,
                    duration_ms=duration_ms,
                    status_code=response.status_code,
                )
                logger.warning(
                    f"[Proxy] Upstream error {response.status_code}: {error_body[:200].decode('utf-8', errors='ignore')}"
                )
                return ProxyResponse(
                    status_code=response.status_code,
                    headers=response_headers,
                    body=error_body,
                    is_streaming=False,
                    # 不设置 error，保持原始响应内容
                )

            # 自动检测：如果响应不是流式的，直接返回普通响应
            is_sse = "text/event-stream" in response_content_type

            if not is_sse:
                # 非流式响应，读取完整内容
                body_content = await response.aread()
                await response.aclose()
                await release_slot()
                logger.debug(f"[Proxy] Non-streaming response detected: {response_content_type}")
                duration_ms = (time.perf_counter() - request_started) * 1000
                await self._record_non_stream_usage(
                    response_body=body_content,
                    response_content_type=response_content_type,
                    request_body=body,
                    config=config,
                    context=context,
                    method=method,
                    path=path,
                    duration_ms=duration_ms,
                    status_code=response.status_code,
                )
                return ProxyResponse(
                    status_code=response.status_code,
                    headers=response_headers,
                    body=body_content,
                    is_streaming=False,
                )

            # 流式响应处理
            async def stream_generator():
                """流式响应生成器"""
                try:
                    async for chunk in response.aiter_raw():
                        if stream_processor:
                            chunk = await stream_processor.process_chunk(chunk)
                        yield chunk

                except Exception as e:
                    logger.error(f"[Proxy] Streaming error: {e}")
                    # 发送错误事件
                    error_event = f"event: error\ndata: {str(e)}\n\n"
                    yield error_event.encode("utf-8")

            async def close_stream():
                try:
                    if stream_processor:
                        try:
                            await stream_processor.finalize()
                        except Exception as finalize_err:
                            logger.debug(f"[Proxy] Failed to finalize stream processor: {finalize_err}")
                finally:
                    async def close_resources():
                        try:
                            await response.aclose()
                        finally:
                            await release_slot()
                    await _finish_cleanup(asyncio.create_task(close_resources()))

            # The consumer now owns response and admission cleanup.
            stream_owns_response = True
            # 返回流式响应，保留原始状态码
            return ProxyResponse(
                status_code=response.status_code,
                headers={
                    "Content-Type": "text/event-stream",
                    "Cache-Control": "no-cache",
                    "Connection": "keep-alive",
                    "X-Accel-Buffering": "no",  # 禁用 nginx 缓冲
                    "X-Gateway-Cache": "BYPASS",
                },
                stream=_OwnedProxyStream(stream_generator(), close_stream),
                is_streaming=True,
            )
        finally:
            if not stream_owns_response:
                try:
                    await response.aclose()
                finally:
                    await release_slot()

    def _is_streaming_path(self, path: str) -> bool:
        """
        检查是否是流式路径

        LangGraph 的流式端点：
        - /runs/stream
        - /threads/{thread_id}/runs/stream
        """
        streaming_suffixes = [
            "/stream",
            "/runs/stream",
            "/sse",
        ]

        path_lower = self._normalize_path(path).lower()

        # 精确匹配流式后缀
        for suffix in streaming_suffixes:
            if path_lower.endswith(suffix):
                return True

        # 检查路径中是否包含 "stream"（但要排除 "upstream" 等）
        return "/stream" in path_lower

    @staticmethod
    def detect_operation_type(method: str, path: str) -> str:
        """
        检测 LangGraph API 操作类型

        用于更精细的限流控制。

        Args:
            method: HTTP 方法
            path: 请求路径

        Returns:
            操作类型字符串，如 "run_stream", "thread_create" 等
        """
        import re

        path_lower = TransparentProxy._normalize_path(path).lower()
        if path_lower != "/":
            path_lower = path_lower.rstrip("/")
        method_upper = method.upper()

        # 首先尝试精确匹配
        key = f"{method_upper} {path_lower}"
        if key in LANGGRAPH_OPERATION_TYPES:
            return LANGGRAPH_OPERATION_TYPES[key]

        # 尝试前缀匹配（带 /）
        for pattern, op_type in LANGGRAPH_OPERATION_TYPES.items():
            p_method, p_path = pattern.split(" ", 1)
            if p_method != method_upper:
                continue

            # 通配符模式匹配
            if "*" in p_path:
                # 将通配符转换为正则
                regex_pattern = p_path.replace("*", "[^/]+")
                if p_path.endswith("/"):
                    regex_pattern = regex_pattern + ".*"
                if re.match(f"^{regex_pattern}$", path_lower):
                    return op_type
            elif p_path.endswith("/"):
                # 前缀匹配
                if path_lower.startswith(p_path[:-1]):
                    return op_type

        # 默认为 proxy
        return "proxy"

    def _filter_response_headers(self, headers: dict[str, str]) -> dict[str, str]:
        """过滤响应头（移除 hop-by-hop 头）"""
        hop_by_hop = {
            "connection",
            "keep-alive",
            "proxy-authenticate",
            "proxy-authorization",
            "te",
            "trailers",
            "transfer-encoding",
            "upgrade",
        }

        return {k: v for k, v in headers.items() if k.lower() not in hop_by_hop}

    async def health_check(self, service_name: str) -> tuple[bool, str]:
        """
        服务健康检查

        Args:
            service_name: 服务名称

        Returns:
            (健康状态, 消息)
        """
        config = await self.config_loader.get_config(service_name)
        if not config:
            return False, f"Service not found: {service_name}"

        if not config.enabled:
            return False, f"Service disabled: {service_name}"
        snapshot = await self.get_service_availability(config)
        status = str(snapshot.get("availability_status") or "unknown")
        if status in {"available", "degraded"}:
            return True, status.upper()
        return False, str(snapshot.get("last_health_error") or "No healthy upstreams")

    async def refresh_all_service_health(self) -> dict[str, dict[str, Any]]:
        snapshots: dict[str, dict[str, Any]] = {}
        for config in await self.config_loader.list_services():
            snapshots[config.service_id] = await self._refresh_service_availability(config)
        return snapshots


def _get_int_env(key: str, default: int) -> int:
    raw = os.getenv(key, "").strip()
    if not raw:
        return default
    try:
        return int(raw)
    except ValueError:
        logger.warning("Invalid %s=%r; using default %s", key, raw, default)
        return default


def _get_float_env(key: str, default: float) -> float:
    raw = os.getenv(key, "").strip()
    if not raw:
        return default
    try:
        return float(raw)
    except ValueError:
        logger.warning("Invalid %s=%r; using default %s", key, raw, default)
        return default


def _load_shedder_from_env() -> AdaptiveLoadShedder:
    return AdaptiveLoadShedder(
        window_seconds=_get_float_env("ADAPTIVE_LOAD_SHEDDING_WINDOW_SECONDS", 30.0),
        low_threshold_ms=_get_float_env("ADAPTIVE_LOAD_SHEDDING_LOW_THRESHOLD_MS", 5_000.0),
        normal_threshold_ms=_get_float_env("ADAPTIVE_LOAD_SHEDDING_NORMAL_THRESHOLD_MS", 10_000.0),
        high_threshold_ms=_get_float_env("ADAPTIVE_LOAD_SHEDDING_HIGH_THRESHOLD_MS", 20_000.0),
    )
