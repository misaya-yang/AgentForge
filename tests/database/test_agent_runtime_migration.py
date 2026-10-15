from __future__ import annotations

import hashlib
import uuid
from collections.abc import AsyncIterator
from pathlib import Path

import asyncpg
import pytest
import pytest_asyncio
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentRuntimeUnavailableError,
    DatabaseAgentRepository,
)

from tests.database.test_agent_studio_migrations import (
    _Holder,
    _insert_graph,
    _postgres_config,
)

ROOT = Path(__file__).resolve().parents[2]
DOMAIN_MIGRATION = ROOT / "database" / "migrations" / "071_agent_studio_domain.sql"
RUNTIME_MIGRATION = ROOT / "database" / "migrations" / "072_agent_runtime_dimensions.sql"
IDENTITY_MIGRATION = (
    ROOT / "database" / "migrations" / "073_agent_runtime_identity_constraints.sql"
)
RUNTIME_TABLES = (
    "sessions",
    "assistant_runs",
    "assistant_run_checkpoints",
    "agent_traces",
)


@pytest_asyncio.fixture
async def agent_runtime_pool() -> AsyncIterator[asyncpg.Pool]:
    config = _postgres_config()
    schema_name = f"agent_runtime_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE SCHEMA "{schema_name}"')
    await admin.close()

    pool = await asyncpg.create_pool(
        **config,
        min_size=1,
        max_size=2,
        server_settings={"search_path": f'"{schema_name}",public'},
    )
    try:
        async with pool.acquire() as conn:
            await conn.execute(
                """
                CREATE TABLE datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL DEFAULT '',
                    name VARCHAR(255) NOT NULL DEFAULT ''
                );
                CREATE TABLE users (
                    user_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    status VARCHAR(50) NOT NULL DEFAULT 'active'
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
                CREATE TABLE sessions (
                    session_id VARCHAR(255) PRIMARY KEY,
                    service_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    tenant_id VARCHAR(255) NOT NULL,
                    state JSONB NOT NULL DEFAULT '{}'::jsonb,
                    history JSONB NOT NULL DEFAULT '[]'::jsonb,
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    config JSONB NOT NULL DEFAULT '{}'::jsonb,
                    status VARCHAR(32) NOT NULL DEFAULT 'active',
                    expires_at TIMESTAMPTZ,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE assistant_runs (
                    run_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    session_id VARCHAR(255) NOT NULL,
                    status VARCHAR(32) NOT NULL,
                    engine VARCHAR(64) NOT NULL,
                    execution_profile VARCHAR(32) NOT NULL,
                    memory_mode VARCHAR(32) NOT NULL,
                    os_agent_enabled BOOLEAN NOT NULL DEFAULT FALSE,
                    request_preview TEXT NOT NULL DEFAULT '',
                    usage JSONB NOT NULL DEFAULT '{}'::jsonb,
                    error TEXT,
                    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    finished_at TIMESTAMPTZ,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE assistant_run_checkpoints (
                    checkpoint_id UUID PRIMARY KEY,
                    run_id UUID NOT NULL,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    session_id VARCHAR(255) NOT NULL,
                    phase VARCHAR(64) NOT NULL,
                    iteration INTEGER NOT NULL DEFAULT 0,
                    message_state_hash TEXT NOT NULL DEFAULT '',
                    pending_tool JSONB NOT NULL DEFAULT '{}'::jsonb,
                    approval_id UUID,
                    idempotency_keys JSONB NOT NULL DEFAULT '{}'::jsonb,
                    resume_payload JSONB NOT NULL DEFAULT '{}'::jsonb,
                    status VARCHAR(32) NOT NULL DEFAULT 'running',
                    error TEXT,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE agent_traces (
                    trace_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    session_id VARCHAR(255) NOT NULL,
                    run_id UUID,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                INSERT INTO sessions (
                    session_id, service_id, user_id, tenant_id
                ) VALUES ('legacy-session', '__builtin_assistant__', 'legacy-user', 'legacy-tenant');
                INSERT INTO assistant_runs (
                    run_id, tenant_id, user_id, session_id, status, engine,
                    execution_profile, memory_mode
                ) VALUES (
                    'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'legacy-tenant',
                    'legacy-user', 'legacy-session', 'succeeded', 'agent_loop',
                    'safe', 'auto'
                );
                INSERT INTO assistant_run_checkpoints (
                    checkpoint_id, run_id, tenant_id, user_id, session_id, phase
                ) VALUES (
                    'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
                    'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'legacy-tenant',
                    'legacy-user', 'legacy-session', 'run_finished'
                );
                INSERT INTO agent_traces (
                    trace_id, tenant_id, user_id, session_id, run_id
                ) VALUES (
                    'cccccccc-cccc-4ccc-8ccc-cccccccccccc', 'legacy-tenant',
                    'legacy-user', 'legacy-session',
                    'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'
                );
                """
            )
            await conn.execute(DOMAIN_MIGRATION.read_text(encoding="utf-8"))
            runtime_sql = RUNTIME_MIGRATION.read_text(encoding="utf-8")
            await conn.execute(runtime_sql)
            await conn.execute(runtime_sql)
            identity_sql = IDENTITY_MIGRATION.read_text(encoding="utf-8")
            await conn.execute(identity_sql)
            await conn.execute(identity_sql)
        yield pool
    finally:
        await pool.close()
        admin = await asyncpg.connect(**config)
        await admin.execute(f'DROP SCHEMA "{schema_name}" CASCADE')
        await admin.close()


