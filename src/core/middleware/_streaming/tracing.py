"""Trace header propagation for the pure ASGI middleware stack."""

from __future__ import annotations

import time
import uuid
from dataclasses import dataclass, field

from ai_gateway_core.logging import get_logger
from starlette.types import ASGIApp, Message, Receive, Scope, Send

from .base import PureASGIMiddleware

logger = get_logger("src.core.middleware.streaming")


@dataclass
class StreamingTracingConfig:
    """流式友好的追踪配置"""

    service_name: str = "gateway"
    log_requests: bool = True
    log_responses: bool = True
    exclude_paths: set[str] = field(
        default_factory=lambda: {"/health", "/health/live", "/health/ready"}
    )


class StreamingTracingMiddleware(PureASGIMiddleware):
    """
    流式友好的追踪中间件

    使用纯 ASGI 实现，避免缓冲 StreamingResponse。
    """

    TRACE_ID_HEADER = b"x-trace-id"
    SPAN_ID_HEADER = b"x-span-id"
    TRACEPARENT_HEADER = b"traceparent"

    def __init__(self, app: ASGIApp, config: StreamingTracingConfig):
        super().__init__(app)
        self.config = config

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return

        path = scope.get("path", "")
        if path in self.config.exclude_paths:
            await self.app(scope, receive, send)
            return

        # 获取或生成 trace_id
        headers = dict(scope.get("headers", []))
        incoming_traceparent = headers.get(self.TRACEPARENT_HEADER, b"").decode(
            "ascii", errors="ignore"
        )
        trace_id = self._trace_id_from_traceparent(incoming_traceparent)
        if not trace_id:
            incoming_trace_id = headers.get(self.TRACE_ID_HEADER, b"").decode(
                "ascii", errors="ignore"
            )
            trace_id = incoming_trace_id if self._is_safe_trace_id(incoming_trace_id) else ""
        if not trace_id:
            trace_id = uuid.uuid4().hex
        span_id = uuid.uuid4().hex[:16]
        traceparent = f"00-{trace_id}-{span_id}-01"

        # 注入到 state
        if "state" not in scope:
            scope["state"] = {}
        scope["state"]["trace_id"] = trace_id
        scope["state"]["span_id"] = span_id
        scope["state"]["traceparent"] = traceparent

        start_time = time.time()
        method = scope.get("method", "")

        if self.config.log_requests:
            logger.info(f"Request started: {method} {path}", extra={"trace_id": trace_id})

        status_code = 0

        async def tracing_send(message: Message) -> None:
            nonlocal status_code
            if message["type"] == "http.response.start":
                status_code = message.get("status", 0)
                headers = list(message.get("headers", []))
                headers.append((self.TRACE_ID_HEADER, trace_id.encode()))
                headers.append((self.SPAN_ID_HEADER, span_id.encode()))
                headers.append((self.TRACEPARENT_HEADER, traceparent.encode()))
                message = {**message, "headers": headers}
            await send(message)

        try:
            await self.app(scope, receive, tracing_send)

            if self.config.log_responses:
                duration = (time.time() - start_time) * 1000
                logger.info(
                    f"Request completed: {method} {path} -> {status_code} ({duration:.2f}ms)",
                    extra={
                        "trace_id": trace_id,
                        "status_code": status_code,
                        "duration_ms": duration,
                    },
                )
        except Exception as e:
            duration = (time.time() - start_time) * 1000
            logger.error(
                f"Request failed: {method} {path} ({duration:.2f}ms)",
                extra={"trace_id": trace_id, "error": str(e), "duration_ms": duration},
                exc_info=True,
            )
            raise

    @staticmethod
    def _is_safe_trace_id(value: str) -> bool:
        if not value or len(value) != 32:
            return False
        try:
            int(value, 16)
        except ValueError:
            return False
        return value != "0" * 32

    @classmethod
    def _trace_id_from_traceparent(cls, value: str) -> str:
        parts = str(value or "").strip().split("-")
        if len(parts) != 4:
            return ""
        version, trace_id, span_id, flags = parts
        if version != "00" or len(span_id) != 16 or len(flags) != 2:
            return ""
        return trace_id if cls._is_safe_trace_id(trace_id) else ""
