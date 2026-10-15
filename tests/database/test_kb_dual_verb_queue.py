"""Real-PostgreSQL behavior tests for the T1 dual-verb queue contract.

PRD T1 items 3/4 + addendum §1: documents carry an ingest verb
(ingest/reprocess/reembed/recover/retry) pinned atomically at claim time on
the queued row's metadata; interactive verbs receive a finite dispatch-age
bias while tenant batches rotate round-robin; crash recovery self-marks the recover verb with
the stage the generation died in; terminal status writes retire the markers;
and document_pipeline_executions keeps the replay snapshot + manifest ledger.

Tier-b pattern: throwaway schema + minimal tables carrying exactly the
columns the production queries read, exercised through the production
DatabaseStorage methods over a live developer PostgreSQL.
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import os
import uuid
from collections.abc import AsyncIterator
from datetime import datetime, timedelta, timezone
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from unittest.mock import AsyncMock

import asyncpg
import pytest
import pytest_asyncio
from dotenv import dotenv_values
from fastapi import HTTPException
from knowledge_service.api.routes.knowledge import _submit_route_execution
from knowledge_service.persistence.database import (
    DOCUMENT_INGEST_ACTION_KEY,
    DOCUMENT_LIFECYCLE_REINDEX_KEY,
    DOCUMENT_PENDING_RESTORE_VERSION_KEY,
    DOCUMENT_PIPELINE_EXECUTION_KEY,
    DOCUMENT_RECOVER_STAGE_KEY,
    DOCUMENT_RESTORED_SOURCE_VERSION_KEY,
    DatabaseStorage,
    dataset_ingestion_identity,
)
from knowledge_service.services.knowledge.special_publication import SpecialPublicationCoordinator
from knowledge_service.services.knowledge.worker import (
    KnowledgeIngestTask,
    KnowledgeWorker,
)
from qdrant_client.http.models import PointStruct

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
async def verb_world() -> AsyncIterator[tuple[DatabaseStorage, asyncpg.Pool]]:
    config = _postgres_config()
    schema_name = f"kb_dual_verb_test_{uuid.uuid4().hex}"
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
                    is_archived BOOLEAN NOT NULL DEFAULT FALSE,
                    collection_name VARCHAR(255),
                    embedding_provider VARCHAR(255),
                    embedding_model VARCHAR(255),
                    embedding_dimension INTEGER,
                    embedding_config JSONB,
                    index_config JSONB,
                    content_revision BIGINT NOT NULL DEFAULT 0,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE dataset_process_rules (
                    id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    mode VARCHAR(50) NOT NULL,
                    rules JSONB NOT NULL DEFAULT '{}'::jsonb,
                    created_by VARCHAR(255),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE documents (
                    document_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    title VARCHAR(255),
                    status VARCHAR(50) NOT NULL DEFAULT 'completed',
                    progress DOUBLE PRECISION,
                    error TEXT,
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    archived BOOLEAN NOT NULL DEFAULT FALSE,
                    metadata JSONB,
                    content TEXT,
                    current_version INTEGER NOT NULL DEFAULT 1,
                    version_count INTEGER NOT NULL DEFAULT 1,
                    word_count INTEGER NOT NULL DEFAULT 0,
                    segment_count INTEGER NOT NULL DEFAULT 0,
                    size_bytes BIGINT NOT NULL DEFAULT 0,
                    source_type VARCHAR(50) NOT NULL DEFAULT 'upload',
                    source_uri TEXT,
                    confluence_version INTEGER,
                    mime_type VARCHAR(100) NOT NULL DEFAULT 'text/plain',
                    process_rule_id VARCHAR(255),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    started_at TIMESTAMPTZ,
                    parsing_started_at TIMESTAMPTZ,
                    splitting_started_at TIMESTAMPTZ,
                    indexing_started_at TIMESTAMPTZ,
                    completed_at TIMESTAMPTZ,
                    disabled_at TIMESTAMPTZ,
                    disabled_by VARCHAR(255),
                    archived_at TIMESTAMPTZ,
                    archived_by VARCHAR(255),
                    archived_reason TEXT
                );
                CREATE TABLE document_pipeline_executions (
                    execution_id VARCHAR(255) PRIMARY KEY
                        DEFAULT gen_random_uuid()::text,
                    document_id VARCHAR(255) NOT NULL
                        REFERENCES documents(document_id) ON DELETE CASCADE,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    action VARCHAR(50) NOT NULL,
                    trigger_source VARCHAR(50) NOT NULL DEFAULT 'api',
                    triggered_by VARCHAR(255),
                    process_rule_id VARCHAR(255),
                    input_snapshot JSONB NOT NULL DEFAULT '{}',
                    manifest JSONB NOT NULL DEFAULT '{}',
                    status VARCHAR(50) NOT NULL DEFAULT 'running',
                    error TEXT,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    completed_at TIMESTAMPTZ
                );
                CREATE TABLE segments (
                    segment_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    document_id VARCHAR(255) NOT NULL
                        REFERENCES documents(document_id) ON DELETE CASCADE,
                    position INTEGER NOT NULL DEFAULT 0 CHECK (position >= 0),
                    text TEXT NOT NULL,
                    token_count INTEGER NOT NULL DEFAULT 0,
                    vector_id VARCHAR(255),
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    content_type VARCHAR(50) NOT NULL DEFAULT 'text',
                    source_type VARCHAR(50) DEFAULT 'unknown',
                    source_reference JSONB NOT NULL DEFAULT '{}'::jsonb,
                    citation_text VARCHAR(500) DEFAULT '',
                    page_number INTEGER,
                    section_header VARCHAR(500) DEFAULT '',
                    language VARCHAR(10) DEFAULT 'en',
                    contextual_prefix TEXT DEFAULT '',
                    content_hash VARCHAR(64),
                    hit_count INTEGER NOT NULL DEFAULT 0,
                    level INTEGER DEFAULT 3,
                    parent_segment_id VARCHAR(255),
                    summary TEXT,
                    page_start INTEGER,
                    page_end INTEGER,
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    disabled_at TIMESTAMPTZ,
                    disabled_by VARCHAR(255),
                    status VARCHAR(50) DEFAULT 'completed',
                    word_count INTEGER DEFAULT 0,
                    keywords JSONB DEFAULT '[]',
                    answer TEXT,
                    image_url TEXT,
                    image_attachment_id VARCHAR(255),
                    image_filename VARCHAR(512),
                    image_media_type VARCHAR(100),
                    image_file_size BIGINT,
                    index_node_id VARCHAR(255),
                    index_node_hash VARCHAR(255),
                    created_by VARCHAR(255),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    UNIQUE (document_id, content_type, position)
                );
                CREATE TABLE document_versions (
                    version_id VARCHAR(255) PRIMARY KEY,
                    document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id) ON DELETE CASCADE,
                    version_number INTEGER NOT NULL,
                    content TEXT NOT NULL,
                    content_hash VARCHAR(64) NOT NULL,
                    confluence_version INTEGER,
                    confluence_updated_at TIMESTAMPTZ,
                    title VARCHAR(512),
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    word_count INTEGER NOT NULL DEFAULT 0,
                    change_type VARCHAR(50) NOT NULL,
                    change_reason TEXT,
                    changed_by VARCHAR(255),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    UNIQUE(document_id, version_number)
                );
                CREATE TABLE document_summaries (
                    document_id VARCHAR(255) PRIMARY KEY REFERENCES documents(document_id) ON DELETE CASCADE,
                    summary TEXT NOT NULL DEFAULT '',
                    keywords JSONB NOT NULL DEFAULT '[]'::jsonb,
                    topics JSONB NOT NULL DEFAULT '[]'::jsonb,
                    vector_id VARCHAR(255),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                INSERT INTO datasets (dataset_id, tenant_id)
                VALUES ('dataset-a', 'tenant-a');
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
    content: str = "persisted alpha\npersisted beta",
    process_rule_id: str | None = None,
    updated_at: datetime | None = None,
    parsing_started_at: datetime | None = None,
    splitting_started_at: datetime | None = None,
    indexing_started_at: datetime | None = None,
) -> None:
    async with pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO documents (
                document_id, dataset_id, status, enabled, archived, metadata,
                content, process_rule_id,
                updated_at, parsing_started_at, splitting_started_at,
                indexing_started_at
            )
            VALUES (
                $1, $2, $3, $4, $5, $6::jsonb, $7, $8, $9, $10, $11, $12
            )
            ON CONFLICT (document_id) DO UPDATE
            SET status = $3, enabled = $4, archived = $5, metadata = $6::jsonb,
                content = $7, process_rule_id = $8,
                updated_at = $9, parsing_started_at = $10,
                splitting_started_at = $11, indexing_started_at = $12
            """,
            document_id,
            dataset_id,
            status,
            enabled,
            archived,
            json.dumps(metadata or {}),
            content,
            process_rule_id,
            updated_at or datetime.now(timezone.utc),
            parsing_started_at,
            splitting_started_at,
            indexing_started_at,
        )


