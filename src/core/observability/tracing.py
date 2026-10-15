"""
分布式追踪模块

提供：
- 请求追踪 ID 生成和传播
- Span 管理
- 上下文传播
"""

from __future__ import annotations

import time
import uuid
from contextvars import ContextVar
from dataclasses import dataclass, field
from typing import Any

from ai_gateway_core.logging import LogContext

# 追踪上下文
_trace_context: ContextVar[TraceContext | None] = ContextVar("trace_context", default=None)


def generate_trace_id() -> str:
    """生成唯一的追踪 ID"""
    return uuid.uuid4().hex


def generate_span_id() -> str:
    """生成唯一的 Span ID"""
    return uuid.uuid4().hex[:16]


@dataclass
class Span:
    """
    追踪 Span

    表示一个操作的追踪单元。
    """

    span_id: str
    name: str
    parent_span_id: str | None = None
    start_time: float = field(default_factory=time.time)
    end_time: float | None = None
    status: str = "ok"  # ok, error
    attributes: dict[str, Any] = field(default_factory=dict)
    events: list[dict[str, Any]] = field(default_factory=list)

    def end(self, status: str = "ok") -> None:
        """结束 Span"""
        self.end_time = time.time()
        self.status = status

    @property
    def duration_ms(self) -> float | None:
        """获取持续时间（毫秒）"""
        if self.end_time is None:
            return None
        return (self.end_time - self.start_time) * 1000

    def add_event(self, name: str, attributes: dict[str, Any] | None = None) -> None:
        """添加事件"""
        self.events.append(
            {
                "name": name,
                "timestamp": time.time(),
                "attributes": attributes or {},
            }
        )

    def set_attribute(self, key: str, value: Any) -> None:
        """设置属性"""
        self.attributes[key] = value

    def to_dict(self) -> dict[str, Any]:
        """转换为字典"""
        return {
            "span_id": self.span_id,
            "name": self.name,
            "parent_span_id": self.parent_span_id,
            "start_time": self.start_time,
            "end_time": self.end_time,
            "duration_ms": self.duration_ms,
            "status": self.status,
            "attributes": self.attributes,
            "events": self.events,
        }


@dataclass
class TraceContext:
    """
    追踪上下文

    在请求生命周期内维护追踪信息。
    """

    trace_id: str
    root_span: Span
    current_span: Span
    spans: list[Span] = field(default_factory=list)

    # 请求元数据
    method: str = ""
    path: str = ""
    client_ip: str = ""
    user_id: str | None = None
    tenant_id: str | None = None

    def __post_init__(self):
        self.spans.append(self.root_span)

    def start_span(
        self,
        name: str,
        attributes: dict[str, Any] | None = None,
    ) -> Span:
        """
        开始新的 Span

        Args:
            name: Span 名称
            attributes: 初始属性

        Returns:
            新创建的 Span
        """
        span = Span(
            span_id=generate_span_id(),
            name=name,
            parent_span_id=self.current_span.span_id,
            attributes=attributes or {},
        )
        self.spans.append(span)
        self.current_span = span
        return span

    def end_span(self, status: str = "ok") -> None:
        """结束当前 Span"""
        self.current_span.end(status)

        # 返回到父 Span
        if self.current_span.parent_span_id:
            for span in reversed(self.spans):
                if span.span_id == self.current_span.parent_span_id:
                    self.current_span = span
                    break

    def to_log_context(self) -> LogContext:
        """转换为日志上下文"""
        return LogContext(
            trace_id=self.trace_id,
            span_id=self.current_span.span_id,
            user_id=self.user_id,
            tenant_id=self.tenant_id,
            client_ip=self.client_ip,
        )

    def to_dict(self) -> dict[str, Any]:
        """转换为字典"""
        return {
            "trace_id": self.trace_id,
            "method": self.method,
            "path": self.path,
            "client_ip": self.client_ip,
            "user_id": self.user_id,
            "tenant_id": self.tenant_id,
            "spans": [span.to_dict() for span in self.spans],
        }


def get_trace_context() -> TraceContext | None:
    """获取当前追踪上下文"""
    return _trace_context.get()


def set_trace_context(context: TraceContext) -> None:
    """设置当前追踪上下文"""
    _trace_context.set(context)


def clear_trace_context() -> None:
    """清除当前追踪上下文"""
    _trace_context.set(None)
