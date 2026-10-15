"""Verify immutable Version Preview pins on an isolated authority database."""

from __future__ import annotations

import json
import uuid
from dataclasses import replace

import asyncpg
import pytest

from database.authority.bootstrap import (
    fresh_install,
    provision_extensions_admin,
    provision_roles_admin,
)
from database.authority.commands import default_paths, load_baseline
from database.authority.fingerprint import compute_fingerprints
from database.authority.manifest import load_epoch_manifest
from database.authority.runner import MigrationAuthority
from tests.database.test_agent_studio_migrations import _postgres_config


@pytest.mark.asyncio
async def test_version_preview_shape_and_publication_pin_rollback() -> None:
    config = _postgres_config()
    database_name = "agent_version_pin_test_" + uuid.uuid4().hex[:10]
    admin = await asyncpg.connect(**config)
    try:
        await admin.execute(f'CREATE DATABASE "{database_name}"')
    finally:
        await admin.close()

    conn = await asyncpg.connect(**{**config, "database": database_name})
    try:
        paths = default_paths()
        prefix = "p1ref_"
        await provision_roles_admin(conn, paths, prefix)
        await provision_extensions_admin(conn, paths)
        baseline, baseline_sha = load_baseline(paths, "2026_08_post_kb_v1")
        await fresh_install(conn, paths, baseline, baseline_sha, role_prefix=prefix)
        directory = paths.epoch_dir(baseline.baseline_id)
        manifest = load_epoch_manifest(directory / "manifest.yml")
        authority = MigrationAuthority(
            "isolated-agent-version-pin-test", paths, role_prefix=prefix
        )
        through_shape_fix = replace(
            manifest,
            epoch=9,
            changes=tuple(change for change in manifest.changes if change.sequence <= 9),
        )
        await authority.apply_epoch(conn, through_shape_fix, directory)
        expected = json.loads((directory / "009_verification.json").read_text())
        assert await compute_fingerprints(
            conn, role_prefix=prefix, reference_sets=baseline.reference_data
        ) == expected["fingerprints"]

        # LIKE copies the deployed CHECK/NOT NULL rules without unrelated FKs.
        await conn.execute(
            "CREATE TEMP TABLE version_pin_shape "
            "(LIKE assistant.sessions INCLUDING DEFAULTS INCLUDING CONSTRAINTS)"
        )
        insert = """
            INSERT INTO version_pin_shape (
                session_id, service_id, user_id, tenant_id,
                state, history, metadata, config, status,
                agent_id, agent_version_id, agent_draft_revision,
                publication_id, channel, runtime_fingerprint, agent_spec_hash
            ) VALUES (
                $1, '__builtin_assistant__', 'builder', 'tenant-a',
                '{}'::jsonb, '[]'::jsonb, '{}'::jsonb, '{}'::jsonb, 'active',
                $2::uuid, $3::uuid, $4, $5::uuid, $6, $7, $8
            )
        """
        agent_id, version_id = uuid.uuid4(), uuid.uuid4()
        fingerprint, spec_hash = "sha256:" + "a" * 64, "sha256:" + "b" * 64
        await conn.execute(
            insert, "version-preview", agent_id, version_id, None, None,
            "preview", fingerprint, spec_hash,
        )
        await conn.execute(
            insert, "draft-preview", agent_id, None, 1, None,
            "preview", fingerprint, spec_hash,
        )
        await conn.execute(
            insert, "builtin", None, None, None, None, None, None, None,
        )
        await conn.execute(
            insert, "published", agent_id, version_id, None, uuid.uuid4(),
            "hosted", fingerprint, spec_hash,
        )
        with pytest.raises(asyncpg.CheckViolationError):
            await conn.execute(
                insert, "ambiguous-preview", agent_id, version_id, 1, None,
                "preview", fingerprint, spec_hash,
            )
        with pytest.raises(asyncpg.CheckViolationError):
            await conn.execute(
                insert, "empty-preview", agent_id, None, None, None,
                "preview", fingerprint, spec_hash,
            )
        with pytest.raises(asyncpg.CheckViolationError):
            await conn.execute(
                insert, "null-channel", agent_id, version_id, None, None,
                None, fingerprint, spec_hash,
            )

        await authority.apply_epoch(conn, manifest, directory)
        expected = json.loads((directory / "010_verification.json").read_text())
        assert await compute_fingerprints(
            conn, role_prefix=prefix, reference_sets=baseline.reference_data
        ) == expected["fingerprints"]

        # An existing hosted session retains its immutable Version pin while
        # rollback changes the Publication's pointer for future sessions.
        await conn.execute("SET search_path TO gateway, assistant, public")
        tenant_id, user_id = "pin-test-tenant", "pin-test-owner"
        agent_id, draft_id = uuid.uuid4(), uuid.uuid4()
        older_version, current_version, publication_id = (
            uuid.uuid4(), uuid.uuid4(), uuid.uuid4()
        )
        await conn.execute(
            "INSERT INTO gateway.users (tenant_id, user_id) VALUES ($1, $2)",
            tenant_id, user_id,
        )
        async with conn.transaction():
            await conn.execute(
                """
                INSERT INTO gateway.agents (
                    tenant_id, agent_id, slug, name, owner_id, created_by, updated_by
                ) VALUES ($1, $2, 'pin-test', 'pin-test', $3, $3, $3)
                """,
                tenant_id, agent_id, user_id,
            )
            await conn.execute(
                """
                INSERT INTO gateway.agent_members (
                    tenant_id, agent_id, principal_type, principal_id, role, created_by
                ) VALUES ($1, $2, 'user', $3, 'owner', $3)
                """,
                tenant_id, agent_id, user_id,
            )
        await conn.execute(
            """
            INSERT INTO gateway.agent_drafts (
                tenant_id, draft_id, agent_id, spec_hash, updated_by
            ) VALUES ($1, $2, $3, $4, $5)
            """,
            tenant_id, draft_id, agent_id, "a" * 64, user_id,
        )
        for number, version_id in enumerate((older_version, current_version), start=1):
            async with conn.transaction():
                await conn.execute(
                    """
                    INSERT INTO gateway.agent_versions (
                        tenant_id, agent_version_id, agent_id, version_number,
                        resolved_spec, spec_hash, source_draft_id,
                        source_draft_revision, created_by
                    ) VALUES ($1, $2, $3, $4, '{}'::jsonb, $5, $6, 1, $7)
                    """,
                    tenant_id, version_id, agent_id, number, "a" * 64,
                    draft_id, user_id,
                )
                await conn.execute(
                    """
                    UPDATE gateway.agent_versions SET bindings_sealed = TRUE
                    WHERE tenant_id = $1 AND agent_version_id = $2
                    """,
                    tenant_id, version_id,
                )
        await conn.execute(
            """
            INSERT INTO gateway.agent_publications (
                tenant_id, publication_id, agent_id, channel, version_id,
                created_by, updated_by
            ) VALUES ($1, $2, $3, 'hosted', $4, $5, $5)
            """,
            tenant_id, publication_id, agent_id, current_version, user_id,
        )
        await conn.execute(
            """
            INSERT INTO assistant.sessions (
                session_id, service_id, user_id, tenant_id,
                agent_id, agent_version_id, publication_id, channel,
                runtime_fingerprint, agent_spec_hash
            ) VALUES (
                'hosted-before-rollback', '__builtin_assistant__', $1, $2,
                $3, $4, $5, 'hosted', $6, $7
            )
            """,
            user_id, tenant_id, agent_id, current_version,
            publication_id, fingerprint, spec_hash,
        )
        await conn.execute(
            """
            UPDATE gateway.agent_publications SET version_id = $3
            WHERE tenant_id = $1 AND publication_id = $2
            """,
            tenant_id, publication_id, older_version,
        )
        assert await conn.fetchval(
            "SELECT agent_version_id FROM assistant.sessions "
            "WHERE session_id = 'hosted-before-rollback'"
        ) == current_version
        assert await conn.fetchval(
            """
            SELECT version_id FROM gateway.agent_publications
            WHERE tenant_id = $1 AND publication_id = $2
            """,
            tenant_id, publication_id,
        ) == older_version
    finally:
        await conn.close()
        admin = await asyncpg.connect(**config)
        try:
            await admin.execute(f'DROP DATABASE "{database_name}"')
        finally:
            await admin.close()
