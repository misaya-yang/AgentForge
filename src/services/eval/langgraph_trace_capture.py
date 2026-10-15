"""Capture LangGraph proxy passthrough requests as langgraph_proxy traces."""

from __future__ import annotations

import time
import uuid
from datetime import datetime, timezone
from typing import Any

from .trace_capture import (
    redact_preview,
    schedule_gateway_trace_ingest,
    span_id_for,
    trace_id_for_request,
)


def _parse_langgraph_ids(upstream_path: str) -> dict[str, str | None]:
    parts = [part for part in upstream_path.split("/") if part]
    thread_id: str | None = None
    run_id: str | None = None
    assistant_id: str | None = None
    if len(parts) >= 2 and parts[0] == "threads" and parts[1] not in {"search", "count"}:
        thread_id = parts[1]
    if len(parts) >= 2 and parts[0] == "assistants" and parts[1] not in {"search", "count"}:
        assistant_id = parts[1]
    if "runs" in parts:
        index = parts.index("runs")
        if index + 1 < len(parts) and parts[index + 1] not in {"stream", "wait"}:
            run_id = parts[index + 1]
    return {
        "thread_id": thread_id,
        "run_id": run_id,
        "assistant_id": assistant_id,
    }


def build_langgraph_trace_payload(
    *,
    request_id: str,
    tenant_id: str,
    user_id: str,
    method: str,
    upstream_path: str,
    started_at: float,
    ended_at: float,
    status: str,
    upstream_status: int | None,
    error_summary: str | None,
    traceparent: str | None,
    streaming: bool,
) -> dict[str, Any]:
    ids = _parse_langgraph_ids(upstream_path)
    trace_id = trace_id_for_request(
        request_id=request_id or str(uuid.uuid4()),
        trace_family="langgraph_proxy",
        route_key=f"{method}:{upstream_path}",
    )
    lifecycle_span_id = span_id_for(trace_id, "lifecycle")
    started_dt = datetime.fromtimestamp(started_at, tz=timezone.utc)
    ended_dt = datetime.fromtimestamp(ended_at, tz=timezone.utc)
    duration_ms = max(0, int((ended_at - started_at) * 1000))
    otel_trace_id = None
    if traceparent and traceparent.startswith("00-"):
        parts = traceparent.split("-")
        if len(parts) >= 2 and parts[1]:
            otel_trace_id = parts[1]
    return {
        "trace_id": trace_id,
        "trace_family": "langgraph_proxy",
        "workflow_kind": "langgraph_agent_run",
        "tenant_id": tenant_id,
        "user_id": user_id,
        "thread_id": ids.get("thread_id"),
        "session_id": ids.get("thread_id"),
        "run_id": ids.get("run_id"),
        "request_id": request_id,
        "otel_trace_id": otel_trace_id,
        "traceparent": traceparent,
        "status": status,
        "started_at": started_dt.isoformat(),
        "ended_at": ended_dt.isoformat(),
        "input_preview": redact_preview(f"{method} {upstream_path}"),
        "output_preview": redact_preview(
            f"upstream_status={upstream_status or 'n/a'} streaming={streaming}"
        ),
        "metrics": {
            "total_latency_ms": duration_ms,
            "upstream_status": upstream_status,
            "streaming": streaming,
        },
        "privacy": {"payloads": "bounded_redacted_preview"},
        "redaction_state": {"headers": "stripped", "body": "not_persisted"},
        "metadata": {
            "upstream_route": upstream_path,
            "http_method": method,
            "assistant_id": ids.get("assistant_id"),
            "error_summary": redact_preview(error_summary) if error_summary else None,
        },
        "source_adapter": "gateway.langgraph_proxy",
        "spans": [
            {
                "span_id": lifecycle_span_id,
                "parent_span_id": None,
                "span_kind": "lifecycle",
                "name": "langgraph_proxy_run",
                "status": status,
                "sequence_no": 0,
                "started_at": started_dt.isoformat(),
                "ended_at": ended_dt.isoformat(),
                "duration_ms": duration_ms,
                "attributes": {
                    "upstream_route": upstream_path,
                    "http_method": method,
                    "upstream_status": upstream_status,
                    "streaming": streaming,
                },
            },
            {
                "span_id": span_id_for(trace_id, "gateway_proxy"),
                "parent_span_id": lifecycle_span_id,
                "span_kind": "gateway_proxy",
                "name": "upstream_request",
                "status": status,
                "sequence_no": 1,
                "started_at": started_dt.isoformat(),
                "ended_at": ended_dt.isoformat(),
                "duration_ms": duration_ms,
                "attributes": {
                    "thread_id": ids.get("thread_id"),
                    "run_id": ids.get("run_id"),
                    "assistant_id": ids.get("assistant_id"),
                },
                "error_message": redact_preview(error_summary) if error_summary else None,
            },
        ],
        "events": [
            {
                "event_type": "proxy_request_accepted",
                "sequence_no": 1,
                "payload": {
                    "method": method,
                    "upstream_path": upstream_path,
                    "streaming": streaming,
                },
            },
            {
                "event_type": "proxy_request_finished",
                "sequence_no": 2,
                "payload": {
                    "status": status,
                    "upstream_status": upstream_status,
                    "duration_ms": duration_ms,
                },
            },
        ],
    }


def record_langgraph_proxy_trace(
    database: Any,
    *,
    tenant_id: str,
    user_id: str,
    request_id: str,
    method: str,
    upstream_path: str,
    started_at: float,
    status: str,
    upstream_status: int | None = None,
    error_summary: str | None = None,
    traceparent: str | None = None,
    streaming: bool = False,
    retention_days: int = 90,
) -> None:
    payload = build_langgraph_trace_payload(
        request_id=request_id,
        tenant_id=tenant_id,
        user_id=user_id,
        method=method,
        upstream_path=upstream_path,
        started_at=started_at,
        ended_at=time.time(),
        status=status,
        upstream_status=upstream_status,
        error_summary=error_summary,
        traceparent=traceparent,
        streaming=streaming,
    )
    schedule_gateway_trace_ingest(
        database,
        tenant_id=tenant_id,
        created_by=user_id,
        trace=payload,
        retention_days=retention_days,
    )
