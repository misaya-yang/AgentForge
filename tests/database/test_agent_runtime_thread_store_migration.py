from __future__ import annotations

import asyncio
import hashlib
import json
import os
import uuid
from collections.abc import AsyncIterator
from datetime import datetime, timedelta, timezone
from pathlib import Path

import asyncpg
import pytest
import pytest_asyncio
from dotenv import dotenv_values

ROOT = Path(__file__).resolve().parents[2]
MIGRATION = ROOT / "database" / "migrations" / "089_agent_runtime_thread_store.sql"
MODEL_LEASE_MIGRATION = ROOT / "database" / "migrations" / "090_agent_runtime_model_leases.sql"
LEGACY_IMPORT_MIGRATION = ROOT / "database" / "migrations" / "092_agent_runtime_legacy_import.sql"
SESSION_FK_MIGRATION = (
    ROOT / "database" / "migrations" / "093_agent_runtime_assistant_session_fks.sql"
)
LEGACY_NORMALIZATION_MIGRATION = (
    ROOT / "database" / "migrations" / "094_agent_runtime_legacy_import_normalization.sql"
)
SINGLE_KERNEL_MIGRATION = ROOT / "database" / "migrations" / "095_agent_runtime_single_kernel.sql"


def _postgres_config() -> dict[str, object]:
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
async def agent_runtime_pool() -> AsyncIterator[asyncpg.Pool]:
    config = _postgres_config()
    schema_name = f"agent_runtime_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE SCHEMA "{schema_name}"')
    await admin.close()

    pool = await asyncpg.create_pool(
        **config,
        min_size=1,
        max_size=8,
        server_settings={"search_path": f'"{schema_name}",public'},
    )
    try:
        async with pool.acquire() as conn:
            await conn.execute(
                """
                CREATE TABLE sessions (
                    session_id VARCHAR(255) PRIMARY KEY,
                    service_id VARCHAR(255),
                    user_id VARCHAR(255),
                    tenant_id VARCHAR(255),
                    state JSONB NOT NULL DEFAULT '{}'::jsonb,
                    history JSONB NOT NULL DEFAULT '[]'::jsonb,
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    config JSONB NOT NULL DEFAULT '{}'::jsonb,
                    status VARCHAR(50) NOT NULL DEFAULT 'active',
                    expires_at TIMESTAMPTZ,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE assistant_runs (
                    run_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    session_id VARCHAR(255) NOT NULL,
                    status VARCHAR(32) NOT NULL DEFAULT 'running',
                    engine VARCHAR(32) NOT NULL DEFAULT 'agent_loop',
                    execution_profile VARCHAR(16) NOT NULL DEFAULT 'safe',
                    memory_mode VARCHAR(16) NOT NULL DEFAULT 'auto',
                    os_agent_enabled BOOLEAN NOT NULL DEFAULT FALSE,
                    request_preview TEXT NOT NULL DEFAULT '',
                    usage JSONB NOT NULL DEFAULT '{}'::jsonb,
                    error TEXT,
                    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    finished_at TIMESTAMPTZ,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE assistant_command_queue (
                    command_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    session_id VARCHAR(255) NOT NULL,
                    status VARCHAR(32) NOT NULL DEFAULT 'queued'
                );
                CREATE TABLE assistant_tool_approvals (
                    approval_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    user_id VARCHAR(255) NOT NULL,
                    session_id VARCHAR(255) NOT NULL,
                    run_id UUID,
                    tool_name VARCHAR(100) NOT NULL DEFAULT 'unknown',
                    arguments JSONB NOT NULL DEFAULT '{}'::jsonb,
                    status VARCHAR(32) NOT NULL DEFAULT 'pending',
                    reason TEXT,
                    approved_by VARCHAR(255),
                    approved_at TIMESTAMPTZ,
                    expires_at TIMESTAMPTZ,
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE OR REPLACE FUNCTION update_assistant_gateway_timestamp()
                RETURNS TRIGGER AS $$
                BEGIN
                    NEW.updated_at = NOW();
                    RETURN NEW;
                END;
                $$ LANGUAGE plpgsql;
                """
            )
            sql = MIGRATION.read_text(encoding="utf-8")
            await conn.execute(sql)
            await conn.execute(sql)
            lease_sql = MODEL_LEASE_MIGRATION.read_text(encoding="utf-8")
            await conn.execute(lease_sql)
            await conn.execute(lease_sql)
            import_sql = LEGACY_IMPORT_MIGRATION.read_text(encoding="utf-8")
            await conn.execute(import_sql)
            await conn.execute(import_sql)
            normalization_sql = LEGACY_NORMALIZATION_MIGRATION.read_text(encoding="utf-8")
            await conn.execute(normalization_sql)
            await conn.execute(normalization_sql)
            single_kernel_sql = SINGLE_KERNEL_MIGRATION.read_text(encoding="utf-8")
            await conn.execute(single_kernel_sql)
            await conn.execute(single_kernel_sql)
        yield pool
    finally:
        await pool.close()
        admin = await asyncpg.connect(**config)
        await admin.execute(f'DROP SCHEMA "{schema_name}" CASCADE')
        await admin.close()


