#!/usr/bin/env python3
"""Validate only the four ARC-07 candidates explicitly named by the PRD."""

from __future__ import annotations

import ast
import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "deploy/repository-quality/known-candidates.json"
EXPECTED = {
    "file-storage-barrel-only-export": ("src/persistence/storage.py", "FileStorage"),
    "unused-get-langgraph-proxy": ("src/api/deps.py", "get_langgraph_proxy"),
    "streaming-timeout-placeholder": (
        "tests/proxy/test_streaming.py",
        "test_streaming_timeout_handling",
    ),
    "streaming-connection-placeholder": (
        "tests/proxy/test_streaming.py",
        "test_connection_error_handling",
    ),
}


class CandidateError(RuntimeError):
    pass


def _load() -> list[dict]:
    try:
        payload = json.loads(MANIFEST.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise CandidateError(f"candidate manifest unreadable: {exc}") from exc
    if payload.get("schema_version") != "ai-platform/repository-quality-candidates/v1":
        raise CandidateError("unsupported candidate manifest schema")
    rows = payload.get("candidates")
    if not isinstance(rows, list) or {row.get("id") for row in rows if isinstance(row, dict)} != set(EXPECTED):
        raise CandidateError("candidate set differs from the PRD-explicit allowlist")
    return rows


def _definition(path: Path, name: str) -> ast.AST | None:
    tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    return next(
        (
            node
            for node in ast.walk(tree)
            if isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef))
            and node.name == name
        ),
        None,
    )


def _code_references(symbol: str, excluded: set[str]) -> list[str]:
    result = subprocess.run(
        ["git", "ls-files", "*.py"], cwd=ROOT, capture_output=True, text=True, check=True
    )
    hits: list[str] = []
    for rel in result.stdout.splitlines():
        if rel in excluded:
            continue
        path = ROOT / rel
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=rel)
            referenced = any(
                (isinstance(node, ast.Name) and node.id == symbol)
                or (isinstance(node, ast.Attribute) and node.attr == symbol)
                or (
                    isinstance(node, (ast.ClassDef, ast.FunctionDef, ast.AsyncFunctionDef))
                    and node.name == symbol
                )
                or (
                    isinstance(node, (ast.Import, ast.ImportFrom))
                    and any(alias.name == symbol or alias.asname == symbol for alias in node.names)
                )
                for node in ast.walk(tree)
            )
            if referenced:
                hits.append(rel)
        except (OSError, UnicodeDecodeError, SyntaxError) as exc:
            raise CandidateError(f"cannot inspect tracked Python source {rel}: {exc}") from exc
    return hits


def validate() -> dict:
    rows = _load()
    repaired: list[str] = []
    pending: list[str] = []
    removed: list[str] = []
    for row in rows:
        expected_path, expected_symbol = EXPECTED[row["id"]]
        if row.get("path") != expected_path or row.get("symbol") != expected_symbol:
            raise CandidateError(f"candidate identity drift: {row['id']}")
        path = ROOT / expected_path
        if row["classification"] == "self_proving_test":
            node = _definition(path, expected_symbol)
            if node is None or any(isinstance(item, ast.Pass) for item in node.body):
                raise CandidateError(f"self-proving candidate is not repaired: {row['id']}")
            source = ast.get_source_segment(path.read_text(encoding="utf-8"), node) or ""
            expected_code = "504" if "timeout" in row["id"] else "502"
            if "_proxy_streaming" not in source or expected_code not in source:
                raise CandidateError(f"replacement does not freeze the failure mode: {row['id']}")
            repaired.append(row["id"])
        else:
            exclusions = {expected_path}
            if expected_symbol == "FileStorage" and row.get("disposition") != "removed":
                exclusions.add("src/persistence/__init__.py")
            references = _code_references(expected_symbol, exclusions)
            if references:
                raise CandidateError(f"confirmed-dead candidate gained consumers: {row['id']}: {references}")
            definition = _definition(path, expected_symbol) if path.is_file() else None
            if definition is None:
                if row.get("disposition") != "removed":
                    raise CandidateError(f"removed candidate has stale disposition: {row['id']}")
                removed.append(row["id"])
                continue
            if row.get("disposition") != "pending_scoped_removal":
                raise CandidateError(f"present dead candidate disposition drift: {row['id']}")
            pending.append(row["id"])
    return {
        "result": "pass" if not pending else "blocked",
        "repaired": repaired,
        "removed": removed,
        "pending_scoped_removal": pending,
    }


def main() -> int:
    try:
        result = validate()
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0 if result["result"] == "pass" else 2
    except (CandidateError, SyntaxError, subprocess.CalledProcessError) as exc:
        print(f"CANDIDATE ERROR: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
