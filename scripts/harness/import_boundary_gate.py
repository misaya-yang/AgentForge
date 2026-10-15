#!/usr/bin/env python3
"""Static import-boundary gate for the platform's package zones (L0/L1).

Scans every Python source root with ``ast`` and fails closed when imports
cross the zone contract:

  Zones (top-level module -> zone):
    src                    -> gateway
    knowledge_service      -> knowledge-service   (app)
    local_node             -> local-node          (app)
    ai_gateway_core        -> ai-gateway-core     (core)
    ai_gateway_contracts   -> ai-gateway-contracts (contracts)

  Allowed imports (importer -> imported):
    gateway            -> ai-gateway-core, ai-gateway-contracts
    knowledge-service  -> ai-gateway-core, ai-gateway-contracts
    local-node         -> ai-gateway-core, ai-gateway-contracts
    ai-gateway-core    -> ai-gateway-contracts
    ai-gateway-contracts -> (nothing inside the platform)

  Everything else between platform zones fails: gateway <-> app, app <-> app,
  core -> gateway/app (core must never reverse-depend on service
  implementations), contracts -> anything.

Relative imports never cross zones and are ignored. Third-party and stdlib
imports are ignored. Known legacy exceptions live in
``scripts/harness/import_boundary_allowlist.json``; each entry needs an owner,
a reason and an expiry date. Expired or stale (no longer matching) allowlist
entries fail the gate, so exceptions cannot outlive their cleanup.

Usage:
  python scripts/harness/import_boundary_gate.py                # real gate
  python scripts/harness/import_boundary_gate.py --selftest     # synthetic violations

Exit codes: 0 = boundary intact, 1 = violations/expired/stale, 2 = gate error.
"""

from __future__ import annotations

import argparse
import ast
import datetime as dt
import json
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_ALLOWLIST = Path(__file__).resolve().parent / "import_boundary_allowlist.json"
DEFAULT_EVIDENCE = ROOT / "tmp" / "gate-evidence" / "architecture-boundary-gate.json"

MODULE_ZONE = {
    "src": "gateway",
    "knowledge_service": "knowledge-service",
    "local_node": "local-node",
    "ai_gateway_core": "ai-gateway-core",
    "ai_gateway_contracts": "ai-gateway-contracts",
}

SOURCE_ROOTS = [
    ("src", "gateway"),
    ("apps/knowledge-service/src", "knowledge-service"),
    ("apps/local-node/src", "local-node"),
    ("packages/ai-gateway-core/src", "ai-gateway-core"),
    ("packages/ai-gateway-contracts/src", "ai-gateway-contracts"),
]

ALLOWED_TARGETS: dict[str, set[str]] = {
    "gateway": {"ai-gateway-core", "ai-gateway-contracts"},
    "knowledge-service": {"ai-gateway-core", "ai-gateway-contracts"},
    "local-node": {"ai-gateway-core", "ai-gateway-contracts"},
    "ai-gateway-core": {"ai-gateway-contracts"},
    "ai-gateway-contracts": set(),
}


def zone_of_module(module: str) -> str | None:
    top = module.split(".")[0]
    return MODULE_ZONE.get(top)


def file_imports(path: Path) -> list[tuple[int, str]]:
    """Return (lineno, absolute module) pairs for cross-module imports."""
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    except (SyntaxError, UnicodeDecodeError, OSError) as exc:
        raise ValueError(f"cannot parse {path}: {exc}") from exc
    found: list[tuple[int, str]] = []
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                found.append((node.lineno, alias.name))
        elif isinstance(node, ast.ImportFrom):
            if node.level and node.level > 0:
                continue  # relative import: stays inside the same package
            if node.module:
                found.append((node.lineno, node.module))
    return found


def scan_root(root: Path) -> tuple[list[dict], int]:
    """Scan all zone source roots under *root*; return (violations, scanned file count)."""
    violations: list[dict] = []
    scanned_files = 0
    for rel, importer_zone in SOURCE_ROOTS:
        base = root / rel
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*.py")):
            scanned_files += 1
            for lineno, module in file_imports(path):
                target_zone = zone_of_module(module)
                if target_zone is None or target_zone == importer_zone:
                    continue
                if target_zone in ALLOWED_TARGETS[importer_zone]:
                    continue
                violations.append(
                    {
                        "file": str(path.relative_to(root)),
                        "line": lineno,
                        "importer_zone": importer_zone,
                        "target_module": module,
                        "target_zone": target_zone,
                        "rule": f"{importer_zone} must not import {target_zone}",
                    }
                )
    return violations, scanned_files


