"""Per-domain endpoint-switch resolver for DashScope and Google families.

Both providers expose two interchangeable endpoints per domain
(chat / image / embedding / OCR) — a paid/full one and a free-tier alternative.
Operators want to be able to route each domain independently, e.g. keep
embeddings on the paid CN endpoint but flip chat to the Intl free tier.

Env-var pattern (per provider family + per domain):

    # DashScope
    DASHSCOPE_{DOMAIN}_API_KEY            # e.g. DASHSCOPE_CHAT_API_KEY
    DASHSCOPE_{DOMAIN}_BASE_URL           # e.g. DASHSCOPE_CHAT_BASE_URL

    # Google (ai_studio ↔ vertex)
    GOOGLE_{DOMAIN}_BACKEND               # "ai_studio" | "vertex"
    VERTEX_{DOMAIN}_API_KEY               # Express-Mode key (AQ.xxx)

Each domain's override falls back through a chain (first non-empty wins):

    DashScope api_key:  DASHSCOPE_{DOMAIN}_API_KEY
                      → DASHSCOPE_API_KEY                     (general)

    DashScope base_url: DASHSCOPE_{DOMAIN}_BASE_URL
                      → DASHSCOPE_BASE_URL                    (general)
                      → {paid CN default for that domain}

    Google backend:     GOOGLE_{DOMAIN}_BACKEND
                      → GOOGLE_API_BACKEND                    (general)
                      → "ai_studio"

    Google api_key:     when backend == vertex:
                          VERTEX_{DOMAIN}_API_KEY
                        → VERTEX_API_KEY
                        → GEMINI_API_KEY / GOOGLE_API_KEY     (shared fallback)
                        when backend == ai_studio:
                          GEMINI_API_KEY
                        → GOOGLE_API_KEY

Callers should prefer this helper over raw ``os.environ.get`` so the
    fallback behaviour stays consistent across chat/image/embedding/OCR. The
module is intentionally pure — no I/O, no side effects, no logging.
"""

from __future__ import annotations

import os
from typing import Literal

Domain = Literal["chat", "image", "embedding", "ocr"]

# DashScope speaks two different URL conventions:
#
#   * ``chat`` uses the OpenAI-compatible HTTP path. The OpenAI-compat
#     client appends ``/v1/chat/completions`` to whatever base it gets,
#     so the base must end with ``/compatible-mode``.
#
#   * ``image`` and ``embedding`` use the dashscope native SDK, which
#     appends ``/services/...`` to ``dashscope.base_http_api_url``. The
#     SDK's default base value is ``https://dashscope.aliyuncs.com/api/v1``
#     — note the ``/api/v1`` segment is part of the BASE, not appended
#     by the SDK. So the embedding/image base must end with ``/api/v1``.
#
# Operators usually set a single ``DASHSCOPE_BASE_URL`` env (the chat-
# style ``…/compatible-mode``). The resolver normalises it per domain
# below so a single env value keeps all three domains working.
DASHSCOPE_DEFAULT_CHAT_BASE_URL = "https://dashscope.aliyuncs.com/compatible-mode"
DASHSCOPE_DEFAULT_NATIVE_BASE_URL = "https://dashscope.aliyuncs.com/api/v1"

_COMPAT_SUFFIX = "/compatible-mode"
_COMPAT_V1_SUFFIX = f"{_COMPAT_SUFFIX}/v1"
_CHAT_COMPLETIONS_SUFFIX = f"{_COMPAT_V1_SUFFIX}/chat/completions"
_NATIVE_SUFFIX = "/api/v1"

GOOGLE_AI_STUDIO_BASE_URL = "https://generativelanguage.googleapis.com"
GOOGLE_VERTEX_BASE_URL = "https://aiplatform.googleapis.com"


def _first(*candidates: str) -> str:
    """Return the first non-empty candidate after strip()."""
    for c in candidates:
        if c and c.strip():
            return c.strip()
    return ""


