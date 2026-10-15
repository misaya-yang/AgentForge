#!/usr/bin/env python3
"""Collect and verify the real parallel coding patch-artifact scenario.

The production Assistant sees an inline, immutable repository snapshot and can
only return a patch artifact.  This host verifier consumes Gateway SSE
observations produced by ``real_agent_scenario_runner``, checks the real plugin
subagent lifecycle, applies the two replacement files to a disposable copy, and
runs deterministic tests in a no-network, read-only Docker sandbox.

Candidate code is never executed directly on the host.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import math
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import uuid
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any

from scripts import real_agent_scenario_runner as real_runner
from scripts.eval_fixtures.coding_host_test_receipt import (
    HMAC_ENVIRONMENT_NAME,
    validator_policy,
)

REPO_ROOT = Path(__file__).resolve().parents[2]
CASE_ROOT = REPO_ROOT / "tests/fixtures/eval/general_agent/coding_parallel_v1"
SCENARIO_PATH = CASE_ROOT / "scenario.json"
CONTRACT_PATH = CASE_ROOT / "validator_contract.json"
TEMPLATE_PATH = CASE_ROOT / "repository"
RECEIPT_SCHEMA = "coding-agent-patch-validation/v1"
DEFAULT_IMAGE = str(validator_policy()["sandbox_image_reference"])
MAX_JSON_BYTES = 2_000_000
MAX_RAW_SSE_JSON_BYTES = 32_000_000
MAX_SOURCE_BYTES = 32_000
MAX_SOURCE_LINE_BYTES = 2_000
ALLOWED_IMAGE_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/:@-]{0,255}$")


class CodingFixtureError(ValueError):
    """Raised when the coding fixture or candidate artifact fails closed."""


def _reject_duplicate_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise CodingFixtureError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load_json_object(path: Path, *, max_bytes: int = MAX_JSON_BYTES) -> dict[str, Any]:
    try:
        if path.is_symlink() or path.stat().st_size > max_bytes:
            raise CodingFixtureError(f"unsafe or oversized JSON artifact: {path.name}")
        value = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=_reject_duplicate_pairs,
            parse_constant=lambda item: (_ for _ in ()).throw(
                CodingFixtureError(f"non-finite JSON constant: {item}")
            ),
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise CodingFixtureError(f"cannot load JSON artifact: {path.name}") from exc
    if not isinstance(value, dict):
        raise CodingFixtureError(f"JSON artifact must be an object: {path.name}")
    return value


def _canonical_bytes(value: Any) -> bytes:
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
        allow_nan=False,
    ).encode("utf-8")


def _sha256(value: Any) -> str:
    return hashlib.sha256(_canonical_bytes(value)).hexdigest()


def _nonempty_string(value: Any, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise CodingFixtureError(f"{field} must be a non-empty string")
    return value


def _finite_number(value: Any, field: str) -> float:
    if (
        not isinstance(value, int | float)
        or isinstance(value, bool)
        or not math.isfinite(float(value))
    ):
        raise CodingFixtureError(f"{field} must be a finite number")
    return float(value)


def _unique_strings(value: Any, field: str) -> list[str]:
    if not isinstance(value, list) or not value:
        raise CodingFixtureError(f"{field} must be a non-empty list")
    result = [_nonempty_string(item, f"{field}[]") for item in value]
    if len(result) != len(set(result)):
        raise CodingFixtureError(f"{field} contains duplicates")
    return result


def _strict_keys(
    value: Mapping[str, Any], *, required: set[str], optional: set[str], label: str
) -> None:
    missing = required - set(value)
    unknown = set(value) - required - optional
    if missing:
        raise CodingFixtureError(f"{label} missing fields: {', '.join(sorted(missing))}")
    if unknown:
        raise CodingFixtureError(f"{label} unsupported fields: {', '.join(sorted(unknown))}")


def _commands(contract: Mapping[str, Any]) -> list[list[str]]:
    value = contract.get("test_commands")
    if not isinstance(value, list) or not value:
        raise CodingFixtureError("test_commands must be a non-empty list")
    commands: list[list[str]] = []
    for raw in value:
        if not isinstance(raw, list) or not raw:
            raise CodingFixtureError("each test command must be a non-empty argv list")
        commands.append([_nonempty_string(item, "test command arg") for item in raw])
    return commands


def load_contract() -> dict[str, Any]:
    contract = load_json_object(CONTRACT_PATH)
    if contract.get("schema_version") != "coding-agent-validator/v1":
        raise CodingFixtureError("unsupported validator contract")
    if contract.get("case_id") != "coding.parallel.settlement-retry":
        raise CodingFixtureError("unexpected validator case_id")
    _unique_strings(contract.get("allowed_changes"), "allowed_changes")
    _unique_strings(
        contract.get("required_investigation_areas"),
        "required_investigation_areas",
    )
    _unique_strings(
        contract.get("required_rejected_hint_ids"),
        "required_rejected_hint_ids",
    )
    _commands(contract)
    return contract


def parse_candidate_answer(output: str) -> dict[str, Any]:
    try:
        answer = real_runner._answer_json(output, "final_json_tag")
    except real_runner.ScenarioContractError as exc:
        raise CodingFixtureError(str(exc)) from exc
    if not isinstance(answer, dict):
        raise CodingFixtureError("candidate FINAL_JSON must be an object")
    return answer


def validate_patch_artifact(
    answer: Mapping[str, Any], contract: Mapping[str, Any]
) -> dict[str, str]:
    """Validate structure and scope without trusting semantic self-assessment."""
    _strict_keys(
        answer,
        required={
            "status",
            "changed_files",
            "investigated_areas",
            "diagnoses",
            "rejected_hint_ids",
            "replacement_files",
            "validation_commands",
            "remaining_verification_boundary",
        },
        optional=set(),
        label="candidate patch artifact",
    )
    if answer.get("status") != "proposed_patch":
        raise CodingFixtureError("candidate status must be proposed_patch")
    allowed = sorted(_unique_strings(contract.get("allowed_changes"), "allowed_changes"))
    if sorted(_unique_strings(answer.get("changed_files"), "changed_files")) != allowed:
        raise CodingFixtureError("changed_files do not exactly match the allowed patch scope")

    expected_areas = sorted(
        _unique_strings(
            contract.get("required_investigation_areas"),
            "required_investigation_areas",
        )
    )
    if sorted(_unique_strings(answer.get("investigated_areas"), "investigated_areas")) != (
        expected_areas
    ):
        raise CodingFixtureError("investigated_areas are incomplete")

    diagnoses = answer.get("diagnoses")
    if not isinstance(diagnoses, list) or len(diagnoses) != len(expected_areas):
        raise CodingFixtureError("diagnoses must contain exactly one entry per area")
    diagnosed: list[str] = []
    required_evidence = contract.get("required_diagnosis_evidence")
    if not isinstance(required_evidence, dict) or sorted(required_evidence) != expected_areas:
        raise CodingFixtureError("validator required_diagnosis_evidence is malformed")
    for diagnosis in diagnoses:
        if not isinstance(diagnosis, dict):
            raise CodingFixtureError("diagnosis must be an object")
        _strict_keys(
            diagnosis,
            required={"area", "root_cause", "evidence"},
            optional=set(),
            label="diagnosis",
        )
        area = _nonempty_string(diagnosis.get("area"), "diagnosis.area")
        diagnosed.append(area)
        _nonempty_string(diagnosis.get("root_cause"), "diagnosis.root_cause")
        evidence = _unique_strings(diagnosis.get("evidence"), "diagnosis.evidence")
        required_agent = _nonempty_string(
            required_evidence.get(area), f"required_diagnosis_evidence[{area}]"
        )
        if required_agent not in evidence:
            raise CodingFixtureError(
                f"diagnosis for {area} does not cite its completed specialist receipt"
            )
    if sorted(diagnosed) != expected_areas:
        raise CodingFixtureError("diagnoses do not cover both investigation areas")

    expected_hints = sorted(
        _unique_strings(
            contract.get("required_rejected_hint_ids"),
            "required_rejected_hint_ids",
        )
    )
    if sorted(_unique_strings(answer.get("rejected_hint_ids"), "rejected_hint_ids")) != (
        expected_hints
    ):
        raise CodingFixtureError("both unsupported incident hints must be rejected")
    if answer.get("validation_commands") != _commands(contract):
        raise CodingFixtureError("validation_commands must exactly match the host contract")
    boundary = _nonempty_string(
        answer.get("remaining_verification_boundary"),
        "remaining_verification_boundary",
    ).casefold()
    if "pending" not in boundary or "host" not in boundary:
        raise CodingFixtureError("candidate must preserve the pending host-test boundary")

    replacements = answer.get("replacement_files")
    if not isinstance(replacements, dict) or sorted(replacements) != allowed:
        raise CodingFixtureError("replacement_files must contain exactly the two allowed paths")
    validated: dict[str, str] = {}
    total_bytes = 0
    for relative in allowed:
        source = _nonempty_string(replacements.get(relative), f"replacement_files[{relative}]")
        encoded = source.encode("utf-8")
        total_bytes += len(encoded)
        if len(encoded) > MAX_SOURCE_BYTES:
            raise CodingFixtureError(f"replacement source exceeds byte limit: {relative}")
        if any(len(line) > MAX_SOURCE_LINE_BYTES for line in encoded.splitlines()):
            raise CodingFixtureError(f"replacement source has an oversized line: {relative}")
        if b"\x00" in encoded:
            raise CodingFixtureError(f"replacement source contains NUL: {relative}")
        validated[relative] = source
    if total_bytes > MAX_SOURCE_BYTES:
        raise CodingFixtureError("combined replacement sources exceed byte limit")
    return validated


def _snapshot(root: Path) -> dict[str, bytes]:
    result: dict[str, bytes] = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root)
        if {"__pycache__", ".pytest_cache"}.intersection(relative.parts):
            continue
        if path.is_symlink():
            raise CodingFixtureError(f"fixture contains a symlink: {relative}")
        if path.is_file() and path.suffix not in {".pyc", ".pyo"}:
            result[relative.as_posix()] = path.read_bytes()
    return result


def _changed_lines(before: bytes, after: bytes) -> int:
    import difflib

    old = before.decode("utf-8").splitlines()
    new = after.decode("utf-8").splitlines()
    return sum(
        1
        for line in difflib.unified_diff(old, new, lineterm="")
        if line.startswith(("+", "-")) and not line.startswith(("+++", "---"))
    )


def materialize_candidate(
    root: Path, replacements: Mapping[str, str], contract: Mapping[str, Any]
) -> dict[str, Any]:
    shutil.copytree(TEMPLATE_PATH, root)
    baseline = _snapshot(TEMPLATE_PATH)
    for relative, source in replacements.items():
        target = (root / relative).resolve()
        if not target.is_relative_to(root.resolve()) or target.is_symlink() or not target.is_file():
            raise CodingFixtureError(f"replacement path is unsafe: {relative}")
        target.write_text(source, encoding="utf-8")
    candidate = _snapshot(root)
    changed = sorted(
        path for path in set(baseline) | set(candidate) if baseline.get(path) != candidate.get(path)
    )
    allowed = sorted(_unique_strings(contract.get("allowed_changes"), "allowed_changes"))
    if changed != allowed:
        raise CodingFixtureError("materialized patch changed an unexpected file")
    changed_lines = sum(_changed_lines(baseline[path], candidate[path]) for path in changed)
    limit = contract.get("max_changed_lines")
    if isinstance(limit, bool) or not isinstance(limit, int) or changed_lines > limit:
        raise CodingFixtureError(f"patch changes {changed_lines} lines; maximum is {limit}")
    return {
        "changed_files": changed,
        "changed_lines": changed_lines,
        "replacement_sha256": {
            path: hashlib.sha256(candidate[path]).hexdigest() for path in changed
        },
    }


def _docker_image_receipt(image: str) -> dict[str, str]:
    if not ALLOWED_IMAGE_RE.fullmatch(image):
        raise CodingFixtureError("sandbox image reference is malformed")
    result = subprocess.run(
        ["docker", "image", "inspect", "--format", "{{.Id}}", image],
        check=False,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        text=True,
        timeout=10,
    )
    image_id = result.stdout.strip()
    expected_image_id = "sha256:" + image.rsplit("@sha256:", 1)[1] if "@sha256:" in image else None
    if (
        result.returncode != 0
        or not image_id.startswith("sha256:")
        or (expected_image_id is not None and image_id != expected_image_id)
    ):
        raise CodingFixtureError(
            "sandbox image is missing or differs from the pinned digest; preload the exact image"
        )
    return {"reference": image, "image_id": image_id}


def run_tests_in_sandbox(
    workspace: Path,
    contract: Mapping[str, Any],
    *,
    image: str,
    timeout_seconds: float = 20.0,
) -> tuple[dict[str, str], list[dict[str, Any]]]:
    """Run candidate code only inside a constrained, no-network container."""
    image_receipt = _docker_image_receipt(image)
    receipts: list[dict[str, Any]] = []
    for index, declared in enumerate(_commands(contract)):
        name = f"general-agent-code-eval-{os.getpid()}-{index}-{uuid.uuid4().hex[:8]}"
        inner = ["python" if item == "$PYTHON" else item for item in declared]
        command = [
            "docker",
            "run",
            "--rm",
            "--pull",
            "never",
            "--name",
            name,
            "--network",
            "none",
            "--read-only",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--pids-limit",
            "64",
            "--memory",
            "128m",
            "--cpus",
            "0.5",
            "--user",
            "65534:65534",
            "--tmpfs",
            "/tmp:rw,noexec,nosuid,size=16m",
            "--log-driver",
            "none",
            "-e",
            "PYTHONPATH=/work/src",
            "-e",
            "PYTHONDONTWRITEBYTECODE=1",
            "-e",
            "PYTHONHASHSEED=0",
            "-v",
            f"{workspace.resolve()}:/work:ro",
            "-w",
            "/work",
            image,
            *inner,
        ]
        started = time.monotonic_ns()
        timed_out = False
        try:
            result = subprocess.run(
                command,
                check=False,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=timeout_seconds,
            )
            exit_code: int | None = result.returncode
        except subprocess.TimeoutExpired:
            timed_out = True
            exit_code = None
            subprocess.run(
                ["docker", "rm", "-f", name],
                check=False,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=10,
            )
        receipts.append(
            {
                "command": declared,
                "exit_code": exit_code,
                "timed_out": timed_out,
                "duration_ms": round((time.monotonic_ns() - started) / 1_000_000, 3),
                "passed": exit_code == 0 and not timed_out,
            }
        )
    return image_receipt, receipts


def _accepted_execution_checks(
    scenario: Mapping[str, Any], observation: Mapping[str, Any]
) -> list[dict[str, Any]]:
    checks = real_runner._execution_checks(scenario, observation)
    if not all(check.get("passed") is True for check in checks):
        failed = [str(check.get("check_id")) for check in checks if not check.get("passed")]
        raise CodingFixtureError("Gateway execution checks failed: " + ", ".join(failed))
    return [dict(item) for item in checks]


def validate_live_event_timeline(
    events: Sequence[Mapping[str, Any]], contract: Mapping[str, Any]
) -> dict[str, Any]:
    """Prove that the final patch was emitted after the two overlapping terminals."""
    starts: dict[int, tuple[int, Mapping[str, Any]]] = {}
    finishes: dict[int, tuple[int, Mapping[str, Any]]] = {}
    spawn_call_id = ""
    spawn_result: tuple[int, Mapping[str, Any]] | None = None
    final_json_event_index: int | None = None
    terminal_index: int | None = None
    accumulated_text = ""

    def call_id(data: Mapping[str, Any]) -> str:
        return str(data.get("tool_call_id") or data.get("call_id") or "")

    for position, event in enumerate(events):
        event_type = str(event.get("event_type") or "")
        raw_data = event.get("data")
        data = raw_data if isinstance(raw_data, dict) else {}
        dispatch_index = data.get("dispatch_index")
        if event_type == "subagent_started" and isinstance(dispatch_index, int):
            if dispatch_index in starts:
                raise CodingFixtureError("duplicate subagent_started dispatch index")
            starts[dispatch_index] = (position, data)
        elif event_type == "subagent_finished" and isinstance(dispatch_index, int):
            if dispatch_index in finishes:
                raise CodingFixtureError("duplicate subagent_finished dispatch index")
            finishes[dispatch_index] = (position, data)
        elif event_type in real_runner.TOOL_START_EVENTS:
            name = str(data.get("name") or data.get("tool_name") or "")
            if name == "spawn_subagent":
                if spawn_call_id:
                    raise CodingFixtureError("more than one spawn_subagent call was observed")
                spawn_call_id = call_id(data)
        elif event_type in real_runner.TOOL_RESULT_EVENTS and spawn_call_id:
            if call_id(data) == spawn_call_id:
                if spawn_result is not None:
                    raise CodingFixtureError("duplicate spawn_subagent result was observed")
                spawn_result = (position, data)
        if event_type in real_runner.TEXT_EVENTS:
            # Production text deltas use a bare string while several protocol
            # adapters wrap it in {delta|content|message}. Preserve both.
            accumulated_text += real_runner._event_text(raw_data)
            if final_json_event_index is None and "<FINAL_JSON>" in accumulated_text:
                final_json_event_index = position
        if event_type in real_runner.TERMINAL_EVENTS:
            if terminal_index is not None:
                raise CodingFixtureError("multiple parent terminal events were observed")
            terminal_index = position

    expected_indexes = {0, 1}
    if set(starts) != expected_indexes or set(finishes) != expected_indexes:
        raise CodingFixtureError("live trace needs exactly two complete child lifecycles")
    if not spawn_call_id or spawn_result is None:
        raise CodingFixtureError("live trace lacks the paired spawn_subagent result")
    result_position, result_data = spawn_result
    if result_data.get("success") is not True and result_data.get("status") not in {
        "completed",
        "success",
    }:
        raise CodingFixtureError("spawn_subagent result is not successful")
    if str(result_data.get("side_effect_state") or "").casefold() not in {
        "none",
        "read_only",
    }:
        raise CodingFixtureError("spawn_subagent result lacks a safe typed side-effect state")
    if final_json_event_index is None or terminal_index is None:
        raise CodingFixtureError("live trace lacks final JSON or parent terminal")

    intervals: list[tuple[float, float]] = []
    child_terminal_sha256s: list[str] = []
    minimum_duration = float(contract.get("minimum_child_duration_ms") or 0)
    for index in sorted(expected_indexes):
        start_position, start_data = starts[index]
        finish_position, finish_data = finishes[index]
        if start_position >= finish_position:
            raise CodingFixtureError("child terminal precedes its start event")
        started_float = _finite_number(
            finish_data.get("started_monotonic_ms"),
            "child terminal started_monotonic_ms",
        )
        finished_float = _finite_number(
            finish_data.get("finished_monotonic_ms"),
            "child terminal finished_monotonic_ms",
        )
        start_receipt = _finite_number(
            start_data.get("started_monotonic_ms"),
            "child start started_monotonic_ms",
        )
        if started_float != start_receipt:
            raise CodingFixtureError("child start and terminal timestamps disagree")
        if finished_float - started_float < minimum_duration:
            raise CodingFixtureError("child interval is too short for the coding task")
        intervals.append((started_float, finished_float))
        child_terminal_sha256s.append(_sha256(finish_data))

    overlap_ms = min(finish for _, finish in intervals) - max(start for start, _ in intervals)
    minimum_overlap = float(contract.get("minimum_parallel_overlap_ms") or 0)
    if overlap_ms < minimum_overlap:
        raise CodingFixtureError(
            f"child overlap is {max(overlap_ms, 0):.3f}ms; minimum is {minimum_overlap:.3f}ms"
        )
    last_child_terminal = max(position for position, _ in finishes.values())
    if not last_child_terminal < result_position < final_json_event_index < terminal_index:
        raise CodingFixtureError(
            "parent patch was not emitted after both child terminals and the aggregate result"
        )
    return {
        "overlap_ms": round(overlap_ms, 3),
        "spawn_call_id_sha256": hashlib.sha256(spawn_call_id.encode()).hexdigest(),
        "child_terminal_sha256s": sorted(child_terminal_sha256s),
        "last_child_terminal_event_index": last_child_terminal,
        "spawn_result_event_index": result_position,
        "final_json_event_index": final_json_event_index,
        "parent_terminal_event_index": terminal_index,
    }


def _load_live_timelines(
    observations_path: Path,
    observations: Mapping[str, Any],
    contract: Mapping[str, Any],
) -> dict[tuple[str, int], dict[str, Any]]:
    receipt = observations.get("raw_sse_artifact")
    if not isinstance(receipt, dict):
        raise CodingFixtureError("raw SSE receipt is unavailable")
    raw_path = observations_path.resolve().parent / str(receipt.get("file_name") or "")
    raw_document = load_json_object(raw_path, max_bytes=MAX_RAW_SSE_JSON_BYTES)
    timelines: dict[tuple[str, int], dict[str, Any]] = {}
    for trial in raw_document.get("trials", []):
        if not isinstance(trial, dict):
            raise CodingFixtureError("raw SSE trial is malformed")
        events: list[Mapping[str, Any]] = []
        for payload in trial.get("raw_sse_payloads", []):
            try:
                event = real_runner._strict_json_loads(payload, label="raw SSE payload")
            except real_runner.ScenarioContractError as exc:
                raise CodingFixtureError(str(exc)) from exc
            if not isinstance(event, dict):
                raise CodingFixtureError("raw SSE event must be an object")
            events.append(event)
        key = (str(trial.get("scenario_id") or ""), int(trial.get("trial") or 0))
        timelines[key] = validate_live_event_timeline(events, contract)
    return timelines


def validate_observations(
    observations_path: Path,
    *,
    image: str = DEFAULT_IMAGE,
    live_collected: bool = False,
    collector_key: str | None = None,
    runtime_attestation: str | None = None,
) -> dict[str, Any]:
    scenarios = real_runner.load_scenarios(SCENARIO_PATH)
    source_receipts = real_runner.verify_source_artifacts(scenarios, scenario_directory=CASE_ROOT)
    plugin_definitions = real_runner.verify_plugin_definitions(scenarios)
    observations = load_json_object(observations_path)
    if not live_collected:
        return {
            "schema_version": RECEIPT_SCHEMA,
            "suite_id": scenarios["suite_id"],
            "scenario_id": scenarios["scenarios"][0]["scenario_id"],
            "validation_passed": False,
            "acceptance_eligible": False,
            "passed": False,
            "trials": [],
            "errors": [
                "offline observations are not accepted; use the live collector path with fresh HMAC, nonce, raw SSE, and runtime binding"
            ],
        }
    resolved_collector_key = real_runner._attestation_key(
        collector_key,
        environment_name="GENERAL_AGENT_COLLECTOR_HMAC_KEY",
        label="coding fixture validation",
    )
    resolved_runtime_attestation = runtime_attestation or os.environ.get(
        "GENERAL_AGENT_RUNTIME_ATTESTATION", ""
    )
    if not resolved_runtime_attestation.strip():
        raise CodingFixtureError("coding fixture requires GENERAL_AGENT_RUNTIME_ATTESTATION")
    try:
        by_key = real_runner._verify_observations(
            scenarios,
            observations,
            source_artifacts=source_receipts,
            plugin_definitions=plugin_definitions,
            observation_path=observations_path,
            collector_key=resolved_collector_key,
            runtime_attestation=resolved_runtime_attestation,
            expected_suite_nonce=real_runner._external_suite_nonce(),
        )
    except real_runner.ScenarioContractError as exc:
        raise CodingFixtureError(str(exc)) from exc
    scenario = scenarios["scenarios"][0]
    contract = load_contract()
    timelines = _load_live_timelines(observations_path, observations, contract)
    trials: list[dict[str, Any]] = []
    for trial_number in range(1, scenario["repetitions"] + 1):
        observation = by_key[(scenario["scenario_id"], trial_number)]
        errors: list[str] = []
        execution_checks: list[dict[str, Any]] = []
        patch_receipt: dict[str, Any] = {}
        test_receipts: list[dict[str, Any]] = []
        image_receipt: dict[str, str] = {}
        try:
            execution_checks = _accepted_execution_checks(scenario, observation)
            answer = parse_candidate_answer(str(observation.get("candidate_output") or ""))
            replacements = validate_patch_artifact(answer, contract)
            with tempfile.TemporaryDirectory(prefix="general-agent-code-eval-") as directory:
                workspace = Path(directory) / "workspace"
                patch_receipt = materialize_candidate(workspace, replacements, contract)
                image_receipt, test_receipts = run_tests_in_sandbox(
                    workspace, contract, image=image
                )
                if not all(item["passed"] for item in test_receipts):
                    raise CodingFixtureError("one or more isolated host tests failed")
        except (CodingFixtureError, OSError, subprocess.SubprocessError) as exc:
            errors.append(str(exc))
        trials.append(
            {
                "trial": trial_number,
                "observation_sha256": observation.get("observation_sha256"),
                "accepted": not errors,
                "execution_checks": execution_checks,
                "timeline": timelines[(scenario["scenario_id"], trial_number)],
                "patch": patch_receipt,
                "sandbox_image": image_receipt,
                "host_test_receipts": test_receipts,
                "errors": errors,
            }
        )
    validation_passed = len(trials) == 3 and all(item["accepted"] for item in trials)
    return {
        "schema_version": RECEIPT_SCHEMA,
        "suite_id": scenarios["suite_id"],
        "scenario_id": scenario["scenario_id"],
        "scenario_contract_sha256": observations["scenario_contract_sha256"],
        "observations_sha256": observations["observations_sha256"],
        "validator_contract_sha256": _sha256(contract),
        "verifier_executable_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "source_artifacts": source_receipts,
        "plugin_definitions": plugin_definitions,
        "runtime_binding": observations.get("runtime_binding"),
        "raw_sse_artifact": observations.get("raw_sse_artifact"),
        "collector_attestation_key_id": (
            observations.get("collector_attestation", {}).get("key_id")
            if isinstance(observations.get("collector_attestation"), dict)
            else None
        ),
        "trials": trials,
        "three_run_policy_met": validation_passed,
        "validation_passed": validation_passed,
        "acceptance_eligible": live_collected,
        "passed": validation_passed and live_collected,
        "evidence_boundary": (
            "Real Gateway SSE lifecycle plus isolated local Docker test execution. "
            "This is not a production deployment receipt."
        ),
    }


def _safe_write_receipt(path: Path, payload: Mapping[str, Any], *, require_hmac: bool) -> None:
    if path.resolve().is_relative_to(CASE_ROOT.resolve()):
        raise CodingFixtureError("receipt output must be outside immutable fixture sources")
    hmac_key = os.environ.get(HMAC_ENVIRONMENT_NAME, "")
    if require_hmac and not hmac_key:
        raise CodingFixtureError(f"{HMAC_ENVIRONMENT_NAME} is required")
    if hmac_key and len(hmac_key.encode("utf-8")) < 32:
        raise CodingFixtureError(f"{HMAC_ENVIRONMENT_NAME} must be at least 32 bytes")
    for environment_name in (
        "GENERAL_AGENT_COLLECTOR_HMAC_KEY",
        "GENERAL_AGENT_GOLDEN_HMAC_KEY",
    ):
        other_key = os.environ.get(environment_name, "")
        if hmac_key and other_key and hmac.compare_digest(hmac_key, other_key):
            raise CodingFixtureError(
                f"{HMAC_ENVIRONMENT_NAME} must be independent from {environment_name}"
            )
    seal = real_runner._seal(payload, hmac_key=hmac_key or None)
    document = {**payload, "seal": seal}
    if path.is_symlink():
        raise CodingFixtureError("refusing to write a receipt through a symlink")
    resolved = path.resolve(strict=False)
    resolved.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(
        resolved,
        os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    try:
        os.fchmod(descriptor, stat.S_IRUSR | stat.S_IWUSR)
        with os.fdopen(descriptor, "w", encoding="utf-8", closefd=False) as handle:
            json.dump(document, handle, ensure_ascii=False, indent=2, sort_keys=True)
            handle.write("\n")
    finally:
        os.close(descriptor)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--observations-out", required=True, type=Path)
    parser.add_argument("--receipt-out", required=True, type=Path)
    parser.add_argument("--image", default=DEFAULT_IMAGE)
    parser.add_argument(
        "--require-hmac",
        action="store_true",
        help="deprecated compatibility flag; live receipts are always HMAC sealed",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        observations_path = args.observations_out
        if observations_path.resolve().is_relative_to(CASE_ROOT.resolve()):
            raise CodingFixtureError("observations output must be outside fixture sources")
        real_runner.collect(SCENARIO_PATH, observations_path)
        receipt = validate_observations(
            observations_path,
            image=args.image,
            live_collected=True,
            collector_key=os.environ.get("GENERAL_AGENT_COLLECTOR_HMAC_KEY"),
            runtime_attestation=os.environ.get("GENERAL_AGENT_RUNTIME_ATTESTATION"),
        )
        # A live coding receipt is a formal release input.  Hash-only output is
        # never useful here because merge and judge require the independent host key.
        _safe_write_receipt(args.receipt_out, receipt, require_hmac=True)
        print(
            json.dumps(
                {
                    "passed": receipt["passed"],
                    "receipt": str(args.receipt_out.resolve()),
                    "observations": str(observations_path.resolve()),
                },
                sort_keys=True,
            )
        )
        return 0 if receipt["passed"] else 1
    except (CodingFixtureError, real_runner.ScenarioContractError, OSError) as exc:
        print(f"coding fixture failed closed: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
