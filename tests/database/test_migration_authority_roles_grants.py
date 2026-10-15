from __future__ import annotations

import hashlib
import json
import os
import re
from pathlib import Path
from typing import Any

import pytest

from database.authority import bootstrap
from database.authority.bootstrap import render_role_sql
from database.authority.constants import LOGICAL_PRINCIPALS, PLATFORM_SCHEMAS
from database.authority.runner import AuthorityError, AuthorityPaths
from scripts.inventory.generate_database_grants import (
    GrantContractError,
    generate_grants_sql,
)
from scripts.inventory.generate_database_grants import (
    main as grants_main,
)

ROOT = Path(__file__).resolve().parents[2]
ROLES_SQL = ROOT / "database/bootstrap/roles.sql"
CUTOVER_SQL = ROOT / "database/baselines/2026_08_post_kb_v1/cutover_convergence.sql"
LEGACY_DUPLICATE_RECONCILIATION_SQL = (
    ROOT / "database/bootstrap/legacy_duplicate_reconciliation.sql"
)
LEGACY_STRUCTURAL_RECONCILIATION_SQL = (
    ROOT / "database/bootstrap/legacy_structural_reconciliation.sql"
)
EXTENSIONS_SQL = ROOT / "database/bootstrap/extensions.sql"
ROLE_SUFFIXES = (
    "owner",
    "migrator",
    "gateway",
    "runtime",
    "capability_worker",
    "knowledge_api",
    "knowledge_worker",
)
APP_ROLE_SUFFIXES = ROLE_SUFFIXES[2:]
REQUIRED_FLAGS = {
    "NOINHERIT",
    "NOSUPERUSER",
    "NOCREATEDB",
    "NOCREATEROLE",
    "NOREPLICATION",
    "NOBYPASSRLS",
}


def _alter_role_flags(sql: str, role: str) -> set[str]:
    match = re.search(rf"ALTER\s+ROLE\s+{re.escape(role)}\s+(.*?);", sql, re.I | re.S)
    assert match is not None, role
    return set(re.findall(r"[A-Z]+", match.group(1).upper()))


def _assert_role_contract(sql: str) -> None:
    assert "ADMIN-ONLY" in sql
    for suffix in ROLE_SUFFIXES:
        role = f"ai_gateway_{suffix}"
        flags = _alter_role_flags(sql, role)
        assert flags >= REQUIRED_FLAGS
        assert ("NOLOGIN" if suffix == "owner" else "LOGIN") in flags

        search_path = re.search(
            rf"ALTER\s+ROLE\s+{re.escape(role)}\s+SET\s+search_path\s*=\s*([^;]+);",
            sql,
            re.I,
        )
        assert search_path is not None, role
        path = [part.strip() for part in search_path.group(1).split(",")]
        assert path[0] == "pg_catalog"
        assert path[-1] == "public"

    assert "CREATE SCHEMA IF NOT EXISTS gateway AUTHORIZATION ai_gateway_owner" in sql
    for schema in ("public", "gateway", "assistant", "knowledge"):
        assert f"ALTER SCHEMA {schema} OWNER TO ai_gateway_owner" in sql
    assert "REVOKE ALL ON SCHEMA public, gateway, assistant, knowledge FROM PUBLIC" in sql
    assert "pg_auth_members" in sql
    assert "GRANT ai_gateway_owner TO ai_gateway_migrator" in sql
    assert "GRANT CREATE ON DATABASE %I TO ai_gateway_owner" in sql
    assert "REVOKE CREATE ON DATABASE %I FROM PUBLIC" in sql


def test_roles_sql_forces_attributes_ownership_and_no_application_create() -> None:
    _assert_role_contract(ROLES_SQL.read_text(encoding="utf-8"))


def test_extensions_have_one_explicit_owner_controlled_target_schema() -> None:
    sql = EXTENSIONS_SQL.read_text(encoding="utf-8")

    for extension in ('"uuid-ossp"', "pgcrypto", "pg_trgm"):
        assert f"CREATE EXTENSION IF NOT EXISTS {extension} WITH SCHEMA public;" in sql


