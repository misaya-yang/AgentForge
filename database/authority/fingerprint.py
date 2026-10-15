"""Four-class schema fingerprints.

Classes (PRD ARC-03 §3B.6):

1. structural — tables/columns/constraints/indexes/sequences/functions/types,
   normalized and environment-independent.
2. acl — object owners, ACL entries and default privileges, mapped to the
   logical principal ids of the baseline manifest so that configurable LOGIN
   role prefixes never change an equivalent-permission hash.
3. extensions — allowlisted extension name/version/schema/logical-owner tuples.
4. reference_data — exact hash over declared system-owned immutable rows only.

Ledger/history tables and catalog schemas are excluded by construction.  User
data and credentials never enter a fingerprint.
"""

from __future__ import annotations

import json
from collections.abc import Iterable
from typing import Any

from .constants import (
    PLATFORM_SCHEMAS,
)
from .fingerprint_catalog import (
    CatalogFingerprintError,
    column_acl_lines,
    database_acl_lines,
    extension_catalog_lines,
    logical_principal_map,
    policy_acl_lines,
    structural_catalog_detail_lines,
)
from .fingerprint_catalog import (
    like_prefix_pattern as _like_prefix_pattern,
)
from .fingerprint_values import (
    UnsupportedFingerprintValue,
    canonical_digest,
    canonical_value_tree,
)
from .fingerprint_values import (
    diff_line_lists as diff_line_lists,
)
from .manifest import ReferenceDataSet

_FINGERPRINT_CLASSES = ("structural", "acl", "extensions", "reference_data")

# pg_class.relkind values the structural fingerprint covers.
_RELKIND_NAMES = {
    "r": "table",
    "p": "partitioned_table",
    "v": "view",
    "m": "materialized_view",
    "S": "sequence",
    "f": "foreign_table",
}

# Ledger and history tables differ between a fresh install and an adopted
# legacy database by design; they never enter a fingerprint.
_EXCLUDED_RELATION_FILTER = """
    AND NOT (n.nspname = 'public' AND c.relname IN (
        'platform_schema_baselines',
        'platform_schema_changes',
        'platform_schema_change_attempts',
        'schema_migrations',
        'schema_migrations_meta'
    ))
"""

_SCHEMA_FILTER = """
    AND n.nspname = ANY($1)
    AND n.nspname NOT LIKE 'pg_temp_%'
    AND n.nspname NOT LIKE 'pg_toast_temp_%'
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

_EXTENSION_TYPE_FILTER = """
    AND NOT EXISTS (
        SELECT 1 FROM pg_depend AS extension_dependency
        WHERE extension_dependency.classid = 'pg_type'::regclass
          AND extension_dependency.objid = t.oid
          AND extension_dependency.deptype = 'e'
    )
"""

_PLATFORM_TYPE_FILTER = """
    AND (
        t.typtype IN ('e', 'd', 'r', 'm')
        OR (t.typtype = 'c' AND type_relation.relkind = 'c')
    )
