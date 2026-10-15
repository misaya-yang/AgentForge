"""AR-06 share columns and audience checks on an isolated synthetic database.

Set INTERNAL_SHARE_SCOPE_DB_LIVE=1 to run. The test retains its dedicated DB.
"""

from __future__ import annotations

import json
import os
import uuid
from pathlib import Path

import asyncpg
import pytest
from dotenv import dotenv_values

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
    os.getenv("INTERNAL_SHARE_SCOPE_DB_LIVE") != "1",
    reason="isolated PostgreSQL share-scope matrix opt-in",
)


@pytest.mark.asyncio
async def test_share_audience_columns_and_scope_checks() -> None:
    env = dotenv_values(Path(__file__).resolve().parents[2] / ".env")
    params = {
        "host": "127.0.0.1", "port": int(env.get("POSTGRES_PORT") or 5432),
        "user": env["POSTGRES_USER"], "password": env["POSTGRES_PASSWORD"],
    }
    name = "internal_share_ref_" + uuid.uuid4().hex[:10]
    admin = await asyncpg.connect(**params, database=env.get("POSTGRES_DB") or "gateway")
    try:
        await admin.execute(f'CREATE DATABASE "{name}"')
    finally:
        await admin.close()
    conn = await asyncpg.connect(**params, database=name)
    try:
        paths = default_paths()
        prefix = "p1ref_"
        await provision_roles_admin(conn, paths, prefix)
        await provision_extensions_admin(conn, paths)
        baseline, baseline_sha = load_baseline(paths, "2026_08_post_kb_v1")
        await fresh_install(conn, paths, baseline, baseline_sha, role_prefix=prefix)
        directory = paths.epoch_dir(baseline.baseline_id)
        manifest = load_epoch_manifest(directory / "manifest.yml")
        await MigrationAuthority(
            "unused-isolated-only", paths, role_prefix=prefix,
        ).apply_epoch(conn, manifest, directory)
        expected = json.loads((directory / "006_verification.json").read_text())
        assert await compute_fingerprints(
            conn, role_prefix=prefix, reference_sets=baseline.reference_data,
        ) == expected["fingerprints"]

        await conn.execute("SET search_path=assistant,gateway,knowledge,public")
        await conn.execute(
            """INSERT INTO conversation_shares
                   (share_code, session_id, user_id, tenant_id, snapshot)
               VALUES ('public-c', 'session-a', 'owner-a', 'tenant-a', '{}'::jsonb)"""
        )
        await conn.execute(
            """INSERT INTO artifact_shares (share_code, kind, tenant_id)
               VALUES ('public-a', 'quiz', 'tenant-a')"""
        )
        assert await conn.fetchrow(
            "SELECT audience, source_scope FROM conversation_shares "
            "WHERE share_code='public-c'"
        ) == ("public", None)
        assert await conn.fetchrow(
            "SELECT audience, source_scope FROM artifact_shares "
            "WHERE share_code='public-a'"
        ) == ("public", None)

        for table, fields in (
            ("conversation_shares", "session_id, user_id, tenant_id, snapshot"),
            ("artifact_shares", "kind, tenant_id"),
        ):
            values = (
                "'session-a', 'owner-a', 'tenant-a', '{}'::jsonb"
                if table == "conversation_shares" else "'quiz', 'tenant-a'"
            )
            await conn.execute(
                f"INSERT INTO {table} (share_code, {fields}, audience, source_scope) "
                f"VALUES ('internal-{table[0]}', {values}, 'internal', '{{}}'::jsonb)"
            )
            with pytest.raises(asyncpg.CheckViolationError):
                await conn.execute(
                    f"INSERT INTO {table} (share_code, {fields}, audience) "
                    f"VALUES ('noscope-{table[0]}', {values}, 'internal')"
                )
            with pytest.raises(asyncpg.CheckViolationError):
                await conn.execute(
                    f"INSERT INTO {table} (share_code, {fields}, source_scope) "
                    f"VALUES ('pscope-{table[0]}', {values}, '{{}}'::jsonb)"
                )
            with pytest.raises(asyncpg.CheckViolationError):
                await conn.execute(
                    f"INSERT INTO {table} (share_code, {fields}, audience) "
                    f"VALUES ('unknown-{table[0]}', {values}, 'other')"
                )
    finally:
        await conn.close()