def test_migration_is_additive_and_has_atomic_append_contract() -> None:
    sql = MIGRATION.read_text(encoding="utf-8")
    upper = sql.upper()
    assert "DROP TABLE" not in upper
    assert "TRUNCATE" not in upper
    assert "CREATE OR REPLACE FUNCTION APPEND_ASSISTANT_RUNTIME_ITEM" in upper
    assert "FOR UPDATE" in upper
    assert "ASSISTANT_RUNTIME_EVENT_KEY_CONFLICT" in upper
    assert "ASSISTANT_RUNTIME_THREAD_SCOPE_MISMATCH" in upper
    assert "ASSISTANT_RUNTIME_MEMBER_SCOPE_MISMATCH" in upper
    assert "ASSISTANT_RUNTIME_ASSIGNMENT_IMMUTABLE" in upper

    single_kernel_sql = SINGLE_KERNEL_MIGRATION.read_text(encoding="utf-8")
    single_kernel_upper = single_kernel_sql.upper()
    assert "RUNTIME_OWNER = 'AGENT_RUNTIME'" in single_kernel_upper
    assert "PYTHON_CONTROL" in single_kernel_upper
    assert "CODEX_CANDIDATE" in single_kernel_upper
    assert "P.PRONAMESPACE = TO_REGNAMESPACE(CURRENT_SCHEMA())" in single_kernel_upper
    assert "UPDATE ASSISTANT_RUNTIME_SNAPSHOTS" not in single_kernel_upper
    assert "UPDATE ASSISTANT_RUNTIME_ITEMS" not in single_kernel_upper

    lease_sql = MODEL_LEASE_MIGRATION.read_text(encoding="utf-8")
    lease_upper = lease_sql.upper()
    assert "DROP TABLE" not in lease_upper
    assert "TRUNCATE" not in lease_upper
    assert "CREATE OR REPLACE FUNCTION ISSUE_ASSISTANT_RUNTIME_TURN" in lease_upper
    assert "CREATE OR REPLACE FUNCTION RESERVE_ASSISTANT_RUNTIME_MODEL_CALL" in lease_upper
    assert "ASSISTANT_RUNTIME_MODEL_CALL_REPLAYED" in lease_upper
    assert "ASSISTANT_RUNTIME_LEASE_BUDGET_EXHAUSTED" in lease_upper

    import_sql = LEGACY_IMPORT_MIGRATION.read_text(encoding="utf-8")
    import_upper = import_sql.upper()
    assert "CREATE OR REPLACE FUNCTION IMPORT_ASSISTANT_LEGACY_SESSION" in import_upper
    assert "ASSISTANT_RUNTIME_IMPORT_IN_FLIGHT" in import_upper
    assert "ASSISTANT_RUNTIME_SOURCE_KIND_INVALID" in import_upper
    assert "DYNAMIC_TOOL_FINGERPRINT" in import_upper
    assert "FOR UPDATE" in import_upper

    session_fk_sql = SESSION_FK_MIGRATION.read_text(encoding="utf-8")
    session_fk_upper = session_fk_sql.upper()
    assert "REFERENCES ASSISTANT.SESSIONS" in session_fk_upper
    assert "GATEWAY.ASSISTANT_SESSION_RUNTIME_ASSIGNMENTS" in session_fk_upper
    assert "GATEWAY.ASSISTANT_RUNTIME_THREADS" in session_fk_upper
    assert "DROP TABLE" not in session_fk_upper
    assert "TRUNCATE" not in session_fk_upper

    normalization_sql = LEGACY_NORMALIZATION_MIGRATION.read_text(encoding="utf-8")
    normalization_upper = normalization_sql.upper()
    assert "CREATE OR REPLACE FUNCTION IMPORT_ASSISTANT_LEGACY_SESSION" in normalization_upper
    assert "TO_REGCLASS('ASSISTANT.SESSIONS')" in normalization_upper
    assert "SET SEARCH_PATH FROM CURRENT" in normalization_upper
    assert "ASSISTANT_RUNTIME_IMPORT_TOOL_PAIRING_INVALID" in normalization_upper
    assert "ASSISTANT_RUNTIME_IMPORT_SOURCE_CHANGED" in normalization_upper
    assert "''BLOCKED''" in normalization_upper
    assert "AGENT-RUNTIME-LEGACY-APPROVAL/V1" in normalization_upper
    assert "DROP TABLE" not in normalization_upper
    assert "TRUNCATE" not in normalization_upper


