"""The single authoritative migration runner.

Contract (PRD ARC-03 §3C):

* one PostgreSQL session advisory lock serializes every writer;
* the runner owns transactions — epoch migration files carry no
  ``BEGIN``/``COMMIT``;
* re-running is idempotent; every change is immutable with a full SHA-256;
* preconditions and postconditions come from the epoch manifest;
* an object that exists with the wrong definition fails closed;
* concurrent runners, out-of-order application, and checksum tampering fail
  closed;
* ``status`` and ``verify`` are absolutely read-only.
"""

from __future__ import annotations

import hashlib
import re
import socket
import time
import uuid
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from . import ledger
from .ledger_acl import owner_scope, inspect as inspect_ledger_acl
from .constants import (
    DEFAULT_ROLE_PREFIX,
    MIGRATION_ADVISORY_LOCK_ID,
    MIGRATION_ADVISORY_LOCK_NAMESPACE,
    ROLE_PREFIX_ENV,
)
from .discovery import (
    LEGACY_FILENAME_ALIASES,
    LegacyMigration,
    discover_legacy_migrations,
    validate_legacy_chain,
)
from .manifest import (
    ChangeSpec,
    EpochManifest,
    TransactionMode,
)

RUNNER_VERSION = "database-authority/1"
# Recovery compatibility is a property of the executable state machine, not a
# manually bumped label. Any source change intentionally invalidates open
# non-transactional attempts until a reviewed repair/resume decision is made.
RUNNER_DIGEST = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()

_TRANSACTION_STATEMENT_RE = re.compile(
    r"^(?P<command>BEGIN\b|START\s+TRANSACTION\b|COMMIT\b|END\b|ROLLBACK\b|"
    r"SAVEPOINT\b|RELEASE(?:\s+SAVEPOINT)?\b|ABORT\b|PREPARE\s+TRANSACTION\b|"
    r"SET\s+TRANSACTION\b|SET\s+SESSION\s+CHARACTERISTICS\s+AS\s+TRANSACTION\b)"
    r"(?:\s+.*)?$",
    re.IGNORECASE,
)
_NON_TRANSACTIONAL_MARKER_RE = re.compile(
    r"^\s*--\s*@checkpoint\s+(?P<name>[a-z0-9_]+)\s*$",
    re.IGNORECASE | re.MULTILINE,
)
_DOLLAR_QUOTE_DELIMITER_RE = re.compile(r"\$(?:[A-Za-z_][A-Za-z0-9_]*)?\$")

ATTEMPT_LEASE_TIMEOUT = timedelta(minutes=15)
_PREAMBLE_CHECKPOINT = "__preamble__"
_RESERVED_CHECKPOINTS = frozenset({_PREAMBLE_CHECKPOINT})

_SELECT_LATEST_ATTEMPT = f"""
SELECT attempt_id, baseline_id, sequence, checksum_sha256, runner_digest,
       phase, checkpoint, lease_owner, fence_generation, state, started_at
FROM public.{ledger.ATTEMPTS_TABLE}
WHERE baseline_id = $1 AND sequence = $2
ORDER BY started_at DESC, attempt_id DESC
LIMIT 1
"""

_CLAIM_ATTEMPT = f"""
UPDATE public.{ledger.ATTEMPTS_TABLE}
SET lease_owner = $2, fence_generation = fence_generation + 1,
    state = '{ledger.ATTEMPT_STATE_RUNNING}', phase = $4,
    started_at = now(), finished_at = NULL, error_code = NULL
WHERE attempt_id = $1 AND fence_generation = $3
  AND state IN ('{ledger.ATTEMPT_STATE_RESUMABLE}', '{ledger.ATTEMPT_STATE_FAILED}')
RETURNING fence_generation
"""

_TRANSITION_EXPIRED_ATTEMPT = f"""
UPDATE public.{ledger.ATTEMPTS_TABLE}
SET state = $4, finished_at = now(), error_code = $5
WHERE attempt_id = $1 AND lease_owner = $2 AND fence_generation = $3
  AND state = '{ledger.ATTEMPT_STATE_RUNNING}'
RETURNING fence_generation
"""

