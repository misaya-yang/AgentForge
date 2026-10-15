"""Tests for ``ai_gateway_core.proxy.route_pattern.extract_route_pattern``.

The route-pattern extractor is the keying function for per-endpoint
circuit breakers. Wrong canonicalisation = wrong breaker scope =
either the breaker dictionary explodes (one key per UUID) or two
distinct routes share state. Both regressions silently break the
microservice boundary contract, so these tests are tight.
"""
from __future__ import annotations

import pytest

from ai_gateway_core.proxy.route_pattern import extract_route_pattern


# ----- UUID collapse -------------------------------------------------


def test_uuid_collapses_in_known_template() -> None:
    """A KB retrieve path with a real UUID should hit the known
    template and return the canonical ``{id}`` form."""
    out = extract_route_pattern(
        "/api/v1/knowledge/datasets/abc12345-1234-1234-1234-1234567890ab/retrieve"
    )
    assert out == "/api/v1/knowledge/datasets/{id}/retrieve"


def test_uuid_collapses_in_unknown_path_via_fallback() -> None:
    """An unmatched path with a UUID still gets the UUID collapsed
    via the fallback regex — keeps the breaker dict bounded even for
    paths we forgot to register."""
    out = extract_route_pattern(
        "/api/v1/random/abc12345-1234-1234-1234-1234567890ab/foo"
    )
    assert out == "/api/v1/random/{uuid}/foo"


def test_multiple_uuids_in_one_path_all_collapse() -> None:
    """Fallback should collapse every UUID it finds, not just the first."""
    path = (
        "/api/v1/x/abc12345-1234-1234-1234-1234567890ab"
        "/y/deadbeef-cafe-babe-face-01234567890a/z"
    )
    out = extract_route_pattern(path)
    assert out == "/api/v1/x/{uuid}/y/{uuid}/z"


def test_uppercase_uuid_still_collapses() -> None:
    """UUIDs from some clients are upper-cased. Our regex is
    case-insensitive in the fallback path."""
    out = extract_route_pattern("/api/v1/x/ABC12345-1234-1234-1234-1234567890AB/y")
    assert out == "/api/v1/x/{uuid}/y"


# ----- Integer id collapse -----------------------------------------


def test_integer_id_collapses() -> None:
    out = extract_route_pattern("/users/12345/profile")
    assert out == "/users/{id}/profile"


def test_multiple_integer_ids_collapse() -> None:
    out = extract_route_pattern("/api/v1/x/123/y/4567/z")
    assert out == "/api/v1/x/{id}/y/{id}/z"


def test_integer_at_end_collapses() -> None:
    """Trailing integer id with no following slash."""
    out = extract_route_pattern("/items/42")
    assert out == "/items/{id}"


def test_word_with_digits_not_collapsed() -> None:
    """``/v1`` is a path segment, not an integer id. It must NOT be
    collapsed to ``{id}`` — that would break API-prefix matching."""
    # Pure word-with-digit segments survive the fallback (no leading
    # slash + bare digits-only match).
    out = extract_route_pattern("/some/v1abc/path")
    assert out == "/some/v1abc/path"


# ----- Query string + fragment stripping ----------------------------


def test_query_string_stripped() -> None:
    out = extract_route_pattern("/health?foo=bar&baz=qux")
    assert out == "/health"


def test_query_string_stripped_for_uuid_path() -> None:
    out = extract_route_pattern(
        "/api/v1/knowledge/datasets/abc12345-1234-1234-1234-1234567890ab"
        "/retrieve?top_k=5"
    )
    assert out == "/api/v1/knowledge/datasets/{id}/retrieve"


def test_fragment_stripped() -> None:
    out = extract_route_pattern("/health#anchor")
    assert out == "/health"


# ----- Known templates ----------------------------------------------


@pytest.mark.parametrize(
    ("path", "expected"),
    [
        ("/health", "/health"),
        ("/health/live", "/health/live"),
        ("/health/ready", "/health/ready"),
        ("/chat", "/chat"),
        ("/chat/stream", "/chat/stream"),
        ("/sessions", "/sessions"),
        ("/api/v1/assistant/chat", "/chat"),
        ("/api/v1/assistant/chat/stream", "/chat/stream"),
        ("/api/v1/assistant/sessions", "/sessions"),
        ("/api/v1/assistant/health", "/health"),
        ("/api/v1/knowledge/datasets", "/api/v1/knowledge/datasets"),
    ],
)
def test_known_templates_match_exactly(path: str, expected: str) -> None:
    assert extract_route_pattern(path) == expected


def test_session_id_template_match() -> None:
    """``/sessions/{uuid}`` collapses to ``/sessions/{id}`` via the
    known template, not the fallback ``{uuid}``."""
    out = extract_route_pattern(
        "/api/v1/assistant/sessions/abc12345-1234-1234-1234-1234567890ab"
    )
    assert out == "/sessions/{id}"


def test_session_messages_template_match() -> None:
    out = extract_route_pattern(
        "/api/v1/assistant/sessions/"
        "abc12345-1234-1234-1234-1234567890ab/messages"
    )
    assert out == "/sessions/{id}/messages"


def test_dataset_documents_template_match() -> None:
    out = extract_route_pattern(
        "/api/v1/knowledge/datasets/"
        "abc12345-1234-1234-1234-1234567890ab/documents"
    )
    assert out == "/api/v1/knowledge/datasets/{id}/documents"


# ----- Unmatched path returns unchanged (after fallback) -----------


def test_unmatched_no_ids_returns_unchanged() -> None:
    """A path with no UUIDs and no integer segments and no known
    template should round-trip identically."""
    out = extract_route_pattern("/some/totally/random/path")
    assert out == "/some/totally/random/path"


def test_root_returns_unchanged() -> None:
    assert extract_route_pattern("/") == "/"


def test_empty_string_returns_empty() -> None:
    assert extract_route_pattern("") == ""


# ----- Stability: same input → same output --------------------------


def test_idempotent_on_template_output() -> None:
    """Feeding the canonical template back in must be a no-op (otherwise
    the breaker key flips between calls)."""
    template = "/api/v1/knowledge/datasets/{id}/retrieve"
    # Note: ``{id}`` will not match any pattern, so the fallback
    # collapses nothing — output equals input.
    assert extract_route_pattern(template) == template
