"""Execute production SELECTs against a controlled in-memory relational collaborator.

SQLite exercises joins/predicates/binds, not PostgreSQL privileges or RLS.
"""
from __future__ import annotations

import sqlite3

import pytest

from src.persistence.database import DatabaseStorage


class Connection:
    def __init__(self):
        self.calls = []
        self.sql = sqlite3.connect(":memory:")
        self.sql.row_factory = sqlite3.Row
        self.sql.create_function("clock_timestamp", 0, lambda: "2026-10-13")
        self.sql.executescript("""
            CREATE TABLE users(user_id TEXT, tenant_id TEXT, email TEXT, display_name TEXT, status TEXT);
            CREATE TABLE user_roles(user_id TEXT, role_name TEXT, granted_at TEXT, expires_at TEXT);
            INSERT INTO users VALUES
                ('own', 'tenant-a', 'own@example.test', 'Own', 'active'),
                ('other', 'tenant-b', 'private@example.test', 'Other', 'active'),
                ('expired', 'tenant-a', 'expired@example.test', 'Expired', 'active');
            INSERT INTO user_roles VALUES
                ('own', 'custom', '2026-01-01', NULL),
                ('other', 'custom', '2026-01-02', NULL),
                ('expired', 'custom', '2026-01-03', '2026-10-12');
        """)

    def acquire(self):
        return self

    async def __aenter__(self):
        return self

    async def __aexit__(self, *_args):
        pass

    async def fetch(self, query, *args):
        self.calls.append((query, args))
        return self.sql.execute(query, args).fetchall()

    async def fetchval(self, query, *args):
        return (await self.fetch(query, *args))[0][0]


@pytest.fixture
def db():
    database = DatabaseStorage(dsn="", enabled=False)
    connection = Connection()
    database._pool = connection
    yield database
    connection.sql.close()


@pytest.mark.parametrize("tenant, expected", [("tenant-a", ["own"]), ("tenant-b", ["other"]), ("missing", [])])
async def test_actual_tenant_selects_exclude_other_and_expired_rows(db, tenant, expected):
    members = await db.get_tenant_users_by_role("custom", tenant)
    assert [row["user_id"] for row in members] == expected
    assert await db.get_tenant_role_user_count("custom", tenant) == len(expected)
    for query, args in db._pool.calls:
        assert args == ("custom", tenant)
        assert "u.tenant_id = $2" in query
        assert "ur.expires_at > clock_timestamp()" in query


async def test_legacy_global_methods_keep_all_assignments_for_delete_safety(db):
    assert await db.get_role_user_count("custom") == 3
    assert {r["user_id"] for r in await db.get_users_by_role("custom")} == {"own", "other", "expired"}
    assert all(args == ("custom",) for _, args in db._pool.calls)


@pytest.mark.parametrize("tenant", ["", " "])
async def test_blank_scope_never_falls_back_to_global(db, tenant):
    with pytest.raises(ValueError, match="Tenant scope"):
        await db.get_tenant_role_user_count("custom", tenant)
    with pytest.raises(ValueError, match="Tenant scope"):
        await db.get_tenant_users_by_role("custom", tenant)
    assert db._pool.calls == []


async def test_role_and_tenant_are_bound_parameters(db):
    assert await db.get_tenant_role_user_count("custom' OR 1=1 --", "tenant-a") == 0
    assert await db.get_tenant_users_by_role("custom", "tenant-a' OR 1=1 --") == []


class PermissionConnection:
    """Controlled PostgreSQL-result collaborator for existing staged grant SQL."""
    def __init__(self, grants=None, fail_at=None):
        self.grants = grants or {}
        self.fail_at = fail_at
        self.calls = []

    def acquire(self):
        return self

    async def __aenter__(self):
        return self

    async def __aexit__(self, *_args):
        pass

    async def fetch(self, query, *args):
        import asyncpg

        self.calls.append((query, args))
        if "JOIN role_permissions" in query:
            stage = "role_grants"
        elif "unnest" in query:
            stage = "role_arrays"
        elif "FROM user_permissions" in query:
            stage = "direct_grants"
        else:
            stage = "account_roles"
        if stage == self.fail_at:
            raise asyncpg.PostgresError("synthetic query failure")
        if stage == "account_roles":
            return [{"permissions": self.grants.get(stage, [])}] if self.grants.get(stage) else []
        return [{"permission_code": value} for value in self.grants.get(stage, [])]

    async def fetchrow(self, query, *args):
        self.calls.append((query, args))
        return {"roles": ["custom_reader"], "permissions": self.grants.get("account_permissions", [])}


def permission_db(connection):
    database = DatabaseStorage(dsn="", enabled=False, permission_cache_ttl_seconds=60)
    database._pool = connection
    return database


async def test_strict_permissions_ignore_stale_cache_and_never_write_cache():
    connection = PermissionConnection({"direct_grants": ["role:list"]})
    database = permission_db(connection)
    await database._set_cached_permissions("actor", ["role:*"])
    assert await database.get_user_permissions("actor") == ["role:*"]
    assert connection.calls == []
    assert await database.get_user_permissions("actor", require_current=True) == ["role:list"]
    assert await database._get_cached_permissions("actor") == ["role:*"]
    await database._invalidate_permission_cache("actor")
    assert await database.get_user_permissions("actor", require_current=True) == ["role:list"]
    assert await database._get_cached_permissions("actor") is None


@pytest.mark.parametrize("stage", ["role_grants", "role_arrays", "account_roles", "direct_grants"])
async def test_strict_permission_query_failures_do_not_fallback_or_cache(stage):
    import asyncpg

    # In particular, a direct-grant error must fail even after an earlier grant.
    connection = PermissionConnection({"role_grants": ["role:list"]} if stage == "direct_grants" else {}, fail_at=stage)
    database = permission_db(connection)
    with pytest.raises(asyncpg.PostgresError):
        await database.get_user_permissions("actor", require_current=True)
    assert await database._get_cached_permissions("actor") is None


@pytest.mark.parametrize("grants, expected", [
    ({"role_grants": ["role:list"], "role_arrays": ["role:*"], "direct_grants": ["audit:read"]}, {"role:list", "audit:read"}),
    ({"role_arrays": ["role:list"], "account_permissions": ["role:*"]}, {"role:list"}),
    ({"account_permissions": ["role:list"], "account_roles": ["audit:read"]}, {"role:list", "audit:read"}),
])
async def test_strict_permissions_keep_existing_stages_and_direct_union(grants, expected):
    database = permission_db(PermissionConnection(grants))
    assert set(await database.get_user_permissions("actor", require_current=True)) == expected
    for query, _args in database._pool.calls:
        if "FROM user_roles ur" in query or "FROM user_permissions" in query:
            assert "expires_at IS NULL OR" in query and "expires_at > NOW()" in query


async def test_strict_missing_pool_fails_while_legacy_empty_result_is_preserved():
    database = DatabaseStorage(dsn="", enabled=False)
    assert await database.get_user_permissions("actor") == []
    with pytest.raises(RuntimeError, match="storage is unavailable"):
        await database.get_user_permissions("actor", require_current=True)
