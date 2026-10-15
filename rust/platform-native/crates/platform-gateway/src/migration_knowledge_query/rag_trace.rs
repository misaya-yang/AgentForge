//! Canonical bounded RAG observations. Persistence and outbox scheduling stay in
//! TraceRepository. Truncated or unfinished response streams never create traces.
use super::types::UserContext;
use crate::{migration_compat::eval_observation::redact_preview, migration_trace::TraceRepository};
use axum::{
    body::{Body, Bytes, to_bytes},
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use chrono::{DateTime, Utc};
use http_body::{Body as HttpBody, Frame, SizeHint};
use serde_json::{Value, json};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
const RESPONSE_LIMIT: usize = 256 * 1024;
#[derive(Clone)]
pub struct RagTraceCapture {
    pub repository: Arc<TraceRepository>,
    pub eval_context_max_chars: usize,
    pub eval_context_max_chunks: usize,
    pub retention_days: i32,
}
impl RagTraceCapture {
    pub fn new(repository: Arc<TraceRepository>) -> Self {
        Self {
            repository,
            eval_context_max_chars: 1500,
            eval_context_max_chunks: 8,
            retention_days: 90,
        }
    }
}
pub fn is_retrieve_path(path: &str) -> bool {
    let p = path.trim_matches('/');
    ["retrieve", "retrieve_batch", "hit_test", "qa", "qa/stream"]
        .iter()
        .any(|s| p.ends_with(s))
}
pub fn dataset_id(path: &str) -> Option<String> {
    let parts: Vec<_> = path
        .trim_matches('/')
        .split('/')
        .filter(|p| !p.is_empty())
        .collect();
    if let Some(i) = parts.iter().position(|p| *p == "knowledge") {
        if let Some(id) = parts.get(i + 1) {
            return Some((*id).into());
        }
    }
    if parts.len() >= 3 && parts[parts.len() - 2..] == ["qa", "stream"] {
        return Some(parts[parts.len() - 3].into());
    }
    if parts.len() >= 2
        && ["retrieve", "retrieve_batch", "hit_test", "qa"].contains(parts.last().unwrap())
    {
        return Some(parts[parts.len() - 2].into());
    }
    if parts.len() == 1 && parts[0] != "retrieve" {
        return Some(parts[0].into());
    }
    None
}
pub fn parse_request(body: &[u8]) -> String {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    v["query"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or(v["text"].as_str())
        .unwrap_or("")
        .into()
}
pub fn parse_response(body: &[u8]) -> (Vec<Value>, Value) {
    if body.is_empty() {
        return (vec![], json!({}));
    }
    let payload = match serde_json::from_slice::<Value>(body) {
        Ok(p) => p,
        Err(_) => {
            let mut terminal = None;
            let mut retrieval = None;
            for line in String::from_utf8_lossy(body).lines() {
                let Some(raw) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<Value>(raw.trim()) else {
                    continue;
                };
                if event["event"] == "done" && event["data"]["result"].is_object() {
                    terminal = Some(event["data"]["result"].clone())
                } else if event["event"] == "retrieval" && event["data"].is_object() {
                    retrieval = Some(event["data"].clone())
                }
            }
            match terminal.or(retrieval) {
                Some(p) => p,
                None => return (vec![], json!({})),
            }
        }
    };
    parse_payload(payload)
}
fn parse_payload(mut p: Value) -> (Vec<Value>, Value) {
    if let Some(items) = p.as_array() {
        return (
            items.iter().filter(|v| v.is_object()).cloned().collect(),
            json!({}),
        );
    }
    if !p.is_object() {
        return (vec![], json!({}));
    }
    let mut results = p.get("results").filter(|v| v.is_array()).cloned();
    if results.is_none() {
        if let Some(first) = p["batch_results"]
            .as_array()
            .and_then(|a| a.iter().find(|v| v.is_object()))
            .cloned()
        {
            results = first.get("results").filter(|v| v.is_array()).cloned();
            if !p["metadata"].is_object() {
                p["metadata"] = first["meta"].clone()
            }
        }
    }
    if results.is_none() {
        results = p.get("context_segments").filter(|v| v.is_array()).cloned()
    }
    if results.is_none() {
        results = p.get("chunks").filter(|v| v.is_array()).cloned()
    }
    let mut metadata = if p["metadata"].is_object() {
        p["metadata"].clone()
    } else if p["retrieval_metadata"].is_object() {
        p["retrieval_metadata"].clone()
    } else {
        json!({})
    };
    for key in ["trace_id", "query_fingerprint"] {
        if p[key].as_str().is_some_and(|s| !s.is_empty()) {
            metadata[key] = p[key].clone()
        }
    }
    (
        results
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter(|v| v.is_object())
            .collect(),
        metadata,
    )
}
pub fn retrieval_documents(results: &[Value], max_chunks: usize, max_chars: usize) -> Vec<Value> {
    results.iter().take(max_chunks.max(1)).enumerate().filter_map(|(index,r)|{if !r.is_object(){return None}let text=r["text"].as_str().filter(|s|!s.is_empty()).or(r["content"].as_str()).unwrap_or("").trim();if text.is_empty(){return None}Some(json!({"rank":index+1,"segment_id":r["segment_id"],"document_id":r["document_id"],"score":r["score"],"content_eval":redact_preview(text,max_chars.max(1)),"content_preview":redact_preview(text,360)}))}).collect()
}
pub struct RagTraceInput<'a> {
    pub request_id: &'a str,
    pub tenant_id: &'a str,
    pub user_id: &'a str,
    pub dataset_id: Option<&'a str>,
    pub query: &'a str,
    pub started: DateTime<Utc>,
    pub ended: DateTime<Utc>,
    pub status: &'a str,
    pub upstream_status: Option<u16>,
    pub document_count: usize,
    pub error: Option<&'a str>,
    pub traceparent: Option<&'a str>,
    pub documents: Vec<Value>,
    pub metadata: Value,
    pub trace_id: Option<&'a str>,
}
pub fn build_rag_trace(input: RagTraceInput<'_>) -> Value {
    let t = input;
    let request_seed = if t.request_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        t.request_id.into()
    };
    let trace = t
        .trace_id
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .unwrap_or_else(|| {
            uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!(
                    "rag:{request_seed}:retrieve:{}:{}",
                    t.dataset_id.unwrap_or("unknown"),
                    t.query.chars().take(80).collect::<String>()
                )
                .as_bytes(),
            )
        });
    let lifecycle = uuid::Uuid::new_v5(&trace, b"lifecycle").to_string();
    let retriever = uuid::Uuid::new_v5(&trace, b"retriever").to_string();
    let duration = (t.ended - t.started).num_milliseconds().max(0);
    let started = t
        .started
        .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, false);
    let ended = t
        .ended
        .to_rfc3339_opts(chrono::SecondsFormat::AutoSi, false);
    let count = if t.document_count == 0 {
        t.documents.len()
    } else {
        t.document_count
    };
    let query = redact_preview(t.query, 500);
    let error = t.error.map(|e| redact_preview(e, 500));
    let datasets = t.dataset_id.map(|id| vec![id]).unwrap_or_default();
    let otel = t
        .traceparent
        .filter(|p| p.starts_with("00-"))
        .and_then(|p| p.split('-').nth(1))
        .filter(|s| !s.is_empty());
    let retrieval = json!({"documents":t.documents,"mode":t.metadata["mode"],"pipeline_stages":t.metadata["pipeline_stages"]});
    let metadata = json!({"dataset_id":t.dataset_id,"answer_source":"retrieval_only","gen_ai.retrieval.query.text":query,"retrieval.dataset_ids":datasets,"error_summary":error,"retrieval":{"mode":t.metadata["mode"],"pipeline_stages":t.metadata["pipeline_stages"],"fusion_method":t.metadata["fusion_method"],"rerank":t.metadata["rerank"]}});
    json!({"trace_id":trace.to_string(),"trace_family":"rag","workflow_kind":"rag_retrieval_chain","tenant_id":t.tenant_id,"user_id":t.user_id,"request_id":t.request_id,"otel_trace_id":otel,"traceparent":t.traceparent,"status":t.status,"started_at":started,"ended_at":ended,"input_preview":query,"output_preview":redact_preview(&format!("{count} retrieved documents"),500),"metrics":{"total_latency_ms":duration,"retrieval.document_count":count,"upstream_status":t.upstream_status},"privacy":{"payloads":"bounded_redacted_preview","eval_contexts":"bounded_eval_only"},"redaction_state":{"chunks":"bounded_eval_context"},"metadata":metadata,"source_adapter":"gateway.knowledge_proxy","spans":[{"span_id":lifecycle,"parent_span_id":null,"span_kind":"lifecycle","name":"rag_retrieval_chain","status":t.status,"sequence_no":0,"started_at":started,"ended_at":ended,"duration_ms":duration},{"span_id":retriever,"parent_span_id":lifecycle,"span_kind":"retriever","name":"rag_retrieval","status":t.status,"sequence_no":1,"started_at":started,"ended_at":ended,"duration_ms":duration,"input_preview":query,"output_preview":redact_preview(&format!("{count} documents"),500),"attributes":{"openinference.span.kind":"RETRIEVER","retrieval.dataset_ids":datasets,"retrieval.document_count":count,"retrieval":retrieval},"error_message":error}],"events":[{"event_type":"rag_retrieval_started","sequence_no":1,"payload":{"query":query,"dataset_id":t.dataset_id}},{"event_type":if t.status=="succeeded"{"rag_retrieval_completed"}else{"rag_retrieval_failed"},"sequence_no":2,"payload":{"document_count":count,"duration_ms":duration,"error":error}}]})
}
struct Observation {
    config: RagTraceCapture,
    user: UserContext,
    path: String,
    query: String,
    request_id: String,
    traceparent: Option<String>,
    started: DateTime<Utc>,
    status: u16,
    captured: Vec<u8>,
    truncated: bool,
    recorded: bool,
}
impl Observation {
    fn append(&mut self, bytes: &[u8]) {
        let remaining = RESPONSE_LIMIT.saturating_sub(self.captured.len());
        if bytes.len() > remaining {
            self.truncated = true
        }
        self.captured
            .extend_from_slice(&bytes[..bytes.len().min(remaining)]);
    }
    fn finish(&mut self) {
        if self.recorded || self.truncated {
            return;
        }
        self.recorded = true;
        let (results, metadata) = parse_response(&self.captured);
        let documents = retrieval_documents(
            &results,
            self.config.eval_context_max_chunks,
            self.config.eval_context_max_chars,
        );
        let trace_id = metadata["trace_id"].as_str().map(str::to_owned);
        let dataset = dataset_id(&self.path);
        let succeeded = (200..400).contains(&self.status);
        let error = (!succeeded).then(|| format!("upstream_status={}", self.status));
        let trace = build_rag_trace(RagTraceInput {
            request_id: &self.request_id,
            tenant_id: &self.user.tenant_id,
            user_id: &self.user.user_id,
            dataset_id: dataset.as_deref(),
            query: &self.query,
            started: self.started,
            ended: Utc::now(),
            status: if succeeded { "succeeded" } else { "failed" },
            upstream_status: Some(self.status),
            document_count: documents.len(),
            error: error.as_deref(),
            traceparent: self.traceparent.as_deref(),
            documents,
            metadata,
            trace_id: trace_id.as_deref(),
        });
        self.config.repository.schedule_gateway_trace_ingest(
            trace,
            self.config.retention_days,
            true,
        );
    }
}
struct CaptureBody {
    inner: Body,
    observation: Observation,
}
impl HttpBody for CaptureBody {
    type Data = Bytes;
    type Error = axum::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Self::Data>, Self::Error>>> {
        let frame = Pin::new(&mut self.inner).poll_frame(cx);
        match &frame {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    self.observation.append(data)
                }
                if self.inner.is_end_stream() {
                    self.observation.finish()
                }
            }
            Poll::Ready(None) => self.observation.finish(),
            Poll::Ready(Some(Err(_))) => self.observation.truncated = true,
            _ => {}
        }
        frame
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
/// Mount inside verified JWT middleware, so identity comes only from the
/// authenticated extension. This middleware never interprets identity headers.
pub async fn capture(
    State(config): State<RagTraceCapture>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    let Some(user) = request
        .extensions()
        .get::<UserContext>()
        .cloned()
        .filter(|_| is_retrieve_path(&path))
    else {
        return next.run(request).await;
    };
    let started = Utc::now();
    let context = request
        .extensions()
        .get::<crate::migration_host::middleware::RequestContext>();
    let request_id = context.map(|c| c.request_id.clone()).unwrap_or_default();
    let traceparent = context
        .map(|c| c.traceparent.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            request
                .headers()
                .get("traceparent")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        });
    let (parts, body) = request.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(_) => return axum::http::StatusCode::BAD_REQUEST.into_response(),
    };
    let query = parse_request(&bytes);
    let response = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    let (parts, body) = response.into_parts();
    let mut observation = Observation {
        config,
        user,
        path,
        query,
        request_id,
        traceparent,
        started,
        status: parts.status.as_u16(),
        captured: vec![],
        truncated: false,
        recorded: false,
    };
    if body.is_end_stream() {
        observation.finish()
    }
    Response::from_parts(
        parts,
        Body::new(CaptureBody {
            inner: body,
            observation,
        }),
    )
}
use axum::response::IntoResponse;