"""


class FingerprintError(RuntimeError):
    """A fingerprint could not be computed or failed its allowlist checks."""


def _normalized_owner(owner: str, owner_map: dict[str, str] | None) -> str:
    if owner_map and owner in owner_map:
        return owner_map[owner]
    return owner


async def structural_lines(conn: Any) -> list[str]:
    """Deterministic structural description of every platform relation."""
    lines: list[str] = []

    schemas = await conn.fetch(
        """
        SELECT n.nspname
        FROM pg_namespace AS n
        WHERE n.nspname = ANY($1)
          AND n.nspname NOT LIKE 'pg_temp_%'
          AND n.nspname NOT LIKE 'pg_toast_temp_%'
        ORDER BY 1
        """,
        list(PLATFORM_SCHEMAS),
    )
    lines.extend(f"schema:{row['nspname']}" for row in schemas)

    relations = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, c.relname AS name, c.relkind AS kind,
               c.relpersistence AS persistence,
               c.relispartition AS is_partition,
               c.relrowsecurity AS row_security,
               c.relforcerowsecurity AS force_row_security,
               c.relreplident AS replica_identity,
               COALESCE(access_method.amname, '') AS access_method,
               COALESCE(pg_get_partkeydef(c.oid), '') AS partition_key,
               CASE WHEN c.relispartition
                    THEN pg_get_expr(c.relpartbound, c.oid, false) ELSE '' END
                    AS partition_bound,
               COALESCE((
                   SELECT string_agg(option, ',' ORDER BY option)
                   FROM unnest(c.reloptions) AS option
               ), '') AS options
        FROM pg_class AS c
        JOIN pg_namespace AS n ON n.oid = c.relnamespace
        LEFT JOIN pg_am AS access_method ON access_method.oid = c.relam
        WHERE c.relkind <> ALL(ARRAY['i', 'I', 't', 'c'])
        {_SCHEMA_FILTER}
        {_EXCLUDED_RELATION_FILTER}
        {_EXTENSION_RELATION_FILTER}
        ORDER BY n.nspname, c.relname
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in relations:
        kind = _RELKIND_NAMES.get(row["kind"], row["kind"])
        lines.append(
            f"relation:{row['schema']}.{row['name']}:{kind}:"
            f"persistence={row['persistence']}:partition={int(row['is_partition'])}:"
            f"rls={int(row['row_security'])}/{int(row['force_row_security'])}:"
            f"replica_identity={row['replica_identity']}:am={row['access_method']}:"
            f"partition_key={row['partition_key']}:bound={row['partition_bound']}:"
            f"options={row['options']}"
        )

    columns = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, c.relname AS relation, a.attname AS column,
               format_type(a.atttypid, a.atttypmod) AS type,
               a.attnotnull AS not_null,
               COALESCE(pg_get_expr(d.adbin, d.adrelid), '') AS default_expr,
               a.attnum AS position, a.attidentity AS identity,
               a.attgenerated AS generated, a.attstorage AS storage,
               a.attcompression AS compression,
               COALESCE(collation_namespace.nspname || '.' || coll.collname, '')
                   AS collation
        FROM pg_class AS c
        JOIN pg_namespace AS n ON n.oid = c.relnamespace
        JOIN pg_attribute AS a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
        LEFT JOIN pg_attrdef AS d ON d.adrelid = c.oid AND d.adnum = a.attnum
        LEFT JOIN pg_collation AS coll ON coll.oid = a.attcollation
        LEFT JOIN pg_namespace AS collation_namespace
          ON collation_namespace.oid = coll.collnamespace
        WHERE c.relkind IN ('r', 'p', 'f', 'm', 'v')
        {_SCHEMA_FILTER}
        {_EXCLUDED_RELATION_FILTER}
        {_EXTENSION_RELATION_FILTER}
        ORDER BY n.nspname, c.relname, a.attnum
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in columns:
        lines.append(
            "column:{schema}.{relation}.{column}:{position}:{type}:"
            "notnull={not_null}:default={default_expr}:identity={identity}:"
            "generated={generated}:storage={storage}:compression={compression}:"
            "collation={collation}".format(
                schema=row["schema"],
                relation=row["relation"],
                column=row["column"],
                position=row["position"],
                type=row["type"],
                not_null="1" if row["not_null"] else "0",
                default_expr=row["default_expr"],
                identity=row["identity"],
                generated=row["generated"],
                storage=row["storage"],
                compression=row["compression"],
                collation=row["collation"],
            )
        )

    constraints = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, c.relname AS relation, con.conname AS name,
               con.contype AS type, con.convalidated AS validated,
               pg_get_constraintdef(con.oid) AS definition
        FROM pg_constraint AS con
        JOIN pg_class AS c ON c.oid = con.conrelid
        JOIN pg_namespace AS n ON n.oid = c.relnamespace
        WHERE TRUE
        {_SCHEMA_FILTER}
        {_EXCLUDED_RELATION_FILTER}
        {_EXTENSION_RELATION_FILTER}
        ORDER BY n.nspname, c.relname, con.conname
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in constraints:
        lines.append(
            f"constraint:{row['schema']}.{row['relation']}.{row['name']}:"
            f"{row['type']}:validated={int(row['validated'])}:{row['definition']}"
        )

    indexes = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, c.relname AS name,
               pg_get_indexdef(c.oid) AS definition,
               i.indisunique AS is_unique, i.indisprimary AS is_primary,
               i.indisvalid AS is_valid, i.indisready AS is_ready,
               i.indisexclusion AS is_exclusion, i.indisreplident AS is_replica_identity,
               i.indisclustered AS is_clustered
        FROM pg_class AS c
        JOIN pg_namespace AS n ON n.oid = c.relnamespace
        JOIN pg_index AS i ON i.indexrelid = c.oid
        WHERE c.relkind IN ('i', 'I')
        {_SCHEMA_FILTER}
        {_EXTENSION_RELATION_FILTER}
        AND NOT (n.nspname = 'public' AND (
            SELECT rc.relname FROM pg_class AS rc WHERE rc.oid = i.indrelid
        ) IN (
            'platform_schema_baselines',
            'platform_schema_changes',
            'platform_schema_change_attempts',
            'schema_migrations',
            'schema_migrations_meta'
        ))
        ORDER BY n.nspname, c.relname
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in indexes:
        flags = "u" if row["is_unique"] else ""
        flags += "p" if row["is_primary"] else ""
        flags += "v" if row["is_valid"] else ""
        flags += "r" if row["is_ready"] else ""
        flags += "x" if row["is_exclusion"] else ""
        flags += "i" if row["is_replica_identity"] else ""
        flags += "c" if row["is_clustered"] else ""
        lines.append(f"index:{row['schema']}.{row['name']}:{flags}:{row['definition']}")

    sequences = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, c.relname AS name,
               format_type(s.seqtypid, NULL) AS data_type,
               s.seqstart AS start_value, s.seqincrement AS increment_by,
               s.seqmin AS min_value, s.seqmax AS max_value,
               s.seqcache AS cache_size, s.seqcycle AS cycle
        FROM pg_sequence AS s
        JOIN pg_class AS c ON c.oid = s.seqrelid
        JOIN pg_namespace AS n ON n.oid = c.relnamespace
        WHERE TRUE
        {_SCHEMA_FILTER}
        {_EXTENSION_RELATION_FILTER}
        ORDER BY n.nspname, c.relname
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in sequences:
        lines.append(
            "sequence:{schema}.{name}:{data_type}:start={start}:by={by}:"
            "min={min}:max={max}:cache={cache}:cycle={cycle}".format(
                schema=row["schema"],
                name=row["name"],
                data_type=row["data_type"],
                start=row["start_value"],
                by=row["increment_by"],
                min=row["min_value"],
                max=row["max_value"],
                cache=row["cache_size"],
                cycle="1" if row["cycle"] else "0",
            )
        )

    functions = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, p.proname AS name,
               pg_get_function_identity_arguments(p.oid) AS identity_args,
               pg_get_functiondef(p.oid) AS definition,
               p.prosecdef AS security_definer
        FROM pg_proc AS p
        JOIN pg_namespace AS n ON n.oid = p.pronamespace
        WHERE TRUE
        {_SCHEMA_FILTER}
        {_EXTENSION_FUNCTION_FILTER}
          AND p.prokind IN ('f', 'p', 'w')
        ORDER BY n.nspname, p.proname, pg_get_function_identity_arguments(p.oid)
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in functions:
        security = "definer" if row["security_definer"] else "invoker"
        lines.append(
            f"function:{row['schema']}.{row['name']}({row['identity_args']}):"
            f"{security}:{row['definition']}"
        )

    lines.extend(await structural_catalog_detail_lines(conn))

    types = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, t.typname AS name, t.typtype AS type,
               CASE WHEN t.typbasetype = 0 THEN ''
                    ELSE format_type(t.typbasetype, t.typtypmod) END AS base_type,
               t.typnotnull AS not_null, COALESCE(t.typdefault, '') AS default_expr,
               COALESCE(collation_namespace.nspname || '.' || coll.collname, '')
                   AS collation,
               CASE WHEN range_definition.rngsubtype IS NULL THEN ''
                    ELSE format_type(range_definition.rngsubtype, NULL) END AS range_subtype,
               CASE WHEN range_definition.rngcanonical = 0 THEN ''
                    ELSE range_definition.rngcanonical::regprocedure::text END AS canonical,
               CASE WHEN range_definition.rngsubdiff = 0 THEN ''
                    ELSE range_definition.rngsubdiff::regprocedure::text END AS subtype_diff
        FROM pg_type AS t
        JOIN pg_namespace AS n ON n.oid = t.typnamespace
        LEFT JOIN pg_class AS type_relation ON type_relation.oid = t.typrelid
        LEFT JOIN pg_collation AS coll ON coll.oid = t.typcollation
        LEFT JOIN pg_namespace AS collation_namespace
          ON collation_namespace.oid = coll.collnamespace
        LEFT JOIN pg_range AS range_definition
          ON range_definition.rngtypid = t.oid OR range_definition.rngmultitypid = t.oid
        WHERE TRUE
        {_SCHEMA_FILTER}
        {_PLATFORM_TYPE_FILTER}
        {_EXTENSION_TYPE_FILTER}
        ORDER BY n.nspname, t.typname
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in types:
        lines.append(
            f"type:{row['schema']}.{row['name']}:{row['type']}:"
            f"base={row['base_type']}:notnull={1 if row['not_null'] else 0}:"
            f"default={row['default_expr']}:collation={row['collation']}:"
            f"range_subtype={row['range_subtype']}:canonical={row['canonical']}:"
            f"subtype_diff={row['subtype_diff']}"
        )
        if row["type"] == "e":
            labels = await conn.fetch(
                "SELECT enumlabel FROM pg_enum WHERE enumtypid = "
                "(SELECT oid FROM pg_type AS t2 JOIN pg_namespace AS n2 ON n2.oid = t2.typnamespace "
                "WHERE n2.nspname = $1 AND t2.typname = $2) ORDER BY enumsortorder",
                row["schema"],
                row["name"],
            )
            lines.extend(
                f"enum:{row['schema']}.{row['name']}:{label['enumlabel']}" for label in labels
            )
        if row["type"] == "d":
            constraints = await conn.fetch(
                "SELECT conname, pg_get_constraintdef(oid) AS definition "
                "FROM pg_constraint WHERE contypid = ("
                "SELECT t2.oid FROM pg_type AS t2 "
                "JOIN pg_namespace AS n2 ON n2.oid = t2.typnamespace "
                "WHERE n2.nspname = $1 AND t2.typname = $2"
                ") ORDER BY conname",
                row["schema"],
                row["name"],
            )
            lines.extend(
                f"domain_constraint:{row['schema']}.{row['name']}."
                f"{constraint['conname']}:{constraint['definition']}"
                for constraint in constraints
            )
        if row["type"] == "c":
            attributes = await conn.fetch(
                "SELECT a.attnum AS position, a.attname AS name, "
                "format_type(a.atttypid, a.atttypmod) AS type, "
                "COALESCE(cn.nspname || '.' || col.collname, '') AS collation "
                "FROM pg_type AS t2 "
                "JOIN pg_namespace AS n2 ON n2.oid = t2.typnamespace "
                "JOIN pg_attribute AS a ON a.attrelid = t2.typrelid "
                "LEFT JOIN pg_collation AS col ON col.oid = a.attcollation "
                "LEFT JOIN pg_namespace AS cn ON cn.oid = col.collnamespace "
                "WHERE n2.nspname = $1 AND t2.typname = $2 "
                "AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
                row["schema"],
                row["name"],
            )
            lines.extend(
                f"composite_attribute:{row['schema']}.{row['name']}."
                f"{attribute['name']}:{attribute['position']}:{attribute['type']}:"
                f"collation={attribute['collation']}"
                for attribute in attributes
            )

    return lines


async def acl_lines(conn: Any, *, role_prefix: str) -> list[str]:
    """Owners, ACLs and default privileges, normalized to logical principals."""
    principal_map = logical_principal_map(role_prefix)
    lines: list[str] = []

    owners = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, c.relname AS name,
               pg_get_userbyid(c.relowner) AS owner
        FROM pg_class AS c
        JOIN pg_namespace AS n ON n.oid = c.relnamespace
        WHERE c.relkind <> ALL(ARRAY['i', 'I', 't', 'c'])
        {_SCHEMA_FILTER}
        {_EXCLUDED_RELATION_FILTER}
        {_EXTENSION_RELATION_FILTER}
        ORDER BY n.nspname, c.relname
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in owners:
        lines.append(
            f"owner:{row['schema']}.{row['name']}:{_normalized_owner(row['owner'], principal_map)}"
        )

    schema_owners = await conn.fetch(
        """
        SELECT n.nspname AS schema, pg_get_userbyid(n.nspowner) AS owner
        FROM pg_namespace AS n
        WHERE n.nspname = ANY($1)
          AND n.nspname NOT LIKE 'pg_temp_%'
          AND n.nspname NOT LIKE 'pg_toast_temp_%'
        ORDER BY n.nspname
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in schema_owners:
        lines.append(
            f"schema_owner:{row['schema']}:{_normalized_owner(row['owner'], principal_map)}"
        )

    function_owners = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, p.proname AS name,
               pg_get_function_identity_arguments(p.oid) AS identity_args,
               pg_get_userbyid(p.proowner) AS owner
        FROM pg_proc AS p
        JOIN pg_namespace AS n ON n.oid = p.pronamespace
        WHERE TRUE
        {_SCHEMA_FILTER}
        {_EXTENSION_FUNCTION_FILTER}
        ORDER BY n.nspname, p.proname, pg_get_function_identity_arguments(p.oid)
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in function_owners:
        lines.append(
            f"function_owner:{row['schema']}.{row['name']}({row['identity_args']}):"
            f"{_normalized_owner(row['owner'], principal_map)}"
        )

    type_owners = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, t.typname AS name, t.typtype AS type,
               pg_get_userbyid(t.typowner) AS owner
        FROM pg_type AS t
        JOIN pg_namespace AS n ON n.oid = t.typnamespace
        LEFT JOIN pg_class AS type_relation ON type_relation.oid = t.typrelid
        WHERE TRUE
        {_SCHEMA_FILTER}
        {_PLATFORM_TYPE_FILTER}
        {_EXTENSION_TYPE_FILTER}
        ORDER BY n.nspname, t.typname
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in type_owners:
        lines.append(
            f"type_owner:{row['schema']}.{row['name']}:{row['type']}:"
            f"{_normalized_owner(row['owner'], principal_map)}"
        )

    acls = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, c.relname AS name,
               COALESCE(pg_get_userbyid(a.grantee), 'PUBLIC') AS grantee,
               a.is_grantable AS grant_option,
               string_agg(a.privilege_type, ',' ORDER BY a.privilege_type) AS privileges,
               pg_get_userbyid(c.relowner) AS grantor_default
        FROM pg_class AS c
        JOIN pg_namespace AS n ON n.oid = c.relnamespace
        CROSS JOIN LATERAL aclexplode(COALESCE(c.relacl, acldefault(
            (CASE c.relkind WHEN 'S' THEN 's' ELSE 'r' END)::"char", c.relowner))) AS a
        WHERE c.relkind <> ALL(ARRAY['i', 'I', 't', 'c'])
        {_SCHEMA_FILTER}
        {_EXCLUDED_RELATION_FILTER}
        {_EXTENSION_RELATION_FILTER}
        GROUP BY n.nspname, c.relname, a.grantee, a.is_grantable, c.relowner
        ORDER BY n.nspname, c.relname, grantee, a.is_grantable
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in acls:
        grantee = _normalized_owner(row["grantee"], principal_map)
        lines.append(
            f"acl:{row['schema']}.{row['name']}:{grantee}:"
            f"{row['privileges']}:grantopt={1 if row['grant_option'] else 0}"
        )

    lines.extend(await column_acl_lines(conn, principal_map))

    function_acls = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, p.proname AS name,
               pg_get_function_identity_arguments(p.oid) AS identity_args,
               COALESCE(pg_get_userbyid(a.grantee), 'PUBLIC') AS grantee,
               a.is_grantable AS grant_option,
               string_agg(a.privilege_type, ',' ORDER BY a.privilege_type) AS privileges
        FROM pg_proc AS p
        JOIN pg_namespace AS n ON n.oid = p.pronamespace
        CROSS JOIN LATERAL aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) AS a
        WHERE TRUE
        {_SCHEMA_FILTER}
        {_EXTENSION_FUNCTION_FILTER}
        GROUP BY n.nspname, p.proname, p.oid, a.grantee, a.is_grantable
        ORDER BY n.nspname, p.proname, identity_args, grantee
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in function_acls:
        grantee = _normalized_owner(row["grantee"], principal_map)
        lines.append(
            f"function_acl:{row['schema']}.{row['name']}({row['identity_args']}):"
            f"{grantee}:{row['privileges']}:grantopt={1 if row['grant_option'] else 0}"
        )

    schema_acls = await conn.fetch(
        """
        SELECT n.nspname AS schema,
               COALESCE(pg_get_userbyid(a.grantee), 'PUBLIC') AS grantee,
               a.is_grantable AS grant_option,
               string_agg(a.privilege_type, ',' ORDER BY a.privilege_type) AS privileges
        FROM pg_namespace AS n
        CROSS JOIN LATERAL aclexplode(COALESCE(n.nspacl, acldefault('n', n.nspowner))) AS a
        WHERE n.nspname = ANY($1)
          AND n.nspname NOT LIKE 'pg_temp_%'
          AND n.nspname NOT LIKE 'pg_toast_temp_%'
        GROUP BY n.nspname, a.grantee, a.is_grantable
        ORDER BY n.nspname, grantee
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in schema_acls:
        grantee = _normalized_owner(row["grantee"], principal_map)
        lines.append(
            f"schema_acl:{row['schema']}:{grantee}:{row['privileges']}:"
            f"grantopt={1 if row['grant_option'] else 0}"
        )

    lines.extend(
        await database_acl_lines(
            conn,
            principal_map,
            _like_prefix_pattern(role_prefix),
        )
    )

    type_acls = await conn.fetch(
        f"""
        SELECT n.nspname AS schema, t.typname AS name, t.typtype AS type,
               COALESCE(pg_get_userbyid(a.grantee), 'PUBLIC') AS grantee,
               a.is_grantable AS grant_option,
               string_agg(a.privilege_type, ',' ORDER BY a.privilege_type) AS privileges
        FROM pg_type AS t
        JOIN pg_namespace AS n ON n.oid = t.typnamespace
        LEFT JOIN pg_class AS type_relation ON type_relation.oid = t.typrelid
        CROSS JOIN LATERAL aclexplode(COALESCE(t.typacl, acldefault('T', t.typowner))) AS a
        WHERE TRUE
        {_SCHEMA_FILTER}
        {_PLATFORM_TYPE_FILTER}
        {_EXTENSION_TYPE_FILTER}
        GROUP BY n.nspname, t.typname, t.typtype, a.grantee, a.is_grantable
        ORDER BY n.nspname, t.typname, grantee, a.is_grantable
        """,
        list(PLATFORM_SCHEMAS),
    )
    for row in type_acls:
        grantee = _normalized_owner(row["grantee"], principal_map)
        lines.append(
            f"type_acl:{row['schema']}.{row['name']}:{row['type']}:{grantee}:"
            f"{row['privileges']}:grantopt={1 if row['grant_option'] else 0}"
        )

    lines.extend(await policy_acl_lines(conn, principal_map))

    default_privileges = await conn.fetch(
        """
        SELECT COALESCE(pg_get_userbyid(d.defaclrole), '?') AS grantor,
               CASE d.defaclobjtype WHEN 'r' THEN 'table' WHEN 'f' THEN 'function'
                    WHEN 'S' THEN 'sequence' WHEN 'n' THEN 'schema' WHEN 'T' THEN 'type'
                    ELSE d.defaclobjtype::text END AS object_type,
               COALESCE(n.nspname, '') AS schema,
               COALESCE(pg_get_userbyid(a.grantee), 'PUBLIC') AS grantee,
               a.is_grantable AS grant_option,
               string_agg(a.privilege_type, ',' ORDER BY a.privilege_type) AS privileges
        FROM pg_default_acl AS d
        LEFT JOIN pg_namespace AS n ON n.oid = d.defaclnamespace
        CROSS JOIN LATERAL aclexplode(d.defaclacl) AS a
        WHERE pg_get_userbyid(d.defaclrole) LIKE $2 ESCAPE E'\\\\'
          AND (
              d.defaclnamespace = 0
              OR (
                  n.nspname = ANY($1)
                  AND n.nspname NOT LIKE 'pg_temp_%'
                  AND n.nspname NOT LIKE 'pg_toast_temp_%'
              )
          )
        GROUP BY d.defaclrole, d.defaclobjtype, n.nspname,
                 a.grantee, a.is_grantable
        ORDER BY grantor, object_type, schema, grantee
        """,
        list(PLATFORM_SCHEMAS),
        _like_prefix_pattern(role_prefix),
    )
    for row in default_privileges:
        grantor = _normalized_owner(row["grantor"], principal_map)
        grantee = _normalized_owner(row["grantee"], principal_map)
        lines.append(
            f"default_privilege:{grantor}:{row['object_type']}:{row['schema']}:"
            f"{grantee}:{row['privileges']}:"
            f"grantopt={1 if row['grant_option'] else 0}"
        )

    # Only the platform's own roles enter the fingerprint.  The bootstrap
    # superuser differs between environments (``postgres`` locally, a DBA
    # role on managed PostgreSQL), so it must never influence the hash.
    roles = await conn.fetch(
        """
        SELECT r.rolname AS name, r.rolsuper, r.rolcreaterole, r.rolcreatedb,
               r.rolcanlogin, r.rolinherit, r.rolreplication, r.rolbypassrls,
               COALESCE((SELECT string_agg(option, ',' ORDER BY option)
                         FROM unnest(r.rolconfig) AS option), '') AS config,
               ARRAY(
                   SELECT granted.rolname || ':' ||
                          membership.admin_option::integer || ':' ||
                          membership.inherit_option::integer || ':' ||
                          membership.set_option::integer
                   FROM pg_auth_members AS membership
                   JOIN pg_roles AS granted ON granted.oid = membership.roleid
                   WHERE membership.member = r.oid
                   ORDER BY granted.rolname
               ) AS memberships
        FROM pg_roles AS r
        WHERE r.rolname LIKE $1 ESCAPE E'\\\\'
        ORDER BY r.rolname
        """,
        _like_prefix_pattern(role_prefix),
    )
    for row in roles:
        name = _normalized_owner(row["name"], principal_map)
        flags = "".join(
            letter
            for letter, present in (
                ("S", row["rolsuper"]),
                ("R", row["rolcreaterole"]),
                ("D", row["rolcreatedb"]),
                ("L", row["rolcanlogin"]),
                ("I", row["rolinherit"]),
                ("P", row["rolreplication"]),
                ("B", row["rolbypassrls"]),
            )
            if present
        )
        memberships = []
        for membership in row["memberships"] or []:
            physical_role, admin, inherit, set_option = str(membership).rsplit(":", 3)
            logical_role = _normalized_owner(physical_role, principal_map)
            memberships.append(f"{logical_role}:admin={admin}:inherit={inherit}:set={set_option}")
        lines.append(f"role:{name}:{flags}:{row['config']}:memberships={','.join(memberships)}")

    return lines


async def extensions_lines(conn: Any, *, role_prefix: str) -> list[str]:
    """Allowlisted extensions only; anything else fails closed."""
    try:
        return await extension_catalog_lines(conn, logical_principal_map(role_prefix))
    except CatalogFingerprintError as exc:
        raise FingerprintError(str(exc)) from exc


async def reference_data_lines(conn: Any, reference_sets: Iterable[ReferenceDataSet]) -> list[str]:
    """Exact rows of declared system-owned immutable tables."""
    lines: list[str] = []
    for ref in sorted(reference_sets, key=lambda item: item.table):
        columns = list(dict.fromkeys([*ref.natural_key, *ref.immutable_columns]))
        column_list = ", ".join(columns)
        where = f" WHERE {ref.where}" if ref.where else ""
        rows = await conn.fetch(f"SELECT {column_list} FROM {ref.table}{where}")
        canonical_rows = sorted(_canonical_row(row[column] for column in columns) for row in rows)
        lines.append(f"reference:{ref.table}:columns={','.join(columns)}:rows={len(rows)}")
        lines.extend(f"row:{ref.table}:{values}" for values in canonical_rows)
    return lines


def _canonical_value(value: Any) -> str:
    """Type-tagged canonical JSON for one PostgreSQL/JSON value."""
    return json.dumps(
        _canonical_value_tree(value),
        ensure_ascii=True,
        separators=(",", ":"),
    )


def _canonical_row(values: Iterable[Any]) -> str:
    return json.dumps(
        [_canonical_value_tree(value) for value in values],
        ensure_ascii=True,
        separators=(",", ":"),
    )


def _canonical_value_tree(value: Any) -> list[Any]:
    try:
        return canonical_value_tree(value)
    except UnsupportedFingerprintValue as exc:
        raise FingerprintError(str(exc)) from exc


async def compute_fingerprints(
    conn: Any,
    *,
    role_prefix: str,
    reference_sets: Iterable[ReferenceDataSet] = (),
) -> dict[str, str]:
    """Compute all four fingerprints in one read-only pass."""
    # PostgreSQL deparse helpers omit schema qualification according to the
    # caller's search_path. Pin it while hashing so admin, migrator and app
    # connections serialize the same catalog facts, then restore the caller.
    async with conn.transaction():
        previous_search_path = await conn.fetchval("SELECT current_setting('search_path')")
        await conn.execute("SELECT set_config('search_path', 'pg_catalog', true)")
        try:
            structural = canonical_digest(await structural_lines(conn))
            acl = canonical_digest(await acl_lines(conn, role_prefix=role_prefix))
            extensions = canonical_digest(await extensions_lines(conn, role_prefix=role_prefix))
            reference = canonical_digest(await reference_data_lines(conn, reference_sets))
        except Exception:
            raise
        else:
            await conn.execute(
                "SELECT set_config('search_path', $1, true)", previous_search_path
            )
    return {
        "structural": structural,
        "acl": acl,
        "extensions": extensions,
        "reference_data": reference,
    }


def fingerprint_classes() -> tuple[str, ...]:
    return _FINGERPRINT_CLASSES
