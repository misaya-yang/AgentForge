//! Cumulative stream observations flush once, with bounded malformed-event discard.
use super::common::{CompatUser, object, text};
use crate::{
    metrics_redaction::redact_sensitive_text,
    realtime_metrics::RealtimeMetricsService,
    usage_parser::{extract_assistant_id, extract_model, extract_provider, extract_token_usage},
    usage_recorder::{UsageRecord, UsageRecorder, UsageSnapshotResolver},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Instant};
fn number(v: &Value) -> Option<i64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .filter(|v| v.is_finite())
        .map(|v| (v as i64).max(0))
}
fn truncate(v: &str, n: usize) -> String {
    v.chars().take(n).collect()
}
pub fn classify_error(
    status: &str,
    code: Option<u16>,
    kind: &str,
    message: &str,
) -> Option<String> {
    if status.trim().eq_ignore_ascii_case("success") && code.is_none_or(|v| v < 400) {
        return None;
    }
    let value = format!("{} {}", kind.to_lowercase(), message.to_lowercase());
    let has = |tokens: &[&str]| tokens.iter().any(|t| value.contains(t));
    Some(
        if code.is_some_and(|v| matches!(v, 408 | 504))
            || has(&["timeout", "timed out", "deadline exceeded"])
        {
            "timeout"
        } else if code == Some(429)
            || has(&[
                "rate limit",
                "too many requests",
                "quota",
                "throttle",
                "resource exhausted",
                "429",
            ])
        {
            "rate_limit"
        } else if code.is_some_and(|v| matches!(v, 401 | 403))
            || has(&[
                "auth",
                "unauthorized",
                "forbidden",
                "permission denied",
                "invalid api key",
                "token",
            ])
        {
            "auth_error"
        } else if has(&[
            "content filter",
            "safety",
            "blocked by policy",
            "moderation",
            "harm",
            "policy violation",
        ]) {
            "content_filter"
        } else if has(&["tool", "function call", "function_call", "tool_call"]) {
            "tool_error"
        } else if code.is_some_and(|v| v >= 500)
            || has(&[
                "upstream",
                "provider",
                "gateway",
                "bad gateway",
                "service unavailable",
            ])
        {
            "provider_error"
        } else {
            "unknown"
        }
        .into(),
    )
}
pub fn extract_duration_breakdown(payload: &Value) -> Value {
    fn walk(v: &Value, flat: &mut Vec<(String, Value)>, tools: &mut BTreeMap<String, i64>) {
        match v {
            Value::Object(map) => {
                let name = if text(v, "tool_name").is_empty() {
                    text(v, "name")
                } else {
                    text(v, "tool_name")
                };
                let duration = ["duration_ms", "tool_call_duration_ms", "elapsed_ms"]
                    .iter()
                    .find_map(|k| v.get(*k).filter(|v| !v.is_null() && number(v) != Some(0)))
                    .and_then(number)
                    .unwrap_or(0);
                if !name.is_empty() && duration > 0 {
                    *tools.entry(name.into()).or_default() += duration;
                }
                for (k, v) in map {
                    if !k.trim().is_empty() {
                        flat.push((k.trim().to_lowercase(), v.clone()));
                        walk(v, flat, tools);
                    }
                }
            }
            Value::Array(a) => {
                for v in a {
                    walk(v, flat, tools)
                }
            }
            _ => {}
        }
    }
    let (mut flat, mut tools) = (vec![], BTreeMap::new());
    walk(payload, &mut flat, &mut tools);
    let aliases: [(&str, &[&str]); 5] = [
        (
            "first_token_latency_ms",
            &[
                "first_token_latency_ms",
                "first_token_ms",
                "ttfb_ms",
                "time_to_first_token_ms",
            ],
        ),
        (
            "llm_inference_duration_ms",
            &[
                "llm_inference_duration_ms",
                "llm_inference_ms",
                "model_inference_ms",
                "llm_latency_ms",
            ],
        ),
        (
            "retrieval_duration_ms",
            &[
                "retrieval_duration_ms",
                "retrieval_ms",
                "knowledge_retrieval_ms",
                "search_knowledge_duration_ms",
                "rag_retrieval_ms",
            ],
        ),
        (
            "tool_call_duration_ms",
            &[
                "tool_call_duration_ms",
                "tool_duration_ms",
                "tools_duration_ms",
                "tool_calls_duration_ms",
            ],
        ),
        (
            "agent_or_graph_overhead_ms",
            &[
                "agent_or_graph_overhead_ms",
                "agent_overhead_ms",
                "graph_overhead_ms",
                "orchestration_overhead_ms",
            ],
        ),
    ];
    let mut result = json!({});
    for (key, names) in aliases {
        if let Some(number) = flat
            .iter()
            .filter(|(k, _)| names.contains(&k.as_str()))
            .find_map(|(_, v)| number(v))
        {
            result[key] = json!(number);
        }
    }
    if !tools.is_empty() {
        if result.get("tool_call_duration_ms").is_none() {
            result["tool_call_duration_ms"] = json!(tools.values().sum::<i64>());
        }
        result["tool_call_breakdown"] = json!(tools);
    }
    result
}
pub fn ensure_duration_breakdown(
    total: i64,
    first: i64,
    llm: i64,
    retrieval: i64,
    tool: i64,
    overhead: i64,
) -> Value {
    let (total, mut first, llm, retrieval, tool, mut overhead) = (
        total.max(0),
        first.max(0),
        llm.max(0),
        retrieval.max(0),
        tool.max(0),
        overhead.max(0),
    );
    let consumed = llm + retrieval + tool + overhead;
    if total > 0 && consumed > total {
        overhead = (overhead - (consumed - total)).max(0);
    }
    let consumed = llm + retrieval + tool + overhead;
    if total > 0 && consumed < total {
        overhead += total - consumed;
    }
    if first <= 0 && total > 0 {
        first = total.min(if llm > 0 { llm } else { total });
    }
    json!({"request_total_duration_ms":total,"first_token_latency_ms":first,"llm_inference_duration_ms":llm,"retrieval_duration_ms":retrieval,"tool_call_duration_ms":tool,"agent_or_graph_overhead_ms":overhead})
}
pub struct StreamAccounting {
    pub user: CompatUser,
    pub service: String,
    pub assistant: String,
    pub request_type: String,
    pub model_hint: String,
    pub provider_hint: String,
    pub status: String,
    pub recorder: Option<Arc<UsageRecorder>>,
    pub pricing: Option<Arc<dyn UsageSnapshotResolver>>,
    pub realtime: Option<Arc<RealtimeMetricsService>>,
    start: Instant,
    buffer: String,
    discarding: bool,
    started: bool,
    first: i64,
    offsets: Vec<(String, i64)>,
    durations: Value,
    errors: Value,
    pending: Option<UsageRecord>,
    collected: bool,
    finished: bool,
}
impl StreamAccounting {
    pub fn new(
        user: CompatUser,
        service: String,
        assistant: String,
        request_type: String,
        model_hint: String,
        provider_hint: String,
        recorder: Option<Arc<UsageRecorder>>,
        pricing: Option<Arc<dyn UsageSnapshotResolver>>,
        realtime: Option<Arc<RealtimeMetricsService>>,
    ) -> Self {
        Self {
            user,
            service,
            assistant,
            request_type,
            model_hint,
            provider_hint,
            status: "success".into(),
            recorder,
            pricing,
            realtime,
            start: Instant::now(),
            buffer: String::new(),
            discarding: false,
            started: false,
            first: 0,
            offsets: vec![],
            durations: json!({}),
            errors: json!({}),
            pending: None,
            collected: false,
            finished: false,
        }
    }
    pub fn mark_error(&mut self, kind: &str, message: &str) {
        self.status = "error".into();
        self.errors["upstream_error_type"] = json!(truncate(kind, 128));
        if !message.is_empty() {
            self.errors["upstream_error_message"] =
                json!(truncate(&redact_sensitive_text(message), 512));
        }
    }
    pub async fn process_chunk(&mut self, bytes: &[u8]) {
        if !self.started {
            self.started = true;
            if let Some(realtime) = &self.realtime {
                realtime
                    .record_request_start(
                        &self.user.request_id,
                        &self.user.user_id,
                        &self.user.tenant_id,
                    )
                    .await;
            }
        }
        if self.first <= 0 {
            self.first = self.start.elapsed().as_millis().max(1) as i64;
        }
        let mut remaining = bytes;
        let mut decoded = String::new();
        while !remaining.is_empty() {
            match std::str::from_utf8(remaining) {
                Ok(text) => {
                    decoded.push_str(text);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    decoded.push_str(std::str::from_utf8(&remaining[..valid]).unwrap_or(""));
                    let Some(length) = error.error_len() else {
                        break;
                    };
                    remaining = &remaining[valid + length..];
                }
            }
        }
        let decoded = decoded.replace("\r\n", "\n").replace('\r', "\n");
        self.buffer.push_str(&decoded);
        if self.discarding {
            if let Some(end) = self.buffer.find("\n\n") {
                self.buffer = self.buffer[end + 2..].into();
                self.discarding = false;
            } else {
                self.buffer = if self.buffer.ends_with('\n') {
                    "\n".into()
                } else {
                    String::new()
                };
                return;
            }
        }
        while let Some(end) = self.buffer.find("\n\n") {
            let block = self.buffer[..end].to_owned();
            self.buffer = self.buffer[end + 2..].into();
            self.event(&block);
        }
        if self.buffer.chars().count() > 1048576 {
            if !self.collected {
                self.mark_error("billing_stream_buffer_overflow", "");
            }
            self.discarding = true;
            self.buffer = if self.buffer.ends_with('\n') {
                "\n".into()
            } else {
                String::new()
            };
        }
    }
    fn event(&mut self, block: &str) {
        let mut event = "";
        let mut data = vec![];
        for line in block.split('\n') {
            if let Some(value) = line.strip_prefix("event:") {
                event = value.trim();
            } else if let Some(value) = line.strip_prefix("data:") {
                data.push(value.trim());
            }
        }
        if event.eq_ignore_ascii_case("error") {
            self.mark_error("stream_error_event", "");
        } else if !event.is_empty() {
            let lower = event.to_lowercase();
            if !self.offsets.iter().any(|(v, _)| v == &lower) {
                self.offsets
                    .push((lower, self.start.elapsed().as_millis() as i64));
            }
        }
        let raw = data.join("\n");
        if raw.is_empty() || raw == "[DONE]" {
            return;
        }
        let payload = match serde_json::from_str::<Value>(&raw) {
            Ok(v) => v,
            Err(_) => {
                if event.eq_ignore_ascii_case("error") {
                    self.mark_error("stream_error_event", &raw);
                }
                return;
            }
        };
        if payload.is_object() {
            if !text(&payload, "error").is_empty() && payload.get("message").is_some() {
                self.mark_error(text(&payload, "error"), text(&payload, "message"));
            } else if let Some(error) = payload
                .get("__error__")
                .or_else(|| payload.get("error"))
                .filter(|v| !v.is_null() && v.as_bool() != Some(false) && v.as_str() != Some(""))
            {
                if error.is_object() {
                    self.mark_error(
                        if text(error, "error").is_empty() {
                            if text(error, "type").is_empty() {
                                "upstream_error"
                            } else {
                                text(error, "type")
                            }
                        } else {
                            text(error, "error")
                        },
                        if text(error, "message").is_empty() {
                            text(error, "detail")
                        } else {
                            text(error, "message")
                        },
                    );
                } else {
                    self.mark_error("upstream_error", error.as_str().unwrap_or("upstream_error"));
                }
            }
            let extracted = extract_duration_breakdown(&payload);
            for (key, value) in object(&extracted) {
                if key == "tool_call_breakdown" {
                    let mut tools = object(&self.durations[&key]);
                    for (name, value) in object(&value) {
                        let n = number(&value).unwrap_or(0);
                        if n > 0 {
                            let old = tools.get(&name).and_then(number).unwrap_or(0);
                            tools.insert(name, json!(old + n));
                        }
                    }
                    self.durations[&key] = json!(tools);
                } else if let Some(value) = number(&value).filter(|v| *v > 0) {
                    self.durations[&key] =
                        json!(value.max(number(&self.durations[&key]).unwrap_or(0)));
                }
            }
        }
        if let Ok(Some(usage)) = extract_token_usage(&payload) {
            self.collected = true;
            let mut record = self.record(&payload, usage.input_tokens, usage.output_tokens);
            if let Some(pending) = &mut self.pending {
                pending.input_tokens = pending.input_tokens.max(record.input_tokens);
                pending.output_tokens = pending.output_tokens.max(record.output_tokens);
                pending.latency_ms = pending.latency_ms.max(record.latency_ms);
                pending.first_token_ms = pending.first_token_ms.max(record.first_token_ms);
                pending.request_total_duration_ms = pending
                    .request_total_duration_ms
                    .max(record.request_total_duration_ms);
                pending.llm_inference_duration_ms = pending
                    .llm_inference_duration_ms
                    .max(record.llm_inference_duration_ms);
                pending.retrieval_duration_ms = pending
                    .retrieval_duration_ms
                    .max(record.retrieval_duration_ms);
                pending.tool_call_duration_ms = pending
                    .tool_call_duration_ms
                    .max(record.tool_call_duration_ms);
                pending.agent_or_graph_overhead_ms = pending
                    .agent_or_graph_overhead_ms
                    .max(record.agent_or_graph_overhead_ms);
                pending.status = record.status;
                pending.error_type = record.error_type;
                pending.trace_steps = record.trace_steps;
                pending.tool_call_breakdown = record.tool_call_breakdown;
                pending.metadata.extend(record.metadata);
            } else {
                record
                    .metadata
                    .entry("token_source")
                    .or_insert(json!("upstream"));
                self.pending = Some(record);
            }
        }
    }
    fn record(&self, payload: &Value, incoming: i64, outgoing: i64) -> UsageRecord {
        let model = extract_model(payload)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                if self.model_hint.is_empty() {
                    "langgraph-agent".into()
                } else {
                    self.model_hint.clone()
                }
            });
        let provider = extract_provider(payload)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                if self.provider_hint.is_empty() {
                    "unattributed".into()
                } else {
                    self.provider_hint.clone()
                }
            });
        let assistant = if self.assistant.is_empty() {
            extract_assistant_id(payload).unwrap_or_default()
        } else {
            self.assistant.clone()
        };
        let elapsed = self.start.elapsed().as_millis() as i64;
        let n = |k: &str| number(&self.durations[k]).unwrap_or(0);
        let durations = ensure_duration_breakdown(
            elapsed,
            self.durations
                .get("first_token_latency_ms")
                .and_then(number)
                .unwrap_or(self.first),
            n("llm_inference_duration_ms"),
            n("retrieval_duration_ms"),
            n("tool_call_duration_ms"),
            n("agent_or_graph_overhead_ms"),
        );
        let mut record = UsageRecord::new(
            &self.user.tenant_id,
            &self.user.user_id,
            &model,
            chrono::Utc::now(),
        );
        record.request_id = self.user.request_id.clone();
        record.service_id = self.service.clone();
        record.assistant_id = assistant;
        record.provider = provider;
        record.request_type = self.request_type.clone();
        record.status = self.status.clone();
        record.input_tokens = incoming;
        record.output_tokens = outgoing;
        record.latency_ms = elapsed;
        record.first_token_ms = durations["first_token_latency_ms"].as_i64().unwrap_or(0);
        record.request_total_duration_ms = elapsed;
        record.llm_inference_duration_ms =
            durations["llm_inference_duration_ms"].as_i64().unwrap_or(0);
        record.retrieval_duration_ms = durations["retrieval_duration_ms"].as_i64().unwrap_or(0);
        record.tool_call_duration_ms = durations["tool_call_duration_ms"].as_i64().unwrap_or(0);
        record.agent_or_graph_overhead_ms = durations["agent_or_graph_overhead_ms"]
            .as_i64()
            .unwrap_or(0);
        record.tool_call_breakdown = object(&self.durations["tool_call_breakdown"]);
        record.error_type = classify_error(
            &self.status,
            None,
            text(&self.errors, "upstream_error_type"),
            text(&self.errors, "upstream_error_message"),
        );
        record.metadata = if payload.is_object() {
            object(payload)
        } else {
            object(&json!({"raw_data":payload}))
        };
        record.metadata.extend(object(&self.errors));
        for (key, value) in [
            ("source", json!("transparent_proxy_stream")),
            ("request_type", json!(self.request_type)),
            ("provider", json!(record.provider)),
            ("model", json!(record.model)),
            ("token_source", json!("upstream")),
        ] {
            record.metadata.entry(key).or_insert(value);
        }
        record
            .metadata
            .insert("error_type".into(), json!(record.error_type));
        record
            .metadata
            .insert("duration_breakdown".into(), durations.clone());
        record.metadata.insert(
            "tool_call_breakdown".into(),
            json!(record.tool_call_breakdown),
        );
        record.trace_steps = vec![
            json!({"name":"gateway.request_total","start_offset_ms":0,"duration_ms":elapsed,"status":self.status}),
        ];
        let mut offset = 0;
        for (name, key) in [
            ("agent.retrieval", "retrieval_duration_ms"),
            ("agent.tool_calls", "tool_call_duration_ms"),
            ("llm.inference", "llm_inference_duration_ms"),
            ("agent.overhead", "agent_or_graph_overhead_ms"),
        ] {
            let n = durations[key].as_i64().unwrap_or(0);
            if n > 0 {
                record.trace_steps.push(json!({"name":name,"start_offset_ms":offset,"duration_ms":n,"status":"success"}));
                offset += n;
            }
        }
        if record.first_token_ms > 0 {
            record.trace_steps.push(json!({"name":"llm.first_token","start_offset_ms":0,"duration_ms":record.first_token_ms,"status":"success"}));
        }
        let mut offsets = self.offsets.clone();
        offsets.sort_by_key(|(_, offset)| *offset);
        for (name, offset) in offsets {
            record.trace_steps.push(json!({"name":format!("stream.event.{name}"),"start_offset_ms":offset,"duration_ms":0,"status":"success"}));
        }
        record
    }
    pub async fn finalize(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let mut record = self.pending.take();
        if record.is_none()
            && !self.collected
            && (self.request_type.starts_with("proxy_run_")
                || self.request_type.starts_with("run_"))
        {
            let mut fallback = self.record(&json!({}), 0, 0);
            fallback.metadata.insert(
                "token_source".into(),
                json!(if self.status == "success" {
                    "estimated"
                } else {
                    "zero_on_failure"
                }),
            );
            record = Some(fallback);
        }
        if let Some(mut record) = record {
            record.status = self.status.clone();
            record.error_type = classify_error(
                &self.status,
                None,
                text(&self.errors, "upstream_error_type"),
                text(&self.errors, "upstream_error_message"),
            );
            record.metadata.extend(object(&self.errors));
            record
                .metadata
                .insert("error_type".into(), json!(record.error_type));
            if self.collected {
                if let Some(realtime) = &self.realtime {
                    realtime
                        .record_token_usage(record.input_tokens, record.output_tokens)
                        .await;
                }
            }
            if let Some(recorder) = &self.recorder {
                if let Some(pricing) = &self.pricing {
                    let _ = recorder.record_with_resolver(record, pricing.clone()).await;
                } else {
                    recorder.record_priced(record).await;
                }
            }
        }
        if let Some(realtime) = &self.realtime {
            realtime
                .record_request_complete(
                    if self.status == "success" { 200 } else { 500 },
                    self.start.elapsed().as_millis() as f64,
                    &self.user.user_id,
                )
                .await;
            if self.started {
                realtime
                    .record_request_end(
                        &self.user.request_id,
                        &self.user.user_id,
                        &self.user.tenant_id,
                    )
                    .await;
            }
        }
    }
}
