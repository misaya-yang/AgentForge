#!/usr/bin/env python3
"""Fail-closed validation of diff-selected CI gate results.

``affected_gates.py`` proves that every changed path matches at least one gate.
This final CI check closes the second half of that contract:

* every changed path must be covered by at least one change-required gate that
  names a CI job;
* every selected CI-wired gate must have a result in the final job's ``needs``
  context; and
* every upstream job supplied to the final check, selected or not, must succeed;
  ``skipped``, ``cancelled``, missing, and failed results all fail closed.

The selector itself runs in the final ``gate-enforcement`` job.  That one local
result may satisfy only the ``affected_gates`` gate; every other result must
come from an upstream CI job.

Usage:
  CI_JOB_RESULTS_JSON='{"frontend":{"result":"success"},...}' \
    python3 scripts/harness/ci_gate_enforcement.py
  python3 scripts/harness/ci_gate_enforcement.py --selftest

Evidence: tmp/gate-evidence/ci-gate-enforcement.json
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_HARNESS = ROOT / "harness.yml"
DEFAULT_SELECTION = ROOT / "tmp" / "gate-evidence" / "affected-gates.json"
DEFAULT_EVIDENCE = ROOT / "tmp" / "gate-evidence" / "ci-gate-enforcement.json"
DEFAULT_CURRENT_JOB = "gate-enforcement"
LOCAL_GATE = "affected_gates"
CI_WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"


def _workflow_job_bodies(path: Path) -> dict[str, str]:
    """Return top-level CI job bodies without requiring a YAML dependency."""
    lines = path.read_text(encoding="utf-8").splitlines()
    bodies: dict[str, list[str]] = {}
    current: str | None = None
    in_jobs = False
    for line in lines:
        if line == "jobs:":
            in_jobs = True
            continue
        if not in_jobs:
            continue
        match = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if match:
            current = match.group(1)
            bodies[current] = []
            continue
        if current is not None:
            bodies[current].append(line)
    return {name: "\n".join(body) for name, body in bodies.items()}


def _workflow_run_scripts(body: str) -> str:
    """Extract only executable ``run:`` values from one workflow job body."""
    lines = body.splitlines()
    commands: list[str] = []
    index = 0
    while index < len(lines):
        line = lines[index]
        match = re.match(r"^(\s*)(?:-\s+)?run:\s*(.*?)\s*$", line)
        if match is None:
            index += 1
            continue
        indent = len(match.group(1))
        value = match.group(2)
        if value and value not in {"|", "|-", "|+", ">", ">-", ">+"}:
            if not value.lstrip().startswith("#"):
                commands.append(value)
            index += 1
            continue
        folded = value.startswith(">")
        block_commands: list[str] = []
        index += 1
        while index < len(lines):
            candidate = lines[index]
            if candidate.strip() and len(candidate) - len(candidate.lstrip()) <= indent:
                break
            stripped = candidate.strip()
            if stripped and not stripped.startswith("#"):
                block_commands.append(stripped)
            index += 1
        if folded and block_commands:
            commands.append(" ".join(block_commands))
        else:
            commands.extend(block_commands)
    return "\n".join(commands)


def _workflow_runs_entrypoint(fields: dict[str, str], body: str) -> bool:
    scripts = _workflow_run_scripts(body)
    if "make" in fields:
        target = _clean_scalar(fields.get("make"))
        return _run_text_executes_make_target(scripts, target)
    shell = _clean_scalar(fields.get("shell"))
    commands = [command.strip() for command in shell.split("&&") if command.strip()]
    return bool(commands) and all(
        _run_text_executes_shell_command(scripts, command) for command in commands
    )


def _run_text_executes_make_target(run_text: str, target: str) -> bool:
    """Recognize a real, outcome-preserving Make invocation."""

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


def _checkout_fetches_full_history(body: str) -> bool:
    """Require ``fetch-depth: 0`` on an actual checkout step."""

    lines = body.splitlines()
    for index, line in enumerate(lines):
        match = re.match(r"^(\s*)-\s+uses:\s*actions/checkout@\S+\s*$", line)
        if match is None:
            continue
        step_indent = len(match.group(1))
        with_indent: int | None = None
        index += 1
        while index < len(lines):
            candidate = lines[index]
            if re.match(rf"^\s{{{step_indent}}}-\s+", candidate):
                break
            candidate_indent = len(candidate) - len(candidate.lstrip())
            if re.match(r"^\s+with:\s*$", candidate):
                with_indent = candidate_indent
            elif (
                with_indent is not None
                and candidate.strip()
                and candidate_indent <= with_indent
            ):
                with_indent = None
            if with_indent is not None and re.match(
                r"^\s+fetch-depth:\s*0\s*$", candidate
            ):
                return True
            index += 1
    return False


def validate_workflow_wiring(
    path: Path = CI_WORKFLOW,
    gates: dict[str, dict[str, str]] | None = None,
) -> list[str]:
    """Prevent a named green job from silently dropping a declared gate."""
    bodies = _workflow_job_bodies(path)
    gates = gates or _load_gate_contract(DEFAULT_HARNESS)
    failures: list[str] = []

    wired_jobs: set[str] = set()
    for gate_name, fields in sorted(gates.items()):
        ci_job = _clean_scalar(fields.get("ci_job"))
        if not ci_job:
            failures.append(f"gate '{gate_name}' has no explicit ci_job")
            continue
        if ci_job == "manual":
            continue
        wired_jobs.add(ci_job)
        body = bodies.get(ci_job)
        if body is None:
            failures.append(f"CI workflow is missing gate '{gate_name}' job '{ci_job}'")
        elif not _workflow_runs_entrypoint(fields, body):
            entrypoint = _clean_scalar(fields.get("make") or fields.get("shell"))
            failures.append(
                f"CI job '{ci_job}' does not execute gate '{gate_name}' entrypoint: {entrypoint}"
            )

    enforcement_body = bodies.get("gate-enforcement", "")
    for job in sorted(wired_jobs - {"gate-enforcement"}):
        if re.search(rf"^\s*-\s+{re.escape(job)}\s*$", enforcement_body, re.MULTILINE) is None:
            failures.append(
                f"gate-enforcement does not require CI job '{job}', so its result is unavailable"
            )

    if not _checkout_fetches_full_history(bodies.get("architecture-gates", "")):
        failures.append(
            "CI job 'architecture-gates' checkout must set fetch-depth: 0"
        )

    fixed_snippets = {
        "compose-and-harness": ["make ci-gate-enforcement-selftest"],
        "architecture-gates": ['exit "$failed"'],
        "agent-runtime-contracts": ['exit "$failed"'],
        "gate-enforcement": ["if: always()", "make ci-gate-enforcement"],
        "release-ready": ["- gate-enforcement"],
        "rust-changed-crate": ['CARGO_BUILD_JOBS: "1"'],
    }
    for job, snippets in fixed_snippets.items():
        body = bodies.get(job, "")
        for snippet in snippets:
            if snippet not in body:
                failures.append(f"CI job '{job}' is missing required wiring: {snippet}")
    return failures


def _load_gate_contract(path: Path) -> dict[str, dict[str, str]]:
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import check_harness  # noqa: PLC0415

    lines = path.read_text(encoding="utf-8").splitlines()
    return check_harness.gate_blocks(lines)


def _inline_list(raw: str) -> list[str]:
    raw = raw.strip()
    if not (raw.startswith("[") and raw.endswith("]")):
        return []
    return [
        item.strip().strip("\"'")
        for item in raw[1:-1].split(",")
        if item.strip()
    ]


def _clean_scalar(raw: str | None) -> str:
    return (raw or "").strip().strip("\"'")


def normalize_job_results(payload: Any) -> tuple[dict[str, str], list[str]]:
    """Accept GitHub's ``toJSON(needs)`` shape or a simple job->result map."""
    if not isinstance(payload, dict):
        return {}, ["CI job results must be a JSON object"]

    results: dict[str, str] = {}
    failures: list[str] = []
    for job, raw in payload.items():
        if not isinstance(job, str) or not job:
            failures.append(f"invalid CI job key: {job!r}")
            continue
        if isinstance(raw, str):
            result = raw
        elif isinstance(raw, dict) and isinstance(raw.get("result"), str):
            result = raw["result"]
        else:
            failures.append(f"CI job '{job}' has no string result")
            continue
        results[job] = result
    return results, failures


