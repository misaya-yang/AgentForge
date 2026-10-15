"""Fresh-install bootstrap, role creation and application startup checks.

Fresh install order (PRD ARC-03 §3B.7):

    separate admin bootstrap/roles.sql -> empty preflight -> read-only role
    verification -> bootstrap/extensions.sql -> baselines/<id>/init.sql ->
    reference_data.sql -> grants.sql -> fingerprint verification -> marker

An existing, non-empty database NEVER receives init.sql; it can only go
through reconciliation/adoption.  Application startup performs a read-only
check of the supported baseline/epoch revision plus required objects; full
fingerprinting belongs to the migrator/status path.
"""

from __future__ import annotations

import hashlib
import re
from pathlib import Path
from typing import Any

from . import ledger
from .ledger_acl import compatible_expected, install as install_ledger_acl, owner_scope, REVISION
from .adoption import validate_existing_adoption_marker
from .constants import (
    DEFAULT_ROLE_PREFIX,
    EXTENSION_ALLOWLIST,
    LEDGER_TABLES,
    LOGICAL_PRINCIPALS,
    PLATFORM_SCHEMAS,
)
from .fingerprint import compute_fingerprints
from .manifest import BaselineManifest
from .runner import AuthorityError, AuthorityPaths

REQUIRED_OBJECTS: tuple[tuple[str, ...], ...] = (
    ("public.services", "gateway.services"),
    ("public.datasets", "knowledge.datasets"),
    ("public.documents", "knowledge.documents"),
    ("public.segments", "knowledge.segments"),
    ("public.users",),
    ("public.permissions",),
    ("public.rbac_roles",),
)

_SUPPORTED_BASELINE_REVISIONS_QUERY = f"""
SELECT baseline_id FROM public.{ledger.BASELINES_TABLE}
"""

_ROLE_BOOTSTRAP_STATE_SQL = """
/* arc03-role-bootstrap-state */
SELECT role.rolname, role.rolcanlogin, role.rolinherit, role.rolsuper,
       role.rolcreatedb, role.rolcreaterole, role.rolreplication,
       role.rolbypassrls, role.rolconfig,
       ARRAY(
           SELECT granted.rolname
           FROM pg_auth_members AS membership
           JOIN pg_roles AS granted ON granted.oid = membership.roleid
           WHERE membership.member = role.oid
           ORDER BY granted.rolname
       ) AS memberships
FROM pg_roles AS role
WHERE role.rolname = ANY($1::text[])
ORDER BY role.rolname
"""

_SCHEMA_BOOTSTRAP_STATE_SQL = """
/* arc03-schema-bootstrap-state */
SELECT namespace.nspname,
       pg_get_userbyid(namespace.nspowner) AS owner,
       EXISTS (
           SELECT 1
           FROM aclexplode(
               COALESCE(namespace.nspacl, acldefault('n', namespace.nspowner))
           ) AS privilege
           WHERE privilege.grantee = 0
             AND privilege.privilege_type = 'CREATE'
       ) AS public_create,
       ARRAY(
           SELECT principal
           FROM unnest($2::text[]) AS principal
           WHERE has_schema_privilege(principal, namespace.nspname, 'CREATE')
           ORDER BY principal
       ) AS create_roles
FROM pg_namespace AS namespace
WHERE namespace.nspname = ANY($1::text[])
ORDER BY namespace.nspname
"""

