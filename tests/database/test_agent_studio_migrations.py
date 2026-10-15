from __future__ import annotations

import asyncio
import hashlib
import json
import uuid
from collections.abc import AsyncIterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import asyncpg
import pytest
import pytest_asyncio
from ai_gateway_core.agents.deletion import (
    RUNTIME_CLEANUP_INVENTORY_SCHEMA,
    RUNTIME_CLEANUP_RECEIPT_SCHEMA,
    canonical_cleanup_digest,
)
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentDraftConflictError,
    AgentLastOwnerError,
    AgentNotFoundError,
    AgentRepositoryError,
    AgentRuntimeUnavailableError,
    AgentValidationError,
    DatabaseAgentRepository,
)
from dotenv import dotenv_values

ROOT = Path(__file__).resolve().parents[2]
MIGRATION = ROOT / "database" / "migrations" / "071_agent_studio_domain.sql"
OPERATIONS_MIGRATION = (
    ROOT / "database" / "migrations" / "081_agent_studio_operations_governance.sql"
)
AGENT_TABLES = (
    "agents",
    "agent_members",
    "agent_drafts",
    "agent_draft_knowledge_bindings",
    "agent_versions",
    "agent_version_capabilities",
    "agent_version_knowledge_bindings",
    "agent_publications",
    "agent_publish_events",
    "agent_api_tokens",
)


async def _freeze_completed_runtime_cleanup(
    repository: DatabaseAgentRepository,
    *,
    prepared: dict[str, Any],
    tenant_id: str,
    agent_id: str,
    user_id: str,
) -> dict[str, Any]:
    plan = prepared["deleted_counts"]["runtime_cleanup_plan"]
    principals = [
        {
            "principal_id": principal_id,
            "source_count": 0,
            "sources": [],
            "vector_count": 0,
            "vector_sets": [],
        }
        for principal_id in plan["principal_handles"]
    ]
    inventory: dict[str, Any] = {
        "schema_version": RUNTIME_CLEANUP_INVENTORY_SCHEMA,
        "deletion_id": prepared["deletion_id"],
        "tenant_id": tenant_id,
        "agent_id": agent_id,
        "plan_digest": plan["plan_digest"],
        "cutoff_at": plan["cutoff_at"],
        "principal_count": len(principals),
        "source_count": 0,
        "vector_count": 0,
        "principals": principals,
    }
    inventory["inventory_digest"] = canonical_cleanup_digest(inventory)
    await repository.freeze_agent_runtime_cleanup_inventory(
        tenant_id=tenant_id,
        agent_id=agent_id,
        deletion_id=prepared["deletion_id"],
        user_id=user_id,
        is_tenant_admin=False,
        inventory=inventory,
    )
    principal_receipts = [
        {
            "principal_id": principal["principal_id"],
            "status": "completed",
            "completed": True,
            "retryable": False,
            "source_count": 0,
            "deleted_source_count": 0,
            "vector_count": 0,
            "deleted_vector_count": 0,
            "idempotent_absent_count": 0,
            "idempotent_absent_vector_count": 0,
            "errors": [],
        }
        for principal in principals
    ]
    receipt: dict[str, Any] = {
        "schema_version": RUNTIME_CLEANUP_RECEIPT_SCHEMA,
        "deletion_id": prepared["deletion_id"],
        "tenant_id": tenant_id,
        "agent_id": agent_id,
        "plan_digest": plan["plan_digest"],
        "inventory_digest": inventory["inventory_digest"],
        "status": "completed",
        "completed": True,
        "retryable": False,
        "principals": principal_receipts,
        "errors": [],
    }
    receipt["receipt_digest"] = canonical_cleanup_digest(receipt)
    return receipt


async def _finish_claimed_data_deletion(
    repository: DatabaseAgentRepository,
    *,
    prepared: dict[str, Any],
    tenant_id: str,
    agent_id: str,
    user_id: str,
    storage_cleanup_succeeded: bool,
    runtime_cleanup_receipt: dict[str, Any] | None = None,
) -> dict[str, Any]:
    async with repository.claim_agent_data_deletion_execution(
        tenant_id=tenant_id,
        agent_id=agent_id,
        deletion_id=str(prepared["deletion_id"]),
        user_id=user_id,
        is_tenant_admin=False,
    ) as claimed:
        if not claimed.get("execution_claimed"):
            return claimed
        return await claimed["_execution_finish"](
            storage_cleanup_succeeded=storage_cleanup_succeeded,
            runtime_cleanup_receipt=runtime_cleanup_receipt,
        )


def _postgres_config() -> dict[str, Any]:
    values = dotenv_values(ROOT / ".env")
    required = ("POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB", "POSTGRES_PORT")
    missing = [key for key in required if not values.get(key)]
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
async def agent_pool() -> AsyncIterator[asyncpg.Pool]:
    config = _postgres_config()
    schema_name = f"agent_studio_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE SCHEMA "{schema_name}"')
    await admin.close()

    pool = await asyncpg.create_pool(
        **config,
        min_size=1,
        max_size=4,
        server_settings={"search_path": f'"{schema_name}",public'},
    )
    try:
        async with pool.acquire() as conn:
            await conn.execute(
                """
                CREATE TABLE datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL DEFAULT '',
                    name VARCHAR(255) NOT NULL DEFAULT '',
                    created_by VARCHAR(255) NOT NULL DEFAULT '',
                    visibility VARCHAR(32) NOT NULL DEFAULT 'private',
                    is_deleted BOOLEAN NOT NULL DEFAULT FALSE
                );
                CREATE TABLE users (
                    user_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    roles VARCHAR(255)[] NOT NULL DEFAULT '{}',
                    status VARCHAR(50) NOT NULL DEFAULT 'active'
                );
                CREATE TABLE dataset_permissions (
                    dataset_id VARCHAR(255) NOT NULL,
                    subject_type VARCHAR(32) NOT NULL,
                    subject_id VARCHAR(255) NOT NULL,
                    permission VARCHAR(32) NOT NULL
                );
                CREATE TABLE audit_logs (
                    id BIGSERIAL PRIMARY KEY,
                    event_type VARCHAR(100) NOT NULL,
                    user_id VARCHAR(255),
                    tenant_id VARCHAR(255),
                    resource_type VARCHAR(100),
                    resource_id VARCHAR(255),
                    action VARCHAR(50) NOT NULL,
                    request_summary JSONB,
                    status VARCHAR(50) NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                """
            )
            sql = MIGRATION.read_text(encoding="utf-8")
            await conn.execute(sql)
            await conn.execute(sql)
        yield pool
    finally:
        await pool.close()
        admin = await asyncpg.connect(**config)
        await admin.execute(f'DROP SCHEMA "{schema_name}" CASCADE')
        await admin.close()


@dataclass
class _Holder:
    _pool: Any
    enabled: bool = True


class _SchemaIsolatedConnection:
    """Keep schema-qualified production SQL inside this test's random schema."""

    def __init__(self, connection: asyncpg.Connection) -> None:
        self._connection = connection

    @staticmethod
    def _query(query: str) -> str:
        return query.replace("assistant.sessions", "sessions")

    async def fetch(self, query: str, *args: Any) -> Any:
        return await self._connection.fetch(self._query(query), *args)

    async def fetchrow(self, query: str, *args: Any) -> Any:
        return await self._connection.fetchrow(self._query(query), *args)

    async def fetchval(self, query: str, *args: Any) -> Any:
        return await self._connection.fetchval(self._query(query), *args)

    async def execute(self, query: str, *args: Any) -> Any:
        return await self._connection.execute(self._query(query), *args)

    def transaction(self, *args: Any, **kwargs: Any) -> Any:
        return self._connection.transaction(*args, **kwargs)

    def __getattr__(self, name: str) -> Any:
        return getattr(self._connection, name)


class _SchemaIsolatedAcquire:
    def __init__(self, acquire: Any) -> None:
        self._acquire = acquire

    async def __aenter__(self) -> _SchemaIsolatedConnection:
        connection = await self._acquire.__aenter__()
        return _SchemaIsolatedConnection(connection)

    async def __aexit__(self, *args: Any) -> Any:
        return await self._acquire.__aexit__(*args)


class _SchemaIsolatedPool:
    def __init__(self, pool: asyncpg.Pool) -> None:
        self._pool = pool

    def acquire(self) -> _SchemaIsolatedAcquire:
        return _SchemaIsolatedAcquire(self._pool.acquire())


