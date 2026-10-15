from __future__ import annotations

import hashlib
import json
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock

import pytest
import yaml

from database.authority import commands
from database.authority.adoption import LedgerState
from database.authority.commands import MigrationCommandResult, _write_migration_evidence
from database.authority.legacy import (
    _strip_outer_transaction,
    reconcile_numeric_legacy_history,
    run_one_legacy,
)
from database.authority.manifest import (
    AuthorityManifestError,
    LegacyChangeSpec,
    RollbackClass,
    TransactionMode,
    load_epoch_manifest,
    load_legacy_manifest,
)
from database.authority.numeric_reconciliation import (
    NUMERIC_EVIDENCE_SQL,
    NumericReconciliationBlocked,
    NumericReconciliationReceipt,
)
from database.authority.numeric_reconciliation import (
    reconcile_numeric_legacy_history as canonical_reconcile_numeric_legacy_history,
)
from database.authority.runner import AuthorityError, AuthorityPaths

ROOT = Path(__file__).resolve().parents[2]
EPOCH_MANIFEST = ROOT / "database/migrations/2026_08_post_kb_v1/manifest.yml"
LEGACY_MANIFEST = ROOT / "database/migrations/legacy-manifest.yml"


def _sha(content: str) -> str:
    return hashlib.sha256(content.encode("utf-8")).hexdigest()


def _write_epoch(directory: Path, *, baseline_id: str | None = None) -> Path:
    directory.mkdir(parents=True)
    sql = "SELECT 1;\n"
    (directory / "001_probe.sql").write_text(sql, encoding="utf-8")
    manifest = directory / "manifest.yml"
    manifest.write_text(
        yaml.safe_dump(
            {
                "baseline_id": baseline_id or directory.name,
                "epoch": 1,
                "changes": [
                    {
                        "sequence": 1,
                        "name": "probe",
                        "file": "001_probe.sql",
                        "sha256": _sha(sql),
                        "owner": "owner",
                        "transaction_mode": "transactional",
                        "rollback_class": "forward-fix-only",
                        "preconditions": ["SELECT TRUE"],
                        "postconditions": ["SELECT TRUE"],
                        "timeout_seconds": 300,
                        "lock_budget_seconds": 30,
                        "resume_handler": None,
                        "repair_handler": None,
                    }
                ],
            },
            sort_keys=False,
        ),
        encoding="utf-8",
    )
    return manifest


def _write_minimal_legacy(directory: Path) -> Path:
    directory.mkdir(parents=True)
    sql = "CREATE INDEX CONCURRENTLY probe_idx ON probe_table (id);\n"
    filename = "049_session_list_performance.sql"
    (directory / filename).write_text(sql, encoding="utf-8")
    manifest = directory / "legacy-manifest.yml"
    manifest.write_text(
        yaml.safe_dump(
            {
                "schema": "migration-authority/legacy-manifest/v1",
                "freeze_point": filename,
                "changes": [
                    {
                        "file": filename,
                        "sha256": _sha(sql),
                        "transaction_mode": "non_transactional",
                        "rollback_class": "old-binary-compatible",
                    }
                ],
                "historical_rollbacks": [],
            },
            sort_keys=False,
        ),
        encoding="utf-8",
    )
    return manifest


class FakeNumericConnection:
    def __init__(
        self,
        rows: list[dict[str, Any]],
        *,
        evidence: dict[str, bool] | None = None,
        columns: set[str] | None = None,
    ) -> None:
        self.rows = rows
        self.evidence = evidence or {}
        self.columns = columns or {"version", "name", "checksum"}

    async def fetch(self, query: str, *_args: Any) -> list[dict[str, Any]]:
        if "information_schema.columns" in query:
            return [{"column_name": column} for column in sorted(self.columns)]
        if "version::text AS version" in query:
            return self.rows
        if "SELECT version FROM public.schema_migrations" in query:
            return [{"version": row["version"]} for row in self.rows]
        raise AssertionError(f"unexpected query: {query}")

    async def fetchval(self, query: str, *_args: Any) -> bool:
        marker = next(
            (
                key
                for key in self.evidence
                if f"arc03-legacy-evidence:{key.replace('_', '-')}" in query
            ),
            None,
        )
        if marker is None:
            # Query markers use 031-hierarchy-effective rather than replacing
            # every underscore mechanically.
            for key in self.evidence:
                if key == "031_hierarchy_effective" and "031-hierarchy-effective" in query:
                    marker = key
                    break
        if marker is None:
            raise AssertionError(f"unexpected evidence query: {query}")
        return self.evidence[marker]


