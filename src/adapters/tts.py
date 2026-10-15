from __future__ import annotations

from ai_gateway_core.exceptions import ValidationFailedError
from ai_gateway_core.enums import ContentType
from ..models.request import ContentItem, UnifiedRequest
from ..models.response import UnifiedResponse
from .base import ProtocolAdapter


class TTSAdapter(ProtocolAdapter):
    async def invoke(self, request: UnifiedRequest) -> UnifiedResponse:
        text = self._extract_text(request.inputs)
        params = request.parameters or {}
        response = await self.connector.post(
            "/v1/audio/speech",
            json={
                "model": params.get("model", "tts-1"),
                "input": text,
                "voice": params.get("voice", "alloy"),
                "response_format": params.get("format", "mp3"),
            },
        )
        audio_bytes = getattr(response, "content", None)
        if audio_bytes is None and isinstance(response, dict):
            audio_bytes = response.get("data")
        if audio_bytes is None:
            raise ValidationFailedError("tts response missing audio")
        return UnifiedResponse(
            request_id=request.request_id,
            status="success",
            outputs=[
                ContentItem(
                    type=ContentType.AUDIO,
                    data=audio_bytes,
                    mime_type="audio/mpeg",
                )
            ],
        )

    def _extract_text(self, inputs: list[ContentItem]) -> str:
        for item in inputs:
            if item.type == ContentType.TEXT:
                return str(item.data or "")
        raise ValidationFailedError("text input is required")
