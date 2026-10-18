//! Strict V2 public event observations and durable Runtime receipt projections.
use super::common::{CompatError, text};
use serde_json::{Value, json};
#[path = "../../../platform-eval/src/runtime_contract.rs"]
mod runtime_contract_source;
pub use runtime_contract_source::{assert_runtime_observation, validate_runtime_observation};
pub fn candidate_fingerprint_from_context(data: &Value) -> Value {
    let mut result = json!({});
    for (out, keys) in [
        (
            "system_prompt_hash",
            vec!["candidate_system_prompt_hash", "system_prompt_hash"],
        ),
        ("runtime_revision", vec!["runtime_revision"]),
        (
            "tool_schema_hash",
            vec!["available_tool_schema_hash", "tool_schema_hash"],
        ),
    ] {
        if let Some(value) = keys.iter().find_map(|key| {
            data.get(*key)
                .filter(|v| !v.is_null() && v.as_str() != Some(""))
        }) {
            result[out] = value.clone();
        }
    }
    let snapshot = &data["context_snapshot"];
    if snapshot.is_object() {
        let policy = &snapshot["policy"];
        result["model_id"] = snapshot["model_id"].clone();
        result["provider"] = snapshot["provider"].clone();
        result["sampling"] = json!({"temperature":snapshot["bootstrap"]["temperature"],"max_tokens":snapshot["bootstrap"]["max_tokens"]});
        result["execution_policy"] = json!({"execution_profile":policy["execution_profile"],"runtime_mode":policy["runtime_mode"],"kb_mode":policy["kb_mode"],"web_search_enabled":policy["web_search_enabled"]});
        result["rag_config_hash"] = policy["rag_config_hash"].clone();
        result["rag_revision_hash"] = policy["rag_revision_hash"].clone();
    }
    result
}
pub use ai_platform_gateway_integrations::observation_redaction::redact_preview;
fn span_id(run: &str, key: &str) -> Result<String, CompatError> {
    let uuid = uuid::Uuid::parse_str(run)
        .map_err(|_| CompatError::new(502, "AGENT_EVAL_TRACE_IDENTITY_UNAVAILABLE"))?;
    Ok(uuid::Uuid::new_v5(&uuid, key.as_bytes()).to_string())
}
pub struct RuntimeTraceInput<'a> {
    pub run: &'a str,
    pub request: &'a str,
    pub tenant: &'a str,
    pub user: &'a str,
    pub session: &'a str,
    pub message: &'a str,
    pub snapshot: &'a Value,
    pub status: &'a str,
    pub started: chrono::DateTime<chrono::Utc>,
    pub ended: chrono::DateTime<chrono::Utc>,
    pub first_token_ms: i64,
    pub output: &'a str,
    pub event_counts: &'a Value,
    pub usage: &'a Value,
    pub error: Option<&'a str>,
}
pub fn build_assistant_runtime_trace(input: RuntimeTraceInput<'_>) -> Result<Value, CompatError> {
    let duration = (input.ended - input.started).num_milliseconds().max(0);
    let metrics = json!({"total_latency_ms":duration,"first_token_latency_ms":input.first_token_ms.max(0),"input_tokens":input.usage["input_tokens"].as_i64().unwrap_or(0).max(0),"output_tokens":input.usage["output_tokens"].as_i64().unwrap_or(0).max(0),"total_tokens":input.usage["total_tokens"].as_i64().unwrap_or(0).max(0)});
    let start = input.started.to_rfc3339();
    let end = input.ended.to_rfc3339();
    let incoming = redact_preview(input.message, 500);
    let outgoing = redact_preview(input.output, 500);
    Ok(
        json!({"trace_id":input.run,"trace_family":"assistant","workflow_kind":"agent_runtime_turn","tenant_id":input.tenant,"user_id":input.user,"session_id":input.session,"thread_id":input.session,"run_id":input.run,"request_id":if input.request.is_empty(){input.run}else{input.request},"model_id":input.snapshot["model"]["id"],"provider":input.snapshot["model"]["provider"],"status":input.status,"started_at":start,"ended_at":end,"input_preview":incoming,"output_preview":outgoing,"metrics":metrics,"privacy":{"payloads":"bounded_redacted_preview"},"redaction_state":{"input":"bounded","output":"bounded"},"metadata":{"agent_id":input.snapshot["agent_id"],"agent_version_id":input.snapshot["agent_version_id"],"publication_id":input.snapshot["publication"]["id"],"channel":input.snapshot["publication"]["channel"],"runtime_trajectory":{"exit_reason":input.status,"event_counts":input.event_counts},"error_type":input.error},"source_adapter":"gateway.agent_runtime","spans":[{"span_id":span_id(input.run,"runtime_turn")?,"span_kind":"agent_runtime","name":"agent_runtime_turn","status":input.status,"sequence_no":0,"started_at":start,"ended_at":end,"duration_ms":duration,"input_preview":incoming,"output_preview":outgoing,"attributes":{"run_id":input.run,"session_id":input.session},"error_type":input.error}],"events":[{"event_type":"agent_runtime_turn_finished","sequence_no":1,"payload":{"status":input.status,"event_counts":input.event_counts}}]}),
    )
}
pub fn runtime_model_evidence(
    evidence: &Value,
    tenant: &str,
    case: &str,
    run: &str,
    expected: &Value,
) -> Result<(Value, Value, Vec<Value>), CompatError> {
    let unavailable = || CompatError::new(503, "AGENT_EVAL_MODEL_EVIDENCE_UNAVAILABLE");
    if evidence["tenant_id"] != tenant
        || evidence["session_id"] != case
        || evidence["run_id"] != run
    {
        return Err(unavailable());
    }
    let reference = json!({"tenant_id":tenant,"provider_id":evidence["provider_id"],"model_id":evidence["model_id"],"capability_revision":evidence["capability_revision"],"price_version":evidence["pricing_snapshot"]["version"]});
    for (key, value) in reference.as_object().unwrap() {
        if value.is_null() || value.as_str() == Some("") {
            return Err(unavailable());
        }
        if expected[key] != *value {
            return Err(CompatError::new(409, "AGENT_EVAL_MODEL_REF_MISMATCH"));
        }
    }
    let mut fingerprint = json!({"runtime_revision":evidence["runtime_revision"],"model_id":reference["model_id"],"provider":reference["provider_id"],"model_ref":reference,"model_ref_verified":true,"sampling":{"temperature":evidence["parameters"]["temperature"],"max_tokens":evidence["limits"]["max_output_tokens"]}});
    for key in ["system_prompt_hash", "tool_schema_hash"] {
        if let Some(hash) = evidence["eval_fingerprint"][key].as_str().filter(|v| {
            v.len() == 64
                && v.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            fingerprint[key] = json!(hash);
        }
    }
    let versions = [
        "agent_id",
        "agent_version_id",
        "agent_spec_hash",
        "agent_runtime_snapshot_hash",
    ];
    if versions.iter().all(|key| !text(evidence, key).is_empty()) {
        for key in versions {
            fingerprint[key] = evidence[key].clone();
        }
        fingerprint["agent_spec_hash"] =
            json!(text(evidence, "agent_spec_hash").trim_start_matches("sha256:"));
        fingerprint["candidate_type"] = json!("agent_version");
    }
    let calls = evidence["calls"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter(|call| !call["dispatched_at"].is_null())
        .collect::<Vec<_>>();
    let mut usage = json!({});
    for key in ["input_tokens", "output_tokens"] {
        if !calls.is_empty()
            && calls
                .iter()
                .all(|call| call[key].is_i64() || call[key].is_u64())
        {
            usage[key] = json!(
                calls
                    .iter()
                    .filter_map(|call| call[key].as_i64())
                    .sum::<i64>()
            );
        }
    }
    if let Some((incoming, outgoing)) = usage["input_tokens"]
        .as_i64()
        .zip(usage["output_tokens"].as_i64())
    {
        usage["total_tokens"] = json!(incoming + outgoing);
    }
    let mut spans = vec![];
    for (index, call) in calls.iter().enumerate() {
        let start = chrono::DateTime::parse_from_rfc3339(text(call, "dispatched_at")).ok();
        let end = chrono::DateTime::parse_from_rfc3339(text(call, "completed_at")).ok();
        spans.push(json!({"span_id":span_id(run,&format!("model_call:{}",text(call,"call_id")))?,"span_kind":"model_invocation","name":"runtime_model_call","sequence_no":index+1,"status":if call["status"]=="completed"{"succeeded"}else{"failed"},"started_at":call["dispatched_at"],"ended_at":call["completed_at"],"duration_ms":start.zip(end).map(|(s,e)|(e-s).num_milliseconds().max(0)),"model_id":reference["model_id"],"provider":reference["provider_id"],"attributes":{"call_id":call["call_id"],"ledger_status":call["status"],"input_tokens":call["input_tokens"],"output_tokens":call["output_tokens"],"cost_microusd":call["cost_microusd"],"model_ref":reference},"error_type":call["error_code"]}));
    }
    for execution in evidence["tool_executions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        if text(execution, "capability_id").is_empty() {
            continue;
        }
        let mut attributes = serde_json::Map::new();
        for key in ["execution_id", "effect", "approval_status", "quiz_id"] {
            if let Some(value) = execution.get(key).filter(|v| !v.is_null()) {
                attributes.insert(key.into(), value.clone());
            }
        }
        spans.push(json!({"span_id":span_id(run,&format!("capability:{}",text(execution,"execution_id")))?,"span_kind":"tool_execution","name":execution["capability_id"],"sequence_no":spans.len()+1,"status":if execution["status"]=="succeeded"{"succeeded"}else{"failed"},"started_at":execution["dispatched_at"],"ended_at":execution["terminal_at"],"attributes":attributes,"error_type":execution["error_code"]}));
    }
    Ok((fingerprint, usage, spans))
}

/// Shared ai_gateway_core.security.redaction union; distinct from trace_capture preview policy.
pub fn redact_trace_text(value: &str, limit: Option<usize>) -> String {
    let mut result = value.to_owned();
    for (pattern, replacement) in [
        (r#"(?i)([?&](?:key|api_key)=)[^&#\s"']+"#, "${1}[redacted]"),
        (
            r"(?i)\bauthorization\s*[:=]\s*bearer\s+[A-Za-z0-9._~+/=-]+",
            "Authorization: Bearer [redacted]",
        ),
        (r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]+", "Bearer [redacted]"),
        (
            r#"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password)\s*[:=]\s*["']?[^"'\s,;}]+"#,
            "${1}=[redacted]",
        ),
        (
            r#"(?i)(["']?(?:api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password)["']?\s*:\s*)["'][^"']+["']"#,
            "${1}\"[redacted]\"",
        ),
        (
            r#"(?i)\b(postgres|postgresql|mysql|redis)://[^\s"']+"#,
            "${1}://[redacted]",
        ),
        (r"\bsk-[A-Za-z0-9_*.\-]{6,}", "sk-[redacted]"),
        (r"\bAIza[A-Za-z0-9_\-]{20,}", "AIza[redacted]"),
        (r"\bAQ\.[A-Za-z0-9._\-]{6,}", "AQ.[redacted]"),
    ] {
        let pattern = regex::Regex::new(pattern).expect("static redaction expression");
        result = pattern.replace_all(&result, replacement).into_owned();
    }
    if let Some(limit) = limit.filter(|limit| result.chars().count() > *limit) {
        format!(
            "{}...[truncated]",
            result.chars().take(limit).collect::<String>()
        )
    } else {
        result
    }
}