@pytest.mark.asyncio
async def test_session_runtime_assignment_is_scope_bound_and_immutable(
    agent_runtime_pool: asyncpg.Pool,
) -> None:
    tenant_id = "tenant-assignment"
    user_id = "user-assignment"
    session_id = f"session-{uuid.uuid4()}"
    async with agent_runtime_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO sessions (session_id, service_id, user_id, tenant_id)
            VALUES ($1, '__builtin_assistant__', $2, $3)
            """,
            session_id,
            user_id,
            tenant_id,
        )
        await conn.execute(
            """
            INSERT INTO assistant_session_runtime_assignments (
                tenant_id, user_id, session_id, runtime_owner, kernel_revision
            ) VALUES ($1, $2, $3, 'agent_runtime', 'kernel-1')
            """,
            tenant_id, user_id, session_id,
        )
        assignment = await conn.fetchrow(
            """
            SELECT runtime_owner, kernel_revision
            FROM assistant_session_runtime_assignments
            WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
            """,
            tenant_id, user_id, session_id,
        )
        assert dict(assignment) == {"runtime_owner": "agent_runtime", "kernel_revision": "kernel-1"}
        with pytest.raises(asyncpg.ObjectNotInPrerequisiteStateError):
            await conn.execute(
                """
                UPDATE assistant_session_runtime_assignments
                SET runtime_owner = 'agent_runtime', kernel_revision = 'fork-sha'
                WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
                """,
                tenant_id, user_id, session_id,
            )
        invalid_session_id = f"session-{uuid.uuid4()}"
        await conn.execute(
            """
            INSERT INTO sessions (session_id, service_id, user_id, tenant_id)
            VALUES ($1, '__builtin_assistant__', $2, $3)
            """,
            invalid_session_id, user_id, tenant_id,
        )
        with pytest.raises(asyncpg.CheckViolationError):
            await conn.execute(
                """
                INSERT INTO assistant_session_runtime_assignments (
                    tenant_id, user_id, session_id, runtime_owner
                ) VALUES ($1, $2, $3, 'python_control')
                """,
                tenant_id, user_id, invalid_session_id,
            )


@pytest.mark.asyncio
async def test_legacy_import_is_idempotent_and_cross_tenant_scoped(
    agent_runtime_pool: asyncpg.Pool,
) -> None:
    session_id = f"legacy-{uuid.uuid4()}"
    runtime_thread_id = uuid.uuid4()
    async with agent_runtime_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO sessions (session_id, service_id, user_id, tenant_id, history)
            VALUES ($1, '__builtin_assistant__', 'legacy-user', 'legacy-tenant', $2::jsonb)
            """,
            session_id,
            json.dumps([{"role": "user", "content": "hello"}, {"role": "assistant", "content": "hi"}]),
        )
        first = await conn.fetchrow(
            "SELECT * FROM import_assistant_legacy_session($1, $2, $3, $4)",
            runtime_thread_id, "legacy-tenant", "legacy-user", session_id,
        )
        second = await conn.fetchrow(
            "SELECT * FROM import_assistant_legacy_session($1, $2, $3, $4)",
            runtime_thread_id, "legacy-tenant", "legacy-user", session_id,
        )
        assert first["import_status"] == "ready"
        assert second["import_status"] == "ready"
        assert second["source_history_count"] == 2
        assert await conn.fetchval(
            "SELECT count(*) FROM assistant_runtime_items WHERE runtime_thread_id = $1",
            runtime_thread_id,
        ) == 2
        rollout_item = await conn.fetchrow(
            """
            SELECT event_type, payload
            FROM assistant_runtime_items
            WHERE runtime_thread_id = $1
            ORDER BY sequence
            LIMIT 1
            """,
            runtime_thread_id,
        )
        assert rollout_item["event_type"] == "rollout/item"
        rollout_payload = json.loads(rollout_item["payload"])
        assert rollout_payload["type"] == "response_item"
        assert rollout_payload["payload"]["type"] == "message"
        original_history = await conn.fetchval(
            "SELECT history FROM sessions WHERE session_id = $1", session_id
        )
        assert json.loads(original_history) == [
            {"role": "user", "content": "hello"},
            {"role": "assistant", "content": "hi"},
        ]
        with pytest.raises(asyncpg.InsufficientPrivilegeError):
            await conn.fetchrow(
                "SELECT * FROM import_assistant_legacy_session($1, $2, $3, $4)",
                uuid.uuid4(), "other-tenant", "legacy-user", session_id,
            )

    async def import_once() -> dict:
        async with agent_runtime_pool.acquire() as concurrent_conn:
            row = await concurrent_conn.fetchrow(
                "SELECT * FROM import_assistant_legacy_session($1, $2, $3, $4)",
                runtime_thread_id, "legacy-tenant", "legacy-user", session_id,
            )
            return dict(row)

    concurrent_results = await asyncio.gather(import_once(), import_once())
    assert [result["import_status"] for result in concurrent_results] == ["ready", "ready"]


