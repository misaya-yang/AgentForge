//! Process-level HTTP admission. Domain/tenant quotas remain separate authorities.
//! Header deadlines stop at response creation; streaming bodies have an idle deadline,
//! never a total lifetime deadline. Permits and cancellation follow body ownership.
use crate::capacity_admission::AdmissionCancellation;
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use http_body::{Body as HttpBody, Frame, SizeHint};
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{Instant, Sleep},
};

/// A request-scoped signal. Consumers can pass it directly to capacity admission.
/// It becomes cancelled on header timeout, body error/end, or caller disconnect/drop.
pub type RequestCancellation = AdmissionCancellation;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "RawConfig")]
pub struct RequestGovernanceConfig {
    max_concurrent_requests: usize,
    response_header_timeout_ms: Option<u64>,
    response_body_idle_timeout_ms: Option<u64>,
}
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    max_concurrent_requests: usize,
    response_header_timeout_ms: Option<u64>,
    response_body_idle_timeout_ms: Option<u64>,
}
impl Default for RawConfig {
    fn default() -> Self {
        Self {
            max_concurrent_requests: 1024,
            response_header_timeout_ms: Some(120_000),
            response_body_idle_timeout_ms: Some(120_000),
        }
    }
}
impl Default for RequestGovernanceConfig {
    fn default() -> Self {
        Self::try_from(RawConfig::default()).expect("valid defaults")
    }
}
impl TryFrom<RawConfig> for RequestGovernanceConfig {
    type Error = String;
    fn try_from(raw: RawConfig) -> Result<Self, String> {
        Self::new(
            raw.max_concurrent_requests,
            raw.response_header_timeout_ms,
            raw.response_body_idle_timeout_ms,
        )
    }
}
impl RequestGovernanceConfig {
    /// None disables only that deadline. Zero is rejected rather than silently disabled.
    pub fn new(
        max_concurrent_requests: usize,
        response_header_timeout_ms: Option<u64>,
        response_body_idle_timeout_ms: Option<u64>,
    ) -> Result<Self, String> {
        if !(1..=1_000_000).contains(&max_concurrent_requests) {
            return Err("max_concurrent_requests must be between 1 and 1000000".into());
        }
        for value in [response_header_timeout_ms, response_body_idle_timeout_ms]
            .into_iter()
            .flatten()
        {
            if !(1..=86_400_000).contains(&value) {
                return Err("HTTP deadline must be 1..86400000 milliseconds or null".into());
            }
        }
        Ok(Self {
            max_concurrent_requests,
            response_header_timeout_ms,
            response_body_idle_timeout_ms,
        })
    }
}
#[derive(Clone)]
pub struct RequestGovernance {
    config: RequestGovernanceConfig,
    permits: Arc<Semaphore>,
}
impl Default for RequestGovernance {
    fn default() -> Self {
        Self::new(RequestGovernanceConfig::default())
    }
}
impl RequestGovernance {
    pub fn new(config: RequestGovernanceConfig) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(config.max_concurrent_requests)),
            config,
        }
    }
    pub fn available_permits(&self) -> usize {
        self.permits.available_permits()
    }
}
struct RequestLease {
    _permit: OwnedSemaphorePermit,
    cancellation: RequestCancellation,
}
impl Drop for RequestLease {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}
fn exempt(path: &str) -> bool {
    matches!(
        path,
        "/health" | "/health/live" | "/health/ready" | "/metrics"
    )
}
/// Mount inside trace/security-header layers, outside authenticated domain handlers.
/// This does not inspect, mutate, or synthesize credentials or tracing metadata.
pub async fn govern(
    State(state): State<RequestGovernance>,
    mut request: Request,
    next: Next,
) -> Response {
    if exempt(request.uri().path()) {
        return next.run(request).await;
    }
    let Ok(permit) = state.permits.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [("retry-after", "1")],
            Json(serde_json::json!({"detail":"request capacity exceeded"})),
        )
            .into_response();
    };
    let cancellation = RequestCancellation::default();
    request.extensions_mut().insert(cancellation.clone());
    let lease = RequestLease {
        _permit: permit,
        cancellation,
    };
    let response = match state.config.response_header_timeout_ms {
        Some(ms) => {
            match tokio::time::timeout(Duration::from_millis(ms), next.run(request)).await {
                Ok(response) => response,
                Err(_) => {
                    return (
                        StatusCode::GATEWAY_TIMEOUT,
                        Json(serde_json::json!({"detail":"response header deadline exceeded"})),
                    )
                        .into_response();
                }
            }
        }
        None => next.run(request).await,
    };
    let (parts, body) = response.into_parts();
    let idle = state
        .config
        .response_body_idle_timeout_ms
        .map(Duration::from_millis);
    Response::from_parts(
        parts,
        Body::new(GovernedBody {
            body: Some(body),
            lease: Some(lease),
            idle,
            timer: idle.map(|duration| Box::pin(tokio::time::sleep(duration))),
        }),
    )
}
struct GovernedBody {
    body: Option<Body>,
    lease: Option<RequestLease>,
    idle: Option<Duration>,
    timer: Option<Pin<Box<Sleep>>>,
}
impl GovernedBody {
    fn finish(&mut self) {
        self.body.take();
        self.lease.take();
        self.timer.take();
    }
}
impl HttpBody for GovernedBody {
    type Data = Bytes;
    type Error = axum::BoxError;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
        let Some(body) = self.body.as_mut() else {
            return Poll::Ready(None);
        };
        match Pin::new(body).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(idle) = self.idle {
                    if let Some(timer) = self.timer.as_mut() {
                        timer.as_mut().reset(Instant::now() + idle);
                    }
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => {
                self.finish();
                Poll::Ready(Some(Err(Box::new(error))))
            }
            Poll::Ready(None) => {
                self.finish();
                Poll::Ready(None)
            }
            Poll::Pending => {
                if self
                    .timer
                    .as_mut()
                    .is_some_and(|timer| timer.as_mut().poll(cx).is_ready())
                {
                    self.finish();
                    Poll::Ready(Some(Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "response body idle deadline exceeded",
                    )))))
                } else {
                    Poll::Pending
                }
            }
        }
    }
    fn is_end_stream(&self) -> bool {
        self.body.as_ref().is_none_or(HttpBody::is_end_stream)
    }
    fn size_hint(&self) -> SizeHint {
        self.body
            .as_ref()
            .map(HttpBody::size_hint)
            .unwrap_or_default()
    }
}