async def _insert_graph(conn: asyncpg.Connection, tenant_id: str, suffix: str) -> dict[str, Any]:
    agent_id = uuid.uuid4()
    draft_id = uuid.uuid4()
    version_id = uuid.uuid4()
    publication_id = uuid.uuid4()
    dataset_id = f"dataset-{suffix}"
    spec_hash = hashlib.sha256(f"spec-{suffix}".encode()).hexdigest()
    owner_id = f"owner-{suffix}"
    transaction = conn.transaction()
    await transaction.start()

    await conn.execute(
        "INSERT INTO datasets (dataset_id, tenant_id, name) VALUES ($1, $2, $3)",
        dataset_id,
        tenant_id,
        dataset_id,
    )
    await conn.execute(
        "INSERT INTO users (user_id, tenant_id) VALUES ($1, $2)", owner_id, tenant_id
    )
    await conn.execute(
        """
        INSERT INTO agents (
            tenant_id, agent_id, slug, name, owner_id, created_by, updated_by
        ) VALUES ($1, $2, $3, $3, $4, $4, $4)
        """,
        tenant_id,
        agent_id,
        f"agent-{suffix}",
        owner_id,
    )
    await conn.execute(
        """
        INSERT INTO agent_drafts (
            tenant_id, draft_id, agent_id, revision, schema_version,
            spec, spec_hash, updated_by
        ) VALUES ($1, $2, $3, 1, 'agent-spec/v1', $4::jsonb, $5, $6)
        """,
        tenant_id,
        draft_id,
        agent_id,
        '{"schema_version":"agent-spec/v1","instructions":"test","model":{"model_id":"qwen3.7-plus"}}',
        spec_hash,
        owner_id,
    )
    await conn.execute(
        """
        INSERT INTO agent_members (
            tenant_id, agent_id, principal_type, principal_id, role, created_by
        ) VALUES ($1, $2, 'user', $3, 'owner', $3)
        """,
        tenant_id,
        agent_id,
        owner_id,
    )
    await conn.execute(
        "UPDATE agents SET current_draft_id = $3 WHERE tenant_id = $1 AND agent_id = $2",
        tenant_id,
        agent_id,
        draft_id,
    )
    await conn.execute(
        """
        INSERT INTO agent_draft_knowledge_bindings (
            tenant_id, draft_id, dataset_id, retrieval_config
        ) VALUES ($1, $2, $3, '{}'::jsonb)
        """,
        tenant_id,
        draft_id,
        dataset_id,
    )
    await conn.execute(
        """
        INSERT INTO agent_versions (
            tenant_id, agent_version_id, agent_id, version_number,
            schema_version, resolved_spec, spec_hash, source_draft_id,
            source_draft_revision, created_by
        ) VALUES ($1, $2, $3, 1, 'agent-spec/v1', $4::jsonb, $5, $6, 1, $7)
        """,
        tenant_id,
        version_id,
        agent_id,
        '{"schema_version":"agent-spec/v1","instructions":"test","model":{"model_id":"qwen3.7-plus"}}',
        spec_hash,
        draft_id,
        owner_id,
    )
    await conn.execute(
        """
        INSERT INTO agent_version_capabilities (
            tenant_id, agent_version_id, capability_type, resource_id, config
        ) VALUES ($1, $2, 'native', 'web_fetch', '{}'::jsonb)
        """,
        tenant_id,
        version_id,
    )
    await conn.execute(
        """
        INSERT INTO agent_version_knowledge_bindings (
            tenant_id, agent_version_id, dataset_id, retrieval_config
        ) VALUES ($1, $2, $3, '{}'::jsonb)
        """,
        tenant_id,
        version_id,
        dataset_id,
    )
    await conn.execute(
        """
        UPDATE agent_versions
        SET bindings_sealed = TRUE
        WHERE tenant_id = $1 AND agent_version_id = $2
        """,
        tenant_id,
        version_id,
    )
    await conn.execute(
        """
        INSERT INTO agent_publications (
            tenant_id, publication_id, agent_id, channel, version_id,
            created_by, updated_by
        ) VALUES ($1, $2, $3, 'api', $4, $5, $5)
        """,
        tenant_id,
        publication_id,
        agent_id,
        version_id,
        owner_id,
    )
    await conn.execute(
        """
        INSERT INTO agent_publish_events (
            tenant_id, publication_id, agent_id, to_version_id, actor_id
        ) VALUES ($1, $2, $3, $4, $5)
        """,
        tenant_id,
        publication_id,
        agent_id,
        version_id,
        owner_id,
    )
    await conn.execute(
        """
        INSERT INTO agent_api_tokens (
            tenant_id, publication_id, token_hash, name, created_by
        ) VALUES ($1, $2, $3, 'test-token', $4)
        """,
        tenant_id,
        publication_id,
        hashlib.sha256(f"token-{suffix}".encode()).hexdigest(),
        owner_id,
    )
    await transaction.commit()
    return {
        "tenant_id": tenant_id,
        "agent_id": agent_id,
        "draft_id": draft_id,
        "version_id": version_id,
        "publication_id": publication_id,
        "dataset_id": dataset_id,
        "owner_id": owner_id,
    }


async def _assert_fk_rejected(conn: asyncpg.Connection, query: str, *args: Any) -> None:
    transaction = conn.transaction()
    await transaction.start()
    try:
        with pytest.raises(asyncpg.ForeignKeyViolationError):
            await conn.execute(query, *args)
            await conn.execute("SET CONSTRAINTS ALL IMMEDIATE")
    finally:
        await transaction.rollback()


@pytest.mark.asyncio
async def test_migration_is_idempotent_additive_and_declares_tenant_columns(
    agent_pool: asyncpg.Pool,
) -> None:
    sql = MIGRATION.read_text(encoding="utf-8")
    upper = sql.upper()
    assert "DROP TABLE" not in upper
    assert "TRUNCATE" not in upper
    assert "ALTER COLUMN SERVICE_ID" not in upper

    async with agent_pool.acquire() as conn:
        rows = await conn.fetch(
            """
            SELECT table_name
            FROM information_schema.columns
            WHERE table_schema = current_schema()
              AND column_name = 'tenant_id'
              AND table_name = ANY($1::text[])
            """,
            list(AGENT_TABLES),
        )
        assert {row["table_name"] for row in rows} == set(AGENT_TABLES)
        trigger_count = await conn.fetchval(
            """
            SELECT COUNT(*)
            FROM pg_trigger AS trigger
            JOIN pg_class AS relation ON relation.oid = trigger.tgrelid
            JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
            WHERE trigger.tgname = 'agent_versions_immutable'
              AND NOT trigger.tgisinternal
              AND namespace.nspname = current_schema()
            """
        )
        assert trigger_count == 1


@pytest.mark.asyncio
async def test_composite_foreign_keys_reject_every_cross_tenant_child(
    agent_pool: asyncpg.Pool,
) -> None:
    async with agent_pool.acquire() as conn:
        parent = await _insert_graph(conn, "tenant-a", uuid.uuid4().hex[:8])
        other = await _insert_graph(conn, "tenant-b", uuid.uuid4().hex[:8])
        hash_value = hashlib.sha256(uuid.uuid4().bytes).hexdigest()

        await _assert_fk_rejected(
            conn,
            """
            INSERT INTO agent_members (
                tenant_id, agent_id, principal_type, principal_id, role, created_by
            ) VALUES ('tenant-b', $1, 'user', 'cross-user', 'viewer', 'cross-user')
            """,
            parent["agent_id"],
        )
        await _assert_fk_rejected(
            conn,
            """
            INSERT INTO agent_drafts (
                tenant_id, draft_id, agent_id, revision, spec, spec_hash, updated_by
            ) VALUES ('tenant-b', $1, $2, 1, '{}'::jsonb, $3, 'cross-user')
            """,
            uuid.uuid4(),
            parent["agent_id"],
            hash_value,
        )
        await _assert_fk_rejected(
            conn,
            "UPDATE agents SET current_draft_id = $1 WHERE tenant_id = 'tenant-b' AND agent_id = $2",
            parent["draft_id"],
            other["agent_id"],
        )
        await _assert_fk_rejected(
            conn,
            """
            INSERT INTO agent_draft_knowledge_bindings (
                tenant_id, draft_id, dataset_id, retrieval_config
            ) VALUES ('tenant-b', $1, $2, '{}'::jsonb)
            """,
            parent["draft_id"],
            parent["dataset_id"],
        )
        await _assert_fk_rejected(
            conn,
            """
            INSERT INTO agent_versions (
                tenant_id, agent_version_id, agent_id, version_number,
                resolved_spec, spec_hash, source_draft_id,
                source_draft_revision, created_by
            ) VALUES ('tenant-b', $1, $2, 2, '{}'::jsonb, $3, $4, 1, 'cross-user')
            """,
            uuid.uuid4(),
            parent["agent_id"],
            hash_value,
            parent["draft_id"],
        )
        await _assert_fk_rejected(
            conn,
            """
            INSERT INTO agent_version_capabilities (
                tenant_id, agent_version_id, capability_type, resource_id
            ) VALUES ('tenant-b', $1, 'native', 'cross-tool')
            """,
            parent["version_id"],
        )
        await _assert_fk_rejected(
            conn,
            """
            INSERT INTO agent_version_knowledge_bindings (
                tenant_id, agent_version_id, dataset_id
            ) VALUES ('tenant-b', $1, $2)
            """,
            parent["version_id"],
            parent["dataset_id"],
        )
        await _assert_fk_rejected(
            conn,
            """
            INSERT INTO agent_publications (
                tenant_id, publication_id, agent_id, channel, version_id,
                created_by, updated_by
            ) VALUES ('tenant-b', $1, $2, 'hosted', $3, 'cross-user', 'cross-user')
            """,
            uuid.uuid4(),
            parent["agent_id"],
            parent["version_id"],
        )
        await _assert_fk_rejected(
            conn,
            """
            INSERT INTO agent_publish_events (
                tenant_id, publication_id, agent_id, to_version_id, actor_id
            ) VALUES ('tenant-b', $1, $2, $3, 'cross-user')
            """,
            parent["publication_id"],
            parent["agent_id"],
            parent["version_id"],
        )
        await _assert_fk_rejected(
            conn,
            """
            INSERT INTO agent_api_tokens (
                tenant_id, publication_id, token_hash, name, created_by
            ) VALUES ('tenant-b', $1, $2, 'cross-token', 'cross-user')
            """,
            parent["publication_id"],
            hashlib.sha256(uuid.uuid4().bytes).hexdigest(),
        )


@pytest.mark.asyncio
async def test_version_bindings_are_immutable_and_publish_events_append_only(
    agent_pool: asyncpg.Pool,
) -> None:
    async with agent_pool.acquire() as conn:
        graph = await _insert_graph(conn, "tenant-immutable", uuid.uuid4().hex[:8])
        extra_dataset_id = f"dataset-extra-{uuid.uuid4().hex[:8]}"
        await conn.execute(
            "INSERT INTO datasets (dataset_id, tenant_id, name) VALUES ($1, $2, $1)",
            extra_dataset_id,
            graph["tenant_id"],
        )
        mutations = [
            (
                "UPDATE agent_versions SET version_number = 9 WHERE tenant_id = $1 AND agent_version_id = $2",
                graph["tenant_id"],
                graph["version_id"],
            ),
            (
                "DELETE FROM agent_version_capabilities WHERE tenant_id = $1 AND agent_version_id = $2",
                graph["tenant_id"],
                graph["version_id"],
            ),
            (
                "DELETE FROM agent_version_knowledge_bindings WHERE tenant_id = $1 AND agent_version_id = $2",
                graph["tenant_id"],
                graph["version_id"],
            ),
            (
                """
                INSERT INTO agent_version_capabilities (
                    tenant_id, agent_version_id, capability_type, resource_id
                ) VALUES ($1, $2, 'native', 'late-tool')
                """,
                graph["tenant_id"],
                graph["version_id"],
            ),
            (
                """
                INSERT INTO agent_version_knowledge_bindings (
                    tenant_id, agent_version_id, dataset_id
                ) VALUES ($1, $2, $3)
                """,
                graph["tenant_id"],
                graph["version_id"],
                extra_dataset_id,
            ),
            (
                "DELETE FROM agent_publish_events WHERE tenant_id = $1 AND publication_id = $2",
                graph["tenant_id"],
                graph["publication_id"],
            ),
        ]
        for mutation in mutations:
            query, *args = mutation
            transaction = conn.transaction()
            await transaction.start()
            try:
                with pytest.raises(
                    asyncpg.ObjectNotInPrerequisiteStateError, match="AGENT_VERSION_IMMUTABLE"
                ):
                    await conn.execute(query, *args)
            finally:
                await transaction.rollback()

        unsealed_version_id = uuid.uuid4()
        with pytest.raises(asyncpg.CheckViolationError, match="AGENT_VERSION_NOT_SEALED"):
            async with conn.transaction():
                await conn.execute(
                    """
                    INSERT INTO agent_versions (
                        tenant_id, agent_version_id, agent_id, version_number,
                        schema_version, resolved_spec, spec_hash, source_draft_id,
                        source_draft_revision, created_by
                    ) VALUES ($1, $2, $3, 2, 'agent-spec/v1', '{}'::jsonb,
                              $4, $5, 1, $6)
                    """,
                    graph["tenant_id"],
                    unsealed_version_id,
                    graph["agent_id"],
                    hashlib.sha256(b"unsealed").hexdigest(),
                    graph["draft_id"],
                    graph["owner_id"],
                )