@pytest.mark.asyncio
@pytest.mark.parametrize("run_status", ["running", "blocked"])
async def test_legacy_import_refuses_in_flight_run(
    agent_runtime_pool: asyncpg.Pool,
    run_status: str,
) -> None:
    session_id = f"legacy-running-{uuid.uuid4()}"
    async with agent_runtime_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO sessions (session_id, user_id, tenant_id, history)
            VALUES ($1, 'running-user', 'running-tenant', '[]'::jsonb)
            """,
            session_id,
        )
        await conn.execute(
            """
            INSERT INTO assistant_runs (run_id, tenant_id, user_id, session_id, status)
            VALUES ($1, 'running-tenant', 'running-user', $2, $3)
            """,
            uuid.uuid4(), session_id, run_status,
        )
        with pytest.raises(asyncpg.ObjectNotInPrerequisiteStateError):
            await conn.fetchrow(
                "SELECT * FROM import_assistant_legacy_session($1, $2, $3, $4)",
                uuid.uuid4(), "running-tenant", "running-user", session_id,
            )


@pytest.mark.asyncio
async def test_legacy_import_refuses_approved_unconsumed_approval(
    agent_runtime_pool: asyncpg.Pool,
) -> None:
    session_id = f"legacy-approved-{uuid.uuid4()}"
    async with agent_runtime_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO sessions (session_id, user_id, tenant_id, history)
            VALUES ($1, 'approval-user', 'approval-tenant', '[]'::jsonb)
            """,
            session_id,
        )
        await conn.execute(
            """
            INSERT INTO assistant_tool_approvals (
                approval_id, tenant_id, user_id, session_id, tool_name, status
            ) VALUES ($1, 'approval-tenant', 'approval-user', $2, 'write_tool', 'approved')
            """,
            uuid.uuid4(),
            session_id,
        )
        with pytest.raises(asyncpg.ObjectNotInPrerequisiteStateError):
            await conn.fetchrow(
                "SELECT * FROM import_assistant_legacy_session($1, $2, $3, $4)",
                uuid.uuid4(),
                "approval-tenant",
                "approval-user",
                session_id,
            )


