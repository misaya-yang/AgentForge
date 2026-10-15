from __future__ import annotations

from typing import Any

import pytest
from ai_gateway_core.eval.evaluator_executor import EvaluatorExecutor, EvaluatorRunResult
from ai_gateway_core.eval.outbox_worker import EvalOutboxWorker
from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository

from src.services.eval import eval_outbox_worker as eval_outbox_worker_module
from src.services.eval.kb_ragas_client import KbRagasMetricResult


class FakeEvalRepository:
    def __init__(self, *, jobs: list[dict[str, Any]] | None = None) -> None:
        self.jobs = list(jobs or [])
        self.succeeded: list[str] = []
        self.failed: list[tuple[str, str]] = []
        self.run_updates: list[dict[str, Any]] = []

    async def claim_outbox_jobs(self, **_kwargs: Any) -> list[dict[str, Any]]:
        if not self.jobs:
            return []
        batch = self.jobs[:]
        self.jobs = []
        return batch

    async def mark_outbox_succeeded(self, job_id: str) -> None:
        self.succeeded.append(job_id)

    async def mark_outbox_failed(
        self,
        job_id: str,
        *,
        error: str,
        retry_after_seconds: int,  # noqa: ARG002
        max_attempts: int,  # noqa: ARG002
    ) -> None:
        self.failed.append((job_id, error))

    async def update_experiment_run(self, **kwargs: Any) -> None:
        self.run_updates.append(kwargs)


class FakeRagasOutboxRepository(FakeEvalRepository):
    metrics = ("context_relevancy",)

    async def update_experiment_run(self, **_kwargs: Any) -> None:
        return None

    async def get_evaluator(self, **_kwargs: Any) -> dict[str, Any]:
        return {
            "evaluator_id": "eval-1",
            "evaluator_type": "ragas",
            "name": "kb-ragas",
            "filter_config": {"metrics": list(self.metrics)},
        }

    async def get_trace_detail(self, **_kwargs: Any) -> dict[str, Any]:
        return {
            "trace": {
                "trace_id": "trace-1",
                "trace_family": "rag",
                "input_preview": "question",
                "status": "succeeded",
            },
            "spans": [
                {
                    "span_kind": "retriever",
                    "attributes": {"retrieval": {"documents": [{"content_eval": "chunk"}]}},
                }
            ],
            "events": [],
        }

    async def create_eval_score(self, **kwargs: Any) -> dict[str, Any]:
        payload = kwargs["payload"]
        return {
            "score_id": "score-1",
            "numeric_value": payload["numeric_value"],
            "label": payload["label"],
        }


class FakeEvaluatorExecutor:
    def __init__(self) -> None:
        self.calls: list[dict[str, Any]] = []

    async def run_job(self, *, tenant_id: str, job_payload: dict[str, Any]) -> EvaluatorRunResult:
        self.calls.append({"tenant_id": tenant_id, "job_payload": job_payload})
        return EvaluatorRunResult(run_id=str(job_payload.get("run_id")), status="succeeded")


