#!/usr/bin/env python3
"""Render ARC-03 relocation and read-only verification from bound policy.

The generated block is not evidence that a live catalog matches.  It gives the
cutover an exact, reviewable destination for every statically inventoried
persistent object; ``verify.sql`` and the live freeze independently query the
catalog and fail closed on missing, duplicate, misplaced or weakly-owned
objects.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any

POLICY_SCHEMA = "arc03-object-ownership/v1"
START_MARKER = "-- BEGIN ARC03 GENERATED OBJECT RELOCATION"
END_MARKER = "-- END ARC03 GENERATED OBJECT RELOCATION"
SCHEMAS = ("assistant", "gateway", "knowledge", "public")
LEDGERS = (
    "platform_schema_baselines",
    "platform_schema_changes",
    "platform_schema_change_attempts",
    "schema_migrations",
    "schema_migrations_meta",
)
IDENTIFIER = re.compile(r"[a-z_][a-z0-9_]{0,62}")
IDENTITY_ARGUMENTS = re.compile(r"[a-z0-9_ ,.\[\]()+]*")
APP_ROLES = (
    "gateway",
    "runtime",
    "capability_worker",
    "knowledge_api",
    "knowledge_worker",
)


class RenderContractError(RuntimeError):
    """The static policy cannot safely produce SQL."""


@dataclass(frozen=True)
class ObjectPolicy:
    kind: str
    name: str
    schema: str
    identity_arguments: str = ""

    @property
    def key(self) -> tuple[str, str]:
        return self.kind, self.name


def _load_json(path: Path) -> tuple[dict[str, Any], bytes]:
    try:
        content = path.read_bytes()
        payload = json.loads(content)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise RenderContractError(f"cannot load {path}: {type(exc).__name__}") from exc
    if not isinstance(payload, dict):
        raise RenderContractError(f"{path} must contain a JSON object")
    return payload, content


def _inventory_keys(inventory: dict[str, Any]) -> set[tuple[str, str]]:
    postgresql = inventory.get("postgresql")
    if not isinstance(postgresql, dict):
        raise RenderContractError("inventory.postgresql must be an object")
    keys: set[tuple[str, str]] = set()

    def add(kind: str, name: object) -> None:
        if not isinstance(name, str) or IDENTIFIER.fullmatch(name) is None:
            raise RenderContractError(f"unsafe inventory {kind} name {name!r}")
        key = (kind, name)
        if key in keys:
            raise RenderContractError(f"duplicate inventory object {kind}:{name}")
        keys.add(key)

    tables = postgresql.get("tables", [])
    if not isinstance(tables, list):
        raise RenderContractError("inventory tables must be a list")
    for row in tables:
        if not isinstance(row, dict):
            raise RenderContractError("inventory table entry must be an object")
        add("table", row.get("table"))
    for kind, field in (("view", "views"), ("function", "functions"), ("type", "types")):
        values = postgresql.get(field, [])
        if not isinstance(values, list):
            raise RenderContractError(f"inventory {field} must be a list")
        for value in values:
            add(kind, value)
    for value in postgresql.get("sequences_explicit", []):
        add("sequence", value)
    for row in postgresql.get("sequences_implicit_serial", []):
        if not isinstance(row, dict):
            raise RenderContractError("implicit sequence entry must be an object")
        add("sequence", row.get("name"))
    return keys


def _validated_policy(
    inventory: dict[str, Any],
    inventory_bytes: bytes,
    policy: dict[str, Any],
) -> list[ObjectPolicy]:
    if policy.get("schema") != POLICY_SCHEMA:
        raise RenderContractError(f"ownership policy schema must be {POLICY_SCHEMA!r}")
    digest = hashlib.sha256(inventory_bytes).hexdigest()
    if policy.get("inventory_sha256") != digest:
        raise RenderContractError("ownership policy does not bind the exact inventory bytes")
    raw_objects = policy.get("objects")
    if not isinstance(raw_objects, list):
        raise RenderContractError("ownership policy objects must be a list")
    objects: list[ObjectPolicy] = []
    seen: set[tuple[str, str]] = set()
    for row in raw_objects:
        if not isinstance(row, dict):
            raise RenderContractError("ownership policy entry must be an object")
        kind = str(row.get("kind", ""))
        name = str(row.get("name", ""))
        schema = str(row.get("schema", ""))
        if kind not in {"table", "view", "sequence", "function", "type"}:
            raise RenderContractError(f"unsupported object kind {kind!r}")
        if IDENTIFIER.fullmatch(name) is None or schema not in SCHEMAS:
            raise RenderContractError(f"unsafe mapping {kind}:{schema}.{name}")
        if row.get("inventory_name") != name or row.get("owner") != "owner":
            raise RenderContractError(f"mapping {kind}:{name} must retain name and owner")
        evidence = row.get("evidence")
        if not isinstance(evidence, list) or not evidence:
            raise RenderContractError(f"mapping {kind}:{name} lacks ownership evidence")
        arguments = str(row.get("identity_arguments", ""))
        if kind == "function" and IDENTITY_ARGUMENTS.fullmatch(arguments) is None:
            raise RenderContractError(f"unsafe function identity for {name}")
        if kind != "function" and arguments:
            raise RenderContractError(f"identity arguments only apply to functions: {name}")
        item = ObjectPolicy(kind, name, schema, arguments)
        if item.key in seen:
            raise RenderContractError(f"duplicate ownership mapping {kind}:{name}")
        seen.add(item.key)
        objects.append(item)
    expected = _inventory_keys(inventory)
    if seen != expected:
        missing = sorted(expected - seen)
        extra = sorted(seen - expected)
        raise RenderContractError(f"ownership coverage mismatch: missing={missing}, extra={extra}")
    return sorted(objects, key=lambda item: (item.kind, item.name))


def load_policy(inventory_path: Path, policy_path: Path) -> list[ObjectPolicy]:
    inventory, inventory_bytes = _load_json(inventory_path)
    policy, _policy_bytes = _load_json(policy_path)
    return _validated_policy(inventory, inventory_bytes, policy)


def load_policy_bytes(inventory_bytes: bytes, policy_bytes: bytes) -> list[ObjectPolicy]:
    """Validate in-memory artifacts before any live freeze file is written."""
    try:
        inventory = json.loads(inventory_bytes)
        policy = json.loads(policy_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise RenderContractError("inventory/policy bytes are invalid JSON") from exc
    if not isinstance(inventory, dict) or not isinstance(policy, dict):
        raise RenderContractError("inventory/policy bytes must contain JSON objects")
    return _validated_policy(inventory, inventory_bytes, policy)


def load_grant_matrix(
    inventory_bytes: bytes,
    policy_bytes: bytes,
) -> set[tuple[str, str, str, str, str, str]]:
    """Return exact explicit ACL rows after validating inventory binding."""
    try:
        policy = json.loads(policy_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise RenderContractError("grants policy bytes are invalid JSON") from exc
    if not isinstance(policy, dict) or policy.get("schema_version") != "arc03-grants-policy/v1":
        raise RenderContractError("grants policy has the wrong schema")
    if policy.get("inventory_sha256") != hashlib.sha256(inventory_bytes).hexdigest():
        raise RenderContractError("grants policy does not bind the exact inventory bytes")
    objects = policy.get("objects")
    if not isinstance(objects, list):
        raise RenderContractError("grants policy objects must be a list")
    matrix: set[tuple[str, str, str, str, str, str]] = set()
    schema_roles: set[tuple[str, str]] = set()
    for item in objects:
        if not isinstance(item, dict):
            raise RenderContractError("grants policy object must be an object")
        kind = str(item.get("inventory_kind", ""))
        schema = str(item.get("schema", ""))
        name = str(item.get("name", ""))
        arguments = str(item.get("identity_arguments", ""))
        accesses = item.get("access")
        if (
            kind not in {"table", "view", "sequence", "function", "type"}
            or schema not in SCHEMAS
            or IDENTIFIER.fullmatch(name) is None
            or not isinstance(accesses, list)
        ):
            raise RenderContractError(f"unsafe grants mapping {kind}:{schema}.{name}")
        for access in accesses:
            if not isinstance(access, dict):
                raise RenderContractError("grants access entry must be an object")
            roles = access.get("roles")
            privileges = access.get("privileges")
            if not isinstance(roles, list) or not isinstance(privileges, list):
                raise RenderContractError("grants access roles/privileges must be lists")
            for role in roles:
                if role not in APP_ROLES:
                    raise RenderContractError(f"unknown grants role {role!r}")
                schema_roles.add((schema, str(role)))
                for privilege in privileges:
                    if not isinstance(privilege, str) or not privilege.isupper():
                        raise RenderContractError("unsafe grants privilege")
                    matrix.add(
                        (
                            kind,
                            schema,
                            name,
                            arguments,
                            f"ai_gateway_{role}",
                            privilege,
                        )
                    )
    for schema, role in schema_roles:
        matrix.add(("schema", schema, schema, "", f"ai_gateway_{role}", "USAGE"))
    return matrix


def _sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def _values(objects: list[ObjectPolicy], fields: tuple[str, ...]) -> str:
    rows = []
    for item in objects:
        rows.append(
            "(" + ", ".join(_sql_literal(str(getattr(item, field))) for field in fields) + ")"
        )
    if not rows:
        # The caller filters the typed sentinel before entering its loop.
        return "VALUES (NULL::text, NULL::text, NULL::text)"
    return "VALUES\n            " + ",\n            ".join(rows)


def render_relocation(objects: list[ObjectPolicy]) -> str:
    relations = [item for item in objects if item.kind in {"table", "view", "sequence"}]
    functions = [item for item in objects if item.kind == "function"]
    types = [item for item in objects if item.kind == "type"]
    relation_values = _values(relations, ("kind", "name", "schema"))
    function_values = _values(functions, ("name", "identity_arguments", "schema"))
    type_values = _values(types, ("name", "schema", "schema"))
    # The third duplicate type column keeps the zero-row helper shape uniform.
    return f"""{START_MARKER}
