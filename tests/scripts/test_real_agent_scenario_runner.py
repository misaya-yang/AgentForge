from __future__ import annotations

import importlib.util
import json
import stat
from pathlib import Path
from typing import Any

import httpx
import pytest

ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts/real_agent_scenario_runner.py"
SPEC = importlib.util.spec_from_file_location("real_agent_scenario_runner", MODULE_PATH)
assert SPEC and SPEC.loader
runner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(runner)

LEGAL_SOURCE = "Article 577 fixture: breach permits continued performance or damages.\n"
FINANCE_SOURCE = "metric,value\ncurrent_assets,125\ncurrent_liabilities,100\n"
COLLECTOR_KEY = "collector-attestation-key-for-tests-000001"
GOLDEN_KEY = "golden-validation-key-for-runner-tests-000001"
RUNTIME_ATTESTATION = "compose-image-and-container-label-binding-test"
SUITE_NONCE = "a" * 64
DELEGATION_PROMPT_EXCLUDES = [
    "Return OK",
    "Do not research",
    "ignore binding",
    "independent answer without child",
]


def _contract() -> dict[str, Any]:
    legal_requirements = [
        {
            "agent_id": "community-doublecheck:doublecheck",
            "prompt_contains_all": ["Article 577", "remedies", "counterexample"],
            "prompt_excludes_all": DELEGATION_PROMPT_EXCLUDES,
            "min_prompt_chars": 160,
        },
        {
            "agent_id": "community-engineering-reviewers:system-architecture-reviewer",
            "prompt_contains_all": ["Article 577", "contract facts", "citations"],
            "prompt_excludes_all": DELEGATION_PROMPT_EXCLUDES,
            "min_prompt_chars": 160,
        },
    ]

    def task(requirement: dict[str, Any]) -> dict[str, Any]:
        prompt = (
            "Analyze independently and return an evidence-backed critique covering "
            + ", ".join(requirement["prompt_contains_all"])
            + ". Verify the supplied record carefully and report counterevidence. "
            + "Evidence detail. " * 12
        )
        return {
            "agent_id": requirement["agent_id"],
            "prompt": prompt,
            "description": "independent review",
        }

    canonical_arguments = {
        "tasks": [task(item) for item in legal_requirements],
        "max_concurrency": 2,
    }
    return {
        "schema_version": runner.SCENARIO_SCHEMA,
        "suite_id": "real.cross-domain.test",
        "scenarios": [
            {
                "scenario_id": "legal.contract-remedy",
                "domain": "law",
                "prompt": (
                    "Analyze the supplied contract facts. Return one <FINAL_JSON> object; "
                    "use the two named read-only specialists in parallel."
                ),
                "repetitions": 1,
                "required_agent_ids": [
                    "community-doublecheck:doublecheck",
                    "community-engineering-reviewers:system-architecture-reviewer",
                ],
                "delegation_task_requirements": legal_requirements,
                "canonical_delegation": {
                    **canonical_arguments,
                    "canonical_sha256": runner._sha256(canonical_arguments),
                },
                "require_parallel": True,
                "answer_locator": "final_json_tag",
                "source_artifacts": [
                    {
                        "artifact_id": "legal.article-577",
                        "path": "legal-source.txt",
                        "sha256": runner._sha256_text(LEGAL_SOURCE),
                    }
                ],
                "expected_assertions": [
                    {
                        "assertion_id": "law.article-exact",
                        "kind": "json_equals",
                        "path": "/law/article",
                        "expected": "Article 577",
                    },
                    {
                        "assertion_id": "law.remedies-exact",
                        "kind": "json_set_equals",
                        "path": "/law/remedies",
                        "expected": ["damages", "continued performance"],
                    },
                ],
            },
            {
                "scenario_id": "finance.credit-memo",
                "domain": "finance",
                "prompt": "Calculate the supplied issuer ratios and return only strict JSON.",
                "repetitions": 1,
                "required_agent_ids": [],
                "require_parallel": False,
                "answer_locator": "whole_output_json",
                "source_artifacts": [
                    {
                        "artifact_id": "finance.input-table",
                        "path": "finance-source.csv",
                        "sha256": runner._sha256_text(FINANCE_SOURCE),
                    }
                ],
                "expected_assertions": [
                    {
                        "assertion_id": "finance.current-ratio",
                        "kind": "json_number",
                        "path": "/ratios/current",
                        "expected": 1.25,
                        "absolute_tolerance": 0.001,
                    },
                    {
                        "assertion_id": "finance.flags-exact",
                        "kind": "json_set_equals",
                        "path": "/risk_flags",
                        "expected": ["negative_fcf", "covenant_headroom_low"],
                    },
                ],
            },
        ],
    }


