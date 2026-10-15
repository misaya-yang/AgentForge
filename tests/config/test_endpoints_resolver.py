"""Tests for ``ai_gateway_core.config.endpoints`` resolver.

Covers the per-domain env-var pattern + domain-specific-first fallback
chain that lets operators route chat/image/embedding independently
between free/paid DashScope and Google endpoints.
"""

from __future__ import annotations

import pytest
from ai_gateway_core.config import endpoints

# ---------------------------------------------------------------------------
# Hygiene fixture: clear every env var the resolver reads so test order
# never matters and os.environ from the dev box doesn't leak in.

_ENV_VARS = [
    "DASHSCOPE_API_KEY",
    "DASHSCOPE_BASE_URL",
    "DASHSCOPE_CHAT_API_KEY",
    "DASHSCOPE_CHAT_BASE_URL",
    "DASHSCOPE_IMAGE_API_KEY",
    "DASHSCOPE_IMAGE_BASE_URL",
    "DASHSCOPE_EMBEDDING_API_KEY",
    "DASHSCOPE_EMBEDDING_BASE_URL",
    "DASHSCOPE_OCR_API_KEY",
    "DASHSCOPE_OCR_BASE_URL",
    "GOOGLE_API_BACKEND",
    "GOOGLE_CHAT_BACKEND",
    "GOOGLE_IMAGE_BACKEND",
    "GOOGLE_EMBEDDING_BACKEND",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "VERTEX_API_KEY",
    "VERTEX_CHAT_API_KEY",
    "VERTEX_IMAGE_API_KEY",
    "VERTEX_EMBEDDING_API_KEY",
]


@pytest.fixture(autouse=True)
def _clear_env(monkeypatch):
    for name in _ENV_VARS:
        monkeypatch.delenv(name, raising=False)


# ---------------------------------------------------------------------------
# DashScope resolver


def test_dashscope_chat_defaults_to_cn_paid_when_no_env(monkeypatch):
    """With nothing set, chat returns the CN compatible-mode URL and no key."""
    api_key, base_url = endpoints.resolve_dashscope("chat")
    assert api_key == ""
    assert base_url == endpoints.DASHSCOPE_DEFAULT_CHAT_BASE_URL


def test_dashscope_general_key_flows_through_to_all_domains(monkeypatch):
    """Single DASHSCOPE_API_KEY satisfies chat + image + embedding."""
    monkeypatch.setenv("DASHSCOPE_API_KEY", "sk-cn-paid")
    assert endpoints.resolve_dashscope("chat")[0] == "sk-cn-paid"
    assert endpoints.resolve_dashscope("image")[0] == "sk-cn-paid"
    assert endpoints.resolve_dashscope("embedding")[0] == "sk-cn-paid"


def test_dashscope_ocr_has_an_independent_native_endpoint(monkeypatch):
    monkeypatch.setenv("DASHSCOPE_API_KEY", "sk-general")
    monkeypatch.setenv("DASHSCOPE_OCR_API_KEY", "sk-ocr")
    monkeypatch.setenv(
        "DASHSCOPE_OCR_BASE_URL",
        "https://workspace.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
    )

    ocr_key, ocr_url = endpoints.resolve_dashscope("ocr")

    assert ocr_key == "sk-ocr"
    assert ocr_url == "https://workspace.ap-southeast-1.maas.aliyuncs.com/api/v1"


def test_dashscope_chat_override_does_not_leak_to_image(monkeypatch):
    """DASHSCOPE_CHAT_API_KEY wins for chat only — image + embedding
    stay on DASHSCOPE_API_KEY."""
    monkeypatch.setenv("DASHSCOPE_API_KEY", "sk-cn-paid")
    monkeypatch.setenv("DASHSCOPE_CHAT_API_KEY", "sk-intl-free")
    monkeypatch.setenv(
        "DASHSCOPE_CHAT_BASE_URL",
        "https://dashscope-intl.aliyuncs.com/compatible-mode",
    )

    chat_key, chat_url = endpoints.resolve_dashscope("chat")
    image_key, image_url = endpoints.resolve_dashscope("image")
    embed_key, embed_url = endpoints.resolve_dashscope("embedding")

    assert chat_key == "sk-intl-free"
    assert "dashscope-intl" in chat_url
    assert image_key == "sk-cn-paid"
    assert "dashscope-intl" not in image_url
    assert embed_key == "sk-cn-paid"
    assert "dashscope-intl" not in embed_url