@pytest.mark.asyncio
async def test_legacy_import_preserves_paired_tool_history_and_hashed_approval_receipt(
    agent_runtime_pool: asyncpg.Pool,
) -> None:
    session_id = f"legacy-tools-{uuid.uuid4()}"
    runtime_thread_id = uuid.uuid4()
    run_id = uuid.uuid4()
    approval_id = uuid.uuid4()
    history = [
        {"role": "user", "content": "look up the account"},
        {
            "role": "assistant",
            "content": "The account is active.",
            "metadata": {
                "tool_calls": [
                    {
                        "id": "call-1",
                        "name": "account_lookup",
                        "arguments": {"account_id": "acct-1"},
                        "status": "completed",
                    }
                ],
                "tool_results": [
                    {
                        "tool_call_id": "call-1",
                        "name": "account_lookup",
                        "result": {"active": True},
                        "error": None,
                    }
                ],
            },
        },
    ]
    async with agent_runtime_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO sessions (session_id, service_id, user_id, tenant_id, history)
            VALUES ($1, '__builtin_assistant__', 'legacy-user', 'legacy-tenant', $2::jsonb)
            """,
            session_id,
            json.dumps(history),
        )
        await conn.execute(
            """
            INSERT INTO assistant_tool_approvals (
                approval_id, tenant_id, user_id, session_id, run_id,
                tool_name, arguments, status, approved_by, approved_at
            ) VALUES (
                $1, 'legacy-tenant', 'legacy-user', $2, $3,
                'account_lookup', '{"account_id":"acct-1"}'::jsonb,
                'consumed', 'legacy-user', NOW()
            )
            """,
            approval_id,
            session_id,
            run_id,
        )

        imported = await conn.fetchrow(
            "SELECT * FROM import_assistant_legacy_session($1, $2, $3, $4)",
            runtime_thread_id,
            "legacy-tenant",
            "legacy-user",
            session_id,
        )
        assert imported["import_status"] == "ready"
        rollout_rows = await conn.fetch(
            """
            SELECT payload
              FROM assistant_runtime_items
             WHERE runtime_thread_id = $1 AND event_type = 'rollout/item'
             ORDER BY sequence
            """,
            runtime_thread_id,
        )
        payloads = [json.loads(row["payload"]) for row in rollout_rows]
        assert [payload["payload"]["type"] for payload in payloads] == [
            "message",
            "function_call",
            "function_call_output",
            "message",
        ]
        assert payloads[1]["payload"] == {
            "type": "function_call",
            "name": "account_lookup",
            "arguments": '{"account_id": "acct-1"}',
            "call_id": "call-1",
        }
        assert payloads[2]["payload"]["call_id"] == "call-1"
        assert json.loads(payloads[2]["payload"]["output"]) == {"active": True}

        approval = await conn.fetchrow(
            """
            SELECT payload
              FROM assistant_runtime_items
             WHERE runtime_thread_id = $1 AND event_type = 'agent-runtime/legacy_approval'
            """,
            runtime_thread_id,
        )
        approval_payload = json.loads(approval["payload"])
        assert approval_payload["approval_id"] == str(approval_id)
        assert approval_payload["arguments_sha256"]
        assert "arguments" not in approval_payload

        projection = await conn.fetchval(
            """
            SELECT projection->'legacy_import'
              FROM assistant_runtime_thread_projections
             WHERE runtime_thread_id = $1
            """,
            runtime_thread_id,
        )
        projection = json.loads(projection)
        assert projection["normalizer_version"] == 2
        assert projection["tool_call_count"] == 1
        assert projection["approval_receipt_count"] == 1
        assert json.loads(await conn.fetchval(
            "SELECT history FROM sessions WHERE session_id = $1", session_id
        )) == history


@pytest.mark.asyncio
async def test_ready_legacy_import_rejects_source_history_drift(
    agent_runtime_pool: asyncpg.Pool,
) -> None:
    session_id = f"legacy-drift-{uuid.uuid4()}"
    runtime_thread_id = uuid.uuid4()
    async with agent_runtime_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO sessions (session_id, user_id, tenant_id, history)
            VALUES ($1, 'legacy-user', 'legacy-tenant',
                    '[{"role":"user","content":"before"}]'::jsonb)
            """,
            session_id,
        )
        first = await conn.fetchrow(
            "SELECT * FROM import_assistant_legacy_session($1, $2, $3, $4)",
            runtime_thread_id,
            "legacy-tenant",
            "legacy-user",
            session_id,
        )
        await conn.execute(
            """
            UPDATE sessions
               SET history = history || '[{"role":"assistant","content":"after"}]'::jsonb
             WHERE session_id = $1
            """,
            session_id,
        )
        with pytest.raises(asyncpg.PostgresError, match="IMPORT_SOURCE_CHANGED"):
            await conn.fetchrow(
                "SELECT * FROM import_assistant_legacy_session($1, $2, $3, $4)",
                runtime_thread_id,
                "legacy-tenant",
                "legacy-user",
                session_id,
            )
        stored = await conn.fetchrow(
            """
            SELECT import_status, source_history_count, source_history_sha256
              FROM assistant_runtime_threads
             WHERE runtime_thread_id = $1
            """,
            runtime_thread_id,
        )
        assert stored["import_status"] == "ready"
        assert stored["source_history_count"] == 1
        assert stored["source_history_sha256"] == first["source_history_sha256"]
        assert await conn.fetchval(
            "SELECT count(*) FROM assistant_runtime_items WHERE runtime_thread_id = $1",
            runtime_thread_id,
        ) == 1


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("tool_calls", "tool_results"),
    [
        ([{"id": "orphan-call", "name": "lookup", "arguments": {}}], []),
        ([], [{"tool_call_id": "orphan-result", "name": "lookup", "result": "x"}]),
    ],
)
async def test_legacy_import_rejects_unpaired_tool_history_atomically(
    agent_runtime_pool: asyncpg.Pool,
    tool_calls: list[dict],
    tool_results: list[dict],
) -> None:
    session_id = f"legacy-unpaired-{uuid.uuid4()}"
    runtime_thread_id = uuid.uuid4()
    history = [
        {
            "role": "assistant",
            "content": "",
            "metadata": {"tool_calls": tool_calls, "tool_results": tool_results},
        }
    ]
    async with agent_runtime_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO sessions (session_id, user_id, tenant_id, history)
            VALUES ($1, 'legacy-user', 'legacy-tenant', $2::jsonb)
            """,
            session_id,
            json.dumps(history),
        )
        with pytest.raises(asyncpg.PostgresError, match="IMPORT_TOOL_PAIRING_INVALID"):
            await conn.fetchrow(
                "SELECT * FROM import_assistant_legacy_session($1, $2, $3, $4)",
                runtime_thread_id,
                "legacy-tenant",
                "legacy-user",
                session_id,
            )
        assert await conn.fetchval(
            "SELECT count(*) FROM assistant_runtime_threads WHERE runtime_thread_id = $1",
            runtime_thread_id,
        ) == 0
        assert await conn.fetchval(
            "SELECT count(*) FROM assistant_runtime_items WHERE runtime_thread_id = $1",
            runtime_thread_id,
        ) == 0


async def _seed_thread(pool: asyncpg.Pool) -> tuple[uuid.UUID, str, str, str]:
    thread_id = uuid.uuid4()
    tenant_id = "tenant-a"
    user_id = "user-a"
    session_id = f"session-{uuid.uuid4()}"
    async with pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO sessions (session_id, service_id, user_id, tenant_id)
            VALUES ($1, '__builtin_assistant__', $2, $3)
            """,
            session_id,
            user_id,
            tenant_id,
        )
        await conn.execute(
            """
            INSERT INTO assistant_runtime_threads (
                runtime_thread_id, tenant_id, user_id, session_id
            ) VALUES ($1, $2, $3, $4)
            """,
            thread_id,
            tenant_id,
            user_id,
            session_id,
        )
        await conn.execute(
            """
            INSERT INTO assistant_runtime_thread_members (
                kernel_thread_id, runtime_thread_id, kernel_session_id,
                relation_kind, tenant_id, user_id, session_id
            ) VALUES ($1, $1, $1, 'root', $2, $3, $4)
            """,
            thread_id,
            tenant_id,
            user_id,
            session_id,
        )
    return thread_id, tenant_id, user_id, session_id