def evaluate(
    selection: dict[str, Any],
    gates: dict[str, dict[str, str]],
    job_results: dict[str, str],
    *,
    current_job: str = DEFAULT_CURRENT_JOB,
    require_all_jobs: bool = False,
) -> dict[str, Any]:
    failures: list[str] = []

    if require_all_jobs:
        for job, result in sorted(job_results.items()):
            if result != "success":
                failures.append(
                    f"upstream CI job '{job}' is {result!r}, expected 'success'"
                )

    if selection.get("result") != "pass":
        failures.append(
            f"affected-gates selector result is {selection.get('result')!r}, expected 'pass'"
        )
    ungated = selection.get("ungated_paths")
    if not isinstance(ungated, list):
        failures.append("affected-gates evidence has no ungated_paths list")
        ungated = []
    elif ungated:
        failures.append(f"affected-gates reported ungated paths: {ungated}")

    changed_paths = selection.get("changed_paths")
    if not isinstance(changed_paths, list) or not all(
        isinstance(path, str) and path for path in changed_paths
    ):
        failures.append("affected-gates evidence has no valid changed_paths list")
        changed_paths = []

    selected = selection.get("selected")
    if not isinstance(selected, dict):
        failures.append("affected-gates evidence has no selected gate map")
        selected = {}

    matched_paths: set[str] = set()
    ci_covered_paths: set[str] = set()
    required_jobs: dict[str, list[str]] = {}

    for gate_name, selected_info in sorted(selected.items()):
        if not isinstance(gate_name, str) or gate_name not in gates:
            failures.append(f"selector returned unknown gate {gate_name!r}")
            continue
        if not isinstance(selected_info, dict):
            failures.append(f"selector gate '{gate_name}' has invalid detail")
            continue

        paths = selected_info.get("matched_paths")
        if not isinstance(paths, list) or not all(isinstance(path, str) for path in paths):
            failures.append(f"selector gate '{gate_name}' has no valid matched_paths list")
            continue
        matched_paths.update(paths)

        contract = gates[gate_name]
        required_on = _inline_list(contract.get("required_on", ""))
        ci_job = _clean_scalar(contract.get("ci_job"))
        evidence = _clean_scalar(contract.get("evidence"))
        if not evidence:
            failures.append(f"selected gate '{gate_name}' has no explicit evidence")
        if not ci_job:
            failures.append(f"selected gate '{gate_name}' has no explicit ci_job")
        elif ci_job == "manual" and "change" in required_on:
            failures.append(
                f"selected change-required gate '{gate_name}' cannot use ci_job 'manual'"
            )
        selected_ci_job = _clean_scalar(selected_info.get("ci_job"))
        if selected_ci_job != ci_job:
            failures.append(
                f"selector gate '{gate_name}' ci_job drifted: "
                f"selection={selected_ci_job or '(none)'}, harness={ci_job or '(none)'}"
            )

        if "change" not in required_on or not ci_job or ci_job == "manual":
            continue
        ci_covered_paths.update(paths)
        required_jobs.setdefault(ci_job, []).append(gate_name)

    changed_set = set(changed_paths)
    for path in sorted(changed_set - matched_paths):
        failures.append(f"changed path is absent from selected gate matches: {path}")
    for path in sorted(changed_set - ci_covered_paths):
        failures.append(
            f"changed path has no change-required CI-wired gate: {path}"
        )

    required_results: dict[str, dict[str, Any]] = {}
    for job, gate_names in sorted(required_jobs.items()):
        gate_names = sorted(gate_names)
        if job == current_job:
            unexpected = [name for name in gate_names if name != LOCAL_GATE]
            if unexpected:
                failures.append(
                    f"current final job may satisfy only '{LOCAL_GATE}', not {unexpected}"
                )
                result = "invalid-local-mapping"
            elif gate_names == [LOCAL_GATE]:
                result = "success"
            else:
                failures.append(
                    f"current final job '{current_job}' has an invalid local gate set {gate_names}"
                )
                result = "invalid-local-mapping"
        else:
            result = job_results.get(job, "missing")
            if result != "success" and not require_all_jobs:
                failures.append(
                    f"required CI job '{job}' for gates {gate_names} is {result!r}, expected 'success'"
                )
            elif result == "missing":
                failures.append(
                    f"required CI job '{job}' for gates {gate_names} has no result"
                )
        required_results[job] = {"gates": gate_names, "result": result}

    return {
        "gate": "ci-gate-enforcement",
        "tier": "L0",
        "changed_paths": changed_paths,
        "required_results": required_results,
        "failures": failures,
        "result": "pass" if not failures else "fail",
    }


