"""Real-PostgreSQL evidence test for the B6 FTS debt (PRD T1-8).

Tier-b pattern (see test_kb_retrieval_zero_window.py): throwaway schema +
minimal tables carrying exactly the columns the FTS/ILIKE legs read,
exercised through the production DatabaseStorage methods over a live
developer PostgreSQL.

What this pins down:
  * The tsvector side is ``to_tsvector('simple', text)`` (the migration-001
    trigger), whose default parser emits ONE lexeme per unbroken CJK run.
    Chinese word tokens (jieba-style) therefore cannot match the GIN leg —
    the FTS query returns ZERO rows even though the segment contains the
    terms. This is the 'simple' vs multilingual-tokenizer inconsistency.
  * The documented substring ILIKE matcher DOES find those rows, which is
    why the retrieval service routes zero-result CJK queries to it.
  * Both legs keep the tenant/dataset and lifecycle predicates intact —
    cross-tenant, cross-dataset, disabled, and archived rows never leak.
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
from knowledge_service.persistence.database import DatabaseStorage

ROOT = Path(__file__).resolve().parents[2]

# Terms as the jieba-style tokenizer would emit them for
# "这是一个机器学习的问题" — real Chinese words, not the whole run.
ZH_TERMS = ["这是", "一个", "机器学习", "问题"]


def _postgres_config() -> dict[str, Any]:
    file_values = dotenv_values(os.environ.get("ENV_FILE") or ROOT / ".env")
    required = ("POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB", "POSTGRES_PORT")
    values = {key: os.environ.get(key) or file_values.get(key) for key in required}
    missing = [key for key, value in values.items() if not value]
    if missing:
        pytest.fail(f"local PostgreSQL test configuration missing keys: {', '.join(missing)}")
    return {
        "host": "127.0.0.1",
        "port": int(str(values["POSTGRES_PORT"])),
        "user": str(values["POSTGRES_USER"]),
        "password": str(values["POSTGRES_PASSWORD"]),
        "database": str(values["POSTGRES_DB"]),
    }


@pytest_asyncio.fixture
async def fts_world() -> AsyncIterator[tuple[DatabaseStorage, asyncpg.Pool]]:
    config = _postgres_config()
    schema_name = f"kb_fts_fallback_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE SCHEMA "{schema_name}"')
    await admin.close()

    pool = await asyncpg.create_pool(
        **config,
        min_size=1,
        max_size=2,
        server_settings={"search_path": f'"{schema_name}",public'},
    )
    try:
        async with pool.acquire() as conn:
            await conn.execute(
                """
                CREATE TABLE datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    is_deleted BOOLEAN NOT NULL DEFAULT FALSE
                );
                CREATE TABLE documents (
                    document_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    archived BOOLEAN NOT NULL DEFAULT FALSE,
                    metadata JSONB
                );
                CREATE TABLE segments (
                    segment_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    document_id VARCHAR(255) NOT NULL
                        REFERENCES documents(document_id) ON DELETE CASCADE,
                    status VARCHAR(50) NOT NULL DEFAULT 'completed',
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    text TEXT NOT NULL DEFAULT '',
                    text_search TSVECTOR,
                    source_type VARCHAR(50) NOT NULL DEFAULT 'manual',
                    language VARCHAR(10) NOT NULL DEFAULT 'en',
                    position INT NOT NULL DEFAULT 0,
                    token_count INT NOT NULL DEFAULT 0,
                    metadata JSONB
                );
                -- Mirror the production trigger from 001_kb_schema.sql:
                -- the tsvector side is 'simple' config.
                CREATE FUNCTION segments_text_search_update() RETURNS trigger AS $$
                BEGIN
                    NEW.text_search := to_tsvector('simple', COALESCE(NEW.text, ''));
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                CREATE TRIGGER trg_segments_text_search
                    BEFORE INSERT OR UPDATE OF text ON segments
                    FOR EACH ROW EXECUTE FUNCTION segments_text_search_update();
                CREATE INDEX kb_fts_test_gin ON segments USING GIN (text_search);

                INSERT INTO datasets (dataset_id, tenant_id) VALUES
                    ('dataset-a', 'tenant-a'),
                    ('dataset-b', 'tenant-a'),
                    ('dataset-c', 'tenant-b');
                INSERT INTO documents (document_id, dataset_id, enabled, archived, metadata) VALUES
                    ('doc-a', 'dataset-a', TRUE, FALSE, '{}'::jsonb),
                    ('doc-a-disabled', 'dataset-a', FALSE, FALSE, '{}'::jsonb),
                    ('doc-a-archived', 'dataset-a', TRUE, TRUE, '{}'::jsonb),
                    ('doc-b', 'dataset-b', TRUE, FALSE, '{}'::jsonb),
                    ('doc-c', 'dataset-c', TRUE, FALSE, '{}'::jsonb);
                """
            )
            await conn.executemany(
                """
                INSERT INTO segments
                    (segment_id, dataset_id, document_id, text, source_type, language)
                VALUES ($1, $2, $3, $4, 'manual', $5)
                """,
                [
                    # Unbroken CJK run: 'simple' tsvector sees ONE lexeme.
                    ("seg-zh-a", "dataset-a", "doc-a", "这是一个机器学习的问题", "zh"),
                    # Latin words tokenize normally under 'simple'.
                    ("seg-en-a", "dataset-a", "doc-a", "gradient descent algorithm", "en"),
                    # Same zh text, different dataset (same tenant) and other tenant.
                    ("seg-zh-b", "dataset-b", "doc-b", "这是一个机器学习的问题", "zh"),
                    ("seg-zh-c", "dataset-c", "doc-c", "这是一个机器学习的问题", "zh"),
                    # Lifecycle-hidden rows with the same zh text.
                    ("seg-zh-disabled", "dataset-a", "doc-a-disabled", "这是一个机器学习的问题", "zh"),
                    ("seg-zh-archived", "dataset-a", "doc-a-archived", "这是一个机器学习的问题", "zh"),
                    ("seg-zh-incomplete", "dataset-a", "doc-a", "这是一个机器学习的问题", "zh"),
                ],
            )
            await conn.execute(
                "UPDATE segments SET status = 'indexing' WHERE segment_id = 'seg-zh-incomplete'"
            )
        database = DatabaseStorage()
        database._pool = pool  # type: ignore[assignment]
        yield database, pool
    finally:
        await pool.close()
        admin = await asyncpg.connect(**config)
        try:
            await admin.execute(f'DROP SCHEMA "{schema_name}" CASCADE')
        finally:
            await admin.close()


class TestSimpleFtsTokenizerMismatch:
    async def test_zh_word_tokens_miss_the_simple_gin_leg(self, fts_world):
        """The B6 evidence: GIN leg returns ZERO rows though text contains terms."""
        database, _pool = fts_world
        fts_rows = await database._search_segments_fts(
            "dataset-a", "tenant-a", ZH_TERMS, None, None, None, 20, None
        )
        assert fts_rows == [], (
            "to_tsvector('simple', CJK-run) must expose the one-lexeme-per-run "
            "behavior this fallback exists for"
        )

    async def test_zh_word_tokens_hit_the_ilike_compat_leg(self, fts_world):
        """The documented fallback finds exactly the row FTS could not."""
        database, _pool = fts_world
        rows = await database._search_segments_ilike(
            "dataset-a", "tenant-a", ZH_TERMS, None, None, None, 20, None
        )
        segment_ids = {row["segment_id"] for row in rows}
        assert "seg-zh-a" in segment_ids
        # ILIKE OR-widens across terms but the lifecycle gates still apply:
        assert "seg-zh-incomplete" not in segment_ids

    async def test_en_tokens_match_both_legs(self, fts_world):
        """'simple' is consistent for Latin word text — fallback is not needed."""
        database, _pool = fts_world
        fts_rows = await database._search_segments_fts(
            "dataset-a", "tenant-a", ["algorithm"], None, None, None, 20, None
        )
        ilike_rows = await database._search_segments_ilike(
            "dataset-a", "tenant-a", ["algorithm"], None, None, None, 20, None
        )
        assert {row["segment_id"] for row in fts_rows} == {"seg-en-a"}
        assert {row["segment_id"] for row in ilike_rows} == {"seg-en-a"}


class TestFallbackLegTenantIsolation:
    """(e) The zero-result fallback must keep the FTS leg's predicates."""

    async def test_ilike_leg_scoped_to_tenant_and_dataset(self, fts_world):
        database, _pool = fts_world
        rows = await database._search_segments_ilike(
            "dataset-a", "tenant-a", ["机器学习"], None, None, None, 20, None
        )
        assert {row["segment_id"] for row in rows} == {"seg-zh-a"}

    async def test_ilike_leg_rejects_wrong_tenant(self, fts_world):
        database, _pool = fts_world
        rows = await database._search_segments_ilike(
            "dataset-a", "tenant-b", ["机器学习"], None, None, None, 20, None
        )
        assert rows == []

    async def test_ilike_leg_hides_disabled_archived_and_unfinished(self, fts_world):
        database, _pool = fts_world
        rows = await database._search_segments_ilike(
            "dataset-a", "tenant-a", ["问题"], None, None, None, 20, None
        )
        segment_ids = {row["segment_id"] for row in rows}
        assert segment_ids == {"seg-zh-a"}
        assert not {"seg-zh-disabled", "seg-zh-archived", "seg-zh-incomplete"} & segment_ids

    async def test_fts_leg_isolation_predicates_unchanged(self, fts_world):
        """Exact-run FTS still respects dataset/tenant/lifecycle gates."""
        database, _pool = fts_world
        rows = await database._search_segments_fts(
            "dataset-a",
            "tenant-a",
            ["gradient", "descent", "algorithm"],
            None,
            None,
            None,
            20,
            None,
        )
        assert {row["segment_id"] for row in rows} == {"seg-en-a"}
