"""Authorized, idempotent Trace imports that preserve manual review."""
from __future__ import annotations

import uuid
from typing import Any


async def import_trace_example(
    repository: Any, *, tenant_id: str, dataset_id: str, created_by: str,
    payload: dict[str, Any], user_id: str | None, trace_family: str,
) -> dict[str, Any] | None:
    detail = await repository.get_trace_detail(
        tenant_id=tenant_id, trace_id=payload["source_trace_id"],
        user_id=user_id, trace_family=trace_family,
    )
    if not detail:
        return None
    source_id = str(uuid.UUID(payload["source_trace_id"]))
    span_id = payload.get("source_span_id")
    spans = detail.get("spans") if isinstance(detail.get("spans"), list) else []
    if span_id and not any(str(span.get("span_id")) == str(span_id) for span in spans if isinstance(span, dict)):
        return None
    existing = await repository.fetchrow(
        """SELECT * FROM eval_examples
           WHERE tenant_id=$1 AND dataset_id=$2::uuid AND source_trace_id=$3::uuid
             AND source_span_id IS NOT DISTINCT FROM $4::uuid
           ORDER BY created_at, example_id LIMIT 1""",
        tenant_id, dataset_id, source_id, span_id,
    )
    if existing:
        return repository._decode_eval_row(existing)
    trace = detail["trace"]
    input_preview = str(trace.get("input_preview") or "")
    input_payload = {
        "message": input_preview, "input_preview": input_preview,
        "thread_id": trace.get("thread_id") or trace.get("session_id"),
        "run_id": trace.get("run_id"), "request_id": trace.get("request_id"),
        "metadata": trace.get("metadata") or {},
    }
    expected_output = payload.get("expected_output") or {"output_preview": trace.get("output_preview") or ""}
    metadata = dict(payload.get("metadata") or {})
    trace_metadata = trace.get("metadata") if isinstance(trace.get("metadata"), dict) else {}
    runtime = trace_metadata.get("runtime_trajectory")
    runtime = runtime if isinstance(runtime, dict) else {}
    span_kinds = sorted({str(span["span_kind"]) for span in spans if isinstance(span, dict) and span.get("span_kind")})
    metadata.setdefault("expected_trajectory", {
        "required_span_kinds": span_kinds,
        "runtime": {"expected_exit_reason": runtime.get("exit_reason") or trace.get("status")},
    })
    metadata.setdefault("assertions", [{"type": "no_sensitive_output"}])
    metadata["behavior_confirmed"] = False
    metadata["review_status"] = "pending"
    origin = repository._json_dumps([
        tenant_id, str(uuid.UUID(dataset_id)), trace_family, source_id,
        str(uuid.UUID(span_id)) if span_id else None,
    ])
    example_id = str(uuid.uuid5(uuid.NAMESPACE_URL, "ai-gateway/trace-example/" + origin))
    row = await repository.fetchrow(
        """INSERT INTO eval_examples (
            dataset_id, tenant_id, split, input, expected_output, metadata,
            source_trace_id, source_span_id, created_by, example_id
        ) VALUES ($1::uuid, $2, $3, $4::jsonb, $5::jsonb, $6::jsonb,
                  $7::uuid, $8::uuid, $9, $10::uuid)
        ON CONFLICT (example_id) DO NOTHING RETURNING *""",
        dataset_id, tenant_id, payload.get("split") or "regression",
        repository._json_dumps(input_payload), repository._json_dumps(expected_output),
        repository._json_dumps(metadata), source_id, span_id, created_by, example_id,
    )
    if row is None:
        row = await repository.fetchrow(
            """SELECT * FROM eval_examples WHERE example_id=$1::uuid
               AND tenant_id=$2 AND dataset_id=$3::uuid AND source_trace_id=$4::uuid
               AND source_span_id IS NOT DISTINCT FROM $5::uuid""",
            example_id, tenant_id, dataset_id, source_id, span_id,
        )
    return repository._decode_eval_row(row) if row else None
