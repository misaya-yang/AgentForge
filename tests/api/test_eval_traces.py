from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from ai_gateway_core.eval.evaluator_executor import REQUIRED_ASSISTANT_HARD_BLOCKERS
from ai_gateway_core.persistence.repositories.agent_trace_repository import (
    AgentTraceRepository,
)
from fastapi import HTTPException
from pydantic import ValidationError

from src.api.deps import AuthContext
from src.api.schemas.eval import (
    AgentTraceScoreCreate,
    EvalBaselinePromotionRequest,
    EvalDatasetCreate,
    EvalEvaluatorCreate,
    EvalEvaluatorRunRequest,
    EvalExampleFromTraceCreate,
    EvalExamplesImportRequest,
    EvalExampleUpdate,
    EvalExperimentCreate,
    EvalExperimentRunCreate,
    EvalGateDryRunRequest,
    EvalKbFailureSave,
    EvalTraceFeedbackRequest,
    KbRagasBatchScoreRequest,
    KbRagasScoreRetrievalRequest,
)
from src.api.v1 import _eval_dataset_routes as eval_dataset_routes_module
from src.api.v1 import eval as eval_routes
from src.api.v1.eval import (
    batch_score_kb_ragas_dataset,
    compare_eval_experiment_runs,
    create_eval_dataset,
    create_eval_evaluator,
    create_eval_example_from_trace,
    create_eval_experiment,
    create_eval_trace_score,
    delete_eval_example,
    dry_run_eval_gate,
    export_eval_examples,
    export_eval_trace,
    get_eval_dashboard,
    get_eval_dataset,
    get_eval_evaluator,
    get_eval_experiment,
    get_eval_experiment_run,
    get_eval_experiment_run_results,
    get_eval_summary,
    get_eval_trace_detail,
    get_eval_trace_thread,
    get_kb_ragas_summary_endpoint,
    get_latest_kb_failure_example,
    import_eval_examples,
    list_eval_datasets,
    list_eval_evaluators,
    list_eval_examples,
    list_eval_experiments,
    list_eval_traces,
    preview_eval_trace_feedback,
    promote_eval_experiment_baseline,
    run_eval_evaluator_async,
    run_eval_experiment,
    save_kb_failure_example,
    score_kb_ragas_retrieval,
    update_eval_example,
)
from src.config.settings import Settings
from src.core.auth.permissions import Capability
from src.core.auth.rbac import RBAC
from src.core.auth.user_resolver import UserContext
from src.main import create_app


def _request() -> SimpleNamespace:
    settings = Settings()
    request = SimpleNamespace()
    request.app = SimpleNamespace()
    request.app.state = SimpleNamespace(settings=SimpleNamespace(default_model="fixture-model"))
    request.app.state.dispatcher = SimpleNamespace(rbac=RBAC(role_permissions=settings.rbac.roles))
    request.app.state.database = SimpleNamespace(enabled=True)
    request.state = SimpleNamespace(request_id="req-eval-trace")
    request.headers = {}
    request.url = SimpleNamespace(path="/api/v1/eval/traces")
    return request


def _auth(
    *,
    user_id: str = "user-a",
    tenant_id: str = "tenant-a",
    roles: list[str] | None = None,
    permissions: list[str] | None = None,
) -> AuthContext:
    return AuthContext(
        user_id=user_id,
        tenant_id=tenant_id,
        roles=roles or ["user"],
        permissions=permissions or ["console:eval:view"],
        is_authenticated=True,
    )


def _trace_row(**overrides: Any) -> dict[str, Any]:
    now = datetime.now(timezone.utc)
    row = {
        "trace_id": "11111111-1111-4111-8111-111111111111",
        "trace_family": "assistant",
        "workflow_kind": "ai_assistant_chat",
        "tenant_id": "tenant-a",
        "user_id": "user-a",
        "thread_id": "session-a",
        "session_id": "session-a",
        "run_id": "run-a",
        "request_id": "request-a",
        "model_id": "qwen3.6-plus",
        "provider": "qwen",
        "status": "succeeded",
        "started_at": now,
        "ended_at": now,
        "first_token_latency_ms": 120,
        "total_latency_ms": 980,
        "input_tokens": 10,
        "output_tokens": 20,
        "total_tokens": 30,
        "total_cost_cents": 0,
        "input_preview": "hello",
        "output_preview": "hi",
        "redaction_state": {"input": "preview"},
        "metadata": {"redacted": True},
        "metrics": {"total_latency_ms": 980},
        "privacy": {"payloads": "bounded_redacted_preview"},
        "source_adapter": "agent-runtime",
        "scores_count": 0,
        "created_at": now,
        "updated_at": now,
    }
    row.update(overrides)
    return row


def _score_row(**overrides: Any) -> dict[str, Any]:
    row = {
        "score_id": "22222222-2222-4222-8222-222222222222",
        "trace_id": "11111111-1111-4111-8111-111111111111",
        "span_id": None,
        "score_name": "quality",
        "score_type": "numeric",
        "numeric_value": 0.9,
        "boolean_value": None,
        "categorical_value": None,
        "text_value": None,
        "label": "good",
        "explanation": "grounded answer",
        "scorer_type": "human",
        "evaluator_version": None,
        "target_type": "trace",
        "target_id": "11111111-1111-4111-8111-111111111111",
        "evaluator_id": None,
        "evaluator_name": None,
        "score_source": "human",
        "confidence": None,
        "created_by": "user-a",
        "metadata": {},
        "created_at": datetime.now(timezone.utc),
    }
    row.update(overrides)
    return row


def _gate_summary(
    *,
    overall_score: float,
    failed_case_count: int,
    trajectory_failed_count: int,
) -> dict[str, Any]:
    case_count = 25
    return {
        "schema_version": "eval-gate-metrics/v2",
        "case_count": case_count,
        "score_sum": overall_score * case_count,
        "failed_case_count": failed_case_count,
        "overall_score": overall_score,
        "average_score": overall_score,
        "pass_rate": (case_count - failed_case_count) / case_count,
        "trajectory_case_count": case_count,
        "trajectory_failed_count": trajectory_failed_count,
        "trajectory_pass_rate": (case_count - trajectory_failed_count) / case_count,
        "critical_case_count": 5,
        "critical_failed_count": 0,
        "critical_pass_rate": 1.0,
        "stateful_case_count": 1,
        "stateful_failed_count": 0,
        "stateful_pass_rate": 1.0,
    }