def _ledger_row(
    version: int,
    *,
    name: str | None = None,
    checksum: str | None = None,
    dirty: bool = False,
) -> dict[str, Any]:
    return {"version": str(version), "name": name, "checksum": checksum, "dirty": dirty}


def _proven_031_row(manifest: Any) -> dict[str, Any]:
    hierarchy = manifest.by_file()["031_hierarchical_segments.sql"]
    return _ledger_row(31, name=hierarchy.file, checksum=hierarchy.sha256)


def test_checked_in_epoch_and_legacy_manifests_are_valid_and_complete() -> None:
    epoch = load_epoch_manifest(EPOCH_MANIFEST)
    legacy = load_legacy_manifest(LEGACY_MANIFEST)

    assert epoch.baseline_id == "2026_08_post_kb_v1"
    assert [(c.sequence, c.name) for c in epoch.changes] == [
        (1, "knowledge_tenant_permissions"),
        (2, "gateway_eval_leases"),
        (3, "assistant_quiz_attempt_idempotency"),
        (4, "runtime_durable_recovery"),
        (5, "knowledge_dataset_archive"),
        (6, "assistant_internal_share_scope"),
    ]
    assert len(legacy.changes) == 108
    assert legacy.freeze_point == "112_kb_document_progress_retention.sql"
    assert legacy.by_file()["049_session_list_performance.sql"].transaction_mode is (
        TransactionMode.NON_TRANSACTIONAL
    )
    assert legacy.by_file()["101_kb_ingestion_lifecycle.sql"].rollback_class is (
        RollbackClass.RESTORE_REQUIRED
    )
    assert legacy.rollback_by_file() == {
        "030_fix_timestamp_and_security_constraint_rollback.sql": legacy.historical_rollbacks[0]
    }


def test_legacy_keeps_the_numeric_reconciliation_public_seam() -> None:
    assert reconcile_numeric_legacy_history is canonical_reconcile_numeric_legacy_history


def test_epoch_ledger_id_allows_leading_digits_but_must_match_directory(tmp_path: Path) -> None:
    manifest = _write_epoch(tmp_path / "2026_08_probe_v1")
    assert load_epoch_manifest(manifest).baseline_id == "2026_08_probe_v1"

    mismatched = _write_epoch(tmp_path / "different_directory", baseline_id="2026_08_probe_v1")
    with pytest.raises(AuthorityManifestError, match="directory"):
        load_epoch_manifest(mismatched)

    unsafe = _write_epoch(tmp_path / "unsafe", baseline_id="2026_08;drop_schema")
    with pytest.raises(AuthorityManifestError, match="ledger id"):
        load_epoch_manifest(unsafe)


def test_epoch_manifest_rejects_extra_sql_and_checksum_drift(tmp_path: Path) -> None:
    directory = tmp_path / "2026_08_probe_v1"
    manifest = _write_epoch(directory)
    (directory / "002_unreviewed.sql").write_text("SELECT 2;\n", encoding="utf-8")
    with pytest.raises(AuthorityManifestError, match="undeclared=.*002_unreviewed"):
        load_epoch_manifest(manifest)

    (directory / "002_unreviewed.sql").unlink()
    (directory / "001_probe.sql").write_text("SELECT 9;\n", encoding="utf-8")
    with pytest.raises(AuthorityManifestError, match="checksum drift"):
        load_epoch_manifest(manifest)


