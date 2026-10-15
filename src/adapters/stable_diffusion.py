from __future__ import annotations

import base64
from typing import Any

from ai_gateway_core.exceptions import ValidationFailedError
from ai_gateway_core.enums import ContentType
from ..models.request import ContentItem, UnifiedRequest
from ..models.response import UnifiedResponse
from .base import ProtocolAdapter


class Text2ImageAdapter(ProtocolAdapter):
    async def invoke(self, request: UnifiedRequest) -> UnifiedResponse:
        prompt = self._extract_prompt(request.inputs)
        params = request.parameters or {}
        service_request: dict[str, Any] = {
            "prompt": prompt,
            "negative_prompt": params.get("negative_prompt", ""),
            "width": params.get("width", 512),
            "height": params.get("height", 512),
            "steps": params.get("steps", 20),
            "cfg_scale": params.get("cfg_scale", 7.0),
            "seed": params.get("seed", -1),
        }
        endpoint = self.service.connector_config.get("endpoint", "/txt2img")
        response = await self.connector.post(endpoint, json=service_request)
        images = response.get("images") if isinstance(response, dict) else None
        if not images:
            raise ValidationFailedError("stable diffusion response missing images")
        image_bytes = base64.b64decode(images[0])
        return UnifiedResponse(
            request_id=request.request_id,
            status="success",
            outputs=[
                ContentItem(
                    type=ContentType.IMAGE,
                    data=image_bytes,
                    mime_type="image/png",
                    metadata={"seed": response.get("seed")} if isinstance(response, dict) else None,
                )
            ],
            usage={"inference_time": response.get("inference_time")}
            if isinstance(response, dict)
            else None,
        )

    def _extract_prompt(self, inputs: list[ContentItem]) -> str:
        for item in inputs:
            if item.type == ContentType.TEXT:
                return str(item.data or "")
        raise ValidationFailedError("text prompt is required")
