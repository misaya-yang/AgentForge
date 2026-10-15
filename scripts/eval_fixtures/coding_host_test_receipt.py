"""Fail-closed verification for live coding host-test receipts.

This module is evaluation-only.  It gives the collector/merge runner and the
release judge one canonical parser for the independently HMAC-sealed receipt
emitted by ``verify_coding_parallel_fixture.py``.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import math
import stat
from collections.abc import Mapping
from pathlib import Path
from typing import Any

REPO_ROOT = Path(__file__).resolve().parents[2]
VALIDATOR_CONTRACT_PATH = (
    REPO_ROOT / "tests/fixtures/eval/general_agent/coding_parallel_v1/validator_contract.json"
)
VERIFIER_PATH = REPO_ROOT / "scripts/eval_fixtures/verify_coding_parallel_fixture.py"
RECEIPT_VERIFIER_PATH = Path(__file__).resolve()
RECEIPT_SCHEMA = "coding-agent-patch-validation/v1"
EVIDENCE_SCHEMA = "coding-host-test-evidence/v1"
SUITE_ID = "general-agent.real-coding.v1"
SCENARIO_ID = "coding.parallel.settlement-retry"
HMAC_ENVIRONMENT_NAME = "GENERAL_AGENT_CODING_HOST_TEST_HMAC_KEY"
MAX_RECEIPT_BYTES = 5_000_000
_SHA_RE = __import__("re").compile(r"^[a-f0-9]{64}$")
_IMAGE_ID_RE = __import__("re").compile(r"^sha256:[a-f0-9]{64}$")


class CodingHostReceiptError(ValueError):
    """The coding host-test receipt is absent, untrusted, or inconsistent."""


def canonical_bytes(value: Any) -> bytes:
    try:
        return json.dumps(
            value,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
    except (TypeError, ValueError) as exc:
        raise CodingHostReceiptError("coding receipt contains non-canonical JSON") from exc


def sha256(value: Any) -> str:
    return hashlib.sha256(canonical_bytes(value)).hexdigest()


def _strict_json(raw: bytes, *, label: str) -> dict[str, Any]:
    def pairs_hook(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate object key {key!r}")
            result[key] = value
        return result

    try:
        value = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=pairs_hook,
            parse_constant=lambda item: (_ for _ in ()).throw(
                ValueError(f"non-finite JSON constant {item}")
            ),
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
        raise CodingHostReceiptError(f"{label} is not strict JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise CodingHostReceiptError(f"{label} must be an object")
    return value


def _strict_keys(
    value: Mapping[str, Any],
    *,
    required: set[str],
    optional: set[str] | None = None,
    label: str,
) -> None:
    optional = optional or set()
    missing = required - set(value)
    unknown = set(value) - required - optional
    if missing:
        raise CodingHostReceiptError(f"{label} missing fields: {', '.join(sorted(missing))}")
    if unknown:
        raise CodingHostReceiptError(f"{label} unsupported fields: {', '.join(sorted(unknown))}")


def _digest(value: Any, *, label: str) -> str:
    if not isinstance(value, str) or not _SHA_RE.fullmatch(value):
        raise CodingHostReceiptError(f"{label} must be a lowercase SHA-256 digest")
    return value


def _integer(value: Any, *, label: str, minimum: int, maximum: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
        raise CodingHostReceiptError(f"{label} must be an integer in [{minimum}, {maximum}]")
    return value


def _finite_number(value: Any, *, label: str, minimum: float = 0.0) -> float:
    if (
        isinstance(value, bool)
        or not isinstance(value, int | float)
        or not math.isfinite(float(value))
        or float(value) < minimum
    ):
        raise CodingHostReceiptError(f"{label} must be a finite number >= {minimum}")
    return float(value)


def _commands(value: Any, *, label: str) -> list[list[str]]:
    if not isinstance(value, list) or not value:
        raise CodingHostReceiptError(f"{label} must be a non-empty command list")
    result: list[list[str]] = []
    for index, command in enumerate(value):
        if not isinstance(command, list) or not command:
            raise CodingHostReceiptError(f"{label}[{index}] must be a non-empty argv list")
        if any(not isinstance(arg, str) or not arg for arg in command):
            raise CodingHostReceiptError(f"{label}[{index}] contains an invalid argv item")
        result.append(list(command))
    return result


def validator_policy() -> dict[str, Any]:
    """Return the immutable host-test policy that is also release-manifest pinned."""

    if VALIDATOR_CONTRACT_PATH.is_symlink() or not VALIDATOR_CONTRACT_PATH.is_file():
        raise CodingHostReceiptError("coding validator contract is missing or symlinked")
    contract = _strict_json(VALIDATOR_CONTRACT_PATH.read_bytes(), label="validator contract")
    _strict_keys(
        contract,
        required={
            "schema_version",
            "case_id",
            "allowed_changes",
            "required_investigation_areas",
            "required_diagnosis_evidence",
            "required_rejected_hint_ids",
            "max_changed_lines",
            "minimum_child_duration_ms",
            "minimum_parallel_overlap_ms",
            "sandbox_image_reference",
            "test_commands",
            "execution_boundary",
        },
        label="validator contract",
    )
    if contract["schema_version"] != "coding-agent-validator/v1":
        raise CodingHostReceiptError("unsupported coding validator contract")
    if contract["case_id"] != SCENARIO_ID:
        raise CodingHostReceiptError("coding validator contract case_id does not match")
    allowed_changes = contract["allowed_changes"]
    if (
        not isinstance(allowed_changes, list)
        or not allowed_changes
        or any(not isinstance(item, str) or not item for item in allowed_changes)
        or len(allowed_changes) != len(set(allowed_changes))
    ):
        raise CodingHostReceiptError("validator allowed_changes is malformed")
    max_changed_lines = _integer(
        contract["max_changed_lines"],
        label="validator max_changed_lines",
        minimum=1,
        maximum=10_000,
    )
    minimum_overlap_ms = _finite_number(
        contract["minimum_parallel_overlap_ms"],
        label="validator minimum_parallel_overlap_ms",
        minimum=25.0,
    )
    image_reference = contract["sandbox_image_reference"]
    if (
        not isinstance(image_reference, str)
        or "@sha256:" not in image_reference
        or not _SHA_RE.fullmatch(image_reference.rsplit("@sha256:", 1)[1])
    ):
        raise CodingHostReceiptError("validator sandbox image must be digest pinned")
    commands = _commands(contract["test_commands"], label="validator test_commands")
    if len(commands) != 3:
        raise CodingHostReceiptError("coding validator must pin exactly three host tests")
    if VERIFIER_PATH.is_symlink() or not VERIFIER_PATH.is_file():
        raise CodingHostReceiptError("coding host verifier is missing or symlinked")
    return {
        "validator_contract_sha256": sha256(contract),
        "validator_contract_file_sha256": hashlib.sha256(
            VALIDATOR_CONTRACT_PATH.read_bytes()
        ).hexdigest(),
        "verifier_executable_sha256": hashlib.sha256(VERIFIER_PATH.read_bytes()).hexdigest(),
        "receipt_verifier_executable_sha256": hashlib.sha256(
            RECEIPT_VERIFIER_PATH.read_bytes()
        ).hexdigest(),
        "sandbox_image_reference": image_reference,
        "test_commands": commands,
        "allowed_changes": sorted(allowed_changes),
        "max_changed_lines": max_changed_lines,
        "minimum_parallel_overlap_ms": minimum_overlap_ms,
    }


def _load_sealed_receipt(path: Path, *, hmac_key: str) -> tuple[dict[str, Any], str]:
    if not isinstance(hmac_key, str) or len(hmac_key.encode("utf-8")) < 32:
        raise CodingHostReceiptError("coding host-test HMAC key must be at least 32 bytes")
    if path.is_symlink():
        raise CodingHostReceiptError("coding host-test receipt symlinks are not allowed")
    try:
        resolved = path.resolve(strict=True)
        metadata = resolved.stat()
    except OSError as exc:
        raise CodingHostReceiptError("coding host-test receipt is unavailable") from exc
    if not resolved.is_file() or metadata.st_size > MAX_RECEIPT_BYTES:
        raise CodingHostReceiptError("coding host-test receipt is not a bounded regular file")
    if stat.S_IMODE(metadata.st_mode) != 0o600:
        raise CodingHostReceiptError("coding host-test receipt mode must be 0600")
    raw = resolved.read_bytes()
    document = _strict_json(raw, label="coding host-test receipt")
    seal = document.get("seal")
    if not isinstance(seal, dict):
        raise CodingHostReceiptError("coding host-test receipt seal is missing")
    _strict_keys(
        seal,
        required={"algorithm", "digest", "hmac_algorithm", "hmac_digest"},
        label="coding host-test receipt seal",
    )
    payload = dict(document)
    payload.pop("seal")
    digest = _digest(seal["digest"], label="coding host-test payload digest")
    if seal["algorithm"] != "sha256" or not hmac.compare_digest(digest, sha256(payload)):
        raise CodingHostReceiptError("coding host-test SHA-256 seal does not match")
    hmac_digest = _digest(seal["hmac_digest"], label="coding host-test HMAC digest")
    expected = hmac.new(hmac_key.encode("utf-8"), canonical_bytes(payload), hashlib.sha256)
    if seal["hmac_algorithm"] != "hmac-sha256" or not hmac.compare_digest(
        hmac_digest, expected.hexdigest()
    ):
        raise CodingHostReceiptError("coding host-test HMAC seal does not match")
    return payload, hashlib.sha256(raw).hexdigest()


def verify_receipt(
    path: Path,
    *,
    hmac_key: str,
    expected_scenario_contract_sha256: str,
    expected_observations_sha256: str,
    expected_collector_attestation_key_id: str,
    expected_trials: Mapping[int, Mapping[str, Any]],
    expected_source_artifacts: Any | None = None,
    expected_plugin_definitions: Any | None = None,
    expected_runtime_binding: Any | None = None,
    expected_raw_sse_artifact: Any | None = None,
) -> dict[str, Any]:
    """Verify one live 3-trial receipt and return its double-HMAC binding evidence."""

    payload, file_sha256 = _load_sealed_receipt(path, hmac_key=hmac_key)
    _strict_keys(
        payload,
        required={
            "schema_version",
            "suite_id",
            "scenario_id",
            "scenario_contract_sha256",
            "observations_sha256",
            "validator_contract_sha256",
            "verifier_executable_sha256",
            "source_artifacts",
            "plugin_definitions",
            "runtime_binding",
            "raw_sse_artifact",
            "collector_attestation_key_id",
            "trials",
            "three_run_policy_met",
            "validation_passed",
            "acceptance_eligible",
            "passed",
            "evidence_boundary",
        },
        label="coding host-test receipt payload",
    )
    if payload["schema_version"] != RECEIPT_SCHEMA:
        raise CodingHostReceiptError("unsupported coding host-test receipt schema")
    if payload["suite_id"] != SUITE_ID or payload["scenario_id"] != SCENARIO_ID:
        raise CodingHostReceiptError("coding host-test suite/scenario identity does not match")
    if payload["scenario_contract_sha256"] != expected_scenario_contract_sha256:
        raise CodingHostReceiptError("coding host-test receipt binds another scenario contract")
    if payload["observations_sha256"] != expected_observations_sha256:
        raise CodingHostReceiptError("coding host-test receipt binds another observation set")
    if payload["collector_attestation_key_id"] != expected_collector_attestation_key_id:
        raise CodingHostReceiptError("coding host-test collector identity does not match")
    for name, expected in (
        ("source_artifacts", expected_source_artifacts),
        ("plugin_definitions", expected_plugin_definitions),
        ("runtime_binding", expected_runtime_binding),
        ("raw_sse_artifact", expected_raw_sse_artifact),
    ):
        if expected is not None and payload[name] != expected:
            raise CodingHostReceiptError(f"coding host-test {name} binding does not match")
    if any(
        payload[field] is not True
        for field in (
            "three_run_policy_met",
            "validation_passed",
            "acceptance_eligible",
            "passed",
        )
    ):
        raise CodingHostReceiptError("coding host-test receipt is non-live or not fully passed")

    policy = validator_policy()
    if payload["validator_contract_sha256"] != policy["validator_contract_sha256"]:
        raise CodingHostReceiptError("coding validator contract digest does not match")
    if payload["verifier_executable_sha256"] != policy["verifier_executable_sha256"]:
        raise CodingHostReceiptError("coding verifier executable digest does not match")
    raw_trials = payload["trials"]
    if not isinstance(raw_trials, list) or len(raw_trials) != 3:
        raise CodingHostReceiptError("coding host-test receipt requires exactly three trials")
    if set(expected_trials) != {1, 2, 3}:
        raise CodingHostReceiptError("merged coding observations require exactly trials 1, 2, 3")
    by_trial: dict[int, Mapping[str, Any]] = {}
    evidence_trials: list[dict[str, Any]] = []
    expected_image_id = "sha256:" + policy["sandbox_image_reference"].rsplit("@sha256:", 1)[1]
    for index, trial in enumerate(raw_trials):
        if not isinstance(trial, dict):
            raise CodingHostReceiptError(f"coding host-test trial {index} must be an object")
        _strict_keys(
            trial,
            required={
                "trial",
                "observation_sha256",
                "accepted",
                "execution_checks",
                "timeline",
                "patch",
                "sandbox_image",
                "host_test_receipts",
                "errors",
            },
            label=f"coding host-test trial {index}",
        )
        number = _integer(trial["trial"], label="coding trial", minimum=1, maximum=3)
        if number in by_trial:
            raise CodingHostReceiptError(f"duplicate coding host-test trial {number}")
        by_trial[number] = trial
        expected = expected_trials[number]
        if trial["observation_sha256"] != expected.get("observation_sha256"):
            raise CodingHostReceiptError(f"coding trial {number} binds another observation")
        execution_checks = trial["execution_checks"]
        if not isinstance(execution_checks, list) or not execution_checks:
            raise CodingHostReceiptError(f"coding trial {number} execution checks are missing")
        expected_checks = expected.get("execution_checks")
        if isinstance(expected_checks, list) and execution_checks != expected_checks:
            raise CodingHostReceiptError(f"coding trial {number} execution checks do not match")
        if any(
            not isinstance(check, dict)
            or check.get("passed") is not True
            or not isinstance(check.get("check_id"), str)
            or not _SHA_RE.fullmatch(str(check.get("observed_sha256") or ""))
            for check in execution_checks
        ):
            raise CodingHostReceiptError(f"coding trial {number} execution checks are not passed")
        if trial["accepted"] is not True or trial["errors"] != []:
            raise CodingHostReceiptError(f"coding trial {number} was not accepted")

        timeline = trial["timeline"]
        if not isinstance(timeline, dict):
            raise CodingHostReceiptError(f"coding trial {number} timeline must be an object")
        _strict_keys(
            timeline,
            required={
                "overlap_ms",
                "spawn_call_id_sha256",
                "child_terminal_sha256s",
                "last_child_terminal_event_index",
                "spawn_result_event_index",
                "final_json_event_index",
                "parent_terminal_event_index",
            },
            label=f"coding trial {number} timeline",
        )
        overlap = _finite_number(timeline["overlap_ms"], label="coding overlap_ms", minimum=25.0)
        if overlap < policy["minimum_parallel_overlap_ms"]:
            raise CodingHostReceiptError(f"coding trial {number} overlap is below policy")
        _digest(timeline["spawn_call_id_sha256"], label="spawn call digest")
        child_digests = timeline["child_terminal_sha256s"]
        if (
            not isinstance(child_digests, list)
            or len(child_digests) != 2
            or len(set(child_digests)) != 2
        ):
            raise CodingHostReceiptError(f"coding trial {number} child digests are malformed")
        for digest in child_digests:
            _digest(digest, label="child terminal digest")
        order = [
            _integer(timeline[field], label=field, minimum=0, maximum=1_000_000)
            for field in (
                "last_child_terminal_event_index",
                "spawn_result_event_index",
                "final_json_event_index",
                "parent_terminal_event_index",
            )
        ]
        if order != sorted(order) or len(order) != len(set(order)):
            raise CodingHostReceiptError(f"coding trial {number} synthesis order is invalid")

        patch = trial["patch"]
        if not isinstance(patch, dict):
            raise CodingHostReceiptError(f"coding trial {number} patch receipt must be an object")
        _strict_keys(
            patch,
            required={"changed_files", "changed_lines", "replacement_sha256"},
            label=f"coding trial {number} patch receipt",
        )
        changed_files = patch["changed_files"]
        if (
            not isinstance(changed_files, list)
            or any(not isinstance(item, str) for item in changed_files)
            or len(changed_files) != len(set(changed_files))
            or sorted(changed_files) != policy["allowed_changes"]
        ):
            raise CodingHostReceiptError(f"coding trial {number} changed file scope does not match")
        _integer(
            patch["changed_lines"],
            label="coding changed_lines",
            minimum=1,
            maximum=policy["max_changed_lines"],
        )
        replacements = patch["replacement_sha256"]
        if not isinstance(replacements, dict) or sorted(replacements) != policy["allowed_changes"]:
            raise CodingHostReceiptError(
                f"coding trial {number} replacement digests are incomplete"
            )
        for digest in replacements.values():
            _digest(digest, label="replacement source digest")

        image = trial["sandbox_image"]
        if not isinstance(image, dict):
            raise CodingHostReceiptError(f"coding trial {number} image receipt must be an object")
        _strict_keys(
            image,
            required={"reference", "image_id"},
            label=f"coding trial {number} image receipt",
        )
        if (
            image["reference"] != policy["sandbox_image_reference"]
            or image["image_id"] != expected_image_id
            or not isinstance(image["image_id"], str)
            or not _IMAGE_ID_RE.fullmatch(image["image_id"])
        ):
            raise CodingHostReceiptError(f"coding trial {number} image is not release pinned")

        test_receipts = trial["host_test_receipts"]
        if not isinstance(test_receipts, list) or len(test_receipts) != 3:
            raise CodingHostReceiptError(f"coding trial {number} needs exactly three host tests")
        for test_index, (test_receipt, expected_command) in enumerate(
            zip(test_receipts, policy["test_commands"], strict=True)
        ):
            if not isinstance(test_receipt, dict):
                raise CodingHostReceiptError("coding host-test command receipt must be an object")
            _strict_keys(
                test_receipt,
                required={"command", "exit_code", "timed_out", "duration_ms", "passed"},
                label=f"coding trial {number} host test {test_index}",
            )
            if test_receipt["command"] != expected_command:
                raise CodingHostReceiptError(f"coding trial {number} test command was substituted")
            if (
                test_receipt["exit_code"] != 0
                or test_receipt["timed_out"] is not False
                or test_receipt["passed"] is not True
            ):
                raise CodingHostReceiptError(f"coding trial {number} host test did not pass")
            _finite_number(test_receipt["duration_ms"], label="host test duration_ms")

        evidence_trials.append(
            {
                "trial": number,
                "observation_sha256": trial["observation_sha256"],
                "patch": patch,
                "patch_sha256": sha256(patch),
                "sandbox_image": image,
                "sandbox_image_sha256": sha256(image),
                "host_test_receipts": test_receipts,
                "host_test_receipts_sha256": sha256(test_receipts),
            }
        )
    if set(by_trial) != {1, 2, 3}:
        raise CodingHostReceiptError("coding host-test trial set is incomplete")
    return {
        "schema_version": EVIDENCE_SCHEMA,
        "receipt_schema": RECEIPT_SCHEMA,
        "receipt_file_sha256": file_sha256,
        "receipt_payload_sha256": sha256(payload),
        "host_attestation_key_id": hashlib.sha256(hmac_key.encode("utf-8")).hexdigest()[:24],
        "suite_id": SUITE_ID,
        "scenario_id": SCENARIO_ID,
        "scenario_contract_sha256": expected_scenario_contract_sha256,
        "observations_sha256": expected_observations_sha256,
        "validator_policy": policy,
        "trials": sorted(evidence_trials, key=lambda item: item["trial"]),
    }
