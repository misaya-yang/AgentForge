"""
LangGraph Server 代理层

负责：
- 完整代理 LangGraph Server 的 Assistant/Thread/Run API
- 负载均衡：支持多个 LangGraph Server 实例
- 限流检查：按用户层级应用不同限流策略
- 权限验证：验证 Thread 所有权、Assistant 访问权限
- 上下文注入：为 Run 注入用户上下文
- 指标收集：记录 token 使用和 run 执行指标
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import os
import random
import time
from collections.abc import AsyncIterator
from dataclasses import dataclass, field
from datetime import datetime
from typing import Any

import httpx
from fastapi import Request

from ..proxy.langgraph_run_body import (
    build_minimal_langgraph_prep_request,
    merge_gateway_domain_policy_metadata,
    prepare_and_finalize_langgraph_run_payload,
    prepare_langgraph_run_payload,
    synthetic_langgraph_service_config,
)
from ..services.metrics import get_metrics_recorder, get_usage_recorder
from ..services.metrics.realtime_metrics import get_realtime_metrics

logger = logging.getLogger(__name__)


# ============ 数据类定义 ============


@dataclass
class UserContext:
    """用户上下文"""

    user_id: str
    tenant_id: str = ""
    tier: str = "anonymous"  # anonymous | normal | premium | enterprise | admin
    is_authenticated: bool = False
    ip: str = ""


@dataclass
class LangGraphInstance:
    """LangGraph Server 实例"""

    instance_id: str
    url: str  # http://langgraph-1:8000
    weight: int = 1  # 权重
    is_healthy: bool = True
    active_connections: int = 0
    avg_latency: float = 0.0
    last_health_check: datetime | None = None

    def __post_init__(self):
        self.url = self.url.rstrip("/")


@dataclass
class LoadBalancerConfig:
    """负载均衡配置"""

    strategy: str = "round_robin"  # round_robin | least_connections | weighted | latency_based
    health_check_interval: int = 10
    instances: list[LangGraphInstance] = field(default_factory=list)


# ============ 异常定义 ============


class LangGraphProxyError(Exception):
    """LangGraph 代理错误基类"""

    pass


class NoHealthyInstanceError(LangGraphProxyError):
    """没有健康的实例可用"""

    def __init__(self):
        super().__init__("No healthy LangGraph instances available")


class ForbiddenError(LangGraphProxyError):
    """权限拒绝"""

    def __init__(self, message: str = "Access denied"):
        super().__init__(message)


class QuotaExceededError(LangGraphProxyError):
    """配额超限"""

    def __init__(self, message: str):
        super().__init__(message)


class ThreadNotFoundError(LangGraphProxyError):
    """Thread 不存在"""

    def __init__(self, thread_id: str):
        super().__init__(f"Thread not found: {thread_id}")


class AssistantNotFoundError(LangGraphProxyError):
    """Assistant 不存在"""

    def __init__(self, assistant_id: str):
        super().__init__(f"Assistant not found: {assistant_id}")


class AssistantAccessDeniedError(LangGraphProxyError):
    """Assistant 访问被拒绝"""

    def __init__(self, assistant_id: str):
        super().__init__(f"Access denied to assistant: {assistant_id}")


# ============ 负载均衡器 ============


class LangGraphLoadBalancer:
    """LangGraph 服务负载均衡器"""

    def __init__(self, config: LoadBalancerConfig):
        self.strategy = config.strategy
        self.instances: list[LangGraphInstance] = config.instances
        self._rr_index = -1

    async def select_instance(self, request_type: str = None) -> LangGraphInstance:
        """选择目标实例"""
        del request_type
        healthy = [i for i in self.instances if i.is_healthy]

        if not healthy:
            raise NoHealthyInstanceError()

        if self.strategy == "round_robin":
            return self._round_robin(healthy)
        elif self.strategy == "least_connections":
            return self._least_connections(healthy)
        elif self.strategy == "weighted":
            return self._weighted(healthy)
        elif self.strategy == "latency_based":
            return self._latency_based(healthy)
        else:
            return self._round_robin(healthy)

    def _round_robin(self, instances: list[LangGraphInstance]) -> LangGraphInstance:
        """轮询"""
        self._rr_index = (self._rr_index + 1) % len(instances)
        return instances[self._rr_index]

    def _least_connections(self, instances: list[LangGraphInstance]) -> LangGraphInstance:
        """最少连接"""
        return min(instances, key=lambda i: i.active_connections)

    def _weighted(self, instances: list[LangGraphInstance]) -> LangGraphInstance:
        """加权（根据实例配置的权重）"""
        total_weight = sum(i.weight for i in instances)
        r = random.uniform(0, total_weight)
        cumulative = 0
        for instance in instances:
            cumulative += instance.weight
            if r <= cumulative:
                return instance
        return instances[-1]

    def _latency_based(self, instances: list[LangGraphInstance]) -> LangGraphInstance:
        """基于延迟（选择响应最快的）"""
        return min(instances, key=lambda i: i.avg_latency)


# ============ LangGraph 代理类 ============


class LangGraphProxy:
    """
    LangGraph Server 代理层
    负责：负载均衡、限流检查、请求增强、响应处理
    """

    # 用户层级对应的配额
    THREAD_QUOTAS = {
        "anonymous": 1,
        "normal": 10,
        "premium": 100,
        "enterprise": float("inf"),
        "admin": float("inf"),
    }

    # 缓存 TTL（秒）
    THREAD_CACHE_TTL = 60
    ASSISTANT_CACHE_TTL = 300
    ASSISTANTS_LIST_CACHE_TTL = 60

    def __init__(
        self,
        load_balancer: LangGraphLoadBalancer,
        rate_limiter: Any | None = None,
        redis_client: Any | None = None,
        auth_token: str | None = None,
    ):
        self.lb = load_balancer
        self.rate_limiter = rate_limiter
        self.redis = redis_client  # RedisStorage instance
        self.auth_token = auth_token
        self._clients: dict[str, httpx.AsyncClient] = {}
        self._background_tasks: set[asyncio.Task] = set()
        # L1 缓存: Thread 元数据 {cache_key: (thread_data, timestamp)}
        self._thread_cache: dict[str, tuple] = {}
        # L1 缓存: Assistant 信息 {assistant_id: (assistant_data, timestamp)}
        self._assistant_cache: dict[str, tuple] = {}
        # L1 缓存: Assistants 列表 {user_id: (assistants_list, timestamp)}
        self._assistants_list_cache: dict[str, tuple] = {}

    async def _get_client(self, instance: LangGraphInstance) -> httpx.AsyncClient:
        """获取或创建实例的 HTTP 客户端"""
        if instance.instance_id not in self._clients:
            self._clients[instance.instance_id] = httpx.AsyncClient(
                base_url=instance.url,
                timeout=httpx.Timeout(connect=10.0, read=300.0, write=60.0, pool=60.0),
            )
        return self._clients[instance.instance_id]

    async def close(self) -> None:
        """关闭所有客户端连接"""
        for client in self._clients.values():
            await client.aclose()
        self._clients.clear()

    def _build_langgraph_headers(
        self,
        user: UserContext,
        original_headers: dict[str, str] | None = None,
    ) -> dict[str, str]:
        """
        构建发送给 LangGraph 的 headers

        注入用户信息，供 LangGraph Server 的 Auth 系统使用：
        - Authorization: Bearer token（用于 LangGraph 认证）
        - X-User-Id: 用户标识
        - X-User-Type: 用户类型 (user/guest/anonymous)
        - X-Tenant-Id: 租户标识
        - X-User-Tier: 用户层级
        """
        headers = {
            "Content-Type": "application/json",
            # 关键：注入用户信息
            "X-User-Id": user.user_id,
            "X-User-Type": "user" if user.is_authenticated else "guest",
            "X-Tenant-Id": user.tenant_id or "",
            "X-User-Tier": user.tier,
        }

        token = self.auth_token or os.environ.get("LANGGRAPH_AUTH_TOKEN", "")
        if not token:
            logger.warning("[LangGraphProxy] No auth token configured")

        # 修正：LangGraph Server/SDK 标准认证使用 X-Api-Key Header
        # 用户指出的 "langgraph-auth" 鉴权通常依赖此 Header
        if token:
            headers["X-Api-Key"] = token
            # 同时提供 Bearer Token 以兼容某些自定义 Auth 配置，但 X-Api-Key 是主要的
            headers["Authorization"] = f"Bearer {token}"

        # 其他 Header 透传保持不变
        if original_headers:
            if trace_id := original_headers.get("X-Trace-Id"):
                headers["X-Trace-Id"] = trace_id
            if request_id := original_headers.get("X-Request-Id"):
                headers["X-Request-Id"] = request_id

        return headers

    # ============ Assistants API ============

    async def list_assistants(
        self,
        user: UserContext,
        limit: int = 10,
        offset: int = 0,
        metadata: dict[str, Any] | None = None,
    ) -> list[dict[str, Any]]:
        """
        列出用户可访问的 Assistants（带二级缓存）

        访问规则:
        1. admin 用户可查看所有 assistant
        2. 普通用户只能查看:
           - 自己创建的 assistant
           - 被共享的 assistant
           - 公开的 assistant (is_public=True)
        """
        # 只缓存无特殊参数的列表请求
        use_cache = limit == 10 and offset == 0 and not metadata
        cache_key = user.user_id

        if use_cache:
            # L1: 检查本地缓存
            cached = self._assistants_list_cache.get(cache_key)
            if cached:
                data, cached_at = cached
                if time.time() - cached_at < self.ASSISTANTS_LIST_CACHE_TTL:
                    return data

            # L2: 检查 Redis 缓存
            if self.redis and self.redis.enabled:
                try:
                    cached_data = await self.redis.get_cached_assistants_list(user.user_id)
                    if cached_data:
                        # 回填 L1 缓存
                        self._assistants_list_cache[cache_key] = (cached_data, time.time())
                        return cached_data
                except Exception:
                    pass

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        # 请求更多数据以便过滤后仍有足够结果
        # admin 用户不需要过滤，直接使用原始 limit
        fetch_limit = limit if user.tier == "admin" else limit * 3
        params = {"limit": fetch_limit, "offset": offset}
        if metadata:
            params["metadata"] = json.dumps(metadata)

        response = await client.post("/assistants/search", json=params, headers=headers)
        response.raise_for_status()
        raw_result = response.json()

        # 过滤用户可访问的 assistants
        result = self._filter_accessible_assistants(raw_result, user, limit)

        # 更新缓存
        if use_cache:
            # L1 缓存
            self._assistants_list_cache[cache_key] = (result, time.time())
            # L2 Redis 缓存
            if self.redis and self.redis.enabled:
                with contextlib.suppress(Exception):
                    await self.redis.cache_assistants_list(
                        user.user_id, result, self.ASSISTANTS_LIST_CACHE_TTL
                    )

        return result

    async def get_assistant(
        self,
        user: UserContext,
        assistant_id: str,
        require_write: bool = False,
    ) -> dict[str, Any]:
        """
        获取 Assistant 详情（带二级缓存和所有权验证）

        Args:
            user: 用户上下文
            assistant_id: Assistant ID
            require_write: 是否需要写权限

        Returns:
            Assistant 详情

        Raises:
            AssistantNotFoundError: Assistant 不存在
            AssistantAccessDeniedError: 无访问权限
        """
        # 使用用户特定的缓存键，防止跨用户数据泄露
        cache_key = f"{assistant_id}:{user.user_id}"

        # L1: 检查本地缓存
        cached = self._assistant_cache.get(cache_key)
        if cached:
            data, cached_at = cached
            if time.time() - cached_at < self.ASSISTANT_CACHE_TTL:
                # 缓存命中也需要验证权限（防止权限变更后缓存残留）
                self._verify_assistant_ownership(data, user, require_write)
                return data

        # L2: 检查 Redis 缓存
        if self.redis and self.redis.enabled:
            try:
                cached_data = await self.redis.get_cached_assistant(assistant_id)
                if cached_data:
                    # 验证所有权
                    self._verify_assistant_ownership(cached_data, user, require_write)
                    # 回填 L1 缓存（用户特定）
                    self._assistant_cache[cache_key] = (cached_data, time.time())
                    return cached_data
            except AssistantAccessDeniedError:
                raise
            except Exception:
                pass

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        response = await client.get(f"/assistants/{assistant_id}", headers=headers)
        if response.status_code == 404:
            raise AssistantNotFoundError(assistant_id)
        response.raise_for_status()
        result = response.json()

        # 验证所有权
        self._verify_assistant_ownership(result, user, require_write)

        # 更新缓存
        # L1 缓存（用户特定）
        self._assistant_cache[cache_key] = (result, time.time())
        # L2 Redis 缓存（共享，权限在读取时验证）
        if self.redis and self.redis.enabled:
            with contextlib.suppress(Exception):
                await self.redis.cache_assistant(assistant_id, result, self.ASSISTANT_CACHE_TTL)

        return result

    async def create_assistant(
        self,
        user: UserContext,
        graph_id: str,
        config: dict[str, Any] | None = None,
        metadata: dict[str, Any] | None = None,
        name: str | None = None,
    ) -> dict[str, Any]:
        """创建自定义 Assistant（需要企业版权限）"""
        # 权限检查
        if user.tier not in ["enterprise", "admin"]:
            raise ForbiddenError("Custom assistants require enterprise plan")

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        payload: dict[str, Any] = {"graph_id": graph_id}
        if config:
            payload["config"] = config
        if name:
            payload["name"] = name

        # 注入创建者信息
        payload["metadata"] = metadata or {}
        payload["metadata"]["created_by"] = user.user_id
        payload["metadata"]["tenant_id"] = user.tenant_id

        response = await client.post("/assistants", json=payload, headers=headers)
        response.raise_for_status()
        return response.json()

    # ============ Threads API ============

    async def create_thread(
        self,
        user: UserContext,
        metadata: dict[str, Any] | None = None,
        if_exists: str = "do_nothing",
    ) -> dict[str, Any]:
        """创建 Thread，注入用户元数据"""

        # 检查 Thread 配额
        await self._check_thread_quota(user)

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        # 注入用户元数据
        thread_metadata = metadata or {}
        thread_metadata.update(
            {
                "owner_id": user.user_id,
                "tenant_id": user.tenant_id,
                "user_tier": user.tier,
                "created_at": datetime.utcnow().isoformat(),
            }
        )

        payload = {
            "metadata": thread_metadata,
            "if_exists": if_exists,
        }

        response = await client.post("/threads", json=payload, headers=headers)
        response.raise_for_status()
        thread = response.json()

        # 预填充二级缓存
        thread_id = thread.get("thread_id")
        if thread_id:
            # L1 缓存
            cache_key = f"{thread_id}:{user.user_id}"
            self._thread_cache[cache_key] = (thread, time.time())
            # L2 Redis 缓存
            if self.redis and self.redis.enabled:
                with contextlib.suppress(Exception):
                    await self.redis.cache_thread(thread_id, thread, self.THREAD_CACHE_TTL)

        # 记录用户的 Thread 计数
        await self._increment_thread_count(user)

        return thread

    async def get_thread(
        self, user: UserContext, thread_id: str, use_cache: bool = True
    ) -> dict[str, Any]:
        """获取 Thread，验证所有权（带二级缓存）

        Args:
            use_cache: 是否使用缓存（默认 True，可显著减少延迟）
        """
        cache_key = f"{thread_id}:{user.user_id}"

        if use_cache:
            # L1: 检查本地缓存
            cached = self._thread_cache.get(cache_key)
            if cached:
                thread_data, cached_at = cached
                if time.time() - cached_at < self.THREAD_CACHE_TTL:
                    return thread_data

            # L2: 检查 Redis 缓存
            if self.redis and self.redis.enabled:
                try:
                    cached_data = await self.redis.get_cached_thread(thread_id)
                    if cached_data:
                        # 验证所有权
                        self._verify_ownership(cached_data, user)
                        # 回填 L1 缓存
                        self._thread_cache[cache_key] = (cached_data, time.time())
                        return cached_data
                except Exception:
                    pass

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        response = await client.get(f"/threads/{thread_id}", headers=headers)
        if response.status_code == 404:
            raise ThreadNotFoundError(thread_id)
        response.raise_for_status()

        thread = response.json()

        # 验证所有权
        self._verify_ownership(thread, user)

        # 更新二级缓存
        if use_cache:
            # L1 缓存
            self._thread_cache[cache_key] = (thread, time.time())
            # L2 Redis 缓存
            if self.redis and self.redis.enabled:
                with contextlib.suppress(Exception):
                    await self.redis.cache_thread(thread_id, thread, self.THREAD_CACHE_TTL)

        return thread

    async def update_thread(
        self,
        user: UserContext,
        thread_id: str,
        metadata: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """更新 Thread"""
        # 先验证所有权
        await self.get_thread(user, thread_id)

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        payload = {}
        if metadata:
            payload["metadata"] = metadata

        response = await client.patch(f"/threads/{thread_id}", json=payload, headers=headers)
        response.raise_for_status()

        # 失效二级缓存
        # L1 缓存
        cache_key = f"{thread_id}:{user.user_id}"
        self._thread_cache.pop(cache_key, None)
        # L2 Redis 缓存
        if self.redis and self.redis.enabled:
            with contextlib.suppress(Exception):
                await self.redis.invalidate_thread(thread_id)

        return response.json()

    async def delete_thread(self, user: UserContext, thread_id: str) -> None:
        """删除 Thread"""
        # 先验证所有权
        await self.get_thread(user, thread_id)

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        response = await client.delete(f"/threads/{thread_id}", headers=headers)
        response.raise_for_status()

        # 失效二级缓存
        # L1 缓存
        cache_key = f"{thread_id}:{user.user_id}"
        self._thread_cache.pop(cache_key, None)
        # L2 Redis 缓存
        if self.redis and self.redis.enabled:
            with contextlib.suppress(Exception):
                await self.redis.invalidate_thread(thread_id)

        # 减少 Thread 计数
        await self._decrement_thread_count(user)

    async def search_threads(
        self,
        user: UserContext,
        metadata: dict[str, Any] | None = None,
        limit: int = 10,
        offset: int = 0,
    ) -> list[dict[str, Any]]:
        """搜索用户的 Threads"""
        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        # Search threads — admin sees all, non-admin filtered by owner_id
        # First search without owner filter (catches legacy threads without owner_id)
        search_metadata = metadata or {}
        is_admin = user.tier == "admin" or getattr(user, "role", "") == "admin" or user.user_id == "admin"

        if not is_admin and user.user_id:
            # For non-admin: search with owner_id filter first, then fallback to all
            search_metadata["owner_id"] = user.user_id

        payload = {
            "metadata": search_metadata,
            "limit": limit,
            "offset": offset,
        }

        response = await client.post("/threads/search", json=payload, headers=headers)
        response.raise_for_status()
        results = response.json()

        # No unfiltered fallback for non-admin — prevents cross-user thread leakage.
        # Legacy threads without owner_id are only visible to admin users.

        return results

    async def get_thread_state(self, user: UserContext, thread_id: str) -> dict[str, Any]:
        """获取 Thread 状态"""
        # 先验证所有权
        await self.get_thread(user, thread_id)

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        response = await client.get(f"/threads/{thread_id}/state", headers=headers)
        response.raise_for_status()
        return response.json()

    async def update_thread_state(
        self,
        user: UserContext,
        thread_id: str,
        values: dict[str, Any],
        as_node: str | None = None,
    ) -> dict[str, Any]:
        """更新 Thread 状态"""
        # 先验证所有权
        await self.get_thread(user, thread_id)

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        payload: dict[str, Any] = {"values": values}
        if as_node:
            payload["as_node"] = as_node

        response = await client.post(f"/threads/{thread_id}/state", json=payload, headers=headers)
        response.raise_for_status()
        return response.json()

    async def get_thread_history(
        self,
        user: UserContext,
        thread_id: str,
        limit: int = 10,
        before: str | None = None,
    ) -> list[dict[str, Any]]:
        """获取 Thread 历史"""
        # 先验证所有权
        await self.get_thread(user, thread_id)

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        params = {"limit": limit}
        if before:
            params["before"] = before

        response = await client.get(f"/threads/{thread_id}/history", params=params, headers=headers)
        response.raise_for_status()
        return response.json()

    # ============ Runs API ============

    async def _prepare_run_payload(
        self,
        *,
        payload: dict[str, Any],
        path: str,
        user: UserContext,
        http_request: Request | None,
        assistant_payload: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        if http_request is None:
            logger.warning(
                "LangGraph run prep without http_request; applying sync scrub and configurable injection only"
            )
            minimal_request = build_minimal_langgraph_prep_request()
            assistant_id = str(payload.get("assistant_id") or "").strip() or None
            service_config = synthetic_langgraph_service_config(assistant_id=assistant_id)
            prepare_langgraph_run_payload(
                payload,
                method="POST",
                path=path,
                request=minimal_request,
                user=user,
                auth=user,
                service_config=service_config,
                assistant_payload=assistant_payload,
            )
            return payload

        return await prepare_and_finalize_langgraph_run_payload(
            payload,
            method="POST",
            path=path,
            request=http_request,
            user=user,
            assistant_id=str(payload.get("assistant_id") or "").strip() or None,
            assistant_payload=assistant_payload,
        )

    async def create_run(
        self,
        user: UserContext,
        thread_id: str,
        assistant_id: str,
        input_data: dict[str, Any],
        config: dict[str, Any] | None = None,
        metadata: dict[str, Any] | None = None,
        webhook: str | None = None,
        interrupt_before: list[str] | None = None,
        interrupt_after: list[str] | None = None,
        http_request: Request | None = None,
    ) -> dict[str, Any]:
        """创建 Run（异步执行，立即返回）"""
        import asyncio

        # 性能优化：并发执行验证调用
        validation_results = await asyncio.gather(
            self.get_thread(user, thread_id),
            self.get_assistant(user, assistant_id),
        )
        assistant_payload = (
            validation_results[1]
            if len(validation_results) > 1 and isinstance(validation_results[1], dict)
            else None
        )

        instance = await self.lb.select_instance()
        instance.active_connections += 1
        start_time = time.time()
        run_status = "success"

        try:
            client = await self._get_client(instance)
            headers = self._build_langgraph_headers(user)

            payload: dict[str, Any] = {
                "assistant_id": assistant_id,
                "input": input_data,
                "config": config or {},
            }
            if metadata:
                payload["metadata"] = metadata
            if webhook:
                payload["webhook"] = webhook
            if interrupt_before:
                payload["interrupt_before"] = interrupt_before
            if interrupt_after:
                payload["interrupt_after"] = interrupt_after

            payload = await self._prepare_run_payload(
                payload=payload,
                path=f"threads/{thread_id}/runs",
                user=user,
                http_request=http_request,
                assistant_payload=assistant_payload,
            )

            response = await client.post(
                f"/threads/{thread_id}/runs", json=payload, headers=headers
            )
            response.raise_for_status()
            return response.json()
        except Exception:
            run_status = "error"
            raise
        finally:
            instance.active_connections -= 1
            # Record run metrics (async run, no token info available yet)
            duration_ms = (time.time() - start_time) * 1000
            try:
                metrics_recorder = get_metrics_recorder()
                await metrics_recorder.record_run(
                    user_id=user.user_id,
                    assistant_id=assistant_id,
                    duration_ms=duration_ms,
                    status=run_status,
                    tenant_id=user.tenant_id,
                )
            except Exception:
                pass

            # Record usage to PostgreSQL (async run, tokens will be 0)
            try:
                usage_recorder = get_usage_recorder()
                await usage_recorder.record_usage(
                    tenant_id=user.tenant_id,
                    user_id=user.user_id,
                    model="langgraph-agent",
                    input_tokens=0,
                    output_tokens=0,
                    service_id="langgraph",
                    assistant_id=assistant_id,
                    latency_ms=int(duration_ms),
                    request_type="run_async",
                    status=run_status,
                )
            except Exception:
                pass

    async def create_run_wait(
        self,
        user: UserContext,
        thread_id: str,
        assistant_id: str,
        input_data: dict[str, Any],
        config: dict[str, Any] | None = None,
        metadata: dict[str, Any] | None = None,
        http_request: Request | None = None,
    ) -> dict[str, Any]:
        """创建 Run 并等待完成"""
        import asyncio

        # 性能优化：并发执行验证调用
        validation_results = await asyncio.gather(
            self.get_thread(user, thread_id),
            self.get_assistant(user, assistant_id),
        )
        assistant_payload = (
            validation_results[1]
            if len(validation_results) > 1 and isinstance(validation_results[1], dict)
            else None
        )

        instance = await self.lb.select_instance()
        instance.active_connections += 1
        start_time = time.time()
        run_status = "success"
        input_tokens = 0
        output_tokens = 0

        try:
            client = await self._get_client(instance)
            headers = self._build_langgraph_headers(user)

            payload: dict[str, Any] = {
                "assistant_id": assistant_id,
                "input": input_data,
                "config": config or {},
            }
            if metadata:
                payload["metadata"] = metadata

            payload = await self._prepare_run_payload(
                payload=payload,
                path=f"threads/{thread_id}/runs/wait",
                user=user,
                http_request=http_request,
                assistant_payload=assistant_payload,
            )

            response = await client.post(
                f"/threads/{thread_id}/runs/wait", json=payload, headers=headers
            )
            try:
                response.raise_for_status()
            except httpx.HTTPStatusError as e:
                logger.error(f"LangGraph create_run_wait failed: {e.response.text}")
                # 抛出包含详细错误信息的异常，以便前端展示
                raise LangGraphProxyError(f"LangGraph Auth Error: {e.response.text}") from e

            result = response.json()

            # Extract token usage from response if available
            usage = result.get("usage", {})
            input_tokens = usage.get("prompt_tokens") or usage.get("input_tokens") or 0
            output_tokens = usage.get("completion_tokens") or usage.get("output_tokens") or 0

            return result
        except Exception:
            run_status = "error"
            raise
        finally:
            instance.active_connections -= 1
            # Record run metrics
            duration_ms = (time.time() - start_time) * 1000
            try:
                metrics_recorder = get_metrics_recorder()
                await metrics_recorder.record_run(
                    user_id=user.user_id,
                    assistant_id=assistant_id,
                    duration_ms=duration_ms,
                    status=run_status,
                    input_tokens=input_tokens,
                    output_tokens=output_tokens,
                    tenant_id=user.tenant_id,
                )
            except Exception:
                pass

            # Record usage to PostgreSQL for billing/analytics
            try:
                usage_recorder = get_usage_recorder()
                await usage_recorder.record_usage(
                    tenant_id=user.tenant_id,
                    user_id=user.user_id,
                    model="langgraph-agent",
                    input_tokens=input_tokens,
                    output_tokens=output_tokens,
                    service_id="langgraph",
                    assistant_id=assistant_id,
                    latency_ms=int(duration_ms),
                    request_type="run",
                    status=run_status,
                )
            except Exception:
                pass

            # Update real-time metrics in Redis for dashboard
            try:
                realtime_metrics = get_realtime_metrics()
                if realtime_metrics and (input_tokens > 0 or output_tokens > 0):
                    await realtime_metrics.record_token_usage(input_tokens, output_tokens)
            except Exception:
                pass

    async def stream_run(
        self,
        user: UserContext,
        thread_id: str | None,
        assistant_id: str,
        input_data: dict[str, Any],
        config: dict[str, Any] | None = None,
        metadata: dict[str, Any] | None = None,
        stream_mode: list[str] | None = None,
        skip_thread_validation: bool = False,
        http_request: Request | None = None,
    ) -> AsyncIterator[dict[str, Any]]:
        """流式执行 Run

        Args:
            skip_thread_validation: 跳过 thread 所有权验证（当调用方已验证时使用）
        """
        import asyncio

        # 性能优化：并发执行验证调用以减少首 token 延迟
        # 之前是顺序执行，每个调用 100-500ms，总计 200-1000ms
        # 现在并发执行，总耗时 = max(get_assistant, get_thread) ≈ 100-500ms
        validation_start = time.time()

        validation_tasks = [self.get_assistant(user, assistant_id)]
        if thread_id and not skip_thread_validation:
            validation_tasks.append(self.get_thread(user, thread_id))

        # 并发执行所有验证
        validation_results = await asyncio.gather(*validation_tasks)
        assistant_payload = (
            validation_results[0]
            if validation_results and isinstance(validation_results[0], dict)
            else None
        )

        validation_ms = (time.time() - validation_start) * 1000
        logger.debug(f"[TIMING] stream_run validation: {validation_ms:.2f}ms (concurrent)")

        # 确定 endpoint
        endpoint = f"/threads/{thread_id}/runs/stream" if thread_id else "/runs/stream"
        prep_path = endpoint.lstrip("/")

        instance = await self.lb.select_instance()
        instance.active_connections += 1

        # Metrics tracking
        start_time = time.time()
        token_tracker = {"input": 0, "output": 0}
        run_status = "success"

        try:
            client = await self._get_client(instance)
            headers = self._build_langgraph_headers(user)

            payload: dict[str, Any] = {
                "assistant_id": assistant_id,
                "input": input_data,
                "config": config or {},
            }
            if metadata:
                payload["metadata"] = metadata
            if stream_mode:
                payload["stream_mode"] = stream_mode

            payload = await self._prepare_run_payload(
                payload=payload,
                path=prep_path,
                user=user,
                http_request=http_request,
                assistant_payload=assistant_payload,
            )

            async with client.stream("POST", endpoint, json=payload, headers=headers) as response:
                response.raise_for_status()

                current_event = ""
                async for line in response.aiter_lines():
                    if not line:
                        current_event = ""
                        continue

                    if line.startswith("event:"):
                        current_event = line[6:].strip()
                        continue

                    if line.startswith("data:"):
                        data_str = line[5:].strip()
                        if data_str and data_str != "[DONE]":
                            try:
                                data = json.loads(data_str)

                                # Extract token usage from stream events (only if data is dict)
                                if isinstance(data, dict):
                                    extracted = self._extract_token_usage(current_event, data)
                                    token_tracker["input"] += extracted.get("input", 0)
                                    token_tracker["output"] += extracted.get("output", 0)

                                yield {"event": current_event, "data": data}
                            except json.JSONDecodeError:
                                continue

        except Exception:
            run_status = "error"
            raise
        finally:
            instance.active_connections -= 1

            # Record run metrics
            duration_ms = (time.time() - start_time) * 1000
            try:
                metrics_recorder = get_metrics_recorder()
                await metrics_recorder.record_run(
                    user_id=user.user_id,
                    assistant_id=assistant_id,
                    duration_ms=duration_ms,
                    status=run_status,
                    input_tokens=token_tracker["input"],
                    output_tokens=token_tracker["output"],
                    tenant_id=user.tenant_id,
                )
            except Exception as metrics_err:
                logger.debug(f"Failed to record run metrics: {metrics_err}")

            # Record usage to PostgreSQL for billing/analytics
            input_tokens = token_tracker["input"]
            output_tokens = token_tracker["output"]
            try:
                usage_recorder = get_usage_recorder()
                await usage_recorder.record_usage(
                    tenant_id=user.tenant_id,
                    user_id=user.user_id,
                    model="langgraph-agent",  # Generic model name for agent runs
                    input_tokens=input_tokens,
                    output_tokens=output_tokens,
                    service_id="langgraph",
                    assistant_id=assistant_id,
                    latency_ms=int(duration_ms),
                    request_type="run",
                    status=run_status,
                )
            except Exception as usage_err:
                logger.debug(f"Failed to record usage: {usage_err}")

            # Update real-time metrics in Redis for dashboard
            try:
                realtime_metrics = get_realtime_metrics()
                if realtime_metrics and (input_tokens > 0 or output_tokens > 0):
                    await realtime_metrics.record_token_usage(input_tokens, output_tokens)
            except Exception:
                pass

    def _extract_token_usage(
        self,
        event: str,
        data: dict[str, Any],
    ) -> dict[str, int]:
        """Extract token usage from stream event data

        Supports various formats from different LLM providers:
        - OpenAI format: usage.prompt_tokens, usage.completion_tokens
        - Anthropic format: usage.input_tokens, usage.output_tokens
        - LangGraph metadata format: metadata.usage.*

        Returns:
            Dict with 'input' and 'output' token counts
        """
        input_tokens = 0
        output_tokens = 0

        # Check for usage in data directly
        usage = data.get("usage") or data.get("data", {}).get("usage", {})
        if usage:
            input_tokens = usage.get("prompt_tokens") or usage.get("input_tokens") or 0
            output_tokens = usage.get("completion_tokens") or usage.get("output_tokens") or 0

        # Check metadata
        metadata = data.get("metadata", {})
        if metadata and "usage" in metadata:
            meta_usage = metadata["usage"]
            input_tokens = max(
                input_tokens,
                meta_usage.get("prompt_tokens", 0) or meta_usage.get("input_tokens", 0),
            )
            output_tokens = max(
                output_tokens,
                meta_usage.get("completion_tokens", 0) or meta_usage.get("output_tokens", 0),
            )

        # Check for messages event with usage
        if event == "messages" and isinstance(data, dict):
            msg_usage = data.get("usage", {})
            if msg_usage:
                input_tokens = max(
                    input_tokens,
                    msg_usage.get("prompt_tokens", 0) or msg_usage.get("input_tokens", 0),
                )
                output_tokens = max(
                    output_tokens,
                    msg_usage.get("completion_tokens", 0) or msg_usage.get("output_tokens", 0),
                )

        return {"input": input_tokens, "output": output_tokens}

    async def get_run(
        self,
        user: UserContext,
        thread_id: str,
        run_id: str,
    ) -> dict[str, Any]:
        """获取 Run 详情"""
        # 验证 Thread 所有权
        await self.get_thread(user, thread_id)

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        response = await client.get(f"/threads/{thread_id}/runs/{run_id}", headers=headers)
        response.raise_for_status()
        return response.json()

    async def list_runs(
        self,
        user: UserContext,
        thread_id: str,
        limit: int = 10,
        offset: int = 0,
    ) -> list[dict[str, Any]]:
        """列出 Thread 的 Runs"""
        # 验证 Thread 所有权
        await self.get_thread(user, thread_id)

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        params = {"limit": limit, "offset": offset}
        response = await client.get(f"/threads/{thread_id}/runs", params=params, headers=headers)
        response.raise_for_status()
        return response.json()

    async def cancel_run(
        self,
        user: UserContext,
        thread_id: str,
        run_id: str,
        wait: bool = False,
    ) -> None:
        """取消 Run"""
        # 验证 Thread 所有权
        await self.get_thread(user, thread_id)

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        params = {"wait": wait}
        response = await client.post(
            f"/threads/{thread_id}/runs/{run_id}/cancel", params=params, headers=headers
        )
        response.raise_for_status()

    # ============ Threadless Runs ============

    async def create_stateless_run(
        self,
        user: UserContext,
        assistant_id: str,
        input_data: dict[str, Any],
        config: dict[str, Any] | None = None,
        metadata: dict[str, Any] | None = None,
        http_request: Request | None = None,
    ) -> dict[str, Any]:
        """创建无状态 Run（一次性对话）"""
        # 验证 Assistant 访问权限（关键！防止未授权访问任意 assistant）
        assistant_payload = await self.get_assistant(user, assistant_id)

        instance = await self.lb.select_instance()
        instance.active_connections += 1
        start_time = time.time()
        run_status = "success"
        input_tokens = 0
        output_tokens = 0

        try:
            client = await self._get_client(instance)
            headers = self._build_langgraph_headers(user)

            payload: dict[str, Any] = {
                "assistant_id": assistant_id,
                "input": input_data,
                "config": config or {},
            }
            if metadata:
                payload["metadata"] = metadata

            payload = await self._prepare_run_payload(
                payload=payload,
                path="runs/wait",
                user=user,
                http_request=http_request,
                assistant_payload=assistant_payload
                if isinstance(assistant_payload, dict)
                else None,
            )

            response = await client.post("/runs/wait", json=payload, headers=headers)
            response.raise_for_status()
            result = response.json()

            # Extract token usage from response if available
            usage = result.get("usage", {})
            input_tokens = usage.get("prompt_tokens") or usage.get("input_tokens") or 0
            output_tokens = usage.get("completion_tokens") or usage.get("output_tokens") or 0

            return result
        except Exception:
            run_status = "error"
            raise
        finally:
            instance.active_connections -= 1
            # Record run metrics
            duration_ms = (time.time() - start_time) * 1000
            try:
                metrics_recorder = get_metrics_recorder()
                await metrics_recorder.record_run(
                    user_id=user.user_id,
                    assistant_id=assistant_id,
                    duration_ms=duration_ms,
                    status=run_status,
                    input_tokens=input_tokens,
                    output_tokens=output_tokens,
                    tenant_id=user.tenant_id,
                )
            except Exception:
                pass

            # Record usage to PostgreSQL for billing/analytics
            try:
                usage_recorder = get_usage_recorder()
                await usage_recorder.record_usage(
                    tenant_id=user.tenant_id,
                    user_id=user.user_id,
                    model="langgraph-agent",
                    input_tokens=input_tokens,
                    output_tokens=output_tokens,
                    service_id="langgraph",
                    assistant_id=assistant_id,
                    latency_ms=int(duration_ms),
                    request_type="run",
                    status=run_status,
                )
            except Exception:
                pass

            # Update real-time metrics in Redis for dashboard
            try:
                realtime_metrics = get_realtime_metrics()
                if realtime_metrics and (input_tokens > 0 or output_tokens > 0):
                    await realtime_metrics.record_token_usage(input_tokens, output_tokens)
            except Exception:
                pass

    # ============ Store API (Memory) ============

    async def store_get(
        self,
        user: UserContext,
        namespace: list[str],
        key: str,
    ) -> dict[str, Any] | None:
        """获取记忆，验证 namespace 权限"""

        if not self._validate_namespace_access(namespace, user):
            raise ForbiddenError("Access denied to this namespace")

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        params = {"namespace": json.dumps(namespace), "key": key}
        response = await client.get("/store/items", params=params, headers=headers)

        if response.status_code == 404:
            return None
        response.raise_for_status()
        return response.json()

    async def store_put(
        self,
        user: UserContext,
        namespace: list[str],
        key: str,
        value: dict[str, Any],
    ) -> None:
        """写入记忆，验证权限"""

        if not self._validate_namespace_access(namespace, user, write=True):
            raise ForbiddenError("Access denied to this namespace")

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        payload = {
            "namespace": namespace,
            "key": key,
            "value": value,
        }

        response = await client.post("/store/items", json=payload, headers=headers)
        response.raise_for_status()

    async def store_delete(
        self,
        user: UserContext,
        namespace: list[str],
        key: str,
    ) -> None:
        """删除记忆项"""

        if not self._validate_namespace_access(namespace, user, write=True):
            raise ForbiddenError("Access denied to this namespace")

        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        params = {"namespace": json.dumps(namespace), "key": key}
        response = await client.delete("/store/items", params=params, headers=headers)
        response.raise_for_status()

    async def store_list_namespaces(
        self,
        user: UserContext,
        prefix: list[str] | None = None,
    ) -> list[list[str]]:
        """列出命名空间"""
        instance = await self.lb.select_instance()
        client = await self._get_client(instance)
        headers = self._build_langgraph_headers(user)

        # 限制只能看到自己的命名空间
        if user.tier != "admin":
            prefix = ["user", user.user_id] + (prefix or [])

        params = {}
        if prefix:
            params["prefix"] = json.dumps(prefix)

        response = await client.get("/store/namespaces", params=params, headers=headers)
        response.raise_for_status()
        return response.json()

    # ============ 辅助方法 ============

    def _verify_ownership(self, thread: dict[str, Any], user: UserContext) -> None:
        """验证 Thread 所有权"""
        metadata = thread.get("metadata", {})
        owner_id = metadata.get("owner_id")

        # 管理员可访问所有
        if user.tier == "admin":
            return

        # 同租户可访问（企业版）
        if user.tier == "enterprise" and metadata.get("tenant_id") == user.tenant_id:
            return

        # 所有者
        if owner_id == user.user_id:
            return

        raise ForbiddenError("Access denied to this thread")

    def _verify_assistant_ownership(
        self,
        assistant: dict[str, Any],
        user: UserContext,
        require_write: bool = False,
    ) -> None:
        """
        验证 Assistant 所有权/访问权限

        访问规则（按优先级）:
        1. admin 用户可访问所有 assistant
        2. 创建者（created_by）可完全访问
        3. shared_with 列表中的用户可读取
        4. is_public=True 的 assistant 所有人可读取

        Args:
            assistant: Assistant 数据
            user: 当前用户上下文
            require_write: 是否需要写权限（更新/删除操作）

        Raises:
            AssistantAccessDeniedError: 权限不足
        """
        metadata = assistant.get("metadata") or {}
        created_by = metadata.get("created_by")
        is_public = self._coerce_bool(metadata.get("is_public", False))
        shared_with = self._normalize_shared_with(metadata.get("shared_with"))
        assistant_id = assistant.get("assistant_id", "unknown")

        # 1. 管理员可访问所有
        if user.tier == "admin":
            return

        # 2. 创建者拥有完全访问权
        if created_by == user.user_id:
            return

        # 写操作只允许创建者和管理员
        if require_write:
            raise AssistantAccessDeniedError(assistant_id)

        # 3. 检查是否在共享列表中
        if user.user_id in shared_with:
            return

        # 4. 公开 assistant 所有人可读
        if is_public:
            return

        # 默认拒绝访问
        raise AssistantAccessDeniedError(assistant_id)

    @staticmethod
    def _normalize_shared_with(shared_with: Any) -> list[str]:
        if not shared_with:
            return []
        if isinstance(shared_with, str):
            return [shared_with]
        if isinstance(shared_with, list):
            return [str(item) for item in shared_with if item]
        return []

    @staticmethod
    def _coerce_bool(value: Any) -> bool:
        if isinstance(value, bool):
            return value
        if isinstance(value, str):
            return value.strip().lower() in {"true", "1", "yes", "y"}
        if isinstance(value, (int, float)):
            return value != 0
        return bool(value)

    def _filter_accessible_assistants(
        self,
        assistants: Any,
        user: UserContext,
        limit: int,
    ) -> Any:
        """
        过滤用户可访问的 Assistants

        访问规则与 _verify_assistant_ownership 一致:
        1. admin 用户可访问所有
        2. 创建者可访问
        3. shared_with 列表中的用户可访问
        4. is_public=True 的可访问
        """
        if isinstance(assistants, dict):
            list_key = None
            for key in ("items", "assistants", "data", "results"):
                if isinstance(assistants.get(key), list):
                    list_key = key
                    break
            if not list_key:
                return assistants
            filtered_list = self._filter_accessible_assistants(assistants[list_key], user, limit)
            payload = dict(assistants)
            payload[list_key] = filtered_list
            if "count" in payload:
                payload["count"] = len(filtered_list)
            if "total" in payload and isinstance(payload["total"], int):
                payload["total"] = len(filtered_list)
            return payload

        if not isinstance(assistants, list):
            return assistants

        # admin 用户返回所有结果
        if user.tier == "admin":
            return assistants[:limit]

        filtered = []
        for assistant in assistants:
            metadata = assistant.get("metadata") or {}
            created_by = metadata.get("created_by")
            is_public = self._coerce_bool(metadata.get("is_public", False))
            shared_with = self._normalize_shared_with(metadata.get("shared_with"))

            # 检查访问权限
            can_access = False

            # 创建者
            if created_by == user.user_id or user.user_id in shared_with or is_public:
                can_access = True

            if can_access:
                filtered.append(assistant)
                if len(filtered) >= limit:
                    break

        return filtered

    def _invalidate_assistant_cache(self, assistant_id: str) -> None:
        """使 Assistant 缓存失效（包括所有用户特定的缓存）"""
        # 删除所有以该 assistant_id 开头的缓存键
        keys_to_remove = [k for k in self._assistant_cache if k.startswith(f"{assistant_id}:")]
        for key in keys_to_remove:
            self._assistant_cache.pop(key, None)

        # 失效 Redis 缓存
        if self.redis and self.redis.enabled:
            task = asyncio.create_task(
                self._async_invalidate_redis_assistant(assistant_id)
            )
            self._background_tasks.add(task)
            task.add_done_callback(self._background_tasks.discard)

    async def _async_invalidate_redis_assistant(self, assistant_id: str) -> None:
        """异步失效 Redis 中的 Assistant 缓存"""
        with contextlib.suppress(Exception):
            await self.redis.delete(f"lg:assistant:{assistant_id}")

    @staticmethod
    def _inject_gateway_domain_policy_metadata(
        *,
        metadata: dict[str, Any] | None,
        assistant_payload: dict[str, Any] | None,
    ) -> dict[str, Any] | None:
        """Backward-compatible wrapper around the shared LangGraph run preparer."""
        from ..proxy.config_loader import ProxyServiceConfig

        service_config = ProxyServiceConfig(
            service_id="langgraph",
            service_name="langgraph",
            upstream_url="",
            metadata={"adapter_type": "langgraph"},
        )
        return merge_gateway_domain_policy_metadata(
            metadata=metadata,
            service_config=service_config,
            assistant_payload=assistant_payload,
        )

    def _build_run_config(
        self, user: UserContext, config: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        """构建 Run 配置，注入用户上下文"""
        run_config = config or {}
        run_config["configurable"] = run_config.get("configurable", {})

        # 注入用户信息（供 Agent 使用）
        run_config["configurable"]["user_id"] = user.user_id
        run_config["configurable"]["user_tier"] = user.tier
        run_config["configurable"]["tenant_id"] = user.tenant_id
        # Namespacing for checkpointing/memory where supported by the upstream runtime.
        run_config["configurable"]["checkpoint_ns"] = user.tenant_id or ""

        return run_config

    async def _check_thread_quota(self, user: UserContext) -> None:
        """检查 Thread 配额"""
        max_threads = self.THREAD_QUOTAS.get(user.tier, 1)

        if max_threads == float("inf"):
            return

        current = await self._count_user_threads(user.user_id)

        if current >= max_threads:
            raise QuotaExceededError(f"Thread quota exceeded: {current}/{int(max_threads)}")

    async def _count_user_threads(self, user_id: str) -> float:
        """统计用户的 Thread 数量

        When Redis is unavailable (or errors), fail-closed: return ``inf`` so
        ``_check_thread_quota`` denies all non-admin/non-enterprise tiers,
        matching prod-with-Redis behavior. Admin and enterprise tiers have a
        ``float("inf")`` quota and are short-circuited before this is called,
        so they remain unaffected.
        """
        if self.redis and self.redis.enabled:
            try:
                return await self.redis.get_user_thread_count(user_id)
            except Exception as exc:
                logger.warning(
                    "Redis thread-count lookup failed for user=%s; defaulting to deny: %s",
                    user_id, exc,
                )
                return float("inf")
        logger.warning(
            "Redis unavailable; thread quota check defaulting to deny for user=%s "
            "(set Redis up or whitelist tier)",
            user_id,
        )
        return float("inf")

    async def _increment_thread_count(self, user: UserContext) -> None:
        """增加 Thread 计数"""
        if self.redis and self.redis.enabled:
            with contextlib.suppress(Exception):
                await self.redis.incr_user_thread_count(user.user_id)

    async def _decrement_thread_count(self, user: UserContext) -> None:
        """减少 Thread 计数"""
        if self.redis and self.redis.enabled:
            with contextlib.suppress(Exception):
                await self.redis.decr_user_thread_count(user.user_id)

    def _validate_namespace_access(
        self,
        namespace: list[str],
        user: UserContext,
        write: bool = False,
    ) -> bool:
        """验证用户是否有权访问该 namespace"""
        if not namespace:
            return False

        # 管理员可访问所有
        if user.tier == "admin":
            return True

        # 全局 namespace 所有人可读，但只有管理员可写
        if namespace[0] == "global":
            return not write

        # 用户 namespace 只能访问自己的
        if namespace[0] == "user":
            return len(namespace) > 1 and namespace[1] == user.user_id

        # 租户 namespace（企业版功能）
        if namespace[0] == "tenant":
            if user.tier not in ["premium", "enterprise", "admin"]:
                return False
            return len(namespace) > 1 and namespace[1] == user.tenant_id

        return False


# ============ 记忆命名空间注入器 ============


class MemoryNamespaceInjector:
    """
    在请求转发到 LangGraph 之前，注入用户相关的 namespace
    确保用户只能访问自己的记忆
    """

    @staticmethod
    def inject_user_namespace(
        namespace: list[str],
        user: UserContext,
    ) -> list[str]:
        """为 namespace 添加用户前缀"""
        if not namespace:
            return ["user", user.user_id]

        # 如果已经有 user 前缀，验证是否是本人
        if namespace[0] == "user":
            if len(namespace) > 1 and namespace[1] != user.user_id:
                raise ForbiddenError("Cannot access other user's namespace")
            return namespace

        # 如果是全局 namespace，不修改
        if namespace[0] == "global":
            return namespace

        # 其他情况，添加用户前缀
        return ["user", user.user_id] + namespace

    @staticmethod
    def validate_namespace_access(
        namespace: list[str],
        user: UserContext,
    ) -> bool:
        """验证用户是否有权访问该 namespace"""
        if not namespace:
            return False

        # 全局 namespace 所有人可读
        if namespace[0] == "global":
            return True

        # 用户 namespace 只能访问自己的
        if namespace[0] == "user":
            return len(namespace) > 1 and namespace[1] == user.user_id

        # 租户 namespace
        if namespace[0] == "tenant":
            return len(namespace) > 1 and namespace[1] == user.tenant_id

        return False
