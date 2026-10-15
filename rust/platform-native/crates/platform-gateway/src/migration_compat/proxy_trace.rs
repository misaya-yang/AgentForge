use super::{common::CompatUser, eval_observation::redact_preview};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
pub fn build_langgraph_trace_payload(
    user: &CompatUser,
    method: &str,
    path: &str,
    started: DateTime<Utc>,
    ended: DateTime<Utc>,
    status: &str,
    upstream_status: Option<u16>,
    error: Option<&str>,
    traceparent: Option<&str>,
    streaming: bool,
) -> Value {
    let parts = path
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    let thread =
        if parts.len() >= 2 && parts[0] == "threads" && !matches!(parts[1], "search" | "count") {
            Some(parts[1])
        } else {
            None
        };
    let assistant = if parts.len() >= 2
        && parts[0] == "assistants"
        && !matches!(parts[1], "search" | "count")
    {
        Some(parts[1])
    } else {
        None
    };
    let run = parts
        .iter()
        .position(|v| *v == "runs")
        .and_then(|index| parts.get(index + 1).copied())
        .filter(|s| !matches!(*s, "stream" | "wait"));
    let request = if user.request_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        user.request_id.clone()
    };
    let trace = uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!("langgraph_proxy:{request}:{method}:{path}").as_bytes(),
    );
    let span = |key: &str| uuid::Uuid::new_v5(&trace, key.as_bytes()).to_string();
    let lifecycle = span("lifecycle");
    let duration = (ended - started).num_milliseconds().max(0);
    let otel = traceparent
        .filter(|v| v.starts_with("00-"))
        .and_then(|v| v.split('-').nth(1))
        .filter(|v| !v.is_empty());
    let error = error
        .filter(|s| !s.is_empty())
        .map(|s| redact_preview(s, 500));
    json!({"trace_id":trace.to_string(),"trace_family":"langgraph_proxy","workflow_kind":"langgraph_agent_run","tenant_id":user.tenant_id,"user_id":user.user_id,"thread_id":thread,"session_id":thread,"run_id":run,"request_id":user.request_id,"otel_trace_id":otel,"traceparent":traceparent,"status":status,"started_at":started.to_rfc3339(),"ended_at":ended.to_rfc3339(),"input_preview":redact_preview(&format!("{method} {path}"),500),"output_preview":format!("upstream_status={} streaming={}",upstream_status.map(|s|s.to_string()).unwrap_or_else(||"n/a".into()),if streaming{"True"}else{"False"}),"metrics":{"total_latency_ms":duration,"upstream_status":upstream_status,"streaming":streaming},"privacy":{"payloads":"bounded_redacted_preview"},"redaction_state":{"headers":"stripped","body":"not_persisted"},"metadata":{"upstream_route":path,"http_method":method,"assistant_id":assistant,"error_summary":error},"source_adapter":"gateway.langgraph_proxy","spans":[{"span_id":lifecycle,"parent_span_id":null,"span_kind":"lifecycle","name":"langgraph_proxy_run","status":status,"sequence_no":0,"started_at":started.to_rfc3339(),"ended_at":ended.to_rfc3339(),"duration_ms":duration,"attributes":{"upstream_route":path,"http_method":method,"upstream_status":upstream_status,"streaming":streaming}},{"span_id":span("gateway_proxy"),"parent_span_id":lifecycle,"span_kind":"gateway_proxy","name":"upstream_request","status":status,"sequence_no":1,"started_at":started.to_rfc3339(),"ended_at":ended.to_rfc3339(),"duration_ms":duration,"attributes":{"thread_id":thread,"run_id":run,"assistant_id":assistant},"error_message":error}],"events":[{"event_type":"proxy_request_accepted","sequence_no":1,"payload":{"method":method,"upstream_path":path,"streaming":streaming}},{"event_type":"proxy_request_finished","sequence_no":2,"payload":{"status":status,"upstream_status":upstream_status,"duration_ms":duration}}]})
}