@pytest.mark.asyncio
async def test_runtime_migration_is_additive_idempotent_and_preserves_builtin_rows(
    agent_runtime_pool: asyncpg.Pool,
) -> None:
    sql = RUNTIME_MIGRATION.read_text(encoding="utf-8").upper()
    identity_sql = IDENTITY_MIGRATION.read_text(encoding="utf-8").upper()
    assert "DROP TABLE" not in sql
    assert "TRUNCATE" not in sql
    assert "DROP TABLE" not in identity_sql
    assert "TRUNCATE" not in identity_sql

    async with agent_runtime_pool.acquire() as conn:
        columns = await conn.fetch(
            """
            SELECT table_name, column_name
            FROM information_schema.columns
            WHERE table_schema = current_schema()
              AND table_name = ANY($1::text[])
              AND column_name = ANY($2::text[])
            """,
            list(RUNTIME_TABLES),
            [
                "agent_id",
                "agent_version_id",
                "agent_draft_revision",
                "publication_id",
                "channel",
                "runtime_fingerprint",
                "agent_spec_hash",
            ],
        )
        assert len(columns) == len(RUNTIME_TABLES) * 7
        for table in RUNTIME_TABLES:
            row = await conn.fetchrow(f"SELECT * FROM {table} LIMIT 1")
            assert row is not None
            assert all(
                row[key] is None
                for key in (
                    "agent_id",
                    "agent_version_id",
                    "agent_draft_revision",
                    "publication_id",
                    "channel",
                    "runtime_fingerprint",
                    "agent_spec_hash",
                )
            )


