from __future__ import annotations

import asyncio
import copy
import hashlib
import json
import uuid
from collections.abc import AsyncIterator, Awaitable, Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import asyncpg
import pytest
import pytest_asyncio
from ai_gateway_core.eval.agent_version_candidate import build_model_authorization_evidence
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentReleaseEvaluationStaleError,
    AgentReleaseEvaluationTerminalError,
    AgentReleaseGateError,
    AgentReleaseIdempotencyConflictError,
    DatabaseAgentRepository,
)

from database.authority.commands import default_paths
from database.authority.manifest import load_epoch_manifest
from database.authority.runner import MigrationAuthority
from tests.database.test_agent_studio_migrations import _postgres_config

ROOT = Path(__file__).resolve().parents[2]
DOMAIN_MIGRATION = ROOT / "database" / "migrations" / "071_agent_studio_domain.sql"
RELEASE_MIGRATION = ROOT / "database" / "migrations" / "077_agent_publication_eval.sql"
LIFECYCLE_MIGRATION = (
    ROOT / "database" / "migrations" / "078_agent_release_lifecycle_hardening.sql"
)


@dataclass
class _Holder:
    _pool: asyncpg.Pool
    enabled: bool = True


class _IsolatedAuthorityConnection:
    """Run authority DDL as the isolated fixture table owner."""

    def __init__(self, conn: asyncpg.Connection, owner_role: str) -> None:
        self._conn = conn
        self._owner_role = owner_role

    def __getattr__(self, name: str) -> Any:
        return getattr(self._conn, name)

    async def execute(self, query: str, *args: Any) -> str:
        if query.startswith("SET LOCAL ROLE "):
            query = f"SET LOCAL ROLE {self._owner_role}"
        return await self._conn.execute(query, *args)


@pytest_asyncio.fixture
async def release_pool() -> AsyncIterator[asyncpg.Pool]:
    config = _postgres_config()
    database_name = f"agent_release_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    try:
        await admin.execute(f'CREATE DATABASE "{database_name}"')
    finally:
        await admin.close()
    pool = await asyncpg.create_pool(
        **{**config, "database": database_name},
        min_size=1,
        max_size=2,
        server_settings={"search_path": "gateway,public"},
    )
    try:
        async with pool.acquire() as conn:
            await conn.execute("CREATE SCHEMA gateway")
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
            await conn.execute(DOMAIN_MIGRATION.read_text(encoding="utf-8"))
            await conn.execute(
                """
                CREATE TABLE eval_datasets (
                    dataset_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    tenant_id VARCHAR(255) NOT NULL,
                    name VARCHAR(160) NOT NULL DEFAULT '',
                    description TEXT NOT NULL DEFAULT '',
                    version VARCHAR(64) NOT NULL DEFAULT 'v1',
                    schema JSONB NOT NULL DEFAULT '{}'::jsonb,
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    created_by VARCHAR(255) NOT NULL DEFAULT '',
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE eval_examples (
                    example_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    dataset_id UUID NOT NULL REFERENCES eval_datasets(dataset_id),
                    tenant_id VARCHAR(255) NOT NULL,
                    split VARCHAR(32) NOT NULL DEFAULT 'regression',
                    input JSONB NOT NULL DEFAULT '{}'::jsonb,
                    expected_output JSONB NOT NULL DEFAULT '{}'::jsonb,
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    source_trace_id UUID,
                    source_span_id UUID,
                    created_by VARCHAR(255) NOT NULL DEFAULT '',
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE eval_experiment_runs (
                    run_id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    tenant_id VARCHAR(255) NOT NULL,
                    dataset_id UUID,
                    run_mode VARCHAR(32) NOT NULL DEFAULT 'rescore_trace',
                    status VARCHAR(32) NOT NULL DEFAULT 'queued',
                    target_snapshot JSONB NOT NULL DEFAULT '{}'::jsonb,
                    score_summary JSONB NOT NULL DEFAULT '{}'::jsonb,
                    metrics JSONB NOT NULL DEFAULT '{}'::jsonb,
                    dataset_manifest_hash VARCHAR(64),
                    evaluator_suite_hash VARCHAR(64)
                );
                CREATE TABLE llm_providers (
                    tenant_id VARCHAR(255) NOT NULL,
                    provider_id VARCHAR(50) NOT NULL,
                    is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, provider_id)
                );
                CREATE TABLE llm_models (
                    tenant_id VARCHAR(255) NOT NULL,
                    provider_id VARCHAR(50) NOT NULL,
                    model_id VARCHAR(100) NOT NULL,
                    access_level VARCHAR(20) NOT NULL DEFAULT 'public',
                    is_enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    PRIMARY KEY (tenant_id, provider_id, model_id),
                    FOREIGN KEY (tenant_id, provider_id)
                        REFERENCES llm_providers(tenant_id, provider_id)
                );
                CREATE TABLE agent_version_revocations (
                    tenant_id VARCHAR(255) NOT NULL,
                    agent_version_id UUID NOT NULL,
                    PRIMARY KEY (tenant_id, agent_version_id)
                );
                """
            )
            sql = RELEASE_MIGRATION.read_text(encoding="utf-8")
            await conn.execute(sql)
            await conn.execute(sql)
            lifecycle_sql = LIFECYCLE_MIGRATION.read_text(encoding="utf-8")
            await conn.execute(lifecycle_sql)
            await conn.execute(lifecycle_sql)
            paths = default_paths()
            epoch_dir = paths.epoch_dir("2026_08_post_kb_v1")
            manifest = load_epoch_manifest(epoch_dir / "manifest.yml")
            owner_role = await conn.fetchval("SELECT quote_ident(current_user)")
            authority = MigrationAuthority("isolated-test", paths, role_prefix="test_")
            await authority.ensure_ledger(conn)
            await authority.apply_change_transactional(
                _IsolatedAuthorityConnection(conn, owner_role),
                manifest.baseline_id,
                manifest.by_sequence()[7],
                epoch_dir,
            )
        yield pool
    finally:
        await pool.close()
        admin = await asyncpg.connect(**config)
        try:
            await admin.execute(f'DROP DATABASE "{database_name}"')
        finally:
            await admin.close()


def _repository(pool: asyncpg.Pool) -> DatabaseAgentRepository:
    return DatabaseAgentRepository(_Holder(pool))


def _spec(instructions: str) -> dict[str, Any]:
    return {
        "schema_version": "agent-spec/v1",
        "identity": {},
        "instructions": instructions,
        "model": {"model_id": "qwen3.7-plus", "provider_id": "dashscope"},
        "capabilities": [],
        "knowledge": [],
        "memory": {"mode": "session"},
    }


def _plain_hash(value: Any) -> str:
    encoded = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(encoded.encode()).hexdigest()


def _release_gate(status: str = "passed") -> dict[str, Any]:
    return {
        "schema_version": "agent-release-gate/v1",
        "status": status,
        "profile_id": "offline_v1",
        "profile_version": "2026-07-19",
        "execution_scope": "provider_free_release_integrity",
        "model_quality_evaluated": False,
        "blocking_findings": [] if status == "passed" else [{
            "code": "SYNTHETIC_FAILURE",
            "field": "release_candidate",
            "message": "synthetic test failure",
        }],
        "non_blocking_findings": [],
        "metrics": {"critical_pass_rate": 1.0 if status == "passed" else 0.0},
    }


def _release_profile() -> dict[str, Any]:
    gate = _release_gate()
    return {
        key: gate[key]
        for key in (
            "profile_id",
            "profile_version",
            "execution_scope",
            "model_quality_evaluated",
        )
    }


async def _create_agent(
    pool: asyncpg.Pool,
    *,
    suffix: str,
) -> tuple[DatabaseAgentRepository, str, str, str]:
    repository = _repository(pool)
    tenant_id = f"tenant-{suffix}"
    user_id = f"owner-{suffix}"
    async with pool.acquire() as conn:
        await conn.execute(
            "INSERT INTO users (user_id, tenant_id) VALUES ($1, $2)",
            user_id,
            tenant_id,
        )
        await conn.execute(
            "INSERT INTO llm_providers (tenant_id, provider_id) "
            "VALUES ($1, 'dashscope')",
            tenant_id,
        )
        await conn.execute(
            "INSERT INTO llm_models (tenant_id, provider_id, model_id) "
            "VALUES ($1, 'dashscope', 'qwen3.7-plus')",
            tenant_id,
        )
    agent = await repository.create_agent(
        tenant_id=tenant_id,
        user_id=user_id,
        name=f"Release {suffix}",
        slug=f"release-{suffix}",
        description="",
        spec=_spec("revision one"),
    )
    return repository, tenant_id, user_id, str(agent["agent_id"])