def test_dashscope_image_override_is_independent_of_chat(monkeypatch):
    monkeypatch.setenv("DASHSCOPE_API_KEY", "sk-cn-paid")
    monkeypatch.setenv("DASHSCOPE_IMAGE_API_KEY", "sk-image-free")
    # Bare host URL: resolver normalises it to ``/api/v1`` for image
    # because the dashscope SDK expects that suffix.
    monkeypatch.setenv(
        "DASHSCOPE_IMAGE_BASE_URL", "https://dashscope-intl.aliyuncs.com",
    )

    image_key, image_url = endpoints.resolve_dashscope("image")
    chat_key, _ = endpoints.resolve_dashscope("chat")

    assert image_key == "sk-image-free"
    assert image_url == "https://dashscope-intl.aliyuncs.com/api/v1"
    assert chat_key == "sk-cn-paid"


def test_dashscope_chat_base_url_normalised_with_compat_suffix(monkeypatch):
    """Bare host on chat domain → ``/compatible-mode`` suffix appended."""
    monkeypatch.setenv("DASHSCOPE_API_KEY", "sk-x")
    monkeypatch.setenv(
        "DASHSCOPE_CHAT_BASE_URL", "https://dashscope-intl.aliyuncs.com",
    )
    _, chat_url = endpoints.resolve_dashscope("chat")
    assert chat_url == "https://dashscope-intl.aliyuncs.com/compatible-mode"


def test_dashscope_url_swapped_between_domains(monkeypatch):
    """A single ``DASHSCOPE_BASE_URL`` env value works for all three
    domains — resolver swaps the suffix per domain."""
    monkeypatch.setenv("DASHSCOPE_API_KEY", "sk-x")
    monkeypatch.setenv(
        "DASHSCOPE_BASE_URL", "https://dashscope-intl.aliyuncs.com/compatible-mode",
    )
    _, chat_url = endpoints.resolve_dashscope("chat")
    _, image_url = endpoints.resolve_dashscope("image")
    _, embed_url = endpoints.resolve_dashscope("embedding")
    assert chat_url == "https://dashscope-intl.aliyuncs.com/compatible-mode"
    assert image_url == "https://dashscope-intl.aliyuncs.com/api/v1"
    assert embed_url == "https://dashscope-intl.aliyuncs.com/api/v1"


@pytest.mark.parametrize(
    "configured_url",
    [
        "https://dashscope-intl.aliyuncs.com",
        "https://dashscope-intl.aliyuncs.com/api/v1",
        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions",
    ],
)
def test_dashscope_general_url_normalises_common_endpoint_shapes(monkeypatch, configured_url):
    monkeypatch.setenv("DASHSCOPE_BASE_URL", configured_url)

    _, chat_url = endpoints.resolve_dashscope("chat")
    _, embed_url = endpoints.resolve_dashscope("embedding")

    assert chat_url == "https://dashscope-intl.aliyuncs.com/compatible-mode"
    assert embed_url == "https://dashscope-intl.aliyuncs.com/api/v1"


def test_dashscope_embedding_override_is_independent(monkeypatch):
    monkeypatch.setenv("DASHSCOPE_API_KEY", "sk-cn-paid")
    monkeypatch.setenv("DASHSCOPE_EMBEDDING_API_KEY", "sk-embed-special")

    embed_key, _ = endpoints.resolve_dashscope("embedding")
    chat_key, _ = endpoints.resolve_dashscope("chat")

    assert embed_key == "sk-embed-special"
    assert chat_key == "sk-cn-paid"


def test_dashscope_image_and_embedding_default_to_native_host(monkeypatch):
    """Image + embedding default to ``…/api/v1`` (the dashscope SDK's
    own base path); chat defaults to ``…/compatible-mode`` (OpenAI-HTTP)."""
    _, image_url = endpoints.resolve_dashscope("image")
    _, embed_url = endpoints.resolve_dashscope("embedding")
    _, chat_url = endpoints.resolve_dashscope("chat")
    assert image_url == endpoints.DASHSCOPE_DEFAULT_NATIVE_BASE_URL
    assert embed_url == endpoints.DASHSCOPE_DEFAULT_NATIVE_BASE_URL
    assert chat_url == endpoints.DASHSCOPE_DEFAULT_CHAT_BASE_URL


# ---------------------------------------------------------------------------
# Google resolver


