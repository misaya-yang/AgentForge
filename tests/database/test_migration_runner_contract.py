from __future__ import annotations

from pathlib import Path

import pytest

from database import cli


def test_discovery_never_collects_rollback_files(tmp_path: Path) -> None:
    forward = tmp_path / "100_forward.sql"
    rollback = tmp_path / "101_forward_rollback.sql"
    forward.write_text("SELECT 1;\n")
    rollback.write_text("SELECT 2;\n")

    migrations = cli.discover_migrations(tmp_path)

    assert [path.name for _version, _description, path in migrations] == [forward.name]


def test_new_duplicate_numeric_versions_fail_before_database_access(
    tmp_path: Path,
) -> None:
    first = tmp_path / "100_first.sql"
    second = tmp_path / "100_second.sql"
    first.write_text("SELECT 1;\n")
    second.write_text("SELECT 2;\n")
    migrations = cli.discover_migrations(tmp_path)

    with pytest.raises(cli.MigrationChainError, match="duplicate migration version 100"):
        cli.validate_migration_chain(
            migrations,
            allow_historical_filename_duplicates=True,
        )


def test_repository_chain_only_grandfathers_exact_filename_ledger_duplicates() -> None:
    migrations = cli.discover_migrations()

    cli.validate_migration_chain(
        migrations,
        allow_historical_filename_duplicates=True,
    )
    with pytest.raises(cli.MigrationChainError, match="duplicate migration version 016"):
        cli.validate_migration_chain(
            migrations,
            allow_historical_filename_duplicates=False,
        )


def test_compatibility_entrypoints_have_no_independent_ledger_writer() -> None:
    python_runner = Path("database/cli.py").read_text(encoding="utf-8")
    per_service_runner = Path("database/migrate_per_service.py").read_text(encoding="utf-8")
    shell_runner = Path("scripts/new/migrate.sh").read_text(encoding="utf-8")

    for source in (python_runner, per_service_runner, shell_runner):
        assert "INSERT INTO public.schema_migrations" not in source
        assert "CREATE TABLE IF NOT EXISTS public.schema_migrations" not in source
        assert "asyncpg.connect" not in source
    assert "command_migrate" in python_runner
    assert "command_migrate" in per_service_runner
    assert "python -m database.authority" in shell_runner


def test_100_to_110_and_both_canonical_runners_are_knowledge_first() -> None:
    expected_local_path = "SET LOCAL search_path = knowledge, gateway, assistant, public;"
    migrations = [
        path
        for path in sorted(Path("database/migrations").glob("1??_*.sql"))
        if not path.name.endswith("_rollback.sql") and 100 <= int(path.name[:3]) <= 110
    ]
    versions = [path.name[:3] for path in migrations]
    assert len(versions) == len(set(versions))
    assert {"100", "101", "102", "103", "104", "105", "106", "107", "109", "110"} <= set(versions)
    for path in migrations:
        source = path.read_text(encoding="utf-8")
        assert "CREATE SCHEMA IF NOT EXISTS knowledge;" in source, path.name
        assert expected_local_path in source, path.name
        statements = [line.strip() for line in source.splitlines()]
        assert statements.count("BEGIN;") == 1, path.name
        assert statements.count("COMMIT;") == 1, path.name

    for wrapper in (
        Path("database/cli.py"),
        Path("database/migrate_per_service.py"),
        Path("scripts/new/migrate.sh"),
    ):
        source = wrapper.read_text(encoding="utf-8")
        assert "SET search_path TO knowledge, gateway, assistant, public" not in source
        assert 'echo "knowledge,gateway,assistant,public"' not in source


def test_retired_single_file_runner_has_no_database_execution_surface() -> None:
    legacy_runner = Path("database/run_migration.py").read_text(encoding="utf-8")

    assert "retired and cannot execute SQL" in legacy_runner
    assert "import asyncpg" not in legacy_runner
    assert "asyncpg.connect" not in legacy_runner
    assert ".execute(" not in legacy_runner


def test_only_authority_owns_the_migration_lock() -> None:
    python_runner = Path("database/cli.py").read_text(encoding="utf-8")
    per_service_runner = Path("database/migrate_per_service.py").read_text(encoding="utf-8")
    shell_runner = Path("scripts/new/migrate.sh").read_text(encoding="utf-8")
    authority_runner = Path("database/authority/runner.py").read_text(encoding="utf-8")

    assert "MIGRATION_ADVISORY_LOCK_NAMESPACE" in authority_runner
    assert "MIGRATION_ADVISORY_LOCK_ID" in authority_runner
    assert "SELECT pg_advisory_lock" in authority_runner
    assert "SELECT pg_advisory_unlock" in authority_runner
    for wrapper in (python_runner, per_service_runner, shell_runner):
        assert "SELECT pg_advisory_lock" not in wrapper
        assert "SELECT pg_advisory_unlock" not in wrapper


def test_102_and_103_are_single_transaction_migrations() -> None:
    migration_102 = Path(
        "database/migrations/102_kb_embedding_versioning_blue_green.sql"
    ).read_text(encoding="utf-8")
    migration_103 = Path("database/migrations/103_kb_process_rule_snapshot.sql").read_text(
        encoding="utf-8"
    )

    for source in (migration_102, migration_103):
        statements = [line.strip() for line in source.splitlines()]
        assert statements.count("BEGIN;") == 1
        assert statements.count("COMMIT;") == 1
        assert statements.index("BEGIN;") < statements.index("COMMIT;")

    assert migration_102.index("INSERT INTO dataset_collection_bindings") < migration_102.index(
        "COMMIT;"
    )


def test_cross_dataset_and_tenant_constraints_are_validated() -> None:
    migration_101 = Path("database/migrations/101_kb_ingestion_lifecycle.sql").read_text(
        encoding="utf-8"
    )
    migration_102 = Path(
        "database/migrations/102_kb_embedding_versioning_blue_green.sql"
    ).read_text(encoding="utf-8")
    migration_105 = Path("database/migrations/105_kb_bm25_v2_lifecycle.sql").read_text(
        encoding="utf-8"
    )
    migration_106 = Path("database/migrations/106_kb_segment_attachment_bindings.sql").read_text(
        encoding="utf-8"
    )
    migration_107 = Path("database/migrations/107_kb_parsing_ir.sql").read_text(encoding="utf-8")

    for constraint in (
        "fk_documents_process_rule_dataset",
        "fk_pipeline_execution_document_dataset",
        "fk_pipeline_execution_rule_dataset",
    ):
        assert f"VALIDATE CONSTRAINT {constraint}" in migration_101
    for constraint in (
        "fk_kb_binding_dataset_tenant",
        "fk_kb_embedding_source_dataset",
        "fk_kb_embedding_target_dataset",
    ):
        assert f"VALIDATE CONSTRAINT {constraint}" in migration_102
    assert "VALIDATE CONSTRAINT fk_kb_bm25_lifecycle_dataset_tenant" in migration_105
    for constraint in (
        "fk_kb_segment_attachment_dataset_tenant",
        "fk_kb_segment_attachment_document_dataset",
        "fk_kb_segment_attachment_segment_document_dataset",
    ):
        assert constraint in migration_106
    for constraint in (
        "fk_kb_parsing_ir_dataset_tenant",
        "fk_kb_parsing_ir_document_dataset",
        "fk_kb_parsing_page_cache_dataset_tenant",
        "fk_kb_parsing_page_cache_document_dataset",
    ):
        assert constraint in migration_107