@pytest.mark.parametrize(
    "mutation",
    [
        lambda sql: sql.replace(" NOBYPASSRLS;", ";", 1),
        lambda sql: sql.replace("NOLOGIN NOINHERIT", "LOGIN NOINHERIT", 1),
        lambda sql: sql.replace("pg_catalog, gateway", "gateway, pg_catalog", 1),
        lambda sql: sql.replace("ALTER SCHEMA public OWNER TO ai_gateway_owner;", ""),
    ],
)
def test_roles_contract_negative_self_tests(mutation: Any) -> None:
    with pytest.raises(AssertionError):
        _assert_role_contract(mutation(ROLES_SQL.read_text(encoding="utf-8")))


class FakeRoleBootstrapConnection:
    def __init__(
        self,
        *,
        ready: bool,
        superuser: bool,
        public_create: bool = False,
        public_database_create: bool = False,
        search_path_override: str | None = None,
    ) -> None:
        self.ready = ready
        self.superuser = superuser
        self.public_create = public_create
        self.public_database_create = public_database_create
        self.search_path_override = search_path_override
        self.executed: list[str] = []

    def transaction(self) -> FakeRoleBootstrapConnection:
        return self

    async def __aenter__(self) -> FakeRoleBootstrapConnection:
        return self

    async def __aexit__(self, *_args: Any) -> None:
        return None

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        if "arc03-role-bootstrap-state" in query:
            if not self.ready:
                return []
            names = list(args[0])
            owner = next(name for name in names if name.endswith("owner"))
            rows = []
            for name in names:
                domain_path = (
                    "knowledge, gateway, assistant"
                    if name.endswith(("knowledge_api", "knowledge_worker"))
                    else "gateway, assistant, knowledge"
                )
                rows.append(
                    {
                        "rolname": name,
                        "rolcanlogin": not name.endswith("owner"),
                        "rolinherit": False,
                        "rolsuper": False,
                        "rolcreatedb": False,
                        "rolcreaterole": False,
                        "rolreplication": False,
                        "rolbypassrls": False,
                        "rolconfig": [
                            "search_path="
                            + (self.search_path_override or f"pg_catalog, {domain_path}, public")
                        ],
                        "memberships": [owner] if name.endswith("migrator") else [],
                    }
                )
            return rows
        if "arc03-schema-bootstrap-state" in query:
            if not self.ready:
                return []
            names = list(args[1])
            owner = next(name for name in names if name.endswith("owner"))
            return [
                {
                    "nspname": schema,
                    "owner": owner,
                    "public_create": self.public_create,
                    "create_roles": [owner],
                }
                for schema in args[0]
            ]
        if "arc03-default-acl-bootstrap-state" in query:
            if not self.ready:
                return []
            return [
                {
                    "nspname": schema,
                    "objtype": object_type,
                    "public_has_privilege": False,
                }
                for schema in args[1]
                for object_type in ("r", "S", "f", "T")
            ]
        raise AssertionError(f"unexpected query: {query}")

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        if "arc03-database-bootstrap-state" in query:
            if not self.ready:
                return None
            names = list(args[0])
            owner = next(name for name in names if name.endswith("owner"))
            return {
                "public_create": self.public_database_create,
                "create_roles": [owner],
            }
        raise AssertionError(f"unexpected query: {query}")

    async def fetchval(self, query: str, *_args: Any) -> bool:
        if "arc03-role-bootstrap-admin" in query or "arc03-extension-bootstrap-admin" in query:
            return self.superuser
        raise AssertionError(f"unexpected query: {query}")

    async def execute(self, query: str, *_args: Any) -> None:
        self.executed.append(query)
        self.ready = True


async def test_nonadmin_authority_verifies_preprovisioned_roles_without_role_ddl() -> None:
    conn = FakeRoleBootstrapConnection(ready=True, superuser=False)

    await bootstrap.bootstrap_roles(
        conn,
        AuthorityPaths(ROOT / "database"),
        "ai_gateway_",
    )

    assert conn.executed == []
    assert await bootstrap.role_bootstrap_issues(conn, "ai_gateway_") == []