def load_allowlist(path: Path) -> list[dict]:
    data = json.loads(path.read_text(encoding="utf-8"))
    entries = data.get("entries") or []
    for entry in entries:
        for key in ("file", "imports", "owner", "reason", "expires"):
            if key not in entry:
                raise ValueError(f"allowlist entry missing '{key}': {entry!r}")
        if not isinstance(entry["imports"], list) or not entry["imports"]:
            raise ValueError(f"allowlist entry 'imports' must be a non-empty list: {entry!r}")
        dt.date.fromisoformat(entry["expires"])  # validates format
    return entries


def apply_allowlist(
    violations: list[dict], entries: list[dict], today: dt.date
) -> tuple[list[dict], list[dict], list[dict], list[dict]]:
    """Split violations into enforced vs allowlisted; find stale/expired entries."""
    enforced: list[dict] = []
    allowlisted: list[dict] = []
    expired: list[dict] = []
    matched_entry_ids: set[int] = set()
    for violation in violations:
        matched = None
        for index, entry in enumerate(entries):
            if entry["file"] != violation["file"]:
                continue
            if any(
                violation["target_module"] == prefix
                or violation["target_module"].startswith(prefix + ".")
                for prefix in entry["imports"]
            ):
                matched = (index, entry)
                break
        if matched is None:
            enforced.append(violation)
            continue
        index, entry = matched
        matched_entry_ids.add(index)
        expiry = dt.date.fromisoformat(entry["expires"])
        record = {**violation, "allowlist_owner": entry["owner"], "expires": entry["expires"]}
        if expiry < today:
            expired.append(record)
        else:
            allowlisted.append(record)
    stale = [
        {"file": entry["file"], "imports": entry["imports"], "owner": entry["owner"]}
        for index, entry in enumerate(entries)
        if index not in matched_entry_ids
    ]
    return enforced, allowlisted, expired, stale


def _print_report(result: dict) -> None:
    enforced = result["violations"]
    allowlisted = result["allowlisted"]
    expired = result["expired_entries"]
    stale = result["stale_entries"]
    print(
        f"scanned {result['scanned_files']} files; "
        f"{len(enforced)} violation(s), {len(allowlisted)} allowlisted, "
        f"{len(expired)} expired allowlist entry(ies), {len(stale)} stale entry(ies)"
    )
    for violation in enforced:
        print(
            f"  VIOLATION {violation['file']}:{violation['line']} "
            f"imports {violation['target_module']} ({violation['rule']})"
        )
    for record in allowlisted:
        print(
            f"  allowlisted {record['file']}:{record['line']} imports {record['target_module']} "
            f"(owner={record['allowlist_owner']}, expires={record['expires']})"
        )
    for record in expired:
        print(
            f"  EXPIRED allowlist entry still needed: {record['file']} imports "
            f"{record['target_module']} (owner={record['allowlist_owner']}, expired {record['expires']})"
        )
    for entry in stale:
        print(f"  STALE allowlist entry matches nothing: {entry['file']} {entry['imports']}")


def run_real_gate(root: Path, allowlist_path: Path, evidence_path: Path) -> int:
    if not root.is_dir():
        print(f"GATE ERROR: repo root not found: {root}", file=sys.stderr)
        return 2
    try:
        entries = load_allowlist(allowlist_path) if allowlist_path.exists() else []
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"GATE ERROR: bad allowlist {allowlist_path}: {exc}", file=sys.stderr)
        return 2

    try:
        violations, scanned_files = scan_root(root)
    except ValueError as exc:
        print(f"GATE ERROR: {exc}", file=sys.stderr)
        return 2

    enforced, allowlisted, expired, stale = apply_allowlist(violations, entries, dt.date.today())
    result = {
        "gate": "architecture-boundary-gate",
        "tier": "L0-static",
        "scanned_files": scanned_files,
        "violations": enforced,
        "allowlisted": allowlisted,
        "expired_entries": expired,
        "stale_entries": stale,
        "allowlist": str(allowlist_path.relative_to(root)) if allowlist_path.exists() else None,
        "result": "pass" if not (enforced or expired or stale) else "fail",
    }
    evidence_path.parent.mkdir(parents=True, exist_ok=True)
    evidence_path.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    _print_report(result)
    if enforced or expired or stale:
        print(
            "Fix the import, or add a dated allowlist entry (owner + reason + expiry) "
            f"in {allowlist_path.relative_to(root)}.",
            file=sys.stderr,
        )
        return 1
    print(f"OK: import boundary intact (evidence: {evidence_path.relative_to(root)})")
    return 0


