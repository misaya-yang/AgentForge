"""Real-PostgreSQL contracts for durable embedding migration action jobs."""

from __future__ import annotations

import asyncio
import os
import uuid
from collections.abc import AsyncIterator
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import asyncpg
import pytest
import pytest_asyncio
from dotenv import dotenv_values
from knowledge_service.api.routes.embedding_migration import backfill_migration
from knowledge_service.persistence.embedding_version_store import (
    EmbeddingVersionStore,
    MigrationStateError,
    action_request_hash,
)
from knowledge_service.services.knowledge.embedding_migration import (
    EmbeddingMigrationService,
)
from knowledge_service.services.knowledge.embedding_migration_worker import (
    EmbeddingMigrationJobWorker,
)

ROOT = Path(__file__).resolve().parents[2]
MIGRATION_102 = ROOT / "database/migrations/102_kb_embedding_versioning_blue_green.sql"
MIGRATION_110 = ROOT / "database/migrations/110_kb_embedding_migration_action_jobs.sql"


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
async def action_world() -> AsyncIterator[tuple[asyncpg.Pool, EmbeddingVersionStore]]:
    config = _postgres_config()
    database_name = f"kb_embedding_jobs_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{database_name}"')
    database_config = {**config, "database": database_name}
    bootstrap = await asyncpg.connect(**database_config)
    try:
        await bootstrap.execute(
            """
            CREATE TABLE datasets (
                dataset_id VARCHAR(255) PRIMARY KEY,
                tenant_id VARCHAR(255) NOT NULL DEFAULT '',
                embedding_provider VARCHAR(50) NOT NULL DEFAULT 'local',
                embedding_model VARCHAR(100) NOT NULL DEFAULT 'model-v1',
                embedding_dimension INTEGER NOT NULL DEFAULT 3,
                collection_name VARCHAR(255),
                content_revision BIGINT NOT NULL DEFAULT 0,
                is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
                created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
            );
            CREATE TABLE documents (
                document_id VARCHAR(255) PRIMARY KEY,
                dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id),
                title VARCHAR(255) NOT NULL,
                status VARCHAR(50) NOT NULL DEFAULT 'completed',
                enabled BOOLEAN NOT NULL DEFAULT TRUE,
                archived BOOLEAN NOT NULL DEFAULT FALSE,
                metadata JSONB NOT NULL DEFAULT '{}'::jsonb
            );
            CREATE TABLE segments (
                segment_id VARCHAR(255) PRIMARY KEY,
                dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id),
                document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id),
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
                embedding_dimension, collection_name
            ) VALUES (
                'ds-a', 'tenant-a', 'local', 'model-v1', 3, 'kb_ds_a_v1'
            );
            INSERT INTO documents (document_id, dataset_id, title)
            VALUES ('doc-a', 'ds-a', 'doc');
            INSERT INTO segments (
                segment_id, dataset_id, document_id, position, text,
                vector_id, content_hash
            ) VALUES (
                'seg-a', 'ds-a', 'doc-a', 0, 'alpha', 'vec-a', 'hash-a'
            );
            """
        )
        await bootstrap.execute(MIGRATION_102.read_text(encoding="utf-8"))
        migration_110 = MIGRATION_110.read_text(encoding="utf-8")
        await bootstrap.execute(migration_110)
        await bootstrap.execute(migration_110)
    except BaseException:
        await bootstrap.close()
        await admin.execute(f'DROP DATABASE "{database_name}"')
        await admin.close()
        raise
    else:
        await bootstrap.close()

    pool = await asyncpg.create_pool(
        **database_config,
        min_size=1,
        max_size=6,
        server_settings={"search_path": "knowledge,gateway,assistant,public"},
    )
    try:
        yield pool, EmbeddingVersionStore(pool)
    finally:
        await pool.close()
        await admin.execute(f'DROP DATABASE "{database_name}"')
        await admin.close()


async def _open_migration(store: EmbeddingVersionStore) -> dict[str, Any]:
    serving = await store.get_serving_binding("ds-a")
    assert serving is not None
    target = await store.create_binding(
        dataset_id="ds-a",
        tenant_id="tenant-a",
        collection_name=f"kb_ds_a_{uuid.uuid4().hex[:8]}",
        embedding_provider="local",
        embedding_model="model-v2",
        embedding_dimension=3,
    )
    return await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str(target["binding_id"]),
    )


