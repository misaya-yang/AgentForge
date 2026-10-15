#!/usr/bin/env python3
"""Build ARC-03 PostgreSQL ownership and least-privilege policy artifacts.

This is static evidence, not live permission proof. It scans Python and Rust
SQL literals, assigns every persistent object to one platform schema, and
emits the exact policy consumed by ``generate_database_grants.py``. Live
freeze still has to validate every object, ACL and application journey.
"""

from __future__ import annotations

import argparse
import ast
import hashlib
import json
import re
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(REPO_ROOT / "scripts/core_boundary"))

import data_access  # noqa: E402
from _common import extract_sql_table_refs, git_head_sha, walk_files  # noqa: E402
from inventory_persistence_sql import owner_of  # noqa: E402

SCHEMA_VERSION = "arc03-data-access-policy/v1"
OWNERSHIP_SCHEMA = "arc03-object-ownership/v1"
GRANTS_SCHEMA = "arc03-grants-policy/v1"
PLATFORM_SCHEMAS = ("assistant", "gateway", "knowledge", "public")
LEDGER_TABLES = {
    "platform_schema_baselines",
    "platform_schema_changes",
    "platform_schema_change_attempts",
    "schema_migrations",
    "schema_migrations_meta",
}
DOMAIN_SCHEMA = {
    "auth": "gateway",
    "platform": "gateway",
    "local-node": "gateway",
    "agent-studio": "gateway",
    "eval-trace": "gateway",
    "mcp-connector": "gateway",
    "agent-runtime": "assistant",
    "memory": "assistant",
    "skills": "assistant",
    "image": "assistant",
    "quiz": "assistant",
    "sharing": "assistant",
    "artifact": "assistant",
    "knowledge": "knowledge",
}
KNOWLEDGE_OVERRIDES = {
    "segment_images",
    "confluence_connections",
    "confluence_space_bindings",
    "confluence_pages",
    "confluence_sync_tasks",
    "confluence_webhooks",
    "confluence_image_sync",
    "dataset_query_feedback",
    "kb_parsing_ir",
    "kb_parsing_page_cache",
    "kb_segment_attachment_bindings",
}
ASSISTANT_OVERRIDES = {
    "artifact_share_attempt_tokens",
    "image_blobs",
    "image_idempotency",
    "image_sessions",
    "image_tasks",
    "image_turns",
}
_WRITE_TARGETS = {
    "INSERT": re.compile(r"\bINSERT\s+INTO\s+([a-z_][a-z0-9_.]*)", re.I),
    "UPDATE": re.compile(r"\bUPDATE\s+([a-z_][a-z0-9_.]*)\s+SET\b", re.I),
    "DELETE": re.compile(r"\bDELETE\s+FROM\s+([a-z_][a-z0-9_.]*)", re.I),
}
_SQLISH = re.compile(r"\b(SELECT|INSERT\s+INTO|UPDATE|DELETE\s+FROM)\b", re.I)
_CREATE_FUNCTION = re.compile(
    r"CREATE\s+(?:OR\s+REPLACE\s+)?FUNCTION\s+([a-z_][a-z0-9_.]*)\s*\(",
    re.I,
)
_TYPE_CANONICAL = {
    "varchar": "character varying",
    "char": "character",
    "int": "integer",
    "int4": "integer",
    "int8": "bigint",
    "bool": "boolean",
    "timestamptz": "timestamp with time zone",
    "timestamp": "timestamp without time zone",
}


class DatabasePolicyError(RuntimeError):
    pass


def _serialized(payload: dict[str, Any]) -> bytes:
    return (json.dumps(payload, indent=2, sort_keys=True) + "\n").encode()


def _python_literals(path: Path) -> list[str]:
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"))
    except (SyntaxError, UnicodeDecodeError):
        return []
    values: list[str] = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            values.append(node.value)
        elif isinstance(node, ast.JoinedStr):
            values.extend(
                value.value
                for value in node.values
                if isinstance(value, ast.Constant) and isinstance(value.value, str)
            )
    return values


