from __future__ import annotations

import hashlib
import importlib.util
import json
from datetime import datetime, timezone
from pathlib import Path

from src.services.eval import golden as golden_module
from src.services.eval import golden_gate as golden_gate_module
from src.services.eval import golden_validation as golden_validation_module
from src.services.eval.golden import (
    GATE_METRICS_SCHEMA_VERSION,
    apply_gate,
    evaluate_case,
    evaluate_cases,
    load_jsonl,
    validate_cases,
)

GOLDEN = Path("tests/fixtures/eval/golden/assistant_regression_v1.jsonl")
OBSERVATIONS = Path("tests/fixtures/eval/observations/assistant_regression_v1.jsonl")


def _load_eval_golden_main():
    script_path = Path(__file__).resolve().parents[3] / "scripts" / "eval_golden.py"
    spec = importlib.util.spec_from_file_location("eval_golden_script", script_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Unable to load eval golden script: {script_path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.main


eval_golden_main = _load_eval_golden_main()


def test_gate_facade_reexports_canonical_gate_contract() -> None:
    assert golden_module.apply_gate is golden_gate_module.apply_gate
    assert golden_module.write_gate_report is golden_gate_module.write_gate_report
    assert (
        golden_module.DEFAULT_GATE_THRESHOLDS
        is golden_gate_module.DEFAULT_GATE_THRESHOLDS
    )
    assert (
        golden_module.GATE_METRICS_SCHEMA_VERSION
        == golden_gate_module.GATE_METRICS_SCHEMA_VERSION
    )
    assert (
        golden_module.GATE_RATE_ABS_TOLERANCE
        == golden_gate_module.GATE_RATE_ABS_TOLERANCE
    )


def test_validation_facade_reexports_canonical_validation_contract() -> None:
    assert golden_module.validate_case is golden_validation_module.validate_case
    assert (
        golden_module.validate_observations
        is golden_validation_module.validate_observations
    )
    assert (
        golden_module._validate_stateful_expectations
        is golden_validation_module._validate_stateful_expectations
    )
    for name in (
        "NUMERIC_ASSERTIONS",
        "REQUIRED_STATEFUL_NESTED_FIELDS",
        "STRING_ASSERTIONS",
        "SUPPORTED_ASSERTIONS",
        "SUPPORTED_STATEFUL_EXPECTATION_FIELDS",
        "SUPPORTED_STATEFUL_NESTED_FIELDS",
        "SUPPORTED_TOOL_EXPECTATION_FIELDS",
    ):
        assert getattr(golden_module, name) is getattr(golden_validation_module, name)


def golden_case(
    *,
    assertions: list[dict[str, object]] | None = None,
    runtime: dict[str, object] | None = None,
) -> dict[str, object]:
    return {
        "case_id": "assistant.runtime.test",
        "split": "regression",
        "input": {"input_preview": "test"},
        "expected_output": {"contains": "ok"},
        "expected_trajectory": {
            "required_span_kinds": [],
            "runtime": runtime or {},
        },
        "assertions": assertions or [{"type": "output_contains", "value": "ok"}],
        "metadata": {"critical": True},
    }


def replay_observation(**evidence: object) -> dict[str, object]:
    return {
        "status": "succeeded",
        "output_preview": "ok",
        "span_kinds": [],
        **evidence,
    }


def test_validation_facade_monkeypatch_remains_visible_to_orchestration(monkeypatch) -> None:
    monkeypatch.setattr(
        golden_module,
        "validate_case",
        lambda _case: ["patched validation failure"],
    )

    validation = golden_module.validate_cases([golden_case()])
    evaluation = golden_module.evaluate_case(golden_case(), replay_observation())

    assert validation["errors"][0]["errors"] == ["patched validation failure"]
    assert evaluation["failures"] == [
        "invalid behavior contract: patched validation failure"
    ]


def test_assistant_golden_fixture_validates_and_has_seed_coverage() -> None:
    cases = load_jsonl(GOLDEN)
    result = validate_cases(cases)

    assert result["valid"] is True
    assert result["case_count"] >= 25
    assert any(case["metadata"].get("critical") is True for case in cases)
    assert all("expected_trajectory" in case for case in cases)
    case_ids = {case["case_id"] for case in cases}
    assert {
        "assistant.runtime.approval_denial",
        "assistant.runtime.approval_argument_mismatch",
        "assistant.runtime.sandbox_unavailable",
        "assistant.runtime.interrupted_memory_skip",
        "assistant.runtime.stop_resume",
        "assistant.runtime.max_iterations",
        "assistant.runtime.policy_bypass",
        "assistant.runtime.repeated_unknown_side_effect",
        "assistant.tool.failure_recovery",
        "assistant.export.redaction",
        "assistant.stateful.plan_retention",
        "assistant.stateful.tool_pairing",
        "assistant.stateful.budget_termination",
        "assistant.stateful.hitl_pause_resume",
        "assistant.stateful.compaction_retention",
        "assistant.security.prompt_injection",
        "assistant.security.tenant_isolation",
    }.issubset(case_ids)


def test_expectations_do_not_embed_replay_observations() -> None:
    cases = load_jsonl(GOLDEN)

    assert all("replay" not in case["metadata"] for case in cases)


def test_observation_fixture_loads_and_joins_every_expectation() -> None:
    load_observations = getattr(golden_module, "load_observations", None)
    validate_observations = getattr(golden_module, "validate_observations", None)
    assert callable(load_observations)
    assert callable(validate_observations)

    cases = load_jsonl(GOLDEN)
    observations = load_observations(OBSERVATIONS)
    result = validate_observations(cases, observations)

    assert result["valid"] is True
    assert result["joined_count"] == 25
    assert len(observations) == 25


def test_offline_gate_passes_recorded_observations_without_model_calls(tmp_path: Path) -> None:
    output = tmp_path / "latest.json"
    markdown = tmp_path / "latest.md"

    exit_code = eval_golden_main(
        [
            "gate",
            str(GOLDEN),
            "--observations",
            str(OBSERVATIONS),
            "--output",
            str(output),
            "--markdown",
            str(markdown),
        ]
    )

    assert exit_code == 0
    payload = json.loads(output.read_text(encoding="utf-8"))
    assert payload["gate"]["status"] == "pass"
    assert payload["suite_scope"] == "canonical"
    assert payload["metrics"]["overall_score"] >= 0.85
    assert payload["evidence_scope"] == "recorded_offline_observation"
    assert payload["observations"]["joined_count"] == 25
    assert payload["gate"]["hard_blockers_passed"] is True
    assert payload["gate"]["required_hard_blockers"] == [
        "assistant.runtime.policy_bypass",
        "assistant.runtime.repeated_unknown_side_effect",
    ]
    assert payload["gate"]["stateful_cases_passed"] is True
    assert payload["metrics"]["stateful_case_count"] == 7
    assert payload["metrics"]["stateful_pass_rate"] == 1.0
    provenance = payload["provenance"]
    assert len(provenance["dataset"]["sha256"]) == 64
    assert len(provenance["observations"]["sha256"]) == 64
    assert provenance["grader"]["id"] == "assistant_deterministic_contract"
    assert len(provenance["grader"]["sha256"]) == 64
    assert provenance["trial"]["repetitions_per_case"] == 1
    assert provenance["trial"]["seed"] == "not_recorded"
    assert provenance["trace"]["receipt"] == "not_recorded"
    assert provenance["coverage"]["latency"] == [
        {"case_id": "assistant.latency.short_answer", "total_latency_ms": 900}
    ]
    assert provenance["coverage"]["tokens"] == []
    assert provenance["coverage"]["cache"] == []
    recovery = {item["case_id"]: item for item in provenance["coverage"]["recovery"]}
    assert recovery["assistant.runtime.repeated_unknown_side_effect"]["blind_replay"] is False
    assert recovery["assistant.runtime.repeated_unknown_side_effect"]["second_dispatch_count"] == 0
    assert provenance["evidence_tiers"]["real_provider"] == "not_run"
    assert "Eval Regression Gate" in markdown.read_text(encoding="utf-8")


def test_offline_provenance_hashes_sorted_grader_components(tmp_path: Path) -> None:
    output = tmp_path / "latest.json"
    markdown = tmp_path / "latest.md"

    exit_code = eval_golden_main(
        [
            "gate",
            str(GOLDEN),
            "--observations",
            str(OBSERVATIONS),
            "--output",
            str(output),
            "--markdown",
            str(markdown),
        ]
    )

    assert exit_code == 0
    grader = json.loads(output.read_text(encoding="utf-8"))["provenance"]["grader"]
    repo_root = Path(__file__).resolve().parents[3]
    component_paths = (
        "src/services/eval/golden.py",
        "src/services/eval/golden_gate.py",
        "src/services/eval/golden_validation.py",
    )
    expected_components = {
        path: hashlib.sha256((repo_root / path).read_bytes()).hexdigest()
        for path in component_paths
    }
    canonical_components = json.dumps(
        expected_components,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")

    assert grader["id"] == "assistant_deterministic_contract"
    assert grader["version"] == "v1"
    assert list(grader["components"]) == sorted(component_paths)
    assert grader["components"] == expected_components
    assert grader["sha256"] == hashlib.sha256(canonical_components).hexdigest()


def test_canonical_filename_does_not_grant_canonical_hard_blocker_trust(
    tmp_path: Path,
) -> None:
    golden = tmp_path / "assistant_regression_v1.jsonl"
    observations = tmp_path / "observations.jsonl"
    output = tmp_path / "latest.json"
    markdown = tmp_path / "latest.md"
    golden.write_text(json.dumps(golden_case()) + "\n", encoding="utf-8")
    observations.write_text(
        json.dumps({"case_id": "assistant.runtime.test", "replay": replay_observation()}) + "\n",
        encoding="utf-8",
    )

    exit_code = eval_golden_main(
        [
            "gate",
            str(golden),
            "--observations",
            str(observations),
            "--output",
            str(output),
            "--markdown",
            str(markdown),
        ]
    )

    assert exit_code == 0
    payload = json.loads(output.read_text(encoding="utf-8"))
    assert payload["suite_scope"] == "custom"
    assert "hard_blockers_passed" not in payload["gate"]
    assert "stateful_cases_passed" not in payload["gate"]


def test_cli_rejects_out_of_range_baseline_report(tmp_path: Path) -> None:
    baseline = tmp_path / "baseline.json"
    output = tmp_path / "latest.json"
    markdown = tmp_path / "latest.md"
    baseline.write_text(json.dumps({"metrics": {"overall_score": 1.03}}), encoding="utf-8")

    exit_code = eval_golden_main(
        [
            "gate",
            str(GOLDEN),
            "--observations",
            str(OBSERVATIONS),
            "--baseline-report",
            str(baseline),
            "--output",
            str(output),
            "--markdown",
            str(markdown),
        ]
    )

    assert exit_code == 1
    payload = json.loads(output.read_text(encoding="utf-8"))
    assert payload["baseline"]["source"] == str(baseline)
    assert any(
        "baseline overall_score must be a finite number in [0, 1]" in failure
        for failure in payload["gate"]["failures"]
    )


def test_gate_fails_low_quality_metrics() -> None:
    gate = apply_gate(
        {
            "schema_version": GATE_METRICS_SCHEMA_VERSION,
            "case_count": 2,
            "score_sum": 1.0,
            "failed_case_count": 1,
            "overall_score": 0.5,
            "pass_rate": 0.5,
            "trajectory_case_count": 2,
            "trajectory_failed_count": 1,
            "trajectory_pass_rate": 0.5,
            "critical_case_count": 2,
            "critical_failed_count": 1,
            "critical_pass_rate": 0.5,
            "stateful_case_count": 0,
            "stateful_failed_count": 0,
            "stateful_pass_rate": None,
        }
    )

    assert gate["status"] == "fail"
    assert len(gate["failures"]) == 3


def test_gate_skips_critical_threshold_when_suite_has_no_critical_cases() -> None:
    case = golden_case()
    case["metadata"]["critical"] = False  # type: ignore[index]

    metrics = evaluate_cases([case], {"assistant.runtime.test": replay_observation()})
    gate = apply_gate(metrics)

    assert metrics["critical_case_count"] == 0
    assert metrics["critical_pass_rate"] is None
    assert gate["status"] == "pass"
    assert gate["skipped_thresholds"] == [
        "critical_pass_rate:no_critical_cases",
        "stateful_pass_rate:no_stateful_cases",
    ]


def test_missing_stateful_observation_counts_as_a_stateful_failure() -> None:
    cases = [golden_case(), golden_case()]
    for index, case in enumerate(cases, start=1):
        case["case_id"] = f"assistant.stateful.{index}"
        case["expected_trajectory"]["stateful"] = {  # type: ignore[index]
            "minimum_turns": 2,
            "tool_pairing": {"required": True},
        }
    observations = {
        "assistant.stateful.1": replay_observation(
            turns=[
                {
                    "turn_index": 1,
                    "tool_calls": [{"call_id": "call-1", "name": "read"}],
                    "tool_results": [],
                },
                {
                    "turn_index": 2,
                    "tool_calls": [],
                    "tool_results": [{"tool_call_id": "call-1", "status": "succeeded"}],
                },
            ]
        )
    }

    metrics = evaluate_cases(cases, observations)
    gate = apply_gate(metrics)

    assert metrics["stateful_case_count"] == 2
    assert metrics["stateful_pass_rate"] == 0.5
    assert gate["status"] == "fail"
    assert "stateful hard gate requires zero failed stateful cases" in gate["failures"]


def test_golden_redaction_regression_is_a_failure() -> None:
    cases = [
        {
            "case_id": "unsafe",
            "input": {},
            "expected_output": {"contains": "safe"},
            "expected_trajectory": {"required_span_kinds": ["model_invocation"]},
            "assertions": [],
            "metadata": {
                "critical": True,
                "replay": {
                    "status": "succeeded",
                    "output_preview": "safe Authorization: Bearer raw-token",
                    "span_kinds": ["model_invocation"],
                },
            },
        }
    ]

    metrics = evaluate_cases(cases)

    assert metrics["critical_pass_rate"] == 0.0
    assert metrics["cases"][0]["passed"] is False
    assert "sensitive replay payload" in metrics["cases"][0]["failures"]


def test_sensitive_output_check_accepts_persisted_trace_timestamps() -> None:
    case = golden_case(assertions=[{"type": "no_sensitive_output"}])
    observation = replay_observation(
        spans=[{"span_kind": "model_invocation", "started_at": datetime.now(timezone.utc)}]
    )

    assert evaluate_case(case, observation)["passed"] is True


def test_runtime_and_latency_mismatches_fail() -> None:
    case = golden_case(
        assertions=[{"type": "latency_ms_lt", "value": 100}],
        runtime={"expected_exit_reason": "approval_denied", "memory_sync": "skipped"},
    )
    observation = replay_observation(
        total_latency_ms=101,
        exit_reason="succeeded",
        memory_sync="written",
    )
    result = evaluate_case(case, observation)

    assert result["passed"] is False
    assert result["trajectory_pass"] is False
    assert "latency_ms_lt" in " ".join(result["failures"])
    assert "expected_exit_reason" in " ".join(result["failures"])
    metrics = evaluate_cases([case], {str(case["case_id"]): observation})
    assert metrics["trajectory_pass_rate"] == 0.0


def test_unknown_assertion_and_missing_observation_fail_closed() -> None:
    case = golden_case(assertions=[{"type": "unknown_rule"}])
    assert validate_cases([case])["valid"] is False
    result = evaluate_case(golden_case(), None)
    assert result["passed"] is False
    assert "missing replay observation" in result["failures"]


def test_zero_critical_rate_is_not_replaced_by_pass_rate() -> None:
    gate = apply_gate(
        {
            "schema_version": GATE_METRICS_SCHEMA_VERSION,
            "case_count": 2,
            "score_sum": 2.0,
            "failed_case_count": 1,
            "overall_score": 1.0,
            "pass_rate": 0.5,
            "trajectory_case_count": 2,
            "trajectory_failed_count": 0,
            "trajectory_pass_rate": 1.0,
            "critical_case_count": 1,
            "critical_failed_count": 1,
            "critical_pass_rate": 0.0,
            "stateful_case_count": 0,
            "stateful_failed_count": 0,
            "stateful_pass_rate": None,
        }
    )

    assert gate["status"] == "fail"


def test_failure_mode_absent_requires_explicit_evidence() -> None:
    case = golden_case(assertions=[{"type": "failure_mode_absent", "value": "tool_error"}])

    missing = evaluate_case(case, replay_observation())
    empty_scalar = evaluate_case(case, replay_observation(failure_mode=""))

    assert missing["passed"] is False
    assert empty_scalar["passed"] is False
    assert "failure_mode_absent missing evidence" in missing["failures"]
    assert "failure_mode_absent missing evidence" in empty_scalar["failures"]


def test_failure_mode_absent_accepts_explicit_empty_or_non_matching_evidence() -> None:
    case = golden_case(assertions=[{"type": "failure_mode_absent", "value": "tool_error"}])

    explicit_empty = evaluate_case(case, replay_observation(failure_modes=[]))
    non_matching_list = evaluate_case(
        case,
        replay_observation(failure_modes=["latency_regression"]),
    )
    non_matching_scalar = evaluate_case(
        case,
        replay_observation(failure_mode="latency_regression"),
    )

    assert explicit_empty["passed"] is True
    assert non_matching_list["passed"] is True
    assert non_matching_scalar["passed"] is True


def test_validate_case_rejects_non_object_runtime_expectations() -> None:
    case = golden_case()
    case["expected_trajectory"]["runtime"] = "not-an-object"  # type: ignore[index]

    validation = validate_cases([case])

    assert validation["valid"] is False
    assert "expected_trajectory.runtime must be an object" in validation["errors"][0]["errors"]


def test_explicit_zero_baseline_score_is_not_replaced_by_average_score() -> None:
    gate = apply_gate(
        {
            "schema_version": GATE_METRICS_SCHEMA_VERSION,
            "case_count": 1,
            "score_sum": 1.0,
            "failed_case_count": 0,
            "overall_score": 1.0,
            "pass_rate": 1.0,
            "trajectory_case_count": 1,
            "trajectory_failed_count": 0,
            "trajectory_pass_rate": 1.0,
            "critical_case_count": 1,
            "critical_failed_count": 0,
            "critical_pass_rate": 1.0,
            "stateful_case_count": 0,
            "stateful_failed_count": 0,
            "stateful_pass_rate": None,
        },
        baseline_metrics={
            "schema_version": GATE_METRICS_SCHEMA_VERSION,
            "case_count": 1,
            "score_sum": 0.0,
            "failed_case_count": 1,
            "overall_score": 0.0,
            "average_score": 1.0,
            "pass_rate": 0.0,
            "trajectory_case_count": 1,
            "trajectory_failed_count": 1,
            "trajectory_pass_rate": 0.0,
            "critical_case_count": 1,
            "critical_failed_count": 1,
            "critical_pass_rate": 0.0,
            "stateful_case_count": 0,
            "stateful_failed_count": 0,
            "stateful_pass_rate": None,
        },
    )

    assert gate["status"] == "pass"
    assert not any("baseline tolerance" in failure for failure in gate["failures"])


def test_gate_rejects_nan_in_metrics_thresholds_and_baseline() -> None:
    valid_metrics = {
        "schema_version": GATE_METRICS_SCHEMA_VERSION,
        "case_count": 1,
        "score_sum": 1.0,
        "failed_case_count": 0,
        "overall_score": 1.0,
        "pass_rate": 1.0,
        "trajectory_case_count": 1,
        "trajectory_failed_count": 0,
        "trajectory_pass_rate": 1.0,
        "critical_case_count": 1,
        "critical_failed_count": 0,
        "critical_pass_rate": 1.0,
        "stateful_case_count": 0,
        "stateful_failed_count": 0,
        "stateful_pass_rate": None,
    }

    nan_metrics = apply_gate({**valid_metrics, "overall_score": float("nan")})
    nan_threshold = apply_gate(
        valid_metrics,
        thresholds={"overall_score": float("nan")},
    )
    infinite_baseline = apply_gate(
        valid_metrics,
        baseline_metrics={"overall_score": float("inf")},
    )

    assert nan_metrics["status"] == "fail"
    assert nan_threshold["status"] == "fail"
    assert infinite_baseline["status"] == "fail"
    assert "overall_score must be a finite number" in " ".join(nan_metrics["failures"])
    assert "threshold overall_score must be a finite number" in " ".join(
        nan_threshold["failures"]
    )
    assert "baseline overall_score must be a finite number" in " ".join(
        infinite_baseline["failures"]
    )


def test_gate_rejects_unknown_threshold_and_count_rate_mismatches() -> None:
    valid_metrics = {
        "schema_version": GATE_METRICS_SCHEMA_VERSION,
        "case_count": 10,
        "score_sum": 10.0,
        "failed_case_count": 0,
        "overall_score": 1.0,
        "pass_rate": 1.0,
        "trajectory_case_count": 10,
        "trajectory_failed_count": 0,
        "trajectory_pass_rate": 1.0,
        "critical_case_count": 10,
        "critical_failed_count": 0,
        "critical_pass_rate": 1.0,
        "stateful_case_count": 0,
        "stateful_failed_count": 0,
        "stateful_pass_rate": None,
    }

    unknown_threshold = apply_gate(valid_metrics, thresholds={"future_metric": 0.0})
    missing_rate = apply_gate({**valid_metrics, "critical_pass_rate": None})
    inconsistent_rate = apply_gate(
        {
            **valid_metrics,
            "critical_failed_count": 1,
            "critical_pass_rate": 1.0,
        }
    )

    assert unknown_threshold["status"] == "fail"
    assert "unsupported gate thresholds" in " ".join(unknown_threshold["failures"])
    assert missing_rate["status"] == "fail"
    assert "critical_pass_rate must be a finite number" in " ".join(
        missing_rate["failures"]
    )
    assert "critical_pass_rate:no_critical_cases" not in missing_rate["skipped_thresholds"]
    assert inconsistent_rate["status"] == "fail"
    assert "inconsistent with critical case counts" in " ".join(
        inconsistent_rate["failures"]
    )


def test_gate_rejects_forged_score_and_trajectory_receipts() -> None:
    valid_metrics = {
        "schema_version": GATE_METRICS_SCHEMA_VERSION,
        "case_count": 10,
        "score_sum": 8.0,
        "failed_case_count": 2,
        "overall_score": 0.8,
        "pass_rate": 0.8,
        "trajectory_case_count": 10,
        "trajectory_failed_count": 1,
        "trajectory_pass_rate": 0.9,
        "critical_case_count": 2,
        "critical_failed_count": 0,
        "critical_pass_rate": 1.0,
        "stateful_case_count": 1,
        "stateful_failed_count": 0,
        "stateful_pass_rate": 1.0,
    }

    forged_score = apply_gate({**valid_metrics, "overall_score": 1.0})
    forged_trajectory = apply_gate({**valid_metrics, "trajectory_pass_rate": 1.0})

    assert "overall_score is inconsistent with score_sum and case_count" in forged_score[
        "failures"
    ]
    assert (
        "trajectory_pass_rate is inconsistent with trajectory case counts"
        in forged_trajectory["failures"]
    )


def test_gate_rejects_impossible_critical_and_stateful_subset_counts() -> None:
    metrics = {
        "schema_version": GATE_METRICS_SCHEMA_VERSION,
        "case_count": 1,
        "score_sum": 1.0,
        "failed_case_count": 0,
        "overall_score": 1.0,
        "pass_rate": 1.0,
        "trajectory_case_count": 1,
        "trajectory_failed_count": 0,
        "trajectory_pass_rate": 1.0,
        "critical_case_count": 999,
        "critical_failed_count": 0,
        "critical_pass_rate": 1.0,
        "stateful_case_count": 999,
        "stateful_failed_count": 0,
        "stateful_pass_rate": 1.0,
    }

    gate = apply_gate(
        metrics,
        require_critical_coverage=True,
        require_stateful_coverage=True,
    )

    assert gate["status"] == "fail"
    assert "critical_case_count exceeds case_count" in gate["failures"]
    assert "stateful_case_count exceeds case_count" in gate["failures"]


def test_gate_derives_authoritative_rates_and_accepts_four_decimal_display_rounding() -> None:
    metrics = {
        "schema_version": GATE_METRICS_SCHEMA_VERSION,
        "case_count": 3,
        "score_sum": 3.0,
        "failed_case_count": 1,
        "overall_score": 1.0,
        "pass_rate": 0.6667,
        "trajectory_case_count": 3,
        "trajectory_failed_count": 1,
        "trajectory_pass_rate": 0.6667,
        "critical_case_count": 3,
        "critical_failed_count": 1,
        "critical_pass_rate": 0.6667,
        "stateful_case_count": 0,
        "stateful_failed_count": 0,
        "stateful_pass_rate": None,
    }

    gate = apply_gate(
        metrics,
        thresholds={
            "overall_score": 0.0,
            "trajectory_pass_rate": 0.0,
            "critical_pass_rate": 0.0,
            "stateful_pass_rate": 0.0,
            "baseline_tolerance": 0.0,
        },
    )

    assert gate["status"] == "pass"
    assert gate["metrics"]["pass_rate"] == 2 / 3
    assert gate["metrics"]["trajectory_pass_rate"] == 2 / 3
    assert gate["metrics"]["critical_pass_rate"] == 2 / 3


def test_release_gate_requires_explicit_nonzero_stateful_coverage() -> None:
    metrics = {
        "schema_version": GATE_METRICS_SCHEMA_VERSION,
        "case_count": 1,
        "score_sum": 1.0,
        "failed_case_count": 0,
        "overall_score": 1.0,
        "pass_rate": 1.0,
        "trajectory_case_count": 1,
        "trajectory_failed_count": 0,
        "trajectory_pass_rate": 1.0,
        "critical_case_count": 1,
        "critical_failed_count": 0,
        "critical_pass_rate": 1.0,
    }

    gate = apply_gate(metrics, require_stateful_coverage=True)

    assert gate["status"] == "fail"
    assert "release gate requires non-zero stateful case coverage" in gate["failures"]


def test_one_failure_in_large_critical_suite_cannot_round_up_to_pass() -> None:
    passing_case = golden_case()
    passing_case["metadata"]["replay"] = replay_observation()  # type: ignore[index]
    failing_case = golden_case()
    failing_case["case_id"] = "assistant.runtime.last-failure"
    failing_case["metadata"]["replay"] = replay_observation(  # type: ignore[index]
        status="failed"
    )

    metrics = evaluate_cases([passing_case] * 20_000 + [failing_case])
    gate = apply_gate(metrics)

    assert metrics["critical_failed_count"] == 1
    assert metrics["critical_pass_rate"] < 1.0
    assert gate["status"] == "fail"
    assert "critical hard gate requires zero failed critical cases" in gate["failures"]


def test_external_observation_wins_over_conflicting_inline_replay() -> None:
    case = golden_case()
    case["metadata"]["replay"] = replay_observation()  # type: ignore[index]
    external = {
        "status": "failed",
        "output_preview": "not ok",
        "span_kinds": [],
    }

    result = evaluate_case(case, external)

    assert result["passed"] is False
    assert "status=failed" in result["failures"]


def test_behavior_contract_validates_and_evaluates_output_tools_and_limits() -> None:
    case = golden_case(
        assertions=[
            {"type": "output_not_contains", "value": "invented"},
            {"type": "tool_called", "value": "get_account_status"},
            {"type": "tool_not_called", "value": "delete_account"},
            {"type": "latency_ms_lt", "value": 3000},
            {"type": "total_tokens_lt", "value": 2000},
            {"type": "cost_cents_lt", "value": 5},
        ]
    )
    case["expected_output"] = {
        "reference": "Explain that the tool is unavailable",
        "rubric": "Must not invent account status",
        "contains": ["ok", "fallback"],
        "not_contains": ["invented"],
    }
    case["expected_trajectory"]["tools"] = [  # type: ignore[index]
        {
            "name": "get_account_status",
            "required": True,
            "arguments_subset": {"account": {"id": "known"}},
            "order": 1,
            "max_calls": 1,
            "status": "succeeded",
        },
        {"name": "delete_account", "forbidden": True},
    ]
    observation = replay_observation(
        output_preview="ok: use the fallback",
        total_latency_ms=250,
        total_tokens=100,
        total_cost_cents=4,
        tool_calls=[
            {
                "name": "get_account_status",
                "arguments": {"account": {"id": "known", "region": "au"}},
                "status": "succeeded",
            }
        ],
    )

    assert validate_cases([case])["valid"] is True
    result = evaluate_case(case, observation)
    assert result["passed"] is True
    assert result["trajectory_pass"] is True


def test_behavior_contract_reports_output_tool_and_performance_regressions() -> None:
    case = golden_case(
        assertions=[
            {"type": "output_not_contains", "value": "invented"},
            {"type": "total_tokens_lt", "value": 100},
            {"type": "cost_cents_lt", "value": 5},
        ]
    )
    case["expected_output"] = {"contains": ["ok"], "not_contains": ["invented"]}
    case["expected_trajectory"]["tools"] = [  # type: ignore[index]
        {
            "name": "search",
            "arguments_subset": {"query": "expected"},
            "order": 1,
            "max_calls": 1,
            "status": "succeeded",
        },
        {"name": "delete_account", "forbidden": True},
    ]
    observation = replay_observation(
        output_preview="ok but invented",
        total_tokens=100,
        total_cost_cents=5,
        tool_calls=[
            {"name": "search", "arguments": {"query": "wrong"}, "status": "failed"},
            {"name": "search", "arguments": {"query": "expected"}, "status": "succeeded"},
            {"name": "delete_account", "arguments": {}, "status": "succeeded"},
        ],
    )

    result = evaluate_case(case, observation)
    failures = " ".join(result["failures"])

    assert result["passed"] is False
    assert result["trajectory_pass"] is False
    assert "output_not_contains" in failures
    assert "found forbidden text" in failures
    assert "total_tokens_lt" in failures
    assert "cost_cents_lt" in failures
    assert "called 2 > 1" in failures
    assert "did not match expected order, status, or arguments" in failures
    assert "forbidden tool called" in failures


def test_behavior_contract_reuses_tool_execution_spans() -> None:
    case = golden_case(assertions=[{"type": "tool_called", "value": "search"}])
    case["expected_trajectory"]["tools"] = [  # type: ignore[index]
        {
            "name": "search",
            "arguments_subset": {"query": "refund"},
            "status": "succeeded",
        }
    ]
    observation = replay_observation(
        spans=[
            {
                "span_kind": "tool_execution",
                "name": "tool:search",
                "input_preview": '{"query": "refund", "limit": 3}',
                "status": "succeeded",
            }
        ]
    )

    assert evaluate_case(case, observation)["passed"] is True


def test_behavior_contract_missing_evidence_and_malformed_rules_fail_closed() -> None:
    missing_evidence = golden_case(
        assertions=[
            {"type": "tool_not_called", "value": "delete_account"},
            {"type": "total_tokens_lt", "value": 100},
        ]
    )
    missing_result = evaluate_case(missing_evidence, replay_observation())
    assert missing_result["passed"] is False
    assert "requires valid tool_calls or spans evidence" in " ".join(missing_result["failures"])
    assert "total_tokens_lt requires numeric evidence" in missing_result["failures"]

    invalid_evidence = evaluate_case(
        golden_case(assertions=[{"type": "total_tokens_lt", "value": 100}]),
        replay_observation(total_tokens=-1),
    )
    assert "total_tokens_lt requires numeric evidence" in invalid_evidence["failures"]

    malformed = golden_case(assertions=[{"type": "cost_cents_lt", "value": 0}])
    malformed["expected_output"] = {"contains": ["ok", 1]}
    malformed["expected_trajectory"]["tools"] = [  # type: ignore[index]
        {"name": "search", "future_constraint": True}
    ]
    validation = validate_cases([malformed])
    assert validation["valid"] is False
    errors = " ".join(validation["errors"][0]["errors"])
    assert "expected_output.contains" in errors
    assert "unsupported fields future_constraint" in errors
    assert "value must be a positive number" in errors

    non_finite = golden_case(assertions=[{"type": "cost_cents_lt", "value": float("nan")}])
    assert validate_cases([non_finite])["valid"] is False

    direct_result = evaluate_case(malformed, replay_observation())
    assert direct_result["passed"] is False
    assert direct_result["failures"][0].startswith("invalid behavior contract:")