def normalize_dashscope_base(url: str, domain: Domain) -> str:
    """Make a DashScope base URL match the convention the consumer wants.

    chat domain → ``…/compatible-mode``  (OpenAI-compat client)
    image/embed → ``…/api/v1``           (dashscope native SDK)

    Compatible-mode bases with or without ``/v1`` (including the full
    chat-completions endpoint) and native SDK ``/api/v1`` bases are accepted
    and converted. A bare host gets the right suffix appended. This is what
    makes a single operator-set ``DASHSCOPE_BASE_URL`` work everywhere —
    incident 2026-04-28 traced an "Arrearage / 404" cascade to the embedding
    path silently keeping ``/compatible-mode`` and the SDK 404'ing.
    """
    cleaned = url.rstrip("/")
    # Strip whichever suffix is present so we can re-add the right one.
    for suffix in (
        _CHAT_COMPLETIONS_SUFFIX,
        _COMPAT_V1_SUFFIX,
        _COMPAT_SUFFIX,
        _NATIVE_SUFFIX,
    ):
        if cleaned.endswith(suffix):
            cleaned = cleaned[: -len(suffix)]
            break
    if domain == "chat":
        return cleaned + _COMPAT_SUFFIX
    return cleaned + _NATIVE_SUFFIX


def resolve_dashscope(domain: Domain) -> tuple[str, str]:
    """Resolve DashScope ``(api_key, base_url)`` for the given domain.

    Falls back through the per-domain env, then the general env, then
    the domain-appropriate default base URL. The returned base URL is
    always normalised to the suffix the domain's client expects:

    * chat  → ``…/compatible-mode``
    * image → ``…/api/v1``
    * embed/ocr → ``…/api/v1``

    Returns ``("", default_url)`` if no key is configured — callers
    decide whether to treat that as "not configured".
    """
    up = domain.upper()
    api_key = _first(
        os.environ.get(f"DASHSCOPE_{up}_API_KEY", ""),
        os.environ.get("DASHSCOPE_API_KEY", ""),
    )
    raw_url = _first(
        os.environ.get(f"DASHSCOPE_{up}_BASE_URL", ""),
        os.environ.get("DASHSCOPE_BASE_URL", ""),
    )
    if raw_url:
        base_url = normalize_dashscope_base(raw_url, domain)
    else:
        base_url = (
            DASHSCOPE_DEFAULT_CHAT_BASE_URL
            if domain == "chat"
            else DASHSCOPE_DEFAULT_NATIVE_BASE_URL
        )
    return api_key, base_url


def resolve_google(domain: Domain) -> tuple[str, str, str]:
    """Resolve Google ``(api_key, base_url, backend)`` for the given domain.

    ``backend`` is always ``"ai_studio"`` or ``"vertex"``. The base URL
    is the default for the selected backend — callers that need a
    non-default host (none exist today) can still override via their
    own logic; this helper only handles the free/paid swap.

    When ``backend == "vertex"`` and no VERTEX key is configured, the
    generic ``GEMINI_API_KEY`` / ``GOOGLE_API_KEY`` is returned as the
    shared-key fallback (the "I know what I'm doing" escape hatch —
    see ``tests/services/assistant/test_google_vertex_switch.py``).
    """
    up = domain.upper()
    backend = _first(
        os.environ.get(f"GOOGLE_{up}_BACKEND", ""),
        os.environ.get("GOOGLE_API_BACKEND", ""),
    ).lower() or "ai_studio"
    if backend not in {"ai_studio", "vertex"}:
        backend = "ai_studio"

    studio_key = _first(
        os.environ.get("GEMINI_API_KEY", ""),
        os.environ.get("GOOGLE_API_KEY", ""),
    )
    if backend == "vertex":
        api_key = _first(
            os.environ.get(f"VERTEX_{up}_API_KEY", ""),
            os.environ.get("VERTEX_API_KEY", ""),
            studio_key,  # shared-key escape hatch
        )
        base_url = GOOGLE_VERTEX_BASE_URL
    else:
        api_key = studio_key
        base_url = GOOGLE_AI_STUDIO_BASE_URL
    return api_key, base_url, backend
