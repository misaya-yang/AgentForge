#!/usr/bin/env python3
"""ARC-04 mechanical boundary gate for ``ai-gateway-core`` / ``ai-gateway-contracts``.

PRD §ARC-04 gates, checked mechanically against the working tree and the
committed inventory baseline (``reports/inventory/core-import-inventory.json``):

1. **Contracts content allowlist** — only whitelisted modules may exist in
   ``ai_gateway_contracts``; adding a module requires updating this gate.
2. **No forbidden dependencies in contracts** — contracts may import only an
   explicit pure-computation stdlib allowlist + ``pydantic``.  Filesystem,
   process, socket/HTTP, database, Redis, provider SDK and service-config
   imports fail the gate.
3. **No new domain implementations in core** — the set of ``ai_gateway_core``
   modules must not grow beyond the committed baseline.
4. **Knowledge→core dependency count does not grow** — the number of core
   modules imported by ``apps/knowledge-service`` is frozen at the baseline.
5. **No circular dependencies** — ``ai_gateway_contracts`` must never import
   ``ai_gateway_core`` (core may import contracts).
6. **Shim consumers do not grow** — every ARC-04 compatibility shim lists its
   consumers in the baseline; new consumers must migrate to contracts instead.
7. **Shim map consistency** — ``CORE_TO_CONTRACTS`` (the authoritative shim
   list) must match the tree: every mapped shim file exists, carries the shim
   marker, and its contracts target exists and is allowlisted.
8. **Mixed exports stay bounded** — concrete core modules that re-export a
   contracts symbol retain a separate consumer/deletion ledger; they are not
   mislabeled as pure compatibility shims.

Run::

    uv run python scripts/core_boundary/check_core_boundary.py

Negative self-test (fabricated violations must all be detected)::

    uv run python scripts/core_boundary/check_core_boundary.py --self-test
"""

from __future__ import annotations

import argparse
import ast
import json
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from inventory_core_consumption import (  # noqa: E402
    CONTRACTS_PKG_DIR,
    CORE_PKG_DIR,
    CORE_TO_CONTRACTS,
    MIXED_CORE_EXPORTS,
    SHIM_MARKER,
    InventoryProvenanceError,
    build_inventory,
    build_inventory_from_git,
    repo_root,
    verify_inventory_provenance,
)

BASELINE_PATH = Path("reports/inventory/core-import-inventory.json")
EVIDENCE_PATH = Path("tmp/gate-evidence/core-boundary-gate.json")

# --- check 1: contracts content allowlist -----------------------------------

# The only modules allowed inside ai_gateway_contracts (besides __init__).
# A new protocol migration adds its module here in the same reviewed change.
CONTRACTS_ALLOWLIST: frozenset[str] = frozenset(
    {
        "ai_gateway_contracts",
        "ai_gateway_contracts.agent_launch",
        "ai_gateway_contracts.agent_runtime",
        "ai_gateway_contracts.agent_runtime_lease",
        "ai_gateway_contracts.capability_proof",
        "ai_gateway_contracts.database_revision",
        "ai_gateway_contracts.pricing",
        "ai_gateway_contracts.retrieval_metrics",
        "ai_gateway_contracts.event_envelope",
        "ai_gateway_contracts.event_errors",
        "ai_gateway_contracts.replay",
    }
)

# --- check 2: forbidden imports ----------------------------------------------

# Third-party packages contracts may import.
CONTRACTS_ALLOWED_THIRD_PARTY: frozenset[str] = frozenset({"pydantic"})

# Explicit pure-computation stdlib surface used by the current contracts.
# Everything else is denied until reviewed; this is intentionally not
# ``sys.stdlib_module_names`` because stdlib includes filesystem, subprocess,
# socket and HTTP clients.
CONTRACTS_ALLOWED_STDLIB: frozenset[str] = frozenset(
    {
        "base64",
        "collections",
        "copy",
        "dataclasses",
        "decimal",
        "datetime",
        "hashlib",
        "hmac",
        "json",
        "math",
        "re",
        "secrets",
        "threading",
        "time",
        "typing",
        "uuid",
    }
)

