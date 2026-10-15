"""Real-PostgreSQL tests for PRD T3 (embedding versioning + blue-green).

Tier-b pattern (see tests/database/test_kb_ingestion_lifecycle_migration.py):
throwaway database, minimal tables carrying exactly the columns the migration
and the production queries read, migration 102 applied twice (idempotency)
followed by the current T3 durable-action schema (110), then behavioral
assertions through the production EmbeddingVersionStore and
EmbeddingMigrationService (the latter with fakes for Qdrant + the embedder
only — the persistence side is the real thing).

Contract exercised:
  * datasets/documents version metadata; seeded serving bindings (082
    reservation semantics preserved at the binding layer).
  * one serving binding per dataset; live collection names reserved.
  * one live migration per dataset; resumable content-hash receipts.
  * gate-gated cutover flipping binding + datasets + document provenance in
    one transaction; abort guards on stale pointers; rollback as a pointer
    flip to the retained collection.
  * retention → reclaimable; batched content-hash vector cache keyed by the
    embedding identity (no pickle).
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import os
import uuid
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any

import asyncpg
import pytest
import pytest_asyncio
from dotenv import dotenv_values
from knowledge_service.persistence.embedding_version_store import (
    EmbeddingVersionStore,
    MigrationStateError,
)
from knowledge_service.services.knowledge.embedding_migration import (
    EmbeddingMigrationError,
    EmbeddingMigrationService,
)

ROOT = Path(__file__).resolve().parents[2]
MIGRATION_102 = ROOT / "database" / "migrations" / "102_kb_embedding_versioning_blue_green.sql"
MIGRATION_110 = ROOT / "database" / "migrations" / "110_kb_embedding_migration_action_jobs.sql"


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
async def t3_world() -> AsyncIterator[tuple[asyncpg.Pool, EmbeddingVersionStore]]:
    config = _postgres_config()
    database_name = f"kb_t3_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{database_name}"')

    pool = await asyncpg.create_pool(
        **{**config, "database": database_name},
        min_size=1,
        max_size=3,
        server_settings={"search_path": "knowledge,gateway,assistant,public"},
    )
    try:
        async with pool.acquire() as conn:
            await conn.execute("CREATE SCHEMA knowledge")
            # Pre-102 shape: datasets with the original embedding identity
            # columns (no embedding_model_version), documents without
            # provenance, segments with the T1-era identity columns.
            await conn.execute(
                """
                CREATE TABLE datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL DEFAULT '',
                    embedding_provider VARCHAR(50) NOT NULL DEFAULT 'gemini',
                    embedding_model VARCHAR(100) NOT NULL DEFAULT 'gemini-embedding-001',
                    embedding_dimension INTEGER NOT NULL DEFAULT 1024,
                    collection_name VARCHAR(255),
                    content_revision BIGINT NOT NULL DEFAULT 0,
                    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE documents (
                    document_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    title VARCHAR(255) NOT NULL,
                    status VARCHAR(50) NOT NULL DEFAULT 'completed',
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    archived BOOLEAN NOT NULL DEFAULT FALSE,
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb
                );
                CREATE TABLE segments (
                    segment_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    document_id VARCHAR(255) NOT NULL
                        REFERENCES documents(document_id) ON DELETE CASCADE,
                    position INTEGER NOT NULL DEFAULT 0,
                    text TEXT NOT NULL,
                    token_count INTEGER NOT NULL DEFAULT 0,
                    vector_id VARCHAR(255),
                    content_hash VARCHAR(64),
                    index_node_hash VARCHAR(64),
                    content_type VARCHAR(50) NOT NULL DEFAULT 'text',
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    status VARCHAR(50) NOT NULL DEFAULT 'completed',
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb
                );
                INSERT INTO datasets (
                    dataset_id, tenant_id, embedding_provider, embedding_model,
                    embedding_dimension, collection_name, is_deleted
                )
                VALUES
                    ('ds-a', 'tenant-a', 'dashscope', 'text-embedding-v4', 1024,
                     'kb_ds-a_1024', FALSE),
                    ('ds-b', 'tenant-b', 'gemini', 'gemini-embedding-001', 3072,
                     'kb_ds-b_3072', FALSE),
                    ('ds-deleted', 'tenant-a', 'dashscope', 'text-embedding-v4', 1024,
                     'kb_ds-deleted_1024', TRUE),
                    ('ds-unbound', 'tenant-a', 'dashscope', 'text-embedding-v4', 1024,
                     NULL, FALSE),
                    ('ds-c', 'tenant-c', 'dashscope', 'text-embedding-v4', 1024,
                     NULL, FALSE);
                INSERT INTO documents (document_id, dataset_id, title)
                VALUES ('doc-a1', 'ds-a', 'first'), ('doc-a2', 'ds-a', 'second');
                INSERT INTO segments (
                    segment_id, dataset_id, document_id, position, text,
                    vector_id, content_hash, metadata
                )
                VALUES
                    ('seg-a1-0', 'ds-a', 'doc-a1', 0, 'alpha text',
                     'vec-a1-0', 'hash-a1-0', '{"source_type": "upload"}'),
                    ('seg-a1-1', 'ds-a', 'doc-a1', 1, 'beta text',
                     NULL, 'hash-a1-1', '{}'),
                    ('seg-a2-0', 'ds-a', 'doc-a2', 0, 'gamma text',
                     'vec-a2-0', NULL, '{}');
                -- The second segment of doc-a1 falls back to its segment id as
                -- vector_id; 'gamma' has only index_node_hash coverage below.
                UPDATE segments SET index_node_hash = 'hash-a2-0'
                WHERE segment_id = 'seg-a2-0';
                """
            )
            sql = MIGRATION_102.read_text(encoding="utf-8")
            await conn.execute(sql)
            await conn.execute(sql)  # idempotency: applying twice must stick
            action_sql = MIGRATION_110.read_text(encoding="utf-8")
            await conn.execute(action_sql)
            await conn.execute(action_sql)  # current T3 schema is restart-safe too
        yield pool, EmbeddingVersionStore(pool)
    finally:
        await pool.close()
        try:
            await admin.execute(f'DROP DATABASE "{database_name}"')
        finally:
            await admin.close()


# --------------------------------------------------------------- migration shape


async def test_102_adds_version_metadata_and_seeds_serving_bindings(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    _pool, store = t3_world
    serving = await store.get_serving_binding("ds-a")
    assert serving is not None
    assert serving["collection_name"] == "kb_ds-a_1024"
    assert serving["embedding_provider"] == "dashscope"
    assert serving["embedding_model"] == "text-embedding-v4"
    assert serving["embedding_dimension"] == 1024
    assert serving["embedding_model_version"] == ""
    assert serving["capabilities"] == []
    assert serving["state"] == "serving"

    # Soft-deleted datasets are not adopted (082 reservation stays on the
    # datasets row); datasets without a collection have no binding.
    assert await store.get_serving_binding("ds-deleted") is None
    assert await store.get_serving_binding("ds-unbound") is None


async def test_documents_provenance_columns_exist(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, _store = t3_world
    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            """
            SELECT embedding_model, embedding_model_version, embedding_dimension
            FROM documents WHERE document_id = 'doc-a1'
            """
        )
    assert row["embedding_model"] == ""
    assert row["embedding_model_version"] == ""
    assert row["embedding_dimension"] is None


# ------------------------------------------------------------ binding invariants


async def test_binding_reservation_semantics(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = t3_world
    # A live shadow binding reserves its collection name dataset-wide.
    shadow = await store.create_binding(
        dataset_id="ds-a",
        tenant_id="tenant-a",
        collection_name="kb_shared_gen_1024",
        embedding_provider="local",
        embedding_model="qwen3-embedding",
        embedding_model_version="2026-08",
        embedding_dimension=1024,
        capabilities=["text"],
    )
    with pytest.raises(asyncpg.UniqueViolationError):
        async with pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO dataset_collection_bindings (
                    dataset_id, tenant_id, collection_name, embedding_provider,
                    embedding_model, embedding_dimension, state
                )
                VALUES ('ds-b', 'tenant-b', 'kb_shared_gen_1024', 'x', 'y', 1024, 'shadow')
                """
            )
    # Retiring releases the reservation (explicit reclamation path).
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE dataset_collection_bindings SET state = 'retained',"
            " retired_at = NOW() WHERE binding_id = $1",
            shadow["binding_id"],
        )
        await conn.execute(
            "UPDATE dataset_collection_bindings SET state = 'retired' WHERE binding_id = $1",
            shadow["binding_id"],
        )
    # And after the retirement above, a fresh live binding may claim the name.
    reclaimed = await store.create_binding(
        dataset_id="ds-b",
        tenant_id="tenant-b",
        collection_name="kb_shared_gen_1024",
        embedding_provider="local",
        embedding_model="qwen3-embedding",
        embedding_model_version="2026-08",
        embedding_dimension=1024,
    )
    assert reclaimed["state"] == "shadow"

    # 'serving' is never creatable: it is granted only by cutover/seed.
    with pytest.raises(ValueError):
        await store.create_binding(
            dataset_id="ds-b",
            tenant_id="tenant-b",
            collection_name="kb_sneaky_1024",
            embedding_provider="local",
            embedding_model="m",
            embedding_dimension=1024,
            state="serving",  # type: ignore[arg-type]
        )


