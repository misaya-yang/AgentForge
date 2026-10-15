"""Request logging and metrics recording for the pure ASGI stack."""

from __future__ import annotations

import asyncio
import time
import uuid
from dataclasses import dataclass, field

from ai_gateway_core.logging import get_logger
from starlette.types import ASGIApp, Message, Receive, Scope, Send

from ....services.metrics import get_metrics_recorder
from ...observability.metrics import get_metrics
from .base import PureASGIMiddleware
from .paths import is_streaming_path

logger = get_logger("src.core.middleware.streaming")


@dataclass
class StreamingLogConfig:
    """流式友好的日志配置"""

    enabled: bool = True
    log_request_body: bool = False
    log_response_body: bool = False
    exclude_paths: list[str] = field(
        default_factory=lambda: ["/health", "/health/live", "/health/ready"]
    )


class StreamingLoggingMiddleware(PureASGIMiddleware):
    """
    流式友好的请求日志中间件

    对于流式路径，仅记录请求开始，不等待响应完成。
    对于非流式路径，记录完整的请求/响应。
    """

    def __init__(self, app: ASGIApp, config: StreamingLogConfig):
        super().__init__(app)
        self.config = config
        # The event loop keeps only weak references to scheduled tasks. Keep
        # metrics writes alive until their callbacks run, then release them.
        self._background_tasks: set[asyncio.Task[None]] = set()
        self._background_task_limit = 128
        self._critical_metrics_semaphore = asyncio.Semaphore(16)
        self._critical_metrics_timeout_seconds = 1.0
        self._slow_request_threshold_ms = 1000.0

    @property
    def metrics_backlog(self) -> int:
        """Current non-critical telemetry backlog."""
        return len(self._background_tasks)

    def _set_backlog_metric(self) -> None:
        get_metrics().request_metrics.telemetry_background_backlog.set(
            float(len(self._background_tasks))
        )

    @staticmethod
    def _record_drop(reason: str) -> None:
        get_metrics().request_metrics.telemetry_records_dropped_total.inc(reason=reason)

    @staticmethod
    def _record_failure(*, priority: str, reason: str) -> None:
        get_metrics().request_metrics.telemetry_record_failures_total.inc(
            priority=priority,
            reason=reason,
        )

    def _schedule_metrics_record(
        self,
        *,
        method: str,
        path: str,
        status_code: int,
        duration_ms: float,
        user_id: str,
        service_id: str,
        error_label: str,
    ) -> bool:
        """Best-effort, non-blocking metrics recording."""
        if len(self._background_tasks) >= self._background_task_limit:
            self._record_drop("capacity")
            return False

        try:
            loop = asyncio.get_running_loop()
            metrics_recorder = get_metrics_recorder()
            task = loop.create_task(
                metrics_recorder.record_request(
                    method=method,
                    path=path,
                    status_code=status_code,
                    duration_ms=duration_ms,
                    user_id=user_id,
                    service_id=service_id,
                )
            )
        except RuntimeError as exc:
            self._record_drop("scheduler_unavailable")
            logger.debug("%s: %s", error_label, exc)
            return False

        self._background_tasks.add(task)
        self._set_backlog_metric()

        def _done_callback(done: asyncio.Task[None]) -> None:
            self._background_tasks.discard(done)
            self._set_backlog_metric()
            if done.cancelled():
                self._record_failure(priority="normal", reason="cancelled")
                logger.debug("%s: telemetry task cancelled", error_label)
                return
            exc = done.exception()
            if exc is not None:
                self._record_failure(priority="normal", reason=type(exc).__name__)
                logger.debug("%s: %s", error_label, exc)

        task.add_done_callback(_done_callback)
        return True

    async def _record_metrics(
        self,
        *,
        method: str,
        path: str,
        status_code: int,
        duration_ms: float,
        user_id: str,
        service_id: str,
        error_label: str,
        critical: bool,
    ) -> None:
        """Record critical telemetry inline; bound ordinary telemetry tasks."""
        if not critical:
            self._schedule_metrics_record(
                method=method,
                path=path,
                status_code=status_code,
                duration_ms=duration_ms,
                user_id=user_id,
                service_id=service_id,
                error_label=error_label,
            )
            return

        metrics_recorder = get_metrics_recorder()
        try:
            async with asyncio.timeout(self._critical_metrics_timeout_seconds):
                async with self._critical_metrics_semaphore:
                    await metrics_recorder.record_request(
                        method=method,
                        path=path,
                        status_code=status_code,
                        duration_ms=duration_ms,
                        user_id=user_id,
                        service_id=service_id,
                    )
        except TimeoutError:
            self._record_failure(priority="critical", reason="timeout")
            logger.warning("%s: critical telemetry timed out", error_label)
        except asyncio.CancelledError:
            self._record_failure(priority="critical", reason="cancelled")
            logger.warning("%s: critical telemetry was cancelled", error_label)
            # Telemetry cancellation must not turn an already completed HTTP
            # response into a second synthetic 499 record. A request-path
            # cancellation is re-raised by its own exception branch.
            return
        except Exception as exc:
            self._record_failure(priority="critical", reason=type(exc).__name__)
            logger.warning("%s: %s", error_label, exc)

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return

        if not self.config.enabled:
            await self.app(scope, receive, send)
            return

        path = scope.get("path", "")
        if self._is_excluded(path):
            await self.app(scope, receive, send)
            return

        # 生成 / 透传请求 ID — preserve incoming X-Request-Id for end-to-end
        # correlation (caller may already have one). Same validation rules as
        # request_logging.py: ≤64 chars, alphanumeric + safe punct.
        incoming_request_id = ""
        for header_name, header_value in scope.get("headers", []):
            if header_name == b"x-request-id":
                try:
                    incoming_request_id = header_value.decode("ascii", errors="ignore").strip()
                except Exception:
                    incoming_request_id = ""
                break
        if (
            incoming_request_id
            and len(incoming_request_id) <= 64
            and all(c.isalnum() or c in "-_." for c in incoming_request_id)
        ):
            request_id = incoming_request_id
        else:
            request_id = str(uuid.uuid4())
        if "state" not in scope:
            scope["state"] = {}
        scope["state"]["request_id"] = request_id

        start_time = time.time()
        method = scope.get("method", "")

        # 对于流式路径，仅记录请求开始
        if is_streaming_path(path):
            logger.info(f"[{request_id}] {method} {path} (streaming)")
            status_code = 200

            # 包装 send 添加请求 ID 头
            async def streaming_send(message: Message) -> None:
                nonlocal status_code
                if message["type"] == "http.response.start":
                    status_code = message.get("status", 200)
                    headers = list(message.get("headers", []))
                    headers.append((b"x-request-id", request_id.encode()))
                    message = {**message, "headers": headers}
                await send(message)

            try:
                await self.app(scope, receive, streaming_send)
            except asyncio.CancelledError:
                duration = (time.time() - start_time) * 1000
                user_info = scope.get("state", {}).get("user_info", {})
                await self._record_metrics(
                    method=method,
                    path=path,
                    status_code=499,
                    duration_ms=duration,
                    user_id=user_info.get("user_id", ""),
                    service_id=scope.get("state", {}).get("service_id", ""),
                    error_label="Failed to record cancelled streaming metrics",
                    critical=True,
                )
                raise
            except Exception:
                duration = (time.time() - start_time) * 1000
                user_info = scope.get("state", {}).get("user_info", {})
                await self._record_metrics(
                    method=method,
                    path=path,
                    status_code=500,
                    duration_ms=duration,
                    user_id=user_info.get("user_id", ""),
                    service_id=scope.get("state", {}).get("service_id", ""),
                    error_label="Failed to record streaming error metrics",
                    critical=True,
                )
                raise

            duration = (time.time() - start_time) * 1000
            logger.info(f"[{request_id}] {method} {path} streaming completed ({duration:.2f}ms)")

            # Record metrics for streaming requests
            user_info = scope.get("state", {}).get("user_info", {})
            # Streaming terminals can encode cancellation or an unknown side
            # effect in a successful HTTP stream. Preserve the record inline
            # rather than silently dropping it when the normal queue is full.
            await self._record_metrics(
                method=method,
                path=path,
                status_code=status_code,
                duration_ms=duration,
                user_id=user_info.get("user_id", ""),
                service_id=scope.get("state", {}).get("service_id", ""),
                error_label="Failed to record streaming metrics",
                critical=True,
            )

            return

        # 非流式路径记录完整请求/响应
        status_code = 0

        async def logging_send(message: Message) -> None:
            nonlocal status_code
            if message["type"] == "http.response.start":
                status_code = message.get("status", 0)
                headers = list(message.get("headers", []))
                headers.append((b"x-request-id", request_id.encode()))
                message = {**message, "headers": headers}
            await send(message)

        try:
            await self.app(scope, receive, logging_send)
            duration = (time.time() - start_time) * 1000
            logger.info(f"[{request_id}] {method} {path} -> {status_code} ({duration:.2f}ms)")

            # Record metrics for dashboard
            user_info = scope.get("state", {}).get("user_info", {})
            await self._record_metrics(
                method=method,
                path=path,
                status_code=status_code,
                duration_ms=duration,
                user_id=user_info.get("user_id", ""),
                service_id=scope.get("state", {}).get("service_id", ""),
                error_label="Failed to record metrics",
                critical=(
                    status_code >= 400 or duration >= self._slow_request_threshold_ms
                ),
            )

        except asyncio.CancelledError:
            duration = (time.time() - start_time) * 1000
            user_info = scope.get("state", {}).get("user_info", {})
            await self._record_metrics(
                method=method,
                path=path,
                status_code=499,
                duration_ms=duration,
                user_id=user_info.get("user_id", ""),
                service_id=scope.get("state", {}).get("service_id", ""),
                error_label="Failed to record cancelled request metrics",
                critical=True,
            )
            raise
        except Exception as e:
            duration = (time.time() - start_time) * 1000
            logger.error(f"[{request_id}] {method} {path} -> ERROR ({duration:.2f}ms): {e}")

            # Record error metrics
            user_info = scope.get("state", {}).get("user_info", {})
            await self._record_metrics(
                method=method,
                path=path,
                status_code=500,
                duration_ms=duration,
                user_id=user_info.get("user_id", ""),
                service_id=scope.get("state", {}).get("service_id", ""),
                error_label="Failed to record error metrics",
                critical=True,
            )

            raise

    def _is_excluded(self, path: str) -> bool:
        """检查路径是否被排除"""
        return any(path == ep or path.startswith(ep + "/") for ep in self.config.exclude_paths)
