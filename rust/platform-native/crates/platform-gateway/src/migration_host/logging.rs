//! Completion-aware streaming telemetry. Response frames pass through untouched.
use super::{metrics::MetricsCollector, middleware::RequestContext};
use crate::metrics_recorder::{MetricsRecorder, RecordOutcome};
use axum::{
    body::{Body, Bytes},
    extract::{Request, State},
    http::HeaderValue,
    middleware::Next,
    response::Response,
};
use http_body::{Body as HttpBody, Frame, SizeHint};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
#[derive(Clone, Default)]
pub struct TelemetryIdentity(pub Arc<Mutex<(String, String)>>);
pub struct LoggingState {
    pub recorder: Arc<MetricsRecorder>,
    pub collector: Arc<MetricsCollector>,
    pub enabled: bool,
    pub excluded: Vec<String>,
    normal: AtomicUsize,
    critical: Semaphore,
}
impl LoggingState {
    pub fn new(recorder: Arc<MetricsRecorder>, collector: Arc<MetricsCollector>) -> Self {
        Self {
            recorder,
            collector,
            enabled: true,
            excluded: ["/health", "/health/live", "/health/ready", "/metrics"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            normal: AtomicUsize::new(0),
            critical: Semaphore::new(16),
        }
    }
    fn metric(&self, name: &str, labels: &[(&str, &str)]) {
        if let Some(metric) = self.collector.metric(name) {
            metric.inc(1., labels);
        }
    }
    fn backlog(&self) {
        if let Some(metric) = self
            .collector
            .metric("gateway_telemetry_background_backlog")
        {
            metric.set(self.normal.load(Ordering::Acquire) as f64, &[]);
        }
    }
    async fn write(self: Arc<Self>, record: Record, critical: bool) {
        if critical {
            let result = tokio::time::timeout(Duration::from_secs(1), async {
                let _permit = self.critical.acquire().await;
                self.recorder
                    .record_request(
                        &record.method,
                        &record.path,
                        record.status,
                        record.duration,
                        &record.user,
                        &record.service,
                    )
                    .await
            })
            .await;
            if result.is_err() {
                self.metric(
                    "gateway_telemetry_record_failures_total",
                    &[("priority", "critical"), ("reason", "timeout")],
                );
            }
            return;
        }
        if self
            .normal
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 128).then_some(n + 1)
            })
            .is_err()
        {
            self.metric(
                "gateway_telemetry_records_dropped_total",
                &[("reason", "capacity")],
            );
            return;
        }
        self.backlog();
        let state = self.clone();
        tokio::spawn(async move {
            let _guard = BacklogGuard(state.clone());
            let outcome = state
                .recorder
                .record_request(
                    &record.method,
                    &record.path,
                    record.status,
                    record.duration,
                    &record.user,
                    &record.service,
                )
                .await;
            if outcome == RecordOutcome::CollectionError {
                state.metric(
                    "gateway_telemetry_record_failures_total",
                    &[("priority", "normal"), ("reason", "CollectionError")],
                );
            }
        });
    }
}
struct BacklogGuard(Arc<LoggingState>);
impl Drop for BacklogGuard {
    fn drop(&mut self) {
        self.0.normal.fetch_sub(1, Ordering::AcqRel);
        self.0.backlog();
    }
}
struct Record {
    method: String,
    path: String,
    status: i32,
    duration: f64,
    user: String,
    service: String,
}
struct Recording {
    state: Arc<LoggingState>,
    method: String,
    path: String,
    start: Instant,
    identity: TelemetryIdentity,
    streaming: bool,
    finished: bool,
}
impl Recording {
    fn record(&self, status: i32) -> Record {
        let identity = self.identity.0.lock().unwrap_or_else(|e| e.into_inner());
        Record {
            method: self.method.clone(),
            path: self.path.clone(),
            status,
            duration: self.start.elapsed().as_secs_f64() * 1000.,
            user: identity.0.clone(),
            service: identity.1.clone(),
        }
    }
    fn finish(&mut self, status: i32) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.finished = true;
        let record = self.record(status);
        let critical = self.streaming || status >= 400 || record.duration >= 1000.;
        let state = self.state.clone();
        Box::pin(async move { state.write(record, critical).await })
    }
}
impl Drop for Recording {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let record = self.record(499);
        let state = self.state.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                state.write(record, true).await;
            });
        } else {
            state.metric(
                "gateway_telemetry_records_dropped_total",
                &[("reason", "scheduler_unavailable")],
            );
        }
    }
}
struct RecordedBody {
    body: Body,
    recording: Recording,
    status: i32,
    completion: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
    complete: bool,
}
impl HttpBody for RecordedBody {
    type Data = Bytes;
    type Error = axum::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        let this = self.get_mut();
        if this.complete {
            return Poll::Ready(None);
        }
        if let Some(completion) = &mut this.completion {
            return match completion.as_mut().poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(()) => {
                    this.complete = true;
                    Poll::Ready(None)
                }
            };
        }
        match Pin::new(&mut this.body).poll_frame(cx) {
            Poll::Ready(None) => {
                this.completion = Some(this.recording.finish(this.status));
                self_poll(this, cx)
            }
            Poll::Ready(Some(Err(error))) => {
                let future = this.recording.finish(500);
                tokio::spawn(future);
                this.complete = true;
                Poll::Ready(Some(Err(error)))
            }
            other => other,
        }
    }
    fn is_end_stream(&self) -> bool {
        self.complete
    }
    fn size_hint(&self) -> SizeHint {
        self.body.size_hint()
    }
}
fn self_poll(
    body: &mut RecordedBody,
    cx: &mut Context<'_>,
) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
    match body
        .completion
        .as_mut()
        .expect("completion set")
        .as_mut()
        .poll(cx)
    {
        Poll::Pending => Poll::Pending,
        Poll::Ready(()) => {
            body.complete = true;
            Poll::Ready(None)
        }
    }
}
fn streaming(path: &str) -> bool {
    path == "/v1/responses"
        || path.ends_with("/stream")
        || path.ends_with("/sse")
        || ([
            "/api/v1/conversations/",
            "/api/v1/langgraph/",
            "/api/v1/proxy/",
            "/proxy/",
        ]
        .iter()
        .any(|p| path.starts_with(p))
            && (path.contains("/stream") || path.contains("/events")))
        || path.to_lowercase().contains("/stream") && !path.to_lowercase().contains("upstream")
}
pub async fn record(
    State(state): State<Arc<LoggingState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_owned();
    if !state.enabled
        || state
            .excluded
            .iter()
            .any(|p| &path == p || path.starts_with(&format!("{p}/")))
    {
        return next.run(request).await;
    }
    let incoming = request
        .headers()
        .get("x-request-id")
        .map(|v| {
            v.as_bytes()
                .iter()
                .filter(|b| b.is_ascii())
                .map(|b| char::from(*b))
                .collect::<String>()
        })
        .unwrap_or_default();
    let incoming = incoming.trim();
    let request_id = if !incoming.is_empty()
        && incoming.chars().count() <= 64
        && incoming
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        incoming.to_owned()
    } else {
        uuid::Uuid::new_v4().to_string()
    };
    if let Some(context) = request.extensions_mut().get_mut::<RequestContext>() {
        context.request_id = request_id.clone();
    }
    let identity = TelemetryIdentity::default();
    request.extensions_mut().insert(identity.clone());
    let recording = Recording {
        state,
        method: request.method().to_string(),
        streaming: streaming(&path),
        path,
        start: Instant::now(),
        identity,
        finished: false,
    };
    let mut response = next.run(request).await;
    if let Ok(id) = HeaderValue::from_str(&request_id) {
        response.headers_mut().append("x-request-id", id);
    }
    let status = i32::from(response.status().as_u16());
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::new(RecordedBody {
            body,
            recording,
            status,
            completion: None,
            complete: false,
        }),
    )
}