async def _get_document_row(
    pool: asyncpg.Pool, document_id: str
) -> dict[str, Any]:
    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            "SELECT * FROM documents WHERE document_id = $1", document_id
        )
    assert row is not None, f"document {document_id} missing"
    metadata = row["metadata"]
    if isinstance(metadata, str):
        metadata = json.loads(metadata)
    return {**dict(row), "metadata": metadata or {}}


@pytest.mark.asyncio
async def test_hierarchy_summary_writes_jsonb_on_live_postgres(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(pool, document_id="doc-summary")

    assert await database.save_document_summary({
        "document_id": "doc-summary", "summary": "one fact",
        "keywords": ["alpha", "beta"], "topics": ["topic"],
        "vector_id": str(uuid.uuid4()),
    })

    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            "SELECT keywords, topics FROM document_summaries WHERE document_id = $1",
            "doc-summary",
        )
    assert row is not None
    assert json.loads(row["keywords"]) == ["alpha", "beta"]
    assert json.loads(row["topics"]) == ["topic"]


@pytest.mark.asyncio
async def test_special_publication_pg_commit_and_crash_recovery(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    generation = str(uuid.uuid4())
    old_point, candidate_point = str(uuid.uuid4()), str(uuid.uuid4())
    await _put_document(
        pool, document_id="doc-special", status="indexing", content="old body",
        metadata={DOCUMENT_PIPELINE_EXECUTION_KEY: generation,
                  "original_file_key": "source/original.pdf"},
    )
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE datasets SET collection_name = 'special_base', "
            "index_config = '{}'::jsonb WHERE dataset_id = 'dataset-a'",
        )
        await conn.execute(
            """INSERT INTO document_pipeline_executions
               (execution_id, document_id, dataset_id, action, status)
               VALUES ($1, 'doc-special', 'dataset-a', 'reprocess', 'running')""",
            generation,
        )

    class Vectors:
        def __init__(self) -> None:
            self.points = {"special_base": {old_point}}
            self.fail_old_cleanup = True

        async def document_point_ids_by_collection(self, **_kwargs: Any) -> dict[str, list[str]]:
            return {name: sorted(ids) for name, ids in self.points.items() if ids}

        async def ensure_collection(self, **kwargs: Any) -> str:
            return str(kwargs["collection_name"])

        async def upsert(self, name: str, points: list[PointStruct], **_kwargs: Any) -> None:
            self.points.setdefault(name, set()).update(str(point.id) for point in points)

        async def delete_document_points_by_ids(
            self, name: str, ids: list[str], **_kwargs: Any,
        ) -> None:
            if self.fail_old_cleanup and old_point in ids:
                self.fail_old_cleanup = False
                raise RuntimeError("injected interruption after PG authority commit")
            self.points.setdefault(name, set()).difference_update(ids)

    vectors = Vectors()
    service = SimpleNamespace(db=database, vector_store=vectors,
                              image_storage_service=SimpleNamespace(),
                              bm25_v2_lifecycle_service=None)
    coordinator = SpecialPublicationCoordinator(service)
    dataset = await database.get_dataset("dataset-a")
    assert dataset is not None
    plan = SimpleNamespace(
        generation_id=generation, document_id="doc-special", dataset_id="dataset-a",
        content="new body", source_hash=hashlib.sha256(b"new body").hexdigest(),
        points_by_collection={"special_base": [PointStruct(
            id=candidate_point, vector=[0.1, 0.2],
            payload={"document_id": "doc-special", "text": "new body"},
        )]},
        segment_rows=[{
            "segment_id": candidate_point, "dataset_id": "dataset-a",
            "document_id": "doc-special", "position": 0, "text": "new body",
            "content_type": "text", "vector_id": candidate_point,
            "enabled": False, "status": "indexing", "metadata": {},
        }],
        segment_ids=[candidate_point], object_manifest=[],
        summary_row={
            "document_id": "doc-special", "summary": "new body",
            "keywords": ["new"], "topics": ["body"], "vector_id": candidate_point,
        },
    )
    await database.record_special_publication_manifest(
        generation, "doc-special", "dataset-a", generation,
        source_hash=plan.source_hash, planned_object_keys=[],
    )

    with pytest.raises(RuntimeError, match="injected interruption"):
        await coordinator.publish(plan, dataset, generation, "old body")
    fenced = await database.get_dataset("dataset-a")
    assert fenced is not None and fenced["content_revision"] < 0
    assert (await _get_document_row(pool, "doc-special"))["content"] == "new body"
    owner = await database.get_active_special_publication_for_dataset("dataset-a")
    assert owner["execution_id"] == generation and owner["phase"] == "committed"
    assert await coordinator.recover_unfinished(fenced)
    assert (await database.get_dataset("dataset-a"))["content_revision"] > 0
    assert vectors.points["special_base"] == {candidate_point}
    assert (await _get_execution_row(pool, generation))["status"] == "completed"
    final = await _get_document_row(pool, "doc-special")
    assert final["current_version"] == 2 and final["version_count"] == 2
    summary = await database.get_document_summary("doc-special")
    assert summary is not None and summary["vector_id"] == candidate_point


async def _get_execution_row(
    pool: asyncpg.Pool,
    execution_id: str,
) -> dict[str, Any]:
    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            "SELECT * FROM document_pipeline_executions WHERE execution_id = $1",
            execution_id,
        )
    assert row is not None, f"execution {execution_id} missing"
    result = dict(row)
    for key in ("input_snapshot", "manifest"):
        if isinstance(result.get(key), str):
            result[key] = json.loads(result[key])
    return result


async def _seed_interrupted_generation(
    pool: asyncpg.Pool,
    *,
    action: str,
    document_id: str = "doc-interrupted",
) -> tuple[str, str | None, dict[str, Any]]:
    execution_id = f"exec-{action}-{uuid.uuid4().hex[:12]}"
    rule_id = None if action == "reembed" else f"rule-{action}-{uuid.uuid4().hex[:12]}"
    snapshot = {
        "index_config": {
            "chunking": {"mode": "automatic", "chunk_size": 300},
            "snapshot_marker": "pinned-before-crash",
        },
        "chunking": {"mode": "automatic", "chunk_size": 300},
        "processing_mode": "text_only",
        "source_input": {"kind": "text", "revision": "source-v1"},
    }
    metadata: dict[str, Any] = {
        "processing_mode": "text_only",
        DOCUMENT_PIPELINE_EXECUTION_KEY: execution_id,
    }
    if action != "ingest":
        metadata[DOCUMENT_INGEST_ACTION_KEY] = action
    await _put_document(
        pool,
        document_id=document_id,
        status="indexing",
        metadata=metadata,
        process_rule_id=rule_id,
        updated_at=datetime.now(timezone.utc) - timedelta(hours=2),
        indexing_started_at=datetime.now(timezone.utc) - timedelta(hours=2),
    )
    async with pool.acquire() as conn, conn.transaction():
        if rule_id is not None:
            await conn.execute(
                """
                INSERT INTO dataset_process_rules (id, dataset_id, mode, rules)
                VALUES ($1, 'dataset-a', 'automatic', $2::jsonb)
                """,
                rule_id,
                json.dumps(snapshot),
            )
        await conn.execute(
            """
            INSERT INTO document_pipeline_executions (
                execution_id, document_id, dataset_id, action,
                trigger_source, process_rule_id, input_snapshot, status
            )
            VALUES ($1, $2, 'dataset-a', $3, 'api', $4, $5::jsonb, 'running')
            """,
            execution_id,
            document_id,
            action,
            rule_id,
            json.dumps(snapshot),
        )
        await conn.executemany(
            """
            INSERT INTO segments (
                segment_id, dataset_id, document_id, position, text,
                token_count, vector_id, content_hash, enabled, status
            )
            VALUES ($1, 'dataset-a', $2, $3, $4, 2, $1, $5, TRUE, 'completed')
            """,
            [
                ("seg-persisted-0", document_id, 0, "persisted alpha", "hash-alpha"),
                ("seg-persisted-1", document_id, 1, "persisted beta", "hash-beta"),
            ],
        )
        await conn.execute(
            """
            UPDATE datasets
            SET index_config = $1::jsonb
            WHERE dataset_id = 'dataset-a'
            """,
            json.dumps(
                {
                    "chunking": {"mode": "automatic", "chunk_size": 999},
                    "snapshot_marker": "live-config-must-not-win",
                }
            ),
        )
    return execution_id, rule_id, snapshot


