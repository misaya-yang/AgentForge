"""Native executor for the immutable pre-baseline migration chain.

The authority owns discovery, execution and legacy-ledger writes under one
session advisory lock.  Filename and historical numeric ledgers remain
compatible until baseline adoption, after which they are frozen evidence.

Transactional files run inside a runner-owned transaction together with the
ledger write.  If a historical file contains a single outer ``BEGIN/COMMIT``
pair, only that pair is removed; procedural ``BEGIN`` tokens inside dollar
quotes are ignored.  Non-transactional migration 049 is executed outside a
transaction, verified, and only then recorded, so retry after a crash is safe.
"""

from __future__ import annotations

import hashlib
import re
from pathlib import Path
from typing import Any

from .constants import DEFAULT_ROLE_PREFIX  # noqa: F401  (public compatibility seam)
from .discovery import (
    LEGACY_FILENAME_ALIASES,
    LEGACY_MANIFEST_NAME,
    LegacyMigration,
    discover_legacy_migrations,
    validate_legacy_chain,
)
from .manifest import LegacyChangeSpec, LegacyManifest, TransactionMode, load_legacy_manifest
from .numeric_reconciliation import (
    NumericReconciliationBlocked,
    NumericReconciliationReceipt,
    reconcile_numeric_legacy_history,
)
from .per_service_manifest import (
    HISTORICAL_MARKER_NOTES,
    PER_SERVICE_MANIFEST_NAME,
    load_per_service_manifest,
)
from .runner import AuthorityBlockedError, AuthorityError, AuthorityPaths

# Same four required objects migrate.sh/base_schema_exists checks.
_BASE_SCHEMA_PROBE = """
SELECT CASE WHEN
    COALESCE(to_regclass('gateway.services'), to_regclass('public.services')) IS NOT NULL
    AND COALESCE(to_regclass('knowledge.datasets'), to_regclass('public.datasets')) IS NOT NULL
    AND COALESCE(to_regclass('knowledge.documents'), to_regclass('public.documents')) IS NOT NULL
    AND COALESCE(to_regclass('knowledge.segments'), to_regclass('public.segments')) IS NOT NULL
THEN TRUE ELSE FALSE END
"""

_TRACKING_FILENAME = "filename"
_TRACKING_VERSION = "version"
_PLATFORM_SCHEMAS = frozenset({"public", "gateway", "assistant", "knowledge"})
_DOLLAR_QUOTE_RE = re.compile(r"\$(?:[A-Za-z_][A-Za-z0-9_]*)?\$")
_TRANSACTION_CONTROLS = {
    "BEGIN",
    "BEGIN TRANSACTION",
    "START TRANSACTION",
    "COMMIT",
    "END",
    "ROLLBACK",
    "ABORT",
}

_NON_TRANSACTIONAL_INDEX_STATE_SQL = """
/* arc03-legacy-049:index-state */
SELECT n.nspname AS schema_name, i.indisvalid, i.indisready,
       pg_get_indexdef(i.indexrelid) AS definition
FROM pg_index AS i
JOIN pg_class AS ic ON ic.oid = i.indexrelid
JOIN pg_namespace AS n ON n.oid = ic.relnamespace
JOIN pg_class AS tc ON tc.oid = i.indrelid
JOIN pg_namespace AS tn ON tn.oid = tc.relnamespace
WHERE n.nspname = tn.nspname
  AND n.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
  AND ic.relname = 'idx_sessions_user_tenant_status_updated'
  AND tc.relname = 'sessions'
ORDER BY n.nspname
"""

_NON_TRANSACTIONAL_049_POSTCHECK_SQL = r"""
/* arc03-legacy-049:postcheck */
WITH matches AS (
    SELECT i.indisvalid, i.indisready, i.indisunique, i.indpred,
           i.indnkeyatts, i.indnatts, pg_get_indexdef(i.indexrelid) AS definition
    FROM pg_index AS i
    JOIN pg_class AS ic ON ic.oid = i.indexrelid
    JOIN pg_namespace AS n ON n.oid = ic.relnamespace
    JOIN pg_class AS tc ON tc.oid = i.indrelid
    JOIN pg_namespace AS tn ON tn.oid = tc.relnamespace
    WHERE n.nspname = tn.nspname
      AND n.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
      AND ic.relname = 'idx_sessions_user_tenant_status_updated'
      AND tc.relname = 'sessions'
)
SELECT count(*) = 1
   AND bool_and(indisvalid AND indisready AND NOT indisunique)
   AND bool_and(indpred IS NULL AND indnkeyatts = 4 AND indnatts = 4)
   AND bool_and(
       definition ~
       '\(user_id, tenant_id, status, updated_at DESC\)$'
   )
FROM matches
"""


