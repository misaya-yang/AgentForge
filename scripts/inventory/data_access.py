"""Data-access inventory baseline.

Produces ``data-access-inventory.json``:

* PostgreSQL objects parsed from ``database/schema.sql`` and every file under
  ``database/migrations/`` (tables, views, sequences, functions, types,
  indexes, schemas, grants) with their source evidence.
* Static writer/reader attribution per table, from an AST scan of SQL string
  literals in every first-party Python unit. This is a heuristic: it records
  which unit *contains SQL touching the table*, with the evidence files, not
  a runtime proof. It is the input ARC-03 uses to generate least-privilege
  grants, so false negatives are the failure mode to watch.
* Table access inside SQL function bodies (append-only stores are written
  through functions, invisible to the Python scan).
* Qdrant, Redis, and object-store namespace ownership with evidence.

The inventory never invents owners. Where ownership is a target rather than a
fact (e.g. per-service PostgreSQL roles), it is marked ``target`` and points
at the work package that creates it.
"""

from __future__ import annotations

import ast
import re
from pathlib import Path

from _common import (
    REPO_ROOT,
    base_envelope,
    extract_sql_table_refs,
    unit_for_path,
    walk_files,
)

_SCHEMA_OBJECT = re.compile(
    r"^\s*CREATE\s+(?:OR\s+REPLACE\s+)?"
    r"(?P<kind>TABLE|VIEW|MATERIALIZED\s+VIEW|SEQUENCE|FUNCTION|TYPE|SCHEMA|INDEX|UNIQUE\s+INDEX)\s+"
    r"(?:IF\s+NOT\s+EXISTS\s+)?(?P<name>[A-Za-z_][\w.]*)",
    re.IGNORECASE | re.MULTILINE,
)

_GRANT = re.compile(
    r"^\s*GRANT\s+(?P<privs>[\w, ]+?)\s+ON\s+(?P<what>[\w ]+?)\s+"
    r"(?P<obj>[\w\"., ]+?)\s+TO\s+(?P<role>[\w\"]+)",
    re.IGNORECASE | re.MULTILINE,
)

_CREATE_TABLE_LINE = re.compile(
    r"^\s*CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?(?P<name>[A-Za-z_][\w.]*)",
    re.IGNORECASE,
)

_SERIAL_COLUMN = re.compile(
    r"^\s*\"?(?P<col>[A-Za-z_]\w*)\"?\s+(?:BIG|SMALL)?SERIAL\b",
    re.IGNORECASE,
)

_ALTER_ADD_SERIAL = re.compile(
    r"^\s*ALTER\s+TABLE\s+(?:ONLY\s+)?(?P<table>[A-Za-z_][\w.]*)\s+"
    r"ADD\s+COLUMN\s+(?:IF\s+NOT\s+EXISTS\s+)?\"?(?P<col>[A-Za-z_]\w*)\"?\s+(?:BIG|SMALL)?SERIAL\b",
    re.IGNORECASE,
)

_FUNCTION_BODY = re.compile(
    r"CREATE\s+(?:OR\s+REPLACE\s+)?FUNCTION\s+(?P<name>[A-Za-z_][\w.]*)"
    r".*?AS\s*\$(?P<tag>\w*)\$(?P<body>.*?)\$(?P=tag)\$",
    re.IGNORECASE | re.DOTALL,
)

_SQL_VERB = {
    "writer": re.compile(r"\b(INSERT\s+INTO|UPDATE|DELETE\s+FROM|TRUNCATE(?:\s+TABLE)?|MERGE\s+INTO)\b", re.IGNORECASE),
    "reader": re.compile(r"\b(SELECT|FROM|JOIN)\b", re.IGNORECASE),
}

_DDL_NOISE = {"schema_migrations", "schema_migrations_meta"}


def _sql_files() -> list[Path]:
    root = REPO_ROOT / "database"
    found = [root / "schema.sql"]
    for path in sorted((root / "migrations").rglob("*.sql")):
        if path.is_file():
            found.append(path)
    return sorted(set(found))


def parse_schema_objects() -> tuple[dict[str, dict], list[dict]]:
    """Parse CREATE statements and GRANTs from schema.sql + migrations."""
    objects: dict[str, dict] = {}
    grants: list[dict] = []
    for path in _sql_files():
        rel = str(path.relative_to(REPO_ROOT))
        text = path.read_text(encoding="utf-8")
        for match in _SCHEMA_OBJECT.finditer(text):
            kind = re.sub(r"\s+", "_", match.group("kind").upper())
            name = match.group("name").strip('"').lower()
            record = objects.setdefault(
                name,
                {
                    "name": name,
                    "kinds": set(),
                    "first_defined_in": rel,
                    "defined_in": set(),
                    "schema_qualified": "." in name,
                },
            )
            record["kinds"].add(kind)
            record["defined_in"].add(rel)
        for match in _GRANT.finditer(text):
            grants.append(
                {
                    "privileges": re.sub(r"\s+", " ", match.group("privs").strip()),
                    "on": match.group("what").strip().lower(),
                    "object": match.group("obj").strip('"').lower(),
                    "role": match.group("role").strip('"').lower(),
                    "source": rel,
                }
            )
    for record in objects.values():
        record["kinds"] = sorted(record["kinds"])
        record["defined_in"] = sorted(record["defined_in"])
    grants.sort(key=lambda g: (g["object"], g["role"], g["privileges"], g["source"]))
    return objects, grants