async def _model_authorization(
    pool: asyncpg.Pool,
    *,
    tenant_id: str,
) -> dict[str, Any]:
    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            """
            SELECT model.model_id, model.provider_id, model.access_level,
                   model.is_enabled AS model_enabled,
                   model.updated_at AS model_updated_at,
                   provider.is_enabled AS provider_enabled,
                   provider.updated_at AS provider_updated_at
            FROM llm_models AS model
            JOIN llm_providers AS provider
              ON provider.tenant_id = model.tenant_id
             AND provider.provider_id = model.provider_id
            WHERE model.tenant_id = $1
              AND model.provider_id = 'dashscope'
              AND model.model_id = 'qwen3.7-plus'
            """,
            tenant_id,
        )
    assert row is not None
    return build_model_authorization_evidence(
        source="database",
        model_id=str(row["model_id"]),
        provider_id=str(row["provider_id"]),
        access_level=str(row["access_level"]),
        model_enabled=bool(row["model_enabled"]),
        provider_enabled=bool(row["provider_enabled"]),
        runtime_provider_configured=True,
        model_updated_at=row["model_updated_at"],
        provider_updated_at=row["provider_updated_at"],
    )


def _model_authorization_revalidator(
    proof: dict[str, Any],
) -> Callable[[], Awaitable[dict[str, Any]]]:
    async def revalidate() -> dict[str, Any]:
        return copy.deepcopy(proof)

    return revalidate


async def _record_evaluation(
    repository: DatabaseAgentRepository,
    *,
    tenant_id: str,
    user_id: str,
    agent_id: str,
    channel: str = "api",
    dataset_id: str | None = None,
) -> tuple[dict[str, Any], dict[str, Any]]:
    draft = await repository.get_draft(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
    )
    policy = {"attachments": False, "high_risk_tools": False, "allowed_origins": []}
    dataset_snapshot = (
        await repository.resolve_eval_dataset_snapshot(
            tenant_id=tenant_id,
            agent_id=agent_id,
            dataset_id=dataset_id,
            user_id=user_id,
            is_tenant_admin=False,
        )
        if dataset_id is not None
        else None
    )
    model_authorization = await _model_authorization(
        repository._pool,  # noqa: SLF001 - disposable real-PostgreSQL fixture
        tenant_id=tenant_id,
    )
    fingerprint = {
        "spec_hash": draft["spec_hash"],
        "model_id": "qwen3.7-plus",
        "provider_id": "dashscope",
        "model_authorization_hash": _plain_hash(model_authorization),
        "prompt_hash": "sha256:" + "1" * 64,
        "tool_schema_hash": "sha256:" + "2" * 64,
        "skill_manifest_hash": "sha256:" + "3" * 64,
        "knowledge_revision": "sha256:" + "4" * 64,
        "eval_dataset_manifest_hash": (
            dataset_snapshot["manifest_hash"]
            if dataset_snapshot else _plain_hash({"dataset_id": None})
        ),
        "runtime_version": "agent-runtime/v1",
        "snapshot_hash": "sha256:" + "5" * 64,
        "channel_policy_hash": "sha256:" + "6" * 64,
    }
    release_identity = {
        "agent_id": agent_id,
        "draft_id": draft["draft_id"],
        "revision": draft["revision"],
        "spec_hash": draft["spec_hash"],
    }
    candidate = {
        "tenant_id": tenant_id,
        "agent_id": agent_id,
        "draft_id": draft["draft_id"],
        "draft_revision": draft["revision"],
        "spec_hash": draft["spec_hash"],
        "channel": channel,
        "auth_mode": "private",
        "channel_policy": policy,
        "channel_policy_hash": _plain_hash(policy),
        "dataset_id": dataset_id,
        "dataset_version": dataset_snapshot["version"] if dataset_snapshot else None,
        "dataset_manifest_hash": dataset_snapshot["manifest_hash"] if dataset_snapshot else None,
        "model_authorization": model_authorization,
        "runtime_fingerprint": fingerprint,
        "runtime_fingerprint_hash": _plain_hash(fingerprint),
        "release_identity_hash": _plain_hash(release_identity),
        "evaluation_identity_hash": _plain_hash(
            {**release_identity, "channel": channel, "policy": policy}
        ),
    }
    gate = _release_gate()
    evaluation = await repository.record_release_evaluation(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        candidate=candidate,
        gate=gate,
        model_authorization_revalidator=_model_authorization_revalidator(
            model_authorization
        ),
    )
    return evaluation, candidate


async def _publish(
    repository: DatabaseAgentRepository,
    *,
    tenant_id: str,
    user_id: str,
    agent_id: str,
    evaluation: dict[str, Any],
    candidate: dict[str, Any],
    key: str,
    model_authorization_revalidator: (
        Callable[[], Awaitable[dict[str, Any]]] | None
    ) = None,
) -> dict[str, Any]:
    return await repository.publish_agent(
        tenant_id=tenant_id,
        agent_id=agent_id,
        evaluation_id=str(evaluation["evaluation_id"]),
        user_id=user_id,
        is_tenant_admin=False,
        idempotency_key=key,
        reason="release",
        current_candidate=candidate,
        actor_model_access_levels={"public"},
        model_authorization_revalidator=(
            model_authorization_revalidator
            or _model_authorization_revalidator(candidate["model_authorization"])
        ),
    )


@pytest.mark.asyncio
async def test_release_migration_is_forward_only_idempotent_and_immutable(
    release_pool: asyncpg.Pool,
) -> None:
    sql = RELEASE_MIGRATION.read_text(encoding="utf-8")
    lifecycle_sql = LIFECYCLE_MIGRATION.read_text(encoding="utf-8")
    for migration_sql in (sql, lifecycle_sql):
        upper = migration_sql.upper()
        assert "DROP TABLE" not in upper
        assert "TRUNCATE" not in upper
    async with release_pool.acquire() as conn:
        tables = {
            row["table_name"]
            for row in await conn.fetch(
                """
                SELECT table_name FROM information_schema.tables
                WHERE table_schema = current_schema()
                  AND table_name LIKE 'agent_release%'
                """
            )
        }
        assert tables == {
            "agent_release_evaluations",
            "agent_release_evaluation_events",
            "agent_release_requests",
        }
        triggers = {
            row["tgname"]
            for row in await conn.fetch(
                """
                SELECT trigger.tgname
                FROM pg_trigger AS trigger
                WHERE NOT trigger.tgisinternal
                  AND trigger.tgrelid IN (
                      'agent_release_evaluations'::regclass,
                      'agent_release_evaluation_events'::regclass,
                      'agent_release_requests'::regclass
                  )
                """
            )
        }
        assert triggers == {
            "agent_release_evaluations_immutable",
            "agent_release_evaluation_events_immutable",
            "agent_release_requests_immutable",
        }
        columns = {
            row["column_name"]: dict(row)
            for row in await conn.fetch(
                """
                SELECT column_name, is_nullable, column_default
                FROM information_schema.columns
                WHERE table_schema = current_schema()
                  AND table_name = 'agent_release_evaluations'
                  AND column_name IN (
                      'dataset_version', 'dataset_manifest_hash',
                      'evaluation_identity_hash', 'started_at', 'completed_at'
                  )
                """
            )
        }
        assert set(columns) == {
            "dataset_version",
            "dataset_manifest_hash",
            "evaluation_identity_hash",
            "started_at",
            "completed_at",
        }
        assert columns["completed_at"]["is_nullable"] == "YES"
        assert columns["completed_at"]["column_default"] is None
        constraints = {
            row["conname"]: row["definition"]
            for row in await conn.fetch(
                """
                SELECT conname, pg_get_constraintdef(oid) AS definition
                FROM pg_constraint
                WHERE conrelid = 'agent_release_evaluations'::regclass
                  AND conname IN (
                      'agent_release_evaluations_status_check',
                      'agent_release_evaluations_dataset_tenant_fk',
                      'agent_release_evaluations_run_tenant_fk'
                  )
                """
            )
        }
        assert set(constraints) == {
            "agent_release_evaluations_status_check",
            "agent_release_evaluations_dataset_tenant_fk",
            "agent_release_evaluations_run_tenant_fk",
        }
        for lifecycle_status in ("queued", "running", "cancelled"):
            assert lifecycle_status in constraints[
                "agent_release_evaluations_status_check"
            ]
        assert "FOREIGN KEY (tenant_id, dataset_id)" in constraints[
            "agent_release_evaluations_dataset_tenant_fk"
        ]
        assert "FOREIGN KEY (tenant_id, experiment_run_id)" in constraints[
            "agent_release_evaluations_run_tenant_fk"
        ]


