"""Shared constants for the single schema-migration authority.

Every schema writer in the repository (Python runners, the shell runner, the
Compose migrate job, Gateway startup) must funnel through this authority and
its ledger.  Constants here are the contract they all share.
"""

from __future__ import annotations

# Session-level advisory lock shared with database/cli.py,
# database/migrate_per_service.py and scripts/new/common.sh.  One lock
# serializes every migration writer, Python or shell, across processes.
MIGRATION_ADVISORY_LOCK_NAMESPACE = 1_095_781_959
MIGRATION_ADVISORY_LOCK_ID = 1

# Ledger tables owned exclusively by the migrator identity.  Names are
# prefixed so they can never collide with application tables.
BASELINES_TABLE = "platform_schema_baselines"
CHANGES_TABLE = "platform_schema_changes"
ATTEMPTS_TABLE = "platform_schema_change_attempts"
LEDGER_TABLES = (BASELINES_TABLE, CHANGES_TABLE, ATTEMPTS_TABLE)

# Tables that record pre-authority history.  After a baseline is adopted they
# are frozen evidence: readable, never written again.
LEGACY_LEDGER_TABLES = ("schema_migrations", "schema_migrations_meta")

DEFAULT_BASELINE_ID = "2026_08_post_kb_v1"

# Epoch layout: database/migrations/<baseline_id>/ holds post-baseline
# changes; database/migrations/legacy/ holds the immutable pre-baseline chain
# (step two of the two-step directory move).  Until the move happens the
# authority also recognizes the historical flat layout database/migrations/.
EPOCH_MANIFEST_NAME = "manifest.yml"
LEGACY_DIR_NAME = "legacy"

# Role model (PRD ARC-03 §3D).  The NOLOGIN object owner never logs in; the
# LOGIN roles get least-privilege grants.  The prefix is configurable per
# deployment so managed PostgreSQL environments can namespace roles, while the
# fingerprint uses the logical ids below instead of the physical names.
ROLE_PREFIX_ENV = "AI_GATEWAY_ROLE_PREFIX"
DEFAULT_ROLE_PREFIX = "ai_gateway_"
LOGICAL_PRINCIPALS = (
    "owner",  # NOLOGIN object owner
    "migrator",  # sole DDL executor, never CREATEROLE
    "gateway",
    "runtime",
    "capability_worker",
    "knowledge_api",
    "knowledge_worker",
)

# Extensions the platform may use.  Anything outside this allowlist fails the
# extensions fingerprint.  ``vector`` is permitted for future embedding work
# but is not installed by bootstrap today.
EXTENSION_ALLOWLIST = ("uuid-ossp", "pgcrypto", "pg_trgm", "vector")

# Schemas the platform uses.  ``public`` stays last in every role
# search_path and no application role may CREATE in it.
PLATFORM_SCHEMAS = ("gateway", "assistant", "knowledge", "public")

# Catalog schemas excluded from every fingerprint.  pg_toast is excluded
# because toast relation names embed OID suffixes that differ between
# environments, which would poison the structural fingerprint.
CATALOG_SCHEMAS = ("pg_catalog", "information_schema", "pg_toast")

# Additional namespace patterns excluded from fingerprints (session-local).
EXCLUDED_NAMESPACE_PREFIXES = ("pg_temp_", "pg_toast_temp_")

# Objects the fingerprints must not see: ledger and history tables differ
# between a fresh install and an adopted legacy database by design.
FINGERPRINT_EXCLUDED_TABLES = frozenset(
    {
        ("public", BASELINES_TABLE),
        ("public", CHANGES_TABLE),
        ("public", ATTEMPTS_TABLE),
        ("public", "schema_migrations"),
        ("public", "schema_migrations_meta"),
    }
)

# Environment
ENV_PRODUCTION = "production"
