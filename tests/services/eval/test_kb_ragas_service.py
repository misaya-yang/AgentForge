from __future__ import annotations

from typing import Any

import pytest
from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository

from src.services.eval.kb_ragas_service import (
    batch_score_kb_ragas_traces,
    score_retrieval_with_kb_ragas,
)


class FakeKbRagasRepository:
    def __init__(self) -> None:
        self.evaluator = {
            "evaluator_id": "eval-ragas",
            "evaluator_type": "ragas",
            "name": "kb-ragas",
        }
        self.traces = [
            {"trace_id": "trace-1"},
            {"trace_id": "trace-2"},
        ]
        self.enqueued: list[dict[str, Any]] = []
        self.existing_scores: set[str] = set()
        self.active_runs: set[str] = set()
        self.score_labels: dict[str, list[str]] = {}
        self.run_statuses: dict[str, str] = {}

    async def get_evaluator(self, **_kwargs: Any) -> dict[str, Any] | None:
        return self.evaluator

    async def list_traces(self, **kwargs: Any) -> tuple[list[dict[str, Any]], int]:
        offset = int(kwargs.get("offset") or 0)
        limit = int(kwargs.get("limit") or len(self.traces))
        page = self.traces[offset : offset + limit]
        return page, len(self.traces)

    async def trace_has_kb_ragas_score(self, **kwargs: Any) -> bool:
        trace_id = str(kwargs.get("trace_id") or "")
        return trace_id in self.existing_scores or any(
            label in {"pass", "fail"} for label in self.score_labels.get(trace_id, [])
        )

    async def has_active_evaluator_run_for_trace(self, **kwargs: Any) -> bool:
        trace_id = str(kwargs.get("trace_id") or "")
        return trace_id in self.active_runs or self.run_statuses.get(trace_id) in {"queued", "running"}

    async def enqueue_evaluator_run(self, **kwargs: Any) -> dict[str, Any]:
        payload = kwargs.get("payload") or {}
        trace_id = str((payload.get("target_snapshot") or {}).get("trace_id") or "")
        job = {"job_id": f"job-{len(self.enqueued) + 1}", "trace_id": trace_id}
        self.enqueued.append(kwargs)
        return job


class RecordingAgentTraceRepository(AgentTraceRepository):
    def __init__(
        self,
        *,
        fetchrow_results: list[dict[str, Any] | None] | None = None,
        fetch_results: list[list[dict[str, Any]]] | None = None,
    ) -> None:
        self.queries: list[tuple[str, str]] = []
        self.fetchrow_results = list(fetchrow_results or [])
        self.fetch_results = list(fetch_results or [])

    async def fetchrow(self, query: str, *_args: Any) -> dict[str, Any] | None:
        self.queries.append(("fetchrow", query))
        return self.fetchrow_results.pop(0) if self.fetchrow_results else None

    async def fetch(self, query: str, *_args: Any) -> list[dict[str, Any]]:
        self.queries.append(("fetch", query))
        return self.fetch_results.pop(0) if self.fetch_results else []


class RevisionContractRepository(AgentTraceRepository):
    def __init__(self, score_revisions: list[dict[str, Any]]) -> None:
        self.score_revisions = score_revisions

    def _query_scores(self, query: str) -> list[dict[str, Any]]:
        if "ROW_NUMBER() OVER" not in query or "score_revision = 1" not in query:
            return list(self.score_revisions)
        latest: dict[tuple[str, str, str, str], dict[str, Any]] = {}
        for row in self.score_revisions:
            key = (
                str(row["trace_id"]),
                str(row["evaluator_id"]),
                str(row.get("evaluator_version") or ""),
                str(row["score_name"]),
            )
            if key not in latest or (row["created_at"], row["score_id"]) > (
                latest[key]["created_at"],
                latest[key]["score_id"],
            ):
                latest[key] = row
        return list(latest.values())

    async def fetchrow(self, query: str, *_args: Any) -> dict[str, Any] | None:
        scores = self._query_scores(query)
        if "ragas_scored_traces" in query:
            return {
                "rag_traces": len({row["trace_id"] for row in self.score_revisions}),
                "ragas_scored_traces": len(
                    {row["trace_id"] for row in scores if row["label"] in {"pass", "fail"}}
                ),
            }
        judge_rows = [row for row in scores if row.get("judge_model")]
        latest_judge = max(
            judge_rows,
            key=lambda row: (row["created_at"], row["score_id"]),
            default=None,
        )
        return {"judge_model": latest_judge["judge_model"]} if latest_judge else None

    async def fetch(self, query: str, *_args: Any) -> list[dict[str, Any]]:
        scores = self._query_scores(query)
        metrics: list[dict[str, Any]] = []
        for metric in sorted({str(row["score_name"]) for row in scores}):
            rows = [row for row in scores if row["score_name"] == metric]
            valid = [row for row in rows if row["label"] in {"pass", "fail"}]
            metrics.append(
                {
                    "metric": metric,
                    "average_score": (
                        sum(float(row["numeric_value"]) for row in valid) / len(valid)
                        if valid
                        else 0.0
                    ),
                    "scored_count": len(valid),
                    "pass_count": sum(1 for row in rows if row["label"] == "pass"),
                    "fail_count": sum(1 for row in rows if row["label"] == "fail"),
                    "review_count": sum(1 for row in rows if row["label"] == "review"),
                }
            )
        return metrics