@pytest.mark.asyncio
async def test_release_evidence_rows_reject_update_and_delete(
    release_pool: asyncpg.Pool,
) -> None:
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool, suffix="immutable"
    )
    evaluation, candidate = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    await _publish(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        evaluation=evaluation,
        candidate=candidate,
        key="publish-immutable-0001",
    )

    statements = [
        (
            "UPDATE agent_release_evaluations SET profile_id = profile_id "
            "WHERE tenant_id = $1 AND evaluation_id = $2",
            tenant_id,
            evaluation["evaluation_id"],
        ),
        (
            "DELETE FROM agent_release_evaluations "
            "WHERE tenant_id = $1 AND evaluation_id = $2",
            tenant_id,
            evaluation["evaluation_id"],
        ),
        (
            "UPDATE agent_release_evaluation_events SET summary = summary "
            "WHERE tenant_id = $1 AND evaluation_id = $2 AND sequence = 1",
            tenant_id,
            evaluation["evaluation_id"],
        ),
        (
            "DELETE FROM agent_release_evaluation_events "
            "WHERE tenant_id = $1 AND evaluation_id = $2 AND sequence = 1",
            tenant_id,
            evaluation["evaluation_id"],
        ),
        (
            "UPDATE agent_release_requests SET request_hash = request_hash "
            "WHERE tenant_id = $1 AND operation = 'promote'",
            tenant_id,
        ),
        (
            "DELETE FROM agent_release_requests "
            "WHERE tenant_id = $1 AND operation = 'promote'",
            tenant_id,
        ),
    ]
    async with release_pool.acquire() as conn:
        for statement in statements:
            with pytest.raises(
                asyncpg.PostgresError,
                match="AGENT_RELEASE_EVIDENCE_IMMUTABLE",
            ):
                await conn.execute(statement[0], *statement[1:])
        counts = await conn.fetchrow(
            """
            SELECT
                (SELECT COUNT(*) FROM agent_release_evaluations
                 WHERE tenant_id = $1) AS evaluations,
                (SELECT COUNT(*) FROM agent_release_evaluation_events
                 WHERE tenant_id = $1) AS events,
                (SELECT COUNT(*) FROM agent_release_requests
                 WHERE tenant_id = $1) AS requests
            """,
            tenant_id,
        )
    assert dict(counts) == {"evaluations": 1, "events": 3, "requests": 1}


@pytest.mark.asyncio
async def test_release_evaluation_lifecycle_is_durable_cancellable_and_terminal(
    release_pool: asyncpg.Pool,
) -> None:
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool, suffix="lifecycle"
    )
    terminal, candidate = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    queued = await repository.create_release_evaluation(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        candidate=candidate,
        profile=_release_profile(),
        actor_model_access_levels={"public"},
        model_authorization_revalidator=_model_authorization_revalidator(
            candidate["model_authorization"]
        ),
    )
    assert queued["status"] == "queued"
    assert queued["completed_at"] is None
    assert [event["status"] for event in queued["events"]] == ["queued"]

    running = await repository.start_release_evaluation(
        tenant_id=tenant_id,
        agent_id=agent_id,
        evaluation_id=str(queued["evaluation_id"]),
        user_id=user_id,
        is_tenant_admin=False,
    )
    assert running["status"] == "running"
    assert running["execution_claimed"] is True
    assert [event["status"] for event in running["events"]] == ["queued", "running"]

    cancelled = await repository.cancel_release_evaluation(
        tenant_id=tenant_id,
        agent_id=agent_id,
        evaluation_id=str(queued["evaluation_id"]),
        user_id=user_id,
        is_tenant_admin=False,
    )
    completion_after_cancel = await repository.complete_release_evaluation(
        tenant_id=tenant_id,
        agent_id=agent_id,
        evaluation_id=str(queued["evaluation_id"]),
        user_id=user_id,
        is_tenant_admin=False,
        candidate=candidate,
        gate=_release_gate(),
        actor_model_access_levels={"public"},
        model_authorization_revalidator=_model_authorization_revalidator(
            candidate["model_authorization"]
        ),
    )
    assert cancelled["status"] == "cancelled"
    assert completion_after_cancel["status"] == "cancelled"
    assert [event["status"] for event in cancelled["events"]] == [
        "queued",
        "running",
        "cancelled",
    ]

    with pytest.raises(AgentReleaseEvaluationTerminalError):
        await repository.cancel_release_evaluation(
            tenant_id=tenant_id,
            agent_id=agent_id,
            evaluation_id=str(terminal["evaluation_id"]),
            user_id=user_id,
            is_tenant_admin=False,
        )


@pytest.mark.asyncio
async def test_eval_dataset_manifest_is_tenant_bound_and_content_stale(
    release_pool: asyncpg.Pool,
) -> None:
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool, suffix="eval-dataset"
    )
    _, base_candidate = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    dataset_id = uuid.uuid4()
    example_id = uuid.uuid4()
    async with release_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO eval_datasets (
                dataset_id, tenant_id, name, description, version, schema,
                metadata, created_by
            ) VALUES ($1, $2, 'Release cases', 'real boundary', 'release-v3',
                      '{}'::jsonb, '{}'::jsonb, $3)
            """,
            dataset_id,
            tenant_id,
            user_id,
        )
        await conn.execute(
            """
            INSERT INTO eval_examples (
                example_id, dataset_id, tenant_id, input, expected_output,
                metadata, created_by
            ) VALUES (
                $1, $2, $3, '{"message":"classify"}'::jsonb,
                '{"queue":"billing"}'::jsonb,
                '{"case_id":"billing-001"}'::jsonb, $4
            )
            """,
            example_id,
            dataset_id,
            tenant_id,
            user_id,
        )
    snapshot = await repository.resolve_eval_dataset_snapshot(
        tenant_id=tenant_id,
        agent_id=agent_id,
        dataset_id=str(dataset_id),
        user_id=user_id,
        is_tenant_admin=False,
    )
    assert snapshot["version"] == "release-v3"
    assert snapshot["example_count"] == 1
    assert len(snapshot["manifest_hash"]) == 64

    candidate = copy.deepcopy(base_candidate)
    candidate.update(
        {
            "dataset_id": str(dataset_id),
            "dataset_version": snapshot["version"],
            "dataset_manifest_hash": snapshot["manifest_hash"],
        }
    )
    candidate["runtime_fingerprint"]["eval_dataset_manifest_hash"] = snapshot[
        "manifest_hash"
    ]
    candidate["runtime_fingerprint_hash"] = _plain_hash(
        candidate["runtime_fingerprint"]
    )
    candidate["release_identity_hash"] = _plain_hash(
        {
            "agent_id": agent_id,
            "draft_id": candidate["draft_id"],
            "dataset_id": str(dataset_id),
            "dataset_version": snapshot["version"],
            "dataset_manifest_hash": snapshot["manifest_hash"],
        }
    )
    candidate["evaluation_identity_hash"] = _plain_hash(
        {
            "release_identity_hash": candidate["release_identity_hash"],
            "runtime_fingerprint_hash": candidate["runtime_fingerprint_hash"],
            "channel": candidate["channel"],
        }
    )
    queued = await repository.create_release_evaluation(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        candidate=candidate,
        profile=_release_profile(),
        actor_model_access_levels={"public"},
        model_authorization_revalidator=_model_authorization_revalidator(
            candidate["model_authorization"]
        ),
    )
    assert queued["dataset_id"] == str(dataset_id)
    assert queued["dataset_manifest_hash"] == snapshot["manifest_hash"]

    async with release_pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE eval_examples
            SET expected_output = '{"queue":"security"}'::jsonb
            WHERE tenant_id = $1 AND example_id = $2
            """,
            tenant_id,
            example_id,
        )
    changed = await repository.resolve_eval_dataset_snapshot(
        tenant_id=tenant_id,
        agent_id=agent_id,
        dataset_id=str(dataset_id),
        user_id=user_id,
        is_tenant_admin=False,
    )
    assert changed["manifest_hash"] != snapshot["manifest_hash"]
    await repository.start_release_evaluation(
        tenant_id=tenant_id,
        agent_id=agent_id,
        evaluation_id=str(queued["evaluation_id"]),
        user_id=user_id,
        is_tenant_admin=False,
    )
    with pytest.raises(AgentReleaseGateError, match="AGENT_EVAL_DATASET_STALE"):
        await repository.complete_release_evaluation(
            tenant_id=tenant_id,
            agent_id=agent_id,
            evaluation_id=str(queued["evaluation_id"]),
            user_id=user_id,
            is_tenant_admin=False,
            candidate=candidate,
            gate=_release_gate(),
            actor_model_access_levels={"public"},
            model_authorization_revalidator=_model_authorization_revalidator(
                candidate["model_authorization"]
            ),
        )
    persisted = await repository.get_release_evaluation(
        tenant_id=tenant_id,
        agent_id=agent_id,
        evaluation_id=str(queued["evaluation_id"]),
        user_id=user_id,
        is_tenant_admin=False,
    )
    assert persisted["status"] == "running"