async def test_binding_and_migration_dataset_identity_is_database_enforced(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = t3_world
    serving_a = await store.get_serving_binding("ds-a")
    serving_b = await store.get_serving_binding("ds-b")
    assert serving_a is not None and serving_b is not None

    async with pool.acquire() as conn:
        with pytest.raises(asyncpg.ForeignKeyViolationError):
            await conn.execute(
                """
                INSERT INTO dataset_collection_bindings (
                    dataset_id, tenant_id, collection_name,
                    embedding_dimension, state
                ) VALUES ('ds-a', 'tenant-b', 'kb_wrong_tenant', 1024, 'shadow')
                """
            )

        with pytest.raises(asyncpg.ForeignKeyViolationError):
            await conn.execute(
                """
                INSERT INTO embedding_migrations (
                    dataset_id, source_binding_id, target_binding_id
                ) VALUES ('ds-a', $1::uuid, $2::uuid)
                """,
                serving_a["binding_id"],
                serving_b["binding_id"],
            )

        with pytest.raises(asyncpg.CheckViolationError):
            await conn.execute(
                """
                INSERT INTO embedding_migrations (
                    dataset_id, source_binding_id, target_binding_id
                ) VALUES ('ds-a', $1::uuid, $1::uuid)
                """,
                serving_a["binding_id"],
            )


async def test_register_serving_binding_is_idempotent_and_conflict_guarded(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    _pool, store = t3_world
    dataset = {
        "dataset_id": "ds-unbound",
        "tenant_id": "tenant-a",
        "collection_name": "kb_ds-unbound_1024",
        "embedding_provider": "dashscope",
        "embedding_model": "text-embedding-v4",
        "embedding_dimension": 1024,
    }
    first = await store.register_serving_binding_from_dataset_row(dataset)
    second = await store.register_serving_binding_from_dataset_row(dataset)
    assert first is not None and second is not None
    assert first["binding_id"] == second["binding_id"]
    assert first["state"] == "serving"

    # Name already reserved by another dataset's live binding: refuse, don't steal.
    clash = dict(dataset, dataset_id="ds-c", collection_name="kb_ds-a_1024")
    from knowledge_service.persistence.embedding_version_store import (
        BindingConflictError,
    )

    with pytest.raises(BindingConflictError):
        await store.register_serving_binding_from_dataset_row(clash)


# --------------------------------------------------------------- progress ledger


async def test_pending_segments_use_authoritative_rows_and_content_hashes(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = t3_world
    serving = await store.get_serving_binding("ds-a")
    target = await store.create_binding(
        dataset_id="ds-a",
        tenant_id="tenant-a",
        collection_name="kb_ds-a_1024_vqwen3",
        embedding_provider="local",
        embedding_model="qwen3-embedding",
        embedding_model_version="2026-08",
        embedding_dimension=1024,
    )
    mig = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=serving["binding_id"],
        target_binding_id=target["binding_id"],
    )
    migration_id = mig["migration_id"]

    pending = await store.list_pending_segments(migration_id, dataset_id="ds-a")
    assert [row["segment_id"] for row in pending] == [
        "seg-a1-0",
        "seg-a1-1",
        "seg-a2-0",
    ]
    # vector_id falls back to the stable segment id; hash falls back to
    # index_node_hash and can be '' (never NULL) so receipts compare cleanly.
    by_id = {row["segment_id"]: row for row in pending}
    assert by_id["seg-a1-1"]["vector_id"] == "seg-a1-1"
    assert by_id["seg-a1-0"]["vector_id"] == "vec-a1-0"
    assert by_id["seg-a1-0"]["content_hash"] == "hash-a1-0"
    assert by_id["seg-a2-0"]["content_hash"] == "hash-a2-0"
    assert await store.count_pending_segments(migration_id, dataset_id="ds-a") == 3
    assert await store.count_enabled_segments("ds-a") == 3

    receipts = [
        {
            "segment_id": row["segment_id"],
            "document_id": row["document_id"],
            "position": row["position"],
            "vector_id": row["vector_id"],
            "content_hash": row["content_hash"],
        }
        for row in pending[:2]
    ]
    await store.record_progress_receipts(migration_id, receipts)
    # Re-recording the same receipts is idempotent (crash between write and
    # receipt replay).
    await store.record_progress_receipts(migration_id, receipts)
    remaining = await store.list_pending_segments(migration_id, dataset_id="ds-a")
    assert [row["segment_id"] for row in remaining] == ["seg-a2-0"]

    # Content changed under a receipted segment → it re-enters the queue
    # (skip is per content hash, not per segment id).
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE segments SET text = 'alpha edited', content_hash = 'hash-a1-0-edited'"
            " WHERE segment_id = 'seg-a1-0'"
        )
    requeued = await store.list_pending_segments(migration_id, dataset_id="ds-a")
    assert {row["segment_id"] for row in requeued} == {"seg-a1-0", "seg-a2-0"}

    # Disabled segment / disabled doc / archived doc / image rows are excluded.
    async with pool.acquire() as conn:
        await conn.execute("UPDATE segments SET enabled = FALSE WHERE segment_id = 'seg-a2-0'")
        await conn.execute("UPDATE documents SET enabled = FALSE WHERE document_id = 'doc-a1'")
    assert await store.count_pending_segments(migration_id, dataset_id="ds-a") == 0
    assert await store.count_enabled_segments("ds-a") == 0


async def test_backfill_advisory_lease_is_exclusive_and_reusable(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = t3_world
    migration_id = str(uuid.uuid4())
    competing_store = EmbeddingVersionStore(pool)

    async with store.backfill_lease(migration_id):
        with pytest.raises(MigrationStateError, match="already running"):
            async with competing_store.backfill_lease(migration_id):
                raise AssertionError("a concurrent backfill lease must not be granted")

    # The owner releases on context exit, so crash/retry recovery is not wedged.
    async with competing_store.backfill_lease(migration_id):
        pass


async def test_cutover_lease_blocks_dataset_writer_and_releases(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = t3_world
    lock_name = "knowledge-dataset-index:ds-a"

    async with store.dataset_exclusive_lease("ds-a"):
        async with pool.acquire() as writer, writer.transaction():
            admitted = await writer.fetchval(
                "SELECT pg_try_advisory_xact_lock_shared(hashtextextended($1, 0))",
                lock_name,
            )
        assert admitted is False

    async with pool.acquire() as writer, writer.transaction():
        admitted_after_release = await writer.fetchval(
            "SELECT pg_try_advisory_xact_lock_shared(hashtextextended($1, 0))",
            lock_name,
        )
    assert admitted_after_release is True


async def test_one_live_migration_per_dataset(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    _pool, store = t3_world
    serving = await store.get_serving_binding("ds-a")
    target = await store.create_binding(
        dataset_id="ds-a",
        tenant_id="tenant-a",
        collection_name="kb_ds-a_1024_vx",
        embedding_provider="local",
        embedding_model="m",
        embedding_dimension=1024,
    )
    await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=serving["binding_id"],
        target_binding_id=target["binding_id"],
    )
    target2 = await store.create_binding(
        dataset_id="ds-a",
        tenant_id="tenant-a",
        collection_name="kb_ds-a_1024_vy",
        embedding_provider="local",
        embedding_model="m2",
        embedding_dimension=1024,
    )
    with pytest.raises(MigrationStateError):
        await store.begin_migration(
            dataset_id="ds-a",
            source_binding_id=serving["binding_id"],
            target_binding_id=target2["binding_id"],
        )


# ------------------------------------------------------ full blue-green protocol


class FakeVectorStore:
    def __init__(self, *, dims: int = 1024) -> None:
        self.dims = dims
        self.collections: dict[str, dict[str, Any]] = {}
        self.points: dict[str, dict[str, Any]] = {}
        self.deleted: list[str] = []
        self.writes: list[tuple[str, int]] = []

    async def ensure_collection(self, **kwargs: Any) -> str:
        name = str(kwargs["collection_name"])
        self.collections.setdefault(
            name,
            {"dimension": int(kwargs["dimension"]), "tenant_id": kwargs.get("tenant_id")},
        )
        return name

    async def upsert(self, *, collection_name: str, points: list[Any], **_kw: Any) -> None:
        assert collection_name in self.collections, "write to unknown collection"
        for point in points:
            self.points[f"{collection_name}:{point.id}"] = point
        self.writes.append((collection_name, len(points)))

    async def count_points(self, collection_name: str) -> int:
        return sum(1 for key in self.points if key.startswith(f"{collection_name}:"))

    async def scan_embedding_migration_scope(
        self,
        collection_name: str,
        *,
        tenant_id: str,
        dataset_id: str,
        embedding_model: str,
        embedding_model_version: str,
        embedding_dimension: int,
    ) -> dict[str, Any]:
        points = [
            point for key, point in self.points.items() if key.startswith(f"{collection_name}:")
        ]
        ids: list[str] = []
        sources: list[tuple[str, str]] = []
        for point in points:
            payload = dict(point.payload or {})
            assert payload.get("tenant_id") == tenant_id
            assert payload.get("dataset_id") == dataset_id
            assert payload.get("embedding_model") == embedding_model
            assert payload.get("embedding_model_version") == embedding_model_version
            assert len(point.vector) == embedding_dimension
            point_id = str(point.id)
            ids.append(point_id)
            sources.append((point_id, str(payload.get("text") or "")))
        point_digest = hashlib.sha256(
            "".join(f"{point_id}\n" for point_id in sorted(ids)).encode("utf-8")
        ).hexdigest()
        source_lines = []
        for point_id, text in sorted(sources):
            text_digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
            source_lines.append(f"{point_id}\0{text_digest}\n")
        return {
            "point_count": len(ids),
            "point_ids_sha256": point_digest,
            "source_text_sha256": hashlib.sha256("".join(source_lines).encode("utf-8")).hexdigest(),
        }

    async def delete_collection(self, collection_name: str) -> None:
        self.deleted.append(collection_name)
        self.collections.pop(collection_name, None)
        for key in [k for k in self.points if k.startswith(f"{collection_name}:")]:
            self.points.pop(key)


class FakeEmbedder:
    def __init__(self, *, dim: int = 1024, fail_after: int | None = None) -> None:
        self.dim = dim
        self.model = "qwen3-embedding"
        self.provider = "local"
        self.fail_after = fail_after
        self.calls = 0
        self.embedded_texts: list[str] = []

    async def embed_texts(
        self, texts: list[str], text_type: str | None = None
    ) -> list[list[float]]:
        self.calls += 1
        if text_type != "document":
            raise AssertionError(f"backfill must embed with text_type='document', got {text_type}")
        if self.fail_after is not None and self.calls > self.fail_after:
            raise RuntimeError("embedder crashed mid-migration")
        out: list[list[float]] = []
        for text in texts:
            self.embedded_texts.append(text)
            out.append([float(len(text) % 7) + 1.0] * self.dim)
        return out


def _make_service(
    store: EmbeddingVersionStore,
) -> tuple[EmbeddingMigrationService, FakeVectorStore]:
    vector_store = FakeVectorStore()
    service = EmbeddingMigrationService(store=store, vector_store=vector_store)
    return service, vector_store


async def _dataset_row(pool: asyncpg.Pool, dataset_id: str) -> dict[str, Any]:
    async with pool.acquire() as conn:
        row = await conn.fetchrow("SELECT * FROM datasets WHERE dataset_id = $1", dataset_id)
    return dict(row)


async def test_full_blue_green_with_resume_gate_cutover_and_rollback(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = t3_world
    service, vector_store = _make_service(store)
    dataset = await _dataset_row(pool, "ds-a")
    dataset["embedding_model_version"] = ""

    # -- 1. open the shadow generation ------------------------------------
    opened = await service.start_migration(
        dataset,
        target_provider="local",
        target_model="qwen3-embedding",
        target_model_version="2026-08",
        target_dimension=1024,
        capabilities=["text"],
        migration_tag="qwen3",
    )
    migration_id = opened["migration"]["migration_id"]
    shadow = "kb_ds-a_1024_vqwen3"
    assert vector_store.collections[shadow]["dimension"] == 1024

    # Zero-window: the old collection is never touched by protocol writes.
    assert all(name == shadow for name, _ in vector_store.writes)

    # -- 2. backfill crashes after the first page, then resumes ------------
    embedder = FakeEmbedder(dim=1024, fail_after=1)
    with pytest.raises(RuntimeError, match="crashed"):
        await service.backfill(migration_id, embedder, page_size=2)
    assert (await store.get_migration(migration_id))["state"] == "failed"
    first_pass = [k for k in vector_store.points if k.startswith(f"{shadow}:")]
    assert len(first_pass) == 2  # the receipted page survived the crash

    embedder_resumed = FakeEmbedder(dim=1024)
    result = await service.backfill(migration_id, embedder_resumed, page_size=2)
    # Resumability: only the never-receipted chunk was re-embedded.
    assert result["embedded"] == 1
    assert embedder_resumed.embedded_texts == ["gamma text"]
    assert len(vector_store.points) == 3

    # -- 3. verify against the PostgreSQL authority ------------------------
    verified = await service.verify(migration_id)
    assert verified["enabled_chunks"] == 3
    assert verified["points"] == 3
    assert (await store.get_migration(migration_id))["state"] == "verified"

    # -- 4. T0 gate: failing verdict blocks cutover, passing one unblocks --
    gate_ctx = await service.run_gate(
        migration_id, lambda _ctx: _const_verdict({"passed": False, "reason": "regression"})
    )
    assert gate_ctx["passed"] is False
    assert (await store.get_migration(migration_id))["state"] == "gate_failed"
    with pytest.raises(MigrationStateError):
        await service.cutover(migration_id)

    # A crashing evaluator fails loudly but also records gate_failed so the
    # verdict (with the crash reason) is auditable on the migration row.
    with pytest.raises(EmbeddingMigrationError, match="gate crashed"):
        await service.run_gate(migration_id, _raising_evaluator)
    crashed = await store.get_migration(migration_id)
    assert crashed["state"] == "gate_failed"
    assert "eval harness unavailable" in crashed["gate"]["error"]

    gate_ok = await service.run_gate(
        migration_id,
        lambda _ctx: _const_verdict({"passed": True, "delta": 0.04}),
    )
    assert gate_ok["passed"] is True
    assert (await store.get_migration(migration_id))["state"] == "ready"

    # -- 5. cutover: pointer flip, retention, provenance --------------------
    done = await service.cutover(migration_id, retention_seconds=3600)
    assert done["state"] == "completed"
    flipped = await _dataset_row(pool, "ds-a")
    assert flipped["collection_name"] == shadow
    assert flipped["embedding_provider"] == "local"
    assert flipped["embedding_model"] == "qwen3-embedding"
    assert flipped["embedding_model_version"] == "2026-08"
    serving_now = await store.get_serving_binding("ds-a")
    assert serving_now["binding_id"] == opened["target_binding"]["binding_id"]
    old = await store.get_binding(opened["serving_binding"]["binding_id"])
    assert old["state"] == "retained"
    assert old["retained_until"] is not None
    async with pool.acquire() as conn:
        stamp = await conn.fetchrow(
            "SELECT embedding_model, embedding_model_version, embedding_dimension"
            " FROM documents WHERE document_id = 'doc-a1'"
        )
    assert stamp["embedding_model"] == "qwen3-embedding"
    assert stamp["embedding_model_version"] == "2026-08"
    assert stamp["embedding_dimension"] == 1024

    # Points in the shadow collection carry the audit identity (T3 item 1).
    sample = next(p for key, p in vector_store.points.items() if key == f"{shadow}:vec-a1-0")
    assert sample.payload["embedding_model"] == "qwen3-embedding"
    assert sample.payload["embedding_model_version"] == "2026-08"
    assert sample.payload["content_hash"] == "hash-a1-0"
    assert sample.payload["dataset_id"] == "ds-a"
    assert sample.payload["metadata"]["source_type"] == "upload"

    refreshed = await service.describe(await _dataset_row(pool, "ds-a"))
    assert refreshed["live_migration"] is None
    assert refreshed["latest_migration"]["migration_id"] == migration_id
    assert refreshed["latest_migration"]["state"] == "completed"
    assert refreshed["recent_migrations"][0]["migration_id"] == migration_id
    assert refreshed["source_binding"]["binding_id"] == opened["serving_binding"]["binding_id"]
    assert refreshed["target_binding"]["binding_id"] == opened["target_binding"]["binding_id"]
    assert refreshed["collection_health"]["status"] == "healthy"
    assert refreshed["collection_health"]["checked_live"] is True
    assert refreshed["collection_health"]["authority"]
    assert refreshed["collection_health"]["target_scope"]

    # -- 6. rollback: the retained old collection serves again -------------
    rolled = await service.rollback(migration_id)
    assert rolled["state"] == "rolled_back"
    back = await _dataset_row(pool, "ds-a")
    assert back["collection_name"] == "kb_ds-a_1024"
    assert back["embedding_provider"] == "dashscope"
    assert back["embedding_model"] == "text-embedding-v4"
    serving_again = await store.get_serving_binding("ds-a")
    assert serving_again["binding_id"] == opened["serving_binding"]["binding_id"]
    assert serving_again["retained_until"] is None
    shadow_after = await store.get_binding(opened["target_binding"]["binding_id"])
    assert shadow_after["state"] == "shadow"
    # keep_shadow=True preserved the finished generation for a retry.
    assert vector_store.deleted == []
    refreshed_after_rollback = await service.describe(await _dataset_row(pool, "ds-a"))
    assert refreshed_after_rollback["latest_migration"]["state"] == "rolled_back"
    assert refreshed_after_rollback["target_binding"]["state"] == "shadow"

    # -- 7. retry path: reopen → verify → gate → cutover again --------------
    reopened = await store.reopen_migration_for_retry(migration_id)
    assert reopened is not None and reopened["state"] == "backfilling"
    retry_embedder = FakeEmbedder(dim=1024)
    resumed = await service.backfill(migration_id, retry_embedder, page_size=10)
    assert resumed["embedded"] == 0  # receipts still cover the corpus
    await service.verify(migration_id)
    await service.run_gate(migration_id, lambda _ctx: _const_verdict({"passed": True}))
    await service.cutover(migration_id, retention_seconds=3600)
    assert (await _dataset_row(pool, "ds-a"))["collection_name"] == shadow

    # -- 8. retention expiry → explicit reclaim -----------------------------
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE dataset_collection_bindings"
            " SET retained_until = NOW() - INTERVAL '1 second'"
            " WHERE binding_id = $1",
            opened["serving_binding"]["binding_id"],
        )
    reclaimable = await store.list_reclaimable_bindings()
    assert [b["collection_name"] for b in reclaimable] == ["kb_ds-a_1024"]
    reclaimed = await service.reclaim_expired()
    assert reclaimed == ["kb_ds-a_1024"]
    assert "kb_ds-a_1024" in vector_store.deleted
    # A serving binding can never be reclaimed out from under the dataset.
    assert await store.retire_binding(str(serving_now["binding_id"])) is None


async def _prepare_ready_migration(
    pool: asyncpg.Pool,
    store: EmbeddingVersionStore,
    *,
    tag: str,
) -> tuple[EmbeddingMigrationService, FakeVectorStore, dict[str, Any]]:
    service, vector_store = _make_service(store)
    opened = await service.start_migration(
        await _dataset_row(pool, "ds-a"),
        target_provider="local",
        target_model="qwen3-embedding",
        target_model_version="2026-08",
        target_dimension=1024,
        migration_tag=tag,
    )
    migration_id = str(opened["migration"]["migration_id"])
    await service.backfill(migration_id, FakeEmbedder(dim=1024))
    await service.verify(migration_id)
    await service.run_gate(
        migration_id,
        lambda _ctx: _const_verdict({"passed": True}),
    )
    assert (await store.get_migration(migration_id))["state"] == "ready"
    return service, vector_store, opened


@pytest.mark.parametrize("drift", ["add", "disable", "modify"])
async def test_cutover_refuses_corpus_drift_after_ready(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
    drift: str,
) -> None:
    """Gate evidence is not a reusable permit after the corpus changes."""

    pool, store = t3_world
    service, _vector_store, opened = await _prepare_ready_migration(
        pool, store, tag=f"drift-{drift}"
    )
    migration_id = str(opened["migration"]["migration_id"])
    async with pool.acquire() as conn:
        if drift == "add":
            await conn.execute(
                """
                INSERT INTO segments (
                    segment_id, dataset_id, document_id, position, text,
                    vector_id, content_hash, metadata
                ) VALUES (
                    'seg-added', 'ds-a', 'doc-a1', 99, 'new after gate',
                    'vec-added', 'hash-added', '{}'::jsonb
                )
                """
            )
        elif drift == "disable":
            await conn.execute("UPDATE segments SET enabled = FALSE WHERE segment_id = 'seg-a1-0'")
        else:
            await conn.execute(
                "UPDATE segments SET text = 'edited after gate',"
                " content_hash = 'hash-edited-after-gate'"
                " WHERE segment_id = 'seg-a1-0'"
            )
        await conn.execute(
            "UPDATE datasets SET content_revision = content_revision + 1 WHERE dataset_id = 'ds-a'"
        )

    with pytest.raises(MigrationStateError, match="drifted"):
        await service.cutover(migration_id)

    dataset = await _dataset_row(pool, "ds-a")
    assert dataset["collection_name"] == "kb_ds-a_1024"
    assert (await store.get_migration(migration_id))["state"] == "ready"
    assert (await store.get_binding(opened["serving_binding"]["binding_id"]))["state"] == "serving"
    assert (await store.get_binding(opened["target_binding"]["binding_id"]))["state"] == "shadow"


async def test_cutover_refuses_stale_target_point_after_ready(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    """Same-count target corruption is caught by the source digest."""

    pool, store = t3_world
    service, vector_store, opened = await _prepare_ready_migration(pool, store, tag="stale-target")
    migration_id = str(opened["migration"]["migration_id"])
    target_collection = str(opened["target_binding"]["collection_name"])
    point = vector_store.points[f"{target_collection}:vec-a1-0"]
    point.payload["text"] = "stale target payload"

    with pytest.raises(MigrationStateError, match="target collection drifted"):
        await service.cutover(migration_id)

    assert (await _dataset_row(pool, "ds-a"))["collection_name"] == "kb_ds-a_1024"
    assert (await store.get_migration(migration_id))["state"] == "ready"


async def _const_verdict(verdict: dict[str, Any]) -> dict[str, Any]:
    return verdict


async def _raising_evaluator(_ctx: dict[str, Any]) -> dict[str, Any]:
    raise RuntimeError("eval harness unavailable")


async def test_cutover_aborts_when_pointer_moved_underneath(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = t3_world
    service, _vector_store = _make_service(store)
    dataset = await _dataset_row(pool, "ds-a")
    opened = await service.start_migration(
        dataset,
        target_provider="local",
        target_model="qwen3-embedding",
        target_dimension=1024,
        migration_tag="vx",
    )
    migration_id = opened["migration"]["migration_id"]
    await service.backfill(migration_id, FakeEmbedder(dim=1024))
    await service.verify(migration_id)
    await service.run_gate(migration_id, lambda _ctx: _const_verdict({"passed": True}))
    # Somebody else re-pointed the datasets row between gate and cutover.
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE datasets SET collection_name = 'kb_hijacked_1024' WHERE dataset_id = 'ds-a'"
        )
    with pytest.raises(MigrationStateError, match="aborted"):
        await service.cutover(migration_id)
    # The migration stays ready (retryable), bindings untouched.
    assert (await store.get_migration(migration_id))["state"] == "ready"


async def test_rollback_after_newer_generation_is_refused(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = t3_world
    service, _vector_store = _make_service(store)
    dataset = await _dataset_row(pool, "ds-a")
    first = await service.start_migration(
        dataset,
        target_provider="local",
        target_model="qwen3-embedding",
        target_dimension=1024,
        migration_tag="gen1",
    )
    await service.backfill(first["migration"]["migration_id"], FakeEmbedder(dim=1024))
    await service.verify(first["migration"]["migration_id"])
    await service.run_gate(
        first["migration"]["migration_id"], lambda _ctx: _const_verdict({"passed": True})
    )
    await service.cutover(first["migration"]["migration_id"], retention_seconds=3600)

    # A second generation takes over; the first migration must no longer roll back.
    updated = await _dataset_row(pool, "ds-a")
    second = await service.start_migration(
        updated,
        target_provider="local",
        target_model="qwen3-embedding",
        target_model_version="2026-12",
        target_dimension=1024,
        migration_tag="gen2",
    )
    await service.backfill(second["migration"]["migration_id"], FakeEmbedder(dim=1024))
    await service.verify(second["migration"]["migration_id"])
    await service.run_gate(
        second["migration"]["migration_id"], lambda _ctx: _const_verdict({"passed": True})
    )
    await service.cutover(second["migration"]["migration_id"], retention_seconds=3600)
    with pytest.raises(MigrationStateError, match="stale"):
        await service.rollback(first["migration"]["migration_id"])


async def test_rollback_serializes_with_expired_binding_reclamation(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    """A reclaim cannot retire the source between rollback validation and
    promotion, even while rollback is blocked on the datasets pointer row."""
    pool, store = t3_world
    serving = await store.get_serving_binding("ds-a")
    assert serving is not None
    target = await store.create_binding(
        dataset_id="ds-a",
        tenant_id="tenant-a",
        collection_name="kb_ds-a_1024_vrace",
        embedding_provider="local",
        embedding_model="qwen3-embedding",
        embedding_model_version="2026-08",
        embedding_dimension=1024,
    )
    migration = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str(target["binding_id"]),
    )
    migration_id = str(migration["migration_id"])
    pending_rows = await store.list_pending_segments(migration_id, dataset_id="ds-a")
    await store.record_progress_receipts(
        migration_id,
        [
            {
                "segment_id": row["segment_id"],
                "document_id": row["document_id"],
                "position": row["position"],
                "vector_id": row["vector_id"],
                "content_hash": row["content_hash"],
            }
            for row in pending_rows
        ],
    )
    authority = await store.authority_snapshot("ds-a")
    target_scope = {
        key: authority[key] for key in ("point_count", "point_ids_sha256", "source_text_sha256")
    }
    gating = await store.transition_migration(
        migration_id,
        to_state="gating",
        from_states=["shadow_build"],
    )
    assert gating is not None
    ready = await store.record_gate_verdict(
        migration_id,
        verdict={
            "passed": True,
            "authority_snapshot": authority,
            "target_scope": target_scope,
        },
        passed=True,
    )
    assert ready is not None
    await store.cutover_migration(
        migration_id,
        retention_seconds=0,
        target_scope=target_scope,
    )

    rollback_has_lock = asyncio.Event()
    reclaim_requested_lock = asyncio.Event()
    original_acquire = store._acquire_dataset_lock

    async def tracked_acquire(conn: Any, dataset_id: str) -> None:
        task = asyncio.current_task()
        task_name = task.get_name() if task is not None else ""
        if task_name == "retire-binding":
            reclaim_requested_lock.set()
        await original_acquire(conn, dataset_id)
        if task_name == "rollback-migration":
            rollback_has_lock.set()

    store._acquire_dataset_lock = tracked_acquire  # type: ignore[method-assign]
    rollback_task: asyncio.Task[dict[str, Any]] | None = None
    retire_task: asyncio.Task[dict[str, Any] | None] | None = None
    try:
        async with pool.acquire() as blocker, blocker.transaction():
            await blocker.fetchrow(
                "SELECT dataset_id FROM datasets WHERE dataset_id = 'ds-a' FOR UPDATE"
            )
            rollback_task = asyncio.create_task(
                store.rollback_migration(str(migration["migration_id"])),
                name="rollback-migration",
            )
            await asyncio.wait_for(rollback_has_lock.wait(), timeout=10)
            retire_task = asyncio.create_task(
                store.retire_binding(str(serving["binding_id"])),
                name="retire-binding",
            )
            await asyncio.wait_for(reclaim_requested_lock.wait(), timeout=10)
            await asyncio.sleep(0)
            assert not retire_task.done(), "reclaim bypassed the rollback dataset lock"

        rolled_back = await asyncio.wait_for(rollback_task, timeout=10)
        retired = await asyncio.wait_for(retire_task, timeout=10)
    finally:
        for task in (rollback_task, retire_task):
            if task is not None and not task.done():
                task.cancel()
        await asyncio.gather(
            *(task for task in (rollback_task, retire_task) if task is not None),
            return_exceptions=True,
        )

    assert rolled_back["state"] == "rolled_back"
    assert retired is None
    dataset = await _dataset_row(pool, "ds-a")
    restored = await store.get_binding(str(serving["binding_id"]))
    migrated = await store.get_binding(str(target["binding_id"]))
    assert dataset["collection_name"] == serving["collection_name"]
    assert restored is not None and restored["state"] == "serving"
    assert migrated is not None and migrated["state"] == "shadow"


async def test_abort_keeps_serving_collection_and_purges_shadow(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = t3_world
    service, vector_store = _make_service(store)
    dataset = await _dataset_row(pool, "ds-a")
    opened = await service.start_migration(
        dataset,
        target_provider="local",
        target_model="qwen3-embedding",
        target_dimension=1024,
        migration_tag="vbad",
    )
    migration_id = opened["migration"]["migration_id"]
    embedder = FakeEmbedder(dim=1024, fail_after=0)
    with pytest.raises(RuntimeError):
        await service.backfill(migration_id, embedder)
    abandoned = await service.abort(migration_id, reason="operator cancelled")
    assert abandoned["state"] == "abandoned"
    # Serving pointer + row never moved; old collection never written or deleted.
    current = await _dataset_row(pool, "ds-a")
    assert current["collection_name"] == "kb_ds-a_1024"
    assert not any(name == "kb_ds-a_1024" for name, _ in vector_store.writes)
    assert "kb_ds-a_1024" not in vector_store.deleted
    # The failed shadow generation is retired AND physically removed (zero orphans).
    shadow = await store.get_binding(opened["target_binding"]["binding_id"])
    assert shadow["state"] == "retired"
    assert "kb_ds-a_1024_vvbad" in vector_store.deleted
    # And a fresh migration may reuse the released name immediately.
    retry = await store.create_binding(
        dataset_id="ds-a",
        tenant_id="tenant-a",
        collection_name="kb_ds-a_1024_vvbad",
        embedding_provider="local",
        embedding_model="qwen3-embedding",
        embedding_dimension=1024,
    )
    assert retry["state"] == "shadow"


async def test_mixed_dimension_drift_fails_backfill_closed(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = t3_world
    service, _vector_store = _make_service(store)
    dataset = await _dataset_row(pool, "ds-a")
    opened = await service.start_migration(
        dataset,
        target_provider="local",
        target_model="qwen3-embedding",
        target_dimension=1024,
        migration_tag="vdim",
    )
    migration_id = opened["migration"]["migration_id"]
    # Embedder returns the wrong width: the page must not be receipted.
    with pytest.raises(EmbeddingMigrationError, match="dimension drift"):
        await service.backfill(migration_id, FakeEmbedder(dim=512))
    assert (await store.get_migration(migration_id))["state"] == "failed"
    assert await store.count_pending_segments(migration_id, dataset_id="ds-a") == 3
    # A same-identity migration is rejected (that's the T1 in-place verb).
    with pytest.raises(EmbeddingMigrationError, match="reembed"):
        await service.start_migration(
            dataset,
            target_provider="dashscope",
            target_model="text-embedding-v4",
            target_dimension=1024,
        )


# ------------------------------------------------------------ vector cache (T3.4)


async def test_vector_cache_is_batched_and_version_keyed(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    _pool, store = t3_world
    identity_v1 = {
        "embedding_provider": "dashscope",
        "embedding_model": "text-embedding-v4",
        "embedding_model_version": "2026-01",
    }
    stored = await store.store_embeddings_batch(
        **identity_v1,
        entries=[("h1", [0.5, 1.5]), ("h2", [2.5]), ("", [9.9])],
    )
    assert stored == 2  # the empty-hash entry is dropped
    hits = await store.lookup_embeddings_batch(**identity_v1, content_hashes=["h1", "h2", "h3", ""])
    assert set(hits) == {"h1", "h2"}
    assert hits["h1"] == [0.5, 1.5]
    # The same content under a new model version is a cache MISS — vectors
    # from two embedding generations can never cross-feed (T3 item 1/4).
    misses = await store.lookup_embeddings_batch(
        embedding_provider="dashscope",
        embedding_model="text-embedding-v4",
        embedding_model_version="2026-08",
        content_hashes=["h1", "h2"],
    )
    assert misses == {}
    # Upsert refreshes in place (no duplicates).
    await store.store_embeddings_batch(**identity_v1, entries=[("h1", [7.0])])
    refreshed = await store.lookup_embeddings_batch(**identity_v1, content_hashes=["h1"])
    assert refreshed["h1"] == [7.0]
    # Purge on model-identity retirement.
    deleted = await store.purge_vector_cache_for_model(**identity_v1)
    assert deleted == 2
    assert await store.lookup_embeddings_batch(**identity_v1, content_hashes=["h1", "h2"]) == {}
    with pytest.raises(ValueError):
        await store.lookup_embeddings_batch(
            embedding_provider="",
            embedding_model="",
            embedding_model_version="",
            content_hashes=["h1"],
        )


# -------------------------------------------------------------- describe surface


async def test_describe_reports_serving_binding_and_live_progress(
    t3_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = t3_world
    service, _vector_store = _make_service(store)
    dataset = await _dataset_row(pool, "ds-a")
    before = await service.describe(dict(dataset))
    assert before["serving_binding"]["collection_name"] == "kb_ds-a_1024"
    assert before["live_migration"] is None
    assert before["enabled_chunks"] == 3

    opened = await service.start_migration(
        dataset,
        target_provider="local",
        target_model="qwen3-embedding",
        target_dimension=1024,
        migration_tag="vd",
    )
    during = await service.describe(dict(dataset))
    assert during["live_migration"]["migration_id"] == opened["migration"]["migration_id"]
    assert during["pending_chunks"] == 3
    assert json.dumps(during, default=str)  # API-serializable
