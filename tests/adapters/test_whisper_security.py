from __future__ import annotations

from types import SimpleNamespace

import pytest
from ai_gateway_core.enums import ContentType

from src.adapters import whisper as whisper_module
from src.adapters.whisper import WhisperAdapter
from src.models.request import ContentItem


class _ForbiddenRawClient:
    async def get(self, _url: str):
        raise AssertionError("raw httpx client must not be used for audio URL fetch")


@pytest.mark.asyncio
async def test_whisper_url_audio_uses_safe_fetch(monkeypatch) -> None:
    calls: list[str] = []

    async def fake_safe_fetch(url: str, **kwargs):
        calls.append(url)
        return b"audio-bytes"

    monkeypatch.setattr(whisper_module, "safe_fetch", fake_safe_fetch, raising=False)

    adapter = WhisperAdapter.__new__(WhisperAdapter)
    adapter.connector = SimpleNamespace(_client=_ForbiddenRawClient())

    result = await adapter._load_audio(
        ContentItem(type=ContentType.AUDIO, data=None, url="https://cdn.example/audio.mp3")
    )

    assert result == b"audio-bytes"
    assert calls == ["https://cdn.example/audio.mp3"]
