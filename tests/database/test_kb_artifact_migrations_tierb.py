"""Tier-b PostgreSQL coverage for migrations 106/107 and asyncpg stores."""

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
MIGRATION_106 = ROOT / "database" / "migrations" / "106_kb_segment_attachment_bindings.sql"
MIGRATION_107 = ROOT / "database" / "migrations" / "107_kb_parsing_ir.sql"


def _postgres_config() -> dict[str, Any]:
    file_values = dotenv_values(os.environ.get("ENV_FILE") or ROOT / ".env")
    required = ("POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB", "POSTGRES_PORT")
    values = {key: os.environ.get(key) or file_values.get(key) for key in required}
    missing = [key for key, value in values.items() if not value]
    if missing:
        pytest.fail(f"local PostgreSQL test configuration missing keys: {', '.join(missing)}")
    return {
        "host": os.environ.get("POSTGRES_HOST")
        or file_values.get("POSTGRES_HOST")
        or "127.0.0.1",
        "port": int(str(values["POSTGRES_PORT"])),
        "user": str(values["POSTGRES_USER"]),
        "password": str(values["POSTGRES_PASSWORD"]),
        "database": str(values["POSTGRES_DB"]),
    }


@pytest_asyncio.fixture
async def artifact_pool() -> AsyncIterator[asyncpg.Pool]:
    config = _postgres_config()
    database_name = f"kb_artifact_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{database_name}"')
    pool = await asyncpg.create_pool(
        **{**config, "database": database_name},
        min_size=1,
        max_size=3,
    )
    try:
        async with pool.acquire() as conn:
            await conn.execute(
                """
                CREATE TABLE datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
                    UNIQUE (dataset_id, tenant_id)
                );
                CREATE TABLE documents (
                    document_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    title VARCHAR(255) NOT NULL DEFAULT '',
                    mime_type VARCHAR(100),
                    current_version INTEGER NOT NULL DEFAULT 1,
                    UNIQUE (document_id, dataset_id)
                );
                CREATE TABLE segments (
                    segment_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    document_id VARCHAR(255) NOT NULL
                        REFERENCES documents(document_id) ON DELETE CASCADE,
                    position INTEGER NOT NULL DEFAULT 0,
                    text TEXT NOT NULL DEFAULT '',
                    token_count INTEGER NOT NULL DEFAULT 0,
                    vector_id VARCHAR(255),
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    content_type VARCHAR(50) NOT NULL DEFAULT 'text',
                    image_url TEXT,
                    image_attachment_id VARCHAR(255),
                    image_filename VARCHAR(255),
                    image_media_type VARCHAR(100),
                    image_file_size INTEGER,
                    has_images BOOLEAN NOT NULL DEFAULT FALSE,
                    image_count INTEGER NOT NULL DEFAULT 0,
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    status VARCHAR(50) NOT NULL DEFAULT 'completed',
                    error TEXT,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    UNIQUE (document_id, content_type, position)
                );
                CREATE TABLE segment_images (
                    id BIGSERIAL PRIMARY KEY,
                    segment_id VARCHAR(255) NOT NULL
                        REFERENCES segments(segment_id) ON DELETE CASCADE,
                    image_segment_id VARCHAR(255) NOT NULL
                        REFERENCES segments(segment_id) ON DELETE CASCADE,
                    position INTEGER NOT NULL DEFAULT 0,
                    proximity_score DOUBLE PRECISION NOT NULL DEFAULT 1.0,
                    char_offset INTEGER NOT NULL DEFAULT 0,
                    page_number INTEGER,
                    UNIQUE (segment_id, image_segment_id)
                );
                INSERT INTO datasets (dataset_id, tenant_id) VALUES
                    ('dataset-a', 'tenant-a'), ('dataset-b', 'tenant-b');
                INSERT INTO documents (document_id, dataset_id, title) VALUES
                    ('document-a', 'dataset-a', 'A'),
                    ('document-b', 'dataset-b', 'B');
                INSERT INTO segments (
                    segment_id, dataset_id, document_id, position,
                    content_type, image_attachment_id
                ) VALUES
                    ('text-a', 'dataset-a', 'document-a', 0, 'text', NULL),
                    ('image-a', 'dataset-a', 'document-a', 1, 'image', 'attachment-a'),
                    ('text-b', 'dataset-b', 'document-b', 0, 'text', NULL),
                    ('image-b', 'dataset-b', 'document-b', 1, 'image', 'attachment-b');
                INSERT INTO segment_images (segment_id, image_segment_id)
                VALUES ('text-a', 'image-a');
                """
            )
            for migration in (MIGRATION_106, MIGRATION_107):
                sql = migration.read_text(encoding="utf-8")
                await conn.execute(sql)
                await conn.execute(sql)
        yield pool
    finally:
        await pool.close()
        try:
            await admin.execute(f'DROP DATABASE "{database_name}"')
        finally:
            await admin.close()


def _storage(pool: asyncpg.Pool) -> DatabaseStorage:
    storage = DatabaseStorage()
    storage._pool = pool
    return storage


