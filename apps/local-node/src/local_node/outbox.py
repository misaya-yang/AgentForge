"""Durable, ordered device receipt outbox.

The outbox normally stores only action state and digests. Bounded read-only file
results may be retained until their contiguous transport acknowledgement, then
are erased. Screenshots, credentials, and authorization material never enter
this database.
"""

from __future__ import annotations

import json
import os
import sqlite3
import threading
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Mapping

from .errors import LocalNodeError
from .models import ActionStatus, canonical_json, digest_payload


class OutboxError(LocalNodeError):
    code = "outbox_error"


class OutboxResultTooLarge(OutboxError):
    code = "local_result_too_large"


@dataclass(frozen=True, slots=True)
class ReceiptEvent:
    sequence: int
    event_id: str
    action_id: str | None
    event_type: str
    status: str | None
    occurred_at: float
    result_digest: str | None
    error_code: str | None
    summary: str | None = None
    result: Mapping[str, Any] | None = None

    def as_dict(self, *, include_result: bool = False) -> dict[str, Any]:
        value = asdict(self)
        if not include_result:
            value.pop("result")
        return value


def _safe_identifier(value: str, *, field: str, max_length: int = 512) -> str:
    if not value or len(value) > max_length or any(ord(character) < 0x20 for character in value):
        raise OutboxError(f"{field} is invalid")
    return value


