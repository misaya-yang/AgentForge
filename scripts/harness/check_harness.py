#!/usr/bin/env python3
"""Harness contract check: structural lint + semantic validation.

Two families of checks, reported separately:

STRUCTURAL lint (shapes, existence, budgets):
  1. every required document exists and is non-empty;
  2. the agent instruction files stay within their line budgets;
  3. relative links in the instruction and harness docs resolve;
  4. workspace hygiene — specs/configs/screenshots live where they belong.

SEMANTIC validation (the contract matches real behaviour):
  5. every canonical command in harness.yml resolves to a real Make target;
  6. every gate entry carries a valid tier/trigger/required_on/resource/skip/
     timeout/evidence schema, its entrypoint exists, and its ci_job exists in
     .github/workflows/ci.yml;
  7. multi-session programs under deploy/runbooks/ carry their required files
     (warning only).

Dependency-free on purpose: this runs in CI before any package is installed.
Evidence: tmp/gate-evidence/harness-check.json

Usage:  make harness-check
   or:  python3 scripts/harness/check_harness.py
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
HARNESS_FILE = REPO_ROOT / "harness.yml"
MAKEFILE = REPO_ROOT / "Makefile"
CI_WORKFLOW = REPO_ROOT / ".github" / "workflows" / "ci.yml"
EVIDENCE_OUT = REPO_ROOT / "tmp" / "gate-evidence" / "harness-check.json"

# Gate schema vocabulary (documented in the harness.yml gates header).
GATE_TIERS = {"L0", "L1", "L2", "L3"}
GATE_REQUIRED_ON = {"change", "release", "manual"}
GATE_RESOURCES = {"offline", "hosted-service", "live-stack", "provider", "docker"}
GATE_SKIP_POLICIES = {"never", "live-stack", "toolchain"}
CI_JOB_SENTINELS = {"manual"}
GATE_FIELDS = {
    "make", "shell", "purpose", "triggers", "tier", "required_on",
    "resource", "skip", "timeout", "evidence", "ci_job",
}

# Docs whose relative links are checked. These are the files agents actually follow.
LINK_CHECKED = (
    "AGENTS.md",
    "CLAUDE.md",
    "docs/README.md",
    "docs/harness/README.md",
    "docs/harness/platform-architecture.md",
    "docs/harness/architecture.md",
    "docs/harness/commands.md",
    "docs/harness/workflow.md",
    "docs/harness/work-packages.md",
    "docs/harness/runtime-and-secrets.md",
    "docs/harness/integration-and-rollback.md",
    "docs/harness/repository-quality.md",
)

errors: list[str] = []
warnings: list[str] = []


def fail(msg: str) -> None:
    errors.append(msg)


def warn(msg: str) -> None:
    warnings.append(msg)


# -- Minimal harness.yml readers ---------------------------------------------
# harness.yml is a file we own and keep flat on purpose, so a targeted scan is
# more robust here than requiring PyYAML to be installed in every environment.


def _block_body(lines: list[str], key: str) -> list[str]:
    """Return the lines nested under `key:`, at any indentation depth."""
    body: list[str] = []
    indent: int | None = None
    for line in lines:
        if indent is None:
            match = re.match(rf"^(\s*){re.escape(key)}:\s*$", line)
            if match:
                indent = len(match.group(1))
            continue
        if not line.strip():
            continue
        if len(line) - len(line.lstrip()) <= indent:
            break
        body.append(line)
    return body


def block_list(lines: list[str], key: str) -> list[str]:
    """Return the entries of a `key:` block, in either block or inline `[a, b]` form."""
    items = []
    for line in lines:
        inline = re.match(rf"^\s*{re.escape(key)}:\s*\[(.+)\]\s*$", line)
        if inline:
            return [
                item.strip().strip("\"'") for item in inline.group(1).split(",") if item.strip()
            ]
    for line in _block_body(lines, key):
        match = re.match(r"^\s*-\s+(.+?)\s*$", line)
        if match:
            items.append(match.group(1).strip().strip("\"'"))
    return items


def block_map(lines: list[str], key: str) -> dict[str, str]:
    """Return the `name: value` pairs of a `key:` block.

    The value is the rest of the line, so an inline `[a, b, c]` list survives.
    """
    pairs: dict[str, str] = {}
    for line in _block_body(lines, key):
        match = re.match(r"^\s*([^:#]+):\s*(.+?)\s*$", line)
        if match:
            pairs[match.group(1).strip()] = match.group(2).strip().strip("\"'")
    return pairs


def make_targets() -> set[str]:
    text = MAKEFILE.read_text(encoding="utf-8")
    return set(re.findall(r"^([A-Za-z0-9_.\-]+):", text, re.MULTILINE))


# -- Checks -------------------------------------------------------------------


def check_commands(lines: list[str], targets: set[str]) -> int:
    declared = re.findall(r"^\s+make:\s*(\S+)\s*$", "\n".join(lines), re.MULTILINE)
    for target in sorted(set(declared)):
        if target not in targets:
            fail(f"harness.yml declares `make {target}` but the Makefile has no such target")
    return len(set(declared))


def check_required_docs(lines: list[str]) -> int:
    docs = block_list(lines, "required_docs")
    if not docs:
        fail("harness.yml has no required_docs block")
    for rel in docs:
        path = REPO_ROOT / rel
        if not path.is_file():
            fail(f"required doc is missing: {rel}")
        elif path.stat().st_size == 0:
            fail(f"required doc is empty: {rel}")
    return len(docs)


def check_budgets(lines: list[str]) -> int:
    budgets = block_map(lines, "budgets")
    if not budgets:
        fail("harness.yml has no budgets block")
    for rel, limit in budgets.items():
        path = REPO_ROOT / rel
        if not path.is_file():
            fail(f"budgeted file is missing: {rel}")
            continue
        count = len(path.read_text(encoding="utf-8").splitlines())
        if count > int(limit):
            fail(
                f"{rel} is {count} lines, over its {limit}-line budget — "
                f"move detail into docs/harness/ rather than raising the budget"
            )
    return len(budgets)


def check_links() -> int:
    checked = 0
    pattern = re.compile(r"\[[^\]]*\]\(([^)]+)\)")
    for rel in LINK_CHECKED:
        path = REPO_ROOT / rel
        if not path.is_file():
            fail(f"link-checked doc is missing: {rel}")
            continue
        for target in pattern.findall(path.read_text(encoding="utf-8")):
            if target.startswith(("http://", "https://", "#", "mailto:")):
                continue
            resolved = (path.parent / target.split("#", 1)[0]).resolve()
            checked += 1
            if not resolved.exists():
                fail(f"{rel}: broken link -> {target}")
    return checked


PRUNED_DIRS = {
    ".git",
    ".tools",
    ".venv",
    "venv",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".playwright",
    ".claude",
    "tmp",
    "temp",
    "logs",
    "uploads",
    "test-results",
    "playwright-report",
    "htmlcov",
    ".worktrees",
}


def _walk_repo():
    """Yield every repo file, skipping generated and scratch directories."""
    stack = [REPO_ROOT]
    while stack:
        current = stack.pop()
        try:
            entries = list(current.iterdir())
        except (PermissionError, OSError):
            continue
        for entry in entries:
            if entry.is_symlink():
                continue
            if entry.is_dir():
                if entry.name not in PRUNED_DIRS:
                    stack.append(entry)
            else:
                yield entry


def check_workspace(lines: list[str]) -> int:
    """Keep tests, configs, and screenshots out of wherever an agent happened to be.

    Without this, new Playwright specs and verification screenshots land at the
    repository root, which is how the root accumulated stray *.png files before.
    """
    spec_roots = block_list(lines, "spec_roots")
    spec_globs = block_list(lines, "spec_globs")
    root_forbidden = block_list(lines, "root_forbidden")
    config_root = block_map(lines, "workspace").get("playwright_config_root", "web")

    if not (spec_roots and spec_globs and root_forbidden):
        fail("harness.yml workspace block is incomplete")
        return 0

    allowed = [REPO_ROOT / root for root in spec_roots]
    checked = 0

    for path in _walk_repo():
        rel = path.relative_to(REPO_ROOT)
        checked += 1

        is_spec = any(path.match(pattern) for pattern in spec_globs)
        if is_spec and not any(root in path.parents for root in allowed):
            fail(
                f"test spec outside the allowed roots: {rel} "
                f"(move it under {', '.join(spec_roots)})"
            )

        if path.match("playwright*.config.ts") and rel.parts[0] != config_root:
            fail(f"Playwright config outside {config_root}/: {rel}")

    for entry in REPO_ROOT.iterdir():
        if entry.is_file() and any(entry.match(p) for p in root_forbidden):
            fail(
                f"stray file at the repository root: {entry.name} "
                f"(screenshots belong in tmp/, E2E artifacts in web/.playwright/)"
            )

    return checked


def check_programs(lines: list[str]) -> int:
    """Check each program against the file contract of its own loop-state schema.

    v3 and v4 of prd-phase-harness require different files — v4 dropped the
    plan/report/ledger artifacts on purpose. Asserting one schema's contract on the
    other produces permanent warnings, which trains people to ignore warnings.
    """
    root = REPO_ROOT / "deploy" / "runbooks"
    by_schema = block_map(lines, "required_files_by_schema")
    if not root.is_dir() or not by_schema:
        return 0

    seen = 0
    for state in sorted(root.glob("*/loop-state.json")):
        seen += 1
        program = state.parent.relative_to(REPO_ROOT)
        try:
            schema = json.loads(state.read_text(encoding="utf-8")).get("schema_version", "")
        except (json.JSONDecodeError, OSError) as exc:
            fail(f"program {program} has an unreadable loop-state.json: {exc}")
            continue

        raw = by_schema.get(schema)
        if raw is None:
            warn(f"program {program} declares unknown loop-state schema {schema or '(none)'}")
            continue

        for name in [item.strip() for item in raw.strip("[]").split(",") if item.strip()]:
            if not (state.parent / name).is_file():
                warn(f"program {program} ({schema.rsplit('/', 1)[-1]}) is missing {name}")
    return seen


def gate_blocks(lines: list[str]) -> dict[str, dict[str, str]]:
    """Return {gate_name: {field: raw value}} for the gates block."""
    gates: dict[str, dict[str, str]] = {}
    current: str | None = None
    name_indent: int | None = None
    for line in _block_body(lines, "gates"):
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(line) - len(line.lstrip())
        if name_indent is None:
            name_indent = indent
        if indent <= name_indent:
            match = re.match(r"^([^:\s]+):\s*$", stripped)
            if not match:
                fail(f"harness.yml gates: unparseable gate header: {line!r}")
                continue
            current = match.group(1)
            gates[current] = {}
            continue
        if current is None:
            fail(f"harness.yml gates: field outside a gate block: {line!r}")
            continue
        match = re.match(r"^([^:]+):\s*(.+?)\s*$", stripped)
        if match:
            gates[current][match.group(1).strip()] = match.group(2).strip()
    return gates


def _inline_list(value: str) -> list[str]:
    value = value.strip()
    if not (value.startswith("[") and value.endswith("]")):
        return []
    return [item.strip().strip("\"'") for item in value[1:-1].split(",") if item.strip()]


def ci_job_bodies(workflow_path: Path = CI_WORKFLOW) -> dict[str, str]:
    """Return job id -> body for top-level jobs in the CI workflow."""
    if not workflow_path.is_file():
        return {}
    bodies: dict[str, list[str]] = {}
    current: str | None = None
    for line in _block_body(workflow_path.read_text(encoding="utf-8").splitlines(), "jobs"):
        match = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if match:
            current = match.group(1)
            bodies[current] = []
            continue
        if current is not None:
            bodies[current].append(line)
    return {name: "\n".join(body) for name, body in bodies.items()}


def _job_runs_entrypoint(fields: dict[str, str], body: str) -> bool:
    run_text = _job_run_text(body)
    if "make" in fields:
        target = fields["make"].strip("\"'")
        return _run_text_executes_make_target(run_text, target)
    shell = fields.get("shell", "").strip().strip("\"'")
    commands = [command.strip() for command in shell.split("&&") if command.strip()]
    return bool(commands) and all(
        _run_text_executes_shell_command(run_text, command) for command in commands
    )


def _run_text_executes_make_target(run_text: str, target: str) -> bool:
    """Recognize a real, outcome-preserving Make invocation.

    Gate wiring is a trust boundary: a comment, dry-run, alternate makefile,
    background command, or explicit error-swallowing suffix must not count as
    executing the declared target.
    """

    if re.search(r"\bset\s+\+(?:e|o\s+pipefail)\b", run_text):
        return False
    pattern = re.compile(
        rf"^\s*(?P<environment>(?:[A-Za-z_][A-Za-z0-9_]*=\S+\s+)*)"
        rf"make\s+{re.escape(target)}(?:\s|$)"
    )
    for line in _logical_shell_lines(run_text):
        match = pattern.match(line)
        if match is None or re.search(
            r"(?:^|\s)(?:MAKEFLAGS|MFLAGS|GNUMAKEFLAGS)=",
            match.group("environment"),
        ):
            continue
        if not _shell_line_preserves_outcome(line):
            continue
        if re.search(
            r"(?:^|\s)(?:-n|--dry-run|--just-print|--reconnaissance|"
            r"-t|--touch|-q|--question|-f|--file|-C|--directory|--eval)"
            r"(?:=|\s|$)",
            line,
        ):
            continue
        return True
    return False


def _run_text_executes_shell_command(run_text: str, command: str) -> bool:
    if re.search(r"\bset\s+\+(?:e|o\s+pipefail)\b", run_text):
        return False
    pattern = re.compile(rf"^\s*{re.escape(command)}(?:\s|$)")
    return any(
        pattern.match(line) is not None and _shell_line_preserves_outcome(line)
        for line in _logical_shell_lines(run_text)
    )


def _logical_shell_lines(run_text: str) -> list[str]:
    logical: list[str] = []
    pending = ""
    for raw in run_text.splitlines():
        line = raw.strip()
        if pending:
            line = pending + line
        if line.endswith("\\"):
            pending = line[:-1].rstrip() + " "
            continue
        if line:
            logical.append(line)
        pending = ""
    if pending:
        logical.append(pending.rstrip())
    return logical


def _shell_line_preserves_outcome(line: str) -> bool:
    if "||" in line or ";" in line or "$(" in line or "`" in line:
        return False
    if re.search(r"(?:^|\s)&(?:\s|$)", line):
        return False
    pipelines = re.split(r"(?<!\|)\|(?!\|)", line)
    return len(pipelines) == 1 or (
        len(pipelines) == 2 and pipelines[1].lstrip().startswith("tee ")
    )


def _job_run_text(body: str) -> str:
    """Extract only shell bodies from workflow ``run`` steps."""
    lines = body.splitlines()
    commands: list[str] = []
    index = 0
    while index < len(lines):
        line = lines[index]
        match = re.match(r"^(\s*)(?:-\s+)?run:\s*(.*?)\s*$", line)
        if not match:
            index += 1
            continue
        indent = len(match.group(1))
        value = match.group(2)
        if value not in {"|", "|+", ">", ">+", ">-", "|-"}:
            if value and not value.lstrip().startswith("#"):
                commands.append(value)
            index += 1
            continue
        folded = value.startswith(">")
        block_commands: list[str] = []
        index += 1
        while index < len(lines):
            nested = lines[index]
            if nested.strip() and len(nested) - len(nested.lstrip()) <= indent:
                break
            stripped = nested.strip()
            if stripped and not stripped.startswith("#"):
                block_commands.append(stripped)
            index += 1
        if folded and block_commands:
            commands.append(" ".join(block_commands))
        else:
            commands.extend(block_commands)
    return "\n".join(commands)


def gate_schema_violations(
    lines: list[str],
    targets: set[str],
    workflow_path: Path = CI_WORKFLOW,
    *,
    gates: dict[str, dict[str, str]] | None = None,
) -> list[str]:
    """Return fail-closed schema/CI wiring violations for every declared gate."""
    violations: list[str] = []
    gates = gates if gates is not None else gate_blocks(lines)
    if not gates:
        return ["harness.yml has no gates block"]
    jobs = ci_job_bodies(workflow_path)

    for name, fields in gates.items():
        where = f"gate '{name}'"
        unknown = set(fields) - GATE_FIELDS
        if unknown:
            violations.append(f"{where}: unknown field(s) {sorted(unknown)}")
        entrypoints = [key for key in ("make", "shell") if key in fields]
        if len(entrypoints) != 1:
            violations.append(
                f"{where}: needs exactly one of make/shell, found {entrypoints or 'none'}"
            )
        elif entrypoints[0] == "make" and fields["make"].strip("\"'") not in targets:
            violations.append(
                f"{where}: make target '{fields['make']}' does not exist in the Makefile"
            )
        triggers = _inline_list(fields.get("triggers", ""))
        if not triggers:
            violations.append(f"{where}: triggers must be a non-empty inline list")
        tier = fields.get("tier", "").strip("\"'")
        if tier not in GATE_TIERS:
            violations.append(
                f"{where}: tier must be one of {sorted(GATE_TIERS)}, found {tier or '(missing)'}"
            )
        required_on = _inline_list(fields.get("required_on", ""))
        if not required_on:
            violations.append(
                f"{where}: required_on must be a non-empty subset of {sorted(GATE_REQUIRED_ON)}"
            )
        else:
            bad = set(required_on) - GATE_REQUIRED_ON
            if bad:
                violations.append(f"{where}: required_on has invalid value(s) {sorted(bad)}")
        resource = fields.get("resource", "").strip("\"'")
        if resource not in GATE_RESOURCES:
            violations.append(
                f"{where}: resource must be one of {sorted(GATE_RESOURCES)}, found {resource or '(missing)'}"
            )
        skip = fields.get("skip", "").strip("\"'")
        if skip not in GATE_SKIP_POLICIES:
            violations.append(
                f"{where}: skip must be one of {sorted(GATE_SKIP_POLICIES)}, found {skip or '(missing)'}"
            )
        timeout = fields.get("timeout", "").strip("\"'")
        if not (timeout.isdigit() and int(timeout) > 0):
            violations.append(
                f"{where}: timeout must be a positive integer (minutes), found {timeout or '(missing)'}"
            )

        evidence = fields.get("evidence", "").strip().strip("\"'")
        if not evidence:
            violations.append(f"{where}: evidence must be explicit and non-empty")

        ci_job = fields.get("ci_job", "").strip().strip("\"'")
        if not ci_job:
            violations.append(
                f"{where}: ci_job must be explicit (use 'manual' only for non-change gates)"
            )
        elif ci_job in CI_JOB_SENTINELS:
            if "change" in required_on:
                violations.append(
                    f"{where}: change-required gate cannot use ci_job '{ci_job}'"
                )
        elif ci_job not in jobs:
            violations.append(f"{where}: ci_job '{ci_job}' is not a job in {workflow_path}")
        elif len(entrypoints) == 1 and not _job_runs_entrypoint(fields, jobs[ci_job]):
            entrypoint = fields[entrypoints[0]].strip("\"'")
            violations.append(
                f"{where}: ci_job '{ci_job}' does not execute its {entrypoints[0]} entrypoint '{entrypoint}'"
            )

        if (
            "change" in required_on
            and resource in {"offline", "hosted-service"}
            and skip == "never"
            and ci_job in ({""} | CI_JOB_SENTINELS)
        ):
            violations.append(
                f"{where}: change-required {resource} gate with skip=never needs a real CI job"
            )
    return violations


def check_gate_schema(lines: list[str], targets: set[str]) -> int:
    """Validate every gate entry against the schema documented in harness.yml."""
    gates = gate_blocks(lines)
    for violation in gate_schema_violations(lines, targets, gates=gates):
        fail(violation)
    return len(gates)


def main() -> int:
    if not HARNESS_FILE.is_file():
        print("FAIL: harness.yml not found at repository root", file=sys.stderr)
        return 1

    lines = HARNESS_FILE.read_text(encoding="utf-8").splitlines()
    targets = make_targets()

    # Structural lint
    n_docs = check_required_docs(lines)
    n_budgets = check_budgets(lines)
    n_links = check_links()
    n_files = check_workspace(lines)

    structural_errors = len(errors)
    structural_warnings = len(warnings)

    # Semantic validation
    n_commands = check_commands(lines, targets)
    n_gates = check_gate_schema(lines, targets)
    n_programs = check_programs(lines)

    print("Harness contract check")
    print("-- structural lint --")
    print(f"  required docs           : {n_docs}")
    print(f"  budgeted files          : {n_budgets}")
    print(f"  relative links resolved : {n_links}")
    print(f"  workspace files scanned : {n_files}")
    print("-- semantic validation --")
    print(f"  make targets referenced : {n_commands}")
    print(f"  gate schemas validated  : {n_gates}")
    print(f"  programs inspected      : {n_programs}")

    for message in warnings[:structural_warnings]:
        print(f"  WARN  {message}")
    for message in warnings[structural_warnings:]:
        print(f"  WARN  {message}")

    if errors:
        print("")
        for message in errors[:structural_errors]:
            print(f"  FAIL  [structural] {message}")
        for message in errors[structural_errors:]:
            print(f"  FAIL  [semantic]   {message}")
        print(f"\n{len(errors)} harness contract violation(s).")
        result = "fail"
    else:
        print("\nHarness contract OK.")
        result = "pass"

    try:
        EVIDENCE_OUT.parent.mkdir(parents=True, exist_ok=True)
        EVIDENCE_OUT.write_text(
            json.dumps(
                {
                    "gate": "harness-check",
                    "tier": "L0",
                    "result": result,
                    "structural": {
                        "required_docs": n_docs,
                        "budgeted_files": n_budgets,
                        "links_resolved": n_links,
                        "workspace_files_scanned": n_files,
                        "errors": structural_errors,
                    },
                    "semantic": {
                        "make_targets_referenced": n_commands,
                        "gate_schemas_validated": n_gates,
                        "programs_inspected": n_programs,
                        "errors": len(errors) - structural_errors,
                    },
                    "errors": errors,
                    "warnings": warnings,
                },
                ensure_ascii=False,
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
    except OSError:
        pass  # evidence writing must never mask the check result

    sys.stdout.flush()
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