def _rust_literals(path: Path) -> list[str]:
    text = path.read_text(encoding="utf-8", errors="replace")
    values = [
        match.group("body")
        for match in re.finditer(
            r'r(?P<tag>#{0,8})"(?P<body>.*?)"(?P=tag)',
            text,
            re.DOTALL,
        )
    ]
    for match in re.finditer(r'"(?:\\.|[^"\\])*"', text, re.DOTALL):
        try:
            value = ast.literal_eval(match.group(0))
        except (SyntaxError, ValueError):
            continue
        if isinstance(value, str):
            values.append(value)
    return values


def _roles_for_path(path: str) -> tuple[str, ...]:
    if path.startswith("rust/agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/src/"):
        return ("runtime",)
    if path.startswith(
        "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/"
    ):
        return ("capability_worker",)
    if path.startswith("apps/knowledge-service/"):
        if any(
            marker in path
            for marker in (
                "/workers/",
                "/worker_",
                "/ingestion",
                "/indexing",
                "/parsing",
                "/embedding",
                "/backfill",
            )
        ):
            return ("knowledge_worker",)
        if "/persistence/database.py" in path or "/repositories/" in path:
            return ("knowledge_api", "knowledge_worker")
        return ("knowledge_api",)
    if path.startswith(("src/", "packages/ai-gateway-core/", "apps/local-node/")):
        return ("gateway",)
    return ()


def _schema_for_table(name: str) -> str:
    leaf = name.rsplit(".", 1)[-1]
    if leaf in KNOWLEDGE_OVERRIDES:
        return "knowledge"
    if leaf in ASSISTANT_OVERRIDES:
        return "assistant"
    domain = owner_of(name)
    if domain.startswith("schema:"):
        schema = domain.split(":", 1)[1]
        if schema in PLATFORM_SCHEMAS:
            return schema
    schema = DOMAIN_SCHEMA.get(domain)
    if schema is None:
        raise DatabasePolicyError(f"table {name!r} has unresolved owner domain {domain!r}")
    return schema


def _canonical_tables(raw_tables: list[dict[str, Any]]) -> list[dict[str, Any]]:
    merged: dict[str, dict[str, Any]] = {}
    for row in raw_tables:
        name = str(row["table"])
        leaf = name.rsplit(".", 1)[-1]
        if leaf in LEDGER_TABLES:
            continue
        record = merged.setdefault(
            leaf,
            {
                "table": leaf,
                "defined_in": set(),
                "function_writers": set(),
                "aliases": set(),
            },
        )
        record["defined_in"].update(row.get("defined_in", []))
        record["function_writers"].update(row.get("function_writers", []))
        record["aliases"].add(name)
    result: list[dict[str, Any]] = []
    for leaf, record in sorted(merged.items()):
        result.append(
            {
                "table": leaf,
                "schema": _schema_for_table(leaf),
                "defined_in": sorted(record["defined_in"]),
                "function_writers": sorted(record["function_writers"]),
                "aliases": sorted(record["aliases"]),
            }
        )
    return result


def _resolver(names: set[str]):
    def resolve(value: str) -> str | None:
        leaf = value.lower().rsplit(".", 1)[-1]
        return leaf if leaf in names else None

    return resolve


def _record_literal_access(
    sql: str,
    *,
    source: str,
    roles: tuple[str, ...],
    resolve: Any,
    access: dict[str, dict[str, dict[str, set[str]]]],
) -> None:
    if not roles or _SQLISH.search(sql) is None:
        return
    refs = {resolved for ref in extract_sql_table_refs(sql) if (resolved := resolve(ref))}
    if re.search(r"\bSELECT\b", sql, re.I):
        for table in refs:
            for role in roles:
                access[table][role]["privileges"].add("SELECT")
                access[table][role]["evidence"].add(source)
    for privilege, pattern in _WRITE_TARGETS.items():
        for match in pattern.finditer(sql):
            table = resolve(match.group(1))
            if table is None:
                continue
            for role in roles:
                access[table][role]["privileges"].add(privilege)
                access[table][role]["evidence"].add(source)
                if privilege == "INSERT" and re.search(r"\bON\s+CONFLICT\b", sql, re.I):
                    access[table][role]["privileges"].add("UPDATE")