@pytest.mark.asyncio
async def test_database_guard_prevents_last_owner_removal_or_demotion(
    agent_pool: asyncpg.Pool,
) -> None:
    async with agent_pool.acquire() as conn:
        graph = await _insert_graph(conn, "tenant-owner", uuid.uuid4().hex[:8])
        for query in (
            "DELETE FROM agent_members WHERE tenant_id = $1 AND agent_id = $2 AND principal_id = $3",
            "UPDATE agent_members SET role = 'editor' WHERE tenant_id = $1 AND agent_id = $2 AND principal_id = $3",
        ):
            transaction = conn.transaction()
            await transaction.start()
            try:
                with pytest.raises(asyncpg.CheckViolationError, match="AGENT_LAST_OWNER"):
                    await conn.execute(
                        query, graph["tenant_id"], graph["agent_id"], graph["owner_id"]
                    )
            finally:
                await transaction.rollback()

        second_owner = f"owner-second-{uuid.uuid4().hex[:8]}"
        await conn.execute(
            "INSERT INTO users (user_id, tenant_id) VALUES ($1, $2)",
            second_owner,
            graph["tenant_id"],
        )
        await conn.execute(
            """
            INSERT INTO agent_members (
                tenant_id, agent_id, principal_type, principal_id, role, created_by
            ) VALUES ($1, $2, 'user', $3, 'owner', $4)
            """,
            graph["tenant_id"],
            graph["agent_id"],
            second_owner,
            graph["owner_id"],
        )
        result = await conn.execute(
            "DELETE FROM agent_members WHERE tenant_id = $1 AND agent_id = $2 AND principal_id = $3",
            graph["tenant_id"],
            graph["agent_id"],
            graph["owner_id"],
        )
        assert result == "DELETE 1"
        owner_id = await conn.fetchval(
            "SELECT owner_id FROM agents WHERE tenant_id = $1 AND agent_id = $2",
            graph["tenant_id"],
            graph["agent_id"],
        )
        assert owner_id == second_owner


@pytest.mark.asyncio
async def test_database_enforces_tenant_principal_and_owner_consistency(
    agent_pool: asyncpg.Pool,
) -> None:
    async with agent_pool.acquire() as conn:
        graph = await _insert_graph(conn, "tenant-principal", uuid.uuid4().hex[:8])
        foreign_user = f"foreign-user-{uuid.uuid4().hex[:8]}"
        await conn.execute(
            "INSERT INTO users (user_id, tenant_id) VALUES ($1, 'tenant-foreign')",
            foreign_user,
        )
        with pytest.raises(asyncpg.ForeignKeyViolationError):
            await conn.execute(
                """
                INSERT INTO agent_members (
                    tenant_id, agent_id, principal_type, principal_id, role, created_by
                ) VALUES ($1, $2, 'user', $3, 'viewer', $4)
                """,
                graph["tenant_id"],
                graph["agent_id"],
                foreign_user,
                graph["owner_id"],
            )

        with pytest.raises(asyncpg.CheckViolationError):
            await conn.execute(
                """
                INSERT INTO agent_members (
                    tenant_id, agent_id, principal_type, principal_id, role, created_by
                ) VALUES ($1, $2, 'group', 'group-without-registry', 'viewer', $3)
                """,
                graph["tenant_id"],
                graph["agent_id"],
                graph["owner_id"],
            )

        replacement_id = f"replacement-{uuid.uuid4().hex[:8]}"
        await conn.execute(
            "INSERT INTO users (user_id, tenant_id) VALUES ($1, $2)",
            replacement_id,
            graph["tenant_id"],
        )
        with pytest.raises(
            asyncpg.ObjectNotInPrerequisiteStateError,
            match="AGENT_MEMBER_IDENTITY_IMMUTABLE",
        ):
            await conn.execute(
                """
                UPDATE agent_members
                SET principal_id = $4
                WHERE tenant_id = $1 AND agent_id = $2 AND principal_id = $3
                """,
                graph["tenant_id"],
                graph["agent_id"],
                graph["owner_id"],
                replacement_id,
            )

        no_owner_id = uuid.uuid4()
        with pytest.raises(asyncpg.CheckViolationError, match="AGENT_OWNER_INVARIANT"):
            async with conn.transaction():
                await conn.execute(
                    """
                    INSERT INTO agents (
                        tenant_id, agent_id, slug, name, owner_id, created_by, updated_by
                    ) VALUES ($1, $2, $3, $3, $4, $4, $4)
                    """,
                    graph["tenant_id"],
                    no_owner_id,
                    f"no-owner-{uuid.uuid4().hex[:8]}",
                    replacement_id,
                )

        await conn.execute(
            """
            INSERT INTO agent_members (
                tenant_id, agent_id, principal_type, principal_id, role, created_by
            ) VALUES ($1, $2, 'user', $3, 'viewer', $4)
            """,
            graph["tenant_id"],
            graph["agent_id"],
            replacement_id,
            graph["owner_id"],
        )
        with pytest.raises(asyncpg.CheckViolationError, match="AGENT_OWNER_INVARIANT"):
            async with conn.transaction():
                await conn.execute(
                    """
                    UPDATE agents SET owner_id = $3
                    WHERE tenant_id = $1 AND agent_id = $2
                    """,
                    graph["tenant_id"],
                    graph["agent_id"],
                    replacement_id,
                )
        current_owner = await conn.fetchval(
            "SELECT owner_id FROM agents WHERE tenant_id = $1 AND agent_id = $2",
            graph["tenant_id"],
            graph["agent_id"],
        )
        assert current_owner == graph["owner_id"]