-- Generated from ownership-policy.json; DO NOT EDIT THIS BLOCK.
-- Missing or duplicate source objects abort before any relocation is accepted.
DO $$
DECLARE
    desired RECORD;
    source_schema TEXT;
    source_relkind TEXT;
    matches BIGINT;
BEGIN
    FOR desired IN
        SELECT * FROM (
            {relation_values}
        ) AS mapping(kind, name, target_schema)
        -- PostgreSQL moves an owned serial/identity sequence together with
        -- its table and rejects moving that sequence independently first.
        -- Relocate tables/views before sequences; the later sequence row then
        -- observes the already-correct target schema and remains a full
        -- existence/uniqueness check rather than a skipped object.
        ORDER BY CASE kind
                     WHEN 'table' THEN 0
                     WHEN 'view' THEN 1
                     WHEN 'sequence' THEN 2
                     ELSE 3
                 END,
                 name
    LOOP
        SELECT count(*), min(namespace.nspname), min(class.relkind::text)
        INTO matches, source_schema, source_relkind
        FROM pg_class AS class
        JOIN pg_namespace AS namespace ON namespace.oid = class.relnamespace
        WHERE namespace.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
          AND class.relname = desired.name
          AND CASE desired.kind
              WHEN 'table' THEN class.relkind IN ('r', 'p', 'f')
              WHEN 'view' THEN class.relkind IN ('v', 'm')
              WHEN 'sequence' THEN class.relkind = 'S'
              ELSE FALSE
          END
          AND NOT EXISTS (
              SELECT 1 FROM pg_depend AS dependency
              WHERE dependency.classid = 'pg_class'::regclass
                AND dependency.objid = class.oid
                AND dependency.deptype = 'e'
          );
        IF matches <> 1 THEN
            RAISE EXCEPTION 'ARC03 relocation % % matched % objects',
                desired.kind, desired.name, matches;
        END IF;
        IF source_schema <> desired.target_schema THEN
            EXECUTE format(
                'ALTER %s %I.%I SET SCHEMA %I',
                CASE
                    WHEN source_relkind = 'S' THEN 'SEQUENCE'
                    WHEN source_relkind = 'v' THEN 'VIEW'
                    WHEN source_relkind = 'm' THEN 'MATERIALIZED VIEW'
                    WHEN source_relkind = 'f' THEN 'FOREIGN TABLE'
                    ELSE 'TABLE'
                END,
                source_schema, desired.name, desired.target_schema
            );
        END IF;
    END LOOP;

    FOR desired IN
        SELECT * FROM (
            {function_values}
        ) AS mapping(name, identity_arguments, target_schema)
        ORDER BY name, identity_arguments
    LOOP
        SELECT count(*), min(namespace.nspname)
        INTO matches, source_schema
        FROM pg_proc AS procedure
        JOIN pg_namespace AS namespace ON namespace.oid = procedure.pronamespace
        WHERE namespace.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
          AND procedure.proname = desired.name
          -- Policy identities are type-only signatures suitable for ALTER /
          -- GRANT syntax. The catalog identity formatter includes input names
          -- when declared, so compare the canonical input type vector instead.
          AND oidvectortypes(procedure.proargtypes) = desired.identity_arguments
          AND procedure.prokind = 'f'
          AND NOT EXISTS (
              SELECT 1 FROM pg_depend AS dependency
              WHERE dependency.classid = 'pg_proc'::regclass
                AND dependency.objid = procedure.oid
                AND dependency.deptype = 'e'
          );
        IF matches <> 1 THEN
            RAISE EXCEPTION 'ARC03 relocation function %(%) matched % objects',
                desired.name, desired.identity_arguments, matches;
        END IF;
        IF source_schema <> desired.target_schema THEN
            EXECUTE format(
                'ALTER FUNCTION %I.%I(%s) SET SCHEMA %I',
                source_schema, desired.name, desired.identity_arguments,
                desired.target_schema
            );
        END IF;
    END LOOP;

    FOR desired IN
        SELECT column1 AS name, column2 AS target_schema FROM (
            {type_values}
        ) AS mapping
        WHERE column1 IS NOT NULL
        ORDER BY name
    LOOP
        SELECT count(*), min(namespace.nspname)
        INTO matches, source_schema
        FROM pg_type AS type_object
        JOIN pg_namespace AS namespace ON namespace.oid = type_object.typnamespace
        LEFT JOIN pg_class AS relation ON relation.oid = type_object.typrelid
        WHERE namespace.nspname IN ('public', 'gateway', 'assistant', 'knowledge')
          AND type_object.typname = desired.name
          AND (type_object.typtype IN ('e', 'd', 'r', 'm')
               OR (type_object.typtype = 'c' AND relation.relkind = 'c'))
          AND NOT EXISTS (
              SELECT 1 FROM pg_depend AS dependency
              WHERE dependency.classid = 'pg_type'::regclass
                AND dependency.objid = type_object.oid
                AND dependency.deptype = 'e'
          );
        IF matches <> 1 THEN
            RAISE EXCEPTION 'ARC03 relocation type % matched % objects', desired.name, matches;
        END IF;
        IF source_schema <> desired.target_schema THEN
            EXECUTE format(
                'ALTER TYPE %I.%I SET SCHEMA %I',
                source_schema, desired.name, desired.target_schema
            );
        END IF;
    END LOOP;
