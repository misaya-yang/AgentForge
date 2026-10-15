"""Real-PostgreSQL test for migration 100 (dataset_queries telemetry column).

Same tier-b pattern as tests/database/test_kb_migrations.py: throwaway
schema, minimal inline prerequisites matching the PRE-migration table shape,
migration applied twice (idempotency), then behavioral assertions against a
live developer PostgreSQL.
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

ROOT = Path(__file__).resolve().parents[2]
MIGRATION_100 = ROOT / "database" / "migrations" / "100_kb_dataset_query_telemetry.sql"


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
async def telemetry_pool() -> AsyncIterator[asyncpg.Pool]:
    config = _postgres_config()
    database_name = f"kb_query_telemetry_test_{uuid.uuid4().hex}"
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
            # Pre-migration shape of the two tables migration 100 touches
            # (matches the original KB schema: no metadata column yet).
            await conn.execute(
                """
                CREATE TABLE datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL DEFAULT ''
                );
                CREATE TABLE dataset_queries (
                    id VARCHAR(255) PRIMARY KEY DEFAULT gen_random_uuid()::text,
                    dataset_id VARCHAR(255) NOT NULL
                        REFERENCES datasets(dataset_id) ON DELETE CASCADE,
                    content TEXT NOT NULL,
                    source VARCHAR(50) NOT NULL DEFAULT 'api',
                    source_app_id VARCHAR(255),
                    created_by_role VARCHAR(50),
                    created_by VARCHAR(255),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                INSERT INTO datasets (dataset_id, tenant_id)
                VALUES ('dataset-a', 'tenant-a');
                -- Legacy row: must survive the migration with '{}'.
                INSERT INTO dataset_queries (dataset_id, content)
                VALUES ('dataset-a', 'legacy query');
                """
            )
            sql = MIGRATION_100.read_text(encoding="utf-8")
            await conn.execute(sql)
            # Second application: idempotency assertion.
            await conn.execute(sql)
        yield pool
    finally:
        await pool.close()
        try:
            await admin.execute(f'DROP DATABASE "{database_name}"')
        finally:
            await admin.close()


@pytest.mark.asyncio
async def test_migration_adds_metadata_column_with_safe_default(
    telemetry_pool: asyncpg.Pool,
) -> None:
    async with telemetry_pool.acquire() as conn:
        column = await conn.fetchrow(
            """
            SELECT column_name, data_type, column_default, is_nullable
            FROM information_schema.columns
            WHERE table_schema = current_schema()
              AND table_name = 'dataset_queries'
              AND column_name = 'metadata'
            """
        )
        assert column is not None
        assert column["data_type"] == "jsonb"
        assert column["is_nullable"] == "NO"
        # information_schema reports the default expression itself.
        assert (column["column_default"] or "").replace(" ", "").lower() == "'{}'::jsonb"

        legacy = await conn.fetchval(
            "SELECT metadata FROM dataset_queries WHERE content = 'legacy query'"
        )
        assert json.loads(legacy) == {}


@pytest.mark.asyncio
async def test_telemetry_insert_round_trips_structured_fields(
    telemetry_pool: asyncpg.Pool,
) -> None:
    metadata = {
        "query_fingerprint": "fp-1",
        "mode": "hybrid",
        "top_k": 5,
        "hit_count": 0,
        "stage_timings": {"total_ms": 12.5},
    }
    async with telemetry_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO dataset_queries (
                dataset_id, content, source, created_by_role, created_by, metadata
            )
            VALUES ($1, $2, $3, $4, $5, $6::jsonb)
            """,
            "dataset-a",
            "零结果查询",
            "api",
            "normal",
            "user-a",
            json.dumps(metadata, ensure_ascii=False),
        )
        row = await conn.fetchrow(
            "SELECT metadata, content FROM dataset_queries WHERE created_by = 'user-a'"
        )
        assert json.loads(row["metadata"]) == metadata
        assert row["content"] == "零结果查询"