async def test_nonadmin_authority_fails_before_role_ddl_when_bootstrap_is_incomplete() -> None:
    conn = FakeRoleBootstrapConnection(ready=False, superuser=False)

    with pytest.raises(AuthorityError, match="ADMIN-ONLY"):
        await bootstrap.bootstrap_roles(
            conn,
            AuthorityPaths(ROOT / "database"),
            "ai_gateway_",
        )

    assert conn.executed == []


async def test_role_bootstrap_rejects_public_schema_create_even_when_named_roles_are_safe() -> None:
    conn = FakeRoleBootstrapConnection(ready=True, superuser=False, public_create=True)

    issues = await bootstrap.role_bootstrap_issues(conn, "ai_gateway_")

    assert issues == [
        f"schema {schema} grants CREATE to PUBLIC" for schema in sorted(PLATFORM_SCHEMAS)
    ]


async def test_role_bootstrap_rejects_public_database_create() -> None:
    conn = FakeRoleBootstrapConnection(
        ready=True,
        superuser=False,
        public_database_create=True,
    )

    assert await bootstrap.role_bootstrap_issues(conn, "ai_gateway_") == [
        "current database grants CREATE to PUBLIC"
    ]


async def test_role_bootstrap_rejects_unexpected_search_path_schema() -> None:
    conn = FakeRoleBootstrapConnection(
        ready=True,
        superuser=False,
        search_path_override="pg_catalog, attacker, public",
    )

    issues = await bootstrap.role_bootstrap_issues(conn, "ai_gateway_")

    assert len(issues) == len(LOGICAL_PRINCIPALS)
    assert all("search_path is" in issue and "attacker" in issue for issue in issues)


async def test_schema_authority_never_uses_its_admin_connection_for_role_ddl() -> None:
    conn = FakeRoleBootstrapConnection(ready=False, superuser=True)

    with pytest.raises(AuthorityError, match="ADMIN-ONLY"):
        await bootstrap.bootstrap_roles(
            conn,
            AuthorityPaths(ROOT / "database"),
            "ai_gateway_",
        )

    assert conn.executed == []


async def test_explicit_admin_role_provisioning_verifies_its_postcondition() -> None:
    conn = FakeRoleBootstrapConnection(ready=False, superuser=True)

    await bootstrap.provision_roles_admin(
        conn,
        AuthorityPaths(ROOT / "database"),
        "ai_gateway_",
    )

    assert len(conn.executed) == 1
    assert "ADMIN-ONLY" in conn.executed[0]
    assert set(LOGICAL_PRINCIPALS) == set(ROLE_SUFFIXES)
    assert set(PLATFORM_SCHEMAS) == {"public", "gateway", "assistant", "knowledge"}


async def test_explicit_admin_extension_provisioning_closes_public_routines(
    tmp_path: Path,
) -> None:
    paths = AuthorityPaths(tmp_path / "database")
    paths.bootstrap_dir.mkdir(parents=True)
    (paths.bootstrap_dir / "extensions.sql").write_text(
        "-- EXTENSION BOOTSTRAP\n", encoding="utf-8"
    )
    conn = FakeRoleBootstrapConnection(ready=True, superuser=True)

    await bootstrap.provision_extensions_admin(conn, paths)

    assert any("EXTENSION BOOTSTRAP" in statement for statement in conn.executed)
    assert any(
        "REVOKE EXECUTE ON ALL ROUTINES IN SCHEMA" in statement for statement in conn.executed
    )


