"""Real-PostgreSQL behavior test for the T1 retrieval zero-window contract.

PRD T1: re-ingest must never black out the serving generation. The
authoritative serving gates (filter_active_segment_ids /
filter_active_document_ids) therefore decide per SEGMENT state, never per
document lifecycle status: a document that is queued, parsing, splitting,
indexing, syncing, or errored keeps serving its completed+enabled segments.
Documents are hidden only when disabled, archived, or under an explicit
reindex marker; staged rows (status='indexing', enabled=FALSE) stay
invisible until the completion flip.

Tier-b pattern: throwaway schema + minimal tables carrying exactly the
columns the gate queries read, exercised through the production
DatabaseStorage methods over a live developer PostgreSQL.
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
async def gate_world() -> AsyncIterator[tuple[DatabaseStorage, asyncpg.Pool]]:
    config = _postgres_config()
    schema_name = f"kb_zero_window_test_{uuid.uuid4().hex}"
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
                    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
                    content_revision BIGINT NOT NULL DEFAULT 0,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE documents (
                    document_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    status VARCHAR(50) NOT NULL DEFAULT 'waiting',
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
                    disabled_by VARCHAR(255),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                INSERT INTO datasets (dataset_id, tenant_id)
                VALUES ('dataset-a', 'tenant-a'), ('dataset-b', 'tenant-b');
                """
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


async def _put_document(
    pool: asyncpg.Pool,
    *,
    document_id: str,
    dataset_id: str = "dataset-a",
    status: str = "completed",
    enabled: bool = True,
    archived: bool = False,
    metadata: dict[str, Any] | None = None,
) -> None:
    import json

    async with pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO documents (document_id, dataset_id, status, enabled, archived, metadata)
            VALUES ($1, $2, $3, $4, $5, $6::jsonb)
            ON CONFLICT (document_id) DO UPDATE
            SET status = $3, enabled = $4, archived = $5, metadata = $6::jsonb
            """,
            document_id,
            dataset_id,
            status,
            enabled,
            archived,
            json.dumps(metadata or {}),
        )


async def _put_segment(
    pool: asyncpg.Pool,
    *,
    segment_id: str,
    document_id: str,
    dataset_id: str = "dataset-a",
    status: str = "completed",
    enabled: bool = True,
) -> None:
    async with pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO segments (segment_id, dataset_id, document_id, status, enabled)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (segment_id) DO UPDATE
            SET status = $4, enabled = $5
            """,
            segment_id,
            dataset_id,
            document_id,
            status,
            enabled,
        )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "document_status",
    [
        "waiting",
        "parsing",
        "splitting",
        "indexing",
        "syncing",
        "error",
        "completed",
        "uploading_images",
    ],
)
async def test_serving_segments_stay_visible_through_every_lifecycle_state(
    gate_world: tuple[DatabaseStorage, asyncpg.Pool],
    document_status: str,
) -> None:
    """The zero-window core: document lifecycle state never hides serving rows."""
    database, pool = gate_world
    await _put_document(pool, document_id="doc-a", status=document_status)
    await _put_segment(pool, segment_id="seg-serving", document_id="doc-a")

    visible = await database.filter_active_segment_ids(
        "dataset-a", "tenant-a", ["seg-serving"]
    )

    assert visible == {"seg-serving"}, (
        f"document status '{document_status}' must not black out serving segments"
    )
    assert await database.filter_active_document_ids(
        "dataset-a", "tenant-a", ["doc-a"]
    ) == {"doc-a"}


