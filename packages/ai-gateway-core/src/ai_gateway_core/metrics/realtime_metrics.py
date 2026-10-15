"""
Real-time Metrics Service - LangSmith-style enterprise monitoring

Provides:
- Real-time RPS (requests per second) calculation
- Active users tracking
- Thread/session tracking per user
- Queue depth monitoring
- Sliding window calculations
- WebSocket broadcast support
"""

from __future__ import annotations

import asyncio
import contextlib
import logging
import time
import uuid
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import datetime
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    # RedisStorage still lives in gateway src/persistence/redis.py — runtime
    # contract is duck-typed, so Any keeps this importable from AS without
    # gateway src/ on the path.
    RedisStorage = Any  # type: ignore[assignment, misc]

from ai_gateway_core.billing import microcents_to_usd

logger = logging.getLogger(__name__)

# TTL constants
TTL_5M = 300  # 5 minutes
TTL_1H = 3600  # 1 hour
TTL_24H = 86400  # 24 hours

# Sliding window configuration
WINDOW_MINUTES = 5  # Sliding window size for rate calculations


@dataclass
class RealtimeSnapshot:
    """Real-time metrics snapshot for dashboard"""

    # Throughput
    rps: float = 0.0
    rps_1m: float = 0.0
    rps_5m: float = 0.0

    # Latency
    latency_p50: int = 0
    latency_p95: int = 0
    latency_p99: int = 0
    latency_avg: int = 0

    # Error rates
    error_rate: float = 0.0
    error_rate_4xx: float = 0.0
    error_rate_5xx: float = 0.0

    # Users & Sessions
    active_users: int = 0
    total_threads: int = 0
    threads_by_user: dict[str, int] = field(default_factory=dict)

    # Queue & Capacity
    queue_depth: int = 0
    concurrent_requests: int = 0
    max_concurrent: int = 100

    # Token/Cost (today)
    total_tokens: int = 0
    input_tokens: int = 0
    output_tokens: int = 0
    token_cost_usd: float = 0.0
    tokens_per_minute: float = 0.0

    # LangGraph Runs
    total_runs: int = 0
    run_success_rate: float = 100.0
    avg_run_duration_ms: int = 0

    # Timestamp
    timestamp: str = ""

    def to_dict(self) -> dict[str, Any]:
        return {
            "rps": round(self.rps, 2),
            "rps_1m": round(self.rps_1m, 2),
            "rps_5m": round(self.rps_5m, 2),
            "latency": {
                "p50": self.latency_p50,
                "p95": self.latency_p95,
                "p99": self.latency_p99,
                "avg": self.latency_avg,
            },
            "errors": {
                "rate": round(self.error_rate, 2),
                "rate_4xx": round(self.error_rate_4xx, 2),
                "rate_5xx": round(self.error_rate_5xx, 2),
            },
            "users": {
                "active": self.active_users,
                "threads_total": self.total_threads,
                "threads_by_user": dict(
                    sorted(self.threads_by_user.items(), key=lambda x: x[1], reverse=True)[:10]
                ),  # Top 10 users
            },
            "capacity": {
                "queue_depth": self.queue_depth,
                "concurrent": self.concurrent_requests,
                "max_concurrent": self.max_concurrent,
                "utilization": round(
                    self.concurrent_requests / max(self.max_concurrent, 1) * 100, 1
                ),
            },
            "tokens": {
                "total": self.total_tokens,
                "input": self.input_tokens,
                "output": self.output_tokens,
                "cost_usd": round(self.token_cost_usd, 4),
                "per_minute": round(self.tokens_per_minute, 1),
            },
            "runs": {
                "total": self.total_runs,
                "success_rate": self.run_success_rate,
                "avg_duration_ms": self.avg_run_duration_ms,
            },
            "timestamp": self.timestamp,
        }