@pytest.mark.asyncio
async def test_kb_ragas_worker_mapping_preserves_failure_kind(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class _Client:
        async def evaluate_retrieval(self, **_kwargs: Any) -> list[KbRagasMetricResult]:
            return [
                KbRagasMetricResult(
                    metric="context_relevancy",
                    score=0.0,
                    explanation="judge unavailable",
                    label="review",
                    failure_kind="infrastructure",
                )
            ]

    monkeypatch.setattr(eval_outbox_worker_module, "_kb_ragas_client", _Client())

    results = await eval_outbox_worker_module._kb_ragas_evaluate(
        query="q",
        contexts=["c"],
    )

    assert results[0]["failure_kind"] == "infrastructure"


@pytest.mark.asyncio
async def test_outbox_worker_executes_eval_job_and_marks_success() -> None:
    repo = FakeEvalRepository(
        jobs=[
            {
                "job_id": "job-1",
                "tenant_id": "tenant-a",
                "job_type": "eval.evaluator.run",
                "payload": {"run_id": "run-1", "evaluator_id": "eval-1"},
                "attempts": 1,
            }
        ]
    )
    executor = FakeEvaluatorExecutor()
    worker = EvalOutboxWorker(repo, executor, poll_interval_s=0.01, batch_size=1)

    await worker._handle_job(
        {
            "job_id": "job-1",
            "tenant_id": "tenant-a",
            "job_type": "eval.evaluator.run",
            "payload": {"run_id": "run-1", "evaluator_id": "eval-1"},
            "attempts": 1,
        }
    )

    assert executor.calls
    assert repo.succeeded == ["job-1"]


@pytest.mark.asyncio
async def test_outbox_worker_schedules_online_eval_on_trace_ingested(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    scheduled: list[dict[str, Any]] = []

    async def _schedule(repository, **kwargs: Any) -> dict[str, Any]:  # noqa: ANN001
        scheduled.append(kwargs)
        return {"scheduled": 1}

    monkeypatch.setattr(
        "ai_gateway_core.eval.outbox_worker.schedule_online_eval_for_trace",
        _schedule,
    )
    repo = FakeEvalRepository()
    worker = EvalOutboxWorker(repo, FakeEvaluatorExecutor(), poll_interval_s=0.01, batch_size=1)

    await worker._handle_job(
        {
            "job_id": "job-trace",
            "tenant_id": "tenant-a",
            "job_type": "trace.ingested",
            "payload": {
                "trace_id": "trace-1",
                "trace_family": "assistant",
                "status": "succeeded",
            },
            "attempts": 1,
        }
    )

    assert repo.succeeded == ["job-trace"]
    assert scheduled[0]["tenant_id"] == "tenant-a"
    assert scheduled[0]["payload"]["trace_id"] == "trace-1"


@pytest.mark.asyncio
async def test_outbox_worker_marks_failed_job_for_retry() -> None:
    repo = FakeEvalRepository()

    class FailingExecutor(FakeEvaluatorExecutor):
        async def run_job(
            self,
            *,
            tenant_id: str,  # noqa: ARG002
            job_payload: dict[str, Any],  # noqa: ARG002
        ) -> EvaluatorRunResult:
            raise RuntimeError("judge unavailable")

    worker = EvalOutboxWorker(repo, FailingExecutor(), poll_interval_s=0.01, batch_size=1)
    await worker._handle_job(
        {
            "job_id": "job-2",
            "tenant_id": "tenant-a",
            "job_type": "eval.evaluator.run",
            "payload": {"run_id": "run-2", "evaluator_id": "eval-1"},
            "attempts": 2,
        }
    )

    assert repo.failed == [("job-2", "eval_job_failed:RuntimeError")]
    assert repo.run_updates[-1]["status"] == "queued"


@pytest.mark.asyncio
async def test_outbox_worker_retries_infrastructure_review_run() -> None:
    repo = FakeRagasOutboxRepository()

    async def _evaluate(**_kwargs: Any) -> list[dict[str, Any]]:
        return [
            {
                "metric": "context_relevancy",
                "score": 0.0,
                "explanation": "judge unavailable",
                "label": "review",
                "failure_kind": "infrastructure",
            }
        ]

    executor = EvaluatorExecutor(repo, kb_ragas_evaluate=_evaluate)  # type: ignore[arg-type]
    worker = EvalOutboxWorker(repo, executor, poll_interval_s=0.01, batch_size=1)
    await worker._handle_job(
        {
            "job_id": "job-infrastructure",
            "tenant_id": "tenant-a",
            "job_type": "eval.evaluator.run",
            "payload": {
                "run_id": "run-infrastructure",
                "evaluator_id": "eval-1",
                "trace_id": "trace-1",
                "target_snapshot": {"trace_family": "rag"},
            },
            "attempts": 1,
        }
    )

    assert repo.failed == [("job-infrastructure", "eval_job_failed:RuntimeError")]
    assert repo.succeeded == []


@pytest.mark.asyncio
async def test_outbox_worker_succeeds_semantic_review_run() -> None:
    repo = FakeRagasOutboxRepository()
    repo.metrics = ("context_precision",)

    async def _evaluate(**_kwargs: Any) -> list[dict[str, Any]]:
        return [
            {
                "metric": "context_precision",
                "score": 0.0,
                "explanation": "ground truth missing",
                "label": "review",
                "failure_kind": "semantic_review",
            }
        ]

    executor = EvaluatorExecutor(repo, kb_ragas_evaluate=_evaluate)  # type: ignore[arg-type]
    worker = EvalOutboxWorker(repo, executor, poll_interval_s=0.01, batch_size=1)
    await worker._handle_job(
        {
            "job_id": "job-semantic",
            "tenant_id": "tenant-a",
            "job_type": "eval.evaluator.run",
            "payload": {
                "run_id": "run-semantic",
                "evaluator_id": "eval-1",
                "trace_id": "trace-1",
                "target_snapshot": {"trace_family": "rag"},
            },
            "attempts": 1,
        }
    )

    assert repo.succeeded == ["job-semantic"]
    assert repo.failed == []


class FakeTransaction:
    async def __aenter__(self) -> None:
        return None

    async def __aexit__(self, *_args: Any) -> None:
        return None


class FakeOutboxConnection:
    def __init__(self) -> None:
        self.fetch_calls: list[tuple[str, tuple[Any, ...]]] = []
        self.fetchrow_calls: list[tuple[str, tuple[Any, ...]]] = []

    def transaction(self) -> FakeTransaction:
        return FakeTransaction()

    async def execute(self, _query: str, *_args: Any) -> str:
        return "UPDATE 0"

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        self.fetch_calls.append((query, args))
        return [
            {
                "job_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                "tenant_id": "tenant-a",
                "job_type": "eval.evaluator.run",
                "payload": {"run_id": "run-a"},
                "attempts": 1,
            }
        ]

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any]:
        self.fetchrow_calls.append((query, args))
        return {
            "job_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "tenant_id": "tenant-a",
            "job_type": "trace.ingested",
            "payload": {"trace_id": "trace-a"},
        }


class FakePoolAcquire:
    def __init__(self, conn: FakeOutboxConnection) -> None:
        self.conn = conn

    async def __aenter__(self) -> FakeOutboxConnection:
        return self.conn

    async def __aexit__(self, *_args: Any) -> None:
        return None


class FakePoolHolder:
    enabled = True

    def __init__(self, conn: FakeOutboxConnection) -> None:
        self.conn = conn
        self._pool = self

    def acquire(self) -> FakePoolAcquire:
        return FakePoolAcquire(self.conn)


class _TraceIngestRepo:
    def __init__(self) -> None:
        self.pending_trace_ids: set[str] = set()
        self.created: list[dict[str, Any]] = []

    async def has_pending_trace_ingested_job(self, *, tenant_id: str, trace_id: str) -> bool:  # noqa: ARG002
        return trace_id in self.pending_trace_ids

    async def create_outbox_job(self, **kwargs: Any) -> dict[str, Any]:
        self.created.append(kwargs)
        return {"job_id": "job-ingest-1"}

    async def create_trace_ingested_outbox_job(self, **kwargs: Any) -> dict[str, Any] | None:
        trace_id = str(kwargs.get("trace_id") or "")
        if await self.has_pending_trace_ingested_job(
            tenant_id=str(kwargs.get("tenant_id")), trace_id=trace_id
        ):
            return None
        self.pending_trace_ids.add(trace_id)
        return await self.create_outbox_job(
            tenant_id=str(kwargs.get("tenant_id")),
            job_type="trace.ingested",
            payload={
                "trace_id": trace_id,
                "trace_family": kwargs.get("trace_family"),
                "status": kwargs.get("status"),
                "source_adapter": kwargs.get("source_adapter"),
            },
        )


@pytest.mark.asyncio
async def test_create_trace_ingested_outbox_job_dedupes_pending_trace() -> None:
    repo = _TraceIngestRepo()
    repo.pending_trace_ids.add("trace-dup")

    result = await repo.create_trace_ingested_outbox_job(
        tenant_id="tenant-a",
        trace_id="trace-dup",
        trace_family="assistant",
        status="succeeded",
        source_adapter="api",
    )

    assert result is None
    assert repo.created == []


@pytest.mark.asyncio
async def test_repository_claim_outbox_jobs_binds_owner_and_lease() -> None:
    conn = FakeOutboxConnection()
    repo = AgentTraceRepository(FakePoolHolder(conn))

    rows = await repo.claim_outbox_jobs(limit=3, max_attempts=7, owner_id="worker-1", lease_seconds=30)

    assert rows[0]["job_id"] == "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    assert conn.fetch_calls
    assert conn.fetch_calls[0][1] == (3, 7, "worker-1", 30)


@pytest.mark.asyncio
async def test_repository_trace_ingest_outbox_uses_unambiguous_parameter_types() -> None:
    conn = FakeOutboxConnection()
    repo = AgentTraceRepository(FakePoolHolder(conn))

    result = await repo.create_trace_ingested_outbox_job(
        tenant_id="tenant-a",
        trace_id="trace-a",
        trace_family="rag",
        status="succeeded",
        source_adapter="api",
    )

    assert result is not None
    query, args = next(call for call in conn.fetchrow_calls if "INSERT INTO agent_trace_outbox" in call[0])
    assert "$1::varchar" in query
    assert "$3::text" in query
    assert args[0] == "tenant-a"
    assert args[2] == "trace-a"