@pytest.mark.asyncio
@pytest.mark.parametrize("mutation_kind", ["update", "insert"])
async def test_publish_serializes_eval_manifest_against_example_mutation(
    release_pool: asyncpg.Pool,
    monkeypatch: pytest.MonkeyPatch,
    mutation_kind: str,
) -> None:
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool, suffix=f"eval-manifest-{mutation_kind}"
    )
    _, candidate = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    dataset_id = uuid.uuid4()
    example_id = uuid.uuid4()
    async with release_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO eval_datasets (
                dataset_id, tenant_id, name, description, version, schema,
                metadata, created_by
            ) VALUES ($1, $2, 'Atomic release cases', 'manifest lock', 'release-v1',
                      '{}'::jsonb, '{}'::jsonb, $3)
            """,
            dataset_id,
            tenant_id,
            user_id,
        )
        await conn.execute(
            """
            INSERT INTO eval_examples (
                example_id, dataset_id, tenant_id, input, expected_output,
                metadata, created_by
            ) VALUES (
                $1, $2, $3, '{"message":"classify"}'::jsonb,
                '{"queue":"billing"}'::jsonb,
                '{"case_id":"atomic-001"}'::jsonb, $4
            )
            """,
            example_id,
            dataset_id,
            tenant_id,
            user_id,
        )
    snapshot = await repository.resolve_eval_dataset_snapshot(
        tenant_id=tenant_id,
        agent_id=agent_id,
        dataset_id=str(dataset_id),
        user_id=user_id,
        is_tenant_admin=False,
    )
    candidate = copy.deepcopy(candidate)
    candidate.update(
        {
            "dataset_id": str(dataset_id),
            "dataset_version": snapshot["version"],
            "dataset_manifest_hash": snapshot["manifest_hash"],
        }
    )
    candidate["runtime_fingerprint"]["eval_dataset_manifest_hash"] = snapshot[
        "manifest_hash"
    ]
    candidate["runtime_fingerprint_hash"] = _plain_hash(
        candidate["runtime_fingerprint"]
    )
    candidate["release_identity_hash"] = _plain_hash(
        {
            "agent_id": agent_id,
            "draft_id": candidate["draft_id"],
            "dataset_id": str(dataset_id),
            "dataset_version": snapshot["version"],
            "dataset_manifest_hash": snapshot["manifest_hash"],
        }
    )
    candidate["evaluation_identity_hash"] = _plain_hash(
        {
            "release_identity_hash": candidate["release_identity_hash"],
            "runtime_fingerprint_hash": candidate["runtime_fingerprint_hash"],
            "channel": candidate["channel"],
        }
    )
    evaluation = await repository.record_release_evaluation(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        candidate=candidate,
        gate=_release_gate(),
        actor_model_access_levels={"public"},
        model_authorization_revalidator=_model_authorization_revalidator(
            candidate["model_authorization"]
        ),
    )
    assert evaluation["status"] == "passed"

    manifest_locked = asyncio.Event()
    allow_publish = asyncio.Event()
    original_snapshot = repository._eval_dataset_snapshot_from_conn  # noqa: SLF001

    async def pause_after_locked_manifest(
        conn: Any,
        *,
        tenant_id: str,
        dataset_id: str,
        lock: bool = False,
    ) -> dict[str, Any]:
        result = await original_snapshot(
            conn,
            tenant_id=tenant_id,
            dataset_id=dataset_id,
            lock=lock,
        )
        if lock:
            manifest_locked.set()
            await allow_publish.wait()
        return result

    monkeypatch.setattr(
        repository,
        "_eval_dataset_snapshot_from_conn",
        pause_after_locked_manifest,
    )
    publish_task = asyncio.create_task(
        _publish(
            repository,
            tenant_id=tenant_id,
            user_id=user_id,
            agent_id=agent_id,
            evaluation=evaluation,
            candidate=candidate,
            key=f"publish-eval-manifest-{mutation_kind}-0001",
        )
    )
    await asyncio.wait_for(manifest_locked.wait(), timeout=5)

    mutation_started = asyncio.Event()

    async def mutate_example() -> str:
        async with release_pool.acquire() as conn:
            mutation_started.set()
            if mutation_kind == "update":
                return await conn.execute(
                    """
                    UPDATE eval_examples
                    SET expected_output = '{"queue":"support"}'::jsonb
                    WHERE tenant_id = $1 AND example_id = $2
                    """,
                    tenant_id,
                    example_id,
                )
            return await conn.execute(
                """
                INSERT INTO eval_examples (
                    dataset_id, tenant_id, input, expected_output, metadata,
                    created_by
                ) VALUES (
                    $1, $2, '{"message":"route"}'::jsonb,
                    '{"queue":"support"}'::jsonb,
                    '{"case_id":"atomic-002"}'::jsonb, $3
                )
                """,
                dataset_id,
                tenant_id,
                user_id,
            )

    mutation_task = asyncio.create_task(mutate_example())
    await asyncio.wait_for(mutation_started.wait(), timeout=5)
    try:
        completed, _ = await asyncio.wait({mutation_task}, timeout=0.15)
        assert completed == set(), "example mutation crossed the locked publish manifest"
    finally:
        allow_publish.set()

    published, mutation_status = await asyncio.gather(publish_task, mutation_task)
    assert published["idempotent_replay"] is False
    assert mutation_status in {"UPDATE 1", "INSERT 0 1"}
    changed = await repository.resolve_eval_dataset_snapshot(
        tenant_id=tenant_id,
        agent_id=agent_id,
        dataset_id=str(dataset_id),
        user_id=user_id,
        is_tenant_admin=False,
    )
    assert changed["manifest_hash"] != snapshot["manifest_hash"]
    async with release_pool.acquire() as conn:
        counts = await conn.fetchrow(
            """
            SELECT
                (SELECT COUNT(*) FROM agent_versions
                 WHERE tenant_id = $1 AND agent_id = $2) AS versions,
                (SELECT COUNT(*) FROM agent_publications
                 WHERE tenant_id = $1 AND agent_id = $2) AS publications,
                (SELECT COUNT(*) FROM agent_publish_events
                 WHERE tenant_id = $1 AND agent_id = $2) AS events,
                (SELECT COUNT(*) FROM agent_release_requests
                 WHERE tenant_id = $1 AND agent_id = $2) AS requests
            """,
            tenant_id,
            uuid.UUID(agent_id),
        )
    assert dict(counts) == {
        "versions": 1,
        "publications": 1,
        "events": 1,
        "requests": 1,
    }


@pytest.mark.asyncio
async def test_selected_live_run_promotes_its_existing_version_and_audits_limited_evidence(
    release_pool: asyncpg.Pool,
) -> None:
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool, suffix="selected-run"
    )
    version = await repository.create_version(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        expected_revision=1,
    )
    dataset_id = uuid.uuid4()
    async with release_pool.acquire() as conn:
        await conn.execute(
            """INSERT INTO eval_datasets (dataset_id, tenant_id, name, created_by)
               VALUES ($1, $2, 'selected run dataset', $3)""",
            dataset_id, tenant_id, user_id,
        )
    evaluation, candidate = await _record_evaluation(
        repository, tenant_id=tenant_id, user_id=user_id, agent_id=agent_id,
        dataset_id=str(dataset_id),
    )
    version_id = str(version["agent_version_id"])
    snapshot_hash = "sha256:" + "9" * 64
    identity = {
        "agent_id": agent_id,
        "agent_version_id": version_id,
        "agent_spec_hash": version["spec_hash"],
        "agent_runtime_snapshot_hash": snapshot_hash,
    }
    metrics = {
        "attempted_trials": 3,
        "completed_trials": 3,
        "total_trials": 3,
        "failed_trials": 0,
        "execution_failed_trials": 0,
        "judge_failed_trials": 0,
        "unscored_trials": 0,
        "unknown_side_effect_trial_count": 0,
        "actual_fingerprint": identity,
        "gate": {"profile": "agent_version_task_suite", "status": "warning", "reason": "fixed_sample_only"},
    }
    run_id = uuid.uuid4()
    async with release_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO eval_experiment_runs (
                run_id, tenant_id, dataset_id, run_mode, status, target_snapshot,
                score_summary, metrics, dataset_manifest_hash, evaluator_suite_hash
            ) VALUES ($1, $2, $3, 'live_candidate', 'succeeded', $4::jsonb,
                      $5::jsonb, $6::jsonb, $7, $8)
            """,
            run_id,
            tenant_id,
            dataset_id,
            json.dumps({**identity, "candidate_type": "agent_version"}),
            json.dumps({"critical_failed_count": 0, "excluded_case_count": 0}),
            json.dumps(metrics),
            candidate["dataset_manifest_hash"],
            "8" * 64,
        )
    result = await repository.publish_agent(
        tenant_id=tenant_id,
        agent_id=agent_id,
        evaluation_id=str(evaluation["evaluation_id"]),
        user_id=user_id,
        is_tenant_admin=False,
        idempotency_key="publish-selected-run-0001",
        reason="selected fixed Version",
        current_candidate=candidate,
        experiment_run_id=str(run_id),
        selected_version_id=version_id,
        selected_version_snapshot_hash=snapshot_hash,
        actor_model_access_levels={"public"},
        model_authorization_revalidator=_model_authorization_revalidator(
            candidate["model_authorization"]
        ),
    )
    assert result["version"]["agent_version_id"] == version_id
    assert result["publication"]["version_id"] == version_id
    audit = result["event"]["validation_snapshot"]
    assert audit["experiment_run_id"] == str(run_id)
    assert audit["quality_gate_status"] == "warning"
    assert audit["quality_evidence_scope"] == "fixed_sample_only"
    assert audit["general_quality_proven"] is False
    async with release_pool.acquire() as conn:
        assert await conn.fetchval(
            "SELECT COUNT(*) FROM agent_versions WHERE tenant_id = $1 AND agent_id = $2",
            tenant_id, uuid.UUID(agent_id),
        ) == 1

    replay = await repository.replay_release_request(
        tenant_id=tenant_id,
        operation="promote",
        idempotency_key="publish-selected-run-0001",
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        reason="selected fixed Version",
        evaluation_id=str(evaluation["evaluation_id"]),
        experiment_run_id=str(run_id),
    )
    assert replay is not None and replay["idempotent_replay"] is True
    with pytest.raises(AgentReleaseIdempotencyConflictError):
        await repository.replay_release_request(
            tenant_id=tenant_id,
            operation="promote",
            idempotency_key="publish-selected-run-0001",
            agent_id=agent_id,
            user_id=user_id,
            is_tenant_admin=False,
            reason="selected fixed Version",
            evaluation_id=str(evaluation["evaluation_id"]),
            experiment_run_id=str(uuid.uuid4()),
        )
    with pytest.raises(AgentReleaseGateError, match="AGENT_EVAL_RUN_IDENTITY_MISMATCH"):
        await repository.publish_agent(
            tenant_id=tenant_id,
            agent_id=agent_id,
            evaluation_id=str(evaluation["evaluation_id"]),
            user_id=user_id,
            is_tenant_admin=False,
            idempotency_key="publish-selected-run-wrong-hash",
            reason="selected fixed Version",
            current_candidate=candidate,
            experiment_run_id=str(run_id),
            selected_version_id=version_id,
            selected_version_snapshot_hash="sha256:" + "0" * 64,
            actor_model_access_levels={"public"},
            model_authorization_revalidator=_model_authorization_revalidator(
                candidate["model_authorization"]
            ),
        )
    bad_run_id = uuid.uuid4()
    async with release_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO eval_experiment_runs (
                run_id, tenant_id, dataset_id, run_mode, status, target_snapshot,
                score_summary, metrics, dataset_manifest_hash, evaluator_suite_hash
            )
            SELECT $1, tenant_id, dataset_id, run_mode, status, target_snapshot,
                   score_summary,
                   jsonb_set(metrics, '{unknown_side_effect_trial_count}', '1'::jsonb),
                   dataset_manifest_hash, evaluator_suite_hash
            FROM eval_experiment_runs WHERE run_id = $2
            """,
            bad_run_id, run_id,
        )
    with pytest.raises(AgentReleaseGateError, match="AGENT_EVAL_RUN_HARD_FAILURE"):
        await repository.publish_agent(
            tenant_id=tenant_id,
            agent_id=agent_id,
            evaluation_id=str(evaluation["evaluation_id"]),
            user_id=user_id,
            is_tenant_admin=False,
            idempotency_key="publish-selected-run-unknown-effect",
            reason="selected fixed Version",
            current_candidate=candidate,
            experiment_run_id=str(bad_run_id),
            selected_version_id=version_id,
            selected_version_snapshot_hash=snapshot_hash,
            actor_model_access_levels={"public"},
            model_authorization_revalidator=_model_authorization_revalidator(
                candidate["model_authorization"]
            ),
        )
    failed_gate_run_id = uuid.uuid4()
    async with release_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO eval_experiment_runs (
                run_id, tenant_id, dataset_id, run_mode, status, target_snapshot,
                score_summary, metrics, dataset_manifest_hash, evaluator_suite_hash
            )
            SELECT $1, tenant_id, dataset_id, run_mode, status, target_snapshot,
                   score_summary,
                   jsonb_set(metrics, '{gate}',
                             '{"profile":"agent_version_task_suite","status":"fail","reason":"known_hard_failure"}'::jsonb),
                   dataset_manifest_hash, evaluator_suite_hash
            FROM eval_experiment_runs WHERE run_id = $2
            """,
            failed_gate_run_id, run_id,
        )
    with pytest.raises(AgentReleaseGateError, match="AGENT_EVAL_RUN_HARD_FAILURE"):
        await repository.publish_agent(
            tenant_id=tenant_id,
            agent_id=agent_id,
            evaluation_id=str(evaluation["evaluation_id"]),
            user_id=user_id,
            is_tenant_admin=False,
            idempotency_key="publish-selected-run-failed-gate",
            reason="selected fixed Version",
            current_candidate=candidate,
            experiment_run_id=str(failed_gate_run_id),
            selected_version_id=version_id,
            selected_version_snapshot_hash=snapshot_hash,
            actor_model_access_levels={"public"},
            model_authorization_revalidator=_model_authorization_revalidator(
                candidate["model_authorization"]
            ),
        )
    other_dataset_id, other_run_id = uuid.uuid4(), uuid.uuid4()
    async with release_pool.acquire() as conn:
        await conn.execute(
            """INSERT INTO eval_datasets (dataset_id, tenant_id, name, created_by)
               VALUES ($1, $2, 'different dataset', $3)""",
            other_dataset_id, tenant_id, user_id,
        )
        await conn.execute(
            """
            INSERT INTO eval_experiment_runs (
                run_id, tenant_id, dataset_id, run_mode, status, target_snapshot,
                score_summary, metrics, dataset_manifest_hash, evaluator_suite_hash
            )
            SELECT $1, tenant_id, $2, run_mode, status, target_snapshot,
                   score_summary, metrics, dataset_manifest_hash, evaluator_suite_hash
            FROM eval_experiment_runs WHERE run_id = $3
            """,
            other_run_id, other_dataset_id, run_id,
        )
    with pytest.raises(AgentReleaseGateError, match="AGENT_EVAL_RUN_DATASET_MISMATCH"):
        await repository.publish_agent(
            tenant_id=tenant_id,
            agent_id=agent_id,
            evaluation_id=str(evaluation["evaluation_id"]),
            user_id=user_id,
            is_tenant_admin=False,
            idempotency_key="publish-selected-run-other-dataset",
            reason="selected fixed Version",
            current_candidate=candidate,
            experiment_run_id=str(other_run_id),
            selected_version_id=version_id,
            selected_version_snapshot_hash=snapshot_hash,
            actor_model_access_levels={"public"},
            model_authorization_revalidator=_model_authorization_revalidator(
                candidate["model_authorization"]
            ),
        )
    async with release_pool.acquire() as conn:
        assert await conn.fetchval(
            "SELECT COUNT(*) FROM agent_publish_events WHERE tenant_id = $1 AND agent_id = $2",
            tenant_id, uuid.UUID(agent_id),
        ) == 1


