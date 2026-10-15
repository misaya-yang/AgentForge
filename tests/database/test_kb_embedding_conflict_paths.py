"""Real-PostgreSQL tests for the T3 one-live-per-dataset conflict paths.

Tier-b pattern (same throwaway-schema world as
test_kb_embedding_versioning_migration.py): the partial unique index
``idx_kb_embedding_migrations_one_live_per_dataset`` is the enforcement
point, and every store path that can hit it must surface a caller-visible
MigrationStateError (route-mapped to 409) — never a raw asyncpg
UniqueViolationError (a 500) and never a transaction-context error.

Contracts pinned here:

* ``begin_migration`` on a dataset that already has a live migration raises
  MigrationStateError (adjudicates the review claim that the catch was dead).
* ``transition_migration`` INTO a live state while another migration is live
  raises MigrationStateError (resume/verify races answer 409, not 500).
* ``reopen_migration_for_retry`` reopens a rolled_back migration whose target
  binding is still 'shadow', and maps the same unique-index collision.
* ``abandon_migration`` admits the operator-actionable non-live states
  (gate_failed, rolled_back) — the post-rollback wedge escape — while
  'completed' stays excluded (its target binding is serving).
"""

from __future__ import annotations

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

ROOT = Path(__file__).resolve().parents[2]
MIGRATION_102 = ROOT / "database" / "migrations" / "102_kb_embedding_versioning_blue_green.sql"


def _postgres_config() -> dict[str, Any]:
    import os

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
async def conflict_world() -> AsyncIterator[tuple[asyncpg.Pool, EmbeddingVersionStore]]:
    config = _postgres_config()
    database_name = f"kb_t3_conflict_{uuid.uuid4().hex}"
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
            # Minimal pre-102 shape: only what migration 102 reads/alters.
            await conn.execute(
                """
                CREATE TABLE datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL DEFAULT '',
                    embedding_provider VARCHAR(50) NOT NULL DEFAULT 'gemini',
                    embedding_model VARCHAR(100) NOT NULL DEFAULT 'gemini-embedding-001',
                    embedding_dimension INTEGER NOT NULL DEFAULT 1024,
                    collection_name VARCHAR(255),
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
                VALUES ('ds-a', 'tenant-a', 'dashscope', 'text-embedding-v4',
                        1024, 'kb_ds-a_1024', FALSE);
                """
            )
            await conn.execute(MIGRATION_102.read_text(encoding="utf-8"))
        yield pool, EmbeddingVersionStore(pool)
    finally:
        await pool.close()
        try:
            await admin.execute(f'DROP DATABASE "{database_name}"')
        finally:
            await admin.close()


async def _shadow_binding(store: EmbeddingVersionStore, suffix: str) -> dict[str, Any]:
    return await store.create_binding(
        dataset_id="ds-a",
        tenant_id="tenant-a",
        collection_name=f"kb_ds-a_1024_v{suffix}",
        embedding_provider="local",
        embedding_model=f"model-{suffix}",
        embedding_dimension=1024,
    )


# ---------------------------------------------------------- begin_migration


async def test_second_begin_migration_raises_state_error_not_raw_violation(
    conflict_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    """The one-live-per-dataset collision inside begin_migration must surface
    as MigrationStateError (409 territory). This adjudicates the review claim
    that the UniqueViolationError catch was dead code (it is not)."""
    _pool, store = conflict_world
    serving = await store.get_serving_binding("ds-a")
    assert serving is not None

    first = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str((await _shadow_binding(store, "aaaa"))["binding_id"]),
    )
    assert first["state"] == "shadow_build"

    with pytest.raises(MigrationStateError, match="already has a live embedding migration"):
        await store.begin_migration(
            dataset_id="ds-a",
            source_binding_id=str(serving["binding_id"]),
            target_binding_id=str((await _shadow_binding(store, "bbbb"))["binding_id"]),
        )

    # The failed second attempt left no row behind (transaction rolled back).
    live = await store.get_live_migration("ds-a")
    assert live is not None
    assert live["migration_id"] == first["migration_id"]


# ------------------------------------------------------ transition_migration


async def test_transition_into_live_state_conflicts_as_state_error(
    conflict_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    """Resuming a failed attempt while ANOTHER migration is live must answer
    a caller-visible conflict (409), never a raw UniqueViolationError (500)."""
    _pool, store = conflict_world
    serving = await store.get_serving_binding("ds-a")

    migration_a = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str((await _shadow_binding(store, "aaaa"))["binding_id"]),
    )
    failed = await store.transition_migration(
        str(migration_a["migration_id"]),
        to_state="failed",
        from_states=["shadow_build"],
        error="boom",
    )
    assert failed is not None and failed["state"] == "failed"

    # With A out of the live set, B may open.
    migration_b = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str((await _shadow_binding(store, "bbbb"))["binding_id"]),
    )
    assert migration_b["state"] == "shadow_build"

    # Now A cannot re-enter the live set while B is live.
    with pytest.raises(MigrationStateError, match="already"):
        await store.transition_migration(
            str(migration_a["migration_id"]),
            to_state="backfilling",
            from_states=["failed"],
        )

    unchanged = await store.get_migration(str(migration_a["migration_id"]))
    assert unchanged is not None and unchanged["state"] == "failed"


# ------------------------------------------------ reopen_migration_for_retry


