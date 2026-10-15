from __future__ import annotations

import json
import uuid
from collections.abc import AsyncIterator
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import asyncpg
import pytest
import pytest_asyncio
from ai_gateway_core.persistence.repositories.agent_repository import (
    AgentValidationError,
    DatabaseAgentRepository,
)
from ai_gateway_core.skills import (
    DatabaseSkillArtifactRepository,
    SkillArtifactConflictError,
    SkillArtifactUnavailableError,
    parse_user_skill_md,
)

from tests.database.test_agent_studio_migrations import _postgres_config

ROOT = Path(__file__).resolve().parents[2]
SKILL_MIGRATION = ROOT / "database" / "migrations" / "037_assistant_skills.sql"
DOMAIN_MIGRATION = ROOT / "database" / "migrations" / "071_agent_studio_domain.sql"
BINDING_MIGRATION = ROOT / "database" / "migrations" / "075_agent_skill_knowledge_bindings.sql"
CONTENT_REVISION_MIGRATION = (
    ROOT / "database" / "migrations" / "076_agent_knowledge_content_revision.sql"
)


@pytest_asyncio.fixture
async def binding_pool() -> AsyncIterator[asyncpg.Pool]:
    config = _postgres_config()
    database_name = f"agent_skill_kb_test_{uuid.uuid4().hex}"
    admin = await asyncpg.connect(**config)
    await admin.execute(f'CREATE DATABASE "{database_name}"')
    await admin.close()
    test_config = {**config, "database": database_name}
    pool = await asyncpg.create_pool(
        **test_config,
        min_size=1,
        max_size=2,
        server_settings={"search_path": "assistant,public"},
    )
    try:
        async with pool.acquire() as connection:
            await connection.execute(
                """
                CREATE EXTENSION IF NOT EXISTS pgcrypto;
                CREATE SCHEMA assistant;
                CREATE TABLE datasets (
                    dataset_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    name VARCHAR(255) NOT NULL,
                    visibility VARCHAR(50) NOT NULL DEFAULT 'private',
                    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
                    created_by VARCHAR(255),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE documents (
                    document_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id),
                    title VARCHAR(255) NOT NULL DEFAULT '',
                    source_type VARCHAR(50) NOT NULL DEFAULT 'upload',
                    source_uri TEXT,
                    mime_type VARCHAR(100),
                    size_bytes BIGINT,
                    status VARCHAR(50) NOT NULL DEFAULT 'uploaded',
                    progress NUMERIC(5,2) NOT NULL DEFAULT 0,
                    error TEXT,
                    content TEXT,
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    disabled_at TIMESTAMPTZ,
                    disabled_by VARCHAR(255),
                    archived BOOLEAN NOT NULL DEFAULT FALSE,
                    archived_reason VARCHAR(255),
                    archived_by VARCHAR(255),
                    archived_at TIMESTAMPTZ,
                    process_rule_id VARCHAR(255),
                    word_count INTEGER DEFAULT 0,
                    segment_count INTEGER DEFAULT 0,
                    tokens INTEGER DEFAULT 0,
                    batch VARCHAR(255),
                    doc_type VARCHAR(50),
                    doc_form VARCHAR(50) DEFAULT 'text_model',
                    doc_language VARCHAR(50),
                    confluence_page_id VARCHAR(255),
                    confluence_binding_id VARCHAR(255),
                    confluence_version INTEGER,
                    confluence_web_url TEXT,
                    current_version INTEGER DEFAULT 1,
                    version_count INTEGER DEFAULT 1,
                    created_by VARCHAR(255),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    started_at TIMESTAMPTZ,
                    completed_at TIMESTAMPTZ,
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
                );
                CREATE TABLE segments (
                    segment_id VARCHAR(255) PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id),
                    document_id VARCHAR(255) NOT NULL REFERENCES documents(document_id),
                    position INTEGER NOT NULL DEFAULT 0,
                    text TEXT NOT NULL,
                    token_count INTEGER NOT NULL DEFAULT 0,
                    vector_id VARCHAR(255),
                    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
                    content_type VARCHAR(50) NOT NULL DEFAULT 'text',
                    image_url TEXT,
                    image_attachment_id VARCHAR(100),
                    image_filename VARCHAR(255),
                    image_media_type VARCHAR(100),
                    image_file_size INTEGER,
                    has_images BOOLEAN NOT NULL DEFAULT FALSE,
                    image_count INTEGER NOT NULL DEFAULT 0,
                    vlm_description TEXT,
                    source_type VARCHAR(50) DEFAULT 'unknown',
                    source_reference JSONB NOT NULL DEFAULT '{}'::jsonb,
                    citation_text VARCHAR(500) DEFAULT '',
                    page_number INTEGER,
                    section_header VARCHAR(500) DEFAULT '',
                    language VARCHAR(10) DEFAULT 'en',
                    contextual_prefix TEXT DEFAULT '',
                    content_hash VARCHAR(64),
                    enabled BOOLEAN NOT NULL DEFAULT TRUE,
                    disabled_at TIMESTAMPTZ,
                    disabled_by VARCHAR(255),
                    status VARCHAR(50) DEFAULT 'completed',
                    hit_count INTEGER NOT NULL DEFAULT 0,
                    word_count INTEGER DEFAULT 0,
                    keywords JSONB DEFAULT '[]'::jsonb,
                    answer TEXT,
                    index_node_id VARCHAR(255),
                    index_node_hash VARCHAR(255),
                    created_by VARCHAR(255),
                    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
                    level INTEGER DEFAULT 3,
                    parent_segment_id VARCHAR(255),
                    summary TEXT,
                    page_start INTEGER,
                    page_end INTEGER
                );
                CREATE TABLE dataset_permissions (
                    id BIGSERIAL PRIMARY KEY,
                    dataset_id VARCHAR(255) NOT NULL REFERENCES datasets(dataset_id),
                    subject_type VARCHAR(50) NOT NULL,
                    subject_id VARCHAR(255) NOT NULL,
                    permission VARCHAR(50) NOT NULL,
                    UNIQUE(dataset_id, subject_type, subject_id)
                );
                CREATE TABLE users (
                    user_id VARCHAR(255) PRIMARY KEY,
                    tenant_id VARCHAR(255) NOT NULL,
                    roles VARCHAR(50)[] NOT NULL DEFAULT ARRAY['user']::VARCHAR(50)[],
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
                """
            )
            await connection.execute(SKILL_MIGRATION.read_text(encoding="utf-8"))
            await connection.execute(DOMAIN_MIGRATION.read_text(encoding="utf-8"))
            sql = BINDING_MIGRATION.read_text(encoding="utf-8")
            await connection.execute(sql)
            await connection.execute(sql)
            await connection.execute(
                """
                CREATE TABLE public.assistant_skill_version_revocations AS
                SELECT * FROM assistant.assistant_skill_version_revocations
                WITH NO DATA
                """
            )
            revision_sql = CONTENT_REVISION_MIGRATION.read_text(encoding="utf-8")
            await connection.execute(revision_sql)
            await connection.execute(revision_sql)
            await connection.executemany(
                "INSERT INTO users (user_id, tenant_id) VALUES ($1, $2)",
                [
                    ("owner-a", "tenant-a"),
                    ("owner-c", "tenant-a"),
                    ("dataset-owner", "tenant-a"),
                    ("owner-b", "tenant-b"),
                ],
            )
        yield pool
    finally:
        await pool.close()
        admin = await asyncpg.connect(**config)
        await admin.execute(f'DROP DATABASE "{database_name}"')
        await admin.close()


