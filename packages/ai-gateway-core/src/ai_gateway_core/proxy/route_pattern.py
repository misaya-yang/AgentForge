"""Route pattern extraction for per-endpoint circuit breaker keying.

The proxy needs a *canonical* key per upstream route so that a flaky
``/chat/stream`` does not poison the breaker for ``/health`` on the same
service. Concrete request paths embed user-controlled IDs (UUIDs,
integers) — keying directly on the path explodes the breaker dictionary
and never gets enough samples per key to trip.

``extract_route_pattern`` maps a concrete path to its template:

    /api/v1/knowledge/datasets/abc-123-def-456-789-012345678901/retrieve
        →  /api/v1/knowledge/datasets/{uuid}/retrieve

Strategy:
    1. Strip the query string (breaker scope is route, not args).
    2. Try each pre-compiled known route regex in order. First match wins
       and returns the template.
    3. Fallback: collapse UUIDs to ``{uuid}`` and bare integer IDs to
       ``{id}``. This still gives a stable key for routes we forgot to
       register up-front.
"""
from __future__ import annotations

import re
from typing import Final

# RFC 4122 8-4-4-4-12 UUID. Matched anywhere in a path segment.
_UUID_RE: Final = re.compile(
    r"/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
    r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}(?=/|$)"
)
# Bare integer path segments (``/123``). Anchored to a leading slash so
# we don't collapse digits embedded in real word segments.
_INT_RE: Final = re.compile(r"/\d+(?=/|$)")


# Known upstream route patterns. Order matters: the first regex that
# matches wins, so put the more specific templates ahead of the
# generic ones (``/sessions/{id}/messages`` before ``/sessions/{id}``).
#
# The regex is written to match the FULL path with optional API prefix
# (``/api/v1/assistant`` or ``/api/v1/knowledge``) — both bare and
# prefixed forms map to the same canonical template, so a downstream
# breaker keys on the upstream-relative shape.
_KNOWN_PATTERNS: Final[list[tuple[re.Pattern[str], str]]] = [
    # -- Health probes ------------------------------------------------
    (re.compile(r"^(?:/api/v\d+/[^/]+)?/health/live$"), "/health/live"),
    (re.compile(r"^(?:/api/v\d+/[^/]+)?/health/ready$"), "/health/ready"),
    (re.compile(r"^(?:/api/v\d+/[^/]+)?/health$"), "/health"),
    # -- Assistant: chat ---------------------------------------------
    (re.compile(r"^(?:/api/v\d+/assistant)?/chat/stream$"), "/chat/stream"),
    (re.compile(r"^(?:/api/v\d+/assistant)?/chat$"), "/chat"),
    # -- Assistant: sessions -----------------------------------------
    (
        re.compile(
            r"^(?:/api/v\d+/assistant)?/sessions/"
            r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
            r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}/messages$"
        ),
        "/sessions/{id}/messages",
    ),
    (
        re.compile(
            r"^(?:/api/v\d+/assistant)?/sessions/"
            r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
            r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}/history$"
        ),
        "/sessions/{id}/history",
    ),
    (
        re.compile(
            r"^(?:/api/v\d+/assistant)?/sessions/"
            r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
            r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
        ),
        "/sessions/{id}",
    ),
    (re.compile(r"^(?:/api/v\d+/assistant)?/sessions$"), "/sessions"),
    # -- Knowledge: datasets -----------------------------------------
    (
        re.compile(
            r"^/api/v\d+/knowledge/datasets/"
            r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
            r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}/retrieve$"
        ),
        "/api/v1/knowledge/datasets/{id}/retrieve",
    ),
    (
        re.compile(
            r"^/api/v\d+/knowledge/datasets/"
            r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
            r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}/documents$"
        ),
        "/api/v1/knowledge/datasets/{id}/documents",
    ),
    (
        re.compile(
            r"^/api/v\d+/knowledge/datasets/"
            r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-"
            r"[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"
        ),
        "/api/v1/knowledge/datasets/{id}",
    ),
    (re.compile(r"^/api/v\d+/knowledge/datasets$"), "/api/v1/knowledge/datasets"),
]


# Memoization cache. Bounded by the number of distinct concrete paths
# the proxy ever sees (UUID/int collapse keeps that count low — a few
# hundred at most over a process lifetime). No eviction needed.
_PATTERN_CACHE: dict[str, str] = {}


def extract_route_pattern(path: str) -> str:
    """Map a concrete request path to its route template.

    >>> extract_route_pattern("/health")
    '/health'
    >>> extract_route_pattern(
    ...     "/api/v1/knowledge/datasets/"
    ...     "abc12345-1234-1234-1234-1234567890ab/retrieve?top_k=5"
    ... )
    '/api/v1/knowledge/datasets/{id}/retrieve'
    >>> extract_route_pattern("/some/unknown/123/path")
    '/some/unknown/{id}/path'
    """
    if not path:
        return path

    cached = _PATTERN_CACHE.get(path)
    if cached is not None:
        return cached
    raw = path

    # Strip query string — breaker scope is the route, not the args.
    qs_idx = path.find("?")
    if qs_idx != -1:
        path = path[:qs_idx]

    # Fragment too, just in case.
    frag_idx = path.find("#")
    if frag_idx != -1:
        path = path[:frag_idx]

    # First pass: known templates win and return their canonical form.
    for regex, template in _KNOWN_PATTERNS:
        if regex.match(path):
            _PATTERN_CACHE[raw] = template
            return template

    # Fallback: collapse UUIDs first (they may contain digits), then
    # bare integer segments. ``re.sub`` handles multiple occurrences.
    collapsed = _UUID_RE.sub("/{uuid}", path)
    collapsed = _INT_RE.sub("/{id}", collapsed)
    _PATTERN_CACHE[raw] = collapsed
    return collapsed


__all__ = ["extract_route_pattern"]
