#!/usr/bin/env python3
"""Validate ARC-08 release evidence, rollback coverage, and plan retirement.

The checked-in matrix is deliberately allowed to remain ``NOT_RUN`` while the
candidate is being assembled.  Candidate validation is stricter: every named
scenario must be a real ``PASS`` with durable evidence bound to one release and
source commit.  Structural validation never upgrades an execution result.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path, PurePosixPath
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MATRIX = ROOT / "deploy/release/release-rollback-matrix.json"
DEFAULT_RETIREMENT = ROOT / "deploy/release/historical-plan-retirement.json"
MATRIX_SCHEMA = "ai-platform/release-rollback-matrix/v1"
RETIREMENT_SCHEMA = "ai-platform/historical-plan-retirement/v1"
RECEIPT_SCHEMA = "ai-platform/integration-gate-receipt/v1"
RESULTS = {"NOT_RUN", "BLOCKED", "FAIL", "PASS"}
HEX_40 = re.compile(r"^[0-9a-f]{40}$")
HEX_64 = re.compile(r"^[0-9a-f]{64}$")

# This is the PRD's finite release surface.  Adding a new scenario is an
# explicit schema change; silently omitting one can never produce a candidate.
REQUIRED_SCENARIOS: dict[str, tuple[str, str]] = {
    "offline-release-suite": ("L1", "release"),
    "database-fresh-upgrade-role-matrix": ("L2", "release"),
    "multi-arch-artifact-and-version-agreement": ("L3", "release"),
    "docker-source-image-identity": ("L3", "release"),
    "assistant-live-journeys": ("L3", "release"),
    "knowledge-live-journeys": ("L3", "release"),
    "degraded-failure-recovery": ("L3", "release"),
    "compact-scale-topology": ("L3", "release"),
    "fresh-environment-quickstart": ("L3", "release"),
    "current-frozen-current": ("L3", "rollback"),
    "database-recovery-classes": ("L3", "rollback"),
    "final-review-and-closeout": ("L3", "release"),
}


class ReleaseEvidenceError(RuntimeError):
    """Release evidence is malformed, incomplete, or overclaims execution."""


def _load(path: Path, label: str) -> dict[str, Any]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ReleaseEvidenceError(f"{label} unreadable: {path}: {exc}") from exc
    if not isinstance(payload, dict):
        raise ReleaseEvidenceError(f"{label} must be a JSON object")
    return payload


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    try:
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError as exc:
        raise ReleaseEvidenceError(f"cannot hash release artifact: {path}: {exc}") from exc
    return digest.hexdigest()


def _safe_repo_path(raw: Any, label: str) -> str:
    if not isinstance(raw, str) or not raw or "\\" in raw:
        raise ReleaseEvidenceError(f"{label} must be one repository POSIX path")
    path = PurePosixPath(raw)
    if path.is_absolute() or ".." in path.parts or path.as_posix() != raw:
        raise ReleaseEvidenceError(f"{label} escapes the repository: {raw!r}")
    return raw


def _required_text(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ReleaseEvidenceError(f"{label} must be non-empty text")
    return value


def _commands(value: Any, scenario_id: str) -> list[list[str]]:
    if not isinstance(value, list) or not value:
        raise ReleaseEvidenceError(f"release scenario has no commands: {scenario_id}")
    for command in value:
        if (
            not isinstance(command, list)
            or not command
            or not all(isinstance(part, str) and part for part in command)
        ):
            raise ReleaseEvidenceError(f"release scenario command is invalid: {scenario_id}")
    return value


def _make_targets(root: Path) -> set[str]:
    makefile = root / "Makefile"
    try:
        text = makefile.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        raise ReleaseEvidenceError(f"canonical Makefile is unreadable: {makefile}: {exc}") from exc
    return set(re.findall(r"^([A-Za-z0-9_.\-]+):", text, re.MULTILINE))


def _validate_command_entrypoint(
    root: Path,
    command: list[str],
    scenario_id: str,
    make_targets: set[str],
) -> None:
    executable = command[0]
    if executable == "make":
        if len(command) < 2 or command[1] not in make_targets:
            target = command[1] if len(command) > 1 else "(missing)"
            raise ReleaseEvidenceError(
                f"release scenario references unknown Make target: {scenario_id}:{target}"
            )
        return
    if executable in {"python", "python3"} and len(command) >= 3 and command[1] == "-m":
        module = command[2]
        module_path = root.joinpath(*module.split("."))
        if (
            not module_path.with_suffix(".py").is_file()
            and not (module_path / "__main__.py").is_file()
        ):
            raise ReleaseEvidenceError(
                f"release scenario Python entrypoint is missing: {scenario_id}:{module}"
            )
        return
    if executable == "pnpm" and len(command) >= 6 and command[1] == "-C":
        project = root / command[2]
        if not (project / "package.json").is_file() or command[3:6] != [
            "exec",
            "playwright",
            "test",
        ]:
            raise ReleaseEvidenceError(
                f"release scenario pnpm entrypoint is missing: {scenario_id}"
            )
        config_index = command.index("--config") if "--config" in command else -1
        if config_index < 0 or config_index + 1 >= len(command):
            raise ReleaseEvidenceError(
                f"release scenario Playwright config is missing: {scenario_id}"
            )
        referenced = [
            command[config_index + 1],
            *[part for part in command if part.endswith(".spec.ts")],
        ]
        missing = [part for part in referenced if not (project / part).is_file()]
        if missing:
            raise ReleaseEvidenceError(
                f"release scenario Playwright entrypoint is missing: {scenario_id}:{missing}"
            )
        return
    raise ReleaseEvidenceError(
        f"release scenario executable entrypoint is unsupported: {scenario_id}:{executable}"
    )


def validate_integration_receipt(
    receipt: dict[str, Any],
    *,
    expected_gate: str | None = None,
    release_id: str | None = None,
    source_git_sha: str | None = None,
    require_pass: bool,
) -> dict[str, Any]:
    """Validate one executable receipt without turning a dry run into a pass."""
    if receipt.get("schema_version") != RECEIPT_SCHEMA:
        raise ReleaseEvidenceError("unsupported integration receipt schema")
    gate = _required_text(receipt.get("gate"), "receipt gate")
    if expected_gate is not None and gate != expected_gate:
        raise ReleaseEvidenceError(f"integration receipt gate mismatch: {gate}")
    result = receipt.get("result")
    if result not in {"blocked", "dry-run", "fail", "pass"}:
        raise ReleaseEvidenceError(f"invalid integration receipt result: {result!r}")
    if release_id is not None and receipt.get("release_id") != release_id:
        raise ReleaseEvidenceError(f"integration receipt release identity mismatch: {gate}")
    if source_git_sha is not None and receipt.get("source_git_sha") != source_git_sha:
        raise ReleaseEvidenceError(f"integration receipt source identity mismatch: {gate}")
    unexpected = receipt.get("unexpected_skips")
    if not isinstance(unexpected, int) or unexpected < 0:
        raise ReleaseEvidenceError(f"integration receipt skip count is invalid: {gate}")
    steps = receipt.get("steps")
    if not isinstance(steps, list):
        raise ReleaseEvidenceError(f"integration receipt steps are invalid: {gate}")
    if require_pass and (result != "pass" or unexpected != 0 or not steps):
        raise ReleaseEvidenceError(f"integration receipt is not a zero-skip pass: {gate}")
    for step in steps:
        if (
            not isinstance(step, dict)
            or not isinstance(step.get("command"), list)
            or not step["command"]
            or not isinstance(step.get("exit_code"), int)
            or not isinstance(step.get("skip_markers"), int)
            or step["skip_markers"] < 0
            or HEX_64.fullmatch(str(step.get("output_sha256"))) is None
        ):
            raise ReleaseEvidenceError(f"integration receipt step is invalid: {gate}")
        if require_pass and (step["exit_code"] != 0 or step["skip_markers"] != 0):
            raise ReleaseEvidenceError(f"integration receipt step did not pass: {gate}")
    return {"gate": gate, "result": result, "steps": len(steps), "unexpected_skips": unexpected}


def _derived_matrix_status(statuses: list[str]) -> str:
    if "FAIL" in statuses:
        return "FAIL"
    if "BLOCKED" in statuses:
        return "BLOCKED"
    if "NOT_RUN" in statuses:
        return "NOT_RUN"
    return "PASS"


def _validate_scenario_receipts(
    root: Path,
    *,
    scenario_id: str,
    evidence_paths: list[str],
    commands: list[list[str]],
    release_id: str,
    source_git_sha: str,
) -> None:
    """Require executable, candidate-bound receipts for every PASS command."""

    executed: set[tuple[str, ...]] = set()
    for rel in evidence_paths:
        receipt = _load(root / rel, f"release scenario receipt {scenario_id}")
        validate_integration_receipt(
            receipt,
            release_id=release_id,
            source_git_sha=source_git_sha,
            require_pass=True,
        )
        executed.update(tuple(step["command"]) for step in receipt["steps"])

    missing = [command for command in commands if tuple(command) not in executed]
    if missing:
        raise ReleaseEvidenceError(
            f"PASS scenario receipt omits declared commands: {scenario_id}:{missing}"
        )


def validate_release_matrix(
    root: Path,
    matrix: dict[str, Any],
    *,
    level: str,
    release_id: str | None = None,
    source_git_sha: str | None = None,
) -> dict[str, Any]:
    if level not in {"draft", "candidate"}:
        raise ReleaseEvidenceError(f"unknown release evidence level: {level}")
    if matrix.get("schema_version") != MATRIX_SCHEMA:
        raise ReleaseEvidenceError("unsupported release/rollback matrix schema")
    status = matrix.get("status")
    if status not in RESULTS:
        raise ReleaseEvidenceError(f"invalid release/rollback matrix status: {status!r}")
    scenarios = matrix.get("scenarios")
    if not isinstance(scenarios, list):
        raise ReleaseEvidenceError("release/rollback matrix scenarios must be a list")
    matrix_release = matrix.get("release_id")
    matrix_source = matrix.get("source_git_sha")
    make_targets = _make_targets(root)

    seen: set[str] = set()
    scenario_statuses: list[str] = []
    passed = 0
    for scenario in scenarios:
        if not isinstance(scenario, dict):
            raise ReleaseEvidenceError("release/rollback scenario must be an object")
        scenario_id = _required_text(scenario.get("id"), "release scenario id")
        if scenario_id in seen:
            raise ReleaseEvidenceError(f"duplicate release scenario: {scenario_id}")
        seen.add(scenario_id)
        expected = REQUIRED_SCENARIOS.get(scenario_id)
        if expected is None:
            raise ReleaseEvidenceError(f"unknown release scenario: {scenario_id}")
        if (scenario.get("tier"), scenario.get("category")) != expected:
            raise ReleaseEvidenceError(f"release scenario tier/category drift: {scenario_id}")
        if scenario.get("required") is not True:
            raise ReleaseEvidenceError(f"release scenario is not required: {scenario_id}")
        commands = _commands(scenario.get("commands"), scenario_id)
        for command in commands:
            _validate_command_entrypoint(root, command, scenario_id, make_targets)
        scenario_status = scenario.get("status")
        if scenario_status not in RESULTS:
            raise ReleaseEvidenceError(f"invalid release scenario status: {scenario_id}")
        scenario_statuses.append(scenario_status)
        evidence = scenario.get("evidence")
        if not isinstance(evidence, list):
            raise ReleaseEvidenceError(f"release scenario evidence must be a list: {scenario_id}")
        evidence_paths: list[str] = []
        for raw in evidence:
            rel = _safe_repo_path(raw, f"release scenario evidence {scenario_id}")
            if not rel.startswith("reports/"):
                raise ReleaseEvidenceError(
                    f"release candidate evidence is not durable: {scenario_id}:{rel}"
                )
            evidence_paths.append(rel)
        blocker = scenario.get("blocker")
        if scenario_status in {"NOT_RUN", "BLOCKED"}:
            _required_text(blocker, f"{scenario_id} blocker")
            if evidence_paths:
                raise ReleaseEvidenceError(
                    f"unexecuted/blocked scenario must not carry pass evidence: {scenario_id}"
                )
        elif blocker not in (None, ""):
            raise ReleaseEvidenceError(f"executed scenario retains a blocker: {scenario_id}")
        if scenario_status == "PASS":
            if not evidence_paths:
                raise ReleaseEvidenceError(f"PASS scenario has no durable evidence: {scenario_id}")
            if not isinstance(matrix_release, str) or not matrix_release.strip():
                raise ReleaseEvidenceError("PASS release matrix has no release_id")
            if HEX_40.fullmatch(str(matrix_source)) is None:
                raise ReleaseEvidenceError("PASS release matrix has no exact source Git SHA")
            for rel in evidence_paths:
                path = root / rel
                if path.is_symlink() or not path.is_file():
                    raise ReleaseEvidenceError(
                        f"PASS scenario evidence is missing/symlinked: {scenario_id}:{rel}"
                    )
            _validate_scenario_receipts(
                root,
                scenario_id=scenario_id,
                evidence_paths=evidence_paths,
                commands=commands,
                release_id=matrix_release,
                source_git_sha=matrix_source,
            )
            passed += 1
        elif scenario_status == "FAIL" and not evidence_paths:
            raise ReleaseEvidenceError(f"FAIL scenario has no failure evidence: {scenario_id}")

    missing = sorted(set(REQUIRED_SCENARIOS) - seen)
    if missing or len(seen) != len(REQUIRED_SCENARIOS):
        raise ReleaseEvidenceError(f"release/rollback scenario set drift: missing={missing}")
    derived = _derived_matrix_status(scenario_statuses)
    if status != derived:
        raise ReleaseEvidenceError(
            f"release/rollback matrix aggregate drift: declared={status} derived={derived}"
        )

    if level == "candidate" and status != "PASS":
        raise ReleaseEvidenceError(f"release candidate is blocked by matrix status {status}")
    if status == "PASS" or level == "candidate":
        if not isinstance(matrix_release, str) or not matrix_release.strip():
            raise ReleaseEvidenceError("candidate release matrix has no release_id")
        if HEX_40.fullmatch(str(matrix_source)) is None:
            raise ReleaseEvidenceError("candidate release matrix has no exact source Git SHA")
    if release_id is not None and matrix_release != release_id:
        raise ReleaseEvidenceError("release matrix/compatibility release_id drift")
    if source_git_sha is not None and matrix_source != source_git_sha:
        raise ReleaseEvidenceError("release matrix/compatibility source Git SHA drift")
    return {
        "status": status,
        "scenarios": len(scenarios),
        "passed": passed,
        "not_passed": len(scenarios) - passed,
    }


def validate_retirement_manifest(root: Path, manifest: dict[str, Any]) -> dict[str, Any]:
    if manifest.get("schema_version") != RETIREMENT_SCHEMA:
        raise ReleaseEvidenceError("unsupported historical-plan retirement schema")
    successor = _safe_repo_path(manifest.get("authoritative_successor"), "authoritative successor")
    successor_path = root / successor
    if successor_path.is_symlink() or not successor_path.is_file():
        raise ReleaseEvidenceError("authoritative retirement successor is missing/symlinked")
    entries = manifest.get("entries")
    if not isinstance(entries, list) or not entries:
        raise ReleaseEvidenceError("historical-plan retirement manifest is empty")
    ids: set[str] = set()
    paths: set[str] = set()
    for entry in entries:
        if not isinstance(entry, dict):
            raise ReleaseEvidenceError("historical-plan retirement entry must be an object")
        entry_id = _required_text(entry.get("id"), "retirement id")
        if entry_id in ids:
            raise ReleaseEvidenceError(f"duplicate historical-plan retirement id: {entry_id}")
        ids.add(entry_id)
        rel = _safe_repo_path(entry.get("path"), f"retirement path {entry_id}")
        if rel in paths or rel == successor or "prd" in PurePosixPath(rel).name.lower():
            raise ReleaseEvidenceError(f"invalid/duplicate/non-PRD retirement path: {rel}")
        paths.add(rel)
        lifecycle = entry.get("lifecycle")
        if lifecycle not in {"superseded", "archived"}:
            raise ReleaseEvidenceError(f"historical plan is not retired: {rel}")
        if entry.get("successor") != successor:
            raise ReleaseEvidenceError(f"historical plan successor drift: {rel}")
        _required_text(entry.get("reason"), f"retirement reason {entry_id}")
        marker = _required_text(entry.get("document_marker"), f"retirement marker {entry_id}")
        path = root / rel
        if path.is_symlink() or not path.is_file():
            raise ReleaseEvidenceError(f"retired historical plan is missing/symlinked: {rel}")
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as exc:
            raise ReleaseEvidenceError(f"retired historical plan is unreadable: {rel}") from exc
        if marker not in text:
            raise ReleaseEvidenceError(f"historical plan retirement marker is absent: {rel}")
    return {
        "entries": len(entries),
        "superseded": sum(e["lifecycle"] == "superseded" for e in entries),
        "archived": sum(e["lifecycle"] == "archived" for e in entries),
    }


def validate_schema_documents(root: Path) -> dict[str, str]:
    schemas = {
        "release_matrix": root / "deploy/release/schemas/release-rollback-matrix-v1.schema.json",
        "integration_receipt": root
        / "deploy/release/schemas/integration-gate-receipt-v1.schema.json",
        "retirement": root / "deploy/release/schemas/historical-plan-retirement-v1.schema.json",
    }
    digests: dict[str, str] = {}
    for name, path in schemas.items():
        schema = _load(path, f"{name} JSON schema")
        if (
            schema.get("$schema") != "https://json-schema.org/draft/2020-12/schema"
            or schema.get("type") != "object"
        ):
            raise ReleaseEvidenceError(f"invalid JSON schema document: {name}")
        digests[name] = file_sha256(path)
    return digests


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", type=Path, default=ROOT)
    parser.add_argument("--matrix", type=Path, default=DEFAULT_MATRIX)
    parser.add_argument("--retirement", type=Path, default=DEFAULT_RETIREMENT)
    parser.add_argument("--level", choices=("draft", "candidate"), default="draft")
    args = parser.parse_args(argv)
    try:
        root = args.repo_root.resolve()
        schemas = validate_schema_documents(root)
        matrix = validate_release_matrix(
            root, _load(args.matrix, "release matrix"), level=args.level
        )
        retirement = validate_retirement_manifest(
            root, _load(args.retirement, "historical-plan retirement manifest")
        )
        print(
            json.dumps(
                {
                    "result": matrix["status"],
                    "level": args.level,
                    "matrix": matrix,
                    "retirement": retirement,
                    "schema_sha256": schemas,
                },
                indent=2,
                sort_keys=True,
            )
        )
        return 0
    except ReleaseEvidenceError as exc:
        print(f"RELEASE EVIDENCE ERROR: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
