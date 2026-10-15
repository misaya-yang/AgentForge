"""Top-level authority operations (the only sanctioned schema writers).

Every write entrypoint acquires the one session advisory lock, keeps
transactions in the runner's hands, and fails closed instead of guessing.

Flow of ``command_migrate`` for a database without a baseline marker:

1. frozen baseline + empty database: install the baseline directly; legacy
   ``schema.sql`` and the historical chain are never replayed;
2. non-empty database guard: an unknown database without any platform object is
   refused (the authority never mistakes foreign data for a legacy install);
3. legacy base schema: required platform objects must already exist; the
   authority never replays ``database/schema.sql`` into a non-empty database;
4. legacy chain: when an existing ledger identifies the history, the
   historical ``002…112`` files are executed by the
   authority's native executor, recorded in whichever legacy ledger shape
   the database already carries; a ledgerless ``schema.sql`` bootstrap is
   preserved and proven only by convergence postconditions plus all four
   frozen fingerprints — no synthetic legacy rows are created;
5. per-service track: topped up ONLY on databases that already carry
   ``public.schema_migrations_meta``;
6. cutover + adoption (when the baseline files are frozen and adoption is
   allowed): roles → extensions → convergence change → grants → ledger →
   fingerprint comparison → ONE adoption marker.  Legacy ledgers are then
   frozen evidence, never written again.

Databases that already carry the marker only receive pending epoch changes
(``database/migrations/<baseline_id>/``).
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ai_gateway_contracts.database_revision import (
    KNOWLEDGE_MAX_SUPPORTED_EPOCH_SEQUENCE,
    KNOWLEDGE_MIN_REQUIRED_EPOCH_SEQUENCE,
    MAX_SUPPORTED_EPOCH_SEQUENCE,
    MIN_REQUIRED_EPOCH_SEQUENCE,
)

from . import ledger, legacy
from .ledger_acl import compatible_expected, owner_scope
from .adoption import (
    adopt_baseline,
    detect_legacy_state,
    legacy_missing_files,
)
from .bootstrap import (
    baseline_manifest_sha256,
    bootstrap_extensions,
    bootstrap_roles,
    database_empty,
    fresh_install,
    provision_extensions_admin,
    provision_roles_admin,
    run_baseline_sql_file,
    startup_schema_check,
    verify_baseline_sql_file,
)
from .constants import DEFAULT_BASELINE_ID, EPOCH_MANIFEST_NAME, PLATFORM_SCHEMAS
from .credentials import APPLICATION_PRINCIPALS
from .discovery import LEGACY_MANIFEST_NAME, discover_legacy_migrations
from .manifest import (
    BASELINE_STATE_FROZEN,
    BaselineManifest,
    baseline_artifact_state,
    load_baseline_manifest,
    load_epoch_manifest,
    load_legacy_manifest,
    verify_baseline_git_provenance,
)
from .numeric_reconciliation import (
    NumericReconciliationBlocked,
    NumericReconciliationReceipt,
    reconcile_numeric_legacy_history,
)
from .runner import AuthorityBlockedError, AuthorityError, AuthorityPaths, MigrationAuthority

SUPPORTED_BASELINES = frozenset({DEFAULT_BASELINE_ID})
# Application windows are reviewed per service; the migration authority itself
# always applies the complete canonical manifest.


@dataclass(frozen=True)
class MigrationCommandResult:
    """Programmatic migrate result; CLI callers consume ``exit_code``."""

    exit_code: int
    reconciliation_receipt: NumericReconciliationReceipt | None = None

    def to_json(self) -> str:
        return json.dumps(
            {
                "exit_code": self.exit_code,
                "numeric_reconciliation": (
                    self.reconciliation_receipt.as_dict()
                    if self.reconciliation_receipt is not None
                    else {"verdict": "not_applicable"}
                ),
            },
            indent=2,
            sort_keys=True,
        )


def _write_migration_evidence(result: MigrationCommandResult, evidence_out: Path | None) -> None:
    """Write evidence only when the operator explicitly names an output path."""
    if evidence_out is None:
        return
    evidence_out.parent.mkdir(parents=True, exist_ok=True)
    evidence_out.write_text(result.to_json() + "\n", encoding="utf-8")


def default_paths() -> AuthorityPaths:
    database_dir = Path(__file__).resolve().parents[1]
    return AuthorityPaths(database_dir=database_dir)


def load_baseline(paths: AuthorityPaths, baseline_id: str) -> tuple[BaselineManifest, str]:
    baseline_dir = paths.baseline_dir(baseline_id)
    manifest_path = baseline_dir / "manifest.json"
    baseline = load_baseline_manifest(manifest_path)
    repo_root = paths.database_dir.parent
    if (repo_root / ".git").exists():
        verify_baseline_git_provenance(
            manifest_path,
            baseline,
            repo_root=repo_root,
        )
    legacy_manifest = load_legacy_manifest(paths.migrations_root / LEGACY_MANIFEST_NAME)
    if baseline.last_legacy_change != legacy_manifest.freeze_point:
        raise AuthorityError(
            f"baseline {baseline.baseline_id} freezes legacy change "
            f"{baseline.last_legacy_change!r}, but the immutable legacy manifest "
            f"freezes {legacy_manifest.freeze_point!r}"
        )
    return baseline, baseline_manifest_sha256(manifest_path)


def baseline_ready(paths: AuthorityPaths, baseline_id: str = DEFAULT_BASELINE_ID) -> bool:
    """A baseline is usable only when all of its frozen files exist."""
    baseline_dir = paths.baseline_dir(baseline_id)
    required = (
        "manifest.json",
        "init.sql",
        "reference_data.sql",
        "grants.sql",
        "verify.sql",
        "cutover_convergence.sql",
    )
    if not all((baseline_dir / name).exists() for name in required):
        return False
    return baseline_artifact_state(baseline_dir / "manifest.json") == BASELINE_STATE_FROZEN


def _validate_adoption_marker(
    adopted: dict[str, Any],
    baseline: BaselineManifest,
    manifest_sha256: str,
) -> None:
    """Prove that an existing marker names this exact frozen baseline."""
    expected = {
        "baseline_id": baseline.baseline_id,
        "manifest_sha256": manifest_sha256,
        "structural_sha256": baseline.structural_sha256,
        "acl_sha256": baseline.acl_sha256,
        "extensions_sha256": baseline.extensions_sha256,
        "reference_data_sha256": baseline.reference_data_sha256,
        "source_git_sha": baseline.source_git_sha,
    }
    drift = [
        f"{field}: marker={adopted.get(field)!r}, manifest={value!r}"
        for field, value in expected.items()
        if str(adopted.get(field, "")) != str(value)
    ]
    if drift:
        raise AuthorityError(
            "adopted baseline marker does not match the frozen manifest; " + "; ".join(drift)
        )


# ----------------------------------------------------------------------
# migrate: the single write path
# ----------------------------------------------------------------------


async def _guard_known_database(conn: Any) -> None:
    """Refuse to treat an unknown non-empty database as a legacy install."""
    if await database_empty(conn):
        return
    if await legacy.base_schema_present(conn):
        return
    raise AuthorityBlockedError(
        "database is not empty and carries none of the required platform "
        "objects (services/datasets/documents/segments); the authority does "
        "not apply schema.sql or migrations to an unknown database"
    )


async def command_migrate(
    authority: MigrationAuthority,
    *,
    baseline_id: str = DEFAULT_BASELINE_ID,
    allow_adoption: bool = True,
    allow_fresh: bool = True,
    reconciliation_evidence_out: Path | None = None,
    log: Any = print,
) -> MigrationCommandResult:
    """Bring one database to the current schema revision.

    * baseline adopted  -> apply pending epoch changes only;
    * no baseline, baseline files ready, empty database -> install the frozen
      baseline and then apply its epoch;
    * no baseline, baseline files ready, existing database -> complete the
      legacy chain, cut over ownership/grants, adopt the baseline;
    * no baseline, baseline files not ready -> compatibility mode: apply the
      legacy chain only (pre-freeze transition behaviour).

    The structured result carries any numeric reconciliation receipt. JSON is
    written only when ``reconciliation_evidence_out`` is explicitly supplied.
    """
    paths = authority.paths

    if baseline_ready(paths, baseline_id):
        baseline, manifest_sha = load_baseline(paths, baseline_id)
    else:
        baseline = None
        manifest_sha = ""
        log(
            "authority: baseline files not frozen yet — compatibility mode "
            "(legacy chain only, no adoption)"
        )

    reconciliation_receipt: NumericReconciliationReceipt | None = None
    lock_conn = await authority.connect()
    await authority.acquire_lock(lock_conn)
    try:
        conn = await authority.connect()
        try:
            adopted = await authority.adopted_baseline(conn)

            if adopted is None:
                if baseline is not None and await database_empty(
                    conn,
                    allowed_empty_schemas=tuple(PLATFORM_SCHEMAS),
                ):
                    if not allow_fresh:
                        raise AuthorityBlockedError(
                            "cutover requires an existing legacy database; empty databases "
                            "must use init-fresh"
                        )
                    await fresh_install(
                        conn,
                        paths,
                        baseline,
                        manifest_sha,
                        role_prefix=authority.role_prefix,
                    )
                    epoch_dir = paths.epoch_dir(baseline_id)
                    manifest_path = epoch_dir / EPOCH_MANIFEST_NAME
                    if manifest_path.exists():
                        epoch_manifest = load_epoch_manifest(manifest_path)
                        for line in await authority.apply_epoch(conn, epoch_manifest, epoch_dir):
                            log(f"authority: {line}")
                    log(f"authority: fresh install on baseline {baseline.baseline_id} complete")
                    result = MigrationCommandResult(0)
                    _write_migration_evidence(result, reconciliation_evidence_out)
                    return result
                await _guard_known_database(conn)
                legacy_state = await detect_legacy_state(conn)
                ledgerless_schema = not legacy_state.has_any
                if ledgerless_schema:
                    if baseline is None:
                        raise AuthorityBlockedError(
                            "existing platform schema has no legacy ledger and no frozen "
                            "baseline is available for fingerprint reconciliation; refusing "
                            "to replay schema.sql or guess historical migrations"
                        )
                    if not allow_adoption:
                        raise AuthorityBlockedError(
                            "ledgerless schema reconciliation cannot stop before adoption; "
                            "convergence, four-fingerprint verification, and the immutable "
                            "baseline marker must complete in one successful authority plan"
                        )
                    log(
                        "authority: ledgerless schema bootstrap detected; preserving existing "
                        "objects for convergence and four-fingerprint proof (no legacy rows "
                        "synthesized)"
                    )
                else:
                    try:
                        _, _, reconciliation_receipt = await legacy.apply_legacy_chain(
                            conn, paths, log=log
                        )
                    except NumericReconciliationBlocked as exc:
                        log("authority: numeric reconciliation BLOCKED:\n" + exc.receipt.to_json())
                        _write_migration_evidence(
                            MigrationCommandResult(1, exc.receipt),
                            reconciliation_evidence_out,
                        )
                        raise
                    await legacy.apply_per_service_chain(conn, paths, log=log)
                if baseline is None or not allow_adoption:
                    result = MigrationCommandResult(0, reconciliation_receipt)
                    _write_migration_evidence(result, reconciliation_evidence_out)
                    return result
                try:
                    await _cutover_and_adopt(
                        conn,
                        authority,
                        baseline,
                        manifest_sha,
                        reconciliation_receipt=reconciliation_receipt,
                        allow_ledgerless_schema=ledgerless_schema,
                        log=log,
                    )
                except NumericReconciliationBlocked as exc:
                    log("authority: numeric reconciliation BLOCKED:\n" + exc.receipt.to_json())
                    _write_migration_evidence(
                        MigrationCommandResult(1, exc.receipt),
                        reconciliation_evidence_out,
                    )
                    raise
                epoch_dir = paths.epoch_dir(baseline_id)
                manifest_path = epoch_dir / EPOCH_MANIFEST_NAME
                if manifest_path.exists():
                    epoch_manifest = load_epoch_manifest(manifest_path)
                    for line in await authority.apply_epoch(conn, epoch_manifest, epoch_dir):
                        log(f"authority: {line}")
                result = MigrationCommandResult(0, reconciliation_receipt)
                _write_migration_evidence(result, reconciliation_evidence_out)
                return result

            # Phase 2: epoch changes under the frozen baseline.
            if baseline is None:
                raise AuthorityError(
                    f"database carries adopted baseline {adopted['baseline_id']!r}, but "
                    f"the frozen local manifest for {baseline_id!r} is unavailable"
                )
            _validate_adoption_marker(adopted, baseline, manifest_sha)
            await _require_ledger_acl_state(authority, conn, baseline)
            epoch_dir = paths.epoch_dir(baseline_id)
            manifest_path = epoch_dir / EPOCH_MANIFEST_NAME
            if manifest_path.exists():
                epoch_manifest = load_epoch_manifest(manifest_path)
                for line in await authority.apply_epoch(conn, epoch_manifest, epoch_dir):
                    log(f"authority: {line}")
                if not epoch_manifest.changes:
                    log("authority: no post-baseline epoch changes declared")
            else:
                log("authority: no epoch manifest; nothing to apply")
        finally:
            await conn.close()
    finally:
        await authority.release_lock(lock_conn)
    result = MigrationCommandResult(0, reconciliation_receipt)
    _write_migration_evidence(result, reconciliation_evidence_out)
    return result


async def _cutover_and_adopt(
    conn: Any,
    authority: MigrationAuthority,
    baseline: BaselineManifest,
    manifest_sha: str,
    *,
    reconciliation_receipt: NumericReconciliationReceipt | None = None,
    allow_ledgerless_schema: bool = False,
    log: Any = print,
) -> None:
    """Cut a completed legacy database over to the baseline, then adopt.

    Runs under the caller's advisory lock.  Order matters: roles and
    extensions first (grants reference the roles), then the convergence
    change, then least-privilege grants, then the ledger + marker.
    """
    existing = await authority.adopted_baseline(conn)
    if existing is not None:
        _validate_adoption_marker(existing, baseline, manifest_sha)
        log(f"authority: baseline already adopted ({existing['baseline_id']})")
        return

    paths = authority.paths
    baseline_dir = paths.baseline_dir(baseline.baseline_id)
    cutover_path = baseline_dir / "cutover_convergence.sql"
    grants_path = baseline_dir / "grants.sql"

    reconciliation_receipt = await _verify_legacy_cutover_ready(
        conn,
        paths,
        reconciliation_receipt=reconciliation_receipt,
        allow_ledgerless_schema=allow_ledgerless_schema,
        log=log,
    )

    # Roles/extensions are idempotent; an adopted database must carry the
    # same role set and search_path configuration as a fresh install.
    await bootstrap_roles(conn, paths, authority.role_prefix)
    await bootstrap_extensions(conn, paths, authority.role_prefix)

    owner_role = f"{authority.role_prefix}owner"
    async with conn.transaction():
        await run_baseline_sql_file(
            conn,
            cutover_path,
            role_prefix=authority.role_prefix,
            execution_role=owner_role,
        )
        await run_baseline_sql_file(
            conn,
            grants_path,
            role_prefix=authority.role_prefix,
            execution_role=owner_role,
        )
        await verify_baseline_sql_file(conn, baseline_dir / "verify.sql", role_prefix=authority.role_prefix)

        await conn.execute(ledger.LEDGER_DDL)
        computed = await adopt_baseline(
            conn,
            baseline,
            manifest_sha256=manifest_sha,
            role_prefix=authority.role_prefix,
        )
    if "already_adopted" in computed:
        adopted = await authority.adopted_baseline(conn)
        if adopted is None:
            raise AuthorityError(
                "adoption reported an existing marker, but no marker row can be read"
            )
        _validate_adoption_marker(adopted, baseline, manifest_sha)
        log(f"authority: baseline already adopted ({computed['already_adopted']})")
    else:
        history = (
            "ledgerless bootstrap preserved without synthetic legacy rows"
            if allow_ledgerless_schema
            else "legacy ledgers frozen as historical evidence"
        )
        log(f"authority: baseline {baseline.baseline_id} adopted; {history}")


async def _verify_legacy_cutover_ready(
    conn: Any,
    paths: AuthorityPaths,
    *,
    reconciliation_receipt: NumericReconciliationReceipt | None = None,
    allow_ledgerless_schema: bool = False,
    log: Any = print,
) -> NumericReconciliationReceipt | None:
    """Read-only proof required by both admin ownership prep and adoption."""
    state = await detect_legacy_state(conn)
    if not state.has_any:
        if allow_ledgerless_schema and await legacy.base_schema_present(conn):
            log(
                "authority: ledgerless schema source accepted for read-only object "
                "reconciliation; convergence postconditions and frozen fingerprints "
                "remain authoritative"
            )
            return reconciliation_receipt
        raise AuthorityBlockedError(
            "no legacy ledger present at adoption time; the compatibility "
            "runner must complete the chain before the baseline marker, or an "
            "explicit ledgerless schema reconciliation must prove the frozen baseline"
        )

    migrations = discover_legacy_migrations(paths.migrations_root)
    missing = legacy_missing_files(state, migrations)
    if missing:
        raise AuthorityBlockedError(
            "legacy chain incomplete; missing files: "
            + ", ".join(m.path.name for m in missing)
            + " — run the compatibility runner first"
        )

    if state.numeric_ledger and not state.filename_ledger:
        receipt = reconciliation_receipt
        if receipt is None:
            receipt = await reconcile_numeric_legacy_history(
                conn,
                load_legacy_manifest(paths.migrations_root / LEGACY_MANIFEST_NAME),
            )
        if receipt.verdict != "proven":
            raise NumericReconciliationBlocked(
                "numeric ledger reconciliation BLOCKED; receipt:",
                receipt,
            )
        log("authority: numeric-ledger reconciliation receipt proven")
        return receipt
    return reconciliation_receipt


async def command_prepare_cutover_ownership(
    authority: MigrationAuthority,
    *,
    expected_database: str,
    baseline_id: str = DEFAULT_BASELINE_ID,
    reconciliation_evidence_out: Path | None = None,
    log: Any = print,
) -> MigrationCommandResult:
    """Admin-only one-time transfer of legacy-owned objects to the owner role.

    The schema migrator cannot alter objects owned by a historical superuser.
    This explicit phase runs only after the legacy chain is proven complete;
    it never grants application privileges and never writes an adoption marker.
    The following migrator ``cutover`` reruns the idempotent convergence under
    the owner role, applies grants, verifies fingerprints and writes the marker.
    """
    if not expected_database:
        raise AuthorityBlockedError("admin ownership cutover requires --expected-database")
    if not baseline_ready(authority.paths, baseline_id):
        raise AuthorityBlockedError("admin ownership cutover requires a frozen baseline")
    baseline, _manifest_sha = load_baseline(authority.paths, baseline_id)
    lock_conn = await authority.connect()
    await authority.acquire_lock(lock_conn)
    receipt: NumericReconciliationReceipt | None = None
    try:
        conn = await authority.connect()
        try:
            actual_database = str(await conn.fetchval("SELECT current_database()"))
            if actual_database != expected_database:
                raise AuthorityBlockedError(
                    "admin ownership cutover DSN does not target --expected-database"
                )
            if await database_empty(
                conn,
                allowed_empty_schemas=tuple(PLATFORM_SCHEMAS),
            ):
                raise AuthorityBlockedError(
                    "admin ownership cutover is forbidden on an empty database"
                )
            await _guard_known_database(conn)
            receipt = await _verify_legacy_cutover_ready(
                conn,
                authority.paths,
                allow_ledgerless_schema=True,
                log=log,
            )
            await provision_roles_admin(conn, authority.paths, authority.role_prefix)
            await provision_extensions_admin(conn, authority.paths)
            await run_baseline_sql_file(
                conn,
                authority.paths.bootstrap_dir / "legacy_duplicate_reconciliation.sql",
                role_prefix=authority.role_prefix,
            )
            await run_baseline_sql_file(
                conn,
                authority.paths.baseline_dir(baseline.baseline_id) / "cutover_convergence.sql",
                role_prefix=authority.role_prefix,
            )
            await run_baseline_sql_file(
                conn,
                authority.paths.bootstrap_dir / "legacy_structural_reconciliation.sql",
                execution_role=f"{authority.role_prefix}owner",
            )
        finally:
            await conn.close()
    finally:
        await authority.release_lock(lock_conn)
    result = MigrationCommandResult(0, receipt)
    _write_migration_evidence(result, reconciliation_evidence_out)
    return result


# ----------------------------------------------------------------------
# init-fresh: empty database -> baseline
# ----------------------------------------------------------------------


async def command_init_fresh(
    authority: MigrationAuthority,
    *,
    baseline_id: str = DEFAULT_BASELINE_ID,
    log: Any = print,
) -> int:
    paths = authority.paths
    if not baseline_ready(paths, baseline_id):
        raise AuthorityError(
            f"baseline {baseline_id} is not frozen (missing files under "
            f"{paths.baseline_dir(baseline_id)}); refusing fresh install"
        )
    baseline, manifest_sha = load_baseline(paths, baseline_id)

    lock_conn = await authority.connect()
    await authority.acquire_lock(lock_conn)
    try:
        conn = await authority.connect()
        try:
            await fresh_install(
                conn, paths, baseline, manifest_sha, role_prefix=authority.role_prefix
            )
            epoch_dir = paths.epoch_dir(baseline_id)
            manifest_path = epoch_dir / EPOCH_MANIFEST_NAME
            if manifest_path.exists():
                epoch_manifest = load_epoch_manifest(manifest_path)
                for line in await authority.apply_epoch(conn, epoch_manifest, epoch_dir):
                    log(f"authority: {line}")
            log(f"authority: fresh install on baseline {baseline.baseline_id} complete")
        finally:
            await conn.close()
    finally:
        await authority.release_lock(lock_conn)
    return 0


# ----------------------------------------------------------------------
# read-only surfaces
# ----------------------------------------------------------------------


async def command_status(authority: MigrationAuthority, *, log: Any = print) -> int:
    """Absolutely read-only status over baseline, ledger and legacy tables."""
    conn = await authority.connect(read_only=True)
    try:
        state = await detect_legacy_state(conn)
        adopted = await authority.adopted_baseline(conn)
        if adopted:
            baseline, manifest_sha = load_baseline(authority.paths, adopted["baseline_id"])
            _validate_adoption_marker(adopted, baseline, manifest_sha)
            await _require_ledger_acl_state(authority, conn, baseline)
            log(f"baseline: {adopted['baseline_id']} (adopted {adopted['adopted_at']})")
            baseline_id = adopted["baseline_id"]
            rows = await conn.fetch(ledger.SELECT_APPLIED_CHANGES, baseline_id)
            if rows:
                for row in rows:
                    log(f"  epoch change {row['sequence']}: {row['checksum_sha256']}")
            else:
                log("  no post-baseline epoch changes applied")
            attempts_present = await conn.fetchval(
                "SELECT to_regclass($1) IS NOT NULL", f"public.{ledger.ATTEMPTS_TABLE}"
            )
            if attempts_present:
                open_attempts = await conn.fetch(
                    f"SELECT attempt_id, sequence, state, checkpoint "
                    f"FROM public.{ledger.ATTEMPTS_TABLE} "
                    "ORDER BY started_at DESC LIMIT 5"
                )
                for attempt in open_attempts:
                    log(
                        f"  attempt {attempt['attempt_id']}: seq={attempt['sequence']} "
                        f"state={attempt['state']} checkpoint={attempt['checkpoint']}"
                    )
        else:
            log("baseline: none adopted")
        if state.filename_ledger:
            log(f"legacy filename ledger rows: {len(state.applied_filenames)}")
        if state.numeric_ledger:
            log(f"legacy numeric ledger rows: {len(state.applied_versions)}")
        if state.per_service_ledger:
            log(f"per-service ledger rows: {len(state.per_service_keys)}")
        pending_legacy = (
            legacy_missing_files(state, authority.discover_legacy())
            if state.has_any
            else authority.discover_legacy()
        )
        log(f"legacy chain pending files: {len(pending_legacy)}")
    finally:
        await conn.close()
    return 0


async def command_source_kind(authority: MigrationAuthority, *, log: Any = print) -> int:
    """Print one stable, read-only source classification for deploy orchestration."""
    conn = await authority.connect(read_only=True)
    try:
        if await authority.adopted_baseline(conn) is not None:
            log("adopted")
            return 0
        if await database_empty(conn, allowed_empty_schemas=tuple(PLATFORM_SCHEMAS)):
            log("empty")
            return 0
        await _guard_known_database(conn)
        state = await detect_legacy_state(conn)
        if state.has_any:
            log("tracked-legacy")
            return 0
        if await legacy.base_schema_present(conn):
            log("ledgerless-platform")
            return 0
        raise AuthorityBlockedError("database source cannot be classified safely")
    finally:
        await conn.close()


async def command_verify(
    authority: MigrationAuthority,
    *,
    baseline_id: str = DEFAULT_BASELINE_ID,
    log: Any = print,
) -> int:
    """Read-only verification against the exact adopted baseline/epoch."""
    paths = authority.paths
    if not baseline_ready(paths, baseline_id):
        raise AuthorityError(f"baseline {baseline_id} is not frozen; cannot verify")
    baseline, manifest_sha = load_baseline(paths, baseline_id)

    conn = await authority.connect(read_only=True)
    try:
        from .fingerprint import compute_fingerprints

        adopted = await authority.adopted_baseline(conn)
        if adopted is not None:
            _validate_adoption_marker(adopted, baseline, manifest_sha)

        from .epoch_verification import expected_epoch_fingerprints

        applied = await authority.applied_changes(conn, baseline_id)
        expected_fingerprints = await compatible_expected(
            conn, expected_epoch_fingerprints(paths, baseline, applied), authority.role_prefix
        )

        async with owner_scope(conn, authority.role_prefix):
            computed = await compute_fingerprints(
                conn, role_prefix=authority.role_prefix, reference_sets=baseline.reference_data
            )
        failures = []
        for name in ("structural", "acl", "extensions", "reference_data"):
            expected = expected_fingerprints[name]
            actual = computed[name]
            status = "match" if expected == actual else "DRIFT"
            log(f"fingerprint {name}: {status}")
            if expected != actual:
                failures.append(name)

        if applied:
            # The frozen grant-matrix SELECT describes epoch zero. Exact epoch
            # hashes replace that obsolete catalog oracle; migration data
            # postconditions are still checked on the read-only connection.
            epoch = load_epoch_manifest(paths.epoch_dir(baseline_id) / EPOCH_MANIFEST_NAME)
            for change in epoch.changes:
                if change.sequence in applied:
                    await authority._evaluate_conditions(
                        conn, change.postconditions, change_name=change.name, phase="verify",
                    )
        else:
            checks = await verify_baseline_sql_file(
                conn, paths.baseline_dir(baseline_id) / "verify.sql", role_prefix=authority.role_prefix,
                ledger_acl_expected=baseline.fingerprints,
            )
            for check in checks:
                log(f"verify check {check['check_name']}: match")

        if failures:
            raise AuthorityError(f"verification failed for: {failures}")
        log(f"verify: baseline/epoch {max(applied, default=0)} fingerprints match; database is read-only-verified")
    finally:
        await conn.close()
    return 0


async def command_startup_check(
    authority: MigrationAuthority,
    *,
    principal: str = "gateway",
    log: Any = print,
) -> int:
    """Application startup gate: principal-specific revision + required objects."""
    if principal not in APPLICATION_PRINCIPALS:
        raise AuthorityError(f"unknown application principal {principal!r}")
    minimum = MIN_REQUIRED_EPOCH_SEQUENCE
    maximum = MAX_SUPPORTED_EPOCH_SEQUENCE
    if principal in {"knowledge_api", "knowledge_worker"}:
        minimum = KNOWLEDGE_MIN_REQUIRED_EPOCH_SEQUENCE
        maximum = KNOWLEDGE_MAX_SUPPORTED_EPOCH_SEQUENCE
    conn = await authority.connect(read_only=True)
    try:
        result = await startup_schema_check(
            conn,
            SUPPORTED_BASELINES,
            max_epoch_sequence=maximum,
            min_epoch_sequence=minimum,
        )
    finally:
        await conn.close()
    if not result["ok"]:
        reason = result.get("reason") or (
            "missing required objects: " + ", ".join(result["missing_objects"])
        )
        raise AuthorityError(f"startup schema check failed: {reason}")
    log(f"startup schema check passed (epoch={result['epoch']})")
    return 0


async def command_bootstrap_ledger_acl(authority: MigrationAuthority, *, baseline_id: str = DEFAULT_BASELINE_ID) -> dict[str, Any]:
    """Explicit admin bootstrap, never a migration retry under admin credentials."""
    from .ledger_acl import inspect, install, receipt
    from .epoch_verification import expected_epoch_fingerprints
    from .fingerprint import compute_fingerprints

    baseline, manifest_sha = load_baseline(authority.paths, baseline_id)
    conn = await authority.connect()
    await authority.acquire_lock(conn)
    try:
        if not await conn.fetchval("SELECT current_setting('is_superuser', true) = 'on'"):
            raise AuthorityError("ledger ACL bootstrap requires the explicit admin connection")
        async with conn.transaction():
            adopted = await authority.adopted_baseline(conn)
            if adopted is None:
                raise AuthorityError("ledger ACL repair requires a verified adopted baseline; empty databases use init-fresh")
            _validate_adoption_marker(adopted, baseline, manifest_sha)
            applied = await authority.applied_changes(conn, baseline_id)
            expected = expected_epoch_fingerprints(authority.paths, baseline, applied)
            active, _ = await inspect(conn, authority.role_prefix)
            before_expected = (await compatible_expected(conn, expected, authority.role_prefix)) if active else expected
            before = await compute_fingerprints(conn, role_prefix=authority.role_prefix, reference_sets=baseline.reference_data)
            if before != before_expected:
                raise AuthorityError("ledger ACL bootstrap refuses pre-existing fingerprint drift")
            await install(conn, authority.role_prefix)
            after = await compute_fingerprints(conn, role_prefix=authority.role_prefix, reference_sets=baseline.reference_data)
            if after != await compatible_expected(conn, expected, authority.role_prefix):
                raise AuthorityError("ledger ACL bootstrap postcondition fingerprint mismatch")
            result = await receipt(conn, expected, authority.role_prefix)
            return {**result, "baseline_id": baseline_id, "baseline_manifest_sha256": manifest_sha,
                    "epoch_sequence": max(applied, default=0), "fingerprints": after}
    finally:
        await authority.release_lock(conn)


async def _require_ledger_acl_state(authority: MigrationAuthority, conn: Any, baseline: BaselineManifest) -> None:
    """Fail closed before migration writes or successful status output."""
    from .epoch_verification import expected_epoch_fingerprints
    from .fingerprint import compute_fingerprints
    applied = await authority.applied_changes(conn, baseline.baseline_id)
    expected = await compatible_expected(
        conn, expected_epoch_fingerprints(authority.paths, baseline, applied), authority.role_prefix
    )
    async with owner_scope(conn, authority.role_prefix):
        actual = await compute_fingerprints(conn, role_prefix=authority.role_prefix, reference_sets=baseline.reference_data)
    if actual != expected:
        raise AuthorityError("authority ledger compatibility gate detected fingerprint drift")
