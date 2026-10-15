"""Stable identities and pagination helpers for KB query observations."""

from __future__ import annotations

import base64
import hashlib
import json
import unicodedata
import uuid
from dataclasses import dataclass
from datetime import datetime


@dataclass(frozen=True, slots=True)
class QueryObservation:
    trace_id: str
    query_fingerprint: str
    normalized_query: str


class QueryObservationConflictError(RuntimeError):
    """A trace exists but the caller supplied a different query identity."""


def normalize_query_text(query: str) -> str:
    """Canonicalize analytics identity without changing the executed query."""

    normalized = unicodedata.normalize("NFKC", str(query or ""))
    return " ".join(normalized.split()).casefold()


def query_fingerprint(query: str) -> str:
    return hashlib.sha256(normalize_query_text(query).encode("utf-8")).hexdigest()


def new_query_observation(query: str) -> QueryObservation:
    normalized = normalize_query_text(query)
    return QueryObservation(
        trace_id=str(uuid.uuid4()),
        query_fingerprint=hashlib.sha256(normalized.encode("utf-8")).hexdigest(),
        normalized_query=normalized,
    )


def encode_query_cursor(created_at: datetime, row_id: str) -> str:
    payload = json.dumps(
        {"created_at": created_at.isoformat(), "id": str(row_id)},
        separators=(",", ":"),
    ).encode("utf-8")
    return base64.urlsafe_b64encode(payload).decode("ascii").rstrip("=")


def decode_query_cursor(cursor: str) -> tuple[datetime, str]:
    try:
        raw = str(cursor or "").strip()
        padded = raw + "=" * (-len(raw) % 4)
        payload = json.loads(base64.urlsafe_b64decode(padded).decode("utf-8"))
        created_at = datetime.fromisoformat(str(payload["created_at"]))
        row_id = str(payload["id"]).strip()
        if created_at.tzinfo is None or not row_id:
            raise ValueError
        return created_at, row_id
    except (KeyError, TypeError, ValueError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError("invalid query pagination cursor") from exc


__all__ = [
    "QueryObservation",
    "QueryObservationConflictError",
    "decode_query_cursor",
    "encode_query_cursor",
    "new_query_observation",
    "normalize_query_text",
    "query_fingerprint",
]