def parse_implicit_sequences() -> list[dict]:
    """Derive the implicit ``<table>_<column>_seq`` sequences of SERIAL columns.

    PostgreSQL creates these automatically for SERIAL/BIGSERIAL/SMALLSERIAL
    columns; they never appear as explicit ``CREATE SEQUENCE`` statements but
    they are real grantable objects, so ARC-03 needs them named.
    """
    sequences: dict[str, dict] = {}

    def record(table: str, column: str, rel: str) -> None:
        table = table.strip('"').lower()
        name = f"{table}_{column.lower()}_seq"
        entry = sequences.setdefault(
            name, {"name": name, "defined_in": set(), "columns": set()}
        )
        entry["defined_in"].add(rel)
        entry["columns"].add(f"{table}.{column.lower()}")

    for path in _sql_files():
        rel = str(path.relative_to(REPO_ROOT))
        table: str | None = None
        for line in path.read_text(encoding="utf-8").splitlines():
            created = _CREATE_TABLE_LINE.match(line)
            if created:
                table = created.group("name")
                continue
            if table is not None:
                if line.strip().startswith(")"):
                    table = None
                    continue
                column = _SERIAL_COLUMN.match(line)
                if column:
                    record(table, column.group("col"), rel)
                    continue
            altered = _ALTER_ADD_SERIAL.match(line)
            if altered:
                record(altered.group("table"), altered.group("col"), rel)
    result = []
    for name in sorted(sequences):
        entry = sequences[name]
        result.append(
            {
                "name": name,
                "kind": "implicit (SERIAL column)",
                "columns": sorted(entry["columns"]),
                "defined_in": sorted(entry["defined_in"]),
            }
        )
    return result


def parse_function_table_refs(known_tables: set[str]) -> dict[str, dict[str, list[str]]]:
    """Attribute table access inside SQL function bodies to the function.

    Append-only runtime stores (items, events, snapshots, …) are written
    through plpgsql functions; the Python string-literal scan cannot see those
    writes. This closes the gap statement-by-statement so a SELECT from one
    table and an INSERT into another inside the same body do not merge.
    """
    resolve = _make_resolver(known_tables)
    writes: dict[str, set[str]] = {}
    reads: dict[str, set[str]] = {}
    for path in _sql_files():
        text = path.read_text(encoding="utf-8")
        for match in _FUNCTION_BODY.finditer(text):
            function = match.group("name").strip('"').lower()
            for statement in match.group("body").split(";"):
                refs = extract_sql_table_refs(statement)
                if not refs:
                    continue
                is_writer = bool(_SQL_VERB["writer"].search(statement))
                is_reader = bool(_SQL_VERB["reader"].search(statement))
                for ref in refs:
                    table = resolve(ref)
                    if table is None:
                        continue
                    if is_writer:
                        writes.setdefault(table, set()).add(function)
                    if is_reader or is_writer:
                        reads.setdefault(table, set()).add(function)
    return {
        "function_written_tables": {t: sorted(f) for t, f in sorted(writes.items())},
        "function_read_tables": {t: sorted(f) for t, f in sorted(reads.items())},
    }


def _python_string_literals(path: Path):
    """Yield string constant fragments from a Python file (incl. f-string parts)."""
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"))
    except (SyntaxError, UnicodeDecodeError):
        return
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            yield node.value


def _looks_like_sql(text: str) -> bool:
    return bool(re.search(r"\b(SELECT|INSERT\s+INTO|UPDATE\s+\w+\s+SET|DELETE\s+FROM|CREATE\s+TABLE)\b", text, re.IGNORECASE))


def _make_resolver(known_tables: set[str]):
    """Resolve a SQL table reference to its DDL name.

    Schema-qualified DDL names (assistant.foo) must still be found when the
    SQL text uses the bare leaf (foo). Ambiguous leaves fail closed (None).
    """
    leaf_index: dict[str, list[str]] = {}
    for table in known_tables:
        leaf_index.setdefault(table.rsplit(".", 1)[-1], []).append(table)

    def resolve(ref: str) -> str | None:
        leaf = ref.rsplit(".", 1)[-1]
        if ref in known_tables:
            return ref
        if leaf in known_tables:
            return leaf
        candidates = leaf_index.get(leaf)
        if candidates and len(candidates) == 1:
            return candidates[0]
        return None

    return resolve