_UPDATE_ATTEMPT_CHECKPOINT_FENCED = f"""
UPDATE public.{ledger.ATTEMPTS_TABLE}
SET checkpoint = $4, fence_generation = fence_generation + 1, started_at = now()
WHERE attempt_id = $1 AND lease_owner = $2 AND fence_generation = $3
  AND state = '{ledger.ATTEMPT_STATE_RUNNING}'
RETURNING fence_generation
"""

_UPDATE_ATTEMPT_TERMINAL_FENCED = f"""
UPDATE public.{ledger.ATTEMPTS_TABLE}
SET state = $4, finished_at = now(), error_code = $5,
    fence_generation = fence_generation + 1
WHERE attempt_id = $1 AND lease_owner = $2 AND fence_generation = $3
  AND state = '{ledger.ATTEMPT_STATE_RUNNING}'
RETURNING fence_generation
"""


def strip_sql_bodies(sql: str) -> str:
    """Reduce SQL to its top-level statements for transaction-control checks.

    Dollar-quoted bodies (DO blocks, function bodies), string literals and
    line comments are blanked out so a PL/pgSQL ``BEGIN`` inside a DO block
    is never mistaken for a transaction-control statement, while a real
    top-level ``BEGIN;``/``COMMIT;`` still is.
    """
    # This must be a lexical pass, not a sequence of regular-expression
    # substitutions. A ``$$`` token inside a comment or quoted string is not
    # a dollar-quote opener and must never mask a later top-level BEGIN/COMMIT.
    masked = list(sql)
    index = 0
    length = len(sql)
    while index < length:
        if sql.startswith("--", index):
            end = sql.find("\n", index + 2)
            end = length if end == -1 else end
            masked[index:end] = " " * (end - index)
            index = end
            continue
        if sql.startswith("/*", index):
            start = index
            depth = 1
            index += 2
            while index < length and depth:
                if sql.startswith("/*", index):
                    depth += 1
                    index += 2
                elif sql.startswith("*/", index):
                    depth -= 1
                    index += 2
                else:
                    index += 1
            if depth:
                raise AuthorityError("SQL contains an unterminated block comment")
            masked[start:index] = " " * (index - start)
            continue
        if sql[index] in {"'", '"'}:
            quote = sql[index]
            start = index
            escape_string = (
                quote == "'"
                and start > 0
                and sql[start - 1] in {"e", "E"}
                and (start < 2 or not (sql[start - 2].isalnum() or sql[start - 2] == "_"))
            )
            index += 1
            while index < length:
                if sql[index] == quote:
                    if index + 1 < length and sql[index + 1] == quote:
                        index += 2
                        continue
                    index += 1
                    break
                # Backslash escapes are lexical only in explicit E'' strings.
                # Plain strings follow modern standard_conforming_strings;
                # treating ``\'`` as an escape there could hide top-level SQL.
                if escape_string and sql[index] == "\\" and index + 1 < length:
                    index += 2
                else:
                    index += 1
            else:
                raise AuthorityError("SQL contains an unterminated quoted string")
            masked[start:index] = " " * (index - start)
            continue
        if sql[index] == "$":
            match = _DOLLAR_QUOTE_DELIMITER_RE.match(sql, index)
            if match is not None:
                delimiter = match.group(0)
                start = index
                end = sql.find(delimiter, match.end())
                if end == -1:
                    raise AuthorityError("SQL contains an unterminated dollar quote")
                index = end + len(delimiter)
                masked[start:index] = " " * (index - start)
                continue
        index += 1
    return "".join(masked)


class AuthorityError(RuntimeError):
    """The authority refused to act; nothing was silently repaired."""


class AuthorityBlockedError(AuthorityError):
    """The authority cannot prove what happened; human reconciliation needed."""


@dataclass(frozen=True)
class AuthorityPaths:
    database_dir: Path

    @property
    def migrations_root(self) -> Path:
        return self.database_dir / "migrations"

    @property
    def bootstrap_dir(self) -> Path:
        return self.database_dir / "bootstrap"

    @property
    def baselines_root(self) -> Path:
        return self.database_dir / "baselines"

    def baseline_dir(self, baseline_id: str) -> Path:
        return self.baselines_root / baseline_id

    def epoch_dir(self, baseline_id: str) -> Path:
        return self.migrations_root / baseline_id


