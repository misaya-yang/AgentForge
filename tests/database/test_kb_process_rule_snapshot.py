"""Real-PostgreSQL behavior tests for the T1 item 7 process-rule snapshot.

Contract under test (PRD T1 item 7):

* record_process_rule content-dedupes by (dataset_id, mode, rules) jsonb
  equality, so an unchanged dataset config keeps a stable rule id across
  generations; distinct content gets a distinct immutable row;
* migration 103 pins immutability: any UPDATE of a dataset_process_rules row
  raises (replay verbs and execution rows resolve these by id);
* pin_document_process_rule round-trips through documents.process_rule_id,
  the replay degrade fallback the worker reads when an execution row carries
  no usable snapshot.

Tier-b pattern: throwaway schema + minimal tables carrying exactly the
columns the production queries read, then migration 103 applied VERBATIM,
exercised through the production DatasetStorage methods over a live
developer PostgreSQL.
"""

from __future__ import annotations

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
from knowledge_service.persistence.database import DatabaseStorage

ROOT = Path(__file__).resolve().parents[2]
MIGRATION_103 = ROOT / "database" / "migrations" / "103_kb_process_rule_snapshot.sql"


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
async def rule_world() -> AsyncIterator[tuple[DatabaseStorage, asyncpg.Pool]]:
    config = _postgres_config()
    database_name = f"kb_process_rule_test_{uuid.uuid4().hex}"
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
            await conn.execute(
                """
                CREATE TABLE datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    is_deleted BOOLEAN NOT NULL DEFAULT FALSE
                );
                CREATE TABLE dataset_process_rules (
                    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    mode VARCHAR(50) NOT NULL DEFAULT 'automatic',
                    rules JSONB NOT NULL DEFAULT '{}',
                    created_by VARCHAR(255),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE documents (
                    document_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    process_rule_id VARCHAR(255),
                    status VARCHAR(50) NOT NULL DEFAULT 'completed'
                );
                CREATE TABLE document_pipeline_executions (
                    execution_id VARCHAR(255) PRIMARY KEY,
                    process_rule_id VARCHAR(255)
                );
                INSERT INTO datasets (dataset_id, tenant_id)
                VALUES ('dataset-a', 'tenant-a'), ('dataset-b', 'tenant-b');
                """
            )
            # The migration itself is under test: apply it verbatim so the
            # guard trigger + contract comments are exactly what deploys.
            migration_sql = MIGRATION_103.read_text()
            await conn.execute(migration_sql)
            await conn.execute(migration_sql)
        storage = DatabaseStorage()
        storage._pool = pool  # type: ignore[assignment]
        yield storage, pool
    finally:
        await pool.close()
        try:
            await admin.execute(f'DROP DATABASE "{database_name}"')
        finally:
            await admin.close()


RULES_AUTOMATIC = {
    "chunking": {"mode": "automatic"},
    "processing_mode": "text_only",
}
RULES_CUSTOM = {
    "chunking": {"mode": "custom", "chunk_size": 123},
    "processing_mode": "text_only",
}