def _scan_access(
    tables: list[dict[str, Any]],
    functions: set[str],
) -> tuple[dict[str, Any], dict[str, Any]]:
    names = {row["table"] for row in tables}
    resolve = _resolver(names)
    access: dict[str, dict[str, dict[str, set[str]]]] = defaultdict(
        lambda: defaultdict(lambda: {"privileges": set(), "evidence": set()})
    )
    callers: dict[str, dict[str, set[str]]] = defaultdict(lambda: defaultdict(set))
    sources: list[tuple[str, list[str]]] = []
    for relative in walk_files((".py",)):
        source = relative.as_posix()
        roles = _roles_for_path(source)
        if roles:
            sources.append((source, _python_literals(REPO_ROOT / relative)))
    for path in sorted((REPO_ROOT / "rust/agent-runtime-overlay/kernel-rs").rglob("*.rs")):
        source = path.relative_to(REPO_ROOT).as_posix()
        if "/tests/" in source or source.endswith("_tests.rs"):
            continue
        roles = _roles_for_path(source)
        if roles:
            sources.append((source, _rust_literals(path)))
    for source, literals in sources:
        roles = _roles_for_path(source)
        for literal in literals:
            _record_literal_access(
                literal,
                source=source,
                roles=roles,
                resolve=resolve,
                access=access,
            )
            if _SQLISH.search(literal) is None:
                continue
            for function in functions:
                leaf = function.rsplit(".", 1)[-1]
                if re.search(rf"\b{re.escape(leaf)}\s*\(", literal, re.I):
                    for role in roles:
                        callers[function][role].add(source)
    return access, callers


def _sql_sources() -> list[Path]:
    files = [REPO_ROOT / "database/schema.sql"]
    files.extend(sorted((REPO_ROOT / "database/migrations").rglob("*.sql")))
    return [path for path in files if path.is_file() and not path.name.endswith("_rollback.sql")]


def _matching_paren(text: str, start: int) -> int:
    depth = 0
    quote: str | None = None
    for index in range(start, len(text)):
        char = text[index]
        if quote:
            if char == quote:
                quote = None
            continue
        if char in {"'", '"'}:
            quote = char
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return index
    raise DatabasePolicyError("unterminated CREATE FUNCTION argument list")


def _split_arguments(value: str) -> list[str]:
    parts: list[str] = []
    start = 0
    depth = 0
    for index, char in enumerate(value):
        if char in "([":
            depth += 1
        elif char in ")]":
            depth -= 1
        elif char == "," and depth == 0:
            parts.append(value[start:index].strip())
            start = index + 1
    final = value[start:].strip()
    if final:
        parts.append(final)
    return parts


def _canonical_type(value: str) -> str:
    rendered = re.split(r"\s+DEFAULT\s+|\s*=", value, maxsplit=1, flags=re.I)[0].strip()
    tokens = rendered.split()
    if tokens and tokens[0].upper() in {"IN", "INOUT", "VARIADIC"}:
        tokens = tokens[1:]
    if tokens and tokens[0].upper() == "OUT":
        return ""
    if len(tokens) > 1 and re.fullmatch(r"[a-z_][a-z0-9_]*", tokens[0], re.I):
        tokens = tokens[1:]
    rendered = " ".join(tokens).lower()
    match = re.fullmatch(r"([a-z]+)(\([0-9, ]+\))", rendered)
    if match and match.group(1) in _TYPE_CANONICAL:
        # PostgreSQL function identity arguments discard type modifiers;
        # varchar(64) and varchar(255) are the same overload identity.
        return _TYPE_CANONICAL[match.group(1)]
    return _TYPE_CANONICAL.get(rendered, rendered)


def _function_definitions() -> dict[str, dict[str, Any]]:
    definitions: dict[str, dict[str, Any]] = {}
    for path in _sql_sources():
        text = path.read_text(encoding="utf-8")
        for match in _CREATE_FUNCTION.finditer(text):
            name = match.group(1).lower()
            end = _matching_paren(text, match.end() - 1)
            argument_types = [
                canonical
                for argument in _split_arguments(text[match.end() : end])
                if (canonical := _canonical_type(argument))
            ]
            definitions[name.rsplit(".", 1)[-1]] = {
                "identity_arguments": ", ".join(argument_types),
                "defined_in": path.relative_to(REPO_ROOT).as_posix(),
            }
    return definitions