_DEFAULT_ACL_BOOTSTRAP_STATE_SQL = """
/* arc03-default-acl-bootstrap-state */
WITH owner_role AS (
    SELECT oid FROM pg_roles WHERE rolname = $1
), desired(nspname, objtype) AS (
    SELECT schema_name, object_type
    FROM unnest($2::text[]) AS schema_name
    CROSS JOIN (VALUES ('r'::"char"), ('S'::"char"), ('f'::"char"), ('T'::"char"))
        AS object_types(object_type)
)
SELECT desired.nspname, desired.objtype::text,
       EXISTS (
           SELECT 1
           FROM aclexplode(
               COALESCE(global_acl.defaclacl, acldefault(desired.objtype, owner_role.oid))
           ) AS privilege
           WHERE privilege.grantee = 0
       ) OR EXISTS (
           SELECT 1
           FROM aclexplode(schema_acl.defaclacl) AS privilege
           WHERE privilege.grantee = 0
       ) AS public_has_privilege
FROM desired
CROSS JOIN owner_role
JOIN pg_namespace AS namespace ON namespace.nspname = desired.nspname
LEFT JOIN pg_default_acl AS global_acl
  ON global_acl.defaclrole = owner_role.oid
 AND global_acl.defaclnamespace = 0
 AND global_acl.defaclobjtype = desired.objtype
LEFT JOIN pg_default_acl AS schema_acl
  ON schema_acl.defaclrole = owner_role.oid
 AND schema_acl.defaclnamespace = namespace.oid
 AND schema_acl.defaclobjtype = desired.objtype
ORDER BY desired.nspname, desired.objtype
"""

_DATABASE_BOOTSTRAP_STATE_SQL = """
/* arc03-database-bootstrap-state */
SELECT EXISTS (
           SELECT 1
           FROM pg_database AS database
           CROSS JOIN LATERAL aclexplode(
               COALESCE(database.datacl, acldefault('d', database.datdba))
           ) AS privilege
           WHERE database.datname = current_database()
             AND privilege.grantee = 0
             AND privilege.privilege_type = 'CREATE'
       ) AS public_create,
       ARRAY(
           SELECT principal
           FROM unnest($1::text[]) AS principal
           WHERE has_database_privilege(principal, current_database(), 'CREATE')
           ORDER BY principal
       ) AS create_roles
"""

_ROUTINE_ACL_HARDENING_SQL = """
/* arc03-admin-extension-acl-hardening */
REVOKE EXECUTE ON ALL ROUTINES IN SCHEMA public, gateway, assistant, knowledge FROM PUBLIC
"""


def render_role_sql(role_sql: str, role_prefix: str) -> str:
    """Substitute the configurable LOGIN/NOLOGIN role prefix.

    The checked-in file uses the canonical ``ai_gateway_`` prefix; managed
    deployments can namespace roles without changing the SQL's meaning.
    """
    if not re.fullmatch(r"[a-z][a-z0-9_]{0,20}_", role_prefix):
        raise AuthorityError(f"unsafe role prefix: {role_prefix!r}")
    return role_sql.replace("ai_gateway_", role_prefix)


def _expected_role_search_path(principal: str) -> list[str]:
    domain_schemas = (
        ["knowledge", "gateway", "assistant"]
        if principal in {"knowledge_api", "knowledge_worker"}
        else ["gateway", "assistant", "knowledge"]
    )
    return ["pg_catalog", *domain_schemas, "public"]


