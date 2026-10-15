"""Dependency baseline.

Produces ``dependency-baseline.json``:

* declared Python dependency edges per workspace member (parsed from each
  pyproject.toml), and
* actual cross-unit Python import edges discovered by an AST scan
  (``src`` ↔ ``apps/*`` ↔ ``packages/ai-gateway-core``).

``docs/harness/architecture.md`` states the intended direction: ``src/`` and
``apps/*`` are siblings that may depend on ``packages/ai-gateway-core``; they
must not import each other, and one app must not import another. This baseline
records where the tree stands against that rule at the pinned revision, so the
static import-boundary gate starts from facts, not assumptions.
"""

from __future__ import annotations

import ast
import re

from _common import MODULE_UNITS, REPO_ROOT, base_envelope, unit_for_path, walk_files

UNIT_DIRS = (
    "src",
    "apps/knowledge-service",
    "apps/local-node",
    "packages/ai-gateway-core",
    "packages/ai-gateway-contracts",
    "sdk/python",
)

# ai-gateway-contracts is the I/O-free protocol layer created by ARC-04: every
# Python unit may consume it, and ai-gateway-core may re-export it as shims.
ALLOWED_EDGES = {
    ("gateway", "ai-gateway-core"),
    ("knowledge-service", "ai-gateway-core"),
    ("local-node", "ai-gateway-core"),
    ("sdk-python", "ai-gateway-core"),
    ("gateway", "ai-gateway-contracts"),
    ("knowledge-service", "ai-gateway-contracts"),
    ("local-node", "ai-gateway-contracts"),
    ("sdk-python", "ai-gateway-contracts"),
    ("ai-gateway-core", "ai-gateway-contracts"),
}


def _parse_dependencies(text: str) -> list[str]:
    """Extract the [project] dependencies list from pyproject.toml text."""
    deps: list[str] = []
    in_deps = False

    def add_items(fragment: str) -> None:
        for part in fragment.split(","):
            item = part.split("#", 1)[0].strip().strip("\"'")
            if item:
                deps.append(item)

    for line in text.splitlines():
        stripped = line.strip()
        inline = re.match(r"^dependencies\s*=\s*\[(.*)\]\s*(?:#.*)?$", stripped)
        if inline:
            # Single-line form, e.g. `dependencies = []`.
            add_items(inline.group(1))
            continue
        if re.match(r"^dependencies\s*=\s*\[", stripped):
            in_deps = True
            continue
        if in_deps:
            if stripped.startswith("]"):
                in_deps = False
                continue
            add_items(stripped.strip(", "))
    return sorted(deps)


def declared_dependencies() -> dict[str, dict]:
    """Parse [project] dependencies / optional-dependencies from each pyproject."""
    result: dict[str, dict] = {}
    pyprojects = {
        "gateway (root)": REPO_ROOT / "pyproject.toml",
        "ai-gateway-core": REPO_ROOT / "packages" / "ai-gateway-core" / "pyproject.toml",
        "ai-gateway-contracts": REPO_ROOT / "packages" / "ai-gateway-contracts" / "pyproject.toml",
        "knowledge-service": REPO_ROOT / "apps" / "knowledge-service" / "pyproject.toml",
        "local-node": REPO_ROOT / "apps" / "local-node" / "pyproject.toml",
        "sdk-python": REPO_ROOT / "sdk" / "python" / "pyproject.toml",
    }
    for name, path in pyprojects.items():
        if not path.is_file():
            result[name] = {"pyproject": str(path.relative_to(REPO_ROOT)), "found": False}
            continue
        text = path.read_text(encoding="utf-8")
        deps = _parse_dependencies(text)
        workspace_sources = sorted(re.findall(r"^([\w\-]+)\s*=\s*\{\s*workspace\s*=\s*true", text, re.MULTILINE))
        result[name] = {
            "pyproject": str(path.relative_to(REPO_ROOT)),
            "found": True,
            "dependencies": sorted(deps),
            "workspace_source_deps": workspace_sources,
        }
    return result


def import_edges() -> dict:
    """AST scan: which unit imports which other unit's modules."""
    edges: dict[tuple[str, str], dict[str, set[str]]] = {}
    for rel in walk_files((".py",), roots=UNIT_DIRS):
        path = REPO_ROOT / rel
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"))
        except (SyntaxError, UnicodeDecodeError):
            continue
        source_unit = unit_for_path(rel)
        for node in ast.walk(tree):
            modules: list[str] = []
            if isinstance(node, ast.Import):
                modules = [alias.name for alias in node.names]
            elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
                modules = [node.module]
            for module in modules:
                top = module.split(".", 1)[0]
                target_unit = MODULE_UNITS.get(top)
                if target_unit is None or target_unit == source_unit:
                    continue
                record = edges.setdefault((source_unit, target_unit), {"modules": set(), "files": set()})
                record["modules"].add(module)
                record["files"].add(str(rel))
    serialized = []
    for (source, target), record in sorted(edges.items()):
        serialized.append(
            {
                "from_unit": source,
                "to_unit": target,
                "allowed_by_agents_md": (source, target) in ALLOWED_EDGES,
                "imported_modules": sorted(record["modules"]),
                "importing_files": sorted(record["files"]),
                "importing_file_count": len(record["files"]),
            }
        )
    return {
        "edges": serialized,
        "violations": [edge for edge in serialized if not edge["allowed_by_agents_md"]],
        "rule": (
            "docs/harness/architecture.md: src/ and apps/* are siblings that may depend on "
            "packages/ai-gateway-core; "
            "they must not import each other, and one app must not import another. "
            "Static enforcement is an ARC-00B gate."
        ),
    }


def build() -> dict:
    return {
        **base_envelope("dependency-baseline"),
        "python_units": {
            unit: {
                "path": path,
                "role": role,
            }
            for unit, path, role in (
                ("gateway", "src/", "Gateway service (public API + control/model planes)"),
                ("knowledge-service", "apps/knowledge-service/", "Knowledge API + worker roles"),
                ("local-node", "apps/local-node/", "Host-side Local Node daemon"),
                ("ai-gateway-core", "packages/ai-gateway-core/", "Shared primitives package"),
                ("ai-gateway-contracts", "packages/ai-gateway-contracts/", "I/O-free cross-service protocol contracts (ARC-04)"),
                ("sdk-python", "sdk/python/", "Published Python SDK"),
            )
        },
        "declared_dependencies": declared_dependencies(),
        "import_edges": import_edges(),
        "non_python_edges": {
            "web": "web/ talks to gateway over HTTP only; no code-level dependency on Python units.",
            "rust": (
                "rust/agent-runtime-overlay is a pinned fork + ai-platform crates; it shares no "
                "Python imports. Cross-language contracts travel as JSON schema/fixtures "
                "(see contract-freeze.json) — a Rust contract crate is the authority (ADR-006 §4)."
            ),
        },
    }


if __name__ == "__main__":
    from _common import write_json

    path = write_json("dependency-baseline.json", build())
    print(f"wrote {path}")
