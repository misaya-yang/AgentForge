"""Tier-B PostgreSQL contract for migration 108 query observability."""

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

ROOT = Path(__file__).resolve().parents[2]
MIGRATION_100 = ROOT / "database/migrations/100_kb_dataset_query_telemetry.sql"
MIGRATION_108 = ROOT / "database/migrations/108_kb_query_feedback_observability.sql"


def _postgres_config() -> dict[str, Any]:
    values = dotenv_values(os.environ.get("ENV_FILE") or ROOT / ".env")
    required = ("POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB", "POSTGRES_PORT")
    resolved = {key: os.environ.get(key) or values.get(key) for key in required}
    missing = [key for key, value in resolved.items() if not value]
    if missing:
        pytest.fail(f"local PostgreSQL test configuration missing keys: {', '.join(missing)}")
    return {
        "host": os.environ.get("POSTGRES_HOST") or values.get("POSTGRES_HOST") or "127.0.0.1",
        "port": int(str(resolved["POSTGRES_PORT"])),
        "user": str(resolved["POSTGRES_USER"]),
        "password": str(resolved["POSTGRES_PASSWORD"]),
        "database": str(resolved["POSTGRES_DB"]),
    }


@pytest_asyncio.fixture
async def observability_pool() -> AsyncIterator[asyncpg.Pool]:
    config = _postgres_config()
    database = f"kb_query_feedback_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{database}"')
    pool = await asyncpg.create_pool(
        **{**config, "database": database},
        min_size=1,
        max_size=2,
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
                    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id)
                );
                CREATE TABLE segments (
                    segment_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id),
                    document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id),
                    hit_count INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE dataset_queries (
                    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
                    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id),
                    content TEXT NOT NULL,
                    source VARCHAR(50) NOT NULL DEFAULT 'api',
                    source_app_id VARCHAR(255),
                    created_by_role VARCHAR(50),
                    created_by VARCHAR(255),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                INSERT INTO datasets VALUES ('dataset-a', 'tenant-a', FALSE);
                INSERT INTO documents VALUES ('document-a', 'dataset-a');
                INSERT INTO segments VALUES ('segment-a', 'dataset-a', 'document-a', 0);
                INSERT INTO dataset_queries (dataset_id, content)
                VALUES ('dataset-a', 'legacy');
                """
            )
            await conn.execute(MIGRATION_100.read_text(encoding="utf-8"))
            await conn.execute(
                """
                INSERT INTO dataset_queries (dataset_id, content, metadata)
                VALUES (
                    'dataset-a', 'oversized legacy',
                    '{"top_k":"999999999999999999999999999999999999",'
                    '"hit_count":"999999999999999999999999999999999999"}'::jsonb
                )
                """
            )
            sql = MIGRATION_108.read_text(encoding="utf-8")
            await conn.execute(sql)
            await conn.execute(sql)
        yield pool
    finally:
        await pool.close()
        await admin.execute(f'DROP DATABASE "{database}"')
        await admin.close()


@pytest.mark.asyncio
async def test_108_is_idempotent_owned_and_does_not_overflow_legacy_metadata(
    observability_pool: asyncpg.Pool,
) -> None:
    async with observability_pool.acquire() as conn:
        row = await conn.fetchrow(
            """
            SELECT top_k, hit_count FROM dataset_queries
            WHERE content = 'oversized legacy'
            """
        )
        assert dict(row) == {"top_k": None, "hit_count": None}
        relation = await conn.fetchrow(
            """
            SELECT n.nspname, pg_get_userbyid(c.relowner) AS owner
            FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
            WHERE c.oid = 'knowledge.dataset_query_feedback'::regclass
            """
        )
        assert relation["nspname"] == "knowledge"
        assert relation["owner"] == await conn.fetchval("SELECT current_user")


@pytest.mark.asyncio
async def test_108_round_trips_observation_and_tenant_feedback(
    observability_pool: asyncpg.Pool,
) -> None:
    trace_id = uuid.uuid4()
    fingerprint = "a" * 64
    async with observability_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO dataset_queries (
                dataset_id, content, metadata, trace_id, query_fingerprint,
                mode, top_k, hit_count, stage_timings
            ) VALUES ($1, $2, $3::jsonb, $4, $5, 'hybrid', 5, 1, $6::jsonb)
            """,
            "dataset-a",
            "query",
            json.dumps({"cache_hit": False}),
            trace_id,
            fingerprint,
            json.dumps({"total_ms": 10}),
        )
        await conn.execute(
            """
            INSERT INTO knowledge.dataset_query_feedback (
                tenant_id, dataset_id, trace_id, query_fingerprint,
                target_type, target_id, rating, reason_code, created_by
            ) VALUES (
                'tenant-a', 'dataset-a', $1, $2,
                'retrieval_hit', 'segment-a', 'negative', 'irrelevant', 'user-a'
            )
            """,
            trace_id,
            fingerprint,
        )
        stored = await conn.fetchrow(
            "SELECT * FROM knowledge.dataset_query_feedback WHERE trace_id = $1",
            trace_id,
        )
        assert stored["tenant_id"] == "tenant-a"
        assert stored["reason_code"] == "irrelevant"

        with pytest.raises(asyncpg.ForeignKeyViolationError):
            foreign_trace = uuid.uuid4()
            await conn.execute(
                """
                INSERT INTO knowledge.dataset_query_feedback (
                    tenant_id, dataset_id, trace_id, query_fingerprint,
                    target_type, target_id, rating, reason_code, created_by
                ) VALUES (
                    'tenant-b', 'dataset-a', $2::uuid, $1,
                    'qa_answer', $2::uuid::text,
                    'positive', 'helpful', 'attacker'
                )
                """,
                fingerprint,
                foreign_trace,
            )
