"""Durable idempotency index and tamper-evident, append-only action events."""

from __future__ import annotations

import hashlib
import json
import sqlite3
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .errors import IdempotencyConflict, LedgerIntegrityError
from .models import (
    ActionContext,
    ActionStatus,
    PlatformSignatureVerifier,
    TERMINAL_STATUSES,
    TrustedLocalApprovalVerifier,
    canonical_json,
    digest_payload,
)


_REDACTED = "[REDACTED]"
_SENSITIVE_KEY_PARTS = (
    "secret",
    "token",
    "password",
    "passwd",
    "credential",
    "authorization",
    "cookie",
    "private_key",
    "error_detail",
)
_SENSITIVE_VALUE_MARKERS = (
    "-----begin private key-----",
    "-----begin rsa private key-----",
    "bearer ",
    "sk-",
    "ghp_",
    "secret_canary",
    "secret-canary",
)

_ALLOWED_TRANSITIONS: dict[ActionStatus, frozenset[ActionStatus]] = {
    ActionStatus.PROPOSED: frozenset({ActionStatus.POLICY_CHECK}),
    ActionStatus.POLICY_CHECK: frozenset(
        {
            ActionStatus.AWAITING_APPROVAL,
            ActionStatus.DISPATCHED,
            ActionStatus.FAILED,
            ActionStatus.CANCELLED,
            ActionStatus.INTERRUPTED,
        }
    ),
    ActionStatus.AWAITING_APPROVAL: frozenset(
        {
            ActionStatus.DISPATCHED,
            ActionStatus.FAILED,
            ActionStatus.CANCELLED,
            ActionStatus.INTERRUPTED,
        }
    ),
    ActionStatus.DISPATCHED: frozenset(
        {
            ActionStatus.RUNNING,
            ActionStatus.OBSERVED,
            ActionStatus.SUCCEEDED,
            ActionStatus.FAILED,
            ActionStatus.CANCELLED,
            ActionStatus.INTERRUPTED,
            ActionStatus.UNKNOWN,
        }
    ),
    ActionStatus.RUNNING: frozenset(
        {
            ActionStatus.OBSERVED,
            ActionStatus.SUCCEEDED,
            ActionStatus.FAILED,
            ActionStatus.CANCELLED,
            ActionStatus.INTERRUPTED,
            ActionStatus.UNKNOWN,
        }
    ),
    ActionStatus.OBSERVED: frozenset(
        {
            ActionStatus.SUCCEEDED,
            ActionStatus.FAILED,
            ActionStatus.CANCELLED,
            ActionStatus.INTERRUPTED,
            ActionStatus.UNKNOWN,
        }
    ),
}


def sanitize_payload(value: Any, *, key: str = "") -> Any:
    """Remove likely secret material before it reaches SQLite or the WAL."""
    lowered_key = key.casefold()
    if any(part in lowered_key for part in _SENSITIVE_KEY_PARTS) or lowered_key in {
        "stdout",
        "stderr",
        "content",
        "raw_content",
    }:
        return _REDACTED
    if isinstance(value, dict):
        return {
            str(item_key): sanitize_payload(item, key=str(item_key))
            for item_key, item in value.items()
        }
    if isinstance(value, (list, tuple)):
        return [sanitize_payload(item) for item in value]
    if isinstance(value, str):
        lowered = value.casefold()
        if any(marker in lowered for marker in _SENSITIVE_VALUE_MARKERS):
            return _REDACTED
    return value


@dataclass(frozen=True, slots=True)
class ActionRecord:
    action_id: str
    idempotency_key: str
    arguments_digest: str
    envelope_digest: str
    status: ActionStatus
    result: dict[str, Any] | None


@dataclass(frozen=True, slots=True)
class BeginResult:
    created: bool
    record: ActionRecord


