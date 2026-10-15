from __future__ import annotations

import asyncio
import time
from collections import deque
from dataclasses import dataclass

from ...models.request import UnifiedRequest
from ...models.service import ServiceDefinition
from ai_gateway_core.exceptions import RateLimitExceededError


@dataclass
class RateLimit:
    requests: int
    window: int
    burst: int = 0
    strategy: str = "sliding_window"


@dataclass
class RateLimitConfig:
    global_limit: RateLimit | None = None
    tenant_limits: dict[str, RateLimit] | None = None
    user_limits: dict[str, RateLimit] | None = None
    service_limits: dict[str, RateLimit] | None = None
    ip_limits: RateLimit | None = None


class RateLimiter:
    """
    基于滑动窗口的速率限制器。

    优化点：
    - 按 key 分片锁，减少锁竞争
    - 定期清理过期数据，防止内存无限增长
    - 优先使用新格式配置，避免重复检查
    """

    def __init__(self, config: RateLimitConfig, max_entries: int = 10000):
        self.config = config
        self._requests: dict[str, deque[float]] = {}
        # 按 key 分片的锁，减少全局锁竞争
        self._locks: dict[str, asyncio.Lock] = {}
        self._global_lock = asyncio.Lock()
        self._max_entries = max_entries
        self._last_cleanup = time.time()
        self._cleanup_interval = 60  # 每 60 秒清理一次过期数据

    async def _get_lock(self, key: str) -> asyncio.Lock:
        """获取指定 key 的锁（如果不存在则创建）

        使用双重检查锁定模式确保线程安全。
        """
        if key not in self._locks:
            async with self._global_lock:
                # 双重检查，避免多个协程同时创建锁
                if key not in self._locks:
                    self._locks[key] = asyncio.Lock()
        return self._locks[key]

    async def _cleanup_expired(self, now: float) -> None:
        """清理过期数据，防止内存无限增长"""
        if now - self._last_cleanup < self._cleanup_interval:
            return

        async with self._global_lock:
            if now - self._last_cleanup < self._cleanup_interval:
                return

            # 获取所有配置中的最大窗口时间
            max_window = 60  # 默认 60 秒
            for limit in [
                self.config.global_limit,
                self.config.ip_limits,
                *(self.config.tenant_limits or {}).values(),
                *(self.config.user_limits or {}).values(),
                *(self.config.service_limits or {}).values(),
            ]:
                if limit and limit.window > max_window:
                    max_window = limit.window

            cutoff = now - max_window
            # 先获取所有 key 的快照，避免在遍历时修改字典
            all_keys = list(self._requests.keys())

            expired_keys = [
                key
                for key in all_keys
                if key in self._requests
                and self._requests[key]
                and self._requests[key][-1] < cutoff
            ]

            for key in expired_keys:
                self._requests.pop(key, None)
                self._locks.pop(key, None)

            self._last_cleanup = now

    async def _check_key(self, key: str, limit: RateLimit) -> None:
        now = time.time()

        # 定期清理过期数据
        await self._cleanup_expired(now)

        # 使用按 key 分片的锁，避免全局锁竞争
        lock = await self._get_lock(key)
        async with lock:
            timestamps = self._requests.setdefault(key, deque())
            window_start = now - limit.window

            # 清理窗口外的旧记录
            while timestamps and timestamps[0] < window_start:
                timestamps.popleft()

            # 检查是否超出限制
            allowed = limit.requests + limit.burst
            if len(timestamps) >= allowed:
                raise RateLimitExceededError(key)

            timestamps.append(now)

    async def enforce(
        self,
        request: UnifiedRequest,
        service: ServiceDefinition,
        client_ip: str | None = None,
    ) -> None:
        checks: list[tuple[str, RateLimit]] = []

        if self.config.global_limit:
            checks.append(("global", self.config.global_limit))

        if (
            request.tenant_id
            and self.config.tenant_limits
            and request.tenant_id in self.config.tenant_limits
        ):
            checks.append(
                (f"tenant:{request.tenant_id}", self.config.tenant_limits[request.tenant_id])
            )

        if (
            request.user_id
            and self.config.user_limits
            and request.user_id in self.config.user_limits
        ):
            checks.append((f"user:{request.user_id}", self.config.user_limits[request.user_id]))

        if (
            service.service_id
            and self.config.service_limits
            and service.service_id in self.config.service_limits
        ):
            checks.append(
                (f"service:{service.service_id}", self.config.service_limits[service.service_id])
            )

        if client_ip and self.config.ip_limits:
            checks.append((f"ip:{client_ip}", self.config.ip_limits))

        # 服务级别限流配置（新格式优先）
        service_config = service.get_service_config()
        if service_config.rate_limit.enabled:
            checks.append(
                (
                    f"service:{service.service_id}:config",
                    RateLimit(
                        requests=service_config.rate_limit.requests,
                        window=service_config.rate_limit.window,
                        burst=service_config.rate_limit.burst,
                        strategy=service_config.rate_limit.strategy,
                    ),
                )
            )
        # 旧格式兼容：仅在新格式未启用时读取旧配置
        elif service.rate_limit:
            checks.append(
                (
                    f"service:{service.service_id}:local",
                    RateLimit(**service.rate_limit),
                )
            )

        for key, limit in checks:
            await self._check_key(key, limit)
