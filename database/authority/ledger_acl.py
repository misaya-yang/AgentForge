"""Versioned authority-only ledger ACL bootstrap; frozen artifacts stay immutable.

No process inherits owner or gains schema CREATE. Ledger DML stays under the
connected migrator. The sole compatible business-ACL delta is proven exactly,
while ledger ACLs (excluded from frozen relation fingerprints) are checked here.
"""
from __future__ import annotations

import hashlib
import json
import re
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Any

REVISION = "authority-ledger-acl/v1"
MANIFEST_SHA256 = "e1e12e4578b3edd3fa8ed9178f4adc0ba4fc22faac1d41a68e1bffd7ee1d1850"


def manifest() -> dict[str, Any]:
    data = Path(__file__).with_name("ledger_acl_v1.json").read_bytes()
    if hashlib.sha256(data).hexdigest() != MANIFEST_SHA256:
        raise RuntimeError("authority ledger ACL manifest digest mismatch")
    return json.loads(data)


def role_names(prefix: str) -> tuple[str, str]:
    if not re.fullmatch(r"[a-z][a-z0-9_]{0,20}_", prefix):
        raise ValueError("unsafe authority role prefix")
    return prefix + "owner", prefix + "migrator"


def receipt_text(prefix: str) -> str:
    role_names(prefix)
    return json.dumps({"revision": REVISION, "manifest_sha256": MANIFEST_SHA256,
                       "role_prefix": prefix}, sort_keys=True, separators=(",", ":"))


@asynccontextmanager
async def owner_scope(conn: Any, role_prefix: str):
    """Bounded transactional scope. Rollback restores role even after SQL errors."""
    owner, _ = role_names(role_prefix)
    async with conn.transaction():
        previous = await conn.fetchval("SELECT current_setting('role')")
        await conn.execute("SELECT set_config('role', $1, true)", owner)
        yield
        # Runs only on success; failed SQL rolls back the savepoint/transaction,
        # avoiding a second failed query that would hide the original exception.
        await conn.execute("SELECT set_config('role', $1, true)", previous)


async def inspect(conn: Any, role_prefix: str) -> tuple[bool, list[dict[str, Any]]]:
    """Catalog-only exact matrix/receipt verification; no grants or table reads."""
    spec = manifest()
    owner, migrator = role_names(role_prefix)
    tables = list(spec["ledger_privileges"])
    rows = await conn.fetch("""
        SELECT c.relname, c.relkind::text AS relkind, pg_get_userbyid(c.relowner) AS owner,
               obj_description(c.oid, 'pg_class') AS receipt
        FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
        WHERE n.nspname='public' AND c.relname=ANY($1::text[])
        ORDER BY c.relname
    """, tables)
    schema = await conn.fetch("""
        SELECT a.privilege_type, a.is_grantable,
               pg_get_userbyid(a.grantor) AS grantor
        FROM pg_namespace n CROSS JOIN LATERAL aclexplode(n.nspacl) a
        WHERE n.nspname='public' AND a.grantee=(SELECT oid FROM pg_roles WHERE rolname=$1)
        ORDER BY a.privilege_type
    """, migrator)
    if not rows:
        if schema:
            raise RuntimeError("authority ACL partial bootstrap: schema grant without ledgers")
        return False, []
    if len(rows) != len(tables) or any(r["owner"] != owner or r["relkind"] != 'r' for r in rows):
        raise RuntimeError("authority ledger ownership/table matrix mismatch")
    marker = next(r["receipt"] for r in rows if r["relname"] == "platform_schema_baselines")
    active = marker == receipt_text(role_prefix)
    if marker is not None and not active:
        raise RuntimeError("authority ledger bootstrap receipt absent or tampered")
    acl = await conn.fetch("""
        SELECT c.relname, pg_get_userbyid(a.grantee) AS grantee,
               pg_get_userbyid(a.grantor) AS grantor, a.privilege_type, a.is_grantable
        FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
        CROSS JOIN LATERAL aclexplode(COALESCE(c.relacl,acldefault('r',c.relowner))) a
        WHERE n.nspname='public' AND c.relname=ANY($1::text[]) AND a.grantee<>c.relowner
        ORDER BY c.relname, a.grantee, a.privilege_type
    """, tables)
    column_grants = await conn.fetchval("""
        SELECT EXISTS(SELECT 1 FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid
          JOIN pg_namespace n ON n.oid=c.relnamespace
          WHERE n.nspname='public' AND c.relname=ANY($1::text[])
            AND a.attnum>0 AND NOT a.attisdropped AND cardinality(a.attacl)>0)
    """, tables)
    if column_grants:
        raise RuntimeError("authority ledger column-level grants are outside the versioned matrix")
    expected = sorted((table, migrator, owner, privilege, False)
                      for table, privileges in spec["ledger_privileges"].items()
                      for privilege in privileges) if active else []
    actual = sorted((r["relname"],r["grantee"],r["grantor"],r["privilege_type"],r["is_grantable"]) for r in acl)
    expected_schema = [("USAGE", False, owner)] if active else []
    if actual != expected or [(r["privilege_type"],r["is_grantable"],r["grantor"]) for r in schema] != expected_schema:
        raise RuntimeError("authority ledger ACL matrix incomplete or exceeds versioned grants")
    return active, [dict(r) for r in acl]