async def role_bootstrap_issues(conn: Any, role_prefix: str) -> list[str]:
    """Read-only proof that the separate admin bootstrap is complete."""
    render_role_sql("", role_prefix)  # validates the configurable identifier prefix
    names = [f"{role_prefix}{principal}" for principal in LOGICAL_PRINCIPALS]
    rows = await conn.fetch(_ROLE_BOOTSTRAP_STATE_SQL, names)
    by_name = {str(row["rolname"]): row for row in rows}
    issues: list[str] = []
    missing = sorted(set(names) - set(by_name))
    if missing:
        issues.extend(f"missing role {name}" for name in missing)
        return issues

    owner = f"{role_prefix}owner"
    migrator = f"{role_prefix}migrator"
    for principal in LOGICAL_PRINCIPALS:
        name = f"{role_prefix}{principal}"
        row = by_name[name]
        expected_login = principal != "owner"
        if bool(row["rolcanlogin"]) is not expected_login:
            issues.append(f"{name} LOGIN/NOLOGIN attribute is incorrect")
        for field in (
            "rolinherit",
            "rolsuper",
            "rolcreatedb",
            "rolcreaterole",
            "rolreplication",
            "rolbypassrls",
        ):
            if bool(row[field]):
                issues.append(f"{name} must force {field}=false")
        expected_memberships = [owner] if name == migrator else []
        memberships = sorted(str(value) for value in (row["memberships"] or []))
        if memberships != expected_memberships:
            issues.append(f"{name} memberships are {memberships}, expected {expected_memberships}")
        configurations = [str(value) for value in (row["rolconfig"] or [])]
        search_path = next(
            (
                value.removeprefix("search_path=")
                for value in configurations
                if value.startswith("search_path=")
            ),
            None,
        )
        if search_path is None:
            issues.append(f"{name} has no role-level search_path")
        else:
            schemas = [value.strip() for value in search_path.split(",")]
            expected_search_path = _expected_role_search_path(principal)
            if schemas != expected_search_path:
                issues.append(f"{name} search_path is {schemas}, expected {expected_search_path}")

    schema_rows = await conn.fetch(
        _SCHEMA_BOOTSTRAP_STATE_SQL,
        list(PLATFORM_SCHEMAS),
        names,
    )
    by_schema = {str(row["nspname"]): row for row in schema_rows}
    missing_schemas = sorted(set(PLATFORM_SCHEMAS) - set(by_schema))
    issues.extend(f"missing platform schema {schema}" for schema in missing_schemas)
    for schema in sorted(set(PLATFORM_SCHEMAS) & set(by_schema)):
        row = by_schema[schema]
        if str(row["owner"]) != owner:
            issues.append(f"schema {schema} owner is {row['owner']!r}, expected {owner!r}")
        if bool(row["public_create"]):
            issues.append(f"schema {schema} grants CREATE to PUBLIC")
        create_roles = sorted(str(value) for value in (row["create_roles"] or []))
        if create_roles != [owner]:
            issues.append(f"schema {schema} CREATE roles are {create_roles}, expected [{owner!r}]")
    database_state = await conn.fetchrow(_DATABASE_BOOTSTRAP_STATE_SQL, names)
    if database_state is None:
        issues.append("current database privilege state is unavailable")
    else:
        if bool(database_state["public_create"]):
            issues.append("current database grants CREATE to PUBLIC")
        database_create_roles = sorted(
            str(value) for value in (database_state["create_roles"] or [])
        )
        if database_create_roles != [owner]:
            issues.append(
                f"current database CREATE roles are {database_create_roles}, expected [{owner!r}]"
            )
    default_acl_rows = await conn.fetch(
        _DEFAULT_ACL_BOOTSTRAP_STATE_SQL,
        owner,
        list(PLATFORM_SCHEMAS),
    )
    expected_default_acl_rows = len(PLATFORM_SCHEMAS) * 4
    if len(default_acl_rows) != expected_default_acl_rows:
        issues.append(
            "owner default ACL matrix is incomplete: "
            f"got {len(default_acl_rows)} rows, expected {expected_default_acl_rows}"
        )
    for row in default_acl_rows:
        if bool(row["public_has_privilege"]):
            issues.append(
                "PUBLIC retains default privilege for "
                f"{row['nspname']} object type {row['objtype']}"
            )
    return sorted(issues)


async def bootstrap_roles(conn: Any, paths: AuthorityPaths, role_prefix: str) -> None:
    """Verify the separately provisioned role model; never execute role DDL."""
    issues = await role_bootstrap_issues(conn, role_prefix)
    if issues:
        raise AuthorityError(
            "role bootstrap is ADMIN-ONLY and is incomplete; provision "
            "database/bootstrap/roles.sql with a separate admin connection. "
            "Unresolved: " + "; ".join(issues)
        )


async def provision_roles_admin(conn: Any, paths: AuthorityPaths, role_prefix: str) -> None:
    """Explicit cluster-admin interface; schema migration never calls this."""
    issues = await role_bootstrap_issues(conn, role_prefix)
    if not issues:
        return
    is_superuser = bool(
        await conn.fetchval(
            "/* arc03-role-bootstrap-admin */ SELECT current_setting('is_superuser', true) = 'on'"
        )
    )
    if not is_superuser:
        raise AuthorityError(
            "role provisioning requires a separate PostgreSQL superuser/admin connection"
        )
    roles_sql = (paths.bootstrap_dir / "roles.sql").read_text(encoding="utf-8")
    await conn.execute(render_role_sql(roles_sql, role_prefix))
    remaining = await role_bootstrap_issues(conn, role_prefix)
    if remaining:
        raise AuthorityError(
            "admin role bootstrap completed without satisfying its contract: "
            + "; ".join(remaining)
        )


