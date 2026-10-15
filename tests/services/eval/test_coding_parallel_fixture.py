from __future__ import annotations

import hashlib
import json
import subprocess
from pathlib import Path
from typing import Any

import pytest

from scripts import judge_real_agent_receipts as release_judge
from scripts import real_agent_scenario_runner as real_runner
from scripts.eval_fixtures import verify_coding_parallel_fixture as coding_verifier
from scripts.eval_fixtures.coding_host_test_receipt import validator_policy
from scripts.eval_fixtures.verify_coding_parallel_fixture import (
    CASE_ROOT,
    CodingFixtureError,
    _accepted_execution_checks,
    _safe_write_receipt,
    load_contract,
    materialize_candidate,
    parse_candidate_answer,
    run_tests_in_sandbox,
    validate_live_event_timeline,
    validate_observations,
    validate_patch_artifact,
)

SCENARIO_PATH = CASE_ROOT / "scenario.json"
TEMPLATE = CASE_ROOT / "repository"
COLLECTOR_KEY = "coding-collector-key-for-live-tests-00000001"
GOLDEN_KEY = "coding-golden-key-for-live-tests-0000000002"
HOST_KEY = "coding-host-test-key-for-live-tests-00000003"
RUNTIME_ATTESTATION = "coding-runtime-attestation-fixture"
SUITE_NONCE = "c" * 64


def _require_pinned_sandbox_image() -> None:
    """Skip live container tests when the exact local prerequisite is absent."""
    image = validator_policy()["sandbox_image_reference"]
    try:
        result = subprocess.run(
            ["docker", "image", "inspect", image],
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=3,
        )
    except (OSError, subprocess.TimeoutExpired):
        pytest.skip("pinned coding sandbox image is not available locally")
    if result.returncode != 0:
        pytest.skip("pinned coding sandbox image is not available locally")


def _reference_replacements() -> dict[str, str]:
    idempotency = (TEMPLATE / "src/settlement/idempotency.py").read_text(encoding="utf-8")
    idempotency = idempotency.replace(
        'json.dumps(identity, ensure_ascii=True, separators=(",", ":"))',
        'json.dumps(identity, ensure_ascii=True, sort_keys=True, separators=(",", ":"))',
    )
    allocation = (TEMPLATE / "src/settlement/allocation.py").read_text(encoding="utf-8")
    allocation = allocation.replace(
        """    total_weight = sum(weight for _, weight in beneficiaries)\n    return {\n        beneficiary_id: total_cents * weight // total_weight\n        for beneficiary_id, weight in beneficiaries\n    }\n""",
        """    total_weight = sum(weight for _, weight in beneficiaries)\n    allocations: dict[str, int] = {}\n    remainders: list[tuple[int, str]] = []\n    for beneficiary_id, weight in beneficiaries:\n        weighted_cents = total_cents * weight\n        allocations[beneficiary_id] = weighted_cents // total_weight\n        remainders.append((weighted_cents % total_weight, beneficiary_id))\n\n    unallocated = total_cents - sum(allocations.values())\n    for _, beneficiary_id in sorted(remainders, key=lambda item: (-item[0], item[1]))[\n        :unallocated\n    ]:\n        allocations[beneficiary_id] += 1\n    return allocations\n""",
    )
    return {
        "src/settlement/allocation.py": allocation,
        "src/settlement/idempotency.py": idempotency,
    }


def _candidate_answer(*, replacements: dict[str, str] | None = None) -> dict[str, Any]:
    return {
        "status": "proposed_patch",
        "changed_files": [
            "src/settlement/allocation.py",
            "src/settlement/idempotency.py",
        ],
        "investigated_areas": ["allocation", "idempotency"],
        "diagnoses": [
            {
                "area": "allocation",
                "root_cause": "Floor division leaves cents unassigned.",
                "evidence": [
                    "allocation examples",
                    "community-doublecheck:doublecheck",
                ],
            },
            {
                "area": "idempotency",
                "root_cause": "Equivalent nested mappings serialize in insertion order.",
                "evidence": [
                    "reordered replay example",
                    "community-engineering-reviewers:security-reviewer",
                ],
            },
        ],
        "rejected_hint_ids": [
            "INCREASE_IDEMPOTENCY_TTL",
            "ROUND_EACH_SHARE_INDEPENDENTLY",
        ],
        "replacement_files": replacements or _reference_replacements(),
        "validation_commands": load_contract()["test_commands"],
        "remaining_verification_boundary": "Isolated host tests are pending.",
    }


