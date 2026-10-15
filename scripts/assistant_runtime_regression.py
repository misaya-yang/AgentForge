#!/usr/bin/env python3
"""Assistant Runtime Regression Gate.

Offline, deterministic, read-only regression gate that aggregates AHR-01
through AHR-04 test suites and the eval golden gate into a single
runtime regression report.

Outputs JSON and Markdown under ``reports/assistant-runtime-regression/``.

Usage::

    python scripts/assistant_runtime_regression.py gate \\
        [--output reports/assistant-runtime-regression/latest.json] \\
        [--markdown reports/assistant-runtime-regression/latest.md]
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Any

# ---------------------------------------------------------------------------
# Test group definitions (Runtime phase → Gateway/Rust contract paths)
# ---------------------------------------------------------------------------

TEST_GROUPS: list[dict[str, Any]] = [
    {
        "id": "runtime-v2-turn-contract",
        "phase": "V2",
        "label": "Runtime Thread/Turn/Item Contract",
        "runner": "pytest",
        "extra_args": [],
        "paths": [
            "tests/services/eval/test_agent_runtime_eval_contract.py",
        ],
    },
    {
        "id": "runtime-control-plane",
        "phase": "V2",
        "label": "Gateway Runtime Control Plane",
        "runner": "pytest",
        "extra_args": [],
        "paths": [
            "tests/services/agent_runtime/test_control_plane.py",
            "tests/services/agent_runtime/test_runtime_configuration.py",
        ],
    },
    {
        "id": "runtime-capability-contract",
        "phase": "V2",
        "label": "Gateway Capability Boundary",
        "runner": "pytest",
        "extra_args": [],
        "paths": [
            "tests/services/agent_runtime/test_readonly_capabilities.py",
        ],
    },
    {
        "id": "runtime-eval-contract",
        "phase": "V2",
        "label": "Gateway Eval And Observation Contract",
        "runner": "pytest",
        "extra_args": [],
        "paths": [
            "tests/services/eval/test_golden_regression_gate.py",
            "tests/services/eval/test_trace_feedback.py",
            "tests/api/test_eval_traces.py",
        ],
    },
    {
        "id": "ahr-04-golden-gate",
        "phase": "AHR-04",
        "label": "Eval Golden Regression Gate",
        "runner": "eval_golden",
        "extra_args": [],
        "paths": [
            "tests/fixtures/eval/golden/assistant_regression_v1.jsonl",
        ],
    },
]

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _run_pytest_group(group: dict[str, Any], repo_root: Path) -> dict[str, Any]:
    """Run a pytest test group and return a result dict."""
    # Build uv run command; extra_args may include --package for uv
    cmd = [
        "uv",
        "run",
        *group["extra_args"],
        "pytest",
        "-q",
        "--no-cov",
        "--tb=line",
        *group["paths"],
    ]
    started = time.monotonic()
    try:
        proc = subprocess.run(
            cmd,
            cwd=repo_root,
            capture_output=True,
            text=True,
            timeout=600,
        )
        elapsed = round(time.monotonic() - started, 2)
        passed = proc.returncode == 0
        # Parse last line for pass/fail counts
        last_line = proc.stdout.strip().splitlines()[-1] if proc.stdout.strip() else ""
        return {
            "id": group["id"],
            "phase": group["phase"],
            "label": group["label"],
            "passed": passed,
            "exit_code": proc.returncode,
            "elapsed_seconds": elapsed,
            "summary_line": last_line,
            "stdout_tail": "\n".join(proc.stdout.strip().splitlines()[-5:]),
            "stderr_tail": "\n".join(proc.stderr.strip().splitlines()[-3:]) if proc.stderr else "",
        }
    except subprocess.TimeoutExpired:
        elapsed = round(time.monotonic() - started, 2)
        return {
            "id": group["id"],
            "phase": group["phase"],
            "label": group["label"],
            "passed": False,
            "exit_code": -1,
            "elapsed_seconds": elapsed,
            "summary_line": "TIMEOUT after 600s",
            "stdout_tail": "",
            "stderr_tail": "timeout",
        }


def _run_eval_golden(group: dict[str, Any], repo_root: Path) -> dict[str, Any]:
    """Run the eval golden gate and return a result dict."""
    golden_path = group["paths"][0]
    # Step 1: validate
    validate_cmd = [
        "uv",
        "run",
        "python",
        "scripts/eval_golden.py",
        "validate",
        golden_path,
    ]
    started = time.monotonic()
    try:
        proc_v = subprocess.run(
            validate_cmd,
            cwd=repo_root,
            capture_output=True,
            text=True,
            timeout=120,
        )
        if proc_v.returncode != 0:
            elapsed = round(time.monotonic() - started, 2)
            return {
                "id": group["id"],
                "phase": group["phase"],
                "label": group["label"],
                "passed": False,
                "exit_code": proc_v.returncode,
                "elapsed_seconds": elapsed,
                "summary_line": "golden validate failed",
                "stdout_tail": proc_v.stdout.strip()[-500:],
                "stderr_tail": proc_v.stderr.strip()[-500:],
            }
        # Step 2: gate
        with TemporaryDirectory(prefix="assistant-eval-golden-") as report_dir:
            output_json = str(Path(report_dir) / "latest.json")
            output_md = str(Path(report_dir) / "latest.md")
            gate_cmd = [
                "uv",
                "run",
                "python",
                "scripts/eval_golden.py",
                "gate",
                golden_path,
                "--observations",
                "tests/fixtures/eval/observations/assistant_regression_v1.jsonl",
                "--output",
                output_json,
                "--markdown",
                output_md,
            ]
            proc_g = subprocess.run(
                gate_cmd,
                cwd=repo_root,
                capture_output=True,
                text=True,
                timeout=120,
            )
        elapsed = round(time.monotonic() - started, 2)
        passed = proc_g.returncode == 0
        # Try to parse the gate result for richer data
        gate_summary = ""
        if proc_g.stdout.strip():
            try:
                gate_data = json.loads(proc_g.stdout)
                gate_info = gate_data.get("gate", {})
                gate_summary = (
                    f"status={gate_info.get('status', '?')}, "
                    f"pass_rate={gate_info.get('metrics', {}).get('pass_rate', '?')}, "
                    f"critical_pass_rate={gate_info.get('metrics', {}).get('critical_pass_rate', '?')}, "
                    f"trajectory_pass_rate={gate_info.get('metrics', {}).get('trajectory_pass_rate', '?')}"
                )
            except (json.JSONDecodeError, KeyError):
                gate_summary = proc_g.stdout.strip()[-200:]
        return {
            "id": group["id"],
            "phase": group["phase"],
            "label": group["label"],
            "passed": passed,
            "exit_code": proc_g.returncode,
            "elapsed_seconds": elapsed,
            "summary_line": gate_summary or ("passed" if passed else "failed"),
            "stdout_tail": proc_g.stdout.strip()[-500:],
            "stderr_tail": proc_g.stderr.strip()[-500:] if proc_g.stderr else "",
        }
    except subprocess.TimeoutExpired:
        elapsed = round(time.monotonic() - started, 2)
        return {
            "id": group["id"],
            "phase": group["phase"],
            "label": group["label"],
            "passed": False,
            "exit_code": -1,
            "elapsed_seconds": elapsed,
            "summary_line": "TIMEOUT",
            "stdout_tail": "",
            "stderr_tail": "timeout",
        }


# ---------------------------------------------------------------------------
# Gate logic
# ---------------------------------------------------------------------------


def run_gate(repo_root: Path) -> dict[str, Any]:
    """Execute all test groups and return the aggregated result."""
    results: list[dict[str, Any]] = []
    for group in TEST_GROUPS:
        if group["runner"] == "eval_golden":
            result = _run_eval_golden(group, repo_root)
        else:
            result = _run_pytest_group(group, repo_root)
        results.append(result)

    all_passed = all(r["passed"] for r in results)
    total_elapsed = round(sum(r["elapsed_seconds"] for r in results), 2)
    passed_count = sum(1 for r in results if r["passed"])

    # Build phases summary
    phases: dict[str, bool] = {}
    for r in results:
        phase = r["phase"]
        phases[phase] = phases.get(phase, True) and r["passed"]

    return {
        "schema_version": "assistant-runtime-regression-gate/v1",
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "status": "pass" if all_passed else "fail",
        "groups": [
            {
                "id": r["id"],
                "phase": r["phase"],
                "label": r["label"],
                "passed": r["passed"],
                "exit_code": r["exit_code"],
                "elapsed_seconds": r["elapsed_seconds"],
                "summary_line": r["summary_line"],
            }
            for r in results
        ],
        "phases": phases,
        "summary": {
            "total_groups": len(results),
            "passed": passed_count,
            "failed": len(results) - passed_count,
            "total_elapsed_seconds": total_elapsed,
        },
        "no_go_thresholds": {
            "all_groups_must_pass": True,
            "critical_phases": ["AHR-01", "AHR-02", "AHR-03", "AHR-04"],
        },
    }


def write_reports(gate_result: dict[str, Any], json_path: Path, md_path: Path) -> None:
    """Write JSON and Markdown reports."""
    json_path.parent.mkdir(parents=True, exist_ok=True)
    md_path.parent.mkdir(parents=True, exist_ok=True)

    # JSON
    json_path.write_text(json.dumps(gate_result, indent=2, sort_keys=True) + "\n")

    # Markdown
    status_emoji = "PASS" if gate_result["status"] == "pass" else "FAIL"
    lines = [
        f"# Assistant Runtime Regression Gate — {status_emoji}",
        "",
        f"**Status:** {gate_result['status']}",
        f"**Timestamp:** {gate_result['timestamp']}",
        f"**Total elapsed:** {gate_result['summary']['total_elapsed_seconds']}s",
        f"**Groups:** {gate_result['summary']['passed']}/{gate_result['summary']['total_groups']} passed",
        "",
        "## Phase Summary",
        "",
        "| Phase | Status |",
        "| --- | --- |",
    ]
    for phase, passed in sorted(gate_result["phases"].items()):
        lines.append(f"| {phase} | {'pass' if passed else 'FAIL'} |")

    lines.extend([
        "",
        "## Group Details",
        "",
        "| Group | Phase | Passed | Elapsed | Summary |",
        "| --- | --- | --- | --- | --- |",
    ])
    for g in gate_result["groups"]:
        passed_str = "pass" if g["passed"] else "FAIL"
        lines.append(
            f"| {g['id']} | {g['phase']} | {passed_str} | {g['elapsed_seconds']}s | {g['summary_line']} |"
        )

    lines.extend([
        "",
        "## No-Go Thresholds",
        "",
        "- All groups must pass; any failure is a no-go.",
        "- Critical phases: AHR-01, AHR-02, AHR-03, AHR-04.",
        "- No production data, secrets, or deployment involved.",
        "",
        "## Waiver Policy",
        "",
        "A failed group may only be waived when:",
        "1. The user explicitly waives the specific group.",
        "2. The failure root cause is documented in this report.",
        "3. Remaining evidence still proves the affected feature-oracle item.",
    ])
    md_path.write_text("\n".join(lines) + "\n")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Assistant Runtime Regression Gate — offline, deterministic, read-only."
    )
    sub = parser.add_subparsers(dest="command", required=True)

    gate = sub.add_parser("gate", help="Run the full regression gate.")
    gate.add_argument(
        "--output",
        default="reports/assistant-runtime-regression/latest.json",
        help="JSON report output path.",
    )
    gate.add_argument(
        "--markdown",
        default="reports/assistant-runtime-regression/latest.md",
        help="Markdown report output path.",
    )
    gate.add_argument(
        "--no-write",
        action="store_true",
        help="Print the gate result without writing JSON or Markdown reports.",
    )
    gate.set_defaults(func=cmd_gate)

    summary = sub.add_parser("summary", help="Print gate configuration summary.")
    summary.set_defaults(func=cmd_summary)

    return parser


def cmd_gate(args: argparse.Namespace) -> int:
    repo_root = Path.cwd()
    gate_result = run_gate(repo_root)
    if not args.no_write:
        write_reports(gate_result, Path(args.output), Path(args.markdown))
    print(json.dumps(gate_result, indent=2, sort_keys=True))
    return 0 if gate_result["status"] == "pass" else 1


def cmd_summary(args: argparse.Namespace) -> int:
    print(json.dumps({
        "schema_version": "assistant-runtime-regression-gate/v1",
        "groups": [
            {"id": g["id"], "phase": g["phase"], "label": g["label"], "paths": g["paths"]}
            for g in TEST_GROUPS
        ],
        "total_groups": len(TEST_GROUPS),
    }, indent=2, sort_keys=True))
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        return args.func(args)
    except Exception as exc:  # noqa: BLE001
        print(f"assistant_runtime_regression failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
