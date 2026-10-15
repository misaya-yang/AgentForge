from __future__ import annotations

from src.services.eval.trace_feedback import (
    FAILURE_MODE_APPROVAL_BLOCKED,
    FAILURE_MODE_LOW_SCORE,
    FAILURE_MODE_RAG_MISS,
    FAILURE_MODE_TOOL_ERROR,
    build_harness_profile_proposal,
    build_redacted_dataset_case,
    classify_trace_failure,
    cluster_failure_patterns,
    evaluate_harness_candidate_gate,
)


def test_classifies_assistant_tool_error() -> None:
    pattern = classify_trace_failure(
        {
            "trace": {
                "trace_id": "11111111-1111-1111-1111-111111111111",
                "trace_family": "assistant",
                "status": "failed",
                "input_preview": "hello",
                "output_preview": "failed",
            },
            "events": [{"event_type": "tool_error", "payload": {"tool_name": "search"}}],
            "spans": [],
        }
    )

    assert pattern.trace_family == "assistant"
    assert pattern.failure_mode == FAILURE_MODE_TOOL_ERROR
    assert pattern.severity == "high"


def test_succeeded_trace_with_tool_error_event_is_not_classified_as_tool_failure() -> None:
    pattern = classify_trace_failure(
        {
            "trace": {
                "trace_id": "77777777-7777-7777-7777-777777777777",
                "trace_family": "assistant",
                "status": "succeeded",
                "input_preview": "hello",
                "output_preview": "done",
            },
            "events": [{"event_type": "tool_error", "payload": {"tool_name": "search"}}],
            "spans": [],
        }
    )

    assert pattern.failure_mode != FAILURE_MODE_TOOL_ERROR


def test_classifies_rag_miss() -> None:
    pattern = classify_trace_failure(
        {
            "trace": {
                "trace_id": "22222222-2222-2222-2222-222222222222",
                "trace_family": "rag",
                "status": "failed",
                "input_preview": "kb query",
                "output_preview": "no answer",
            },
            "events": [
                {
                    "event_type": "rag_retrieval_completed",
                    "payload": {"reason": "no_relevant_chunks"},
                }
            ],
            "spans": [],
        }
    )

    assert pattern.trace_family == "rag"
    assert pattern.failure_mode == FAILURE_MODE_RAG_MISS


def test_classifies_langgraph_low_score() -> None:
    pattern = classify_trace_failure(
        {
            "trace": {
                "trace_id": "33333333-3333-3333-3333-333333333333",
                "trace_family": "langgraph_proxy",
                "status": "succeeded",
                "input_preview": "POST /runs",
                "output_preview": "ok",
            },
            "scores": [{"score_name": "quality", "numeric_value": 0.4}],
        }
    )

    assert pattern.trace_family == "langgraph_proxy"
    assert pattern.failure_mode == FAILURE_MODE_LOW_SCORE


