"""Trace-to-eval feedback helpers.

The first feedback-loop cut is service-level and self-hosted: classify bounded
failure modes, build redacted dataset import items, and keep harness/profile
changes in a proposed state until evaluator gates and review evidence pass.
"""

from __future__ import annotations

import hashlib
from collections import Counter
from dataclasses import dataclass
from typing import Any

from ...api.eval_export import _redact_export_text, _safe_export_value
from .golden import apply_gate

SUPPORTED_TRACE_FAMILIES = {"assistant", "langgraph_proxy", "rag"}

FAILURE_MODE_TOOL_ERROR = "tool_error"
FAILURE_MODE_CONTEXT_OVERFLOW = "context_overflow"
FAILURE_MODE_LOOP_DETECTED = "loop_detected"
FAILURE_MODE_RAG_MISS = "rag_miss"
FAILURE_MODE_APPROVAL_BLOCKED = "approval_blocked"
FAILURE_MODE_MODEL_EMPTY_OUTPUT = "model_empty_output"
FAILURE_MODE_LOW_SCORE = "low_score"
FAILURE_MODE_LATENCY_REGRESSION = "latency_regression"
FAILURE_MODE_UNKNOWN = "unknown_failure"

_CONTROL_METADATA_KEYS = {"raw_input", "raw_output", "messages", "tool_arguments"}
_SECRET_METADATA_KEY_MARKERS = (
    "authorization",
    "api_key",
    "apikey",
    "password",
    "secret",
    "token",
    "cookie",
    "credential",
)


@dataclass(frozen=True)
class TraceFailurePattern:
    trace_id: str
    trace_family: str
    failure_mode: str
    reasons: list[str]
    severity: str = "medium"


def classify_trace_failure(
    detail: dict[str, Any],
    *,
    low_score_threshold: float = 0.75,
    latency_threshold_ms: int = 30_000,
) -> TraceFailurePattern:
    """Classify a trace detail payload into one bounded failure mode."""
    trace = _trace(detail)
    events = _events(detail)
    spans = _spans(detail)
    scores = _scores(detail)
    trace_id = str(trace.get("trace_id") or "")
    trace_family = _supported_family(trace.get("trace_family"))
    status = str(trace.get("status") or "").lower()
    reasons: list[str] = []

    event_types = {str(event.get("event_type") or "") for event in events}
    span_errors = [
        span
        for span in spans
        if span.get("error_type") or span.get("error_message") or span.get("status") == "failed"
    ]

    if status in {"failed", "error"} and (
        "tool_error" in event_types or span_errors or trace.get("error")
    ):
        reasons.append("trace_failed_with_tool_or_span_error")
        return TraceFailurePattern(
            trace_id=trace_id,
            trace_family=trace_family,
            failure_mode=FAILURE_MODE_TOOL_ERROR,
            reasons=reasons,
            severity="high",
        )

    if ("context_budget" in event_types or "context_compacted" in event_types) and (
        _event_payload_contains(events, "dropped_history_messages")
        or _event_payload_contains(events, "context_overflow")
    ):
        reasons.append("context_budget_or_compaction_dropped_context")
        return TraceFailurePattern(
            trace_id=trace_id,
            trace_family=trace_family,
            failure_mode=FAILURE_MODE_CONTEXT_OVERFLOW,
            reasons=reasons,
            severity="high",
        )

    if "loop_detected" in event_types or _repeated_tool_call_count(events) >= 3:
        reasons.append("repeated_tool_call_sequence")
        return TraceFailurePattern(
            trace_id=trace_id,
            trace_family=trace_family,
            failure_mode=FAILURE_MODE_LOOP_DETECTED,
            reasons=reasons,
            severity="high",
        )

    if "approval_required" in event_types and status not in {"succeeded", "success"}:
        reasons.append("approval_required_without_successful_resume")
        return TraceFailurePattern(
            trace_id=trace_id,
            trace_family=trace_family,
            failure_mode=FAILURE_MODE_APPROVAL_BLOCKED,
            reasons=reasons,
        )

    if trace_family == "rag" and (
        status in {"failed", "error"} or _event_payload_contains(events, "no_relevant_chunks")
    ):
        reasons.append("rag_trace_failed_or_no_relevant_chunks")
        return TraceFailurePattern(
            trace_id=trace_id,
            trace_family=trace_family,
            failure_mode=FAILURE_MODE_RAG_MISS,
            reasons=reasons,
        )

    if not str(trace.get("output_preview") or "").strip() or _run_error_mentions(
        events, "model_produced_no_text"
    ):
        reasons.append("empty_output_preview")
        return TraceFailurePattern(
            trace_id=trace_id,
            trace_family=trace_family,
            failure_mode=FAILURE_MODE_MODEL_EMPTY_OUTPUT,
            reasons=reasons,
            severity="high",
        )

    low_scores = [
        score
        for score in scores
        if _numeric_score(score) is not None and _numeric_score(score) < low_score_threshold
    ]
    if low_scores:
        reasons.append("score_below_threshold")
        return TraceFailurePattern(
            trace_id=trace_id,
            trace_family=trace_family,
            failure_mode=FAILURE_MODE_LOW_SCORE,
            reasons=reasons,
        )

    latency_ms = int(trace.get("total_latency_ms") or 0)
    if latency_ms > latency_threshold_ms:
        reasons.append("latency_above_threshold")
        return TraceFailurePattern(
            trace_id=trace_id,
            trace_family=trace_family,
            failure_mode=FAILURE_MODE_LATENCY_REGRESSION,
            reasons=reasons,
        )

    reasons.append("unclassified_failure")
    return TraceFailurePattern(
        trace_id=trace_id,
        trace_family=trace_family,
        failure_mode=FAILURE_MODE_UNKNOWN,
        reasons=reasons,
    )