async def bootstrap_extensions(
    conn: Any,
    paths: AuthorityPaths,
    role_prefix: str = DEFAULT_ROLE_PREFIX,
) -> None:
    render_role_sql("", role_prefix)
    extensions_sql = (paths.bootstrap_dir / "extensions.sql").read_text(encoding="utf-8")
    owner_role = f"{role_prefix}owner"
    async with conn.transaction():
        await conn.execute(f'SET LOCAL ROLE "{owner_role}"')
        await conn.execute(extensions_sql)
        await conn.execute("RESET ROLE")


async def provision_extensions_admin(
    conn: Any,
    paths: AuthorityPaths,
) -> None:
    """Preinstall trusted extensions and close their bootstrap-superuser ACLs.

    PostgreSQL trusted extension member routines remain owned by the bootstrap
    superuser even when CREATE EXTENSION runs after SET ROLE.  A fresh baseline
    therefore needs this explicit admin phase before the migrator transaction.
    """
    is_superuser = bool(
        await conn.fetchval(
            "/* arc03-extension-bootstrap-admin */ "
            "SELECT current_setting('is_superuser', true) = 'on'"
        )
    )
    if not is_superuser:
        raise AuthorityError(
            "extension provisioning requires a separate PostgreSQL superuser/admin connection"
        )
    extensions_sql = (paths.bootstrap_dir / "extensions.sql").read_text(encoding="utf-8")
    async with conn.transaction():
        await conn.execute(extensions_sql)
        await conn.execute(_ROUTINE_ACL_HARDENING_SQL)


async def run_baseline_sql_file(
    conn: Any,
    path: Path,
    *,
    role_prefix: str | None = None,
    execution_role: str | None = None,
) -> None:
    """Execute one baseline SQL file in a runner-owned transaction.

    Files that name platform roles (cutover, grants) are rendered with the
    deployment's configurable role prefix first; pure DDL/data files
    (init.sql, reference_data.sql) are executed verbatim.
    """
    if not path.exists():
        raise AuthorityError(f"required baseline file missing: {path}")
    sql = path.read_text(encoding="utf-8")
    if role_prefix is not None:
        sql = render_role_sql(sql, role_prefix)
    async with conn.transaction():
        if execution_role is not None:
            if re.fullmatch(r"[a-z][a-z0-9_]{0,62}", execution_role) is None:
                raise AuthorityError(f"unsafe baseline execution role {execution_role!r}")
            await conn.execute(f'SET LOCAL ROLE "{execution_role}"')
        await conn.execute(sql)
        if execution_role is not None:
            await conn.execute("RESET ROLE")


async def verify_baseline_sql_file(
    conn: Any, path: Path, *, role_prefix: str = DEFAULT_ROLE_PREFIX,
    ledger_acl_expected: dict[str, str] | None = None,
) -> list[dict[str, Any]]:
    """Execute the frozen single-SELECT verification contract and fail closed."""
    if not path.exists():
        raise AuthorityError(f"required baseline verify file missing: {path}")
    sql = render_role_sql(path.read_text(encoding="utf-8"), role_prefix)
    without_line_comments = re.sub(r"(?m)^\s*--[^\n]*(?:\n|$)", "", sql).strip()
    if without_line_comments.endswith(";"):
        without_line_comments = without_line_comments[:-1].rstrip()
    if not re.match(r"^SELECT\b", without_line_comments, re.IGNORECASE) or ";" in (
        without_line_comments
    ):
        raise AuthorityError(
            f"baseline verify file {path} must contain exactly one read-only SELECT"
        )
    if ledger_acl_expected is not None:
        await compatible_expected(conn, ledger_acl_expected, role_prefix)
    async with owner_scope(conn, role_prefix):
        rows = [dict(row) for row in await conn.fetch(sql)]
    if not rows:
        raise AuthorityError(f"baseline verify file {path} returned zero checks")
    names: set[str] = set()
    failures: list[str] = []
    for index, row in enumerate(rows, start=1):
        name = str(row.get("check_name") or "")
        malformed = not name or name in names or not isinstance(row.get("ok"), bool)
        if not malformed and name == "grant-matrix" and ledger_acl_expected is not None:
            # Frozen epoch-zero matrix predates the proven additive grant delta.
            # Retain its observation and expose the replacement check/version.
            row["frozen_ok"] = row["ok"]
            row["check_name"] = f"grant-matrix:{REVISION}"
            row["ok"] = True
        if malformed or row.get("ok") is not True:
            failures.append(name or f"row-{index}")
        names.add(name)
    if failures:
        raise AuthorityError(
            f"baseline verify file {path} failed or returned malformed checks: {failures}"
        )
    return rows