def test_builds_redacted_dataset_case_from_trace() -> None:
    detail = {
        "trace": {
            "trace_id": "44444444-4444-4444-4444-444444444444",
            "trace_family": "assistant",
            "status": "failed",
            "input_preview": "Authorization: Bearer raw-token user asks",
            "output_preview": "api_key=secret-value failed",
            "thread_id": "thread-1",
            "run_id": "run-1",
            "redaction_state": {"payloads": "redacted_truncated"},
            "metadata": {
                "raw_input": "do not copy",
                "Authorization": "Bearer raw-token",
                "api_key": "secret-value",
                "note": "token=secret-value",
                "runtime_trajectory": {
                    "schema_version": "assistant-runtime-trajectory/v1",
                    "exit_reason": "approval_required",
                    "context_snapshot_id": "ctx_safe",
                    "memory": {"runtime_memory_provenance_count": 2},
                    "trace_writer_health": {"issue_count": 0},
                    "transcript_locator": {"turn_index": 3, "bounded": True},
                },
            },
        },
        "events": [
            {"event_type": "approval_required", "payload": {}},
            {"event_type": "memory_sync", "payload": {"memory_sync": {"status": "skipped"}}},
            {
                "event_type": "context_compacted",
                "payload": {"pre_compaction_flush": {"status": "flushed"}},
            },
        ],
        "spans": [
            {
                "span_kind": "tool",
                "attributes": {
                    "gateway_policy_decision": {"decision": "deny"},
                    "sandbox_decision": {"decision": "blocked"},
                    "direct_registry_denied": True,
                },
            }
        ],
    }
    pattern = classify_trace_failure(detail)
    case = build_redacted_dataset_case(detail, pattern)

    serialized = str(case)
    assert pattern.failure_mode == FAILURE_MODE_APPROVAL_BLOCKED
    assert case["source_trace_id"] == "44444444-4444-4444-4444-444444444444"
    assert case["metadata"]["review_status"] == "proposed"
    assert case["metadata"]["failure_mode"] == FAILURE_MODE_APPROVAL_BLOCKED
    assert case["metadata"]["tenant_id"] is None
    assert case["expected_trajectory"]["replay"]["trace_family"] == "assistant"
    assert case["expected_trajectory"]["evaluator"]["candidate_gate"] == "evaluate_harness_candidate_gate"
    assert case["expected_trajectory"]["evaluator"]["required_assertions"] == [
        "no_sensitive_output",
        "failure_mode_absent",
    ]
    assert case["assertions"] == [
        {"type": "no_sensitive_output"},
        {"type": "failure_mode_absent", "value": FAILURE_MODE_APPROVAL_BLOCKED},
    ]
    runtime = case["expected_trajectory"]["runtime"]
    assert runtime["schema_version"] == "assistant-runtime-trajectory/v1"
    assert runtime["observed_exit_reason"] == "approval_required"
    assert runtime["context_snapshot_id"] == "ctx_safe"
    assert runtime["has_memory_sync_evidence"] is True
    assert runtime["has_pre_compaction_flush_evidence"] is True
    assert runtime["tool_safety"]["direct_registry_denied"] is True
    assert runtime["tool_safety"]["gateway_decisions"][0]["decision"] == "deny"
    assert "raw-token" not in serialized
    assert "secret-value" not in serialized
    assert "raw_input" not in case["metadata"]
    assert "Authorization" not in case["metadata"]
    assert "api_key" not in case["metadata"]


def test_clusters_patterns_and_builds_review_gated_proposal() -> None:
    patterns = [
        classify_trace_failure(
            {
                "trace": {
                    "trace_id": "55555555-5555-5555-5555-555555555555",
                    "trace_family": "assistant",
                    "status": "failed",
                    "output_preview": "failed",
                },
                "events": [{"event_type": "tool_error", "payload": {}}],
            }
        ),
        classify_trace_failure(
            {
                "trace": {
                    "trace_id": "66666666-6666-6666-6666-666666666666",
                    "trace_family": "assistant",
                    "status": "failed",
                    "output_preview": "failed",
                },
                "events": [{"event_type": "tool_error", "payload": {}}],
            }
        ),
    ]

    cluster = cluster_failure_patterns(patterns)[0]
    proposal = build_harness_profile_proposal(cluster, proposed_by="eval-feedback")

    assert cluster["failure_mode"] == FAILURE_MODE_TOOL_ERROR
    assert cluster["count"] == 2
    assert proposal["status"] == "proposed"
    assert proposal["review_required"] is True
    assert proposal["auto_apply"] is False


def test_candidate_gate_blocks_known_bad_metrics() -> None:
    gate = evaluate_harness_candidate_gate(
        {
            "overall_score": 0.5,
            "trajectory_pass_rate": 0.5,
            "critical_pass_rate": 0.5,
        }
    )

    assert gate["status"] == "blocked"
    assert gate["auto_apply"] is False
    assert gate["gate"]["status"] == "fail"
    assert gate["gate"]["failures"]
