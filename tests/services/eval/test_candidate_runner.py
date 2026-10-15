from __future__ import annotations

import uuid
from datetime import datetime, timezone
from typing import Any

import pytest
from ai_gateway_core.eval.evaluator_executor import (
    REQUIRED_ASSISTANT_HARD_BLOCKERS,
    EvaluatorExecutor,
)

from src.services.eval import eval_outbox_worker as outbox_module
from src.services.eval.eval_candidate_client import EvalCandidateResult
from src.services.eval.eval_outbox_worker import _candidate_cost_cents


def _run_case(case_id: str, trial_index: int) -> dict[str, Any]:
    return {
        "run_case_id": f"{case_id}-{trial_index}",
        "case_id": case_id,
        "example_id": f"example-{case_id}",
        "trial_index": trial_index,
        "status": "queued",
        "input": {"message": f"message {case_id}"},
        "expected_output": {},
        "expected_trajectory": {},
        "assertions": [],
        "metadata": {
            "critical": case_id == "critical" or case_id in REQUIRED_ASSISTANT_HARD_BLOCKERS
        },
        "observed_metrics": {},
    }


def test_candidate_contract_cost_is_unknown_without_catalog_pricing() -> None:
    assert (
        _candidate_cost_cents("unknown-eval-model", {"input_tokens": 1000, "output_tokens": 1000})
        is None
    )