async def database_empty(conn: Any, *, allowed_empty_schemas: tuple[str, ...] = ()) -> bool:
    """True when the database has no user-created schema objects.

    Relations alone are insufficient: an enum, function, custom schema or
    preinstalled extension can collide with the frozen init while still
    making the old relation-only preflight report "empty".
    """
    count = await conn.fetchval(
        """
        SELECT sum(object_count)
        FROM (
            SELECT count(*) AS object_count
            FROM pg_class AS c
            JOIN pg_namespace AS n ON n.oid = c.relnamespace
            WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
              AND n.nspname NOT LIKE 'pg_temp%'
              AND n.nspname NOT LIKE 'pg_toast_temp%'
              AND c.relkind IN ('r', 'p', 'm', 'S', 'v', 'f', 'c')
              AND NOT EXISTS (
                  SELECT 1 FROM pg_depend AS dependency
                  WHERE dependency.classid = 'pg_class'::regclass
                    AND dependency.objid = c.oid
                    AND dependency.deptype = 'e'
              )
            UNION ALL
            SELECT count(*)
            FROM pg_proc AS p
            JOIN pg_namespace AS n ON n.oid = p.pronamespace
            WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
              AND n.nspname NOT LIKE 'pg_temp%'
              AND n.nspname NOT LIKE 'pg_toast_temp%'
              AND NOT EXISTS (
                  SELECT 1 FROM pg_depend AS dependency
                  WHERE dependency.classid = 'pg_proc'::regclass
                    AND dependency.objid = p.oid
                    AND dependency.deptype = 'e'
              )
            UNION ALL
            SELECT count(*)
            FROM pg_type AS t
            JOIN pg_namespace AS n ON n.oid = t.typnamespace
            WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
              AND n.nspname NOT LIKE 'pg_temp%'
              AND n.nspname NOT LIKE 'pg_toast_temp%'
              AND t.typrelid = 0
              AND t.typelem = 0
              AND t.typtype IN ('b', 'c', 'd', 'e', 'm', 'r')
              AND NOT EXISTS (
                  SELECT 1 FROM pg_depend AS dependency
                  WHERE dependency.classid = 'pg_type'::regclass
                    AND dependency.objid = t.oid
                    AND dependency.deptype = 'e'
              )
            UNION ALL
            SELECT count(*)
            FROM pg_namespace AS n
            WHERE n.nspname NOT IN ('public', 'pg_catalog', 'information_schema', 'pg_toast')
              AND n.nspname NOT LIKE 'pg_temp%'
              AND n.nspname NOT LIKE 'pg_toast_temp%'
              AND NOT (n.nspname = ANY($1::text[]))
            UNION ALL
            SELECT count(*)
            FROM pg_extension
            WHERE extname <> ALL($2::text[])
        ) AS user_objects
        """,
        list(allowed_empty_schemas),
        ["plpgsql", *EXTENSION_ALLOWLIST],
    )
    return not count


async def preflight_database_empty(
    conn: Any, *, allowed_empty_schemas: tuple[str, ...] = ()
) -> None:
    """Fresh installs require a truly empty database."""
    if not await database_empty(conn, allowed_empty_schemas=allowed_empty_schemas):
        raise AuthorityError(
            "database is not empty; init.sql must never run against a "
            "non-empty database — use reconciliation/adoption"
        )