def legacy_checksum(content: str) -> str:
    """Truncated checksum exactly as the historical ledgers recorded it."""
    return hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]


async def base_schema_present(conn: Any) -> bool:
    return bool(await conn.fetchval(_BASE_SCHEMA_PROBE))


async def ensure_base_schema(conn: Any, paths: AuthorityPaths, log: Any = print) -> None:
    """Require an existing legacy base without replaying current ``schema.sql``.

    The frozen baseline is the only fresh-install source. Applying the current
    compatibility snapshot to a partial or foreign database would invent
    migration history and can replay data-changing legacy SQL.
    """
    if await base_schema_present(conn):
        return
    raise AuthorityBlockedError(
        "legacy platform base objects are incomplete; database/schema.sql replay is retired. "
        "Use the frozen baseline for an empty database or restore/reconcile this database "
        "explicitly."
    )


async def configure_legacy_search_path(conn: Any) -> None:
    """Match the historical runners: knowledge-first migration resolution."""
    await conn.execute("CREATE SCHEMA IF NOT EXISTS knowledge")
    await conn.execute("SET search_path TO knowledge, gateway, assistant, public")


async def tracking_mode(conn: Any) -> str | None:
    """Detect the legacy ledger shape; None when no ledger exists yet."""
    columns = await conn.fetch(
        """
        SELECT column_name
        FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'schema_migrations'
        """
    )
    names = {str(row["column_name"]) for row in columns}
    if "filename" in names:
        return _TRACKING_FILENAME
    if "version" in names:
        return _TRACKING_VERSION
    if names:
        raise AuthorityError(
            "public.schema_migrations exists but has neither filename nor "
            "version column; refusing to guess a tracking mode"
        )
    return None


async def ensure_filename_ledger(conn: Any) -> None:
    """Create the canonical filename ledger only when no legacy ledger exists."""
    await conn.execute(
        """
        CREATE TABLE IF NOT EXISTS public.schema_migrations (
            filename   TEXT PRIMARY KEY,
            applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )
        """
    )


async def applied_legacy_names(conn: Any, mode: str) -> set[str]:
    """Applied filenames (filename mode) or normalized three-digit versions."""
    if mode == _TRACKING_FILENAME:
        rows = await conn.fetch("SELECT filename FROM public.schema_migrations")
        return {str(row["filename"]) for row in rows}

    columns = await conn.fetch(
        """
        SELECT column_name
        FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'schema_migrations'
        """
    )
    names = {str(row["column_name"]) for row in columns}
    dirty_filter = " WHERE dirty = FALSE" if "dirty" in names else ""
    rows = await conn.fetch(f"SELECT version FROM public.schema_migrations{dirty_filter}")
    return {f"{int(row['version']):03d}" for row in rows}


def legacy_is_applied(mode: str, applied: set[str], migration: LegacyMigration) -> bool:
    if mode == _TRACKING_VERSION:
        return migration.version in applied
    if migration.path.name in applied:
        return True
    alias = LEGACY_FILENAME_ALIASES.get(migration.path.name)
    return alias in applied if alias else False


async def pending_legacy_migrations(
    conn: Any,
    paths: AuthorityPaths,
    *,
    mode: str | None,
    migrations: list[LegacyMigration] | None = None,
    legacy_manifest: LegacyManifest | None = None,
) -> tuple[str, list[LegacyMigration], NumericReconciliationReceipt | None]:
    """Return mode, pending files and any numeric reconciliation receipt."""
    migrations = migrations or discover_legacy_migrations(paths.migrations_root)
    legacy_manifest = legacy_manifest or load_legacy_manifest(
        paths.migrations_root / LEGACY_MANIFEST_NAME
    )
    effective_mode = mode
    if effective_mode is None:
        effective_mode = _TRACKING_FILENAME
        await ensure_filename_ledger(conn)
    receipt = None
    if effective_mode == _TRACKING_VERSION:
        # Reconciliation deliberately precedes duplicate-chain validation.
        receipt = await reconcile_numeric_legacy_history(conn, legacy_manifest)
        if receipt.verdict != "proven":
            raise NumericReconciliationBlocked(
                "numeric legacy reconciliation BLOCKED before migration discovery:",
                receipt,
            )
    validate_legacy_chain(migrations, allow_historical_filename_duplicates=True)
    applied = await applied_legacy_names(conn, effective_mode)
    pending = [
        migration
        for migration in migrations
        if not legacy_is_applied(effective_mode, applied, migration)
    ]
    return effective_mode, pending, receipt


