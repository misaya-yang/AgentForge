"""Canonical-ledger smoke for the complete RAG upgrade migration chain."""

from __future__ import annotations

import os
import re
import uuid
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any
from urllib.parse import quote

import asyncpg
import pytest
import pytest_asyncio
from dotenv import dotenv_values

from database import cli
from database.authority.commands import command_migrate, default_paths
from database.authority.runner import MigrationAuthority

ROOT = Path(__file__).resolve().parents[2]
RAG_FILENAMES = {
    path.name
    for path in (ROOT / "database" / "migrations").glob("*.sql")
    if path.name.split("_", 1)[0].isdigit() and 100 <= int(path.name.split("_", 1)[0]) <= 112
    if not path.name.endswith("_rollback.sql")
}

KB_100_TO_105_TABLES = (
    "document_pipeline_executions",
    "dataset_collection_bindings",
    "embedding_migrations",
    "embedding_migration_progress",
    "embedding_vector_cache",
    "kb_eval_golden",
    "kb_eval_golden_release",
    "kb_bm25_v2_lifecycle",
)

KB_106_TO_110_TABLES = (
    "kb_segment_attachment_bindings",
    "kb_parsing_ir",
    "kb_parsing_page_cache",
    "dataset_query_feedback",
    "kb_document_batch_operations",
    "kb_document_batch_items",
    "embedding_migration_action_jobs",
)

KB_111_TABLES = ("kb_document_progress_events",)

_LEGACY_SEGMENT_IDENTITY = re.compile(
    r"UNIQUE\s*\(\s*document_id\s*,\s*position\s*\)",
    re.IGNORECASE,
)


def _schema_for_layout(layout: str) -> str:
    schema = (ROOT / "database" / "schema.sql").read_text(encoding="utf-8")
    # schema.sql is intentionally the stable pre-RAG bootstrap baseline.  All
    # three layouts must prove that 100-112 build the final shape rather than
    # succeeding because branch-final objects were selectively folded in.
    assert len(_LEGACY_SEGMENT_IDENTITY.findall(schema)) == 1
    return schema


def _postgres_config() -> dict[str, Any]:
    file_values = dotenv_values(os.environ.get("ENV_FILE") or ROOT / ".env")
    required = ("POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB", "POSTGRES_PORT")
    values = {key: os.environ.get(key) or file_values.get(key) for key in required}
    missing = [key for key, value in values.items() if not value]
    if missing:
        pytest.fail(f"local PostgreSQL test configuration missing keys: {', '.join(missing)}")
    return {
        "host": os.environ.get("POSTGRES_HOST") or file_values.get("POSTGRES_HOST") or "127.0.0.1",
        "port": int(str(values["POSTGRES_PORT"])),
        "user": str(values["POSTGRES_USER"]),
        "password": str(values["POSTGRES_PASSWORD"]),
        "database": str(values["POSTGRES_DB"]),
    }


def _dsn(config: dict[str, Any], database: str) -> str:
    user = quote(str(config["user"]), safe="")
    password = quote(str(config["password"]), safe="")
    return f"postgresql://{user}:{password}@{config['host']}:{config['port']}/{database}"


async def _migrate_legacy_chain(dsn: str) -> None:
    result = await command_migrate(
        MigrationAuthority(dsn, default_paths()),
        allow_adoption=False,
    )
    assert result.exit_code == 0


@pytest_asyncio.fixture(params=("public", "main", "split"))
async def migration_database(
    request: pytest.FixtureRequest,
) -> AsyncIterator[tuple[str, str]]:
    config = _postgres_config()
    database_name = f"kb_rag_chain_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{database_name}"')

    database_config = {**config, "database": database_name}
    conn = await asyncpg.connect(**database_config)
    try:
        layout = str(request.param)
        await conn.execute(_schema_for_layout(layout))
        if layout == "split":
            await conn.execute("CREATE SCHEMA IF NOT EXISTS knowledge")
            for table in (
                "dataset_queries",
                "dataset_process_rules",
                "segments",
                "documents",
                "datasets",
            ):
                await conn.execute(f'ALTER TABLE public."{table}" SET SCHEMA knowledge')
        await conn.execute(
            """
            CREATE TABLE public.schema_migrations (
                filename TEXT PRIMARY KEY,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            """
        )
        await conn.executemany(
            "INSERT INTO public.schema_migrations (filename) VALUES ($1)",
            [
                (path.name,)
                for version, _description, path in cli.discover_migrations()
                if int(version) < 100
            ],
        )
    finally:
        await conn.close()

    try:
        yield _dsn(config, database_name), str(request.param)
    finally:
        await admin.execute(f'DROP DATABASE "{database_name}"')
        await admin.close()


