"""Common utilities shared across knowledge service modules.

Consolidates duplicated functions that previously existed in multiple files:
- _ensure_dict: document_service, knowledge_service, ingestion_service, dataset_service
- SensitiveDataFilter: embedding, text_reranker
- _permission_rank: knowledge_service, dataset_service
- import_pymupdf: 10+ files with identical try/except pattern
- normalize_arabic / Arabic diacritics: retrieval, chunking
- detect_language / detect_query_language: retrieval, chunking
"""

from __future__ import annotations

import inspect
import json
import logging
import re
from typing import Any

SPECIALIZED_REBUILD_UNAVAILABLE = "specialized_rebuild_unavailable"


async def maybe_await(value: Any) -> Any:
    """Await production coroutines while preserving synchronous test adapters."""

    if inspect.isawaitable(value):
        return await value
    return value


# =============================================================================
# Data Conversion
# =============================================================================

def ensure_dict(value: Any) -> dict[str, Any]:
    """Safely convert a value to a dict. Handles None, dict, and JSON strings."""
    if value is None:
        return {}
    if isinstance(value, dict):
        return value
    if isinstance(value, str):
        try:
            parsed = json.loads(value)
            return parsed if isinstance(parsed, dict) else {}
        except json.JSONDecodeError:
            return {}
    return {}


# =============================================================================
# Permission Utilities
# =============================================================================

_PERMISSION_RANKS = {"viewer": 1, "editor": 2, "owner": 3}


def permission_rank(p: str | None) -> int:
    """Get numeric rank for a permission level."""
    if not p:
        return 0
    return _PERMISSION_RANKS.get(str(p).lower(), 0)


# =============================================================================
# Logging: Sensitive Data Filter
# =============================================================================

class SensitiveDataFilter(logging.Filter):
    """Filter that redacts sensitive information like API keys from log records."""

    _PATTERNS = [
        (re.compile(r"Bearer\s+[a-zA-Z0-9_-]+"), "Bearer ***"),
        (re.compile(r"api_key[=:]\s*[a-zA-Z0-9_-]+"), "api_key=***"),
        (re.compile(r"key[=:]\s*[a-zA-Z0-9_-]+"), "key=***"),
    ]

    def filter(self, record: logging.LogRecord) -> bool:
        """Redact sensitive data from log message."""
        if isinstance(record.msg, str):
            record.msg = self._redact(record.msg)
        if record.args:
            record.args = tuple(self._redact(str(arg)) for arg in record.args)
        return True

    def _redact(self, text: str) -> str:
        for pattern, replacement in self._PATTERNS:
            text = pattern.sub(replacement, text)
        return text


# =============================================================================
# PyMuPDF Import Helper
# =============================================================================

def import_pymupdf():
    """Import PyMuPDF with compatibility for old/new module names.

    Returns the ``fitz`` module regardless of whether the package was
    installed as ``pymupdf`` (modern) or ``fitz`` (legacy).
    """
    try:
        import pymupdf as fitz  # type: ignore
        return fitz
    except ImportError:
        import fitz  # type: ignore
        return fitz


# =============================================================================
# Arabic Text Processing
# =============================================================================

# Arabic Unicode ranges (comprehensive)
RE_ARABIC_CHARS = re.compile(
    r"[\u0600-\u06ff\u0750-\u077f\u08a0-\u08ff\ufb50-\ufdff\ufe70-\ufeff]"
)

# Arabic diacritics (tashkeel)
RE_ARABIC_DIACRITICS = re.compile(r"[\u064b-\u0652\u0670]")

# Alef variations for normalization
_RE_ALEF_VARIATIONS = re.compile(r"[أإآٱ]")


def normalize_arabic(text: str) -> str:
    """Normalize Arabic text for better matching.

    - Remove diacritics (tashkeel)
    - Normalize alef variations (أ إ آ ٱ → ا)
    - Normalize taa marbuta (ة → ه)
    - Normalize alef maksura (ى → ي)
    """
    if not text:
        return ""
    result = RE_ARABIC_DIACRITICS.sub("", text)
    result = _RE_ALEF_VARIATIONS.sub("ا", result)
    result = result.replace("ة", "ه")
    result = result.replace("ى", "ي")
    return result


# =============================================================================
# Language Detection
# =============================================================================

# CJK Unicode range
RE_CJK_CHARS = re.compile(r"[\u4e00-\u9fff\u3400-\u4dbf]")
# Latin word pattern
RE_LATIN_WORD = re.compile(r"[A-Za-z0-9\-_]+")


def detect_language(text: str, *, sample_length: int = 2000) -> str:
    """Detect primary language from text.

    Returns: ``"ar"`` | ``"en"`` | ``"zh"`` | ``"mixed"``
    """
    if not text:
        return "en"

    sample = text[:sample_length]
    total = max(len(sample), 1)

    arabic_count = len(RE_ARABIC_CHARS.findall(sample))
    cjk_count = len(RE_CJK_CHARS.findall(sample))
    latin_count = len(RE_LATIN_WORD.findall(sample))

    arabic_ratio = arabic_count / total
    cjk_ratio = cjk_count / total

    if arabic_ratio > 0.2:
        if latin_count > arabic_count * 0.3:
            return "mixed"
        return "ar"
    elif cjk_ratio > 0.1:
        return "zh"
    return "en"


def detect_query_language(query: str) -> str:
    """Detect primary language of a query (simplified, for weight adjustment).

    Returns: ``"ar"`` for Arabic, ``"en"`` for English/other.
    """
    if not query:
        return "en"

    sample = query[:500]
    arabic_chars = len(RE_ARABIC_CHARS.findall(sample))
    total = max(len(sample), 1)

    if arabic_chars / total > 0.25:
        return "ar"
    return "en"
