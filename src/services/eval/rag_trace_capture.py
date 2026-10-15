"""Capture knowledge-service retrieval proxy calls as rag family traces."""

from __future__ import annotations

import json
import os
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

_DEFAULT_EVAL_CONTEXT_MAX_CHARS = 1500
_DEFAULT_EVAL_CONTEXT_MAX_CHUNKS = 8
_DEFAULT_UI_PREVIEW_MAX_CHARS = 360


def _read_int_env(name: str, default: int) -> int:
    raw = os.getenv(name, "").strip()
    if not raw:
        return default
    try:
        return max(1, int(raw))
    except ValueError:
        return default


def eval_context_max_chars() -> int:
    return _read_int_env("KB_TRACE_EVAL_CONTEXT_MAX_CHARS", _DEFAULT_EVAL_CONTEXT_MAX_CHARS)


def eval_context_max_chunks() -> int:
    return _read_int_env("KB_TRACE_EVAL_MAX_CHUNKS", _DEFAULT_EVAL_CONTEXT_MAX_CHUNKS)


def _parse_dataset_id(path: str) -> str | None:
    parts = [part for part in path.strip("/").split("/") if part]
    if not parts:
        return None
    if "knowledge" in parts:
        index = parts.index("knowledge")
        if len(parts) > index + 1:
            return parts[index + 1]
    if len(parts) >= 3 and parts[-2:] == ["qa", "stream"]:
        return parts[-3]
    if parts[-1] in {"retrieve", "retrieve_batch", "hit_test", "qa"} and len(parts) >= 2:
        return parts[-2]
    if len(parts) == 1 and parts[0] != "retrieve":
        return parts[0]
    return None


def is_retrieve_path(path: str) -> bool:
    normalized = path.strip("/")
    return any(
        normalized.endswith(suffix)
        for suffix in (
            "retrieve",
            "retrieve_batch",
            "hit_test",
            "qa",
            "qa/stream",
        )
    )


def _bounded_eval_text(value: Any, *, limit: int | None = None) -> str:
    return redact_preview(value, limit=limit or eval_context_max_chars())


def _bounded_ui_preview(value: Any) -> str:
    return redact_preview(value, limit=_DEFAULT_UI_PREVIEW_MAX_CHARS)


def build_retrieval_documents(
    results: list[dict[str, Any]],
    *,
    max_chunks: int | None = None,
    eval_max_chars: int | None = None,
) -> list[dict[str, Any]]:
    chunk_limit = max_chunks or eval_context_max_chunks()
    char_limit = eval_max_chars or eval_context_max_chars()
    documents: list[dict[str, Any]] = []
    for index, result in enumerate(results[:chunk_limit], start=1):
        if not isinstance(result, dict):
            continue
        text = str(result.get("text") or result.get("content") or "").strip()
        if not text:
            continue
        documents.append(
            {
                "rank": index,
                "segment_id": result.get("segment_id"),
                "document_id": result.get("document_id"),
                "score": result.get("score"),
                "content_eval": _bounded_eval_text(text, limit=char_limit),
                "content_preview": _bounded_ui_preview(text),
            }
        )
    return documents