@pytest.mark.parametrize(
    ("patch", "match"),
    [
        ({"timeout_seconds": 0}, "positive integer"),
        ({"timeout_seconds": 10, "lock_budget_seconds": 11}, "cannot exceed"),
        (
            {"transaction_mode": "non_transactional"},
            "must declare a resume_handler or repair_handler",
        ),
        ({"transaction_mode": {"invalid": "shape"}}, "unknown transaction_mode"),
        ({"rollback_class": ["invalid"]}, "unknown rollback_class"),
        ({"resume_handler": "unexpected"}, "cannot declare recovery handlers"),
        ({"preconditions": []}, "at least one read-only SELECT"),
        ({"postconditions": ["DELETE FROM widgets RETURNING TRUE"]}, "read-only SELECT"),
        ({"typo_field": True}, "unknown fields"),
    ],
)
def test_epoch_manifest_rejects_unsafe_execution_contracts(
    tmp_path: Path, patch: dict[str, Any], match: str
) -> None:
    manifest = _write_epoch(tmp_path / "2026_08_probe_v1")
    payload = yaml.safe_load(manifest.read_text(encoding="utf-8"))
    payload["changes"][0].update(patch)
    manifest.write_text(yaml.safe_dump(payload, sort_keys=False), encoding="utf-8")

    with pytest.raises(AuthorityManifestError, match=match):
        load_epoch_manifest(manifest)


def test_legacy_manifest_rejects_extra_sql_and_checksum_drift(tmp_path: Path) -> None:
    manifest = _write_minimal_legacy(tmp_path / "migrations")
    assert load_legacy_manifest(manifest).freeze_point == "049_session_list_performance.sql"

    extra = manifest.parent / "050_unreviewed.sql"
    extra.write_text("SELECT 2;\n", encoding="utf-8")
    with pytest.raises(AuthorityManifestError, match="undeclared=.*050_unreviewed"):
        load_legacy_manifest(manifest)

    extra.unlink()
    (manifest.parent / "049_session_list_performance.sql").write_text(
        "CREATE INDEX CONCURRENTLY changed_idx ON probe_table (id);\n",
        encoding="utf-8",
    )
    with pytest.raises(AuthorityManifestError, match="checksum drift"):
        load_legacy_manifest(manifest)


async def test_numeric_016_accepts_known_historical_name_checksum_overwrite() -> None:
    manifest = load_legacy_manifest(LEGACY_MANIFEST)
    first = manifest.by_file()["016_confluence_multi_root_pages.sql"]
    last = manifest.by_file()["016_usage_hourly_aggregates.sql"]
    conn = FakeNumericConnection(
        [
            _ledger_row(
                16,
                name="Confluence Multi Root Pages",
                checksum=last.legacy_checksum,
            ),
            _proven_031_row(manifest),
        ],
        evidence={"016_effective": True, "031_hierarchy_effective": True},
    )

    receipt = await reconcile_numeric_legacy_history(conn, manifest)

    assert receipt.verdict == "proven"
    assert receipt.versions["016"]["identified_file"] == last.file
    assert receipt.versions["016"]["identity_basis"] == "historical_name_checksum_overwrite"
    assert first.legacy_checksum != last.legacy_checksum


async def test_numeric_031_bare_or_data_only_row_is_blocked_without_price_replay() -> None:
    manifest = load_legacy_manifest(LEGACY_MANIFEST)
    bare = FakeNumericConnection(
        [_ledger_row(31)],
        columns={"version"},
    )
    receipt = await reconcile_numeric_legacy_history(bare, manifest)
    assert receipt.verdict == "blocked"
    assert "bare numeric" in receipt.versions["031"]["reason"]

    data_only = manifest.by_file()["031_align_model_prices_20260211.sql"]
    identified = FakeNumericConnection(
        [_ledger_row(31, name=data_only.file, checksum=data_only.sha256)],
    )
    receipt = await reconcile_numeric_legacy_history(identified, manifest)
    assert receipt.verdict == "blocked"
    assert "prices will not be replayed" in receipt.versions["031"]["reason"]


