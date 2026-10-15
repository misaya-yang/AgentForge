"""Tests for ``ai_gateway_core.security.redaction`` — shared trace/log redaction.

This module replaces two copies of ``_TRACE_REDACTION_PATTERNS`` that used to live
independently in ``trace_writer.py`` and ``agent_loop.py``. The copies started
identical but drifted: only one recognized DB connection strings and JSON-quoted
secret values, and only the other recognized ``sk-``/``AIza`` provider key
prefixes. These tests assert the merged module covers every pattern from both
original copies, so that drift cannot silently reoccur.
"""

from __future__ import annotations

import pytest
from ai_gateway_core.security import redact_trace_text


class _BoomError(Exception):
    pass


@pytest.mark.parametrize(
    ("text", "must_disappear", "must_appear"),
    [
        (
            "Authorization: Bearer super-secret-value",
            ["super-secret-value"],
            ["Authorization: Bearer [redacted]"],
        ),
        (
            "curl -H 'Bearer raw-token-123'",
            ["raw-token-123"],
            ["Bearer [redacted]"],
        ),
        (
            "api_key=abc123XYZ and password: hunter2",
            ["abc123XYZ", "hunter2"],
            ["api_key=[redacted]", "password=[redacted]"],
        ),
        (
            '{"api_key": "sk-abc123XYZsecret"}',
            ["sk-abc123XYZsecret"],
            ["[redacted]"],
        ),
        (
            "postgres://user:pw@host:5432/db",
            ["user:pw@host"],
            ["postgres://[redacted]"],
        ),
        (
            "Incorrect API key provided: sk-fb4d4***********************f34c",
            ["sk-fb4d4", "f34c"],
            ["sk-[redacted]"],
        ),
        (
            "google key AIzaSyA123456789012345678901234567890",
            ["AIzaSyA123456789012345678901234567890"],
            ["AIza[redacted]"],
        ),
        (
            "https://provider.test/generate?key=AQ.vertex-secret-123&alt=sse",
            ["AQ.vertex-secret-123"],
            ["key=[redacted]", "alt=sse"],
        ),
        (
            "vertex credential AQ.vertex-secret-456",
            ["AQ.vertex-secret-456"],
            ["AQ.[redacted]"],
        ),
    ],
)
def test_redact_trace_text_covers_union_of_both_original_pattern_sets(
    text: str, must_disappear: list[str], must_appear: list[str]
) -> None:
    redacted = redact_trace_text(text)
    for secret in must_disappear:
        assert secret not in redacted
    for marker in must_appear:
        assert marker in redacted


def test_redact_trace_text_keeps_exception_type_name() -> None:
    redacted = redact_trace_text(_BoomError("db unreachable"))
    assert "_BoomError" in redacted
    assert "db unreachable" in redacted


def test_redact_trace_text_truncates_only_when_limit_given() -> None:
    long_text = "x" * 1000
    assert redact_trace_text(long_text) == long_text
    truncated = redact_trace_text(long_text, limit=10)
    assert truncated == "xxxxxxxxxx...[truncated]"


def test_redact_trace_text_handles_none_and_empty() -> None:
    assert redact_trace_text(None) == ""
    assert redact_trace_text("") == ""