@pytest.mark.asyncio
async def test_repository_enforces_tenant_revision_version_and_hash_only_token_contracts(
    agent_pool: asyncpg.Pool,
) -> None:
    repository = DatabaseAgentRepository(_Holder(agent_pool))
    tenant_id = f"tenant-repo-{uuid.uuid4().hex[:8]}"
    user_id = f"owner-repo-{uuid.uuid4().hex[:8]}"
    async with agent_pool.acquire() as conn:
        await conn.execute(
            "INSERT INTO users (user_id, tenant_id) VALUES ($1, $2)", user_id, tenant_id
        )

    spec = {
        "schema_version": "agent-spec/v1",
        "instructions": "Repository contract",
        "model": {"model_id": "qwen3.7-plus", "max_tokens": 2048},
        "capabilities": [],
        "knowledge": [],
    }
    agent = await repository.create_agent(
        tenant_id=tenant_id,
        user_id=user_id,
        name="Repository Agent",
        slug=None,
        description="",
        spec=spec,
    )
    agent_id = agent["agent_id"]
    with pytest.raises(AgentNotFoundError):
        await repository.get_agent(
            tenant_id="tenant-other",
            agent_id=agent_id,
            user_id=user_id,
            is_tenant_admin=True,
        )

    updated_spec = {**spec, "instructions": "Revision two"}
    draft = await repository.update_draft(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        expected_revision=1,
        spec=updated_spec,
        agent_changes={
            "name": "Atomic Repository Agent",
            "description": "Saved with revision two",
        },
    )
    assert draft["revision"] == 2
    updated_agent = await repository.get_agent(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
    )
    assert updated_agent["name"] == "Atomic Repository Agent"
    assert updated_agent["description"] == "Saved with revision two"
    with pytest.raises(AgentDraftConflictError) as conflict:
        await repository.update_draft(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=user_id,
            is_tenant_admin=False,
            expected_revision=1,
            spec={**spec, "instructions": "Stale"},
            agent_changes={
                "name": "Must not survive conflict",
                "description": "Must not survive conflict",
            },
        )
    assert conflict.value.current_revision == 2
    after_conflict = await repository.get_agent(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
    )
    assert after_conflict["name"] == "Atomic Repository Agent"
    assert after_conflict["description"] == "Saved with revision two"

    with pytest.raises(AgentValidationError):
        await repository.update_draft(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=user_id,
            is_tenant_admin=False,
            expected_revision=2,
            spec={
                **spec,
                "knowledge": [
                    {
                        "dataset_id": "missing-dataset",
                        "retrieval_config": {"mode": "auto"},
                    }
                ],
            },
            agent_changes={
                "name": "Must not survive validation",
                "description": "Must not survive validation",
            },
        )
    after_validation = await repository.get_agent(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
    )
    assert after_validation["name"] == "Atomic Repository Agent"
    assert after_validation["description"] == "Saved with revision two"
    assert after_validation["draft"]["revision"] == 2

    async with agent_pool.acquire() as conn:
        await conn.execute(
            """
            CREATE FUNCTION reject_synthetic_draft_storage_failure()
            RETURNS trigger AS $$
            BEGIN
                IF NEW.spec ->> 'instructions' = 'force storage failure' THEN
                    RAISE EXCEPTION 'synthetic draft storage failure';
                END IF;
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER reject_synthetic_draft_storage_failure
            BEFORE UPDATE ON agent_drafts
            FOR EACH ROW EXECUTE FUNCTION reject_synthetic_draft_storage_failure();
            """
        )
    with pytest.raises(asyncpg.PostgresError, match="synthetic draft storage failure"):
        await repository.update_draft(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=user_id,
            is_tenant_admin=False,
            expected_revision=2,
            spec={**spec, "instructions": "force storage failure"},
            agent_changes={
                "name": "Must not survive storage failure",
                "description": "Must not survive storage failure",
            },
        )
    after_storage_failure = await repository.get_agent(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
    )
    assert after_storage_failure["name"] == "Atomic Repository Agent"
    assert after_storage_failure["description"] == "Saved with revision two"
    assert after_storage_failure["draft"]["revision"] == 2

    version = await repository.create_version(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        expected_revision=2,
    )
    await repository.update_draft(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        expected_revision=2,
        spec={**spec, "instructions": "Revision three"},
    )
    versions = await repository.list_versions(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
    )
    assert versions[0]["spec_hash"] == version["spec_hash"]
    assert versions[0]["source_draft_revision"] == 2

    publication_id = uuid.uuid4()
    async with agent_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO agent_publications (
                tenant_id, publication_id, agent_id, channel, version_id,
                status, created_by, updated_by
            ) VALUES ($1, $2, $3, 'api', $4, 'active', $5, $5)
            """,
            tenant_id,
            publication_id,
            uuid.UUID(agent_id),
            uuid.UUID(version["agent_version_id"]),
            user_id,
        )
    raw_token, token_record = await repository.create_api_token(
        tenant_id=tenant_id,
        publication_id=str(publication_id),
        user_id=user_id,
        name="runtime",
        scopes=["chat:write"],
        expires_at=None,
    )
    assert raw_token.startswith("agt_")
    assert "token_hash" not in token_record
    async with agent_pool.acquire() as conn:
        stored_hash = await conn.fetchval(
            "SELECT token_hash FROM agent_api_tokens WHERE tenant_id = $1 AND token_id = $2",
            tenant_id,
            uuid.UUID(token_record["token_id"]),
        )
        token_audit = await conn.fetchrow(
            """
            SELECT resource_id, request_summary
            FROM audit_logs
            WHERE tenant_id = $1 AND action = 'api_token_create'
            ORDER BY id DESC LIMIT 1
            """,
            tenant_id,
        )
    assert stored_hash == hashlib.sha256(raw_token.encode()).hexdigest()
    assert raw_token != stored_hash
    assert token_audit["resource_id"] == agent_id
    summary = token_audit["request_summary"]
    if isinstance(summary, str):
        summary = json.loads(summary)
    rendered_summary = json.dumps(summary, sort_keys=True)
    assert summary["token_id"] == token_record["token_id"]
    assert raw_token not in rendered_summary
    assert stored_hash not in rendered_summary


@pytest.mark.asyncio
async def test_production_repository_enforces_role_matrix_and_tenant_admin_boundary(
    agent_pool: asyncpg.Pool,
) -> None:
    repository = DatabaseAgentRepository(_Holder(agent_pool))
    tenant_id = f"tenant-rbac-{uuid.uuid4().hex[:8]}"
    other_tenant_id = f"tenant-rbac-other-{uuid.uuid4().hex[:8]}"
    owner_id = f"owner-rbac-{uuid.uuid4().hex[:8]}"
    editor_id = f"editor-rbac-{uuid.uuid4().hex[:8]}"
    viewer_id = f"viewer-rbac-{uuid.uuid4().hex[:8]}"
    admin_id = f"admin-rbac-{uuid.uuid4().hex[:8]}"
    other_admin_id = f"admin-other-{uuid.uuid4().hex[:8]}"
    async with agent_pool.acquire() as conn:
        await conn.executemany(
            "INSERT INTO users (user_id, tenant_id) VALUES ($1, $2)",
            [
                (owner_id, tenant_id),
                (editor_id, tenant_id),
                (viewer_id, tenant_id),
                (admin_id, tenant_id),
                (other_admin_id, other_tenant_id),
            ],
        )

    spec = {
        "schema_version": "agent-spec/v1",
        "instructions": "Production RBAC contract",
        "model": {"model_id": "qwen3.7-plus"},
        "capabilities": [],
        "knowledge": [],
        "memory": {},
    }
    agent = await repository.create_agent(
        tenant_id=tenant_id,
        user_id=owner_id,
        name="Production RBAC Agent",
        slug=None,
        description="",
        spec=spec,
    )
    agent_id = agent["agent_id"]
    for principal_id, role in ((editor_id, "editor"), (viewer_id, "viewer")):
        await repository.upsert_member(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=owner_id,
            is_tenant_admin=False,
            principal_type="user",
            principal_id=principal_id,
            role=role,
        )

    assert (
        await repository.get_agent(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=viewer_id,
            is_tenant_admin=False,
        )
    )["caller_role"] == "viewer"
    assert (
        await repository.get_draft(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=viewer_id,
            is_tenant_admin=False,
        )
    )["revision"] == 1
    assert (
        await repository.list_versions(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=viewer_id,
            is_tenant_admin=False,
        )
        == []
    )
    assert (
        len(
            await repository.list_members(
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=viewer_id,
                is_tenant_admin=False,
            )
        )
        == 3
    )
    viewer_page = await repository.list_agents(
        tenant_id=tenant_id,
        user_id=viewer_id,
        is_tenant_admin=False,
        limit=20,
    )
    assert [item["agent_id"] for item in viewer_page["items"]] == [agent_id]

    with pytest.raises(AgentNotFoundError):
        await repository.update_agent(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=viewer_id,
            is_tenant_admin=False,
            changes={"description": "denied"},
        )
    with pytest.raises(AgentNotFoundError):
        await repository.update_draft(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=viewer_id,
            is_tenant_admin=False,
            expected_revision=1,
            spec={**spec, "instructions": "denied"},
        )
    for owner_operation in (
        repository.create_version(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=viewer_id,
            is_tenant_admin=False,
            expected_revision=1,
        ),
        repository.copy_agent(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=viewer_id,
            is_tenant_admin=False,
            name=None,
            slug=None,
        ),
        repository.archive_agent(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=viewer_id,
            is_tenant_admin=False,
            disable_publications=False,
        ),
        repository.soft_delete_agent(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=viewer_id,
            is_tenant_admin=False,
        ),
        repository.upsert_member(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=viewer_id,
            is_tenant_admin=False,
            principal_type="user",
            principal_id=admin_id,
            role="viewer",
        ),
        repository.remove_member(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=viewer_id,
            is_tenant_admin=False,
            principal_type="user",
            principal_id=editor_id,
        ),
    ):
        with pytest.raises(AgentNotFoundError):
            await owner_operation

    edited = await repository.update_agent(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=editor_id,
        is_tenant_admin=False,
        changes={"description": "editor update"},
    )
    assert edited["description"] == "editor update"
    draft = await repository.update_draft(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=editor_id,
        is_tenant_admin=False,
        expected_revision=1,
        spec={**spec, "instructions": "editor draft"},
    )
    assert draft["revision"] == 2
    with pytest.raises(AgentNotFoundError):
        await repository.create_version(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=editor_id,
            is_tenant_admin=False,
            expected_revision=2,
        )
    for editor_owner_operation in (
        repository.copy_agent(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=editor_id,
            is_tenant_admin=False,
            name=None,
            slug=None,
        ),
        repository.archive_agent(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=editor_id,
            is_tenant_admin=False,
            disable_publications=False,
        ),
        repository.upsert_member(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=editor_id,
            is_tenant_admin=False,
            principal_type="user",
            principal_id=admin_id,
            role="viewer",
        ),
    ):
        with pytest.raises(AgentNotFoundError):
            await editor_owner_operation

    versions = await asyncio.gather(
        *[
            repository.create_version(
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=owner_id,
                is_tenant_admin=False,
                expected_revision=2,
            )
            for _ in range(2)
        ]
    )
    assert sorted(item["version_number"] for item in versions) == [1, 2]
    assert all(item["bindings_sealed"] is True for item in versions)

    admin_edit = await repository.update_agent(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=admin_id,
        is_tenant_admin=True,
        changes={"description": "tenant admin update"},
    )
    assert admin_edit["description"] == "tenant admin update"

    other_page = await repository.list_agents(
        tenant_id=other_tenant_id,
        user_id=other_admin_id,
        is_tenant_admin=True,
        limit=20,
    )
    assert other_page == {"items": [], "next_cursor": None}
    cross_tenant_calls = (
        repository.get_agent(
            tenant_id=other_tenant_id,
            agent_id=agent_id,
            user_id=other_admin_id,
            is_tenant_admin=True,
        ),
        repository.get_draft(
            tenant_id=other_tenant_id,
            agent_id=agent_id,
            user_id=other_admin_id,
            is_tenant_admin=True,
        ),
        repository.update_agent(
            tenant_id=other_tenant_id,
            agent_id=agent_id,
            user_id=other_admin_id,
            is_tenant_admin=True,
            changes={"description": "cross tenant"},
        ),
        repository.update_draft(
            tenant_id=other_tenant_id,
            agent_id=agent_id,
            user_id=other_admin_id,
            is_tenant_admin=True,
            expected_revision=2,
            spec={**spec, "instructions": "cross tenant"},
        ),
        repository.create_version(
            tenant_id=other_tenant_id,
            agent_id=agent_id,
            user_id=other_admin_id,
            is_tenant_admin=True,
            expected_revision=2,
        ),
        repository.list_versions(
            tenant_id=other_tenant_id,
            agent_id=agent_id,
            user_id=other_admin_id,
            is_tenant_admin=True,
        ),
        repository.list_members(
            tenant_id=other_tenant_id,
            agent_id=agent_id,
            user_id=other_admin_id,
            is_tenant_admin=True,
        ),
        repository.upsert_member(
            tenant_id=other_tenant_id,
            agent_id=agent_id,
            user_id=other_admin_id,
            is_tenant_admin=True,
            principal_type="user",
            principal_id=other_admin_id,
            role="viewer",
        ),
        repository.remove_member(
            tenant_id=other_tenant_id,
            agent_id=agent_id,
            user_id=other_admin_id,
            is_tenant_admin=True,
            principal_type="user",
            principal_id=owner_id,
        ),
        repository.copy_agent(
            tenant_id=other_tenant_id,
            agent_id=agent_id,
            user_id=other_admin_id,
            is_tenant_admin=True,
            name=None,
            slug=None,
        ),
        repository.archive_agent(
            tenant_id=other_tenant_id,
            agent_id=agent_id,
            user_id=other_admin_id,
            is_tenant_admin=True,
            disable_publications=False,
        ),
        repository.soft_delete_agent(
            tenant_id=other_tenant_id,
            agent_id=agent_id,
            user_id=other_admin_id,
            is_tenant_admin=True,
        ),
    )
    for cross_tenant_call in cross_tenant_calls:
        with pytest.raises(AgentNotFoundError):
            await cross_tenant_call


@pytest.mark.asyncio
async def test_production_repository_rejects_and_redacts_unsafe_specs_and_copies_fail_closed(
    agent_pool: asyncpg.Pool,
) -> None:
    repository = DatabaseAgentRepository(_Holder(agent_pool))
    tenant_id = f"tenant-spec-{uuid.uuid4().hex[:8]}"
    owner_id = f"owner-spec-{uuid.uuid4().hex[:8]}"
    async with agent_pool.acquire() as conn:
        await conn.execute(
            "INSERT INTO users (user_id, tenant_id) VALUES ($1, $2)",
            owner_id,
            tenant_id,
        )

    spec = {
        "schema_version": "agent-spec/v1",
        "identity": {"welcome_message": "Safe"},
        "instructions": "Safe repository spec",
        "model": {"model_id": "qwen3.7-plus", "max_tokens": 2048},
        "capabilities": [],
        "knowledge": [],
        "memory": {},
    }
    unsafe_create = {**spec, "apiKey": "synthetic-not-a-real-key"}
    with pytest.raises(AgentValidationError):
        await repository.create_agent(
            tenant_id=tenant_id,
            user_id=owner_id,
            name="Rejected unsafe Agent",
            slug=None,
            description="",
            spec=unsafe_create,
        )

    agent = await repository.create_agent(
        tenant_id=tenant_id,
        user_id=owner_id,
        name="Legacy source Agent",
        slug=None,
        description="",
        spec=spec,
    )
    agent_id = agent["agent_id"]
    unsafe_update = {
        **spec,
        "model": {**spec["model"], "private_key": "synthetic-private-material"},
    }
    with pytest.raises(AgentValidationError):
        await repository.update_draft(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=owner_id,
            is_tenant_admin=False,
            expected_revision=1,
            spec=unsafe_update,
        )

    legacy_spec = {
        **spec,
        "apiKey": "synthetic-not-a-real-key",
        "secretRef": "synthetic-secret-reference",
        "tool_bindings": [{"resource_id": "legacy-tool"}],
        "model": {**spec["model"], "private_key": "synthetic-private-material"},
        "memory": {"authorization": "synthetic-authorization-value", "mode": "session"},
    }
    async with agent_pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE agent_drafts
            SET spec = $3::jsonb, spec_hash = $4
            WHERE tenant_id = $1 AND agent_id = $2
            """,
            tenant_id,
            uuid.UUID(agent_id),
            json.dumps(legacy_spec),
            hashlib.sha256(json.dumps(legacy_spec, sort_keys=True).encode()).hexdigest(),
        )

    redacted = await repository.get_draft(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
    )
    rendered = json.dumps(redacted["spec"], sort_keys=True)
    for forbidden in (
        "apiKey",
        "secretRef",
        "tool_bindings",
        "private_key",
        "authorization",
        "synthetic-not-a-real-key",
        "synthetic-private-material",
    ):
        assert forbidden not in rendered
    assert redacted["spec"]["memory"] == {"mode": "session"}

    copied = await repository.copy_agent(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        name="Fail-closed copy",
        slug=None,
    )
    copied_draft = await repository.get_draft(
        tenant_id=tenant_id,
        agent_id=copied["agent_id"],
        user_id=owner_id,
        is_tenant_admin=False,
    )
    assert copied_draft["spec"] == {
        "schema_version": "agent-spec/v1",
        "identity": {"welcome_message": "Safe"},
        "instructions": "Safe repository spec",
        "model": {"model_id": "qwen3.7-plus", "max_tokens": 2048},
        "capabilities": [],
        "knowledge": [],
        "memory": {},
    }