@pytest.mark.asyncio
async def test_runtime_dimensions_enforce_tenant_shape_and_immutable_revocation(
    agent_runtime_pool: asyncpg.Pool,
) -> None:
    async with agent_runtime_pool.acquire() as conn:
        graph = await _insert_graph(conn, "tenant-runtime-a", uuid.uuid4().hex[:8])
        await conn.execute(
            "UPDATE agent_publications SET status = 'active' "
            "WHERE tenant_id = $1 AND publication_id = $2",
            graph["tenant_id"],
            graph["publication_id"],
        )
        runtime_values = (
            graph["tenant_id"],
            graph["agent_id"],
            graph["version_id"],
            graph["publication_id"],
            "api",
            "sha256:runtime",
            "sha256:spec",
        )
        await conn.execute(
            """
            INSERT INTO sessions (
                session_id, service_id, user_id, tenant_id, agent_id,
                agent_version_id, publication_id, channel,
                runtime_fingerprint, agent_spec_hash
            ) VALUES (
                'agent-session', '__builtin_assistant__', $8,
                $1, $2, $3, $4, $5, $6, $7
            )
            """,
            *runtime_values,
            graph["owner_id"],
        )
        row = await conn.fetchrow(
            "SELECT * FROM sessions WHERE session_id = 'agent-session'"
        )
        assert row is not None
        assert row["agent_id"] == graph["agent_id"]
        assert row["agent_version_id"] == graph["version_id"]

        transaction = conn.transaction()
        await transaction.start()
        try:
            with pytest.raises(asyncpg.ForeignKeyViolationError):
                await conn.execute(
                    """
                    INSERT INTO sessions (
                        session_id, service_id, user_id, tenant_id, agent_id,
                        agent_version_id, publication_id, channel,
                        runtime_fingerprint, agent_spec_hash
                    ) VALUES (
                        'cross-tenant-session', '__builtin_assistant__', 'attacker',
                        'tenant-runtime-b', $1, $2, $3, 'api',
                        'sha256:runtime', 'sha256:spec'
                    )
                    """,
                    graph["agent_id"],
                    graph["version_id"],
                    graph["publication_id"],
                )
                await conn.execute("SET CONSTRAINTS ALL IMMEDIATE")
        finally:
            await transaction.rollback()

        await conn.execute(
            """
            INSERT INTO agent_version_revocations (
                tenant_id, agent_version_id, revoked_by, reason
            ) VALUES ($1, $2, $3, 'test revocation')
            """,
            graph["tenant_id"],
            graph["version_id"],
            graph["owner_id"],
        )
        with pytest.raises(asyncpg.PostgresError, match="AGENT_VERSION_REVOCATION_IMMUTABLE"):
            await conn.execute(
                "UPDATE agent_version_revocations SET reason = 'forged' "
                "WHERE tenant_id = $1 AND agent_version_id = $2",
                graph["tenant_id"],
                graph["version_id"],
            )


