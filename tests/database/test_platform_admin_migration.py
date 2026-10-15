"""Platform-admin migration contracts."""

from __future__ import annotations

import os
import uuid
from collections.abc import AsyncIterator
from pathlib import Path

import asyncpg
import pytest
import pytest_asyncio

from tests.database.test_agent_studio_migrations import _postgres_config

ROOT = Path(__file__).resolve().parents[2]
MIGRATION = ROOT / "database" / "migrations" / "086_platform_admin_role.sql"
SCHEMA = ROOT / "database" / "schema.sql"


def test_platform_admin_schema_contract_is_strict_and_forward_only() -> None:
    migration_sql = MIGRATION.read_text(encoding="utf-8")
    schema_sql = SCHEMA.read_text(encoding="utf-8")
    upper_sql = migration_sql.upper()

    assert "'platform_admin'" in migration_sql
    assert "ARRAY['admin:*']" in migration_sql
    assert "WHERE user_id = 'admin'\n  AND created_by = 'system'" in migration_sql
    assert migration_sql.count("WHERE user_id = 'admin'") == 2
    assert "ON CONFLICT (user_id, role_name) DO NOTHING" in migration_sql
    assert "'platform_admin'" in schema_sql
    assert "WHERE user_id = 'admin'\n  AND created_by = 'system'" in schema_sql
    assert "DROP TABLE" not in upper_sql
    assert "TRUNCATE" not in upper_sql
    assert "DELETE FROM USERS" not in upper_sql


@pytest_asyncio.fixture
async def platform_admin_pool() -> AsyncIterator[asyncpg.Pool]:
    if os.getenv("PLATFORM_ADMIN_TEST_ENABLED") != "1":
        pytest.skip(
            "set PLATFORM_ADMIN_TEST_ENABLED=1 for the PostgreSQL migration gate"
        )
    config = _postgres_config()
    schema_name = f"platform_admin_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE SCHEMA "{schema_name}"')
    await admin.close()

    pool = await asyncpg.create_pool(
        **config,
        min_size=1,
        max_size=2,
        server_settings={"search_path": f'"{schema_name}"'},
    )
    try:
        async with pool.acquire() as conn:
            await conn.execute(
                """
                CREATE TABLE users (
                    user_id VARCHAR(255) PRIMARY KEY,
                    roles VARCHAR(50)[] NOT NULL DEFAULT ARRAY['user']::VARCHAR(50)[],
                    created_by VARCHAR(255),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE rbac_roles (
                    id SERIAL PRIMARY KEY,
                    role_name VARCHAR(100) NOT NULL UNIQUE,
                    description TEXT,
                    permissions VARCHAR(100)[] NOT NULL DEFAULT ARRAY[]::VARCHAR(100)[],
                    is_system BOOLEAN NOT NULL DEFAULT FALSE,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE user_roles (
                    id SERIAL PRIMARY KEY,
                    user_id VARCHAR(255) NOT NULL,
                    role_name VARCHAR(100) NOT NULL,
                    granted_by VARCHAR(255),
                    granted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    expires_at TIMESTAMPTZ,
                    UNIQUE (user_id, role_name)
                );

                INSERT INTO users (user_id, roles, created_by) VALUES
                    ('admin', ARRAY['admin'], 'system'),
                    ('tenant-admin', ARRAY['admin'], 'system'),
                    ('ordinary-admin', ARRAY['admin'], 'tenant-owner');
                """
            )
        yield pool
    finally:
        await pool.close()
        admin = await asyncpg.connect(**config)
        await admin.execute(f'DROP SCHEMA "{schema_name}" CASCADE')
        await admin.close()


@pytest.mark.asyncio
async def test_migration_promotes_only_bootstrap_operator_idempotently(
    platform_admin_pool: asyncpg.Pool,
) -> None:
    sql = MIGRATION.read_text(encoding="utf-8")
    async with platform_admin_pool.acquire() as conn:
        await conn.execute(sql)
        await conn.execute(sql)

        roles = {
            row["user_id"]: list(row["roles"])
            for row in await conn.fetch("SELECT user_id, roles FROM users ORDER BY user_id")
        }
        mappings = await conn.fetch(
            "SELECT user_id, role_name, granted_by FROM user_roles ORDER BY user_id, role_name"
        )
        platform_role = await conn.fetchrow(
            "SELECT permissions, is_system FROM rbac_roles WHERE role_name = 'platform_admin'"
        )

    assert roles["admin"] == ["admin", "platform_admin"]
    assert roles["tenant-admin"] == ["admin"]
    assert roles["ordinary-admin"] == ["admin"]
    assert [dict(row) for row in mappings] == [
        {"user_id": "admin", "role_name": "platform_admin", "granted_by": "system"}
    ]
    assert list(platform_role["permissions"]) == ["admin:*"]
    assert platform_role["is_system"] is True