@pytest.mark.asyncio
async def test_publish_is_idempotent_and_event_failure_rolls_back_every_row(
    release_pool: asyncpg.Pool,
) -> None:
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool, suffix="atomic"
    )
    evaluation, candidate = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    first = await _publish(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        evaluation=evaluation,
        candidate=candidate,
        key="publish-atomic-0001",
    )
    replay = await _publish(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        evaluation=evaluation,
        candidate=candidate,
        key="publish-atomic-0001",
    )
    assert replay["idempotent_replay"] is True
    assert replay["version"]["agent_version_id"] == first["version"]["agent_version_id"]
    assert replay["event"]["event_id"] == first["event"]["event_id"]
    await repository.update_draft(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        expected_revision=1,
        spec=_spec("changed after committed release"),
    )
    durable_replay = await repository.replay_release_request(
        tenant_id=tenant_id,
        operation="promote",
        idempotency_key="publish-atomic-0001",
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        reason="release",
        evaluation_id=str(evaluation["evaluation_id"]),
    )
    assert durable_replay is not None
    assert durable_replay["idempotent_replay"] is True
    assert durable_replay["event"]["event_id"] == first["event"]["event_id"]
    with pytest.raises(AgentReleaseIdempotencyConflictError):
        await repository.publish_agent(
            tenant_id=tenant_id,
            agent_id=agent_id,
            evaluation_id=str(evaluation["evaluation_id"]),
            user_id=user_id,
            is_tenant_admin=False,
            idempotency_key="publish-atomic-0001",
            reason="different payload",
            current_candidate=candidate,
            actor_model_access_levels={"public"},
        )

    failing_repo, failing_tenant, failing_user, failing_agent = await _create_agent(
        release_pool, suffix="event-failure"
    )
    failing_eval, failing_candidate = await _record_evaluation(
        failing_repo,
        tenant_id=failing_tenant,
        user_id=failing_user,
        agent_id=failing_agent,
    )
    async with release_pool.acquire() as conn:
        await conn.execute(
            """
            CREATE FUNCTION reject_publish_event_for_atomicity()
            RETURNS trigger AS $$
            BEGIN
                RAISE EXCEPTION 'synthetic publish event failure';
            END;
            $$ LANGUAGE plpgsql;
            CREATE TRIGGER reject_publish_event_for_atomicity
            BEFORE INSERT ON agent_publish_events
            FOR EACH ROW EXECUTE FUNCTION reject_publish_event_for_atomicity();
            """
        )
    with pytest.raises(asyncpg.PostgresError, match="synthetic publish event failure"):
        await _publish(
            failing_repo,
            tenant_id=failing_tenant,
            user_id=failing_user,
            agent_id=failing_agent,
            evaluation=failing_eval,
            candidate=failing_candidate,
            key="publish-failure-0001",
        )
    async with release_pool.acquire() as conn:
        counts = await conn.fetchrow(
            """
            SELECT
                (SELECT COUNT(*) FROM agent_versions
                 WHERE tenant_id = $1 AND agent_id = $2) AS versions,
                (SELECT COUNT(*) FROM agent_publications
                 WHERE tenant_id = $1 AND agent_id = $2) AS publications,
                (SELECT COUNT(*) FROM agent_publish_events
                 WHERE tenant_id = $1 AND agent_id = $2) AS events,
                (SELECT COUNT(*) FROM agent_release_requests
                 WHERE tenant_id = $1 AND agent_id = $2) AS requests
            """,
            failing_tenant,
            uuid.UUID(failing_agent),
        )
        assert dict(counts) == {"versions": 0, "publications": 0, "events": 0, "requests": 0}
        await conn.execute("DROP TRIGGER reject_publish_event_for_atomicity ON agent_publish_events")
        await conn.execute("DROP FUNCTION reject_publish_event_for_atomicity()")