def _candidate_output(answer: dict[str, Any]) -> str:
    return f"<FINAL_JSON>{json.dumps(answer, sort_keys=True)}</FINAL_JSON>"


def _events(
    scenario: dict[str, Any], trial_number: int, answer: dict[str, Any]
) -> list[dict[str, Any]]:
    attempt = f"attempt-code-{trial_number}"
    definitions = ["a" * 64, "b" * 64]
    profiles = scenario["required_agent_ids"]
    canonical_arguments = real_runner._canonical_delegation_arguments(scenario)
    assert canonical_arguments is not None

    return [
        {
            "event_type": "tool_call_start",
            "data": {
                "tool_call_id": f"spawn-{trial_number}",
                "name": "spawn_subagent",
                "arguments": canonical_arguments,
            },
        },
        *[
            {
                "event_type": "subagent_started",
                "data": {
                    "agent_id": f"child-{trial_number}-{index}",
                    "profile_id": profile,
                    "source_plugin": profile.split(":", 1)[0],
                    "definition_sha256": definitions[index],
                    "dispatch_index": index,
                    "attempt_id": attempt,
                    "started_monotonic_ms": 1000.0 + index * 10,
                },
            }
            for index, profile in enumerate(profiles)
        ],
        *[
            {
                "event_type": "subagent_finished",
                "data": {
                    "agent_id": f"child-{trial_number}-{index}",
                    "profile_id": profile,
                    "source_plugin": profile.split(":", 1)[0],
                    "definition_sha256": definitions[index],
                    "dispatch_index": index,
                    "attempt_id": attempt,
                    "status": "completed",
                    "started_monotonic_ms": 1000.0 + index * 10,
                    "finished_monotonic_ms": 1200.0 + index * 10,
                    "duration_ms": 200.0,
                    "tool_calls": 0,
                },
            }
            for index, profile in enumerate(profiles)
        ],
        {
            "event_type": "tool_call_result",
            "data": {
                "tool_call_id": f"spawn-{trial_number}",
                "name": "spawn_subagent",
                "success": True,
                "status": "completed",
                "side_effect_state": "read_only",
            },
        },
        {
            "event_type": "tool_call_end",
            "data": {
                "tool_call_id": f"spawn-{trial_number}",
                "name": "spawn_subagent",
                "status": "completed",
                "side_effect_state": "read_only",
            },
        },
        {"event_type": "text_delta", "data": {"delta": _candidate_output(answer)}},
        {
            "event_type": "run_finished",
            "data": {"attempt_id": attempt, "status": "completed", "duration_ms": 250},
        },
    ]


def _trial(scenario: dict[str, Any], trial_number: int, answer: dict[str, Any]) -> dict[str, Any]:
    observation = real_runner.summarize_sse_events(
        _events(scenario, trial_number, answer),
        stream_sha256=hashlib.sha256(f"stream-{trial_number}".encode()).hexdigest(),
    )
    observation.update(
        {
            "scenario_id": scenario["scenario_id"],
            "trial": trial_number,
            "session_id": f"session-code-{trial_number}",
            "prompt_sha256": real_runner._sha256_text(real_runner._candidate_prompt(scenario)),
        }
    )
    observation["observation_sha256"] = real_runner._sha256(observation)
    return observation