def scan_table_access(known_tables: set[str]) -> dict[str, dict]:
    """Attribute table references in Python SQL strings to source units."""
    resolve = _make_resolver(known_tables)
    access: dict[str, dict] = {
        table: {"readers": set(), "writers": set(), "reader_files": set(), "writer_files": set()}
        for table in known_tables
    }
    for rel in walk_files((".py",)):
        if str(rel).startswith("tests/"):
            continue  # tests pin behaviour; they are not data-access owners
        unit = unit_for_path(rel)
        literals = [text for text in _python_string_literals(REPO_ROOT / rel) if _looks_like_sql(text)]
        if not literals:
            continue
        for text in literals:
            refs = extract_sql_table_refs(text)
            is_writer = bool(_SQL_VERB["writer"].search(text))
            is_reader = bool(_SQL_VERB["reader"].search(text))
            for ref in refs:
                table = resolve(ref)
                if table is None:
                    continue
                if is_writer:
                    access[table]["writers"].add(unit)
                    access[table]["writer_files"].add(str(rel))
                if is_reader or is_writer:
                    access[table]["readers"].add(unit)
                    access[table]["reader_files"].add(str(rel))
    return access


def build() -> dict:
    objects, grants = parse_schema_objects()
    known_tables = {
        name for name, record in objects.items() if "TABLE" in record["kinds"]
    }
    access = scan_table_access(known_tables)
    function_access = parse_function_table_refs(known_tables)
    function_writes = function_access["function_written_tables"]

    tables = []
    for name in sorted(known_tables):
        record = objects[name]
        tables.append(
            {
                "table": name,
                "schema_qualified_in_ddl": record["schema_qualified"],
                "defined_in": record["defined_in"],
                "static_readers": sorted(access[name]["readers"]),
                "static_writers": sorted(access[name]["writers"]),
                "reader_files": sorted(access[name]["reader_files"]),
                "writer_files": sorted(access[name]["writer_files"]),
                "function_writers": function_writes.get(name, []),
                "cross_unit_write": sorted(
                    access[name]["writers"] - {"database-tools"}
                ),
            }
        )

    functions = sorted(
        name for name, record in objects.items() if "FUNCTION" in record["kinds"]
    )
    views = sorted(
        name for name, record in objects.items()
        if "VIEW" in record["kinds"] or "MATERIALIZED_VIEW" in record["kinds"]
    )
    explicit_sequences = sorted(
        name for name, record in objects.items() if "SEQUENCE" in record["kinds"]
    )
    implicit_sequences = parse_implicit_sequences()
    types = sorted(name for name, record in objects.items() if "TYPE" in record["kinds"])
    schemas = sorted(name for name, record in objects.items() if "SCHEMA" in record["kinds"])

    cross_unit_writes = sorted(
        {
            f"{row['table']}: {'+'.join(w for w in row['static_writers'] if w != 'database-tools')}"
            for row in tables
            if len(set(row["static_writers"]) - {"database-tools"}) > 1
        }
    )

    return {
        **base_envelope("data-access-inventory"),
        "purpose": (
            "Input for ARC-03 least-privilege grants and ARC-04 persistence owner separation. "
            "Static writer/reader attribution is a heuristic over SQL string literals; "
            "verify against runtime queries before narrowing any grant."
        ),
        "postgresql": {
            "ddl_sources": [str(p.relative_to(REPO_ROOT)) for p in _sql_files()],
            "object_counts": {
                "tables": len(tables),
                "functions": len(functions),
                "views": len(views),
                "sequences_explicit": len(explicit_sequences),
                "sequences_implicit_serial": len(implicit_sequences),
                "types": len(types),
                "schemas_declared": len(schemas),
                "grants_found": len(grants),
            },
            "schemas_declared": schemas,
            "functions": functions,
            "views": views,
            "sequences_explicit": explicit_sequences,
            "sequences_implicit_serial": implicit_sequences,
            "types": types,
            "grants": grants,
            "tables": tables,
            "tables_with_multiple_python_unit_writers": cross_unit_writes,
            "function_mediated_access": {
                "note": (
                    "Table access performed inside SQL function bodies, statement by "
                    "statement. Append-only stores such as assistant_runtime_items are "
                    "written only through these functions; their rows therefore show no "
                    "Python writer. Grants for these tables must follow the function "
                    "ownership (ARC-03 SECURITY DEFINER rules)."
                ),
                "function_written_tables": function_access["function_written_tables"],
                "function_read_tables": function_access["function_read_tables"],
            },
            "role_model": {
                "current": (
                    "All application services share one PostgreSQL user with a wide search_path; "
                    "schemas are named apart but data privileges are not separated (PRD §1)."
                ),
                "target": "ARC-03 per-service roles (ai_gateway_gateway/runtime/capability_worker/knowledge_api/knowledge_worker) generated from this inventory.",
            },
        },
        "qdrant": {
            "namespaces": [
                {
                    "namespace": "kb_{base}_{dimension} (+_summary / _sections suffixes)",
                    "owner": "knowledge-service",
                    "kind": "dataset collections",
                    "evidence": [
                        "apps/knowledge-service/src/knowledge_service/services/knowledge/vector_store.py (make_collection_name)",
                        "apps/knowledge-service/src/knowledge_service/services/knowledge/hierarchical_indexer.py (SUMMARY_COLLECTION_SUFFIX, SECTION_COLLECTION_SUFFIX)",
                    ],
                },
                {
                    "namespace": "kb_bm25_v2_canary_*",
                    "owner": "knowledge-service",
                    "kind": "BM25 v2 canary collections",
                    "evidence": [
                        "apps/knowledge-service/src/knowledge_service/services/knowledge/vector_store.py (bm25 v2 canary)"
                    ],
                },
                {
                    "namespace": "collection metadata keys knowledge_lexical / knowledge_scope",
                    "owner": "knowledge-service",
                    "kind": "collection metadata contract",
                    "evidence": [
                        "apps/knowledge-service/src/knowledge_service/services/knowledge/lexical_config.py (COLLECTION_METADATA_KEY, COLLECTION_SCOPE_METADATA_KEY)"
                    ],
                },
                {
                    "namespace": "agent memory vector collections (dynamic names from cleanup metadata vector_collections)",
                    "owner": "gateway data governance (Agent memory namespace)",
                    "kind": "memory vectors",
                    "evidence": [
                        "src/services/agent_runtime_cleanup.py (memory_vector_* cleanup over /collections/... endpoints)",
                        "packages/ai-gateway-core/src/ai_gateway_core/agents/runtime.py (agent_memory_principal am_* identity)",
                        "packages/ai-gateway-core/src/ai_gateway_core/persistence/repositories/agent_repository.py (agent memory principals)",
                    ],
                },
            ],
            "target": (
                "ARC-00/ARC-03: every collection/alias/prefix gets exactly one owner plus a "
                "negative-deletion test; cross-namespace reads/writes need a named API/function."
            ),
        },
        "redis": {
            "note": "Cache/counter only; no durable system of record lives in Redis.",
            "keyspaces": [
                {"prefix": "session:{session_id}", "owner": "ai-gateway-core (session cache)", "evidence": "packages/ai-gateway-core/src/ai_gateway_core/persistence/redis.py"},
                {"prefix": "cache:{service_id}:{input_hash}", "owner": "ai-gateway-core (response cache)", "evidence": "packages/ai-gateway-core/src/ai_gateway_core/persistence/redis.py"},
                {"prefix": "health:{service_id}", "owner": "ai-gateway-core (health cache)", "evidence": "packages/ai-gateway-core/src/ai_gateway_core/persistence/redis.py"},
                {"prefix": "service:{service_id}", "owner": "gateway service registry", "evidence": "src/services/registry/database_storage.py"},
                {"prefix": "task:{task_id}", "owner": "gateway task storage", "evidence": "src/services/task/database_task_storage.py"},
                {"prefix": "guest_session:{session_id}", "owner": "gateway guest sessions", "evidence": "src/services/session/guest_session_manager.py"},
                {"prefix": "ratelimit:global|ip|user|tenant|assistant|op", "owner": "gateway rate limiter", "evidence": "src/core/gateway/multi_dimension_rate_limiter.py"},
            ],
        },
        "object_storage": {
            "note": "Local path or S3/OSS bucket selected by config; prefixes below are the logical namespaces.",
            "namespaces": [
                {"prefix": "uploads (FileStorage.KEY_PREFIX)", "owner": "gateway user uploads", "evidence": "packages/ai-gateway-core/src/ai_gateway_core/storage/file_storage.py"},
                {"prefix": "configurable key_prefix (StorageConfig)", "owner": "gateway artifacts", "evidence": "packages/ai-gateway-core/src/ai_gateway_core/storage/artifact_storage.py"},
                {"prefix": "GATEWAY_STORAGE__KEY_PREFIX env prefix", "owner": "gateway image storage", "evidence": "packages/ai-gateway-core/src/ai_gateway_core/storage/image_storage.py"},
                {"prefix": "StorageSettings.key_prefix (default dev), local ./data/files or S3 bucket", "owner": "knowledge-service uploads + extracted images", "evidence": "apps/knowledge-service/src/knowledge_service/config/__init__.py (StorageSettings)"},
            ],
        },
    }


if __name__ == "__main__":
    from _common import write_json

    path = write_json("data-access-inventory.json", build())
    print(f"wrote {path}")