@pytest.mark.asyncio
async def test_same_tenant_key_serializes_across_different_agents(
    release_pool: asyncpg.Pool,
) -> None:
    repository, tenant_id, user_id, agent_one = await _create_agent(
        release_pool, suffix="global-key"
    )
    second = await repository.create_agent(
        tenant_id=tenant_id,
        user_id=user_id,
        name="Second release target",
        slug="second-release-target",
        description="",
        spec=_spec("second Agent"),
    )
    agent_two = str(second["agent_id"])
    evaluation_one, candidate_one = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_one,
    )
    evaluation_two, candidate_two = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_two,
    )

    async def publish(
        agent_id: str,
        evaluation: dict[str, Any],
        candidate: dict[str, Any],
    ) -> dict[str, Any]:
        return await _publish(
            repository,
            tenant_id=tenant_id,
            user_id=user_id,
            agent_id=agent_id,
            evaluation=evaluation,
            candidate=candidate,
            key="tenant-global-key-0001",
        )

    outcomes = await asyncio.gather(
        publish(agent_one, evaluation_one, candidate_one),
        publish(agent_two, evaluation_two, candidate_two),
        return_exceptions=True,
    )
    successes = [item for item in outcomes if isinstance(item, dict)]
    failures = [item for item in outcomes if isinstance(item, BaseException)]
    assert len(successes) == 1
    assert len(failures) == 1
    assert isinstance(failures[0], AgentReleaseIdempotencyConflictError)
    assert getattr(failures[0], "sqlstate", None) is None

    async with release_pool.acquire() as conn:
        counts = await conn.fetchrow(
            """
            SELECT
                (SELECT COUNT(*) FROM agent_versions WHERE tenant_id = $1) AS versions,
                (SELECT COUNT(*) FROM agent_publications WHERE tenant_id = $1) AS publications,
                (SELECT COUNT(*) FROM agent_publish_events WHERE tenant_id = $1) AS events,
                (SELECT COUNT(*) FROM agent_release_requests WHERE tenant_id = $1) AS requests
            """,
            tenant_id,
        )
    assert dict(counts) == {
        "versions": 1,
        "publications": 1,
        "events": 1,
        "requests": 1,
    }


@pytest.mark.asyncio
async def test_stale_evaluation_cannot_create_version_or_pointer(
    release_pool: asyncpg.Pool,
) -> None:
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool, suffix="stale"
    )
    evaluation, candidate = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    await repository.update_draft(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        expected_revision=1,
        spec=_spec("revision two"),
    )
    with pytest.raises(AgentReleaseEvaluationStaleError):
        await _publish(
            repository,
            tenant_id=tenant_id,
            user_id=user_id,
            agent_id=agent_id,
            evaluation=evaluation,
            candidate=candidate,
            key="publish-stale-0001",
        )
    async with release_pool.acquire() as conn:
        counts = await conn.fetchrow(
            """
            SELECT
                (SELECT COUNT(*) FROM agent_versions
                 WHERE tenant_id = $1 AND agent_id = $2) AS versions,
                (SELECT COUNT(*) FROM agent_publications
                 WHERE tenant_id = $1 AND agent_id = $2) AS publications,
                (SELECT COUNT(*) FROM agent_publish_events
                 WHERE tenant_id = $1 AND agent_id = $2) AS events
            """,
            tenant_id,
            uuid.UUID(agent_id),
        )
        assert dict(counts) == {"versions": 0, "publications": 0, "events": 0}


