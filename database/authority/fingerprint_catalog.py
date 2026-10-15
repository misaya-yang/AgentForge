"""Catalog details whose omission could make distinct schemas hash alike."""

from __future__ import annotations

from typing import Any

from .constants import EXTENSION_ALLOWLIST, PLATFORM_SCHEMAS

_SCHEMA_FILTER = """
    AND n.nspname = ANY($1)
    AND n.nspname NOT LIKE 'pg_temp_%'
    AND n.nspname NOT LIKE 'pg_toast_temp_%'
"""

_EXCLUDED_RELATIONS = """
    AND NOT (n.nspname = 'public' AND c.relname IN (
        'platform_schema_baselines',
        'platform_schema_changes',
        'platform_schema_change_attempts',
        'schema_migrations',
        'schema_migrations_meta'
    ))
"""

_EXTENSION_RELATION_FILTER = """
    AND NOT EXISTS (
        SELECT 1 FROM pg_depend AS extension_dependency
        WHERE extension_dependency.classid = 'pg_class'::regclass
          AND extension_dependency.objid = c.oid
          AND extension_dependency.deptype = 'e'
    )
"""

_EXTENSION_FUNCTION_FILTER = """
    AND NOT EXISTS (
        SELECT 1 FROM pg_depend AS extension_dependency
        WHERE extension_dependency.classid = 'pg_proc'::regclass
          AND extension_dependency.objid = p.oid
          AND extension_dependency.deptype = 'e'
    )
"""


class CatalogFingerprintError(RuntimeError):
    pass


def logical_principal_map(role_prefix: str) -> dict[str, str]:
    """Map physical role names to logical principal ids for ACL hashing."""
    return {
        f"{role_prefix}owner": "owner",
        f"{role_prefix}migrator": "migrator",
        f"{role_prefix}gateway": "gateway",
        f"{role_prefix}runtime": "runtime",
        f"{role_prefix}capability_worker": "capability_worker",
        f"{role_prefix}knowledge_api": "knowledge_api",
        f"{role_prefix}knowledge_worker": "knowledge_worker",
    }


def like_prefix_pattern(prefix: str) -> str:
    """Literal SQL LIKE prefix; ``_``/``%`` in role names are not wildcards."""
    escaped = prefix.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")
    return f"{escaped}%"


def _regprocedure(column: str, alias: str) -> str:
    return f"CASE WHEN {column} = 0 THEN '' ELSE {column}::regprocedure::text END AS {alias}"


