//! Terminal-only observation; disconnect never creates a guessed Runtime result.
use super::*;
use crate::migration_compat::eval_observation::{RuntimeTraceInput, build_assistant_runtime_trace};
use chrono::{DateTime, Utc};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::{mpsc, watch};
#[derive(Clone)]
pub struct CaptureContext {
    pub run_id: String,
    pub request_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub message: String,
    pub snapshot: Value,
    pub started_at: DateTime<Utc>,
    pub first_token_latency_ms: Option<i64>,
}
pub type FrameReceiver = mpsc::Receiver<Result<Vec<u8>, String>>;
pub type ReplaySource = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<FrameReceiver, String>> + Send>> + Send + Sync,
>;
pub fn terminal_status(kind: &str, data: &Value) -> Option<&'static str> {
    if !["run_finished", "run_error", "cancelled"].contains(&kind) {
        return None;
    }
    let raw = text(data, "status").to_lowercase();
    Some(if kind == "cancelled" || raw == "cancelled" {
        "cancelled"
    } else if kind == "run_finished" && ["completed", "succeeded"].contains(&raw.as_str()) {
        "succeeded"
    } else {
        "failed"
    })
}
fn parsed(v: &Value) -> Value {
    if let Some(s) = v.as_str() {
        serde_json::from_str(s).unwrap_or(json!({}))
    } else if v.is_object() {
        v.clone()
    } else {
        json!({})
    }
}
fn frame_event(frame: &[u8]) -> (String, Value) {
    let raw = String::from_utf8_lossy(frame);
    let kind = raw
        .lines()
        .find_map(|l| l.strip_prefix("event:"))
        .unwrap_or("")
        .trim()
        .to_owned();
    let data = raw
        .lines()
        .find_map(|l| l.strip_prefix("data:"))
        .unwrap_or("")
        .trim();
    let envelope: Value = serde_json::from_str(data).unwrap_or(Value::Null);
    // V2 transports the original Runtime envelope inside an item event. Only
    // observation unwraps it; the receiver still forwards the original bytes.
    if kind == "item" && envelope["schema_version"] == "agent-event/v2" {
        let payload = &envelope["event"]["payload"];
        let event_type = payload["event_type"].as_str().unwrap_or("");
        let event_type = event_type.strip_prefix("compat/v1/").unwrap_or(event_type);
        return (
            event_type.to_owned(),
            payload
                .get("data")
                .filter(|v| v.is_object())
                .cloned()
                .unwrap_or(json!({})),
        );
    }
    (
        kind,
        envelope
            .get("data")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(json!({})),
    )
}
struct Observation {
    context: CaptureContext,
    status: &'static str,
    first: Option<DateTime<Utc>>,
    output: String,
    counts: Value,
    usage: Value,
    error: Option<String>,
}
impl Observation {
    fn new(context: CaptureContext) -> Self {
        Self {
            context,
            status: "running",
            first: None,
            output: String::new(),
            counts: json!({}),
            usage: json!({}),
            error: None,
        }
    }
    fn observe(&mut self, frame: &[u8]) {
        let (kind, data) = frame_event(frame);
        if !kind.is_empty() {
            self.counts[&kind] = json!(self.counts[&kind].as_i64().unwrap_or(0) + 1);
        }
        if kind == "text_delta" {
            if let Some(content) = data["content"].as_str() {
                self.first.get_or_insert_with(Utc::now);
                self.output.extend(
                    content
                        .chars()
                        .take(2000usize.saturating_sub(self.output.chars().count())),
                );
            }
        }
        if let Some(usage) = data["usage"].as_object() {
            self.usage.as_object_mut().unwrap().extend(usage.clone());
        }
        if data["run_id"] == self.context.run_id {
            if let Some(status) = terminal_status(&kind, &data) {
                self.status = status;
            }
        }
    }
    fn first_ms(&self) -> i64 {
        self.context.first_token_latency_ms.unwrap_or_else(|| {
            self.first
                .map(|t| (t - self.context.started_at).num_milliseconds())
                .unwrap_or(0)
        })
    }
    fn trace(&self) -> Result<Value, TraceError> {
        let c = &self.context;
        build_assistant_runtime_trace(RuntimeTraceInput {
            run: &c.run_id,
            request: &c.request_id,
            tenant: &c.tenant_id,
            user: &c.user_id,
            session: &c.session_id,
            message: &c.message,
            snapshot: &c.snapshot,
            status: self.status,
            started: c.started_at,
            ended: Utc::now(),
            first_token_ms: self.first_ms(),
            output: &self.output,
            event_counts: &self.counts,
            usage: &self.usage,
            error: self.error.as_deref(),
        })
        .map_err(|_| TraceError::invalid())
    }
}
/// The returned channel forwards original byte frames. Detached replay only reads
/// the caller-provided original durable cursor and has a 900-second source limit.
pub fn capture_assistant_runtime_stream(
    mut source: FrameReceiver,
    repository: Arc<TraceRepository>,
    context: CaptureContext,
    replay: Option<ReplaySource>,
) -> FrameReceiver {
    let (tx, rx) = mpsc::channel(1);
    tokio::spawn(async move {
        let mut observation = Observation::new(context);
        loop {
            tokio::select! {_ = tx.closed()=>break,item=source.recv()=>{let Some(item)=item else{break;};match &item{Ok(frame)=>observation.observe(frame),Err(error)=>observation.error=Some(error.clone())};let failed=item.is_err();if tx.send(item).await.is_err()||failed{break;}}}
        }
        drop(source);
        if observation.status != "running" {
            if let Ok(trace) = observation.trace() {
                repository.schedule_gateway_trace_ingest(trace, 90, true);
            }
            return;
        }
        if let Some(replay) = replay {
            let mut context = observation.context.clone();
            context.first_token_latency_ms = Some(observation.first_ms());
            let _ = tokio::time::timeout(Duration::from_secs(900), async move {
                let Ok(mut source) = replay().await else {
                    return;
                };
                let mut observation = Observation::new(context);
                while let Some(frame) = source.recv().await {
                    match frame {
                        Ok(frame) => observation.observe(&frame),
                        Err(error) => {
                            observation.error = Some(error);
                            break;
                        }
                    }
                }
                if observation.status != "running" {
                    if let Ok(trace) = observation.trace() {
                        repository.schedule_gateway_trace_ingest(trace, 90, true);
                    }
                }
            })
            .await;
        }
    });
    rx
}
impl TraceRepository {
    pub fn schedule_gateway_trace_ingest(
        &self,
        mut trace: Value,
        retention_days: i32,
        enqueue: bool,
    ) {
        let repository = self.clone();
        trace["retention_expires_at"] =
            json!((Utc::now() + chrono::Duration::days(retention_days.max(1) as i64)).to_rfc3339());
        let tenant = text(&trace, "tenant_id").to_owned();
        let user = text(&trace, "user_id").to_owned();
        tokio::spawn(async move {
            if trace["source_adapter"] == "gateway.agent_runtime"
                && repository
                    .enrich_runtime_trace_usage(&mut trace)
                    .await
                    .is_err()
            {
                return;
            }
            let _ = repository
                .ingest_trace(
                    &tenant,
                    &user,
                    &json!({"trace":trace,"enqueue":enqueue}),
                    enqueue,
                )
                .await;
        });
    }
    pub async fn enrich_runtime_trace_usage(&self, trace: &mut Value) -> Result<(), TraceError> {
        let params = [
            text(trace, "run_id"),
            text(trace, "tenant_id"),
            text(trace, "user_id"),
            text(trace, "session_id"),
        ]
        .into_iter()
        .map(|s| super::summary::Param::Text(s.into()))
        .collect();
        let rows=self.query_rows("SELECT harness_thread_id FROM assistant_runs WHERE run_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND engine='agent_runtime' AND status IN ('completed','succeeded','failed','cancelled')".into(),params).await?;
        let Some(thread) = rows
            .first()
            .and_then(|r| r["harness_thread_id"].as_str())
            .filter(|s| !s.is_empty())
        else {
            return Ok(());
        };
        let scope = RuntimeTraceScope {
            run_id: text(trace, "run_id").into(),
            tenant_id: text(trace, "tenant_id").into(),
            user_id: text(trace, "user_id").into(),
            session_id: text(trace, "session_id").into(),
            runtime_thread_id: thread.into(),
        };
        let receipt = self.get_assistant_runtime_trace_model_usage(&scope).await?;
        let dispatched = receipt["dispatched_calls"].as_i64().unwrap_or(0);
        let measured = receipt["measured_calls"].as_i64().unwrap_or(0);
        let complete = dispatched > 0 && measured == dispatched;
        let mut usage = json!({"dispatched_calls":dispatched,"measured_calls":measured,"tokens_complete":complete});
        if complete {
            let i = receipt["input_tokens"].as_i64().unwrap_or(0);
            let o = receipt["output_tokens"].as_i64().unwrap_or(0);
            trace["metrics"]["input_tokens"] = json!(i);
            trace["metrics"]["output_tokens"] = json!(o);
            trace["metrics"]["total_tokens"] = json!(i + o);
        }
        if dispatched > 0 && receipt["cost_measured_calls"].as_i64() == Some(dispatched) {
            usage["cost_microusd"] = receipt["cost_microusd"].clone();
            trace["metrics"]["total_cost_cents"] =
                json!(receipt["cost_microusd"].as_f64().unwrap_or(0.) / 10000.);
        }
        trace["metadata"]["runtime_model_usage"] = usage;
        Ok(())
    }
    pub async fn reconcile_assistant_runtime_trace_page(
        &self,
        retention_days: i32,
        limit: i64,
        cursor: Option<(DateTime<Utc>, String)>,
    ) -> Result<(usize, Option<(DateTime<Utc>, String)>), TraceError> {
        let cutoff = Utc::now() - chrono::Duration::days(retention_days.max(1) as i64);
        let rows = self
            .list_assistant_runtime_trace_reconciliation_candidates(
                cutoff,
                limit,
                cursor.as_ref().map(|c| c.0),
                cursor.as_ref().map(|c| c.1.as_str()),
            )
            .await?;
        let mut inserted = 0;
        for run in &rows {
            if self
                .reconcile_run(run, retention_days)
                .await
                .unwrap_or(false)
            {
                inserted += 1;
            }
        }
        let next = if rows.len() == limit.clamp(1, 500) as usize {
            rows.last().and_then(|r| {
                super::ingest::timestamp(&r["ended_at"]).map(|d| (d, text(r, "run_id").into()))
            })
        } else {
            None
        };
        Ok((inserted, next))
    }
    async fn reconcile_run(&self, run: &Value, retention_days: i32) -> Result<bool, TraceError> {
        let scope: RuntimeTraceScope = serde_json::from_value(run.clone())?;
        let Some(terminal) = self.get_assistant_runtime_trace_terminal(&scope).await? else {
            return Ok(false);
        };
        let raw_kind = text(&terminal, "event_type");
        let kind = raw_kind.strip_prefix("compat/v1/").unwrap_or(&raw_kind);
        let data = parsed(&parsed(&terminal["payload"])["data"]);
        let expected = match text(run, "status").as_str() {
            "completed" | "succeeded" => "succeeded",
            "failed" => "failed",
            "cancelled" => "cancelled",
            _ => return Ok(false),
        };
        if data["run_id"] != scope.run_id
            || data["session_id"] != scope.session_id
            || expected == "succeeded" && kind != "run_finished"
            || expected == "failed" && kind != "run_error"
            || expected == "cancelled" && !["cancelled", "run_error"].contains(&kind)
            || terminal_status(kind, &data) != Some(expected)
        {
            return Ok(false);
        }
        let Some((started, ended)) = super::ingest::timestamp(&run["started_at"])
            .zip(super::ingest::timestamp(&run["ended_at"]))
        else {
            return Ok(false);
        };
        let counts = self.count_assistant_runtime_trace_events(&scope).await?;
        let mut output = String::new();
        let mut sequence = 0;
        while output.chars().count() < 2000 {
            let page = self
                .read_assistant_runtime_trace_text_page(&scope, sequence, 250)
                .await?;
            if page.is_empty() {
                break;
            }
            for row in &page {
                sequence = row["sequence"].as_i64().ok_or_else(TraceError::invalid)?;
                output.extend(
                    text(row, "content")
                        .chars()
                        .take(2000usize.saturating_sub(output.chars().count())),
                );
                if output.chars().count() >= 2000 {
                    break;
                }
            }
            if page.len() < 250 {
                break;
            }
        }
        let receipt = self.get_assistant_runtime_trace_model_usage(&scope).await?;
        let dispatched = receipt["dispatched_calls"].as_i64().unwrap_or(0);
        let measured = receipt["measured_calls"].as_i64().unwrap_or(0);
        let complete = dispatched > 0 && measured == dispatched;
        let mut usage = parsed(&run["usage"]);
        if complete {
            let i = receipt["input_tokens"].as_i64().unwrap_or(0);
            let o = receipt["output_tokens"].as_i64().unwrap_or(0);
            usage = json!({"input_tokens":i,"output_tokens":o,"total_tokens":i+o});
        }
        let mut snapshot = parsed(&run["snapshot"]);
        if !snapshot["model"].is_object() {
            snapshot["model"] = json!({});
        }
        if !truthy(&snapshot["model"]["provider"]) {
            snapshot["model"]["provider"] = snapshot["model"]["provider_id"].clone();
        }
        let mut trace = build_assistant_runtime_trace(RuntimeTraceInput {
            run: &scope.run_id,
            request: &scope.run_id,
            tenant: &scope.tenant_id,
            user: &scope.user_id,
            session: &scope.session_id,
            message: &text(run, "request_preview"),
            snapshot: &snapshot,
            status: expected,
            started,
            ended,
            first_token_ms: 0,
            output: &output,
            event_counts: &counts,
            usage: &usage,
            error: None,
        })
        .map_err(|_| TraceError::invalid())?;
        trace["metadata"]["runtime_model_usage"] = json!({"dispatched_calls":dispatched,"measured_calls":measured,"tokens_complete":complete});
        if dispatched > 0 && receipt["cost_measured_calls"].as_i64() == Some(dispatched) {
            trace["metadata"]["runtime_model_usage"]["cost_microusd"] =
                receipt["cost_microusd"].clone();
        }
        trace["events"][0]["payload"]["runtime_sequence"] = terminal["sequence"].clone();
        trace["retention_expires_at"] =
            json!((ended + chrono::Duration::days(retention_days.max(1) as i64)).to_rfc3339());
        self.insert_reconciled_assistant_runtime_trace(run, &terminal, &trace)
            .await
    }
    pub async fn run_assistant_trace_reconciler(
        &self,
        retention_days: i32,
        page_limit: i64,
        interval: Duration,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut cursor = None;
        loop {
            if *shutdown.borrow() {
                break;
            }
            let result = ai_platform_gateway_foundation::worker_lease::with_effect(
                "assistant_trace_reconciler_task",
                self.reconcile_assistant_runtime_trace_page(retention_days, page_limit, cursor),
            )
            .await;
            let Ok(result) = result else {
                return;
            };
            cursor = result.ok().and_then(|(_, cursor)| cursor);
            tokio::select! {_=tokio::time::sleep(interval)=>{},_=shutdown.changed()=>{if *shutdown.borrow(){break;}}}
        }
    }
}