async def test_numeric_031_absence_is_blocked_instead_of_replaying_prices() -> None:
    manifest = load_legacy_manifest(LEGACY_MANIFEST)
    receipt = await reconcile_numeric_legacy_history(FakeNumericConnection([]), manifest)

    assert receipt.verdict == "blocked"
    assert "will not be replayed" in receipt.versions["031"]["reason"]


async def test_numeric_031_last_sibling_proves_order_without_comparing_mutable_prices() -> None:
    manifest = load_legacy_manifest(LEGACY_MANIFEST)
    first = manifest.by_file()["031_align_model_prices_20260211.sql"]
    last = manifest.by_file()["031_hierarchical_segments.sql"]
    conn = FakeNumericConnection(
        [_ledger_row(31, name=_description(first.file), checksum=last.legacy_checksum)],
        evidence={"031_hierarchy_effective": True},
    )

    receipt = await reconcile_numeric_legacy_history(conn, manifest)

    assert receipt.verdict == "proven"
    assert "neither compared nor replayed" in receipt.versions["031"]["reason"]


def _description(filename: str) -> str:
    return filename[4:-4].replace("_", " ").title()


async def test_numeric_030_forward_is_proven_but_rollback_and_mixed_states_block() -> None:
    manifest = load_legacy_manifest(LEGACY_MANIFEST)
    forward = manifest.by_file()["030_fix_timestamp_and_security_constraint.sql"]
    rollback = manifest.rollback_by_file()["030_fix_timestamp_and_security_constraint_rollback.sql"]

    proven = FakeNumericConnection(
        [
            _ledger_row(30, name=forward.file, checksum=forward.sha256),
            _proven_031_row(manifest),
        ],
        evidence={
            "030_forward": True,
            "030_rollback": False,
            "031_hierarchy_effective": True,
        },
    )
    assert (await reconcile_numeric_legacy_history(proven, manifest)).verdict == "proven"

    historical_rollback = FakeNumericConnection(
        [
            _ledger_row(30, name=_description(forward.file), checksum=rollback.legacy_checksum),
            _proven_031_row(manifest),
        ],
        evidence={
            "030_forward": False,
            "030_rollback": True,
            "031_hierarchy_effective": True,
        },
    )
    receipt = await reconcile_numeric_legacy_history(historical_rollback, manifest)
    assert receipt.verdict == "blocked"
    assert "historical rollback" in receipt.versions["030"]["reason"]

    mixed = FakeNumericConnection(
        [
            _ledger_row(30, name=forward.file, checksum=forward.legacy_checksum),
            _proven_031_row(manifest),
        ],
        evidence={
            "030_forward": True,
            "030_rollback": True,
            "031_hierarchy_effective": True,
        },
    )
    receipt = await reconcile_numeric_legacy_history(mixed, manifest)
    assert receipt.verdict == "blocked"
    assert "mixed" in receipt.versions["030"]["reason"]


@pytest.mark.parametrize(
    ("row", "reason"),
    [
        (_ledger_row(50, dirty=True), "dirty"),
        (_ledger_row(50, checksum="0" * 16), "does not match immutable"),
        (_ledger_row(50, name="Unrelated Migration"), "does not identify"),
        (_ledger_row(999), "no unique immutable forward file"),
    ],
)
async def test_numeric_nonduplicate_rows_fail_closed_on_untrusted_history(
    row: dict[str, Any],
    reason: str,
) -> None:
    manifest = load_legacy_manifest(LEGACY_MANIFEST)
    conn = FakeNumericConnection(
        [row, _proven_031_row(manifest)],
        evidence={"031_hierarchy_effective": True},
    )

    receipt = await reconcile_numeric_legacy_history(conn, manifest)

    version = f"{int(row['version']):03d}"
    assert receipt.verdict == "blocked"
    assert reason in receipt.versions[version]["reason"]


