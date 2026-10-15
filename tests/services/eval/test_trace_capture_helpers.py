from __future__ import annotations

import time
from types import SimpleNamespace
from typing import Any

import pytest

from src.services.eval.langgraph_trace_capture import build_langgraph_trace_payload
from src.services.eval.rag_trace_capture import (
    build_rag_trace_payload,
    build_retrieval_documents,
    is_retrieve_path,
    parse_retrieve_document_count,
    parse_retrieve_request_body,
    parse_retrieve_response,
    record_rag_retrieval_trace,
)
from src.services.eval.trace_capture import schedule_gateway_trace_ingest


def test_is_retrieve_path_matches_knowledge_retrieve_routes() -> None:
    assert is_retrieve_path("dataset-1/retrieve")
    assert is_retrieve_path("knowledge/dataset-1/retrieve")
    assert is_retrieve_path("dataset-1/hit_test")
    assert is_retrieve_path("dataset-1/qa")
    assert is_retrieve_path("dataset-1/qa/stream")
    assert not is_retrieve_path("dataset-1/documents")


def test_build_langgraph_trace_payload_extracts_thread_and_run_ids() -> None:
    started = time.time()
    payload = build_langgraph_trace_payload(
        request_id="req-1",
        tenant_id="tenant-a",
        user_id="user-a",
        method="POST",
        upstream_path="/threads/thread-1/runs/run-1/stream",
        started_at=started,
        ended_at=started + 0.25,
        status="succeeded",
        upstream_status=200,
        error_summary=None,
        traceparent="00-abc123def456789012345678901234-0123456789abcdef-01",
        streaming=True,
    )

    assert payload["trace_family"] == "langgraph_proxy"
    assert payload["thread_id"] == "thread-1"
    assert payload["run_id"] == "run-1"
    assert payload["otel_trace_id"] == "abc123def456789012345678901234"
    assert payload["spans"][0]["span_kind"] == "lifecycle"
    assert payload["events"][0]["event_type"] == "proxy_request_accepted"


@pytest.mark.parametrize(
    ("upstream_path", "thread_id", "run_id", "assistant_id", "streaming"),
    [
        ("/assistants/assistant-1/runs/stream", None, None, "assistant-1", True),
        ("/threads/thread-1/runs/stream", "thread-1", None, None, True),
        ("/threads/thread-1/runs/run-1/stream", "thread-1", "run-1", None, True),
        ("/threads/thread-1/runs/run-1/wait", "thread-1", "run-1", None, False),
        ("/threads/thread-1/runs/run-1/cancel", "thread-1", "run-1", None, False),
    ],
)
def test_langgraph_trace_payload_route_matrix(
    upstream_path: str,
    thread_id: str | None,
    run_id: str | None,
    assistant_id: str | None,
    streaming: bool,
) -> None:
    started = time.time()
    payload = build_langgraph_trace_payload(
        request_id=f"req-{upstream_path}",
        tenant_id="tenant-a",
        user_id="user-a",
        method="POST",
        upstream_path=upstream_path,
        started_at=started,
        ended_at=started + 0.05,
        status="succeeded",
        upstream_status=200,
        error_summary=None,
        traceparent=None,
        streaming=streaming,
    )

    assert payload["trace_family"] == "langgraph_proxy"
    assert payload["thread_id"] == thread_id
    assert payload["session_id"] == thread_id
    assert payload["run_id"] == run_id
    assert payload["metadata"]["assistant_id"] == assistant_id
    assert payload["metrics"]["streaming"] is streaming


def test_build_rag_trace_payload_parses_query_and_document_count() -> None:
    started = time.time()
    payload = build_rag_trace_payload(
        request_id="req-2",
        tenant_id="tenant-a",
        user_id="user-a",
        dataset_id="dataset-9",
        query="refund policy",
        started_at=started,
        ended_at=started + 0.1,
        status="succeeded",
        upstream_status=200,
        document_count=2,
        error_summary=None,
        traceparent=None,
    )

    assert payload["trace_family"] == "rag"
    assert payload["metadata"]["dataset_id"] == "dataset-9"
    assert payload["metadata"]["answer_source"] == "retrieval_only"
    assert payload["spans"][1]["span_kind"] == "retriever"
    assert payload["spans"][1]["parent_span_id"] == payload["spans"][0]["span_id"]
    assert payload["events"][1]["event_type"] == "rag_retrieval_completed"


def test_langgraph_child_spans_reference_lifecycle_parent() -> None:
    started = time.time()
    payload = build_langgraph_trace_payload(
        request_id="req-lg",
        tenant_id="tenant-a",
        user_id="user-a",
        method="POST",
        upstream_path="/threads/t-1/runs/r-1",
        started_at=started,
        ended_at=started + 0.05,
        status="succeeded",
        upstream_status=200,
        error_summary=None,
        traceparent=None,
        streaming=False,
    )
    lifecycle = payload["spans"][0]
    child = payload["spans"][1]
    assert lifecycle["parent_span_id"] is None
    assert child["parent_span_id"] == lifecycle["span_id"]
    assert child["span_kind"] == "gateway_proxy"