@dataclass(frozen=True)
class RecoveryContext:
    baseline_id: str
    spec: ChangeSpec
    attempt_id: str
    checkpoint: str
    fence_generation: int
    prior_state: str


RecoveryHandler = Callable[[Any, RecoveryContext], Awaitable[None]]


def role_prefix_from_env(environ: dict[str, str] | None = None) -> str:
    import os

    env = environ if environ is not None else dict(os.environ)
    prefix = env.get(ROLE_PREFIX_ENV, DEFAULT_ROLE_PREFIX)
    if not re.fullmatch(r"[a-z][a-z0-9_]{0,20}_", prefix):
        raise AuthorityError(
            f"{ROLE_PREFIX_ENV}={prefix!r} is not a safe role prefix "
            "(lowercase identifier ending in '_')"
        )
    return prefix


class MigrationAuthority:
    """Async façade over one migration run against one PostgreSQL database."""

    def __init__(
        self,
        dsn: str,
        paths: AuthorityPaths,
        *,
        role_prefix: str = DEFAULT_ROLE_PREFIX,
        asyncpg_module: Any = None,
        recovery_handlers: dict[str, RecoveryHandler] | None = None,
    ) -> None:
        if asyncpg_module is None:
            import asyncpg

            asyncpg_module = asyncpg
        if not re.fullmatch(r"[a-z][a-z0-9_]{0,20}_", role_prefix):
            raise AuthorityError(f"unsafe role prefix: {role_prefix!r}")
        self._asyncpg = asyncpg_module
        self.dsn = dsn
        self.paths = paths
        self.role_prefix = role_prefix
        self._recovery_handlers = dict(recovery_handlers or {})

    # ------------------------------------------------------------------
    # connections
    # ------------------------------------------------------------------

    async def connect(self, *, read_only: bool = False) -> Any:
        """Open one connection with the migrator application name."""
        options = {"application_name": f"ai_gateway_authority_{uuid.uuid4().hex[:8]}"}
        if read_only:
            options["default_transaction_read_only"] = "on"
        return await self._asyncpg.connect(self.dsn, server_settings=options)

    async def acquire_lock(self, lock_conn: Any) -> None:
        await lock_conn.execute(
            "SELECT pg_advisory_lock($1::integer, $2::integer)",
            MIGRATION_ADVISORY_LOCK_NAMESPACE,
            MIGRATION_ADVISORY_LOCK_ID,
        )

    async def release_lock(self, lock_conn: Any) -> None:
        try:
            await lock_conn.execute(
                "SELECT pg_advisory_unlock($1::integer, $2::integer)",
                MIGRATION_ADVISORY_LOCK_NAMESPACE,
                MIGRATION_ADVISORY_LOCK_ID,
            )
        finally:
            await lock_conn.close()

    # ------------------------------------------------------------------
    # ledger
    # ------------------------------------------------------------------

    async def ensure_ledger(self, conn: Any) -> None:
        async with owner_scope(conn, self.role_prefix):
            await conn.execute(ledger.LEDGER_DDL)

    @staticmethod
    async def _ledger_table_exists(conn: Any, table: str) -> bool:
        return bool(await conn.fetchval(
            "SELECT EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace "
            "WHERE n.nspname='public' AND c.relname=$1)", table
        ))

    async def adopted_baseline(self, conn: Any) -> dict[str, Any] | None:
        """The adoption marker row, or None.

        A database without the ledger tables has adopted nothing; the ledger
        is never created by a read.
        """
        if not await self._ledger_table_exists(conn, ledger.BASELINES_TABLE):
            return None
        rows = await conn.fetch(ledger.SELECT_BASELINE)
        if not rows:
            return None
        if len(rows) > 1:
            raise AuthorityError(
                f"{len(rows)} baselines adopted in one database; "
                "exactly one baseline can be authoritative"
            )
        return dict(rows[0])

    async def applied_changes(self, conn: Any, baseline_id: str) -> dict[int, str]:
        if not await self._ledger_table_exists(conn, ledger.CHANGES_TABLE):
            return {}
        rows = await conn.fetch(ledger.SELECT_APPLIED_CHANGES, baseline_id)
        return {int(row["sequence"]): str(row["checksum_sha256"]) for row in rows}

    # ------------------------------------------------------------------
    # epoch changes
    # ------------------------------------------------------------------

    @staticmethod
    def _reject_embedded_transactions(sql: str, change_name: str) -> None:
        for statement in strip_sql_bodies(sql).split(";"):
            normalized = " ".join(statement.split())
            match = _TRANSACTION_STATEMENT_RE.fullmatch(normalized)
            if match:
                raise AuthorityError(
                    f"change {change_name} contains transaction control "
                    f"({match.group('command').upper()}); the runner owns transactions"
                )

    @staticmethod
    def _split_checkpoints(sql: str) -> list[tuple[str, str]]:
        """Split SQL into segments named by their durable completion checkpoint."""
        segments: list[tuple[str, str]] = []
        current_name = _PREAMBLE_CHECKPOINT
        last_end = 0
        seen: set[str] = set()
        for match in _NON_TRANSACTIONAL_MARKER_RE.finditer(sql):
            part = sql[last_end : match.start()]
            if part.strip():
                segments.append((current_name, part))
            current_name = match.group("name")
            if current_name in _RESERVED_CHECKPOINTS or current_name in seen:
                raise AuthorityError(
                    f"non-transactional SQL has duplicate/reserved checkpoint {current_name!r}"
                )
            seen.add(current_name)
            last_end = match.end()
        final_part = sql[last_end:]
        if final_part.strip():
            segments.append((current_name, final_part))
        elif seen:
            raise AuthorityError(
                f"non-transactional checkpoint {current_name!r} has no SQL segment"
            )
        if not segments:
            raise AuthorityError("non-transactional change contains no executable SQL")
        return segments

    @staticmethod
    def _attempt_expired(attempt: dict[str, Any], *, now: datetime | None = None) -> bool:
        started_at = attempt.get("started_at")
        if not isinstance(started_at, datetime):
            raise AuthorityError("open migration attempt has no valid started_at timestamp")
        if started_at.tzinfo is None:
            started_at = started_at.replace(tzinfo=timezone.utc)
        current = now or datetime.now(timezone.utc)
        return current - started_at.astimezone(timezone.utc) >= ATTEMPT_LEASE_TIMEOUT

    async def _run_recovery_handler(
        self,
        name: str | None,
        conn: Any,
        context: RecoveryContext,
        *,
        kind: str,
    ) -> None:
        if not name:
            raise AuthorityBlockedError(
                f"change {context.spec.sequence} needs a declared {kind}_handler "
                f"for attempt state {context.prior_state!r}"
            )
        handler = self._recovery_handlers.get(name)
        if handler is None:
            raise AuthorityBlockedError(
                f"change {context.spec.sequence} declares {kind}_handler {name!r}, "
                "but this runner has no registered implementation"
            )
        await handler(conn, context)

    @staticmethod
    def _require_fence(value: Any, *, action: str, attempt_id: str) -> int:
        if value is None:
            raise AuthorityBlockedError(
                f"attempt {attempt_id} lost its lease/fence while trying to {action}"
            )
        return int(value)

    async def _evaluate_conditions(
        self, conn: Any, conditions: tuple[str, ...], *, change_name: str, phase: str
    ) -> None:
        async with owner_scope(conn, self.role_prefix):
            for index, condition in enumerate(conditions):
                value = await conn.fetchval(condition)
                if value is not True:
                    raise AuthorityError(
                        f"{phase}condition {index + 1} of change {change_name} failed: "
                        f"{condition.strip()} returned {value!r}; refusing to continue"
                    )

    async def apply_change_transactional(
        self, conn: Any, baseline_id: str, spec: ChangeSpec, epoch_dir: Path
    ) -> int:
        """Apply one transactional change; DDL and success ledger share a txn."""
        sql = (epoch_dir / spec.file).read_text(encoding="utf-8")
        actual_sha = hashlib.sha256(sql.encode("utf-8")).hexdigest()
        if actual_sha != spec.sha256:
            raise AuthorityError(
                f"change {spec.sequence} ({spec.file}) checksum mismatch: "
                f"manifest declares {spec.sha256}, file is {actual_sha}; "
                "checksums are immutable once declared"
            )
        self._reject_embedded_transactions(sql, spec.name)

        start = time.monotonic()
        async with conn.transaction():
            await conn.execute(f"SET LOCAL statement_timeout = {int(spec.timeout_seconds) * 1000}")
            await conn.execute(f"SET LOCAL lock_timeout = {int(spec.lock_budget_seconds) * 1000}")
            await self._evaluate_conditions(
                conn, spec.preconditions, change_name=spec.name, phase="pre"
            )
            previous_role = await conn.fetchval("SELECT current_setting('role')")
            execution_role = f"{self.role_prefix}{spec.owner}"
            await conn.execute(f'SET LOCAL ROLE "{execution_role}"')
            await conn.execute(sql)
            # Ledger rows are written by the connected migrator identity,
            # never by the temporary object-owner role.
            await conn.execute("SELECT set_config('role', $1, true)", previous_role)
            await self._evaluate_conditions(
                conn, spec.postconditions, change_name=spec.name, phase="post"
            )
            duration_ms = int((time.monotonic() - start) * 1000)
            await conn.execute(
                ledger.INSERT_CHANGE_SUCCESS,
                baseline_id,
                spec.sequence,
                spec.name,
                spec.sha256,
                duration_ms,
                RUNNER_DIGEST,
            )
        return duration_ms

    async def apply_change_non_transactional(
        self, conn: Any, baseline_id: str, spec: ChangeSpec, epoch_dir: Path
    ) -> int:
        """Apply an explicitly non-transactional change with attempts/leases.

        This path never pretends to be atomic: attempt rows record the phase,
        checkpoint and lease so a crash lands in a decidable resumable/failed
        state.  The success ledger row is written only after every
        postcondition passes.
        """
        sql = (epoch_dir / spec.file).read_text(encoding="utf-8")
        actual_sha = hashlib.sha256(sql.encode("utf-8")).hexdigest()
        if actual_sha != spec.sha256:
            raise AuthorityError(
                f"change {spec.sequence} ({spec.file}) checksum mismatch: "
                f"manifest declares {spec.sha256}, file is {actual_sha}"
            )
        self._reject_embedded_transactions(sql, spec.name)

        declared_handlers = {name for name in (spec.resume_handler, spec.repair_handler) if name}
        if not declared_handlers:
            raise AuthorityBlockedError(
                f"non-transactional change {spec.sequence} has no declared "
                "resume_handler or repair_handler"
            )
        missing_handlers = sorted(declared_handlers - self._recovery_handlers.keys())
        if missing_handlers:
            raise AuthorityBlockedError(
                f"non-transactional change {spec.sequence} has no registered "
                f"recovery implementation for {missing_handlers}"
            )

        segments = self._split_checkpoints(sql)
        known_checkpoints = {name for name, _segment in segments}
        lease_owner = f"{RUNNER_DIGEST}:{socket.gethostname()}:{uuid.uuid4().hex[:8]}"
        rows = await conn.fetch(_SELECT_LATEST_ATTEMPT, baseline_id, spec.sequence)
        latest = dict(rows[0]) if rows else None
        recovery_kind: str | None = None
        handler_name: str | None = None
        prior_state = "new"

        if latest is None:
            attempt_id = f"{spec.sequence:03d}-{uuid.uuid4().hex}"
            checkpoint = ""
            fence_generation = 0
            await conn.execute(
                ledger.INSERT_ATTEMPT,
                attempt_id,
                baseline_id,
                spec.sequence,
                spec.sha256,
                RUNNER_DIGEST,
                "apply",
                checkpoint,
                lease_owner,
                fence_generation,
            )
        else:
            attempt_id = str(latest["attempt_id"])
            if str(latest["checksum_sha256"]) != spec.sha256:
                raise AuthorityBlockedError(
                    f"change {spec.sequence} latest attempt has checksum "
                    f"{latest['checksum_sha256']}, expected {spec.sha256}"
                )
            if str(latest["runner_digest"]) != RUNNER_DIGEST:
                raise AuthorityBlockedError(
                    f"change {spec.sequence} latest attempt was created by runner "
                    f"{latest['runner_digest']}, current runner is {RUNNER_DIGEST}"
                )
            checkpoint = str(latest.get("checkpoint") or "")
            if checkpoint and checkpoint not in known_checkpoints:
                raise AuthorityBlockedError(
                    f"change {spec.sequence} attempt {attempt_id} has unknown "
                    f"checkpoint {checkpoint!r}; known={sorted(known_checkpoints)}"
                )
            prior_state = str(latest["state"])
            fence_generation = int(latest["fence_generation"])

            if prior_state == ledger.ATTEMPT_STATE_RUNNING:
                if not self._attempt_expired(latest):
                    raise AuthorityBlockedError(
                        f"change {spec.sequence} attempt {attempt_id} still has an active lease"
                    )
                expired_state = (
                    ledger.ATTEMPT_STATE_RESUMABLE
                    if spec.resume_handler
                    else ledger.ATTEMPT_STATE_FAILED
                )
                transitioned = await conn.fetchval(
                    _TRANSITION_EXPIRED_ATTEMPT,
                    attempt_id,
                    str(latest["lease_owner"]),
                    fence_generation,
                    expired_state,
                    "lease_expired",
                )
                self._require_fence(
                    transitioned, action="expire stale lease", attempt_id=attempt_id
                )
                prior_state = expired_state

            if prior_state == ledger.ATTEMPT_STATE_RESUMABLE:
                recovery_kind = "resume"
                handler_name = spec.resume_handler
            elif prior_state == ledger.ATTEMPT_STATE_FAILED:
                recovery_kind = "repair"
                handler_name = spec.repair_handler
            else:
                raise AuthorityBlockedError(
                    f"change {spec.sequence} attempt {attempt_id} has unsupported "
                    f"state {prior_state!r}"
                )

            claimed = await conn.fetchval(
                _CLAIM_ATTEMPT,
                attempt_id,
                lease_owner,
                fence_generation,
                recovery_kind,
            )
            fence_generation = self._require_fence(
                claimed, action="claim recovery lease", attempt_id=attempt_id
            )

        start_index = 0
        if checkpoint:
            start_index = next(
                index + 1 for index, (name, _segment) in enumerate(segments) if name == checkpoint
            )
        start = time.monotonic()
        await conn.execute(f"SET statement_timeout = {int(spec.timeout_seconds) * 1000}")
        await conn.execute(f"SET lock_timeout = {int(spec.lock_budget_seconds) * 1000}")
        try:
            if recovery_kind is not None:
                context = RecoveryContext(
                    baseline_id=baseline_id,
                    spec=spec,
                    attempt_id=attempt_id,
                    checkpoint=checkpoint,
                    fence_generation=fence_generation,
                    prior_state=prior_state,
                )
                await self._run_recovery_handler(
                    handler_name,
                    conn,
                    context,
                    kind=recovery_kind,
                )

            await self._evaluate_conditions(
                conn, spec.preconditions, change_name=spec.name, phase="pre"
            )
            for checkpoint_name, segment in segments[start_index:]:
                execution_role = f"{self.role_prefix}{spec.owner}"
                previous_role = await conn.fetchval("SELECT current_setting('role')")
                await conn.execute(f'SET ROLE "{execution_role}"')
                try:
                    await conn.execute(segment)
                finally:
                    await conn.execute("SELECT set_config('role', $1, false)", previous_role)
                updated = await conn.fetchval(
                    _UPDATE_ATTEMPT_CHECKPOINT_FENCED,
                    attempt_id,
                    lease_owner,
                    fence_generation,
                    checkpoint_name,
                )
                fence_generation = self._require_fence(
                    updated, action="record checkpoint", attempt_id=attempt_id
                )
            await self._evaluate_conditions(
                conn, spec.postconditions, change_name=spec.name, phase="post"
            )
        except Exception as exc:  # noqa: BLE001 - attempt must record the failure
            failed_state = (
                ledger.ATTEMPT_STATE_RESUMABLE
                if spec.resume_handler
                else ledger.ATTEMPT_STATE_FAILED
            )
            terminal = await conn.fetchval(
                _UPDATE_ATTEMPT_TERMINAL_FENCED,
                attempt_id,
                lease_owner,
                fence_generation,
                failed_state,
                type(exc).__name__,
            )
            self._require_fence(terminal, action="record failed attempt", attempt_id=attempt_id)
            raise
        finally:
            # Non-transactional changes use session-scoped settings. Always
            # restore them before this connection can be reused by ledger or
            # status work, including recovery-handler and postcheck failures.
            await conn.execute("RESET statement_timeout")
            await conn.execute("RESET lock_timeout")
        duration_ms = int((time.monotonic() - start) * 1000)
        async with conn.transaction():
            await conn.execute(
                ledger.INSERT_CHANGE_SUCCESS,
                baseline_id,
                spec.sequence,
                spec.name,
                spec.sha256,
                duration_ms,
                RUNNER_DIGEST,
            )
            terminal = await conn.fetchval(
                _UPDATE_ATTEMPT_TERMINAL_FENCED,
                attempt_id,
                lease_owner,
                fence_generation,
                ledger.ATTEMPT_STATE_SUCCEEDED,
                None,
            )
            self._require_fence(terminal, action="record successful attempt", attempt_id=attempt_id)
        return duration_ms

    async def apply_epoch(
        self, conn: Any, epoch_manifest: EpochManifest, epoch_dir: Path
    ) -> list[str]:
        """Apply all pending epoch changes in strict sequence order."""
        active, _ = await inspect_ledger_acl(conn, self.role_prefix)
        if not active:
            raise AuthorityError("versioned ledger ACL bootstrap required before epoch discovery")
        applied = await self.applied_changes(conn, epoch_manifest.baseline_id)
        applied_lines: list[str] = []

        declared = epoch_manifest.by_sequence()
        unknown_sequences = sorted(set(applied) - set(declared))
        if unknown_sequences:
            raise AuthorityError(
                f"baseline {epoch_manifest.baseline_id} ledger contains sequences "
                f"outside its immutable manifest: {unknown_sequences}"
            )
        for sequence, recorded_checksum in sorted(applied.items()):
            declared_checksum = declared[sequence].sha256
            if recorded_checksum != declared_checksum:
                raise AuthorityError(
                    f"change {sequence} is recorded with checksum {recorded_checksum} "
                    f"but the manifest declares {declared_checksum}; ledger history "
                    "is immutable — refusing to migrate"
                )
        applied_sequences = sorted(applied)
        expected_applied = list(range(1, len(applied_sequences) + 1))
        if applied_sequences != expected_applied:
            raise AuthorityError(
                f"baseline {epoch_manifest.baseline_id} ledger is not a contiguous "
                f"prefix: recorded={applied_sequences}, expected={expected_applied}"
            )

        expected_next = len(applied_sequences) + 1
        for spec in epoch_manifest.changes:
            recorded = applied.get(spec.sequence)
            if recorded is not None:
                continue
            if spec.sequence != expected_next:
                raise AuthorityError(
                    f"change {spec.sequence} cannot be applied before sequence "
                    f"{expected_next}; out-of-order application is rejected"
                )
            await self.ensure_ledger(conn)
            if spec.transaction_mode is TransactionMode.TRANSACTIONAL:
                duration = await self.apply_change_transactional(
                    conn, epoch_manifest.baseline_id, spec, epoch_dir
                )
            else:
                duration = await self.apply_change_non_transactional(
                    conn, epoch_manifest.baseline_id, spec, epoch_dir
                )
            applied_lines.append(
                f"applied {epoch_manifest.baseline_id}:{spec.sequence:03d} "
                f"{spec.name} ({duration}ms, rollback={spec.rollback_class.value})"
            )
            expected_next += 1

        return applied_lines

    # ------------------------------------------------------------------
    # legacy chain (pre-baseline history, compatibility path)
    # ------------------------------------------------------------------

    def discover_legacy(self) -> list[LegacyMigration]:
        migrations = discover_legacy_migrations(self.paths.migrations_root)
        validate_legacy_chain(migrations, allow_historical_filename_duplicates=True)
        return migrations

    @staticmethod
    def legacy_is_applied(applied: set[str], migration: LegacyMigration) -> bool:
        if migration.path.name in applied:
            return True
        alias = LEGACY_FILENAME_ALIASES.get(migration.path.name)
        return alias in applied if alias else False

    async def pending_legacy(
        self, _conn: Any, tracking_mode: str, applied: set[str]
    ) -> list[LegacyMigration]:
        migrations = self.discover_legacy()
        if tracking_mode == "version":
            validate_legacy_chain(migrations, allow_historical_filename_duplicates=False)
            numeric_applied = {name[:3] for name in applied}
            return [m for m in migrations if m.version not in numeric_applied]
        return [m for m in migrations if not self.legacy_is_applied(applied, m)]