def _assert_cutover_contract(sql: str) -> None:
    assert "n.nspname IN ('public', 'gateway', 'assistant', 'knowledge')" in sql
    assert "nspname IN ('public', 'gateway', 'assistant', 'knowledge')" in sql
    assert sql.count("dependency.classid = 'pg_class'::regclass") >= 2
    assert "dependency.classid = 'pg_type'::regclass" in sql
    assert "type_relation.relkind = 'c'" in sql
    assert "dependency.deptype = 'e'" in sql
    assert "WHEN 'p' THEN 'PROCEDURE'" in sql
    assert "WHEN 'a' THEN 'AGGREGATE'" in sql
    assert "SET search_path = pg_catalog, %I" in sql
    assert "REVOKE EXECUTE ON %s %s FROM PUBLIC" in sql
    assert sql.count("REVOKE EXECUTE ON %s %s FROM PUBLIC") >= 2
    assert "platform_routine" in sql
    # One global PostgreSQL default plus one explicit row per platform schema.
    assert sql.count("REVOKE EXECUTE ON ROUTINES FROM PUBLIC") == 5
    for schema in ("public", "gateway", "assistant", "knowledge"):
        assert f"ALTER DEFAULT PRIVILEGES FOR ROLE ai_gateway_owner IN SCHEMA {schema}" in sql
    assert sql.count("REVOKE USAGE ON TYPES FROM PUBLIC") == 5
    assert "REVOKE USAGE ON TYPE %I.%I FROM PUBLIC" in sql


def test_cutover_covers_public_types_schema_default_acl_and_definers() -> None:
    _assert_cutover_contract(CUTOVER_SQL.read_text(encoding="utf-8"))


@pytest.mark.parametrize(
    "mutation",
    [
        lambda sql: sql.replace(
            "('public', 'gateway', 'assistant', 'knowledge')",
            "('gateway', 'assistant', 'knowledge')",
        ),
        lambda sql: sql.replace("SET search_path = pg_catalog, %I", "SET search_path = %I"),
        lambda sql: sql.replace("REVOKE EXECUTE ON %s %s FROM PUBLIC", ""),
        lambda sql: sql.replace("REVOKE USAGE ON TYPES FROM PUBLIC", "", 1),
    ],
)
def test_cutover_contract_negative_self_tests(mutation: Any) -> None:
    with pytest.raises(AssertionError):
        _assert_cutover_contract(mutation(CUTOVER_SQL.read_text(encoding="utf-8")))


def _inventory_bytes() -> bytes:
    inventory = {
        "postgresql": {
            "tables": [
                {
                    "table": "sessions",
                    "static_readers": ["gateway"],
                    "static_writers": ["gateway"],
                    "function_writers": [],
                }
            ],
            "views": ["session_summaries"],
            "functions": ["append_runtime_item"],
            "types": ["runtime_state"],
            "sequences_explicit": ["runtime_event_seq"],
            "sequences_implicit_serial": [],
        }
    }
    return (json.dumps(inventory, sort_keys=True) + "\n").encode()


def _policy_bytes(inventory_bytes: bytes) -> bytes:
    objects = [
        {
            "inventory_kind": "table",
            "inventory_name": "sessions",
            "schema": "assistant",
            "name": "sessions",
            "owner": "owner",
            "evidence": ["src/session_repository.py:10"],
            "access": [
                {
                    "units": ["gateway"],
                    "roles": ["runtime"],
                    "privileges": ["SELECT", "INSERT", "UPDATE"],
                    "evidence": ["review:session-runtime"],
                }
            ],
        },
        {
            "inventory_kind": "view",
            "inventory_name": "session_summaries",
            "schema": "assistant",
            "name": "session_summaries",
            "owner": "owner",
            "evidence": ["database/view.sql:1"],
            "access": [
                {
                    "units": ["gateway"],
                    "roles": ["gateway"],
                    "privileges": ["SELECT"],
                    "evidence": ["review:gateway-session-list"],
                }
            ],
        },
        {
            "inventory_kind": "function",
            "inventory_name": "append_runtime_item",
            "schema": "assistant",
            "name": "append_runtime_item",
            "identity_arguments": "uuid, jsonb",
            "owner": "owner",
            "evidence": ["database/function.sql:1"],
            "access": [
                {
                    "units": ["gateway"],
                    "roles": ["runtime"],
                    "privileges": ["EXECUTE"],
                    "evidence": ["review:append-function"],
                }
            ],
        },
        {
            "inventory_kind": "type",
            "inventory_name": "runtime_state",
            "schema": "assistant",
            "name": "runtime_state",
            "owner": "owner",
            "evidence": ["database/type.sql:1"],
            "access": [
                {
                    "units": ["gateway"],
                    "roles": ["runtime"],
                    "privileges": ["USAGE"],
                    "evidence": ["review:runtime-state"],
                }
            ],
        },
        {
            "inventory_kind": "sequence",
            "inventory_name": "runtime_event_seq",
            "schema": "assistant",
            "name": "runtime_event_seq",
            "owner": "owner",
            "evidence": ["database/sequence.sql:1"],
            "access": [
                {
                    "units": ["gateway"],
                    "roles": ["runtime"],
                    "privileges": ["USAGE"],
                    "evidence": ["review:event-sequence"],
                }
            ],
        },
    ]
    policy = {
        "schema_version": "arc03-grants-policy/v1",
        "inventory_sha256": hashlib.sha256(inventory_bytes).hexdigest(),
        "objects": objects,
    }
    return (json.dumps(policy, sort_keys=True) + "\n").encode()