def _holder(pool: asyncpg.Pool) -> SimpleNamespace:
    return SimpleNamespace(enabled=True, _pool=pool)


class MutableKnowledgeResolver:
    """Test double for the KS authorization endpoint."""

    def __init__(self) -> None:
        self.allowed: set[str] = set()
        self.calls: list[dict[str, Any]] = []

    async def resolve(self, **kwargs: Any) -> list[dict[str, Any]]:
        self.calls.append(kwargs)
        return [
            dict(binding)
            for binding in kwargs["bindings"]
            if binding["dataset_id"] in self.allowed
        ]


def _skill_content(name: str, instruction: str) -> str:
    return f"""---
name: {name}
title: Report Helper
description: Produces a bounded report
version: 1.0.0
generated: false
enabled: true
permissions:
  - knowledge:read
---
# Instructions
{instruction}
"""


async def _create_skill(
    repository: DatabaseSkillArtifactRepository,
    *,
    tenant_id: str,
    user_id: str,
    instruction: str,
) -> dict:
    content, manifest = parse_user_skill_md(_skill_content("report-helper", instruction))
    manifest = replace(
        manifest,
        generated=False,
        enabled=True,
        lifecycle_status="active",
    )
    return await repository.create_version(
        tenant_id=tenant_id,
        user_id=user_id,
        content=content,
        manifest=manifest,
        created_by=user_id,
    )