async def structural_catalog_detail_lines(conn: Any) -> list[str]:
    """Definitions not represented by relation/column/index names alone."""
    lines: list[str] = []

    views = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, c.relname AS name, c.relkind AS kind,
               pg_get_viewdef(c.oid, false) AS definition
        FROM pg_class AS c
        JOIN pg_namespace AS n ON n.oid = c.relnamespace
        WHERE c.relkind IN ('v', 'm')
        {_SCHEMA_FILTER}
        {_EXCLUDED_RELATIONS}
        {_EXTENSION_RELATION_FILTER}
        ORDER BY n.nspname, c.relname
        """,
        list(PLATFORM_SCHEMAS),
    )
    lines.extend(
        f"view:{row['schema']}.{row['name']}:{row['kind']}:{row['definition']}" for row in views
    )

    triggers = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, c.relname AS relation, trigger.tgname AS name,
               trigger.tgenabled AS enabled,
               pg_get_triggerdef(trigger.oid, false) AS definition
        FROM pg_trigger AS trigger
        JOIN pg_class AS c ON c.oid = trigger.tgrelid
        JOIN pg_namespace AS n ON n.oid = c.relnamespace
        WHERE NOT trigger.tgisinternal
        {_SCHEMA_FILTER}
        {_EXCLUDED_RELATIONS}
        {_EXTENSION_RELATION_FILTER}
          AND NOT EXISTS (
              SELECT 1 FROM pg_depend AS extension_dependency
              WHERE extension_dependency.classid = 'pg_trigger'::regclass
                AND extension_dependency.objid = trigger.oid
                AND extension_dependency.deptype = 'e'
          )
        ORDER BY n.nspname, c.relname, trigger.tgname
        """,
        list(PLATFORM_SCHEMAS),
    )
    lines.extend(
        f"trigger:{row['schema']}.{row['relation']}.{row['name']}:"
        f"enabled={row['enabled']}:{row['definition']}"
        for row in triggers
    )

    aggregates = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, p.proname AS name,
               pg_get_function_identity_arguments(p.oid) AS identity_args,
               a.aggkind, a.aggnumdirectargs,
               {_regprocedure("a.aggtransfn", "transition_function")},
               {_regprocedure("a.aggfinalfn", "final_function")},
               {_regprocedure("a.aggcombinefn", "combine_function")},
               {_regprocedure("a.aggserialfn", "serial_function")},
               {_regprocedure("a.aggdeserialfn", "deserial_function")},
               {_regprocedure("a.aggmtransfn", "moving_transition_function")},
               {_regprocedure("a.aggminvtransfn", "moving_inverse_function")},
               {_regprocedure("a.aggmfinalfn", "moving_final_function")},
               format_type(a.aggtranstype, NULL) AS transition_type,
               CASE WHEN a.aggmtranstype = 0 THEN ''
                    ELSE format_type(a.aggmtranstype, NULL) END AS moving_transition_type,
               a.aggtransspace, a.aggmtransspace,
               a.aggfinalextra, a.aggmfinalextra,
               a.aggfinalmodify, a.aggmfinalmodify,
               CASE WHEN a.aggsortop = 0 THEN ''
                    ELSE a.aggsortop::regoperator::text END AS sort_operator,
               COALESCE(a.agginitval, '') AS initial_value,
               COALESCE(a.aggminitval, '') AS moving_initial_value
        FROM pg_proc AS p
        JOIN pg_namespace AS n ON n.oid = p.pronamespace
        JOIN pg_aggregate AS a ON a.aggfnoid = p.oid
        WHERE TRUE
        {_SCHEMA_FILTER}
        {_EXTENSION_FUNCTION_FILTER}
        ORDER BY n.nspname, p.proname, pg_get_function_identity_arguments(p.oid)
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in aggregates:
        functions = ",".join(
            str(row[field])
            for field in (
                "transition_function",
                "final_function",
                "combine_function",
                "serial_function",
                "deserial_function",
                "moving_transition_function",
                "moving_inverse_function",
                "moving_final_function",
            )
        )
        lines.append(
            f"aggregate:{row['schema']}.{row['name']}({row['identity_args']}):"
            f"kind={row['aggkind']}:direct={row['aggnumdirectargs']}:functions={functions}:"
            f"state={row['transition_type']}/{row['moving_transition_type']}:"
            f"space={row['aggtransspace']}/{row['aggmtransspace']}:"
            f"extra={int(row['aggfinalextra'])}/{int(row['aggmfinalextra'])}:"
            f"modify={row['aggfinalmodify']}/{row['aggmfinalmodify']}:"
            f"sort={row['sort_operator']}:"
            f"init={row['initial_value']}/{row['moving_initial_value']}"
        )

    return lines


