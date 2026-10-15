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

ROOT = Path(__file__).resolve().parents[2]
ROOT_MIGRATION = ROOT / "database/migrations/097_image_task_runtime_scope.sql"
ASSISTANT_002 = ROOT / "database/migrations/per_service/assistant/002_image_p0_tasks_blobs.sql"
ASSISTANT_004 = ROOT / "database/migrations/per_service/assistant/004_image_task_runtime_scope.sql"


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
async def image_migration_connection() -> AsyncIterator[asyncpg.Connection]:
    config = _postgres_config()
    database_name = f"image_scope_migration_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{database_name}"')
    conn = await asyncpg.connect(**{**config, "database": database_name})
    try:
        yield conn
    finally:
        await conn.close()
        try:
            await admin.execute(f'DROP DATABASE "{database_name}"')
        finally:
            await admin.close()


def test_image_task_scope_migration_is_additive_and_fail_closed() -> None:
    sql = ROOT_MIGRATION.read_text(encoding="utf-8")

    assert "ADD COLUMN IF NOT EXISTS tenant_id" in sql
    assert "ADD COLUMN IF NOT EXISTS user_id" in sql
    assert "runtime_scope_version SMALLINT NOT NULL DEFAULT 0" in sql
    assert "runtime_scope_version = 1" in sql
    assert "tenant_id IS NOT NULL" in sql
    assert "user_id IS NOT NULL" in sql
    assert "DROP TABLE" not in sql.upper()
    assert "TRUNCATE" not in sql.upper()
    assert "to_regclass('assistant.image_tasks') IS NULL" in sql
    assert "RETURN;" in sql

    per_service_sql = ASSISTANT_004.read_text(encoding="utf-8")
    for contract in (
        "assistant_image_tasks_runtime_scope_check",
        "idx_image_tasks_runtime_queue",
        "idx_image_tasks_runtime_owner",
    ):
        assert contract in sql
        assert contract in per_service_sql


@pytest.mark.asyncio
async def test_097_skips_absent_table_then_004_hardens_later_table(
    image_migration_connection: asyncpg.Connection,
) -> None:
    conn = image_migration_connection
    await conn.execute("CREATE SCHEMA assistant")

    root_sql = ROOT_MIGRATION.read_text(encoding="utf-8")
    await conn.execute(root_sql)
    await conn.execute(root_sql)
    assert await conn.fetchval("SELECT to_regclass('assistant.image_tasks')") is None

    # Minimal prerequisites for the real assistant/002 table creator.
    await conn.execute(
        """
        CREATE TABLE assistant.artifacts (
            artifact_id VARCHAR(64) PRIMARY KEY
        );
        CREATE TABLE assistant.image_turns (
            turn_id VARCHAR(64) PRIMARY KEY,
            status VARCHAR(32),
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        );
        """
    )
    await conn.execute(ASSISTANT_002.read_text(encoding="utf-8"))
    per_service_sql = ASSISTANT_004.read_text(encoding="utf-8")
    await conn.execute(per_service_sql)
    await conn.execute(per_service_sql)

    constraints = await conn.fetch(
        """
        SELECT conname, convalidated
        FROM pg_constraint
        WHERE conrelid = 'assistant.image_tasks'::regclass
          AND conname = 'assistant_image_tasks_runtime_scope_check'
        """
    )
    assert [(row["conname"], row["convalidated"]) for row in constraints] == [
        ("assistant_image_tasks_runtime_scope_check", True)
    ]
    indexes = {
        row["indexname"]
        for row in await conn.fetch(
            """
            SELECT indexname FROM pg_indexes
            WHERE schemaname = 'assistant'
              AND indexname = ANY($1::text[])
            """,
            ["idx_image_tasks_runtime_queue", "idx_image_tasks_runtime_owner"],
        )
    }
    assert indexes == {
        "idx_image_tasks_runtime_queue",
        "idx_image_tasks_runtime_owner",
    }

    with pytest.raises(asyncpg.CheckViolationError):
        await conn.execute(
            """
            INSERT INTO assistant.image_tasks (
                task_id, owner_scope, prompt, model_id, runtime_scope_version
            ) VALUES ('invalid', 'opaque', 'prompt', 'model', 1)
            """
        )
    await conn.execute(
        """
        INSERT INTO assistant.image_tasks (
            task_id, tenant_id, user_id, owner_scope, prompt, model_id,
            runtime_scope_version
        ) VALUES ('valid', 'tenant-a', 'user-a', 'opaque', 'prompt', 'model', 1)
        """
    )
