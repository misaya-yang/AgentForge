"""SSE stream usage extraction for gateway billing."""

from __future__ import annotations

import json
import time
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

from ai_gateway_core.logging import get_logger

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

if TYPE_CHECKING:
    from .billing_interceptor import BillingInterceptor

# Keep the historical logger category even though the implementation moved.
logger = get_logger(f"{__package__}.billing_interceptor")


@dataclass
class UsageData:
    """Token 使用量数据"""

    # Token 计数
    input_tokens: int = 0
    output_tokens: int = 0
    total_tokens: int = 0

    # 请求信息
    request_id: str = ""
    service_id: str = ""
    user_id: str = ""
    tenant_id: str = ""

    # 模型信息
    model: str = ""
    provider: str = ""
    assistant_id: str = ""
    status: str = "success"
    request_type: str = "chat"

    # 时间信息
    timestamp: float = field(default_factory=time.time)
    duration_ms: float = 0.0
    first_token_latency_ms: int = 0
    request_total_duration_ms: int = 0
    llm_inference_duration_ms: int = 0
    retrieval_duration_ms: int = 0
    tool_call_duration_ms: int = 0
    agent_or_graph_overhead_ms: int = 0
    tool_call_breakdown: dict[str, int] = field(default_factory=dict)
    error_type: str | None = None
    trace_steps: list[dict[str, Any]] = field(default_factory=list)

    # 原始元数据
    raw_metadata: dict[str, Any] = field(default_factory=dict)

    def __post_init__(self):
        if self.total_tokens == 0:
            self.total_tokens = self.input_tokens + self.output_tokens
        if self.request_total_duration_ms <= 0 and self.duration_ms > 0:
            self.request_total_duration_ms = int(self.duration_ms)