@pytest_asyncio.fixture
async def fresh_central_database() -> AsyncIterator[str]:
    config = _postgres_config()
    database_name = f"central_chain_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{database_name}"')
    conn = await asyncpg.connect(**{**config, "database": database_name})
    try:
        await conn.execute(_schema_for_layout("public"))
        await conn.execute(
            """
            CREATE TABLE public.schema_migrations (
                filename TEXT PRIMARY KEY,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            )
            """
        )
        await conn.executemany(
            "INSERT INTO public.schema_migrations (filename) VALUES ($1)",
            [
                (path.name,)
                for version, _description, path in cli.discover_migrations()
                if int(version) < 97
            ],
        )
    finally:
        await conn.close()

    try:
        yield _dsn(config, database_name)
    finally:
        await admin.execute(f'DROP DATABASE "{database_name}"')
        await admin.close()


@pytest.mark.asyncio
async def test_fresh_schema_replays_current_upgrade_forward_chain(
    fresh_central_database: str,
) -> None:
    """The current upgrade chain tolerates optional per-service tables being absent."""
    await _migrate_legacy_chain(fresh_central_database)
    await _migrate_legacy_chain(fresh_central_database)

    conn = await asyncpg.connect(fresh_central_database)
    try:
        expected_upgrade = {
            path.name
            for version, _description, path in cli.discover_migrations()
            if int(version) >= 97
        }
        recorded = {
            row["filename"]
            for row in await conn.fetch("SELECT filename FROM public.schema_migrations")
        }
        assert expected_upgrade <= recorded
        assert "097_image_task_runtime_scope.sql" in recorded
        assert await conn.fetchval("SELECT to_regclass('assistant.image_tasks')") is None
    finally:
        await conn.close()


@pytest.mark.asyncio
async def test_100_to_112_full_chain_uses_one_idempotent_public_ledger(
    migration_database: tuple[str, str],
) -> None:
    dsn, layout = migration_database
    preflight = await asyncpg.connect(dsn)
    try:
        assert (
            await preflight.fetchval("SELECT to_regclass('knowledge.dataset_query_feedback')")
            is None
        )
        assert (
            await preflight.fetchval("SELECT to_regclass('knowledge.kb_document_batch_operations')")
            is None
        )
        assert (
            await preflight.fetchval(
                "SELECT to_regclass('knowledge.embedding_migration_action_jobs')"
            )
            is None
        )
        assert not await preflight.fetchval(
            """
            SELECT EXISTS (
                SELECT 1 FROM information_schema.columns
                WHERE table_name = 'dataset_queries' AND column_name = 'trace_id'
            )
            """
        )
    finally:
        await preflight.close()

    await _migrate_legacy_chain(dsn)
    await _migrate_legacy_chain(dsn)

    conn = await asyncpg.connect(dsn)
    try:
        ledger_columns = {
            row["column_name"]
            for row in await conn.fetch(
                """
                SELECT column_name FROM information_schema.columns
                WHERE table_schema = 'public' AND table_name = 'schema_migrations'
                """
            )
        }
        assert ledger_columns == {"filename", "applied_at"}
        recorded = {
            row["filename"]
            for row in await conn.fetch("SELECT filename FROM public.schema_migrations")
        }
        assert recorded >= RAG_FILENAMES
        assert all(not name.endswith("_rollback.sql") for name in recorded)

        metadata_owner = await conn.fetchval(
            """
            SELECT table_schema FROM information_schema.columns
            WHERE table_name = 'dataset_queries' AND column_name = 'metadata'
            """
        )
        assert metadata_owner == ("knowledge" if layout == "split" else "public")

        canonical_owner = await conn.fetchval("SELECT current_user")
        for table in KB_100_TO_105_TABLES:
            relation = await conn.fetchrow(
                """
                SELECT n.nspname AS namespace,
                       pg_get_userbyid(c.relowner) AS owner
                FROM pg_class AS c
                JOIN pg_namespace AS n ON n.oid = c.relnamespace
                WHERE c.relname = $1 AND c.relkind = 'r'
                """,
                table,
            )
            assert relation is not None, table
            assert relation["namespace"] == "knowledge", (layout, table)
            assert relation["owner"] == canonical_owner, (layout, table)
        for table in KB_106_TO_110_TABLES:
            relation = await conn.fetchrow(
                """
                SELECT n.nspname AS namespace,
                       pg_get_userbyid(c.relowner) AS owner
                FROM pg_class AS c
                JOIN pg_namespace AS n ON n.oid = c.relnamespace
                WHERE c.oid = to_regclass($1)
                """,
                f"knowledge.{table}",
            )
            assert relation is not None, table
            assert relation["namespace"] == "knowledge", (layout, table)
            assert relation["owner"] == canonical_owner, (layout, table)
        for table in KB_111_TABLES:
            relation = await conn.fetchrow(
                """
                SELECT n.nspname AS namespace,
                       pg_get_userbyid(c.relowner) AS owner
                FROM pg_class AS c
                JOIN pg_namespace AS n ON n.oid = c.relnamespace
                WHERE c.oid = to_regclass($1)
                """,
                f"knowledge.{table}",
            )
            assert relation is not None, table
            assert relation["namespace"] == "knowledge", (layout, table)
            assert relation["owner"] == canonical_owner, (layout, table)

        duplicate_central_tables = await conn.fetchval(
            """
            SELECT count(*)
            FROM pg_class AS c
            JOIN pg_namespace AS n ON n.oid = c.relnamespace
            WHERE c.relkind = 'r'
              AND c.relname = ANY($1::text[])
              AND n.nspname <> 'knowledge'
            """,
            list(KB_106_TO_110_TABLES) + list(KB_111_TABLES),
        )
        assert duplicate_central_tables == 0, layout

        invalid_constraints = await conn.fetchval(
            """
            SELECT count(*)
            FROM pg_constraint c
            JOIN pg_namespace n ON n.oid = c.connamespace
            WHERE n.nspname = $1 AND c.conname LIKE 'fk_kb_%' AND NOT c.convalidated
            """,
            "knowledge" if layout == "split" else "public",
        )
        assert invalid_constraints == 0
        invalid_knowledge_constraints = await conn.fetchval(
            """
            SELECT count(*)
            FROM pg_constraint c
            JOIN pg_namespace n ON n.oid = c.connamespace
            WHERE n.nspname = 'knowledge'
              AND c.conname LIKE 'fk_kb_%'
              AND NOT c.convalidated
            """
        )
        assert invalid_knowledge_constraints == 0
    finally:
        await conn.close()