@pytest.mark.parametrize(
    ("suffix", "statement", "expected_code"),
    [
        (
            "model-disabled",
            "UPDATE llm_models SET is_enabled = FALSE "
            "WHERE tenant_id = $1 AND provider_id = 'dashscope'",
            "AGENT_RUNTIME_MODEL_UNAVAILABLE",
        ),
        (
            "provider-disabled",
            "UPDATE llm_providers SET is_enabled = FALSE "
            "WHERE tenant_id = $1 AND provider_id = 'dashscope'",
            "AGENT_RUNTIME_MODEL_UNAVAILABLE",
        ),
        (
            "model-access",
            "UPDATE llm_models SET access_level = 'admin' "
            "WHERE tenant_id = $1 AND provider_id = 'dashscope'",
            "AGENT_RUNTIME_MODEL_FORBIDDEN",
        ),
    ],
)
@pytest.mark.asyncio
async def test_publish_rechecks_model_authorization_before_any_release_write(
    release_pool: asyncpg.Pool,
    suffix: str,
    statement: str,
    expected_code: str,
) -> None:
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool, suffix=suffix
    )
    evaluation, candidate = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    async with release_pool.acquire() as conn:
        await conn.execute(statement, tenant_id)
    with pytest.raises(AgentReleaseGateError, match=expected_code):
        await _publish(
            repository,
            tenant_id=tenant_id,
            user_id=user_id,
            agent_id=agent_id,
            evaluation=evaluation,
            candidate=candidate,
            key=f"publish-{suffix}-0001",
        )
    async with release_pool.acquire() as conn:
        counts = await conn.fetchrow(
            """
            SELECT
                (SELECT COUNT(*) FROM agent_versions
                 WHERE tenant_id = $1 AND agent_id = $2) AS versions,
                (SELECT COUNT(*) FROM agent_publications
                 WHERE tenant_id = $1 AND agent_id = $2) AS publications,
                (SELECT COUNT(*) FROM agent_publish_events
                 WHERE tenant_id = $1 AND agent_id = $2) AS events,
                (SELECT COUNT(*) FROM agent_release_requests
                 WHERE tenant_id = $1 AND agent_id = $2) AS requests
            """,
            tenant_id,
            uuid.UUID(agent_id),
        )
    assert dict(counts) == {
        "versions": 0,
        "publications": 0,
        "events": 0,
        "requests": 0,
    }


@pytest.mark.asyncio
async def test_publish_rechecks_provider_readiness_inside_atomic_decision(
    release_pool: asyncpg.Pool,
) -> None:
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool, suffix="publish-readiness-race"
    )
    evaluation, candidate = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )

    with pytest.raises(
        AgentReleaseGateError,
        match="AGENT_RUNTIME_MODEL_AUTHORIZATION_UNVERIFIABLE",
    ):
        await repository.publish_agent(
            tenant_id=tenant_id,
            agent_id=agent_id,
            evaluation_id=str(evaluation["evaluation_id"]),
            user_id=user_id,
            is_tenant_admin=False,
            idempotency_key="publish-readiness-missing-0001",
            reason="must fail closed",
            current_candidate=candidate,
            actor_model_access_levels={"public"},
        )

    readiness_calls = 0

    async def readiness_revoked() -> dict[str, Any]:
        nonlocal readiness_calls
        readiness_calls += 1
        raise AgentReleaseGateError("AGENT_RUNTIME_MODEL_UNAVAILABLE")

    with pytest.raises(AgentReleaseGateError, match="AGENT_RUNTIME_MODEL_UNAVAILABLE"):
        await _publish(
            repository,
            tenant_id=tenant_id,
            user_id=user_id,
            agent_id=agent_id,
            evaluation=evaluation,
            candidate=candidate,
            key="publish-readiness-race-0001",
            model_authorization_revalidator=readiness_revoked,
        )
    assert readiness_calls == 1

    async with release_pool.acquire() as conn:
        counts = await conn.fetchrow(
            """
            SELECT
                (SELECT COUNT(*) FROM agent_versions
                 WHERE tenant_id = $1 AND agent_id = $2) AS versions,
                (SELECT COUNT(*) FROM agent_publications
                 WHERE tenant_id = $1 AND agent_id = $2) AS publications,
                (SELECT COUNT(*) FROM agent_publish_events
                 WHERE tenant_id = $1 AND agent_id = $2) AS events,
                (SELECT COUNT(*) FROM agent_release_requests
                 WHERE tenant_id = $1 AND agent_id = $2) AS requests
            """,
            tenant_id,
            uuid.UUID(agent_id),
        )
    assert dict(counts) == {
        "versions": 0,
        "publications": 0,
        "events": 0,
        "requests": 0,
    }