@pytest.mark.asyncio
async def test_claim_pins_action_and_execution_on_queued_row(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(
        pool,
        document_id="doc-a",
        parsing_started_at=datetime.now(timezone.utc),
        splitting_started_at=datetime.now(timezone.utc),
        indexing_started_at=datetime.now(timezone.utc),
        metadata={
            DOCUMENT_INGEST_ACTION_KEY: "retry",
            DOCUMENT_RECOVER_STAGE_KEY: "parsing",
            DOCUMENT_PIPELINE_EXECUTION_KEY: "stale-exec",
            "user_key": "kept",
        },
    )

    assert (
        await database.claim_document_for_enqueue(
            "dataset-a", "doc-a", action="reembed", execution_id="exec-9"
        )
        is True
    )

    row = await _get_document_row(pool, "doc-a")
    assert row["status"] == "waiting"
    assert row["metadata"][DOCUMENT_INGEST_ACTION_KEY] == "reembed"
    assert row["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY] == "exec-9"
    assert row["parsing_started_at"] is None
    assert row["splitting_started_at"] is None
    assert row["indexing_started_at"] is None
    # A verb can never inherit stale replay state from a previous generation.
    assert DOCUMENT_RECOVER_STAGE_KEY not in row["metadata"]
    assert row["metadata"]["user_key"] == "kept"


@pytest.mark.asyncio
async def test_claim_pins_recover_stage_only_with_recover_action(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(pool, document_id="doc-a")

    assert (
        await database.claim_document_for_enqueue(
            "dataset-a",
            "doc-a",
            action="recover",
            recover_stage="indexing",
        )
        is True
    )
    row = await _get_document_row(pool, "doc-a")
    assert row["metadata"][DOCUMENT_INGEST_ACTION_KEY] == "recover"
    assert row["metadata"][DOCUMENT_RECOVER_STAGE_KEY] == "indexing"

    with pytest.raises(ValueError):
        await database.claim_document_for_enqueue(
            "dataset-a", "doc-a", action="reprocess", recover_stage="indexing"
        )
    with pytest.raises(ValueError):
        await database.claim_document_for_enqueue(
            "dataset-a", "doc-a", action="explode"
        )
    with pytest.raises(ValueError):
        await database.claim_document_for_enqueue(
            "dataset-a", "doc-a", action="recover", recover_stage="exploding"
        )


@pytest.mark.asyncio
async def test_plain_claim_strips_stale_verb_markers(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(
        pool,
        document_id="doc-a",
        metadata={
            DOCUMENT_INGEST_ACTION_KEY: "reembed",
            DOCUMENT_RECOVER_STAGE_KEY: "indexing",
            DOCUMENT_PIPELINE_EXECUTION_KEY: "old-exec",
        },
    )

    assert await database.claim_document_for_enqueue("dataset-a", "doc-a") is True

    row = await _get_document_row(pool, "doc-a")
    assert row["status"] == "waiting"
    for key in (
        DOCUMENT_INGEST_ACTION_KEY,
        DOCUMENT_RECOVER_STAGE_KEY,
        DOCUMENT_PIPELINE_EXECUTION_KEY,
    ):
        assert key not in row["metadata"]


@pytest.mark.asyncio
async def test_claim_rejects_in_flight_and_upload_marked_documents(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(pool, document_id="doc-busy", status="indexing")
    await _put_document(
        pool,
        document_id="doc-upload",
        metadata={"_document_upload_generation": {"status": "running"}},
    )

    assert (
        await database.claim_document_for_enqueue(
            "dataset-a", "doc-busy", action="reembed"
        )
        is False
    )
    assert (
        await database.claim_document_for_enqueue(
            "dataset-a", "doc-upload", action="reprocess"
        )
        is False
    )


@pytest.mark.asyncio
async def test_waiting_row_refuses_conflicting_verb_reclaim(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    """A queued generation's pinned verb can never be swapped or stripped.

    A waiting row carrying a verb marker belongs to that verb: a re-claim
    with a different verb, or a plain claim that would strip the marker, is
    refused at claim level. Only an identical verb re-pin is idempotent.
    """

    database, pool = verb_world
    await _put_document(
        pool,
        document_id="doc-queued",
        status="waiting",
        metadata={
            DOCUMENT_INGEST_ACTION_KEY: "reprocess",
            DOCUMENT_PIPELINE_EXECUTION_KEY: "exec-original",
        },
    )

    # A different verb cannot steal the queued generation.
    assert (
        await database.claim_document_for_enqueue(
            "dataset-a", "doc-queued", action="retry", execution_id="exec-thief"
        )
        is False
    )
    # A plain claim cannot demote/strip it either.
    assert await database.claim_document_for_enqueue("dataset-a", "doc-queued") is False

    row = await _get_document_row(pool, "doc-queued")
    assert row["status"] == "waiting"
    assert row["metadata"][DOCUMENT_INGEST_ACTION_KEY] == "reprocess"
    assert row["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY] == "exec-original"

    # An identical verb may be retried only by its original execution.
    assert (
        await database.claim_document_for_enqueue(
            "dataset-a", "doc-queued", action="reprocess", execution_id="exec-repin"
        )
        is False
    )
    row = await _get_document_row(pool, "doc-queued")
    assert row["status"] == "waiting"
    assert row["metadata"][DOCUMENT_INGEST_ACTION_KEY] == "reprocess"
    assert row["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY] == "exec-original"
    assert await database.claim_document_for_enqueue(
        "dataset-a", "doc-queued", action="reprocess", execution_id="exec-original",
    )


@pytest.mark.asyncio
async def test_unmarked_waiting_row_accepts_any_verb(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    """The re-claim guard must not produce false positives on bulk rows."""

    database, pool = verb_world
    await _put_document(pool, document_id="doc-bulk", status="waiting")

    assert (
        await database.claim_document_for_enqueue(
            "dataset-a", "doc-bulk", action="reembed", execution_id="exec-1"
        )
        is True
    )
    row = await _get_document_row(pool, "doc-bulk")
    assert row["status"] == "waiting"
    assert row["metadata"][DOCUMENT_INGEST_ACTION_KEY] == "reembed"


@pytest.mark.asyncio
async def test_replay_claim_pins_its_own_rule_and_cannot_replace_recovery_owner(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(
        pool, document_id="doc-owned", status="completed", process_rule_id="old-rule",
    )
    async with pool.acquire() as conn:
        for execution_id, rule_id in (("exec-original", "rule-original"), ("exec-other", "rule-other")):
            await conn.execute(
                "INSERT INTO document_pipeline_executions "
                "(execution_id, document_id, dataset_id, action, process_rule_id, input_snapshot) "
                "VALUES ($1, 'doc-owned', 'dataset-a', 'reprocess', $2, '{\"chunking\":{}}'::jsonb)",
                execution_id, rule_id,
            )
    # Two requests can record and pin before either claims; the winning
    # execution's rule must be installed atomically with its queue ownership.
    assert await database.pin_document_process_rule("doc-owned", "rule-other", idle_only=True)
    assert await database.claim_document_for_enqueue(
        "dataset-a", "doc-owned", action="reprocess",
        execution_id="exec-original", pin_execution_rule=True,
    )
    row = await _get_document_row(pool, "doc-owned")
    assert row["process_rule_id"] == "rule-original"
    assert row["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY] == "exec-original"
    assert not await database.pin_document_process_rule(
        "doc-owned", "rule-other", idle_only=True,
    )

    # Preparing recovery temporarily uses waiting with no verb marker, but
    # retains the original running ledger link. A fresh request cannot steal it.
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE documents SET metadata = metadata - $2::text WHERE document_id = $1",
            "doc-owned", DOCUMENT_INGEST_ACTION_KEY,
        )
    assert not await database.claim_document_for_enqueue(
        "dataset-a", "doc-owned", action="reprocess",
        execution_id="exec-other", pin_execution_rule=True,
    )
    row = await _get_document_row(pool, "doc-owned")
    assert row["process_rule_id"] == "rule-original"
    assert row["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY] == "exec-original"


@pytest.mark.asyncio
async def test_route_submission_commits_execution_and_claim_or_neither(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(pool, document_id="doc-atomic", status="completed")
    service = SimpleNamespace(db=database)
    worker = SimpleNamespace(enqueue_claimed=AsyncMock())

    execution_id = await _submit_route_execution(
        service, worker, "dataset-a", "doc-atomic", action="reprocess",
    )
    row = await _get_document_row(pool, "doc-atomic")
    assert row["status"] == "waiting"
    assert row["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY] == execution_id
    assert row["process_rule_id"]
    worker.enqueue_claimed.assert_awaited_once_with("dataset-a", "doc-atomic")

    with pytest.raises(HTTPException) as duplicate:
        await _submit_route_execution(
            service, worker, "dataset-a", "doc-atomic", action="reprocess",
        )
    assert duplicate.value.status_code == 409
    async with pool.acquire() as conn:
        assert await conn.fetchval(
            "SELECT count(*) FROM document_pipeline_executions WHERE document_id = $1",
            "doc-atomic",
        ) == 1
        await conn.execute(
            "UPDATE documents SET status = 'completed', metadata = '{}'::jsonb "
            "WHERE document_id = $1", "doc-atomic",
        )
        await conn.execute("UPDATE datasets SET is_archived = TRUE WHERE dataset_id = 'dataset-a'")
    with pytest.raises(HTTPException) as archived:
        await _submit_route_execution(
            service, worker, "dataset-a", "doc-atomic", action="reprocess",
        )
    assert archived.value.status_code == 409
    async with pool.acquire() as conn:
        assert await conn.fetchval(
            "SELECT count(*) FROM document_pipeline_executions WHERE document_id = $1",
            "doc-atomic",
        ) == 1


@pytest.mark.asyncio
async def test_worker_record_and_link_rollback_together_on_link_failure(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(
        pool, document_id="doc-worker-atomic", status="parsing",
        metadata={"processing_mode": "text_only"},
    )
    worker = KnowledgeWorker(SimpleNamespace(
        db=database, settings=SimpleNamespace(knowledge=None),
    ))
    database.link_pipeline_execution = AsyncMock(side_effect=RuntimeError("link failed"))

    async with pool.acquire() as conn:
        result = await worker._ensure_pipeline_execution(
            KnowledgeIngestTask("dataset-a", "doc-worker-atomic"),
            "ingest", "", connection=conn,
        )
        assert result == ""
        assert await conn.fetchval(
            "SELECT count(*) FROM document_pipeline_executions WHERE document_id = $1",
            "doc-worker-atomic",
        ) == 0


@pytest.mark.asyncio
async def test_interactive_verbs_dispatch_ahead_of_bulk_backlog(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    now = datetime.now(timezone.utc)
    # Bulk rows are older, but still inside the finite interactive bias window.
    await _put_document(
        pool,
        document_id="doc-bulk-1",
        status="waiting",
        updated_at=now - timedelta(minutes=4),
    )
    await _put_document(
        pool,
        document_id="doc-bulk-2",
        status="waiting",
        updated_at=now - timedelta(minutes=3),
    )
    await _put_document(
        pool,
        document_id="doc-verb-late",
        status="waiting",
        metadata={DOCUMENT_INGEST_ACTION_KEY: "reprocess"},
        updated_at=now - timedelta(minutes=1),
    )
    await _put_document(
        pool,
        document_id="doc-verb-early",
        status="waiting",
        metadata={DOCUMENT_INGEST_ACTION_KEY: "reembed"},
        updated_at=now - timedelta(minutes=2),
    )

    rows = await database.list_queued_documents(limit=100)
    order = [row["document_id"] for row in rows]

    assert order == [
        "doc-verb-early",
        "doc-verb-late",
        "doc-bulk-1",
        "doc-bulk-2",
    ]
    assert await database.count_queued_documents() == 4


@pytest.mark.asyncio
async def test_active_bm25_v2_rows_enqueue_dispatch_and_claim(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    async with pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE datasets
            SET index_config = $1::jsonb
            WHERE dataset_id = 'dataset-a'
            """,
            json.dumps(
                {
                    "retrieval": {
                        "lexical": {
                            "active_version": "bm25_v2",
                            "bm25_v2": {"shadow_write_enabled": True},
                        }
                    }
                }
            ),
        )
    await _put_document(
        pool,
        document_id="doc-active-v2",
        status="completed",
    )

    assert await database.claim_document_for_enqueue(
        "dataset-a",
        "doc-active-v2",
        action="reembed",
    )
    rows = await database.list_queued_documents(limit=100)
    assert [row["document_id"] for row in rows] == ["doc-active-v2"]
    assert await database.count_queued_documents() == 1
    assert await database.claim_queued_document_for_processing(
        "dataset-a",
        "doc-active-v2",
    )


@pytest.mark.asyncio
async def test_aged_bulk_overtakes_a_continuous_interactive_stream(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    """Finite age bias: new tenant-A repairs cannot starve old tenant-A bulk."""

    database, pool = verb_world
    now = datetime.now(timezone.utc)
    await _put_document(
        pool,
        document_id="doc-aged-bulk",
        status="waiting",
        updated_at=now - timedelta(minutes=6),
    )
    for index in range(12):
        await _put_document(
            pool,
            document_id=f"doc-interactive-{index:02d}",
            status="waiting",
            metadata={DOCUMENT_INGEST_ACTION_KEY: "reembed"},
            updated_at=now - timedelta(seconds=12 - index),
        )

    first = await database.list_queued_documents(limit=1)
    assert [row["document_id"] for row in first] == ["doc-aged-bulk"]
    assert first[0]["dispatch_lane"] == "bulk"


@pytest.mark.asyncio
async def test_tenant_cursor_round_robins_one_row_poll_batches(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    """A short poll window rotates tenants even while prior rows stay queued."""

    database, pool = verb_world
    async with pool.acquire() as conn:
        await conn.execute(
            "INSERT INTO datasets (dataset_id, tenant_id) "
            "VALUES ('dataset-b', 'tenant-b')"
        )
    now = datetime.now(timezone.utc)
    await _put_document(
        pool,
        document_id="doc-a",
        status="waiting",
        metadata={DOCUMENT_INGEST_ACTION_KEY: "reembed"},
        updated_at=now,
    )
    await _put_document(
        pool,
        document_id="doc-b",
        dataset_id="dataset-b",
        status="waiting",
        metadata={DOCUMENT_INGEST_ACTION_KEY: "reembed"},
        updated_at=now,
    )

    first = await database.list_queued_documents(limit=1)
    assert first[0]["tenant_id"] == "tenant-a"
    second = await database.list_queued_documents(
        limit=1,
        tenant_cursor=first[0]["tenant_id"],
    )
    assert second[0]["tenant_id"] == "tenant-b"


@pytest.mark.asyncio
async def test_two_tenant_reembeds_and_bulk_backlog_never_starve_or_double_claim(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    """§6.4 scenario 6: concurrent two-document reembed + bulk import.

    Two tenants' interactive reembeds land on top of a (deliberately older)
    bulk-import backlog. The dual-queue contract must hold across tenants:
    both interactive verbs dispatch ahead of the backlog (neither tenant
    starves behind the other's import), the consumer CAS admits exactly one
    claimant per document (no double-claim), and the bulk lane survives
    intact afterwards in FIFO order (the backlog is starved by nothing but
    its own age)."""
    database, pool = verb_world
    now = datetime.now(timezone.utc)

    # A second tenant with its own dataset.
    async with pool.acquire() as conn:
        await conn.execute(
            "INSERT INTO datasets (dataset_id, tenant_id) "
            "VALUES ('dataset-b', 'tenant-b')"
        )

    # Tenant-a bulk is older but still inside the interactive bias window.
    await _put_document(
        pool,
        document_id="doc-bulk-1",
        status="waiting",
        updated_at=now - timedelta(minutes=6),
    )
    await _put_document(
        pool,
        document_id="doc-bulk-2",
        status="waiting",
        updated_at=now - timedelta(minutes=5),
    )
    # Interactive reembeds from BOTH tenants arrive much later.
    await _put_document(
        pool,
        document_id="doc-reembed-a",
        status="waiting",
        metadata={DOCUMENT_INGEST_ACTION_KEY: "reembed"},
        updated_at=now - timedelta(minutes=3),
    )
    await _put_document(
        pool,
        document_id="doc-reembed-b",
        dataset_id="dataset-b",
        status="waiting",
        metadata={DOCUMENT_INGEST_ACTION_KEY: "reembed"},
        updated_at=now - timedelta(minutes=2),
    )

    # Dispatch: both tenants' interactive verbs jump the bulk backlog.
    order = [row["document_id"] for row in await database.list_queued_documents(limit=100)]
    assert order == ["doc-reembed-a", "doc-reembed-b", "doc-bulk-1", "doc-bulk-2"]

    # Concurrent consumer race on tenant-a's reembed: exactly ONE claimant
    # wins the queued->processing CAS; the loser gets False, never a
    # duplicate generation.
    race = await asyncio.gather(
        database.claim_queued_document_for_processing("dataset-a", "doc-reembed-a"),
        database.claim_queued_document_for_processing("dataset-a", "doc-reembed-a"),
    )
    assert sorted(race) == [False, True]

    # Tenant-b's reembed still claims cleanly: tenant-a's race starved it of
    # nothing.
    assert (
        await database.claim_queued_document_for_processing(
            "dataset-b", "doc-reembed-b"
        )
        is True
    )

    # The bulk lane survives intact, FIFO, still dispatchable.
    remaining = [
        row["document_id"] for row in await database.list_queued_documents(limit=100)
    ]
    assert remaining == ["doc-bulk-1", "doc-bulk-2"]
    assert await database.count_queued_documents() == 2


@pytest.mark.asyncio
@pytest.mark.parametrize("died_stage", ["parsing", "splitting", "indexing"])
async def test_claim_stuck_publishes_snapshot_identical_recovery_generation(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool], died_stage: str
) -> None:
    database, pool = verb_world
    old_execution_id, rule_id, snapshot = await _seed_interrupted_generation(
        pool,
        action="ingest",
        document_id="doc-dead",
    )
    stage_column = {
        "parsing": "parsing_started_at",
        "splitting": "splitting_started_at",
        "indexing": "indexing_started_at",
    }[died_stage]
    async with pool.acquire() as conn:
        await conn.execute(
            f"""
            UPDATE documents
            SET status = $1,
                {stage_column} = NOW() - INTERVAL '2 hours',
                updated_at = NOW() - INTERVAL '2 hours'
            WHERE document_id = 'doc-dead'
            """,
            died_stage,
        )

    rows = await database.claim_stuck_documents(stuck_threshold_minutes=1)

    assert [row["document_id"] for row in rows] == ["doc-dead"]
    assert rows[0]["old_status"] == died_stage
    row = await _get_document_row(pool, "doc-dead")
    assert row["status"] == "waiting"
    assert row["metadata"][DOCUMENT_INGEST_ACTION_KEY] == "recover"
    assert row["metadata"][DOCUMENT_RECOVER_STAGE_KEY] == died_stage
    recovery_execution_id = row["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY]
    assert recovery_execution_id != old_execution_id

    interrupted = await _get_execution_row(pool, old_execution_id)
    assert interrupted["status"] == "error"
    assert "superseded by crash recovery" in interrupted["error"]
    recovered = await _get_execution_row(pool, recovery_execution_id)
    assert recovered["action"] == "recover"
    assert recovered["trigger_source"] == "recover"
    assert recovered["process_rule_id"] == rule_id
    assert recovered["input_snapshot"] == snapshot
    assert recovered["manifest"] == {
        "recovered_from_execution_id": old_execution_id,
        "recovered_from_stage": died_stage,
    }
    # The fresh waiting generation is not duplicated by another recovery pass.
    assert await database.claim_stuck_documents(stuck_threshold_minutes=1) == []


@pytest.mark.asyncio
@pytest.mark.parametrize("action", ["ingest", "reprocess", "retry"])
async def test_repeated_indexing_recovery_replays_pinned_pipeline(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool], action: str
) -> None:
    database, pool = verb_world
    old_execution_id, rule_id, snapshot = await _seed_interrupted_generation(
        pool,
        action=action,
    )

    first = await database.claim_stuck_documents(stuck_threshold_minutes=1)
    assert [row["document_id"] for row in first] == ["doc-interrupted"]
    first_row = await _get_document_row(pool, "doc-interrupted")
    first_recovery_id = first_row["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY]
    assert first_row["metadata"][DOCUMENT_INGEST_ACTION_KEY] == "recover"
    assert first_row["metadata"][DOCUMENT_RECOVER_STAGE_KEY] == "indexing"

    # Simulate another hard death after the first recovery reached indexing.
    async with pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE documents
            SET status = 'indexing',
                updated_at = NOW() - INTERVAL '2 hours',
                indexing_started_at = NOW() - INTERVAL '2 hours'
            WHERE document_id = 'doc-interrupted'
            """
        )
    second = await database.claim_stuck_documents(stuck_threshold_minutes=1)
    assert [row["document_id"] for row in second] == ["doc-interrupted"]
    second_row = await _get_document_row(pool, "doc-interrupted")
    second_recovery_id = second_row["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY]
    assert second_recovery_id not in {old_execution_id, first_recovery_id}

    for closed_id in (old_execution_id, first_recovery_id):
        assert (await _get_execution_row(pool, closed_id))["status"] == "error"
    latest = await _get_execution_row(pool, second_recovery_id)
    assert latest["status"] == "running"
    assert latest["action"] == "recover"
    assert latest["process_rule_id"] == rule_id
    assert latest["input_snapshot"] == snapshot

    class RecoveryService:
        def __init__(self) -> None:
            self.db = database
            self.settings = SimpleNamespace(
                knowledge=SimpleNamespace(
                    large_file_threshold=1024 * 1024,
                    pdf_split_enabled=True,
                    pdf_split_max_size_bytes=1024,
                    pdf_split_min_pages_per_part=1,
                    ocr_strategy="hybrid",
                    document_recovery_interval_seconds=1,
                    document_stuck_threshold_minutes=1,
                    document_worker_concurrency=1,
                )
            )
            self.parser_calls = 0
            self.reembed_segment_ids: list[str] = []
            self.vector_store = SimpleNamespace(
                bm25_v2_enabled=True,
                document_has_points=AsyncMock(return_value=False),
                document_has_specialized_points=AsyncMock(return_value=False),
            )

        async def ingest_document(self, *_args: Any, **_kwargs: Any) -> list[str]:
            self.parser_calls += 1
            assert _kwargs["index_config_override"] == snapshot["index_config"]
            await database.update_document_status(
                "doc-interrupted",
                status="completed",
                progress=100,
            )
            return ["seg-new-candidate"]

        async def reembed_document(
            self,
            _dataset_id: str,
            _document_id: str,
        ) -> list[str]:
            raise AssertionError("indexing recovery cannot assume old segments are this candidate")

    service = RecoveryService()
    worker = KnowledgeWorker(service)  # type: ignore[arg-type]
    task = KnowledgeIngestTask("dataset-a", "doc-interrupted")
    async with database.document_index_update_lease(
        "dataset-a",
        "doc-interrupted",
    ) as connection:
        assert await database.claim_queued_document_for_processing(
            "dataset-a",
            "doc-interrupted",
            connection=connection,
        )
        execution_id = await worker._ensure_pipeline_execution(
            task,
            "recover",
            second_recovery_id,
            connection=connection,
        )
        assert execution_id == second_recovery_id
        await worker._prepare_document_generation(task, connection=connection)
        manifest = await worker._process_task(task, connection=connection)

    assert manifest == ["seg-new-candidate"]
    assert service.reembed_segment_ids == []
    assert service.parser_calls == 1
    await worker._finish_pipeline_execution(
        second_recovery_id,
        status="completed",
        manifest=manifest,
    )
    completed = await _get_execution_row(pool, second_recovery_id)
    assert completed["status"] == "completed"
    assert completed["manifest"]["segment_ids"] == manifest


@pytest.mark.asyncio
async def test_active_bm25_v2_stuck_recovery_requires_enabled_runtime(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _seed_interrupted_generation(pool, action="reprocess")
    async with pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE datasets SET index_config = $1::jsonb WHERE dataset_id = 'dataset-a'
            """,
            json.dumps({"retrieval": {"lexical": {"active_version": "bm25_v2"}}}),
        )

    assert await database.claim_stuck_documents(stuck_threshold_minutes=1) == []
    claimed = await database.claim_stuck_documents(
        stuck_threshold_minutes=1,
        allow_active_bm25_v2=True,
    )
    assert [row["document_id"] for row in claimed] == ["doc-interrupted"]
    assert (await _get_document_row(pool, "doc-interrupted"))["status"] == "waiting"


@pytest.mark.asyncio
async def test_generic_recovery_cannot_steal_positive_revision_special_preparation(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    execution_id, _rule_id, _snapshot = await _seed_interrupted_generation(
        pool, action="reprocess",
    )
    generation_id = "00000000-0000-4000-8000-000000000099"
    async with pool.acquire() as connection:
        await connection.execute(
            "UPDATE documents SET metadata = metadata || $2::jsonb "
            "WHERE document_id = $1",
            "doc-interrupted",
            json.dumps({"_special_publication_generation_id": generation_id}),
        )

    assert await database.claim_stuck_documents(stuck_threshold_minutes=1) == []
    document = await _get_document_row(pool, "doc-interrupted")
    assert document["status"] == "indexing"
    assert document["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY] == execution_id
    assert document["metadata"]["_special_publication_generation_id"] == generation_id
    assert (await _get_execution_row(pool, execution_id))["status"] == "running"


@pytest.mark.asyncio
async def test_stuck_reembed_renews_consistent_reembed_ledger(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    old_execution_id, _rule_id, snapshot = await _seed_interrupted_generation(
        pool,
        action="reembed",
    )

    rows = await database.claim_stuck_documents(stuck_threshold_minutes=1)

    assert [row["document_id"] for row in rows] == ["doc-interrupted"]
    document = await _get_document_row(pool, "doc-interrupted")
    assert document["metadata"][DOCUMENT_INGEST_ACTION_KEY] == "reembed"
    assert DOCUMENT_RECOVER_STAGE_KEY not in document["metadata"]
    execution_id = document["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY]
    assert execution_id != old_execution_id
    assert (await _get_execution_row(pool, old_execution_id))["status"] == "error"
    renewed = await _get_execution_row(pool, execution_id)
    assert renewed["action"] == "reembed"
    assert renewed["process_rule_id"] is None
    assert renewed["input_snapshot"] == snapshot


@pytest.mark.asyncio
async def test_restore_candidate_and_serving_source_publish_together(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(pool, document_id="doc-restore-version", content="old serving fact")
    async with pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO document_versions (
                version_id, document_id, version_number, content, content_hash, change_type
            ) VALUES ('version-1', 'doc-restore-version', 1, 'old serving fact', $1, 'created')
            """,
            hashlib.sha256(b"old serving fact").hexdigest(),
        )

    candidate_text = "restored historical KITE-42"
    async with pool.acquire() as conn, conn.transaction():
        before_snapshot = await database.create_document_version(
            document_id="doc-restore-version",
            content="old serving fact",
            content_hash=hashlib.sha256(b"old serving fact").hexdigest(),
            change_type="pending_before_restore",
            connection=conn,
            activate=False,
        )
        assert before_snapshot is not None and before_snapshot["version_number"] == 2
        candidate = await database.create_document_version(
            document_id="doc-restore-version",
            content=candidate_text,
            content_hash=hashlib.sha256(candidate_text.encode()).hexdigest(),
            change_type="pending_restore",
            connection=conn,
            activate=False,
        )
        assert candidate is not None and candidate["version_number"] == 3
        await database.update_document_status(
            "doc-restore-version", status="waiting", progress=0, error="", connection=conn,
        )
        assert await database.set_pending_restore_version(
            "doc-restore-version", "dataset-a", 2, 3, "exec-restore", connection=conn,
        )
        await database.update_document_status(
            "doc-restore-version", status="indexing", progress=70, connection=conn,
        )

    assert await database.get_document_version("doc-restore-version", 3) is None
    assert await database.get_document_version("doc-restore-version", 2) is None
    assert (await database.get_document_version("doc-restore-version", 3, include_pending=True))["content"] == candidate_text
    assert await database.get_document_version_count("doc-restore-version") == 1
    assert len(await database.list_document_versions("doc-restore-version")) == 1
    before = await _get_document_row(pool, "doc-restore-version")
    assert before["content"] == "old serving fact"
    assert before["current_version"] == 1
    assert before["version_count"] == 1
    assert before["metadata"][DOCUMENT_PENDING_RESTORE_VERSION_KEY] == {
        "previous_version": 2,
        "candidate_version": 3,
    }

    dataset = await database.get_dataset("dataset-a")
    expected_identity = dataset_ingestion_identity(dataset)
    async with pool.acquire() as conn:
        with pytest.raises(RuntimeError, match="source changed"):
            await database.commit_text_segment_publication(
                dataset_id="dataset-a", document_id="doc-restore-version",
                segment_rows=[], keep_segment_ids=[], staged_segment_ids=[],
                delete_excess=False, expected_ingestion_identity=expected_identity,
                connection=conn, finish_publication=False,
                candidate_content=candidate_text, expected_content="wrong source",
                candidate_version_number=3, previous_version_number=2,
            )
    assert (await _get_document_row(pool, "doc-restore-version"))["content"] == "old serving fact"
    assert await database.get_document_version("doc-restore-version", 3) is None

    async with pool.acquire() as conn:
        await database.commit_text_segment_publication(
            dataset_id="dataset-a", document_id="doc-restore-version",
            segment_rows=[], keep_segment_ids=[], staged_segment_ids=[],
            delete_excess=False, expected_ingestion_identity=expected_identity,
            connection=conn, finish_publication=False,
            candidate_content=candidate_text, expected_content="old serving fact",
            candidate_version_number=3, previous_version_number=2,
            candidate_word_count=3,
        )
    after = await _get_document_row(pool, "doc-restore-version")
    assert after["content"] == candidate_text
    assert after["current_version"] == 3
    assert after["version_count"] == 3
    assert after["word_count"] == 3
    assert DOCUMENT_PENDING_RESTORE_VERSION_KEY not in after["metadata"]
    assert after["metadata"]["_document_restored_source_version"] == 3
    assert (await database.get_document_version("doc-restore-version", 2))["change_type"] == "updated"
    assert (await database.get_document_version("doc-restore-version", 3))["change_type"] == "restored"
    assert await database.get_document_version_count("doc-restore-version") == 3
    async with pool.acquire() as conn:
        with pytest.raises(RuntimeError, match="restore candidate changed"):
            await database.commit_text_segment_publication(
                dataset_id="dataset-a", document_id="doc-restore-version",
                segment_rows=[], keep_segment_ids=[], staged_segment_ids=[],
                delete_excess=False, expected_ingestion_identity=expected_identity,
                connection=conn, finish_publication=False,
                candidate_content=candidate_text, expected_content=candidate_text,
                candidate_version_number=3, previous_version_number=2,
            )
    assert (await _get_document_row(pool, "doc-restore-version"))["version_count"] == 3


@pytest.mark.asyncio
async def test_text_restore_publishes_version_body_and_terminal_in_one_transaction(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(pool, document_id="doc-finalize", status="waiting", content="old body")
    async with pool.acquire() as conn, conn.transaction():
        await conn.execute(
            """INSERT INTO document_versions
               (version_id, document_id, version_number, content, content_hash, change_type)
               VALUES ('v1', 'doc-finalize', 1, 'old body', $1, 'created')""",
            hashlib.sha256(b"old body").hexdigest(),
        )
        before = await database.create_document_version(
            document_id="doc-finalize", content="old body",
            content_hash=hashlib.sha256(b"old body").hexdigest(),
            change_type="pending_before_restore", connection=conn, activate=False,
        )
        candidate = await database.create_document_version(
            document_id="doc-finalize", content="new body",
            content_hash=hashlib.sha256(b"new body").hexdigest(),
            change_type="pending_restore", connection=conn, activate=False,
        )
        assert before["version_number"] == 2 and candidate["version_number"] == 3
        assert await database.set_pending_restore_version(
            "doc-finalize", "dataset-a", 2, 3, "exec-finalize", connection=conn,
        )
        await database.update_document_status(
            "doc-finalize", status="indexing", progress=70, connection=conn,
        )
        await conn.execute("UPDATE datasets SET content_revision = -1 WHERE dataset_id = 'dataset-a'")
    dataset = await database.get_dataset("dataset-a")
    async with pool.acquire() as conn:
        await database.commit_text_segment_publication(
            dataset_id="dataset-a", document_id="doc-finalize",
            segment_rows=[], keep_segment_ids=[], staged_segment_ids=[],
            delete_excess=False, expected_ingestion_identity=dataset_ingestion_identity(dataset),
            connection=conn, finish_publication=True, finalize_document=True,
            candidate_content="new body", expected_content="old body",
            candidate_version_number=3, previous_version_number=2,
        )
    published = await _get_document_row(pool, "doc-finalize")
    assert (published["content"], published["current_version"], published["status"]) == (
        "new body", 3, "completed",
    )
    assert DOCUMENT_PENDING_RESTORE_VERSION_KEY not in published["metadata"]
    for marker in (DOCUMENT_INGEST_ACTION_KEY, DOCUMENT_RECOVER_STAGE_KEY,
                   DOCUMENT_PIPELINE_EXECUTION_KEY):
        assert marker not in published["metadata"]
    assert published["metadata"][DOCUMENT_RESTORED_SOURCE_VERSION_KEY] == 3
    assert (await database.get_dataset("dataset-a"))["content_revision"] > 0


@pytest.mark.asyncio
async def test_claim_stuck_waiting_lifecycle_replay_preserves_queued_verb(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    """Addendum B2 repair promise: a waiting row is a DURABLY queued
    generation. Stuck recovery re-claims it for dispatch but must keep its
    pinned verb, execution link and lifecycle marker untouched — stripping
    them would silently degrade a queued repair into a hash-skipping ingest
    that reports success without repairing. Terminal writes are the only
    legitimate marker cleaners."""

    database, pool = verb_world
    await _put_document(
        pool,
        document_id="doc-restore",
        status="waiting",
        metadata={
            "_document_lifecycle_reindex": {
                "status": "pending",
                "desired_enabled": "true",
                "desired_archived": "false",
            },
            DOCUMENT_INGEST_ACTION_KEY: "reembed",
            DOCUMENT_PIPELINE_EXECUTION_KEY: "old-exec",
            "user_key": "kept",
        },
        updated_at=datetime.now(timezone.utc) - timedelta(hours=2),
    )

    rows = await database.claim_stuck_documents(stuck_threshold_minutes=1)

    assert [row["document_id"] for row in rows] == ["doc-restore"]
    row = await _get_document_row(pool, "doc-restore")
    assert row["status"] == "waiting"
    # The queued verb, its execution link and the lifecycle marker survive.
    assert row["metadata"][DOCUMENT_INGEST_ACTION_KEY] == "reembed"
    assert row["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY] == "old-exec"
    assert row["metadata"]["_document_lifecycle_reindex"]["status"] == "pending"
    assert row["metadata"]["user_key"] == "kept"
    # A waiting row never gains a recover stage: it did not die mid-stage.
    assert DOCUMENT_RECOVER_STAGE_KEY not in row["metadata"]


@pytest.mark.asyncio
@pytest.mark.parametrize("terminal_status", ["completed", "error"])
async def test_terminal_status_writes_retire_verb_markers(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool], terminal_status: str
) -> None:
    database, pool = verb_world
    await _put_document(
        pool,
        document_id="doc-a",
        status="indexing",
        metadata={
            DOCUMENT_INGEST_ACTION_KEY: "reembed",
            DOCUMENT_RECOVER_STAGE_KEY: "indexing",
            DOCUMENT_PIPELINE_EXECUTION_KEY: "exec-1",
            "user_key": "kept",
        },
    )

    await database.update_document_status(
        "doc-a", status=terminal_status, progress=100
    )

    row = await _get_document_row(pool, "doc-a")
    assert row["status"] == terminal_status
    for key in (
        DOCUMENT_INGEST_ACTION_KEY,
        DOCUMENT_RECOVER_STAGE_KEY,
        DOCUMENT_PIPELINE_EXECUTION_KEY,
    ):
        assert key not in row["metadata"]
    assert row["metadata"]["user_key"] == "kept"


@pytest.mark.asyncio
async def test_lifecycle_restore_pin_sets_reembed_verb_and_strips_stale_replay_state(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    """Restore pins the reembed verb under the dataset lifecycle lease.

    The pin is the owner-scoped writer for generations whose waiting-state is
    persisted directly by a lifecycle transition; it must strip stale replay
    state like the claim path while leaving user keys and the lifecycle
    marker untouched.
    """

    database, pool = verb_world
    await _put_document(
        pool,
        document_id="doc-restore",
        status="waiting",
        metadata={
            "_document_lifecycle_reindex": {
                "status": "pending",
                "desired_enabled": "true",
                "desired_archived": "false",
            },
            DOCUMENT_RECOVER_STAGE_KEY: "indexing",
            DOCUMENT_PIPELINE_EXECUTION_KEY: "stale-exec",
            "user_key": "kept",
        },
    )

    assert (
        await database.pin_document_ingest_action(
            "dataset-a", "doc-restore", "reembed"
        )
        is True
    )

    row = await _get_document_row(pool, "doc-restore")
    assert row["status"] == "waiting"
    assert row["metadata"][DOCUMENT_INGEST_ACTION_KEY] == "reembed"
    assert DOCUMENT_RECOVER_STAGE_KEY not in row["metadata"]
    assert DOCUMENT_PIPELINE_EXECUTION_KEY not in row["metadata"]
    # The lifecycle marker and user metadata survive the pin.
    assert row["metadata"]["_document_lifecycle_reindex"]["status"] == "pending"
    assert row["metadata"]["user_key"] == "kept"

    with pytest.raises(ValueError):
        await database.pin_document_ingest_action(
            "dataset-a", "doc-restore", "explode"
        )
    assert (
        await database.pin_document_ingest_action(
            "dataset-a", "doc-missing", "reembed"
        )
        is False
    )


@pytest.mark.asyncio
async def test_execution_log_round_trip_and_single_close(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(pool, document_id="doc-a")

    execution_id = await database.record_pipeline_execution(
        "doc-a",
        "dataset-a",
        action="reprocess",
        input_snapshot={"chunking": {"mode": "automatic"}, "processing_mode": "text_only"},
    )
    assert execution_id

    execution = await database.get_pipeline_execution(execution_id)
    assert execution is not None
    assert execution["action"] == "reprocess"
    assert execution["status"] == "running"
    snapshot = execution["input_snapshot"]
    if isinstance(snapshot, str):
        snapshot = json.loads(snapshot)
    assert snapshot["chunking"] == {"mode": "automatic"}

    assert await database.link_pipeline_execution("doc-a", execution_id) is True
    row = await _get_document_row(pool, "doc-a")
    assert row["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY] == execution_id

    latest = await database.get_latest_pipeline_execution("doc-a")
    assert latest is not None
    assert latest["execution_id"] == execution_id

    assert (
        await database.complete_pipeline_execution(
            execution_id, status="completed", manifest={"segment_ids": ["s1", "s2"]}
        )
        is True
    )
    closed = await database.get_pipeline_execution(execution_id)
    assert closed is not None
    assert closed["status"] == "completed"
    assert closed["completed_at"] is not None
    manifest = closed["manifest"]
    if isinstance(manifest, str):
        manifest = json.loads(manifest)
    assert manifest == {"segment_ids": ["s1", "s2"]}

    # A closed execution can never be closed twice.
    assert (
        await database.complete_pipeline_execution(
            execution_id, status="error", error="late"
        )
        is False
    )


@pytest.mark.asyncio
async def test_execution_log_error_close_records_message(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(pool, document_id="doc-a")

    execution_id = await database.record_pipeline_execution(
        "doc-a", "dataset-a", action="retry"
    )

    assert (
        await database.complete_pipeline_execution(
            execution_id, status="error", error="sweep failed"
        )
        is True
    )
    closed = await database.get_pipeline_execution(execution_id)
    assert closed is not None
    assert closed["status"] == "error"
    assert closed["error"] == "sweep failed"

    with pytest.raises(ValueError):
        await database.complete_pipeline_execution(execution_id, status="running")


@pytest.mark.asyncio
async def test_execution_completion_preserves_crash_recovery_lineage(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(pool, document_id="doc-a")
    execution_id = await database.record_pipeline_execution(
        "doc-a", "dataset-a", action="recover",
    )
    async with pool.acquire() as connection:
        await connection.execute(
            "UPDATE document_pipeline_executions "
            "SET manifest = $2::jsonb WHERE execution_id = $1",
            execution_id,
            json.dumps({"recovered_from_execution_id": "original-execution"}),
        )

    assert await database.complete_pipeline_execution(
        execution_id, status="completed", manifest={"segment_ids": ["segment-a"]},
    )
    execution = await database.get_pipeline_execution(execution_id)
    assert execution is not None
    manifest = execution["manifest"]
    if isinstance(manifest, str):
        manifest = json.loads(manifest)
    assert manifest == {
        "recovered_from_execution_id": "original-execution",
        "segment_ids": ["segment-a"],
    }


async def _dataset_revision(pool: asyncpg.Pool, dataset_id: str = "dataset-a") -> int:
    async with pool.acquire() as conn:
        return int(
            await conn.fetchval(
                "SELECT content_revision FROM datasets WHERE dataset_id = $1",
                dataset_id,
            )
        )


@pytest.mark.asyncio
async def test_restore_activation_write_bumps_dataset_content_revision(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    """PRD T1 unified contract (§885-886) + §6.4 scenario 5: the completed
    write that activates a pending restore advances the dataset revision the
    retrieval cache is keyed on, atomically with the visibility flip."""

    database, pool = verb_world
    await _put_document(
        pool,
        document_id="doc-restore",
        status="indexing",
        enabled=False,
        archived=True,
        metadata={
            DOCUMENT_INGEST_ACTION_KEY: "reembed",
            DOCUMENT_LIFECYCLE_REINDEX_KEY: {
                "status": "pending",
                "desired_enabled": True,
                "desired_archived": False,
            },
        },
    )
    assert await _dataset_revision(pool) == 0

    await database.update_document_status("doc-restore", "completed", progress=100)

    row = await _get_document_row(pool, "doc-restore")
    assert row["status"] == "completed"
    assert row["enabled"] is True
    assert row["archived"] is False
    assert DOCUMENT_LIFECYCLE_REINDEX_KEY not in row["metadata"]
    assert DOCUMENT_INGEST_ACTION_KEY not in row["metadata"]
    assert await _dataset_revision(pool) == 1

    # A plain completed write without a pending restore must not move the
    # revision again: the bump is bound to the visibility change, not to the
    # terminal write itself.
    await database.update_document_status("doc-restore", "completed", progress=100)
    assert await _dataset_revision(pool) == 1


@pytest.mark.asyncio
async def test_failed_restore_write_keeps_revision_and_marker(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(
        pool,
        document_id="doc-failed",
        status="indexing",
        enabled=False,
        archived=True,
        metadata={
            DOCUMENT_INGEST_ACTION_KEY: "reembed",
            DOCUMENT_LIFECYCLE_REINDEX_KEY: {
                "status": "pending",
                "desired_enabled": True,
                "desired_archived": False,
            },
        },
    )

    await database.update_document_status(
        "doc-failed", "error", error="embedding failed"
    )

    row = await _get_document_row(pool, "doc-failed")
    # Fail-closed: hidden under its marker, verb retired, cache untouched.
    assert row["enabled"] is False
    assert row["archived"] is True
    assert row["metadata"][DOCUMENT_LIFECYCLE_REINDEX_KEY]["status"] == "pending"
    assert DOCUMENT_INGEST_ACTION_KEY not in row["metadata"]
    assert await _dataset_revision(pool) == 0


@pytest.mark.asyncio
async def test_explicit_revision_writer_and_soft_delete_guard(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    async with pool.acquire() as conn:
        await conn.execute(
            "INSERT INTO datasets (dataset_id, tenant_id, is_deleted) "
            "VALUES ('dataset-gone', 'tenant-a', TRUE)"
        )

    assert await database.bump_dataset_content_revision("dataset-a") is True
    assert await _dataset_revision(pool) == 1

    async with pool.acquire() as conn:
        assert (
            await database.bump_dataset_content_revision(
                "dataset-a", connection=conn
            )
            is True
        )
    assert await _dataset_revision(pool) == 2

    # Soft-deleted datasets and unknown ids never move a revision.
    assert await database.bump_dataset_content_revision("dataset-gone") is False
    assert await database.bump_dataset_content_revision("dataset-missing") is False
    assert await _dataset_revision(pool) == 2

    with pytest.raises(ValueError):
        await database.bump_dataset_content_revision("   ")


def _segment_row(
    *,
    segment_id: str,
    document_id: str = "doc-a",
    position: int,
    text: str,
    content_type: str = "text",
    status: str = "completed",
    enabled: bool = True,
    content_hash: str | None = None,
) -> dict[str, Any]:
    return {
        "segment_id": segment_id,
        "dataset_id": "dataset-a",
        "document_id": document_id,
        "position": position,
        "text": text,
        "token_count": len(text.split()),
        "content_type": content_type,
        "status": status,
        "enabled": enabled,
        "content_hash": content_hash or f"hash-{segment_id}",
        "index_node_id": f"{document_id}::{content_type}::{position}",
        "index_node_hash": content_hash or f"hash-{segment_id}",
    }


@pytest.mark.asyncio
async def test_special_replay_authority_checks_rows_not_document_enabled(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(pool, document_id="doc-a", status="parsing")
    assert not await database.document_has_completed_segments(
        "dataset-a", "tenant-a", "doc-a"
    )
    assert not await database.document_has_specialized_segments(
        "dataset-a", "tenant-a", "doc-a"
    )
    await database.insert_segments([
        _segment_row(segment_id="staged", position=0, text="candidate", status="indexing", enabled=False),
    ])
    assert not await database.document_has_completed_segments(
        "dataset-a", "tenant-a", "doc-a"
    )
    orphan_l3 = _segment_row(segment_id="orphan-l3", position=1, text="old paragraph")
    orphan_l3["parent_segment_id"] = "missing-parent"
    await database.insert_segments([orphan_l3])
    assert await database.document_has_specialized_segments(
        "dataset-a", "tenant-a", "doc-a"
    )
    await database.insert_segments([
        _segment_row(segment_id="published", position=2, text="published", content_type="image"),
    ])
    await _put_document(pool, document_id="doc-a", status="waiting", enabled=False)
    assert await database.document_has_completed_segments(
        "dataset-a", "tenant-a", "doc-a"
    )
    assert await database.document_has_specialized_segments(
        "dataset-a", "tenant-a", "doc-a"
    )
    assert not await database.document_has_completed_segments(
        "dataset-a", "another-tenant", "doc-a"
    )


@pytest.mark.asyncio
async def test_viewer_pages_hide_disabled_archived_pending_and_staged_content(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(pool, document_id="doc-a")
    await _put_document(pool, document_id="doc-b", enabled=False)
    await _put_document(pool, document_id="doc-c", archived=True)
    await _put_document(
        pool, document_id="doc-d",
        metadata={DOCUMENT_LIFECYCLE_REINDEX_KEY: {"status": "pending"}},
    )
    await database.insert_segments([
        _segment_row(segment_id="visible", position=0, text="public active text"),
        _segment_row(segment_id="staged", position=1, text="private pending text", status="indexing", enabled=False),
        _segment_row(segment_id="disabled", document_id="doc-b", position=0, text="private disabled text"),
        _segment_row(segment_id="archived", document_id="doc-c", position=0, text="private archived text"),
        _segment_row(segment_id="lifecycle", document_id="doc-d", position=0, text="private lifecycle text"),
    ])
    documents = await database.list_documents(
        "dataset-a", active_only=True, tenant_id="tenant-a",
    )
    assert [item["document_id"] for item in documents] == ["doc-a"]
    assert await database.count_documents(
        "dataset-a", active_only=True, tenant_id="tenant-a",
    ) == 1
    assert await database.list_document_ids_by_dataset(
        "dataset-a", active_only=True, tenant_id="tenant-a",
    ) == ["doc-a"]
    segments = await database.list_segments(
        "dataset-a", active_only=True, tenant_id="tenant-a",
    )
    assert [item["segment_id"] for item in segments] == ["visible"]
    assert await database.count_segments(
        "dataset-a", active_only=True, tenant_id="tenant-a",
    ) == 1


@pytest.mark.asyncio
async def test_new_confluence_source_generation_retires_restored_source_marker(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    database, pool = verb_world
    await _put_document(
        pool, document_id="doc-a", status="syncing", content="restored old text",
        metadata={
            DOCUMENT_RESTORED_SOURCE_VERSION_KEY: 4,
            "_confluence_sync_generation": {"generation": "generation-new"},
        },
    )
    async with pool.acquire() as conn, conn.transaction():
        assert await database.prepare_confluence_document_update(
            "doc-a", "dataset-a", generation="generation-new",
            title="new source", content="new Confluence body", confluence_version=5,
            source_metadata={}, connection=conn,
        )
    row = await _get_document_row(pool, "doc-a")
    assert row["content"] == "new Confluence body"
    assert row["status"] == "waiting"
    assert DOCUMENT_RESTORED_SOURCE_VERSION_KEY not in row["metadata"]


async def _segment_rows(pool: asyncpg.Pool, document_id: str) -> list[dict[str, Any]]:
    async with pool.acquire() as conn:
        rows = await conn.fetch(
            "SELECT * FROM segments WHERE document_id = $1 "
            "ORDER BY position, content_type",
            document_id,
        )
    return [dict(row) for row in rows]


@pytest.mark.asyncio
async def test_insert_segments_batch_is_all_or_nothing(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    """PRD T1 item 5 (revision atomicity): a replacement batch commits as a
    unit. asyncpg executemany without an explicit transaction can commit rows
    one at a time; a mid-batch failure must roll the WHOLE batch back, so a
    committed row always has its vectors — the invariant the replay
    classifier's staged-resumable shortcut depends on."""

    database, pool = verb_world
    await _put_document(pool, document_id="doc-a")

    good_first = _segment_row(segment_id="seg-1", position=0, text="first chunk")
    good_second = _segment_row(segment_id="seg-2", position=1, text="second chunk")
    # Injected failure at the END of the batch: under one-at-a-time commits
    # the two good rows would already be durable when this row violates the
    # primary key. The transaction wrapper must roll them back too.
    bad = _segment_row(segment_id="seg-bad", position=2, text="bad chunk")
    bad["segment_id"] = None

    with pytest.raises(asyncpg.PostgresError):
        await database.insert_segments([good_first, good_second, bad])

    assert await _segment_rows(pool, "doc-a") == []


@pytest.mark.asyncio
async def test_restage_disabled_segment_keeps_operator_disable(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    """PRD T1 item 6 / §6.3: an operator's segment disable is an explicit
    visibility decision that survives re-ingestion. Re-staging changed
    content at a disabled position refreshes the CONTENT columns but keeps
    enabled=FALSE and a non-staging status, so the completion flip can never
    promote the row and silently revoke the operator's decision."""

    database, pool = verb_world
    await _put_document(pool, document_id="doc-a")
    await database.insert_segments(
        [
            _segment_row(segment_id="seg-disabled", position=0, text="old text"),
            _segment_row(segment_id="seg-enabled", position=1, text="sibling text"),
        ]
    )
    # The operator disables one segment (document_service's disable path).
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE segments SET enabled = FALSE, disabled_by = 'operator-a', "
            "disabled_at = NOW() WHERE segment_id = 'seg-disabled'"
        )

    # Re-ingestion stages changed content at BOTH positions.
    await database.insert_segments(
        [
            _segment_row(
                segment_id="seg-disabled-v2",
                position=0,
                text="new text",
                status="indexing",
                enabled=False,
                content_hash="hash-v2",
            ),
            _segment_row(
                segment_id="seg-enabled-v2",
                position=1,
                text="sibling new text",
                status="indexing",
                enabled=False,
                content_hash="hash-sibling-v2",
            ),
        ]
    )

    rows = {row["segment_id"]: row for row in await _segment_rows(pool, "doc-a")}
    disabled = rows["seg-disabled-v2"]
    # Content refreshed ...
    assert disabled["text"] == "new text"
    assert disabled["content_hash"] == "hash-v2"
    # ... but the disable survives and the row is NOT in staging state.
    assert disabled["enabled"] is False
    assert disabled["status"] == "completed"
    assert disabled["disabled_by"] == "operator-a"
    # The non-disabled sibling re-stages normally.
    sibling = rows["seg-enabled-v2"]
    assert sibling["enabled"] is False  # staging rows persist disabled ...
    assert sibling["status"] == "indexing"  # ... until the completion flip
    assert sibling["disabled_by"] is None

    # The completion flip promotes only the staging row; the disabled row is
    # refused even if explicitly named (defense in depth).
    promoted = await database.activate_staged_segments(
        "doc-a", ["seg-disabled-v2", "seg-enabled-v2"]
    )
    assert promoted == 1
    rows = {row["segment_id"]: row for row in await _segment_rows(pool, "doc-a")}
    assert rows["seg-disabled-v2"]["enabled"] is False
    assert rows["seg-disabled-v2"]["status"] == "completed"
    assert rows["seg-enabled-v2"]["enabled"] is True
    assert rows["seg-enabled-v2"]["status"] == "completed"


@pytest.mark.asyncio
async def test_list_segments_orders_by_document_position_content_type(
    verb_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    """Positions restart per content_type, so the sort key must include
    content_type: without it text/image rows tie on position and a paginated
    reader (reembed's full-generation walk) can skip or duplicate rows at a
    page boundary that splits a tied group."""

    database, pool = verb_world
    await _put_document(pool, document_id="doc-a")
    await database.insert_segments(
        [
            _segment_row(segment_id="seg-t0", position=0, text="text zero"),
            _segment_row(
                segment_id="seg-i1", position=1, text="image one", content_type="image"
            ),
            _segment_row(segment_id="seg-t1", position=1, text="text one"),
            _segment_row(segment_id="seg-t2", position=2, text="text two"),
        ]
    )

    # Walk the list one row at a time: every row must appear exactly once.
    walked: list[str] = []
    offset = 0
    while True:
        page = await database.list_segments(
            "dataset-a", document_id="doc-a", limit=1, offset=offset
        )
        if not page:
            break
        walked.extend(row["segment_id"] for row in page)
        offset += 1

    assert walked == ["seg-t0", "seg-i1", "seg-t1", "seg-t2"]

    full = await database.list_segments("dataset-a", document_id="doc-a", limit=100)
    assert [row["segment_id"] for row in full] == [
        "seg-t0",
        "seg-i1",
        "seg-t1",
        "seg-t2",
    ]