def _selftest() -> int:
    gates = {
        "affected_gates": {
            "required_on": "[change, release]",
            "ci_job": "gate-enforcement",
            "evidence": "tmp/gate-evidence/affected-gates.json",
        },
        "frontend": {
            "required_on": "[change, release]",
            "ci_job": "frontend",
            "evidence": "tmp/gate-evidence/frontend.log",
        },
        "manual_live": {
            "required_on": "[manual]",
            "ci_job": "manual",
            "evidence": "tmp/gate-evidence/manual-live.log",
        },
    }
    base_selection = {
        "result": "pass",
        "changed_paths": ["harness.yml", "web/src/App.tsx"],
        "ungated_paths": [],
        "selected": {
            "affected_gates": {
                "ci_job": "gate-enforcement",
                "matched_paths": ["harness.yml"],
            },
            "frontend": {
                "ci_job": "frontend",
                "matched_paths": ["web/src/App.tsx"],
            },
        },
    }

    cases: list[tuple[str, dict[str, Any], dict[str, str], bool]] = [
        ("all required results succeed", base_selection, {"frontend": "success"}, True),
        ("missing result fails closed", base_selection, {}, False),
        ("skipped result fails closed", base_selection, {"frontend": "skipped"}, False),
    ]

    manual_only = json.loads(json.dumps(base_selection))
    manual_only["changed_paths"] = ["web/src/App.tsx"]
    manual_only["selected"] = {
        "manual_live": {"ci_job": "manual", "matched_paths": ["web/src/App.tsx"]}
    }
    cases.append(("manual-only coverage is not CI coverage", manual_only, {}, False))

    unknown = json.loads(json.dumps(base_selection))
    unknown["selected"]["ghost"] = {"ci_job": "ghost", "matched_paths": []}
    cases.append(("unknown selected gate fails", unknown, {"frontend": "success"}, False))

    failures = 0
    for name, selection, results, expected_pass in cases:
        outcome = evaluate(selection, gates, results)
        passed = outcome["result"] == "pass"
        ok = passed == expected_pass
        print(f"[{'ok' if ok else 'FAIL'}] {name}")
        if not ok:
            failures += 1
            print(f"       {outcome['failures']}")
    wiring_failures = validate_workflow_wiring()
    if wiring_failures:
        failures += 1
        print("[FAIL] CI workflow wiring")
        for failure in wiring_failures:
            print(f"       {failure}")
    else:
        print("[ok] CI workflow jobs still execute every declared ARC-00B gate")
    if failures:
        print(f"SELFTEST FAILED: {failures} case(s)", file=sys.stderr)
        return 1
    print(f"SELFTEST OK: {len(cases)} required-result cases classified correctly")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--harness", type=Path, default=DEFAULT_HARNESS)
    parser.add_argument("--selection", type=Path, default=DEFAULT_SELECTION)
    parser.add_argument("--evidence-out", type=Path, default=DEFAULT_EVIDENCE)
    parser.add_argument("--job-results-json")
    parser.add_argument("--current-job", default=DEFAULT_CURRENT_JOB)
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args(argv)

    if args.selftest:
        return _selftest()

    try:
        gates = _load_gate_contract(args.harness)
        selection = json.loads(args.selection.read_text(encoding="utf-8"))
        raw_results = args.job_results_json or os.environ.get("CI_JOB_RESULTS_JSON", "")
        if not raw_results:
            raise ValueError("CI_JOB_RESULTS_JSON is required")
        payload = json.loads(raw_results)
        job_results, result_errors = normalize_job_results(payload)
        outcome = evaluate(
            selection,
            gates,
            job_results,
            current_job=args.current_job,
            require_all_jobs=True,
        )
        outcome["failures"] = validate_workflow_wiring(gates=gates) + outcome["failures"]
        outcome["failures"] = result_errors + outcome["failures"]
        outcome["result"] = "pass" if not outcome["failures"] else "fail"
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        outcome = {
            "gate": "ci-gate-enforcement",
            "tier": "L0",
            "changed_paths": [],
            "required_results": {},
            "failures": [f"gate enforcement input error: {exc}"],
            "result": "fail",
        }

    args.evidence_out.parent.mkdir(parents=True, exist_ok=True)
    args.evidence_out.write_text(
        json.dumps(outcome, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )

    print(
        f"changed paths: {len(outcome['changed_paths'])}; "
        f"required CI results: {len(outcome['required_results'])}"
    )
    for job, detail in outcome["required_results"].items():
        print(f"  {job}: {detail['result']} ({', '.join(detail['gates'])})")
    if outcome["failures"]:
        print(f"CI GATE ENFORCEMENT FAILED: {len(outcome['failures'])} issue(s)", file=sys.stderr)
        for failure in outcome["failures"]:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print(f"OK: every selected CI-required gate has a successful result (evidence: {args.evidence_out.relative_to(ROOT)})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