@pytest.mark.asyncio
async def test_production_repository_maps_last_owner_violation(
    agent_pool: asyncpg.Pool,
) -> None:
    repository = DatabaseAgentRepository(_Holder(agent_pool))
    tenant_id = f"tenant-owner-map-{uuid.uuid4().hex[:8]}"
    owner_id = f"owner-map-{uuid.uuid4().hex[:8]}"
    async with agent_pool.acquire() as conn:
        await conn.execute(
            "INSERT INTO users (user_id, tenant_id) VALUES ($1, $2)",
            owner_id,
            tenant_id,
        )
    agent = await repository.create_agent(
        tenant_id=tenant_id,
        user_id=owner_id,
        name="Last Owner Agent",
        slug=None,
        description="",
        spec={
            "schema_version": "agent-spec/v1",
            "instructions": "Owner invariant",
            "model": {"model_id": "qwen3.7-plus"},
        },
    )
    with pytest.raises(AgentLastOwnerError):
        await repository.upsert_member(
            tenant_id=tenant_id,
            agent_id=agent["agent_id"],
            user_id=owner_id,
            is_tenant_admin=False,
            principal_type="user",
            principal_id=owner_id,
            role="editor",
        )


@pytest_asyncio.fixture
async def operations_pool() -> AsyncIterator[asyncpg.Pool]:
    config = _postgres_config()
    schema_name = f"agent_operations_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE SCHEMA "{schema_name}"')
    await admin.close()
    pool = await asyncpg.create_pool(
        **config,
        min_size=1,
        max_size=4,
        server_settings={"search_path": f'"{schema_name}",public'},
    )
    try:
        async with pool.acquire() as conn:
            await conn.execute(
                """
                CREATE TABLE agents (
                    tenant_id VARCHAR(255) NOT NULL,
                    agent_id UUID NOT NULL,
                    owner_id VARCHAR(255) NOT NULL,
                    status VARCHAR(32) NOT NULL DEFAULT 'active',
                    current_draft_id UUID,
                    deleted_at TIMESTAMPTZ,
                    updated_by VARCHAR(255) NOT NULL DEFAULT '',
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, agent_id)
                );
                CREATE TABLE users (
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    PRIMARY KEY (tenant_id, user_id)
                );
                CREATE TABLE agent_members (
                    tenant_id VARCHAR(255) NOT NULL,
                    agent_id UUID NOT NULL,
                    principal_type VARCHAR(32) NOT NULL,
                    principal_id VARCHAR(255) NOT NULL,
                    role VARCHAR(16) NOT NULL,
                    PRIMARY KEY (tenant_id, agent_id, principal_type, principal_id)
                );
                CREATE TABLE agent_publications (
                    tenant_id VARCHAR(255) NOT NULL,
                    publication_id UUID NOT NULL,
                    agent_id UUID NOT NULL,
                    channel VARCHAR(16) NOT NULL,
                    status VARCHAR(16) NOT NULL DEFAULT 'active',
                    updated_by VARCHAR(255) NOT NULL DEFAULT '',
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, publication_id)
                );
                CREATE TABLE agent_drafts (
                    tenant_id VARCHAR(255) NOT NULL,
                    draft_id UUID NOT NULL,
                    agent_id UUID NOT NULL,
                    revision INTEGER NOT NULL DEFAULT 1,
                    spec JSONB NOT NULL DEFAULT '{}'::jsonb,
                    spec_hash CHAR(64) NOT NULL DEFAULT repeat('0', 64),
                    updated_by VARCHAR(255) NOT NULL DEFAULT '',
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, draft_id)
                );
                CREATE TABLE agent_versions (
                    tenant_id VARCHAR(255) NOT NULL,
                    agent_version_id UUID NOT NULL,
                    agent_id UUID NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, agent_version_id)
                );
                CREATE TABLE agent_version_capabilities (
                    tenant_id VARCHAR(255) NOT NULL,
                    agent_version_id UUID NOT NULL,
                    capability_type VARCHAR(32) NOT NULL,
                    resource_id VARCHAR(255) NOT NULL
                );
                CREATE TABLE agent_draft_knowledge_bindings (
                    tenant_id VARCHAR(255) NOT NULL,
                    draft_id UUID NOT NULL,
                    dataset_id VARCHAR(255) NOT NULL
                );
                CREATE TABLE agent_draft_skill_bindings (
                    tenant_id VARCHAR(255) NOT NULL,
                    draft_id UUID NOT NULL,
                    skill_version_id UUID NOT NULL
                );
                CREATE TABLE mcp_channel_grants (
                    tenant_id VARCHAR(255) NOT NULL,
                    tool_id UUID NOT NULL,
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE audit_logs (
                    id BIGSERIAL PRIMARY KEY,
                    event_type VARCHAR(100) NOT NULL,
                    user_id VARCHAR(255),
                    tenant_id VARCHAR(255),
                    resource_type VARCHAR(100),
                    resource_id VARCHAR(255),
                    action VARCHAR(50) NOT NULL,
                    request_summary JSONB,
                    response_summary JSONB,
                    status VARCHAR(50) NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE sessions (
                    session_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255),
                    user_id VARCHAR(255),
                    agent_id UUID,
                    agent_version_id UUID,
                    agent_draft_revision INTEGER,
                    publication_id UUID,
                    channel VARCHAR(16),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE assistant_runs (
                    run_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    agent_id UUID,
                    status VARCHAR(32) NOT NULL DEFAULT 'running',
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE assistant_run_checkpoints (
                    checkpoint_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    agent_id UUID,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE agent_traces (
                    trace_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255),
                    agent_id UUID,
                    agent_version_id UUID,
                    publication_id UUID,
                    channel VARCHAR(16),
                    total_tokens BIGINT NOT NULL DEFAULT 0,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE agent_trace_spans (
                    span_id UUID PRIMARY KEY,
                    trace_id UUID NOT NULL,
                    span_kind VARCHAR(64) NOT NULL,
                    name VARCHAR(160) NOT NULL,
                    attributes JSONB NOT NULL DEFAULT '{}'::jsonb
                );
                CREATE TABLE agent_runtime_attachments (
                    tenant_id VARCHAR(255) NOT NULL,
                    attachment_id UUID NOT NULL,
                    publication_id UUID NOT NULL,
                    principal_id VARCHAR(255) NOT NULL,
                    channel VARCHAR(16) NOT NULL,
                    storage_key TEXT NOT NULL,
                    filename VARCHAR(255) NOT NULL DEFAULT 'fixture.txt',
                    mime_type VARCHAR(255) NOT NULL DEFAULT 'text/plain',
                    size_bytes BIGINT NOT NULL DEFAULT 1,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    expires_at TIMESTAMPTZ NOT NULL,
                    PRIMARY KEY (tenant_id, attachment_id)
                );
                CREATE TABLE agent_runtime_idempotency (
                    tenant_id VARCHAR(255) NOT NULL,
                    publication_id UUID NOT NULL,
                    principal_id VARCHAR(255) NOT NULL,
                    idempotency_key VARCHAR(255) NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, publication_id, principal_id, idempotency_key)
                );
                CREATE TABLE agent_runtime_feedback (
                    tenant_id VARCHAR(255) NOT NULL,
                    feedback_id UUID NOT NULL,
                    publication_id UUID NOT NULL,
                    principal_id VARCHAR(255) NOT NULL,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, feedback_id)
                );
                CREATE TABLE session_memory (
                    tenant_id VARCHAR(255) NOT NULL,
                    session_id VARCHAR(255) NOT NULL,
                    key VARCHAR(255) NOT NULL
                );
                CREATE TABLE user_memory (
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    key VARCHAR(255) NOT NULL,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE assistant_memory_sources (
                    source_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL
                );
                CREATE TABLE assistant_memory_reflections (
                    reflection_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE semantic_cache (
                    id BIGSERIAL PRIMARY KEY,
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE agent_api_tokens (
                    tenant_id VARCHAR(255) NOT NULL,
                    token_id UUID NOT NULL,
                    publication_id UUID NOT NULL,
                    created_by VARCHAR(255) NOT NULL,
                    revoked_at TIMESTAMPTZ,
                    PRIMARY KEY (tenant_id, token_id)
                );
                CREATE TABLE mcp_connections (
                    tenant_id VARCHAR(255) NOT NULL,
                    connection_id UUID NOT NULL,
                    owner_user_id VARCHAR(255),
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    revoked_at TIMESTAMPTZ,
                    updated_by VARCHAR(255) NOT NULL,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, connection_id)
                );
                CREATE TABLE connector_credential_principals (
                    tenant_id VARCHAR(255) NOT NULL,
                    grant_id UUID NOT NULL,
                    owner_user_id VARCHAR(255),
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    revoked_at TIMESTAMPTZ,
                    updated_by VARCHAR(255) NOT NULL,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, grant_id)
                );
                """
            )
            sql = OPERATIONS_MIGRATION.read_text(encoding="utf-8")
            await conn.execute(sql)
            await conn.execute(sql)
        yield pool
    finally:
        await pool.close()
        admin = await asyncpg.connect(**config)
        await admin.execute(f'DROP SCHEMA "{schema_name}" CASCADE')
        await admin.close()