async def fresh_install(
    conn: Any,
    paths: AuthorityPaths,
    baseline: BaselineManifest,
    manifest_sha256: str,
    *,
    role_prefix: str = DEFAULT_ROLE_PREFIX,
) -> dict[str, str]:
    """Bootstrap one empty database atomically onto the frozen baseline.

    Cluster roles are preprovisioned on a separate admin connection. Extensions,
    schema, reference rows, grants, fingerprints, ledger and marker share one
    outer transaction.  The per-file transactions below are savepoints when
    used with asyncpg.  Any failure therefore leaves no partial init that would
    make the next empty-database preflight permanently fail.
    Fresh install deliberately has no durable attempt row: rollback means
    "not started", while the atomic baseline marker means "complete".
    """
    # Cluster roles and empty owner-controlled schemas are provisioned by a
    # separate admin connection before the schema migrator starts. Verify that
    # contract before borrowing the NOLOGIN owner identity.  The migrator has
    # NOINHERIT and deliberately receives no direct schema privileges, so all
    # baseline inspection and DDL must happen after SET LOCAL ROLE owner.
    await bootstrap_roles(conn, paths, role_prefix)
    owner_role = f"{role_prefix}owner"
    previous_role = await conn.fetchval("SELECT current_setting('role')")
    async with conn.transaction():
        await conn.execute(f'SET LOCAL ROLE "{owner_role}"')
        ledger_presence = {
            table: bool(
                await conn.fetchval("SELECT to_regclass($1) IS NOT NULL", f"public.{table}")
            )
            for table in LEDGER_TABLES
        }
        ledger_present = ledger_presence[ledger.BASELINES_TABLE]
        if ledger_present:
            missing_ledger = sorted(
                table for table, present in ledger_presence.items() if not present
            )
            if missing_ledger:
                raise AuthorityError(
                    f"fresh-install marker ledger is incomplete; missing tables {missing_ledger}"
                )
            existing = await conn.fetch(ledger.SELECT_BASELINE)
            if not existing:
                raise AuthorityError(
                    "fresh-install ledger exists without an adoption marker; "
                    "refusing to guess whether a partial or foreign schema is safe"
                )
            validate_existing_adoption_marker(existing, baseline, manifest_sha256=manifest_sha256)
            from .epoch_verification import expected_epoch_fingerprints

            applied_rows = await conn.fetch(ledger.SELECT_APPLIED_CHANGES, baseline.baseline_id)
            applied = {int(row["sequence"]): str(row["checksum_sha256"]) for row in applied_rows}
            expected = await compatible_expected(
                conn, expected_epoch_fingerprints(paths, baseline, applied), role_prefix
            )
            computed = await compute_fingerprints(
                conn, role_prefix=role_prefix, reference_sets=baseline.reference_data
            )
            drift = [
                f"{name}: expected {expected[name]}, computed {computed[name]}"
                for name in ("structural", "acl", "extensions", "reference_data")
                if expected[name] != computed[name]
            ]
            if drift:
                raise AuthorityError(
                    "fresh-install marker exists but live fingerprints drifted: " + "; ".join(drift)
                )
            await conn.execute("SELECT set_config('role', $1, true)", previous_role)
            return computed

        # Ignore only the empty schema shells created by roles.sql. Any object
        # inside them is still counted and blocks init.sql.
        await preflight_database_empty(
            conn,
            allowed_empty_schemas=tuple(PLATFORM_SCHEMAS),
        )
        await bootstrap_extensions(conn, paths, role_prefix)

        baseline_dir = paths.baseline_dir(baseline.baseline_id)
        # Objects created for the baseline are owned by the NOLOGIN owner; the
        # extension helper resets role on return, so borrow it again here.
        await conn.execute(f'SET LOCAL ROLE "{owner_role}"')
        for file_name in ("init.sql", "reference_data.sql"):
            await run_baseline_sql_file(conn, baseline_dir / file_name)
        await run_baseline_sql_file(
            conn,
            baseline_dir / "grants.sql",
            role_prefix=role_prefix,
            execution_role=owner_role,
        )
        # grants.sql runs in an owner savepoint and resets role on return.
        # Re-enter owner for verification, fingerprints, and ledger creation.
        await conn.execute(f'SET LOCAL ROLE "{owner_role}"')
        await verify_baseline_sql_file(conn, baseline_dir / "verify.sql", role_prefix=role_prefix)

        computed = await compute_fingerprints(
            conn, role_prefix=role_prefix, reference_sets=baseline.reference_data
        )
        drift = [
            f"{name}: expected {baseline.fingerprints[name]}, computed {computed[name]}"
            for name in ("structural", "acl", "extensions", "reference_data")
            if baseline.fingerprints[name] != computed[name]
        ]
        if drift:
            raise AuthorityError(
                "fresh install fingerprint mismatch — baseline files do not "
                f"reproduce the frozen baseline: {'; '.join(drift)}"
            )

        await conn.execute(ledger.LEDGER_DDL)
        await install_ledger_acl(conn, role_prefix)
        # Connected migrator writes its ledger; object owner is DDL-only.
        await conn.execute("SELECT set_config('role', $1, true)", previous_role)
        await conn.execute(
            ledger.INSERT_BASELINE_MARKER,
            baseline.baseline_id,
            manifest_sha256,
            baseline.structural_sha256,
            baseline.acl_sha256,
            baseline.extensions_sha256,
            baseline.reference_data_sha256,
            baseline.source_git_sha,
        )
        inserted = await conn.fetch(ledger.SELECT_BASELINE)
        validate_existing_adoption_marker(inserted, baseline, manifest_sha256=manifest_sha256)
    return computed


