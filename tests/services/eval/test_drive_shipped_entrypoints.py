"""Drive shipped eval/trace entry points and emit observable stdout for evidence logs."""

from __future__ import annotations

import json
import time
from typing import Any

import pytest
from ai_gateway_core.eval.evaluator_executor import EvaluatorExecutor
from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository

from src.services.eval.langgraph_trace_capture import build_langgraph_trace_payload
from src.services.eval.rag_trace_capture import build_rag_trace_payload


def _assert_span_tree(payload: dict[str, Any], *, family: str) -> None:
    spans = payload["spans"]
    lifecycle = next(s for s in spans if s.get("parent_span_id") is None)
    children = [s for s in spans if s.get("parent_span_id") is not None]
    print(
        f"DRIVE span_tree family={family} lifecycle={lifecycle['span_id']} "
        f"children={len(children)} otel={payload.get('otel_trace_id')}"
    )
    assert children, f"{family} trace must include child spans"
    assert all(child["parent_span_id"] == lifecycle["span_id"] for child in children)


@pytest.mark.asyncio
async def test_drive_langgraph_and_rag_payload_builders(capsys: pytest.CaptureFixture[str]) -> None:
    started = time.time()
    langgraph = build_langgraph_trace_payload(
        request_id="drive-req-lg",
        tenant_id="tenant-a",
        user_id="user-a",
        method="POST",
        upstream_path="/threads/t-1/runs/r-1",
        started_at=started,
        ended_at=started + 0.05,
        status="succeeded",
        upstream_status=200,
        error_summary=None,
        traceparent="00-abc123def456789012345678901234-0123456789abcdef-01",
        streaming=False,
    )
    _assert_span_tree(langgraph, family="langgraph_proxy")
    assert langgraph["otel_trace_id"] == "abc123def456789012345678901234"

    rag = build_rag_trace_payload(
        request_id="drive-req-rag",
        tenant_id="tenant-a",
        user_id="user-a",
        dataset_id="ds-1",
        query="refund policy",
        started_at=started,
        ended_at=started + 0.02,
        status="succeeded",
        upstream_status=200,
        document_count=3,
        error_summary=None,
        traceparent=None,
    )
    _assert_span_tree(rag, family="rag")
    assert rag["metrics"]["retrieval.document_count"] == 3

    out = capsys.readouterr().out
    print("DRIVE builders stdout captured:")
    print(out)
    assert "DRIVE span_tree family=langgraph_proxy" in out
    assert "DRIVE span_tree family=rag" in out


class _DriveEvalRepo:
    def __init__(self) -> None:
        self.evaluator = {
            "evaluator_id": "eval-drive",
            "name": "quality",
            "evaluator_type": "llm",
            "version": "v1",
            "rubric": "Score grounding.",
            "metadata": {"judge_model_id": "judge-model"},
        }
        self.trace_detail = {
            "trace": {
                "trace_id": "trace-drive",
                "input_preview": "question",
                "output_preview": "grounded answer with enough detail",
                "status": "succeeded",
                "total_latency_ms": 400,
                "model_id": "test-model",
                "metadata": {},
            }
        }
        self.statuses: list[str] = []

    async def update_experiment_run(self, **kwargs: Any) -> None:
        self.statuses.append(str(kwargs.get("status")))

    async def get_evaluator(self, **_kwargs: Any) -> dict[str, Any]:
        return self.evaluator

    async def get_trace_detail(self, **_kwargs: Any) -> dict[str, Any]:
        return self.trace_detail

    async def create_eval_score(self, **kwargs: Any) -> dict[str, Any]:
        return {
            "score_id": "score-drive",
            "numeric_value": kwargs["payload"]["numeric_value"],
            "label": kwargs["payload"].get("label"),
        }


@pytest.mark.asyncio
async def test_drive_evaluator_executor_run_job(capsys: pytest.CaptureFixture[str]) -> None:
    repo = _DriveEvalRepo()

    async def _complete(model_id: str, prompt: str) -> str:
        assert model_id == "judge-model"
        assert "grounded answer" in prompt
        return json.dumps(
            {
                "numeric_value": 0.93,
                "label": "pass",
                "explanation": "grounded",
                "confidence": 0.9,
            }
        )

    executor = EvaluatorExecutor(repo, llm_complete=_complete)
    result = await executor.run_job(
        tenant_id="tenant-a",
        job_payload={
            "run_id": "run-drive",
            "evaluator_id": "eval-drive",
            "trace_id": "trace-drive",
            "target_snapshot": {"trace_family": "assistant"},
        },
    )

    print(
        f"DRIVE executor status={result.status} scores={result.scores_written} "
        f"avg={result.score_summary.get('average_score')} transitions={repo.statuses}"
    )
    assert result.status == "succeeded"
    assert result.scores_written == 1
    assert result.score_summary["average_score"] == 0.93
    assert repo.statuses[0] == "running"
    assert repo.statuses[-1] == "succeeded"

    out = capsys.readouterr().out
    print(out)
    assert "DRIVE executor status=succeeded" in out


class _ListTracesRepo(AgentTraceRepository):
    def __init__(self) -> None:
        self.fetchrow_calls: list[tuple[str, tuple[Any, ...]]] = []
        self.fetch_calls: list[tuple[str, tuple[Any, ...]]] = []

    @property
    def enabled(self) -> bool:
        return True

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        self.fetchrow_calls.append((query, args))
        return {"total": 2}

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        self.fetch_calls.append((query, args))
        return []


@pytest.mark.asyncio
async def test_drive_list_traces_transcript_filter_sql(capsys: pytest.CaptureFixture[str]) -> None:
    repo = _ListTracesRepo()
    rows, total = await repo.list_traces(
        tenant_id="tenant-a",
        trace_family="assistant",
        transcript_query="refund transcript",
        turn_index=2,
        score_name="quality",
        dataset_id="aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    )
    count_query, count_args = repo.fetchrow_calls[-1]
    page_query, _page_args = repo.fetch_calls[-1]

    print(
        f"DRIVE list_traces total={total} rows={len(rows)} "
        f"has_transcript={'transcript_excerpt' in count_query} "
        f"has_dataset={'eval_examples' in count_query}"
    )
    assert total == 2
    assert rows == []
    assert "transcript_excerpt" in count_query
    assert "eval_examples" in count_query
    assert "%refund transcript%" in count_args
    assert "ORDER BY t.created_at DESC" in page_query

    out = capsys.readouterr().out
    print(out)
    assert "DRIVE list_traces total=2" in out
