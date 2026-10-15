#!/usr/bin/env python3
"""ARC-04 persistence god-class SQL inventory.

PRD §ARC-04 goal 7: ``DatabaseStorage``, ``AgentRepository`` and Knowledge
persistence must be separated by *table writer owner* — cross-domain SQL
first, then ARC-03 tightens database roles.  This script produces the
mechanical evidence:

- every table referenced by SQL string constants inside
  ``ai_gateway_core`` (read vs write), extracted via AST so dynamic string
  building is out of scope (see caveats);
- a table → writer-owner domain map used to flag cross-domain access;
- the set of tables defined by ``database/schema.sql`` + migrations, so
  CTE names and regex noise are reported as ``unresolved`` instead of being
  mistaken for real tables.

Regenerate::

    uv run python scripts/core_boundary/inventory_persistence_sql.py

Output: ``reports/inventory/core-persistence-sql-inventory.json``.

Caveats (also printed in the JSON):

- the scanner reads SQL in string constants only; table names assembled at
  runtime are invisible to it;
- ``read`` entries include CTE names the regex cannot distinguish from
  tables — entries not present in the known-table set are marked
  ``unresolved`` and excluded from cross-domain counts;
- the writer-owner map below is a documented claim to review with ARC-03,
  not yet enforced database state.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from datetime import UTC, datetime
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from inventory_core_consumption import iter_python_files, sql_tables  # noqa: E402

SCHEMA_VERSION = "arc04-persistence-sql/v1"
CORE_SRC = Path("packages/ai-gateway-core/src/ai_gateway_core")

_CREATE_TABLE = re.compile(r"CREATE TABLE (?:IF NOT EXISTS )?([a-z_][a-z0-9_.]*)", re.I)

# Writer-owner domain per table (unqualified name; ``assistant.*`` tables are
# owned by the assistant-runtime schema owner).  Review with ARC-03 before
# any grant generation — PRD §ARC-04 goal 7, PRD §ARC-03.
TABLE_OWNER: dict[str, str] = {}
_OWNER_GROUPS: dict[str, tuple[str, ...]] = {
    "auth": (
        "users", "user_roles", "user_permissions", "role_permissions", "rbac_roles",
        "permissions", "api_keys", "auth_config", "login_audit", "password_history",
        "email_domain_config", "audit_logs",
    ),
    "platform": (
        "tenants", "services", "service_health_records", "proxy_routes",
        "rate_limit_config", "tasks", "langgraph_threads", "request_traces",
        "security_event_daily_aggregates", "semantic_cache", "user_quotas",
        "quota_alerts", "billing_events", "model_pricing", "llm_models",
        "llm_providers", "usage_records", "usage_statistics",
        "usage_daily_aggregates", "usage_hourly_aggregates",
        "version_retention_policies",
    ),
    # Conversation artifacts (images/documents/charts/code produced inside
    # assistant sessions).  Written exclusively by the artifact storage
    # module; the sharing domain reads + shares them via artifact_shares.
    "artifact": ("artifacts", "assistant.artifacts"),
    "local-node": tuple(
        f"local_node_{suffix}"
        for suffix in (
            "channels", "devices", "events", "executions", "grants",
            "pairing_challenges", "receipts",
        )
    ),
    "agent-studio": (
        "agents", "agent_drafts", "agent_versions", "agent_publications",
        "agent_members", "agent_api_tokens", "agent_governance_policies",
        "agent_draft_knowledge_bindings", "agent_draft_skill_bindings",
        "agent_version_capabilities", "agent_version_knowledge_bindings",
        "agent_version_skill_bindings", "agent_version_revocations",
        "agent_publish_events", "agent_release_requests",
        "agent_release_evaluations", "agent_release_evaluation_events",
        "agent_data_deletion_requests",
    ),
    "agent-runtime": (
        "sessions", "assistant.sessions", "assistant_runs",
        "assistant_run_checkpoints", "assistant_audit_events",
        "assistant_capability_events", "assistant_capability_executions",
        "assistant_command_queue", "assistant_context_breakdown",
        "assistant_runtime_items", "assistant_runtime_model_calls",
        "assistant_runtime_model_leases", "assistant_runtime_snapshots",
        "assistant_runtime_snapshot_revocations",
        "assistant_runtime_threads", "assistant_runtime_thread_members",
        "assistant_runtime_thread_projections", "assistant_scheduler_jobs",
        "assistant_session_runtime_assignments", "assistant_tool_approvals",
        "agent_runtime_attachments", "agent_runtime_feedback",
        "agent_runtime_idempotency",
    ),
    "memory": (
        "session_memory", "user_memory", "assistant_memory_chunks",
        "assistant_memory_reflections", "assistant_memory_sources",
    ),
    "skills": (
        "assistant_skills", "assistant_skill_versions",
        "assistant_skill_version_revocations", "assistant_skill_runs",
        "assistant.assistant_skills", "assistant.assistant_skill_versions",
    ),
    "image": (
        "assistant.image_tasks", "assistant.image_turns",
        "assistant.image_sessions", "assistant.image_blobs",
        "assistant.image_idempotency", "segment_images",
    ),
    "quiz": (
        "quizzes", "quiz_questions", "quiz_attempts", "quiz_shares",
        "assistant.quizzes", "assistant.quiz_attempts",
        "conversation_shares", "conversation_share_quiz_attempts",
        "exams", "exam_analysis_reports",
    ),
    "sharing": (
        "artifact_shares", "artifact_share_submitters",
        "assistant.artifact_shares", "assistant.artifact_share_submitters",
        "assistant.artifact_share_attempt_tokens",
    ),
    "eval-trace": (
        "agent_traces", "agent_trace_spans", "agent_trace_events",
        "agent_trace_scores", "agent_trace_outbox", "eval_datasets",
        "eval_examples", "eval_evaluators", "eval_experiments",
        "eval_experiment_runs", "eval_experiment_run_cases",
        "eval_baseline_promotions",
    ),
    "mcp-connector": (
        "mcp_servers", "mcp_tools", "mcp_tool_snapshots", "mcp_connections",
        "mcp_schema_diffs", "mcp_channel_grants", "connector_configs",
        "connector_credential_principals", "user_connectors",
        "tenant_mcp_configs", "tenant_tool_policies", "tool_audit_log",
    ),
    "knowledge": (
        "datasets", "dataset_permissions", "dataset_process_rules",
        "dataset_queries", "dataset_keyword_tables", "dataset_collection_bindings",
        "documents", "document_versions", "document_summaries",
        "document_pipeline_executions", "segments", "child_chunks",
        "confluence_connections", "confluence_pages", "confluence_sync_tasks",
        "confluence_space_bindings", "confluence_webhooks",
        "confluence_image_sync", "kb_document_batch_items",
        "kb_document_batch_operations", "kb_document_progress_events",
        "kb_bm25_v2_lifecycle", "kb_eval_golden", "kb_eval_golden_release",
        "knowledge.dataset_query_feedback", "knowledge.kb_parsing_ir",
        "knowledge.kb_parsing_page_cache",
        "knowledge.kb_segment_attachment_bindings",
        "embedding_migrations", "embedding_migration_progress",
        "embedding_migration_action_jobs", "embedding_vector_cache",
    ),
}
for _owner, _tables in _OWNER_GROUPS.items():
    for _table in _tables:
        TABLE_OWNER[_table] = _owner

# Core module → domain it belongs to (its "home" for cross-domain detection).
MODULE_DOMAIN_PREFIXES: tuple[tuple[str, str], ...] = (
    ("ai_gateway_core.persistence.repositories.agent_trace_repository", "eval-trace"),
    ("ai_gateway_core.persistence.repositories.agent_repository", "agent-studio"),
    ("ai_gateway_core.persistence.repositories.mcp_repository", "mcp-connector"),
    ("ai_gateway_core.persistence.repositories.api_key_repository", "auth"),
    ("ai_gateway_core.persistence.repositories.user_repository", "auth"),
    ("ai_gateway_core.persistence.repositories.session_repository", "agent-runtime"),
    ("ai_gateway_core.persistence.repositories.service_repository", "platform"),
    ("ai_gateway_core.persistence.repositories.task_repository", "platform"),
    ("ai_gateway_core.persistence", "platform"),
    ("ai_gateway_core.session", "agent-runtime"),
    ("ai_gateway_core.agents", "agent-studio"),
    ("ai_gateway_core.quiz", "quiz"),
    ("ai_gateway_core.skills", "skills"),
    ("ai_gateway_core.image", "image"),
    ("ai_gateway_core.memory", "memory"),
    ("ai_gateway_core.sharing", "sharing"),
    ("ai_gateway_core.knowledge", "knowledge"),
    ("ai_gateway_core.eval", "eval-trace"),
    ("ai_gateway_core.storage", "artifact"),
)


def module_domain(dotted: str) -> str:
    for prefix, domain in MODULE_DOMAIN_PREFIXES:
        if dotted == prefix or dotted.startswith(prefix + "."):
            return domain
    return "other"


_MOVE_SCHEMA = re.compile(
    r"ALTER TABLE (?:IF EXISTS )?([a-z_][a-z0-9_.]*)\s+SET SCHEMA\s+([a-z_][a-z0-9_]*)", re.I
)
# per_service/_global/002_move_tables.sql executes its moves through
# ``EXECUTE format('ALTER TABLE %I.%I SET SCHEMA %I', …)`` driven by a JSONB
# array; the static entries look like jsonb_build_object('table', 'x', 'to', 'y').
_MOVE_JSONB = re.compile(
    r"jsonb_build_object\(\s*'table'\s*,\s*'([a-z_][a-z0-9_]*)'\s*,\s*'to'\s*,\s*"
    r"'([a-z_][a-z0-9_]*)'\s*\)",
    re.I,
)


def known_tables(root: Path) -> set[str]:
    """Tables defined by schema.sql + every migration (incl. per_service).

    Follows ``ALTER TABLE … SET SCHEMA`` moves — both literal statements and
    the JSONB-driven loop in ``per_service/_global/002_move_tables.sql`` —
    so schema-qualified names such as ``assistant.sessions`` resolve as known.
    """
    tables: set[str] = set()
    moves: list[tuple[str, str]] = []
    sources = [root / "database" / "schema.sql"]
    migrations = root / "database" / "migrations"
    if migrations.is_dir():
        sources.extend(sorted(migrations.rglob("*.sql")))
    for source in sources:
        if not source.is_file():
            continue
        text = source.read_text(encoding="utf-8", errors="replace")
        for match in _CREATE_TABLE.finditer(text):
            tables.add(match.group(1).lower())
        for match in _MOVE_SCHEMA.finditer(text):
            moves.append((match.group(1).lower(), match.group(2).lower()))
        for match in _MOVE_JSONB.finditer(text):
            moves.append((match.group(1).lower(), match.group(2).lower()))
    for table, schema in moves:
        tables.add(f"{schema}.{table}")
    return tables


def owner_of(table: str) -> str:
    if table in TABLE_OWNER:
        return TABLE_OWNER[table]
    if "." in table:
        schema, _, name = table.partition(".")
        if name in TABLE_OWNER:
            return TABLE_OWNER[name]
        return f"schema:{schema}"
    return "unknown"


def build_inventory(root: Path) -> dict:
    known = known_tables(root)
    modules: dict[str, dict] = {}
    cross_domain: dict[str, dict[str, list[str]]] = {}

    src_root = root / CORE_SRC.parent
    for path in iter_python_files(root, CORE_SRC.as_posix()):
        rel = path.relative_to(src_root)
        parts = list(rel.with_suffix("").parts)
        if parts[-1] == "__init__":
            parts = parts[:-1]
        if not parts:
            continue
        dotted = ".".join(parts)
        tables = sql_tables(path)
        if not tables["write"] and not tables["read"]:
            continue

        def annotate(names: list[str]) -> list[dict]:
            out = []
            for name in names:
                resolved = name in known
                out.append(
                    {
                        "table": name,
                        "owner": owner_of(name) if resolved else "unresolved",
                        "known_table": resolved,
                    }
                )
            return out

        home = module_domain(dotted)
        writes = annotate(tables["write"])
        reads = annotate(tables["read"])
        foreign_writes = sorted(
            {
                entry["owner"]
                for entry in writes
                if entry["known_table"] and entry["owner"] not in (home, "unknown")
            }
        )
        foreign_read_only = sorted(
            {
                entry["owner"]
                for entry in reads
                if entry["known_table"]
                and entry["owner"] not in (home, "unknown")
                and entry["owner"] not in foreign_writes
            }
        )
        modules[dotted] = {
            "file": path.relative_to(root).as_posix(),
            "home_domain": home,
            "write": writes,
            "read": reads,
            "cross_domain_write_owners": foreign_writes,
            "cross_domain_read_only_owners": foreign_read_only,
        }
        if foreign_writes:
            cross_domain[dotted] = {
                "home_domain": home,
                "writes_into": foreign_writes,
                "tables": sorted(
                    entry["table"]
                    for entry in writes
                    if entry["known_table"] and entry["owner"] in foreign_writes
                ),
            }

    return {
        "schema_version": SCHEMA_VERSION,
        "generated_at": datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "core_package_dir": CORE_SRC.as_posix(),
        "known_table_count": len(known),
        "caveats": [
            "SQL in string constants only; runtime-assembled SQL is invisible",
            "read entries may include CTE names; unresolved entries are noise",
            "owner map is a documented claim to review with ARC-03",
        ],
        "modules": modules,
        "cross_domain_writes": cross_domain,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        default="reports/inventory/core-persistence-sql-inventory.json",
        help="inventory JSON destination (repo-relative)",
    )
    args = parser.parse_args(argv)
    root = Path(__file__).resolve().parents[2]
    inventory = build_inventory(root)
    out_path = root / args.output
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(inventory, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(
        f"wrote {out_path.relative_to(root)}: "
        f"{len(inventory['modules'])} modules with SQL, "
        f"{len(inventory['cross_domain_writes'])} with cross-domain writes, "
        f"{inventory['known_table_count']} known tables"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