async def test_numeric_unique_bare_and_historical_alias_rows_remain_supported() -> None:
    manifest = load_legacy_manifest(LEGACY_MANIFEST)
    renamed = manifest.by_file()["089_agent_runtime_thread_store.sql"]
    conn = FakeNumericConnection(
        [
            _ledger_row(50),
            _ledger_row(
                89,
                name="Codex Runtime Thread Store",
                checksum=renamed.legacy_checksum,
            ),
            _proven_031_row(manifest),
        ],
        evidence={"031_hierarchy_effective": True},
    )

    receipt = await reconcile_numeric_legacy_history(conn, manifest)

    assert receipt.verdict == "proven"
    assert receipt.versions["050"]["identity_basis"] == "unique_version_only"
    assert receipt.versions["089"]["identified_file"] == renamed.file


class FakeExecutionConnection:
    def __init__(
        self,
        *,
        index_states: list[list[dict[str, Any]]] | None = None,
        postcheck: bool = True,
        fail_ledger_once: bool = False,
    ) -> None:
        self.executed: list[str] = []
        self.transaction_entries = 0
        self.in_transaction = False
        self.transactional_executes: list[str] = []
        self.index_states = index_states or [[]]
        self.postcheck = postcheck
        self.fail_ledger_once = fail_ledger_once

    async def execute(self, query: str, *_args: Any) -> None:
        self.executed.append(query)
        if self.in_transaction:
            self.transactional_executes.append(query)
        if "INSERT INTO public.schema_migrations" in query and self.fail_ledger_once:
            self.fail_ledger_once = False
            raise RuntimeError("simulated crash before ledger commit")

    async def fetch(self, query: str, *_args: Any) -> list[dict[str, Any]]:
        if "arc03-legacy-049:index-state" in query:
            if len(self.index_states) > 1:
                return self.index_states.pop(0)
            return self.index_states[0]
        if "information_schema.columns" in query:
            return [{"column_name": "filename"}]
        raise AssertionError(f"unexpected query: {query}")

    async def fetchval(self, query: str, *_args: Any) -> bool:
        if "arc03-legacy-049:postcheck" in query:
            return self.postcheck
        raise AssertionError(f"unexpected query: {query}")

    @asynccontextmanager
    async def transaction(self):
        self.transaction_entries += 1
        assert not self.in_transaction
        self.in_transaction = True
        try:
            yield
        finally:
            self.in_transaction = False


async def test_049_executes_without_runner_transaction(tmp_path: Path) -> None:
    content = (
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_sessions_user_tenant_status_updated "
        "ON sessions (user_id, tenant_id, status, updated_at DESC);\n"
    )
    path = tmp_path / "049_session_list_performance.sql"
    path.write_text(content, encoding="utf-8")
    migration = type("Migration", (), {"path": path})()
    spec = LegacyChangeSpec(
        file=path.name,
        sha256=_sha(content),
        transaction_mode=TransactionMode.NON_TRANSACTIONAL,
        rollback_class=RollbackClass.OLD_BINARY_COMPATIBLE,
    )
    conn = FakeExecutionConnection()

    await run_one_legacy(conn, "filename", migration, spec, log=lambda _message: None)

    assert conn.transaction_entries == 0
    assert conn.executed[0] == content
    assert "INSERT INTO public.schema_migrations" in conn.executed[-1]


async def test_transactional_legacy_strips_only_outer_control_and_records_atomically(
    tmp_path: Path,
) -> None:
    content = "-- header\nBEGIN;\nDO $$ BEGIN PERFORM 1; END $$;\nSELECT 1;\nCOMMIT;\n"
    path = tmp_path / "050_probe.sql"
    path.write_text(content, encoding="utf-8")
    migration = type("Migration", (), {"path": path, "version": "050"})()
    spec = LegacyChangeSpec(
        file=path.name,
        sha256=_sha(content),
        transaction_mode=TransactionMode.TRANSACTIONAL,
        rollback_class=RollbackClass.FORWARD_FIX_ONLY,
    )
    conn = FakeExecutionConnection()

    await run_one_legacy(conn, "filename", migration, spec, log=lambda _message: None)

    assert conn.transaction_entries == 1
    assert len(conn.transactional_executes) == 2
    assert "DO $$ BEGIN PERFORM 1; END $$" in conn.transactional_executes[0]
    assert "\nBEGIN;" not in conn.transactional_executes[0]
    assert "COMMIT;" not in conn.transactional_executes[0]
    assert "INSERT INTO public.schema_migrations" in conn.transactional_executes[1]