async def record_legacy_migration(
    conn: Any, mode: str, migration: LegacyMigration, duration_ms: int
) -> None:
    """Write success in exactly the ledger shape the database already has."""
    if mode == _TRACKING_FILENAME:
        await conn.execute(
            "INSERT INTO public.schema_migrations (filename) VALUES ($1) "
            "ON CONFLICT (filename) DO NOTHING",
            migration.path.name,
        )
        return

    columns = await conn.fetch(
        """
        SELECT column_name
        FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'schema_migrations'
        """
    )
    names = {str(row["column_name"]) for row in columns}
    numeric_version = int(migration.version)
    if "dirty" in names:
        await conn.execute(
            "INSERT INTO public.schema_migrations (version, dirty) VALUES ($1, FALSE) "
            "ON CONFLICT (version) DO UPDATE SET dirty = FALSE",
            numeric_version,
        )
        return
    if {"name", "checksum", "execution_time_ms"} <= names:
        content = migration.path.read_text(encoding="utf-8")
        await conn.execute(
            """
            INSERT INTO public.schema_migrations (
                version, name, checksum, execution_time_ms
            ) VALUES ($1, $2, $3, $4)
            ON CONFLICT (version) DO UPDATE SET
                applied_at = NOW(), checksum = $3, execution_time_ms = $4
            """,
            migration.version,
            migration.path.name,
            legacy_checksum(content),
            duration_ms,
        )
        return
    await conn.execute(
        "INSERT INTO public.schema_migrations (version) VALUES ($1) "
        "ON CONFLICT (version) DO NOTHING",
        numeric_version,
    )


def _mask_non_sql(sql: str) -> str:
    """Blank comments and quoted bodies while preserving offsets."""
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
                raise AuthorityError("legacy SQL contains an unterminated block comment")
            masked[start:index] = " " * (index - start)
            continue
        if sql[index] in {"'", '"'}:
            quote = sql[index]
            start = index
            index += 1
            while index < length:
                if sql[index] == quote:
                    if index + 1 < length and sql[index + 1] == quote:
                        index += 2
                        continue
                    index += 1
                    break
                if sql[index] == "\\" and quote == "'" and index + 1 < length:
                    index += 2
                else:
                    index += 1
            else:
                raise AuthorityError("legacy SQL contains an unterminated quoted string")
            masked[start:index] = " " * (index - start)
            continue
        if sql[index] == "$":
            match = _DOLLAR_QUOTE_RE.match(sql, index)
            if match is not None:
                delimiter = match.group(0)
                start = index
                end = sql.find(delimiter, match.end())
                if end == -1:
                    raise AuthorityError("legacy SQL contains an unterminated dollar quote")
                index = end + len(delimiter)
                masked[start:index] = " " * (index - start)
                continue
        index += 1
    return "".join(masked)


def _strip_outer_transaction(sql: str) -> tuple[str, bool]:
    """Remove exactly one complete outer transaction, rejecting other control."""
    masked = _mask_non_sql(sql)
    statements: list[tuple[int, int, str]] = []
    start = 0
    for index, char in enumerate(masked):
        if char != ";":
            continue
        normalized = " ".join(masked[start:index].split()).upper()
        if normalized:
            statements.append((start, index + 1, normalized))
        start = index + 1
    trailing = " ".join(masked[start:].split()).upper()
    if trailing:
        statements.append((start, len(sql), trailing))

    controls = [
        (position, statement)
        for position, (_start, _end, statement) in enumerate(statements)
        if statement in _TRANSACTION_CONTROLS
    ]
    if not controls:
        return sql, False
    accepted_begin = {"BEGIN", "BEGIN TRANSACTION", "START TRANSACTION"}
    accepted_commit = {"COMMIT", "END"}
    valid_outer = (
        len(controls) == 2
        and controls[0][0] == 0
        and controls[0][1] in accepted_begin
        and controls[1][0] == len(statements) - 1
        and controls[1][1] in accepted_commit
    )
    if not valid_outer:
        descriptions = ", ".join(f"{position}:{statement}" for position, statement in controls)
        raise AuthorityError(
            "legacy SQL transaction control must be one paired outer BEGIN/COMMIT; "
            f"found [{descriptions}]"
        )
    begin_end = statements[0][1]
    commit_start = statements[-1][0]
    return sql[begin_end:commit_start], True