@pytest.mark.asyncio
async def test_rollback_rechecks_revocation_and_preserves_pointer_on_failure(
    release_pool: asyncpg.Pool,
) -> None:
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool, suffix="rollback"
    )
    evaluation_one, candidate_one = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    published_one = await _publish(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        evaluation=evaluation_one,
        candidate=candidate_one,
        key="publish-rollback-0001",
    )
    await repository.update_draft(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        expected_revision=1,
        spec=_spec("revision two"),
    )
    evaluation_two, candidate_two = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    published_two = await _publish(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        evaluation=evaluation_two,
        candidate=candidate_two,
        key="publish-rollback-0002",
    )
    publication_id = str(published_two["publication"]["publication_id"])
    version_one = str(published_one["version"]["agent_version_id"])
    version_two = str(published_two["version"]["agent_version_id"])
    rollback = await repository.rollback_publication(
        tenant_id=tenant_id,
        publication_id=publication_id,
        target_version_id=version_one,
        user_id=user_id,
        is_tenant_admin=False,
        idempotency_key="rollback-atomic-0001",
        reason="known healthy",
        runtime_snapshot_hash="7" * 64,
        runtime_spec_hash=str(published_one["version"]["spec_hash"]),
        model_authorization=candidate_one["model_authorization"],
        actor_model_access_levels={"public"},
        model_authorization_revalidator=_model_authorization_revalidator(
            candidate_one["model_authorization"]
        ),
    )
    replay = await repository.rollback_publication(
        tenant_id=tenant_id,
        publication_id=publication_id,
        target_version_id=version_one,
        user_id=user_id,
        is_tenant_admin=False,
        idempotency_key="rollback-atomic-0001",
        reason="known healthy",
        runtime_snapshot_hash="7" * 64,
        runtime_spec_hash=str(published_one["version"]["spec_hash"]),
        model_authorization=candidate_one["model_authorization"],
        actor_model_access_levels={"public"},
        model_authorization_revalidator=_model_authorization_revalidator(
            candidate_one["model_authorization"]
        ),
    )
    assert rollback["event"]["operation"] == "rollback"
    assert replay["event"]["event_id"] == rollback["event"]["event_id"]
    assert replay["idempotent_replay"] is True

    repromoted = await _publish(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        evaluation=evaluation_two,
        candidate=candidate_two,
        key="publish-rollback-0003",
    )
    assert repromoted["publication"]["version_id"] == version_two
    async with release_pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO agent_version_revocations (tenant_id, agent_version_id)
            VALUES ($1, $2)
            """,
            tenant_id,
            uuid.UUID(version_one),
        )
        event_count = await conn.fetchval(
            "SELECT COUNT(*) FROM agent_publish_events WHERE tenant_id = $1 AND agent_id = $2",
            tenant_id,
            uuid.UUID(agent_id),
        )
    with pytest.raises(AgentReleaseGateError, match="AGENT_ROLLBACK_VERSION_UNAVAILABLE"):
        await repository.rollback_publication(
            tenant_id=tenant_id,
            publication_id=publication_id,
            target_version_id=version_one,
            user_id=user_id,
            is_tenant_admin=False,
            idempotency_key="rollback-atomic-0002",
            reason="must fail",
            runtime_snapshot_hash="7" * 64,
            runtime_spec_hash=str(published_one["version"]["spec_hash"]),
            model_authorization=candidate_one["model_authorization"],
            actor_model_access_levels={"public"},
        )
    async with release_pool.acquire() as conn:
        pointer = await conn.fetchval(
            """
            SELECT version_id FROM agent_publications
            WHERE tenant_id = $1 AND publication_id = $2
            """,
            tenant_id,
            uuid.UUID(publication_id),
        )
        after_event_count = await conn.fetchval(
            "SELECT COUNT(*) FROM agent_publish_events WHERE tenant_id = $1 AND agent_id = $2",
            tenant_id,
            uuid.UUID(agent_id),
        )
    assert str(pointer) == version_two
    assert after_event_count == event_count


@pytest.mark.asyncio
async def test_rollback_model_disable_race_preserves_pointer_event_and_request(
    release_pool: asyncpg.Pool,
) -> None:
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool, suffix="rollback-model-race"
    )
    evaluation_one, candidate_one = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    published_one = await _publish(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        evaluation=evaluation_one,
        candidate=candidate_one,
        key="publish-rollback-model-0001",
    )
    await repository.update_draft(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        expected_revision=1,
        spec=_spec("revision two"),
    )
    evaluation_two, candidate_two = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    published_two = await _publish(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        evaluation=evaluation_two,
        candidate=candidate_two,
        key="publish-rollback-model-0002",
    )
    publication_id = str(published_two["publication"]["publication_id"])
    version_one = str(published_one["version"]["agent_version_id"])
    version_two = str(published_two["version"]["agent_version_id"])
    async with release_pool.acquire() as conn:
        before = await conn.fetchrow(
            """
            SELECT
                (SELECT COUNT(*) FROM agent_publish_events
                 WHERE tenant_id = $1 AND agent_id = $2) AS events,
                (SELECT COUNT(*) FROM agent_release_requests
                 WHERE tenant_id = $1 AND agent_id = $2) AS requests
            """,
            tenant_id,
            uuid.UUID(agent_id),
        )
        await conn.execute(
            """
            UPDATE llm_models SET is_enabled = FALSE
            WHERE tenant_id = $1 AND provider_id = 'dashscope'
              AND model_id = 'qwen3.7-plus'
            """,
            tenant_id,
        )
    with pytest.raises(AgentReleaseGateError, match="AGENT_RUNTIME_MODEL_UNAVAILABLE"):
        await repository.rollback_publication(
            tenant_id=tenant_id,
            publication_id=publication_id,
            target_version_id=version_one,
            user_id=user_id,
            is_tenant_admin=False,
            idempotency_key="rollback-model-race-0001",
            reason="must fail atomically",
            runtime_snapshot_hash="7" * 64,
            runtime_spec_hash=str(published_one["version"]["spec_hash"]),
            model_authorization=candidate_one["model_authorization"],
            actor_model_access_levels={"public"},
        )
    async with release_pool.acquire() as conn:
        after = await conn.fetchrow(
            """
            SELECT
                (SELECT version_id FROM agent_publications
                 WHERE tenant_id = $1 AND publication_id = $2) AS pointer,
                (SELECT COUNT(*) FROM agent_publish_events
                 WHERE tenant_id = $1 AND agent_id = $3) AS events,
                (SELECT COUNT(*) FROM agent_release_requests
                 WHERE tenant_id = $1 AND agent_id = $3) AS requests
            """,
            tenant_id,
            uuid.UUID(publication_id),
            uuid.UUID(agent_id),
        )
    assert str(after["pointer"]) == version_two
    assert after["events"] == before["events"]
    assert after["requests"] == before["requests"]


@pytest.mark.asyncio
async def test_rollback_rechecks_provider_readiness_and_preserves_every_release_row(
    release_pool: asyncpg.Pool,
) -> None:
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool, suffix="rollback-readiness-race"
    )
    evaluation_one, candidate_one = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    published_one = await _publish(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        evaluation=evaluation_one,
        candidate=candidate_one,
        key="publish-rollback-readiness-0001",
    )
    await repository.update_draft(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        expected_revision=1,
        spec=_spec("revision two"),
    )
    evaluation_two, candidate_two = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    published_two = await _publish(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        evaluation=evaluation_two,
        candidate=candidate_two,
        key="publish-rollback-readiness-0002",
    )
    publication_id = str(published_two["publication"]["publication_id"])
    version_one = str(published_one["version"]["agent_version_id"])
    version_two = str(published_two["version"]["agent_version_id"])

    async with release_pool.acquire() as conn:
        before = await conn.fetchrow(
            """
            SELECT
                (SELECT version_id FROM agent_publications
                 WHERE tenant_id = $1 AND publication_id = $2) AS pointer,
                (SELECT COUNT(*) FROM agent_versions
                 WHERE tenant_id = $1 AND agent_id = $3) AS versions,
                (SELECT COUNT(*) FROM agent_publications
                 WHERE tenant_id = $1 AND agent_id = $3) AS publications,
                (SELECT COUNT(*) FROM agent_publish_events
                 WHERE tenant_id = $1 AND agent_id = $3) AS events,
                (SELECT COUNT(*) FROM agent_release_requests
                 WHERE tenant_id = $1 AND agent_id = $3) AS requests
            """,
            tenant_id,
            uuid.UUID(publication_id),
            uuid.UUID(agent_id),
        )

    readiness_calls = 0

    async def readiness_revoked() -> dict[str, Any]:
        nonlocal readiness_calls
        readiness_calls += 1
        raise AgentReleaseGateError("AGENT_RUNTIME_MODEL_UNAVAILABLE")

    with pytest.raises(AgentReleaseGateError, match="AGENT_RUNTIME_MODEL_UNAVAILABLE"):
        await repository.rollback_publication(
            tenant_id=tenant_id,
            publication_id=publication_id,
            target_version_id=version_one,
            user_id=user_id,
            is_tenant_admin=False,
            idempotency_key="rollback-readiness-race-0001",
            reason="must fail atomically",
            runtime_snapshot_hash="7" * 64,
            runtime_spec_hash=str(published_one["version"]["spec_hash"]),
            model_authorization=candidate_one["model_authorization"],
            actor_model_access_levels={"public"},
            model_authorization_revalidator=readiness_revoked,
        )
    assert readiness_calls == 1

    async with release_pool.acquire() as conn:
        after = await conn.fetchrow(
            """
            SELECT
                (SELECT version_id FROM agent_publications
                 WHERE tenant_id = $1 AND publication_id = $2) AS pointer,
                (SELECT COUNT(*) FROM agent_versions
                 WHERE tenant_id = $1 AND agent_id = $3) AS versions,
                (SELECT COUNT(*) FROM agent_publications
                 WHERE tenant_id = $1 AND agent_id = $3) AS publications,
                (SELECT COUNT(*) FROM agent_publish_events
                 WHERE tenant_id = $1 AND agent_id = $3) AS events,
                (SELECT COUNT(*) FROM agent_release_requests
                 WHERE tenant_id = $1 AND agent_id = $3) AS requests
            """,
            tenant_id,
            uuid.UUID(publication_id),
            uuid.UUID(agent_id),
        )
    assert str(before["pointer"]) == version_two
    assert dict(after) == dict(before)


@pytest.mark.asyncio
async def test_rollback_rejects_version_that_was_never_channel_history(
    release_pool: asyncpg.Pool,
) -> None:
    repository, tenant_id, user_id, agent_id = await _create_agent(
        release_pool, suffix="rollback-unreleased"
    )
    evaluation, candidate = await _record_evaluation(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
    )
    published = await _publish(
        repository,
        tenant_id=tenant_id,
        user_id=user_id,
        agent_id=agent_id,
        evaluation=evaluation,
        candidate=candidate,
        key="publish-unreleased-0001",
    )
    await repository.update_draft(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        expected_revision=1,
        spec=_spec("never promoted"),
    )
    unreleased = await repository.create_version(
        tenant_id=tenant_id,
        agent_id=agent_id,
        user_id=user_id,
        is_tenant_admin=False,
        expected_revision=2,
    )
    publication_id = str(published["publication"]["publication_id"])

    with pytest.raises(
        AgentReleaseGateError,
        match="AGENT_ROLLBACK_TARGET_NOT_HISTORICAL",
    ):
        await repository.rollback_publication(
            tenant_id=tenant_id,
            publication_id=publication_id,
            target_version_id=str(unreleased["agent_version_id"]),
            user_id=user_id,
            is_tenant_admin=False,
            idempotency_key="rollback-unreleased-0001",
            reason="must not bypass evaluation",
            runtime_snapshot_hash="7" * 64,
            runtime_spec_hash=str(unreleased["spec_hash"]),
            model_authorization=candidate["model_authorization"],
            actor_model_access_levels={"public"},
        )

    async with release_pool.acquire() as conn:
        pointer = await conn.fetchval(
            """
            SELECT version_id FROM agent_publications
            WHERE tenant_id = $1 AND publication_id = $2
            """,
            tenant_id,
            uuid.UUID(publication_id),
        )
        event_count = await conn.fetchval(
            """
            SELECT COUNT(*) FROM agent_publish_events
            WHERE tenant_id = $1 AND publication_id = $2
            """,
            tenant_id,
            uuid.UUID(publication_id),
        )
        request_count = await conn.fetchval(
            """
            SELECT COUNT(*) FROM agent_release_requests
            WHERE tenant_id = $1 AND operation = 'rollback'
            """,
            tenant_id,
        )
    assert str(pointer) == str(published["version"]["agent_version_id"])
    assert event_count == 1
    assert request_count == 0
