//! Exact source fixture and assertions: tests/services/eval/test_trace_feedback.py.
use ai_platform_gateway::migration_compat::eval_feedback::*;
use serde_json::{Value, json};
fn fixture(name: &str) -> Value {
    serde_json::from_str::<Value>(include_str!("fixtures/trace_feedback_source.json")).unwrap()
        [name]
        .clone()
}
fn classify(detail: &Value) -> Value {
    classify_trace_failure(detail, 0.75, 30000)
}
#[test]
fn test_classifies_assistant_tool_error() {
    let p = classify(&fixture("test_classifies_assistant_tool_error")[0]);
    assert_eq!(p["trace_family"], "assistant");
    assert_eq!(p["failure_mode"], "tool_error");
    assert_eq!(p["severity"], "high");
}
#[test]
fn test_succeeded_trace_with_tool_error_event_is_not_classified_as_tool_failure() {
    let p = classify(
        &fixture("test_succeeded_trace_with_tool_error_event_is_not_classified_as_tool_failure")[0],
    );
    assert_ne!(p["failure_mode"], "tool_error");
}
#[test]
fn test_classifies_rag_miss() {
    let p = classify(&fixture("test_classifies_rag_miss")[0]);
    assert_eq!(p["trace_family"], "rag");
    assert_eq!(p["failure_mode"], "rag_miss");
}
#[test]
fn test_classifies_langgraph_low_score() {
    let p = classify(&fixture("test_classifies_langgraph_low_score")[0]);
    assert_eq!(p["trace_family"], "langgraph_proxy");
    assert_eq!(p["failure_mode"], "low_score");
}
#[test]
fn test_builds_redacted_dataset_case_from_trace() {
    let detail = &fixture("test_builds_redacted_dataset_case_from_trace")[0];
    let p = classify(detail);
    let case = build_redacted_dataset_case(detail, &p, "test");
    assert_eq!(p["failure_mode"], "approval_blocked");
    assert_eq!(
        case["source_trace_id"],
        "44444444-4444-4444-4444-444444444444"
    );
    assert_eq!(case["metadata"]["review_status"], "proposed");
    assert_eq!(case["metadata"]["failure_mode"], "approval_blocked");
    assert!(case["metadata"]["tenant_id"].is_null());
    assert_eq!(
        case["expected_trajectory"]["replay"]["trace_family"],
        "assistant"
    );
    assert_eq!(
        case["expected_trajectory"]["evaluator"]["candidate_gate"],
        "evaluate_harness_candidate_gate"
    );
    assert_eq!(
        case["expected_trajectory"]["evaluator"]["required_assertions"],
        json!(["no_sensitive_output", "failure_mode_absent"])
    );
    assert_eq!(
        case["assertions"],
        json!([{"type":"no_sensitive_output"},{"type":"failure_mode_absent","value":"approval_blocked"}])
    );
    let runtime = &case["expected_trajectory"]["runtime"];
    assert_eq!(runtime["schema_version"], "assistant-runtime-trajectory/v1");
    assert_eq!(runtime["observed_exit_reason"], "approval_required");
    assert_eq!(runtime["context_snapshot_id"], "ctx_safe");
    assert_eq!(runtime["has_memory_sync_evidence"], true);
    assert_eq!(runtime["has_pre_compaction_flush_evidence"], true);
    assert_eq!(runtime["tool_safety"]["direct_registry_denied"], true);
    assert_eq!(
        runtime["tool_safety"]["gateway_decisions"][0]["decision"],
        "deny"
    );
    let serialized = case.to_string();
    assert!(!serialized.contains("raw-token"));
    assert!(!serialized.contains("secret-value"));
    for key in ["raw_input", "Authorization", "api_key"] {
        assert!(case["metadata"].get(key).is_none());
    }
}
#[test]
fn test_clusters_patterns_and_builds_review_gated_proposal() {
    let fixtures = fixture("test_clusters_patterns_and_builds_review_gated_proposal");
    let patterns: Vec<_> = fixtures.as_array().unwrap().iter().map(classify).collect();
    let clusters = cluster_failure_patterns(&patterns);
    let cluster = &clusters[0];
    let proposal = build_harness_profile_proposal(cluster, "eval-feedback", None);
    assert_eq!(cluster["failure_mode"], "tool_error");
    assert_eq!(cluster["count"], 2);
    assert_eq!(proposal["status"], "proposed");
    assert_eq!(proposal["review_required"], true);
    assert_eq!(proposal["auto_apply"], false);
}
#[test]
fn test_candidate_gate_blocks_known_bad_metrics() {
    let gate = evaluate_harness_candidate_gate(
        &json!({"overall_score":0.5,"trajectory_pass_rate":0.5,"critical_pass_rate":0.5}),
        None,
        None,
    );
    assert_eq!(gate["status"], "blocked");
    assert_eq!(gate["auto_apply"], false);
    assert_eq!(gate["gate"]["status"], "fail");
    assert!(!gate["gate"]["failures"].as_array().unwrap().is_empty());
}
