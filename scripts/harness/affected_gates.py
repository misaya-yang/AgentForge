#!/usr/bin/env python3
"""Diff -> gate selector: every changed path must map to at least one gate.

Given a base SHA, lists every changed path (committed diff against the base
plus untracked files) and resolves it against the `triggers:` globs of the
gates declared in harness.yml. The selector FAILS CLOSED: any changed path
that matches no gate is an error, because an ungated path is an unverified
change.

Usage:
  python3 scripts/harness/affected_gates.py --base <sha>
  python3 scripts/harness/affected_gates.py --base <sha> --selftest   # glob logic only

Exit codes: 0 = every path gated, 1 = ungated paths, 2 = usage/git error.
Evidence: tmp/gate-evidence/affected-gates.json
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_EVIDENCE = ROOT / "tmp" / "gate-evidence" / "affected-gates.json"


def glob_to_regex(pattern: str) -> re.Pattern[str]:
    """Translate a trigger glob into a path regex.

    Semantics: `*` never crosses `/`, `?` matches one non-`/` char, `**/`
    matches zero or more whole segments, a trailing `**` matches anything.
    """
    out: list[str] = []
    i, n = 0, len(pattern)
    while i < n:
        ch = pattern[i]
        if ch == "*":
            if pattern.startswith("**/", i):
                out.append("(?:[^/]+/)*")
                i += 3
            elif pattern.startswith("**", i):
                out.append(".*")
                i += 2
            else:
                out.append("[^/]*")
                i += 1
        elif ch == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(ch))
            i += 1
    return re.compile("^" + "".join(out) + "$")


def load_gates(harness_path: Path) -> dict[str, dict[str, str]]:
    """Parse the gates block of harness.yml without PyYAML (CI runs pre-install)."""
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import check_harness  # noqa: PLC0415  (same directory, dependency-free readers)

    lines = harness_path.read_text(encoding="utf-8").splitlines()
    gates: dict[str, dict[str, str]] = {}
    current: str | None = None
    name_indent: int | None = None
    for line in check_harness._block_body(lines, "gates"):
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(line) - len(line.lstrip())
        if name_indent is None:
            name_indent = indent
        if indent <= name_indent:
            match = re.match(r"^([^:\s]+):\s*$", stripped)
            if not match:
                raise ValueError(f"unparseable gate header in harness.yml: {line!r}")
            current = match.group(1)
            gates[current] = {}
            continue
        if current is None:
            raise ValueError(f"gate field outside a gate block: {line!r}")
        match = re.match(r"^([^:]+):\s*(.+?)\s*$", stripped)
        if match:
            gates[current][match.group(1).strip()] = match.group(2).strip()
    return gates


def inline_list(value: str) -> list[str]:
    value = value.strip()
    if not (value.startswith("[") and value.endswith("]")):
        return []
    items = []
    for item in value[1:-1].split(","):
        item = item.strip().strip("\"'")
        if item:
            items.append(item)
    return items


def select_gates(
    gates: dict[str, dict[str, str]], changed_paths: list[str]
) -> tuple[dict[str, dict], list[str]]:
    """Return ({gate_name: info incl. matched paths}, ungated_paths)."""
    compiled: list[tuple[str, str, re.Pattern[str]]] = []
    for name, fields in gates.items():
        for trigger in inline_list(fields.get("triggers", "")):
            compiled.append((name, trigger, glob_to_regex(trigger)))

    selected: dict[str, dict] = {}
    ungated: list[str] = []
    for path in changed_paths:
        hits = [
            (name, trigger)
            for name, trigger, regex in compiled
            if regex.match(path)
        ]
        if not hits:
            ungated.append(path)
            continue
        for name, trigger in hits:
            entry = selected.setdefault(
                name,
                {
                    "make": gates[name].get("make"),
                    "shell": gates[name].get("shell"),
                    "tier": gates[name].get("tier"),
                    "required_on": inline_list(gates[name].get("required_on", "")),
                    "resource": gates[name].get("resource"),
                    "ci_job": gates[name].get("ci_job"),
                    "matched_paths": [],
                    "matched_triggers": set(),
                },
            )
            entry["matched_paths"].append(path)
            entry["matched_triggers"].add(trigger)
    for entry in selected.values():
        entry["matched_triggers"] = sorted(entry["matched_triggers"])
    return selected, ungated


def changed_paths_since(base: str) -> list[str]:
    def run(*args: str) -> str:
        result = subprocess.run(
            ["git", *args], cwd=ROOT, capture_output=True, text=True, check=False
        )
        if result.returncode != 0:
            raise RuntimeError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
        return result.stdout

    try:
        run("rev-parse", "--verify", "--quiet", base)
    except RuntimeError as exc:
        raise RuntimeError(f"invalid base SHA {base!r}: {exc}") from exc
    diff = run("diff", "--name-only", base, "--")
    untracked = run("ls-files", "--others", "--exclude-standard")
    paths = {p.strip() for p in (diff + untracked).splitlines() if p.strip()}
    return sorted(paths)


def _selftest() -> int:
    failures = 0

    def check(name: str, got, expected) -> None:
        nonlocal failures
        ok = got == expected
        print(f"[{'ok' if ok else 'FAIL'}] {name}")
        if not ok:
            failures += 1
            print(f"       expected {expected!r}, got {got!r}")

    check("exact file", glob_to_regex("harness.yml").match("harness.yml") is not None, True)
    check("star stays in segment", glob_to_regex("src/*.py").match("src/a/b.py") is None, True)
    check("doublestar crosses segments", glob_to_regex("src/**").match("src/a/b/c.py") is not None, True)
    check("doublestar prefix", glob_to_regex("**/loop-state.json").match("deploy/runbooks/x/loop-state.json") is not None, True)
    check("suffix glob", glob_to_regex("docs/architecture/ADR-006*").match("docs/architecture/ADR-006-x.md") is not None, True)
    check("question mark", glob_to_regex("database/migrations/09?.sql").match("database/migrations/095.sql") is not None, True)
    check("compose star", glob_to_regex("docker-compose*").match("docker-compose.override.yml.example") is not None, True)

    gates = {
        "a": {"make": "gate-a", "triggers": '["src/**", "tests/unit/**"]', "tier": "L1", "required_on": "[change]"},
        "b": {"make": "gate-b", "triggers": "[harness.yml]", "tier": "L0", "required_on": "[change, release]"},
    }
    selected, ungated = select_gates(gates, ["src/x.py", "harness.yml"])
    check("both paths gated", ungated, [])
    check("gate a selected", sorted(selected), ["a", "b"])
    check("gate a path", selected["a"]["matched_paths"], ["src/x.py"])
    selected, ungated = select_gates(gates, ["src/x.py", "ungulated/path.txt"])
    check("unmatched path fails closed", ungated, ["ungulated/path.txt"])

    if failures:
        print(f"SELFTEST FAILED: {failures} check(s)", file=sys.stderr)
        return 1
    print("SELFTEST OK: glob semantics and fail-closed selection verified")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", help="base git SHA to diff against (required)")
    parser.add_argument("--harness", type=Path, default=ROOT / "harness.yml")
    parser.add_argument("--evidence-out", type=Path, default=DEFAULT_EVIDENCE)
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args(argv)

    if args.selftest:
        return _selftest()
    if not args.base:
        print("ERROR: --base <sha> is required (make affected-gates BASE_SHA=<sha>)", file=sys.stderr)
        return 2

    try:
        gates = load_gates(args.harness)
    except (OSError, ValueError) as exc:
        print(f"GATE ERROR: cannot read gates from {args.harness}: {exc}", file=sys.stderr)
        return 2
    if not gates:
        print("GATE ERROR: no gates found in harness.yml", file=sys.stderr)
        return 2

    try:
        paths = changed_paths_since(args.base)
    except (RuntimeError, OSError) as exc:
        print(f"GATE ERROR: {exc}", file=sys.stderr)
        return 2
    selected, ungated = select_gates(gates, paths)

    evidence = {
        "gate": "affected-gates",
        "tier": "L0-selector",
        "base": args.base,
        "changed_paths": paths,
        "selected": selected,
        "ungated_paths": ungated,
        "result": "pass" if not ungated else "fail",
    }
    args.evidence_out.parent.mkdir(parents=True, exist_ok=True)
    args.evidence_out.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    print(f"changed paths: {len(paths)}; affected gates: {len(selected)}")
    if not paths:
        print("no changed paths relative to base; empty selection recorded")
    for name in sorted(selected):
        entry = selected[name]
        entrypoint = entry["make"] or entry["shell"]
        required = "required_on=" + ",".join(entry["required_on"])
        print(
            f"  {name}: {entrypoint} (tier={entry['tier']}, {required}, "
            f"{len(entry['matched_paths'])} path(s))"
        )
    if ungated:
        print(f"\nFAIL: {len(ungated)} changed path(s) match no gate:", file=sys.stderr)
        for path in ungated:
            print(f"  - {path}", file=sys.stderr)
        print(
            "Every changed path must map to a gate. Extend a gate's triggers in "
            "harness.yml (or add a gate) and re-run.",
            file=sys.stderr,
        )
        return 1
    try:
        evidence_display = args.evidence_out.relative_to(ROOT)
    except ValueError:
        evidence_display = args.evidence_out
    print(
        "OK: every changed path maps to at least one gate "
        f"(evidence: {evidence_display})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