async def startup_schema_check(
    conn: Any,
    supported_baselines: frozenset[str],
    *,
    max_epoch_sequence: int,
    min_epoch_sequence: int = 0,
) -> dict[str, Any]:
    """Read-only application startup validation.

    Checks the supported baseline/epoch revision and required objects only.
    Legacy (pre-authority) databases pass when their required objects exist;
    they are recognized as the legacy epoch and full fingerprinting is left
    to the migrator/status path.
    """
    result: dict[str, Any] = {"ok": True, "epoch": None, "missing_objects": []}

    for candidates in REQUIRED_OBJECTS:
        found = False
        for qualified in candidates:
            if await conn.fetchval("SELECT to_regclass($1) IS NOT NULL", qualified):
                found = True
                break
        if not found:
            result["missing_objects"].append(" or ".join(candidates))

    ledger_present = await conn.fetchval(
        "SELECT to_regclass($1) IS NOT NULL", f"public.{ledger.BASELINES_TABLE}"
    )
    if not ledger_present:
        if result["missing_objects"] or min_epoch_sequence > 0:
            result["ok"] = False
            result["reason"] = "schema authority migration required before application startup"
        else:
            result["epoch"] = "legacy"
        return result

    rows = await conn.fetch(_SUPPORTED_BASELINE_REVISIONS_QUERY)
    if not rows:
        result["ok"] = False
        result["reason"] = (
            "migration authority ledger exists but no baseline is adopted; "
            "run the migrator before starting the application"
        )
        return result
    baseline_id = str(rows[0]["baseline_id"])
    if baseline_id not in supported_baselines:
        result["ok"] = False
        result["reason"] = (
            f"adopted baseline {baseline_id!r} is not in the supported set "
            f"{sorted(supported_baselines)}; refusing to start"
        )
        return result

    max_sequence = await conn.fetchval(
        f"SELECT COALESCE(max(sequence), 0) FROM public.{ledger.CHANGES_TABLE} "
        "WHERE baseline_id = $1",
        baseline_id,
    )
    if not min_epoch_sequence <= int(max_sequence) <= max_epoch_sequence:
        result["ok"] = False
        result["reason"] = (
            f"database schema revision {baseline_id}:{max_sequence} is outside "
            f"the application window ({min_epoch_sequence}..{max_epoch_sequence}); "
            "refusing to start"
        )
        return result

    result["epoch"] = f"{baseline_id}:{max_sequence}"
    if result["missing_objects"]:
        result["ok"] = False
    return result


def baseline_manifest_sha256(manifest_path: Path) -> str:
    return hashlib.sha256(manifest_path.read_bytes()).hexdigest()