async def _append(
    pool: asyncpg.Pool,
    *,
    thread_id: uuid.UUID,
    tenant_id: str,
    user_id: str,
    session_id: str,
    event_id: uuid.UUID,
    event_key: str,
    payload: dict[str, object],
) -> int:
    encoded = json.dumps(payload, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    payload_hash = hashlib.sha256(encoded.encode("utf-8")).hexdigest()
    async with pool.acquire() as conn:
        return int(
            await conn.fetchval(
                """
                SELECT append_assistant_runtime_item(
                    $1, $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11,
                    $12::jsonb, $13
                )
                """,
                thread_id,
                tenant_id,
                user_id,
                session_id,
                event_id,
                event_key,
                "turn-1",
                "item-1",
                "item/started",
                "agent_message",
                "in_progress",
                encoded,
                payload_hash,
            )
        )


@pytest.mark.asyncio
async def test_atomic_append_is_idempotent_gap_free_and_scope_bound(
    agent_runtime_pool: asyncpg.Pool,
) -> None:
    thread_id, tenant_id, user_id, session_id = await _seed_thread(agent_runtime_pool)
    event_id = uuid.uuid4()
    payload = {"text": "hello"}

    first = await _append(
        agent_runtime_pool,
        thread_id=thread_id,
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
        event_id=event_id,
        event_key="event-1",
        payload=payload,
    )
    replay = await _append(
        agent_runtime_pool,
        thread_id=thread_id,
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
        event_id=event_id,
        event_key="event-1",
        payload=payload,
    )
    assert first == replay == 1

    with pytest.raises(asyncpg.UniqueViolationError):
        await _append(
            agent_runtime_pool,
            thread_id=thread_id,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            event_id=uuid.uuid4(),
            event_key="event-1",
            payload={"text": "different"},
        )

    with pytest.raises(asyncpg.InsufficientPrivilegeError):
        await _append(
            agent_runtime_pool,
            thread_id=thread_id,
            tenant_id="tenant-b",
            user_id=user_id,
            session_id=session_id,
            event_id=uuid.uuid4(),
            event_key="cross-tenant",
            payload=payload,
        )

    concurrent = await asyncio.gather(
        *(
            _append(
                agent_runtime_pool,
                thread_id=thread_id,
                tenant_id=tenant_id,
                user_id=user_id,
                session_id=session_id,
                event_id=uuid.uuid4(),
                event_key=f"parallel-{index}",
                payload={"index": index},
            )
            for index in range(20)
        )
    )
    assert sorted(concurrent) == list(range(2, 22))

    async with agent_runtime_pool.acquire() as conn:
        sequences = await conn.fetch(
            """
            SELECT sequence FROM assistant_runtime_items
            WHERE runtime_thread_id = $1 ORDER BY sequence
            """,
            thread_id,
        )
        assert [row["sequence"] for row in sequences] == list(range(1, 22))
        assert (
            await conn.fetchval(
                "SELECT last_sequence FROM assistant_runtime_threads WHERE runtime_thread_id = $1",
                thread_id,
            )
            == 21
        )


@pytest.mark.asyncio
async def test_snapshots_items_and_run_identity_fail_closed(
    agent_runtime_pool: asyncpg.Pool,
) -> None:
    thread_id, tenant_id, user_id, session_id = await _seed_thread(agent_runtime_pool)
    run_id = uuid.uuid4()
    snapshot_id = uuid.uuid4()
    snapshot = {"schema_version": "agent-runtime-snapshot/v1", "permissions": {}}
    snapshot_text = json.dumps(snapshot, separators=(",", ":"), sort_keys=True)
    snapshot_hash = hashlib.sha256(snapshot_text.encode()).hexdigest()

    async with agent_runtime_pool.acquire() as conn:
        async with conn.transaction():
            await conn.execute(
                """
                INSERT INTO assistant_runtime_snapshots (
                    snapshot_id, runtime_thread_id, run_id, tenant_id, user_id,
                    session_id, schema_version, snapshot, snapshot_sha256
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9)
                """,
                snapshot_id,
                thread_id,
                run_id,
                tenant_id,
                user_id,
                session_id,
                "agent-runtime-snapshot/v1",
                snapshot_text,
                snapshot_hash,
            )
            await conn.execute(
                """
                INSERT INTO assistant_runs (
                    run_id, tenant_id, user_id, session_id, engine,
                    harness_thread_id, harness_turn_id, runtime_snapshot_id,
                    kernel_revision, capability_revision
                ) VALUES (
                    $1, $2, $3, $4, 'agent_runtime', $5, 'turn-1', $6, 'fork-sha', 1
                )
                """,
                run_id,
                tenant_id,
                user_id,
                session_id,
                thread_id,
                snapshot_id,
            )

        await conn.execute(
            """
            INSERT INTO assistant_runtime_snapshot_revocations (
                snapshot_id, tenant_id, user_id, session_id, reason_code, revoked_by
            ) VALUES ($1, $2, $3, $4, 'lease_revoked', 'test-operator')
            """,
            snapshot_id,
            tenant_id,
            user_id,
            session_id,
        )
        with pytest.raises(asyncpg.ObjectNotInPrerequisiteStateError):
            await conn.execute(
                """
                UPDATE assistant_runtime_snapshot_revocations
                SET reason_code = 'changed' WHERE snapshot_id = $1
                """,
                snapshot_id,
            )
        with pytest.raises(asyncpg.ObjectNotInPrerequisiteStateError):
            await conn.execute(
                "DELETE FROM assistant_runtime_snapshots WHERE snapshot_id = $1",
                snapshot_id,
            )

        with pytest.raises(asyncpg.CheckViolationError):
            await conn.execute(
                """
                INSERT INTO assistant_runs (
                    run_id, tenant_id, user_id, session_id, engine
                ) VALUES ($1, $2, $3, $4, 'agent_runtime')
                """,
                uuid.uuid4(),
                tenant_id,
                user_id,
                session_id,
            )

    event_id = uuid.uuid4()
    await _append(
        agent_runtime_pool,
        thread_id=thread_id,
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
        event_id=event_id,
        event_key="immutable-item",
        payload={"text": "immutable"},
    )
    async with agent_runtime_pool.acquire() as conn:
        with pytest.raises(asyncpg.ObjectNotInPrerequisiteStateError):
            await conn.execute(
                "UPDATE assistant_runtime_items SET status = 'completed' WHERE event_id = $1",
                event_id,
            )


@pytest.mark.asyncio
async def test_runtime_model_lease_is_atomic_bounded_and_replay_safe(
    agent_runtime_pool: asyncpg.Pool,
) -> None:
    thread_id, tenant_id, user_id, session_id = await _seed_thread(agent_runtime_pool)
    run_id = uuid.uuid4()
    snapshot_id = uuid.uuid4()
    lease_id = uuid.uuid4()
    snapshot = {
        "schema_version": "agent-runtime-snapshot/v1",
        "model": {"id": "model-a", "provider_id": "provider-a"},
        "capability_revision": 3,
    }
    snapshot_text = json.dumps(snapshot, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    snapshot_hash = hashlib.sha256(snapshot_text.encode()).hexdigest()
    expires_at = datetime.now(timezone.utc) + timedelta(minutes=5)

    async with agent_runtime_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO assistant_session_runtime_assignments (
                tenant_id, user_id, session_id, runtime_owner, kernel_revision
            ) VALUES ($1, $2, $3, 'agent_runtime', 'fork-sha')
            """,
            tenant_id,
            user_id,
            session_id,
        )
        await conn.execute(
            """
            SELECT issue_assistant_runtime_turn(
                $1, $2, $3, $4, $5, $6, $7, 'fork-sha',
                'agent-runtime-snapshot/v1', $8::jsonb, $9, 3, 'balanced',
                'agent-runtime-model-lease/v1', 'provider-a', 'model-a',
                'provider-revision-a', $10, 2, 1000, 500, 1000000, $11, 'hello'
            )
            """,
            snapshot_id,
            lease_id,
            run_id,
            thread_id,
            tenant_id,
            user_id,
            session_id,
            snapshot_text,
            snapshot_hash,
            "a" * 64,
            expires_at,
        )
        run = await conn.fetchrow(
            """
            SELECT engine, harness_thread_id, harness_turn_id, runtime_snapshot_id
              FROM assistant_runs WHERE run_id = $1
            """,
            run_id,
        )
        assert dict(run) == {
            "engine": "agent_runtime",
            "harness_thread_id": thread_id,
            "harness_turn_id": str(run_id),
            "runtime_snapshot_id": snapshot_id,
        }

        call_id = uuid.uuid4()
        await conn.execute(
            "SELECT reserve_assistant_runtime_model_call($1, $2, $3, 100, 200, 250000)",
            call_id,
            lease_id,
            "b" * 64,
        )
        with pytest.raises(asyncpg.ObjectNotInPrerequisiteStateError):
            await conn.execute(
                "SELECT reserve_assistant_runtime_model_call($1, $2, $3, 100, 200, 250000)",
                uuid.uuid4(),
                lease_id,
                "b" * 64,
            )
        await conn.execute(
            """
            UPDATE assistant_runtime_model_calls
               SET status = 'dispatched', dispatched_at = NOW(), updated_at = NOW()
             WHERE call_id = $1 AND status = 'reserved'
            """,
            call_id,
        )
        await conn.execute(
            "SELECT complete_assistant_runtime_model_call($1, 90, 50, 120000, 'provider-request')",
            call_id,
        )
        lease = await conn.fetchrow(
            """
            SELECT calls_reserved, calls_completed, used_input_tokens,
                   used_output_tokens, used_cost_microusd
              FROM assistant_runtime_model_leases WHERE lease_id = $1
            """,
            lease_id,
        )
        assert dict(lease) == {
            "calls_reserved": 1,
            "calls_completed": 1,
            "used_input_tokens": 90,
            "used_output_tokens": 50,
            "used_cost_microusd": 120000,
        }

        with pytest.raises(asyncpg.InsufficientPrivilegeError):
            await conn.execute(
                """
                SELECT issue_assistant_runtime_turn(
                    $1, $2, $3, $4, 'tenant-b', $5, $6, 'fork-sha',
                    'agent-runtime-snapshot/v1', '{}'::jsonb, $7, 1, 'auto',
                    'agent-runtime-model-lease/v1', 'provider-a', 'model-a',
                    'provider-revision-a', $8, 1, 100, 100, 1000, $9, 'cross-tenant'
                )
                """,
                uuid.uuid4(),
                uuid.uuid4(),
                uuid.uuid4(),
                thread_id,
                user_id,
                session_id,
                hashlib.sha256(b"{}").hexdigest(),
                "c" * 64,
                expires_at,
            )