def _schema_for_function(name: str) -> str:
    if name.startswith("knowledge."):
        return "knowledge"
    if name.startswith("assistant."):
        return "assistant"
    leaf = name.rsplit(".", 1)[-1]
    if leaf.startswith(
        (
            "knowledge_",
            "segments_",
            "guard_dataset",
            "prune_kb_",
            "record_kb_",
            "update_document",
            "update_segment",
        )
    ):
        return "knowledge"
    if leaf.startswith(("assistant_", "append_assistant", "reserve_assistant", "dispatch_assistant", "ensure_assistant", "import_assistant", "issue_assistant", "complete_assistant", "update_assistant", "update_memory")):
        return "assistant"
    return "gateway"


def build(
    source_git_sha: str | None = None,
    *,
    pending_live_freeze: bool = False,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    if pending_live_freeze:
        source_sha = None
        state_suffix = "pending-live-freeze"
    else:
        source_sha = source_git_sha or git_head_sha()
        state_suffix = "source-commit-bound-pending-live-acl"
    raw = data_access.build()["postgresql"]
    tables = _canonical_tables(raw["tables"])
    functions = sorted({name.rsplit(".", 1)[-1] for name in raw["functions"]})
    access, callers = _scan_access(tables, set(functions))
    definitions = _function_definitions()

    table_rows: list[dict[str, Any]] = []
    ownership_objects: list[dict[str, Any]] = []
    grant_objects: list[dict[str, Any]] = []
    for table in tables:
        name = table["table"]
        role_access = access.get(name, {})
        readers: list[str] = []
        writers: list[str] = []
        accesses: list[dict[str, Any]] = []
        for role, contract in sorted(role_access.items()):
            privileges = sorted(
                contract["privileges"],
                key=("SELECT", "INSERT", "UPDATE", "DELETE").index,
            )
            evidence = sorted(contract["evidence"])
            units = [f"{role}:{path}" for path in evidence]
            if "SELECT" in privileges:
                readers.extend(units)
            if set(privileges) & {"INSERT", "UPDATE", "DELETE"}:
                writers.extend(units)
            accesses.append(
                {
                    "units": units,
                    "roles": [role],
                    "privileges": privileges,
                    "evidence": evidence,
                }
            )
        table_rows.append(
            {
                **table,
                "static_readers": sorted(set(readers)),
                "static_writers": sorted(set(writers)),
                "reader_files": sorted(
                    {path for contract in role_access.values() for path in contract["evidence"]}
                ),
                "writer_files": sorted(
                    {
                        path
                        for contract in role_access.values()
                        if contract["privileges"] & {"INSERT", "UPDATE", "DELETE"}
                        for path in contract["evidence"]
                    }
                ),
            }
        )
        ownership = {
            "kind": "table",
            "inventory_name": name,
            "schema": table["schema"],
            "name": name,
            "owner": "owner",
            "evidence": table["defined_in"],
        }
        ownership_objects.append(ownership)
        grant_objects.append(
            {
                "inventory_kind": "table",
                "inventory_name": name,
                "schema": table["schema"],
                "name": name,
                "owner": "owner",
                "evidence": table["defined_in"],
                "access": accesses,
                "function_only": bool(table["function_writers"] and not writers),
                "function_evidence": table["function_writers"],
            }
        )

    function_rows: list[str] = []
    for name in functions:
        definition = definitions.get(name)
        if definition is None:
            raise DatabasePolicyError(f"function {name!r} has no parseable definition")
        schema = _schema_for_function(name)
        function_rows.append(name)
        evidence = [definition["defined_in"]]
        ownership_objects.append(
            {
                "kind": "function",
                "inventory_name": name,
                "schema": schema,
                "name": name,
                "identity_arguments": definition["identity_arguments"],
                "owner": "owner",
                "evidence": evidence,
            }
        )
        accesses = [
            {
                "units": [f"{role}:{path}" for path in sorted(paths)],
                "roles": [role],
                "privileges": ["EXECUTE"],
                "evidence": sorted(paths),
            }
            for role, paths in sorted(callers.get(name, {}).items())
        ]
        grant_objects.append(
            {
                "inventory_kind": "function",
                "inventory_name": name,
                "schema": schema,
                "name": name,
                "identity_arguments": definition["identity_arguments"],
                "owner": "owner",
                "evidence": evidence,
                "access": accesses,
            }
        )

    views = sorted({name.rsplit(".", 1)[-1] for name in raw["views"]})
    for name in views:
        evidence = ["database/schema.sql", "database/migrations"]
        ownership_objects.append(
            {
                "kind": "view",
                "inventory_name": name,
                "schema": "gateway",
                "name": name,
                "owner": "owner",
                "evidence": evidence,
            }
        )
        grant_objects.append(
            {
                "inventory_kind": "view",
                "inventory_name": name,
                "schema": "gateway",
                "name": name,
                "owner": "owner",
                "evidence": evidence,
                "access": [
                    {
                        "units": [f"gateway:view:{name}"],
                        "roles": ["gateway"],
                        "privileges": ["SELECT"],
                        "evidence": [f"reviewed-view:{name}"],
                    }
                ],
            }
        )

    sequences: list[dict[str, Any]] = []
    table_by_name = {row["table"]: row for row in table_rows}
    for sequence in raw["sequences_implicit_serial"]:
        parent = str(sequence["columns"][0]).split(".", 1)[0].rsplit(".", 1)[-1]
        table = table_by_name.get(parent)
        if table is None:
            raise DatabasePolicyError(f"sequence {sequence['name']} has unknown parent {parent}")
        roles = sorted(
            role
            for role, contract in access.get(parent, {}).items()
            if "INSERT" in contract["privileges"]
        )
        sequences.append({**sequence, "schema": table["schema"], "parent_table": parent})
        evidence = list(sequence["defined_in"])
        ownership_objects.append(
            {
                "kind": "sequence",
                "inventory_name": sequence["name"],
                "schema": table["schema"],
                "name": sequence["name"],
                "owner": "owner",
                "evidence": evidence,
            }
        )
        grant_objects.append(
            {
                "inventory_kind": "sequence",
                "inventory_name": sequence["name"],
                "schema": table["schema"],
                "name": sequence["name"],
                "owner": "owner",
                "evidence": evidence,
                "access": [
                    {
                        "units": [f"{role}:insert:{parent}"],
                        "roles": [role],
                        "privileges": ["USAGE"],
                        "evidence": [f"serial-parent:{table['schema']}.{parent}"],
                    }
                    for role in roles
                ],
            }
        )

    inventory = {
        "schema": SCHEMA_VERSION,
        "state": state_suffix,
        "source_git_sha": source_sha,
        "postgresql": {
            "tables": table_rows,
            "views": views,
            "functions": function_rows,
            "types": [],
            "sequences_explicit": [],
            "sequences_implicit_serial": sequences,
        },
    }
    inventory_bytes = _serialized(inventory)
    inventory_sha = hashlib.sha256(inventory_bytes).hexdigest()
    ownership = {
        "schema": OWNERSHIP_SCHEMA,
        "state": state_suffix,
        "source_git_sha": source_sha,
        "inventory_sha256": inventory_sha,
        "objects": sorted(
            ownership_objects,
            key=lambda row: (
                row["kind"],
                row["schema"],
                row["name"],
                row.get("identity_arguments", ""),
            ),
        ),
    }
    grants = {
        "schema_version": GRANTS_SCHEMA,
        "state": state_suffix,
        "source_git_sha": source_sha,
        "inventory_sha256": inventory_sha,
        "objects": sorted(
            grant_objects,
            key=lambda row: (row["inventory_kind"], row["inventory_name"]),
        ),
    }
    return inventory, ownership, grants


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    source = parser.add_mutually_exclusive_group(required=True)
    source.add_argument("--source-rev")
    source.add_argument("--pending-live-freeze", action="store_true")
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--write", action="store_true")
    args = parser.parse_args()
    if args.source_rev is not None and not re.fullmatch(r"[0-9a-f]{40}", args.source_rev):
        raise SystemExit("--source-rev must be a full lowercase Git SHA")
    inventory, ownership, grants = build(
        args.source_rev,
        pending_live_freeze=args.pending_live_freeze,
    )
    outputs = {
        "data-access-inventory.json": _serialized(inventory),
        "ownership-policy.json": _serialized(ownership),
        "grants-policy.json": _serialized(grants),
    }
    if not args.write:
        for name, content in outputs.items():
            target = args.output_dir / name
            if not target.is_file() or target.read_bytes() != content:
                raise SystemExit(f"DRIFT {target}")
        return 0
    args.output_dir.mkdir(parents=True, exist_ok=True)
    for name, content in outputs.items():
        (args.output_dir / name).write_bytes(content)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