async def test_110_is_idempotent_and_owns_knowledge_schema(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = action_world
    await store.require_action_job_store()
    async with pool.acquire() as conn:
        namespace = await conn.fetchval(
            """
            SELECT n.nspname
            FROM pg_class c
            JOIN pg_namespace n ON n.oid = c.relnamespace
            WHERE c.relname = 'embedding_migration_action_jobs'
            """
        )
        constraint_validated = await conn.fetchval(
            """
            SELECT c.convalidated
            FROM pg_constraint c
            JOIN pg_class r ON r.oid = c.conrelid
            JOIN pg_namespace n ON n.oid = r.relnamespace
            WHERE n.nspname = 'knowledge'
              AND r.relname = 'embedding_migration_action_jobs'
              AND c.conname = 'fk_kb_embedding_action_migration_dataset'
            """
        )
    assert namespace == "knowledge"
    assert constraint_validated is True


def test_action_request_hash_is_canonical_and_action_bound() -> None:
    left = action_request_hash("gate", {"top_k": 3, "sample_size": 8})
    right = action_request_hash("gate", {"sample_size": 8, "top_k": 3})
    assert left == right
    assert left != action_request_hash("verify", {"sample_size": 8, "top_k": 3})


async def test_job_fk_rejects_cross_dataset_migration_pair(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = action_world
    migration = await _open_migration(store)
    async with pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO datasets (
                dataset_id, tenant_id, embedding_provider, embedding_model,
                embedding_dimension, collection_name
            ) VALUES ('ds-b', 'tenant-b', 'local', 'm', 3, 'kb_ds_b')
            """
        )
        with pytest.raises(asyncpg.ForeignKeyViolationError):
            await conn.execute(
                """
                INSERT INTO knowledge.embedding_migration_action_jobs (
                    migration_id, dataset_id, action, request_hash
                ) VALUES ($1::uuid, 'ds-b', 'backfill', $2)
                """,
                migration["migration_id"],
                "0" * 64,
            )


async def test_concurrent_same_action_reuses_one_job(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])

    submissions = await asyncio.gather(
        *[
            store.enqueue_action_job(
                migration_id,
                action="backfill",
                requested_by=f"user-{index}",
            )
            for index in range(12)
        ]
    )
    assert len({job["job_id"] for job, _reused in submissions}) == 1
    assert len({job["request_hash"] for job, _reused in submissions}) == 1
    assert sum(not reused for _job, reused in submissions) == 1
    async with pool.acquire() as conn:
        count = await conn.fetchval(
            "SELECT count(*) FROM knowledge.embedding_migration_action_jobs"
        )
    assert count == 1

    with pytest.raises(MigrationStateError, match="active 'backfill'"):
        await store.enqueue_action_job(migration_id, action="verify")


async def test_action_payload_validation_is_fail_closed_and_canonical(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    _pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    with pytest.raises(ValueError, match="does not accept parameters"):
        await store.enqueue_action_job(
            migration_id,
            action="backfill",
            payload={"sample_size": 8},
        )
    with pytest.raises(ValueError, match="unsupported gate parameters"):
        await store.enqueue_action_job(
            migration_id,
            action="gate",
            payload={"ignored": True},
        )
    with pytest.raises(ValueError, match="between 0 and 1"):
        await store.enqueue_action_job(
            migration_id,
            action="gate",
            payload={"floor": float("nan")},
        )

    assert await store.transition_migration(
        migration_id,
        to_state="verified",
        from_states=("shadow_build",),
    ) is not None
    first, reused = await store.enqueue_action_job(
        migration_id,
        action="gate",
        payload={"floor": 0},
    )
    replay, replay_reused = await store.enqueue_action_job(
        migration_id,
        action="gate",
        payload={"floor": 0.0},
    )
    assert reused is False
    assert replay_reused is True
    assert replay["job_id"] == first["job_id"]
    assert replay["request_hash"] == first["request_hash"]


async def test_cross_process_claim_and_token_cas(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    queued, _ = await store.enqueue_action_job(migration_id, action="backfill")
    peer = EmbeddingVersionStore(pool)

    first, second = await asyncio.gather(
        store.claim_next_action_job(worker_id="worker-a", lease_seconds=60),
        peer.claim_next_action_job(worker_id="worker-b", lease_seconds=60),
    )
    claimed = first or second
    assert claimed is not None
    assert (first is None) != (second is None)
    assert claimed["job_id"] == queued["job_id"]

    wrong = await store.finish_action_job(
        claimed["job_id"],
        claim_token=str(uuid.uuid4()),
        result={"ok": False},
    )
    assert wrong is None
    done = await store.finish_action_job(
        claimed["job_id"],
        claim_token=str(claimed["claim_token"]),
        result={"ok": True},
    )
    assert done is not None and done["state"] == "succeeded"
    repeated = await store.finish_action_job(
        claimed["job_id"],
        claim_token=str(claimed["claim_token"]),
        result={"ignored_replay_payload": True},
    )
    assert repeated is not None and repeated["result"] == {"ok": True}


async def test_completed_action_rejects_parameter_hash_drift(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    await store.record_progress_receipts(
        migration_id,
        [
            {
                "segment_id": "seg-a",
                "document_id": "doc-a",
                "position": 0,
                "vector_id": "vec-a",
                "content_hash": "hash-a",
            }
        ],
    )
    assert await store.transition_migration(
        migration_id,
        to_state="verified",
        from_states=("shadow_build",),
    ) is not None
    queued, _ = await store.enqueue_action_job(
        migration_id,
        action="gate",
        payload={"sample_size": 8},
    )
    claimed = await store.claim_next_action_job(
        worker_id="worker-a", lease_seconds=60
    )
    assert claimed is not None
    assert await store.transition_migration(
        migration_id,
        to_state="gating",
        from_states=("verified",),
    ) is not None
    authority = await store.authority_snapshot("ds-a")
    assert await store.record_gate_verdict(
        migration_id,
        verdict={"passed": True, "authority_snapshot": authority},
        passed=True,
    ) is not None
    assert await store.finish_action_job(
        queued["job_id"],
        claim_token=str(claimed["claim_token"]),
        result={"passed": True},
    ) is not None

    same, reused = await store.enqueue_action_job(
        migration_id,
        action="gate",
        payload={"sample_size": 8},
    )
    assert reused is True and same["job_id"] == queued["job_id"]
    with pytest.raises(MigrationStateError, match="different parameters"):
        await store.enqueue_action_job(
            migration_id,
            action="gate",
            payload={"sample_size": 9},
        )
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE datasets SET content_revision = content_revision + 1"
            " WHERE dataset_id = 'ds-a'"
        )
    with pytest.raises(MigrationStateError, match="cannot enqueue"):
        await store.enqueue_action_job(
            migration_id,
            action="gate",
            payload={"sample_size": 8},
        )


async def test_verify_success_is_not_reused_after_corpus_revision_changes(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    await store.record_progress_receipts(
        migration_id,
        [
            {
                "segment_id": "seg-a",
                "document_id": "doc-a",
                "position": 0,
                "vector_id": "vec-a",
                "content_hash": "hash-a",
            }
        ],
    )
    assert await store.transition_migration(
        migration_id,
        to_state="backfilling",
        from_states=("shadow_build",),
    ) is not None
    first, _ = await store.enqueue_action_job(migration_id, action="verify")
    claimed = await store.claim_next_action_job(
        worker_id="worker-a", lease_seconds=60
    )
    assert claimed is not None
    authority = await store.authority_snapshot("ds-a")
    await store.merge_migration_progress(
        migration_id,
        totals={"verified_authority": authority},
    )
    assert await store.transition_migration(
        migration_id,
        to_state="verified",
        from_states=("backfilling",),
    ) is not None
    assert await store.finish_action_job(
        first["job_id"],
        claim_token=str(claimed["claim_token"]),
        result={"state": "verified"},
    ) is not None

    same, reused = await store.enqueue_action_job(migration_id, action="verify")
    assert reused is True and same["job_id"] == first["job_id"]
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE datasets SET content_revision = content_revision + 1"
            " WHERE dataset_id = 'ds-a'"
        )
    changed, changed_reused = await store.enqueue_action_job(
        migration_id, action="verify"
    )
    assert changed_reused is False
    assert changed["job_id"] != first["job_id"]


async def test_failed_job_requeues_same_id_and_can_succeed(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    _pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    queued, _ = await store.enqueue_action_job(migration_id, action="backfill")
    claimed = await store.claim_next_action_job(
        worker_id="worker-a", lease_seconds=60
    )
    assert claimed is not None
    failed = await store.fail_action_job(
        claimed["job_id"],
        claim_token=str(claimed["claim_token"]),
        error="provider unavailable",
    )
    assert failed is not None and failed["state"] == "failed"
    repeated_failure = await store.fail_action_job(
        claimed["job_id"],
        claim_token=str(claimed["claim_token"]),
        error="different replay error",
    )
    assert repeated_failure is not None
    assert repeated_failure["error"] == "provider unavailable"

    retried, reused = await store.enqueue_action_job(
        migration_id, action="backfill"
    )
    assert reused is True
    assert retried["job_id"] == queued["job_id"]
    assert retried["state"] == "queued"
    claimed_again = await store.claim_next_action_job(
        worker_id="worker-b", lease_seconds=60
    )
    assert claimed_again is not None
    assert claimed_again["attempt_count"] == 2


async def test_failed_job_with_new_parameters_gets_new_audit_row(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    _pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    assert await store.transition_migration(
        migration_id,
        to_state="verified",
        from_states=("shadow_build",),
    ) is not None
    first, _ = await store.enqueue_action_job(
        migration_id,
        action="gate",
        payload={"sample_size": 8},
    )
    claimed = await store.claim_next_action_job(
        worker_id="worker-a", lease_seconds=60
    )
    assert claimed is not None
    assert await store.fail_action_job(
        first["job_id"],
        claim_token=str(claimed["claim_token"]),
        error="temporary evaluator outage",
    ) is not None

    changed, reused = await store.enqueue_action_job(
        migration_id,
        action="gate",
        payload={"sample_size": 9},
    )
    assert reused is False
    assert changed["job_id"] != first["job_id"]
    history = await store.list_recent_action_jobs(migration_id)
    assert {job["job_id"] for job in history} == {
        first["job_id"],
        changed["job_id"],
    }


async def test_abort_cancels_queued_job_atomically(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    _pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    queued, _ = await store.enqueue_action_job(
        migration_id, action="backfill"
    )

    abandoned = await store.abandon_migration(migration_id, reason="operator abort")
    assert abandoned["state"] == "abandoned"
    job = await store.get_action_job(queued["job_id"])
    assert job is not None and job["state"] == "failed"
    assert job["error"] == "cancelled by migration abort"
    assert await store.claim_next_action_job(
        worker_id="worker-a", lease_seconds=60
    ) is None


async def test_abort_refuses_running_job(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    _pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    await store.enqueue_action_job(migration_id, action="backfill")
    claimed = await store.claim_next_action_job(
        worker_id="worker-a", lease_seconds=60
    )
    assert claimed is not None

    with pytest.raises(MigrationStateError, match="job is running"):
        await store.abandon_migration(migration_id)
    assert (await store.get_migration(migration_id))["state"] == "shadow_build"


@pytest.mark.parametrize("terminal_state", ["completed", "rolled_back", "abandoned"])
async def test_terminal_migration_cannot_enqueue_action(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
    terminal_state: str,
) -> None:
    pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE embedding_migrations SET state = $2 WHERE migration_id = $1::uuid",
            migration_id,
            terminal_state,
        )
    for action in ("backfill", "verify", "gate"):
        with pytest.raises(MigrationStateError, match="cannot enqueue"):
            await store.enqueue_action_job(migration_id, action=action)


async def test_abandoned_migration_does_not_reuse_historical_backfill(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    queued, _ = await store.enqueue_action_job(migration_id, action="backfill")
    claimed = await store.claim_next_action_job(
        worker_id="worker-a", lease_seconds=60
    )
    assert claimed is not None
    assert await store.transition_migration(
        migration_id,
        to_state="backfilling",
        from_states=("shadow_build",),
    ) is not None
    assert await store.finish_action_job(
        queued["job_id"],
        claim_token=str(claimed["claim_token"]),
        result={"pending": 0},
    ) is not None
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE embedding_migrations SET state = 'abandoned'"
            " WHERE migration_id = $1::uuid",
            migration_id,
        )

    with pytest.raises(MigrationStateError, match="cannot enqueue"):
        await store.enqueue_action_job(migration_id, action="backfill")


async def test_worker_restart_reclaims_expired_lease_without_late_commit(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    await store.enqueue_action_job(migration_id, action="backfill")
    abandoned = await store.claim_next_action_job(
        worker_id="dead-worker", lease_seconds=60
    )
    assert abandoned is not None
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE knowledge.embedding_migration_action_jobs"
            " SET lease_expires_at = NOW() - INTERVAL '1 second'"
            " WHERE job_id = $1::uuid",
            abandoned["job_id"],
        )

    recovered = await store.claim_next_action_job(
        worker_id="replacement-worker", lease_seconds=60
    )
    assert recovered is not None
    assert recovered["job_id"] == abandoned["job_id"]
    assert recovered["recovered_from_running"] is True
    assert recovered["claim_token"] != abandoned["claim_token"]
    assert await store.heartbeat_action_job(
        abandoned["job_id"],
        claim_token=str(abandoned["claim_token"]),
        lease_seconds=60,
    ) is False
    assert (
        await store.finish_action_job(
            abandoned["job_id"],
            claim_token=str(abandoned["claim_token"]),
            result={"late": True},
        )
        is None
    )
    finished = await store.finish_action_job(
        recovered["job_id"],
        claim_token=str(recovered["claim_token"]),
        result={"recovered": True},
    )
    assert finished is not None and finished["state"] == "succeeded"


async def test_describe_recovers_scoped_jobs_without_claim_capabilities(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    _pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    queued, _ = await store.enqueue_action_job(
        migration_id,
        action="backfill",
        requested_by="owner-a",
    )
    claimed = await store.claim_next_action_job(
        worker_id="worker-secret", lease_seconds=60
    )
    assert claimed is not None
    service = EmbeddingMigrationService(
        store=store,
        vector_store=SimpleNamespace(),
    )
    dataset = {
        "dataset_id": "ds-a",
        "tenant_id": "tenant-a",
        "collection_name": "kb_ds_a_v1",
        "embedding_provider": "local",
        "embedding_model": "model-v1",
        "embedding_dimension": 3,
    }

    running = await service.describe(dataset)
    assert running["active_action_job"]["job_id"] == queued["job_id"]
    assert running["recent_action_jobs"] == []
    assert "claim_token" not in running["active_action_job"]
    assert "terminal_claim_token" not in running["active_action_job"]
    assert "claimed_by" not in running["active_action_job"]
    polled = await service.get_action_job(
        queued["job_id"],
        migration_id=migration_id,
        dataset_id="ds-a",
    )
    assert polled is not None and polled["state"] == "running"
    assert "claim_token" not in polled
    assert "claimed_by" not in polled
    assert await service.get_action_job(
        queued["job_id"],
        migration_id=migration_id,
        dataset_id="another-dataset",
    ) is None
    assert await store.get_scoped_action_job(
        queued["job_id"],
        migration_id=migration_id,
        dataset_id="another-dataset",
    ) is None

    assert await store.fail_action_job(
        queued["job_id"],
        claim_token=str(claimed["claim_token"]),
        error="provider unavailable",
    ) is not None
    terminal = await service.describe(dataset)
    assert terminal["active_action_job"] is None
    assert terminal["recent_action_jobs"][0]["job_id"] == queued["job_id"]
    assert terminal["recent_action_jobs"][0]["state"] == "failed"
    assert "terminal_claim_token" not in terminal["recent_action_jobs"][0]


async def test_dataset_scope_surfaces_old_terminal_migration_worker_receipt(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    _pool, store = action_world
    old = await _open_migration(store)
    old_id = str(old["migration_id"])
    assert await store.transition_migration(
        old_id,
        to_state="verified",
        from_states=("shadow_build",),
    ) is not None
    old_job, _ = await store.enqueue_action_job(old_id, action="gate")
    claimed = await store.claim_next_action_job(
        worker_id="old-worker", lease_seconds=60
    )
    assert claimed is not None
    assert await store.transition_migration(
        old_id,
        to_state="completed",
        from_states=("verified",),
    ) is not None

    serving = await store.get_serving_binding("ds-a")
    assert serving is not None
    next_target = await store.create_binding(
        dataset_id="ds-a",
        tenant_id="tenant-a",
        collection_name=f"kb_ds_a_{uuid.uuid4().hex[:8]}",
        embedding_provider="local",
        embedding_model="model-v3",
        embedding_dimension=3,
    )
    current = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str(next_target["binding_id"]),
    )
    with pytest.raises(MigrationStateError, match="dataset already has an active"):
        await store.enqueue_action_job(
            str(current["migration_id"]), action="backfill"
        )

    service = EmbeddingMigrationService(
        store=store,
        vector_store=SimpleNamespace(),
    )
    description = await service.describe(
        {
            "dataset_id": "ds-a",
            "tenant_id": "tenant-a",
            "collection_name": "kb_ds_a_v1",
            "embedding_provider": "local",
            "embedding_model": "model-v1",
            "embedding_dimension": 3,
        }
    )
    assert description["live_migration"]["migration_id"] == current["migration_id"]
    assert description["active_action_job"]["job_id"] == old_job["job_id"]
    assert description["active_action_job"]["migration_id"] == old_id


class ControlledMigrationService:
    def __init__(self, store: EmbeddingVersionStore, *, fail_first: bool = False) -> None:
        self.store = store
        self.fail_first = fail_first
        self.calls = 0
        self.started = asyncio.Event()
        self.release = asyncio.Event()

    async def backfill(self, migration_id: str) -> dict[str, Any]:
        self.calls += 1
        self.started.set()
        await self.release.wait()
        if self.fail_first and self.calls == 1:
            raise RuntimeError("simulated provider failure")
        return {"migration_id": migration_id, "simulated_duration_seconds": 31}


async def test_workers_execute_once_and_disconnect_does_not_cancel(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    _pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    await store.enqueue_action_job(migration_id, action="backfill")
    controlled = ControlledMigrationService(store)
    service = SimpleNamespace(embedding_migration_service=controlled, db=SimpleNamespace())
    worker_a = EmbeddingMigrationJobWorker(
        service,
        worker_id="worker-a",
        lease_seconds=2,
        heartbeat_interval_seconds=0.02,
    )
    worker_b = EmbeddingMigrationJobWorker(
        service,
        worker_id="worker-b",
        lease_seconds=2,
        heartbeat_interval_seconds=0.02,
    )

    executions = [
        asyncio.create_task(worker_a.run_once()),
        asyncio.create_task(worker_b.run_once()),
    ]
    await asyncio.wait_for(controlled.started.wait(), timeout=1)
    disconnected_client = asyncio.create_task(asyncio.sleep(60))
    disconnected_client.cancel()
    await asyncio.gather(disconnected_client, return_exceptions=True)
    await asyncio.sleep(0.05)
    assert controlled.calls == 1

    controlled.release.set()
    outcomes = await asyncio.gather(*executions)
    assert sorted(outcomes) == [False, True]
    jobs = await store.list_recent_action_jobs(migration_id)
    assert jobs[0]["state"] == "succeeded"
    assert jobs[0]["result"]["simulated_duration_seconds"] == 31


async def test_real_pg_enqueue_commits_after_http_task_disconnect(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    service = EmbeddingMigrationService(
        store=store,
        vector_store=SimpleNamespace(),
    )

    async def require_dataset_access(
        _user: Any, dataset_id: str, required: str = "viewer"
    ) -> dict[str, Any]:
        assert required == "owner"
        return {"dataset_id": dataset_id, "tenant_id": "tenant-a"}

    api_service = SimpleNamespace(
        embedding_migration_service=service,
        embedding_version_store=store,
        require_dataset_access=require_dataset_access,
    )
    blocker = await pool.acquire()
    transaction = blocker.transaction()
    await transaction.start()
    try:
        await blocker.fetchrow(
            "SELECT migration_id FROM embedding_migrations"
            " WHERE migration_id = $1::uuid FOR UPDATE",
            migration_id,
        )
        request = asyncio.create_task(
            backfill_migration(
                "ds-a",
                migration_id,
                svc=api_service,
                user=SimpleNamespace(user_id="owner-a", tenant_id="tenant-a"),
            )
        )
        await asyncio.sleep(0.05)
        assert not request.done()
        request.cancel()
        request.cancel()
        await asyncio.sleep(0)
        assert not request.done()
    finally:
        await transaction.rollback()
        await pool.release(blocker)

    with pytest.raises(asyncio.CancelledError):
        await asyncio.wait_for(request, timeout=1)
    jobs = await store.list_recent_action_jobs(migration_id)
    assert len(jobs) == 1
    assert jobs[0]["state"] == "queued"
    assert jobs[0]["requested_by"] == "owner-a"


async def test_concurrent_gate_job_invokes_paid_evaluator_once(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    moved = await store.transition_migration(
        migration_id,
        to_state="verified",
        from_states=("shadow_build",),
    )
    assert moved is not None
    submissions = await asyncio.gather(
        *[
            store.enqueue_action_job(
                migration_id,
                action="gate",
                payload={"sample_size": 8},
            )
            for _ in range(8)
        ]
    )
    assert len({job["job_id"] for job, _reused in submissions}) == 1
    with pytest.raises(MigrationStateError, match="different parameters"):
        await store.enqueue_action_job(
            migration_id,
            action="gate",
            payload={"sample_size": 9},
        )

    evaluator_calls = 0

    async def evaluator_factory(
        _service: Any, _dataset: dict[str, Any], **overrides: Any
    ) -> Any:
        assert overrides == {"sample_size": 8}

        async def evaluate(_context: dict[str, Any]) -> dict[str, Any]:
            nonlocal evaluator_calls
            evaluator_calls += 1
            return {"passed": True}

        return evaluate

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.embedding_gate."
        "shadow_serving_gate_evaluator",
        evaluator_factory,
    )

    class GateService:
        def __init__(self) -> None:
            self.store = store
            self.calls = 0

        async def run_gate(self, migration_id: str, evaluate: Any) -> dict[str, Any]:
            self.calls += 1
            verdict = await evaluate({"dataset_id": "ds-a"})
            return {
                "migration_id": migration_id,
                "verdict": verdict,
                "passed": bool(verdict["passed"]),
            }

    async def get_dataset(_dataset_id: str) -> dict[str, Any]:
        return {"dataset_id": "ds-a", "tenant_id": "tenant-a"}

    gate_service = GateService()
    service = SimpleNamespace(
        embedding_migration_service=gate_service,
        db=SimpleNamespace(get_dataset=get_dataset),
    )
    workers = [
        EmbeddingMigrationJobWorker(service, worker_id="gate-a"),
        EmbeddingMigrationJobWorker(service, worker_id="gate-b"),
    ]
    outcomes = await asyncio.gather(*(worker.run_once() for worker in workers))
    assert sorted(outcomes) == [False, True]
    assert gate_service.calls == 1
    assert evaluator_calls == 1
    jobs = await store.list_recent_action_jobs(migration_id)
    assert jobs[0]["result"]["verdict"]["action_job_id"] == jobs[0]["job_id"]


async def test_failed_execution_retries_same_job_without_wedge(
    action_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    _pool, store = action_world
    migration = await _open_migration(store)
    migration_id = str(migration["migration_id"])
    first_job, _ = await store.enqueue_action_job(
        migration_id, action="backfill"
    )
    controlled = ControlledMigrationService(store, fail_first=True)
    controlled.release.set()
    service = SimpleNamespace(embedding_migration_service=controlled, db=SimpleNamespace())
    worker = EmbeddingMigrationJobWorker(service, worker_id="worker-a")

    assert await worker.run_once() is True
    failed = await store.get_action_job(first_job["job_id"])
    assert failed is not None and failed["state"] == "failed"
    retried, reused = await store.enqueue_action_job(
        migration_id, action="backfill"
    )
    assert reused is True and retried["job_id"] == first_job["job_id"]
    assert await worker.run_once() is True
    succeeded = await store.get_action_job(first_job["job_id"])
    assert succeeded is not None and succeeded["state"] == "succeeded"
    assert succeeded["attempt_count"] == 2
    assert controlled.calls == 2