def _trial(
    *, scenario_id: str, trial: int, candidate_output: str, offset: float = 0
) -> dict[str, Any]:
    value = {
        "scenario_id": scenario_id,
        "trial": trial,
        "session_id": f"session-{scenario_id}",
        "prompt_sha256": "1" * 64,
        "event_counts": {"run_finished": 1},
        "stream_sha256": "2" * 64,
        "attempt_ids": [f"attempt-{scenario_id}"],
        "candidate_output": candidate_output,
        "candidate_output_sha256": runner._sha256_text(candidate_output),
        "subagent_starts": [],
        "subagent_finishes": [],
        "parallel_overlaps": [],
        "tool_starts": [],
        "tool_results": [],
        "terminal_events": [
            {
                "event_type": "run_finished",
                "attempt_id": f"attempt-{scenario_id}",
                "duration_ms": 10 + offset,
            }
        ],
    }
    value["observation_sha256"] = runner._sha256(value)
    return value


def _observations(
    contract: dict[str, Any],
    directory: Path,
    *,
    collector_key: str,
    runtime_attestation: str,
) -> dict[str, Any]:
    legal = _trial(
        scenario_id="legal.contract-remedy",
        trial=1,
        candidate_output=(
            '<FINAL_JSON>{"law":{"article":"Article 577",'
            '"remedies":["continued performance","damages"]}}</FINAL_JSON>'
        ),
    )
    finance = _trial(
        scenario_id="finance.credit-memo",
        trial=1,
        candidate_output=(
            '{"ratios":{"current":1.2504},"risk_flags":["covenant_headroom_low","negative_fcf"]}'
        ),
    )
    suite_nonce = SUITE_NONCE
    raw_trials: list[dict[str, Any]] = []
    scenario_by_id = {item["scenario_id"]: item for item in contract["scenarios"]}
    for index, trial in enumerate((legal, finance), start=1):
        scenario = scenario_by_id[trial["scenario_id"]]
        challenge = f"{index:x}" * 64
        attempt_id = trial["attempt_ids"][0]
        events = [
            {"event_type": "text_delta", "data": {"delta": trial["candidate_output"]}},
            {
                "event_type": "run_finished",
                "data": {"attempt_id": attempt_id, "duration_ms": 10},
            },
        ]
        payloads = [json.dumps(event, separators=(",", ":")) for event in events]
        stream_hasher = runner.hashlib.sha256()
        for payload in payloads:
            stream_hasher.update(payload.encode())
            stream_hasher.update(b"\n")
        stream_sha = stream_hasher.hexdigest()
        summarized = runner.summarize_sse_events(events, stream_sha256=stream_sha)
        trial.update(summarized)
        trial.update(
            {
                "suite_nonce": suite_nonce,
                "collector_challenge": challenge,
                "prompt_sha256": runner._sha256_text(runner._candidate_prompt(scenario)),
            }
        )
        trial["observation_sha256"] = runner._sha256(
            {key: value for key, value in trial.items() if key != "observation_sha256"}
        )
        raw_trial = {
            "scenario_id": trial["scenario_id"],
            "trial": trial["trial"],
            "session_id": trial["session_id"],
            "suite_nonce": suite_nonce,
            "collector_challenge": challenge,
            "raw_sse_payloads": payloads,
            "stream_sha256": stream_sha,
        }
        raw_trial["raw_trial_sha256"] = runner._sha256(raw_trial)
        raw_trials.append(raw_trial)
    raw_document = {
        "schema_version": runner.RAW_SSE_SCHEMA,
        "suite_id": contract["suite_id"],
        "suite_nonce": suite_nonce,
        "trials": raw_trials,
    }
    raw_document["raw_sse_sha256"] = runner._sha256(raw_document)
    raw_document["collector_attestation"] = runner._collector_attestation(
        raw_document, key=collector_key
    )
    raw_path = directory / "observations.raw-sse.json"
    runner._safe_write_json(raw_path, raw_document)
    document = {
        "schema_version": runner.OBSERVATION_SCHEMA,
        "suite_id": contract["suite_id"],
        "scenario_contract_sha256": runner._sha256(contract),
        "suite_nonce": suite_nonce,
        "collector": {
            "transport": "gateway-sse",
            "candidate_model_default": "provider-model",
            "semantic_verdicts_emitted": False,
        },
        "source_artifacts": [
            {
                "scenario_id": "legal.contract-remedy",
                "artifact_id": "legal.article-577",
                "relative_path": "legal-source.txt",
                "content_sha256": runner._sha256_text(LEGAL_SOURCE),
                "size_bytes": len(LEGAL_SOURCE.encode()),
            },
            {
                "scenario_id": "finance.credit-memo",
                "artifact_id": "finance.input-table",
                "relative_path": "finance-source.csv",
                "content_sha256": runner._sha256_text(FINANCE_SOURCE),
                "size_bytes": len(FINANCE_SOURCE.encode()),
            },
        ],
        "plugin_definitions": runner.verify_plugin_definitions(contract),
        "runtime_binding": {
            "gateway_health_sha256": "3" * 64,
            "authenticated_tool_catalog_sha256": "4" * 64,
            "operator_container_runtime_attestation_sha256": runner._sha256_text(
                runtime_attestation
            ),
        },
        "raw_sse_artifact": {
            "file_name": raw_path.name,
            "content_sha256": runner.hashlib.sha256(raw_path.read_bytes()).hexdigest(),
            "mode": "0600",
        },
        "trials": [legal, finance],
    }
    document["observations_sha256"] = runner._sha256(document)
    document["collector_attestation"] = runner._collector_attestation(document, key=collector_key)
    return document