# Explicit names kept for readable error messages (subset of the effective
# rule "stdlib + pydantic only", listed so a violation message can point at
# the exact PRD category).
FORBIDDEN_EXAMPLES: frozenset[str] = frozenset(
    {
        "asyncpg",
        "redis",
        "httpx",
        "aiohttp",
        "requests",
        "fastapi",
        "starlette",
        "oss2",
        "boto3",
        "aioboto3",
        "aiofiles",
        "PIL",
        "numpy",
        "cryptography",
        "opentelemetry",
        "pydantic_settings",
        "dotenv",
        "dashscope",
        "qdrant_client",
        "yaml",
        "tiktoken",
        "websockets",
        "sse_starlette",
        # stdlib I/O / process / network entry points
        "http",
        "io",
        "os",
        "pathlib",
        "shutil",
        "socket",
        "subprocess",
        "tempfile",
        "urllib",
    }
)

FORBIDDEN_IO_CALLS: frozenset[str] = frozenset(
    {
        "open",
        "__import__",
        "connect",
        "mkdir",
        "read_bytes",
        "read_text",
        "recv",
        "rename",
        "request",
        "rmdir",
        "send",
        "system",
        "touch",
        "unlink",
        "urlopen",
        "write_bytes",
        "write_text",
    }
)