def _selftest() -> int:
    """Negative self-test: synthetic violations must fail, legal imports must pass."""
    layout = {
        "src/gw_bad_app.py": "import knowledge_service.services.knowledge.worker\n",
        "src/gw_bad_local.py": "from local_node.transport import something\n",
        "src/gw_ok.py": "from ai_gateway_core.events.envelope import Envelope\nimport ai_gateway_contracts\n",
        "apps/knowledge-service/src/knowledge_service/app_bad.py": "import src.api.v1.agents\n",
        "apps/knowledge-service/src/knowledge_service/app_ok.py": "from ai_gateway_core.storage.artifact_storage import ArtifactInfo\n",
        "apps/local-node/src/local_node/cross_app_bad.py": "import knowledge_service.main\n",
        "apps/local-node/src/local_node/local_ok.py": "import ai_gateway_core.comm.client\n",
        "packages/ai-gateway-core/src/ai_gateway_core/core_bad.py": "from src.persistence.database import DatabaseStorage\n",
        "packages/ai-gateway-core/src/ai_gateway_core/core_bad_app.py": "import knowledge_service.services\n",
        "packages/ai-gateway-core/src/ai_gateway_core/core_ok.py": "from ai_gateway_contracts.v1 import events\n",
        "packages/ai-gateway-contracts/src/ai_gateway_contracts/leaf_bad.py": "import ai_gateway_core\n",
        "packages/ai-gateway-contracts/src/ai_gateway_contracts/leaf_ok.py": "import datetime\n",
        "src/relative_ok.py": "from . import sibling\n",
    }
    expected_bad = {
        "src/gw_bad_app.py",
        "src/gw_bad_local.py",
        "apps/knowledge-service/src/knowledge_service/app_bad.py",
        "apps/local-node/src/local_node/cross_app_bad.py",
        "packages/ai-gateway-core/src/ai_gateway_core/core_bad.py",
        "packages/ai-gateway-core/src/ai_gateway_core/core_bad_app.py",
        "packages/ai-gateway-contracts/src/ai_gateway_contracts/leaf_bad.py",
    }
    failures = 0
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        for rel, content in layout.items():
            path = root / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
        violations, scanned = scan_root(root)
        bad_files = {violation["file"] for violation in violations}
        if bad_files != expected_bad:
            failures += 1
            print(f"[FAIL] expected violations in {sorted(expected_bad)}")
            print(f"       got {sorted(bad_files)}")
        else:
            print(f"[ok] all {len(expected_bad)} synthetic violations detected, "
                  f"{scanned} files scanned")
        legal_only = {
            "src/gw_ok.py",
            "apps/knowledge-service/src/knowledge_service/app_ok.py",
            "apps/local-node/src/local_node/local_ok.py",
            "packages/ai-gateway-core/src/ai_gateway_core/core_ok.py",
            "packages/ai-gateway-contracts/src/ai_gateway_contracts/leaf_ok.py",
            "src/relative_ok.py",
        }
        false_positives = bad_files & legal_only
        if false_positives:
            failures += 1
            print(f"[FAIL] false positives on legal imports: {sorted(false_positives)}")
        else:
            print("[ok] legal imports (gateway->core, app->core, core->contracts, relative) produce nothing")

        # Allowlist behaviour: match + expiry + staleness.
        entries = [
            {
                "file": "src/gw_bad_app.py",
                "imports": ["knowledge_service.services.knowledge.worker"],
                "owner": "selftest",
                "reason": "synthetic",
                "expires": "2999-01-01",
            },
            {
                "file": "src/does_not_exist.py",
                "imports": ["knowledge_service"],
                "owner": "selftest",
                "reason": "stale entry must fail",
                "expires": "2999-01-01",
            },
        ]
        enforced, allowlisted, expired, stale = apply_allowlist(violations, entries, dt.date.today())
        if len(allowlisted) != 1 or allowlisted[0]["file"] != "src/gw_bad_app.py":
            failures += 1
            print(f"[FAIL] allowlist match wrong: {allowlisted}")
        else:
            print("[ok] matching dated allowlist entry suppresses exactly its violation")
        if len(stale) != 1:
            failures += 1
            print(f"[FAIL] stale entry not detected: {stale}")
        else:
            print("[ok] stale allowlist entry flagged")
        expired_entries = [
            {
                "file": "src/gw_bad_local.py",
                "imports": ["local_node"],
                "owner": "selftest",
                "reason": "expired entry must fail",
                "expires": "2000-01-01",
            }
        ]
        _, _, expired, _ = apply_allowlist(violations, expired_entries, dt.date.today())
        if len(expired) != 1:
            failures += 1
            print(f"[FAIL] expired entry not detected: {expired}")
        else:
            print("[ok] expired allowlist entry flagged as violation")

    if failures:
        print(f"SELFTEST FAILED: {failures} check(s) failed", file=sys.stderr)
        return 1
    print("SELFTEST OK: synthetic boundary violations fail closed, legal imports pass")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--allowlist", type=Path, default=DEFAULT_ALLOWLIST)
    parser.add_argument("--evidence-out", type=Path, default=DEFAULT_EVIDENCE)
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args(argv)
    if args.selftest:
        return _selftest()
    return run_real_gate(args.root, args.allowlist, args.evidence_out)


if __name__ == "__main__":
    raise SystemExit(main())
