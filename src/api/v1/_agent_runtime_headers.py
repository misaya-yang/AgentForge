"""Validation for client-supplied Agent runtime fields."""

from __future__ import annotations

from typing import Final

from fastapi import HTTPException, Request

_ALLOWED_AGENT_HEADERS: Final = frozenset({"x-agent-embed-token", "x-agent-embed-origin"})
_RESERVED_AGENT_FIELDS: Final = frozenset(
    {
        "agent_id",
        "agent_version_id",
        "draft_revision",
        "publication_id",
        "channel",
        "resolved_snapshot",
        "runtime_envelope",
        "snapshot_hash",
        "spec_hash",
        "runtime_fingerprint",
    }
)


def reject_client_agent_forgery(
    request: Request, body: dict[str, object] | None = None
) -> None:
    if any(
        (header := name.lower()).startswith("x-agent-")
        and header not in _ALLOWED_AGENT_HEADERS
        for name in request.headers
    ):
        raise HTTPException(
            status_code=400,
            detail={
                "code": "AGENT_RUNTIME_FIELD_FORBIDDEN",
                "message": "Client-supplied Agent runtime headers are forbidden",
            },
        )
    forbidden = sorted(_RESERVED_AGENT_FIELDS.intersection(body or {}))
    if forbidden:
        raise HTTPException(
            status_code=422,
            detail={
                "code": "AGENT_RUNTIME_FIELD_FORBIDDEN",
                "message": "Client-supplied Agent runtime fields are forbidden",
                "fields": forbidden,
            },
        )


__all__ = ["reject_client_agent_forgery"]
