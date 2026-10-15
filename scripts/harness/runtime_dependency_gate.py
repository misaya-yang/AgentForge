#!/usr/bin/env python3
"""AST/text gate for the Gateway/Rust Runtime ownership boundary.

The retired source trees were removed from the checkout; the exclusion
patterns remain so a partial re-add during recovery can never mask the
rest of the tree. Everything else must not reintroduce the retired Python
loop, Assistant Service process dependencies, or the old Python
docgen/OpenAPI snapshot entrypoints. The report is intentionally
path-specific so cleanup can proceed without hiding residual consumers.
"""

from __future__ import annotations

import ast
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCAN_ROOTS = (ROOT / "src", ROOT / "scripts", ROOT / "tests")
EXCLUDED_PARTS = {
    ("apps", "assistant" + "-service"),
    ("packages", "mcp" + "-docgen-server"),
}
FORBIDDEN_MODULES = (
    "assistant_" + "service",
    "assistant_" + "service.core.agent.agent_loop",
    "assistant_" + "service.core.agent.subagent_manager",
    "assistant_" + "service.core.tools",
    "mcp_" + "docgen_server",
)
FORBIDDEN_TEXT = re.compile(
    r"(?:apps/assistant" + r"-service|packages/mcp" + r"-docgen-server|"
    r"ASSISTANT_SERVICE_URL|http://assistant" + r"-service|--package assistant" + r"-service|"
    r"snapshot_" + r"assistant_openapi|assistant_" + r"openapi_baseline|"
    r"assistant_" + r"service\.main|mcp_" + r"docgen_server)",
    re.IGNORECASE,
)


def _excluded(path: Path) -> bool:
    parts = path.relative_to(ROOT).parts
    return any(parts[:2] == prefix for prefix in EXCLUDED_PARTS)


def _imports(path: Path) -> list[str]:
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    except (OSError, SyntaxError, UnicodeDecodeError):
        return []
    findings: list[str] = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            modules = [alias.name for alias in node.names]
        elif isinstance(node, ast.ImportFrom):
            modules = [node.module or ""]
        else:
            continue
        for module in modules:
            if any(module == forbidden or module.startswith(forbidden + ".") for forbidden in FORBIDDEN_MODULES):
                findings.append(f"{path.relative_to(ROOT)}:{node.lineno}: import {module}")
        if isinstance(node, ast.While):
            names = {
                item.id.lower()
                for item in ast.walk(node)
                if isinstance(item, ast.Name)
            }
            strings = {
                item.value.lower()
                for item in ast.walk(node)
                if isinstance(item, ast.Constant) and isinstance(item.value, str)
            }
            if names & {"tool_calls", "chat_stream", "model_tool_loop"} or any(
                "tool_calls" in value or "chat_stream" in value for value in strings
            ):
                findings.append(
                    f"{path.relative_to(ROOT)}:{node.lineno}: model/tool while loop"
                )
    return findings


def main() -> int:
    findings: list[str] = []
    for root in SCAN_ROOTS:
        for path in sorted(root.rglob("*.py")):
            if (
                _excluded(path)
                or path.name == "runtime_dependency_gate.py"
                or "__pycache__" in path.parts
            ):
                continue
            findings.extend(_imports(path))
            try:
                lines = path.read_text(encoding="utf-8").splitlines()
            except (OSError, UnicodeDecodeError):
                continue
            for line_number, line in enumerate(lines, start=1):
                if FORBIDDEN_TEXT.search(line):
                    findings.append(f"{path.relative_to(ROOT)}:{line_number}: forbidden legacy reference")
    for path in (ROOT / "Makefile", ROOT / "harness.yml", ROOT / "pyproject.toml"):
        for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
            if FORBIDDEN_TEXT.search(line):
                findings.append(f"{path.relative_to(ROOT)}:{line_number}: forbidden legacy reference")
    if findings:
        print("Runtime dependency gate: FAIL")
        print("\n".join(sorted(set(findings))))
        return 1
    print("Runtime dependency gate: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