class RealtimeMetricsService:
    """
    Real-time metrics service with sliding window calculations

    Uses Redis sorted sets for time-based sliding windows.
    """

    def __init__(self, redis: RedisStorage | None = None):
        self.redis = redis
        self._subscribers: set[Callable] = set()
        self._broadcast_task: asyncio.Task | None = None
        self._running = False

        # In-memory concurrent tracking
        self._concurrent_requests = 0
        self._max_concurrent = 100

    def set_redis(self, redis: RedisStorage) -> None:
        """Set or update Redis storage"""
        self.redis = redis

    async def start_broadcast(self, interval_seconds: float = 5.0) -> None:
        """Start periodic broadcast to subscribers"""
        if self._running:
            return

        self._running = True
        self._broadcast_task = asyncio.create_task(self._broadcast_loop(interval_seconds))
        logger.info(f"Real-time metrics broadcast started (interval={interval_seconds}s)")

    async def stop_broadcast(self) -> None:
        """Stop the broadcast loop"""
        self._running = False
        if self._broadcast_task:
            self._broadcast_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._broadcast_task
        logger.info("Real-time metrics broadcast stopped")

    def subscribe(self, callback: Callable) -> None:
        """Subscribe to real-time updates"""
        self._subscribers.add(callback)

    def unsubscribe(self, callback: Callable) -> None:
        """Unsubscribe from updates"""
        self._subscribers.discard(callback)

    @staticmethod
    def _decode_redis_text(value: Any) -> str:
        if isinstance(value, bytes):
            return value.decode("utf-8", errors="ignore")
        return str(value or "")

    @classmethod
    def _tenant_user_member(cls, tenant_id: str = "", user_id: str = "") -> str:
        if tenant_id:
            return f"{tenant_id}:{user_id}"
        return user_id

    @classmethod
    def _split_tenant_user_member(cls, member: Any) -> tuple[str, str]:
        value = cls._decode_redis_text(member)
        if ":" not in value:
            return "", value
        tenant_id, user_id = value.split(":", 1)
        return tenant_id, user_id

    @classmethod
    def _thread_key(cls, user_id: str, tenant_id: str = "") -> str:
        if tenant_id:
            return f"metrics:rt:threads:tenant:{tenant_id}:user:{user_id}"
        return f"metrics:rt:threads:{user_id}"

    async def _broadcast_loop(self, interval: float) -> None:
        """Periodic broadcast loop"""
        while self._running:
            try:
                snapshot = await self.get_realtime_snapshot()
                for callback in list(self._subscribers):
                    try:
                        if asyncio.iscoroutinefunction(callback):
                            await callback(snapshot.to_dict())
                        else:
                            callback(snapshot.to_dict())
                    except Exception as e:
                        logger.warning(f"Broadcast callback error: {e}")
            except Exception as e:
                logger.error(f"Broadcast loop error: {e}")

            await asyncio.sleep(interval)

    # ========== Recording Methods ==========

    async def record_request_start(
        self, request_id: str, user_id: str = "", tenant_id: str = ""
    ) -> None:
        """Record request start for concurrent tracking"""
        self._concurrent_requests += 1

        if not self.redis or not self.redis._client:
            return

        try:
            now = time.time()
            pipe = self.redis._client.pipeline()

            # Add to active requests set
            pipe.zadd("metrics:rt:active_requests", {request_id: now})
            pipe.expire("metrics:rt:active_requests", TTL_5M)

            # Track active user
            if user_id:
                active_user = self._tenant_user_member(tenant_id=tenant_id, user_id=user_id)
                pipe.zadd("metrics:rt:active_users", {active_user: now})
                pipe.expire("metrics:rt:active_users", TTL_5M)

                # Track user thread count
                thread_key = self._thread_key(user_id=user_id, tenant_id=tenant_id)
                pipe.incr(thread_key)
                pipe.expire(thread_key, TTL_5M)

            await pipe.execute()

        except Exception as e:
            logger.debug(f"Failed to record request start: {e}")

    async def record_request_end(
        self, request_id: str, user_id: str = "", tenant_id: str = ""
    ) -> None:
        """Record request end"""
        self._concurrent_requests = max(0, self._concurrent_requests - 1)

        if not self.redis or not self.redis._client:
            return

        try:
            pipe = self.redis._client.pipeline()

            # Remove from active requests
            pipe.zrem("metrics:rt:active_requests", request_id)

            # Decrement user threads
            if user_id:
                thread_key = self._thread_key(user_id=user_id, tenant_id=tenant_id)
                pipe.decr(thread_key)

            await pipe.execute()

        except Exception as e:
            logger.debug(f"Failed to record request end: {e}")

    async def record_request_complete(
        self,
        status_code: int,
        duration_ms: float,
        user_id: str = "",
    ) -> None:
        """Record completed request for RPS and latency calculations"""
        del user_id
        if not self.redis or not self.redis._client:
            return

        try:
            now = time.time()
            pipe = self.redis._client.pipeline()

            # Add to sliding window (sorted set by timestamp)
            request_data = f"{now}:{status_code}:{duration_ms}"
            pipe.zadd("metrics:rt:requests_window", {request_data: now})

            # Remove old entries (older than 5 minutes)
            cutoff = now - 300
            pipe.zremrangebyscore("metrics:rt:requests_window", 0, cutoff)

            # Track errors separately
            # 修复：使用唯一 ID 而非时间戳作为 member，避免秒级并发覆盖计数
            if status_code >= 400:
                error_type = "4xx" if status_code < 500 else "5xx"
                error_id = f"{now}:{uuid.uuid4().hex[:8]}"  # 唯一标识符
                pipe.zadd(f"metrics:rt:errors_{error_type}", {error_id: now})
                pipe.zremrangebyscore(f"metrics:rt:errors_{error_type}", 0, cutoff)
                pipe.expire(f"metrics:rt:errors_{error_type}", TTL_5M)

            pipe.expire("metrics:rt:requests_window", TTL_5M)

            await pipe.execute()

        except Exception as e:
            logger.debug(f"Failed to record request complete: {e}")

    async def update_queue_depth(self, depth: int) -> None:
        """Update current queue depth"""
        if not self.redis or not self.redis._client:
            return

        try:
            await self.redis._client.set("metrics:rt:queue_depth", depth, ex=TTL_5M)
        except Exception as e:
            logger.debug(f"Failed to update queue depth: {e}")

    async def record_token_usage(self, input_tokens: int, output_tokens: int) -> None:
        """Record token usage for real-time metrics.

        NOTE: Daily token counters (``metrics:tokens:input:{today}`` /
        ``metrics:tokens:output:{today}``) are written exclusively by
        :meth:`MetricsRecorder.record_tokens` (see
        ``src/services/metrics/metrics_recorder.py`` around line 181). This
        method is kept for backward-compatible call sites that lack
        ``user_id``/``service_id`` context; it is now a Redis no-op to avoid
        double-incrementing and inflating token counters by 2x. Postgres
        ``usage_daily_aggregates`` remains the reconciliation source of truth.

        Args:
            input_tokens: Number of input/prompt tokens
            output_tokens: Number of output/completion tokens
        """
        # Intentionally no-op: see docstring for single-writer rationale.
        del input_tokens, output_tokens
        return

    # ========== Query Methods ==========

    async def get_realtime_snapshot(self) -> RealtimeSnapshot:
        """Get current real-time metrics snapshot"""
        snapshot = RealtimeSnapshot(timestamp=datetime.now().isoformat())

        if not self.redis or not self.redis._client:
            snapshot.concurrent_requests = self._concurrent_requests
            snapshot.max_concurrent = self._max_concurrent
            return snapshot

        try:
            now = time.time()
            client = self.redis._client

            # Calculate RPS from sliding window
            window_data = await self._get_window_data(now)
            snapshot.rps = self._calculate_rps(window_data, now, 1)
            snapshot.rps_1m = self._calculate_rps(window_data, now, 60)
            snapshot.rps_5m = self._calculate_rps(window_data, now, 300)

            # Calculate latency percentiles
            latencies = [d["latency"] for d in window_data if now - d["time"] <= 60]
            if latencies:
                latencies.sort()
                n = len(latencies)
                snapshot.latency_avg = int(sum(latencies) / n)
                snapshot.latency_p50 = int(latencies[int(n * 0.5)])
                snapshot.latency_p95 = int(latencies[min(int(n * 0.95), n - 1)])
                snapshot.latency_p99 = int(latencies[min(int(n * 0.99), n - 1)])

            # Calculate error rates
            total_1m = len([d for d in window_data if now - d["time"] <= 60])
            errors_4xx = await client.zcount("metrics:rt:errors_4xx", now - 60, now)
            errors_5xx = await client.zcount("metrics:rt:errors_5xx", now - 60, now)

            if total_1m > 0:
                snapshot.error_rate = ((errors_4xx + errors_5xx) / total_1m) * 100
                snapshot.error_rate_4xx = (errors_4xx / total_1m) * 100
                snapshot.error_rate_5xx = (errors_5xx / total_1m) * 100

            # Get active users
            active_users = await client.zrangebyscore("metrics:rt:active_users", now - 300, now)
            snapshot.active_users = len(active_users)

            # Get threads by user
            threads_by_user = {}
            for active_user in active_users[:20]:  # Limit to 20 users
                tenant_id, user_id = self._split_tenant_user_member(active_user)
                if not user_id:
                    continue
                thread_count = await client.get(
                    self._thread_key(user_id=user_id, tenant_id=tenant_id)
                )
                if thread_count:
                    threads_by_user[user_id] = threads_by_user.get(user_id, 0) + max(
                        0, int(thread_count)
                    )
            snapshot.threads_by_user = threads_by_user
            snapshot.total_threads = sum(threads_by_user.values())

            # Queue depth
            queue_depth = await client.get("metrics:rt:queue_depth")
            snapshot.queue_depth = int(queue_depth or 0)

            # Concurrent requests - 修复：从 Redis 获取而非进程内变量，支持多进程部署
            # 使用 active_requests zset 的 cardinality 作为并发数
            active_count = await client.zcard("metrics:rt:active_requests")
            snapshot.concurrent_requests = active_count or self._concurrent_requests
            snapshot.max_concurrent = self._max_concurrent

            # Get today's token/run stats from MetricsRecorder keys
            today = datetime.now().strftime("%Y-%m-%d")

            pipe = client.pipeline()
            pipe.get(f"metrics:tokens:input:{today}")
            pipe.get(f"metrics:tokens:output:{today}")
            pipe.get(f"metrics:tokens:cost:{today}")
            pipe.get(f"metrics:tokens:cost_micro:{today}")
            pipe.get(f"metrics:runs:total:{today}")
            pipe.get(f"metrics:runs:success:{today}")
            pipe.get(f"metrics:runs:duration:sum:{today}")
            pipe.get(f"metrics:runs:duration:count:{today}")

            results = await pipe.execute()

            input_tokens = int(results[0] or 0)
            output_tokens = int(results[1] or 0)
            cost_cents = int(results[2] or 0)
            cost_microcents = int(results[3] or 0)
            total_runs = int(results[4] or 0)
            runs_success = int(results[5] or 0)
            duration_sum = int(results[6] or 0)
            duration_count = int(results[7] or 0)

            snapshot.input_tokens = input_tokens
            snapshot.output_tokens = output_tokens
            snapshot.total_tokens = input_tokens + output_tokens
            snapshot.token_cost_usd = (
                microcents_to_usd(cost_microcents)
                if cost_microcents > 0
                else round(cost_cents / 100, 6)
            )

            # 修复：tokens/minute 使用最近5分钟滑动窗口计算（从请求窗口估算）
            # 而非"日累计/分钟数"算法
            try:
                now = time.time()
                cutoff_5m = now - 300  # 5 分钟前

                # 获取最近5分钟的请求数据
                window_data = await client.zrangebyscore(
                    "metrics:rt:requests_window", cutoff_5m, now, withscores=True
                )

                # 从窗口估算 tokens (使用请求数 * 平均 token)
                # 如果有今日总量数据，计算更精确的平均值
                requests_5m = len(window_data) if window_data else 0
                if requests_5m > 0 and total_runs > 0:
                    # 使用今日平均每请求 token 数估算
                    avg_tokens_per_request = snapshot.total_tokens / total_runs
                    snapshot.tokens_per_minute = round(
                        (requests_5m * avg_tokens_per_request) / WINDOW_MINUTES, 1
                    )
                else:
                    # 回退到简单计算 — 使用距当日零点的秒数除以60
                    _now = datetime.now()
                    _day_start = _now.replace(hour=0, minute=0, second=0, microsecond=0)
                    elapsed_minutes = max(1, (_now - _day_start).total_seconds() / 60)
                    snapshot.tokens_per_minute = round(snapshot.total_tokens / elapsed_minutes, 1)
            except Exception:
                # 发生错误时回退到简单计算
                _now = datetime.now()
                _day_start = _now.replace(hour=0, minute=0, second=0, microsecond=0)
                elapsed_minutes = max(1, (_now - _day_start).total_seconds() / 60)
                snapshot.tokens_per_minute = round(snapshot.total_tokens / elapsed_minutes, 1)

            snapshot.total_runs = total_runs
            snapshot.run_success_rate = round(
                (runs_success / total_runs * 100) if total_runs > 0 else 100.0, 1
            )
            snapshot.avg_run_duration_ms = (
                int(duration_sum / duration_count) if duration_count > 0 else 0
            )

        except Exception as e:
            logger.warning(f"Failed to get realtime snapshot: {e}")

        return snapshot

    async def _get_window_data(self, now: float) -> list[dict[str, Any]]:
        """Get request data from sliding window"""
        if not self.redis or not self.redis._client:
            return []

        try:
            # Get all requests in window
            entries = await self.redis._client.zrangebyscore(
                "metrics:rt:requests_window",
                now - 300,  # Last 5 minutes
                now,
            )

            data = []
            for entry in entries:
                try:
                    parts = entry.split(":")
                    if len(parts) >= 3:
                        data.append(
                            {
                                "time": float(parts[0]),
                                "status": int(parts[1]),
                                "latency": float(parts[2]),
                            }
                        )
                except (ValueError, IndexError):
                    continue

            return data

        except Exception as e:
            logger.debug(f"Failed to get window data: {e}")
            return []

    def _calculate_rps(
        self, window_data: list[dict[str, Any]], now: float, window_seconds: int
    ) -> float:
        """Calculate RPS for a given window"""
        cutoff = now - window_seconds
        count = len([d for d in window_data if d["time"] >= cutoff])
        return count / window_seconds if window_seconds > 0 else 0.0

    async def get_user_metrics(self, user_id: str, tenant_id: str = "") -> dict[str, Any]:
        """Get metrics for a specific user (multi-tenant support)"""
        if not self.redis or not self.redis._client:
            return {}

        try:
            today = datetime.now().strftime("%Y-%m-%d")
            client = self.redis._client

            pipe = client.pipeline()
            if tenant_id:
                input_key = f"metrics:tokens:tenant:{tenant_id}:user:{user_id}:input:{today}"
                output_key = f"metrics:tokens:tenant:{tenant_id}:user:{user_id}:output:{today}"
            else:
                input_key = f"metrics:tokens:user:{user_id}:input:{today}"
                output_key = f"metrics:tokens:user:{user_id}:output:{today}"
            pipe.get(input_key)
            pipe.get(output_key)
            pipe.get(self._thread_key(user_id=user_id, tenant_id=tenant_id))

            results = await pipe.execute()

            input_tokens = int(results[0] or 0)
            output_tokens = int(results[1] or 0)
            threads = max(0, int(results[2] or 0))

            return {
                "user_id": user_id,
                "tokens": {
                    "input": input_tokens,
                    "output": output_tokens,
                    "total": input_tokens + output_tokens,
                },
                "active_threads": threads,
                "timestamp": datetime.now().isoformat(),
            }

        except Exception as e:
            logger.warning(f"Failed to get user metrics: {e}")
            return {}


# Global singleton
_realtime_service: RealtimeMetricsService | None = None


def get_realtime_metrics() -> RealtimeMetricsService:
    """Get the global RealtimeMetricsService singleton"""
    global _realtime_service
    if _realtime_service is None:
        _realtime_service = RealtimeMetricsService()
    return _realtime_service


def init_realtime_metrics(redis: RedisStorage) -> RealtimeMetricsService:
    """Initialize the global RealtimeMetricsService with Redis"""
    global _realtime_service
    if _realtime_service is None:
        _realtime_service = RealtimeMetricsService(redis)
    else:
        _realtime_service.set_redis(redis)
    return _realtime_service