async def _seed_operations_agent(
    pool: asyncpg.Pool,
) -> tuple[str, str, str, str]:
    tenant_id = f"tenant-ops-{uuid.uuid4().hex[:8]}"
    agent_id = str(uuid.uuid4())
    publication_id = str(uuid.uuid4())
    owner_id = f"owner-{uuid.uuid4().hex[:8]}"
    async with pool.acquire() as conn, conn.transaction():
        await conn.execute(
            "INSERT INTO users (tenant_id, user_id) VALUES ($1, $2)",
            tenant_id,
            owner_id,
        )
        await conn.execute(
            "INSERT INTO agents (tenant_id, agent_id, owner_id) VALUES ($1, $2::uuid, $3)",
            tenant_id,
            agent_id,
            owner_id,
        )
        await conn.execute(
            """
            INSERT INTO agent_members (
                tenant_id, agent_id, principal_type, principal_id, role
            ) VALUES ($1, $2::uuid, 'user', $3, 'owner')
            """,
            tenant_id,
            agent_id,
            owner_id,
        )
        await conn.execute(
            """
            INSERT INTO agent_publications (
                tenant_id, publication_id, agent_id, channel
            ) VALUES ($1, $2::uuid, $3::uuid, 'api')
            """,
            tenant_id,
            publication_id,
            agent_id,
        )
    return tenant_id, agent_id, publication_id, owner_id


@pytest.mark.asyncio
async def test_operations_migration_is_reentrant_and_projects_audit_dimensions(
    operations_pool: asyncpg.Pool,
) -> None:
    tenant_id, agent_id, publication_id, owner_id = await _seed_operations_agent(operations_pool)
    version_id = str(uuid.uuid4())
    async with operations_pool.acquire() as conn:
        row = await conn.fetchrow(
            """
            INSERT INTO audit_logs (
                event_type, user_id, tenant_id, resource_type, resource_id,
                action, request_summary, status
            ) VALUES (
                'agent_studio', $1, $2, 'agent', $3, 'publication_promote',
                jsonb_build_object(
                    'agent_version_id', $4::text,
                    'publication_id', $5::text,
                    'channel', 'api',
                    'authorization', 'synthetic-secret'
                ), 'success'
            )
            RETURNING agent_id, agent_version_id, publication_id, channel,
                      request_summary, response_summary, redaction_state
            """,
            owner_id,
            tenant_id,
            agent_id,
            version_id,
            publication_id,
        )
        assert str(row["agent_id"]) == agent_id
        assert str(row["agent_version_id"]) == version_id
        assert str(row["publication_id"]) == publication_id
        assert row["channel"] == "api"
        request_summary = row["request_summary"]
        if isinstance(request_summary, str):
            request_summary = json.loads(request_summary)
        assert request_summary["authorization"] == "[REDACTED]"
        assert "synthetic-secret" not in json.dumps(request_summary)
        redaction_state = row["redaction_state"]
        if isinstance(redaction_state, str):
            redaction_state = json.loads(redaction_state)
        assert redaction_state["sensitive_fields"] == "removed"
        with pytest.raises(asyncpg.CheckViolationError):
            await conn.execute(
                """
                INSERT INTO agent_governance_policies (
                    tenant_id, agent_id, trace_retention_days, updated_by
                ) VALUES ($1, $2::uuid, 0, $3)
                """,
                tenant_id,
                agent_id,
                owner_id,
            )


@pytest.mark.asyncio
async def test_legal_hold_blocks_cleanup_and_receipt_is_terminal(
    operations_pool: asyncpg.Pool,
) -> None:
    tenant_id, agent_id, _, owner_id = await _seed_operations_agent(operations_pool)
    repository = DatabaseAgentRepository(_Holder(operations_pool))
    await repository.update_governance_policy(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        changes={"legal_hold": True},
    )
    result = await repository.prepare_agent_data_deletion(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        scope="tenant",
        subject_user_id=None,
        idempotency_key="tenant-delete-0001",
    )
    assert result["status"] == "blocked"
    assert result["error_code"] == "AGENT_LEGAL_HOLD_ACTIVE"
    async with operations_pool.acquire() as conn:
        with pytest.raises(asyncpg.ObjectNotInPrerequisiteStateError):
            await conn.execute(
                """
                UPDATE agent_data_deletion_requests SET error_code = NULL
                WHERE deletion_id = $1::uuid
                """,
                result["deletion_id"],
            )


@pytest.mark.asyncio
async def test_legal_hold_activation_atomically_blocks_unclaimed_cleanup(
    operations_pool: asyncpg.Pool,
) -> None:
    tenant_id, agent_id, _, owner_id = await _seed_operations_agent(operations_pool)
    repository = DatabaseAgentRepository(_Holder(operations_pool))
    prepared = await repository.prepare_agent_data_deletion(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        scope="tenant",
        subject_user_id=None,
        idempotency_key="tenant-delete-hold-race",
    )
    assert prepared["status"] == "pending"
    await repository.update_governance_policy(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        changes={"legal_hold": True},
    )

    blocked = await _finish_claimed_data_deletion(
        repository,
        prepared=prepared,
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        storage_cleanup_succeeded=True,
    )

    assert blocked["status"] == "blocked"
    assert blocked["error_code"] == "AGENT_LEGAL_HOLD_ACTIVE"
    assert blocked["attempt_count"] == 0
    async with operations_pool.acquire() as conn:
        assert (
            await conn.fetchval(
                "SELECT status FROM agents WHERE tenant_id = $1 AND agent_id = $2::uuid",
                tenant_id,
                agent_id,
            )
            == "active"
        )


@pytest.mark.asyncio
async def test_active_cleanup_claim_fences_concurrent_legal_hold_activation(
    operations_pool: asyncpg.Pool,
) -> None:
    tenant_id, agent_id, _, owner_id = await _seed_operations_agent(operations_pool)
    repository = DatabaseAgentRepository(_Holder(operations_pool))
    prepared = await repository.prepare_agent_data_deletion(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        scope="retention",
        subject_user_id=None,
        idempotency_key="retention-hold-execution-fence",
    )
    external_delete_started = asyncio.Event()

    async def activate_legal_hold() -> dict[str, Any]:
        await external_delete_started.wait()
        return await repository.update_governance_policy(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=owner_id,
            is_tenant_admin=False,
            changes={"legal_hold": True},
        )

    async with repository.claim_agent_data_deletion_execution(
        tenant_id=tenant_id,
        agent_id=agent_id,
        deletion_id=prepared["deletion_id"],
        user_id=owner_id,
        is_tenant_admin=False,
    ) as claimed:
        assert claimed["execution_claimed"] is True
        await claimed["_execution_guard"]()
        hold_task = asyncio.create_task(activate_legal_hold())
        external_delete_started.set()
        with pytest.raises(AgentRepositoryError, match="AGENT_LEGAL_HOLD_CLEANUP_ACTIVE"):
            await hold_task
        policy = await repository.get_governance_policy(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=owner_id,
            is_tenant_admin=False,
        )
        assert policy["legal_hold"] is False
        failed = await claimed["_execution_finish"](
            storage_cleanup_succeeded=False,
        )
        assert failed["status"] == "failed"

    applied = await repository.update_governance_policy(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        changes={"legal_hold": True},
    )
    assert applied["legal_hold"] is True
    async with operations_pool.acquire() as conn:
        blocked = await conn.fetchrow(
            """
            SELECT status, error_code, deleted_counts
            FROM agent_data_deletion_requests
            WHERE deletion_id = $1::uuid
            """,
            prepared["deletion_id"],
        )
    assert blocked["status"] == "blocked"
    assert blocked["error_code"] == "AGENT_LEGAL_HOLD_ACTIVE_AFTER_INTERRUPTED_CLEANUP"
    blocked_counts = json.loads(blocked["deleted_counts"])
    assert blocked_counts["cleanup_execution"]["state"] == "blocked"

    blocked_retry = await repository.prepare_agent_data_deletion(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        scope="retention",
        subject_user_id=None,
        idempotency_key="retention-under-active-hold",
    )
    assert blocked_retry["status"] == "blocked"
    assert blocked_retry["error_code"] == "AGENT_LEGAL_HOLD_ACTIVE"


