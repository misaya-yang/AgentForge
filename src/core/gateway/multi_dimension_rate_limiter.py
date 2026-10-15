"""
多维度限流器

支持：
- 全局限流（保护系统）
- IP 限流（防止滥用）
- 用户限流（按层级区分）
- 租户限流
- Assistant 限流
- 操作类型限流
"""

from __future__ import annotations

import asyncio
import os
import time
from collections import deque
from dataclasses import dataclass, field
from types import SimpleNamespace
from typing import Any

from ..auth.user_resolver import UserContext
from ..hot_path_metrics import gateway_hot_path_metrics
from .lua_scripts import SLIDING_WINDOW_CHECK_LUA, eval_script


def _get_bool_env(key: str, default: bool) -> bool:
    """Get boolean from environment variable."""
    val = os.getenv(key, "").lower()
    if val in ("true", "1", "yes"):
        return True
    if val in ("false", "0", "no"):
        return False
    return default


def _get_int_env(key: str, default: int) -> int:
    """Get integer from environment variable."""
    val = os.getenv(key)
    if val:
        try:
            return int(val)
        except ValueError:
            pass
    return default


@dataclass
class RateLimitResult:
    """限流检查结果"""

    allowed: bool = True
    dimension: str = ""  # 触发限流的维度
    limit: int = 0  # 限制值
    remaining: int = 0  # 剩余配额
    reset_at: int = 0  # 重置时间戳
    retry_after: int = 0  # 建议等待秒数


@dataclass
class TierLimit:
    """用户层级限流配置"""

    requests: int
    window: int
    burst: int = 0


@dataclass
class MultiDimensionRateLimitConfig:
    """多维度限流配置

    Environment variables for development override:
    - RATE_LIMIT_IP_ENABLED: Set to "false" to disable IP rate limiting
    - RATE_LIMIT_IP_LIMIT: Override IP rate limit (default: 30)
    - RATE_LIMIT_ANONYMOUS_LIMIT: Override anonymous user limit (default: 10)
    """

    # 全局限流
    global_enabled: bool = True
    global_limit: int = 10000
    global_window: int = 60

    # IP 限流 (生产环境默认启用，可通过环境变量禁用)
    # Set RATE_LIMIT_IP_ENABLED=false in development
    ip_enabled: bool = field(default_factory=lambda: _get_bool_env("RATE_LIMIT_IP_ENABLED", True))
    ip_limit: int = field(default_factory=lambda: _get_int_env("RATE_LIMIT_IP_LIMIT", 30))
    ip_window: int = 60

    # 用户分层限流 (可通过环境变量调整 anonymous 限制)
    user_tier_limits: dict[str, TierLimit] = field(
        default_factory=lambda: {
            "anonymous": TierLimit(
                requests=_get_int_env("RATE_LIMIT_ANONYMOUS_LIMIT", 10), window=60
            ),
            "normal": TierLimit(requests=_get_int_env("RATE_LIMIT_NORMAL_LIMIT", 60), window=60),
            "premium": TierLimit(requests=_get_int_env("RATE_LIMIT_PREMIUM_LIMIT", 300), window=60),
            "enterprise": TierLimit(
                requests=_get_int_env("RATE_LIMIT_ENTERPRISE_LIMIT", 1000), window=60
            ),
            "admin": TierLimit(requests=_get_int_env("RATE_LIMIT_ADMIN_LIMIT", 10000), window=60),
        }
    )

    # 租户限流
    tenant_enabled: bool = True
    tenant_default_limit: int = 5000
    tenant_window: int = 60
    tenant_limits: dict[str, int] = field(default_factory=dict)  # 特定租户的自定义限制

    # Assistant 限流
    assistant_enabled: bool = True
    assistant_default_limit: int = 1000
    assistant_window: int = 60
    assistant_limits: dict[str, int] = field(default_factory=dict)

    # 操作类型限流
    operation_limits: dict[str, TierLimit] = field(
        default_factory=lambda: {
            "run_create": TierLimit(requests=100, window=60),
            "thread_create": TierLimit(requests=50, window=60),
            "store_write": TierLimit(requests=200, window=60),
            # Assistant heavy operations (per-user, sliding window)
            "assistant_chat": TierLimit(
                requests=_get_int_env("RATE_LIMIT_ASSISTANT_CHAT_LIMIT", 60), window=60
            ),
            "quiz_generate": TierLimit(
                requests=_get_int_env("RATE_LIMIT_QUIZ_GENERATE_LIMIT", 10), window=60
            ),
            "image_generate": TierLimit(
                requests=_get_int_env("RATE_LIMIT_IMAGE_GENERATE_LIMIT", 20), window=60
            ),
            "file_upload": TierLimit(
                requests=_get_int_env("RATE_LIMIT_FILE_UPLOAD_LIMIT", 30), window=60
            ),
            "code_execute": TierLimit(
                requests=_get_int_env("RATE_LIMIT_CODE_EXECUTE_LIMIT", 30), window=60
            ),
            "quiz_submit_public": TierLimit(
                requests=_get_int_env("RATE_LIMIT_QUIZ_SUBMIT_PUBLIC_LIMIT", 10),
                window=60,
            ),
        }
    )