@pytest.mark.asyncio
async def test_skill_versions_are_immutable_exact_and_tenant_scoped(
    binding_pool: asyncpg.Pool,
) -> None:
    repository = DatabaseSkillArtifactRepository(_holder(binding_pool))
    first = await _create_skill(
        repository,
        tenant_id="tenant-a",
        user_id="owner-a",
        instruction="FIRST IMMUTABLE CONTENT",
    )
    second = await _create_skill(
        repository,
        tenant_id="tenant-a",
        user_id="owner-a",
        instruction="SECOND CONTENT",
    )
    tenant_b = await _create_skill(
        repository,
        tenant_id="tenant-b",
        user_id="owner-b",
        instruction="TENANT B CONTENT",
    )
    same_tenant_other_user = await _create_skill(
        repository,
        tenant_id="tenant-a",
        user_id="owner-c",
        instruction="SAME TENANT OTHER USER CONTENT",
    )

    old = await repository.authorize_version(
        tenant_id="tenant-a",
        user_id="owner-a",
        version_id=first["version_id"],
    )
    assert "FIRST IMMUTABLE CONTENT" in old["content"]
    assert old["version_id"] != second["version_id"]
    assert tenant_b["name"] == first["name"]

    with pytest.raises(SkillArtifactConflictError):
        await repository.update_metadata(
            tenant_id="tenant-a",
            user_id="owner-a",
            name="report-helper",
            changes={"permissions": ["exec:shell"]},
            updated_by="owner-a",
        )

    with pytest.raises(SkillArtifactUnavailableError):
        await repository.authorize_version(
            tenant_id="tenant-b",
            user_id="owner-b",
            version_id=first["version_id"],
        )
    with pytest.raises(SkillArtifactUnavailableError):
        await repository.authorize_version(
            tenant_id="tenant-a",
            user_id="owner-a",
            version_id=same_tenant_other_user["version_id"],
        )

    async with binding_pool.acquire() as connection:
        with pytest.raises(asyncpg.ObjectNotInPrerequisiteStateError):
            await connection.execute(
                "UPDATE assistant_skill_versions SET content = 'drift' WHERE version_id = $1",
                uuid.UUID(first["version_id"]),
            )


@pytest.mark.asyncio
async def test_agent_version_seals_exact_skill_and_current_disable_revokes_run(
    binding_pool: asyncpg.Pool,
) -> None:
    skill_repository = DatabaseSkillArtifactRepository(_holder(binding_pool))
    artifact = await _create_skill(
        skill_repository,
        tenant_id="tenant-a",
        user_id="owner-a",
        instruction="BOUND VERSION CONTENT",
    )
    repository = DatabaseAgentRepository(_holder(binding_pool))
    spec = {
        "schema_version": "agent-spec/v1",
        "identity": {},
        "instructions": "Use only the exact Skill version.",
        "model": {"model_id": "qwen3.7-plus"},
        "capabilities": [
            {
                "type": "skill",
                "resource_id": artifact["version_id"],
                "config": {"risk": "low"},
            }
        ],
        "knowledge": [],
        "memory": {},
    }
    agent = await repository.create_agent(
        tenant_id="tenant-a",
        user_id="owner-a",
        name=f"Skill Agent {uuid.uuid4().hex[:8]}",
        slug=None,
        description="",
        spec=spec,
    )
    version = await repository.create_version(
        tenant_id="tenant-a",
        agent_id=str(agent["agent_id"]),
        user_id="owner-a",
        is_tenant_admin=False,
        expected_revision=1,
    )
    async with binding_pool.acquire() as connection:
        sealed = await connection.fetchrow(
            """
            SELECT skill_name, skill_version_id, content_hash
            FROM agent_version_skill_bindings
            WHERE tenant_id = 'tenant-a' AND agent_version_id = $1
            """,
            version["agent_version_id"],
        )
    assert str(sealed["skill_version_id"]) == artifact["version_id"]
    assert sealed["skill_name"] == "report-helper"
    assert sealed["content_hash"] == artifact["content_hash"]

    await skill_repository.set_enabled(
        tenant_id="tenant-a",
        user_id="owner-a",
        name="report-helper",
        enabled=False,
    )
    with pytest.raises(SkillArtifactUnavailableError):
        await skill_repository.authorize_version(
            tenant_id="tenant-a",
            user_id="owner-a",
            version_id=artifact["version_id"],
        )
    reenabled = await skill_repository.update_metadata(
        tenant_id="tenant-a",
        user_id="owner-a",
        name="report-helper",
        changes={"title": "Re-enabled Exact Version", "enabled": True},
        updated_by="owner-a",
    )
    authorized = await skill_repository.authorize_version(
        tenant_id="tenant-a",
        user_id="owner-a",
        version_id=reenabled["version_id"],
    )
    assert authorized["title"] == "Re-enabled Exact Version"
    assert authorized["version_id"] != artifact["version_id"]
    assert authorized["content_hash"] != artifact["content_hash"]
    assert "title: Re-enabled Exact Version" in authorized["content"]