@pytest.mark.asyncio
async def test_112_bounds_document_progress_retention(
    fresh_central_database: str,
) -> None:
    await _migrate_legacy_chain(fresh_central_database)

    conn = await asyncpg.connect(fresh_central_database)
    try:
        await conn.execute(
            "ALTER TABLE knowledge.kb_document_progress_events "
            "DISABLE TRIGGER trg_kb_document_progress_retention"
        )
        await conn.execute(
            """
            INSERT INTO knowledge.kb_document_progress_events (
                dataset_id, document_id, event_type, payload, created_at
            )
            SELECT
                'retention-dataset',
                'document-' || value::text,
                'progress',
                '{}'::jsonb,
                CASE WHEN value = 1
                    THEN NOW() - INTERVAL '8 days'
                    ELSE NOW()
                END
            FROM generate_series(1, 10005) AS value
            """
        )
        await conn.execute(
            "ALTER TABLE knowledge.kb_document_progress_events "
            "ENABLE TRIGGER trg_kb_document_progress_retention"
        )
        maximum = await conn.fetchval(
            "SELECT MAX(event_sequence) FROM knowledge.kb_document_progress_events"
        )
        next_cleanup = ((int(maximum) // 128) + 1) * 128
        sequence_name = await conn.fetchval(
            "SELECT pg_get_serial_sequence("
            "'knowledge.kb_document_progress_events', 'event_sequence')"
        )
        await conn.execute("SELECT setval($1, $2, TRUE)", sequence_name, next_cleanup - 1)
        await conn.execute(
            """
            INSERT INTO knowledge.kb_document_progress_events (
                dataset_id, document_id, event_type, payload
            ) VALUES ('retention-dataset', 'cleanup-trigger', 'progress', '{}'::jsonb)
            """
        )

        count = await conn.fetchval(
            """
            SELECT COUNT(*)
            FROM knowledge.kb_document_progress_events
            WHERE dataset_id = 'retention-dataset'
            """
        )
        expired = await conn.fetchval(
            """
            SELECT COUNT(*)
            FROM knowledge.kb_document_progress_events
            WHERE dataset_id = 'retention-dataset'
              AND created_at < NOW() - INTERVAL '7 days'
            """
        )
        assert count == 10000
        assert expired == 0
    finally:
        await conn.close()