class ReceiptOutbox:
    """SQLite-backed append/ack queue with a contiguous acknowledgement seal."""

    def __init__(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.path = path
        self._lock = threading.RLock()
        self._db = sqlite3.connect(path, check_same_thread=False, isolation_level=None)
        os.chmod(path, 0o600)
        self._db.execute("PRAGMA journal_mode=WAL")
        self._db.execute("PRAGMA synchronous=FULL")
        self._db.execute("PRAGMA secure_delete=ON")
        self._db.executescript(
            """
            CREATE TABLE IF NOT EXISTS receipt_events (
              sequence INTEGER PRIMARY KEY AUTOINCREMENT,
              event_id TEXT UNIQUE NOT NULL,
              action_id TEXT,
              event_type TEXT NOT NULL,
              status TEXT,
              occurred_at REAL NOT NULL,
              result_digest TEXT,
              error_code TEXT,
              summary TEXT,
              result_json TEXT
            );
            CREATE TABLE IF NOT EXISTS receipt_meta (
              id INTEGER PRIMARY KEY CHECK(id = 1),
              acked_through INTEGER NOT NULL
            );
            INSERT OR IGNORE INTO receipt_meta(id, acked_through) VALUES(1, 0);
            """
        )
        columns = {
            str(row[1]) for row in self._db.execute("PRAGMA table_info(receipt_events)")
        }
        if "result_json" not in columns:
            self._db.execute("ALTER TABLE receipt_events ADD COLUMN result_json TEXT")

    @staticmethod
    def _event(row: tuple[Any, ...]) -> ReceiptEvent:
        return ReceiptEvent(
            sequence=row[0],
            event_id=row[1],
            action_id=row[2],
            event_type=row[3],
            status=row[4],
            occurred_at=row[5],
            result_digest=row[6],
            error_code=row[7],
            summary=row[8],
            result=None if row[9] is None else json.loads(str(row[9])),
        )

    def append(
        self,
        *,
        event_id: str,
        event_type: str,
        action_id: str | None = None,
        status: ActionStatus | str | None = None,
        result: Mapping[str, Any] | None = None,
        result_digest: str | None = None,
        retain_result: bool = False,
        error_code: str | None = None,
        summary: str | None = None,
        occurred_at: float | None = None,
    ) -> ReceiptEvent:
        """Append a receipt, retaining only an explicitly allowed file result.

        Supplying ``result`` computes a canonical SHA-256 digest. The raw value
        is persisted only when ``retain_result`` is explicitly true and is
        erased after the server acknowledges the receipt.
        """

        event_id = _safe_identifier(event_id, field="event id")
        event_type = _safe_identifier(event_type, field="event type", max_length=80)
        if action_id is not None:
            action_id = _safe_identifier(action_id, field="action id")
        status_value = None if status is None else str(status)
        if status_value is not None and status_value not in {item.value for item in ActionStatus}:
            raise OutboxError("receipt status is invalid")
        if result is not None and result_digest is not None:
            raise OutboxError("receipt cannot include both result and result digest")
        stored_result: dict[str, Any] | None = None
        if result is not None:
            # Ensure the result is finite canonical JSON before hashing it.
            encoded = canonical_json(dict(result)).encode("utf-8")
            if len(encoded) > 2 * 1024 * 1024:
                raise OutboxResultTooLarge("receipt result exceeds digest budget")
            result_digest = digest_payload(dict(result))
            if retain_result:
                stored_result = dict(result)
        elif retain_result:
            raise OutboxError("retained receipt result is missing")
        if result_digest is not None and (
            len(result_digest) != 64
            or any(character not in "0123456789abcdef" for character in result_digest)
        ):
            raise OutboxError("receipt result digest is invalid")
        if error_code is not None:
            error_code = _safe_identifier(error_code, field="error code", max_length=80)
        if summary is not None:
            if len(summary) > 500 or any(
                ord(character) < 0x20 and character not in "\t" for character in summary
            ):
                raise OutboxError("receipt summary is invalid")
        timestamp = time.time() if occurred_at is None else occurred_at
        with self._lock:
            self._db.execute("BEGIN IMMEDIATE")
            try:
                existing = self._db.execute(
                    "SELECT sequence,event_id,action_id,event_type,status,occurred_at,"
                    "result_digest,error_code,summary,result_json FROM receipt_events "
                    "WHERE event_id=?",
                    (event_id,),
                ).fetchone()
                if existing is not None:
                    event = self._event(existing)
                    expected = ReceiptEvent(
                        event.sequence,
                        event_id,
                        action_id,
                        event_type,
                        status_value,
                        timestamp,
                        result_digest,
                        error_code,
                        summary,
                        stored_result,
                    )
                    # Idempotent retry is allowed even though the caller's
                    # wall-clock value is not byte-for-byte stable.
                    if asdict(event) | {"occurred_at": timestamp} != asdict(expected):
                        raise OutboxError("receipt event id was reused with different data")
                    self._db.execute("COMMIT")
                    return event
                cursor = self._db.execute(
                    "INSERT INTO receipt_events(event_id,action_id,event_type,status,occurred_at,"
                    "result_digest,error_code,summary,result_json) VALUES(?,?,?,?,?,?,?,?,?)",
                    (
                        event_id,
                        action_id,
                        event_type,
                        status_value,
                        timestamp,
                        result_digest,
                        error_code,
                        summary,
                        None if stored_result is None else canonical_json(stored_result),
                    ),
                )
                if cursor.lastrowid is None:
                    raise OutboxError("receipt sequence allocation failed")
                sequence = int(cursor.lastrowid)
                self._db.execute("COMMIT")
            except Exception:
                self._db.execute("ROLLBACK")
                raise
        return ReceiptEvent(
            sequence,
            event_id,
            action_id,
            event_type,
            status_value,
            timestamp,
            result_digest,
            error_code,
            summary,
            stored_result,
        )

    @property
    def acked_through(self) -> int:
        with self._lock:
            row = self._db.execute("SELECT acked_through FROM receipt_meta WHERE id=1").fetchone()
        if row is None:
            raise OutboxError("receipt acknowledgement seal is missing")
        return int(row[0])

    def pending(self, *, limit: int = 200) -> tuple[ReceiptEvent, ...]:
        if limit <= 0 or limit > 200:
            raise OutboxError("receipt batch limit is invalid")
        with self._lock:
            rows = self._db.execute(
                "SELECT sequence,event_id,action_id,event_type,status,occurred_at,"
                "result_digest,error_code,summary,result_json FROM receipt_events "
                "WHERE sequence>? ORDER BY sequence LIMIT ?",
                (self.acked_through, limit),
            ).fetchall()
        return tuple(self._event(row) for row in rows)

    def acknowledge(self, accepted_through: int) -> None:
        with self._lock:
            current = self.acked_through
            maximum_row = self._db.execute(
                "SELECT COALESCE(MAX(sequence), 0) FROM receipt_events"
            ).fetchone()
            maximum = int(maximum_row[0])
            if accepted_through < current or accepted_through > maximum:
                raise OutboxError("receipt acknowledgement is outside the local sequence range")
            # Acknowledging a sequence when an earlier event is absent would
            # silently skip evidence. AUTOINCREMENT normally prevents holes;
            # this explicit check also detects local tampering.
            count_row = self._db.execute(
                "SELECT COUNT(*) FROM receipt_events WHERE sequence>? AND sequence<=?",
                (current, accepted_through),
            ).fetchone()
            if int(count_row[0]) != accepted_through - current:
                raise OutboxError("receipt acknowledgement would cross an event gap")
            self._db.execute(
                "UPDATE receipt_meta SET acked_through=? WHERE id=1",
                (accepted_through,),
            )
            self._db.execute(
                "UPDATE receipt_events SET result_json=NULL WHERE sequence<=?",
                (accepted_through,),
            )
            self._db.execute("PRAGMA wal_checkpoint(TRUNCATE)")

    def export_pending_json(self, *, limit: int = 200) -> str:
        """Useful for adapter tests; never contains tool output or credentials."""
        return json.dumps(
            [event.as_dict() for event in self.pending(limit=limit)],
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
        )

    def close(self) -> None:
        self._db.close()
