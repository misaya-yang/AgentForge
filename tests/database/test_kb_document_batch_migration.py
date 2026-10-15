from __future__ import annotations

import os
import uuid
from pathlib import Path
from urllib.parse import quote

import asyncpg
import pytest
from dotenv import dotenv_values
from knowledge_service.persistence.document_batches import DocumentBatchStore

ROOT = Path(__file__).resolve().parents[2]
MIGRATION = ROOT / "database" / "migrations" / "109_kb_document_batch_operations.sql"


def _postgres_config() -> dict[str, object]:
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


def _dsn(config: dict[str, object], database: str) -> str:
    user = quote(str(config["user"]), safe="")
    password = quote(str(config["password"]), safe="")
    return f"postgresql://{user}:{password}@{config['host']}:{config['port']}/{database}"


@pytest.mark.asyncio
async def test_109_uses_knowledge_namespace_and_fair_skip_locked_claims() -> None:
    config = _postgres_config()
    database_name = f"kb_batch_109_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{database_name}"')
    pool: asyncpg.Pool | None = None
    try:
        dsn = _dsn(config, database_name)
        conn = await asyncpg.connect(dsn)
        try:
            await conn.execute(
                """
                CREATE SCHEMA knowledge;
                CREATE TABLE knowledge.datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
                    UNIQUE (dataset_id, tenant_id)
                );
                CREATE TABLE knowledge.documents (
                    document_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                """
            )
            sql = MIGRATION.read_text(encoding="utf-8")
            await conn.execute(sql)
            await conn.execute(sql)
            namespaces = {
                row["relname"]: row["namespace"]
                for row in await conn.fetch(
                    """
                    SELECT c.relname, n.nspname AS namespace
                    FROM pg_class AS c
                    JOIN pg_namespace AS n ON n.oid = c.relnamespace
                    WHERE c.relname = ANY($1::text[])
                    """,
                    ["kb_document_batch_operations", "kb_document_batch_items"],
                )
            }
            assert namespaces == {
                "kb_document_batch_operations": "knowledge",
                "kb_document_batch_items": "knowledge",
            }
            await conn.execute(
                """
                INSERT INTO knowledge.datasets (dataset_id, tenant_id)
                VALUES ('dataset-a', 'tenant-a');
                INSERT INTO knowledge.documents (document_id, dataset_id)
                VALUES ('doc-a1', 'dataset-a'), ('doc-a2', 'dataset-a'),
                       ('doc-a3', 'dataset-a'), ('doc-b1', 'dataset-a');
                """
            )
            constraint = await conn.fetchrow(
                """
                SELECT convalidated
                FROM pg_constraint
                WHERE conrelid = 'knowledge.kb_document_batch_operations'::regclass
                  AND conname = 'fk_kb_document_batch_dataset_tenant'
                """
            )
            assert constraint is not None
            assert constraint["convalidated"] is True
            with pytest.raises(asyncpg.ForeignKeyViolationError):
                await conn.execute(
                    """
                    INSERT INTO knowledge.kb_document_batch_operations (
                        operation_id, tenant_id, dataset_id, operation, created_by
                    ) VALUES (
                        '00000000-0000-4000-8000-000000000109',
                        'tenant-b', 'dataset-a', 'reembed', 'cross-tenant-user'
                    )
                    """
                )
        finally:
            await conn.close()

        pool = await asyncpg.create_pool(dsn, min_size=1, max_size=4)
        store = DocumentBatchStore(pool)
        first = await store.create_operation(
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            operation="reembed",
            created_by="user-a",
            actor_roles=["editor"],
            document_ids=["doc-a1", "doc-a2", "doc-a3"],
        )
        second = await store.create_operation(
            tenant_id="tenant-a",
            dataset_id="dataset-a",
            operation="reembed",
            created_by="user-a",
            actor_roles=["editor"],
            document_ids=["doc-b1"],
        )

        claim_one = await store.claim_next_item(worker_id="worker-1")
        claim_two = await store.claim_next_item(worker_id="worker-2")
        claim_three = await store.claim_next_item(worker_id="worker-1")

        assert claim_one is not None
        assert claim_two is not None
        assert claim_three is not None
        assert {claim_one.operation_id, claim_two.operation_id} == {
            first["operation_id"],
            second["operation_id"],
        }
        assert claim_three.operation_id == first["operation_id"]
        assert len({claim_one.document_id, claim_two.document_id, claim_three.document_id}) == 3
    finally:
        if pool is not None:
            await pool.close()
        await admin.execute(f'DROP DATABASE IF EXISTS "{database_name}"')
        await admin.close()