@dataclass
class RateLimitContext:
    """限流上下文"""

    ip: str
    user_id: str | None = None
    user_tier: str = "anonymous"
    tenant_id: str | None = None
    assistant_id: str | None = None
    operation: str | None = None  # run_create | thread_create | store_write

    @classmethod
    def from_user_context(
        cls,
        user: UserContext,
        assistant_id: str | None = None,
        operation: str | None = None,
    ) -> RateLimitContext:
        return cls(
            ip=user.ip,
            user_id=user.user_id,
            user_tier=user.tier,
            tenant_id=user.tenant_id,
            assistant_id=assistant_id,
            operation=operation,
        )


def create_rate_limit_config(
    user_tier_overrides: dict[str, TierLimit] | None = None,
) -> MultiDimensionRateLimitConfig:
    """Build rate-limit config without dropping default tiers.

    Runtime settings may override selected tiers. They must merge into the
    default env-aware config instead of replacing it, otherwise roles such as
    admin silently fall back to the anonymous limit.
    """

    config = MultiDimensionRateLimitConfig()
    if user_tier_overrides:
        config.user_tier_limits.update(user_tier_overrides)
    return config


class MultiDimensionRateLimiter:
    """多维度限流器"""

    def __init__(
        self,
        config: MultiDimensionRateLimitConfig,
        redis_client: Any | None = None,
    ):
        self.config = config
        self.redis = redis_client

        # 内存存储（当 Redis 不可用时使用）
        self._requests: dict[str, deque[float]] = {}
        self._lock = asyncio.Lock()

    async def check(
        self,
        context: RateLimitContext,
        *,
        skip_dimensions: set[str] | None = None,
    ) -> RateLimitResult:
        """
        按优先级检查所有限流维度
        任一维度超限则拒绝

        ``skip_dimensions`` lets the ASGI rate-limit middleware remain the
        single authoritative counter for the dimensions it already counted
        (SPO-02); the route-level check then only counts the dimensions the
        middleware cannot know (e.g. per-operation limits).
        """
        skip = skip_dimensions or set()
        checks = []

        # 1. 全局限流
        if self.config.global_enabled and "global" not in skip:
            checks.append(("global", self._check_global, context))

        # 2. IP 限流
        if self.config.ip_enabled and context.ip and "ip" not in skip:
            checks.append(("ip", self._check_ip, context))

        # 3. 用户限流
        if context.user_id and "user" not in skip:
            checks.append(("user", self._check_user, context))

        # 4. 租户限流
        if self.config.tenant_enabled and context.tenant_id and "tenant" not in skip:
            checks.append(("tenant", self._check_tenant, context))

        # 5. Assistant 限流
        if self.config.assistant_enabled and context.assistant_id and "assistant" not in skip:
            checks.append(("assistant", self._check_assistant, context))

        # 6. 操作类型限流
        if (
            context.operation
            and context.operation in self.config.operation_limits
            and "operation" not in skip
        ):
            checks.append(("operation", self._check_operation, context))

        allowed_result: RateLimitResult | None = None
        for _dimension, check_func, ctx in checks:
            result = await check_func(ctx)
            if not result.allowed:
                return result
            if result.limit > 0:
                allowed_result = result

        return allowed_result or RateLimitResult(allowed=True)

    async def check_custom_limit(
        self,
        *,
        key: str,
        limit: int,
        window: int,
        dimension: str,
    ) -> RateLimitResult:
        """Check a custom explicit limit rule (used by service-level overrides)."""
        results = await self.check_custom_limits(
            policies=[
                SimpleNamespace(
                    key=key,
                    requests=max(int(limit or 0), 1),
                    window=max(int(window or 0), 1),
                    dimension=dimension,
                )
            ]
        )
        return results[-1]

    async def check_custom_limits(self, *, policies: list[Any]) -> list[RateLimitResult]:
        """Atomically check custom policies sharing one window.

        Callers group policies by window so Redis performs at most one script
        round trip per distinct window. Results retain input order and stop at
        the first rejected policy, matching the historical sequential checks.
        """
        if not policies:
            return []
        windows = {max(int(policy.window or 0), 1) for policy in policies}
        if len(windows) != 1:
            raise ValueError("custom rate-limit batch must share one window")

        window = next(iter(windows))
        normalized = [
            SimpleNamespace(
                key=str(policy.key),
                limit=max(int(policy.requests or 0), 1),
                dimension=str(policy.dimension),
            )
            for policy in policies
        ]
        now = time.time()
        window_start = now - window
        if self.redis:
            return await self._redis_sliding_window_many(
                policies=normalized,
                window=window,
                now=now,
                window_start=window_start,
            )

        results: list[RateLimitResult] = []
        for policy in normalized:
            result = await self._memory_sliding_window(
                policy.key,
                policy.limit,
                window,
                policy.dimension,
                now,
                window_start,
            )
            results.append(result)
            if not result.allowed:
                break
        return results

    async def _check_global(self, _context: RateLimitContext) -> RateLimitResult:
        """检查全局限流"""
        return await self._sliding_window_check(
            key="ratelimit:global",
            limit=self.config.global_limit,
            window=self.config.global_window,
            dimension="global",
        )

    async def _check_ip(self, context: RateLimitContext) -> RateLimitResult:
        """检查 IP 限流"""
        return await self._sliding_window_check(
            key=f"ratelimit:ip:{context.ip}",
            limit=self.config.ip_limit,
            window=self.config.ip_window,
            dimension="ip",
        )

    async def _check_user(self, context: RateLimitContext) -> RateLimitResult:
        """检查用户限流（按层级）"""
        tier_limit = self.config.user_tier_limits.get(
            context.user_tier, self.config.user_tier_limits["anonymous"]
        )

        return await self._sliding_window_check(
            key=f"ratelimit:user:{context.user_id}",
            limit=tier_limit.requests + tier_limit.burst,
            window=tier_limit.window,
            dimension="user",
        )

    async def _check_tenant(self, context: RateLimitContext) -> RateLimitResult:
        """检查租户限流"""
        limit = self.config.tenant_limits.get(context.tenant_id, self.config.tenant_default_limit)

        return await self._sliding_window_check(
            key=f"ratelimit:tenant:{context.tenant_id}",
            limit=limit,
            window=self.config.tenant_window,
            dimension="tenant",
        )

    async def _check_assistant(self, context: RateLimitContext) -> RateLimitResult:
        """检查 Assistant 限流"""
        limit = self.config.assistant_limits.get(
            context.assistant_id, self.config.assistant_default_limit
        )

        return await self._sliding_window_check(
            key=f"ratelimit:assistant:{context.assistant_id}",
            limit=limit,
            window=self.config.assistant_window,
            dimension="assistant",
        )

    async def _check_operation(self, context: RateLimitContext) -> RateLimitResult:
        """检查操作类型限流"""
        op_limit = self.config.operation_limits[context.operation]

        return await self._sliding_window_check(
            key=f"ratelimit:op:{context.operation}:{context.user_id or context.ip}",
            limit=op_limit.requests + op_limit.burst,
            window=op_limit.window,
            dimension=f"operation:{context.operation}",
        )

    async def _sliding_window_check(
        self,
        key: str,
        limit: int,
        window: int,
        dimension: str,
    ) -> RateLimitResult:
        """滑动窗口限流检查"""
        now = time.time()
        window_start = now - window

        if self.redis:
            # 使用 Redis 实现分布式限流
            return await self._redis_sliding_window(
                key, limit, window, dimension, now, window_start
            )
        else:
            # 使用内存实现本地限流
            return await self._memory_sliding_window(
                key, limit, window, dimension, now, window_start
            )

    async def _memory_sliding_window(
        self,
        key: str,
        limit: int,
        window: int,
        dimension: str,
        now: float,
        window_start: float,
    ) -> RateLimitResult:
        """内存滑动窗口"""
        async with self._lock:
            timestamps = self._requests.setdefault(key, deque())

            # 清理过期请求
            while timestamps and timestamps[0] < window_start:
                timestamps.popleft()

            current_count = len(timestamps)
            remaining = max(0, limit - current_count - 1)
            reset_at = int(now + window)

            if current_count >= limit:
                # 计算需要等待的时间
                retry_after = int(timestamps[0] - window_start) + 1 if timestamps else window

                return RateLimitResult(
                    allowed=False,
                    dimension=dimension,
                    limit=limit,
                    remaining=0,
                    reset_at=reset_at,
                    retry_after=retry_after,
                )

            # 记录当前请求
            timestamps.append(now)

            return RateLimitResult(
                allowed=True,
                dimension=dimension,
                limit=limit,
                remaining=remaining,
                reset_at=reset_at,
            )

    async def _redis_sliding_window(
        self,
        key: str,
        limit: int,
        window: int,
        dimension: str,
        now: float,
        window_start: float,
    ) -> RateLimitResult:
        """Redis 滑动窗口（单 EVAL 原子检查，SPO-02 消除 TOCTOU）"""
        results = await self._redis_sliding_window_many(
            policies=[SimpleNamespace(key=key, limit=limit, dimension=dimension)],
            window=window,
            now=now,
            window_start=window_start,
        )
        return results[-1]

    async def _redis_sliding_window_many(
        self,
        *,
        policies: list[Any],
        window: int,
        now: float,
        window_start: float,
    ) -> list[RateLimitResult]:
        """Check one same-window policy batch in a single Redis script."""
        try:
            result = await eval_script(
                self.redis,
                SLIDING_WINDOW_CHECK_LUA,
                keys=[policy.key for policy in policies],
                args=[
                    now,
                    window_start,
                    window + 1,
                    f"{now}:{time.time_ns()}",
                    *(policy.limit for policy in policies),
                ],
            )
            gateway_hot_path_metrics.redis_round_trips += 1
            rejected_index, earliest_score = int(result[0]), float(result[1])

            reset_at = int(now + window)
            results: list[RateLimitResult] = []
            for index, policy in enumerate(policies):
                if rejected_index >= 0 and index > rejected_index:
                    break
                if index == rejected_index:
                    retry_after = (
                        int(earliest_score - window_start) + 1
                        if earliest_score > window_start
                        else window
                    )
                    results.append(
                        RateLimitResult(
                            allowed=False,
                            dimension=policy.dimension,
                            limit=policy.limit,
                            remaining=0,
                            reset_at=reset_at,
                            retry_after=retry_after,
                        )
                    )
                    break

                remaining_index = 3 + index
                remaining = (
                    max(int(result[remaining_index]), 0)
                    if len(result) > remaining_index
                    and int(result[remaining_index]) >= 0
                    else (
                        max(int(result[2]), 0)
                        if len(result) > 2
                        else max(policy.limit - 1, 0)
                    )
                )
                results.append(
                    RateLimitResult(
                        allowed=True,
                        dimension=policy.dimension,
                        limit=policy.limit,
                        remaining=remaining,
                        reset_at=reset_at,
                    )
                )
            return results
        except Exception:
            # Redis 不可用时回退到内存限流
            results = []
            for policy in policies:
                fallback = await self._memory_sliding_window(
                    policy.key,
                    policy.limit,
                    window,
                    policy.dimension,
                    now,
                    window_start,
                )
                results.append(fallback)
                if not fallback.allowed:
                    break
            return results


class RateLimitHeaders:
    """标准限流响应头构建器"""

    @staticmethod
    def build(result: RateLimitResult) -> dict[str, str]:
        headers = {
            "X-RateLimit-Limit": str(result.limit),
            "X-RateLimit-Remaining": str(result.remaining),
            "X-RateLimit-Reset": str(result.reset_at),
            "X-RateLimit-Dimension": result.dimension,
        }
        if result.retry_after > 0:
            headers["Retry-After"] = str(result.retry_after)
        return headers

    @staticmethod
    def build_exceeded_response(result: RateLimitResult) -> dict[str, Any]:
        """构建 429 响应体"""
        return {
            "error": {
                "code": "RATE_LIMIT_EXCEEDED",
                "message": "Too many requests",
                "dimension": result.dimension,
                "limit": result.limit,
                "remaining": 0,
                "reset_at": result.reset_at,
                "retry_after": result.retry_after,
            }
        }
