"""
流式计费拦截器

针对 LangGraph 等 LLM 服务的流式输出，解析包含 `event: metadata` 的数据块，
提取 `usage` (token 计数) 并异步推送到计费系统。

支持的事件格式：
- LangGraph: event: metadata\ndata: {"usage": {"input_tokens": 100, "output_tokens": 50}}
- OpenAI: data: {..., "usage": {"prompt_tokens": 100, "completion_tokens": 50}}
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import re
import time
from collections.abc import Awaitable, Callable
from dataclasses import asdict
from typing import Any

from ai_gateway_core.logging import get_logger

from ..core.observability.metrics import get_metrics
from .billing_stream import StreamProcessor, UsageData

# Preserve the historical dotted names for repr, pickling, and callers that
# inspect the public billing module while implementations live separately.
for _compat_symbol in (StreamProcessor, UsageData):
    _compat_symbol.__module__ = __name__
del _compat_symbol

# Phase 0 hotfix: retry & DLQ constants for billing flush failures
_FLUSH_RETRY_BACKOFFS: tuple[float, ...] = (0.1, 0.5, 2.0)
_BILLING_DLQ_KEY = "metrics:billing:dead_letter"
_BILLING_DLQ_REPLAYED_KEY = "metrics:billing:dead_letter:replayed"
_BILLING_DLQ_CAP = 10_000

logger = get_logger(__name__)


def _get_explicit_attr(obj: Any, name: str) -> Any:
    if obj is None:
        return None
    if name in getattr(obj, "__dict__", {}):
        return getattr(obj, name)
    if getattr(type(obj), name, None) is not None:
        return getattr(obj, name)
    return None


def _resolve_redis_client(redis_client: Any) -> Any:
    native_getter = _get_explicit_attr(redis_client, "get_native_client")
    if callable(native_getter):
        with contextlib.suppress(Exception):
            native = native_getter()
            if native is not None:
                return native

    client = _get_explicit_attr(redis_client, "_client")
    if client is not None:
        return client

    return redis_client


# 计费回调类型
BillingCallback = Callable[[UsageData], Awaitable[None]]


class BillingInterceptor:
    """
    流式计费拦截器

    解析 SSE 流中的 metadata 事件，提取 usage 信息并异步推送。
    """

    # SSE 事件正则
    EVENT_PATTERN = re.compile(r"^event:\s*(.+)$", re.MULTILINE)
    DATA_PATTERN = re.compile(r"^data:\s*(.+)$", re.MULTILINE)

    def __init__(
        self,
        callback: BillingCallback | None = None,
        redis_client=None,
        realtime_metrics=None,  # 修复：添加实时指标服务
        buffer_size: int = 100,
        flush_interval: float = 5.0,
    ):
        """
        初始化计费拦截器

        Args:
            callback: 计费回调函数（接收 UsageData）
            redis_client: Redis 客户端（用于发布计费事件）
            realtime_metrics: RealtimeMetricsService 实例（用于记录实时指标）
            buffer_size: 缓冲区大小（批量推送）
            flush_interval: 刷新间隔（秒）
        """
        self.callback = callback
        self.redis = _resolve_redis_client(redis_client)
        self._realtime_metrics = realtime_metrics  # 可能为 None，使用延迟获取
        self.buffer_size = buffer_size
        self.flush_interval = flush_interval

        # 计费数据缓冲区
        self._buffer: list[UsageData] = []
        self._buffer_lock = asyncio.Lock()

        # 后台刷新任务
        self._flush_task: asyncio.Task | None = None
        self._running = False

        # Phase 0 hotfix: tracked fire-and-forget buffer flushes so that
        # shutdown can await them instead of GC'ing them mid-write.
        self._inflight_flushes: set[asyncio.Task] = set()

        # 统计
        self._total_events = 0
        self._total_tokens = 0

    def _spawn_tracked_flush(self) -> asyncio.Task:
        """Spawn a buffer flush task that ``stop()`` can drain before exit.

        Replaces direct ``asyncio.create_task(self._flush_buffer())`` calls
        from StreamProcessor. Without tracking, those tasks could be
        garbage-collected on interpreter shutdown before their
        database/Redis writes completed, silently dropping billing records.
        """
        task = asyncio.create_task(self._flush_buffer())
        self._inflight_flushes.add(task)
        task.add_done_callback(self._inflight_flushes.discard)
        return task

    @property
    def realtime_metrics(self):
        """获取实时指标服务（支持延迟初始化）"""
        if self._realtime_metrics is None:
            try:
                from ..services.metrics.realtime_metrics import get_realtime_metrics

                self._realtime_metrics = get_realtime_metrics()
            except Exception:
                pass
        return self._realtime_metrics

    async def start(self) -> None:
        """启动后台刷新任务"""
        if self._running:
            return

        self._running = True
        self._flush_task = asyncio.create_task(self._flush_loop())
        logger.info("Billing interceptor started")

    async def stop(self) -> None:
        """停止后台任务并刷新剩余数据.

        Phase 0 hotfix: drains any fire-and-forget flushes spawned by
        StreamProcessor BEFORE running the final ``_flush_buffer()``. Without
        this, tasks created via ``_spawn_tracked_flush()`` could be
        garbage-collected mid-write on shutdown, silently dropping billing
        records.
        """
        self._running = False

        if self._flush_task:
            self._flush_task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await self._flush_task

        # Drain in-flight flushes spawned by StreamProcessor
        if self._inflight_flushes:
            await asyncio.gather(*self._inflight_flushes, return_exceptions=True)

        # Final flush of anything still in the buffer
        await self._flush_buffer()
        logger.info(
            f"Billing interceptor stopped. Total events: {self._total_events}, tokens: {self._total_tokens}"
        )

    async def _flush_loop(self) -> None:
        """后台刷新循环"""
        while self._running:
            try:
                await asyncio.sleep(self.flush_interval)
                await self._flush_buffer()
            except asyncio.CancelledError:
                break
            except Exception as e:
                logger.error(f"Billing flush error: {e}")

    async def _flush_buffer(self) -> None:
        """刷新缓冲区"""
        async with self._buffer_lock:
            if not self._buffer:
                return

            to_flush = self._buffer[:]
            self._buffer.clear()

        # 批量推送
        for usage in to_flush:
            await self._push_usage(usage)

    async def _push_usage(self, usage: UsageData) -> None:
        """推送单条计费数据.

        Phase 0 hotfix: each sub-stage (callback / redis / database) is
        classified independently via ``gateway_billing_flush_failures_total``.
        A failing stage triggers exponential-backoff retry (0.1/0.5/2.0s);
        if all retries exhaust, the full record is pushed to the Redis DLQ
        (``metrics:billing:dead_letter``) and
        ``gateway_billing_records_dropped_total`` is incremented.

        Stages are independent: a callback failure does NOT prevent the DB
        write from proceeding — this preserves durability of the primary
        persistence path even when broadcast/callback paths are broken.
        """
        reached_durable = False

        # Stage 1: user-supplied callback (optional)
        if self.callback:
            await self._run_stage_with_retry(
                stage="callback",
                coro_factory=lambda: self.callback(usage),
                usage=usage,
            )

        # Stage 2: Redis pub/sub broadcast (optional)
        if self.redis:
            await self._run_stage_with_retry(
                stage="redis",
                coro_factory=lambda: self._publish_to_redis(usage),
                usage=usage,
            )

        # Stage 3: durable DB persistence (primary sink)
        reached_durable = await self._run_stage_with_retry(
            stage="database",
            coro_factory=lambda: self._record_to_database(usage),
            usage=usage,
        )

        if reached_durable:
            self._total_events += 1
            self._total_tokens += usage.total_tokens

    async def _run_stage_with_retry(
        self,
        *,
        stage: str,
        coro_factory: Callable[[], Awaitable[None]],
        usage: UsageData,
    ) -> bool:
        """Run one flush stage with retry + DLQ. Returns True on success.

        Classifies the first failure via
        ``gateway_billing_flush_failures_total{stage, error_type}``. Retries
        with backoffs from ``_FLUSH_RETRY_BACKOFFS``; if all attempts fail,
        pushes the record to the DLQ and increments
        ``gateway_billing_records_dropped_total{reason="max_retries_exceeded"}``.
        """
        metrics = get_metrics().request_metrics
        try:
            await coro_factory()
            return True
        except Exception as first_error:
            metrics.billing_flush_failures_total.inc(
                stage=stage, error_type=type(first_error).__name__
            )
            logger.exception("Billing flush failed (stage=%s)", stage)

        # Backoff retries
        for delay in _FLUSH_RETRY_BACKOFFS:
            await asyncio.sleep(delay)
            try:
                await coro_factory()
                return True
            except Exception as retry_error:
                metrics.billing_flush_failures_total.inc(
                    stage=stage, error_type=type(retry_error).__name__
                )
                logger.debug(
                    "Billing flush retry failed (stage=%s delay=%s): %s",
                    stage,
                    delay,
                    retry_error,
                )
                continue

        # All retries exhausted — push to DLQ
        await self._push_to_dead_letter(usage, stage=stage)
        metrics.billing_records_dropped_total.inc(reason="max_retries_exceeded")
        return False

    async def _push_to_dead_letter(self, usage: UsageData, *, stage: str) -> None:
        """LPUSH failed record to DLQ with a bounded retention window."""
        if self.redis is None:
            # No Redis → nowhere to persist the DLQ; count as a final drop.
            logger.error(
                "Cannot DLQ billing record (stage=%s, request_id=%s): no Redis client",
                stage,
                usage.request_id,
            )
            get_metrics().request_metrics.billing_records_dropped_total.inc(
                reason="dead_letter_full"
            )

            return

        try:
            payload = json.dumps(
                {
                    "stage": stage,
                    "usage": asdict(usage),
                    "dropped_at": time.time(),
                },
                default=str,
            )
            await self.redis.lpush(_BILLING_DLQ_KEY, payload)
            await self.redis.ltrim(_BILLING_DLQ_KEY, 0, _BILLING_DLQ_CAP - 1)
        except Exception:
            logger.exception(
                "Failed to push billing record to dead-letter (stage=%s)", stage
            )
            get_metrics().request_metrics.billing_records_dropped_total.inc(
                reason="dead_letter_full"
            )

    async def replay_dead_letter(self, limit: int = 100) -> dict[str, int]:
        """Replay Redis DLQ items and keep an audit copy of successful replays."""
        if self.redis is None:
            return {"attempted": 0, "replayed": 0, "failed": 0}

        attempted = 0
        replayed = 0
        failed = 0
        limit = max(int(limit or 0), 0)

        for _ in range(limit):
            raw = await self.redis.rpop(_BILLING_DLQ_KEY)
            if raw is None:
                break
            attempted += 1

            raw_text = (
                raw.decode("utf-8", errors="replace") if isinstance(raw, bytes) else str(raw)
            )

            try:
                payload = json.loads(raw_text)
                usage_payload = payload.get("usage") if isinstance(payload, dict) else None
                if not isinstance(usage_payload, dict):
                    raise ValueError("DLQ payload missing usage")
                usage = UsageData(**usage_payload)
                stage = str(payload.get("stage") or "database")

                if stage == "redis":
                    ok = await self._run_stage_with_retry(
                        stage="redis",
                        coro_factory=lambda usage=usage: self._publish_to_redis(usage),
                        usage=usage,
                    )
                elif stage == "callback" and self.callback:
                    ok = await self._run_stage_with_retry(
                        stage="callback",
                        coro_factory=lambda usage=usage: self.callback(usage),
                        usage=usage,
                    )
                else:
                    ok = await self._run_stage_with_retry(
                        stage="database",
                        coro_factory=lambda usage=usage: self._record_to_database(usage),
                        usage=usage,
                    )

                if not ok:
                    raise RuntimeError(f"replay stage failed: {stage}")

                payload["replayed_at"] = time.time()
                await self.redis.lpush(
                    _BILLING_DLQ_REPLAYED_KEY,
                    json.dumps(payload, default=str),
                )
                await self.redis.ltrim(
                    _BILLING_DLQ_REPLAYED_KEY,
                    0,
                    _BILLING_DLQ_CAP - 1,
                )
                replayed += 1
            except Exception:
                failed += 1
                logger.exception("Failed to replay billing dead-letter item")
                await self.redis.lpush(_BILLING_DLQ_KEY, raw_text)
                break

        return {"attempted": attempted, "replayed": replayed, "failed": failed}

    async def _record_to_database(self, usage: UsageData) -> None:
        """持久化使用记录到数据库 — raises on failure.

        Phase 0 hotfix: the inner try/except has been removed so that the
        caller (``_run_stage_with_retry``) can classify failures and trigger
        retry/DLQ. A silent no-op occurs only when no UsageRecorder is
        configured (best-effort skip).
        """
        from ..services.metrics import get_usage_recorder

        recorder = get_usage_recorder()
        if recorder is None:
            return
        await recorder.record_usage(
            tenant_id=usage.tenant_id or "default",
            user_id=usage.user_id or "anonymous",
            model=usage.model or "unknown",
            input_tokens=usage.input_tokens,
            output_tokens=usage.output_tokens,
            request_id=usage.request_id,
            service_id=usage.service_id,
            assistant_id=usage.assistant_id,
            provider=usage.provider,
            latency_ms=int(usage.duration_ms),
            first_token_ms=usage.first_token_latency_ms,
            request_total_duration_ms=usage.request_total_duration_ms,
            llm_inference_duration_ms=usage.llm_inference_duration_ms,
            retrieval_duration_ms=usage.retrieval_duration_ms,
            tool_call_duration_ms=usage.tool_call_duration_ms,
            agent_or_graph_overhead_ms=usage.agent_or_graph_overhead_ms,
            tool_call_breakdown=usage.tool_call_breakdown,
            error_type=usage.error_type,
            status=usage.status,
            request_type=usage.request_type,
            metadata=usage.raw_metadata,
            trace_steps=usage.trace_steps,
        )

    async def _publish_to_redis(self, usage: UsageData) -> None:
        """发布计费事件到 Redis — raises on failure (see _run_stage_with_retry)."""
        if not self.redis:
            return

        event_data = {
            "type": "billing",
            "request_id": usage.request_id,
            "service_id": usage.service_id,
            "user_id": usage.user_id,
            "tenant_id": usage.tenant_id,
            "model": usage.model,
            "provider": usage.provider,
            "assistant_id": usage.assistant_id,
            "input_tokens": usage.input_tokens,
            "output_tokens": usage.output_tokens,
            "total_tokens": usage.total_tokens,
            "status": usage.status,
            "request_type": usage.request_type,
            "timestamp": usage.timestamp,
            "duration_ms": usage.duration_ms,
        }
        await self.redis.publish("gateway:billing", json.dumps(event_data))

    def create_stream_processor(
        self,
        request_id: str,
        service_id: str,
        user_id: str = "",
        tenant_id: str = "",
        assistant_id: str = "",
        request_type: str = "proxy_run_stream",
        model_hint: str = "",
        provider_hint: str = "",
    ) -> StreamProcessor:
        """
        创建流处理器

        Args:
            request_id: 请求 ID
            service_id: 服务 ID
            user_id: 用户 ID
            tenant_id: 租户 ID
            assistant_id: Assistant ID
            request_type: 请求类型（用于统计维度）
            model_hint: 请求侧模型提示
            provider_hint: 请求侧厂商提示

        Returns:
            StreamProcessor 实例
        """
        return StreamProcessor(
            interceptor=self,
            request_id=request_id,
            service_id=service_id,
            user_id=user_id,
            tenant_id=tenant_id,
            assistant_id=assistant_id,
            request_type=request_type,
            model_hint=model_hint,
            provider_hint=provider_hint,
        )


__all__ = ["BillingInterceptor", "StreamProcessor", "UsageData"]
