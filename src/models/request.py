from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime
from typing import Any

from ai_gateway_core.enums import ContentType


@dataclass
class ContentItem:
    type: ContentType
    data: str | bytes | None = None
    url: str | None = None
    mime_type: str | None = None
    metadata: dict[str, Any] | None = None


@dataclass
class UnifiedRequest:
    request_id: str
    service_id: str
    inputs: list[ContentItem]
    session_id: str | None = None
    context: dict[str, Any] | None = None
    parameters: dict[str, Any] | None = None
    callback_url: str | None = None
    priority: int = 0
    user_id: str = ""
    tenant_id: str = ""
    timestamp: datetime = field(default_factory=datetime.utcnow)