def parse_retrieve_response(response_body: bytes) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    if not response_body:
        return [], {}
    try:
        payload = json.loads(response_body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        # QA streaming responses contain one JSON payload per SSE data line.
        terminal: dict[str, Any] | None = None
        retrieval: dict[str, Any] | None = None
        for raw_line in response_body.decode("utf-8", errors="ignore").splitlines():
            if not raw_line.startswith("data:"):
                continue
            try:
                event = json.loads(raw_line[len("data:") :].strip())
            except json.JSONDecodeError:
                continue
            if not isinstance(event, dict):
                continue
            data = event.get("data") if isinstance(event.get("data"), dict) else {}
            if event.get("event") == "done" and isinstance(data.get("result"), dict):
                terminal = data["result"]
            elif event.get("event") == "retrieval":
                retrieval = data
        selected = terminal or retrieval
        if selected is None:
            return [], {}
        return parse_retrieve_response(json.dumps(selected).encode("utf-8"))
    if not isinstance(payload, dict):
        if isinstance(payload, list):
            return [item for item in payload if isinstance(item, dict)], {}
        return [], {}

    results = payload.get("results")
    if not isinstance(results, list) and isinstance(payload.get("batch_results"), list):
        first = next(
            (item for item in payload["batch_results"] if isinstance(item, dict)),
            {},
        )
        results = first.get("results")
        if not isinstance(payload.get("metadata"), dict):
            payload["metadata"] = first.get("meta")
    if not isinstance(results, list) and isinstance(payload.get("context_segments"), list):
        results = payload.get("context_segments")
    if not isinstance(results, list):
        chunks = payload.get("chunks")
        results = chunks if isinstance(chunks, list) else []
    metadata = payload.get("metadata")
    if not isinstance(metadata, dict):
        metadata = payload.get("retrieval_metadata")
    metadata = dict(metadata) if isinstance(metadata, dict) else {}
    for key in ("trace_id", "query_fingerprint"):
        if payload.get(key):
            metadata[key] = payload[key]
    return [item for item in results if isinstance(item, dict)], metadata


def build_rag_trace_payload(
    *,
    request_id: str,
    tenant_id: str,
    user_id: str,
    dataset_id: str | None,
    query: str,
    started_at: float,
    ended_at: float,
    status: str,
    upstream_status: int | None,
    document_count: int,
    error_summary: str | None,
    traceparent: str | None,
    retrieval_documents: list[dict[str, Any]] | None = None,
    retrieval_metadata: dict[str, Any] | None = None,
    trace_id: str | None = None,
) -> dict[str, Any]:
    try:
        trace_id = str(uuid.UUID(str(trace_id))) if trace_id else None
    except ValueError:
        trace_id = None
    trace_id = trace_id or trace_id_for_request(
        request_id=request_id or str(uuid.uuid4()),
        trace_family="rag",
        route_key=f"retrieve:{dataset_id or 'unknown'}:{query[:80]}",
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

    documents = list(retrieval_documents or [])
    effective_document_count = document_count or len(documents)
    retrieval_meta = dict(retrieval_metadata or {})
    retrieval_mode = retrieval_meta.get("mode")

    retriever_attributes: dict[str, Any] = {
        "openinference.span.kind": "RETRIEVER",
        "retrieval.dataset_ids": [dataset_id] if dataset_id else [],
        "retrieval.document_count": effective_document_count,
        "retrieval": {
            "documents": documents,
            "mode": retrieval_mode,
            "pipeline_stages": retrieval_meta.get("pipeline_stages"),
        },
    }

    trace_metadata: dict[str, Any] = {
        "dataset_id": dataset_id,
        "answer_source": "retrieval_only",
        "gen_ai.retrieval.query.text": redact_preview(query),
        "retrieval.dataset_ids": [dataset_id] if dataset_id else [],
        "error_summary": redact_preview(error_summary) if error_summary else None,
        "retrieval": {
            "mode": retrieval_mode,
            "pipeline_stages": retrieval_meta.get("pipeline_stages"),
            "fusion_method": retrieval_meta.get("fusion_method"),
            "rerank": retrieval_meta.get("rerank"),
        },
    }

    return {
        "trace_id": trace_id,
        "trace_family": "rag",
        "workflow_kind": "rag_retrieval_chain",
        "tenant_id": tenant_id,
        "user_id": user_id,
        "request_id": request_id,
        "otel_trace_id": otel_trace_id,
        "traceparent": traceparent,
        "status": status,
        "started_at": started_dt.isoformat(),
        "ended_at": ended_dt.isoformat(),
        "input_preview": redact_preview(query),
        "output_preview": redact_preview(f"{effective_document_count} retrieved documents"),
        "metrics": {
            "total_latency_ms": duration_ms,
            "retrieval.document_count": effective_document_count,
            "upstream_status": upstream_status,
        },
        "privacy": {"payloads": "bounded_redacted_preview", "eval_contexts": "bounded_eval_only"},
        "redaction_state": {"chunks": "bounded_eval_context"},
        "metadata": trace_metadata,
        "source_adapter": "gateway.knowledge_proxy",
        "spans": [
            {
                "span_id": lifecycle_span_id,
                "parent_span_id": None,
                "span_kind": "lifecycle",
                "name": "rag_retrieval_chain",
                "status": status,
                "sequence_no": 0,
                "started_at": started_dt.isoformat(),
                "ended_at": ended_dt.isoformat(),
                "duration_ms": duration_ms,
            },
            {
                "span_id": span_id_for(trace_id, "retriever"),
                "parent_span_id": lifecycle_span_id,
                "span_kind": "retriever",
                "name": "rag_retrieval",
                "status": status,
                "sequence_no": 1,
                "started_at": started_dt.isoformat(),
                "ended_at": ended_dt.isoformat(),
                "duration_ms": duration_ms,
                "input_preview": redact_preview(query),
                "output_preview": redact_preview(f"{effective_document_count} documents"),
                "attributes": retriever_attributes,
                "error_message": redact_preview(error_summary) if error_summary else None,
            },
        ],
        "events": [
            {
                "event_type": "rag_retrieval_started",
                "sequence_no": 1,
                "payload": {"query": redact_preview(query), "dataset_id": dataset_id},
            },
            {
                "event_type": "rag_retrieval_completed" if status == "succeeded" else "rag_retrieval_failed",
                "sequence_no": 2,
                "payload": {
                    "document_count": effective_document_count,
                    "duration_ms": duration_ms,
                    "error": redact_preview(error_summary) if error_summary else None,
                },
            },
        ],
    }


def parse_retrieve_request_body(body: bytes) -> str:
    if not body:
        return ""
    try:
        payload = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        return ""
    if not isinstance(payload, dict):
        return ""
    return str(payload.get("query") or payload.get("text") or "")


def parse_retrieve_document_count(response_body: bytes) -> int:
    results, _metadata = parse_retrieve_response(response_body)
    return len(results)


def record_rag_retrieval_trace(
    database: Any,
    *,
    tenant_id: str,
    user_id: str,
    request_id: str,
    path: str,
    body: bytes,
    response_status: int,
    response_body: bytes,
    started_at: float,
    traceparent: str | None = None,
    retention_days: int = 90,
) -> None:
    status = "succeeded" if 200 <= response_status < 400 else "failed"
    error_summary = None if status == "succeeded" else f"upstream_status={response_status}"
    results, retrieval_metadata = parse_retrieve_response(response_body)
    documents = build_retrieval_documents(results)
    payload = build_rag_trace_payload(
        request_id=request_id,
        tenant_id=tenant_id,
        user_id=user_id,
        dataset_id=_parse_dataset_id(path),
        query=parse_retrieve_request_body(body),
        started_at=started_at,
        ended_at=time.time(),
        status=status,
        upstream_status=response_status,
        document_count=len(documents),
        error_summary=error_summary,
        traceparent=traceparent,
        retrieval_documents=documents,
        retrieval_metadata=retrieval_metadata,
        trace_id=str(retrieval_metadata.get("trace_id") or "") or None,
    )
    schedule_gateway_trace_ingest(
        database,
        tenant_id=tenant_id,
        created_by=user_id,
        trace=payload,
        retention_days=retention_days,
        enqueue=True,
    )