async def test_reopen_rolled_back_migration_with_shadow_target(
    conflict_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    """The retry escape: rolled_back + a still-'shadow' target binding moves
    back to 'backfilling' with the error cleared."""
    pool, store = conflict_world
    serving = await store.get_serving_binding("ds-a")
    migration = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str((await _shadow_binding(store, "aaaa"))["binding_id"]),
    )
    # Direct state placement: this file pins STORE-level contracts, the
    # orchestration path that produces 'rolled_back' is covered separately.
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE embedding_migrations SET state = 'rolled_back', error = 'rolled'"
            " WHERE migration_id = $1::uuid",
            str(migration["migration_id"]),
        )

    reopened = await store.reopen_migration_for_retry(str(migration["migration_id"]))
    assert reopened is not None
    assert reopened["state"] == "backfilling"
    assert reopened["error"] is None

    live = await store.get_live_migration("ds-a")
    assert live is not None and live["migration_id"] == migration["migration_id"]


async def test_reopen_conflicts_with_another_live_migration(
    conflict_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    """Reopening into 'backfilling' (a live state) while another migration is
    live maps the unique-index collision to MigrationStateError — not a raw
    violation leaking through the transaction context."""
    pool, store = conflict_world
    serving = await store.get_serving_binding("ds-a")
    migration_a = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str((await _shadow_binding(store, "aaaa"))["binding_id"]),
    )
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE embedding_migrations SET state = 'rolled_back' WHERE migration_id = $1::uuid",
            str(migration_a["migration_id"]),
        )
    migration_b = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str((await _shadow_binding(store, "bbbb"))["binding_id"]),
    )
    assert migration_b["state"] == "shadow_build"

    with pytest.raises(MigrationStateError, match="already has a live"):
        await store.reopen_migration_for_retry(str(migration_a["migration_id"]))

    unchanged = await store.get_migration(str(migration_a["migration_id"]))
    assert unchanged is not None and unchanged["state"] == "rolled_back"


async def test_reopen_requires_shadow_target_and_rolled_back_state(
    conflict_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = conflict_world
    serving = await store.get_serving_binding("ds-a")
    target = await _shadow_binding(store, "aaaa")
    migration = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str(target["binding_id"]),
    )
    # Not rolled_back -> None (no-op), never an exception.
    assert await store.reopen_migration_for_retry(str(migration["migration_id"])) is None

    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE embedding_migrations SET state = 'rolled_back' WHERE migration_id = $1::uuid",
            str(migration["migration_id"]),
        )
        # A retired target binding lost its vectors' claim: not retryable.
        await conn.execute(
            "UPDATE dataset_collection_bindings SET state = 'retired', retired_at = NOW()"
            " WHERE binding_id = $1::uuid",
            str(target["binding_id"]),
        )
    assert await store.reopen_migration_for_retry(str(migration["migration_id"])) is None


# ------------------------------------------------------------ abandon states


async def test_abandon_admits_gate_failed_migration(
    conflict_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    pool, store = conflict_world
    serving = await store.get_serving_binding("ds-a")
    target = await _shadow_binding(store, "aaaa")
    migration = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str(target["binding_id"]),
    )
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE embedding_migrations SET state = 'gate_failed' WHERE migration_id = $1::uuid",
            str(migration["migration_id"]),
        )

    abandoned = await store.abandon_migration(
        str(migration["migration_id"]), reason="eval below floor"
    )
    assert abandoned["state"] == "abandoned"
    retired = await store.get_binding(str(target["binding_id"]))
    assert retired is not None and retired["state"] == "retired"
    # The dataset is still served; only the shadow generation was released.
    assert await store.get_serving_binding("ds-a") is not None
    assert await store.get_live_migration("ds-a") is None


async def test_abandon_admits_rolled_back_migration_keeps_dataset_serving(
    conflict_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    """The wedge escape: rollback(keep_shadow=True) leaves rolled_back +
    shadow binding; abort must be able to release that reservation."""
    pool, store = conflict_world
    serving = await store.get_serving_binding("ds-a")
    target = await _shadow_binding(store, "aaaa")
    migration = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str(target["binding_id"]),
    )
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE embedding_migrations SET state = 'rolled_back' WHERE migration_id = $1::uuid",
            str(migration["migration_id"]),
        )

    abandoned = await store.abandon_migration(str(migration["migration_id"]))
    assert abandoned["state"] == "abandoned"
    retired = await store.get_binding(str(target["binding_id"]))
    assert retired is not None and retired["state"] == "retired"

    # The released name allows a fresh start for the same target identity.
    fresh_target = await store.create_binding(
        dataset_id="ds-a",
        tenant_id="tenant-a",
        collection_name="kb_ds-a_1024_vaaaa",
        embedding_provider="local",
        embedding_model="model-aaaa",
        embedding_dimension=1024,
    )
    fresh = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str(fresh_target["binding_id"]),
    )
    assert fresh["state"] == "shadow_build"


async def test_abandon_still_refuses_completed_migration(
    conflict_world: tuple[asyncpg.Pool, EmbeddingVersionStore],
) -> None:
    """completed stays excluded: its target binding IS the serving one;
    retiring it would take the dataset offline (rollback is the exit)."""
    pool, store = conflict_world
    serving = await store.get_serving_binding("ds-a")
    migration = await store.begin_migration(
        dataset_id="ds-a",
        source_binding_id=str(serving["binding_id"]),
        target_binding_id=str((await _shadow_binding(store, "aaaa"))["binding_id"]),
    )
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE embedding_migrations SET state = 'completed' WHERE migration_id = $1::uuid",
            str(migration["migration_id"]),
        )
    with pytest.raises(MigrationStateError, match="cannot be abandoned"):
        await store.abandon_migration(str(migration["migration_id"]))