@pytest.mark.asyncio
async def test_released_cleanup_claim_denies_hold_until_idempotent_recovery_finishes(
    operations_pool: asyncpg.Pool,
) -> None:
    tenant_id, agent_id, _, owner_id = await _seed_operations_agent(operations_pool)
    repository = DatabaseAgentRepository(_Holder(operations_pool))
    prepared = await repository.prepare_agent_data_deletion(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        scope="retention",
        subject_user_id=None,
        idempotency_key="retention-abandoned-execution-fence",
    )
    async with repository.claim_agent_data_deletion_execution(
        tenant_id=tenant_id,
        agent_id=agent_id,
        deletion_id=prepared["deletion_id"],
        user_id=owner_id,
        is_tenant_admin=False,
    ) as claimed:
        assert claimed["execution_claimed"] is True
        await claimed["_execution_guard"]()

    with pytest.raises(AgentRepositoryError, match="AGENT_LEGAL_HOLD_CLEANUP_ACTIVE"):
        await repository.update_governance_policy(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=owner_id,
            is_tenant_admin=False,
            changes={"legal_hold": True},
        )
    policy = await repository.get_governance_policy(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
    )
    assert policy["legal_hold"] is False

    async with repository.claim_agent_data_deletion_execution(
        tenant_id=tenant_id,
        agent_id=agent_id,
        deletion_id=prepared["deletion_id"],
        user_id=owner_id,
        is_tenant_admin=False,
    ) as recovered:
        assert recovered["execution_claimed"] is True
        assert recovered["_execution_generation"] == 2
        failed = await recovered["_execution_finish"](
            storage_cleanup_succeeded=False,
        )
        assert failed["status"] == "failed"
        assert failed["error_code"] == "AGENT_STORAGE_CLEANUP_FAILED"
        assert failed["completed_at"] is None

    applied = await repository.update_governance_policy(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        changes={"legal_hold": True},
    )
    assert applied["legal_hold"] is True
    async with operations_pool.acquire() as conn:
        blocked = await conn.fetchrow(
            """
            SELECT status, error_code, deleted_counts
            FROM agent_data_deletion_requests
            WHERE deletion_id = $1::uuid
            """,
            prepared["deletion_id"],
        )
    assert blocked["status"] == "blocked"
    assert blocked["error_code"] == "AGENT_LEGAL_HOLD_ACTIVE_AFTER_INTERRUPTED_CLEANUP"
    blocked_counts = json.loads(blocked["deleted_counts"])
    assert blocked_counts["cleanup_execution"]["state"] == "blocked"
    assert "claim_digest" not in blocked_counts["cleanup_execution"]


@pytest.mark.asyncio
async def test_storage_failure_can_retry_same_deletion_receipt(
    operations_pool: asyncpg.Pool,
) -> None:
    tenant_id, agent_id, _, owner_id = await _seed_operations_agent(operations_pool)
    repository = DatabaseAgentRepository(_Holder(operations_pool))
    prepared = await repository.prepare_agent_data_deletion(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        scope="retention",
        subject_user_id=None,
        idempotency_key="retention-retry-0001",
    )
    failed = await _finish_claimed_data_deletion(
        repository,
        prepared=prepared,
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        storage_cleanup_succeeded=False,
    )
    assert failed["status"] == "failed"
    assert failed["completed_at"] is None

    runtime_receipt = await _freeze_completed_runtime_cleanup(
        repository,
        prepared=prepared,
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
    )
    completed = await _finish_claimed_data_deletion(
        repository,
        prepared=prepared,
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        storage_cleanup_succeeded=True,
        runtime_cleanup_receipt=runtime_receipt,
    )

    assert completed["status"] == "completed"
    assert completed["attempt_count"] == 2
    assert completed["error_code"] is None


@pytest.mark.asyncio
async def test_runtime_governance_usage_enforces_caps_and_emits_threshold_alerts(
    operations_pool: asyncpg.Pool,
) -> None:
    tenant_id, agent_id, publication_id, owner_id = await _seed_operations_agent(operations_pool)
    repository = DatabaseAgentRepository(_Holder(operations_pool))
    await repository.update_governance_policy(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        changes={
            "max_concurrent_runs": 1,
            "max_daily_tokens": 10,
            "max_daily_mcp_calls": 1,
            "max_storage_bytes": 5,
            "alert_threshold_percent": 50,
        },
    )
    trace_id = str(uuid.uuid4())
    async with operations_pool.acquire() as conn:
        await conn.execute(
            "INSERT INTO assistant_runs (run_id, tenant_id, user_id, agent_id) VALUES (gen_random_uuid(), $1, $2, $3::uuid)",
            tenant_id,
            owner_id,
            agent_id,
        )
        await conn.execute(
            """
            INSERT INTO agent_traces (
                trace_id, tenant_id, user_id, agent_id, publication_id,
                channel, total_tokens
            ) VALUES ($1::uuid, $2, $3, $4::uuid, $5::uuid, 'api', 6)
            """,
            trace_id,
            tenant_id,
            owner_id,
            agent_id,
            publication_id,
        )
        await conn.execute(
            """
            INSERT INTO agent_trace_spans (
                span_id, trace_id, span_kind, name
            ) VALUES (gen_random_uuid(), $1::uuid, 'tool_execution', 'tool:mcp_docs_write')
            """,
            trace_id,
        )
        await conn.execute(
            """
            INSERT INTO agent_runtime_attachments (
                tenant_id, attachment_id, publication_id, principal_id,
                channel, storage_key, size_bytes, expires_at
            ) VALUES ($1, gen_random_uuid(), $2::uuid, $3, 'api',
                      'tenant/quota-existing', 3, NOW() + INTERVAL '1 day')
            """,
            tenant_id,
            publication_id,
            owner_id,
        )

    result = await repository.get_runtime_governance_usage(
        tenant_id=tenant_id,
        agent_id=agent_id,
        publication_id=publication_id,
    )
    assert result["usage"] == {
        "concurrent_runs": 1,
        "daily_tokens": 6,
        "daily_mcp_calls": 1,
        "storage_bytes": 3,
    }
    assert set(result["exceeded"]) == {
        "AGENT_RUNTIME_CONCURRENCY_QUOTA_EXCEEDED",
        "AGENT_RUNTIME_MCP_QUOTA_EXCEEDED",
    }
    async with operations_pool.acquire() as conn:
        assert (
            await conn.fetchval(
                """
            SELECT COUNT(*) FROM audit_logs
            WHERE tenant_id = $1 AND agent_id = $2::uuid
              AND action IN ('quota_threshold_reached', 'quota_exceeded')
            """,
                tenant_id,
                agent_id,
            )
            == 4
        )

    with pytest.raises(AgentRuntimeUnavailableError) as storage_error:
        await repository.create_runtime_attachment(
            tenant_id=tenant_id,
            publication_id=publication_id,
            principal_id=owner_id,
            channel="api",
            storage_key="tenant/quota-rejected",
            filename="quota.txt",
            mime_type="text/plain",
            size_bytes=3,
        )
    assert str(storage_error.value) == "AGENT_RUNTIME_STORAGE_QUOTA_EXCEEDED"


@pytest.mark.asyncio
async def test_tenant_deletion_disables_delivery_and_scrubs_mutable_state(
    operations_pool: asyncpg.Pool,
) -> None:
    tenant_id, agent_id, publication_id, owner_id = await _seed_operations_agent(operations_pool)
    repository = DatabaseAgentRepository(_Holder(operations_pool))
    subject_id = f"subject-{uuid.uuid4().hex[:8]}"
    draft_id = str(uuid.uuid4())
    version_id = str(uuid.uuid4())
    mcp_tool_id = str(uuid.uuid4())
    connector_grant_id = str(uuid.uuid4())
    digest = hashlib.sha256(f"{subject_id}:{agent_id}:version:{version_id}".encode()).hexdigest()
    orphan_memory_id = f"agent-memory:{digest}"
    async with operations_pool.acquire() as conn:
        await conn.execute(
            "INSERT INTO users (tenant_id, user_id) VALUES ($1, $2)",
            tenant_id,
            subject_id,
        )
        await conn.execute(
            """
            INSERT INTO agent_drafts (
                tenant_id, draft_id, agent_id, revision, spec, spec_hash, updated_by
            ) VALUES ($1, $2::uuid, $3::uuid, 2, $4::jsonb, $5, $6)
            """,
            tenant_id,
            draft_id,
            agent_id,
            json.dumps({"instructions": "private mutable prompt"}),
            hashlib.sha256(b"private mutable prompt").hexdigest(),
            owner_id,
        )
        await conn.execute(
            "UPDATE agents SET current_draft_id = $3::uuid WHERE tenant_id = $1 AND agent_id = $2::uuid",
            tenant_id,
            agent_id,
            draft_id,
        )
        await conn.execute(
            "INSERT INTO agent_versions (tenant_id, agent_version_id, agent_id) VALUES ($1, $2::uuid, $3::uuid)",
            tenant_id,
            version_id,
            agent_id,
        )
        await conn.executemany(
            """
            INSERT INTO agent_version_capabilities (
                tenant_id, agent_version_id, capability_type, resource_id
            ) VALUES ($1, $2::uuid, $3, $4)
            """,
            [
                (tenant_id, version_id, "mcp", mcp_tool_id),
                (tenant_id, version_id, "connector", connector_grant_id),
            ],
        )
        await conn.execute(
            "INSERT INTO mcp_channel_grants (tenant_id, tool_id) VALUES ($1, $2::uuid)",
            tenant_id,
            mcp_tool_id,
        )
        await conn.execute(
            """
            INSERT INTO connector_credential_principals (
                tenant_id, grant_id, enabled, updated_by
            ) VALUES ($1, $2::uuid, TRUE, $3)
            """,
            tenant_id,
            connector_grant_id,
            owner_id,
        )
        await conn.execute(
            "INSERT INTO agent_draft_knowledge_bindings (tenant_id, draft_id, dataset_id) VALUES ($1, $2::uuid, 'dataset-a')",
            tenant_id,
            draft_id,
        )
        await conn.execute(
            "INSERT INTO agent_draft_skill_bindings (tenant_id, draft_id, skill_version_id) VALUES ($1, $2::uuid, gen_random_uuid())",
            tenant_id,
            draft_id,
        )
        await conn.execute(
            "INSERT INTO user_memory (tenant_id, user_id, key) VALUES ($1, $2, 'orphan')",
            tenant_id,
            orphan_memory_id,
        )

    prepared = await repository.prepare_agent_data_deletion(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        scope="tenant",
        subject_user_id=None,
        idempotency_key="tenant-complete-0001",
    )
    runtime_receipt = await _freeze_completed_runtime_cleanup(
        repository,
        prepared=prepared,
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
    )
    completed = await _finish_claimed_data_deletion(
        repository,
        prepared=prepared,
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        storage_cleanup_succeeded=True,
        runtime_cleanup_receipt=runtime_receipt,
    )
    assert completed["status"] == "completed"
    replayed = await repository.prepare_agent_data_deletion(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        scope="tenant",
        subject_user_id=None,
        idempotency_key="tenant-complete-0001",
    )
    assert replayed["deletion_id"] == completed["deletion_id"]
    assert replayed["status"] == "completed"
    assert replayed["attempt_count"] == completed["attempt_count"] == 1
    assert replayed["deleted_counts"] == completed["deleted_counts"]
    with pytest.raises(AgentNotFoundError, match="AGENT_NOT_FOUND"):
        await repository.prepare_agent_data_deletion(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=subject_id,
            is_tenant_admin=False,
            scope="tenant",
            subject_user_id=None,
            idempotency_key="tenant-complete-0001",
        )
    async with operations_pool.acquire() as conn:
        agent = await conn.fetchrow(
            "SELECT status, current_draft_id FROM agents WHERE tenant_id = $1 AND agent_id = $2::uuid",
            tenant_id,
            agent_id,
        )
        assert agent["status"] == "deleted"
        assert agent["current_draft_id"] is None
        assert (
            await conn.fetchval(
                "SELECT status FROM agent_publications WHERE tenant_id = $1 AND publication_id = $2::uuid",
                tenant_id,
                publication_id,
            )
            == "disabled"
        )
        assert (
            await conn.fetchval(
                "SELECT COUNT(*) FROM agent_members WHERE tenant_id = $1 AND agent_id = $2::uuid",
                tenant_id,
                agent_id,
            )
            == 0
        )
        assert (
            await conn.fetchval(
                "SELECT spec->>'deleted' FROM agent_drafts WHERE tenant_id = $1 AND draft_id = $2::uuid",
                tenant_id,
                draft_id,
            )
            == "true"
        )
        assert (
            await conn.fetchval(
                "SELECT enabled FROM mcp_channel_grants WHERE tenant_id = $1 AND tool_id = $2::uuid",
                tenant_id,
                mcp_tool_id,
            )
            is False
        )
        assert (
            await conn.fetchval(
                "SELECT enabled FROM connector_credential_principals WHERE tenant_id = $1 AND grant_id = $2::uuid",
                tenant_id,
                connector_grant_id,
            )
            is False
        )
        assert (
            await conn.fetchval(
                "SELECT COUNT(*) FROM user_memory WHERE tenant_id = $1 AND user_id = $2",
                tenant_id,
                orphan_memory_id,
            )
            == 0
        )
        assert (
            await conn.fetchval(
                "SELECT COUNT(*) FROM agent_versions WHERE tenant_id = $1 AND agent_id = $2::uuid",
                tenant_id,
                agent_id,
            )
            == 1
        )