async def _prepare_non_transactional_legacy(conn: Any, filename: str) -> None:
    if filename != "049_session_list_performance.sql":
        raise AuthorityError(f"no safe non-transactional recovery contract for {filename}")
    rows = await conn.fetch(_NON_TRANSACTIONAL_INDEX_STATE_SQL)
    if len(rows) > 1:
        raise AuthorityError(
            "migration 049 found duplicate same-named indexes across platform schemas"
        )
    if not rows:
        return
    row = rows[0]
    if bool(row["indisvalid"]) and bool(row["indisready"]):
        return
    schema = str(row["schema_name"])
    if schema not in _PLATFORM_SCHEMAS:
        raise AuthorityError(f"migration 049 recovery refused unexpected schema {schema!r}")
    # CREATE INDEX CONCURRENTLY can leave an invalid same-named shell after a
    # crash.  It has never been usable by queries, and IF NOT EXISTS cannot
    # repair it, so remove only that exact invalid artifact before retrying.
    await conn.execute(
        f'DROP INDEX CONCURRENTLY IF EXISTS "{schema}"."idx_sessions_user_tenant_status_updated"'
    )


def _validate_non_transactional_contract(filename: str, sql: str) -> None:
    if filename != "049_session_list_performance.sql":
        raise AuthorityError(f"no safe non-transactional contract for {filename}")
    normalized = " ".join(_mask_non_sql(sql).split()).upper()
    expected = (
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS "
        "IDX_SESSIONS_USER_TENANT_STATUS_UPDATED ON SESSIONS "
        "(USER_ID, TENANT_ID, STATUS, UPDATED_AT DESC);"
    )
    if normalized != expected:
        raise AuthorityError(
            "migration 049 no longer matches its single-statement idempotent "
            "CREATE INDEX CONCURRENTLY contract"
        )


async def _verify_non_transactional_legacy(conn: Any, filename: str) -> None:
    if filename != "049_session_list_performance.sql":
        raise AuthorityError(f"no postcondition contract for {filename}")
    if not bool(await conn.fetchval(_NON_TRANSACTIONAL_049_POSTCHECK_SQL)):
        raise AuthorityError(
            "migration 049 postcondition failed; the valid/ready exact index was not proven"
        )


async def run_one_legacy(
    conn: Any,
    mode: str,
    migration: LegacyMigration,
    spec: LegacyChangeSpec,
    log: Any = print,
) -> None:
    """Execute one immutable migration and atomically record transactional SQL."""
    content = migration.path.read_text(encoding="utf-8")
    actual_sha = hashlib.sha256(content.encode("utf-8")).hexdigest()
    if spec.file != migration.path.name or spec.sha256 != actual_sha:
        raise AuthorityError(
            f"legacy migration identity changed after manifest validation: {migration.path.name}"
        )
    executable_sql, had_outer_transaction = _strip_outer_transaction(content)
    log(f"authority: legacy: applying {migration.path.name}")
    start = _monotonic_ms()

    if spec.transaction_mode is TransactionMode.NON_TRANSACTIONAL:
        if had_outer_transaction:
            raise AuthorityError(
                f"legacy manifest marks {spec.file} non-transactional but it embeds a transaction"
            )
        _validate_non_transactional_contract(spec.file, executable_sql)
        await _prepare_non_transactional_legacy(conn, spec.file)
        await conn.execute(executable_sql)
        await _verify_non_transactional_legacy(conn, spec.file)
        duration_ms = _monotonic_ms() - start
        await record_legacy_migration(conn, mode, migration, duration_ms)
        return

    async with conn.transaction():
        await conn.execute(executable_sql)
        duration_ms = _monotonic_ms() - start
        await record_legacy_migration(conn, mode, migration, duration_ms)