@pytest.mark.asyncio
async def test_106_replaces_bindings_and_fks_reject_cross_tenant_rows(
    artifact_pool: asyncpg.Pool,
) -> None:
    storage = _storage(artifact_pool)
    assert await storage.replace_document_attachment_bindings(
        "document-a",
        "dataset-a",
        tenant_id="tenant-a",
    ) == (1, 0)
    await storage.store_image_segments(
        [
            {
                "segment_id": "image-a",
                "dataset_id": "dataset-a",
                "document_id": "document-a",
                "position": 1,
                "text": "updated image",
                "vector_id": "image-a",
                "metadata": {"page_number": 1},
                "image_url": "s3://bucket/image-a",
                "image_attachment_id": "attachment-a",
                "image_filename": "image-a.png",
                "image_media_type": "image/png",
                "image_file_size": 10,
            }
        ]
    )

    async with artifact_pool.acquire() as conn:
        namespaces = {
            row["relname"]: row["nspname"]
            for row in await conn.fetch(
                """
                SELECT c.relname, n.nspname
                FROM pg_class AS c
                JOIN pg_namespace AS n ON n.oid = c.relnamespace
                WHERE c.relname = ANY($1::text[])
                  AND c.relkind = 'r'
                """,
                [
                    "kb_segment_attachment_bindings",
                    "kb_parsing_ir",
                    "kb_parsing_page_cache",
                ],
            )
        }
        assert namespaces == {
            "kb_segment_attachment_bindings": "knowledge",
            "kb_parsing_ir": "knowledge",
            "kb_parsing_page_cache": "knowledge",
        }
        row = await conn.fetchrow(
            "SELECT * FROM knowledge.kb_segment_attachment_bindings"
        )
        assert {
            "tenant_id": "tenant-a",
            "dataset_id": "dataset-a",
            "document_id": "document-a",
            "segment_id": "text-a",
            "attachment_id": "attachment-a",
        }.items() <= dict(row).items()
        assert await conn.fetchval(
            "SELECT text FROM segments WHERE segment_id = 'image-a'"
        ) == "updated image"
        with pytest.raises(asyncpg.ForeignKeyViolationError):
            await conn.execute(
                """
                INSERT INTO knowledge.kb_segment_attachment_bindings (
                    tenant_id, dataset_id, document_id, segment_id, attachment_id
                ) VALUES ('tenant-b', 'dataset-a', 'document-a', 'text-a', 'bad')
                """
            )
        await conn.execute("DELETE FROM segments WHERE segment_id = 'text-a'")
        assert await conn.fetchval(
            "SELECT COUNT(*) FROM knowledge.kb_segment_attachment_bindings"
        ) == 0


@pytest.mark.asyncio
async def test_association_replacement_failure_rolls_back_previous_generation(
    artifact_pool: asyncpg.Pool,
) -> None:
    storage = _storage(artifact_pool)
    with pytest.raises(RuntimeError, match="crossed document ownership"):
        await storage.replace_document_image_associations(
            "document-a",
            "dataset-a",
            "tenant-a",
            [
                {
                    "segment_id": "text-a",
                    "image_segment_id": "image-b",
                    "proximity_score": 1.0,
                }
            ],
        )

    async with artifact_pool.acquire() as conn:
        pairs = await conn.fetch(
            "SELECT segment_id, image_segment_id FROM segment_images"
        )
        assert [(row["segment_id"], row["image_segment_id"]) for row in pairs] == [
            ("text-a", "image-a")
        ]


@pytest.mark.asyncio
async def test_107_asyncpg_ir_and_page_cache_are_tenant_scoped(
    artifact_pool: asyncpg.Pool,
) -> None:
    storage = _storage(artifact_pool)
    ir = {
        "doc_id": "document-a",
        "content_hash": "a" * 64,
        "schema_version": "1",
        "pages": [{"page_number": 1, "blocks": []}],
        "metadata": {},
    }
    assert await storage.store_parsing_ir(
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        document_id="document-a",
        generation_key=f"v1:{'a' * 64}",
        content_hash="a" * 64,
        schema_version="1",
        parser_bundle="bundle-a",
        parser_config_hash="b" * 64,
        cascade_config={"stages": [{"backend": "text_layer"}]},
        ir=ir,
        stats={"pages": 1},
    )
    assert not await storage.store_parsing_ir(
        tenant_id="tenant-b",
        dataset_id="dataset-a",
        document_id="document-a",
        generation_key=f"v1:{'a' * 64}",
        content_hash="a" * 64,
        schema_version="1",
        parser_bundle="bundle-a",
        parser_config_hash="b" * 64,
        cascade_config={},
        ir=ir,
        stats={},
    )
    loaded = await storage.load_parsing_ir(
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        document_id="document-a",
        generation_key=f"v1:{'a' * 64}",
        parser_bundle="bundle-a",
        parser_config_hash="b" * 64,
    )
    assert loaded and loaded["ir"]["doc_id"] == "document-a"
    assert await storage.store_parsing_page_cache(
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        document_id="document-a",
        generation_key=f"v1:{'a' * 64}",
        cache_key="c" * 64,
        content_hash="a" * 64,
        page_number=1,
        backend="text_layer",
        backend_version="1-boundary",
        parser_config_hash="b" * 64,
        page_ir={"page_number": 1, "blocks": []},
        confidence=1.0,
        hard_page=False,
    )
    page = await storage.load_parsing_page_cache(
        tenant_id="tenant-a",
        dataset_id="dataset-a",
        document_id="document-a",
        generation_key=f"v1:{'a' * 64}",
        cache_key="c" * 64,
        parser_config_hash="b" * 64,
    )
    assert page and page["backend_version"] == "1-boundary"
    assert await storage.load_parsing_page_cache(
        tenant_id="tenant-b",
        dataset_id="dataset-a",
        document_id="document-a",
        generation_key=f"v1:{'a' * 64}",
        cache_key="c" * 64,
        parser_config_hash="b" * 64,
    ) is None
