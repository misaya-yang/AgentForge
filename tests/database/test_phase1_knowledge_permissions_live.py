"""Opt-in real PostgreSQL baseline/epoch and service-role security matrix.

Run PHASE1_DB_LIVE=1 with the existing local .env. Test databases contain only
synthetic rows and are retained under p1_acl_* for inspection; never print DSNs.
"""
from __future__ import annotations

import json
import os
import uuid
from pathlib import Path
from urllib.parse import quote

import asyncpg
import pytest
from dotenv import dotenv_values
from knowledge_service.persistence.database import DatabaseStorage
from knowledge_service.persistence.schema_compatibility import require_compatible_schema

from database.authority.bootstrap import (
    fresh_install,
    provision_extensions_admin,
    provision_roles_admin,
)
from database.authority.commands import (
    command_init_fresh,
    command_migrate,
    command_verify,
    default_paths,
    load_baseline,
)
from database.authority.manifest import load_epoch_manifest
from database.authority.runner import AuthorityError, MigrationAuthority

pytestmark = pytest.mark.skipif(os.getenv("PHASE1_DB_LIVE") != "1", reason="PHASE1_DB_LIVE=1 required for isolated PostgreSQL")


@pytest.mark.parametrize("prefix", ["ai_gateway_", "p1ref_"])
async def test_epoch_role_binding_privileges_and_repeatability(prefix):
    env = dotenv_values(".env")
    kwargs = {"host": "127.0.0.1", "port": int(env.get("POSTGRES_PORT") or 5432),
              "user": env["POSTGRES_USER"], "password": env["POSTGRES_PASSWORD"]}
    name = "p1_acl_" + uuid.uuid4().hex[:10]
    admin = await asyncpg.connect(**kwargs, database=env.get("POSTGRES_DB") or "gateway")
    await admin.execute(f'CREATE DATABASE "{name}"')
    await admin.close()
    conn = await asyncpg.connect(**kwargs, database=name)
    paths = default_paths()
    await provision_roles_admin(conn, paths, prefix)
    await provision_extensions_admin(conn, paths)
    baseline, sha = load_baseline(paths, "2026_08_post_kb_v1")
    await fresh_install(conn, paths, baseline, sha, role_prefix=prefix)
    with pytest.raises(RuntimeError, match="epoch"):
        await require_compatible_schema(conn)
    await conn.execute("INSERT INTO knowledge.datasets(dataset_id,name,tenant_id,collection_name) VALUES ('a','A','tenant-a','a'),('b','B','tenant-b','b')")
    grant_id = await conn.fetchval("INSERT INTO knowledge.dataset_permissions(dataset_id,subject_type,subject_id,permission) VALUES ('a','role','admin','owner') RETURNING id")
    await conn.execute("INSERT INTO knowledge.dataset_permissions(dataset_id,subject_type,subject_id,permission) VALUES ('a','user','shared-user','viewer')")
    dsn = f"postgresql://{quote(kwargs['user'],safe='')}:{quote(kwargs['password'],safe='')}@127.0.0.1:{kwargs['port']}/{name}"
    authority = MigrationAuthority(dsn, paths, role_prefix=prefix)
    manifest = load_epoch_manifest(paths.epoch_dir(baseline.baseline_id) / "manifest.yml")
    # An invalid source grant must roll back DDL, data and ledger together.
    await conn.execute("INSERT INTO knowledge.dataset_permissions(dataset_id,subject_type,subject_id,permission) VALUES ('a','unknown','bad','viewer')")
    with pytest.raises(AuthorityError):
        await authority.apply_epoch(conn, manifest, paths.epoch_dir(baseline.baseline_id))
    assert not await conn.fetchval("SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_schema='knowledge' AND table_name='dataset_permissions' AND column_name='subject_tenant_id')")
    await conn.execute("DELETE FROM knowledge.dataset_permissions WHERE subject_type='unknown'")
    await command_migrate(authority, log=lambda _: None)
    await require_compatible_schema(conn)
    row = await conn.fetchrow("SELECT * FROM knowledge.dataset_permissions WHERE id=$1", grant_id)
    assert row["subject_type"] == "tenant_role" and row["subject_tenant_id"] == "tenant-a"
    assert not await conn.fetchval("SELECT EXISTS(SELECT 1 FROM knowledge.dataset_permissions WHERE subject_type='role')")
    for kind, tenant in [("role", None), ("tenant_role", None), ("tenant_role", ""), ("tenant_role", "tenant-b")]:
        with pytest.raises(asyncpg.IntegrityConstraintViolationError):
            async with conn.transaction():
                await conn.execute("INSERT INTO knowledge.dataset_permissions(dataset_id,subject_type,subject_id,permission,subject_tenant_id) VALUES ('a',$1,'bad','viewer',$2)", kind, tenant)
    for principal in ("knowledge_api", "knowledge_worker"):
        for table in ("users", "user_roles", "user_permissions", "rbac_roles", "role_permissions"):
            assert not await conn.fetchval("SELECT has_table_privilege($1,$2,'SELECT,INSERT,UPDATE,DELETE')", prefix + principal, "gateway." + table)
            with pytest.raises(asyncpg.InsufficientPrivilegeError):
                async with conn.transaction():
                    await conn.execute(f'SET LOCAL ROLE "{prefix}{principal}"')
                    await conn.fetch(f'SELECT * FROM gateway."{table}" LIMIT 1')
        assert await conn.fetchval("SELECT has_table_privilege($1,'gateway.audit_logs','INSERT')", prefix + principal)
    async def initialize(c):
        await c.execute(f'SET ROLE "{prefix}knowledge_api"')
        await c.execute("SET search_path=pg_catalog,knowledge,gateway,public")
    pool = await asyncpg.create_pool(**kwargs, database=name, min_size=1, max_size=1, setup=initialize)
    storage = DatabaseStorage.__new__(DatabaseStorage)
    storage._pool = pool
    await storage.grant_dataset_permission("a", "role", "editor", "editor")
    assert (await storage.get_dataset_permission("a", "role", "editor"))["permission"] == "editor"
    listed = await storage.list_dataset_permissions("a")
    assert any(r["subject_type"] == "role" and r["subject_id"] == "editor" for r in listed)
    assert all("subject_tenant_id" not in r for r in listed)
    assert await storage.revoke_dataset_permission("a", "role", "editor")
    await pool.close()
    await command_verify(authority, log=lambda _: None)
    await command_migrate(authority, log=lambda _: None)
    await command_init_fresh(authority, log=lambda _: None)
    assert await conn.fetchval("SELECT count(*) FROM public.platform_schema_changes") == len(manifest.changes)
    assert await conn.fetchval("SELECT id FROM knowledge.dataset_permissions WHERE subject_type='tenant_role'") == grant_id
    # Restoring a removed permission is detectable drift, not an epoch exception.
    await conn.execute(f'GRANT SELECT ON gateway.users TO "{prefix}knowledge_api"')
    with pytest.raises(AuthorityError, match="verification failed"):
        await command_verify(authority, log=lambda _: None)
    await conn.execute(f'REVOKE SELECT ON gateway.users FROM "{prefix}knowledge_api"')
    await command_verify(authority, log=lambda _: None)
    await conn.close()
    evidence = Path("tmp/agent-platform-vnext-phase1")
    evidence.mkdir(parents=True, exist_ok=True)
    (evidence / f"postgres-{prefix}.json").write_text(json.dumps({"database": name, "role_prefix": prefix, "baseline_before_epoch": "matched", "matrix": "PASS"}) + "\n")