async def policy_acl_lines(conn: Any, principal_map: dict[str, str]) -> list[str]:
    """RLS policy definitions with environment role names normalized."""
    rows = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, c.relname AS relation, policy.polname AS name,
               policy.polpermissive AS permissive, policy.polcmd AS command,
               ARRAY(
                   SELECT CASE WHEN role_oid = 0 THEN 'PUBLIC'
                               ELSE pg_get_userbyid(role_oid) END
                   FROM unnest(policy.polroles) AS role_oid
                   ORDER BY 1
               ) AS roles,
               COALESCE(pg_get_expr(policy.polqual, policy.polrelid, false), '') AS using_expr,
               COALESCE(pg_get_expr(policy.polwithcheck, policy.polrelid, false), '') AS check_expr
        FROM pg_policy AS policy
        JOIN pg_class AS c ON c.oid = policy.polrelid
        JOIN pg_namespace AS n ON n.oid = c.relnamespace
        WHERE TRUE
        {_SCHEMA_FILTER}
        {_EXCLUDED_RELATIONS}
        {_EXTENSION_RELATION_FILTER}
          AND NOT EXISTS (
              SELECT 1 FROM pg_depend AS extension_dependency
              WHERE extension_dependency.classid = 'pg_policy'::regclass
                AND extension_dependency.objid = policy.oid
                AND extension_dependency.deptype = 'e'
          )
        ORDER BY n.nspname, c.relname, policy.polname
        """,
        list(PLATFORM_SCHEMAS),
    )
    lines: list[str] = []
    for row in rows:
        roles = ",".join(principal_map.get(str(role), str(role)) for role in (row["roles"] or []))
        lines.append(
            f"policy:{row['schema']}.{row['relation']}.{row['name']}:"
            f"permissive={int(row['permissive'])}:command={row['command']}:roles={roles}:"
            f"using={row['using_expr']}:check={row['check_expr']}"
        )
    return lines


async def database_acl_lines(
    conn: Any,
    principal_map: dict[str, str],
    role_prefix_pattern: str,
) -> list[str]:
    """Database-level grants for PUBLIC and platform roles."""
    rows = await conn.fetch(
        """
        SELECT COALESCE(pg_get_userbyid(privilege.grantee), 'PUBLIC') AS grantee,
               privilege.is_grantable AS grant_option,
               string_agg(
                   privilege.privilege_type, ',' ORDER BY privilege.privilege_type
               ) AS privileges
        FROM pg_database AS database
        CROSS JOIN LATERAL aclexplode(
            COALESCE(database.datacl, acldefault('d', database.datdba))
        ) AS privilege
        WHERE database.datname = current_database()
          AND (
              privilege.grantee = 0
              OR pg_get_userbyid(privilege.grantee) LIKE $1 ESCAPE E'\\\\'
          )
        GROUP BY privilege.grantee, privilege.is_grantable
        ORDER BY grantee, privilege.is_grantable
        """,
        role_prefix_pattern,
    )
    lines: list[str] = []
    for row in rows:
        grantee = principal_map.get(str(row["grantee"]), str(row["grantee"]))
        lines.append(
            f"database_acl:{grantee}:{row['privileges']}:grantopt={int(row['grant_option'])}"
        )
    return lines


async def column_acl_lines(conn: Any, principal_map: dict[str, str]) -> list[str]:
    """Explicit column grants, which table-level ACL revocation does not erase."""
    rows = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, c.relname AS relation,
               attribute.attname AS column,
               COALESCE(pg_get_userbyid(privilege.grantee), 'PUBLIC') AS grantee,
               privilege.is_grantable AS grant_option,
               string_agg(
                   privilege.privilege_type, ',' ORDER BY privilege.privilege_type
               ) AS privileges
        FROM pg_attribute AS attribute
        JOIN pg_class AS c ON c.oid = attribute.attrelid
        JOIN pg_namespace AS n ON n.oid = c.relnamespace
        CROSS JOIN LATERAL aclexplode(attribute.attacl) AS privilege
        WHERE attribute.attnum > 0 AND NOT attribute.attisdropped
          AND attribute.attacl IS NOT NULL
        {_SCHEMA_FILTER}
        {_EXCLUDED_RELATIONS}
        {_EXTENSION_RELATION_FILTER}
        GROUP BY n.nspname, c.relname, attribute.attname,
                 privilege.grantee, privilege.is_grantable
        ORDER BY n.nspname, c.relname, attribute.attname, grantee,
                 privilege.is_grantable
        """,
        list(PLATFORM_SCHEMAS),
    )
    lines: list[str] = []
    for row in rows:
        grantee = principal_map.get(str(row["grantee"]), str(row["grantee"]))
        lines.append(
            f"column_acl:{row['schema']}.{row['relation']}.{row['column']}:"
            f"{grantee}:{row['privileges']}:grantopt={int(row['grant_option'])}"
        )
    return lines


async def extension_catalog_lines(conn: Any, principal_map: dict[str, str]) -> list[str]:
    """Allowlisted extension identity, version, schema and logical owner."""
    rows = await conn.fetch(
        """
        SELECT extension.extname, extension.extversion,
               namespace.nspname AS schema,
               pg_get_userbyid(extension.extowner) AS owner
        FROM pg_extension AS extension
        JOIN pg_namespace AS namespace ON namespace.oid = extension.extnamespace
        ORDER BY extension.extname
        """
    )
    lines: list[str] = []
    for row in rows:
        if row["extname"] == "plpgsql":
            continue
        if row["extname"] not in EXTENSION_ALLOWLIST:
            raise CatalogFingerprintError(
                f"extension {row['extname']!r} is not in the allowlist {EXTENSION_ALLOWLIST}"
            )
        owner = principal_map.get(str(row["owner"]), str(row["owner"]))
        lines.append(
            f"extension:{row['extname']}:{row['extversion']}:schema={row['schema']}:owner={owner}"
        )
    return lines
