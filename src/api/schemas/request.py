from __future__ import annotations

import uuid
from datetime import datetime
from typing import Any

from pydantic import BaseModel, ConfigDict

from ai_gateway_core.enums import ContentType
from ...models.request import ContentItem, UnifiedRequest


class ContentItemSchema(BaseModel):
    model_config = ConfigDict(extra="allow")

    type: ContentType
    data: str | bytes | None = None
    url: str | None = None
    mime_type: str | None = None
    metadata: dict[str, Any] | None = None

    def to_domain(self) -> ContentItem:
        return ContentItem(
            type=self.type,
            data=self.data,
            url=self.url,
            mime_type=self.mime_type,
            metadata=self.metadata,
        )


class UnifiedRequestSchema(BaseModel):
    model_config = ConfigDict(extra="allow")

    request_id: str | None = None
    service_id: str
    inputs: list[ContentItemSchema]
    session_id: str | None = None
    context: dict[str, Any] | None = None
    parameters: dict[str, Any] | None = None
    callback_url: str | None = None
    priority: int = 0
    user_id: str | None = None
    tenant_id: str | None = None
    timestamp: datetime | None = None

    def to_domain(self, default_user_id: str = "", default_tenant_id: str = "") -> UnifiedRequest:
        rid = self.request_id or f"req_{uuid.uuid4().hex}"
        return UnifiedRequest(
            request_id=rid,
            service_id=self.service_id,
            inputs=[i.to_domain() for i in self.inputs],
            session_id=self.session_id,
            context=self.context,
            parameters=self.parameters,
            callback_url=self.callback_url,
            priority=self.priority or 0,
            # User identity must be derived from gateway auth/session context,
            # not from client-provided request fields.
            user_id=default_user_id,
            tenant_id=default_tenant_id,
            timestamp=self.timestamp or datetime.utcnow(),
        )