@pytest.mark.asyncio
async def test_candidate_runner_links_trace_only_after_persistence(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class Repository:
        async def list_traces(self, **_kwargs: Any):
            return [], 0

        async def get_trace_detail(self, **_kwargs: Any) -> dict[str, Any]:
            return {
                "trace": {
                    "trace_id": "trace-persisted",
                    "trace_family": "assistant",
                    "status": "succeeded",
                    "output_preview": "EVAL-BASELINE-OK",
                    "total_latency_ms": 10,
                    "model_id": "qwen3.7-plus",
                    "metadata": {},
                },
                "spans": [
                    {
                        "span_kind": "model_invocation",
                        "started_at": datetime.now(timezone.utc),
                    }
                ],
                "events": [],
            }

        async def update_experiment_run_case(self, **kwargs: Any) -> None:
            assert "candidate_trace_id" not in kwargs

    class Candidate:
        async def run(self, **kwargs: Any) -> EvalCandidateResult:
            callback = kwargs.get("on_run_started")
            if callback is not None:
                await callback("trace-persisted")
            return EvalCandidateResult(
                trace_id="trace-persisted",
                output="EVAL-BASELINE-OK",
                usage={"input_tokens": 10, "output_tokens": 2, "total_tokens": 12},
                fingerprint={},
            )

    monkeypatch.setattr(outbox_module, "_eval_candidate_client", Candidate())
    result = await outbox_module._build_candidate_runner(Repository())(
        tenant_id="tenant-a",
        run_case={
            "run_case_id": "run-case-1",
            "case_id": "case-1",
            "input": {"message": "return marker"},
            "expected_output": {"contains": ["EVAL-BASELINE-OK"]},
            "expected_trajectory": {},
            "assertions": [{"type": "no_sensitive_output"}],
            "metadata": {"critical": True},
        },
        execution_config={},
    )

    assert result["contract_result"]["passed"] is True


@pytest.mark.asyncio
async def test_candidate_runner_persists_v2_trace_before_loading_detail(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    trace_id = str(uuid.uuid4())

    class Repository:
        def __init__(self) -> None:
            self.detail: dict[str, Any] | None = None
            self.ingested: list[dict[str, Any]] = []

        async def list_traces(self, **_kwargs: Any):
            return [], 0

        async def get_trace_detail(self, **_kwargs: Any) -> dict[str, Any] | None:
            return self.detail

        async def ingest_trace(self, **kwargs: Any) -> dict[str, Any]:
            self.ingested.append(kwargs)
            self.detail = {
                "trace": {
                    "trace_id": trace_id,
                    "trace_family": "assistant",
                    "status": "succeeded",
                    "output_preview": "EVAL-V2-OK",
                    "total_latency_ms": 12,
                    "model_id": "qwen3.7-plus",
                    "metadata": {},
                },
                "spans": [],
                "events": [],
            }
            return {"trace_id": trace_id, "status": "stored"}

    class Candidate:
        async def run(self, **_kwargs: Any) -> EvalCandidateResult:
            return EvalCandidateResult(
                trace_id=trace_id,
                output="EVAL-V2-OK",
                usage={"input_tokens": 4, "output_tokens": 2, "total_tokens": 6},
                trace_payload={"trace_id": trace_id, "trace_family": "assistant", "user_id": "eval-user"},
            )

    repository = Repository()
    monkeypatch.setattr(outbox_module, "_eval_candidate_client", Candidate())
    result = await outbox_module._build_candidate_runner(repository)(
        tenant_id="tenant-a",
        run_case={
            "run_case_id": "run-case-v2",
            "case_id": "case-v2",
            "input": {"message": "return marker"},
            "expected_output": {"contains": ["EVAL-V2-OK"]},
            "expected_trajectory": {},
            "assertions": [],
            "metadata": {},
        },
        execution_config={},
    )

    assert result["trace_id"] == trace_id
    assert len(repository.ingested) == 1
    assert repository.ingested[0]["created_by"] == "eval-user"
    assert repository.ingested[0]["enqueue"] is False


@pytest.mark.asyncio
async def test_agent_version_worker_uses_creator_delegation_even_with_admin_token(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    trace_id = str(uuid.uuid4())
    actor = {
        "actor": "eval-worker", "subject": "creator", "tenant_id": "tenant-a",
        "run_id": "run", "job_id": "job",
    }
    expected_ref = {
        "tenant_id": "tenant-a", "provider_id": "provider", "model_id": "model",
        "capability_revision": 1, "price_version": "price-v1",
    }
    identity = {
        "agent_id": "agent-a", "agent_version_id": "version-a",
        "agent_spec_hash": "a" * 64,
        "agent_runtime_snapshot_hash": "sha256:" + "b" * 64,
    }

    class Repository:
        def __init__(self) -> None:
            self.persisted = False
            self.actor_calls = 0

        async def get_trace_detail(self, **_kwargs: Any) -> dict[str, Any] | None:
            if not self.persisted:
                return None
            return {"trace": {
                "trace_id": trace_id, "status": "succeeded", "model_id": "model",
                "provider": "provider", "output_preview": "ok", "metadata": {},
            }, "spans": [], "events": []}

        async def list_traces(self, **_kwargs: Any) -> tuple[list[dict[str, Any]], int]:
            return [], 0

        async def resolve_eval_job_actor(self, **_kwargs: Any) -> dict[str, str]:
            self.actor_calls += 1
            return actor

        async def get_candidate_runtime_evidence(self, **_kwargs: Any) -> dict[str, Any]:
            return {}

        async def ingest_trace(self, **_kwargs: Any) -> None:
            self.persisted = True

    class Candidate:
        token = "worker-admin-token"
        api_key = ""
        allow_service_identity = True

        async def run(self, **kwargs: Any) -> EvalCandidateResult:
            assert kwargs["delegation"] == actor
            return EvalCandidateResult(
                trace_id=trace_id, output="ok", fingerprint=identity,
                trace_payload={"user_id": "creator", "spans": []},
            )

    repository = Repository()
    monkeypatch.setattr(outbox_module, "_eval_candidate_client", Candidate())
    monkeypatch.setattr(outbox_module, "runtime_model_evidence", lambda **_kwargs: ({
        "model_id": "model", "provider": "provider", "runtime_revision": "runtime-a",
        "model_ref": expected_ref, "model_ref_verified": True,
    }, {}, []))
    result = await outbox_module._build_candidate_runner(repository)(
        tenant_id="tenant-a",
        run_case={
            "run_case_id": "case-a", "case_id": "case-a", "input": {"message": "hello"},
            "expected_output": {}, "expected_trajectory": {}, "assertions": [], "metadata": {},
        },
        execution_config={
            "candidate_type": "agent_version", **identity, "model_ref": expected_ref,
        },
    )

    assert repository.actor_calls == 1
    assert result["fingerprint"]["agent_version_id"] == "version-a"
    assert result["fingerprint"]["model_ref_verified"] is True


class _LiveRepository:
    def __init__(self, cases: list[dict[str, Any]]) -> None:
        self.cases = cases
        self.run = {
            "run_id": "run-live",
            "execution_config": {
                "evaluators": [
                    {
                        "evaluator_id": "rule-a",
                        "name": "status",
                        "evaluator_type": "rule",
                        "version": "v1",
                        "filter_config": {"rules": [{"type": "status_eq", "value": "succeeded"}]},
                    },
                    {
                        "evaluator_id": "trajectory-a",
                        "name": "trajectory",
                        "evaluator_type": "trajectory",
                        "version": "v1",
                        "filter_config": {},
                    },
                ]
            },
        }
        self.run_updates: list[dict[str, Any]] = []
        self.scores: list[dict[str, Any]] = []

    async def update_experiment_run(self, **kwargs: Any) -> None:
        self.run_updates.append(kwargs)

    async def get_experiment_run(self, **_kwargs: Any) -> dict[str, Any]:
        return self.run

    async def list_experiment_run_cases(self, **_kwargs: Any) -> list[dict[str, Any]]:
        return self.cases

    async def update_experiment_run_case(self, *, run_case_id: str, **kwargs: Any) -> None:
        row = next(item for item in self.cases if item["run_case_id"] == run_case_id)
        row.update(kwargs)

    async def create_eval_score(self, **kwargs: Any) -> dict[str, Any]:
        self.scores.append(kwargs)
        payload = kwargs["payload"]
        return {
            "score_id": f"score-{len(self.scores)}",
            "numeric_value": payload.get("numeric_value"),
            "label": payload.get("label"),
        }


def _candidate_result(run_case: dict[str, Any]) -> dict[str, Any]:
    trace_id = f"trace-{run_case['run_case_id']}"
    latency = 100 if int(run_case["trial_index"]) == 1 else 200
    return {
        "detail": {
            "trace": {
                "trace_id": trace_id,
                "trace_family": "assistant",
                "status": "succeeded",
                "total_latency_ms": latency,
                "model_id": "qwen3.7-plus",
                "provider": "dashscope",
                "output_preview": "ok",
                "metadata": {"runtime_trajectory": {"exit_reason": "completed"}},
            },
            "spans": [
                {
                    "span_kind": "tool_execution",
                    "name": "lookup",
                    "status": "succeeded",
                    "sequence_no": 1,
                }
            ],
            "events": [],
        },
        "usage": {"input_tokens": 60, "output_tokens": 40, "total_tokens": 100},
        "fingerprint": {
            "system_prompt_hash": "prompt-a",
            "tool_schema_hash": "tools-a",
            "model_id": "qwen3.7-plus",
            "provider": "dashscope",
            "runtime_revision": "runtime-a",
        },
        "contract_result": {
            "passed": True,
            "trajectory_pass": True,
            "stateful_pass": None,
            "failures": [],
        },
    }


@pytest.mark.asyncio
async def test_live_executor_runs_each_trial_once_and_reuses_trace_for_evaluator_suite() -> None:
    repository = _LiveRepository(
        [
            _run_case("critical", 1),
            _run_case("critical", 2),
            _run_case("normal", 1),
            _run_case("normal", 2),
        ]
    )
    calls: list[str] = []

    async def run_candidate(**kwargs: Any) -> dict[str, Any]:
        calls.append(kwargs["run_case"]["run_case_id"])
        return _candidate_result(kwargs["run_case"])

    result = await EvaluatorExecutor(
        repository,  # type: ignore[arg-type]
        candidate_run=run_candidate,
    ).run_job(
        tenant_id="tenant-a",
        job_payload={
            "run_id": "run-live",
            "evaluator_id": "rule-a",
            "run_mode": "live_candidate",
        },
    )

    assert result.status == "succeeded"
    assert len(calls) == 4
    assert len(repository.scores) == 12
    final = repository.run_updates[-1]
    assert final["metrics"]["attempted_trials"] == 4
    assert final["metrics"]["latency_p50_ms"] == 150
    assert final["metrics"]["total_tokens_per_task"] == 100
    assert final["score_summary"]["behavior_pass_rate"] == 1.0
    assert final["score_summary"]["schema_version"] == "eval-gate-metrics/v2"
    assert final["score_summary"]["score_sum"] == 2.0
    assert final["score_summary"]["trajectory_case_count"] == 2
    assert final["score_summary"]["trajectory_failed_count"] == 0
    assert final["score_summary"]["trajectory_pass_rate"] == 1.0
    assert final["metrics"]["mixed_runtime"] is False
    assert all(case["status"] == "succeeded" for case in repository.cases)


@pytest.mark.asyncio
async def test_live_execution_failure_does_not_become_zero_quality_or_replay() -> None:
    repository = _LiveRepository([_run_case("critical", 1), _run_case("normal", 1)])
    calls: list[str] = []

    async def run_candidate(**kwargs: Any) -> dict[str, Any]:
        case = kwargs["run_case"]
        calls.append(case["case_id"])
        if case["case_id"] == "normal":
            raise RuntimeError("provider unavailable")
        return _candidate_result(case)

    executor = EvaluatorExecutor(repository, candidate_run=run_candidate)  # type: ignore[arg-type]
    result = await executor.run_job(
        tenant_id="tenant-a",
        job_payload={"run_id": "run-live", "evaluator_id": "rule-a", "run_mode": "live_candidate"},
    )

    assert result.status == "failed"
    assert result.score_summary["case_count"] == 1
    assert result.score_summary["excluded_case_count"] == 1
    assert result.score_summary["overall_score"] == 1.0
    assert result.metrics["gate"]["status"] == "unavailable"
    assert repository.cases[1]["observed_metrics"]["behavior_pass"] is None

    await executor.run_job(
        tenant_id="tenant-a",
        job_payload={"run_id": "run-live", "evaluator_id": "rule-a", "run_mode": "live_candidate"},
    )
    assert calls == ["critical", "normal"]


@pytest.mark.asyncio
async def test_live_judge_failure_keeps_execution_separate_from_quality() -> None:
    repository = _LiveRepository([_run_case("critical", 1)])

    async def run_candidate(**kwargs: Any) -> dict[str, Any]:
        return _candidate_result(kwargs["run_case"])

    executor = EvaluatorExecutor(repository, candidate_run=run_candidate)  # type: ignore[arg-type]

    async def fail_judge(*_args: Any, **_kwargs: Any) -> list[dict[str, Any]]:
        raise RuntimeError("judge unavailable")

    executor._score_target_payloads = fail_judge  # type: ignore[method-assign]
    result = await executor.run_job(
        tenant_id="tenant-a",
        job_payload={"run_id": "run-live", "evaluator_id": "rule-a", "run_mode": "live_candidate"},
    )

    assert result.status == "failed"
    assert result.score_summary["case_count"] == 0
    assert result.score_summary["overall_score"] is None
    assert result.metrics["completed_trials"] == 1
    assert result.metrics["unscored_trials"] == 1
    assert repository.cases[0]["observed_metrics"]["execution_outcome"] == "judge_failed"


@pytest.mark.asyncio
async def test_live_agent_version_mismatch_cannot_be_scored() -> None:
    repository = _LiveRepository([_run_case("critical", 1)])
    repository.run["execution_config"].update({
        "candidate_type": "agent_version", "agent_id": "agent-a",
        "agent_version_id": "version-a", "agent_spec_hash": "a" * 64,
        "agent_runtime_snapshot_hash": "sha256:" + "b" * 64,
    })

    async def run_candidate(**kwargs: Any) -> dict[str, Any]:
        return _candidate_result(kwargs["run_case"])

    result = await EvaluatorExecutor(
        repository, candidate_run=run_candidate,  # type: ignore[arg-type]
    ).run_job(
        tenant_id="tenant-a",
        job_payload={"run_id": "run-live", "evaluator_id": "rule-a", "run_mode": "live_candidate"},
    )

    assert result.status == "failed"
    assert result.score_summary["case_count"] == 0
    assert "AGENT_EVAL_VERSION_FINGERPRINT_MISMATCH" in str(result.error_message)
    assert repository.scores == []


@pytest.mark.asyncio
async def test_typed_agent_suite_is_warning_without_builtin_hard_blocker_cases() -> None:
    repository = _LiveRepository([_run_case("critical", 1)])
    identity = {
        "candidate_type": "agent_version", "agent_id": "agent-a",
        "agent_version_id": "version-a", "agent_spec_hash": "a" * 64,
        "agent_runtime_snapshot_hash": "sha256:" + "b" * 64,
    }
    repository.run["execution_config"].update(identity)

    async def run_candidate(**kwargs: Any) -> dict[str, Any]:
        result = _candidate_result(kwargs["run_case"])
        result["fingerprint"].update(identity)
        return result

    result = await EvaluatorExecutor(
        repository, candidate_run=run_candidate,  # type: ignore[arg-type]
    ).run_job(
        tenant_id="tenant-a",
        job_payload={"run_id": "run-live", "evaluator_id": "rule-a", "run_mode": "live_candidate"},
    )

    assert result.status == "succeeded"
    assert result.metrics["gate"] == {
        "profile": "agent_version_task_suite", "reason": "fixed_sample_only", "status": "warning",
    }
    assert result.metrics["hard_blockers_passed"] is None
    assert result.metrics["unknown_side_effect_trial_count"] == 0


@pytest.mark.asyncio
async def test_typed_agent_unknown_side_effect_is_hard_failure() -> None:
    repository = _LiveRepository([_run_case("critical", 1)])
    identity = {
        "candidate_type": "agent_version", "agent_id": "agent-a",
        "agent_version_id": "version-a", "agent_spec_hash": "a" * 64,
        "agent_runtime_snapshot_hash": "sha256:" + "b" * 64,
    }
    repository.run["execution_config"].update(identity)

    async def run_candidate(**kwargs: Any) -> dict[str, Any]:
        result = _candidate_result(kwargs["run_case"])
        result["fingerprint"].update(identity)
        result["detail"]["trace"]["metadata"]["runtime_trajectory"]["exit_reason"] = "side_effect_unknown"
        return result

    result = await EvaluatorExecutor(
        repository, candidate_run=run_candidate,  # type: ignore[arg-type]
    ).run_job(
        tenant_id="tenant-a",
        job_payload={"run_id": "run-live", "evaluator_id": "rule-a", "run_mode": "live_candidate"},
    )

    assert result.metrics["unknown_side_effect_trial_count"] == 1
    assert result.metrics["gate"]["status"] == "fail"


@pytest.mark.asyncio
async def test_live_gate_requires_critical_and_mandatory_safety_cases() -> None:
    async def run_candidate(**kwargs: Any) -> dict[str, Any]:
        return _candidate_result(kwargs["run_case"])

    missing = _LiveRepository([_run_case("normal", 1)])
    await EvaluatorExecutor(
        missing,  # type: ignore[arg-type]
        candidate_run=run_candidate,
    ).run_job(
        tenant_id="tenant-a",
        job_payload={
            "run_id": "run-live",
            "evaluator_id": "rule-a",
            "run_mode": "live_candidate",
        },
    )
    missing_metrics = missing.run_updates[-1]["metrics"]
    assert missing_metrics["critical_case_count"] == 0
    assert missing_metrics["hard_blockers_passed"] is False
    assert missing_metrics["gate"]["status"] == "fail"

    covered = _LiveRepository(
        [_run_case(case_id, 1) for case_id in REQUIRED_ASSISTANT_HARD_BLOCKERS]
    )
    await EvaluatorExecutor(
        covered,  # type: ignore[arg-type]
        candidate_run=run_candidate,
    ).run_job(
        tenant_id="tenant-a",
        job_payload={
            "run_id": "run-live",
            "evaluator_id": "rule-a",
            "run_mode": "live_candidate",
        },
    )
    covered_metrics = covered.run_updates[-1]["metrics"]
    assert covered_metrics["critical_case_count"] == 2
    assert covered_metrics["hard_blockers_passed"] is True
    assert covered_metrics["gate"]["status"] == "pass"


@pytest.mark.asyncio
async def test_live_executor_retry_reuses_completed_trial_without_candidate_call() -> None:
    completed = _run_case("critical", 1)
    completed.update(
        {
            "status": "succeeded",
            "candidate_trace_id": "trace-existing",
            "observed_metrics": {
                "case_id": "critical",
                "trial_index": 1,
                "critical": True,
                "trace_id": "trace-existing",
                "execution_succeeded": True,
                "behavior_pass": True,
                "aggregate_score": 1.0,
                "latency_ms": 100,
                "input_tokens": 60,
                "output_tokens": 40,
                "total_tokens": 100,
                "cost_cents": 0.1,
                "fingerprint": {
                    "system_prompt_hash": "prompt-a",
                    "tool_schema_hash": "tools-a",
                    "model_id": "qwen3.7-plus",
                    "provider": "dashscope",
                    "runtime_revision": "runtime-a",
                },
            },
        }
    )
    pending = _run_case("critical", 2)
    repository = _LiveRepository([completed, pending])
    calls: list[str] = []

    async def run_candidate(**kwargs: Any) -> dict[str, Any]:
        calls.append(kwargs["run_case"]["run_case_id"])
        return _candidate_result(kwargs["run_case"])

    result = await EvaluatorExecutor(
        repository,  # type: ignore[arg-type]
        candidate_run=run_candidate,
    ).run_job(
        tenant_id="tenant-a",
        job_payload={
            "run_id": "run-live",
            "evaluator_id": "rule-a",
            "run_mode": "live_candidate",
        },
    )

    assert result.status == "succeeded"
    assert calls == ["critical-2"]


@pytest.mark.asyncio
async def test_live_executor_does_not_verify_partial_fingerprint() -> None:
    repository = _LiveRepository([_run_case("critical", 1)])

    async def run_candidate(**kwargs: Any) -> dict[str, Any]:
        candidate = _candidate_result(kwargs["run_case"])
        candidate["fingerprint"] = {}
        return candidate

    result = await EvaluatorExecutor(
        repository,  # type: ignore[arg-type]
        candidate_run=run_candidate,
    ).run_job(
        tenant_id="tenant-a",
        job_payload={
            "run_id": "run-live",
            "evaluator_id": "rule-a",
            "run_mode": "live_candidate",
        },
    )

    assert result.status == "succeeded"
    final_metrics = repository.run_updates[-1]["metrics"]
    assert final_metrics["fingerprint_complete"] is False
    assert final_metrics["actual_fingerprint"] == {}
