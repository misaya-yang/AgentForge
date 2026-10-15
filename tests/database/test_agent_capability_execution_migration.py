from __future__ import annotations

import asyncio
import hashlib
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
MIGRATION = ROOT / "database" / "migrations" / "096_agent_capability_executions.sql"


def _config() -> dict[str, object]:
    values = dotenv_values(os.environ.get("ENV_FILE") or ROOT / ".env")
    keys = ("POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB", "POSTGRES_PORT")
    missing = [key for key in keys if not (os.environ.get(key) or values.get(key))]
    if missing:
        pytest.fail(f"PostgreSQL configuration missing: {', '.join(missing)}")
    return {
        "host": "127.0.0.1",
        "port": int(os.environ.get("POSTGRES_PORT") or values["POSTGRES_PORT"]),
        "user": os.environ.get("POSTGRES_USER") or values["POSTGRES_USER"],
        "password": os.environ.get("POSTGRES_PASSWORD") or values["POSTGRES_PASSWORD"],
        "database": os.environ.get("POSTGRES_DB") or values["POSTGRES_DB"],
    }


@pytest_asyncio.fixture
async def pool() -> AsyncIterator[asyncpg.Pool]:
    config = _config()
    schema = f"capability_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE SCHEMA "{schema}"')
    await admin.close()
    database = await asyncpg.create_pool(
        **config,
        min_size=1,
        max_size=2,
        server_settings={"search_path": f'"{schema}",public'},
    )
    try:
        async with database.acquire() as connection:
            await connection.execute(
                """
                CREATE TABLE assistant_runs (
                    run_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(64) NOT NULL,
                    user_id VARCHAR(64) NOT NULL,
                    session_id VARCHAR(100) NOT NULL,
                    status VARCHAR(32) NOT NULL DEFAULT 'running',
                    engine VARCHAR(32) NOT NULL DEFAULT 'agent_runtime',
                    UNIQUE (run_id, tenant_id, user_id, session_id)
                );
                CREATE TABLE assistant_runtime_snapshots (
                    snapshot_id UUID PRIMARY KEY,
                    run_id UUID NOT NULL,
                    tenant_id VARCHAR(64) NOT NULL,
                    user_id VARCHAR(64) NOT NULL,
                    session_id VARCHAR(100) NOT NULL,
                    capability_revision BIGINT NOT NULL DEFAULT 1,
                    snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
                    expires_at TIMESTAMPTZ
                );
                CREATE TABLE assistant_runtime_model_leases (
                    lease_id UUID PRIMARY KEY,
                    snapshot_id UUID NOT NULL,
                    run_id UUID NOT NULL,
                    tenant_id VARCHAR(64) NOT NULL,
                    user_id VARCHAR(64) NOT NULL,
                    session_id VARCHAR(100) NOT NULL,
                    capability_revision BIGINT NOT NULL DEFAULT 1,
                    status VARCHAR(32) NOT NULL DEFAULT 'active',
                    expires_at TIMESTAMPTZ NOT NULL
                );
                CREATE TABLE assistant_runtime_snapshot_revocations (
                    snapshot_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(64) NOT NULL,
                    user_id VARCHAR(64) NOT NULL,
                    session_id VARCHAR(100) NOT NULL
                );
                CREATE TABLE assistant_tool_approvals (
                    approval_id UUID PRIMARY KEY,
                    tenant_id VARCHAR(64) NOT NULL,
                    user_id VARCHAR(64) NOT NULL,
                    session_id VARCHAR(100) NOT NULL,
                    run_id UUID,
                    tool_name VARCHAR(160) NOT NULL,
                    arguments JSONB NOT NULL DEFAULT '{}'::jsonb,
                    status VARCHAR(32) NOT NULL DEFAULT 'pending',
                    expires_at TIMESTAMPTZ,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                """
            )
            sql = MIGRATION.read_text(encoding="utf-8")
            await connection.execute(sql)
            await connection.execute(sql)
        yield database
    finally:
        await database.close()
        admin = await asyncpg.connect(**config)
        await admin.execute(f'DROP SCHEMA "{schema}" CASCADE')
        await admin.close()