async def install(conn: Any, role_prefix: str) -> None:
    """Only called by fresh bootstrap or explicit admin repair after base proof."""
    active, _ = await inspect(conn, role_prefix)
    if active:
        return
    spec = manifest()
    _, migrator = role_names(role_prefix)
    async with owner_scope(conn, role_prefix):
        await conn.execute(f'GRANT USAGE ON SCHEMA public TO "{migrator}"')
        for table, privileges in spec["ledger_privileges"].items():
            await conn.execute(f'GRANT {", ".join(privileges)} ON TABLE public."{table}" TO "{migrator}"')
        # Version/digest/prefix are validated constants, not caller SQL.
        await conn.execute("COMMENT ON TABLE public.platform_schema_baselines IS '" + receipt_text(role_prefix) + "'")
    active, _ = await inspect(conn, role_prefix)
    if not active:
        raise RuntimeError("authority ledger ACL bootstrap did not verify")


async def compatible_expected(conn: Any, expected: dict[str, str], role_prefix: str) -> dict[str, str]:
    """Prove frozen ACL + exactly the immutable v1 delta; retain raw live hashes."""
    from .fingerprint import acl_lines
    from .fingerprint_values import canonical_digest
    active, _ = await inspect(conn, role_prefix)
    if not active:
        raise RuntimeError("authority ledger ACL v1 receipt is required; run explicit bootstrap")
    async with owner_scope(conn, role_prefix):
        previous = await conn.fetchval("SELECT current_setting('search_path')")
        await conn.execute("SELECT set_config('search_path','pg_catalog',true)")
        lines = await acl_lines(conn, role_prefix=role_prefix)
        await conn.execute("SELECT set_config('search_path',$1,true)", previous)
    delta = manifest()["schema_acl_delta"]
    if any(lines.count(line) != 1 for line in delta):
        raise RuntimeError("authority ACL delta is absent, duplicated, or altered")
    # Exact matrix was verified above. Remove only its exact canonical line,
    # preserving ordering and every other ACL fact before frozen-hash comparison.
    original = [line for line in lines if line not in delta]
    if canonical_digest(original) != expected["acl"]:
        raise RuntimeError("base ACL drift outside the versioned authority delta")
    return {**expected, "acl": canonical_digest(lines)}


async def receipt(conn: Any, expected: dict[str, str], role_prefix: str) -> dict[str, Any]:
    active, matrix = await inspect(conn, role_prefix)
    if not active:
        raise RuntimeError("authority ledger ACL receipt unavailable")
    derived = await compatible_expected(conn, expected, role_prefix)
    return {"revision": REVISION, "manifest_sha256": MANIFEST_SHA256,
            "role_prefix": role_prefix, "base_acl_sha256": expected["acl"],
            "effective_acl_sha256": derived["acl"], "ledger_matrix": matrix}