@pytest.mark.asyncio
async def test_staged_rows_are_invisible_until_completion_flip(
    gate_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = gate_world
    await _put_document(pool, document_id="doc-a", status="indexing")
    await _put_segment(
        pool,
        segment_id="seg-staged",
        document_id="doc-a",
        status="indexing",
        enabled=False,
    )
    await _put_segment(pool, segment_id="seg-serving", document_id="doc-a")

    visible = await database.filter_active_segment_ids(
        "dataset-a", "tenant-a", ["seg-staged", "seg-serving"]
    )

    assert visible == {"seg-serving"}


@pytest.mark.asyncio
async def test_operator_disabled_and_errored_segments_stay_hidden(
    gate_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = gate_world
    await _put_document(pool, document_id="doc-a", status="completed")
    await _put_segment(
        pool, segment_id="seg-disabled", document_id="doc-a", enabled=False
    )
    await _put_segment(
        pool, segment_id="seg-errored", document_id="doc-a", status="error"
    )

    visible = await database.filter_active_segment_ids(
        "dataset-a", "tenant-a", ["seg-disabled", "seg-errored"]
    )

    assert visible == set()


@pytest.mark.asyncio
async def test_disabled_archived_and_reindex_marked_documents_hide_all_rows(
    gate_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = gate_world
    await _put_document(pool, document_id="doc-off", enabled=False)
    await _put_document(pool, document_id="doc-archived", archived=True)
    await _put_document(
        pool,
        document_id="doc-reindex",
        metadata={"_document_lifecycle_reindex": {"status": "pending"}},
    )
    for document_id, segment_id in (
        ("doc-off", "seg-off"),
        ("doc-archived", "seg-archived"),
        ("doc-reindex", "seg-reindex"),
    ):
        await _put_segment(pool, segment_id=segment_id, document_id=document_id)

    visible_segments = await database.filter_active_segment_ids(
        "dataset-a",
        "tenant-a",
        ["seg-off", "seg-archived", "seg-reindex"],
    )
    visible_documents = await database.filter_active_document_ids(
        "dataset-a",
        "tenant-a",
        ["doc-off", "doc-archived", "doc-reindex"],
    )

    assert visible_segments == set()
    assert visible_documents == set()


@pytest.mark.asyncio
async def test_gate_keeps_tenant_scope_and_soft_deleted_datasets(
    gate_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = gate_world
    await _put_document(pool, document_id="doc-foreign", dataset_id="dataset-b")
    await _put_segment(
        pool, segment_id="seg-foreign", document_id="doc-foreign", dataset_id="dataset-b"
    )
    async with pool.acquire() as conn:
        await conn.execute(
            "INSERT INTO datasets (dataset_id, tenant_id, is_deleted) "
            "VALUES ('dataset-gone', 'tenant-a', TRUE)"
        )
        await conn.execute(
            "INSERT INTO documents (document_id, dataset_id, status) "
            "VALUES ('doc-gone', 'dataset-gone', 'completed')"
        )
        await conn.execute(
            "INSERT INTO segments (segment_id, dataset_id, document_id) "
            "VALUES ('seg-gone', 'dataset-gone', 'doc-gone')"
        )

    assert await database.filter_active_segment_ids(
        "dataset-b", "tenant-a", ["seg-foreign"]
    ) == set()
    assert await database.filter_active_segment_ids(
        "dataset-b", "tenant-b", ["seg-foreign"]
    ) == {"seg-foreign"}
    assert await database.filter_active_segment_ids(
        "dataset-gone", "tenant-a", ["seg-gone"]
    ) == set()


@pytest.mark.asyncio
async def test_reembed_publication_holds_negative_revision_until_atomic_flip(
    gate_world: tuple[DatabaseStorage, asyncpg.Pool],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    database, pool = gate_world
    await _put_document(pool, document_id="doc-publication", status="indexing")
    await _put_segment(
        pool,
        segment_id="seg-publication",
        document_id="doc-publication",
        status="indexing",
        enabled=False,
    )

    async def accept_identity(*_args: Any, **_kwargs: Any) -> dict[str, Any]:
        return {}

    monkeypatch.setattr(
        database,
        "_require_dataset_ingestion_identity",
        accept_identity,
    )
    async with database.dataset_index_publication_lease(
        "dataset-a",
        expected_ingestion_identity="identity-a",
    ) as publication:
        publication_connection = publication.connection
        assert publication.recovered is False
        async with pool.acquire() as observer:
            publishing_revision = await observer.fetchval(
                "SELECT content_revision FROM datasets WHERE dataset_id = 'dataset-a'"
            )
        assert publishing_revision <= -100_000

        promoted = await database.commit_reembed_publication(
            dataset_id="dataset-a",
            document_id="doc-publication",
            staged_segment_ids=["seg-publication"],
            expected_ingestion_identity="identity-a",
            connection=publication_connection,
        )
        assert promoted == 1

    async with pool.acquire() as observer:
        row = await observer.fetchrow(
            """
            SELECT s.status, s.enabled, d.content_revision
            FROM segments AS s
            JOIN datasets AS d ON d.dataset_id = s.dataset_id
            WHERE s.segment_id = 'seg-publication'
            """
        )
    assert row is not None
    assert (row["status"], row["enabled"]) == ("completed", True)
    assert row["content_revision"] > 0


@pytest.mark.asyncio
async def test_aborted_publication_releases_revision_only_after_rollback_receipt(
    gate_world: tuple[DatabaseStorage, asyncpg.Pool],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    database, pool = gate_world

    async def accept_identity(*_args: Any, **_kwargs: Any) -> dict[str, Any]:
        return {}

    monkeypatch.setattr(
        database,
        "_require_dataset_ingestion_identity",
        accept_identity,
    )
    async with pool.acquire() as setup:
        await setup.execute(
            "UPDATE datasets SET content_revision = -777 WHERE dataset_id = 'dataset-a'"
        )

    async with database.dataset_index_publication_lease(
        "dataset-a",
        expected_ingestion_identity="identity-a",
    ) as publication:
        publication_connection = publication.connection
        assert publication.recovered is True
        assert publication.revision == -777
        assert await publication_connection.fetchval(
            "SELECT content_revision < 0 FROM datasets WHERE dataset_id = 'dataset-a'"
        )
        await database.abort_index_publication(
            "dataset-a",
            connection=publication_connection,
        )

    async with pool.acquire() as observer:
        revision = await observer.fetchval(
            "SELECT content_revision FROM datasets WHERE dataset_id = 'dataset-a'"
        )
    assert revision > 0
