from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_core.persistence.repositories.agent_trace_repository import (
    AgentTraceRepository,
    _aggregate_live_case_rows,
)


def _fingerprint(*, prompt: str = "prompt-a") -> dict[str, Any]:
    return {
        "system_prompt_hash": prompt,
        "tool_schema_hash": "tools-a",
        "model_id": "qwen3.7-plus",
        "provider": "dashscope",
        "sampling": {"temperature": 0.5, "max_tokens": 4096},
        "runtime_revision": "runtime-a",
        "rag_config_hash": "rag-a",
        "rag_revision_hash": "rag-revision-a",
        "execution_policy": {"execution_profile": "safe"},
    }


def _run(
    run_id: str,
    *,
    score: float = 0.9,
    latency: float = 100,
    tokens: float = 100,
    cost: float = 1,
    errors: int = 0,
    manifest: str = "manifest-a",
    prompt: str = "prompt-a",
) -> dict[str, Any]:
    case_count = 12
    failed_case_count = max(0, min(errors, case_count))
    critical_failed_count = min(failed_case_count, 1)
    return {
        "run_id": run_id,
        "experiment_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        "run_mode": "live_candidate",
        "status": "succeeded",
        "repetitions": 1,
        "dataset_manifest_hash": manifest,
        "evaluator_suite_hash": "suite-a",
        "score_summary": {
            "schema_version": "eval-gate-metrics/v2",
            "case_count": case_count,
            "score_sum": score * case_count,
            "failed_case_count": failed_case_count,
            "overall_score": score,
            "pass_rate": (case_count - failed_case_count) / case_count,
            "trajectory_case_count": case_count,
            "trajectory_failed_count": failed_case_count,
            "trajectory_pass_rate": (case_count - failed_case_count) / case_count,
            "critical_case_count": 1,
            "critical_failed_count": critical_failed_count,
            "critical_pass_rate": 1.0 - critical_failed_count,
            "stateful_case_count": 0,
            "stateful_failed_count": 0,
            "stateful_pass_rate": None,
            "behavior_pass_rate": 1.0,
            "flaky_rate": 0.0,
        },
        "metrics": {
            "latency_p50_ms": latency,
            "latency_p95_ms": latency,
            "input_tokens_per_task": tokens * 0.6,
            "output_tokens_per_task": tokens * 0.4,
            "total_tokens_per_task": tokens,
            "cost_per_task_cents": cost,
            "execution_error_rate": errors / 12,
            "behavior_failure_rate": 0.0,
            "failed_trials": errors,
            "mixed_runtime": False,
            "actual_fingerprint": _fingerprint(prompt=prompt),
        },
    }


def _cases(
    *,
    failed_case: str | None = None,
    performance_limit: float | None = None,
    latency: float = 100,
) -> list[dict[str, Any]]:
    rows = []
    for index in range(12):
        case_id = f"case-{index:02d}"
        passed = case_id != failed_case
        assertions = (
            [{"type": "latency_ms_lt", "value": performance_limit}]
            if performance_limit is not None and index == 1
            else []
        )
        rows.append(
            {
                "run_case_id": f"run-case-{index}",
                "case_id": case_id,
                "example_id": f"example-{index}",
                "trial_index": 1,
                "status": "succeeded",
                "input": {"message": f"message {index}"},
                "expected_output": {},
                "assertions": assertions,
                "metadata": {"critical": index == 0},
                "observed_metrics": {
                    "case_id": case_id,
                    "trial_index": 1,
                    "trace_id": f"trace-{index}",
                    "execution_succeeded": True,
                    "behavior_pass": passed,
                    "aggregate_score": 0.0 if not passed else 0.9,
                    "latency_ms": latency,
                    "input_tokens": 60,
                    "output_tokens": 40,
                    "total_tokens": 100,
                    "cost_cents": 1.0,
                    "output_preview": f"output {index}",
                    "tool_trajectory": [],
                    "rag_evidence": [],
                    "contract_failures": [] if passed else ["behavior failed"],
                },
            }
        )
    return rows


class _ComparisonRepository(AgentTraceRepository):
    def __init__(
        self,
        baseline: dict[str, Any],
        candidate: dict[str, Any],
        baseline_cases: list[dict[str, Any]],
        candidate_cases: list[dict[str, Any]],
    ) -> None:
        super().__init__(SimpleNamespace(_pool=None, enabled=False))
        self.runs = {baseline["run_id"]: baseline, candidate["run_id"]: candidate}
        self.cases = {
            baseline["run_id"]: baseline_cases,
            candidate["run_id"]: candidate_cases,
        }

    async def get_experiment_run(self, *, run_id: str, **_kwargs: Any) -> dict[str, Any]:
        return self.runs[run_id]

    async def list_experiment_run_cases(
        self, *, run_id: str, **_kwargs: Any
    ) -> list[dict[str, Any]]:
        return self.cases[run_id]