def _write(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value), encoding="utf-8")


def _write_sources(path: Path) -> None:
    (path / "legal-source.txt").write_text(LEGAL_SOURCE, encoding="utf-8")
    (path / "finance-source.csv").write_text(FINANCE_SOURCE, encoding="utf-8")


def _configure_attestation(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GENERAL_AGENT_COLLECTOR_HMAC_KEY", COLLECTOR_KEY)
    monkeypatch.setenv("GENERAL_AGENT_GOLDEN_HMAC_KEY", GOLDEN_KEY)
    monkeypatch.setenv("GENERAL_AGENT_RUNTIME_ATTESTATION", RUNTIME_ATTESTATION)
    monkeypatch.setenv(
        "GENERAL_AGENT_EXPECTED_RUNTIME_ATTESTATION_SHA256",
        runner._sha256_text(RUNTIME_ATTESTATION),
    )
    monkeypatch.setenv("GENERAL_AGENT_SUITE_NONCE", SUITE_NONCE)


def _reseal_observations(document: dict[str, Any]) -> None:
    document.pop("collector_attestation", None)
    document.pop("observations_sha256", None)
    document["observations_sha256"] = runner._sha256(document)
    document["collector_attestation"] = runner._collector_attestation(document, key=COLLECTOR_KEY)


def _reseal_raw(document: dict[str, Any]) -> None:
    document.pop("collector_attestation", None)
    document.pop("raw_sse_sha256", None)
    document["raw_sse_sha256"] = runner._sha256(document)
    document["collector_attestation"] = runner._collector_attestation(document, key=COLLECTOR_KEY)


def test_load_scenarios_rejects_duplicate_json_keys(tmp_path: Path) -> None:
    path = tmp_path / "scenarios.json"
    path.write_text(
        '{"schema_version":"real-agent-scenarios/v1",'
        '"suite_id":"one","suite_id":"two","scenarios":[]}',
        encoding="utf-8",
    )

    with pytest.raises(runner.ScenarioContractError, match="duplicate object key"):
        runner.load_scenarios(path)


def test_load_scenarios_requires_exact_task_contract_for_each_agent(tmp_path: Path) -> None:
    contract = _contract()
    contract["scenarios"][0].pop("delegation_task_requirements")
    path = tmp_path / "scenarios.json"
    _write(path, contract)

    with pytest.raises(runner.ScenarioContractError, match="delegation_task_requirements"):
        runner.load_scenarios(path)

    contract = _contract()
    contract["scenarios"][0]["delegation_task_requirements"][1]["agent_id"] = contract["scenarios"][
        0
    ]["required_agent_ids"][0]
    _write(path, contract)
    with pytest.raises(runner.ScenarioContractError, match="exactly match|required agents"):
        runner.load_scenarios(path)

    contract = _contract()
    contract["scenarios"][0].pop("canonical_delegation")
    _write(path, contract)
    with pytest.raises(runner.ScenarioContractError, match="canonical_delegation"):
        runner.load_scenarios(path)


def test_load_scenarios_rejects_tampered_canonical_task_digest(tmp_path: Path) -> None:
    contract = _contract()
    contract["scenarios"][0]["canonical_delegation"]["tasks"][0]["prompt"] += " altered"
    path = tmp_path / "scenarios.json"
    _write(path, contract)

    with pytest.raises(runner.ScenarioContractError, match="digest does not match"):
        runner.load_scenarios(path)


def test_summarize_sse_records_facts_without_semantic_scores() -> None:
    events = [
        {
            "event_type": "subagent_started",
            "data": {
                "agent_id": "child-a",
                "profile_id": "plugin:a",
                "dispatch_index": 0,
                "attempt_id": "attempt-1",
                "started_monotonic_ms": 100.0,
            },
        },
        {
            "event_type": "subagent_started",
            "data": {
                "agent_id": "child-b",
                "profile_id": "plugin:b",
                "dispatch_index": 1,
                "attempt_id": "attempt-1",
                "started_monotonic_ms": 110.0,
            },
        },
        {
            "event_type": "tool_call_start",
            "data": {"tool_call_id": "call-1", "name": "spawn_subagent"},
        },
        {
            "event_type": "subagent_finished",
            "data": {
                "agent_id": "child-a",
                "profile_id": "plugin:a",
                "dispatch_index": 0,
                "attempt_id": "attempt-1",
                "status": "completed",
                "started_monotonic_ms": 100.0,
                "finished_monotonic_ms": 160.0,
                "tool_calls": 0,
            },
        },
        {
            "event_type": "subagent_finished",
            "data": {
                "agent_id": "child-b",
                "profile_id": "plugin:b",
                "dispatch_index": 1,
                "attempt_id": "attempt-1",
                "status": "completed",
                "started_monotonic_ms": 110.0,
                "finished_monotonic_ms": 170.0,
                "tool_calls": 0,
            },
        },
        {
            "event_type": "text_delta",
            "data": {"delta": "candidate sk-thismustberedacted"},
        },
        {
            "event_type": "run_finished",
            "data": {
                "attempt_id": "attempt-1",
                "metadata": {"terminal_envelope": {"tenant_id": "tenant-a"}},
            },
        },
    ]

    receipt = runner.summarize_sse_events(events, stream_sha256="a" * 64)

    assert receipt["parallel_overlaps"] == [
        {
            "left_dispatch_index": 0,
            "right_dispatch_index": 1,
            "observed": True,
            "overlap_ms": 50.0,
        }
    ]
    assert receipt["candidate_output"] == "candidate [REDACTED]"
    assert receipt["terminal_events"][0]["terminal_envelope"]["tenant_id"] == "tenant-a"
    assert "candidate_scores" not in receipt
    assert "hard_gates" not in receipt
    assert "golden_passed" not in receipt


def test_collect_trial_sends_prompt_not_goldens_and_records_real_sse() -> None:
    scenario = _contract()["scenarios"][0]

    def handler(request: httpx.Request) -> httpx.Response:
        payload = json.loads(request.content)
        assert payload["message"] == runner._candidate_prompt(scenario)
        assert scenario["prompt"] in payload["message"]
        assert "expected_assertions" not in payload
        assert request.headers["Authorization"] == "Bearer opaque-token"
        frames = [
            {
                "event_type": "text_delta",
                "data": {
                    "delta": '<FINAL_JSON>{"law":{"article":"Article 577",'
                    '"remedies":["damages","continued performance"]}}</FINAL_JSON>'
                },
            },
            {
                "event_type": "run_finished",
                "data": {"attempt_id": "attempt-live", "run_id": "run-live"},
            },
        ]
        body = "".join(f"data: {json.dumps(item)}\n\n" for item in frames)
        return httpx.Response(200, text=body, headers={"content-type": "text/event-stream"})

    with httpx.Client(transport=httpx.MockTransport(handler)) as client:
        receipt, raw_trial = runner._collect_trial(
            client,
            api_prefix="http://gateway.test/api/v1",
            token="opaque-token",
            scenario=scenario,
            trial_number=1,
            default_model="provider-model",
            suite_nonce="f" * 64,
        )

    assert receipt["terminal_events"] == [
        {
            "attempt_id": "attempt-live",
            "run_id": "run-live",
            "event_type": "run_finished",
            "ordinal": 1,
        }
    ]
    serialized = json.dumps(receipt)
    assert "opaque-token" not in serialized
    assert "Article 577" in receipt["candidate_output"]
    assert receipt["observation_sha256"] == runner._sha256(
        {key: value for key, value in receipt.items() if key != "observation_sha256"}
    )
    assert raw_trial["suite_nonce"] == "f" * 64


def test_independent_goldens_seal_and_merge_law_and_finance(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    contract = _contract()
    observations = _observations(
        contract,
        tmp_path,
        collector_key=COLLECTOR_KEY,
        runtime_attestation=RUNTIME_ATTESTATION,
    )
    scenario_path = tmp_path / "scenarios.json"
    observation_path = tmp_path / "observations.json"
    validation_path = tmp_path / "validation.json"
    merged_path = tmp_path / "merged.json"
    _write(scenario_path, contract)
    _write(observation_path, observations)
    _write_sources(tmp_path)
    _configure_attestation(monkeypatch)
    validation = runner.validate(scenario_path, observation_path, validation_path)
    merged = runner.merge(
        scenario_path,
        observation_path,
        validation_path,
        merged_path,
    )

    assert all(trial["golden_passed"] for trial in validation["trials"])
    assert validation["validator"]["semantic_model_used"] is False
    assert validation["seal"]["hmac_algorithm"] == "hmac-sha256"
    assert merged["provenance"]["validation_seal_strength"] == "hmac-sha256"
    assert len(merged["trials"]) == 2
    assert stat.S_IMODE(validation_path.stat().st_mode) == 0o600
    assert stat.S_IMODE(merged_path.stat().st_mode) == 0o600


def test_candidate_self_reported_perfect_score_cannot_override_golden() -> None:
    scenario = _contract()["scenarios"][0]
    observation = _trial(
        scenario_id="legal.contract-remedy",
        trial=1,
        candidate_output=(
            '<FINAL_JSON>{"passed":true,"score":100,'
            '"law":{"article":"invented","remedies":[]}}</FINAL_JSON>'
        ),
    )

    result = runner._validated_trial(scenario, observation)

    assert result["golden_passed"] is False
    assert [item["passed"] for item in result["assertions"]] == [False, False]


def _valid_execution_receipt() -> tuple[dict[str, Any], dict[str, Any]]:
    contract = _contract()
    scenario = contract["scenarios"][0]
    plugin_hashes = {
        item["qualified_agent_id"]: item["definition_sha256"]
        for item in runner.verify_plugin_definitions(contract)
    }
    required = scenario["required_agent_ids"]
    attempt_id = "attempt-execution"
    observation = _trial(
        scenario_id=scenario["scenario_id"],
        trial=1,
        candidate_output='<FINAL_JSON>{"law":{}}</FINAL_JSON>',
    )
    observation["attempt_ids"] = [attempt_id]
    observation["prompt_sha256"] = runner._sha256_text(runner._candidate_prompt(scenario))
    observation["subagent_starts"] = [
        {
            "agent_id": f"child-{index}",
            "profile_id": profile,
            "source_plugin": profile.split(":", 1)[0],
            "definition_sha256": plugin_hashes[profile],
            "dispatch_index": index,
            "attempt_id": attempt_id,
            "started_monotonic_ms": 100 + index,
            "ordinal": 1 + index,
        }
        for index, profile in enumerate(required)
    ]
    observation["subagent_finishes"] = [
        {
            **item,
            "status": "completed",
            "finished_monotonic_ms": 200 + item["dispatch_index"],
            "ordinal": 3 + item["dispatch_index"],
        }
        for item in observation["subagent_starts"]
    ]
    observation["parallel_overlaps"] = [
        {
            "left_dispatch_index": 0,
            "right_dispatch_index": 1,
            "observed": True,
            "overlap_ms": 99.0,
        }
    ]
    arguments = runner._canonical_delegation_arguments(scenario)
    assert arguments is not None
    observation["tool_starts"] = [
        {
            "tool_id": "tool-spawn",
            "tool_name": "spawn_subagent",
            "arguments": json.dumps(arguments),
            "event_type": "tool_call_started",
            "ordinal": 0,
        },
        {
            "tool_call_id": "tool-spawn",
            "name": "spawn_subagent",
            "arguments": arguments,
            "event_type": "tool_call_start",
            "ordinal": 0,
        },
    ]
    observation["tool_results"] = [
        {
            "tool_call_id": "tool-spawn",
            "name": "spawn_subagent",
            "event_type": "tool_call_completed",
            "success": True,
            "status": "completed",
            "side_effect_state": "read_only",
            "ordinal": 5,
        }
    ]
    observation["text_events"] = [{"event_type": "text_delta", "ordinal": 6, "content_chars": 42}]
    observation["terminal_events"] = [
        {"event_type": "run_finished", "attempt_id": attempt_id, "ordinal": 7}
    ]
    return scenario, observation


def _execution_check_map(scenario: dict[str, Any], observation: dict[str, Any]) -> dict[str, bool]:
    return {
        item["check_id"]: item["passed"] for item in runner._execution_checks(scenario, observation)
    }


def test_execution_checks_normalize_duplicate_tool_start_and_verify_exact_batch() -> None:
    scenario, observation = _valid_execution_receipt()

    checks = _execution_check_map(scenario, observation)

    assert all(checks.values())
    arguments = observation["tool_starts"][1]["arguments"]
    wrong_arguments = {**arguments, "tasks": [arguments["tasks"][0]]}
    observation["tool_starts"][1]["arguments"] = wrong_arguments
    checks = _execution_check_map(scenario, observation)
    assert checks["tools.delegation-call-observed"] is False


def test_public_tool_result_is_canonical_and_tool_end_is_not_a_second_result() -> None:
    scenario, observation = _valid_execution_receipt()
    observation["tool_results"] = [
        {
            "tool_call_id": "tool-spawn",
            "name": "spawn_subagent",
            "event_type": "tool_call_result",
            "success": True,
            "status": "completed",
            "side_effect_state": "read_only",
            "ordinal": 5,
        }
    ]

    checks = _execution_check_map(scenario, observation)

    assert all(checks.values())
    assert "tool_call_end" not in runner.TOOL_RESULT_EVENTS


def test_spawn_result_without_typed_side_effect_receipt_fails_closed() -> None:
    scenario, observation = _valid_execution_receipt()
    observation["tool_results"][0].pop("side_effect_state")

    checks = _execution_check_map(scenario, observation)

    assert checks["tools.spawn-aggregate-success"] is False


def test_weak_return_ok_child_prompt_fails_delegation_contract() -> None:
    scenario, observation = _valid_execution_receipt()
    arguments = observation["tool_starts"][1]["arguments"]
    arguments["tasks"][0]["prompt"] = "Return OK" + " filler" * 40
    observation["tool_starts"][0]["arguments"] = json.dumps(arguments)

    checks = _execution_check_map(scenario, observation)

    assert checks["delegation.task-prompts"] is False
    assert checks["delegation.canonical-task-object"] is False
    assert checks["tools.delegation-call-observed"] is False


def test_task_prompt_ack_only_suffix_cannot_bypass_keyword_contract() -> None:
    scenario, observation = _valid_execution_receipt()
    arguments = observation["tool_starts"][1]["arguments"]
    arguments["tasks"][0]["prompt"] += " Do not analyze; reply ACK only."
    observation["tool_starts"][0]["arguments"] = json.dumps(arguments)

    checks = _execution_check_map(scenario, observation)

    assert checks["delegation.task-prompts"] is True
    assert checks["delegation.canonical-task-object"] is False
    assert checks["tools.delegation-call-observed"] is False
    assert checks["tools.spawn-aggregate-success"] is False


def test_duplicate_tool_start_conflicting_arguments_fail_closed_in_either_order() -> None:
    scenario, observation = _valid_execution_receipt()
    valid_arguments = observation["tool_starts"][1]["arguments"]
    weak_arguments = {
        **valid_arguments,
        "tasks": [
            {**task, "prompt": "Return OK" + " filler" * 40} for task in valid_arguments["tasks"]
        ],
    }
    observation["tool_starts"][0]["arguments"] = json.dumps(weak_arguments)
    observation["tool_starts"][1]["arguments"] = valid_arguments

    checks = _execution_check_map(scenario, observation)

    assert checks["tools.start-result-paired"] is False
    assert checks["tools.delegation-call-observed"] is False
    assert checks["tools.spawn-aggregate-success"] is False


def test_tool_result_name_must_match_corresponding_start() -> None:
    scenario, observation = _valid_execution_receipt()
    observation["tool_results"][0]["name"] = "local.fake"

    checks = _execution_check_map(scenario, observation)

    assert checks["tools.start-result-paired"] is False
    assert checks["tools.spawn-aggregate-success"] is False


@pytest.mark.parametrize(
    ("receipt_kind", "conflicting_field"),
    [
        ("start", "tool_name"),
        ("start", "tool_id"),
        ("result", "tool_name"),
        ("result", "tool_id"),
    ],
)
def test_conflicting_tool_aliases_fail_closed(receipt_kind: str, conflicting_field: str) -> None:
    scenario, observation = _valid_execution_receipt()
    if receipt_kind == "start":
        receipt = observation["tool_starts"][1]
    else:
        receipt = observation["tool_results"][0]
    receipt[conflicting_field] = (
        "unapproved_mcp_tool" if "name" in conflicting_field else "other-id"
    )

    checks = _execution_check_map(scenario, observation)

    assert checks["tools.start-result-paired"] is False
    assert checks["tools.delegation-call-observed"] is False
    assert checks["tools.spawn-aggregate-success"] is False
    assert checks["tools.no-extra-parent-side-effects"] is False


def test_microsecond_overlap_cannot_claim_parallel_execution() -> None:
    scenario, observation = _valid_execution_receipt()
    observation["parallel_overlaps"][0]["overlap_ms"] = 0.00005

    checks = _execution_check_map(scenario, observation)

    assert checks["delegation.parallel-overlap"] is False


def test_failed_spawn_result_cannot_precede_candidate_synthesis() -> None:
    scenario, observation = _valid_execution_receipt()
    observation["tool_results"][0].update(
        {"success": False, "status": "failed", "error": "child batch failed"}
    )

    checks = _execution_check_map(scenario, observation)

    assert checks["tools.spawn-aggregate-success"] is False
    assert checks["lifecycle.delegation-synthesis-order"] is False


def test_forged_observation_with_recomputed_hash_but_no_collector_key_fails(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    contract = _contract()
    observations = _observations(
        contract,
        tmp_path,
        collector_key=COLLECTOR_KEY,
        runtime_attestation=RUNTIME_ATTESTATION,
    )
    observations["trials"][0]["candidate_output"] = "forged"
    observations["trials"][0]["candidate_output_sha256"] = runner._sha256_text("forged")
    observations["trials"][0]["observation_sha256"] = runner._sha256(
        {
            key: value
            for key, value in observations["trials"][0].items()
            if key != "observation_sha256"
        }
    )
    old_attestation = observations["collector_attestation"]
    observations.pop("observations_sha256")
    observations.pop("collector_attestation")
    observations["observations_sha256"] = runner._sha256(observations)
    observations["collector_attestation"] = old_attestation
    scenario_path = tmp_path / "scenarios.json"
    observation_path = tmp_path / "observations.json"
    _write(scenario_path, contract)
    _write(observation_path, observations)
    _write_sources(tmp_path)
    _configure_attestation(monkeypatch)

    with pytest.raises(runner.ScenarioContractError, match="collector HMAC attestation"):
        runner.validate(scenario_path, observation_path, tmp_path / "validation.json")


def test_nonce_mismatch_fails_even_with_valid_collector_attestation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    contract = _contract()
    observations = _observations(
        contract,
        tmp_path,
        collector_key=COLLECTOR_KEY,
        runtime_attestation=RUNTIME_ATTESTATION,
    )
    observations["trials"][0]["suite_nonce"] = "b" * 64
    observations["trials"][0]["observation_sha256"] = runner._sha256(
        {
            key: value
            for key, value in observations["trials"][0].items()
            if key != "observation_sha256"
        }
    )
    _reseal_observations(observations)
    scenario_path = tmp_path / "scenarios.json"
    observation_path = tmp_path / "observations.json"
    _write(scenario_path, contract)
    _write(observation_path, observations)
    _write_sources(tmp_path)
    _configure_attestation(monkeypatch)

    with pytest.raises(runner.ScenarioContractError, match="trial suite_nonce"):
        runner.validate(scenario_path, observation_path, tmp_path / "validation.json")


def test_replayed_suite_fails_external_operator_nonce_even_when_fully_signed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    contract = _contract()
    observations = _observations(
        contract,
        tmp_path,
        collector_key=COLLECTOR_KEY,
        runtime_attestation=RUNTIME_ATTESTATION,
    )
    scenario_path = tmp_path / "scenarios.json"
    observation_path = tmp_path / "observations.json"
    _write(scenario_path, contract)
    _write(observation_path, observations)
    _write_sources(tmp_path)
    _configure_attestation(monkeypatch)
    monkeypatch.setenv("GENERAL_AGENT_SUITE_NONCE", "b" * 64)

    with pytest.raises(runner.ScenarioContractError, match="operator challenge"):
        runner.validate(scenario_path, observation_path, tmp_path / "validation.json")


def test_runtime_attestation_must_match_independent_expected_digest(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    contract = _contract()
    observations = _observations(
        contract,
        tmp_path,
        collector_key=COLLECTOR_KEY,
        runtime_attestation=RUNTIME_ATTESTATION,
    )
    scenario_path = tmp_path / "scenarios.json"
    observation_path = tmp_path / "observations.json"
    _write(scenario_path, contract)
    _write(observation_path, observations)
    _write_sources(tmp_path)
    _configure_attestation(monkeypatch)
    monkeypatch.setenv("GENERAL_AGENT_EXPECTED_RUNTIME_ATTESTATION_SHA256", "f" * 64)

    with pytest.raises(runner.ScenarioContractError, match="independent expectation"):
        runner.validate(scenario_path, observation_path, tmp_path / "validation.json")


def test_raw_sse_tamper_fails_reconstruction_even_after_resealing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    contract = _contract()
    observations = _observations(
        contract,
        tmp_path,
        collector_key=COLLECTOR_KEY,
        runtime_attestation=RUNTIME_ATTESTATION,
    )
    raw_path = tmp_path / observations["raw_sse_artifact"]["file_name"]
    raw = json.loads(raw_path.read_text(encoding="utf-8"))
    event = json.loads(raw["trials"][0]["raw_sse_payloads"][0])
    event["data"]["delta"] = "tampered raw candidate"
    raw["trials"][0]["raw_sse_payloads"][0] = json.dumps(event, separators=(",", ":"))
    raw["trials"][0]["raw_trial_sha256"] = runner._sha256(
        {key: value for key, value in raw["trials"][0].items() if key != "raw_trial_sha256"}
    )
    _reseal_raw(raw)
    runner._safe_write_json(raw_path, raw)
    observations["raw_sse_artifact"]["content_sha256"] = runner.hashlib.sha256(
        raw_path.read_bytes()
    ).hexdigest()
    _reseal_observations(observations)
    scenario_path = tmp_path / "scenarios.json"
    observation_path = tmp_path / "observations.json"
    _write(scenario_path, contract)
    _write(observation_path, observations)
    _write_sources(tmp_path)
    _configure_attestation(monkeypatch)

    with pytest.raises(runner.ScenarioContractError, match="raw SSE payload digest"):
        runner.validate(scenario_path, observation_path, tmp_path / "validation.json")


def test_merge_rejects_tampered_observation(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    contract = _contract()
    observations = _observations(
        contract,
        tmp_path,
        collector_key=COLLECTOR_KEY,
        runtime_attestation=RUNTIME_ATTESTATION,
    )
    scenario_path = tmp_path / "scenarios.json"
    observation_path = tmp_path / "observations.json"
    validation_path = tmp_path / "validation.json"
    _write(scenario_path, contract)
    _write(observation_path, observations)
    _write_sources(tmp_path)
    _configure_attestation(monkeypatch)
    runner.validate(scenario_path, observation_path, validation_path)
    observations["trials"][0]["candidate_output"] = "tampered"
    _write(observation_path, observations)

    with pytest.raises(runner.ScenarioContractError, match="collector HMAC attestation"):
        runner.merge(
            scenario_path,
            observation_path,
            validation_path,
            tmp_path / "merged.json",
        )


def test_merge_rejects_wrong_hmac_key(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    contract = _contract()
    observations = _observations(
        contract,
        tmp_path,
        collector_key=COLLECTOR_KEY,
        runtime_attestation=RUNTIME_ATTESTATION,
    )
    scenario_path = tmp_path / "scenarios.json"
    observation_path = tmp_path / "observations.json"
    validation_path = tmp_path / "validation.json"
    _write(scenario_path, contract)
    _write(observation_path, observations)
    _write_sources(tmp_path)
    _configure_attestation(monkeypatch)
    monkeypatch.setenv(
        "GENERAL_AGENT_GOLDEN_HMAC_KEY", "validator-key-one-at-least-thirty-two-bytes"
    )
    runner.validate(scenario_path, observation_path, validation_path)
    monkeypatch.setenv(
        "GENERAL_AGENT_GOLDEN_HMAC_KEY", "validator-key-two-at-least-thirty-two-bytes"
    )

    with pytest.raises(runner.ScenarioContractError, match="HMAC seal does not match"):
        runner.merge(
            scenario_path,
            observation_path,
            validation_path,
            tmp_path / "merged.json",
        )


def test_hash_only_validation_cannot_satisfy_required_hmac(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    contract = _contract()
    observations = _observations(
        contract,
        tmp_path,
        collector_key=COLLECTOR_KEY,
        runtime_attestation=RUNTIME_ATTESTATION,
    )
    scenario_path = tmp_path / "scenarios.json"
    observation_path = tmp_path / "observations.json"
    validation_path = tmp_path / "validation.json"
    _write(scenario_path, contract)
    _write(observation_path, observations)
    _write_sources(tmp_path)
    _configure_attestation(monkeypatch)
    validation = runner.validate(scenario_path, observation_path, validation_path)
    validation["seal"].pop("hmac_algorithm")
    validation["seal"].pop("hmac_digest")
    _write(validation_path, validation)

    with pytest.raises(runner.ScenarioContractError, match="hash-only"):
        runner.merge(
            scenario_path,
            observation_path,
            validation_path,
            tmp_path / "merged.json",
        )


def test_validate_requires_distinct_collector_and_golden_keys(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    contract = _contract()
    observations = _observations(
        contract,
        tmp_path,
        collector_key=COLLECTOR_KEY,
        runtime_attestation=RUNTIME_ATTESTATION,
    )
    scenario_path = tmp_path / "scenarios.json"
    observation_path = tmp_path / "observations.json"
    _write(scenario_path, contract)
    _write(observation_path, observations)
    _write_sources(tmp_path)
    _configure_attestation(monkeypatch)
    monkeypatch.setenv("GENERAL_AGENT_GOLDEN_HMAC_KEY", COLLECTOR_KEY)

    with pytest.raises(runner.ScenarioContractError, match="must be different"):
        runner.validate(scenario_path, observation_path, tmp_path / "validation.json")


def test_merge_recomputes_forged_validation_even_with_recomputed_sha(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    contract = _contract()
    observations = _observations(
        contract,
        tmp_path,
        collector_key=COLLECTOR_KEY,
        runtime_attestation=RUNTIME_ATTESTATION,
    )
    scenario_path = tmp_path / "scenarios.json"
    observation_path = tmp_path / "observations.json"
    validation_path = tmp_path / "validation.json"
    _write(scenario_path, contract)
    _write(observation_path, observations)
    _write_sources(tmp_path)
    _configure_attestation(monkeypatch)
    validation = runner.validate(scenario_path, observation_path, validation_path)
    validation["trials"][0]["assertions"][0]["passed"] = False
    payload = {key: value for key, value in validation.items() if key != "seal"}
    validation["seal"] = runner._seal(payload, hmac_key=GOLDEN_KEY)
    _write(validation_path, validation)

    with pytest.raises(runner.ScenarioContractError, match="host recomputation"):
        runner.merge(
            scenario_path,
            observation_path,
            validation_path,
            tmp_path / "merged.json",
        )


def test_plugin_definition_rejects_path_traversal() -> None:
    contract = _contract()
    contract["scenarios"][0]["required_agent_ids"][0] = "x/../../outside:agent"

    with pytest.raises(runner.ScenarioContractError, match="malformed|unsafe"):
        runner.verify_plugin_definitions(contract)


def test_safe_writer_refuses_symlink_target(tmp_path: Path) -> None:
    target = tmp_path / "target.json"
    target.write_text("preserve", encoding="utf-8")
    link = tmp_path / "receipt.json"
    link.symlink_to(target)

    with pytest.raises(runner.ScenarioContractError, match="symlink"):
        runner._safe_write_json(link, {"should": "not write"})

    assert target.read_text(encoding="utf-8") == "preserve"