def cluster_failure_patterns(patterns: list[TraceFailurePattern]) -> list[dict[str, Any]]:
    counts = Counter(pattern.failure_mode for pattern in patterns)
    return [
        {
            "failure_mode": failure_mode,
            "count": count,
            "trace_ids": [
                pattern.trace_id for pattern in patterns if pattern.failure_mode == failure_mode
            ],
        }
        for failure_mode, count in counts.most_common()
    ]


def build_redacted_dataset_case(
    detail: dict[str, Any],
    pattern: TraceFailurePattern | None = None,
    *,
    split: str = "regression",
) -> dict[str, Any]:
    trace = _trace(detail)
    resolved = pattern or classify_trace_failure(detail)
    trace_id = str(trace.get("trace_id") or resolved.trace_id)
    case_hash = hashlib.sha1(f"{resolved.trace_family}:{trace_id}".encode()).hexdigest()[:10]
    metadata = _redacted_metadata(trace.get("metadata") or {})
    return {
        "case_id": f"{resolved.trace_family}-{case_hash}-{resolved.failure_mode}",
        "split": split,
        "input": {
            "input_preview": _redact_export_text(trace.get("input_preview") or ""),
            "trace_family": resolved.trace_family,
            "thread_id": trace.get("thread_id") or trace.get("session_id"),
            "run_id": trace.get("run_id"),
        },
        "expected_output": {
            "output_preview": _redact_export_text(trace.get("output_preview") or ""),
            "expected_status": "succeeded",
        },
        "expected_trajectory": {
            "source_failure_mode": resolved.failure_mode,
            "required_span_kinds": _span_kinds(detail),
            "runtime": _runtime_trajectory_expectation(detail),
            "replay": {
                "trace_family": resolved.trace_family,
                "source_trace_id": trace_id,
                "expected_status": "succeeded",
            },
            "evaluator": {
                "candidate_gate": "evaluate_harness_candidate_gate",
                "required_assertions": ["no_sensitive_output", "failure_mode_absent"],
            },
        },
        "assertions": [
            {"type": "no_sensitive_output"},
            {"type": "failure_mode_absent", "value": resolved.failure_mode},
        ],
        "metadata": {
            **metadata,
            "source": "trace_feedback",
            "source_trace_id": trace_id,
            "trace_family": resolved.trace_family,
            "tenant_id": trace.get("tenant_id"),
            "failure_mode": resolved.failure_mode,
            "redacted": True,
            "review_status": "proposed",
        },
        "source_trace_id": trace_id,
    }


