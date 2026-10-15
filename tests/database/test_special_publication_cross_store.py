"""Live PostgreSQL, Qdrant, and local-object publication for one document.

Opt in with ``KB_SPECIAL_CROSS_STORE_TEST=1``. The fixture owns a disposable
PostgreSQL schema, two random Qdrant collections, and a private object prefix.
The point and page bytes are deterministic test data; the production
DatabaseStorage, VectorStore, ImageStorageService and publication coordinator
perform every persistent write and recovery operation.
"""

from __future__ import annotations

import hashlib
import json
import os
import uuid
from collections.abc import AsyncIterator
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import Any
from urllib.parse import urlparse

import asyncpg
import pytest
import pytest_asyncio
from ai_gateway_core.storage.image_storage import (
    ImageStorageService,
    StorageConfig,
)
from dotenv import dotenv_values
from knowledge_service.persistence.database import (
    DOCUMENT_PIPELINE_EXECUTION_KEY,
    DatabaseStorage,
)
from knowledge_service.services.knowledge.special_publication import (
    SOURCE_MANIFEST_KEY,
    SpecialPublicationCoordinator,
)
from knowledge_service.services.knowledge.vector_store import VectorStore
from qdrant_client.http.models import PointStruct

pytestmark = pytest.mark.integration
ROOT = Path(__file__).resolve().parents[2]
DIMENSION = 8


def _hash(data: bytes | str) -> str:
    return hashlib.sha256(data.encode() if isinstance(data, str) else data).hexdigest()


def _postgres_config() -> dict[str, Any]:
    # Never render this dict in assertion messages or test output.
    file_values = dotenv_values(os.environ.get("ENV_FILE") or ROOT / ".env")
    names = ("POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB", "POSTGRES_PORT")
    values = {name: os.environ.get(name) or file_values.get(name) for name in names}
    missing = [name for name, value in values.items() if not value]
    if missing:
        pytest.fail(f"PostgreSQL test configuration missing: {', '.join(missing)}")
    return {
        "host": "127.0.0.1",
        "port": int(str(values["POSTGRES_PORT"])),
        "user": str(values["POSTGRES_USER"]),
        "password": str(values["POSTGRES_PASSWORD"]),
        "database": str(values["POSTGRES_DB"]),
    }


@dataclass
class World:
    db: DatabaseStorage
    pool: asyncpg.Pool
    vectors: VectorStore
    storage: ImageStorageService
    tenant_id: str
    dataset_id: str
    document_id: str
    base: str
    sections: str

    async def dataset(self) -> dict[str, Any]:
        dataset = await self.db.get_dataset(self.dataset_id)
        assert dataset is not None
        return dataset

    def coordinator(self, vectors: Any | None = None) -> SpecialPublicationCoordinator:
        return SpecialPublicationCoordinator(SimpleNamespace(
            db=self.db,
            vector_store=vectors or self.vectors,
            image_storage_service=self.storage,
            bm25_v2_lifecycle_service=None,
        ))


