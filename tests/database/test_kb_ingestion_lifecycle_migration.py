"""Real-PostgreSQL test for migration 101 (T1 ingestion lifecycle).

Same tier-b pattern as tests/database/test_kb_query_telemetry_migration.py:
throwaway schema, minimal inline prerequisites matching the PRE-migration
table shape (legacy status vocabulary, no stage timestamps, no execution log),
migration applied twice (idempotency), then behavioral assertions against a
live developer PostgreSQL.
"""

from __future__ import annotations

import os
import uuid
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any

import asyncpg
import pytest
import pytest_asyncio
from dotenv import dotenv_values

ROOT = Path(__file__).resolve().parents[2]
MIGRATION_101 = ROOT / "database" / "migrations" / "101_kb_ingestion_lifecycle.sql"


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


@pytest_asyncio.fixture
async def lifecycle_pool() -> AsyncIterator[asyncpg.Pool]:
    config = _postgres_config()
    database_name = f"kb_lifecycle_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{database_name}"')

    pool = await asyncpg.create_pool(
        **{**config, "database": database_name},
        min_size=1,
        max_size=2,
        server_settings={"search_path": "knowledge,gateway,assistant,public"},
    )
    try:
        async with pool.acquire() as conn:
            await conn.execute("CREATE SCHEMA knowledge")
            # Pre-migration shape: legacy status vocabulary, no stage
            # timestamps, segments without error text, no execution log.
            await conn.execute(
                """
                CREATE TABLE datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL DEFAULT ''
                );
                CREATE TABLE dataset_process_rules (
                    id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    mode VARCHAR(50) NOT NULL DEFAULT 'automatic',
                    rules JSONB NOT NULL DEFAULT '{}'
                );
                CREATE TABLE documents (
                    document_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    title VARCHAR(255) NOT NULL,
                    status VARCHAR(50) NOT NULL DEFAULT 'uploaded',
                    error TEXT,
                    metadata JSONB,
                    process_rule_id VARCHAR(255),
                    started_at TIMESTAMPTZ,
                    completed_at TIMESTAMPTZ
                );
                CREATE TABLE segments (
                    segment_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    document_id VARCHAR(255) NOT NULL
                        REFERENCES documents(document_id) ON DELETE CASCADE,
                    position INTEGER NOT NULL DEFAULT 0,
                    text TEXT NOT NULL,
                    content_type VARCHAR(50) NOT NULL DEFAULT 'text',
                    status VARCHAR(50) DEFAULT 'completed',
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    content_hash VARCHAR(64),
                    index_node_id VARCHAR(255),
                    index_node_hash VARCHAR(255),
                    -- Legacy position identity spanning text and image rows;
                    -- the migration must swap it for the per-content-type
                    -- constraint (auto-named
                    -- segments_document_id_position_key).
                    UNIQUE(document_id, position)
                );
                INSERT INTO datasets (dataset_id, tenant_id)
                VALUES ('dataset-a', 'tenant-a'), ('dataset-b', 'tenant-b');
                INSERT INTO dataset_process_rules (id, dataset_id)
                VALUES ('rule-1', 'dataset-a'), ('rule-b', 'dataset-b');
                -- Legacy rows covering every pre-migration status value.
                INSERT INTO documents (document_id, dataset_id, title, status) VALUES
                    ('doc-uploaded', 'dataset-a', 'u', 'uploaded'),
                    ('doc-queued', 'dataset-a', 'q', 'queued'),
                    ('doc-processing', 'dataset-a', 'p', 'processing'),
                    ('doc-detecting', 'dataset-a', 'd', 'detecting'),
                    ('doc-segmenting', 'dataset-a', 'sg', 'segmenting'),
                    ('doc-embedding', 'dataset-a', 'e', 'embedding'),
                    ('doc-img-ingest', 'dataset-a', 'ii', 'embedding_images'),
                    ('doc-associating', 'dataset-a', 'ai', 'associating_images'),
                    ('doc-failed', 'dataset-a', 'f', 'failed'),
                    ('doc-completed', 'dataset-a', 'c', 'completed'),
                    ('doc-syncing', 'dataset-a', 'sy', 'syncing');
                INSERT INTO documents (document_id, dataset_id, title, status)
                VALUES ('doc-b', 'dataset-b', 'b', 'completed');
                -- Upload-owned 'embedding_images': same legacy status, but it
                -- belongs to the upload phase, not the ingest pipeline.
                INSERT INTO documents (
                    document_id, dataset_id, title, status, metadata
                ) VALUES (
                    'doc-img-upload', 'dataset-a', 'iu', 'embedding_images',
                    jsonb_build_object(
                        '_document_upload_generation',
                        jsonb_build_object('generation', 'gen-1')
                    )
                );
                INSERT INTO segments (segment_id, dataset_id, document_id, position, text)
                VALUES ('seg-1', 'dataset-a', 'doc-completed', 0, 'legacy segment');
                -- Legacy 023-style row: MD5 digest, no stable identity yet.
                INSERT INTO segments (
                    segment_id, dataset_id, document_id, position, text, content_hash
                )
                VALUES (
                    'seg-md5', 'dataset-a', 'doc-completed', 1, 'md5 legacy',
                    md5('md5 legacy')
                );
                """
            )
            sql = MIGRATION_101.read_text(encoding="utf-8")
            await conn.execute(sql)
            # Second application: idempotency assertion.
            await conn.execute(sql)
        yield pool
    finally:
        await pool.close()
        try:
            await admin.execute(f'DROP DATABASE "{database_name}"')
        finally:
            await admin.close()