@pytest.mark.asyncio
async def test_batch_score_kb_ragas_queues_unscored_traces() -> None:
    repo = FakeKbRagasRepository()
    repo.existing_scores.add("trace-2")

    result = await batch_score_kb_ragas_traces(
        repo,  # type: ignore[arg-type]
        tenant_id="tenant-a",
        dataset_id="dataset-1",
        evaluator_id="eval-ragas",
        created_by="tester",
        limit=10,
        only_unscored=True,
    )

    assert result["queued"] == 1
    assert result["skipped"] == 1
    assert len(result["jobs"]) == 1
    assert repo.enqueued[0]["payload"]["metadata"]["kb_ragas_batch"] is True


@pytest.mark.asyncio
async def test_batch_score_kb_ragas_rejects_non_ragas_evaluator() -> None:
    repo = FakeKbRagasRepository()
    repo.evaluator["evaluator_type"] = "rule"

    with pytest.raises(ValueError, match="ragas"):
        await batch_score_kb_ragas_traces(
            repo,  # type: ignore[arg-type]
            tenant_id="tenant-a",
            dataset_id="dataset-1",
            evaluator_id="eval-ragas",
            created_by="tester",
        )


@pytest.mark.asyncio
async def test_batch_score_kb_ragas_paginates_until_limit_reached() -> None:
    repo = FakeKbRagasRepository()
    repo.traces = [{"trace_id": f"trace-{index}"} for index in range(1, 8)]

    result = await batch_score_kb_ragas_traces(
        repo,  # type: ignore[arg-type]
        tenant_id="tenant-a",
        dataset_id="dataset-1",
        evaluator_id="eval-ragas",
        created_by="tester",
        limit=3,
        only_unscored=True,
    )

    assert result["queued"] == 3
    assert result["skipped"] == 0
    assert len(result["jobs"]) == 3


@pytest.mark.asyncio
async def test_completed_review_only_run_is_retryable() -> None:
    repo = FakeKbRagasRepository()
    repo.traces = [{"trace_id": "trace-review"}]
    repo.score_labels["trace-review"] = ["review"]
    repo.run_statuses["trace-review"] = "succeeded"

    result = await batch_score_kb_ragas_traces(
        repo,  # type: ignore[arg-type]
        tenant_id="tenant-a",
        dataset_id="dataset-1",
        evaluator_id="eval-ragas",
        created_by="tester",
        limit=1,
        only_unscored=True,
    )

    assert result["queued"] == 1
    assert result["skipped"] == 0


@pytest.mark.asyncio
async def test_active_run_query_only_blocks_queued_and_running() -> None:
    repo = RecordingAgentTraceRepository()

    await repo.has_active_evaluator_run_for_trace(
        tenant_id="tenant-a",
        evaluator_id="11111111-1111-4111-8111-111111111111",
        trace_id="22222222-2222-4222-8222-222222222222",
    )

    query = repo.queries[0][1]
    assert "status IN ('queued', 'running')" in query
    assert "'succeeded'" not in query


@pytest.mark.asyncio
async def test_kb_ragas_summary_uses_only_pass_fail_rows_for_valid_scores() -> None:
    repo = RecordingAgentTraceRepository(
        fetchrow_results=[
            {"rag_traces": 3, "ragas_scored_traces": 2},
            {"judge_model": "qwen-test"},
        ],
        fetch_results=[
            [
                {
                    "metric": "context_relevancy",
                    "average_score": 0.8,
                    "scored_count": 2,
                    "pass_count": 1,
                    "fail_count": 1,
                    "review_count": 1,
                }
            ]
        ],
    )

    summary = await repo.get_kb_ragas_summary(tenant_id="tenant-a")

    metric_query = next(query for kind, query in repo.queries if kind == "fetch")
    assert "FILTER (WHERE s.label IN ('pass', 'fail'))" in metric_query
    assert summary["metrics"][0]["average_score"] == 0.8
    assert summary["metrics"][0]["scored_count"] == 2
    assert summary["metrics"][0]["review_count"] == 1


