#!/usr/bin/env python3
"""LOC no-growth gate (L0), bound to the checked-in baseline ledger.

Enforces the AC-M22 / ARC-07E discipline against the immutable baseline at
``docs/architecture/baselines/2026-08-post-rag/loc-baseline.json`` (produced
by scripts/inventory/loc_baseline.py, ARC-00A):

  1. every file already over threshold in the ledger may NOT grow
     (shrinking or deletion is fine);
  2. every file NOT in the ledger must stay at or under the threshold
     (Python 800, TypeScript/TSX 500 lines).

The scan universe mirrors the baseline generator exactly (same roots, same
pruned directories, same line counting), so the gate and the ledger always
speak about the same files.

Usage:
  python3 scripts/harness/loc_no_growth_gate.py            # real gate
  python3 scripts/harness/loc_no_growth_gate.py --selftest # synthetic ledger

Exit codes: 0 no growth, 1 violations, 2 gate error.
Evidence: tmp/gate-evidence/loc-no-growth.json
"""

from __future__ import annotations

import argparse
import json
import re
import stat
import subprocess
import sys
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_BASELINE = ROOT / "docs" / "architecture" / "baselines" / "2026-08-post-rag" / "loc-baseline.json"
DEFAULT_EVIDENCE = ROOT / "tmp" / "gate-evidence" / "loc-no-growth.json"
PY_THRESHOLD = 800
TS_THRESHOLD = 500

# Must mirror scripts/inventory/loc_baseline.py and scripts/inventory/_common.py.
SCAN_ROOTS = ("src", "apps", "packages", "scripts", "database", "web", "sdk", "tests")
PRUNED_DIRS = {
    ".git", ".venv", "venv", "node_modules", "dist", "build", "__pycache__",
    ".mypy_cache", ".ruff_cache", ".pytest_cache", ".playwright", ".claude",
    ".worktrees", "tmp", "temp", "logs", "uploads", "test-results",
    "playwright-report", "htmlcov", "target",
}


class LocBaselineError(RuntimeError):
    """Raised when the checked-in LOC ledger has invalid Git provenance."""


class LocScanError(RuntimeError):
    """Raised when the current source tree cannot be scanned completely."""


