from __future__ import annotations

import pytest
from knowledge_service.services.knowledge.metadata_extractor import MetadataExtractor


@pytest.fixture(autouse=True)
def _clear_dashscope_endpoint_env(monkeypatch: pytest.MonkeyPatch) -> None:
    for name in (
        "DASHSCOPE_API_KEY",
        "DASHSCOPE_BASE_URL",
        "DASHSCOPE_CHAT_API_KEY",
        "DASHSCOPE_CHAT_BASE_URL",
    ):
        monkeypatch.delenv(name, raising=False)


@pytest.mark.parametrize(
    "configured_url",
    [
        "https://dashscope-intl.aliyuncs.com",
        "https://dashscope-intl.aliyuncs.com/api/v1",
        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions",
    ],
)
def test_metadata_extractor_uses_normalized_chat_endpoint(
    monkeypatch: pytest.MonkeyPatch, configured_url: str
) -> None:
    monkeypatch.setenv("DASHSCOPE_BASE_URL", configured_url)

    extractor = MetadataExtractor(api_key="test-key")

    assert extractor.base_url == "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"


def test_metadata_extractor_prefers_chat_specific_endpoint_and_key(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("DASHSCOPE_API_KEY", "general-key")
    monkeypatch.setenv("DASHSCOPE_BASE_URL", "https://dashscope.aliyuncs.com")
    monkeypatch.setenv("DASHSCOPE_CHAT_API_KEY", "chat-key")
    monkeypatch.setenv(
        "DASHSCOPE_CHAT_BASE_URL",
        "https://dashscope-intl.aliyuncs.com/api/v1",
    )

    extractor = MetadataExtractor()

    assert extractor.api_key == "chat-key"
    assert extractor.base_url == "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