class FakeTraceRepository:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any]]] = []
        self.detail: dict[str, Any] | None = {
            "trace": _trace_row(scores_count=1),
            "spans": [],
            "events": [],
            "scores": [_score_row()],
        }
        self.score: dict[str, Any] | None = _score_row()
        self.dataset = {
            "dataset_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "tenant_id": "tenant-a",
            "name": "assistant-regression",
            "description": "regression set",
            "version": "v1",
            "schema": {},
            "metadata": {},
            "created_by": "user-a",
            "created_at": datetime.now(timezone.utc),
            "updated_at": datetime.now(timezone.utc),
        }
        self.example = {
            "example_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "dataset_id": self.dataset["dataset_id"],
            "tenant_id": "tenant-a",
            "split": "regression",
            "input": {"input_preview": "hello"},
            "expected_output": {"output_preview": "hi"},
            "metadata": {},
            "source_trace_id": "11111111-1111-4111-8111-111111111111",
            "source_span_id": None,
            "created_by": "user-a",
            "created_at": datetime.now(timezone.utc),
        }
        self.imported_case_ids: set[str] = set()
        self.example_exists = True
        self.kb_failure_rows: list[dict[str, Any]] = []
        self.evaluator = {
            "evaluator_id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            "tenant_id": "tenant-a",
            "name": "quality",
            "evaluator_type": "human",
            "rubric": "score quality",
            "version": "v1",
            "sampling_config": {},
            "filter_config": {},
            "metadata": {},
            "created_by": "user-a",
            "created_at": datetime.now(timezone.utc),
            "updated_at": datetime.now(timezone.utc),
        }
        self.experiment = {
            "experiment_id": "dddddddd-dddd-4ddd-8ddd-dddddddddddd",
            "tenant_id": "tenant-a",
            "dataset_id": self.dataset["dataset_id"],
            "name": "baseline",
            "description": "",
            "target_config": {"model": "qwen"},
            "metadata": {},
            "created_by": "user-a",
            "created_at": datetime.now(timezone.utc),
            "updated_at": datetime.now(timezone.utc),
            "runs": [],
        }
        self.baseline_run = {
            "run_id": "12121212-1212-4212-8212-121212121212",
            "experiment_id": self.experiment["experiment_id"],
            "tenant_id": "tenant-a",
            "evaluator_id": self.evaluator["evaluator_id"],
            "dataset_id": self.dataset["dataset_id"],
            "status": "succeeded",
            "target_snapshot": {"candidate_label": "baseline"},
            "score_summary": _gate_summary(
                overall_score=0.9,
                failed_case_count=2,
                trajectory_failed_count=0,
            ),
            "metrics": {"targets": 10},
            "error_message": None,
            "created_by": "user-a",
            "started_at": datetime.now(timezone.utc),
            "finished_at": datetime.now(timezone.utc),
            "created_at": datetime.now(timezone.utc),
            "updated_at": datetime.now(timezone.utc),
        }
        self.candidate_run = {
            **self.baseline_run,
            "run_id": "34343434-3434-4434-8434-343434343434",
            "target_snapshot": {"candidate_label": "candidate"},
            "score_summary": _gate_summary(
                overall_score=0.88,
                failed_case_count=3,
                trajectory_failed_count=1,
            ),
        }

    async def list_traces(self, **kwargs: Any) -> tuple[list[dict[str, Any]], int]:
        self.calls.append(("list", kwargs))
        return [_trace_row()], 1

    async def get_trace_detail(self, **kwargs: Any) -> dict[str, Any] | None:
        self.calls.append(("detail", kwargs))
        return self.detail

    async def create_score(self, **kwargs: Any) -> dict[str, Any] | None:
        self.calls.append(("score", kwargs))
        return self.score

    async def ingest_trace(self, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("ingest", kwargs))
        return {
            "trace_id": "11111111-1111-4111-8111-111111111111",
            "status": "stored",
            "job_id": None,
        }

    async def get_thread(self, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("thread", kwargs))
        return {
            "thread_id": kwargs["thread_id"],
            "traces": [_trace_row()],
            "total": 1,
            "metrics": {"trace_count": 1, "total_latency_ms": 980},
        }

    async def create_dataset(self, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("dataset", kwargs))
        return self.dataset

    async def get_dataset(self, **kwargs: Any) -> dict[str, Any] | None:
        self.calls.append(("get_dataset", kwargs))
        return self.dataset

    async def create_example_from_trace(self, **kwargs: Any) -> dict[str, Any] | None:
        self.calls.append(("example", kwargs))
        return self.example

    async def get_example(self, **kwargs: Any) -> dict[str, Any] | None:
        self.calls.append(("get_example", kwargs))
        return self.example if self.example_exists else None

    async def get_latest_kb_failure_example(self, **kwargs: Any) -> dict[str, Any] | None:
        self.calls.append(("get_latest_kb_failure_example", kwargs))
        rows = [row for row in self.kb_failure_rows if row["metadata"]["case_id"] == kwargs["case_id"]]
        return max(rows, key=lambda row: row["metadata"]["case_revision"]) if rows else None

    async def save_kb_failure_revision(self, **kwargs: Any) -> tuple[dict[str, Any], bool]:
        self.calls.append(("save_kb_failure_revision", kwargs))
        payload = kwargs["payload"]
        current = await self.get_latest_kb_failure_example(case_id=payload["case_id"])
        revision = current["metadata"]["case_revision"] + 1 if current else 1
        row = {
            **self.example,
            "example_id": f"{revision:08d}-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "split": "review",
            "input": {"query": payload["query"]},
            "expected_output": {"answer": payload["expected_answer"]},
            "source_trace_id": payload.get("source_trace_id"),
            "metadata": {
                "case_id": payload["case_id"],
                "case_revision": revision,
                "source_kind": "kb_failure",
                "review_status": "pending",
                "kb_dataset_id": payload["kb_dataset_id"],
                "kb_observed_segment_ids": payload["observed_segment_ids"],
                "kb_source_versions": payload["source_versions"],
                "kb_source_versions_verified": payload["source_versions_verified"],
            },
        }
        self.kb_failure_rows.append(row)
        return row, True

    async def update_example(self, **kwargs: Any) -> dict[str, Any] | None:
        self.calls.append(("update_example", kwargs))
        payload = kwargs["payload"]
        metadata = {
            **self.example["metadata"],
            **payload.get("metadata", {}),
        }
        for key in (
            "expected_trajectory",
            "assertions",
            "tags",
            "difficulty",
            "owner",
            "review_status",
        ):
            if payload.get(key) is not None:
                metadata[key] = payload[key]
        self.example = {
            **self.example,
            "split": payload.get("split") or self.example["split"],
            "input": payload.get("input") or self.example["input"],
            "expected_output": payload.get("expected_output") or self.example["expected_output"],
            "metadata": metadata,
        }
        return self.example

    async def delete_example(self, **kwargs: Any) -> bool:
        self.calls.append(("delete_example", kwargs))
        matches = (
            self.example_exists
            and kwargs["tenant_id"] == self.example["tenant_id"]
            and kwargs["dataset_id"] == self.example["dataset_id"]
            and kwargs["example_id"] == self.example["example_id"]
        )
        if matches:
            self.example_exists = False
        return matches

    async def list_dataset_example_case_ids(self, **kwargs: Any) -> set[str]:
        self.calls.append(("list_dataset_example_case_ids", kwargs))
        return set(self.imported_case_ids)

    async def import_examples(self, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("import_examples", kwargs))
        mode = kwargs.get("mode", "skip_duplicates")
        existing_case_ids = (
            await self.list_dataset_example_case_ids(
                tenant_id=kwargs["tenant_id"],
                dataset_id=kwargs["dataset_id"],
            )
            if mode == "skip_duplicates"
            else set()
        )
        seen_in_request: set[str] = set()
        examples = []
        skipped = 0
        for item in kwargs["examples"]:
            case_id = str(item.get("case_id") or "").strip()
            if (
                mode == "skip_duplicates"
                and case_id
                and (case_id in existing_case_ids or case_id in seen_in_request)
            ):
                skipped += 1
                continue
            examples.append(
                {
                    **self.example,
                    "example_id": f"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbb{len(examples)}",
                    "input": item.get("input") or {},
                    "expected_output": item.get("expected_output") or {},
                    "metadata": {
                        "case_id": item.get("case_id"),
                        "expected_trajectory": item.get("expected_trajectory") or {},
                        "assertions": item.get("assertions") or [],
                        **(item.get("metadata") or {}),
                    },
                }
            )
            if case_id:
                existing_case_ids.add(case_id)
                seen_in_request.add(case_id)
                self.imported_case_ids.add(case_id)
        return {"imported": len(examples), "skipped": skipped, "examples": examples}

    async def create_evaluator(self, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("evaluator", kwargs))
        return self.evaluator

    async def create_experiment(self, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("experiment", kwargs))
        return self.experiment

    async def get_experiment(self, **kwargs: Any) -> dict[str, Any] | None:
        self.calls.append(("experiment_detail", kwargs))
        return self.experiment

    async def enqueue_evaluator_run(self, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("evaluator_run", kwargs))
        return {
            "job_id": "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee",
            "status": "queued",
            "run_id": "ffffffff-ffff-4fff-8fff-ffffffffffff",
        }

    async def enqueue_live_experiment_run(self, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("live_experiment_run", kwargs))
        return {
            "job_id": "abababab-abab-4bab-8bab-abababababab",
            "status": "queued",
            "run_id": "cdcdcdcd-cdcd-4dcd-8dcd-cdcdcdcdcdcd",
        }

    async def list_datasets(self, **kwargs: Any) -> tuple[list[dict[str, Any]], int]:
        self.calls.append(("list_datasets", kwargs))
        return [self.dataset], 1

    async def list_dataset_manifest(self, **kwargs: Any) -> list[dict[str, Any]]:
        self.calls.append(("list_dataset_manifest", kwargs))
        return [self.dataset]

    async def list_examples(self, **kwargs: Any) -> tuple[list[dict[str, Any]], int]:
        self.calls.append(("list_examples", kwargs))
        return [self.example], 1

    async def list_example_manifest(self, **kwargs: Any) -> list[dict[str, Any]]:
        self.calls.append(("example_manifest", kwargs))
        return [self.example]

    async def list_evaluators(self, **kwargs: Any) -> tuple[list[dict[str, Any]], int]:
        self.calls.append(("list_evaluators", kwargs))
        return [self.evaluator], 1

    async def list_experiments(self, **kwargs: Any) -> tuple[list[dict[str, Any]], int]:
        self.calls.append(("list_experiments", kwargs))
        return [self.experiment], 1

    async def get_evaluator(self, **kwargs: Any) -> dict[str, Any] | None:
        self.calls.append(("get_evaluator", kwargs))
        return self.evaluator

    async def get_kb_ragas_summary(self, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("kb_ragas_summary", kwargs))
        return {
            "window_days": kwargs.get("days", 7),
            "dataset_id": kwargs.get("dataset_id"),
            "rag_traces": 2,
            "ragas_scored_traces": 1,
            "metrics": [
                {
                    "metric": "context_relevancy",
                    "average_score": 0.86,
                    "scored_count": 1,
                    "pass_count": 1,
                    "fail_count": 0,
                    "review_count": 0,
                }
            ],
            "latest_judge_model": "qwen-test",
        }

    async def trace_has_kb_ragas_score(self, **kwargs: Any) -> bool:
        self.calls.append(("trace_has_kb_ragas_score", kwargs))
        return False

    async def has_active_evaluator_run_for_trace(self, **kwargs: Any) -> bool:
        self.calls.append(("has_active_evaluator_run_for_trace", kwargs))
        return False

    async def get_summary(self, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("summary", kwargs))
        return {
            "total_traces": 12,
            "failed_traces": 1,
            "succeeded_traces": 11,
            "assistant_traces": 8,
            "langgraph_traces": 2,
            "rag_traces": 2,
            "avg_latency_ms": 420,
            "p95_latency_ms": 980,
            "total_tokens": 1500,
            "total_cost_cents": 12,
            "scored_traces": 3,
            "window_days": kwargs.get("days", 7),
        }

    async def get_dashboard(self, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("dashboard", kwargs))
        return {
            "metrics": {
                "total_traces": 12,
                "scored_traces": 3,
                "example_count": 10,
                "pass_rate": 0.9,
                "trajectory_pass_rate": 0.96,
                "critical_failures": 0,
                "judge_pending_count": 2,
            },
            "run_health": {"succeeded_runs": 2, "failed_runs": 0},
            "queue_health": {"queued_jobs": 0, "failed_jobs": 1},
            "runtime_health": {
                "assistant_status": "enabled",
                "assistant_captured_traces": 8,
                "runtime_trajectory_traces": 7,
                "pass_rate": 0.9,
                "trajectory_pass_rate": 0.875,
                "tool_safety_failures": 1,
                "critical_runtime_failures": 1,
                "rag_status": "wired",
                "langgraph_status": "partial",
            },
            "latest_gate_status": {"status": "pass"},
        }

    async def get_experiment_run(self, **kwargs: Any) -> dict[str, Any] | None:
        self.calls.append(("get_experiment_run", kwargs))
        if kwargs.get("run_id") == self.baseline_run["run_id"]:
            return self.baseline_run
        if kwargs.get("run_id") == self.candidate_run["run_id"]:
            return self.candidate_run
        return {
            "run_id": "ffffffff-ffff-4fff-8fff-ffffffffffff",
            "experiment_id": self.experiment["experiment_id"],
            "tenant_id": "tenant-a",
            "evaluator_id": self.evaluator["evaluator_id"],
            "dataset_id": self.dataset["dataset_id"],
            "trace_id": "11111111-1111-4111-8111-111111111111",
            "status": "succeeded",
            "score_summary": {"average_score": 0.9},
            "metrics": {"targets": 1},
            "error_message": None,
            "created_by": "user-a",
            "started_at": datetime.now(timezone.utc),
            "finished_at": datetime.now(timezone.utc),
            "created_at": datetime.now(timezone.utc),
            "updated_at": datetime.now(timezone.utc),
        }

    async def promote_experiment_baseline(self, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(("promote_baseline", kwargs))
        return {
            "experiment_id": kwargs["experiment_id"],
            "baseline_run_id": kwargs["run_id"],
            "previous_baseline_run_id": self.experiment.get("baseline_run_id"),
            "promoted_by": kwargs["promoted_by"],
            "promoted_at": datetime.now(timezone.utc),
        }

    async def list_experiment_run_case_results(
        self, **kwargs: Any
    ) -> tuple[list[dict[str, Any]], int]:
        self.calls.append(("run_results", kwargs))
        return (
            [
                {
                    "example_id": self.example["example_id"],
                    "case_id": "assistant.case.one",
                    "candidate_trace_id": "11111111-1111-4111-8111-111111111111",
                    "source_trace_id": self.example["source_trace_id"],
                    "status": "failed",
                    "aggregate_score": 0.4,
                    "failure_reason": "answer was not grounded",
                    "input": self.example["input"],
                    "expected_output": self.example["expected_output"],
                    "trace": {
                        "status": "succeeded",
                        "model_id": "qwen3.7-plus",
                        "total_latency_ms": 980,
                        "output_preview": "candidate answer",
                    },
                    "scores": [
                        {
                            "score_name": "quality",
                            "numeric_value": 0.4,
                            "label": "fail",
                            "explanation": "answer was not grounded",
                            "failure_kind": None,
                        }
                    ],
                }
            ],
            1,
        )

    async def compare_experiment_runs(self, **kwargs: Any) -> dict[str, Any] | None:
        self.calls.append(("compare_runs", kwargs))
        return {
            "baseline_run_id": kwargs["baseline_run_id"],
            "candidate_run_id": kwargs["candidate_run_id"],
            "baseline_summary": self.baseline_run["score_summary"],
            "candidate_summary": self.candidate_run["score_summary"],
            "deltas": {"overall_score": -0.02, "trajectory_pass_rate": -0.04},
            "regression_summary": {"regressed_metrics": ["overall_score", "trajectory_pass_rate"]},
            "compatibility": {"status": "compatible", "compatible": True, "reasons": []},
            "gate": {"status": "pass", "failures": [], "warnings": []},
            "case_diffs": [],
        }


class ExampleFromTraceRepository(AgentTraceRepository):
    def __init__(self) -> None:
        super().__init__(SimpleNamespace(_pool=None, enabled=False))
        self.insert_args: tuple[Any, ...] | None = None

    async def get_trace_detail(self, **_kwargs: Any) -> dict[str, Any] | None:
        return {
            "trace": _trace_row(
                metadata={
                    "runtime_trajectory": {
                        "context_snapshot_id": "ctx_runtime",
                        "trace_writer_health": {"issue_count": 0},
                    }
                },
                redaction_state={"payloads": "redacted_truncated"},
            ),
            "spans": [{"span_kind": "lifecycle"}, {"span_kind": "tool_execution"}],
            "events": [],
            "scores": [],
        }

    async def fetchrow(self, _query: str, *args: Any) -> dict[str, Any] | None:
        if _query.lstrip().startswith("SELECT * FROM eval_examples"):
            return None
        self.insert_args = args
        return {
            "example_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "dataset_id": args[0],
            "tenant_id": args[1],
            "split": args[2],
            "input": args[3],
            "expected_output": args[4],
            "metadata": args[5],
            "source_trace_id": args[6],
            "source_span_id": args[7],
            "created_by": args[8],
            "created_at": datetime.now(timezone.utc),
        }


class RunResultsRepository(AgentTraceRepository):
    def __init__(self) -> None:
        super().__init__(SimpleNamespace(_pool=None, enabled=False))
        self.queries: list[str] = []

    async def fetchrow(self, query: str, *_args: Any) -> dict[str, Any] | None:
        self.queries.append(" ".join(query.split()))
        return {"total": 1}

    async def fetch(self, query: str, *_args: Any) -> list[dict[str, Any]]:
        self.queries.append(" ".join(query.split()))
        common = {
            "case_key": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "candidate_trace_id": "11111111-1111-4111-8111-111111111111",
            "example_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
            "source_trace_id": "99999999-9999-4999-8999-999999999999",
            "score_metadata": {
                "example_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                "case_id": "assistant.case.one",
            },
            "example_metadata": {"case_id": "assistant.case.one"},
            "input": {"input_preview": "hello"},
            "expected_output": {"output_preview": "hi"},
            "trace_family": "assistant",
            "trace_status": "succeeded",
            "model_id": "qwen3.7-plus",
            "provider": "dashscope",
            "total_latency_ms": 980,
            "total_tokens": 30,
            "output_preview": "candidate",
            "score_source": "rule",
        }
        return [
            {
                **common,
                "score_id": "22222222-2222-4222-8222-222222222222",
                "score_name": "quality",
                "numeric_value": 0.4,
                "score_label": "fail",
                "score_explanation": "not grounded",
            },
            {
                **common,
                "score_id": "33333333-3333-4333-8333-333333333333",
                "score_name": "safety",
                "numeric_value": 1.0,
                "score_label": "pass",
                "score_explanation": "safe",
            },
        ]


@pytest.mark.asyncio
async def test_repository_groups_experiment_scores_into_case_results() -> None:
    repo = RunResultsRepository()

    cases, total = await repo.list_experiment_run_case_results(
        tenant_id="tenant-a",
        run_id="ffffffff-ffff-4fff-8fff-ffffffffffff",
        limit=50,
        offset=0,
    )

    assert total == 1
    assert cases[0]["case_id"] == "assistant.case.one"
    assert cases[0]["status"] == "failed"
    assert cases[0]["aggregate_score"] == 0.7
    assert len(cases[0]["scores"]) == 2
    score_queries = [query for query in repo.queries if "agent_trace_scores" in query]
    assert all("experiment_run_id" in query for query in score_queries)
    assert "ROW_NUMBER() OVER" in score_queries[-1]
    assert (
        "COALESCE(NULLIF(s.target_id, ''), s.span_id::text, s.trace_id::text)" in score_queries[-1]
    )


@pytest.mark.asyncio
async def test_eval_list_rejects_legacy_usage_permission_only(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    with pytest.raises(HTTPException) as exc:
        await list_eval_traces(
            request=_request(),
            auth=_auth(permissions=["console:usage:view"]),
        )

    assert exc.value.status_code == 403
    assert repo.calls == []


@pytest.mark.asyncio
async def test_eval_trace_list_supports_score_and_dataset_filters(monkeypatch):
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    await list_eval_traces(
        request=_request(),
        score_name="quality",
        score_label="pass",
        min_score=0.8,
        max_score=1.0,
        dataset_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        span_kind="retriever",
        auth=_auth(user_id="admin", tenant_id="tenant-a", roles=["admin"]),
    )

    call_name, kwargs = repo.calls[-1]
    assert call_name == "list"
    assert kwargs["score_name"] == "quality"
    assert kwargs["score_label"] == "pass"
    assert kwargs["min_score"] == 0.8
    assert kwargs["max_score"] == 1.0
    assert kwargs["dataset_id"] == "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    assert kwargs["span_kind"] == "retriever"


@pytest.mark.asyncio
async def test_get_eval_summary_scopes_tenant_and_user(monkeypatch):
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    result = await get_eval_summary(
        request=_request(),
        days=14,
        user_id="user-b",
        auth=_auth(user_id="user-a", tenant_id="tenant-a"),
    )

    assert result.total_traces == 12
    assert result.langgraph_traces == 2
    assert result.window_days == 14
    call_name, kwargs = repo.calls[-1]
    assert call_name == "summary"
    assert kwargs["tenant_id"] == "tenant-a"
    assert kwargs["user_id"] == "user-a"
    assert kwargs["days"] == 14


@pytest.mark.asyncio
async def test_kb_ragas_summary_endpoint_scopes_tenant_and_dataset(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    result = await get_kb_ragas_summary_endpoint(
        request=_request(),
        days=14,
        dataset_id="dataset-a",
        auth=_auth(user_id="user-a", tenant_id="tenant-a"),
    )

    assert result.window_days == 14
    assert result.dataset_id == "dataset-a"
    assert result.rag_traces == 2
    assert result.ragas_scored_traces == 1
    assert result.metrics[0].metric == "context_relevancy"
    call_name, kwargs = repo.calls[-1]
    assert call_name == "kb_ragas_summary"
    assert kwargs["tenant_id"] == "tenant-a"
    assert kwargs["dataset_id"] == "dataset-a"


@pytest.mark.asyncio
async def test_kb_ragas_batch_score_endpoint_queues_rag_traces(monkeypatch) -> None:
    repo = FakeTraceRepository()
    repo.evaluator = {
        **repo.evaluator,
        "name": "kb-ragas",
        "evaluator_type": "ragas",
    }
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    result = await batch_score_kb_ragas_dataset(
        dataset_id="dataset-a",
        body=KbRagasBatchScoreRequest(
            evaluator_id=repo.evaluator["evaluator_id"],
            limit=10,
            only_unscored=True,
        ),
        request=_request(),
        auth=_auth(
            user_id="user-a",
            tenant_id="tenant-a",
            permissions=["console:eval:view", "console:eval:run"],
        ),
    )

    assert result.queued == 1
    assert result.skipped == 0
    assert result.jobs[0].status == "queued"
    list_call = next(call for call in repo.calls if call[0] == "list")
    assert list_call[1]["trace_family"] == "rag"
    assert list_call[1]["metadata_dataset_id"] == "dataset-a"
    enqueue_call = next(call for call in repo.calls if call[0] == "evaluator_run")
    assert enqueue_call[1]["payload"]["metadata"]["kb_ragas_batch"] is True


@pytest.mark.asyncio
async def test_kb_ragas_score_retrieval_endpoint_maps_service_payload(monkeypatch) -> None:
    async def _score_retrieval_with_kb_ragas(**kwargs: Any) -> dict[str, Any]:
        assert kwargs["query"] == "refund policy"
        assert kwargs["answer"] == "Refunds are available for 30 days."
        assert kwargs["contexts"] == ["Refunds are allowed within 30 days."]
        return {
            "judge_model": "qwen-test",
            "results": [
                {
                    "metric": "context_relevancy",
                    "score": 0.91,
                    "explanation": "Relevant.",
                    "label": "pass",
                }
            ],
        }

    monkeypatch.setattr(
        eval_routes, "score_retrieval_with_kb_ragas", _score_retrieval_with_kb_ragas
    )

    result = await score_kb_ragas_retrieval(
        body=KbRagasScoreRetrievalRequest(
            query="refund policy",
            answer="Refunds are available for 30 days.",
            contexts=["Refunds are allowed within 30 days."],
            metrics=["context_relevancy"],
        ),
        request=_request(),
        auth=_auth(
            user_id="user-a",
            tenant_id="tenant-a",
            permissions=["console:eval:view", "console:eval:run"],
        ),
    )

    assert result.judge_model == "qwen-test"
    assert result.results[0].metric == "context_relevancy"
    assert result.results[0].score == 0.91


@pytest.mark.asyncio
async def test_list_eval_traces_uses_server_tenant_and_non_admin_user_scope(monkeypatch):
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    result = await list_eval_traces(
        request=_request(),
        user_id="user-b",
        auth=_auth(user_id="user-a", tenant_id="tenant-a"),
    )

    assert result.total == 1
    assert result.traces[0].tenant_id == "tenant-a"
    call_name, kwargs = repo.calls[-1]
    assert call_name == "list"
    assert kwargs["tenant_id"] == "tenant-a"
    assert kwargs["user_id"] == "user-a"


@pytest.mark.asyncio
async def test_admin_eval_trace_list_can_filter_user_within_server_tenant(monkeypatch):
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    await list_eval_traces(
        request=_request(),
        user_id="user-b",
        auth=_auth(user_id="admin", tenant_id="tenant-a", roles=["admin"]),
    )

    call_name, kwargs = repo.calls[-1]
    assert call_name == "list"
    assert kwargs["tenant_id"] == "tenant-a"
    assert kwargs["user_id"] == "user-b"


@pytest.mark.asyncio
async def test_eval_trace_list_accepts_transcript_locator_filters(monkeypatch):
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    await list_eval_traces(
        request=_request(),
        session_id="session-a",
        request_id="request-c",
        transcript_query="  refund transcript turn  ",
        turn_index=3,
        auth=_auth(user_id="admin", tenant_id="tenant-a", roles=["admin"]),
    )

    call_name, kwargs = repo.calls[-1]
    assert call_name == "list"
    assert kwargs["tenant_id"] == "tenant-a"
    assert kwargs["session_id"] == "session-a"
    assert kwargs["request_id"] == "request-c"
    assert kwargs["transcript_query"] == "refund transcript turn"
    assert kwargs["turn_index"] == 3


@pytest.mark.asyncio
async def test_non_operator_eval_trace_list_rejects_missing_authenticated_user(monkeypatch):
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    with pytest.raises(HTTPException) as exc:
        await list_eval_traces(
            request=_request(),
            user_id="user-b",
            auth=_auth(user_id="", tenant_id="tenant-a"),
        )

    assert exc.value.status_code == 403
    assert repo.calls == []


@pytest.mark.asyncio
async def test_eval_trace_detail_missing_cross_scope_trace_returns_404(monkeypatch):
    repo = FakeTraceRepository()
    repo.detail = None
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    with pytest.raises(HTTPException) as exc:
        await get_eval_trace_detail(
            trace_id="other-tenant-trace",
            request=_request(),
            auth=_auth(user_id="user-a", tenant_id="tenant-a"),
        )

    assert exc.value.status_code == 404
    call_name, kwargs = repo.calls[-1]
    assert call_name == "detail"
    assert kwargs["tenant_id"] == "tenant-a"
    assert kwargs["user_id"] == "user-a"


@pytest.mark.asyncio
async def test_create_eval_trace_score_uses_authenticated_evaluator(monkeypatch):
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    body = AgentTraceScoreCreate(
        score_name="quality",
        numeric_value=0.9,
        label="good",
        explanation="grounded answer",
    )
    result = await create_eval_trace_score(
        trace_id="11111111-1111-4111-8111-111111111111",
        body=body,
        request=_request(),
        auth=_auth(
            user_id="user-a",
            tenant_id="tenant-a",
            permissions=["console:eval:view", "console:eval:run"],
        ),
    )

    assert result.created_by == "user-a"
    call_name, kwargs = repo.calls[-1]
    assert call_name == "score"
    assert kwargs["tenant_id"] == "tenant-a"
    assert kwargs["user_id"] == "user-a"
    assert kwargs["created_by"] == "user-a"
    assert "tenant_id" not in kwargs["payload"]


@pytest.mark.asyncio
async def test_eval_api_allows_future_trace_families_and_rejects_unknown(monkeypatch):
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    await list_eval_traces(
        request=_request(),
        trace_family="rag",
        auth=_auth(user_id="user-a", tenant_id="tenant-a"),
    )
    assert repo.calls[-1][1]["trace_family"] == "rag"

    with pytest.raises(HTTPException) as exc:
        await list_eval_traces(
            request=_request(),
            trace_family="unknown",
            auth=_auth(user_id="user-a", tenant_id="tenant-a"),
        )

    assert exc.value.status_code == 400


@pytest.mark.asyncio
async def test_eval_trace_thread_uses_scoped_user(monkeypatch):
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    result = await get_eval_trace_thread(
        thread_id="session-a",
        request=_request(),
        auth=_auth(user_id="user-a", tenant_id="tenant-a"),
    )

    assert result.total == 1
    call_name, kwargs = repo.calls[-1]
    assert call_name == "thread"
    assert kwargs["tenant_id"] == "tenant-a"
    assert kwargs["thread_id"] == "session-a"
    assert kwargs["user_id"] == "user-a"


@pytest.mark.asyncio
async def test_eval_trace_export_returns_semantic_payload(monkeypatch):
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    result = await export_eval_trace(
        trace_id="11111111-1111-4111-8111-111111111111",
        request=_request(),
        format="otel",
        auth=_auth(user_id="user-a", tenant_id="tenant-a"),
    )

    assert result.format == "otel"
    assert result.redaction_policy
    assert isinstance(result.payload, dict)
    assert result.payload["trace"]["attributes"]["gen_ai.request.model"] == "qwen3.6-plus"
    assert result.payload["trace"]["attributes"]["gen_ai.usage.total_tokens"] == 30


@pytest.mark.asyncio
async def test_eval_trace_export_defensively_redacts_sensitive_payload(monkeypatch) -> None:
    repo = FakeTraceRepository()
    repo.detail = {
        "trace": _trace_row(
            input_preview="hello Authorization: Bearer raw-trace-token",
            output_preview="safe",
            metadata={
                "headers": {
                    "Authorization": "Bearer raw-header-token",
                    "Cookie": "sid=raw-cookie",
                },
                "callback_url": "https://user:raw-password@example.test/path",
            },
        ),
        "spans": [
            {
                "span_id": "33333333-3333-4333-8333-333333333333",
                "trace_id": "11111111-1111-4111-8111-111111111111",
                "parent_span_id": None,
                "span_kind": "tool_execution",
                "name": "tool",
                "status": "succeeded",
                "started_at": datetime.now(timezone.utc),
                "ended_at": datetime.now(timezone.utc),
                "duration_ms": 10,
                "attributes": {
                    "api_key": "raw-api-key",
                    "note": "access_token=raw-access-token",
                },
            }
        ],
        "events": [
            {
                "event_type": "tool_call",
                "occurred_at": datetime.now(timezone.utc),
                "payload": {
                    "headers": {"Authorization": "Bearer raw-event-token"},
                    "cookie": "raw-event-cookie",
                    "body": "password=raw-password",
                },
            }
        ],
        "scores": [],
    }
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    result = await export_eval_trace(
        trace_id="11111111-1111-4111-8111-111111111111",
        request=_request(),
        format="otel",
        auth=_auth(user_id="user-a", tenant_id="tenant-a"),
    )

    serialized = json.dumps(result.payload, default=str)
    assert "raw-trace-token" not in serialized
    assert "raw-header-token" not in serialized
    assert "raw-event-token" not in serialized
    assert "raw-cookie" not in serialized
    assert "raw-event-cookie" not in serialized
    assert "raw-api-key" not in serialized
    assert "raw-access-token" not in serialized
    assert "raw-password" not in serialized
    assert "[redacted]" in serialized


@pytest.mark.asyncio
async def test_eval_dataset_evaluator_experiment_workflow(monkeypatch):
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    request = _request()
    auth = _auth(
        user_id="user-a",
        tenant_id="tenant-a",
        permissions=["console:eval:view", "console:eval:run"],
    )

    dataset = await create_eval_dataset(
        body=EvalDatasetCreate(name="assistant-regression", description="regression set"),
        request=request,
        auth=auth,
    )
    example = await create_eval_example_from_trace(
        dataset_id=dataset.dataset_id,
        body=EvalExampleFromTraceCreate(source_trace_id="11111111-1111-4111-8111-111111111111"),
        request=request,
        auth=auth,
    )
    evaluator = await create_eval_evaluator(
        body=EvalEvaluatorCreate(name="quality", rubric="score quality"),
        request=request,
        auth=auth,
    )
    experiment = await create_eval_experiment(
        body=EvalExperimentCreate(name="baseline", dataset_id=dataset.dataset_id),
        request=request,
        auth=auth,
    )
    job = await run_eval_evaluator_async(
        evaluator_id=evaluator.evaluator_id,
        body=EvalEvaluatorRunRequest(
            experiment_id=experiment.experiment_id, dataset_id=dataset.dataset_id
        ),
        request=request,
        auth=auth,
    )

    assert example.source_trace_id == "11111111-1111-4111-8111-111111111111"
    assert evaluator.evaluator_type == "human"
    assert experiment.dataset_id == dataset.dataset_id
    assert job.status == "queued"
    assert [call[0] for call in repo.calls[-6:]] == [
        "dataset",
        "get_dataset",
        "example",
        "evaluator",
        "experiment",
        "evaluator_run",
    ]


@pytest.mark.asyncio
async def test_eval_example_from_trace_preserves_requested_trace_family(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    await create_eval_example_from_trace(
        dataset_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        body=EvalExampleFromTraceCreate(
            source_trace_id="11111111-1111-4111-8111-111111111111",
            trace_family="rag",
        ),
        request=_request(),
        auth=_auth(
            user_id="user-a",
            tenant_id="tenant-a",
            permissions=["console:eval:view", "console:eval:run"],
        ),
    )

    assert repo.calls[-1][0] == "example"
    assert repo.calls[-1][1]["trace_family"] == "rag"


@pytest.mark.asyncio
async def test_repository_example_from_trace_records_runtime_trajectory_metadata() -> None:
    repo = ExampleFromTraceRepository()

    example = await repo.create_example_from_trace(
        tenant_id="tenant-a",
        dataset_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        created_by="user-a",
        user_id="user-a",
        trace_family="assistant",
        payload={
            "source_trace_id": "11111111-1111-4111-8111-111111111111",
            "metadata": {"review_status": "pending"},
        },
    )

    assert example is not None
    assert repo.insert_args is not None
    metadata = example["metadata"]
    assert metadata["review_status"] == "pending"
    assert metadata["behavior_confirmed"] is False
    assert example["input"]["message"] == example["input"]["input_preview"]
    assert metadata["expected_trajectory"]["required_span_kinds"] == [
        "lifecycle",
        "tool_execution",
    ]
    runtime = metadata["expected_trajectory"]["runtime"]
    assert runtime["expected_exit_reason"] == "succeeded"
    assert metadata["assertions"] == [{"type": "no_sensitive_output"}]


@pytest.mark.asyncio
async def test_eval_trace_feedback_preview_builds_redacted_case_and_proposal(monkeypatch) -> None:
    repo = FakeTraceRepository()
    repo.detail = {
        "trace": _trace_row(
            status="failed",
            input_preview="Authorization: Bearer raw-token user asks",
            output_preview="api_key=secret-value failed",
            metadata={"raw_input": "do not copy", "note": "token=secret-value"},
        ),
        "events": [{"event_type": "tool_error", "payload": {"tool_name": "search"}}],
        "spans": [{"span_kind": "tool", "name": "search", "status": "failed"}],
        "scores": [],
    }
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    result = await preview_eval_trace_feedback(
        body=EvalTraceFeedbackRequest(
            trace_ids=["11111111-1111-4111-8111-111111111111"],
            dataset_id=repo.dataset["dataset_id"],
        ),
        request=_request(),
        auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
    )

    serialized = json.dumps(result.model_dump(), default=str)
    assert result.patterns[0].failure_mode == "tool_error"
    assert result.clusters[0]["count"] == 1
    assert result.dataset_cases[0].metadata["tenant_id"] == "tenant-a"
    assert result.dataset_cases[0].source_trace_id == "11111111-1111-4111-8111-111111111111"
    assert result.dataset_cases[0].expected_trajectory["replay"]["trace_family"] == "assistant"
    assert result.import_request is not None
    assert result.proposals[0]["status"] == "proposed"
    assert result.proposals[0]["auto_apply"] is False
    assert "raw-token" not in serialized
    assert "secret-value" not in serialized
    assert repo.calls[-1][0] == "detail"
    assert repo.calls[-1][1]["tenant_id"] == "tenant-a"
    assert repo.calls[-1][1]["user_id"] == "user-a"


@pytest.mark.asyncio
async def test_eval_list_get_endpoints_use_trace_read_capability(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    request = _request()
    auth = _auth(user_id="user-a", tenant_id="tenant-a", permissions=["console:eval:view"])

    datasets = await list_eval_datasets(request=request, auth=auth)
    examples = await list_eval_examples(
        dataset_id=datasets.datasets[0].dataset_id,
        request=request,
        auth=auth,
    )
    evaluators = await list_eval_evaluators(request=request, auth=auth)
    experiments = await list_eval_experiments(request=request, auth=auth)
    dataset = await get_eval_dataset(
        dataset_id=datasets.datasets[0].dataset_id,
        request=request,
        auth=auth,
    )
    evaluator = await get_eval_evaluator(
        evaluator_id=evaluators.evaluators[0].evaluator_id,
        request=request,
        auth=auth,
    )
    experiment = await get_eval_experiment(
        experiment_id=experiments.experiments[0].experiment_id,
        request=request,
        auth=auth,
    )
    run = await get_eval_experiment_run(
        run_id="ffffffff-ffff-4fff-8fff-ffffffffffff",
        request=request,
        auth=auth,
    )

    assert datasets.total == 1
    assert examples.total == 1
    assert evaluators.total == 1
    assert experiments.total == 1
    assert dataset.dataset_id == datasets.datasets[0].dataset_id
    assert evaluator.evaluator_id == evaluators.evaluators[0].evaluator_id
    assert experiment.experiment_id == experiments.experiments[0].experiment_id
    assert run.status == "succeeded"


@pytest.mark.asyncio
async def test_eval_experiment_run_results_return_case_scores(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    result = await get_eval_experiment_run_results(
        run_id="ffffffff-ffff-4fff-8fff-ffffffffffff",
        request=_request(),
        limit=25,
        offset=0,
        auth=_auth(permissions=["console:eval:view"]),
    )

    assert result.run.status == "succeeded"
    assert result.total == 1
    assert result.cases[0]["status"] == "failed"
    assert result.cases[0]["scores"][0]["numeric_value"] == 0.4
    assert repo.calls[-1] == (
        "run_results",
        {
            "tenant_id": "tenant-a",
            "run_id": "ffffffff-ffff-4fff-8fff-ffffffffffff",
            "limit": 25,
            "offset": 0,
        },
    )


@pytest.mark.asyncio
async def test_legacy_rescore_manifest_is_hidden_on_run_and_experiment_get(monkeypatch) -> None:
    fixture = FakeTraceRepository()
    private_case = {"input": {"message": "revoked KB private text"}}
    legacy_run = {
        **fixture.baseline_run,
        "target_snapshot": {
            "dataset_version": "v1",
            "dataset_manifest": [private_case],
        },
    }

    class LegacyRepository(AgentTraceRepository):
        def __init__(self) -> None:
            super().__init__(SimpleNamespace(_pool=None, enabled=False))

        async def fetchrow(self, query: str, *_args: Any) -> dict[str, Any] | None:
            return fixture.experiment if "FROM eval_experiments" in query else legacy_run

        async def fetch(self, _query: str, *_args: Any) -> list[dict[str, Any]]:
            return [legacy_run]

    repo = LegacyRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    auth = _auth(permissions=["console:eval:view"])
    run = await get_eval_experiment_run(
        run_id=legacy_run["run_id"], request=_request(), auth=auth,
    )
    experiment = await get_eval_experiment(
        experiment_id=fixture.experiment["experiment_id"], request=_request(), auth=auth,
    )

    assert run.target_snapshot == {"dataset_version": "v1"}
    assert experiment.runs[0].target_snapshot == {"dataset_version": "v1"}
    assert "revoked KB private text" not in run.model_dump_json()
    assert "revoked KB private text" not in experiment.model_dump_json()


@pytest.mark.asyncio
async def test_eval_run_endpoint_requires_eval_run_permission(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    request = _request()

    with pytest.raises(HTTPException) as exc_info:
        await run_eval_evaluator_async(
            evaluator_id="cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            body=EvalEvaluatorRunRequest(trace_id="11111111-1111-4111-8111-111111111111"),
            request=request,
            auth=_auth(permissions=["console:eval:view"]),
        )

    assert exc_info.value.status_code == 403
    detail = exc_info.value.detail
    assert detail["required_capability"] == Capability.GATEWAY_EVAL_RUN.value


@pytest.mark.asyncio
async def test_direct_evaluator_run_reports_missing_dataset_as_422(monkeypatch) -> None:
    class MissingDatasetRepository:
        async def enqueue_evaluator_run(self, **_kwargs: Any) -> dict[str, Any]:
            raise ValueError("eval_dataset_not_found")

    monkeypatch.setattr(
        eval_routes, "_get_trace_repository", lambda _request: MissingDatasetRepository(),
    )
    with pytest.raises(HTTPException) as error:
        await run_eval_evaluator_async(
            evaluator_id="cccccccc-cccc-4ccc-8ccc-cccccccccccc",
            body=EvalEvaluatorRunRequest(dataset_id="ffffffff-ffff-4fff-8fff-ffffffffffff"),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )

    assert error.value.status_code == 422
    assert error.value.detail == "eval_dataset_not_found"


@pytest.mark.asyncio
async def test_eval_dashboard_returns_platform_health(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    result = await get_eval_dashboard(
        request=_request(),
        days=14,
        auth=_auth(user_id="admin", tenant_id="tenant-a", roles=["admin"]),
    )

    assert result.metrics["example_count"] == 10
    assert result.queue_health["failed_jobs"] == 1
    assert result.runtime_health["assistant_status"] == "enabled"
    assert result.runtime_health["tool_safety_failures"] == 1
    assert result.latest_gate_status["status"] == "pass"
    assert repo.calls[-1] == ("dashboard", {"tenant_id": "tenant-a", "days": 14})


@pytest.mark.asyncio
async def test_eval_example_review_import_and_export(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    auth = _auth(permissions=["console:eval:view", "console:eval:run"])

    updated = await update_eval_example(
        dataset_id=repo.dataset["dataset_id"],
        example_id=repo.example["example_id"],
        body=EvalExampleUpdate(
            expected_trajectory={"required_span_kinds": ["model_invocation"]},
            assertions=[{"type": "output_contains", "value": "hi"}],
            review_status="approved",
            tags=["golden"],
        ),
        request=_request(),
        auth=auth,
    )
    assert updated.metadata["review_status"] == "approved"
    assert updated.metadata["expected_trajectory"]["required_span_kinds"] == ["model_invocation"]

    imported = await import_eval_examples(
        dataset_id=repo.dataset["dataset_id"],
        body=EvalExamplesImportRequest(
            examples=[
                {
                    "case_id": "assistant.case.one",
                    "input": {"input_preview": "hello"},
                    "expected_output": {"contains": "hi"},
                    "expected_trajectory": {"required_span_kinds": ["model_invocation"]},
                    "assertions": [{"type": "output_contains", "value": "hi"}],
                    "metadata": {"review_status": "approved"},
                }
            ]
        ),
        request=_request(),
        auth=auth,
    )
    assert imported.imported == 1
    assert imported.examples[0].metadata["case_id"] == "assistant.case.one"

    exported = await export_eval_examples(
        dataset_id=repo.dataset["dataset_id"],
        request=_request(),
        auth=_auth(),
    )
    assert exported.dataset.dataset_id == repo.dataset["dataset_id"]
    assert exported.examples[0].case_id in {"assistant.case.one", repo.example["example_id"]}


@pytest.mark.parametrize(
    ("patch", "expected_error"),
    [
        ({"expected_trajectory": {"required_span_kinds": "invalid"}}, "required_span_kinds"),
        ({"assertions": [{"type": "output_contains"}]}, "assertions[1].value"),
    ],
)
@pytest.mark.asyncio
async def test_eval_patch_validates_persisted_behavior_fields(
    monkeypatch, patch: dict[str, Any], expected_error: str,
) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    with pytest.raises(HTTPException) as error:
        await update_eval_example(
            dataset_id=repo.dataset["dataset_id"], example_id=repo.example["example_id"],
            body=EvalExampleUpdate(**patch), request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )

    assert error.value.status_code == 422
    assert expected_error in str(error.value.detail)
    assert not any(call[0] == "update_example" for call in repo.calls)


@pytest.mark.asyncio
async def test_kb_failure_save_checks_dataset_and_exact_source_identity(monkeypatch) -> None:
    repo = FakeTraceRepository()
    repo.dataset["metadata"] = {"kb_dataset_id": "kb-1"}
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    async def visible(_request, _user):
        return {"kb-1": "Knowledge base"}

    async def authorized(_request, _user, refs):
        return set(refs)

    monkeypatch.setattr(eval_dataset_routes_module, "visible_dataset_names", visible)
    monkeypatch.setattr(eval_dataset_routes_module, "visible_source_version_keys", authorized)
    auth = _auth(permissions=["console:eval:view", "console:eval:run"])
    user = UserContext(user_id="user-a", tenant_id="tenant-a", roles=["user"], is_authenticated=True)
    body = EvalKbFailureSave(
        case_id="kb-failure-1", kb_dataset_id="kb-1", query="What changed?",
        expected_answer="The revised policy", source="kb-hit-test",
        observed_segment_ids=["seg-1"],
        source_versions=[{
            "kb_dataset_id": "kb-1", "document_id": "doc-1", "segment_id": "seg-1",
            "source_version": 2, "source_hash": "a" * 64,
        }],
        expected_revision=0,
    )
    response = await save_kb_failure_example(
        dataset_id=repo.dataset["dataset_id"], body=body, request=_request(), auth=auth, user=user,
    )
    assert response.created is True
    assert response.revision == 1
    assert response.example.metadata["kb_source_versions_verified"] is True
    latest = await get_latest_kb_failure_example(
        dataset_id=repo.dataset["dataset_id"], case_id=body.case_id,
        request=_request(), auth=auth, user=user,
    )
    assert latest.example_id == response.example.example_id

    with pytest.raises(HTTPException) as error:
        await save_kb_failure_example(
            dataset_id=repo.dataset["dataset_id"],
            body=body.model_copy(update={"source_versions": []}),
            request=_request(), auth=auth, user=user,
        )
    assert error.value.status_code == 422

    async def invisible(_request, _user):
        return {}

    monkeypatch.setattr(eval_dataset_routes_module, "visible_dataset_names", invisible)
    with pytest.raises(HTTPException) as error:
        await save_kb_failure_example(
            dataset_id=repo.dataset["dataset_id"],
            body=body.model_copy(update={"source_versions": [], "observed_segment_ids": []}),
            request=_request(), auth=auth, user=user,
        )
    assert error.value.status_code == 403

    monkeypatch.setattr(eval_dataset_routes_module, "visible_dataset_names", visible)

    async def deny_source(_request, _user, _refs):
        return set()

    monkeypatch.setattr(eval_dataset_routes_module, "visible_source_version_keys", deny_source)
    with pytest.raises(HTTPException) as error:
        await save_kb_failure_example(
            dataset_id=repo.dataset["dataset_id"], body=body,
            request=_request(), auth=auth, user=user,
        )
    assert error.value.status_code == 422


def test_kb_failure_source_version_rejects_boolean() -> None:
    with pytest.raises(ValidationError):
        EvalKbFailureSave(
            case_id="kb-failure-bool", kb_dataset_id="kb-1", query="q",
            expected_answer="a", source="kb-hit-test", expected_revision=0,
            source_versions=[{
                "kb_dataset_id": "kb-1", "document_id": "doc-1", "segment_id": "seg-1",
                "source_version": True, "source_hash": "a" * 64,
            }],
        )
    with pytest.raises(ValidationError):
        EvalKbFailureSave(
            case_id="kb-failure-bool", kb_dataset_id="kb-1", query="q",
            expected_answer="a", source="kb-hit-test", expected_revision=True,
        )


@pytest.mark.asyncio
async def test_generic_eval_import_cannot_forge_kb_failure_provenance(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    with pytest.raises(HTTPException) as error:
        await import_eval_examples(
            dataset_id=repo.dataset["dataset_id"],
            body=EvalExamplesImportRequest(examples=[{
                "case_id": "forged", "input": {"query": "q"},
                "expected_output": {"answer": "a"},
                "metadata": {"source_kind": "kb_failure", "kb_source_versions_verified": True},
            }]),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )
    assert error.value.status_code == 422
    assert not any(call[0] == "import_examples" for call in repo.calls)
    with pytest.raises(HTTPException) as error:
        await create_eval_example_from_trace(
            dataset_id=repo.dataset["dataset_id"],
            body=EvalExampleFromTraceCreate(
                source_trace_id="11111111-1111-4111-8111-111111111111",
                metadata={"source_kind": "kb_failure"},
            ),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )
    assert error.value.status_code == 422
    with pytest.raises(HTTPException) as error:
        await update_eval_example(
            dataset_id=repo.dataset["dataset_id"], example_id=repo.example["example_id"],
            body=EvalExampleUpdate(metadata={"case_revision": 999}),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )
    assert error.value.status_code == 422


@pytest.mark.asyncio
async def test_kb_failure_eval_reads_recheck_current_dataset_and_source_acl(monkeypatch) -> None:
    repo = FakeTraceRepository()
    repo.dataset["metadata"] = {"kb_dataset_id": "kb-1"}
    repo.example = {
        **repo.example,
        "metadata": {
            "case_id": "kb-case", "source_kind": "kb_failure", "kb_dataset_id": "kb-1",
            "kb_observed_segment_ids": ["seg-1"],
            "kb_source_versions": [{
                "kb_dataset_id": "kb-1", "document_id": "doc-1", "segment_id": "seg-1",
                "source_version": 2, "source_hash": "a" * 64,
            }],
        },
    }
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    auth = _auth(permissions=["console:eval:view", "console:eval:run"])
    user = UserContext(user_id="user-a", tenant_id="tenant-a", roles=["user"], is_authenticated=True)

    async def visible(_request, _user):
        return {"kb-1": "Knowledge base"}

    async def authorized(_request, _user, refs):
        return set(refs)

    monkeypatch.setattr(eval_dataset_routes_module, "visible_dataset_names", visible)
    monkeypatch.setattr(eval_dataset_routes_module, "visible_source_version_keys", authorized)
    listed = await list_eval_examples(
        dataset_id=repo.dataset["dataset_id"], request=_request(), auth=auth, user=user,
    )
    assert listed.examples[0].metadata["case_id"] == "kb-case"
    with pytest.raises(HTTPException) as error:
        await update_eval_example(
            dataset_id=repo.dataset["dataset_id"], example_id=repo.example["example_id"],
            body=EvalExampleUpdate(expected_output={"answer": "mutated in place"}),
            request=_request(), auth=auth, user=user,
        )
    assert error.value.status_code == 409
    with pytest.raises(HTTPException) as error:
        await delete_eval_example(
            dataset_id=repo.dataset["dataset_id"], example_id=repo.example["example_id"],
            request=_request(), auth=auth, user=user,
        )
    assert error.value.status_code == 409
    original_sources = repo.example["metadata"]["kb_source_versions"]
    repo.example["metadata"]["kb_source_versions"] = []
    with pytest.raises(HTTPException) as error:
        await list_eval_examples(
            dataset_id=repo.dataset["dataset_id"], request=_request(), auth=auth, user=user,
        )
    assert error.value.status_code == 403
    repo.example["metadata"]["kb_source_versions"] = original_sources

    async def revoked(_request, _user, _refs):
        return set()

    monkeypatch.setattr(eval_dataset_routes_module, "visible_source_version_keys", revoked)
    with pytest.raises(HTTPException) as error:
        await list_eval_examples(
            dataset_id=repo.dataset["dataset_id"], request=_request(), auth=auth, user=user,
        )
    assert error.value.status_code == 403
    with pytest.raises(HTTPException) as error:
        await export_eval_examples(
            dataset_id=repo.dataset["dataset_id"], request=_request(), auth=auth, user=user,
        )
    assert error.value.status_code == 403

    async def invisible(_request, _user):
        return {}

    monkeypatch.setattr(eval_dataset_routes_module, "visible_dataset_names", invisible)
    catalog = await list_eval_datasets(request=_request(), auth=auth, user=user)
    assert catalog.datasets == []
    assert catalog.total == 0
    with pytest.raises(HTTPException) as error:
        await get_eval_dataset(
            dataset_id=repo.dataset["dataset_id"], request=_request(), auth=auth, user=user,
        )
    assert error.value.status_code == 403


@pytest.mark.asyncio
async def test_eval_dataset_catalog_paginates_after_kb_acl_filter(monkeypatch) -> None:
    repo = FakeTraceRepository()
    catalog = [
        {**repo.dataset, "dataset_id": "00000000-0000-4000-8000-000000000001", "metadata": {}},
        {**repo.dataset, "dataset_id": "00000000-0000-4000-8000-000000000002", "metadata": {"kb_dataset_id": "private-kb"}},
        {**repo.dataset, "dataset_id": "00000000-0000-4000-8000-000000000003", "metadata": {"kb_dataset_id": "visible-kb"}},
        {**repo.dataset, "dataset_id": "00000000-0000-4000-8000-000000000004", "metadata": {}},
    ]

    async def manifest(**_kwargs):
        return catalog

    async def visible(_request, _user):
        return {"visible-kb": "Visible"}

    monkeypatch.setattr(repo, "list_dataset_manifest", manifest)
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    monkeypatch.setattr(eval_dataset_routes_module, "visible_dataset_names", visible)
    auth = _auth()
    user = UserContext(user_id="user-a", tenant_id="tenant-a", roles=["user"], is_authenticated=True)
    first = await list_eval_datasets(request=_request(), limit=2, offset=0, auth=auth, user=user)
    second = await list_eval_datasets(request=_request(), limit=2, offset=2, auth=auth, user=user)
    assert first.total == second.total == 3
    assert [row.dataset_id for row in first.datasets] == [catalog[0]["dataset_id"], catalog[2]["dataset_id"]]
    assert [row.dataset_id for row in second.datasets] == [catalog[3]["dataset_id"]]


@pytest.mark.asyncio
async def test_delete_eval_example_is_tenant_scoped_and_repeated_delete_is_404(
    monkeypatch,
) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    auth = _auth(permissions=["console:eval:view", "console:eval:run"])

    deleted = await delete_eval_example(
        dataset_id=repo.dataset["dataset_id"],
        example_id=repo.example["example_id"],
        request=_request(),
        auth=auth,
    )

    assert deleted is None
    assert repo.calls[-1] == (
        "delete_example",
        {
            "tenant_id": "tenant-a",
            "dataset_id": repo.dataset["dataset_id"],
            "example_id": repo.example["example_id"],
        },
    )

    with pytest.raises(HTTPException) as error:
        await delete_eval_example(
            dataset_id=repo.dataset["dataset_id"],
            example_id=repo.example["example_id"],
            request=_request(),
            auth=auth,
        )
    assert error.value.status_code == 404
    assert error.value.detail == "Example not found"


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("tenant_id", "example_id"),
    [
        ("tenant-b", "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),
        ("tenant-a", "ffffffff-ffff-4fff-8fff-ffffffffffff"),
    ],
)
async def test_delete_eval_example_hides_cross_tenant_and_missing_targets(
    monkeypatch,
    tenant_id: str,
    example_id: str,
) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    with pytest.raises(HTTPException) as error:
        await delete_eval_example(
            dataset_id=repo.dataset["dataset_id"],
            example_id=example_id,
            request=_request(),
            auth=_auth(
                tenant_id=tenant_id,
                permissions=["console:eval:view", "console:eval:run"],
            ),
        )

    assert error.value.status_code == 404
    assert repo.calls[-1][1]["tenant_id"] == tenant_id


@pytest.mark.asyncio
async def test_delete_eval_example_requires_eval_run_permission(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    with pytest.raises(HTTPException) as error:
        await delete_eval_example(
            dataset_id=repo.dataset["dataset_id"],
            example_id=repo.example["example_id"],
            request=_request(),
            auth=_auth(permissions=["console:eval:view"]),
        )

    assert error.value.status_code == 403
    assert not any(call[0] == "delete_example" for call in repo.calls)


@pytest.mark.asyncio
async def test_eval_example_import_skips_duplicate_case_ids(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    auth = _auth(permissions=["console:eval:view", "console:eval:run"])
    body = EvalExamplesImportRequest(
        examples=[
            {
                "case_id": "assistant.case.one",
                "input": {"input_preview": "hello"},
                "expected_output": {"contains": "hi"},
                "expected_trajectory": {"required_span_kinds": ["model_invocation"]},
                "assertions": [{"type": "output_contains", "value": "hi"}],
                "metadata": {"review_status": "approved"},
            },
            {
                "case_id": "assistant.case.one",
                "input": {"input_preview": "duplicate"},
                "expected_output": {"contains": "dup"},
                "expected_trajectory": {"required_span_kinds": ["model_invocation"]},
                "assertions": [{"type": "output_contains", "value": "dup"}],
                "metadata": {"review_status": "approved"},
            },
            {
                "case_id": "assistant.case.two",
                "input": {"input_preview": "second"},
                "expected_output": {"contains": "two"},
                "expected_trajectory": {"required_span_kinds": ["model_invocation"]},
                "assertions": [{"type": "output_contains", "value": "two"}],
                "metadata": {"review_status": "approved"},
            },
        ]
    )

    first = await import_eval_examples(
        dataset_id=repo.dataset["dataset_id"],
        body=body,
        request=_request(),
        auth=auth,
    )
    assert first.imported == 2
    assert first.skipped == 1

    second = await import_eval_examples(
        dataset_id=repo.dataset["dataset_id"],
        body=body,
        request=_request(),
        auth=auth,
    )
    assert second.imported == 0
    assert second.skipped == 3


@pytest.mark.asyncio
async def test_eval_experiment_rejects_effective_dataset_and_trace_targets(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    auth = _auth(permissions=["console:eval:view", "console:eval:run"])

    with pytest.raises(HTTPException) as exc_info:
        await run_eval_experiment(
            experiment_id=repo.experiment["experiment_id"],
            body=EvalExperimentRunCreate(
                evaluator_ids=[repo.evaluator["evaluator_id"]],
                target_snapshot={"trace_id": "11111111-1111-4111-8111-111111111111"},
            ),
            request=_request(),
            auth=auth,
        )

    assert exc_info.value.status_code == 422
    assert "mutually exclusive" in str(exc_info.value.detail).lower()
    assert not any(call[0] == "evaluator_run" for call in repo.calls)


@pytest.mark.asyncio
async def test_eval_experiment_batch_compare_and_gate(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    auth = _auth(permissions=["console:eval:view", "console:eval:run"])

    batch = await run_eval_experiment(
        experiment_id=repo.experiment["experiment_id"],
        body=EvalExperimentRunCreate(
            dataset_id=repo.dataset["dataset_id"],
            evaluator_ids=[repo.evaluator["evaluator_id"]],
            target_snapshot={"trace_family": "assistant", "candidate_label": "candidate"},
        ),
        request=_request(),
        auth=auth,
    )
    assert batch.jobs[0].status == "queued"

    comparison = await compare_eval_experiment_runs(
        request=_request(),
        baseline_run_id=repo.baseline_run["run_id"],
        candidate_run_id=repo.candidate_run["run_id"],
        auth=_auth(),
    )
    assert comparison.deltas["overall_score"] == -0.02
    assert "overall_score" in comparison.regression_summary["regressed_metrics"]

    gate = await dry_run_eval_gate(
        body=EvalGateDryRunRequest(
            result_payload={
                "metrics": _gate_summary(
                    overall_score=0.88,
                    failed_case_count=3,
                    trajectory_failed_count=1,
                )
            }
        ),
        request=_request(),
        auth=auth,
    )
    assert gate.status == "pass"


@pytest.mark.asyncio
async def test_eval_gate_run_ids_preserve_authoritative_compatibility_and_gate(
    monkeypatch,
) -> None:
    repo = FakeTraceRepository()
    original_compare = repo.compare_experiment_runs

    async def incompatible_compare(**kwargs: Any) -> dict[str, Any] | None:
        comparison = await original_compare(**kwargs)
        assert comparison is not None
        return {
            **comparison,
            "compatibility": {
                "status": "incompatible",
                "compatible": False,
                "reasons": ["dataset_manifest_mismatch"],
            },
            "gate": {
                "status": "fail",
                "failures": ["critical_case_regression"],
                "warnings": [],
            },
        }

    repo.compare_experiment_runs = incompatible_compare  # type: ignore[method-assign]
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    gate = await dry_run_eval_gate(
        body=EvalGateDryRunRequest(
            baseline_run_id=repo.baseline_run["run_id"],
            candidate_run_id=repo.candidate_run["run_id"],
        ),
        request=_request(),
        auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
    )

    assert gate.status == "fail"
    assert gate.compatibility["compatible"] is False
    assert gate.authoritative_gate["status"] == "fail"
    assert "dataset_manifest_mismatch" in " ".join(gate.failures)
    assert "critical_case_regression" in " ".join(gate.failures)


@pytest.mark.asyncio
async def test_eval_gate_rejects_unversioned_metrics_as_incompatible_schema(
    monkeypatch,
) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    with pytest.raises(HTTPException) as error:
        await dry_run_eval_gate(
            body=EvalGateDryRunRequest(
                result_payload={
                    "metrics": {
                        "overall_score": 1.0,
                        "trajectory_pass_rate": 1.0,
                        "critical_pass_rate": 1.0,
                    }
                }
            ),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )

    assert error.value.status_code == 422
    assert error.value.detail["error"] == "incompatible_gate_metrics_schema"


@pytest.mark.asyncio
async def test_eval_gate_rejects_coercible_boolean_counts(monkeypatch) -> None:
    repo = FakeTraceRepository()
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    forged = _gate_summary(
        overall_score=1.0,
        failed_case_count=0,
        trajectory_failed_count=0,
    )
    forged["case_count"] = True

    with pytest.raises(HTTPException) as error:
        await dry_run_eval_gate(
            body=EvalGateDryRunRequest(result_payload={"metrics": forged}),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )

    assert error.value.status_code == 422
    assert error.value.detail["error"] == "incompatible_gate_metrics_schema"


@pytest.mark.asyncio
async def test_eval_experiment_live_run_freezes_private_prompt_and_defaults_repetitions(
    monkeypatch,
) -> None:
    repo = FakeTraceRepository()
    repo.example = {
        **repo.example,
        "input": {"message": "hello"},
        "expected_output": {"contains": ["hello"]},
        "metadata": {
            "case_id": "assistant.case.live",
            "expected_trajectory": {"required_span_kinds": ["model_invocation"]},
            "assertions": [{"type": "output_contains", "value": "hello"}],
            "critical": True,
        },
    }
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    response = await run_eval_experiment(
        experiment_id=repo.experiment["experiment_id"],
        body=EvalExperimentRunCreate(
            evaluator_ids=[repo.evaluator["evaluator_id"]],
            run_mode="live_candidate",
            candidate_config={"system_prompt_override": "private eval prompt"},
        ),
        request=_request(),
        auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
    )

    assert response.jobs[0].status == "queued"
    payload = next(call[1] for call in repo.calls if call[0] == "live_experiment_run")
    assert payload["repetitions"] == 3
    assert payload["execution_config"]["model_id"] == "fixture-model"
    assert payload["execution_config"]["system_prompt_override"] == "private eval prompt"
    assert "private eval prompt" not in json.dumps(payload["target_snapshot"])
    assert payload["candidate_fingerprint"]["verification"] == "pending"


@pytest.mark.asyncio
async def test_eval_live_agent_version_candidate_uses_server_identity_only(monkeypatch) -> None:
    agent_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    version_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
    spec_hash = "a" * 64
    repo = FakeTraceRepository()
    repo.example = {
        **repo.example,
        "input": {"message": "hello"},
        "expected_output": {"contains": ["hello"]},
        "metadata": {"case_id": "agent.case", "assertions": [], "critical": True},
    }
    request = _request()
    calls: list[dict[str, Any]] = []

    class AgentRepository:
        async def resolve_version_runtime(self, **kwargs: Any) -> dict[str, Any]:
            calls.append(kwargs)
            return {
                "agent": {"agent_id": agent_id},
                "version": {"agent_version_id": version_id, "spec_hash": spec_hash},
                "spec": {},
            }

    async def snapshot(_request, resolution, user, *, channel):
        assert resolution["version"]["agent_version_id"] == version_id
        assert user.user_id == "user-a" and channel == "preview"
        return {
            "agent_id": agent_id,
            "agent_version_id": version_id,
            "fingerprints": {"spec": f"sha256:{spec_hash}"},
            "model": {"id": "server-model", "provider": "server-provider"},
            "knowledge": {"datasets": ["server-kb"]},
        }

    request.app.state.agent_repository = AgentRepository()
    monkeypatch.setattr(eval_routes, "_build_snapshot", snapshot)
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    result = await run_eval_experiment(
        experiment_id=repo.experiment["experiment_id"],
        body=EvalExperimentRunCreate(
            evaluator_ids=[repo.evaluator["evaluator_id"]],
            run_mode="live_candidate",
            candidate_config={"agent_id": agent_id, "agent_version_id": version_id},
        ),
        request=request,
        auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
    )

    assert result.jobs[0].status == "queued"
    assert calls == [{
        "tenant_id": "tenant-a", "agent_id": agent_id, "agent_version_id": version_id,
        "user_id": "user-a", "is_tenant_admin": False,
    }]
    payload = next(call[1] for call in repo.calls if call[0] == "live_experiment_run")
    assert payload["execution_config"] == {
        "candidate_type": "agent_version", "agent_id": agent_id,
        "agent_version_id": version_id, "agent_spec_hash": spec_hash,
        "agent_runtime_snapshot_hash": payload["target_snapshot"]["agent_runtime_snapshot_hash"],
        "model_id": "server-model", "provider_id": "server-provider",
        "knowledge_dataset_ids": ["server-kb"],
    }
    assert payload["target_snapshot"]["agent_version_id"] == version_id
    assert payload["candidate_fingerprint"]["agent_spec_hash"] == spec_hash
    assert "resolved_spec" not in json.dumps(payload["target_snapshot"])


def test_eval_agent_version_candidate_rejects_partial_or_client_overrides() -> None:
    agent_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    version_id = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
    with pytest.raises(ValidationError):
        EvalExperimentRunCreate(
            evaluator_ids=["evaluator"], candidate_config={"agent_id": agent_id},
        )
    with pytest.raises(ValidationError):
        EvalExperimentRunCreate(
            evaluator_ids=["evaluator"],
            candidate_config={
                "agent_id": agent_id, "agent_version_id": version_id,
                "system_prompt_override": "forged",
            },
        )
    with pytest.raises(ValidationError):
        EvalExperimentRunCreate(
            evaluator_ids=["evaluator"],
            candidate_config={"agent_id": agent_id, "agent_version_id": version_id,
                              "model_id": "forged"},
        )


@pytest.mark.asyncio
async def test_eval_live_rejects_agent_identity_forged_in_experiment_target(monkeypatch) -> None:
    repo = FakeTraceRepository()
    repo.experiment["target_config"] = {
        "candidate_type": "agent_version", "agent_id": "forged-agent",
        "agent_version_id": "forged-version", "agent_spec_hash": "forged-hash",
    }
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    with pytest.raises(HTTPException) as error:
        await run_eval_experiment(
            experiment_id=repo.experiment["experiment_id"],
            body=EvalExperimentRunCreate(
                evaluator_ids=[repo.evaluator["evaluator_id"]], run_mode="live_candidate",
            ),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )

    assert error.value.status_code == 422
    assert "typed candidate_config" in str(error.value.detail)
    assert not any(call[0] == "live_experiment_run" for call in repo.calls)


@pytest.mark.asyncio
async def test_eval_experiment_live_run_rejects_unconfirmed_trace_case(monkeypatch) -> None:
    repo = FakeTraceRepository()
    repo.example = {
        **repo.example,
        "input": {"message": "hello"},
        "metadata": {
            "case_id": "assistant.case.unconfirmed",
            "expected_trajectory": {},
            "assertions": [],
            "behavior_confirmed": False,
        },
    }
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    with pytest.raises(HTTPException) as error:
        await run_eval_experiment(
            experiment_id=repo.experiment["experiment_id"],
            body=EvalExperimentRunCreate(
                evaluator_ids=[repo.evaluator["evaluator_id"]],
                run_mode="live_candidate",
            ),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )

    assert error.value.status_code == 422
    assert "confirmed" in json.dumps(error.value.detail)


@pytest.mark.asyncio
async def test_eval_live_run_ignores_pending_cases_and_freezes_only_approved(monkeypatch) -> None:
    repo = FakeTraceRepository()
    approved = {
        **repo.example,
        "split": "regression",
        "input": {"message": "approved request"},
        "metadata": {"case_id": "approved-case", "review_status": "approved"},
    }
    pending = {
        **repo.example,
        "example_id": "pending-id",
        "split": "review",
        "input": {"query": "KB failure"},
        "metadata": {"case_id": "pending-case", "review_status": "pending", "behavior_confirmed": False},
    }
    approved_kb = {
        **repo.example,
        "example_id": "approved-kb-id",
        "split": "review",
        "input": {"query": "Approved KB question"},
        "expected_output": {"answer": "Expected KB answer"},
        "metadata": {"case_id": "approved-kb", "source_kind": "kb_failure", "review_status": "approved"},
    }

    async def manifest(**_kwargs):
        return [pending, approved_kb, approved]

    monkeypatch.setattr(repo, "list_example_manifest", manifest)
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    response = await run_eval_experiment(
        experiment_id=repo.experiment["experiment_id"],
        body=EvalExperimentRunCreate(
            evaluator_ids=[repo.evaluator["evaluator_id"]], run_mode="live_candidate",
        ),
        request=_request(),
        auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
    )

    assert response.jobs[0].status == "queued"
    payload = next(call[1] for call in repo.calls if call[0] == "live_experiment_run")
    assert [example["metadata"]["case_id"] for example in payload["examples"]] == ["approved-case"]


@pytest.mark.asyncio
async def test_eval_live_run_with_only_pending_cases_is_not_enqueued(monkeypatch) -> None:
    repo = FakeTraceRepository()
    repo.example = {
        **repo.example,
        "split": "review",
        "input": {"query": "KB failure"},
        "metadata": {"case_id": "pending-case", "review_status": "pending"},
    }
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    with pytest.raises(HTTPException) as error:
        await run_eval_experiment(
            experiment_id=repo.experiment["experiment_id"],
            body=EvalExperimentRunCreate(
                evaluator_ids=[repo.evaluator["evaluator_id"]], run_mode="live_candidate",
            ),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )

    assert error.value.status_code == 422
    assert error.value.detail == "Dataset has no approved examples"
    assert not any(call[0] == "live_experiment_run" for call in repo.calls)


@pytest.mark.asyncio
async def test_eval_live_run_with_approved_kb_case_is_still_not_enqueued(monkeypatch) -> None:
    repo = FakeTraceRepository()
    repo.example = {
        **repo.example,
        "split": "review",
        "input": {"query": "KB question"},
        "expected_output": {"answer": "Human approved answer"},
        "metadata": {"case_id": "kb-approved", "source_kind": "kb_failure", "review_status": "approved"},
    }
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)
    with pytest.raises(HTTPException) as error:
        await run_eval_experiment(
            experiment_id=repo.experiment["experiment_id"],
            body=EvalExperimentRunCreate(
                evaluator_ids=[repo.evaluator["evaluator_id"]], run_mode="live_candidate",
            ),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )
    assert error.value.status_code == 422
    assert error.value.detail == "Dataset has no approved examples"
    assert not any(call[0] == "live_experiment_run" for call in repo.calls)


@pytest.mark.asyncio
async def test_eval_baseline_promotion_requires_complete_verified_live_run(monkeypatch) -> None:
    repo = FakeTraceRepository()
    repo.experiment["baseline_run_id"] = None
    repo.candidate_run = {
        **repo.candidate_run,
        "run_mode": "live_candidate",
        "status": "succeeded",
        "dataset_manifest_hash": "a" * 64,
        "evaluator_suite_hash": "b" * 64,
        "score_summary": _gate_summary(
            overall_score=0.9,
            failed_case_count=0,
            trajectory_failed_count=0,
        ),
        "metrics": {
            "gate": {"status": "pass"},
            "critical_case_count": 5,
            "required_hard_blockers": list(REQUIRED_ASSISTANT_HARD_BLOCKERS),
            "hard_blocker_results": dict.fromkeys(REQUIRED_ASSISTANT_HARD_BLOCKERS, True),
            "hard_blockers_passed": True,
            "attempted_trials": 3,
            "failed_trials": 0,
            "completed_trials": 3,
            "total_trials": 3,
            "mixed_runtime": False,
            "actual_fingerprint": {
                "system_prompt_hash": "prompt-a",
                "tool_schema_hash": "tools-a",
                "model_id": "qwen3.7-plus",
                "provider": "dashscope",
                "runtime_revision": "runtime-a",
            },
        },
    }
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    promoted = await promote_eval_experiment_baseline(
        experiment_id=repo.experiment["experiment_id"],
        body=EvalBaselinePromotionRequest(run_id=repo.candidate_run["run_id"]),
        request=_request(),
        auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
    )

    assert promoted.baseline_run_id == repo.candidate_run["run_id"]
    assert any(call[0] == "promote_baseline" for call in repo.calls)
    promotion_call = next(call for call in repo.calls if call[0] == "promote_baseline")
    assert promotion_call[1]["expected_previous_baseline_run_id"] is None

    for field in (
        "attempted_trials",
        "completed_trials",
        "failed_trials",
        "total_trials",
    ):
        saved = repo.candidate_run["metrics"].pop(field)
        with pytest.raises(HTTPException, match="complete, error-free live run"):
            await promote_eval_experiment_baseline(
                experiment_id=repo.experiment["experiment_id"],
                body=EvalBaselinePromotionRequest(run_id=repo.candidate_run["run_id"]),
                request=_request(),
                auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
            )
        repo.candidate_run["metrics"][field] = saved

    repo.candidate_run["metrics"].pop("mixed_runtime")
    with pytest.raises(HTTPException, match="explicit single-runtime"):
        await promote_eval_experiment_baseline(
            experiment_id=repo.experiment["experiment_id"],
            body=EvalBaselinePromotionRequest(run_id=repo.candidate_run["run_id"]),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )


@pytest.mark.asyncio
async def test_eval_baseline_promotion_rejects_partial_runtime_fingerprint(monkeypatch) -> None:
    repo = FakeTraceRepository()
    repo.experiment["baseline_run_id"] = None
    repo.candidate_run = {
        **repo.candidate_run,
        "run_mode": "live_candidate",
        "status": "succeeded",
        "dataset_manifest_hash": "a" * 64,
        "evaluator_suite_hash": "b" * 64,
        "score_summary": _gate_summary(
            overall_score=0.9,
            failed_case_count=0,
            trajectory_failed_count=0,
        ),
        "metrics": {
            "gate": {"status": "pass"},
            "critical_case_count": 5,
            "required_hard_blockers": list(REQUIRED_ASSISTANT_HARD_BLOCKERS),
            "hard_blocker_results": dict.fromkeys(REQUIRED_ASSISTANT_HARD_BLOCKERS, True),
            "hard_blockers_passed": True,
            "attempted_trials": 3,
            "failed_trials": 0,
            "completed_trials": 3,
            "total_trials": 3,
            "mixed_runtime": False,
            "actual_fingerprint": {"runtime_revision": "runtime-a"},
        },
    }
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    with pytest.raises(HTTPException) as error:
        await promote_eval_experiment_baseline(
            experiment_id=repo.experiment["experiment_id"],
            body=EvalBaselinePromotionRequest(run_id=repo.candidate_run["run_id"]),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )

    assert error.value.status_code == 409
    assert "complete verified runtime fingerprint" in error.value.detail


@pytest.mark.asyncio
async def test_eval_baseline_promotion_rejects_missing_safety_or_provenance(monkeypatch) -> None:
    repo = FakeTraceRepository()
    repo.experiment["baseline_run_id"] = None
    repo.candidate_run = {
        **repo.candidate_run,
        "run_mode": "live_candidate",
        "status": "succeeded",
        "score_summary": _gate_summary(
            overall_score=0.9,
            failed_case_count=0,
            trajectory_failed_count=0,
        ),
        "metrics": {
            "gate": {"status": "pass"},
            "critical_case_count": 0,
            "hard_blocker_results": {},
            "hard_blockers_passed": False,
        },
    }
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    with pytest.raises(HTTPException, match="mandatory Assistant safety blockers"):
        await promote_eval_experiment_baseline(
            experiment_id=repo.experiment["experiment_id"],
            body=EvalBaselinePromotionRequest(run_id=repo.candidate_run["run_id"]),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )

    repo.candidate_run["metrics"].update(
        {
            "critical_case_count": 5,
            "required_hard_blockers": list(REQUIRED_ASSISTANT_HARD_BLOCKERS),
            "hard_blocker_results": dict.fromkeys(REQUIRED_ASSISTANT_HARD_BLOCKERS, True),
            "hard_blockers_passed": True,
        }
    )
    with pytest.raises(HTTPException, match="verified dataset and evaluator provenance"):
        await promote_eval_experiment_baseline(
            experiment_id=repo.experiment["experiment_id"],
            body=EvalBaselinePromotionRequest(run_id=repo.candidate_run["run_id"]),
            request=_request(),
            auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
        )


@pytest.mark.asyncio
async def test_eval_baseline_promotion_rejects_legacy_coercible_and_forged_receipts(
    monkeypatch,
) -> None:
    repo = FakeTraceRepository()
    repo.experiment["baseline_run_id"] = None
    repo.candidate_run = {
        **repo.candidate_run,
        "run_mode": "live_candidate",
        "status": "succeeded",
    }
    monkeypatch.setattr(eval_routes, "_get_trace_repository", lambda _request: repo)

    valid = _gate_summary(
        overall_score=1.0,
        failed_case_count=0,
        trajectory_failed_count=0,
    )
    receipts = [
        ({"overall_score": 1.0, "critical_pass_rate": 1.0}, "incompatible_gate_metrics_schema"),
        ({**valid, "case_count": True}, "incompatible_gate_metrics_schema"),
        (
            {**valid, "critical_case_count": 999, "stateful_case_count": 999},
            "incompatible_gate_metrics_schema",
        ),
        ({**valid, "failed_case_count": 1}, "baseline_quality_gate_failed"),
    ]

    for score_summary, expected_error in receipts:
        repo.candidate_run["score_summary"] = score_summary
        with pytest.raises(HTTPException) as error:
            await promote_eval_experiment_baseline(
                experiment_id=repo.experiment["experiment_id"],
                body=EvalBaselinePromotionRequest(run_id=repo.candidate_run["run_id"]),
                request=_request(),
                auth=_auth(permissions=["console:eval:view", "console:eval:run"]),
            )
        assert error.value.status_code == 409
        assert error.value.detail["error"] == expected_error


def test_eval_dataset_subrouter_reexports_endpoint_identity() -> None:
    actual = {
        (method, route.path): route.endpoint
        for route in eval_routes._eval_dataset_routes.router.routes
        for method in route.methods or set()
    }
    assert actual == {
        ("GET", "/datasets"): eval_routes.list_eval_datasets,
        ("GET", "/datasets/{dataset_id}"): eval_routes.get_eval_dataset,
        ("GET", "/datasets/{dataset_id}/examples"): eval_routes.list_eval_examples,
        (
            "PATCH",
            "/datasets/{dataset_id}/examples/{example_id}",
        ): eval_routes.update_eval_example,
        (
            "DELETE",
            "/datasets/{dataset_id}/examples/{example_id}",
        ): eval_routes.delete_eval_example,
        (
            "POST",
            "/datasets/{dataset_id}/examples:import",
        ): eval_routes.import_eval_examples,
        (
            "GET",
            "/datasets/{dataset_id}/examples:export",
        ): eval_routes.export_eval_examples,
        ("POST", "/datasets"): eval_routes.create_eval_dataset,
        (
            "POST",
            "/datasets/{dataset_id}/examples:from-trace",
        ): eval_routes.create_eval_example_from_trace,
        (
            "GET",
            "/datasets/{dataset_id}/kb-failures/{case_id}",
        ): eval_routes.get_latest_kb_failure_example,
        (
            "POST",
            "/datasets/{dataset_id}/kb-failures:save",
        ): eval_routes.save_kb_failure_example,
        ("POST", "/trace-feedback:preview"): eval_routes.preview_eval_trace_feedback,
    }


def test_eval_openapi_paths_are_registered() -> None:
    paths = create_app().openapi()["paths"]

    assert "/api/v1/eval/summary" in paths
    assert "/api/v1/eval/dashboard" in paths
    assert "/api/v1/eval/traces" in paths
    assert "/api/v1/eval/traces/{trace_id}" in paths
    assert "/api/v1/eval/traces/{trace_id}/scores" in paths
    assert "/api/v1/eval/ingest/traces" in paths
    assert "/api/v1/eval/threads/{thread_id}" in paths
    assert "/api/v1/eval/traces/{trace_id}/export" in paths
    assert "/api/v1/eval/datasets" in paths
    assert "/api/v1/eval/datasets/{dataset_id}/examples" in paths
    assert "/api/v1/eval/datasets/{dataset_id}/examples/{example_id}" in paths
    assert "/api/v1/eval/datasets/{dataset_id}/examples:import" in paths
    assert "/api/v1/eval/datasets/{dataset_id}/examples:export" in paths
    assert "/api/v1/eval/datasets/{dataset_id}/examples:from-trace" in paths
    assert "/api/v1/eval/datasets/{dataset_id}/kb-failures/{case_id}" in paths
    assert "/api/v1/eval/datasets/{dataset_id}/kb-failures:save" in paths
    assert "/api/v1/eval/trace-feedback:preview" in paths
    assert "/api/v1/eval/evaluators" in paths
    assert "/api/v1/eval/evaluators/{evaluator_id}:run-async" in paths
    assert "/api/v1/eval/experiments" in paths
    assert "/api/v1/eval/experiments/{experiment_id}:run" in paths
    assert "/api/v1/eval/experiment-runs/{run_id}" in paths
    assert "/api/v1/eval/experiment-runs:compare" in paths
    assert "/api/v1/eval/gates:dry-run" in paths


def test_eval_openapi_and_web_types_expose_shared_trace_contract() -> None:
    schemas = create_app().openapi()["components"]["schemas"]

    assert {
        "thread_id",
        "metrics",
        "privacy",
        "source_adapter",
    }.issubset(schemas["AgentTraceSummary"]["properties"])
    assert {
        "target_type",
        "target_id",
        "evaluator_id",
        "evaluator_name",
        "score_source",
        "confidence",
    }.issubset(schemas["AgentTraceScore"]["properties"])
    assert "redaction_policy" in schemas["EvalTraceExportResponse"]["properties"]
    assert "runtime_health" in schemas["EvalDashboardResponse"]["properties"]

    web_types = Path("web/src/api/eval.ts").read_text(encoding="utf-8")
    for token in (
        "thread_id?: string | null",
        "metrics: Record<string, unknown>",
        "privacy: Record<string, unknown>",
        "source_adapter?: string | null",
        "target_type: ScoreTargetType",
        "target_id?: string | null",
        "evaluator_id?: string | null",
        "evaluator_name?: string | null",
        "score_source: string",
        "confidence?: number | null",
        "redaction_policy: Record<string, unknown>",
        "runs: EvalExperimentRun[]",
        "EvalDashboardResponse",
        "runtime_health: Record<string, unknown>",
        "updateEvalExample",
        "importEvalExamples",
        "exportEvalExamples",
        "compareEvalExperimentRuns",
        "dryRunEvalGate",
    ):
        assert token in web_types


class CapturingTraceRepository(AgentTraceRepository):
    def __init__(self) -> None:
        self.fetchrow_calls: list[tuple[str, tuple[Any, ...]]] = []
        self.fetch_calls: list[tuple[str, tuple[Any, ...]]] = []

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        self.fetchrow_calls.append((query, args))
        return {"total": 0}

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        self.fetch_calls.append((query, args))
        return []


@pytest.mark.asyncio
async def test_agent_trace_repository_filters_by_transcript_locator() -> None:
    repo = CapturingTraceRepository()

    rows, total = await repo.list_traces(
        tenant_id="tenant-a",
        trace_family="assistant",
        session_id="session-a",
        request_id="request-c",
        transcript_query="refund transcript turn",
        turn_index=3,
    )

    assert rows == []
    assert total == 0
    count_query, count_args = repo.fetchrow_calls[-1]
    page_query, page_args = repo.fetch_calls[-1]
    assert "t.request_id" in count_query
    assert "t.metadata->'transcript_locator'->>'turn_index'" in count_query
    assert "current_message_preview" in count_query
    assert "transcript_excerpt" in count_query
    assert "turn_id" in count_query
    assert "request-c" in count_args
    assert "3" in count_args
    assert "%refund transcript turn%" in count_args
    assert page_args[-2:] == (50, 0)
    assert "ORDER BY t.created_at DESC" in page_query
