from __future__ import annotations

import base64

from ai_gateway_core.enums import ContentType
from ai_gateway_core.exceptions import ValidationFailedError
from ai_gateway_core.security import SafeFetchError, safe_fetch

from ..models.request import ContentItem, UnifiedRequest
from ..models.response import UnifiedResponse
from .base import ProtocolAdapter

MAX_AUDIO_FETCH_BYTES = 50 * 1024 * 1024


class WhisperAdapter(ProtocolAdapter):
    async def invoke(self, request: UnifiedRequest) -> UnifiedResponse:
        audio_item = self._find_audio_input(request.inputs)
        audio_data = await self._load_audio(audio_item)
        params = request.parameters or {}
        response = await self.connector.post(
            "/v1/audio/transcriptions",
            files={"file": ("audio", audio_data)},
            data={
                "model": params.get("model", "whisper-1"),
                "language": params.get("language"),
                "response_format": params.get("format", "json"),
            },
        )
        text = response.get("text") if isinstance(response, dict) else None
        if text is None:
            text = str(response)
        return UnifiedResponse(
            request_id=request.request_id,
            status="success",
            outputs=[ContentItem(type=ContentType.TEXT, data=text)],
        )

    def _find_audio_input(self, inputs: list[ContentItem]) -> ContentItem:
        for item in inputs:
            if item.type == ContentType.AUDIO:
                return item
        raise ValidationFailedError("audio input is required")

    async def _load_audio(self, item: ContentItem) -> bytes:
        if isinstance(item.data, (bytes, bytearray)):
            return bytes(item.data)
        if isinstance(item.data, str) and item.data:
            try:
                return base64.b64decode(item.data)
            except Exception:
                return item.data.encode("utf-8")
        if item.url:
            try:
                return await safe_fetch(item.url, max_bytes=MAX_AUDIO_FETCH_BYTES, timeout=30.0)
            except SafeFetchError as exc:
                raise ValidationFailedError(f"failed to fetch audio url: {exc}") from exc
        raise ValidationFailedError("audio data or url is required")