def _observations(
    tmp_path: Path,
    *,
    answer: dict[str, Any] | None = None,
    mutate_trial: Any = None,
) -> Path:
    scenarios = real_runner.load_scenarios(SCENARIO_PATH)
    scenario = scenarios["scenarios"][0]
    source_artifacts = real_runner.verify_source_artifacts(scenarios, scenario_directory=CASE_ROOT)
    trials = [_trial(scenario, index, answer or _candidate_answer()) for index in range(1, 4)]
    if mutate_trial:
        mutate_trial(trials[0])
        unsigned = dict(trials[0])
        unsigned.pop("observation_sha256")
        trials[0]["observation_sha256"] = real_runner._sha256(unsigned)
    document = {
        "schema_version": real_runner.OBSERVATION_SCHEMA,
        "suite_id": scenarios["suite_id"],
        "scenario_contract_sha256": real_runner._sha256(scenarios),
        "collector": {
            "transport": "gateway-sse",
            "candidate_model_default": "fixture-provider",
            "semantic_verdicts_emitted": False,
        },
        "source_artifacts": source_artifacts,
        "trials": trials,
    }
    document["observations_sha256"] = real_runner._sha256(document)
    path = tmp_path / "observations.json"
    path.write_text(json.dumps(document), encoding="utf-8")
    return path


def _live_observations(tmp_path: Path) -> Path:
    scenarios = real_runner.load_scenarios(SCENARIO_PATH)
    scenario = scenarios["scenarios"][0]
    source_artifacts = real_runner.verify_source_artifacts(scenarios, scenario_directory=CASE_ROOT)
    plugin_definitions = real_runner.verify_plugin_definitions(scenarios)
    plugin_hashes = {
        item["qualified_agent_id"]: item["definition_sha256"] for item in plugin_definitions
    }
    trials: list[dict[str, Any]] = []
    raw_trials: list[dict[str, Any]] = []
    for trial_number in range(1, 4):
        events = _events(scenario, trial_number, _candidate_answer())
        for event in events:
            data = event.get("data", {})
            if event["event_type"] in real_runner.TOOL_START_EVENTS:
                data["attempt_id"] = f"attempt-code-{trial_number}"
            if event["event_type"] in real_runner.TOOL_RESULT_EVENTS:
                data["attempt_id"] = f"attempt-code-{trial_number}"
                data["side_effect_state"] = "none"
            profile_id = data.get("profile_id")
            if profile_id in plugin_hashes:
                data["definition_sha256"] = plugin_hashes[profile_id]
            if event["event_type"] == "run_finished":
                data["run_id"] = f"run-code-{trial_number}"
                data["metadata"] = {
                    "terminal_envelope": {
                        "attempt_id": f"attempt-code-{trial_number}",
                        "run_id": f"run-code-{trial_number}",
                        "tenant_id": "tenant-code-fixture",
                        "status": "succeeded",
                    }
                }
        payloads = [json.dumps(event, separators=(",", ":")) for event in events]
        stream_hasher = hashlib.sha256()
        for payload in payloads:
            stream_hasher.update(payload.encode())
            stream_hasher.update(b"\n")
        stream_sha256 = stream_hasher.hexdigest()
        observation = {
            "scenario_id": scenario["scenario_id"],
            "trial": trial_number,
            "session_id": f"live-session-code-{trial_number}",
            "suite_nonce": SUITE_NONCE,
            "collector_challenge": hashlib.sha256(
                f"coding-challenge:{trial_number}".encode()
            ).hexdigest(),
            "prompt_sha256": real_runner._sha256_text(real_runner._candidate_prompt(scenario)),
            **real_runner.summarize_sse_events(events, stream_sha256=stream_sha256),
        }
        observation["observation_sha256"] = real_runner._sha256(observation)
        trials.append(observation)
        raw_trial = {
            "scenario_id": scenario["scenario_id"],
            "trial": trial_number,
            "session_id": observation["session_id"],
            "suite_nonce": SUITE_NONCE,
            "collector_challenge": observation["collector_challenge"],
            "raw_sse_payloads": payloads,
            "stream_sha256": stream_sha256,
        }
        raw_trial["raw_trial_sha256"] = real_runner._sha256(raw_trial)
        raw_trials.append(raw_trial)
    raw_document = {
        "schema_version": real_runner.RAW_SSE_SCHEMA,
        "suite_id": scenarios["suite_id"],
        "suite_nonce": SUITE_NONCE,
        "trials": raw_trials,
    }
    raw_document["raw_sse_sha256"] = real_runner._sha256(raw_document)
    raw_document["collector_attestation"] = real_runner._collector_attestation(
        raw_document, key=COLLECTOR_KEY
    )
    raw_path = tmp_path / "coding-observations.raw-sse.json"
    real_runner._safe_write_json(raw_path, raw_document)
    document = {
        "schema_version": real_runner.OBSERVATION_SCHEMA,
        "suite_id": scenarios["suite_id"],
        "scenario_contract_sha256": real_runner._sha256(scenarios),
        "suite_nonce": SUITE_NONCE,
        "collector": {
            "transport": "gateway-sse",
            "candidate_model_default": "fixture-provider",
            "semantic_verdicts_emitted": False,
        },
        "source_artifacts": source_artifacts,
        "plugin_definitions": plugin_definitions,
        "runtime_binding": {
            "gateway_health_sha256": "3" * 64,
            "authenticated_tool_catalog_sha256": "4" * 64,
            "operator_container_runtime_attestation_sha256": real_runner._sha256_text(
                RUNTIME_ATTESTATION
            ),
        },
        "raw_sse_artifact": {
            "file_name": raw_path.name,
            "content_sha256": hashlib.sha256(raw_path.read_bytes()).hexdigest(),
            "mode": "0600",
        },
        "trials": trials,
    }
    document["observations_sha256"] = real_runner._sha256(document)
    document["collector_attestation"] = real_runner._collector_attestation(
        document, key=COLLECTOR_KEY
    )
    path = tmp_path / "coding-observations.json"
    real_runner._safe_write_json(path, document)
    return path


