//! Read-only trace failure classification and review-required dataset/profile proposals.
use super::{
    common::{CompatError, CompatUser, hex, object, text},
    eval::EvalService,
    eval_export::{redact_text, redaction_policy, safe_value},
    eval_http::EvalHttpOptions,
};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::collections::{BTreeSet, HashMap};
fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn trace(v: &Value) -> &Value {
    v.get("trace").filter(|v| v.is_object()).unwrap_or(v)
}
fn payload(v: &Value) -> &Value {
    v.get("payload")
        .filter(|v| !v.is_null() && !object(v).is_empty())
        .unwrap_or(&v["data"])
}
fn contains(events: &[Value], needle: &str) -> bool {
    events
        .iter()
        .any(|v| payload(v).to_string().contains(needle))
}
fn numeric(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .or_else(|| v.as_bool().map(|b| if b { 1.0 } else { 0.0 }))
}
fn fallback<'a>(a: &'a Value, b: &'a Value) -> &'a Value {
    if a.is_null()
        || a.as_str() == Some("")
        || a.as_object().is_some_and(|v| v.is_empty())
        || a.as_array().is_some_and(|v| v.is_empty())
    {
        b
    } else {
        a
    }
}
pub fn classify_trace_failure(detail: &Value, low_score: f64, latency_ms: i64) -> Value {
    let trace = trace(detail);
    let events = arr(&detail["events"]);
    let event_types = events
        .iter()
        .filter_map(|v| v["event_type"].as_str())
        .collect::<BTreeSet<_>>();
    let status = text(trace, "status").to_lowercase();
    let family = if matches!(
        text(trace, "trace_family"),
        "assistant" | "langgraph_proxy" | "rag"
    ) {
        text(trace, "trace_family")
    } else {
        "assistant"
    };
    let errors = arr(&detail["spans"]).iter().any(|s| {
        !text(s, "error_type").is_empty()
            || !text(s, "error_message").is_empty()
            || s["status"] == "failed"
    });
    let mut tools = HashMap::new();
    for event in events
        .iter()
        .filter(|v| matches!(text(v, "event_type"), "tool_call_start" | "tool_call"))
    {
        let name = fallback(&event["payload"]["tool_name"], &event["data"]["tool_name"]);
        *tools.entry(name.to_string()).or_insert(0) += 1;
    }
    let score_values = if arr(&detail["scores"]).is_empty() {
        arr(&trace["scores"])
    } else {
        arr(&detail["scores"])
    };
    let (mode, reason, severity) = if matches!(status.as_str(), "failed" | "error")
        && (event_types.contains("tool_error")
            || errors
            || !trace["error"].is_null() && trace["error"] != false)
    {
        ("tool_error", "trace_failed_with_tool_or_span_error", "high")
    } else if (event_types.contains("context_budget") || event_types.contains("context_compacted"))
        && (contains(events, "dropped_history_messages") || contains(events, "context_overflow"))
    {
        (
            "context_overflow",
            "context_budget_or_compaction_dropped_context",
            "high",
        )
    } else if event_types.contains("loop_detected") || tools.values().any(|v| *v >= 3) {
        ("loop_detected", "repeated_tool_call_sequence", "high")
    } else if event_types.contains("approval_required")
        && !matches!(status.as_str(), "succeeded" | "success")
    {
        (
            "approval_blocked",
            "approval_required_without_successful_resume",
            "medium",
        )
    } else if family == "rag"
        && (matches!(status.as_str(), "failed" | "error") || contains(events, "no_relevant_chunks"))
    {
        (
            "rag_miss",
            "rag_trace_failed_or_no_relevant_chunks",
            "medium",
        )
    } else if text(trace, "output_preview").trim().is_empty()
        || events.iter().any(|v| {
            v["event_type"] == "run_error"
                && payload(v).to_string().contains("model_produced_no_text")
        })
    {
        ("model_empty_output", "empty_output_preview", "high")
    } else if score_values.iter().any(|v| {
        numeric(v.get("numeric_value").unwrap_or(&v["score"])).is_some_and(|v| v < low_score)
    }) {
        ("low_score", "score_below_threshold", "medium")
    } else if trace["total_latency_ms"].as_i64().unwrap_or(0) > latency_ms {
        ("latency_regression", "latency_above_threshold", "medium")
    } else {
        ("unknown_failure", "unclassified_failure", "medium")
    };
    json!({"trace_id":text(trace,"trace_id"),"trace_family":family,"failure_mode":mode,"reasons":[reason],"severity":severity})
}
pub fn cluster_failure_patterns(patterns: &[Value]) -> Vec<Value> {
    let mut clusters: Vec<Value> = vec![];
    for pattern in patterns {
        let mode = text(pattern, "failure_mode");
        if let Some(cluster) = clusters.iter_mut().find(|v| v["failure_mode"] == mode) {
            cluster["count"] = json!(cluster["count"].as_i64().unwrap_or(0) + 1);
            cluster["trace_ids"]
                .as_array_mut()
                .unwrap()
                .push(pattern["trace_id"].clone());
        } else {
            clusters.push(json!({"failure_mode":mode,"count":1,"trace_ids":[pattern["trace_id"]]}));
        }
    }
    clusters.sort_by(|a, b| b["count"].as_i64().cmp(&a["count"].as_i64()));
    clusters
}
pub fn runtime_trajectory_expectation(detail: &Value) -> Value {
    let trace = trace(detail);
    let runtime = &trace["metadata"]["runtime_trajectory"];
    let terminal = &trace["metadata"]["terminal_envelope"];
    let events = arr(&detail["events"]);
    let event_types = events
        .iter()
        .filter_map(|v| v["event_type"].as_str().filter(|s| !s.is_empty()))
        .collect::<BTreeSet<_>>();
    let attrs = arr(&detail["spans"])
        .iter()
        .map(|v| &v["attributes"])
        .filter(|v| v.is_object())
        .collect::<Vec<_>>();
    safe_value(
        &json!({"schema_version":"assistant-runtime-trajectory/v1","expected_status":"succeeded","observed_status":trace["status"],"observed_exit_reason":fallback(&runtime["exit_reason"],&terminal["exit_reason"]),"context_snapshot_id":fallback(&runtime["context_snapshot_id"],&terminal["context_snapshot_id"]),"requires_redaction":true,"redaction_state":object(fallback(&trace["redaction_state"],&runtime["redaction_state"])),"memory":object(&runtime["memory"]),"trace_writer_health":object(&runtime["trace_writer_health"]),"transcript_locator":object(fallback(&runtime["transcript_locator"],&trace["metadata"]["transcript_locator"])),"event_types":event_types,"has_memory_sync_evidence":contains(events,"memory_sync"),"has_pre_compaction_flush_evidence":contains(events,"pre_compaction_flush"),"tool_safety":{"gateway_decisions":attrs.iter().filter_map(|v|v.get("gateway_policy_decision").filter(|v|!v.is_null())).take(8).collect::<Vec<_>>(),"sandbox_decisions":attrs.iter().filter_map(|v|v.get("sandbox_decision").filter(|v|!v.is_null())).take(8).collect::<Vec<_>>(),"direct_registry_denied":attrs.iter().any(|v|v["direct_registry_denied"]==true||text(v,"direct_registry_denied").eq_ignore_ascii_case("true"))}}),
    )
}
pub fn build_redacted_dataset_case(detail: &Value, pattern: &Value, split: &str) -> Value {
    let trace = trace(detail);
    let id = if text(trace, "trace_id").is_empty() {
        text(pattern, "trace_id")
    } else {
        text(trace, "trace_id")
    };
    let hash = hex(&Sha1::digest(
        format!("{}:{id}", text(pattern, "trace_family")).as_bytes(),
    ));
    let mut metadata = serde_json::Map::new();
    for (key, value) in object(&trace["metadata"]) {
        let lowered = key.to_lowercase();
        if matches!(
            lowered.as_str(),
            "raw_input" | "raw_output" | "messages" | "tool_arguments"
        ) || [
            "authorization",
            "api_key",
            "apikey",
            "password",
            "secret",
            "token",
            "cookie",
            "credential",
        ]
        .iter()
        .any(|v| lowered.contains(v))
        {
            continue;
        }
        metadata.insert(key, safe_value(&value));
    }
    metadata.extend(object(&json!({"source":"trace_feedback","source_trace_id":id,"trace_family":pattern["trace_family"],"tenant_id":trace["tenant_id"],"failure_mode":pattern["failure_mode"],"redacted":true,"review_status":"proposed"})));
    let kinds = arr(&detail["spans"])
        .iter()
        .filter_map(|v| {
            fallback(&v["span_kind"], &v["kind"])
                .as_str()
                .filter(|s| !s.is_empty())
        })
        .collect::<BTreeSet<_>>();
    json!({"case_id":format!("{}-{}-{}",text(pattern,"trace_family"),&hash[..10],text(pattern,"failure_mode")),"split":split,"input":{"input_preview":redact_text(text(trace,"input_preview")),"trace_family":pattern["trace_family"],"thread_id":fallback(&trace["thread_id"],&trace["session_id"]),"run_id":trace["run_id"]},"expected_output":{"output_preview":redact_text(text(trace,"output_preview")),"expected_status":"succeeded"},"expected_trajectory":{"source_failure_mode":pattern["failure_mode"],"required_span_kinds":kinds,"runtime":runtime_trajectory_expectation(detail),"replay":{"trace_family":pattern["trace_family"],"source_trace_id":id,"expected_status":"succeeded"},"evaluator":{"candidate_gate":"evaluate_harness_candidate_gate","required_assertions":["no_sensitive_output","failure_mode_absent"]}},"assertions":[{"type":"no_sensitive_output"},{"type":"failure_mode_absent","value":pattern["failure_mode"]}],"metadata":metadata,"source_trace_id":id})
}
pub fn build_harness_profile_proposal(
    cluster: &Value,
    proposed_by: &str,
    rollback: Option<&Value>,
) -> Value {
    let mode = if text(cluster, "failure_mode").is_empty() {
        "unknown_failure"
    } else {
        text(cluster, "failure_mode")
    };
    let (key, state) = match mode {
        "loop_detected" => ("loop_detection", "tighten"),
        "approval_blocked" => ("approval_resume", "inspect"),
        "rag_miss" => ("rag_retrieval", "evaluate_retrieval"),
        "context_overflow" => ("context_budget", "lower_risk"),
        _ => ("runtime_harness", "review"),
    };
    json!({"proposal_id":format!("trace-feedback-{mode}"),"status":"proposed","auto_apply":false,"review_required":true,"eval_required":true,"proposed_by":proposed_by,"failure_mode":mode,"trace_count":cluster["count"].as_i64().unwrap_or(0),"source_trace_ids":arr(&cluster["trace_ids"]),"change":{"profile_key":key,"suggested_state":state},"rollback":rollback.cloned().unwrap_or(json!({"type":"disable_proposal","safe_default":"no_change"}))})
}
pub fn evaluate_harness_candidate_gate(
    candidate: &Value,
    baseline: Option<&Value>,
    thresholds: Option<&Value>,
) -> Value {
    let gate = super::eval_gate::apply_gate(candidate, thresholds, baseline, false, false);
    json!({"status":if gate["status"]=="fail"{"blocked"}else{"ready_for_review"},"gate":gate,"review_required":true,"auto_apply":false})
}
impl EvalService {
    pub async fn preview_trace_feedback(
        &self,
        user: &CompatUser,
        body: &Value,
        options: &EvalHttpOptions,
    ) -> Result<Value, CompatError> {
        let family = body["trace_family"].as_str().unwrap_or("assistant");
        if !matches!(family, "assistant" | "langgraph_proxy" | "rag") {
            return Err(CompatError::new(400, "Unsupported trace_family."));
        }
        let ids = body["trace_ids"]
            .as_array()
            .filter(|v| (1..=50).contains(&v.len()) && v.iter().all(Value::is_string))
            .ok_or_else(|| CompatError::new(422, "Invalid trace_ids"))?;
        let split = body["split"].as_str().unwrap_or("regression");
        let proposed = body["proposed_by"].as_str().unwrap_or("eval-feedback-api");
        if split.chars().count() > 32 || proposed.chars().count() > 128 {
            return Err(CompatError::new(422, "Invalid feedback label"));
        }
        let low = body
            .get("low_score_threshold")
            .map(|v| {
                v.as_f64()
                    .filter(|v| (0.0..=1.0).contains(v))
                    .ok_or_else(|| CompatError::new(422, "Invalid low_score_threshold"))
            })
            .transpose()?
            .unwrap_or(0.75);
        let latency = body
            .get("latency_threshold_ms")
            .map(|v| {
                v.as_i64()
                    .filter(|v| *v > 0)
                    .ok_or_else(|| CompatError::new(422, "Invalid latency_threshold_ms"))
            })
            .transpose()?
            .unwrap_or(30000);
        let dataset = body["dataset_id"].as_str().filter(|s| !s.is_empty());
        if let Some(dataset) = dataset {
            if self
                .get(&user.tenant_id, "datasets", dataset)
                .await?
                .is_none()
            {
                return Err(CompatError::new(
                    404,
                    format!("Dataset not found: {dataset}"),
                ));
            }
        }
        let proposed = if proposed.is_empty() {
            format!(
                "eval-feedback:{}",
                if user.user_id.is_empty() {
                    "system"
                } else {
                    &user.user_id
                }
            )
        } else {
            proposed.into()
        };
        let repo = options
            .traces
            .as_ref()
            .ok_or_else(|| CompatError::new(503, "Database not available"))?;
        let (mut seen, mut patterns, mut cases) = (BTreeSet::new(), vec![], vec![]);
        for id in ids {
            let id = id.as_str().unwrap();
            if !seen.insert(id) {
                continue;
            }
            let detail = repo
                .get_trace_detail(
                    &user.tenant_id,
                    id,
                    if user.operator() {
                        None
                    } else {
                        Some(&user.user_id)
                    },
                    family,
                )
                .await
                .map_err(|e| CompatError::new(e.status, e.code))?
                .ok_or_else(|| CompatError::new(404, format!("Trace not found: {id}")))?;
            let pattern = classify_trace_failure(&detail, low, latency);
            cases.push(build_redacted_dataset_case(&detail, &pattern, split));
            patterns.push(pattern);
        }
        let clusters = cluster_failure_patterns(&patterns);
        let proposals = clusters
            .iter()
            .map(|v| build_harness_profile_proposal(v, &proposed, None))
            .collect::<Vec<_>>();
        Ok(
            json!({"trace_family":family,"dataset_id":dataset,"patterns":patterns,"clusters":clusters,"dataset_cases":cases,"import_request":if dataset.is_some(){json!({"examples":cases,"mode":"skip_duplicates"})}else{Value::Null},"proposals":proposals,"redaction_policy":redaction_policy()}),
        )
    }
}