@pytest.mark.parametrize(
    "content",
    [
        "BEGIN; SELECT 1;",
        "SELECT 1; COMMIT;",
        "BEGIN; BEGIN; SELECT 1; COMMIT; COMMIT;",
        "BEGIN; SELECT 1; ROLLBACK;",
    ],
)
def test_transaction_control_rejects_unpaired_nested_or_rollback(content: str) -> None:
    with pytest.raises(AuthorityError, match="one paired outer"):
        _strip_outer_transaction(content)


def test_all_manifest_legacy_sql_has_safe_transaction_control() -> None:
    manifest = load_legacy_manifest(LEGACY_MANIFEST)
    migrations_root = LEGACY_MANIFEST.parent
    outer_transaction_files = []

    for spec in manifest.changes:
        _inner_sql, has_outer = _strip_outer_transaction(
            (migrations_root / spec.file).read_text(encoding="utf-8")
        )
        if has_outer:
            outer_transaction_files.append(spec.file)

    assert len(outer_transaction_files) == 53
    assert "031_align_model_prices_20260211.sql" in outer_transaction_files
    assert "112_kb_document_progress_retention.sql" in outer_transaction_files
    assert "049_session_list_performance.sql" not in outer_transaction_files


async def test_049_invalid_crash_artifact_is_removed_before_retry(tmp_path: Path) -> None:
    content = (
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_sessions_user_tenant_status_updated "
        "ON sessions (user_id, tenant_id, status, updated_at DESC);\n"
    )
    path = tmp_path / "049_session_list_performance.sql"
    path.write_text(content, encoding="utf-8")
    migration = type("Migration", (), {"path": path, "version": "049"})()
    spec = LegacyChangeSpec(
        file=path.name,
        sha256=_sha(content),
        transaction_mode=TransactionMode.NON_TRANSACTIONAL,
        rollback_class=RollbackClass.OLD_BINARY_COMPATIBLE,
    )
    conn = FakeExecutionConnection(
        index_states=[
            [
                {
                    "schema_name": "public",
                    "indisvalid": False,
                    "indisready": False,
                    "definition": "invalid",
                }
            ]
        ]
    )

    await run_one_legacy(conn, "filename", migration, spec, log=lambda _message: None)

    assert conn.executed[0] == (
        'DROP INDEX CONCURRENTLY IF EXISTS "public"."idx_sessions_user_tenant_status_updated"'
    )
    assert conn.executed[1] == content
    assert "INSERT INTO public.schema_migrations" in conn.executed[2]


async def test_049_postcondition_precedes_ledger_and_crash_retry_is_idempotent(
    tmp_path: Path,
) -> None:
    content = (
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_sessions_user_tenant_status_updated "
        "ON sessions (user_id, tenant_id, status, updated_at DESC);\n"
    )
    path = tmp_path / "049_session_list_performance.sql"
    path.write_text(content, encoding="utf-8")
    migration = type("Migration", (), {"path": path, "version": "049"})()
    spec = LegacyChangeSpec(
        file=path.name,
        sha256=_sha(content),
        transaction_mode=TransactionMode.NON_TRANSACTIONAL,
        rollback_class=RollbackClass.OLD_BINARY_COMPATIBLE,
    )
    valid_index = {
        "schema_name": "public",
        "indisvalid": True,
        "indisready": True,
        "definition": "valid",
    }
    conn = FakeExecutionConnection(
        index_states=[[], [valid_index]],
        fail_ledger_once=True,
    )

    with pytest.raises(RuntimeError, match="simulated crash"):
        await run_one_legacy(conn, "filename", migration, spec, log=lambda _message: None)
    await run_one_legacy(conn, "filename", migration, spec, log=lambda _message: None)

    assert conn.executed.count(content) == 2
    assert sum("INSERT INTO public.schema_migrations" in sql for sql in conn.executed) == 2


