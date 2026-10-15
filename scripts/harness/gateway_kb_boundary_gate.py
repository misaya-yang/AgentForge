#!/usr/bin/env python3
"""Contract-drift gate: the gateway must not own the KB domain contract (PRD T8.4).

Two rules, both born from the drift this cleanup removed (PRD T8.2):

1. **No KB-table SQL in the gateway data plane.** Dataset/Document/Segment/
   permission/confluence/version tables are knowledge-service's storage. The
   gateway reaches them only through the KS internal authorize endpoint
   (`src/services/knowledge_authz.py`) and the proxy. There are no function
   allowlists: migration probes and authoring checks are boundary violations
   too.

2. **No KB request schemas defined in the gateway.** KB bodies are validated
   by knowledge-service; the gateway proxies JSON. A gateway model is a
   violation when it mirrors a KS API contract class — i.e. it shares a name
   with any pydantic class defined under knowledge-service's `api/` package.
   Mirrors rot into dead copies — exactly what `src/api/schemas/knowledge.py`
   and `kb_tools.py` were before deletion.

Exit code 0 = PASS, 1 = violations listed on stdout.
"""

from __future__ import annotations

import ast
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCAN_ROOTS = (ROOT / "src", ROOT / "packages" / "ai-gateway-core" / "src")

# --- Rule 1: KB-table SQL -----------------------------------------------------

KB_TABLES = frozenset(
    {
        "child_chunks",
        "confluence_connections",
        "confluence_image_sync",
        "confluence_pages",
        "confluence_space_bindings",
        "confluence_sync_tasks",
        "confluence_webhooks",
        "dataset_collection_bindings",
        "dataset_keyword_tables",
        "dataset_permissions",
        "dataset_process_rules",
        "dataset_queries",
        "datasets",
        "document_permissions",
        "document_pipeline_executions",
        "document_summaries",
        "document_versions",
        "documents",
        "embedding_migration_progress",
        "embedding_migrations",
        "embedding_vector_cache",
        "image_segments",
        "kb_bm25_v2_lifecycle",
        "kb_eval_golden",
        "kb_eval_golden_release",
        "segment_images",
        "segments",
        "version_retention_policies",
    }
)
_KB_TABLE_PATTERN = "|".join(sorted(map(re.escape, KB_TABLES), key=len, reverse=True))
_IDENTIFIER_PREFIX = r"(?:[`\"]?knowledge[`\"]?\s*\.\s*)?"
_IDENTIFIER_SUFFIX = r"\b[`\"]?"
KB_SQL_RE = re.compile(
    r"\b(?:FROM|INTO|UPDATE|JOIN|REFERENCES|"
    r"(?:ALTER|CREATE|DROP|TRUNCATE)\s+TABLE(?:\s+IF\s+(?:NOT\s+)?EXISTS)?|"
    r"CREATE\s+(?:UNIQUE\s+)?INDEX(?:\s+IF\s+NOT\s+EXISTS)?\s+\S+\s+ON)"
    r"\s+(?:ONLY\s+)?"
    + _IDENTIFIER_PREFIX
    + r"[`\"]?(?:"
    + _KB_TABLE_PATTERN
    + r")"
    + _IDENTIFIER_SUFFIX,
    re.IGNORECASE,
)
KB_METADATA_RE = re.compile(
    r"(?:to_regclass\(\s*['\"](?:knowledge\.)?|"
    r"\btable_name\s*=\s*['\"]|"
    r"\btablename\s*=\s*['\"])(?:"
    + _KB_TABLE_PATTERN
    + r")\b[`\"]?",
    re.IGNORECASE,
)

# --- Rule 2: KB request schemas ----------------------------------------------