@pytest_asyncio.fixture
async def cross_store_world(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> AsyncIterator[World]:
    if os.environ.get("KB_SPECIAL_CROSS_STORE_TEST") != "1":
        pytest.skip("set KB_SPECIAL_CROSS_STORE_TEST=1 for live cross-store testing")

    nonce = uuid.uuid4().hex
    schema = f"kb_special_cross_{nonce}"
    dataset_id = f"cross-dataset-{nonce}"
    tenant_id = f"cross-tenant-{nonce}"
    document_id = f"cross-document-{nonce}"
    base = f"kb_special_cross_{nonce[:16]}"
    sections = f"{base}_sections"
    config = _postgres_config()
    qdrant_url = os.environ.get("QDRANT_URL", "http://127.0.0.1:6333")
    host = urlparse(qdrant_url).hostname or "127.0.0.1"
    for name in ("NO_PROXY", "no_proxy"):
        current = os.environ.get(name, "")
        monkeypatch.setenv(name, f"{current},{host}".strip(","))

    admin = await asyncpg.connect(**config)
    try:
        await admin.execute(f'CREATE SCHEMA "{schema}"')
    finally:
        await admin.close()
    pool: asyncpg.Pool | None = None
    vectors: VectorStore | None = None
    storage: ImageStorageService | None = None
    try:
        pool = await asyncpg.create_pool(
            **config, min_size=1, max_size=4,
            server_settings={"search_path": f'"{schema}",public'},
        )
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
                CREATE TABLE documents (
                    document_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id),
                    title VARCHAR(255),
                    status VARCHAR(50) NOT NULL DEFAULT 'completed',
                    progress DOUBLE PRECISION,
                    error TEXT,
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    archived BOOLEAN NOT NULL DEFAULT FALSE,
                    metadata JSONB,
                    content TEXT,
                    current_version INTEGER NOT NULL DEFAULT 0,
                    version_count INTEGER NOT NULL DEFAULT 0,
                    word_count INTEGER NOT NULL DEFAULT 0,
                    segment_count INTEGER NOT NULL DEFAULT 0,
                    size_bytes BIGINT NOT NULL DEFAULT 0,
                    source_type VARCHAR(50) NOT NULL DEFAULT 'upload',
                    source_uri TEXT,
                    confluence_version INTEGER,
                    mime_type VARCHAR(100) NOT NULL DEFAULT 'application/pdf',
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
                    execution_id VARCHAR(255) PRIMARY KEY,
                    document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id),
                    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id),
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
                    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id),
                    document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id),
                    position INTEGER NOT NULL DEFAULT 0,
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
                    keywords JSONB DEFAULT '[]'::jsonb,
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
                    document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id),
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
                    UNIQUE (document_id, version_number)
                );
                CREATE TABLE document_summaries (
                    document_id VARCHAR(255) PRIMARY KEY REFERENCES documents(document_id),
                    summary TEXT NOT NULL DEFAULT '',
                    keywords JSONB NOT NULL DEFAULT '[]'::jsonb,
                    topics JSONB NOT NULL DEFAULT '[]'::jsonb,
                    vector_id VARCHAR(255),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                """
            )
            await conn.execute(
                """INSERT INTO datasets
                   (dataset_id, tenant_id, collection_name, index_config)
                   VALUES ($1, $2, $3, '{}'::jsonb)""",
                dataset_id, tenant_id, base,
            )
            await conn.execute(
                """INSERT INTO documents
                   (document_id, dataset_id, title, status, content, metadata)
                   VALUES ($1, $2, 'Cross-store document', 'completed',
                           'initial source', $3::jsonb)""",
                document_id, dataset_id,
                json.dumps({"original_file_key": f"test-original/{nonce}.pdf"}),
            )
        db = DatabaseStorage(pool_max_size=4)
        db._pool = pool  # type: ignore[assignment]
        vectors = VectorStore(url=qdrant_url, timeout_seconds=10.0)
        # A real dataset already has its base collection before reprocessing.
        # The coordinator checks its live lexical profile before opening the
        # publication lease; auxiliary collections are created by publish.
        await vectors.ensure_collection(
            dataset_id=dataset_id, dimension=DIMENSION,
            collection_name=base, tenant_id=tenant_id,
            lifecycle_lease_held=True,
        )
        storage = ImageStorageService(
            StorageConfig(local_base_path=str(tmp_path), key_prefix=f"cross_{nonce}"),
            signing_key="test-only-local-signing-key",
        )
        yield World(db, pool, vectors, storage, tenant_id, dataset_id, document_id, base, sections)
    finally:
        try:
            if vectors is not None:
                for collection in (sections, base):
                    if await vectors.collection_exists(collection):
                        await vectors.delete_collection(collection)
        finally:
            try:
                if vectors is not None:
                    await vectors.close()
                if storage is not None:
                    await storage.close()
            finally:
                try:
                    if pool is not None:
                        await pool.close()
                finally:
                    admin = await asyncpg.connect(**config)
                    try:
                        await admin.execute(f'DROP SCHEMA "{schema}" CASCADE')
                    finally:
                        await admin.close()


async def _document(world: World) -> dict[str, Any]:
    async with world.pool.acquire() as conn:
        row = await conn.fetchrow(
            "SELECT * FROM documents WHERE document_id = $1", world.document_id,
        )
    assert row is not None
    result = dict(row)
    if isinstance(result["metadata"], str):
        result["metadata"] = json.loads(result["metadata"])
    return result


async def _ledger(world: World) -> list[dict[str, Any]]:
    async with world.pool.acquire() as conn:
        rows = await conn.fetch(
            "SELECT execution_id, status, manifest FROM document_pipeline_executions "
            "WHERE document_id = $1 ORDER BY created_at, execution_id",
            world.document_id,
        )
    result = [dict(row) for row in rows]
    for row in result:
        if isinstance(row["manifest"], str):
            row["manifest"] = json.loads(row["manifest"])
    return result


async def _versions(world: World) -> list[dict[str, Any]]:
    async with world.pool.acquire() as conn:
        rows = await conn.fetch(
            "SELECT version_number, content, content_hash, metadata "
            "FROM document_versions WHERE document_id = $1 ORDER BY version_number",
            world.document_id,
        )
    result = [dict(row) for row in rows]
    for row in result:
        if isinstance(row["metadata"], str):
            row["metadata"] = json.loads(row["metadata"])
    return result


async def _segments(world: World) -> list[dict[str, Any]]:
    async with world.pool.acquire() as conn:
        rows = await conn.fetch(
            "SELECT segment_id, content_type, text, status, enabled, metadata "
            "FROM segments WHERE document_id = $1 ORDER BY content_type, position",
            world.document_id,
        )
    return [dict(row) for row in rows]


async def _points(world: World) -> dict[str, list[str]]:
    return await world.vectors.document_point_ids_by_collection(
        tenant_id=world.tenant_id, dataset_id=world.dataset_id,
        document_id=world.document_id,
    )


async def _point_payload(world: World, collection: str, point_id: str) -> dict[str, Any]:
    rows = await world.vectors._client.retrieve(
        collection_name=collection, ids=[point_id],
        with_payload=True, with_vectors=False,
    )
    assert len(rows) == 1 and str(rows[0].id) == point_id
    return dict(rows[0].payload or {})


async def _object_hash(world: World, plan: Any) -> str:
    receipt = plan.object_manifest[0]
    content = await world.storage.download_image(
        world.tenant_id, world.document_id,
        receipt["attachment_id"], "page_1.png",
    )
    return _hash(content)


async def _prepare(world: World, label: str) -> Any:
    generation = str(uuid.uuid4())
    image_id, section_id = str(uuid.uuid4()), str(uuid.uuid4())
    attachment = f"page_1_g{uuid.UUID(generation).hex}"
    image_bytes = f"image bytes for {label}".encode()
    image_text = f"{label} source text"
    page_text = f"[Page 1]\n{image_text}"
    key = world.storage._generate_key(
        world.tenant_id, world.document_id, attachment, "page_1.png",
    )
    async with world.pool.acquire() as conn, conn.transaction():
        await conn.execute(
            """UPDATE documents
               SET status = 'indexing', error = NULL,
                   metadata = COALESCE(metadata, '{}'::jsonb) || $2::jsonb
               WHERE document_id = $1""",
            world.document_id,
            json.dumps({DOCUMENT_PIPELINE_EXECUTION_KEY: generation}),
        )
        await conn.execute(
            """INSERT INTO document_pipeline_executions
               (execution_id, document_id, dataset_id, action, status)
               VALUES ($1, $2, $3, 'reprocess', 'running')""",
            generation, world.document_id, world.dataset_id,
        )
    source_hash = _hash(f"pdf source for {label}")
    await world.db.record_special_publication_manifest(
        generation, world.document_id, world.dataset_id, generation,
        source_hash=source_hash, planned_object_keys=[key],
    )
    image_url = await world.storage.upload_image(
        world.tenant_id, world.document_id, attachment, "page_1.png",
        image_bytes, "image/png",
    )
    assert await _object_hash(world, SimpleNamespace(object_manifest=[{
        "attachment_id": attachment,
    }])) == _hash(image_bytes)
    return SimpleNamespace(
        generation_id=generation,
        document_id=world.document_id,
        dataset_id=world.dataset_id,
        content=page_text,
        source_hash=source_hash,
        points_by_collection={
            world.base: [PointStruct(
                id=image_id, vector=[1.0] + [0.0] * (DIMENSION - 1),
                payload={"document_id": world.document_id, "segment_id": image_id,
                         "text": page_text, "content_type": "image", "level": 3},
            )],
            world.sections: [PointStruct(
                id=section_id, vector=[0.0, 1.0] + [0.0] * (DIMENSION - 2),
                payload={"document_id": world.document_id, "segment_id": section_id,
                         "text": image_text, "content_type": "section", "level": 2},
            )],
        },
        segment_rows=[
            {"segment_id": image_id, "dataset_id": world.dataset_id,
             "document_id": world.document_id, "position": 0, "text": page_text,
             "content_type": "image", "vector_id": image_id,
             "page_number": 1, "image_url": image_url,
             "image_attachment_id": attachment, "image_filename": "page_1.png",
             "image_media_type": "image/png", "image_file_size": len(image_bytes),
             "enabled": False, "status": "indexing", "metadata": {"page_number": 1}},
            {"segment_id": section_id, "dataset_id": world.dataset_id,
             "document_id": world.document_id, "position": 0, "text": image_text,
             "content_type": "section", "vector_id": section_id,
             "enabled": False, "status": "indexing", "metadata": {}},
        ],
        segment_ids=[image_id, section_id],
        object_manifest=[{
            "storage_key": key, "sha256": _hash(image_bytes),
            "attachment_id": attachment,
        }],
        total_pages=1,
        extracted_texts={1: image_text},
    )


async def _assert_generation(world: World, plan: Any, version: int) -> None:
    document = await _document(world)
    assert document["content"] == plan.content
    assert document["current_version"] == version
    assert document["version_count"] == version
    assert document["status"] == "completed"
    assert document["segment_count"] == 2
    assert document["metadata"].get("_special_publication_generation_id") is None
    assert document["metadata"][SOURCE_MANIFEST_KEY]["generation_id"] == plan.generation_id
    rows = await _segments(world)
    assert {row["segment_id"] for row in rows} == set(plan.segment_ids)
    assert all(row["enabled"] and row["status"] == "completed" for row in rows)
    assert await _points(world) == {
        world.base: [plan.segment_ids[0]],
        world.sections: [plan.segment_ids[1]],
    }
    payload = await _point_payload(world, world.base, plan.segment_ids[0])
    assert payload["text"] == plan.content
    assert payload["source_version"] == version
    assert payload["source_hash"] == _hash(plan.content)
    versions = await _versions(world)
    assert len(versions) == version
    assert versions[-1]["content"] == plan.content
    assert versions[-1]["content_hash"] == _hash(plan.content)
    assert versions[-1]["metadata"][SOURCE_MANIFEST_KEY]["objects"] == {
        plan.object_manifest[0]["storage_key"]: plan.object_manifest[0]["sha256"],
    }
    assert await _object_hash(world, plan) == plan.object_manifest[0]["sha256"]


class InjectedCrash(BaseException):
    """Simulate loss of the publisher before its in-process compensator runs."""


class FaultingVectors:
    def __init__(self, delegate: VectorStore, *, stage: str, old_ids: set[str] = frozenset()) -> None:
        self.delegate = delegate
        self.stage = stage
        self.old_ids = old_ids
        self.triggered = False

    def __getattr__(self, name: str) -> Any:
        return getattr(self.delegate, name)

    async def upsert(self, collection: str, points: list[PointStruct], **kwargs: Any) -> None:
        await self.delegate.upsert(collection, points, **kwargs)
        if self.stage == "after_first_upsert" and not self.triggered:
            self.triggered = True
            raise InjectedCrash("after real Qdrant upsert")

    async def delete_document_points_by_ids(
        self, collection: str, point_ids: list[str], **kwargs: Any,
    ) -> None:
        if self.stage == "before_old_cleanup" and self.old_ids.intersection(point_ids):
            self.triggered = True
            raise InjectedCrash("after PostgreSQL commit, before old point cleanup")
        await self.delegate.delete_document_points_by_ids(collection, point_ids, **kwargs)
        if (
            self.stage == "during_old_cleanup"
            and self.old_ids.intersection(point_ids)
            and not self.triggered
        ):
            self.triggered = True
            raise InjectedCrash("after first real old point deletion")


async def _publish(world: World, plan: Any, vectors: Any | None = None) -> None:
    coordinator = world.coordinator(vectors)
    await coordinator.publish(
        plan, await world.dataset(), plan.generation_id,
        (await _document(world))["content"],
    )


async def _assert_single_owner(world: World, plan: Any, phase: str) -> None:
    dataset = await world.dataset()
    assert dataset["content_revision"] < 0
    owner = await world.db.get_active_special_publication_for_dataset(world.dataset_id)
    assert owner["execution_id"] == plan.generation_id
    assert owner["generation_id"] == plan.generation_id
    assert owner["phase"] == phase
    ledger = await _ledger(world)
    assert [row["execution_id"] for row in ledger if row["status"] == "running"] == [
        plan.generation_id,
    ]


async def _recover(world: World) -> None:
    assert await world.coordinator().recover_unfinished(await world.dataset())
    assert (await world.dataset())["content_revision"] > 0
    assert not await world.coordinator().recover_unfinished(await world.dataset())


@pytest.mark.asyncio
async def test_preparing_crash_requeues_original_execution_after_object_cleanup(
    cross_store_world: World,
) -> None:
    world = cross_store_world
    plan = await _prepare(world, "preparing-crash")
    async with world.pool.acquire() as conn:
        await conn.execute(
            "UPDATE document_pipeline_executions "
            "SET process_rule_id = 'pinned-rule', input_snapshot = $2::jsonb "
            "WHERE execution_id = $1",
            plan.generation_id,
            json.dumps({"index_config": {"chunking": {"mode": "hierarchical"}}}),
        )

    await world.coordinator().abort_preparing(
        await world.dataset(), plan.generation_id,
        plan.generation_id, plan.source_hash,
        resume_same_execution=True,
    )

    document = await _document(world)
    ledger = await _ledger(world)
    assert document["status"] == "waiting"
    assert document["metadata"][DOCUMENT_PIPELINE_EXECUTION_KEY] == plan.generation_id
    assert "_special_publication_generation_id" not in document["metadata"]
    assert len(ledger) == 1 and ledger[0]["execution_id"] == plan.generation_id
    assert ledger[0]["status"] == "running"
    assert SOURCE_MANIFEST_KEY not in document["metadata"]
    manifest = ledger[0]["manifest"]
    assert "special_publication" not in manifest
    assert manifest["preparing_replayed_after_restart"] is True
    receipt = plan.object_manifest[0]
    assert not await world.storage.image_exists(
        world.tenant_id, world.document_id,
        receipt["attachment_id"], "page_1.png",
    )
    with pytest.raises(RuntimeError, match="matching durable owner"):
        await world.coordinator().abort_preparing(
            await world.dataset(), plan.generation_id,
            plan.generation_id, plan.source_hash,
            resume_same_execution=True,
        )
    assert len(await _ledger(world)) == 1


async def test_two_generations_and_fault_windows_share_one_cross_store_owner(
    cross_store_world: World,
) -> None:
    world = cross_store_world
    first = await _prepare(world, "first")
    await _publish(world, first)
    await _assert_generation(world, first, 1)

    second = await _prepare(world, "second")
    await _publish(world, second)
    await _assert_generation(world, second, 2)
    assert await _object_hash(world, first) == first.object_manifest[0]["sha256"]
    versions = await _versions(world)
    assert [row["content"] for row in versions] == [first.content, second.content]
    assert all(row["metadata"][SOURCE_MANIFEST_KEY]["source_kind"] == "vision" for row in versions)

    # Preparation stopped after the declared object was written, before a
    # negative revision or point plan existed. The producer recovery path
    # retires this exact execution and leaves the serving generation intact.
    preparing = await _prepare(world, "preparing-crash")
    assert (await world.dataset())["content_revision"] > 0
    assert (await world.db.get_special_publication_manifest(preparing.generation_id))["phase"] == "preparing"
    assert await world.coordinator().abort_preparing(
        await world.dataset(), preparing.generation_id,
        preparing.generation_id, preparing.source_hash,
    )
    assert (await world.dataset())["content_revision"] > 0
    assert (await _document(world))["content"] == second.content
    assert await _points(world) == {
        world.base: [second.segment_ids[0]], world.sections: [second.segment_ids[1]],
    }
    assert (await _point_payload(world, world.base, second.segment_ids[0]))["text"] == second.content
    assert not await world.storage.image_exists(
        world.tenant_id, world.document_id,
        preparing.object_manifest[0]["attachment_id"], "page_1.png",
    )
    assert await _object_hash(world, second) == second.object_manifest[0]["sha256"]

    # A hard stop after the first real Qdrant upsert leaves a prepared owner,
    # one orphan candidate point, and its uploaded object. Recovery uses the
    # frozen manifest to remove only candidate assets.
    partial = await _prepare(world, "upsert-crash")
    upsert_fault = FaultingVectors(world.vectors, stage="after_first_upsert")
    coordinator = world.coordinator(upsert_fault)

    async def no_in_process_compensation(*_args: Any, **_kwargs: Any) -> None:
        return None

    coordinator._abort_uncommitted = no_in_process_compensation  # type: ignore[method-assign]
    with pytest.raises(InjectedCrash, match="real Qdrant upsert"):
        await coordinator.publish(
            partial, await world.dataset(), partial.generation_id, second.content,
        )
    assert upsert_fault.triggered
    await _assert_single_owner(world, partial, "prepared")
    assert (await _document(world))["content"] == second.content
    assert {row["segment_id"] for row in await _segments(world)} == set(second.segment_ids)
    assert set((await _points(world))[world.base]) == {
        second.segment_ids[0], partial.segment_ids[0],
    }
    assert (await _point_payload(world, world.base, second.segment_ids[0]))["text"] == second.content
    assert len(await _versions(world)) == 2
    assert await _object_hash(world, second) == second.object_manifest[0]["sha256"]
    await _recover(world)
    assert (await _document(world))["content"] == second.content
    assert {row["segment_id"] for row in await _segments(world)} == set(second.segment_ids)
    assert await _points(world) == {
        world.base: [second.segment_ids[0]], world.sections: [second.segment_ids[1]],
    }
    assert not await world.storage.image_exists(
        world.tenant_id, world.document_id,
        partial.object_manifest[0]["attachment_id"], "page_1.png",
    )
    assert await _object_hash(world, second) == second.object_manifest[0]["sha256"]

    # PostgreSQL commits the new body, segments, version and committed ledger
    # in one transaction. The old Qdrant points still exist at this cut, and
    # restart must remove exactly those IDs without reverting the PG version.
    committed = await _prepare(world, "committed-crash")
    old_ids = set(second.segment_ids)
    cleanup_fault = FaultingVectors(
        world.vectors, stage="before_old_cleanup", old_ids=old_ids,
    )
    with pytest.raises(InjectedCrash, match="before old point cleanup"):
        await _publish(world, committed, cleanup_fault)
    assert cleanup_fault.triggered
    await _assert_single_owner(world, committed, "committed")
    assert (await _document(world))["content"] == committed.content
    assert {row["segment_id"] for row in await _segments(world)} == set(committed.segment_ids)
    points_before_cleanup = await _points(world)
    assert set(points_before_cleanup[world.base]) == {
        second.segment_ids[0], committed.segment_ids[0],
    }
    assert set(points_before_cleanup[world.sections]) == {
        second.segment_ids[1], committed.segment_ids[1],
    }
    assert (await _point_payload(world, world.base, second.segment_ids[0]))["source_version"] == 2
    assert (await _point_payload(world, world.base, committed.segment_ids[0]))["source_version"] == 3
    assert [row["content"] for row in await _versions(world)] == [
        first.content, second.content, committed.content,
    ]
    await _recover(world)
    await _assert_generation(world, committed, 3)

    # A second cut interrupts cleanup after one collection was actually
    # cleared. Recovery must tolerate an already-missing old ID and finish
    # the remaining collection without losing the committed candidate.
    partial_cleanup = await _prepare(world, "partial-cleanup-crash")
    cleanup_fault = FaultingVectors(
        world.vectors, stage="during_old_cleanup",
        old_ids=set(committed.segment_ids),
    )
    with pytest.raises(InjectedCrash, match="first real old point deletion"):
        await _publish(world, partial_cleanup, cleanup_fault)
    assert cleanup_fault.triggered
    await _assert_single_owner(world, partial_cleanup, "committed")
    remaining = await _points(world)
    assert remaining[world.base] == [partial_cleanup.segment_ids[0]]
    assert set(remaining[world.sections]) == {
        committed.segment_ids[1], partial_cleanup.segment_ids[1],
    }
    await _recover(world)
    await _assert_generation(world, partial_cleanup, 4)
    assert await _object_hash(world, first) == first.object_manifest[0]["sha256"]
    assert await _object_hash(world, second) == second.object_manifest[0]["sha256"]
    assert await _object_hash(world, committed) == committed.object_manifest[0]["sha256"]
    ledger = await _ledger(world)
    assert [row["status"] for row in ledger].count("completed") == 4
    assert [row["status"] for row in ledger].count("error") == 2
    assert all(row["status"] != "running" for row in ledger)
    assert [row["manifest"]["special_publication"]["phase"] for row in ledger].count(
        "committed"
    ) == 4
