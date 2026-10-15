"""Shared secret-redaction primitives for assistant/gateway trace and log output.

Previously `trace_writer.py` and `agent_loop.py` each carried their own copy of
`_TRACE_REDACTION_PATTERNS` / `_redact_trace_text`. The two copies started identical
but drifted: one recognized DB connection strings and JSON-quoted secret values,
the other recognized `sk-`/`AIza` provider key prefixes, and neither recognized the
other's patterns. This module is the single shared implementation both call sites
now use, so redaction coverage no longer depends on which file happens to run.
"""

from __future__ import annotations

import re
from typing import Any

REDACTION_PATTERNS: tuple[tuple[re.Pattern[str], str], ...] = (
    (
        re.compile(r"(?i)([?&](?:key|api_key)=)[^&#\s\"']+"),
        r"\1[redacted]",
    ),
    (
        re.compile(r"(?i)\bauthorization\s*[:=]\s*bearer\s+[A-Za-z0-9._~+/=-]+"),
        "Authorization: Bearer [redacted]",
    ),
    (
        re.compile(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]+"),
        "Bearer [redacted]",
    ),
    (
        re.compile(
            r"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password)"
            r"\s*[:=]\s*[\"']?[^\"'\s,;}]+"
        ),
        r"\1=[redacted]",
    ),
    (
        re.compile(
            r"(?i)([\"']?(?:api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password)"
            r"[\"']?\s*:\s*)[\"'][^\"']+[\"']"
        ),
        r"\1\"[redacted]\"",
    ),
    (
        re.compile(r"(?i)\b(postgres|postgresql|mysql|redis)://[^\s\"']+"),
        r"\1://[redacted]",
    ),
    (
        re.compile(r"\bsk-[A-Za-z0-9_*.\-]{6,}"),
        "sk-[redacted]",
    ),
    (
        re.compile(r"\bAIza[A-Za-z0-9_\-]{20,}"),
        "AIza[redacted]",
    ),
    (
        re.compile(r"\bAQ\.[A-Za-z0-9._\-]{6,}"),
        "AQ.[redacted]",
    ),
)

SENSITIVE_KEY_RE = re.compile(
    r"(?i)(authorization|api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password)"
)


def redact_trace_text(value: Any, *, limit: int | None = None) -> str:
    """Redact secret-looking substrings from a trace/log value.

    Applies the full union of patterns previously split across the two
    diverged copies. `BaseException` inputs keep their type name (callers rely
    on this for error-classification in trace payloads). When `limit` is
    given, the redacted text is truncated with a `...[truncated]` suffix.
    """
    if isinstance(value, BaseException):
        detail = str(value).strip() or repr(value)
        text = f"{type(value).__name__}: {detail}" if detail else type(value).__name__
    else:
        text = str(value or "")
    for pattern, replacement in REDACTION_PATTERNS:
        text = pattern.sub(replacement, text)
    if limit is not None and len(text) > limit:
        return f"{text[:limit]}...[truncated]"
    return text