def _monotonic_ms() -> int:
    import time

    return int(time.monotonic() * 1000)


async def apply_legacy_chain(
    conn: Any, paths: AuthorityPaths, log: Any = print
) -> tuple[str, int, NumericReconciliationReceipt | None]:
    """Apply the legacy chain and return mode, count, and reconciliation receipt."""
    await ensure_base_schema(conn, paths, log=log)
    await configure_legacy_search_path(conn)
    mode = await tracking_mode(conn)
    migrations = discover_legacy_migrations(paths.migrations_root)
    legacy_manifest = load_legacy_manifest(paths.migrations_root / LEGACY_MANIFEST_NAME)
    effective_mode, pending, receipt = await pending_legacy_migrations(
        conn,
        paths,
        mode=mode,
        migrations=migrations,
        legacy_manifest=legacy_manifest,
    )
    if receipt is not None:
        log("authority: numeric reconciliation receipt:\n" + receipt.to_json())
    specs = legacy_manifest.by_file()
    for migration in pending:
        await run_one_legacy(
            conn,
            effective_mode,
            migration,
            specs[migration.path.name],
            log=log,
        )
    if not pending:
        log("authority: legacy chain already complete")
    return effective_mode, len(pending), receipt


async def per_service_ledger_present(conn: Any) -> bool:
    return bool(
        await conn.fetchval("SELECT to_regclass('public.schema_migrations_meta') IS NOT NULL")
    )


def _per_service_root(paths: AuthorityPaths) -> Path:
    return paths.migrations_root / "per_service"


async def apply_per_service_chain(conn: Any, paths: AuthorityPaths, log: Any = print) -> int:
    """Top up only databases that already carry the per-service ledger."""
    if not await per_service_ledger_present(conn):
        return 0
    root = _per_service_root(paths)
    if not root.is_dir():
        return 0
    manifest = load_per_service_manifest(root / PER_SERVICE_MANIFEST_NAME)
    applied_rows = await conn.fetch("SELECT name, notes FROM public.schema_migrations_meta")
    applied = {str(row["name"]) for row in applied_rows}
    applied_notes = {str(row["name"]): str(row["notes"] or "") for row in applied_rows}
    global_bootstrap = manifest.changes[0]
    if not global_bootstrap.is_recorded(applied):
        raise AuthorityBlockedError(
            "per-service ledger exists without the atomic phase6_schemas_created "
            "or canonical _global:001 receipt; refusing to replay its historical "
            "PUBLIC CREATE/database search_path change"
        )
    applied_count = 0
    for spec in manifest.changes:
        if spec.key in applied:
            note = applied_notes.get(spec.key, "")
            legacy_note = f"file=migrations/per_service/{spec.file}"
            authority_note = (
                f"{legacy_note};sha256={spec.sha256};rollback={spec.rollback_class.value}"
            )
            if note not in {legacy_note, authority_note}:
                raise AuthorityBlockedError(
                    f"per-service ledger {spec.key} has no exact historical file "
                    "receipt or current checksum receipt"
                )
            continue
        historical = sorted(set(spec.historical_markers) & applied)
        if historical:
            for marker in historical:
                expected_note = HISTORICAL_MARKER_NOTES.get(marker)
                if expected_note is None or applied_notes.get(marker) != expected_note:
                    raise AuthorityBlockedError(
                        f"per-service historical marker {marker} has no exact notes receipt"
                    )
            continue
        log(f"authority: per-service: applying {spec.key}")
        sql, _had_outer_transaction = _strip_outer_transaction(
            (root / spec.file).read_text(encoding="utf-8")
        )
        async with conn.transaction():
            await conn.execute(sql)
            await conn.execute(
                "INSERT INTO public.schema_migrations_meta(name, notes) "
                "VALUES ($1, $2) ON CONFLICT (name) DO NOTHING",
                spec.key,
                f"file=migrations/per_service/{spec.file};"
                f"sha256={spec.sha256};rollback={spec.rollback_class.value}",
            )
        applied_count += 1
    if applied_count == 0:
        log("authority: per-service track already complete")
    return applied_count