def _resolve_base_commit(root: Path, raw_sha: object) -> str:
    if not isinstance(raw_sha, str) or not re.fullmatch(r"[0-9a-f]{40}", raw_sha):
        raise LocBaselineError("base_git_sha must be a full lowercase 40-character Git SHA")
    resolved = subprocess.run(
        ["git", "rev-parse", "--verify", f"{raw_sha}^{{commit}}"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if resolved.returncode != 0 or resolved.stdout.strip() != raw_sha:
        raise LocBaselineError(f"base_git_sha is not a resolvable commit: {raw_sha}")
    ancestor = subprocess.run(
        ["git", "merge-base", "--is-ancestor", raw_sha, "HEAD"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if ancestor.returncode != 0:
        raise LocBaselineError(f"base_git_sha is not an ancestor of HEAD: {raw_sha}")
    return raw_sha


def _safe_ledger_path(raw: object) -> str:
    if not isinstance(raw, str) or not raw or "\\" in raw:
        raise LocBaselineError(f"invalid LOC ledger path: {raw!r}")
    path = PurePosixPath(raw)
    if (
        path.is_absolute()
        or path.as_posix() != raw
        or ".." in path.parts
        or any(ord(char) < 32 for char in raw)
    ):
        raise LocBaselineError(f"invalid LOC ledger path: {raw!r}")
    return raw


def _git_blob_lines(root: Path, base_sha: str, rel_path: str) -> int:
    blob = subprocess.run(
        ["git", "show", f"{base_sha}:{rel_path}"],
        cwd=root,
        capture_output=True,
        check=False,
    )
    if blob.returncode != 0:
        raise LocBaselineError(
            f"LOC ledger path is missing from base_git_sha: {rel_path}"
        )
    try:
        text = blob.stdout.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise LocBaselineError(
            f"LOC ledger path is not UTF-8 at base_git_sha: {rel_path}"
        ) from exc
    return len(text.splitlines())


def verify_baseline_provenance(root: Path, baseline: dict) -> dict:
    """Bind every constrained LOC entry to the immutable Git base object."""

    thresholds = baseline.get("thresholds")
    if not isinstance(thresholds, dict):
        raise LocBaselineError("LOC baseline thresholds are missing")
    if thresholds.get("python_new_file_max") != PY_THRESHOLD:
        raise LocBaselineError(
            f"python LOC threshold must remain {PY_THRESHOLD}, got "
            f"{thresholds.get('python_new_file_max')!r}"
        )
    if thresholds.get("typescript_new_file_max") != TS_THRESHOLD:
        raise LocBaselineError(
            f"TypeScript LOC threshold must remain {TS_THRESHOLD}, got "
            f"{thresholds.get('typescript_new_file_max')!r}"
        )

    base_sha = _resolve_base_commit(root, baseline.get("base_git_sha"))
    checked: list[dict[str, object]] = []
    seen: set[str] = set()
    for ledger_key, suffixes, threshold in (
        ("oversized_python", (".py",), PY_THRESHOLD),
        ("oversized_typescript", (".ts", ".tsx"), TS_THRESHOLD),
    ):
        ledger = baseline.get(ledger_key)
        rows = ledger.get("files") if isinstance(ledger, dict) else None
        if not isinstance(rows, list):
            raise LocBaselineError(f"LOC baseline ledger is missing: {ledger_key}.files")
        for row in rows:
            if not isinstance(row, dict):
                raise LocBaselineError(f"LOC baseline row is not an object: {ledger_key}")
            rel_path = _safe_ledger_path(row.get("file"))
            if rel_path in seen:
                raise LocBaselineError(f"duplicate LOC baseline path: {rel_path}")
            seen.add(rel_path)
            if not rel_path.endswith(suffixes):
                raise LocBaselineError(
                    f"LOC baseline path has the wrong language suffix: {rel_path}"
                )
            recorded = row.get("lines")
            if isinstance(recorded, bool) or not isinstance(recorded, int):
                raise LocBaselineError(f"LOC baseline line count is invalid: {rel_path}")
            if recorded <= threshold:
                raise LocBaselineError(
                    f"LOC baseline constrained file is not over threshold: {rel_path}"
                )
            actual = _git_blob_lines(root, base_sha, rel_path)
            if actual != recorded:
                raise LocBaselineError(
                    f"LOC baseline line count does not match {base_sha}:{rel_path}: "
                    f"recorded {recorded}, Git object {actual}"
                )
            checked.append({"file": rel_path, "lines": actual, "ledger": ledger_key})
    return {
        "result": "pass",
        "base_git_sha": base_sha,
        "constrained_files_checked": len(checked),
    }


def count_lines(path: Path) -> int:
    try:
        return len(path.read_text(encoding="utf-8").splitlines())
    except (UnicodeDecodeError, OSError) as exc:
        raise LocScanError(f"cannot read source file {path}: {exc}") from exc


def walk(root: Path, suffix: str) -> dict[str, int]:
    """Map repo-relative path -> line count for every matching file."""
    counts: dict[str, int] = {}
    for scan_root in SCAN_ROOTS:
        start = root / scan_root
        try:
            start_mode = start.lstat().st_mode
        except FileNotFoundError:
            continue
        except OSError as exc:
            raise LocScanError(f"cannot inspect scan root {start}: {exc}") from exc
        if stat.S_ISLNK(start_mode) or not stat.S_ISDIR(start_mode):
            raise LocScanError(f"scan root is not a real directory: {start}")
        stack = [start]
        while stack:
            current = stack.pop()
            try:
                entries = sorted(current.iterdir())
            except OSError as exc:
                raise LocScanError(f"cannot list scan directory {current}: {exc}") from exc
            for entry in entries:
                try:
                    mode = entry.lstat().st_mode
                    if stat.S_ISLNK(mode):
                        raise LocScanError(
                            f"symlink inside scan roots is not inspectable: {entry}"
                        )
                    if stat.S_ISDIR(mode):
                        if entry.name not in PRUNED_DIRS:
                            stack.append(entry)
                    elif stat.S_ISREG(mode) and entry.name.endswith(suffix):
                        counts[str(entry.relative_to(root))] = count_lines(entry)
                except OSError as exc:
                    raise LocScanError(f"cannot inspect scan path {entry}: {exc}") from exc
    return counts


def evaluate(
    baseline: dict, current_py: dict[str, int], current_ts: dict[str, int]
) -> tuple[list[dict], list[dict]]:
    """Return (violations, informational)."""
    violations: list[dict] = []
    informational: list[dict] = []
    py_threshold = int(baseline["thresholds"]["python_new_file_max"])
    ts_threshold = int(baseline["thresholds"]["typescript_new_file_max"])

    def check(ledger_key: str, current: dict[str, int], threshold: int, kind: str) -> None:
        ledger = {
            row["file"]: int(row["lines"])
            for row in (baseline.get(ledger_key, {}) or {}).get("files", [])
        }
        for file, base_lines in ledger.items():
            now = current.get(file)
            if now is None:
                informational.append({"file": file, "kind": kind, "note": f"deleted since baseline (was {base_lines} lines)"})
            elif now > base_lines:
                violations.append(
                    {"file": file, "kind": kind, "issue": "oversized file grew",
                     "baseline_lines": base_lines, "current_lines": now}
                )
            elif now < base_lines:
                informational.append(
                    {"file": file, "kind": kind, "note": f"shrank {base_lines} -> {now}"}
                )
        for file, lines in current.items():
            if file not in ledger and lines > threshold:
                violations.append(
                    {"file": file, "kind": kind, "issue": "new file over threshold",
                     "baseline_lines": None, "current_lines": lines, "threshold": threshold}
                )

    check("oversized_python", current_py, py_threshold, "python")
    check("oversized_typescript", current_ts, ts_threshold, "typescript")
    violations.sort(key=lambda item: (item["file"],))
    informational.sort(key=lambda item: (item["file"],))
    return violations, informational


def run(base: Path, baseline_path: Path, evidence_path: Path) -> int:
    if not baseline_path.is_file():
        print(f"GATE ERROR: baseline ledger not found: {baseline_path}", file=sys.stderr)
        return 2
    try:
        baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        print(f"GATE ERROR: unreadable baseline {baseline_path}: {exc}", file=sys.stderr)
        return 2
    if baseline.get("schema") != "ai-gateway/baseline/loc-baseline/v1":
        print(f"GATE ERROR: unexpected baseline schema: {baseline.get('schema')!r}", file=sys.stderr)
        return 2

    try:
        provenance = verify_baseline_provenance(base, baseline)
    except (LocBaselineError, OSError, subprocess.SubprocessError) as exc:
        print(f"GATE ERROR: invalid LOC baseline provenance: {exc}", file=sys.stderr)
        if evidence_path is not None:
            evidence_path.parent.mkdir(parents=True, exist_ok=True)
            evidence_path.write_text(
                json.dumps(
                    {
                        "gate": "loc-no-growth",
                        "tier": "L0",
                        "baseline": str(baseline_path),
                        "base_git_sha": baseline.get("base_git_sha"),
                        "provenance": {"result": "fail", "error": str(exc)},
                        "result": "error",
                    },
                    ensure_ascii=False,
                    indent=2,
                )
                + "\n",
                encoding="utf-8",
            )
        return 2

    current_py: dict[str, int] = {}
    current_ts: dict[str, int] = {}
    try:
        current_py = walk(base, ".py")
        current_ts = {**walk(base, ".ts"), **walk(base, ".tsx")}
    except LocScanError as exc:
        print(f"GATE ERROR: LOC scan incomplete: {exc}", file=sys.stderr)
        if evidence_path is not None:
            evidence_path.parent.mkdir(parents=True, exist_ok=True)
            evidence_path.write_text(
                json.dumps(
                    {
                        "gate": "loc-no-growth",
                        "tier": "L0",
                        "baseline": str(baseline_path),
                        "base_git_sha": baseline.get("base_git_sha"),
                        "provenance": provenance,
                        "scanned": {
                            "python_files": len(current_py),
                            "typescript_files": len(current_ts),
                        },
                        "scan_error": str(exc),
                        "result": "error",
                    },
                    ensure_ascii=False,
                    indent=2,
                )
                + "\n",
                encoding="utf-8",
            )
        return 2
    empty_scans = [
        kind
        for kind, files in (("python", current_py), ("typescript", current_ts))
        if not files
    ]
    if empty_scans:
        detail = f"no source files discovered for: {', '.join(empty_scans)}"
        print(f"GATE ERROR: LOC scan incomplete: {detail}", file=sys.stderr)
        if evidence_path is not None:
            evidence_path.parent.mkdir(parents=True, exist_ok=True)
            evidence_path.write_text(
                json.dumps(
                    {
                        "gate": "loc-no-growth",
                        "tier": "L0",
                        "baseline": str(baseline_path),
                        "base_git_sha": baseline.get("base_git_sha"),
                        "provenance": provenance,
                        "scanned": {
                            "python_files": len(current_py),
                            "typescript_files": len(current_ts),
                        },
                        "scan_error": detail,
                        "result": "error",
                    },
                    ensure_ascii=False,
                    indent=2,
                )
                + "\n",
                encoding="utf-8",
            )
        return 2
    violations, informational = evaluate(baseline, current_py, current_ts)

    result = {
        "gate": "loc-no-growth",
        "tier": "L0",
        "baseline": str(baseline_path.relative_to(base)) if baseline_path.is_relative_to(base) else str(baseline_path),
        "baseline_id": baseline.get("baseline_id"),
        "base_git_sha": baseline.get("base_git_sha"),
        "provenance": provenance,
        "scanned": {"python_files": len(current_py), "typescript_files": len(current_ts)},
        "violations": violations,
        "informational": informational,
        "result": "pass" if not violations else "fail",
    }
    if evidence_path is not None:
        evidence_path.parent.mkdir(parents=True, exist_ok=True)
        evidence_path.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    print(
        f"scanned {len(current_py)} python + {len(current_ts)} typescript files; "
        f"{len(violations)} violation(s), {len(informational)} informational"
    )
    for item in violations:
        base_lines = item["baseline_lines"]
        detail = f"{base_lines} -> {item['current_lines']}" if base_lines else f"{item['current_lines']} lines (threshold {item.get('threshold')})"
        print(f"  FAIL {item['file']}: {item['issue']} ({detail})")
    for item in informational[:10]:
        print(f"  info {item['file']}: {item['note']}")
    if len(informational) > 10:
        print(f"  ... {len(informational) - 10} more informational entries in the evidence")
    if violations:
        print(
            "No-growth is the policy: shrink the file or split it. New files must stay "
            "under the threshold (Python 800 / TypeScript 500).",
            file=sys.stderr,
        )
        return 1
    suffix = f" (evidence: {evidence_path.relative_to(ROOT)})" if evidence_path and base == ROOT else ""
    print(f"OK: LOC no-growth holds{suffix}")
    return 0


def _selftest() -> int:
    baseline = {
        "schema": "ai-gateway/baseline/loc-baseline/v1",
        "baseline_id": "selftest",
        "base_git_sha": "0" * 40,
        "thresholds": {"python_new_file_max": 10, "typescript_new_file_max": 5},
        "oversized_python": {"files": [
            {"file": "src/big.py", "lines": 12},
            {"file": "src/grown.py", "lines": 12},
            {"file": "src/shrunk.py", "lines": 12},
            {"file": "src/gone.py", "lines": 12},
        ]},
        "oversized_typescript": {"files": [{"file": "web/src/big.ts", "lines": 8}]},
    }
    current_py = {
        "src/big.py": 12,          # unchanged oversized: ok
        "src/grown.py": 13,        # grew: violation
        "src/shrunk.py": 11,       # shrank: ok, informational
        "src/new_small.py": 9,      # new under threshold: ok
        "src/new_big.py": 11,       # new over threshold: violation
    }
    current_ts = {"web/src/big.ts": 8, "web/src/new.tsx": 6}  # new.tsx over 5: violation
    violations, informational = evaluate(baseline, current_py, current_ts)
    failed = sorted(v["file"] for v in violations)
    expected = ["src/grown.py", "src/new_big.py", "web/src/new.tsx"]
    notes = sorted(i["file"] for i in informational)
    if failed != expected or "src/gone.py" not in notes or "src/shrunk.py" not in notes:
        print(f"SELFTEST FAILED\n  violations={failed}\n  informational={notes}", file=sys.stderr)
        return 1
    print("SELFTEST OK: growth/new-oversize fail; shrink/delete/under-threshold pass")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    parser.add_argument("--evidence-out", type=Path, default=DEFAULT_EVIDENCE)
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args(argv)
    if args.selftest:
        return _selftest()
    if not args.root.is_dir():
        print(f"GATE ERROR: root not found: {args.root}", file=sys.stderr)
        return 2
    return run(args.root, args.baseline, args.evidence_out)


if __name__ == "__main__":
    raise SystemExit(main())
