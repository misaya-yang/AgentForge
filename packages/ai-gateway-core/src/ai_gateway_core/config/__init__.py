"""Shared configuration helpers.

Pure environment-read utilities: no I/O, no side effects, safe to call
during module import. See ``endpoints.py`` for the per-domain
free/paid endpoint-switch resolver.
"""

from .endpoints import (
    DASHSCOPE_DEFAULT_CHAT_BASE_URL,
    DASHSCOPE_DEFAULT_NATIVE_BASE_URL,
    GOOGLE_AI_STUDIO_BASE_URL,
    GOOGLE_VERTEX_BASE_URL,
    normalize_dashscope_base,
    resolve_dashscope,
    resolve_google,
)

__all__ = [
    "DASHSCOPE_DEFAULT_CHAT_BASE_URL",
    "DASHSCOPE_DEFAULT_NATIVE_BASE_URL",
    "GOOGLE_AI_STUDIO_BASE_URL",
    "GOOGLE_VERTEX_BASE_URL",
    "normalize_dashscope_base",
    "resolve_dashscope",
    "resolve_google",
]