@pytest.mark.asyncio
async def test_legacy_status_values_map_to_canonical_vocabulary(
    lifecycle_pool: asyncpg.Pool,
) -> None:
    async with lifecycle_pool.acquire() as conn:
        rows = await conn.fetch("SELECT document_id, status FROM documents ORDER BY document_id")
        mapped = {row["document_id"]: row["status"] for row in rows}
        # Waiting absorbs every pre-processing state; a full replay is the
        # safe superset under deterministic chunk IDs.
        assert mapped["doc-uploaded"] == "waiting"
        assert mapped["doc-queued"] == "waiting"
        assert mapped["doc-processing"] == "waiting"
        assert mapped["doc-detecting"] == "waiting"
        assert mapped["doc-segmenting"] == "splitting"
        assert mapped["doc-embedding"] == "indexing"
        # 'embedding_images' splits by ownership: upload-owned rows stay in
        # the upload-phase vocabulary; ingest-owned rows are mid-pipeline.
        assert mapped["doc-img-upload"] == "uploading_images"
        assert mapped["doc-img-ingest"] == "indexing"
        assert mapped["doc-associating"] == "indexing"
        assert mapped["doc-failed"] == "error"
        # Unchanged states.
        assert mapped["doc-completed"] == "completed"
        assert mapped["doc-syncing"] == "syncing"


@pytest.mark.asyncio
async def test_stage_timestamp_and_error_columns_added(
    lifecycle_pool: asyncpg.Pool,
) -> None:
    async with lifecycle_pool.acquire() as conn:
        columns = {
            (row["table_name"], row["column_name"]): row
            for row in await conn.fetch(
                """
                SELECT table_name, column_name, data_type, is_nullable
                FROM information_schema.columns
                WHERE table_schema = current_schema()
                  AND (
                    (table_name = 'documents' AND column_name IN (
                        'parsing_started_at', 'splitting_started_at',
                        'indexing_started_at'))
                    OR (table_name = 'segments' AND column_name = 'error')
                  )
                """
            )
        }
        for table, column in (
            ("documents", "parsing_started_at"),
            ("documents", "splitting_started_at"),
            ("documents", "indexing_started_at"),
            ("segments", "error"),
        ):
            row = columns[(table, column)]
            assert row["data_type"] == "timestamp with time zone" or (
                table == "segments" and row["data_type"] == "text"
            )
            assert row["is_nullable"] == "YES"


@pytest.mark.asyncio
async def test_pipeline_execution_log_round_trip_and_cascade(
    lifecycle_pool: asyncpg.Pool,
) -> None:
    async with lifecycle_pool.acquire() as conn:
        execution_id = await conn.fetchval(
            """
            INSERT INTO document_pipeline_executions (
                document_id, dataset_id, action, trigger_source, triggered_by,
                process_rule_id, input_snapshot, manifest
            )
            VALUES (
                'doc-completed', 'dataset-a', 'reprocess', 'api', 'user-a',
                'rule-1',
                '{"embedding_model": "text-embedding-v4"}'::jsonb,
                '{"staging_points": []}'::jsonb
            )
            RETURNING execution_id
            """
        )
        assert execution_id

        row = await conn.fetchrow(
            """
            SELECT action, trigger_source, status,
                   input_snapshot->>'embedding_model' AS model
            FROM document_pipeline_executions
            WHERE execution_id = $1
            """,
            execution_id,
        )
        assert row["action"] == "reprocess"
        assert row["trigger_source"] == "api"
        assert row["status"] == "running"
        assert row["model"] == "text-embedding-v4"

        # ON DELETE CASCADE keeps the log from outliving its document.
        await conn.execute("DELETE FROM documents WHERE document_id = 'doc-completed'")
        orphan = await conn.fetchval(
            "SELECT count(*) FROM document_pipeline_executions WHERE execution_id = $1",
            execution_id,
        )
        assert orphan == 0


