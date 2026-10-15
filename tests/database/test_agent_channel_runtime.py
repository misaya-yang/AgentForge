from __future__ import annotations

import asyncio
import hashlib
from datetime import datetime, timedelta, timezone
from pathlib import Path

import asyncpg
import pytest
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentRuntimeUnavailableError,
)

from tests.database.test_agent_publication_atomicity import (
    _create_agent,
    _publish,
    _record_evaluation,
)

pytest_plugins = ("tests.database.test_agent_publication_atomicity",)

ROOT = Path(__file__).resolve().parents[2]
CHANNEL_MIGRATION = ROOT / "database" / "migrations" / "079_agent_channel_runtime.sql"
HARDENING_MIGRATION = ROOT / "database" / "migrations" / "080_agent_channel_delivery_hardening.sql"
OPERATIONS_MIGRATION = (
    ROOT / "database" / "migrations" / "081_agent_studio_operations_governance.sql"
)


@pytest.mark.asyncio
async def test_channel_migration_is_forward_only_idempotent_and_constrained(
    release_pool: asyncpg.Pool,
) -> None:
    sql = CHANNEL_MIGRATION.read_text(encoding="utf-8")
    hardening_sql = HARDENING_MIGRATION.read_text(encoding="utf-8")
    assert "DROP TABLE" not in sql.upper()
    assert "DROP COLUMN" not in sql.upper()
    async with release_pool.acquire() as conn:
        await conn.execute(sql)
        await conn.execute(sql)
        await conn.execute(hardening_sql)
        await conn.execute(hardening_sql)
        columns = await conn.fetch(
            """
            SELECT column_name
            FROM information_schema.columns
            WHERE table_schema = current_schema()
              AND table_name = 'agent_api_tokens'
            """
        )
        assert {row["column_name"] for row in columns} >= {
            "last_used_at",
            "rotated_from_token_id",
        }
        tables = await conn.fetch(
            """
            SELECT table_name
            FROM information_schema.tables
            WHERE table_schema = current_schema()
              AND table_name IN (
                  'agent_runtime_idempotency', 'agent_runtime_feedback',
                  'agent_runtime_attachments'
              )
            """
        )
        assert {row["table_name"] for row in tables} == {
            "agent_runtime_idempotency",
            "agent_runtime_feedback",
            "agent_runtime_attachments",
        }
        idempotency_columns = await conn.fetch(
            """
            SELECT column_name
            FROM information_schema.columns
            WHERE table_schema = current_schema()
              AND table_name = 'agent_runtime_idempotency'
            """
        )
        assert {row["column_name"] for row in idempotency_columns} >= {
            "status",
            "response_body",
            "response_media_type",
            "response_status_code",
            "completed_at",
        }


@pytest.mark.asyncio
async def test_real_postgres_token_hash_scope_rotate_revoke_and_last_used(
    release_pool: asyncpg.Pool,
) -> None:
    async with release_pool.acquire() as conn:
        await conn.execute(CHANNEL_MIGRATION.read_text(encoding="utf-8"))
        await conn.execute(HARDENING_MIGRATION.read_text(encoding="utf-8"))
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool,
        suffix="channel-token",
    )
    evaluation, candidate = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        channel="api",
    )
    release = await _publish(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        evaluation=evaluation,
        candidate=candidate,
        key="channel-token-publish",
    )
    publication_id = str(release["publication"]["publication_id"])
    raw, metadata = await repository.create_api_token(
        tenant_id=tenant_id,
        publication_id=publication_id,
        user_id=user_id,
        name="primary",
        scopes=["chat:write", "sessions:write"],
        expires_at=datetime.now(timezone.utc) + timedelta(hours=1),
    )
    async with release_pool.acquire() as conn:
        stored = await conn.fetchrow(
            "SELECT token_hash, last_used_at FROM agent_api_tokens WHERE token_id = $1",
            metadata["token_id"],
        )
    assert stored["token_hash"] == hashlib.sha256(raw.encode()).hexdigest()
    assert raw != stored["token_hash"]
    assert stored["last_used_at"] is None

    resolved = await repository.resolve_api_token_runtime(
        raw_token=raw,
        publication_id=publication_id,
        required_scopes=["chat:write"],
    )
    assert resolved["publication"]["publication_id"] == publication_id
    assert resolved["api_token"]["token_id"] == metadata["token_id"]
    async with release_pool.acquire() as conn:
        assert await conn.fetchval(
            "SELECT last_used_at IS NOT NULL FROM agent_api_tokens WHERE token_id = $1",
            metadata["token_id"],
        )

    with pytest.raises(AgentRuntimeUnavailableError, match="TOKEN_SCOPE_FORBIDDEN"):
        await repository.resolve_api_token_runtime(
            raw_token=raw,
            publication_id=publication_id,
            required_scopes=["attachments:write"],
        )

    replacement, replacement_metadata = await repository.rotate_api_token(
        tenant_id=tenant_id,
        publication_id=publication_id,
        token_id=str(metadata["token_id"]),
        user_id=user_id,
        name=None,
        scopes=["chat:write", "feedback:write"],
        expires_at=None,
    )
    assert replacement != raw
    assert replacement_metadata["rotated_from_token_id"] == metadata["token_id"]
    with pytest.raises(AgentRuntimeUnavailableError, match="TOKEN_INVALID"):
        await repository.resolve_api_token_runtime(
            raw_token=raw,
            publication_id=publication_id,
            required_scopes=["chat:write"],
        )
    await repository.resolve_api_token_runtime(
        raw_token=replacement,
        publication_id=publication_id,
        required_scopes=["feedback:write"],
    )
    revoked = await repository.revoke_api_token(
        tenant_id=tenant_id,
        publication_id=publication_id,
        token_id=str(replacement_metadata["token_id"]),
        user_id=user_id,
    )
    assert revoked["revoked_at"] is not None
    with pytest.raises(AgentRuntimeUnavailableError, match="TOKEN_INVALID"):
        await repository.resolve_api_token_runtime(
            raw_token=replacement,
            publication_id=publication_id,
            required_scopes=["chat:write"],
        )

    listed = await repository.list_api_tokens(
        tenant_id=tenant_id,
        publication_id=publication_id,
        user_id=user_id,
    )
    assert len(listed) == 2
    assert all("token_hash" not in item for item in listed)
    assert all(raw not in repr(item) and replacement not in repr(item) for item in listed)