async def test_049_failed_postcondition_never_records_ledger(tmp_path: Path) -> None:
    content = (
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS idx_sessions_user_tenant_status_updated "
        "ON sessions (user_id, tenant_id, status, updated_at DESC);\n"
    )
    path = tmp_path / "049_session_list_performance.sql"
    path.write_text(content, encoding="utf-8")
    migration = type("Migration", (), {"path": path, "version": "049"})()
    spec = LegacyChangeSpec(
        file=path.name,
        sha256=_sha(content),
        transaction_mode=TransactionMode.NON_TRANSACTIONAL,
        rollback_class=RollbackClass.OLD_BINARY_COMPATIBLE,
    )
    conn = FakeExecutionConnection(postcheck=False)

    with pytest.raises(AuthorityError, match="postcondition failed"):
        await run_one_legacy(conn, "filename", migration, spec, log=lambda _message: None)

    assert not any("INSERT INTO public.schema_migrations" in sql for sql in conn.executed)


async def test_049_rejects_non_idempotent_sql_before_execution(tmp_path: Path) -> None:
    content = (
        "CREATE INDEX CONCURRENTLY idx_sessions_user_tenant_status_updated "
        "ON sessions (user_id, tenant_id, status, updated_at DESC);\n"
    )
    path = tmp_path / "049_session_list_performance.sql"
    path.write_text(content, encoding="utf-8")
    migration = type("Migration", (), {"path": path, "version": "049"})()
    spec = LegacyChangeSpec(
        file=path.name,
        sha256=_sha(content),
        transaction_mode=TransactionMode.NON_TRANSACTIONAL,
        rollback_class=RollbackClass.OLD_BINARY_COMPATIBLE,
    )
    conn = FakeExecutionConnection()

    with pytest.raises(AuthorityError, match="idempotent"):
        await run_one_legacy(conn, "filename", migration, spec, log=lambda _message: None)

    assert conn.executed == []


def test_numeric_evidence_queries_bind_complete_objects_not_names_only() -> None:
    evidence_016 = NUMERIC_EVIDENCE_SQL["016_effective"]
    evidence_030 = NUMERIC_EVIDENCE_SQL["030_forward"]
    evidence_031 = NUMERIC_EVIDENCE_SQL["031_hierarchy_effective"]

    for required in (
        "root_page_titles",
        "image_max_size_bytes",
        "uq_usage_hourly_aggregates_dimensions",
        "idx_usage_hourly_tenant_date",
        "column_default",
    ):
        assert required in evidence_016
    for required in (
        "count(*) = 7",
        "attnotnull",
        "column_default",
        "uq_security_event_daily_dimensions",
        "idx_security_event_daily_unique",
    ):
        assert required in evidence_030
    for required in (
        "count(*) = 7 FROM actual_columns",
        "confrelid",
        "proowner",
        "prosecdef",
        "search_path=public",
        "tgfoid",
        "idx_document_summaries_keywords",
    ):
        assert required in evidence_031


def test_reconciliation_evidence_is_written_only_to_explicit_path(tmp_path: Path) -> None:
    receipt = type(
        "Receipt",
        (),
        {
            "as_dict": lambda _self: {"verdict": "proven"},
        },
    )()
    result = MigrationCommandResult(0, receipt)
    explicit = tmp_path / "operator-selected" / "receipt.json"

    _write_migration_evidence(result, None)
    assert list(tmp_path.rglob("*.json")) == []

    _write_migration_evidence(result, explicit)
    assert json.loads(explicit.read_text(encoding="utf-8")) == {
        "exit_code": 0,
        "numeric_reconciliation": {"verdict": "proven"},
    }


class FakeCommandConnection:
    def __init__(self) -> None:
        self.closed = False

    async def close(self) -> None:
        self.closed = True


