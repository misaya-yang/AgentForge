from __future__ import annotations

import shutil
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

import pytest

from database.authority.legacy import apply_per_service_chain
from database.authority.manifest import AuthorityManifestError, RollbackClass, TransactionMode
from database.authority.per_service_manifest import (
    HISTORICAL_MARKER_NOTES,
    PER_SERVICE_MANIFEST_NAME,
    load_per_service_manifest,
)
from database.authority.runner import AuthorityBlockedError, AuthorityPaths

ROOT = Path(__file__).resolve().parents[2]
PER_SERVICE_ROOT = ROOT / "database/migrations/per_service"


def test_checked_in_per_service_manifest_is_complete_and_conservative() -> None:
    manifest = load_per_service_manifest(PER_SERVICE_ROOT / PER_SERVICE_MANIFEST_NAME)

    assert len(manifest.changes) == 8
    assert all(
        change.transaction_mode is TransactionMode.TRANSACTIONAL for change in manifest.changes
    )
    assert all(
        change.rollback_class is RollbackClass.RESTORE_REQUIRED for change in manifest.changes
    )
    assert manifest.changes[0].historical_markers == ("phase6_schemas_created",)
    assert manifest.changes[1].historical_markers == ("phase6_tables_moved",)


def test_per_service_manifest_rejects_extra_sql_and_checksum_drift(tmp_path: Path) -> None:
    root = tmp_path / "per_service"
    shutil.copytree(PER_SERVICE_ROOT, root)
    manifest_path = root / PER_SERVICE_MANIFEST_NAME
    (root / "assistant/999_unreviewed.sql").write_text("SELECT 1;\n", encoding="utf-8")

    with pytest.raises(AuthorityManifestError, match="SQL coverage mismatch"):
        load_per_service_manifest(manifest_path)

    (root / "assistant/999_unreviewed.sql").unlink()
    target = root / "knowledge/001_dataset_collection_identity.sql"
    target.write_text(target.read_text(encoding="utf-8") + "-- drift\n", encoding="utf-8")
    with pytest.raises(AuthorityManifestError, match="checksum drift"):
        load_per_service_manifest(manifest_path)


class PerServiceConnection:
    def __init__(self, applied: set[str], *, notes: dict[str, str] | None = None) -> None:
        self.applied = applied
        self.notes = notes or {}
        self.depth = 0
        self.executed: list[tuple[str, tuple[Any, ...], int]] = []

    async def fetchval(self, _query: str, *_args: Any) -> bool:
        return True

    async def fetch(self, _query: str, *_args: Any) -> list[dict[str, str]]:
        return [{"name": name, "notes": self.notes.get(name, "")} for name in sorted(self.applied)]

    async def execute(self, query: str, *args: Any) -> None:
        self.executed.append((query, args, self.depth))

    @asynccontextmanager
    async def transaction(self):
        self.depth += 1
        try:
            yield
        finally:
            self.depth -= 1


async def test_per_service_historical_phase_markers_are_atomic_adoption_evidence() -> None:
    manifest = load_per_service_manifest(PER_SERVICE_ROOT / PER_SERVICE_MANIFEST_NAME)
    applied = {
        "phase6_schemas_created",
        "phase6_tables_moved",
        *(change.key for change in manifest.changes[2:]),
    }
    notes = {
        **HISTORICAL_MARKER_NOTES,
        **{
            change.key: f"file=migrations/per_service/{change.file}"
            for change in manifest.changes[2:]
        },
    }
    conn = PerServiceConnection(applied, notes=notes)

    count = await apply_per_service_chain(
        conn,
        AuthorityPaths(ROOT / "database"),
        log=lambda _message: None,
    )

    assert count == 0
    assert conn.executed == []


async def test_per_service_sql_and_checksum_receipt_commit_atomically() -> None:
    manifest = load_per_service_manifest(PER_SERVICE_ROOT / PER_SERVICE_MANIFEST_NAME)
    missing = manifest.changes[-1]
    prior = manifest.changes[:-1]
    conn = PerServiceConnection(
        {change.key for change in prior},
        notes={
            change.key: (
                f"file=migrations/per_service/{change.file};"
                f"sha256={change.sha256};rollback={change.rollback_class.value}"
            )
            for change in prior
        },
    )

    count = await apply_per_service_chain(
        conn,
        AuthorityPaths(ROOT / "database"),
        log=lambda _message: None,
    )

    assert count == 1
    assert len(conn.executed) == 2
    assert all(depth == 1 for _query, _args, depth in conn.executed)
    ledger_query, ledger_args, _depth = conn.executed[-1]
    assert "INSERT INTO public.schema_migrations_meta" in ledger_query
    assert ledger_args[0] == missing.key
    assert f"sha256={missing.sha256}" in ledger_args[1]
    assert "rollback=restore-required" in ledger_args[1]


async def test_per_service_unknown_global_bootstrap_and_checksum_drift_block() -> None:
    manifest = load_per_service_manifest(PER_SERVICE_ROOT / PER_SERVICE_MANIFEST_NAME)
    paths = AuthorityPaths(ROOT / "database")
    unknown = PerServiceConnection(set())

    with pytest.raises(AuthorityBlockedError, match="PUBLIC CREATE"):
        await apply_per_service_chain(unknown, paths, log=lambda _message: None)
    assert unknown.executed == []

    first = manifest.changes[0]
    drifted = PerServiceConnection(
        {change.key for change in manifest.changes},
        notes={
            change.key: (
                f"file=migrations/per_service/{change.file};"
                f"sha256={'0' * 64 if change is first else change.sha256};"
                f"rollback={change.rollback_class.value}"
            )
            for change in manifest.changes
        },
    )
    with pytest.raises(AuthorityBlockedError, match="no exact historical file"):
        await apply_per_service_chain(drifted, paths, log=lambda _message: None)
    assert drifted.executed == []


@pytest.mark.parametrize(
    ("applied", "notes", "message"),
    [
        ({"_global:001_create_schemas.sql"}, {}, "no exact historical file"),
        (
            {"phase6_schemas_created"},
            {"phase6_schemas_created": "forged"},
            "historical marker",
        ),
    ],
)
async def test_per_service_recorded_state_requires_an_exact_historical_receipt(
    applied: set[str],
    notes: dict[str, str],
    message: str,
) -> None:
    conn = PerServiceConnection(applied, notes=notes)

    with pytest.raises(AuthorityBlockedError, match=message):
        await apply_per_service_chain(
            conn,
            AuthorityPaths(ROOT / "database"),
            log=lambda _message: None,
        )

    assert conn.executed == []
