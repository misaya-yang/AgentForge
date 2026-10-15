"""Single migration authority for AI Gateway PostgreSQL schema.

All schema writers (make migrate, database/cli.py, migrate_per_service.py,
scripts/new/migrate.sh, Gateway AUTO_INIT, the Compose migrate job) funnel
through this package and its ledger tables.  See docs/plans/
platform-architecture-convergence-prd-2026-08.md §ARC-03.
"""

from .adoption import (
    LedgerState,
    adopt_baseline,
    detect_legacy_state,
)
from .bootstrap import (
    bootstrap_roles,
    database_empty,
    fresh_install,
    preflight_database_empty,
    provision_roles_admin,
    role_bootstrap_issues,
    startup_schema_check,
)
from .commands import (
    MigrationCommandResult,
    command_init_fresh,
    command_migrate,
    command_prepare_cutover_ownership,
    command_source_kind,
    command_startup_check,
    command_status,
    command_verify,
    default_paths,
    load_baseline,
)
from .constants import (
    ATTEMPTS_TABLE,
    BASELINES_TABLE,
    CHANGES_TABLE,
    DEFAULT_BASELINE_ID,
    DEFAULT_ROLE_PREFIX,
    MIGRATION_ADVISORY_LOCK_ID,
    MIGRATION_ADVISORY_LOCK_NAMESPACE,
)
from .discovery import (
    DiscoveryError,
    discover_legacy_migrations,
    validate_legacy_chain,
)
from .legacy import (
    apply_legacy_chain,
    apply_per_service_chain,
    base_schema_present,
    ensure_base_schema,
    per_service_ledger_present,
)
from .manifest import (
    AuthorityManifestError,
    BaselineManifest,
    ChangeSpec,
    EpochManifest,
    RollbackClass,
    TransactionMode,
    load_baseline_manifest,
    load_epoch_manifest,
)
from .numeric_reconciliation import (
    NumericReconciliationBlocked,
    NumericReconciliationReceipt,
    reconcile_numeric_legacy_history,
)
from .runner import (
    RUNNER_DIGEST,
    AuthorityBlockedError,
    AuthorityError,
    AuthorityPaths,
    MigrationAuthority,
)

# Historical import name; the canonical receipt now lives in the dedicated
# fail-closed reconciliation module.
ReconciliationReceipt = NumericReconciliationReceipt

__all__ = [
    "ATTEMPTS_TABLE",
    "AuthorityBlockedError",
    "AuthorityError",
    "AuthorityManifestError",
    "AuthorityPaths",
    "BASELINES_TABLE",
    "BaselineManifest",
    "CHANGES_TABLE",
    "ChangeSpec",
    "DEFAULT_BASELINE_ID",
    "DEFAULT_ROLE_PREFIX",
    "DiscoveryError",
    "EpochManifest",
    "LedgerState",
    "apply_legacy_chain",
    "apply_per_service_chain",
    "base_schema_present",
    "bootstrap_roles",
    "database_empty",
    "ensure_base_schema",
    "per_service_ledger_present",
    "MIGRATION_ADVISORY_LOCK_ID",
    "MIGRATION_ADVISORY_LOCK_NAMESPACE",
    "MigrationAuthority",
    "MigrationCommandResult",
    "NumericReconciliationBlocked",
    "NumericReconciliationReceipt",
    "RUNNER_DIGEST",
    "ReconciliationReceipt",
    "RollbackClass",
    "TransactionMode",
    "adopt_baseline",
    "command_init_fresh",
    "command_migrate",
    "command_prepare_cutover_ownership",
    "command_source_kind",
    "command_startup_check",
    "command_status",
    "command_verify",
    "default_paths",
    "detect_legacy_state",
    "discover_legacy_migrations",
    "fresh_install",
    "load_baseline",
    "load_baseline_manifest",
    "load_epoch_manifest",
    "preflight_database_empty",
    "provision_roles_admin",
    "reconcile_numeric_legacy_history",
    "role_bootstrap_issues",
    "startup_schema_check",
    "validate_legacy_chain",
]
