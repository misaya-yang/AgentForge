from __future__ import annotations

from typing import Any

from ai_gateway_core.enums import ContentType
from ..models.request import ContentItem, UnifiedRequest
from ..models.response import UnifiedResponse
from .base import ProtocolAdapter


class ComfyUIAdapter(ProtocolAdapter):
    async def invoke(self, request: UnifiedRequest) -> UnifiedResponse:
        workflow: dict[str, Any] = request.parameters or {}
        ws = await self.connector.websocket()
        try:
            await ws.send_json({"type": "execute", "data": workflow})
            async for msg in ws:
                if msg.get("type") == "executed":
                    return UnifiedResponse(
                        request_id=request.request_id,
                        status="success",
                        outputs=[ContentItem(type=ContentType.JSON, data=msg)],
                    )
                if msg.get("type") == "error":
                    return UnifiedResponse(
                        request_id=request.request_id,
                        status="error",
                        outputs=[],
                        error={"message": msg.get("error")},
                    )
        finally:
            await ws.close()
        return UnifiedResponse(
            request_id=request.request_id,
            status="error",
            outputs=[],
            error={"message": "comfyui no result"},
        )