@pytest.mark.asyncio
async def test_real_postgres_idempotency_is_atomic_and_replays_terminal_result(
    release_pool: asyncpg.Pool,
) -> None:
    async with release_pool.acquire() as conn:
        await conn.execute(CHANNEL_MIGRATION.read_text(encoding="utf-8"))
        await conn.execute(HARDENING_MIGRATION.read_text(encoding="utf-8"))
        # Migration 081 also indexes pre-existing runtime/audit tables. Keep
        # those dependencies in this isolated schema so the unqualified
        # migration can never fall through to public during the test.
        await conn.execute(
            """
            ALTER TABLE audit_logs
                ADD COLUMN IF NOT EXISTS response_summary JSONB;
            CREATE TABLE agent_traces (
                tenant_id VARCHAR(255), agent_id UUID, agent_version_id UUID,
                publication_id UUID, channel VARCHAR(16), created_at TIMESTAMPTZ
            );
            CREATE TABLE sessions (
                tenant_id VARCHAR(255), agent_id UUID, user_id VARCHAR(255),
                created_at TIMESTAMPTZ
            );
            CREATE TABLE assistant_runs (
                tenant_id VARCHAR(255), agent_id UUID, user_id VARCHAR(255),
                created_at TIMESTAMPTZ
            );
            """
        )
        operations_sql = OPERATIONS_MIGRATION.read_text(encoding="utf-8")
        await conn.execute(operations_sql)
        await conn.execute(operations_sql)
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool,
        suffix="channel-idempotency",
    )
    evaluation, candidate = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        channel="api",
    )
    release = await _publish(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        evaluation=evaluation,
        candidate=candidate,
        key="channel-idempotency-publish",
    )
    publication_id = str(release["publication"]["publication_id"])
    reservation_args = {
        "tenant_id": tenant_id,
        "publication_id": publication_id,
        "principal_id": "runtime-principal",
        "idempotency_key": "same-turn",
        "request_hash": "a" * 64,
    }
    reservations = await asyncio.gather(
        repository.reserve_runtime_idempotency(**reservation_args, session_id="session-a"),
        repository.reserve_runtime_idempotency(**reservation_args, session_id="session-b"),
    )
    assert sum(bool(item["created"]) for item in reservations) == 1
    assert {item["status"] for item in reservations} == {"pending"}
    winner = next(item for item in reservations if item["created"])
    await repository.complete_runtime_idempotency(
        **reservation_args,
        response_body=b'data: {"content":"once"}\n\n',
        response_media_type="text/event-stream",
        response_status_code=200,
    )
    replay = await repository.reserve_runtime_idempotency(
        **reservation_args,
        session_id="ignored-session",
    )
    assert replay["created"] is False
    assert replay["session_id"] == winner["session_id"]
    assert replay["status"] == "completed"
    assert replay["response_body"] == b'data: {"content":"once"}\n\n'

    attachment = await repository.create_runtime_attachment(
        tenant_id=tenant_id,
        publication_id=publication_id,
        principal_id="runtime-principal",
        channel="api",
        storage_key="uploads/runtime-principal/policy.txt",
        filename="policy.txt",
        mime_type="text/plain",
        size_bytes=12,
    )
    resolved = await repository.resolve_runtime_attachments(
        tenant_id=tenant_id,
        publication_id=publication_id,
        principal_id="runtime-principal",
        channel="api",
        attachment_ids=[str(attachment["attachment_id"])],
    )
    assert resolved[0]["file_path"] == "/uploads/runtime-principal/policy.txt"
    with pytest.raises(AgentRuntimeUnavailableError, match="ATTACHMENT_NOT_FOUND"):
        await repository.resolve_runtime_attachments(
            tenant_id=tenant_id,
            publication_id=publication_id,
            principal_id="other-principal",
            channel="api",
            attachment_ids=[str(attachment["attachment_id"])],
        )