class FakeCommandAuthority:
    def __init__(self, tmp_path: Path) -> None:
        self.paths = AuthorityPaths(database_dir=tmp_path / "database")
        self.lock_connection = FakeCommandConnection()
        self.work_connection = FakeCommandConnection()
        self.connections = 0

    async def connect(self) -> FakeCommandConnection:
        self.connections += 1
        return self.lock_connection if self.connections == 1 else self.work_connection

    async def acquire_lock(self, _conn: FakeCommandConnection) -> None:
        return None

    async def release_lock(self, conn: FakeCommandConnection) -> None:
        await conn.close()

    async def adopted_baseline(self, _conn: FakeCommandConnection) -> None:
        return None


async def test_command_returns_and_explicitly_writes_proven_receipt(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    receipt = NumericReconciliationReceipt()
    authority = FakeCommandAuthority(tmp_path)
    evidence = tmp_path / "receipt.json"
    monkeypatch.setattr(commands, "baseline_ready", lambda *_args: False)

    numeric_ledger = AsyncMock(return_value=LedgerState(numeric_ledger=True))
    async def apply(_conn: Any, _paths: AuthorityPaths, *, log: Any) -> tuple[Any, ...]:
        log("authority: numeric reconciliation receipt:\n" + receipt.to_json())
        return "version", 0, receipt

    async def per_service(_conn: Any, _paths: AuthorityPaths, *, log: Any) -> int:
        return 0

    monkeypatch.setattr(commands, "_guard_known_database", AsyncMock())
    monkeypatch.setattr(commands, "detect_legacy_state", numeric_ledger)
    monkeypatch.setattr(commands.legacy, "apply_legacy_chain", apply)
    monkeypatch.setattr(commands.legacy, "apply_per_service_chain", per_service)
    messages: list[str] = []

    result = await commands.command_migrate(
        authority,
        allow_adoption=False,
        reconciliation_evidence_out=evidence,
        log=messages.append,
    )

    assert result.exit_code == 0
    assert result.reconciliation_receipt is receipt
    assert "numeric reconciliation receipt" in "\n".join(messages)
    assert (
        json.loads(evidence.read_text(encoding="utf-8"))["numeric_reconciliation"]["verdict"]
        == "proven"
    )
    assert authority.work_connection.closed
    assert authority.lock_connection.closed


async def test_command_preserves_blocked_receipt_at_explicit_output(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    receipt = NumericReconciliationReceipt(verdict="blocked")
    receipt.block("031", "bare numeric row")
    authority = FakeCommandAuthority(tmp_path)
    evidence = tmp_path / "blocked.json"
    monkeypatch.setattr(commands, "baseline_ready", lambda *_args: False)

    numeric_ledger = AsyncMock(return_value=LedgerState(numeric_ledger=True))
    async def blocked_apply(_conn: Any, _paths: AuthorityPaths, *, log: Any) -> tuple[Any, ...]:
        raise NumericReconciliationBlocked("blocked", receipt)

    monkeypatch.setattr(commands, "_guard_known_database", AsyncMock())
    monkeypatch.setattr(commands, "detect_legacy_state", numeric_ledger)
    monkeypatch.setattr(commands.legacy, "apply_legacy_chain", blocked_apply)

    with pytest.raises(NumericReconciliationBlocked):
        await commands.command_migrate(
            authority,
            allow_adoption=False,
            reconciliation_evidence_out=evidence,
            log=lambda _message: None,
        )

    payload = json.loads(evidence.read_text(encoding="utf-8"))
    assert payload["exit_code"] == 1
    assert payload["numeric_reconciliation"]["verdict"] == "blocked"


async def test_numeric_reconciliation_runs_before_duplicate_chain_validation() -> None:
    conn = FakeNumericConnection([_ledger_row(31)], columns={"version"})
    paths = AuthorityPaths(database_dir=ROOT / "database")

    from database.authority.legacy import pending_legacy_migrations
    from database.authority.runner import AuthorityBlockedError

    with pytest.raises(AuthorityBlockedError, match="numeric legacy reconciliation BLOCKED"):
        await pending_legacy_migrations(conn, paths, mode="version")