@pytest.mark.asyncio
async def test_dataset_acl_is_rechecked_at_save_publish_and_run(
    binding_pool: asyncpg.Pool,
) -> None:
    async with binding_pool.acquire() as connection:
        await connection.execute(
            """
            INSERT INTO datasets (
                dataset_id, tenant_id, name, visibility, created_by
            ) VALUES ('private-kb', 'tenant-a', 'Private KB', 'private', 'dataset-owner')
            """
        )
    resolver = MutableKnowledgeResolver()
    repository = DatabaseAgentRepository(
        _holder(binding_pool),
        knowledge_resolver=resolver,
    )
    spec = {
        "schema_version": "agent-spec/v1",
        "identity": {},
        "instructions": "Ground answers in the bound Dataset.",
        "model": {"model_id": "qwen3.7-plus"},
        "capabilities": [],
        "knowledge": [
            {
                "dataset_id": "private-kb",
                "retrieval_config": {"mode": "tool", "top_k": 4},
            }
        ],
        "memory": {},
    }
    agent_name = f"KB Agent {uuid.uuid4().hex[:8]}"
    with pytest.raises(AgentValidationError):
        await repository.create_agent(
            tenant_id="tenant-a",
            user_id="owner-a",
            name=agent_name,
            slug=None,
            description="",
            spec=spec,
        )
    resolver.allowed.add("private-kb")
    agent = await repository.create_agent(
        tenant_id="tenant-a",
        user_id="owner-a",
        name=agent_name,
        slug=None,
        description="",
        spec=spec,
    )
    version = await repository.create_version(
        tenant_id="tenant-a",
        agent_id=str(agent["agent_id"]),
        user_id="owner-a",
        is_tenant_admin=False,
        expected_revision=1,
    )
    async with binding_pool.acquire() as connection:
        sealed = await connection.fetchrow(
            """
            SELECT retrieval_config, bound_by, authorization_checked_at,
                   content_mode, historical_replayable
            FROM agent_version_knowledge_bindings
            WHERE tenant_id = 'tenant-a' AND agent_version_id = $1
            """,
            version["agent_version_id"],
        )
    retrieval_config = sealed["retrieval_config"]
    if isinstance(retrieval_config, str):
        retrieval_config = json.loads(retrieval_config)
    assert retrieval_config == {"mode": "tool", "top_k": 4}
    assert sealed["bound_by"] == "owner-a"
    assert sealed["authorization_checked_at"] is not None
    assert sealed["content_mode"] == "live_latest"
    assert sealed["historical_replayable"] is False

    resolver.allowed.clear()
    with pytest.raises(AgentValidationError):
        await repository.create_version(
            tenant_id="tenant-a",
            agent_id=str(agent["agent_id"]),
            user_id="owner-a",
            is_tenant_admin=False,
            expected_revision=1,
        )

    assert resolver.calls
    assert all(call["tenant_id"] == "tenant-a" for call in resolver.calls)
    assert all(call["user_id"] == "owner-a" for call in resolver.calls)
    assert all(call["roles"] == ["user"] for call in resolver.calls)
    assert all(call["channel"] == "authoring" for call in resolver.calls)