def _arguments_hash(arguments: dict[str, Any]) -> str:
    encoded = json.dumps(arguments, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


async def _runtime_binding(
    connection: asyncpg.Connection,
    run_id: uuid.UUID,
    *,
    schema_hash: str,
    tenant_id: str = "tenant-a",
    user_id: str = "user-a",
    session_id: str = "session-a",
    capability_type: str = "tool",
    name: str = "platform.echo",
    capability_id: str = "platform.echo",
    version: str = "1",
) -> asyncpg.Record | None:
    return await connection.fetchrow(
        """
        SELECT s.snapshot_id,
               ARRAY(
                 SELECT DISTINCT item->'payload'->>'dataset_id'
                   FROM jsonb_array_elements(
                     CASE WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,items}')='array'
                          THEN s.snapshot #> '{readonly_capabilities,items}'
                          ELSE '[]'::jsonb END
                   ) AS item
                  WHERE item->>'kind'='knowledge'
                    AND item->>'tenant_id'=s.tenant_id
                    AND item->>'capability_revision'=s.capability_revision::text
               ) AS bound_dataset_ids
          FROM assistant_runtime_snapshots AS s
          JOIN assistant_runtime_model_leases AS l
            ON l.snapshot_id=s.snapshot_id AND l.run_id=s.run_id
           AND l.tenant_id=s.tenant_id AND l.user_id=s.user_id
           AND l.session_id=s.session_id
           AND l.capability_revision=s.capability_revision
          JOIN assistant_runs AS r
            ON r.run_id=s.run_id AND r.tenant_id=s.tenant_id
           AND r.user_id=s.user_id AND r.session_id=s.session_id
         WHERE s.run_id=$1 AND s.tenant_id=$2 AND s.user_id=$3 AND s.session_id=$4
           AND r.status='running' AND r.engine='agent_runtime'
           AND l.status='active' AND l.expires_at>NOW()
           AND jsonb_typeof(s.snapshot #> '{readonly_capabilities,capability_allowlist}')='array'
           AND EXISTS (
             SELECT 1
               FROM jsonb_array_elements(s.snapshot #> '{readonly_capabilities,capability_allowlist}') AS capability
              WHERE capability->>'type'=$5 AND capability->>'name'=$6
                AND capability->>'id'=$7 AND COALESCE(capability->>'version','')=$8
                AND capability->>'schema_hash'=$9
           )
           AND NOT EXISTS (
             SELECT 1 FROM assistant_runtime_snapshot_revocations AS rev
              WHERE rev.snapshot_id=s.snapshot_id AND rev.tenant_id=s.tenant_id
                AND rev.user_id=s.user_id AND rev.session_id=s.session_id
           )
        """,
        run_id,
        tenant_id,
        user_id,
        session_id,
        capability_type,
        name,
        capability_id,
        version,
        schema_hash,
    )


async def _create_run(
    connection: asyncpg.Connection,
    *,
    tenant_id: str = "tenant-a",
    user_id: str = "user-a",
    session_id: str = "session-a",
) -> uuid.UUID:
    run_id = uuid.uuid4()
    await connection.execute(
        "INSERT INTO assistant_runs(run_id,tenant_id,user_id,session_id) VALUES($1,$2,$3,$4)",
        run_id,
        tenant_id,
        user_id,
        session_id,
    )
    snapshot_id = uuid.uuid4()
    snapshot = {
        # This object is intentionally not authoritative.  The worker must
        # only read the readonly capability projection below.
        "capabilities": {"platform.echo": {"version": "wrong"}},
        "readonly_capabilities": {
            "capability_allowlist": [
                {
                    "type": "tool",
                    "name": "platform.echo",
                    "id": "platform.echo",
                    "version": "1",
                    "schema_hash": "sha256:"
                    + hashlib.sha256(
                        json.dumps(
                            {"type": "object", "additionalProperties": True},
                            sort_keys=True,
                            separators=(",", ":"),
                        ).encode()
                    ).hexdigest(),
                }
            ],
            "items": [
                {
                    "kind": "knowledge",
                    "tenant_id": tenant_id,
                    "capability_revision": 1,
                    "payload": {"dataset_id": "dataset-a"},
                }
            ],
        },
    }
    await connection.execute(
        "INSERT INTO assistant_runtime_snapshots("
        "snapshot_id,run_id,tenant_id,user_id,session_id,capability_revision,snapshot) "
        "VALUES($1,$2,$3,$4,$5,1,$6::jsonb)",
        snapshot_id,
        run_id,
        tenant_id,
        user_id,
        session_id,
        json.dumps(snapshot),
    )
    await connection.execute(
        "INSERT INTO assistant_runtime_model_leases("
        "lease_id,snapshot_id,run_id,tenant_id,user_id,session_id,capability_revision,expires_at) "
        "VALUES($1,$2,$3,$4,$5,$6,1,NOW()+INTERVAL '5 minutes')",
        uuid.uuid4(),
        snapshot_id,
        run_id,
        tenant_id,
        user_id,
        session_id,
    )
    return run_id


async def _reserve(
    connection: asyncpg.Connection,
    *,
    run_id: uuid.UUID,
    execution_id: uuid.UUID | None = None,
    lease_id: uuid.UUID | None = None,
    tool_call_id: str = "call-a",
    attempt_id: str = "attempt-a",
    capability_id: str = "platform.echo",
    arguments: dict[str, Any] | None = None,
    idempotency_key: str = "idem-a",
    effect: str = "read",
    approval_policy: str = "never",
    approval_id: uuid.UUID | None = None,
    approval_status: str = "not_required",
    resource_binding: dict[str, Any] | None = None,
    tenant_id: str = "tenant-a",
    user_id: str = "user-a",
    session_id: str = "session-a",
) -> asyncpg.Record:
    arguments = arguments or {}
    execution_id = execution_id or uuid.uuid4()
    lease_id = lease_id or uuid.uuid4()
    resource_binding = resource_binding or {}
    return await connection.fetchrow(
        "SELECT * FROM reserve_assistant_capability_execution("
        "$1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11::jsonb,$12,$13,$14,$15,$16,$17,$18,$19::jsonb)",
        execution_id,
        lease_id,
        tenant_id,
        user_id,
        session_id,
        run_id,
        tool_call_id,
        attempt_id,
        capability_id,
        1,
        json.dumps(arguments),
        _arguments_hash(arguments),
        idempotency_key,
        effect,
        approval_policy,
        approval_id,
        approval_status,
        f"/internal/v2/capabilities/executions/{execution_id}/events",
        json.dumps(resource_binding),
    )


def test_migration_is_additive_and_declares_the_v2_contract() -> None:
    sql = MIGRATION.read_text(encoding="utf-8").upper()
    assert "DROP TABLE" not in sql
    assert "TRUNCATE" not in sql
    for marker in (
        "ASSISTANT_CAPABILITY_EXECUTIONS",
        "ASSISTANT_CAPABILITY_EVENTS",
        "ASSISTANT_CAPABILITY_IDEMPOTENCY_CONFLICT",
        "ASSISTANT_CAPABILITY_APPROVAL_REQUIRED",
        "ASSISTANT_CAPABILITY_TERMINAL_IMMUTABLE",
        "ASSISTANT_CAPABILITY_EVENT_IMMUTABLE",
        "SIDE_EFFECT_UNKNOWN",
        "DISPATCH_FENCE",
        "WORKER_LEASE_UNTIL",
        "RESOURCE_BINDING",
        "CAPABILITY_REVISION",
        "TOOL_CALL_ID",
    ):
        assert marker in sql

    binding_source = (
        (
            ROOT
            / Path(
                "rust/agent-runtime-overlay/kernel-rs/"
                "ai-platform-capability-worker/src/postgres_store.rs"
            )
        )
        .read_text()
        .upper()
    )
    for marker in (
        "ASSISTANT_RUNTIME_MODEL_LEASES",
        "READONLY_CAPABILITIES",
        "CAPABILITY_ALLOWLIST",
        "DATASET_ID",
    ):
        assert marker in binding_source


@pytest.mark.asyncio
async def test_fresh_idempotent_replay_and_changed_arguments_conflict(
    pool: asyncpg.Pool,
) -> None:
    async with pool.acquire() as connection:
        run_id = await _create_run(connection)
        lease_id = uuid.uuid4()
        first = await _reserve(connection, run_id=run_id, lease_id=lease_id)
        replay = await _reserve(
            connection,
            run_id=run_id,
            execution_id=uuid.uuid4(),
            lease_id=lease_id,
        )
        assert first["execution_id"] == replay["execution_id"]
        with pytest.raises(asyncpg.UniqueViolationError, match="IDEMPOTENCY_CONFLICT"):
            await _reserve(
                connection,
                run_id=run_id,
                lease_id=lease_id,
                arguments={"changed": True},
            )


@pytest.mark.asyncio
async def test_server_derived_resource_binding_is_persisted_and_part_of_replay_identity(
    pool: asyncpg.Pool,
) -> None:
    async with pool.acquire() as connection:
        run_id = await _create_run(connection)
        trusted = {
            "snapshot_id": str(uuid.uuid4()),
            "capability_revision": 1,
            "capability_id": "platform.echo",
            "capability_version": "1",
            "schema_hash": "sha256:" + "a" * 64,
            "bound_dataset_ids": ["dataset-a"],
        }
        first = await _reserve(connection, run_id=run_id, resource_binding=trusted)
        stored_binding = first["resource_binding"]
        if isinstance(stored_binding, str):
            stored_binding = json.loads(stored_binding)
        assert stored_binding == trusted
        with pytest.raises(asyncpg.UniqueViolationError, match="IDEMPOTENCY_CONFLICT"):
            await _reserve(
                connection,
                run_id=run_id,
                resource_binding={**trusted, "bound_dataset_ids": ["dataset-b"]},
            )


@pytest.mark.asyncio
async def test_runtime_binding_uses_readonly_projection_and_scoped_dataset_items(
    pool: asyncpg.Pool,
) -> None:
    async with pool.acquire() as connection:
        run_id = await _create_run(connection)
        schema_hash = (
            "sha256:"
            + hashlib.sha256(
                json.dumps(
                    {"type": "object", "additionalProperties": True},
                    sort_keys=True,
                    separators=(",", ":"),
                ).encode()
            ).hexdigest()
        )
        await connection.execute(
            "UPDATE assistant_runtime_snapshots SET snapshot = jsonb_set("
            "snapshot, '{readonly_capabilities,items}', "
            "$2::jsonb) WHERE run_id=$1",
            run_id,
            json.dumps(
                [
                    {
                        "kind": "knowledge",
                        "tenant_id": "tenant-a",
                        "capability_revision": 1,
                        "payload": {"dataset_id": "dataset-a"},
                    },
                    {
                        "kind": "knowledge",
                        "tenant_id": "tenant-b",
                        "capability_revision": 1,
                        "payload": {"dataset_id": "cross-tenant"},
                    },
                    {
                        "kind": "knowledge",
                        "tenant_id": "tenant-a",
                        "capability_revision": 99,
                        "payload": {"dataset_id": "wrong-revision"},
                    },
                ]
            ),
        )
        binding = await _runtime_binding(connection, run_id, schema_hash=schema_hash)
        assert binding is not None
        assert binding["bound_dataset_ids"] == ["dataset-a"]
        # The legacy top-level capabilities object is intentionally malformed;
        # it must not authorize or alter the readonly projection result.
        assert (
            await _runtime_binding(
                connection,
                run_id,
                schema_hash=schema_hash,
                capability_id="not-in-readonly-projection",
            )
            is None
        )


@pytest.mark.asyncio
async def test_write_dispatch_consumes_one_bound_approval_and_is_fenced(
    pool: asyncpg.Pool,
) -> None:
    async with pool.acquire() as connection:
        run_id = await _create_run(connection)
        arguments = {"path": "workspace/report.txt", "content": "approved"}
        approval_id = uuid.uuid4()
        await connection.execute(
            "INSERT INTO assistant_tool_approvals("
            "approval_id,tenant_id,user_id,session_id,run_id,tool_call_id,tool_name,arguments,status,expires_at"
            ") VALUES($1,'tenant-a','user-a','session-a',$2,'call-a','platform.write_fixture',"
            "$3::jsonb,'pending',NOW()+INTERVAL '5 minutes')",
            approval_id,
            run_id,
            json.dumps(arguments),
        )
        execution = await _reserve(
            connection,
            run_id=run_id,
            capability_id="platform.write_fixture",
            arguments=arguments,
            effect="write",
            approval_policy="always",
            approval_id=approval_id,
            approval_status="pending",
        )
        fence = uuid.uuid4()
        with pytest.raises(asyncpg.InsufficientPrivilegeError, match="APPROVAL_REQUIRED"):
            await connection.fetchrow(
                "SELECT * FROM dispatch_assistant_capability_execution("
                "$1,'tenant-a','user-a','session-a',$2)",
                execution["execution_id"],
                fence,
            )
        await connection.execute(
            "UPDATE assistant_tool_approvals SET status='approved', tool_call_id='different-call' "
            "WHERE approval_id=$1",
            approval_id,
        )
        await connection.execute(
            "UPDATE assistant_capability_executions SET approval_status='approved' "
            "WHERE execution_id=$1",
            execution["execution_id"],
        )
        with pytest.raises(asyncpg.InsufficientPrivilegeError, match="APPROVAL_REQUIRED"):
            await connection.fetchrow(
                "SELECT * FROM dispatch_assistant_capability_execution("
                "$1,'tenant-a','user-a','session-a',$2)",
                execution["execution_id"],
                fence,
            )
        await connection.execute(
            "UPDATE assistant_tool_approvals SET tool_call_id='call-a' WHERE approval_id=$1",
            approval_id,
        )
        claimed = await connection.fetchrow(
            "SELECT * FROM dispatch_assistant_capability_execution("
            "$1,'tenant-a','user-a','session-a',$2)",
            execution["execution_id"],
            fence,
        )
        assert claimed["claimed"] is True
        assert (
            await connection.fetchval(
                "SELECT status FROM assistant_tool_approvals WHERE approval_id=$1",
                approval_id,
            )
            == "consumed"
        )
        replay = await connection.fetchrow(
            "SELECT * FROM dispatch_assistant_capability_execution("
            "$1,'tenant-a','user-a','session-a',$2)",
            execution["execution_id"],
            fence,
        )
        assert replay["claimed"] is False
        with pytest.raises(asyncpg.InsufficientPrivilegeError, match="FENCE_MISMATCH"):
            await connection.fetchrow(
                "SELECT * FROM dispatch_assistant_capability_execution("
                "$1,'tenant-a','user-a','session-a',$2)",
                execution["execution_id"],
                uuid.uuid4(),
            )


@pytest.mark.asyncio
async def test_read_dispatch_uses_a_single_recoverable_worker_lease(
    pool: asyncpg.Pool,
) -> None:
    async with pool.acquire() as connection:
        run_id = await _create_run(connection)
        execution = await _reserve(connection, run_id=run_id)
        first_fence = uuid.uuid4()
        first = await connection.fetchrow(
            "SELECT * FROM dispatch_assistant_capability_execution("
            "$1,'tenant-a','user-a','session-a',$2,30000)",
            execution["execution_id"],
            first_fence,
        )
        assert first["claimed"] is True
        replay = await connection.fetchrow(
            "SELECT * FROM dispatch_assistant_capability_execution("
            "$1,'tenant-a','user-a','session-a',$2,30000)",
            execution["execution_id"],
            uuid.uuid4(),
        )
        assert replay["claimed"] is False
        await connection.execute(
            "UPDATE assistant_capability_executions SET worker_lease_until=NOW()-INTERVAL '1 second' "
            "WHERE execution_id=$1",
            execution["execution_id"],
        )
        recovered_fence = uuid.uuid4()
        recovered = await connection.fetchrow(
            "SELECT * FROM dispatch_assistant_capability_execution("
            "$1,'tenant-a','user-a','session-a',$2,30000)",
            execution["execution_id"],
            recovered_fence,
        )
        assert recovered["claimed"] is True
        with pytest.raises(asyncpg.InsufficientPrivilegeError, match="FENCE_MISMATCH"):
            await connection.fetchval(
                "SELECT append_assistant_capability_event("
                "$1,'tenant-a','user-a','session-a',$2,'progress','running','{}'::jsonb,$3)",
                execution["execution_id"],
                uuid.uuid4(),
                first_fence,
            )


@pytest.mark.asyncio
async def test_concurrent_events_are_monotonic_and_event_replay_is_idempotent(
    pool: asyncpg.Pool,
) -> None:
    async with pool.acquire() as connection:
        run_id = await _create_run(connection)
        execution = await _reserve(connection, run_id=run_id)
        fence = uuid.uuid4()
        await connection.fetchrow(
            "SELECT * FROM dispatch_assistant_capability_execution("
            "$1,'tenant-a','user-a','session-a',$2)",
            execution["execution_id"],
            fence,
        )
    event_ids = [uuid.uuid4(), uuid.uuid4()]

    async def append(event_id: uuid.UUID) -> int:
        async with pool.acquire() as connection:
            return await connection.fetchval(
                "SELECT append_assistant_capability_event("
                "$1,'tenant-a','user-a','session-a',$2,'progress','running',"
                "'{}'::jsonb,$3)",
                execution["execution_id"],
                event_id,
                fence,
            )

    sequences = await asyncio.gather(*(append(event_id) for event_id in event_ids))
    assert sorted(sequences) == [1, 2]
    assert await append(event_ids[0]) == sequences[0]
    async with pool.acquire() as connection:
        rows = await connection.fetch(
            "SELECT sequence FROM assistant_capability_events "
            "WHERE execution_id=$1 ORDER BY sequence",
            execution["execution_id"],
        )
        assert [row["sequence"] for row in rows] == [1, 2]


@pytest.mark.asyncio
async def test_terminal_and_event_rows_are_immutable_and_scope_does_not_leak(
    pool: asyncpg.Pool,
) -> None:
    async with pool.acquire() as connection:
        run_id = await _create_run(connection)
        execution = await _reserve(connection, run_id=run_id)
        event_id = uuid.uuid4()
        await connection.fetchval(
            "SELECT append_assistant_capability_event("
            "$1,'tenant-a','user-a','session-a',$2,'terminal','cancelled',"
            '\'{"error_code":"cancelled"}\'::jsonb)',
            execution["execution_id"],
            event_id,
        )
        with pytest.raises(
            asyncpg.ObjectNotInPrerequisiteStateError,
            match="TERMINAL_IMMUTABLE",
        ):
            await connection.execute(
                "UPDATE assistant_capability_executions SET status='running' WHERE execution_id=$1",
                execution["execution_id"],
            )
        with pytest.raises(
            asyncpg.ObjectNotInPrerequisiteStateError,
            match="EVENT_IMMUTABLE",
        ):
            await connection.execute(
                "DELETE FROM assistant_capability_events WHERE execution_id=$1",
                execution["execution_id"],
            )
        with pytest.raises(asyncpg.InsufficientPrivilegeError, match="SCOPE_MISMATCH"):
            await connection.fetchrow(
                "SELECT * FROM dispatch_assistant_capability_execution("
                "$1,'tenant-a','other-user','session-a',$2)",
                execution["execution_id"],
                uuid.uuid4(),
            )