class StreamProcessor:
    """
    单个请求的流处理器

    解析 SSE 流并提取 metadata。
    """

    MAX_BUFFER_CHARS = 1_048_576

    def __init__(
        self,
        interceptor: BillingInterceptor,
        request_id: str,
        service_id: str,
        user_id: str = "",
        tenant_id: str = "",
        assistant_id: str = "",
        request_type: str = "proxy_run_stream",
        model_hint: str = "",
        provider_hint: str = "",
    ):
        self.interceptor = interceptor
        self.request_id = request_id
        self.service_id = service_id
        self.user_id = user_id
        self.tenant_id = tenant_id
        self.assistant_id = assistant_id
        self.request_type = request_type
        self.model_hint = model_hint
        self.provider_hint = provider_hint

        self.start_time = time.time()

        # SSE 解析状态
        self._buffer = ""
        self._buffer_overflowed = False
        self._discarding_oversized_event = False
        self._usage_collected = False
        self._realtime_started = False  # 跟踪是否已开始实时指标记录
        self._request_complete_recorded = False
        self._status = "success"
        self._error_metadata: dict[str, Any] = {}
        self._first_chunk_offset_ms = 0
        self._event_offsets_ms: dict[str, int] = {}
        self._duration_breakdown: dict[str, Any] = {}
        self._pending_usage: UsageData | None = None

    async def process_chunk(self, chunk: bytes) -> bytes:
        """
        处理流数据块

        Args:
            chunk: 原始数据块

        Returns:
            原样返回数据块（透传）
        """
        # 首次调用时记录请求开始（用于实时指标）
        if not self._realtime_started:
            self._realtime_started = True
            if self.interceptor.realtime_metrics:
                try:
                    await self.interceptor.realtime_metrics.record_request_start(
                        request_id=self.request_id,
                        user_id=self.user_id,
                        tenant_id=self.tenant_id,
                    )
                except Exception as e:
                    logger.debug(f"Failed to record request start: {e}")

        if self._first_chunk_offset_ms <= 0:
            self._first_chunk_offset_ms = max(
                int((time.time() - self.start_time) * 1000),
                1,
            )

        try:
            # 尝试解码
            text = chunk.decode("utf-8", errors="ignore")
            # 统一行分隔符，避免上游使用 CRLF 时无法按 SSE 事件分块。
            text = text.replace("\r\n", "\n").replace("\r", "\n")

            if self._discarding_oversized_event:
                self._buffer += text
                if "\n\n" not in self._buffer:
                    # Retain only a possible cross-chunk event-boundary prefix.
                    self._buffer = "\n" if self._buffer.endswith("\n") else ""
                    return chunk
                _, self._buffer = self._buffer.split("\n\n", 1)
                self._discarding_oversized_event = False
            else:
                self._buffer += text

            # Parse complete events before bounding the remaining incomplete
            # event. This avoids dropping valid usage events when one upstream
            # chunk contains many complete SSE records.
            await self._parse_events()

            # Guard against unbounded buffer growth from malformed/malicious
            # streams that never emit "\n\n" (SSE event boundary).
            if len(self._buffer) > self.MAX_BUFFER_CHARS:
                if not self._buffer_overflowed:
                    self._buffer_overflowed = True
                    logger.warning(
                        "[Billing] StreamProcessor buffer exceeded %d characters "
                        "for request=%s – discarding oversized event",
                        self.MAX_BUFFER_CHARS,
                        self.request_id,
                    )
                if not self._usage_collected:
                    self._mark_error("billing_stream_buffer_overflow")
                self._discarding_oversized_event = True
                self._buffer = "\n" if self._buffer.endswith("\n") else ""

        except Exception as e:
            logger.debug(f"Error processing chunk: {e}")

        # 透传原始数据
        return chunk

    async def _parse_events(self) -> None:
        """解析缓冲区中的 SSE 事件"""
        while "\n\n" in self._buffer:
            event_block, self._buffer = self._buffer.split("\n\n", 1)
            await self._process_event_block(event_block)

    async def _process_event_block(self, block: str) -> None:
        """处理单个 SSE 事件块"""
        event_type = ""
        data_lines = []

        for line in block.split("\n"):
            if line.startswith("event:"):
                event_type = line[6:].strip()
            elif line.startswith("data:"):
                data_lines.append(line[5:].strip())

        if event_type.lower() == "error":
            self._mark_error("stream_error_event")
        elif event_type:
            lowered = event_type.lower()
            if lowered not in self._event_offsets_ms:
                self._event_offsets_ms[lowered] = max(
                    int((time.time() - self.start_time) * 1000),
                    0,
                )

        # 不限制事件类型，直接尝试从任意事件解析 usage。
        # LangGraph/OpenAI 在不同版本会把 usage 放在 metadata/messages/updates 等事件里。
        if data_lines:
            data_str = "\n".join(data_lines)
            await self._extract_usage(data_str, event_type)

    @staticmethod
    def _should_record_fallback(request_type: str) -> bool:
        normalized = (request_type or "").lower()
        return normalized.startswith("proxy_run_") or normalized.startswith("run_")

    @staticmethod
    def _extract_error_payload(data: dict[str, Any]) -> dict[str, str]:
        """从事件数据中抽取上游错误信息，统一元数据字段。"""
        if isinstance(data.get("error"), str) and data.get("error") and "message" in data:
            error_type = str(data.get("error"))
            error_message = redact_sensitive_text(str(data.get("message") or ""))
            payload: dict[str, str] = {"upstream_error_type": error_type[:128]}
            if error_message:
                payload["upstream_error_message"] = error_message[:512]
            return payload

        error_payload = data.get("__error__") or data.get("error")
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

        payload: dict[str, str] = {"upstream_error_type": error_type[:128]}
        if error_message:
            payload["upstream_error_message"] = error_message[:512]
        return payload

    def _mark_error(self, error_type: str, error_message: str = "") -> None:
        """标记本次流为错误状态。"""
        self._status = "error"
        self._error_metadata["upstream_error_type"] = str(error_type)[:128]
        if error_message:
            self._error_metadata["upstream_error_message"] = redact_sensitive_text(
                str(error_message)
            )[:512]

    async def _record_request_complete(self) -> None:
        """记录实时请求完成事件（只记录一次）。"""
        if self._request_complete_recorded or not self.interceptor.realtime_metrics:
            return

        duration_ms = (time.time() - self.start_time) * 1000
        status_code = 200 if self._status == "success" else 500

        try:
            await self.interceptor.realtime_metrics.record_request_complete(
                status_code=status_code,
                duration_ms=int(duration_ms),
            )
            self._request_complete_recorded = True
        except Exception as e:
            logger.debug(f"Failed to record request complete: {e}")

    async def _extract_usage(self, data_str: str, event_type: str) -> None:
        """从数据中提取 usage 信息

        SPO-05 / G1: a stream may emit partial usage before the final usage;
        every usage-bearing event records, and the usage_records conflict
        branch takes the monotonic GREATEST across partial + final writes.
        """
        if not data_str or data_str == "[DONE]":
            return

        try:
            data = json.loads(data_str)
        except json.JSONDecodeError:
            if event_type.lower() == "error":
                self._mark_error("stream_error_event", data_str)
            return

        if isinstance(data, dict):
            error_meta = self._extract_error_payload(data)
            if error_meta:
                self._status = "error"
                self._error_metadata.update(error_meta)
            self._merge_duration_breakdown(data)

        usage = extract_token_usage(data)
        if usage:
            await self._record_usage(usage, data)
            return

        # 仅做调试日志，避免噪声。
        if event_type == "metadata" and isinstance(data, dict) and "run_id" in data:
            logger.debug(f"LangGraph run metadata: run_id={data.get('run_id')}")

    def _merge_duration_breakdown(self, payload: Any) -> None:
        extracted = extract_duration_breakdown(payload)
        if not extracted:
            return

        for key, value in extracted.items():
            if key == "tool_call_breakdown" and isinstance(value, dict):
                existing = self._duration_breakdown.get("tool_call_breakdown", {})
                if not isinstance(existing, dict):
                    existing = {}
                merged = dict(existing)
                for tool_name, duration in value.items():
                    try:
                        parsed = max(int(float(duration)), 0)
                    except (TypeError, ValueError):
                        parsed = 0
                    if parsed > 0:
                        merged[str(tool_name)] = int(merged.get(str(tool_name), 0)) + parsed
                self._duration_breakdown["tool_call_breakdown"] = merged
                continue

            try:
                parsed = max(int(float(value)), 0)
            except (TypeError, ValueError):
                continue
            if parsed <= 0:
                continue
            current = int(self._duration_breakdown.get(key, 0) or 0)
            self._duration_breakdown[key] = max(current, parsed)

    def _build_trace_steps(
        self, total_duration_ms: int, breakdown: dict[str, int]
    ) -> list[dict[str, Any]]:
        steps: list[dict[str, Any]] = []
        steps.append(
            {
                "name": "gateway.request_total",
                "start_offset_ms": 0,
                "duration_ms": total_duration_ms,
                "status": self._status,
            }
        )

        phase_order = [
            ("agent.retrieval", breakdown.get("retrieval_duration_ms", 0)),
            ("agent.tool_calls", breakdown.get("tool_call_duration_ms", 0)),
            ("llm.inference", breakdown.get("llm_inference_duration_ms", 0)),
            ("agent.overhead", breakdown.get("agent_or_graph_overhead_ms", 0)),
        ]

        offset = 0
        for name, duration in phase_order:
            parsed = max(int(duration or 0), 0)
            if parsed <= 0:
                continue
            steps.append(
                {
                    "name": name,
                    "start_offset_ms": offset,
                    "duration_ms": parsed,
                    "status": "success",
                }
            )
            offset += parsed

        first_token = max(int(breakdown.get("first_token_latency_ms", 0) or 0), 0)
        if first_token > 0:
            steps.append(
                {
                    "name": "llm.first_token",
                    "start_offset_ms": 0,
                    "duration_ms": first_token,
                    "status": "success",
                }
            )

        for event_name, event_offset in sorted(
            self._event_offsets_ms.items(), key=lambda item: item[1]
        ):
            steps.append(
                {
                    "name": f"stream.event.{event_name}",
                    "start_offset_ms": max(int(event_offset), 0),
                    "duration_ms": 0,
                    "status": "success",
                }
            )

        return steps

    async def _record_usage(self, usage: dict[str, Any], raw_data: dict[str, Any]) -> None:
        """记录 usage 数据"""
        self._usage_collected = True

        # usage 在 _extract_usage 中已经做过归一化。
        input_tokens = int(usage.get("input_tokens") or 0)
        output_tokens = int(usage.get("output_tokens") or 0)
        total_tokens = int(usage.get("total_tokens") or (input_tokens + output_tokens))

        # 计算耗时
        duration_ms = (time.time() - self.start_time) * 1000
        breakdown = ensure_duration_breakdown(
            request_total_duration_ms=int(duration_ms),
            first_token_latency_ms=self._duration_breakdown.get(
                "first_token_latency_ms",
                self._first_chunk_offset_ms,
            ),
            llm_inference_duration_ms=self._duration_breakdown.get(
                "llm_inference_duration_ms", 0
            ),
            retrieval_duration_ms=self._duration_breakdown.get("retrieval_duration_ms", 0),
            tool_call_duration_ms=self._duration_breakdown.get("tool_call_duration_ms", 0),
            agent_or_graph_overhead_ms=self._duration_breakdown.get(
                "agent_or_graph_overhead_ms",
                0,
            ),
        )
        tool_call_breakdown = self._duration_breakdown.get("tool_call_breakdown", {})
        if not isinstance(tool_call_breakdown, dict):
            tool_call_breakdown = {}
        trace_steps = self._build_trace_steps(
            breakdown["request_total_duration_ms"], breakdown
        )
        error_type = classify_error_type(
            status=self._status,
            upstream_error_type=str(self._error_metadata.get("upstream_error_type") or ""),
            upstream_error_message=str(
                self._error_metadata.get("upstream_error_message") or ""
            ),
        )

        model_name = extract_model(raw_data) or self.model_hint or "langgraph-agent"
        provider = extract_provider(raw_data) or self.provider_hint or "unattributed"
        assistant_id = self.assistant_id or extract_assistant_id(raw_data) or ""
        metadata = dict(raw_data) if isinstance(raw_data, dict) else {"raw_data": raw_data}
        metadata.update(self._error_metadata)
        metadata.setdefault("source", "transparent_proxy_stream")
        metadata.setdefault("request_type", self.request_type)
        metadata.setdefault("provider", provider)
        metadata.setdefault("model", model_name)
        metadata.setdefault("token_source", "upstream")
        metadata["error_type"] = error_type
        metadata["duration_breakdown"] = breakdown
        metadata["tool_call_breakdown"] = tool_call_breakdown

        # 创建 UsageData
        usage_data = UsageData(
            input_tokens=input_tokens,
            output_tokens=output_tokens,
            total_tokens=total_tokens,
            request_id=self.request_id,
            service_id=self.service_id,
            user_id=self.user_id,
            tenant_id=self.tenant_id,
            model=model_name,
            provider=provider,
            assistant_id=assistant_id,
            status=self._status,
            request_type=self.request_type,
            timestamp=time.time(),
            duration_ms=duration_ms,
            first_token_latency_ms=breakdown["first_token_latency_ms"],
            request_total_duration_ms=breakdown["request_total_duration_ms"],
            llm_inference_duration_ms=breakdown["llm_inference_duration_ms"],
            retrieval_duration_ms=breakdown["retrieval_duration_ms"],
            tool_call_duration_ms=breakdown["tool_call_duration_ms"],
            agent_or_graph_overhead_ms=breakdown["agent_or_graph_overhead_ms"],
            tool_call_breakdown=tool_call_breakdown,
            error_type=error_type,
            trace_steps=trace_steps,
            raw_metadata=metadata,
        )

        logger.info(
            f"[Billing] request={self.request_id} "
            f"input={input_tokens} output={output_tokens} total={total_tokens} "
            f"duration={duration_ms:.2f}ms"
        )

        # Usage events in SSE streams are cumulative snapshots. Keep only the
        # monotonic latest observation and publish it once from finalize();
        # appending every partial snapshot double-counts realtime and durable
        # aggregates when partial + final cross a recorder flush boundary.
        pending = self._pending_usage
        if pending is None:
            self._pending_usage = usage_data
        else:
            pending.input_tokens = max(pending.input_tokens, usage_data.input_tokens)
            pending.output_tokens = max(pending.output_tokens, usage_data.output_tokens)
            pending.total_tokens = max(
                pending.total_tokens,
                usage_data.total_tokens,
                pending.input_tokens + pending.output_tokens,
            )
            pending.duration_ms = max(pending.duration_ms, usage_data.duration_ms)
            pending.first_token_latency_ms = max(
                pending.first_token_latency_ms, usage_data.first_token_latency_ms
            )
            pending.request_total_duration_ms = max(
                pending.request_total_duration_ms,
                usage_data.request_total_duration_ms,
            )
            pending.llm_inference_duration_ms = max(
                pending.llm_inference_duration_ms,
                usage_data.llm_inference_duration_ms,
            )
            pending.retrieval_duration_ms = max(
                pending.retrieval_duration_ms, usage_data.retrieval_duration_ms
            )
            pending.tool_call_duration_ms = max(
                pending.tool_call_duration_ms, usage_data.tool_call_duration_ms
            )
            pending.agent_or_graph_overhead_ms = max(
                pending.agent_or_graph_overhead_ms,
                usage_data.agent_or_graph_overhead_ms,
            )
            pending.status = usage_data.status
            pending.error_type = usage_data.error_type
            pending.trace_steps = usage_data.trace_steps
            pending.tool_call_breakdown = usage_data.tool_call_breakdown
            pending.raw_metadata.update(usage_data.raw_metadata)

    async def _flush_pending_usage(self) -> None:
        pending = self._pending_usage
        if pending is None:
            return
        self._pending_usage = None

        pending.status = self._status
        pending.error_type = classify_error_type(
            status=self._status,
            upstream_error_type=str(
                self._error_metadata.get("upstream_error_type") or ""
            ),
            upstream_error_message=str(
                self._error_metadata.get("upstream_error_message") or ""
            ),
        )
        pending.raw_metadata.update(self._error_metadata)
        pending.raw_metadata["error_type"] = pending.error_type

        if self.interceptor.realtime_metrics:
            try:
                await self.interceptor.realtime_metrics.record_token_usage(
                    input_tokens=pending.input_tokens,
                    output_tokens=pending.output_tokens,
                )
            except Exception as e:
                logger.debug(f"Failed to record realtime token metrics: {e}")

        async with self.interceptor._buffer_lock:
            self.interceptor._buffer.append(pending)
            if len(self.interceptor._buffer) >= self.interceptor.buffer_size:
                self.interceptor._spawn_tracked_flush()

    async def finalize(self) -> None:
        """
        完成流处理

        处理剩余缓冲区数据并记录请求结束。
        """
        if self._buffer:
            await self._parse_events()

        await self._flush_pending_usage()

        if not self._usage_collected and self._should_record_fallback(self.request_type):
            duration_ms = (time.time() - self.start_time) * 1000
            fallback_breakdown = ensure_duration_breakdown(
                request_total_duration_ms=int(duration_ms),
                first_token_latency_ms=self._first_chunk_offset_ms,
            )
            error_type = classify_error_type(
                status=self._status,
                upstream_error_type=str(self._error_metadata.get("upstream_error_type") or ""),
                upstream_error_message=str(
                    self._error_metadata.get("upstream_error_message") or ""
                ),
            )
            usage_data = UsageData(
                input_tokens=0,
                output_tokens=0,
                total_tokens=0,
                request_id=self.request_id,
                service_id=self.service_id,
                user_id=self.user_id,
                tenant_id=self.tenant_id,
                model=self.model_hint or "langgraph-agent",
                provider=self.provider_hint or "unattributed",
                assistant_id=self.assistant_id,
                status=self._status,
                request_type=self.request_type,
                timestamp=time.time(),
                duration_ms=duration_ms,
                first_token_latency_ms=fallback_breakdown["first_token_latency_ms"],
                request_total_duration_ms=fallback_breakdown["request_total_duration_ms"],
                llm_inference_duration_ms=fallback_breakdown["llm_inference_duration_ms"],
                retrieval_duration_ms=fallback_breakdown["retrieval_duration_ms"],
                tool_call_duration_ms=fallback_breakdown["tool_call_duration_ms"],
                agent_or_graph_overhead_ms=fallback_breakdown[
                    "agent_or_graph_overhead_ms"
                ],
                error_type=error_type,
                trace_steps=self._build_trace_steps(
                    fallback_breakdown["request_total_duration_ms"],
                    fallback_breakdown,
                ),
                raw_metadata={
                    "source": "transparent_proxy_stream",
                    "request_type": self.request_type,
                    "token_source": "zero_on_failure"
                    if self._status != "success"
                    else "estimated",
                    "error_type": error_type,
                    "duration_breakdown": fallback_breakdown,
                    **self._error_metadata,
                },
            )

            async with self.interceptor._buffer_lock:
                self.interceptor._buffer.append(usage_data)
                # Phase 0 hotfix: tracked so stop() can drain before shutdown
                if len(self.interceptor._buffer) >= self.interceptor.buffer_size:
                    self.interceptor._spawn_tracked_flush()

        await self._record_request_complete()

        # 记录请求结束（用于实时并发计数）
        if self._realtime_started and self.interceptor.realtime_metrics:
            try:
                await self.interceptor.realtime_metrics.record_request_end(
                    request_id=self.request_id,
                    user_id=self.user_id,
                    tenant_id=self.tenant_id,
                )
            except Exception as e:
                logger.debug(f"Failed to record request end: {e}")


__all__ = ["StreamProcessor", "UsageData"]