@pytest.mark.asyncio
async def test_runtime_identity_rejects_incomplete_or_cross_agent_rows(
    agent_runtime_pool: asyncpg.Pool,
) -> None:
    async with agent_runtime_pool.acquire() as conn:
        graph_a = await _insert_graph(conn, "tenant-runtime-shape", uuid.uuid4().hex[:8])
        graph_b = await _insert_graph(conn, "tenant-runtime-shape", uuid.uuid4().hex[:8])
        for graph in (graph_a, graph_b):
            await conn.execute(
                "UPDATE agent_publications SET status = 'active' "
                "WHERE tenant_id = $1 AND publication_id = $2",
                graph["tenant_id"],
                graph["publication_id"],
            )

        transaction = conn.transaction()
        await transaction.start()
        try:
            with pytest.raises(asyncpg.CheckViolationError):
                await conn.execute(
                    """
                    INSERT INTO assistant_runs (
                        run_id, tenant_id, user_id, session_id, status, engine,
                        execution_profile, memory_mode, agent_id,
                        agent_version_id, agent_draft_revision, publication_id,
                        channel, runtime_fingerprint, agent_spec_hash
                    ) VALUES (
                        $1, $2, $3, 'session-incomplete', 'running', 'agent_loop',
                        'safe', 'strict', $4, $5, 999, NULL, 'api',
                        'sha256:runtime', 'sha256:spec'
                    )
                    """,
                    uuid.uuid4(),
                    graph_a["tenant_id"],
                    graph_a["owner_id"],
                    graph_a["agent_id"],
                    graph_a["version_id"],
                )
        finally:
            await transaction.rollback()

        transaction = conn.transaction()
        await transaction.start()
        try:
            with pytest.raises(asyncpg.CheckViolationError):
                await conn.execute(
                    """
                    INSERT INTO sessions (
                        session_id, service_id, user_id, tenant_id, agent_id,
                        agent_draft_revision, channel, runtime_fingerprint,
                        agent_spec_hash
                    ) VALUES (
                        'preview-zero', '__builtin_assistant__', $1, $2, $3,
                        0, 'preview', 'sha256:runtime', 'sha256:spec'
                    )
                    """,
                    graph_a["owner_id"],
                    graph_a["tenant_id"],
                    graph_a["agent_id"],
                )
        finally:
            await transaction.rollback()

        transaction = conn.transaction()
        await transaction.start()
        try:
            with pytest.raises(asyncpg.ForeignKeyViolationError):
                await conn.execute(
                    """
                    INSERT INTO sessions (
                        session_id, service_id, user_id, tenant_id, agent_id,
                        agent_version_id, publication_id, channel,
                        runtime_fingerprint, agent_spec_hash
                    ) VALUES (
                        'cross-agent-version', '__builtin_assistant__', $1, $2,
                        $3, $4, $5, 'api', 'sha256:runtime', 'sha256:spec'
                    )
                    """,
                    graph_a["owner_id"],
                    graph_a["tenant_id"],
                    graph_a["agent_id"],
                    graph_b["version_id"],
                    graph_a["publication_id"],
                )
                await conn.execute("SET CONSTRAINTS ALL IMMEDIATE")
        finally:
            await transaction.rollback()

        transaction = conn.transaction()
        await transaction.start()
        try:
            with pytest.raises(asyncpg.ForeignKeyViolationError):
                await conn.execute(
                    """
                    INSERT INTO sessions (
                        session_id, service_id, user_id, tenant_id, agent_id,
                        agent_version_id, publication_id, channel,
                        runtime_fingerprint, agent_spec_hash
                    ) VALUES (
                        'cross-agent-publication', '__builtin_assistant__', $1, $2,
                        $3, $4, $5, 'api', 'sha256:runtime', 'sha256:spec'
                    )
                    """,
                    graph_a["owner_id"],
                    graph_a["tenant_id"],
                    graph_a["agent_id"],
                    graph_a["version_id"],
                    graph_b["publication_id"],
                )
                await conn.execute("SET CONSTRAINTS ALL IMMEDIATE")
        finally:
            await transaction.rollback()


@pytest.mark.asyncio
async def test_checkpoint_session_must_match_persisted_run_scope(
    agent_runtime_pool: asyncpg.Pool,
) -> None:
    async with agent_runtime_pool.acquire() as conn:
        graph = await _insert_graph(conn, "tenant-resume-scope", uuid.uuid4().hex[:8])
        await conn.execute(
            "UPDATE agent_publications SET status = 'active' "
            "WHERE tenant_id = $1 AND publication_id = $2",
            graph["tenant_id"],
            graph["publication_id"],
        )
        run_id = uuid.uuid4()
        await conn.execute(
            """
            INSERT INTO assistant_runs (
                run_id, tenant_id, user_id, session_id, status, engine,
                execution_profile, memory_mode, agent_id, agent_version_id,
                publication_id, channel, runtime_fingerprint, agent_spec_hash
            ) VALUES (
                $1, $2, $3, 'session-a', 'running', 'agent_loop', 'safe',
                'strict', $4, $5, $6, 'api', 'sha256:runtime', 'sha256:spec'
            )
            """,
            run_id,
            graph["tenant_id"],
            graph["owner_id"],
            graph["agent_id"],
            graph["version_id"],
            graph["publication_id"],
        )

        transaction = conn.transaction()
        await transaction.start()
        try:
            with pytest.raises(asyncpg.ForeignKeyViolationError):
                await conn.execute(
                    """
                    INSERT INTO assistant_run_checkpoints (
                        checkpoint_id, run_id, tenant_id, user_id, session_id,
                        phase, agent_id, agent_version_id, publication_id,
                        channel, runtime_fingerprint, agent_spec_hash
                    ) VALUES (
                        $1, $2, $3, $4, 'session-b', 'tool_call_pending',
                        $5, $6, $7, 'api', 'sha256:runtime', 'sha256:spec'
                    )
                    """,
                    uuid.uuid4(),
                    run_id,
                    graph["tenant_id"],
                    graph["owner_id"],
                    graph["agent_id"],
                    graph["version_id"],
                    graph["publication_id"],
                )
                await conn.execute("SET CONSTRAINTS ALL IMMEDIATE")
        finally:
            await transaction.rollback()


