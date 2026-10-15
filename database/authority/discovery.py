"""Migration discovery with dual-path legacy support.

Step one of the two-step directory move: the authority recognizes BOTH the
historical flat layout (``database/migrations/*.sql``) and the future layout
(``database/migrations/legacy/*.sql``).  If both exist for the same filename
the authority fails closed instead of guessing which copy is real.  Step two
(the physical move) is a separate change that only runs once every consumer
points at the authority.

Epoch directories (``database/migrations/<baseline_id>/``) are discovered
separately and never mixed into the legacy chain.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

from .constants import DEFAULT_BASELINE_ID, LEGACY_DIR_NAME

MIGRATION_PATTERN = re.compile(r"^(\d{3})_(.+)\.sql$")
ROLLBACK_SUFFIX = "_rollback.sql"
LEGACY_MANIFEST_NAME = "legacy-manifest.yml"

# These duplicate numeric prefixes pre-date both legacy ledgers.  A filename
# ledger can distinguish them; a numeric ledger cannot.  Never extend this
# list: new duplicate prefixes are always an error.
HISTORICAL_FILENAME_DUPLICATES = {
    "016": frozenset({"016_confluence_multi_root_pages.sql", "016_usage_hourly_aggregates.sql"}),
    "031": frozenset({"031_align_model_prices_20260211.sql", "031_hierarchical_segments.sql"}),
}

# Ordered identities needed to interpret legacy numeric-ledger rows. Version
# 030 has only one current forward file, but the pre-62587dc9 Python runner
# auto-discovered its rollback file and could overwrite the same numeric row.
# This is reconciliation evidence, never an execution order for new SQL.
HISTORICAL_NUMERIC_IDENTITIES = {
    "016": (
        "016_confluence_multi_root_pages.sql",
        "016_usage_hourly_aggregates.sql",
    ),
    "030": (
        "030_fix_timestamp_and_security_constraint.sql",
        "030_fix_timestamp_and_security_constraint_rollback.sql",
    ),
    "031": (
        "031_align_model_prices_20260211.sql",
        "031_hierarchical_segments.sql",
    ),
}

# Historical rename aliases recorded by old deployments.
LEGACY_FILENAME_ALIASES = {
    "089_agent_runtime_thread_store.sql": "089_codex_runtime_thread_store.sql",
    "090_agent_runtime_model_leases.sql": "090_codex_runtime_model_leases.sql",
    "092_agent_runtime_legacy_import.sql": "092_codex_runtime_legacy_import.sql",
    "093_agent_runtime_assistant_session_fks.sql": "093_codex_runtime_assistant_session_fks.sql",
    "094_agent_runtime_legacy_import_normalization.sql": (
        "094_codex_runtime_legacy_import_normalization.sql"
    ),
}


class DiscoveryError(RuntimeError):
    """The on-disk migration layout is ambiguous or corrupt."""


@dataclass(frozen=True)
class LegacyMigration:
    version: str
    description: str
    path: Path


def _is_epoch_dir(path: Path) -> bool:
    return path.is_dir() and path.name not in (LEGACY_DIR_NAME, "per_service", "__pycache__")


def legacy_candidates(migrations_root: Path) -> list[Path]:
    """Return the legacy migration directories the authority recognizes.

    Both the flat layout and the ``legacy/`` layout are accepted during the
    two-step move; empty slots are skipped.
    """
    candidates: list[Path] = []
    if migrations_root.is_dir():
        candidates.append(migrations_root)
    legacy_dir = migrations_root / LEGACY_DIR_NAME
    if legacy_dir.is_dir():
        candidates.append(legacy_dir)
    return candidates


def discover_legacy_migrations(migrations_root: Path) -> list[LegacyMigration]:
    """Discover the immutable pre-baseline chain across both layouts.

    A filename present in BOTH the flat and the legacy/ layout is ambiguity
    and fails closed: the authority never guesses which copy to run.
    """
    seen: dict[str, Path] = {}
    ordered: list[LegacyMigration] = []

    for directory in legacy_candidates(migrations_root):
        for file_path in sorted(directory.glob("*.sql")):
            if file_path.name.endswith(ROLLBACK_SUFFIX):
                continue
            match = MIGRATION_PATTERN.fullmatch(file_path.name)
            if not match:
                continue
            existing = seen.get(file_path.name)
            if existing is not None and existing != file_path:
                raise DiscoveryError(
                    f"migration {file_path.name} exists in both "
                    f"{existing.parent} and {file_path.parent}; "
                    "complete the two-step move before running the authority"
                )
            if existing is None:
                seen[file_path.name] = file_path
                ordered.append(
                    LegacyMigration(
                        version=match.group(1),
                        description=match.group(2).replace("_", " ").title(),
                        path=file_path,
                    )
                )

    ordered.sort(key=lambda migration: (migration.version, migration.path.name))
    return ordered


def validate_legacy_chain(
    migrations: list[LegacyMigration],
    *,
    allow_historical_filename_duplicates: bool,
) -> None:
    """Reject ambiguous numeric revisions before executing migration SQL."""
    files_by_version: dict[str, set[str]] = {}
    for migration in migrations:
        files_by_version.setdefault(migration.version, set()).add(migration.path.name)

    for version, filenames in sorted(files_by_version.items()):
        if len(filenames) == 1:
            continue
        if allow_historical_filename_duplicates and frozenset(filenames) == (
            HISTORICAL_FILENAME_DUPLICATES.get(version)
        ):
            continue
        joined = ", ".join(sorted(filenames))
        raise DiscoveryError(
            f"duplicate migration version {version}: {joined}; "
            "each new forward migration needs a unique numeric prefix"
        )


def discover_epoch_dirs(migrations_root: Path) -> list[Path]:
    """Epoch directories hold post-baseline changes keyed by baseline id."""
    if not migrations_root.is_dir():
        return []
    return sorted(path for path in migrations_root.iterdir() if _is_epoch_dir(path))


def last_legacy_change(migrations: list[LegacyMigration]) -> str:
    """Filename of the highest legacy migration (the baseline freeze point)."""
    if not migrations:
        raise DiscoveryError("no legacy migrations discovered")
    return migrations[-1].path.name


def default_migrations_root(database_dir: Path) -> Path:
    return database_dir / "migrations"


def baseline_epoch_dir(migrations_root: Path, baseline_id: str = DEFAULT_BASELINE_ID) -> Path:
    return migrations_root / baseline_id