@pytest.mark.asyncio
async def test_user_deletion_removes_ephemeral_data_and_preserves_history(
    operations_pool: asyncpg.Pool,
) -> None:
    tenant_id, agent_id, publication_id, owner_id = await _seed_operations_agent(operations_pool)
    repository = DatabaseAgentRepository(_Holder(_SchemaIsolatedPool(operations_pool)))
    subject_id = "subject-user"
    session_id = str(uuid.uuid4())
    version_id = str(uuid.uuid4())
    run_id = str(uuid.uuid4())
    digest = hashlib.sha256(f"{subject_id}:{agent_id}:version:{version_id}".encode()).hexdigest()
    memory_id = f"agent-memory:{digest}"
    async with operations_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO sessions (
                session_id, tenant_id, user_id, agent_id, agent_version_id,
                publication_id, channel
            ) VALUES ($1, $2, $3, $4::uuid, $5::uuid, $6::uuid, 'api')
            """,
            session_id,
            tenant_id,
            subject_id,
            agent_id,
            version_id,
            publication_id,
        )
        await conn.execute(
            """
            INSERT INTO assistant_runs (run_id, tenant_id, user_id, agent_id)
            VALUES ($1::uuid, $2, $3, $4::uuid)
            """,
            run_id,
            tenant_id,
            subject_id,
            agent_id,
        )
        await conn.execute(
            """
            INSERT INTO assistant_run_checkpoints (
                checkpoint_id, tenant_id, user_id, agent_id
            ) VALUES (gen_random_uuid(), $1, $2, $3::uuid)
            """,
            tenant_id,
            subject_id,
            agent_id,
        )
        await conn.execute(
            """
            INSERT INTO session_memory (tenant_id, session_id, key)
            VALUES ($1, $2, 'agent/key')
            """,
            tenant_id,
            session_id,
        )
        await conn.execute(
            """
            INSERT INTO user_memory (tenant_id, user_id, key)
            VALUES ($1, $2, 'agent/fact')
            """,
            tenant_id,
            memory_id,
        )
        await conn.execute(
            """
            INSERT INTO assistant_memory_sources (source_id, tenant_id, user_id)
            VALUES (gen_random_uuid(), $1, $2)
            """,
            tenant_id,
            memory_id,
        )
        await conn.execute(
            """
            INSERT INTO assistant_memory_reflections (
                reflection_id, tenant_id, user_id
            ) VALUES (gen_random_uuid(), $1, $2)
            """,
            tenant_id,
            memory_id,
        )
        await conn.execute(
            """
            INSERT INTO agent_traces (
                trace_id, tenant_id, user_id, agent_id, agent_version_id,
                publication_id, channel
            ) VALUES (gen_random_uuid(), $1, $2, $3::uuid, $4::uuid, $5::uuid, 'api')
            """,
            tenant_id,
            subject_id,
            agent_id,
            version_id,
            publication_id,
        )
        await conn.execute(
            """
            INSERT INTO agent_runtime_attachments (
                tenant_id, attachment_id, publication_id, principal_id,
                channel, storage_key, expires_at
            ) VALUES ($1, gen_random_uuid(), $2::uuid, $3, 'api',
                      'tenant/object-key', NOW() + INTERVAL '1 day')
            """,
            tenant_id,
            publication_id,
            subject_id,
        )
        await conn.execute(
            """
            INSERT INTO agent_runtime_idempotency (
                tenant_id, publication_id, principal_id, idempotency_key
            ) VALUES ($1, $2::uuid, $3, 'request-key')
            """,
            tenant_id,
            publication_id,
            subject_id,
        )
        await conn.execute(
            """
            INSERT INTO agent_runtime_feedback (
                tenant_id, feedback_id, publication_id, principal_id
            ) VALUES ($1, gen_random_uuid(), $2::uuid, $3)
            """,
            tenant_id,
            publication_id,
            subject_id,
        )
        await conn.execute(
            """
            INSERT INTO agent_api_tokens (
                tenant_id, token_id, publication_id, created_by
            ) VALUES ($1, gen_random_uuid(), $2::uuid, $3)
            """,
            tenant_id,
            publication_id,
            subject_id,
        )
        await conn.execute(
            """
            INSERT INTO semantic_cache (metadata)
            VALUES (
                jsonb_build_object(
                    'tenant_id', $1::text,
                    'agent_id', $2::text,
                    'user_id', $3::text
                )
            ), (
                jsonb_build_object(
                    'tenant_id', $1::text,
                    'agent_id', $2::text,
                    'user_id', 'other-user'
                )
            )
            """,
            tenant_id,
            agent_id,
            subject_id,
        )
    prepared = await repository.prepare_agent_data_deletion(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        is_tenant_admin=False,
        scope="user",
        subject_user_id=subject_id,
        idempotency_key="user-delete-0001",
    )
    assert prepared["status"] == "pending"
    assert prepared["object_keys"] == ["tenant/object-key"]
    async with operations_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO semantic_cache (metadata)
            VALUES (
                jsonb_build_object(
                    'tenant_id', $1::text,
                    'agent_id', $2::text,
                    'user_id', $3::text
                )
            )
            """,
            tenant_id,
            agent_id,
            subject_id,
        )
    runtime_receipt = await _freeze_completed_runtime_cleanup(
        repository,
        prepared=prepared,
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
    )
    completed = await _finish_claimed_data_deletion(
        repository,
        prepared=prepared,
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=owner_id,
        storage_cleanup_succeeded=True,
        runtime_cleanup_receipt=runtime_receipt,
    )
    assert completed["status"] == "completed"
    assert completed["deleted_counts"]["sessions"] == 1
    assert completed["deleted_counts"]["traces"] == 1
    assert completed["deleted_counts"]["attachments"] == 1
    assert completed["deleted_counts"]["user_memory"] == 1
    assert completed["deleted_counts"]["semantic_cache"] == 1
    async with operations_pool.acquire() as conn:
        assert (
            await conn.fetchval(
                "SELECT COUNT(*) FROM agents WHERE tenant_id = $1 AND agent_id = $2::uuid",
                tenant_id,
                agent_id,
            )
            == 1
        )
        assert (
            await conn.fetchval(
                "SELECT COUNT(*) FROM agent_publications WHERE tenant_id = $1 AND agent_id = $2::uuid",
                tenant_id,
                agent_id,
            )
            == 1
        )
        assert (
            await conn.fetchval(
                "SELECT COUNT(*) FROM sessions WHERE tenant_id = $1 AND agent_id = $2::uuid",
                tenant_id,
                agent_id,
            )
            == 0
        )
        assert (
            await conn.fetchval(
                "SELECT revoked_at IS NOT NULL FROM agent_api_tokens WHERE tenant_id = $1",
                tenant_id,
            )
            is True
        )
        assert (
            await conn.fetchval(
                """
            SELECT COUNT(*) FROM semantic_cache
            WHERE metadata->>'tenant_id' = $1 AND metadata->>'agent_id' = $2
            """,
                tenant_id,
                agent_id,
            )
            == 2
        )
        assert (
            await conn.fetchval(
                """
            SELECT COUNT(*) FROM audit_logs
            WHERE tenant_id = $1 AND agent_id = $2::uuid
              AND action = 'data_deletion_completed'
            """,
                tenant_id,
                agent_id,
            )
            == 1
        )