KS_API_ROOT = ROOT / "apps" / "knowledge-service" / "src" / "knowledge_service" / "api"
# KB-shaped names that are banned even if KS has not declared them yet, so a
# "copy first, land later" drift cannot slip through the name mirror.
KB_SCHEMA_NAME_RE = re.compile(
    r"^(?:"
    r"Dataset\w*(?:Schema|Request|Payload)"
    r"|Segment\w*(?:Schema|Request|Payload)"
    r"|Document\w*(?:Schema|Request|Payload)"
    r"|Retrieve\w*(?:Schema|Request|Payload)"
    r"|KBSearchRequest|KBQueryRequest|Confluence\w*(?:Schema|CreateSchema|UpdateSchema)"
    r")$"
)


def _base_name(base: ast.expr) -> str:
    if isinstance(base, ast.Name):
        return base.id
    if isinstance(base, ast.Attribute):
        return base.attr
    if isinstance(base, ast.Call) and isinstance(base.func, (ast.Name, ast.Attribute)):
        return _base_name(base.func)
    return ""


def _pydantic_model_names(tree: ast.Module) -> set[str]:
    """Local class names that (transitively) inherit pydantic.BaseModel."""
    parents: dict[str, set[str]] = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.ClassDef):
            parents[node.name] = {_base_name(b) for b in node.bases}
    models = {name for name, bases in parents.items() if "BaseModel" in bases}
    changed = True
    while changed:
        changed = False
        for name, bases in parents.items():
            if name not in models and bases & models:
                models.add(name)
                changed = True
    return models


_KS_API_MODELS: set[str] | None = None


def _ks_api_model_names() -> set[str]:
    """Pydantic model names defined anywhere under KS's api/ package."""
    global _KS_API_MODELS
    if _KS_API_MODELS is None:
        names: set[str] = set()
        for path in sorted(KS_API_ROOT.rglob("*.py")):
            if "__pycache__" in path.parts:
                continue
            try:
                names |= _pydantic_model_names(ast.parse(path.read_text(encoding="utf-8")))
            except (OSError, SyntaxError, UnicodeDecodeError):
                continue
        _KS_API_MODELS = names
    return _KS_API_MODELS


def _check_file(path: Path) -> list[str]:
    rel = str(path.relative_to(ROOT))
    findings: list[str] = []
    try:
        src = path.read_text(encoding="utf-8")
        tree = ast.parse(src, filename=rel)
    except (OSError, SyntaxError, UnicodeDecodeError) as exc:
        return [f"{rel}:0: unreadable ({exc.__class__.__name__})"]

    # Rule 1: any KB-table SQL or metadata probe is a boundary violation.
    for node in ast.walk(tree):
        if not isinstance(node, ast.Constant) or not isinstance(node.value, str):
            continue
        if KB_SQL_RE.search(node.value) or KB_METADATA_RE.search(node.value):
            findings.append(
                f"{rel}:{node.lineno}: KB-table SQL in gateway data plane"
                f" (knowledge-service owns this storage; PRD T8.4 rule 1)"
            )

    # Rule 2: schemas that mirror the knowledge-service API contract.
    models = _pydantic_model_names(tree)
    ks_names = _ks_api_model_names()
    for node in ast.walk(tree):
        if not (isinstance(node, ast.ClassDef) and node.name in models):
            continue
        if node.name in ks_names:
            findings.append(
                f"{rel}:{node.lineno}: '{node.name}' mirrors a knowledge-service API"
                f" contract class (proxy KB bodies through; PRD T8.4 rule 2)"
            )
        elif KB_SCHEMA_NAME_RE.match(node.name):
            findings.append(
                f"{rel}:{node.lineno}: KB request schema '{node.name}' defined in gateway"
                f" (PRD T8.4 rule 2)"
            )
    return findings


def main() -> int:
    findings: list[str] = []
    for root in SCAN_ROOTS:
        for path in sorted(root.rglob("*.py")):
            if "__pycache__" in path.parts:
                continue
            findings.extend(_check_file(path))
    unique = sorted(set(findings))
    for finding in unique:
        print(finding)
    if unique:
        print(f"Gateway KB boundary gate: FAIL ({len(unique)} violations)")
        return 1
    print("Gateway KB boundary gate: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