@pytest.mark.asyncio
async def test_pipeline_rejects_cross_dataset_document_and_rule_references(
    lifecycle_pool: asyncpg.Pool,
) -> None:
    async with lifecycle_pool.acquire() as conn:
        with pytest.raises(asyncpg.ForeignKeyViolationError):
            await conn.execute(
                "UPDATE documents SET process_rule_id = 'rule-b' WHERE document_id = 'doc-uploaded'"
            )

        with pytest.raises(asyncpg.ForeignKeyViolationError):
            await conn.execute(
                """
                INSERT INTO document_pipeline_executions (
                    document_id, dataset_id, action, process_rule_id
                ) VALUES ('doc-b', 'dataset-a', 'reprocess', 'rule-1')
                """
            )

        with pytest.raises(asyncpg.ForeignKeyViolationError):
            await conn.execute(
                """
                INSERT INTO document_pipeline_executions (
                    document_id, dataset_id, action, process_rule_id
                ) VALUES ('doc-uploaded', 'dataset-a', 'reprocess', 'rule-b')
                """
            )


@pytest.mark.asyncio
async def test_legacy_md5_hashes_unified_to_sha256(
    lifecycle_pool: asyncpg.Pool,
) -> None:
    import hashlib

    async with lifecycle_pool.acquire() as conn:
        row = await conn.fetchrow("SELECT content_hash FROM segments WHERE segment_id = 'seg-md5'")
        expected = hashlib.sha256(b"md5 legacy").hexdigest()
        assert row["content_hash"] == expected
        assert len(row["content_hash"]) == 64

        # Rows without a legacy digest keep their NULL (no fabrication).
        untouched = await conn.fetchval(
            "SELECT content_hash FROM segments WHERE segment_id = 'seg-1'"
        )
        assert untouched is None


@pytest.mark.asyncio
async def test_stable_identity_backfilled_for_existing_segments(
    lifecycle_pool: asyncpg.Pool,
) -> None:
    async with lifecycle_pool.acquire() as conn:
        row = await conn.fetchrow(
            """
            SELECT index_node_id, index_node_hash, content_hash
            FROM segments WHERE segment_id = 'seg-md5'
            """
        )
        assert row["index_node_id"] == "doc-completed::text::1"
        assert row["index_node_hash"] == row["content_hash"]

        # A row without content_hash gets no fabricated identity.
        bare = await conn.fetchrow("SELECT index_node_id FROM segments WHERE segment_id = 'seg-1'")
        assert bare["index_node_id"] is None


@pytest.mark.asyncio
async def test_segment_error_text_and_identity_index(
    lifecycle_pool: asyncpg.Pool,
) -> None:
    async with lifecycle_pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE segments
            SET status = 'error', error = 'embedding provider timeout',
                index_node_id = 'doc-completed::0'
            WHERE segment_id = 'seg-1'
            """
        )
        row = await conn.fetchrow(
            "SELECT status, error, index_node_id FROM segments WHERE segment_id = 'seg-1'"
        )
        assert row["status"] == "error"
        assert row["error"] == "embedding provider timeout"

        index = await conn.fetchval(
            """
            SELECT count(*) FROM pg_indexes
            WHERE schemaname = current_schema()
              AND tablename = 'segments'
              AND indexname = 'idx_segments_index_node_id'
            """
        )
        assert index == 1


@pytest.mark.asyncio
async def test_position_identity_scoped_per_content_type(
    lifecycle_pool: asyncpg.Pool,
) -> None:
    async with lifecycle_pool.acquire() as conn:
        # The legacy cross-content-type constraint is gone; the per-content
        # constraint is present (and survived the idempotent second apply).
        constraints = {
            row["conname"]
            for row in await conn.fetch(
                """
                SELECT conname FROM pg_constraint
                WHERE conrelid = 'segments'::regclass
                  AND contype = 'u'
                """
            )
        }
        assert "segments_document_id_position_key" not in constraints
        assert "uq_segments_doc_content_position" in constraints

        # A text row and an image row may share a numeric position...
        await conn.execute(
            """
            INSERT INTO segments (
                segment_id, dataset_id, document_id, position, text,
                content_type
            )
            VALUES ('seg-img-0', 'dataset-a', 'doc-completed', 0, 'img', 'image')
            """
        )

        # ...but two rows of the same content type may not.
        with pytest.raises(asyncpg.UniqueViolationError):
            await conn.execute(
                """
                INSERT INTO segments (
                    segment_id, dataset_id, document_id, position, text,
                    content_type
                )
                VALUES (
                    'seg-text-dup', 'dataset-a', 'doc-completed', 0, 'dup',
                    'text'
                )
                """
            )
