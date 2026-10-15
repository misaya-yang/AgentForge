from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from typing import Any

from ai_gateway_core.enums import StreamEventType
from .request import ContentItem


@dataclass
class UnifiedResponse:
    request_id: str
    status: str
    outputs: list[ContentItem]
    session_id: str | None = None
    task_id: str | None = None
    usage: dict[str, Any] | None = None
    error: dict[str, Any] | None = None
    metadata: dict[str, Any] | None = None
    timestamp: datetime = field(default_factory=datetime.utcnow)


@dataclass
class ToolCall:
    """工具调用信息"""

    tool_call_id: str
    name: str
    arguments: str = ""  # JSON 字符串格式的参数
    status: str = "pending"  # pending, running, completed, error


@dataclass
class StreamChunk:
    request_id: str
    chunk_index: int
    content: ContentItem
    is_final: bool = False
    event_type: StreamEventType = StreamEventType.TEXT_DELTA
    tool_call: ToolCall | None = None
    metadata: dict[str, Any] | None = None