def test_case_metric_aggregate_stays_unknown_when_any_trial_is_missing() -> None:
    trials = _cases()[:1] * 2
    trials[0] = {**trials[0], "run_case_id": "run-case-a", "trial_index": 1}
    trials[1] = {
        **trials[1],
        "run_case_id": "run-case-b",
        "trial_index": 2,
        "observed_metrics": {
            **trials[1]["observed_metrics"],
            "trial_index": 2,
            "cost_cents": None,
            "total_tokens": None,
        },
    }

    metrics = _aggregate_live_case_rows(trials)["case-00"]["observed_metrics"]

    assert metrics["latency_ms"] == 100
    assert metrics["total_tokens"] is None
    assert metrics["cost_cents"] is None


def test_judge_failure_keeps_candidate_execution_success_separate_from_quality() -> None:
    row = _cases()[0]
    row["status"] = "failed"
    row["observed_metrics"] = {
        **row["observed_metrics"],
        "execution_succeeded": True,
        "execution_outcome": "judge_failed",
        "behavior_pass": None,
        "aggregate_score": None,
        "error": "eval_judge_infrastructure_failure",
    }

    case = _aggregate_live_case_rows([row])["case-00"]

    assert case["execution_status"] == "succeeded"
    assert case["status"] == "unscored"
    assert case["behavior_pass"] is None


@pytest.mark.asyncio
async def test_compare_blocks_critical_quality_and_execution_regressions() -> None:
    baseline = _run("baseline")
    candidate = _run("candidate", score=0.86, errors=1, prompt="prompt-b")
    repository = _ComparisonRepository(
        baseline,
        candidate,
        _cases(),
        _cases(failed_case="case-00"),
    )

    comparison = await repository.compare_experiment_runs(
        tenant_id="tenant-a",
        baseline_run_id="baseline",
        candidate_run_id="candidate",
    )

    assert comparison is not None
    assert comparison["compatibility"]["compatible"] is True
    assert comparison["attribution"] == "isolated_change"
    assert comparison["statistics"]["paired_case_count"] == 12
    assert comparison["case_diffs"][0]["case_id"] == "case-00"
    assert comparison["case_diffs"][0]["status"] == "regressed"
    assert comparison["gate"]["status"] == "fail"
    assert set(comparison["gate"]["failures"]) >= {
        "critical_case_regression",
        "quality_regression",
        "execution_error_regression",
    }


@pytest.mark.asyncio
async def test_compare_does_not_call_execution_fault_a_quality_regression() -> None:
    baseline = _run("baseline")
    candidate = _run("candidate", errors=1)
    candidate["status"] = "failed"
    candidate_cases = _cases()
    candidate_cases[0] = {
        **candidate_cases[0],
        "status": "failed",
        "observed_metrics": {
            **candidate_cases[0]["observed_metrics"],
            "execution_succeeded": False,
            "behavior_pass": None,
            "aggregate_score": None,
            "error": "provider unavailable",
        },
    }
    repository = _ComparisonRepository(baseline, candidate, _cases(), candidate_cases)

    comparison = await repository.compare_experiment_runs(
        tenant_id="tenant-a", baseline_run_id="baseline", candidate_run_id="candidate",
    )

    assert comparison is not None
    assert comparison["case_diffs"][0]["status"] == "unscored"
    assert comparison["case_diffs"][0]["candidate_execution_status"] == "failed"
    assert comparison["statistics"]["paired_case_count"] == 11
    assert comparison["statistics"]["evidence_status"] == "unverifiable"
    assert "unscored_case_results" in comparison["compatibility"]["reasons"]
    assert comparison["gate"]["status"] == "fail"