class ActionLedger:
    def __init__(
        self,
        path: Path,
        *,
        platform_signature_verifier: PlatformSignatureVerifier | None = None,
        trusted_local_approval_verifier: TrustedLocalApprovalVerifier | None = None,
    ) -> None:
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        self.path = path
        self.platform_signature_verifier = platform_signature_verifier
        self.trusted_local_approval_verifier = trusted_local_approval_verifier
        self._lock = threading.RLock()
        self._db = sqlite3.connect(path, check_same_thread=False, isolation_level=None)
        os_chmod_private(path)
        self._db.execute("PRAGMA journal_mode=WAL")
        self._db.execute("PRAGMA synchronous=FULL")
        self._db.executescript(
            """
            CREATE TABLE IF NOT EXISTS actions (
              action_id TEXT PRIMARY KEY,
              idempotency_key TEXT UNIQUE NOT NULL,
              arguments_digest TEXT NOT NULL,
              envelope_digest TEXT NOT NULL,
              status TEXT NOT NULL,
              result_json TEXT,
              created_at REAL NOT NULL,
              updated_at REAL NOT NULL
            );
            CREATE TABLE IF NOT EXISTS events (
              seq INTEGER PRIMARY KEY AUTOINCREMENT,
              action_id TEXT NOT NULL,
              event_type TEXT NOT NULL,
              payload_json TEXT NOT NULL,
              created_at REAL NOT NULL,
              prev_hash TEXT NOT NULL,
              event_hash TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS ledger_meta (
              id INTEGER PRIMARY KEY CHECK(id = 1),
              event_count INTEGER NOT NULL,
              head_hash TEXT NOT NULL
            );
            """
        )
        action_columns = {
            row[1] for row in self._db.execute("PRAGMA table_info(actions)").fetchall()
        }
        if "envelope_digest" not in action_columns:
            # The pre-signature schema cannot reconstruct a canonical envelope
            # from its arguments digest.  Preserve receipts for audit, but mark
            # them with an impossible sentinel so no new signed claim can replay
            # an old result as if it had the same authority scope.
            self._db.execute(
                "ALTER TABLE actions ADD COLUMN envelope_digest TEXT NOT NULL "
                "DEFAULT 'legacy-unbound'"
            )
        if self._db.execute("SELECT 1 FROM ledger_meta WHERE id=1").fetchone() is None:
            event_count = self._db.execute("SELECT COUNT(*) FROM events").fetchone()[0]
            head = self._db.execute(
                "SELECT event_hash FROM events ORDER BY seq DESC LIMIT 1"
            ).fetchone()
            self._db.execute(
                "INSERT INTO ledger_meta(id,event_count,head_hash) VALUES(1,?,?)",
                (event_count, "0" * 64 if head is None else head[0]),
            )

    def _append(self, action_id: str, event_type: str, payload: dict[str, Any]) -> None:
        previous = self._db.execute(
            "SELECT event_hash FROM events ORDER BY seq DESC LIMIT 1"
        ).fetchone()
        prev_hash = "0" * 64 if previous is None else previous[0]
        created_at = time.time()
        payload_json = canonical_json(sanitize_payload(payload))
        material = canonical_json(
            {
                "action_id": action_id,
                "event_type": event_type,
                "payload": payload_json,
                "created_at": created_at,
                "prev_hash": prev_hash,
            }
        )
        event_hash = hashlib.sha256(material.encode()).hexdigest()
        self._db.execute(
            "INSERT INTO events(action_id,event_type,payload_json,created_at,prev_hash,event_hash) "
            "VALUES(?,?,?,?,?,?)",
            (action_id, event_type, payload_json, created_at, prev_hash, event_hash),
        )
        self._db.execute(
            "UPDATE ledger_meta SET event_count=event_count+1,head_hash=? WHERE id=1",
            (event_hash,),
        )

    @staticmethod
    def _record(row: tuple[Any, ...]) -> ActionRecord:
        result = None if row[5] is None else json.loads(row[5])
        return ActionRecord(row[0], row[1], row[2], row[3], ActionStatus(row[4]), result)

    def begin(self, action: ActionContext) -> BeginResult:
        # The durable reservation is itself part of the execution boundary.
        # Transport-provided verifier objects are never accepted from the
        # envelope; only this independently injected local dependency is used.
        action.verify_platform_signature(self.platform_signature_verifier)
        envelope_digest = hashlib.sha256(action.canonical_signed_payload()).hexdigest()
        with self._lock:
            self._db.execute("BEGIN IMMEDIATE")
            try:
                by_idempotency = self._db.execute(
                    "SELECT action_id,idempotency_key,arguments_digest,envelope_digest,"
                    "status,result_json "
                    "FROM actions WHERE idempotency_key=?",
                    (action.idempotency_key,),
                ).fetchone()
                by_action = self._db.execute(
                    "SELECT action_id,idempotency_key,arguments_digest,envelope_digest,"
                    "status,result_json "
                    "FROM actions WHERE action_id=?",
                    (action.action_id,),
                ).fetchone()
                if (
                    by_idempotency is not None
                    and by_action is not None
                    and by_idempotency != by_action
                ):
                    raise IdempotencyConflict(
                        "action id and idempotency key refer to different actions"
                    )
                existing = by_idempotency or by_action
                if existing is not None:
                    record = self._record(existing)
                    if (
                        record.action_id != action.action_id
                        or record.idempotency_key != action.idempotency_key
                        or record.arguments_digest != action.arguments_digest
                        or record.envelope_digest != envelope_digest
                    ):
                        raise IdempotencyConflict(
                            "idempotency key or action id reused with new intent"
                        )
                    self._db.execute("COMMIT")
                    return BeginResult(False, record)
                now = time.time()
                self._db.execute(
                    "INSERT INTO actions VALUES(?,?,?,?,?,?,?,?)",
                    (
                        action.action_id,
                        action.idempotency_key,
                        action.arguments_digest,
                        envelope_digest,
                        ActionStatus.POLICY_CHECK.value,
                        None,
                        now,
                        now,
                    ),
                )
                self._append(
                    action.action_id,
                    ActionStatus.POLICY_CHECK.value,
                    {
                        "tenant_id": action.tenant_id,
                        "user_id": action.user_id,
                        "device_id": action.device_id,
                        "session_id": action.session_id,
                        "run_id": action.run_id,
                        "agent_id": action.agent_id,
                        "agent_version": action.agent_version,
                        "call_id": action.call_id,
                        "envelope_version": action.envelope_version,
                        "capability": action.capability,
                        "tool_name": action.tool_name,
                        "operation": action.operation,
                        "capability_lease_id": action.capability_lease_id,
                        "resource_refs_digest": digest_payload(list(action.resource_refs)),
                        "arguments_digest": action.arguments_digest,
                        "target_snapshot_digest": action.target_snapshot_digest,
                        "policy_snapshot_digest": action.policy_snapshot_digest,
                        "envelope_digest": envelope_digest,
                        "nonce": action.nonce,
                        "platform_key_id": action.platform_key_id,
                        "approval_id": None
                        if action.approval is None
                        else action.approval.approval_id,
                    },
                )
                self._db.execute("COMMIT")
                return BeginResult(
                    True,
                    ActionRecord(
                        action.action_id,
                        action.idempotency_key,
                        action.arguments_digest,
                        envelope_digest,
                        ActionStatus.POLICY_CHECK,
                        None,
                    ),
                )
            except Exception:
                self._db.execute("ROLLBACK")
                raise

    def mark_awaiting_approval(self, action_id: str) -> None:
        self._transition(action_id, ActionStatus.AWAITING_APPROVAL, None)

    def mark_dispatched(self, action_id: str) -> None:
        self._transition(action_id, ActionStatus.DISPATCHED, None)

    def mark_running(self, action_id: str) -> None:
        self._transition(action_id, ActionStatus.RUNNING, None)

    def mark_observed(self, action_id: str) -> None:
        self._transition(action_id, ActionStatus.OBSERVED, None)

    def finish(self, action_id: str, status: ActionStatus, result: dict[str, Any]) -> ActionRecord:
        if status not in TERMINAL_STATUSES:
            raise ValueError("finish requires a terminal status")
        return self._transition(action_id, status, result)

    def _transition(
        self, action_id: str, status: ActionStatus, result: dict[str, Any] | None
    ) -> ActionRecord:
        with self._lock:
            self._db.execute("BEGIN IMMEDIATE")
            try:
                row = self._db.execute(
                    "SELECT action_id,idempotency_key,arguments_digest,envelope_digest,"
                    "status,result_json "
                    "FROM actions WHERE action_id=?",
                    (action_id,),
                ).fetchone()
                if row is None:
                    raise KeyError(action_id)
                before = self._record(row)
                if before.status in TERMINAL_STATUSES:
                    if before.status == status and before.result == result:
                        self._db.execute("COMMIT")
                        return before
                    raise IdempotencyConflict("action already has a different terminal result")
                allowed = _ALLOWED_TRANSITIONS.get(before.status, frozenset())
                if status not in allowed:
                    raise IdempotencyConflict(
                        f"invalid action transition {before.status.value} -> {status.value}"
                    )
                safe_result = None if result is None else sanitize_payload(result)
                result_json = None if safe_result is None else canonical_json(safe_result)
                self._db.execute(
                    "UPDATE actions SET status=?,result_json=?,updated_at=? WHERE action_id=?",
                    (status.value, result_json, time.time(), action_id),
                )
                self._append(action_id, status.value, {} if safe_result is None else safe_result)
                self._db.execute("COMMIT")
                return ActionRecord(
                    before.action_id,
                    before.idempotency_key,
                    before.arguments_digest,
                    before.envelope_digest,
                    status,
                    safe_result,
                )
            except Exception:
                self._db.execute("ROLLBACK")
                raise

    def get(self, action_id: str) -> ActionRecord | None:
        with self._lock:
            row = self._db.execute(
                "SELECT action_id,idempotency_key,arguments_digest,envelope_digest,"
                "status,result_json "
                "FROM actions WHERE action_id=?",
                (action_id,),
            ).fetchone()
        return None if row is None else self._record(row)

    def get_by_idempotency_key(self, idempotency_key: str) -> ActionRecord | None:
        with self._lock:
            row = self._db.execute(
                "SELECT action_id,idempotency_key,arguments_digest,envelope_digest,"
                "status,result_json "
                "FROM actions WHERE idempotency_key=?",
                (idempotency_key,),
            ).fetchone()
        return None if row is None else self._record(row)

    def entries(self) -> tuple[dict[str, Any], ...]:
        """Return redaction-safe event metadata for diagnostics and tests."""
        with self._lock:
            rows = self._db.execute(
                "SELECT seq,action_id,event_type,created_at,prev_hash,event_hash "
                "FROM events ORDER BY seq"
            ).fetchall()
        return tuple(
            {
                "seq": row[0],
                "action_id": row[1],
                "event_type": row[2],
                "created_at": row[3],
                "prev_hash": row[4],
                "event_hash": row[5],
            }
            for row in rows
        )

    def interrupt_running(self, *, exclude: frozenset[str] = frozenset()) -> tuple[str, ...]:
        with self._lock:
            rows = self._db.execute(
                "SELECT action_id,status FROM actions WHERE status IN (?,?,?,?,?)",
                (
                    ActionStatus.POLICY_CHECK.value,
                    ActionStatus.AWAITING_APPROVAL.value,
                    ActionStatus.DISPATCHED.value,
                    ActionStatus.RUNNING.value,
                    ActionStatus.OBSERVED.value,
                ),
            ).fetchall()
        interrupted: list[str] = []
        for action_id, prior_status in rows:
            if action_id in exclude:
                continue
            terminal = (
                ActionStatus.INTERRUPTED
                if prior_status
                in {
                    ActionStatus.POLICY_CHECK.value,
                    ActionStatus.AWAITING_APPROVAL.value,
                }
                else ActionStatus.UNKNOWN
            )
            self.finish(
                action_id,
                terminal,
                {
                    "error_code": "transport_disconnected",
                    "replay_allowed": False,
                    "side_effect_unknown": terminal is ActionStatus.UNKNOWN,
                },
            )
            interrupted.append(action_id)
        return tuple(interrupted)

    def verify(self) -> bool:
        with self._lock:
            rows = self._db.execute(
                "SELECT action_id,event_type,payload_json,created_at,prev_hash,event_hash "
                "FROM events ORDER BY seq"
            ).fetchall()
            meta = self._db.execute(
                "SELECT event_count,head_hash FROM ledger_meta WHERE id=1"
            ).fetchone()
            actions = self._db.execute(
                "SELECT action_id,envelope_digest,status,result_json FROM actions"
            ).fetchall()
        if meta is None or meta[0] != len(rows):
            raise LedgerIntegrityError("action ledger tail/count seal is invalid")
        previous = "0" * 64
        for action_id, event_type, payload_json, created_at, prev_hash, event_hash in rows:
            material = canonical_json(
                {
                    "action_id": action_id,
                    "event_type": event_type,
                    "payload": payload_json,
                    "created_at": created_at,
                    "prev_hash": prev_hash,
                }
            )
            expected = hashlib.sha256(material.encode()).hexdigest()
            if prev_hash != previous or not hmac_compare(expected, event_hash):
                raise LedgerIntegrityError("action ledger integrity check failed")
            previous = event_hash
        if meta[1] != previous:
            raise LedgerIntegrityError("action ledger head seal is invalid")
        event_by_action: dict[str, list[tuple[str, str]]] = {}
        for action_id, event_type, payload_json, *_ in rows:
            event_by_action.setdefault(action_id, []).append((event_type, payload_json))
        if set(event_by_action) != {row[0] for row in actions}:
            raise LedgerIntegrityError("action index and event chain disagree")
        for action_id, envelope_digest, status, result_json in actions:
            if envelope_digest == "legacy-unbound":
                # Legacy receipts remain visible for manual reconciliation but
                # are deliberately impossible to replay through begin().
                continue
            if len(envelope_digest) != 64:
                raise LedgerIntegrityError("action envelope digest is invalid")
            last_type, last_payload = event_by_action[action_id][-1]
            expected_event = status
            if last_type != expected_event:
                raise LedgerIntegrityError("action state does not match its last ledger event")
            if status in {item.value for item in TERMINAL_STATUSES}:
                if result_json is None or canonical_json(json.loads(result_json)) != last_payload:
                    raise LedgerIntegrityError(
                        "action terminal result does not match its ledger event"
                    )
        return True

    def verify_integrity(self) -> bool:
        return self.verify()

    def close(self) -> None:
        self._db.close()


def hmac_compare(left: str, right: str) -> bool:
    # Constant-time comparison without making hmac part of the stored format.
    import hmac

    return hmac.compare_digest(left, right)


def os_chmod_private(path: Path) -> None:
    import os

    os.chmod(path, 0o600)
