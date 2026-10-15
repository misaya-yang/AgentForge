"""Legacy-ledger detection, reconciliation receipts and baseline adoption.

Existing installations are never deleted, rewritten or faked.  The authority:

1. detects which pre-authority ledgers exist (filename, numeric, per-service
   meta);
2. verifies the legacy chain is complete up to the baseline freeze point,
   topping up partial databases through the compatibility runner first;
3. for numeric ledgers, duplicate version prefixes (016/031) are ambiguous —
   a reconciliation receipt records per-file object evidence and anything
   that cannot be proven stays ``BLOCKED``;
4. computes the four fingerprints and writes ONE adoption marker when (and
   only when) they match the frozen baseline; otherwise it emits a drift
   report and fails closed;
5. freezes the legacy ledgers as historical evidence — readable, never
   written again.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from . import ledger
from .constants import DEFAULT_ROLE_PREFIX
from .discovery import LEGACY_FILENAME_ALIASES, LegacyMigration
from .fingerprint import compute_fingerprints
from .manifest import BaselineManifest
from .numeric_reconciliation import NumericReconciliationReceipt
from .runner import AuthorityBlockedError, AuthorityError


@dataclass(frozen=True)
class LedgerState:
    """Which legacy ledgers a database carries and what they record."""

    filename_ledger: bool = False
    numeric_ledger: bool = False
    numeric_has_dirty: bool = False
    per_service_ledger: bool = False
    applied_filenames: frozenset[str] = frozenset()
    applied_versions: frozenset[str] = frozenset()
    per_service_keys: frozenset[str] = frozenset()

    @property
    def has_any(self) -> bool:
        return self.filename_ledger or self.numeric_ledger or self.per_service_ledger


# Compatibility name for consumers that imported the receipt from adoption.
ReconciliationReceipt = NumericReconciliationReceipt


def validate_existing_adoption_marker(
    existing: list[Any],
    baseline: BaselineManifest,
    *,
    manifest_sha256: str,
) -> str | None:
    """Validate the one immutable marker row; return its baseline id if present."""
    if not existing:
        return None
    if len(existing) != 1:
        raise AuthorityError(
            f"database contains {len(existing)} adoption markers; exactly one is allowed"
        )
    marker = dict(existing[0])
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
        f"{field}: marker={marker.get(field)!r}, manifest={value!r}"
        for field, value in expected.items()
        if str(marker.get(field, "")) != str(value)
    ]
    if drift:
        raise AuthorityError(
            "existing adoption marker does not match the frozen baseline; "
            + "; ".join(drift)
        )
    return baseline.baseline_id


async def detect_legacy_state(conn: Any) -> LedgerState:
    """Read every legacy ledger without writing anything."""
    columns = await conn.fetch(
        """
        SELECT column_name
        FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'schema_migrations'
        """
    )
    column_names = {str(row["column_name"]) for row in columns}
    filename_ledger = "filename" in column_names
    numeric_ledger = "version" in column_names

    applied_filenames: set[str] = set()
    applied_versions: set[str] = set()
    numeric_has_dirty = False
    if filename_ledger:
        rows = await conn.fetch("SELECT filename FROM public.schema_migrations")
        applied_filenames = {str(row["filename"]) for row in rows}
    if numeric_ledger:
        numeric_has_dirty = "dirty" in column_names
        dirty_filter = " WHERE dirty = FALSE" if numeric_has_dirty else ""
        rows = await conn.fetch(
            f"SELECT version FROM public.schema_migrations{dirty_filter}"
        )
        applied_versions = {f"{int(row['version']):03d}" for row in rows}

    per_service = await conn.fetchval(
        "SELECT to_regclass('public.schema_migrations_meta') IS NOT NULL"
    )
    per_service_keys: set[str] = set()
    if per_service:
        rows = await conn.fetch("SELECT name FROM public.schema_migrations_meta")
        per_service_keys = {str(row["name"]) for row in rows}

    return LedgerState(
        filename_ledger=filename_ledger,
        numeric_ledger=numeric_ledger,
        numeric_has_dirty=numeric_has_dirty,
        per_service_ledger=bool(per_service),
        applied_filenames=frozenset(applied_filenames),
        applied_versions=frozenset(applied_versions),
        per_service_keys=frozenset(per_service_keys),
    )


def legacy_missing_files(
    state: LedgerState, migrations: list[LegacyMigration]
) -> list[LegacyMigration]:
    """Legacy files not covered by any legacy ledger row.

    Historical rename aliases count as applied: old deployments recorded the
    earlier ``codex_runtime`` filenames for the same changes.
    """
    missing: list[LegacyMigration] = []
    for migration in migrations:
        name = migration.path.name
        if state.filename_ledger:
            alias = LEGACY_FILENAME_ALIASES.get(name)
            if name in state.applied_filenames or (
                alias is not None and alias in state.applied_filenames
            ):
                continue
            missing.append(migration)
            continue
        if state.numeric_ledger and migration.version in state.applied_versions:
            continue
        missing.append(migration)
    return missing


async def adopt_baseline(
    conn: Any,
    baseline: BaselineManifest,
    *,
    manifest_sha256: str,
    role_prefix: str = DEFAULT_ROLE_PREFIX,
) -> dict[str, str]:
    """Write the adoption marker iff all four fingerprints match.

    Never executes init.sql; never repairs drift.  Returns the computed
    fingerprint dict on success and raises with a drift report otherwise.
    """
    existing = await conn.fetch(ledger.SELECT_BASELINE)
    adopted_id = validate_existing_adoption_marker(
        existing, baseline, manifest_sha256=manifest_sha256
    )
    if adopted_id is not None:
        return {"already_adopted": adopted_id}

    computed = await compute_fingerprints(
        conn, role_prefix=role_prefix, reference_sets=baseline.reference_data
    )
    drift: list[str] = []
    for fingerprint_class in ("structural", "acl", "extensions", "reference_data"):
        expected = baseline.fingerprints[fingerprint_class]
        actual = computed[fingerprint_class]
        if expected != actual:
            drift.append(
                f"{fingerprint_class}: expected {expected}, computed {actual}"
            )
    if drift:
        report = "; ".join(drift)
        raise AuthorityError(
            "baseline adoption refused — fingerprint drift detected; "
            f"drift report: {report}. No automatic repair is attempted."
        )

    await conn.execute(
        ledger.INSERT_BASELINE_MARKER,
        baseline.baseline_id,
        manifest_sha256,
        baseline.structural_sha256,
        baseline.acl_sha256,
        baseline.extensions_sha256,
        baseline.reference_data_sha256,
        baseline.source_git_sha,
    )
    # ``INSERT_BASELINE_MARKER`` deliberately uses ON CONFLICT so concurrent
    # writers cannot replace immutable evidence.  Never interpret the command
    # completing as proof that *our* marker won the race: read the single row
    # back and compare every frozen identity field before reporting success.
    inserted = await conn.fetch(ledger.SELECT_BASELINE)
    validate_existing_adoption_marker(
        inserted, baseline, manifest_sha256=manifest_sha256
    )
    return computed


def assert_legacy_ledgers_frozen(
    state: LedgerState, baseline_adopted: bool
) -> None:
    """After adoption the legacy ledgers are evidence, never writers."""
    if baseline_adopted and not state.has_any:
        raise AuthorityBlockedError(
            "baseline marker present but no legacy ledger found; "
            "the adoption history is incomplete"
        )