def test_parse_retrieve_helpers() -> None:
    assert parse_retrieve_request_body(b'{"query":"hello"}') == "hello"
    assert parse_retrieve_document_count(b'{"results":[{"id":1},{"id":2}]}') == 2


def test_parse_retrieve_response_and_build_retrieval_documents() -> None:
    body = b'{"results":[{"text":"Refund within 30 days","score":0.9},{"content":"Support email"}]}'
    results, metadata = parse_retrieve_response(body)
    documents = build_retrieval_documents(results)

    assert len(results) == 2
    assert isinstance(metadata, dict)
    assert len(documents) == 2
    assert documents[0]["content_eval"] == "Refund within 30 days"
    assert "content_preview" in documents[0]


def test_parse_qa_sse_response_preserves_backend_trace() -> None:
    trace_id = "d04d53c8-acde-49d0-b3eb-49890dbd5673"
    body = (
        'data: {"event":"done","data":{"result":'
        '{"context_segments":[{"text":"answer context"}],'
        f'"retrieval_metadata":{{}},"trace_id":"{trace_id}",'
        '"query_fingerprint":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}}\n\n'
    ).encode()
    results, metadata = parse_retrieve_response(body)
    assert results[0]["text"] == "answer context"
    assert metadata["trace_id"] == trace_id


class _IngestRepository:
    def __init__(self) -> None:
        self.calls: list[dict[str, Any]] = []

    async def ingest_trace(self, **kwargs: Any) -> dict[str, Any]:
        self.calls.append(kwargs)
        return {"trace_id": "trace-1", "status": "stored", "job_id": None}


@pytest.mark.asyncio
async def test_schedule_gateway_trace_ingest_runs_in_background(monkeypatch) -> None:
    import asyncio

    repo = _IngestRepository()
    database = SimpleNamespace(enabled=True)
    monkeypatch.setattr(
        "src.services.eval.trace_capture.AgentTraceRepository",
        lambda _database: repo,
    )

    schedule_gateway_trace_ingest(
        database,
        tenant_id="tenant-a",
        created_by="user-a",
        trace={"trace_family": "rag", "trace_id": "trace-1"},
        retention_days=30,
    )
    for _ in range(20):
        if repo.calls:
            break
        await asyncio.sleep(0.01)

    assert len(repo.calls) == 1
    assert repo.calls[0]["enqueue"] is False
    assert "retention_expires_at" in repo.calls[0]["payload"]["trace"]


def test_schedule_gateway_trace_ingest_skips_disabled_database() -> None:
    repo = _IngestRepository()
    schedule_gateway_trace_ingest(
        SimpleNamespace(enabled=False),
        tenant_id="tenant-a",
        created_by="user-a",
        trace={"trace_family": "rag", "trace_id": "trace-1"},
    )
    assert repo.calls == []


def test_record_rag_retrieval_trace_schedules_payload(monkeypatch) -> None:
    scheduled: list[dict[str, Any]] = []

    def _capture(_database: Any, **kwargs: Any) -> None:
        scheduled.append(kwargs)

    monkeypatch.setattr(
        "src.services.eval.rag_trace_capture.schedule_gateway_trace_ingest",
        _capture,
    )
    record_rag_retrieval_trace(
        SimpleNamespace(enabled=True),
        tenant_id="tenant-a",
        user_id="user-a",
        request_id="req-4",
        path="dataset-1/retrieve",
        body=b'{"query":"hello"}',
        response_status=200,
        response_body=(
            b'{"trace_id":"d04d53c8-acde-49d0-b3eb-49890dbd5673",'
            b'"results":[{"text":"hello chunk"}]}'
        ),
        started_at=time.time(),
    )
    assert len(scheduled) == 1
    assert scheduled[0]["enqueue"] is True
    assert scheduled[0]["trace"]["trace_family"] == "rag"
    assert scheduled[0]["trace"]["trace_id"] == "d04d53c8-acde-49d0-b3eb-49890dbd5673"
    assert scheduled[0]["trace"]["metadata"]["dataset_id"] == "dataset-1"
    retriever_span = next(
        span for span in scheduled[0]["trace"]["spans"] if span["span_kind"] == "retriever"
    )
    assert retriever_span["attributes"]["retrieval"]["documents"]


def test_record_rag_retrieval_trace_extracts_dataset_from_full_knowledge_path(monkeypatch) -> None:
    scheduled: list[dict[str, Any]] = []

    def _capture(_database: Any, **kwargs: Any) -> None:
        scheduled.append(kwargs)

    monkeypatch.setattr(
        "src.services.eval.rag_trace_capture.schedule_gateway_trace_ingest",
        _capture,
    )
    record_rag_retrieval_trace(
        SimpleNamespace(enabled=True),
        tenant_id="tenant-a",
        user_id="user-a",
        request_id="req-5",
        path="/api/v1/knowledge/dataset-1/retrieve",
        body=b'{"query":"hello"}',
        response_status=200,
        response_body=b'{"results":[{"text":"hello chunk"}]}',
        started_at=time.time(),
    )

    assert scheduled[0]["trace"]["metadata"]["dataset_id"] == "dataset-1"