def test_grants_generation_is_deterministic_explicit_and_never_broad() -> None:
    inventory = _inventory_bytes()
    policy = _policy_bytes(inventory)

    first = generate_grants_sql(inventory, policy)
    second = generate_grants_sql(inventory, policy)

    assert first == second
    assert 'GRANT USAGE ON SCHEMA "assistant" TO "ai_gateway_runtime";' in first
    assert (
        'GRANT SELECT, INSERT, UPDATE ON TABLE "assistant"."sessions" TO "ai_gateway_runtime";'
    ) in first
    assert (
        'REVOKE ALL PRIVILEGES ON TABLE "assistant"."session_summaries" FROM "ai_gateway_gateway";'
    ) in first
    assert ('REVOKE ALL PRIVILEGES ON SCHEMA "public" FROM "ai_gateway_gateway";') in first
    assert ('REVOKE ALL PRIVILEGES ON SCHEMA "public" FROM "ai_gateway_migrator";') in first
    assert (
        'GRANT EXECUTE ON FUNCTION "assistant"."append_runtime_item"(uuid, jsonb) '
        'TO "ai_gateway_runtime";'
    ) in first
    assert "GRANT ALL" not in first
    assert "GRANT CREATE" not in first
    assert " TO PUBLIC" not in first


def test_grants_generation_lists_all_unresolved_owner_mappings() -> None:
    inventory = _inventory_bytes()
    empty_policy = (
        json.dumps(
            {
                "schema_version": "arc03-grants-policy/v1",
                "inventory_sha256": hashlib.sha256(inventory).hexdigest(),
                "objects": [],
            },
            sort_keys=True,
        )
        + "\n"
    ).encode()

    with pytest.raises(GrantContractError) as exc_info:
        generate_grants_sql(inventory, empty_policy)

    assert exc_info.value.unresolved == sorted(
        [
            "missing owner mapping for function:append_runtime_item",
            "missing owner mapping for sequence:runtime_event_seq",
            "missing owner mapping for table:sessions",
            "missing owner mapping for type:runtime_state",
            "missing owner mapping for view:session_summaries",
        ]
    )


def test_grants_generation_rejects_unproven_generic_write_privilege() -> None:
    inventory = _inventory_bytes()
    policy = json.loads(_policy_bytes(inventory))
    table = next(row for row in policy["objects"] if row["inventory_kind"] == "table")
    table["access"][0]["privileges"] = ["SELECT"]

    with pytest.raises(GrantContractError, match="exact DML must be reviewed"):
        generate_grants_sql(inventory, (json.dumps(policy, sort_keys=True) + "\n").encode())