END
$$;
{END_MARKER}"""


def _location_check(item: ObjectPolicy) -> str:
    name = _sql_literal(item.name)
    schema = _sql_literal(item.schema)
    if item.kind in {"table", "view", "sequence"}:
        relkinds = {
            "table": "('r', 'p', 'f')",
            "view": "('v', 'm')",
            "sequence": "('S')",
        }[item.kind]
        expression = (
            "SELECT count(*) = 1 FROM pg_class AS class "
            "JOIN pg_namespace AS namespace ON namespace.oid = class.relnamespace "
            f"WHERE namespace.nspname = {schema} AND class.relname = {name} "
            f"AND class.relkind IN {relkinds}"
        )
    elif item.kind == "function":
        arguments = _sql_literal(item.identity_arguments)
        expression = (
            "SELECT count(*) = 1 FROM pg_proc AS procedure "
            "JOIN pg_namespace AS namespace ON namespace.oid = procedure.pronamespace "
            f"WHERE namespace.nspname = {schema} AND procedure.proname = {name} "
            f"AND oidvectortypes(procedure.proargtypes) = {arguments}"
        )
    else:
        expression = (
            "SELECT count(*) = 1 FROM pg_type AS type_object "
            "JOIN pg_namespace AS namespace ON namespace.oid = type_object.typnamespace "
            f"WHERE namespace.nspname = {schema} AND type_object.typname = {name}"
        )
    check = _sql_literal(f"location:{item.kind}:{item.schema}.{item.name}")
    return f"({check}, ({expression}))"


def _grant_matrix_check(
    matrix: set[tuple[str, str, str, str, str, str]],
) -> str:
    if not matrix:
        raise RenderContractError("exact grant matrix cannot be empty")
    expected = ",\n                ".join(
        "(" + ", ".join(_sql_literal(value) for value in row) + ")"
        for row in sorted(matrix)
    )
    roles = ", ".join(
        _sql_literal(f"ai_gateway_{role}") for role in (*APP_ROLES, "migrator")
    )
    schemas = ", ".join(_sql_literal(schema) for schema in SCHEMAS)
    return f"""('grant-matrix', NOT EXISTS (
        WITH expected(kind, schema_name, object_name, identity_arguments, role_name, privilege) AS (
            VALUES
                {expected}
        ), actual AS (
            SELECT CASE
                       WHEN class.relkind = 'S' THEN 'sequence'
                       WHEN class.relkind IN ('v', 'm') THEN 'view'
                       ELSE 'table'
                   END,
                   namespace.nspname, class.relname, ''::text, role.rolname,
                   acl.privilege_type || CASE WHEN acl.is_grantable THEN ':GRANT' ELSE '' END
            FROM pg_class AS class
            JOIN pg_namespace AS namespace ON namespace.oid = class.relnamespace
            CROSS JOIN LATERAL aclexplode(
                COALESCE(
                    class.relacl,
                    acldefault(
                        CASE WHEN class.relkind = 'S' THEN 'S'::\"char\" ELSE 'r'::\"char\" END,
                        class.relowner
                    )
                )
            ) AS acl
            JOIN pg_roles AS role ON role.oid = acl.grantee
            WHERE namespace.nspname IN ({schemas})
              AND class.relkind IN ('r', 'p', 'f', 'v', 'm', 'S')
              AND role.rolname IN ({roles})
            UNION ALL
            SELECT 'function', namespace.nspname, procedure.proname,
                   oidvectortypes(procedure.proargtypes), role.rolname,
                   acl.privilege_type || CASE WHEN acl.is_grantable THEN ':GRANT' ELSE '' END
            FROM pg_proc AS procedure
            JOIN pg_namespace AS namespace ON namespace.oid = procedure.pronamespace
            CROSS JOIN LATERAL aclexplode(
                COALESCE(procedure.proacl, acldefault('f', procedure.proowner))
            ) AS acl
            JOIN pg_roles AS role ON role.oid = acl.grantee
            WHERE namespace.nspname IN ({schemas})
              AND role.rolname IN ({roles})
            UNION ALL
            SELECT 'type', namespace.nspname, type_object.typname, ''::text, role.rolname,
                   acl.privilege_type || CASE WHEN acl.is_grantable THEN ':GRANT' ELSE '' END
            FROM pg_type AS type_object
            JOIN pg_namespace AS namespace ON namespace.oid = type_object.typnamespace
            CROSS JOIN LATERAL aclexplode(
                COALESCE(type_object.typacl, acldefault('T', type_object.typowner))
            ) AS acl
            JOIN pg_roles AS role ON role.oid = acl.grantee
            WHERE namespace.nspname IN ({schemas})
              AND role.rolname IN ({roles})
            UNION ALL
            SELECT 'schema', namespace.nspname, namespace.nspname, ''::text, role.rolname,
                   acl.privilege_type || CASE WHEN acl.is_grantable THEN ':GRANT' ELSE '' END
            FROM pg_namespace AS namespace
            CROSS JOIN LATERAL aclexplode(
                COALESCE(namespace.nspacl, acldefault('n', namespace.nspowner))
            ) AS acl
            JOIN pg_roles AS role ON role.oid = acl.grantee
            WHERE namespace.nspname IN ({schemas})
              AND role.rolname IN ({roles})
        )
        SELECT 1 FROM (
            (SELECT * FROM expected EXCEPT SELECT * FROM actual)
            UNION ALL
            (SELECT * FROM actual EXCEPT SELECT * FROM expected)
        ) AS acl_drift
    ))"""


def render_verify(
    objects: list[ObjectPolicy],
    grant_matrix: set[tuple[str, str, str, str, str, str]],
) -> str:
    rows = [_location_check(item) for item in objects]
    schemas = ", ".join(_sql_literal(schema) for schema in SCHEMAS)
    ledgers = ", ".join(_sql_literal(name) for name in LEDGERS)
    principals = ", ".join(
        _sql_literal(f"ai_gateway_{name}")
        for name in (
            "owner",
            "migrator",
            "gateway",
            "runtime",
            "capability_worker",
            "knowledge_api",
            "knowledge_worker",
        )
    )
    app_principals = ", ".join(
        _sql_literal(f"ai_gateway_{name}")
        for name in (
            "migrator",
            "gateway",
            "runtime",
            "capability_worker",
            "knowledge_api",
            "knowledge_worker",
        )
    )
    rows.extend(
        [
            "('schema-owners', (SELECT count(*) = 4 FROM pg_namespace "
            f"WHERE nspname IN ({schemas}) AND pg_get_userbyid(nspowner) = 'ai_gateway_owner'))",
            "('role-attributes', (SELECT count(*) = 7 FROM pg_roles WHERE rolname IN "
            f"({principals}) AND NOT rolinherit AND NOT rolsuper AND NOT rolcreatedb "
            "AND NOT rolcreaterole AND NOT rolreplication AND NOT rolbypassrls))",
            "('owner-nologin', (SELECT NOT rolcanlogin FROM pg_roles "
            "WHERE rolname = 'ai_gateway_owner'))",
            "('application-login', (SELECT count(*) = 6 FROM pg_roles WHERE rolname IN "
            f"({app_principals}) AND rolcanlogin))",
            "('role-search-path', NOT EXISTS (SELECT 1 FROM pg_roles AS role "
            f"WHERE role.rolname IN ({principals}) AND NOT EXISTS (SELECT 1 FROM "
            "unnest(COALESCE(role.rolconfig, ARRAY[]::text[])) AS setting "
            "WHERE setting ~ '^search_path=pg_catalog,.*public$')))",
            "('no-schema-create', NOT EXISTS (SELECT 1 FROM unnest(ARRAY["
            f"{app_principals}]) AS role_name CROSS JOIN unnest(ARRAY[{schemas}]) AS schema_name "
            "WHERE has_schema_privilege(role_name, schema_name, 'CREATE')))",
            "('relation-owner', NOT EXISTS (SELECT 1 FROM pg_class AS class JOIN pg_namespace "
            "AS namespace ON namespace.oid = class.relnamespace WHERE namespace.nspname IN "
            f"({schemas}) AND class.relkind IN ('r','p','f','v','m','S') AND class.relname NOT IN "
            f"({ledgers}) AND pg_get_userbyid(class.relowner) <> 'ai_gateway_owner'))",
            "('routine-owner', NOT EXISTS (SELECT 1 FROM pg_proc AS procedure JOIN pg_namespace "
            "AS namespace ON namespace.oid = procedure.pronamespace WHERE namespace.nspname IN "
            f"({schemas}) AND pg_get_userbyid(procedure.proowner) <> 'ai_gateway_owner' AND NOT "
            "EXISTS (SELECT 1 FROM pg_depend AS dependency WHERE dependency.classid = "
            "'pg_proc'::regclass AND dependency.objid = procedure.oid AND dependency.deptype = 'e')))",
            "('security-definer', NOT EXISTS (SELECT 1 FROM pg_proc AS procedure JOIN pg_namespace "
            "AS namespace ON namespace.oid = procedure.pronamespace WHERE namespace.nspname IN "
            f"({schemas}) AND procedure.prosecdef AND (pg_get_userbyid(procedure.proowner) <> "
            "'ai_gateway_owner' OR NOT (COALESCE(procedure.proconfig, ARRAY[]::text[]) @> "
            "ARRAY[format('search_path=pg_catalog, %s', namespace.nspname)]))))",
            "('required-extensions', (SELECT array_agg(extname ORDER BY extname) FROM pg_extension "
            "WHERE extname <> 'plpgsql') = ARRAY['pg_trgm','pgcrypto','uuid-ossp']::name[])",
            "('no-public-schema-acl', NOT EXISTS (SELECT 1 FROM pg_namespace AS namespace CROSS "
            "JOIN LATERAL aclexplode(COALESCE(namespace.nspacl, acldefault('n', namespace.nspowner))) "
            f"AS acl WHERE namespace.nspname IN ({schemas}) AND acl.grantee = 0))",
            "('no-public-relation-acl', NOT EXISTS (SELECT 1 FROM pg_class AS class JOIN "
            "pg_namespace AS namespace ON namespace.oid = class.relnamespace CROSS JOIN LATERAL "
            "aclexplode(COALESCE(class.relacl, acldefault(CASE WHEN class.relkind = 'S' THEN 'S'::"
            "\"char\" ELSE 'r'::\"char\" END, class.relowner))) AS acl WHERE namespace.nspname IN "
            f"({schemas}) AND class.relkind IN ('r','p','f','v','m','S') AND acl.grantee = 0))",
            "('no-public-routine-acl', NOT EXISTS (SELECT 1 FROM pg_proc AS procedure JOIN "
            "pg_namespace AS namespace ON namespace.oid = procedure.pronamespace CROSS JOIN "
            "LATERAL aclexplode(COALESCE(procedure.proacl, acldefault('f', procedure.proowner))) "
            f"AS acl WHERE namespace.nspname IN ({schemas}) AND acl.grantee = 0))",
            "('no-public-default-acl', NOT EXISTS (SELECT 1 FROM pg_default_acl AS defaults "
            "CROSS JOIN LATERAL aclexplode(defaults.defaclacl) AS acl WHERE defaults.defaclrole = "
            "(SELECT oid FROM pg_roles WHERE rolname = 'ai_gateway_owner') AND acl.grantee = 0))",
        ]
    )
    rows.append(_grant_matrix_check(grant_matrix))
    return (
        "-- Generated by scripts/database/render_baseline_contract.py; DO NOT EDIT.\n"
        "SELECT checks.check_name, checks.ok\n"
        "FROM (VALUES\n    "
        + ",\n    ".join(rows)
        + "\n) AS checks(check_name, ok)\nORDER BY checks.check_name;\n"
    )


def replace_relocation(sql: str, generated: str) -> str:
    start = sql.find(START_MARKER)
    end = sql.find(END_MARKER)
    if start < 0 or end < start or sql.find(START_MARKER, start + 1) >= 0:
        raise RenderContractError("cutover must contain exactly one generated marker pair")
    end += len(END_MARKER)
    return sql[:start] + generated + sql[end:]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inventory", type=Path, required=True)
    parser.add_argument("--ownership-policy", type=Path, required=True)
    parser.add_argument("--grants-policy", type=Path, required=True)
    parser.add_argument("--cutover", type=Path, required=True)
    parser.add_argument("--verify", type=Path, required=True)
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    objects = load_policy(args.inventory, args.ownership_policy)
    grants = load_grant_matrix(
        args.inventory.read_bytes(),
        args.grants_policy.read_bytes(),
    )
    cutover = replace_relocation(
        args.cutover.read_text(encoding="utf-8"), render_relocation(objects)
    )
    verify = render_verify(objects, grants)
    if args.write:
        args.cutover.write_text(cutover, encoding="utf-8")
        args.verify.write_text(verify, encoding="utf-8")
        return 0
    drift = []
    if args.cutover.read_text(encoding="utf-8") != cutover:
        drift.append(str(args.cutover))
    if not args.verify.is_file() or args.verify.read_text(encoding="utf-8") != verify:
        drift.append(str(args.verify))
    if drift:
        raise SystemExit(f"DRIFT generated ARC-03 SQL: {sorted(drift)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
