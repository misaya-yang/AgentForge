//! Bounded trace export projections and defensive credential redaction.
use super::common::{object, text};
use serde_json::{Value, json};
pub fn redaction_policy() -> Value {
    json!({"headers":"authorization, cookie, set-cookie, and api key headers are removed","credentials":"bearer tokens, passwords, secrets, and URL userinfo are redacted","payloads":"export payloads use already-bounded trace previews plus defensive redaction"})
}
pub fn redact_text(value: &str) -> String {
    let mut result = value.to_owned();
    for (pattern, replacement) in [
        (
            r"(?i)(authorization\s*[:=]\s*bearer\s+)[^\s,;]+",
            "${1}[redacted]",
        ),
        (r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]+", "${1}[redacted]"),
        (
            r"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|id[_-]?token|token|password|secret|cookie)\b\s*[:=]\s*[^,\s;&]+",
            "${1}=[redacted]",
        ),
        (r"(?i)(://[^:\s/@]+:)[^@\s/]+(@)", "${1}[redacted]${2}"),
    ] {
        if let Ok(pattern) = regex::Regex::new(pattern) {
            result = pattern.replace_all(&result, replacement).into_owned();
        }
    }
    result
}
pub fn safe_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => json!(
            map.iter()
                .map(|(key, value)| {
                    let normalized = key.to_lowercase().replace(['-', '.'], "_");
                    let sensitive = matches!(
                        normalized.as_str(),
                        "authorization"
                            | "cookie"
                            | "set_cookie"
                            | "api_key"
                            | "apikey"
                            | "x_api_key"
                            | "secret"
                            | "password"
                            | "token"
                            | "access_token"
                            | "refresh_token"
                            | "id_token"
                            | "auth_token"
                            | "bearer_token"
                    ) || normalized.ends_with("_token")
                        && !normalized.ends_with("_tokens");
                    (
                        key.clone(),
                        if sensitive {
                            json!("[redacted]")
                        } else {
                            safe_value(value)
                        },
                    )
                })
                .collect::<serde_json::Map<_, _>>()
        ),
        Value::Array(a) => json!(a.iter().map(safe_value).collect::<Vec<_>>()),
        Value::String(s) => json!(redact_text(s)),
        _ => value.clone(),
    }
}
fn fallback<'a>(v: &'a Value, key: &str, default: &'a str) -> &'a str {
    let s = text(v, key);
    if s.is_empty() { default } else { s }
}
fn attrs(t: &Value) -> Value {
    safe_value(
        &json!({"gen_ai.operation.name":fallback(t,"workflow_kind","chat"),"gen_ai.system":text(t,"provider"),"gen_ai.request.model":text(t,"model_id"),"gen_ai.response.model":text(t,"model_id"),"gen_ai.usage.input_tokens":t["input_tokens"].as_i64().unwrap_or(0),"gen_ai.usage.output_tokens":t["output_tokens"].as_i64().unwrap_or(0),"gen_ai.usage.total_tokens":t["total_tokens"].as_i64().unwrap_or(0),"session.id":fallback(t,"thread_id",text(t,"session_id")),"user.id":text(t,"user_id"),"ai_gateway.trace_family":text(t,"trace_family"),"ai_gateway.request_id":text(t,"request_id"),"ai_gateway.run_id":text(t,"run_id")}),
    )
}
fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
pub fn export_trace(detail: &Value, format: &str) -> Value {
    let trace = &detail["trace"];
    let id = text(trace, "trace_id");
    let spans = arr(&detail["spans"]);
    match format {
        "otel" => {
            let mut attributes = object(&attrs(trace));
            attributes.insert(
                "ai_gateway.input_preview".into(),
                json!(redact_text(text(trace, "input_preview"))),
            );
            attributes.insert(
                "ai_gateway.output_preview".into(),
                json!(redact_text(text(trace, "output_preview"))),
            );
            json!({"resource":{"service.name":"ai-gateway","ai_gateway.schema_version":"ate-03"},"trace":{"trace_id":id,"span_id":id,"parent_span_id":null,"name":fallback(trace,"workflow_kind","ai_assistant_chat"),"kind":"INTERNAL","status":trace["status"],"start_time":trace["started_at"],"end_time":trace["ended_at"],"duration_ns":trace["total_latency_ms"].as_i64().unwrap_or(0)*1_000_000,"attributes":attributes,"events":arr(&detail["events"]).iter().map(|event|json!({"name":event["event_type"],"time":event["occurred_at"],"attributes":safe_value(&json!(object(&event["payload"])))})).collect::<Vec<_>>()},"spans":spans.iter().map(|span|json!({"trace_id":fallback(span,"trace_id",id),"span_id":text(span,"span_id"),"parent_span_id":fallback(span,"parent_span_id",id),"name":fallback(span,"name",text(span,"span_kind")),"kind":"INTERNAL","status":span["status"],"start_time":span["started_at"],"end_time":span["ended_at"],"duration_ns":span["duration_ms"].as_i64().unwrap_or(0)*1_000_000,"attributes":safe_value(&json!(object(&span["attributes"])))})).collect::<Vec<_>>()})
        }
        "openinference" => {
            json!({"trace_id":id,"project_name":"ai-gateway","schema_url":"https://arize-ai.github.io/openinference/spec/semantic_conventions.html","root":{"openinference.span.kind":"AGENT","input.value":redact_text(text(trace,"input_preview")),"output.value":redact_text(text(trace,"output_preview")),"metadata":safe_value(&json!(object(&trace["metadata"]))),"attributes":attrs(trace)},"spans":spans.iter().map(|span|json!({"span_id":text(span,"span_id"),"parent_span_id":span["parent_span_id"],"name":fallback(span,"name",text(span,"span_kind")),"openinference.span.kind":match text(span,"span_kind"){"model_invocation"=>"LLM","tool_execution"=>"TOOL","retriever"=>"RETRIEVER","embedding"=>"EMBEDDING","reranker"=>"RERANKER","lifecycle"=>"AGENT",_=>"CHAIN"},"input.value":redact_text(text(span,"input_preview")),"output.value":redact_text(text(span,"output_preview")),"attributes":safe_value(&json!(object(&span["attributes"]))),"status":span["status"]})).collect::<Vec<_>>()})
        }
        _ => {
            let root = json!({"id":id,"name":fallback(trace,"workflow_kind","ai_assistant_chat"),"run_type":"chain","session_name":trace.get("thread_id").filter(|v|!v.is_null()&&v.as_str()!=Some("")).unwrap_or(&trace["session_id"]),"inputs":{"preview":redact_text(text(trace,"input_preview"))},"outputs":{"preview":redact_text(text(trace,"output_preview"))},"start_time":trace["started_at"],"end_time":trace["ended_at"],"extra":{"metadata":safe_value(&json!(object(&trace["metadata"]))),"tags":[fallback(trace,"trace_family","assistant")],"ai_gateway":attrs(trace)}});
            let mut rows = vec![root];
            rows.extend(spans.iter().map(|span|json!({"id":text(span,"span_id"),"parent_run_id":fallback(span,"parent_span_id",id),"name":fallback(span,"name",text(span,"span_kind")),"run_type":match text(span,"span_kind"){"model_invocation"=>"llm","tool_execution"=>"tool","retriever"=>"retriever","embedding"=>"embedding",_=>"chain"},"inputs":{"preview":redact_text(text(span,"input_preview"))},"outputs":{"preview":redact_text(text(span,"output_preview"))},"start_time":span["started_at"],"end_time":span["ended_at"],"extra":{"metadata":safe_value(&json!(object(&span["attributes"])))}})));
            json!(rows)
        }
    }
}
