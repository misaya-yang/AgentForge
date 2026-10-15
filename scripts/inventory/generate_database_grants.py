#!/usr/bin/env python3
"""Generate or verify fail-closed ARC-03 least-privilege ``grants.sql``.

The data-access inventory is discovery evidence, not an authorization policy:
it can say that a source unit reads or writes a table, but it cannot prove the
exact DML verb, runtime role, final schema, or overloaded function signature.
This tool therefore requires a separately reviewed owner policy and refuses to
emit SQL until every grantable inventory object and every observed access has
explicit evidence.

Policy shape (``arc03-grants-policy/v1``)::

    {
      "schema_version": "arc03-grants-policy/v1",
      "inventory_sha256": "<sha256 of exact inventory bytes>",
      "objects": [
        {
          "inventory_kind": "table",
          "inventory_name": "sessions",
          "schema": "assistant",
          "name": "sessions",
          "owner": "owner",
          "evidence": ["review-or-source-reference"],
          "access": [
            {
              "units": ["gateway"],
              "roles": ["runtime"],
              "privileges": ["SELECT"],
              "evidence": ["runtime-query-review"]
            }
          ]
        }
      ]
    }

Functions additionally require ``identity_arguments`` (possibly ``""``).
Tables written only through reviewed functions may set ``function_only`` true
and list every inventory function writer in ``function_evidence``.  No broad
schema DML, ``ALL``, schema ``CREATE``, or PUBLIC grant can be generated.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

POLICY_SCHEMA = "arc03-grants-policy/v1"
ROLE_SUFFIXES = frozenset(
    {"gateway", "runtime", "capability_worker", "knowledge_api", "knowledge_worker"}
)
REVOKE_ROLE_SUFFIXES = frozenset({*ROLE_SUFFIXES, "migrator"})
PLATFORM_SCHEMAS = frozenset({"public", "gateway", "assistant", "knowledge"})
KIND_PRIVILEGES: dict[str, frozenset[str]] = {
    "table": frozenset({"SELECT", "INSERT", "UPDATE", "DELETE"}),
    "view": frozenset({"SELECT"}),
    "sequence": frozenset({"USAGE", "SELECT"}),
    "function": frozenset({"EXECUTE"}),
    "type": frozenset({"USAGE"}),
}
PRIVILEGE_ORDER = {
    name: position
    for position, name in enumerate(("SELECT", "INSERT", "UPDATE", "DELETE", "USAGE", "EXECUTE"))
}
IDENTIFIER_RE = re.compile(r"[a-z_][a-z0-9_]{0,62}")
ROLE_PREFIX_RE = re.compile(r"[a-z][a-z0-9_]{0,20}_")
IDENTITY_ARGUMENTS_RE = re.compile(r"[a-z0-9_ ,.\[\]()+]*")
WRITE_PRIVILEGES = frozenset({"INSERT", "UPDATE", "DELETE"})


class GrantContractError(RuntimeError):
    """Raised with deterministic unresolved findings; no SQL may be written."""

    def __init__(self, unresolved: list[str]) -> None:
        self.unresolved = sorted(set(unresolved))
        super().__init__("grant policy is unresolved:\n- " + "\n- ".join(self.unresolved))


@dataclass(frozen=True)
class InventoryObject:
    kind: str
    name: str
    readers: tuple[str, ...] = ()
    writers: tuple[str, ...] = ()
    function_writers: tuple[str, ...] = ()

    @property
    def key(self) -> tuple[str, str]:
        return self.kind, self.name


@dataclass(frozen=True)
class Grant:
    kind: str
    schema: str
    name: str
    identity_arguments: str
    role: str
    privileges: tuple[str, ...]


@dataclass(frozen=True)
class GrantObject:
    kind: str
    schema: str
    name: str
    identity_arguments: str


def _strings(value: object, context: str, unresolved: list[str]) -> tuple[str, ...]:
    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        unresolved.append(f"{context} must be a list of strings")
        return ()
    return tuple(sorted(set(value)))


def _inventory_objects(inventory: dict[str, Any]) -> dict[tuple[str, str], InventoryObject]:
    unresolved: list[str] = []
    postgresql = inventory.get("postgresql")
    if not isinstance(postgresql, dict):
        raise GrantContractError(["inventory.postgresql must be an object"])

    objects: dict[tuple[str, str], InventoryObject] = {}

    def add(item: InventoryObject) -> None:
        if item.key in objects:
            unresolved.append(f"inventory duplicates {item.kind} {item.name!r}")
        else:
            objects[item.key] = item

    tables = postgresql.get("tables")
    if not isinstance(tables, list):
        unresolved.append("inventory.postgresql.tables must be a list")
    else:
        for index, row in enumerate(tables):
            context = f"inventory table[{index}]"
            if not isinstance(row, dict) or not isinstance(row.get("table"), str):
                unresolved.append(f"{context} must name a table")
                continue
            add(
                InventoryObject(
                    kind="table",
                    name=row["table"],
                    readers=_strings(
                        row.get("static_readers", []), f"{context}.readers", unresolved
                    ),
                    writers=_strings(
                        row.get("static_writers", []), f"{context}.writers", unresolved
                    ),
                    function_writers=_strings(
                        row.get("function_writers", []),
                        f"{context}.function_writers",
                        unresolved,
                    ),
                )
            )

    for kind, field in (
        ("view", "views"),
        ("function", "functions"),
        ("type", "types"),
    ):
        values = postgresql.get(field, [])
        if not isinstance(values, list) or any(not isinstance(value, str) for value in values):
            unresolved.append(f"inventory.postgresql.{field} must be a list of strings")
            continue
        for value in values:
            add(InventoryObject(kind=kind, name=value))

    explicit_sequences = postgresql.get("sequences_explicit", [])
    if not isinstance(explicit_sequences, list) or any(
        not isinstance(value, str) for value in explicit_sequences
    ):
        unresolved.append("inventory.postgresql.sequences_explicit must be a list of strings")
    else:
        for value in explicit_sequences:
            add(InventoryObject(kind="sequence", name=value))

    implicit_sequences = postgresql.get("sequences_implicit_serial", [])
    if not isinstance(implicit_sequences, list):
        unresolved.append("inventory.postgresql.sequences_implicit_serial must be a list")
    else:
        for index, row in enumerate(implicit_sequences):
            if not isinstance(row, dict) or not isinstance(row.get("name"), str):
                unresolved.append(f"inventory implicit sequence[{index}] must name a sequence")
                continue
            add(InventoryObject(kind="sequence", name=row["name"]))

    if unresolved:
        raise GrantContractError(unresolved)
    return objects


def _validate_identifier(value: object, context: str, unresolved: list[str]) -> str:
    if not isinstance(value, str) or IDENTIFIER_RE.fullmatch(value) is None:
        unresolved.append(f"{context} must be a lowercase SQL identifier")
        return "invalid"
    return value


def _load_policy_entries(
    inventory_objects: dict[tuple[str, str], InventoryObject],
    policy: dict[str, Any],
) -> tuple[list[Grant], list[GrantObject]]:
    unresolved: list[str] = []
    if policy.get("schema_version") != POLICY_SCHEMA:
        unresolved.append(f"policy.schema_version must be {POLICY_SCHEMA!r}")
    raw_objects = policy.get("objects")
    if not isinstance(raw_objects, list):
        raise GrantContractError([*unresolved, "policy.objects must be a list"])

    seen: set[tuple[str, str]] = set()
    qualified_seen: set[tuple[str, str, str, str]] = set()
    grants: list[Grant] = []
    managed_objects: list[GrantObject] = []
    for index, row in enumerate(raw_objects):
        context = f"policy object[{index}]"
        if not isinstance(row, dict):
            unresolved.append(f"{context} must be an object")
            continue
        kind = str(row.get("inventory_kind", ""))
        inventory_name = str(row.get("inventory_name", ""))
        key = (kind, inventory_name)
        item = inventory_objects.get(key)
        if item is None:
            unresolved.append(
                f"{context} references unknown inventory object {kind}:{inventory_name}"
            )
            continue
        if key in seen:
            unresolved.append(f"policy duplicates {kind}:{inventory_name}")
            continue
        seen.add(key)

        schema = _validate_identifier(row.get("schema"), f"{context}.schema", unresolved)
        name = _validate_identifier(row.get("name"), f"{context}.name", unresolved)
        if schema not in PLATFORM_SCHEMAS:
            unresolved.append(f"{context}.schema must be one of {sorted(PLATFORM_SCHEMAS)}")
        if row.get("owner") != "owner":
            unresolved.append(f"{context}.owner must be the NOLOGIN logical owner 'owner'")
        evidence = _strings(row.get("evidence"), f"{context}.evidence", unresolved)
        if not evidence:
            unresolved.append(f"{context} has no ownership evidence")

        identity_arguments = ""
        if kind == "function":
            raw_arguments = row.get("identity_arguments")
            if (
                not isinstance(raw_arguments, str)
                or IDENTITY_ARGUMENTS_RE.fullmatch(raw_arguments) is None
            ):
                unresolved.append(f"{context}.identity_arguments is missing or unsafe")
            else:
                identity_arguments = raw_arguments
        elif "identity_arguments" in row:
            unresolved.append(f"{context}.identity_arguments is only valid for functions")

        qualified_key = (kind, schema, name, identity_arguments)
        if qualified_key in qualified_seen:
            unresolved.append(f"policy maps multiple inventory objects to {qualified_key}")
        qualified_seen.add(qualified_key)
        managed_objects.append(GrantObject(kind, schema, name, identity_arguments))

        accesses = row.get("access")
        if not isinstance(accesses, list):
            unresolved.append(f"{context}.access must be a list")
            accesses = []
        covered_readers: set[str] = set()
        covered_writers: set[str] = set()
        for access_index, access in enumerate(accesses):
            access_context = f"{context}.access[{access_index}]"
            if not isinstance(access, dict):
                unresolved.append(f"{access_context} must be an object")
                continue
            units = set(_strings(access.get("units"), f"{access_context}.units", unresolved))
            roles = _strings(access.get("roles"), f"{access_context}.roles", unresolved)
            privileges = _strings(
                access.get("privileges"), f"{access_context}.privileges", unresolved
            )
            access_evidence = _strings(
                access.get("evidence"), f"{access_context}.evidence", unresolved
            )
            if not units:
                unresolved.append(f"{access_context} has no source unit")
            if not roles:
                unresolved.append(f"{access_context} has no runtime role")
            if not privileges:
                unresolved.append(f"{access_context} has no privileges")
            if not access_evidence:
                unresolved.append(f"{access_context} has no reviewed evidence")
            invalid_roles = sorted(set(roles) - ROLE_SUFFIXES)
            if invalid_roles:
                unresolved.append(f"{access_context} has unknown roles {invalid_roles}")
            invalid_privileges = sorted(set(privileges) - KIND_PRIVILEGES[kind])
            if invalid_privileges:
                unresolved.append(
                    f"{access_context} has invalid {kind} privileges {invalid_privileges}"
                )
            for unit in units:
                if unit in item.readers and "SELECT" in privileges:
                    covered_readers.add(unit)
                if unit in item.writers and set(privileges) & WRITE_PRIVILEGES:
                    covered_writers.add(unit)
            for role in roles:
                if role not in ROLE_SUFFIXES or invalid_privileges or not privileges:
                    continue
                grants.append(
                    Grant(
                        kind=kind,
                        schema=schema,
                        name=name,
                        identity_arguments=identity_arguments,
                        role=role,
                        privileges=tuple(sorted(set(privileges), key=PRIVILEGE_ORDER.__getitem__)),
                    )
                )

        missing_readers = sorted(set(item.readers) - {"database-tools"} - covered_readers)
        missing_writers = sorted(set(item.writers) - {"database-tools"} - covered_writers)
        if missing_readers:
            unresolved.append(f"{context} has unresolved readers {missing_readers}")
        if missing_writers:
            unresolved.append(
                f"{context} has unresolved generic writers {missing_writers}; exact DML must be reviewed"
            )

        if item.function_writers:
            function_only = row.get("function_only") is True
            function_evidence = set(
                _strings(
                    row.get("function_evidence", []),
                    f"{context}.function_evidence",
                    unresolved,
                )
            )
            missing_functions = sorted(set(item.function_writers) - function_evidence)
            if missing_functions:
                unresolved.append(
                    f"{context} function-mediated writes are unresolved: {missing_functions or list(item.function_writers)}"
                )
            if not item.writers and not function_only:
                unresolved.append(
                    f"{context} has only function-mediated writes and must set function_only"
                )
            if function_only and any(
                set(grant.privileges) & WRITE_PRIVILEGES
                for grant in grants
                if grant.kind == kind and grant.schema == schema and grant.name == name
            ):
                unresolved.append(f"{context} is function_only but grants direct table DML")

    missing_policy = sorted(set(inventory_objects) - seen)
    if missing_policy:
        unresolved.extend(
            f"missing owner mapping for {kind}:{name}" for kind, name in missing_policy
        )
    if unresolved:
        raise GrantContractError(unresolved)
    return grants, managed_objects


def _quote_identifier(value: str) -> str:
    return f'"{value}"'


def _object_sql(grant: Grant | GrantObject) -> tuple[str, str]:
    qualified = f"{_quote_identifier(grant.schema)}.{_quote_identifier(grant.name)}"
    if grant.kind == "function":
        return "FUNCTION", f"{qualified}({grant.identity_arguments})"
    if grant.kind in {"table", "view"}:
        return "TABLE", qualified
    return grant.kind.upper(), qualified


def generate_grants_sql(
    inventory_bytes: bytes,
    policy_bytes: bytes,
    *,
    role_prefix: str = "ai_gateway_",
) -> str:
    """Return deterministic SQL or raise with every unresolved mapping."""
    if ROLE_PREFIX_RE.fullmatch(role_prefix) is None:
        raise GrantContractError([f"unsafe role prefix {role_prefix!r}"])
    try:
        inventory = json.loads(inventory_bytes)
        policy = json.loads(policy_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise GrantContractError([f"invalid JSON input: {type(exc).__name__}"]) from exc
    if not isinstance(inventory, dict) or not isinstance(policy, dict):
        raise GrantContractError(["inventory and policy must both be JSON objects"])

    inventory_sha = hashlib.sha256(inventory_bytes).hexdigest()
    if policy.get("inventory_sha256") != inventory_sha:
        raise GrantContractError(
            [
                "policy.inventory_sha256 does not bind the exact data-access inventory: "
                f"expected {inventory_sha}, got {policy.get('inventory_sha256')!r}"
            ]
        )
    objects = _inventory_objects(inventory)
    grants, managed_objects = _load_policy_entries(objects, policy)
    policy_sha = hashlib.sha256(policy_bytes).hexdigest()

    merged: dict[tuple[str, str, str, str, str], set[str]] = {}
    for grant in grants:
        key = (grant.kind, grant.schema, grant.name, grant.identity_arguments, grant.role)
        merged.setdefault(key, set()).update(grant.privileges)

    lines = [
        "-- Generated by scripts/inventory/generate_database_grants.py; DO NOT EDIT.",
        f"-- data-access-inventory-sha256: {inventory_sha}",
        f"-- reviewed-grants-policy-sha256: {policy_sha}",
        "-- This file grants no schema CREATE and no privileges to PUBLIC.",
        "",
    ]
    for schema in sorted(PLATFORM_SCHEMAS):
        for role in sorted(REVOKE_ROLE_SUFFIXES):
            lines.append(
                f"REVOKE ALL PRIVILEGES ON SCHEMA {_quote_identifier(schema)} "
                f"FROM {_quote_identifier(role_prefix + role)};"
            )
    lines.append("")

    for managed in sorted(
        managed_objects,
        key=lambda item: (item.kind, item.schema, item.name, item.identity_arguments),
    ):
        object_kind, object_name = _object_sql(managed)
        for role in sorted(REVOKE_ROLE_SUFFIXES):
            lines.append(
                f"REVOKE ALL PRIVILEGES ON {object_kind} {object_name} "
                f"FROM {_quote_identifier(role_prefix + role)};"
            )
    if managed_objects:
        lines.append("")

    schema_roles = sorted({(schema, role) for _kind, schema, _name, _arguments, role in merged})
    for schema, role in schema_roles:
        lines.append(
            f"GRANT USAGE ON SCHEMA {_quote_identifier(schema)} "
            f"TO {_quote_identifier(role_prefix + role)};"
        )
    if schema_roles:
        lines.append("")

    for key in sorted(merged):
        kind, schema, name, identity_arguments, role = key
        privileges = tuple(sorted(merged[key], key=PRIVILEGE_ORDER.__getitem__))
        grant = Grant(kind, schema, name, identity_arguments, role, privileges)
        object_kind, object_name = _object_sql(grant)
        lines.append(
            f"GRANT {', '.join(privileges)} ON {object_kind} {object_name} "
            f"TO {_quote_identifier(role_prefix + role)};"
        )
    return "\n".join(lines) + "\n"


def _read(path: Path) -> bytes:
    try:
        return path.read_bytes()
    except OSError as exc:
        raise GrantContractError([f"cannot read {path}: {type(exc).__name__}"]) from exc


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inventory", type=Path, required=True)
    parser.add_argument("--policy", type=Path, required=True)
    destination = parser.add_mutually_exclusive_group(required=True)
    destination.add_argument("--output", type=Path)
    destination.add_argument("--check", type=Path)
    args = parser.parse_args(argv)

    try:
        rendered = generate_grants_sql(_read(args.inventory), _read(args.policy))
        if args.check is not None:
            actual = _read(args.check).decode("utf-8")
            if actual != rendered:
                raise GrantContractError(
                    [f"{args.check} differs from deterministic inventory+policy output"]
                )
        else:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(rendered, encoding="utf-8")
    except GrantContractError as exc:
        print(
            json.dumps(
                {"status": "BLOCKED", "unresolved": exc.unresolved},
                indent=2,
                sort_keys=True,
            ),
            file=sys.stderr,
        )
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