@pytest.mark.asyncio
async def test_record_process_rule_dedups_identical_content(
    rule_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    storage, pool = rule_world

    first = await storage.record_process_rule("dataset-a", mode="automatic", rules=RULES_AUTOMATIC)
    second = await storage.record_process_rule(
        "dataset-a", mode="automatic", rules=dict(RULES_AUTOMATIC)
    )

    assert first is not None
    assert second == first
    async with pool.acquire() as conn:
        count = await conn.fetchval(
            "SELECT count(*) FROM dataset_process_rules WHERE dataset_id = 'dataset-a'"
        )
    assert count == 1

    # Distinct content is a distinct immutable row.
    third = await storage.record_process_rule("dataset-a", mode="custom", rules=RULES_CUSTOM)
    assert third is not None and third != first
    async with pool.acquire() as conn:
        count = await conn.fetchval(
            "SELECT count(*) FROM dataset_process_rules WHERE dataset_id = 'dataset-a'"
        )
    assert count == 2


@pytest.mark.asyncio
async def test_record_process_rule_scopes_by_dataset_and_mode(
    rule_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    storage, _pool = rule_world

    rule_a = await storage.record_process_rule("dataset-a", mode="automatic", rules=RULES_AUTOMATIC)
    rule_b = await storage.record_process_rule("dataset-b", mode="automatic", rules=RULES_AUTOMATIC)
    # Same content under another dataset never collapses into the same row:
    # the snapshot must stay attributable to the generation's own dataset.
    assert rule_a is not None and rule_b is not None and rule_a != rule_b

    same_dataset_other_mode = await storage.record_process_rule(
        "dataset-a", mode="hierarchical", rules=RULES_AUTOMATIC
    )
    assert same_dataset_other_mode not in {rule_a, rule_b}


@pytest.mark.asyncio
async def test_migration_103_trigger_blocks_rule_updates(
    rule_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    storage, pool = rule_world

    rule_id = await storage.record_process_rule(
        "dataset-a", mode="automatic", rules=RULES_AUTOMATIC
    )
    assert rule_id is not None

    with pytest.raises(asyncpg.PostgresError, match="immutable generation snapshots"):
        async with pool.acquire() as conn:
            await conn.execute(
                "UPDATE dataset_process_rules SET mode = 'custom' WHERE id = $1",
                rule_id,
            )

    # The failed update changed nothing.
    row = await storage.get_process_rule(rule_id)
    assert row is not None
    assert row["mode"] == "automatic"
    rules = row["rules"]
    if isinstance(rules, str):
        rules = json.loads(rules)
    assert rules == RULES_AUTOMATIC


@pytest.mark.asyncio
async def test_pin_document_process_rule_round_trip(
    rule_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    storage, pool = rule_world

    rule_id = await storage.record_process_rule(
        "dataset-a", mode="custom", rules=RULES_CUSTOM, created_by="user-a"
    )
    assert rule_id is not None

    async with pool.acquire() as conn:
        await conn.execute(
            "INSERT INTO documents (document_id, dataset_id) VALUES ('doc-a', 'dataset-a')"
        )

    pinned = await storage.pin_document_process_rule("doc-a", rule_id)
    assert pinned is True

    async with pool.acquire() as conn:
        stored = await conn.fetchval(
            "SELECT process_rule_id FROM documents WHERE document_id = 'doc-a'"
        )
    assert stored == rule_id

    # The replay degrade fallback reads this row back verbatim.
    row = await storage.get_process_rule(rule_id)
    assert row is not None
    assert row["dataset_id"] == "dataset-a"
    assert row["mode"] == "custom"
    rules = row["rules"]
    if isinstance(rules, str):
        rules = json.loads(rules)
    assert rules == RULES_CUSTOM
    assert row["created_by"] == "user-a"

    # Unknown ids and empty inputs degrade to None/False, never raise.
    assert await storage.get_process_rule("rule-gone") is None
    assert await storage.get_process_rule("") is None
    assert await storage.pin_document_process_rule("", rule_id) is False
    assert await storage.pin_document_process_rule("doc-a", "") is False


@pytest.mark.asyncio
async def test_record_process_rule_requires_pool_and_valid_input(
    rule_world: tuple[DatabaseStorage, asyncpg.Pool],
) -> None:
    storage, _pool = rule_world

    assert await storage.record_process_rule("", mode="automatic", rules=RULES_AUTOMATIC) is None
    assert await storage.record_process_rule("dataset-a", mode="", rules=RULES_AUTOMATIC) is None
    with pytest.raises(ValueError, match="must be a dict"):
        await storage.record_process_rule(
            "dataset-a",
            mode="automatic",
            rules=json.dumps(RULES_AUTOMATIC),  # type: ignore[arg-type]
        )