@pytest.mark.asyncio
async def test_knowledge_content_revision_changes_for_same_count_content_edit(
    binding_pool: asyncpg.Pool,
) -> None:
    async with binding_pool.acquire() as connection:
        await connection.execute(
            """
            INSERT INTO datasets (
                dataset_id, tenant_id, name, visibility, created_by
            ) VALUES ('revision-kb', 'tenant-a', 'Revision KB', 'private', 'owner-a')
            """
        )
        await connection.execute(
            """
            INSERT INTO documents (document_id, dataset_id, content)
            VALUES ('revision-doc', 'revision-kb', 'document-v1')
            """
        )
        await connection.execute(
            """
            INSERT INTO segments (
                segment_id, dataset_id, document_id, text
            ) VALUES ('revision-segment', 'revision-kb', 'revision-doc', 'segment-v1')
            """
        )
        before = await connection.fetchrow(
            """
            SELECT content_revision, updated_at,
                   (SELECT COUNT(*) FROM documents WHERE dataset_id = 'revision-kb') AS documents,
                   (SELECT COUNT(*) FROM segments WHERE dataset_id = 'revision-kb') AS segments
            FROM datasets WHERE dataset_id = 'revision-kb'
            """
        )
        await connection.execute(
            "UPDATE segments SET text = 'segment-v2' WHERE segment_id = 'revision-segment'"
        )
        after = await connection.fetchrow(
            """
            SELECT content_revision, updated_at,
                   (SELECT COUNT(*) FROM documents WHERE dataset_id = 'revision-kb') AS documents,
                   (SELECT COUNT(*) FROM segments WHERE dataset_id = 'revision-kb') AS segments
            FROM datasets WHERE dataset_id = 'revision-kb'
            """
        )
        assert after["content_revision"] == before["content_revision"] + 1
        assert after["updated_at"] == before["updated_at"]
        assert after["documents"] == before["documents"] == 1
        assert after["segments"] == before["segments"] == 1

        await connection.execute(
            """
            UPDATE segments
            SET hit_count = hit_count + 1, updated_at = NOW()
            WHERE segment_id = 'revision-segment'
            """
        )
        telemetry_only = await connection.fetchval(
            "SELECT content_revision FROM datasets WHERE dataset_id = 'revision-kb'"
        )
        assert telemetry_only == after["content_revision"]

        await connection.execute(
            """
            UPDATE documents
            SET progress = 45,
                error = 'transient ingestion status',
                started_at = NOW(),
                completed_at = NOW(),
                word_count = 99,
                segment_count = 99,
                tokens = 99,
                size_bytes = 999,
                updated_at = NOW()
            WHERE document_id = 'revision-doc'
            """
        )
        await connection.execute(
            """
            UPDATE segments
            SET token_count = 99,
                word_count = 99,
                image_count = 99,
                content_hash = repeat('a', 64),
                index_node_hash = 'derived-index-hash',
                updated_at = NOW()
            WHERE segment_id = 'revision-segment'
            """
        )
        derived_telemetry_only = await connection.fetchval(
            "SELECT content_revision FROM datasets WHERE dataset_id = 'revision-kb'"
        )
        assert derived_telemetry_only == telemetry_only

        await connection.execute(
            """
            UPDATE segments
            SET level = 2,
                parent_segment_id = 'revision-parent',
                summary = 'hierarchical summary',
                page_start = 1,
                page_end = 2
            WHERE segment_id = 'revision-segment'
            """
        )
        after_hierarchy_edit = await connection.fetchval(
            "SELECT content_revision FROM datasets WHERE dataset_id = 'revision-kb'"
        )
        assert after_hierarchy_edit == derived_telemetry_only + 1

        await connection.execute(
            """
            INSERT INTO segments (segment_id, dataset_id, document_id, text)
            VALUES
                ('revision-segment-2', 'revision-kb', 'revision-doc', 'segment-2'),
                ('revision-segment-3', 'revision-kb', 'revision-doc', 'segment-3')
            """
        )
        after_bulk_insert = await connection.fetchval(
            "SELECT content_revision FROM datasets WHERE dataset_id = 'revision-kb'"
        )
        assert after_bulk_insert == after_hierarchy_edit + 1

        await connection.execute(
            """
            DELETE FROM segments
            WHERE segment_id IN ('revision-segment-2', 'revision-segment-3')
            """
        )
        after_bulk_delete = await connection.fetchval(
            "SELECT content_revision FROM datasets WHERE dataset_id = 'revision-kb'"
        )
        assert after_bulk_delete == after_bulk_insert + 1

        await connection.execute(
            "UPDATE documents SET content = 'document-v2' WHERE document_id = 'revision-doc'"
        )
        after_document_edit = await connection.fetchval(
            "SELECT content_revision FROM datasets WHERE dataset_id = 'revision-kb'"
        )
        assert after_document_edit == after_bulk_delete + 1

        await connection.execute(
            "UPDATE documents SET archived = TRUE WHERE document_id = 'revision-doc'"
        )
        after_archive = await connection.fetchval(
            "SELECT content_revision FROM datasets WHERE dataset_id = 'revision-kb'"
        )
        assert after_archive == after_document_edit + 1

        await connection.execute(
            "UPDATE segments SET enabled = FALSE WHERE segment_id = 'revision-segment'"
        )
        after_disable = await connection.fetchval(
            "SELECT content_revision FROM datasets WHERE dataset_id = 'revision-kb'"
        )
        assert after_disable == after_archive + 1

        await connection.execute(
            "UPDATE documents SET updated_at = NOW() WHERE document_id = 'revision-doc'"
        )
        timestamp_only = await connection.fetchval(
            "SELECT content_revision FROM datasets WHERE dataset_id = 'revision-kb'"
        )
        assert timestamp_only == after_disable