@pytest.mark.asyncio
async def test_publication_resolution_pins_version_and_rejects_revocation(
    agent_runtime_pool: asyncpg.Pool,
) -> None:
    repository = DatabaseAgentRepository(_Holder(agent_runtime_pool))
    async with agent_runtime_pool.acquire() as conn:
        graph = await _insert_graph(conn, "tenant-pin", uuid.uuid4().hex[:8])
        await conn.execute(
            "UPDATE agent_publications SET status = 'active' "
            "WHERE tenant_id = $1 AND publication_id = $2",
            graph["tenant_id"],
            graph["publication_id"],
        )

    first = await repository.resolve_publication_runtime(
        tenant_id=graph["tenant_id"],
        publication_id=str(graph["publication_id"]),
        user_id=graph["owner_id"],
        is_tenant_admin=False,
    )
    assert first["version"]["agent_version_id"] == str(graph["version_id"])

    second_version = uuid.uuid4()
    second_hash = hashlib.sha256(second_version.bytes).hexdigest()
    async with agent_runtime_pool.acquire() as conn, conn.transaction():
        await conn.execute(
            """
            INSERT INTO agent_versions (
                tenant_id, agent_version_id, agent_id, version_number,
                schema_version, resolved_spec, spec_hash, source_draft_id,
                source_draft_revision, created_by
            )
            SELECT tenant_id, $3, agent_id, 2, schema_version, resolved_spec,
                   $4, source_draft_id, source_draft_revision, $5
            FROM agent_versions
            WHERE tenant_id = $1 AND agent_version_id = $2
            """,
            graph["tenant_id"],
            graph["version_id"],
            second_version,
            second_hash,
            graph["owner_id"],
        )
        await conn.execute(
            "UPDATE agent_versions SET bindings_sealed = TRUE "
            "WHERE tenant_id = $1 AND agent_version_id = $2",
            graph["tenant_id"],
            second_version,
        )
        await conn.execute(
            "UPDATE agent_publications SET version_id = $3 "
            "WHERE tenant_id = $1 AND publication_id = $2",
            graph["tenant_id"],
            graph["publication_id"],
            second_version,
        )

    pinned = await repository.resolve_publication_runtime(
        tenant_id=graph["tenant_id"],
        publication_id=str(graph["publication_id"]),
        user_id=graph["owner_id"],
        is_tenant_admin=False,
        pinned_version_id=str(graph["version_id"]),
    )
    current = await repository.resolve_publication_runtime(
        tenant_id=graph["tenant_id"],
        publication_id=str(graph["publication_id"]),
        user_id=graph["owner_id"],
        is_tenant_admin=False,
    )
    assert pinned["version"]["agent_version_id"] == str(graph["version_id"])
    assert current["version"]["agent_version_id"] == str(second_version)

    async with agent_runtime_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO agent_version_revocations (
                tenant_id, agent_version_id, revoked_by, reason
            ) VALUES ($1, $2, $3, 'runtime revoked')
            """,
            graph["tenant_id"],
            graph["version_id"],
            graph["owner_id"],
        )
    with pytest.raises(AgentRuntimeUnavailableError, match="AGENT_VERSION_REVOKED"):
        await repository.resolve_publication_runtime(
            tenant_id=graph["tenant_id"],
            publication_id=str(graph["publication_id"]),
            user_id=graph["owner_id"],
            is_tenant_admin=False,
            pinned_version_id=str(graph["version_id"]),
        )