def _configure_live_environment(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GENERAL_AGENT_COLLECTOR_HMAC_KEY", COLLECTOR_KEY)
    monkeypatch.setenv("GENERAL_AGENT_GOLDEN_HMAC_KEY", GOLDEN_KEY)
    monkeypatch.setenv("GENERAL_AGENT_CODING_HOST_TEST_HMAC_KEY", HOST_KEY)
    monkeypatch.setenv("GENERAL_AGENT_RUNTIME_ATTESTATION", RUNTIME_ATTESTATION)
    monkeypatch.setenv(
        "GENERAL_AGENT_EXPECTED_RUNTIME_ATTESTATION_SHA256",
        real_runner._sha256_text(RUNTIME_ATTESTATION),
    )
    monkeypatch.setenv("GENERAL_AGENT_SUITE_NONCE", SUITE_NONCE)


def _fake_live_sandbox(
    workspace: Path, contract: dict[str, Any], *, image: str
) -> tuple[dict[str, str], list[dict[str, Any]]]:
    policy = validator_policy()
    assert image == policy["sandbox_image_reference"]
    image_id = "sha256:" + image.rsplit("@sha256:", 1)[1]
    return (
        {"reference": image, "image_id": image_id},
        [
            {
                "command": command,
                "exit_code": 0,
                "timed_out": False,
                "duration_ms": 12.5 + index,
                "passed": True,
            }
            for index, command in enumerate(contract["test_commands"])
        ],
    )


def _build_live_chain(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    *,
    real_docker: bool = False,
) -> tuple[dict[str, Any], dict[str, Any], Path, Path, Path]:
    _configure_live_environment(monkeypatch)
    observation_path = _live_observations(tmp_path)
    if not real_docker:
        monkeypatch.setattr(coding_verifier, "run_tests_in_sandbox", _fake_live_sandbox)
    host_payload = validate_observations(
        observation_path,
        live_collected=True,
        collector_key=COLLECTOR_KEY,
        runtime_attestation=RUNTIME_ATTESTATION,
    )
    host_receipt_path = tmp_path / "coding-host-test-receipt.json"
    _safe_write_receipt(host_receipt_path, host_payload, require_hmac=True)
    validation_path = tmp_path / "coding-golden.json"
    merged_path = tmp_path / "coding-merged.json"
    real_runner.validate(SCENARIO_PATH, observation_path, validation_path)
    merged = real_runner.merge(
        SCENARIO_PATH,
        observation_path,
        validation_path,
        merged_path,
        coding_host_test_receipt_path=host_receipt_path,
    )
    return host_payload, merged, observation_path, validation_path, host_receipt_path


def test_scenario_uses_real_schema_real_plugins_and_immutable_sources() -> None:
    scenarios = real_runner.load_scenarios(SCENARIO_PATH)
    scenario = scenarios["scenarios"][0]

    assert scenarios["schema_version"] == "real-agent-scenarios/v1"
    assert scenario["required_agent_ids"] == [
        "community-engineering-reviewers:security-reviewer",
        "community-doublecheck:doublecheck",
    ]
    assert scenario["repetitions"] == 3
    assert scenario["require_parallel"] is True
    assert "builtin:code" not in scenario["prompt"]
    assert "sort_keys=True" not in scenario["prompt"]
    assert "largest remainder" not in scenario["prompt"].casefold()
    assert len(real_runner.verify_source_artifacts(scenarios, scenario_directory=CASE_ROOT)) == 8


def test_patch_artifact_rejects_scope_expansion_and_self_reported_test_success() -> None:
    answer = _candidate_answer()
    answer["replacement_files"]["tests/test_service.py"] = "pass\n"
    answer["tests_passed"] = True

    with pytest.raises(CodingFixtureError, match="unsupported fields"):
        validate_patch_artifact(answer, load_contract())


def test_offline_observations_are_ineligible_for_acceptance(tmp_path: Path) -> None:
    receipt = validate_observations(_observations(tmp_path))

    assert receipt["validation_passed"] is False
    assert (
        receipt["acceptance_eligible"] is False
    )  # synthetic/offline receipts cannot claim live E2E
    assert receipt["passed"] is False
    assert receipt["trials"] == []
    assert "offline observations are not accepted" in receipt["errors"][0]


def test_offline_contract_checks_overlap_but_cannot_promote_it_to_live_evidence(
    tmp_path: Path,
) -> None:
    scenario = real_runner.load_scenarios(SCENARIO_PATH)["scenarios"][0]
    observation = _trial(scenario, 1, _candidate_answer())

    checks = _accepted_execution_checks(scenario, observation)
    timeline = validate_live_event_timeline(
        _events(scenario, 1, _candidate_answer()), load_contract()
    )
    offline = validate_observations(_observations(tmp_path))

    assert all(item["passed"] for item in checks)
    assert timeline["overlap_ms"] >= 25
    assert timeline["last_child_terminal_event_index"] < timeline["final_json_event_index"]
    assert offline["acceptance_eligible"] is False
    assert offline["passed"] is False


@pytest.mark.integration
def test_reference_patch_runs_real_tests_only_in_restricted_container(
    tmp_path: Path,
) -> None:
    _require_pinned_sandbox_image()
    workspace = tmp_path / "candidate"
    patch = materialize_candidate(workspace, _reference_replacements(), load_contract())

    image, receipts = run_tests_in_sandbox(
        workspace, load_contract(), image=validator_policy()["sandbox_image_reference"]
    )

    assert image["image_id"].startswith("sha256:")
    assert patch["changed_files"] == load_contract()["allowed_changes"]
    assert patch["changed_lines"] <= 48
    assert len(receipts) == 3
    assert all(receipt["passed"] for receipt in receipts)


@pytest.mark.integration
def test_unsolved_patch_cannot_pass_from_candidate_claims(tmp_path: Path) -> None:
    _require_pinned_sandbox_image()
    replacements = {
        path: (TEMPLATE / path).read_text(encoding="utf-8") + "\n# reviewed\n"
        for path in load_contract()["allowed_changes"]
    }
    validated = validate_patch_artifact(
        _candidate_answer(replacements=replacements), load_contract()
    )
    workspace = tmp_path / "candidate"
    materialize_candidate(workspace, validated, load_contract())
    _, receipts = run_tests_in_sandbox(
        workspace, load_contract(), image=validator_policy()["sandbox_image_reference"]
    )

    assert not all(receipt["passed"] for receipt in receipts)


def test_serial_or_wrong_profile_lifecycle_is_rejected_before_code_execution(
    tmp_path: Path,
) -> None:
    def break_parallel(trial: dict[str, Any]) -> None:
        trial["parallel_overlaps"] = [
            {"left_dispatch_index": 0, "right_dispatch_index": 1, "observed": False}
        ]

    receipt = validate_observations(_observations(tmp_path, mutate_trial=break_parallel))

    assert receipt["validation_passed"] is False
    assert receipt["trials"] == []


def test_timeline_rejects_serial_children_even_with_completed_status() -> None:
    scenario = real_runner.load_scenarios(SCENARIO_PATH)["scenarios"][0]
    events = _events(scenario, 1, _candidate_answer())
    for event in events:
        data = event.get("data", {})
        if event["event_type"] == "subagent_started" and data.get("dispatch_index") == 1:
            data["started_monotonic_ms"] = 1300.0
        if event["event_type"] == "subagent_finished" and data.get("dispatch_index") == 1:
            data["started_monotonic_ms"] = 1300.0
            data["finished_monotonic_ms"] = 1500.0

    with pytest.raises(CodingFixtureError, match="overlap"):
        validate_live_event_timeline(events, load_contract())


def test_timeline_accepts_production_bare_string_text_deltas() -> None:
    scenario = real_runner.load_scenarios(SCENARIO_PATH)["scenarios"][0]
    events = _events(scenario, 1, _candidate_answer())
    final_event = next(event for event in events if event["event_type"] == "text_delta")
    final_text = str(final_event["data"]["delta"])
    position = events.index(final_event)
    events[position : position + 1] = [
        {"event_type": "text_delta", "data": chunk}
        for chunk in (final_text[:10], final_text[10:])
    ]

    timeline = validate_live_event_timeline(events, load_contract())

    assert timeline["final_json_event_index"] > timeline["spawn_result_event_index"]


def test_raw_sse_has_a_separate_bound_while_regular_json_keeps_the_small_cap(
    tmp_path: Path,
) -> None:
    path = tmp_path / "large.json"
    payload = {"padding": "x" * (coding_verifier.MAX_JSON_BYTES + 1)}
    path.write_text(json.dumps(payload), encoding="utf-8")

    with pytest.raises(CodingFixtureError, match="oversized"):
        coding_verifier.load_json_object(path)

    assert coding_verifier.load_json_object(
        path,
        max_bytes=coding_verifier.MAX_RAW_SSE_JSON_BYTES,
    )["padding"].startswith("x")


def test_duplicate_json_keys_fail_closed(tmp_path: Path) -> None:
    path = tmp_path / "observations.json"
    path.write_text('{"schema_version":"x","schema_version":"y"}', encoding="utf-8")

    with pytest.raises(CodingFixtureError, match="duplicate JSON key"):
        validate_observations(path)


def test_final_json_parser_rejects_multiple_artifacts() -> None:
    output = _candidate_output(_candidate_answer()) * 2

    with pytest.raises(CodingFixtureError, match="exactly one FINAL_JSON"):
        parse_candidate_answer(output)


def test_final_receipt_uses_independent_host_test_hmac_contract(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("GENERAL_AGENT_CODING_HOST_TEST_HMAC_KEY", "h" * 32)
    payload = {
        "schema_version": "coding-agent-patch-validation/v1",
        "passed": False,
        "acceptance_eligible": False,
    }
    path = tmp_path / "coding-receipt.json"

    _safe_write_receipt(path, payload, require_hmac=True)
    document = json.loads(path.read_text(encoding="utf-8"))
    verified, strength = real_runner._verify_seal(document, hmac_key="h" * 32, require_hmac=True)

    assert verified == payload
    assert strength == "hmac-sha256"


@pytest.mark.integration
def test_live_three_trial_verifier_runner_merge_and_judge_preflight_chain(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _require_pinned_sandbox_image()
    host_payload, merged, _, _, host_receipt_path = _build_live_chain(
        tmp_path, monkeypatch, real_docker=True
    )

    assert host_payload["passed"] is True
    assert len(host_payload["trials"]) == 3
    assert all(len(trial["host_test_receipts"]) == 3 for trial in host_payload["trials"])
    evidence = merged["provenance"]["coding_host_test_evidence"]
    assert (
        evidence["receipt_file_sha256"]
        == hashlib.sha256(host_receipt_path.read_bytes()).hexdigest()
    )
    assert len(evidence["trials"]) == 3
    assert all(len(trial["host_test_receipts"]) == 3 for trial in evidence["trials"])
    prepared = release_judge.prepare_input(
        real_runner.load_scenarios(SCENARIO_PATH),
        merged,
        expected_receipt_sha256=merged["merged_receipt_sha256"],
        scenario_directory=CASE_ROOT,
        collector_hmac_key=COLLECTOR_KEY,
        golden_hmac_key=GOLDEN_KEY,
        coding_host_test_hmac_key=HOST_KEY,
        coding_host_test_receipt_path=host_receipt_path,
        expected_suite_nonce=SUITE_NONCE,
    )
    assert prepared.coding_host_test_evidence == evidence
    assert len(prepared.trials) == 3
    assert all(
        trial.deterministic_summary["coding_host_tests_verified"] is True
        for trial in prepared.trials
    )


def test_coding_merge_rejects_omitted_host_test_receipt(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _configure_live_environment(monkeypatch)
    observation_path = _live_observations(tmp_path)
    validation_path = tmp_path / "coding-golden.json"
    real_runner.validate(SCENARIO_PATH, observation_path, validation_path)

    with pytest.raises(real_runner.ScenarioContractError, match="requires"):
        real_runner.merge(
            SCENARIO_PATH,
            observation_path,
            validation_path,
            tmp_path / "merged.json",
        )


def test_coding_host_receipt_tamper_is_rejected_before_merge(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _, _, observation_path, validation_path, host_receipt_path = _build_live_chain(
        tmp_path, monkeypatch
    )
    document = json.loads(host_receipt_path.read_text(encoding="utf-8"))
    document["trials"][0]["host_test_receipts"][0]["passed"] = False
    real_runner._safe_write_json(host_receipt_path, document)

    with pytest.raises(real_runner.ScenarioContractError, match="seal does not match"):
        real_runner.merge(
            SCENARIO_PATH,
            observation_path,
            validation_path,
            tmp_path / "tampered-merged.json",
            coding_host_test_receipt_path=host_receipt_path,
        )


def test_coding_host_receipt_replacement_and_nonlive_fail_closed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    host_payload, _, observation_path, validation_path, host_receipt_path = _build_live_chain(
        tmp_path, monkeypatch
    )
    replacement_payload = dict(host_payload)
    replacement_payload["observations_sha256"] = "0" * 64
    _safe_write_receipt(host_receipt_path, replacement_payload, require_hmac=True)
    with pytest.raises(real_runner.ScenarioContractError, match="another observation set"):
        real_runner.merge(
            SCENARIO_PATH,
            observation_path,
            validation_path,
            tmp_path / "replaced-merged.json",
            coding_host_test_receipt_path=host_receipt_path,
        )

    nonlive_payload = dict(host_payload)
    nonlive_payload["acceptance_eligible"] = False
    nonlive_payload["passed"] = False
    _safe_write_receipt(host_receipt_path, nonlive_payload, require_hmac=True)
    with pytest.raises(real_runner.ScenarioContractError, match="non-live"):
        real_runner.merge(
            SCENARIO_PATH,
            observation_path,
            validation_path,
            tmp_path / "nonlive-merged.json",
            coding_host_test_receipt_path=host_receipt_path,
        )


def test_judge_reloads_actual_host_receipt_and_rejects_post_merge_tamper(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _, merged, _, _, host_receipt_path = _build_live_chain(tmp_path, monkeypatch)
    document = json.loads(host_receipt_path.read_text(encoding="utf-8"))
    document["trials"][2]["sandbox_image"]["image_id"] = "sha256:" + "f" * 64
    real_runner._safe_write_json(host_receipt_path, document)

    with pytest.raises(release_judge.ReleaseJudgeError, match="seal does not match"):
        release_judge.prepare_input(
            real_runner.load_scenarios(SCENARIO_PATH),
            merged,
            expected_receipt_sha256=merged["merged_receipt_sha256"],
            scenario_directory=CASE_ROOT,
            collector_hmac_key=COLLECTOR_KEY,
            golden_hmac_key=GOLDEN_KEY,
            coding_host_test_hmac_key=HOST_KEY,
            coding_host_test_receipt_path=host_receipt_path,
            expected_suite_nonce=SUITE_NONCE,
        )
