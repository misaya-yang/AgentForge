"""Verify the R3 public Agent deadline on an isolated authority database.

Set AGENT_PUBLICATION_EXPIRY_DB_LIVE=1 with the local Compose PostgreSQL running.
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
    os.getenv("AGENT_PUBLICATION_EXPIRY_DB_LIVE") != "1",
    reason="isolated PostgreSQL Agent publication migration opt-in",
)


@pytest.mark.asyncio
async def test_publication_deadline_is_nullable_and_authority_fingerprint_matches() -> None:
    env = dotenv_values(Path(__file__).resolve().parents[2] / ".env")
    params = {
        "host": "127.0.0.1",
        "port": int(env.get("POSTGRES_PORT") or 5432),
        "user": env["POSTGRES_USER"],
        "password": env["POSTGRES_PASSWORD"],
    }
    name = "agent_pub_expiry_ref_" + uuid.uuid4().hex[:10]
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
        expected = json.loads((directory / "007_verification.json").read_text())
        assert await compute_fingerprints(
            conn, role_prefix=prefix, reference_sets=baseline.reference_data,
        ) == expected["fingerprints"]

        column = await conn.fetchrow(
            """SELECT data_type, is_nullable, column_default
               FROM information_schema.columns
               WHERE table_schema='gateway' AND table_name='agent_publications'
                 AND column_name='expires_at'"""
        )
        assert dict(column) == {
            "data_type": "timestamp with time zone",
            "is_nullable": "YES",
            "column_default": None,
        }
    finally:
        await conn.close()
        admin = await asyncpg.connect(**params, database=env.get("POSTGRES_DB") or "gateway")
        try:
            await admin.execute(f'DROP DATABASE "{name}"')
        finally:
            await admin.close()
