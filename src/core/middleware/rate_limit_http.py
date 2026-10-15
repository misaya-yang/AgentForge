"""
HTTP 请求级限流中间件

在 FastAPI 层面实现多维度限流：
- 全局限流：保护后端服务
- 用户级限流：防止单用户滥用
- IP 级限流：防止未登录滥用
- 游客限流：更严格的限制
"""

from __future__ import annotations

import time
import uuid
from collections import deque
from dataclasses import dataclass
from typing import Any

from ai_gateway_core.logging import get_logger

from ..gateway.lua_scripts import SLIDING_WINDOW_CHECK_LUA, eval_script
from ..hot_path_metrics import gateway_hot_path_metrics

logger = get_logger(__name__)


@dataclass
class RateLimitInfo:
    """限流信息"""

    allowed: bool
    dimension: str  # "global" | "user" | "guest" | "ip"
    limit: int
    remaining: int
    reset_at: int
    retry_after: int = 0
    # Index into the ``check_many`` checks list that rejected the request.
    dimension_index: int = -1


class SlidingWindowRateLimiter:
    """
    滑动窗口限流器

    使用 Redis Sorted Set 实现精确滑动窗口
    当 Redis 不可用时回退到内存实现
    """

    def __init__(self, redis_client: Any | None = None):
        self.redis = redis_client
        # 内存存储（Redis 不可用时）
        self._requests: dict[str, deque[float]] = {}

    async def check(
        self,
        key: str,
        limit: int,
        window: int,
    ) -> RateLimitInfo:
        return await self.check_many([(key, limit, window)])

    async def check_many(
        self,
        checks: list[tuple[str, int, int]],
    ) -> RateLimitInfo:
        """
        检查并记录请求（一个或多个维度）。

        Redis 可用时按 window 分组，每组一个原子 EVAL（清理 + 计数 +
        条件写入 + 过期，SPO-02 消除多副本 TOCTOU 且每组仅 1 RTT）。
        Redis 不可用时逐维度回退内存滑窗。
        """
        now = time.time()
        if not checks:
            return RateLimitInfo(
                allowed=True,
                dimension="",
                limit=0,
                remaining=0,
                reset_at=int(now),
            )

        if self.redis:
            try:
                return await self._redis_check_many(checks, now)
            except Exception as e:
                logger.warning(f"Redis rate limit check failed: {e}, falling back to memory")

        for index, (key, limit, window) in enumerate(checks):
            result = await self._memory_check(key, limit, window, now, now - window)
            if not result.allowed:
                result.dimension_index = index
                return result
        return RateLimitInfo(
            allowed=True,
            dimension="",
            limit=checks[-1][1],
            remaining=0,
            reset_at=int(now + checks[-1][2]),
        )

    async def _redis_check_many(
        self,
        checks: list[tuple[str, int, int]],
        now: float,
    ) -> RateLimitInfo:
        """One atomic EVAL per distinct window; rejected requests are not recorded."""
        member = str(uuid.uuid4())
        for window in {check[2] for check in checks}:
            group = [(index, key, limit) for index, (key, limit, w) in enumerate(checks) if w == window]
            window_start = now - window
            result = await eval_script(
                self.redis,
                SLIDING_WINDOW_CHECK_LUA,
                keys=[key for _, key, _ in group],
                args=[
                    now,
                    window_start,
                    window + 1,
                    member,
                    *[limit for _, _, limit in group],
                ],
            )
            gateway_hot_path_metrics.redis_round_trips += 1
            rejected_index, earliest_score = int(result[0]), float(result[1])
            if rejected_index < 0:
                continue
            checks_index = group[rejected_index][0]
            _, key, limit = checks[checks_index]
            retry_after = (
                int(earliest_score - window_start) + 1
                if earliest_score > window_start
                else window
            )
            return RateLimitInfo(
                allowed=False,
                dimension="",
                limit=limit,
                remaining=0,
                reset_at=int(now + window),
                retry_after=retry_after,
                dimension_index=checks_index,
            )
        return RateLimitInfo(
            allowed=True,
            dimension="",
            limit=checks[-1][1],
            remaining=0,
            reset_at=int(now + checks[-1][2]),
        )

    async def _memory_check(
        self,
        key: str,
        limit: int,
        window: int,
        now: float,
        window_start: float,
    ) -> RateLimitInfo:
        """内存滑动窗口检查"""
        timestamps = self._requests.setdefault(key, deque())

        # 清理过期请求
        while timestamps and timestamps[0] < window_start:
            timestamps.popleft()

        current_count = len(timestamps)
        remaining = max(0, limit - current_count - 1)
        reset_at = int(now + window)

        if current_count >= limit:
            retry_after = int(timestamps[0] - window_start) + 1 if timestamps else window
            return RateLimitInfo(
                allowed=False,
                dimension="",
                limit=limit,
                remaining=0,
                reset_at=reset_at,
                retry_after=retry_after,
            )

        # 记录当前请求
        timestamps.append(now)

        return RateLimitInfo(
            allowed=True,
            dimension="",
            limit=limit,
            remaining=remaining,
            reset_at=reset_at,
        )
