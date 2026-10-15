"""KNO-01 archive contract against an isolated, newly installed database.

Set KB_DATASET_ARCHIVE_DB_LIVE=1 to run. The test creates and retains a
dedicated synthetic database; it never migrates the configured application DB.
"""

from __future__ import annotations

import json
import os
import uuid
from dataclasses import replace
from pathlib import Path

import asyncpg
import pytest
from dotenv import dotenv_values
from knowledge_service.auth.user_context import UserContext
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.persistence.database import DatabaseStorage, IndexLeaseUnavailableError
from knowledge_service.services.knowledge.dataset_service import DatasetService

from database.authority.bootstrap import (
    fresh_install,
    provision_extensions_admin,
    provision_roles_admin,
)
from database.authority.commands import default_paths, load_baseline
from database.authority.fingerprint import compute_fingerprints
from database.authority.manifest import load_epoch_manifest
from database.authority.runner import MigrationAuthority

pytestmark = pytest.mark.skipif(
    os.getenv("KB_DATASET_ARCHIVE_DB_LIVE") != "1",
    reason="isolated PostgreSQL archive matrix opt-in",
)


@pytest.mark.asyncio
async def test_dataset_archive_retains_index_acl_and_binding() -> None:
    env = dotenv_values(Path(__file__).resolve().parents[2] / ".env")
    params = {
        "host": "127.0.0.1",
        "port": int(env.get("POSTGRES_PORT") or 5432),
        "user": env["POSTGRES_USER"],
        "password": env["POSTGRES_PASSWORD"],
    }
    name = "kno_archive_live_" + uuid.uuid4().hex[:10]
    admin = await asyncpg.connect(**params, database=env.get("POSTGRES_DB") or "gateway")
    try:
        await admin.execute(f'CREATE DATABASE "{name}"')
    finally:
        await admin.close()
    conn = await asyncpg.connect(**params, database=name)
    pool = None
    try:
        paths = default_paths()
        prefix = "p1ref_"
        await provision_roles_admin(conn, paths, prefix)
        await provision_extensions_admin(conn, paths)
        baseline, baseline_sha = load_baseline(paths, "2026_08_post_kb_v1")
        await fresh_install(conn, paths, baseline, baseline_sha, role_prefix=prefix)
        directory = paths.epoch_dir(baseline.baseline_id)
        manifest = load_epoch_manifest(directory / "manifest.yml")
        first_five = replace(
            manifest, epoch=5,
            changes=tuple(change for change in manifest.changes if change.sequence <= 5),
        )
        await MigrationAuthority(
            "unused-isolated-only", paths, role_prefix=prefix,
        ).apply_epoch(conn, first_five, directory)
        expected = json.loads((directory / "005_verification.json").read_text())
        actual = await compute_fingerprints(
            conn, role_prefix=prefix, reference_sets=baseline.reference_data,
        )
        assert actual == expected["fingerprints"]

        await conn.execute("SET search_path=knowledge,public")
        await conn.execute(
            """INSERT INTO datasets (
                   dataset_id, name, tenant_id, visibility, created_by,
                   collection_name, embedding_provider, embedding_model,
                   embedding_dimension
               ) VALUES ('archive-fixture', 'Archive fixture', 'tenant-a',
                         'public', 'owner-a', 'collection-a', 'local', 'hash-384', 384)"""
        )
        await conn.execute(
            """INSERT INTO dataset_permissions
                   (dataset_id, subject_type, subject_id, permission)
               VALUES ('archive-fixture', 'user', 'editor-a', 'editor')"""
        )
        await conn.execute(
            """INSERT INTO dataset_collection_bindings (
                   dataset_id, tenant_id, collection_name,
                   embedding_provider, embedding_model, embedding_dimension,
                   state
               ) VALUES ('archive-fixture', 'tenant-a', 'collection-a',
                         'local', 'hash-384', 384, 'serving')"""
        )
        await conn.execute(
            "INSERT INTO gateway.tenants (tenant_id, name) VALUES ('tenant-a', 'Tenant A')"
        )
        await conn.execute(
            "INSERT INTO gateway.users (tenant_id, user_id) VALUES ('tenant-a', 'owner-a')"
        )
        await conn.execute("SET search_path=gateway,knowledge,public")
        agent_id, draft_id = uuid.uuid4(), uuid.uuid4()
        async with conn.transaction():
            await conn.execute(
                """INSERT INTO gateway.agents (
                       tenant_id, agent_id, slug, name, owner_id, created_by, updated_by
                   ) VALUES ('tenant-a', $1, 'archive-agent', 'Archive agent',
                             'owner-a', 'owner-a', 'owner-a')""",
                agent_id,
            )
            await conn.execute(
                """INSERT INTO gateway.agent_members (
                       tenant_id, agent_id, principal_id, role, created_by
                   ) VALUES ('tenant-a', $1, 'owner-a', 'owner', 'owner-a')""",
                agent_id,
            )
            await conn.execute(
                """INSERT INTO gateway.agent_drafts (
                       tenant_id, draft_id, agent_id, spec_hash, updated_by
                   ) VALUES ('tenant-a', $1, $2, $3, 'owner-a')""",
                draft_id, agent_id, "a" * 64,
            )
            await conn.execute(
                """INSERT INTO gateway.agent_draft_knowledge_bindings (
                       tenant_id, draft_id, dataset_id, bound_by
                   ) VALUES ('tenant-a', $1, 'archive-fixture', 'owner-a')""",
                draft_id,
            )
        await conn.execute("SET search_path=knowledge,public")
        pool = await asyncpg.create_pool(
            **params, database=name, min_size=1, max_size=3,
            server_settings={"search_path": "knowledge,public"},
        )
        db = DatabaseStorage()
        db._pool = pool
        service = DatasetService(None, db)
        owner = UserContext(user_id="owner-a", tenant_id="tenant-a")
        editor = UserContext(user_id="editor-a", tenant_id="tenant-a")

        await conn.execute(
            """INSERT INTO documents (document_id, dataset_id, title, status)
               VALUES ('document-a', 'archive-fixture', 'A', 'indexing')"""
        )
        await conn.execute(
            """INSERT INTO document_pipeline_executions (
                   execution_id, document_id, dataset_id, action, status, manifest
               ) VALUES ('execution-a', 'document-a', 'archive-fixture',
                         'reprocess', 'running',
                         '{"special_publication":{"phase":"preparing"}}'::jsonb)"""
        )
        await conn.execute(
            """UPDATE documents SET metadata =
                   '{"_document_pipeline_execution_id":"execution-a",
                     "_special_publication_generation_id":"generation-a"}'::jsonb
               WHERE document_id='document-a'"""
        )
        revision_before = (await db.get_dataset("archive-fixture"))["content_revision"]
        async with db.document_index_update_lease("archive-fixture", "document-a"):
            with pytest.raises(IndexLeaseUnavailableError):
                await service.set_dataset_archived(
                    owner, "archive-fixture", archived=True,
                )
        with pytest.raises(ValidationFailedError, match="running document execution"):
            await service.set_dataset_archived(
                owner, "archive-fixture", archived=True,
            )
        assert (await db.get_dataset("archive-fixture"))["content_revision"] == revision_before
        await conn.execute(
            "UPDATE document_pipeline_executions SET status='failed' "
            "WHERE execution_id='execution-a'"
        )
        # Old unlinked or terminal receipts must not make archive impossible.
        await conn.execute(
            """INSERT INTO document_pipeline_executions (
                   execution_id, document_id, dataset_id, action, status
               ) VALUES ('execution-orphan', 'document-a', 'archive-fixture',
                         'ingest', 'running')"""
        )
        await conn.execute(
            """INSERT INTO documents (document_id, dataset_id, title, status, metadata)
               VALUES ('document-terminal', 'archive-fixture', 'Finished', 'completed',
                       '{"_document_pipeline_execution_id":"execution-terminal"}'::jsonb)"""
        )
        await conn.execute(
            """INSERT INTO document_pipeline_executions (
                   execution_id, document_id, dataset_id, action, status
               ) VALUES ('execution-terminal', 'document-terminal', 'archive-fixture',
                         'ingest', 'running')"""
        )
        revision_before = (await db.get_dataset("archive-fixture"))["content_revision"]

        first = await service.set_dataset_archived(
            owner, "archive-fixture", archived=True,
        )
        replay = await service.set_dataset_archived(
            owner, "archive-fixture", archived=True,
        )
        assert first["is_archived"] and replay["is_archived"]
        assert await conn.fetchval(
            "SELECT status FROM document_pipeline_executions "
            "WHERE execution_id='execution-orphan'"
        ) == "error"
        assert await conn.fetchval(
            "SELECT status FROM document_pipeline_executions "
            "WHERE execution_id='execution-terminal'"
        ) == "completed"
        assert first["content_revision"] == replay["content_revision"] == revision_before + 1
        assert await db.get_dataset("archive-fixture") is None
        assert await db.list_datasets(tenant_id="tenant-a") == []
        assert await service.authorize_datasets(owner, ["archive-fixture"]) == []
        with pytest.raises(ValidationFailedError, match="not found"):
            await service.require_dataset_access(editor, "archive-fixture")
        assert [item["dataset_id"] for item in (
            await service.list_datasets_page(owner, archived_only=True)
        )["items"]] == ["archive-fixture"]
        assert (await service.list_datasets_page(editor, archived_only=True))["items"] == []
        assert await conn.fetchval(
            "SELECT count(*) FROM dataset_permissions WHERE dataset_id='archive-fixture'"
        ) == 1
        assert await conn.fetchval(
            "SELECT count(*) FROM dataset_collection_bindings "
            "WHERE dataset_id='archive-fixture' AND collection_name='collection-a'"
        ) == 1
        assert await conn.fetchval(
            "SELECT count(*) FROM gateway.agent_draft_knowledge_bindings "
            "WHERE tenant_id='tenant-a' AND dataset_id='archive-fixture'"
        ) == 1

        restored = await service.set_dataset_archived(
            owner, "archive-fixture", archived=False,
        )
        assert restored["content_revision"] == revision_before + 2
        assert restored["is_archived"] is False
        assert (await db.get_dataset("archive-fixture"))["collection_name"] == "collection-a"
        assert await service.authorize_datasets(editor, ["archive-fixture"]) == [
            "archive-fixture",
        ]
        repeated = await service.set_dataset_archived(
            owner, "archive-fixture", archived=False,
        )
        assert repeated["content_revision"] == revision_before + 2
        async with db.dataset_index_delete_lease("archive-fixture") as lease_connection:
            with pytest.raises(PermissionError, match="owner"):
                await db.set_dataset_archived(
                    "archive-fixture", archived=True, user_id="editor-a",
                    tenant_id="tenant-a", roles=["user"], tenant_admin=False,
                    connection=lease_connection,
                )

        # A prior archive implementation could strand a positive-revision
        # preparing execution. Owner restore must reopen the default read path
        # so the recovery worker can find and finish that execution.
        await conn.execute(
            """UPDATE datasets SET is_archived=TRUE, archived_at=NOW(),
                   archived_by='owner-a', content_revision=content_revision+1
               WHERE dataset_id='archive-fixture'"""
        )
        await conn.execute(
            "UPDATE document_pipeline_executions SET status='running' "
            "WHERE execution_id='execution-a'"
        )
        recovered = await service.set_dataset_archived(
            owner, "archive-fixture", archived=False,
        )
        assert recovered["is_archived"] is False
        assert await db.get_dataset("archive-fixture") is not None
        await conn.execute(
            "UPDATE document_pipeline_executions SET status='error' "
            "WHERE execution_id='execution-a'"
        )
        await conn.execute(
            """INSERT INTO dataset_permissions (
                   dataset_id, subject_type, subject_id, subject_tenant_id, permission
               ) VALUES ('archive-fixture', 'tenant_role', 'archive-owner-role',
                         'tenant-a', 'owner')"""
        )
        role_owner = UserContext(
            user_id="role-owner-a", tenant_id="tenant-a", roles=["archive-owner-role"],
        )
        role_archive = await service.set_dataset_archived(
            role_owner, "archive-fixture", archived=True,
        )
        assert role_archive["is_archived"] is True
        assert (await service.set_dataset_archived(
            role_owner, "archive-fixture", archived=False,
        ))["is_archived"] is False
    finally:
        if pool is not None:
            await pool.close()
        await conn.close()
