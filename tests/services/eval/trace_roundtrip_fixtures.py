"""Canonical Runtime V2 event fixtures for trace-ingest contract tests."""

from __future__ import annotations

import time
import uuid
from typing import Any

from src.services.eval.langgraph_trace_capture import build_langgraph_trace_payload
from src.services.eval.rag_trace_capture import build_rag_trace_payload
from tests.services.eval.in_memory_trace_repository import InMemoryTraceRepository


def _runtime_trace(*, request_id: str, started: float, ended: float) -> dict[str, Any]:
    trace_id = str(uuid.uuid4())
    run_id = str(uuid.uuid4())
    lifecycle_span_id = str(uuid.uuid5(uuid.UUID(trace_id), "lifecycle"))
    tool_span_id = str(uuid.uuid5(uuid.UUID(trace_id), "tool-1"))
    return {
        "trace_id": trace_id,
        "trace_family": "assistant",
        "workflow_kind": "agent_runtime_v2",
        "tenant_id": "tenant-a",
        "user_id": "user-a",
        "session_id": "session-a",
        "run_id": run_id,
        "request_id": request_id,
        "otel_trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
        "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        "model_id": "test-model",
        "provider": "test-provider",
        "status": "succeeded",
        "started_at": started,
        "ended_at": ended,
        "source_adapter": "agent-runtime-v2",
        "events": [
            {"event_type": "run_started", "sequence_no": 1, "payload": {"run_id": run_id}},
            {
                "event_type": "tool_call_start",
                "sequence_no": 2,
                "payload": {"call_id": "tool-1", "tool_name": "search_knowledge_base"},
            },
            {
                "event_type": "tool_call_result",
                "sequence_no": 3,
                "payload": {"call_id": "tool-1", "status": "succeeded"},
            },
            {
                "event_type": "tool_call_end",
                "sequence_no": 4,
                "payload": {"call_id": "tool-1", "status": "succeeded"},
            },
            {"event_type": "run_finished", "sequence_no": 5, "payload": {"run_id": run_id}},
        ],
        # Runtime V2 carries both the ordered event envelope and its Eval span
        # projection.  Keep the projection explicit in this fixture: ingest
        # persists spans as supplied and must not infer hierarchy from event
        # names alone.
        "spans": [
            {
                "span_id": lifecycle_span_id,
                "parent_span_id": None,
                "span_kind": "lifecycle",
                "name": "agent_runtime_turn",
                "status": "succeeded",
                "sequence_no": 1,
                "started_at": started,
                "ended_at": ended,
                "duration_ms": 80,
            },
            {
                "span_id": tool_span_id,
                "parent_span_id": lifecycle_span_id,
                "span_kind": "tool_execution",
                "name": "search_knowledge_base",
                "status": "succeeded",
                "sequence_no": 2,
                "started_at": started,
                "ended_at": ended,
                "duration_ms": 80,
                "attributes": {"call_id": "tool-1"},
            },
        ],
        "metrics": {"total_latency_ms": 80, "input_tokens": 10, "output_tokens": 20},
    }


async def seed_family(
    repo: InMemoryTraceRepository,
    family: str,
    *,
    request_suffix: str = "default",
) -> str:
    """Seed Runtime V2, LangGraph, or RAG trace data into the ingest contract."""
    started = time.time()
    ended = started + 0.08
    request_id = f"roundtrip-{family}-{request_suffix}"
    if family == "assistant":
        trace = _runtime_trace(request_id=request_id, started=started, ended=ended)
    elif family == "langgraph_proxy":
        trace = build_langgraph_trace_payload(
            request_id=request_id,
            tenant_id="tenant-a",
            user_id="user-a",
            method="POST",
            upstream_path="/threads/t-1/runs/r-1",
            started_at=started,
            ended_at=ended,
            status="succeeded",
            upstream_status=200,
            error_summary=None,
            traceparent="00-abc123def456789012345678901234-0123456789abcdef-01",
            streaming=False,
        )
    elif family == "rag":
        trace = build_rag_trace_payload(
            request_id=request_id,
            tenant_id="tenant-a",
            user_id="user-a",
            dataset_id="ds-1",
            query="refund policy",
            started_at=started,
            ended_at=ended,
            status="succeeded",
            upstream_status=200,
            document_count=3,
            error_summary=None,
            traceparent=None,
        )
    else:
        raise ValueError(f"unsupported trace family: {family}")
    await repo.ingest_trace(
        tenant_id="tenant-a", created_by="user-a", payload={"trace": trace}, enqueue=False
    )
    return str(trace["trace_id"])