@pytest.mark.asyncio
async def test_kb_ragas_summary_queries_share_latest_revision_rule() -> None:
    repo = RecordingAgentTraceRepository(
        fetchrow_results=[
            {"rag_traces": 1, "ragas_scored_traces": 1},
            {"judge_model": "qwen-test"},
        ],
        fetch_results=[[]],
    )

    await repo.get_kb_ragas_summary(tenant_id="tenant-a")

    normalized_queries = [" ".join(query.split()) for _kind, query in repo.queries]
    partition = (
        "PARTITION BY s.trace_id, s.evaluator_id, "
        "COALESCE(s.evaluator_version, ''), s.score_name"
    )
    for query in normalized_queries:
        assert "latest_scores AS" in query
        assert "INNER JOIN in_window_traces t ON t.trace_id = s.trace_id" in query
        assert partition in query
        assert "ORDER BY s.created_at DESC, s.score_id DESC" in query
        assert "score_revision = 1" in query
        assert "FROM latest_scores s" in query


@pytest.mark.asyncio
async def test_latest_numeric_revision_replaces_older_score_in_summary() -> None:
    repo = RevisionContractRepository(
        [
            {
                "trace_id": "trace-1",
                "evaluator_id": "eval-1",
                "evaluator_version": "v1",
                "score_name": "context_relevancy",
                "numeric_value": 0.2,
                "label": "fail",
                "judge_model": "qwen-old",
                "created_at": 1,
                "score_id": "score-1",
            },
            {
                "trace_id": "trace-1",
                "evaluator_id": "eval-1",
                "evaluator_version": "v1",
                "score_name": "context_relevancy",
                "numeric_value": 0.8,
                "label": "pass",
                "judge_model": "qwen-new",
                "created_at": 2,
                "score_id": "score-2",
            },
        ]
    )

    summary = await repo.get_kb_ragas_summary(tenant_id="tenant-a")

    assert summary["ragas_scored_traces"] == 1
    assert summary["metrics"] == [
        {
            "metric": "context_relevancy",
            "average_score": 0.8,
            "scored_count": 1,
            "pass_count": 1,
            "fail_count": 0,
            "review_count": 0,
        }
    ]
    assert summary["latest_judge_model"] == "qwen-new"


@pytest.mark.asyncio
async def test_latest_review_revision_replaces_older_pass_in_summary() -> None:
    repo = RevisionContractRepository(
        [
            {
                "trace_id": "trace-1",
                "evaluator_id": "eval-1",
                "evaluator_version": "v1",
                "score_name": "context_precision",
                "numeric_value": 0.8,
                "label": "pass",
                "judge_model": "qwen-old",
                "created_at": 1,
                "score_id": "score-1",
            },
            {
                "trace_id": "trace-1",
                "evaluator_id": "eval-1",
                "evaluator_version": "v1",
                "score_name": "context_precision",
                "numeric_value": 0.0,
                "label": "review",
                "judge_model": "qwen-new",
                "created_at": 2,
                "score_id": "score-2",
            },
        ]
    )

    summary = await repo.get_kb_ragas_summary(tenant_id="tenant-a")

    assert summary["ragas_scored_traces"] == 0
    assert summary["metrics"][0]["average_score"] == 0.0
    assert summary["metrics"][0]["scored_count"] == 0
    assert summary["metrics"][0]["review_count"] == 1


@pytest.mark.asyncio
async def test_score_retrieval_with_kb_ragas_maps_client_results() -> None:
    class _Client:
        async def evaluate_retrieval(self, **kwargs: Any) -> list[Any]:
            assert kwargs["query"] == "refund policy"
            assert kwargs["answer"] == "Refunds are available for 30 days."
            assert kwargs["contexts"] == ["chunk"]
            from src.services.eval.kb_ragas_client import KbRagasMetricResult

            return [
                KbRagasMetricResult(
                    metric="context_relevancy",
                    score=0.88,
                    explanation="Relevant",
                    label="pass",
                    judge_model="qwen-test",
                )
            ]

    payload = await score_retrieval_with_kb_ragas(
        query="refund policy",
        answer="Refunds are available for 30 days.",
        contexts=["chunk"],
        client=_Client(),  # type: ignore[arg-type]
    )

    assert payload["judge_model"] == "qwen-test"
    assert payload["results"][0]["metric"] == "context_relevancy"
    assert payload["results"][0]["score"] == 0.88