def build_harness_profile_proposal(
    cluster: dict[str, Any],
    *,
    proposed_by: str,
    rollback: dict[str, Any] | None = None,
) -> dict[str, Any]:
    failure_mode = str(cluster.get("failure_mode") or FAILURE_MODE_UNKNOWN)
    return {
        "proposal_id": f"trace-feedback-{failure_mode}",
        "status": "proposed",
        "auto_apply": False,
        "review_required": True,
        "eval_required": True,
        "proposed_by": proposed_by,
        "failure_mode": failure_mode,
        "trace_count": int(cluster.get("count") or 0),
        "source_trace_ids": [str(item) for item in cluster.get("trace_ids") or []],
        "change": _proposal_change_for_failure_mode(failure_mode),
        "rollback": rollback or {"type": "disable_proposal", "safe_default": "no_change"},
    }


def evaluate_harness_candidate_gate(
    candidate_metrics: dict[str, Any],
    *,
    baseline_metrics: dict[str, Any] | None = None,
    thresholds: dict[str, float] | None = None,
) -> dict[str, Any]:
    gate = apply_gate(
        candidate_metrics,
        baseline_metrics=baseline_metrics,
        thresholds=thresholds,
    )
    return {
        "status": "blocked" if gate["status"] == "fail" else "ready_for_review",
        "gate": gate,
        "review_required": True,
        "auto_apply": False,
    }


def _trace(detail: dict[str, Any]) -> dict[str, Any]:
    trace = detail.get("trace")
    return trace if isinstance(trace, dict) else detail


def _events(detail: dict[str, Any]) -> list[dict[str, Any]]:
    events = detail.get("events")
    return [event for event in events if isinstance(event, dict)] if isinstance(events, list) else []


def _spans(detail: dict[str, Any]) -> list[dict[str, Any]]:
    spans = detail.get("spans")
    return [span for span in spans if isinstance(span, dict)] if isinstance(spans, list) else []


def _scores(detail: dict[str, Any]) -> list[dict[str, Any]]:
    scores = detail.get("scores") or _trace(detail).get("scores")
    return [score for score in scores if isinstance(score, dict)] if isinstance(scores, list) else []


def _supported_family(value: Any) -> str:
    family = str(value or "assistant")
    return family if family in SUPPORTED_TRACE_FAMILIES else "assistant"


def _event_payload_contains(events: list[dict[str, Any]], needle: str) -> bool:
    return any(needle in str(event.get("payload") or event.get("data") or "") for event in events)


def _run_error_mentions(events: list[dict[str, Any]], needle: str) -> bool:
    return any(
        str(event.get("event_type") or "") == "run_error"
        and needle in str(event.get("payload") or event.get("data") or "")
        for event in events
    )


def _repeated_tool_call_count(events: list[dict[str, Any]]) -> int:
    names = [
        str((event.get("payload") or {}).get("tool_name") or (event.get("data") or {}).get("tool_name"))
        for event in events
        if str(event.get("event_type") or "") in {"tool_call_start", "tool_call"}
    ]
    if not names:
        return 0
    return Counter(names).most_common(1)[0][1]


def _numeric_score(score: dict[str, Any]) -> float | None:
    value = score.get("numeric_value", score.get("score"))
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def _span_kinds(detail: dict[str, Any]) -> list[str]:
    kinds = [
        str(span.get("span_kind") or span.get("kind") or "")
        for span in _spans(detail)
        if span.get("span_kind") or span.get("kind")
    ]
    return sorted(set(kinds))