def _all_import_targets(path: Path) -> list[str]:
    """Every absolute dotted import target appearing anywhere in ``path``."""
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    except (SyntaxError, UnicodeDecodeError):
        return []
    targets: list[str] = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            targets.extend(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
            targets.append(node.module)
    return targets


def _forbidden_io_calls(path: Path) -> list[str]:
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    except (SyntaxError, UnicodeDecodeError):
        return []
    found: set[str] = set()
    for node in ast.walk(tree):
        if not isinstance(node, ast.Call):
            continue
        if isinstance(node.func, ast.Name) and node.func.id in FORBIDDEN_IO_CALLS:
            found.add(node.func.id)
        elif isinstance(node.func, ast.Attribute) and node.func.attr in FORBIDDEN_IO_CALLS:
            found.add(node.func.attr)
    return sorted(found)


def _iter_pkg_files(root: Path, pkg_dir: Path) -> list[Path]:
    base = root / pkg_dir
    if not base.is_dir():
        return []
    return sorted(p for p in base.rglob("*.py") if "__pycache__" not in p.parts)


def check_contracts_allowlist(root: Path) -> list[str]:
    from inventory_core_consumption import package_modules

    violations: list[str] = []
    for dotted in sorted(package_modules(root, CONTRACTS_PKG_DIR)):
        if dotted not in CONTRACTS_ALLOWLIST:
            violations.append(
                f"contracts module not in allowlist: {dotted} "
                f"(update CONTRACTS_ALLOWLIST in {Path(__file__).name} after review)"
            )
    for expected in sorted(CONTRACTS_ALLOWLIST):
        parts = expected.split(".")
        rel = CONTRACTS_PKG_DIR / Path(*parts[1:])
        file_candidates = [rel.with_suffix(".py"), rel / "__init__.py"]
        if expected == "ai_gateway_contracts":
            file_candidates = [CONTRACTS_PKG_DIR / "__init__.py"]
        if not any((root / c).is_file() for c in file_candidates):
            violations.append(f"allowlisted contracts module missing from tree: {expected}")
    return violations


def check_contracts_imports(root: Path) -> list[str]:
    violations: list[str] = []
    for path in _iter_pkg_files(root, CONTRACTS_PKG_DIR):
        for target in _all_import_targets(path):
            top = target.split(".")[0]
            if top in ("ai_gateway_contracts", "__future__"):
                continue
            if top == "ai_gateway_core":
                violations.append(
                    f"circular dependency: contracts imports core "
                    f"({target} in {path.relative_to(root)})"
                )
                continue
            if top in CONTRACTS_ALLOWED_THIRD_PARTY:
                continue
            if top in CONTRACTS_ALLOWED_STDLIB:
                continue
            violations.append(
                f"forbidden contracts dependency: {top} ({target} in "
                f"{path.relative_to(root)}) — contracts allows only the reviewed "
                "pure-computation stdlib surface + pydantic"
            )
        for call in _forbidden_io_calls(path):
            violations.append(
                f"forbidden contracts I/O call: {call} ({path.relative_to(root)})"
            )
    return violations


def check_core_no_new_modules(root: Path, baseline: dict) -> list[str]:
    from inventory_core_consumption import package_modules

    baseline_modules = set(baseline.get("modules", {}))
    live_modules = set(package_modules(root, CORE_PKG_DIR))
    added = sorted(live_modules - baseline_modules)
    return [
        f"new core module beyond baseline (domain implementation growth): {m} "
        f"— move domain code to its owner, not into ai-gateway-core"
        for m in added
    ]


def check_knowledge_no_growth(root: Path, baseline: dict) -> list[str]:
    live = build_inventory(root)
    baseline_count = baseline.get("knowledge_core_module_count", 0)
    live_count = live["knowledge_core_module_count"]
    violations: list[str] = []
    if live_count > baseline_count:
        added = sorted(set(live["knowledge_core_modules"]) - set(baseline["knowledge_core_modules"]))
        violations.append(
            f"knowledge→core dependency count grew {baseline_count} → {live_count}; "
            f"new modules: {', '.join(added) or '(module-level growth)'}"
        )
    return violations


def check_shim_consumers_no_growth(root: Path, baseline: dict) -> list[str]:
    live = build_inventory(root)
    violations: list[str] = []
    baseline_shims = baseline.get("shim_consumers", {})
    live_shims = live.get("shim_consumers", {})
    for module in sorted(set(live_shims) - set(baseline_shims)):
        violations.append(
            f"new compatibility shim without baseline entry: {module} "
            f"— add it to CORE_TO_CONTRACTS after review"
        )
    for module, base in sorted(baseline_shims.items()):
        live_entry = live_shims.get(module)
        if live_entry is None:
            # Shim deleted — the goal of the migration, not a violation.
            continue
        base_count = base.get("count", 0)
        live_count = live_entry.get("count", 0)
        added = sorted(set(live_entry["files"]) - set(base["files"]))
        if added:
            violations.append(
                f"shim gained consumers for {module} ({base_count} → {live_count}); "
                f"new consumers must import the contracts module instead: "
                f"{', '.join(added)}"
            )
    return violations


def check_mixed_export_consumers_no_growth(root: Path, baseline: dict) -> list[str]:
    """Bound contracts re-exports from core modules that retain real behavior."""

    live = build_inventory(root).get("mixed_export_consumers", {})
    recorded = baseline.get("mixed_export_consumers")
    if not isinstance(recorded, dict):
        return [
            "mixed-export consumer ledger missing from baseline — regenerate and review "
            "the ARC-04 inventory before this gate can pass"
        ]

    violations: list[str] = []
    for module, spec in sorted(MIXED_CORE_EXPORTS.items()):
        live_entry = live.get(module)
        base_entry = recorded.get(module)
        if not isinstance(live_entry, dict):
            violations.append(f"configured mixed export missing from live inventory: {module}")
            continue
        if not isinstance(base_entry, dict):
            violations.append(f"mixed export missing from baseline: {module}")
            continue
        for field in ("contracts_module", "replacement", "deletion_condition"):
            if base_entry.get(field) != live_entry.get(field):
                violations.append(
                    f"mixed export metadata drift for {module}.{field}: "
                    f"{base_entry.get(field)!r} -> {live_entry.get(field)!r}"
                )
        base_symbols = base_entry.get("symbols")
        live_symbols = live_entry.get("symbols")
        if not isinstance(base_symbols, dict) or not isinstance(live_symbols, dict):
            violations.append(f"mixed export symbol ledger malformed: {module}")
            continue
        for symbol in sorted(spec["symbols"]):
            base_symbol = base_symbols.get(symbol)
            live_symbol = live_symbols.get(symbol)
            if not isinstance(base_symbol, dict) or not isinstance(live_symbol, dict):
                violations.append(f"mixed export symbol missing from ledger: {module}.{symbol}")
                continue
            base_files = set(base_symbol.get("files") or [])
            live_files = set(live_symbol.get("files") or [])
            added = sorted(live_files - base_files)
            if added:
                violations.append(
                    f"mixed export gained consumers for {module}.{symbol} "
                    f"({len(base_files)} → {len(live_files)}); import "
                    f"{spec['contracts_module']} directly: {', '.join(added)}"
                )
    return violations


def check_shim_map_consistency(root: Path) -> list[str]:
    """``CORE_TO_CONTRACTS`` must match the tree in both directions.

    Each mapped core shim must exist and carry the shim marker; each mapped
    contracts target must exist.  A finished migration removes the entry from
    ``CORE_TO_CONTRACTS`` together with the shim file.
    """
    from inventory_core_consumption import package_modules

    violations: list[str] = []
    catalog = package_modules(root, CORE_PKG_DIR)
    contracts_catalog = package_modules(root, CONTRACTS_PKG_DIR)
    for shim, contracts_module in sorted(CORE_TO_CONTRACTS.items()):
        shim_path = catalog.get(shim)
        if shim_path is None:
            violations.append(
                f"CORE_TO_CONTRACTS shim module missing from tree: {shim} "
                f"(remove the entry when the shim is deleted)"
            )
            continue
        if SHIM_MARKER not in shim_path.read_text(encoding="utf-8"):
            violations.append(f"shim marker missing from {shim} — expected: {SHIM_MARKER!r}")
        if contracts_module not in contracts_catalog:
            violations.append(
                f"CORE_TO_CONTRACTS contracts target missing from tree: {contracts_module}"
            )
    return violations


def check_mixed_export_map_consistency(root: Path) -> list[str]:
    """Mixed-export config must name real contracts imports and modules."""

    from inventory_core_consumption import package_modules

    violations: list[str] = []
    core_catalog = package_modules(root, CORE_PKG_DIR)
    contracts_catalog = package_modules(root, CONTRACTS_PKG_DIR)
    for module, spec in sorted(MIXED_CORE_EXPORTS.items()):
        path = core_catalog.get(module)
        contracts_module = str(spec["contracts_module"])
        if path is None:
            violations.append(f"configured mixed-export module missing from tree: {module}")
            continue
        if contracts_module not in contracts_catalog:
            violations.append(
                f"mixed-export contracts target missing from tree: {contracts_module}"
            )
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        except (SyntaxError, UnicodeDecodeError) as exc:
            violations.append(f"cannot parse mixed-export module {module}: {exc}")
            continue
        imported: set[str] = set()
        for node in tree.body:
            if (
                isinstance(node, ast.ImportFrom)
                and node.level == 0
                and node.module == contracts_module
            ):
                imported.update(alias.name for alias in node.names)
        missing = sorted(set(spec["symbols"]) - imported)
        if missing:
            violations.append(
                f"mixed-export symbols not imported from {contracts_module} by {module}: "
                f"{', '.join(missing)}"
            )
        if not str(spec.get("deletion_condition") or "").strip():
            violations.append(f"mixed export has no deletion condition: {module}")
    return violations


def run_checks(root: Path, baseline: dict) -> list[str]:
    violations: list[str] = []
    violations += check_contracts_allowlist(root)
    violations += check_contracts_imports(root)
    violations += check_core_no_new_modules(root, baseline)
    violations += check_knowledge_no_growth(root, baseline)
    violations += check_shim_consumers_no_growth(root, baseline)
    violations += check_mixed_export_consumers_no_growth(root, baseline)
    violations += check_shim_map_consistency(root)
    violations += check_mixed_export_map_consistency(root)
    return violations


# --- negative self-test -------------------------------------------------------


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def self_test() -> int:
    """Fabricate every violation class in a sandbox and require detection."""

    failures: list[str] = []

    def expect(label: str, violations: list[str], needle: str) -> None:
        if not any(needle in v for v in violations):
            failures.append(f"{label}: expected a violation containing {needle!r}, got {violations}")

    clean_baseline = {
        "schema_version": "self-test/v2",
        "modules": {
            "ai_gateway_core": {},
            "ai_gateway_core.keep": {},
        },
        "knowledge_core_modules": ["ai_gateway_core.keep"],
        "knowledge_core_module_count": 1,
        "shim_consumers": {},
    }

    # 0: v3 provenance must be reconstructable from the declared Git object;
    # editing payload facts or tree identity can never self-bless the baseline.
    with tempfile.TemporaryDirectory(prefix="arc04-gate-provenance-") as tmp:
        root = Path(tmp)
        subprocess.run(["git", "init", "-q"], cwd=root, check=True)
        subprocess.run(
            ["git", "config", "user.email", "arc04@example.invalid"],
            cwd=root,
            check=True,
        )
        subprocess.run(
            ["git", "config", "user.name", "ARC04 Selftest"],
            cwd=root,
            check=True,
        )
        _write(root / CORE_PKG_DIR / "__init__.py", "")
        _write(root / CORE_PKG_DIR / "keep.py", "VALUE = 1\n")
        _write(root / CONTRACTS_PKG_DIR / "__init__.py", "")
        subprocess.run(["git", "add", "."], cwd=root, check=True)
        subprocess.run(["git", "commit", "-q", "-m", "source"], cwd=root, check=True)
        source_commit = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=root,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
        proven = build_inventory_from_git(root, source_commit)
        try:
            verify_inventory_provenance(root, proven)
        except InventoryProvenanceError as exc:
            failures.append(f"provenance-clean: valid Git inventory failed: {exc}")
        forged_tree = json.loads(json.dumps(proven))
        forged_tree["provenance"]["source_tree"] = "0" * 40
        try:
            verify_inventory_provenance(root, forged_tree)
            failures.append("provenance-tree: forged tree was accepted")
        except InventoryProvenanceError:
            pass
        forged_payload = json.loads(json.dumps(proven))
        forged_payload["gateway_core_module_count"] = 99
        try:
            verify_inventory_provenance(root, forged_payload)
            failures.append("provenance-payload: payload drift was accepted")
        except InventoryProvenanceError:
            pass

    # 1+2: contracts with a non-allowlisted module and forbidden imports.
    with tempfile.TemporaryDirectory(prefix="arc04-gate-selftest-") as tmp:
        root = Path(tmp)
        contracts = CONTRACTS_PKG_DIR
        _write(root / contracts / "__init__.py", "")
        _write(
            root / contracts / "capability_proof.py",
            "import redis\nimport urllib.request\nopen('forbidden')\n",
        )
        _write(root / contracts / "rogue_module.py", "import fastapi\n")
        core = CORE_PKG_DIR
        _write(root / core / "__init__.py", "")
        _write(root / core / "keep.py", "VALUE = 1\n")

        allowlist = check_contracts_allowlist(root)
        expect("allowlist", allowlist, "rogue_module")
        imports = check_contracts_imports(root)
        expect("forbidden-dependency", imports, "redis")
        expect("forbidden-stdlib-io", imports, "urllib")
        expect("forbidden-builtin-io", imports, "I/O call: open")
        expect("forbidden-dependency", imports, "fastapi")
        if check_core_no_new_modules(root, clean_baseline):
            failures.append("core-clean fixture unexpectedly reported growth")

    # 2b: contracts importing core is a cycle violation.
    with tempfile.TemporaryDirectory(prefix="arc04-gate-selftest-") as tmp:
        root = Path(tmp)
        contracts = CONTRACTS_PKG_DIR
        _write(root / contracts / "__init__.py", "")
        _write(root / contracts / "capability_proof.py", "import ai_gateway_core.auth\n")
        cycle = check_contracts_imports(root)
        expect("circular-dependency", cycle, "circular dependency")

    # 3: core grows a new domain module.
    with tempfile.TemporaryDirectory(prefix="arc04-gate-selftest-") as tmp:
        root = Path(tmp)
        core = CORE_PKG_DIR
        _write(root / core / "__init__.py", "")
        _write(root / core / "keep.py", "")
        _write(root / core / "quiz" / "__init__.py", "")
        _write(root / core / "quiz" / "engine.py", "")
        growth = check_core_no_new_modules(root, clean_baseline)
        expect("core-new-module", growth, "ai_gateway_core.quiz.engine")

    # 4: knowledge consumes one more core module than the baseline.
    with tempfile.TemporaryDirectory(prefix="arc04-gate-selftest-") as tmp:
        root = Path(tmp)
        core = CORE_PKG_DIR
        _write(root / core / "__init__.py", "")
        _write(root / core / "keep.py", "")
        _write(root / core / "extra.py", "")
        _write(
            root / "apps" / "knowledge-service" / "src" / "knowledge_service" / "user.py",
            "from ai_gateway_core import extra\nfrom ai_gateway_core import keep\n",
        )
        knowledge = check_knowledge_no_growth(root, clean_baseline)
        expect("knowledge-growth", knowledge, "dependency count grew")

    # 7: the shim map names modules that do not exist in the tree.
    with tempfile.TemporaryDirectory(prefix="arc04-gate-selftest-") as tmp:
        root = Path(tmp)
        _write(root / CORE_PKG_DIR / "__init__.py", "")
        _write(root / CONTRACTS_PKG_DIR / "__init__.py", "")
        map_violations = check_shim_map_consistency(root)
        expect("shim-map", map_violations, "shim module missing from tree")

    # 6: a shim consumer appears that was not in the baseline.
    shim_baseline = json.loads(json.dumps(clean_baseline))
    shim_baseline["shim_consumers"] = {
        "ai_gateway_core.keep": {"files": [], "count": 0},
    }
    with tempfile.TemporaryDirectory(prefix="arc04-gate-selftest-") as tmp:
        root = Path(tmp)
        core = CORE_PKG_DIR
        _write(root / core / "__init__.py", "")
        _write(
            root / core / "keep.py",
            '"""Compatibility shim — implementation moved to ``ai_gateway_contracts``."""\n'
            "from ai_gateway_contracts.capability_proof import SCHEMA_VERSION  # noqa: F401\n",
        )
        _write(
            root / "src" / "new_consumer.py",
            "from ai_gateway_core.keep import SCHEMA_VERSION\n",
        )
        contracts = CONTRACTS_PKG_DIR
        _write(root / contracts / "__init__.py", "")
        _write(root / contracts / "capability_proof.py", "SCHEMA_VERSION = 'x'\n")
        shim = check_shim_consumers_no_growth(root, shim_baseline)
        expect("shim-growth", shim, "shim gained consumers")

    # 6b: package-facade imports must remain attributed to their real shim.
    # A textual scan that stops at ``ai_gateway_core.agents`` would otherwise
    # report zero runtime consumers while production imports the re-export.
    facade_baseline = json.loads(json.dumps(clean_baseline))
    facade_path = "packages/ai-gateway-core/src/ai_gateway_core/agents/__init__.py"
    facade_baseline["shim_consumers"] = {
        "ai_gateway_core.agents.runtime": {
            "files": [facade_path],
            "count": 1,
        }
    }
    with tempfile.TemporaryDirectory(prefix="arc04-gate-selftest-") as tmp:
        root = Path(tmp)
        core = CORE_PKG_DIR
        _write(root / core / "__init__.py", "")
        _write(
            root / core / "agents" / "__init__.py",
            "from .runtime import runtime_sha256\n",
        )
        _write(
            root / core / "agents" / "runtime.py",
            '"""Compatibility shim — implementation moved to ``ai_gateway_contracts``."""\n'
            "from ai_gateway_contracts.agent_runtime import runtime_sha256\n",
        )
        _write(
            root / "src" / "new_facade_consumer.py",
            "from ai_gateway_core.agents import runtime_sha256\n",
        )
        contracts = CONTRACTS_PKG_DIR
        _write(root / contracts / "__init__.py", "")
        _write(root / contracts / "agent_runtime.py", "def runtime_sha256(value): return value\n")
        shim = check_shim_consumers_no_growth(root, facade_baseline)
        expect("facade-shim-growth", shim, "shim gained consumers")

    # 8: mixed-module contracts exports keep their own facade/consumer ledger.
    with tempfile.TemporaryDirectory(prefix="arc04-gate-selftest-") as tmp:
        root = Path(tmp)
        core = CORE_PKG_DIR
        facade_path = "packages/ai-gateway-core/src/ai_gateway_core/auth/__init__.py"
        _write(root / core / "__init__.py", "")
        _write(
            root / core / "auth" / "gateway_secret.py",
            "from ai_gateway_contracts.replay import InMemoryReplayStore, ReplayStore\n",
        )
        _write(
            root / core / "auth" / "__init__.py",
            "from .gateway_secret import InMemoryReplayStore, ReplayStore\n",
        )
        _write(
            root / "src" / "new_mixed_consumer.py",
            "from ai_gateway_core.auth import ReplayStore\n",
        )
        contracts = CONTRACTS_PKG_DIR
        _write(root / contracts / "__init__.py", "")
        _write(
            root / contracts / "replay.py",
            "class InMemoryReplayStore: pass\nclass ReplayStore: pass\n",
        )
        mixed_baseline = json.loads(json.dumps(clean_baseline))
        spec = MIXED_CORE_EXPORTS["ai_gateway_core.auth.gateway_secret"]
        mixed_baseline["mixed_export_consumers"] = {
            "ai_gateway_core.auth.gateway_secret": {
                "contracts_module": spec["contracts_module"],
                "replacement": spec["replacement"],
                "deletion_condition": spec["deletion_condition"],
                "symbols": {
                    "InMemoryReplayStore": {"files": [facade_path], "count": 1},
                    "ReplayStore": {"files": [facade_path], "count": 1},
                },
            }
        }
        mixed = check_mixed_export_consumers_no_growth(root, mixed_baseline)
        expect("mixed-export-growth", mixed, "mixed export gained consumers")
        consistency = check_mixed_export_map_consistency(root)
        if consistency:
            failures.append(f"mixed-export-map clean fixture failed: {consistency}")

    if failures:
        print("SELF-TEST FAILED")
        for failure in failures:
            print(f"  - {failure}")
        return 1
    print("self-test OK: all fabricated violations were detected")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--self-test",
        action="store_true",
        help="run the negative self-test instead of the real gate",
    )
    parser.add_argument(
        "--baseline",
        default=str(BASELINE_PATH),
        help="inventory baseline JSON (repo-relative)",
    )
    parser.add_argument(
        "--evidence-out",
        default=str(EVIDENCE_PATH),
        help="gate evidence JSON (repo-relative)",
    )
    args = parser.parse_args(argv)
    if args.self_test:
        return self_test()

    root = repo_root()
    baseline_path = root / args.baseline
    evidence_path = Path(args.evidence_out)
    if not evidence_path.is_absolute():
        evidence_path = root / evidence_path
    evidence: dict[str, object] = {
        "gate": "core-boundary",
        "tier": "L0",
        "baseline": args.baseline,
        "violations": [],
    }
    if not baseline_path.is_file():
        print(f"baseline not found: {args.baseline}")
        evidence.update({"result": "error", "error": "baseline not found"})
        exit_code = 2
    else:
        try:
            baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
            provenance = verify_inventory_provenance(root, baseline)
        except (
            InventoryProvenanceError,
            json.JSONDecodeError,
            OSError,
        ) as exc:
            print(f"baseline provenance invalid: {exc}")
            evidence.update(
                {
                    "result": "error",
                    "provenance": {"result": "fail", "error": str(exc)},
                    "error": str(exc),
                }
            )
            exit_code = 2
        else:
            evidence["provenance"] = {"result": "pass", **provenance}
            violations = run_checks(root, baseline)
            evidence["violations"] = violations
            if violations:
                print(f"core boundary gate FAILED — {len(violations)} violation(s):")
                for violation in violations:
                    print(f"  - {violation}")
                evidence["result"] = "fail"
                exit_code = 1
            else:
                live = build_inventory(root)
                evidence["result"] = "pass"
                evidence["scanned"] = {
                    "contracts_modules": len(live["contracts_modules"]),
                    "core_modules": len(live["modules"]),
                    "shim_ledgers": len(live["shim_consumers"]),
                    "mixed_export_ledgers": len(live["mixed_export_consumers"]),
                }
                print(
                    "core boundary gate OK — "
                    f"{len(live['contracts_modules'])} contracts modules within allowlist, "
                    f"{len(live['modules'])} core modules (no growth), "
                    f"knowledge→core = {live['knowledge_core_module_count']} "
                    f"(baseline {baseline.get('knowledge_core_module_count')}), "
                    f"{len(live['shim_consumers'])} shims + "
                    f"{len(live['mixed_export_consumers'])} mixed exports tracked; "
                    f"baseline source={provenance['source_commit']}"
                )
                exit_code = 0

    try:
        evidence_path.parent.mkdir(parents=True, exist_ok=True)
        evidence_path.write_text(
            json.dumps(evidence, ensure_ascii=False, indent=2) + "\n",
            encoding="utf-8",
        )
    except OSError as exc:
        print(f"cannot write core boundary evidence: {exc}", file=sys.stderr)
        return 2
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