def test_grants_cli_never_writes_partial_sql_when_policy_is_blocked(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    inventory = tmp_path / "inventory.json"
    policy = tmp_path / "policy.json"
    output = tmp_path / "grants.sql"
    inventory.write_bytes(_inventory_bytes())
    policy.write_text(
        json.dumps(
            {
                "schema_version": "arc03-grants-policy/v1",
                "inventory_sha256": hashlib.sha256(inventory.read_bytes()).hexdigest(),
                "objects": [],
            }
        ),
        encoding="utf-8",
    )

    assert (
        grants_main(
            ["--inventory", str(inventory), "--policy", str(policy), "--output", str(output)]
        )
        == 2
    )
    assert not output.exists()
    payload = json.loads(capsys.readouterr().err)
    assert payload["status"] == "BLOCKED"
    assert len(payload["unresolved"]) == 5


def test_grants_cli_check_detects_stale_or_hand_edited_sql(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    inventory = tmp_path / "inventory.json"
    policy = tmp_path / "policy.json"
    grants = tmp_path / "grants.sql"
    inventory.write_bytes(_inventory_bytes())
    policy.write_bytes(_policy_bytes(inventory.read_bytes()))
    grants.write_text(
        generate_grants_sql(inventory.read_bytes(), policy.read_bytes()),
        encoding="utf-8",
    )

    assert (
        grants_main(
            ["--inventory", str(inventory), "--policy", str(policy), "--check", str(grants)]
        )
        == 0
    )
    grants.write_text(grants.read_text(encoding="utf-8") + "-- hand edit\n", encoding="utf-8")
    assert (
        grants_main(
            ["--inventory", str(inventory), "--policy", str(policy), "--check", str(grants)]
        )
        == 2
    )
    assert json.loads(capsys.readouterr().err)["status"] == "BLOCKED"


def test_legacy_duplicate_reconciliation_is_lossless_and_fail_closed() -> None:
    sql = LEGACY_DUPLICATE_RECONCILIATION_SQL.read_text(encoding="utf-8")
    assert "sessions overlap" in sql
    assert "session_memory overlap" in sql
    assert "assistant_runs overlap" in sql
    assert "unexpected data" in sql
    assert "platform_legacy" in sql
    assert "ALTER COLUMN tool_name TYPE VARCHAR(160)" in sql
    assert "REFERENCES knowledge.datasets(dataset_id)" in sql
    assert "live archive dependencies remain" in sql
    assert "DROP TABLE" not in sql.upper()
    assert "TRUNCATE" not in sql.upper()


def test_legacy_structural_reconciliation_matches_frozen_scope() -> None:
    sql = LEGACY_STRUCTURAL_RECONCILIATION_SQL.read_text(encoding="utf-8")
    assert sql.count("DROP CONSTRAINT IF EXISTS") == 90
    assert sql.count("ADD CONSTRAINT") == 90
    assert sql.count("DROP INDEX IF EXISTS") == 10
    assert sql.count("CREATE INDEX") + sql.count("CREATE UNIQUE INDEX") == 10
    assert "DROP TABLE" not in sql.upper()
    assert "TRUNCATE" not in sql.upper()


@pytest.mark.asyncio
async def test_roles_and_cutover_contract_on_explicit_scratch_postgres() -> None:
    dsn = os.environ.get("ARC03_SCRATCH_DATABASE_URL")
    if not dsn:
        pytest.skip("ARC03_SCRATCH_DATABASE_URL is not configured")
    asyncpg = pytest.importorskip("asyncpg")
    prefix = f"arc03t{os.getpid() % 100000}_"
    cutover = render_role_sql(CUTOVER_SQL.read_text(encoding="utf-8"), prefix)
    conn = await asyncpg.connect(dsn)
    transaction = conn.transaction()
    await transaction.start()
    try:
        await bootstrap.provision_roles_admin(
            conn,
            AuthorityPaths(ROOT / "database"),
            prefix,
        )
        roles = await conn.fetch(
            "SELECT rolname, rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, "
            "rolinherit, rolreplication, rolbypassrls, rolconfig FROM pg_roles "
            "WHERE rolname = ANY($1::text[]) ORDER BY rolname",
            [prefix + suffix for suffix in ROLE_SUFFIXES],
        )
        assert len(roles) == len(ROLE_SUFFIXES)
        for role in roles:
            assert role["rolcanlogin"] is (role["rolname"] != prefix + "owner")
            assert not role["rolsuper"]
            assert not role["rolcreatedb"]
            assert not role["rolcreaterole"]
            assert not role["rolinherit"]
            assert not role["rolreplication"]
            assert not role["rolbypassrls"]
            search_path = next(
                value for value in role["rolconfig"] if value.startswith("search_path=")
            )
            assert search_path.removeprefix("search_path=").split(", ")[0] == "pg_catalog"
            assert search_path.endswith("public")

        for suffix in (*APP_ROLE_SUFFIXES, "migrator"):
            role = prefix + suffix
            for schema in ("public", "gateway", "assistant", "knowledge"):
                assert not await conn.fetchval(
                    "SELECT has_schema_privilege($1, $2, 'CREATE')", role, schema
                )
            assert not await conn.fetchval(
                "SELECT has_database_privilege($1, current_database(), 'CREATE')", role
            )
        assert await conn.fetchval(
            "SELECT has_database_privilege($1, current_database(), 'CREATE')",
            prefix + "owner",
        )

        assert await conn.fetchval(
            "SELECT pg_has_role($1, $2, 'MEMBER')",
            prefix + "migrator",
            prefix + "owner",
        )
        assert await conn.fetchval(
            "SELECT pg_has_role($1, $2, 'SET')",
            prefix + "migrator",
            prefix + "owner",
        )
        assert not await conn.fetchval(
            "SELECT pg_has_role($1, $2, 'USAGE')",
            prefix + "migrator",
            prefix + "owner",
        )
        for suffix in APP_ROLE_SUFFIXES:
            assert not await conn.fetchval(
                "SELECT pg_has_role($1, $2, 'SET')",
                prefix + suffix,
                prefix + "owner",
            )

        session_user = await conn.fetchval("SELECT session_user")
        await conn.execute(f'SET LOCAL ROLE "{prefix}migrator"')
        identity = await conn.fetchrow("SELECT current_user, session_user")
        assert identity["current_user"] == prefix + "migrator"
        assert identity["session_user"] == session_user
        assert identity["current_user"] != identity["session_user"]
        with pytest.raises(asyncpg.InsufficientPrivilegeError):
            async with conn.transaction():
                await conn.execute("CREATE TABLE public.arc03_migrator_must_set_role (id integer)")

        await conn.execute(f'SET LOCAL ROLE "{prefix}owner"')
        identity = await conn.fetchrow("SELECT current_user, session_user")
        assert identity["current_user"] == prefix + "owner"
        assert identity["session_user"] == session_user
        for schema in ("public", "gateway", "assistant", "knowledge"):
            await conn.execute(f'CREATE TABLE "{schema}".arc03_owner_probe (id integer)')
        await conn.execute("RESET ROLE")
        owners = await conn.fetch(
            "SELECT pg_get_userbyid(c.relowner) AS owner FROM pg_class AS c "
            "JOIN pg_namespace AS n ON n.oid = c.relnamespace "
            "WHERE c.relname = 'arc03_owner_probe'"
        )
        assert {row["owner"] for row in owners} == {prefix + "owner"}
        for schema in ("public", "gateway", "assistant", "knowledge"):
            await conn.execute(f'DROP TABLE "{schema}".arc03_owner_probe')

        await conn.execute(
            "CREATE TYPE public.arc03_cutover_state AS ENUM ('ready'); "
            "CREATE FUNCTION assistant.arc03_cutover_definer() RETURNS integer "
            "LANGUAGE sql SECURITY DEFINER AS 'SELECT 1';"
        )
        await conn.execute(cutover)
        assert await conn.fetchval(
            "SELECT pg_get_userbyid(t.typowner) = $1 FROM pg_type AS t "
            "JOIN pg_namespace AS n ON n.oid = t.typnamespace "
            "WHERE n.nspname = 'public' AND t.typname = 'arc03_cutover_state'",
            prefix + "owner",
        )
        function = await conn.fetchrow(
            "SELECT pg_get_userbyid(p.proowner) AS owner, p.prosecdef, p.proconfig, "
            "has_function_privilege('public', p.oid, 'EXECUTE') AS public_execute "
            "FROM pg_proc AS p JOIN pg_namespace AS n ON n.oid = p.pronamespace "
            "WHERE n.nspname = 'assistant' AND p.proname = 'arc03_cutover_definer'"
        )
        assert function["owner"] == prefix + "owner"
        assert function["prosecdef"]
        assert "search_path=pg_catalog, assistant" in function["proconfig"]
        assert not function["public_execute"]
    finally:
        await transaction.rollback()
        await conn.close()