def _runtime_trajectory_expectation(detail: dict[str, Any]) -> dict[str, Any]:
    trace = _trace(detail)
    metadata = trace.get("metadata") if isinstance(trace.get("metadata"), dict) else {}
    runtime = (
        metadata.get("runtime_trajectory")
        if isinstance(metadata.get("runtime_trajectory"), dict)
        else {}
    )
    terminal = (
        metadata.get("terminal_envelope")
        if isinstance(metadata.get("terminal_envelope"), dict)
        else {}
    )
    event_types = sorted(
        {
            str(event.get("event_type") or "")
            for event in _events(detail)
            if event.get("event_type")
        }
    )
    span_attributes = [
        span.get("attributes")
        for span in _spans(detail)
        if isinstance(span.get("attributes"), dict)
    ]
    gateway_decisions = [
        attributes.get("gateway_policy_decision")
        for attributes in span_attributes
        if attributes.get("gateway_policy_decision") is not None
    ]
    sandbox_decisions = [
        attributes.get("sandbox_decision")
        for attributes in span_attributes
        if attributes.get("sandbox_decision") is not None
    ]
    return _safe_export_value(
        {
            "schema_version": "assistant-runtime-trajectory/v1",
            "expected_status": "succeeded",
            "observed_status": trace.get("status"),
            "observed_exit_reason": runtime.get("exit_reason") or terminal.get("exit_reason"),
            "context_snapshot_id": runtime.get("context_snapshot_id")
            or terminal.get("context_snapshot_id"),
            "requires_redaction": True,
            "redaction_state": trace.get("redaction_state") or runtime.get("redaction_state") or {},
            "memory": runtime.get("memory") or {},
            "trace_writer_health": runtime.get("trace_writer_health") or {},
            "transcript_locator": runtime.get("transcript_locator")
            or metadata.get("transcript_locator")
            or {},
            "event_types": event_types,
            "has_memory_sync_evidence": _event_payload_contains(_events(detail), "memory_sync"),
            "has_pre_compaction_flush_evidence": _event_payload_contains(
                _events(detail),
                "pre_compaction_flush",
            ),
            "tool_safety": {
                "gateway_decisions": gateway_decisions[:8],
                "sandbox_decisions": sandbox_decisions[:8],
                "direct_registry_denied": any(
                    attributes.get("direct_registry_denied") is True
                    or str(attributes.get("direct_registry_denied")).lower() == "true"
                    for attributes in span_attributes
                ),
            },
        }
    )


def _redacted_metadata(metadata: dict[str, Any]) -> dict[str, Any]:
    redacted: dict[str, Any] = {}
    for key, value in metadata.items():
        key_str = str(key)
        if _is_sensitive_metadata_key(key_str):
            continue
        if isinstance(value, str):
            redacted[key_str] = _redact_export_text(value)
        elif isinstance(value, (int, float, bool)) or value is None:
            redacted[key_str] = value
        else:
            redacted[key_str] = _safe_export_value(value)
    return redacted


def _is_sensitive_metadata_key(key: str) -> bool:
    lowered = key.lower()
    if lowered in _CONTROL_METADATA_KEYS:
        return True
    return any(marker in lowered for marker in _SECRET_METADATA_KEY_MARKERS)


def _proposal_change_for_failure_mode(failure_mode: str) -> dict[str, Any]:
    if failure_mode == FAILURE_MODE_LOOP_DETECTED:
        return {"profile_key": "loop_detection", "suggested_state": "tighten"}
    if failure_mode == FAILURE_MODE_APPROVAL_BLOCKED:
        return {"profile_key": "approval_resume", "suggested_state": "inspect"}
    if failure_mode == FAILURE_MODE_RAG_MISS:
        return {"profile_key": "rag_retrieval", "suggested_state": "evaluate_retrieval"}
    if failure_mode == FAILURE_MODE_CONTEXT_OVERFLOW:
        return {"profile_key": "context_budget", "suggested_state": "lower_risk"}
    return {"profile_key": "runtime_harness", "suggested_state": "review"}