def test_google_defaults_to_ai_studio_with_no_env(monkeypatch):
    api_key, base_url, backend = endpoints.resolve_google("chat")
    assert backend == "ai_studio"
    assert base_url == endpoints.GOOGLE_AI_STUDIO_BASE_URL
    assert api_key == ""


def test_google_ai_studio_reads_gemini_key_first(monkeypatch):
    monkeypatch.setenv("GEMINI_API_KEY", "AIzaSy-new")
    monkeypatch.setenv("GOOGLE_API_KEY", "AIzaSy-legacy")
    api_key, base_url, backend = endpoints.resolve_google("chat")
    assert backend == "ai_studio"
    assert api_key == "AIzaSy-new"
    assert base_url == endpoints.GOOGLE_AI_STUDIO_BASE_URL


def test_google_ai_studio_falls_back_to_google_api_key(monkeypatch):
    monkeypatch.setenv("GOOGLE_API_KEY", "AIzaSy-legacy")
    api_key, _, _ = endpoints.resolve_google("chat")
    assert api_key == "AIzaSy-legacy"


def test_google_backend_global_flip_affects_all_domains(monkeypatch):
    monkeypatch.setenv("GOOGLE_API_BACKEND", "vertex")
    monkeypatch.setenv("VERTEX_API_KEY", "AQ.global")
    for domain in ("chat", "image", "embedding"):
        api_key, base_url, backend = endpoints.resolve_google(domain)
        assert backend == "vertex", domain
        assert base_url == endpoints.GOOGLE_VERTEX_BASE_URL, domain
        assert api_key == "AQ.global", domain


def test_google_chat_backend_override_does_not_leak_to_image(monkeypatch):
    """Chat on Vertex, image + embedding on AI Studio."""
    monkeypatch.setenv("GEMINI_API_KEY", "AIzaSy-studio")
    monkeypatch.setenv("GOOGLE_CHAT_BACKEND", "vertex")
    monkeypatch.setenv("VERTEX_CHAT_API_KEY", "AQ.chat-only")

    chat_key, chat_url, chat_backend = endpoints.resolve_google("chat")
    img_key, img_url, img_backend = endpoints.resolve_google("image")
    embed_key, embed_url, embed_backend = endpoints.resolve_google("embedding")

    assert chat_backend == "vertex"
    assert chat_key == "AQ.chat-only"
    assert chat_url == endpoints.GOOGLE_VERTEX_BASE_URL

    assert img_backend == "ai_studio"
    assert img_key == "AIzaSy-studio"
    assert img_url == endpoints.GOOGLE_AI_STUDIO_BASE_URL

    assert embed_backend == "ai_studio"
    assert embed_key == "AIzaSy-studio"
    assert embed_url == endpoints.GOOGLE_AI_STUDIO_BASE_URL


def test_google_vertex_shared_key_fallback(monkeypatch):
    """If backend=vertex but no VERTEX key set, fall through to
    GEMINI_API_KEY (the 'shared key' escape hatch — rare, but
    matches the model_registry behaviour for the legacy chat switch)."""
    monkeypatch.setenv("GEMINI_API_KEY", "AIzaSy-shared")
    monkeypatch.setenv("GOOGLE_IMAGE_BACKEND", "vertex")
    api_key, _, backend = endpoints.resolve_google("image")
    assert backend == "vertex"
    assert api_key == "AIzaSy-shared"


def test_google_unknown_backend_falls_back_to_ai_studio(monkeypatch):
    """Any unknown backend name is normalized to ai_studio, not an error.
    Matches the existing provider-init leniency in src/main.py."""
    monkeypatch.setenv("GOOGLE_EMBEDDING_BACKEND", "OpenAI")  # nonsense
    _, base_url, backend = endpoints.resolve_google("embedding")
    assert backend == "ai_studio"
    assert base_url == endpoints.GOOGLE_AI_STUDIO_BASE_URL


def test_google_domain_specific_vertex_key_wins_over_general(monkeypatch):
    monkeypatch.setenv("GOOGLE_EMBEDDING_BACKEND", "vertex")
    monkeypatch.setenv("VERTEX_API_KEY", "AQ.general")
    monkeypatch.setenv("VERTEX_EMBEDDING_API_KEY", "AQ.embedding-specific")
    api_key, _, backend = endpoints.resolve_google("embedding")
    assert backend == "vertex"
    assert api_key == "AQ.embedding-specific"