@pytest.mark.asyncio
async def test_compare_identifies_two_verified_fixed_agent_versions() -> None:
    baseline = _run("baseline")
    candidate = _run("candidate")
    for run, version, spec in ((baseline, "version-a", "spec-a"), (candidate, "version-b", "spec-b")):
        identity = {
            "agent_id": "agent-a", "agent_version_id": version,
            "agent_spec_hash": spec,
            "agent_runtime_snapshot_hash": f"sha256:{version}",
        }
        run["target_snapshot"] = {"candidate_type": "agent_version", **identity}
        run["metrics"]["actual_fingerprint"].update(identity)
    repository = _ComparisonRepository(baseline, candidate, _cases(), _cases())

    comparison = await repository.compare_experiment_runs(
        tenant_id="tenant-a", baseline_run_id="baseline", candidate_run_id="candidate",
    )

    assert comparison is not None
    assert comparison["compatibility"]["compatible"] is True
    assert "agent_version" in comparison["changed_dimensions"]
    assert comparison["statistics"]["paired_case_count"] == 12
    assert comparison["gate"]["status"] == "warning"
    assert "fixed_sample_only" in comparison["gate"]["warnings"]


@pytest.mark.asyncio
async def test_compare_blocks_shared_critical_failure_in_fixed_agent_versions() -> None:
    baseline = _run("baseline")
    candidate = _run("candidate")
    for run, version in ((baseline, "version-a"), (candidate, "version-b")):
        identity = {
            "agent_id": "agent-a", "agent_version_id": version,
            "agent_spec_hash": f"spec-{version}",
            "agent_runtime_snapshot_hash": f"sha256:{version}",
        }
        run["target_snapshot"] = {"candidate_type": "agent_version", **identity}
        run["metrics"]["actual_fingerprint"].update(identity)
        run["metrics"]["gate"] = {
            "profile": "agent_version_task_suite", "status": "fail", "reason": "known_hard_failure",
        }
        run["score_summary"].update({
            "score_sum": 9.9,
            "overall_score": 0.825,
            "failed_case_count": 1,
            "pass_rate": 11 / 12,
            "critical_failed_count": 1,
            "critical_pass_rate": 0.0,
            "behavior_pass_rate": 11 / 12,
        })
    repository = _ComparisonRepository(
        baseline, candidate, _cases(failed_case="case-00"), _cases(failed_case="case-00"),
    )

    comparison = await repository.compare_experiment_runs(
        tenant_id="tenant-a", baseline_run_id="baseline", candidate_run_id="candidate",
    )

    assert comparison is not None
    assert comparison["compatibility"]["compatible"] is True
    assert comparison["case_diffs"][0]["status"] == "same_failure"
    assert comparison["gate"]["status"] == "fail"
    assert "candidate_critical_case_failed" in comparison["gate"]["failures"]


@pytest.mark.asyncio
async def test_compare_warns_but_does_not_block_efficiency_increase() -> None:
    baseline = _run("baseline")
    candidate = _run("candidate", latency=150, tokens=130, cost=1.5)
    repository = _ComparisonRepository(baseline, candidate, _cases(), _cases(latency=150))

    comparison = await repository.compare_experiment_runs(
        tenant_id="tenant-a",
        baseline_run_id="baseline",
        candidate_run_id="candidate",
    )

    assert comparison is not None
    assert comparison["gate"]["status"] == "pass"
    assert set(comparison["gate"]["warnings"]) >= {
        "latency_ms_increased",
        "total_tokens_per_task_increased",
        "cost_per_task_cents_increased",
    }


@pytest.mark.asyncio
async def test_compare_blocks_explicit_performance_constraint_and_manifest_mismatch() -> None:
    baseline = _run("baseline")
    candidate = _run("candidate", manifest="manifest-b")
    repository = _ComparisonRepository(
        baseline,
        candidate,
        _cases(),
        _cases(performance_limit=90, latency=100),
    )

    comparison = await repository.compare_experiment_runs(
        tenant_id="tenant-a",
        baseline_run_id="baseline",
        candidate_run_id="candidate",
    )

    assert comparison is not None
    assert comparison["compatibility"]["compatible"] is False
    assert comparison["gate"]["status"] == "fail"
    assert set(comparison["gate"]["failures"]) >= {
        "dataset_manifest_mismatch",
        "explicit_performance_constraint_failed",
    }


@pytest.mark.asyncio
async def test_compare_rejects_forged_versioned_gate_metrics() -> None:
    baseline = _run("baseline")
    candidate = _run("candidate")
    candidate["score_summary"]["failed_case_count"] = 1
    repository = _ComparisonRepository(baseline, candidate, _cases(), _cases())

    comparison = await repository.compare_experiment_runs(
        tenant_id="tenant-a",
        baseline_run_id="baseline",
        candidate_run_id="candidate",
    )

    assert comparison is not None
    assert comparison["compatibility"]["compatible"] is False
    assert "candidate_gate_metrics_unverifiable" in comparison["compatibility"]["reasons"]
    assert comparison["gate"]["status"] == "fail"
