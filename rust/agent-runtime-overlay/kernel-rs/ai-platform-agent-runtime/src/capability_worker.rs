//! Private Runtime transport for Capability Contract V2.

use std::net::IpAddr;
use std::time::Duration;

use ai_platform_capability_contract::{
    CAPABILITY_CATALOG_SCHEMA_VERSION, CapabilityCatalogRequestV2, CapabilityCatalogV2,
    CapabilityEventPageV2, CapabilityExecutionV2, CapabilityScopeV2,
    CreateCapabilityExecutionRequestV2,
};
use reqwest::{Client, Method, Response, StatusCode, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::trace_context::InternalTraceContext;

const MAX_RESPONSE_BYTES: usize = 1_048_576;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityWorkerError {
    InvalidBaseUrl,
    MissingInternalToken,
    InvalidRequest,
    Unavailable,
    Timeout,
    Rejected(u16),
    ResponseTooLarge,
    InvalidResponse,
    EventSequenceViolation,
}

impl CapabilityWorkerError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidBaseUrl => "capability_worker_url_invalid",
            Self::MissingInternalToken => "capability_worker_token_missing",
            Self::InvalidRequest => "capability_worker_request_invalid",
            Self::Unavailable => "capability_worker_unavailable",
            Self::Timeout => "capability_worker_timeout",
            Self::Rejected(_) => "capability_worker_rejected",
            Self::ResponseTooLarge => "capability_worker_response_too_large",
            Self::InvalidResponse => "capability_worker_response_invalid",
            Self::EventSequenceViolation => "capability_worker_event_sequence_invalid",
        }
    }
}

#[derive(Clone)]
pub struct CapabilityWorkerClient {
    client: Client,
    base_url: Url,
    internal_token: String,
    trace_context: InternalTraceContext,
    run_id: Option<String>,
    execution_owner: Option<(String, i64)>,
}

impl CapabilityWorkerClient {
    /// The `reqwest::Client` is injected so the Runtime's connection pool,
    /// proxy exclusions, DNS, and TLS policy remain authoritative.
    pub fn new(
        client: Client,
        base_url: &str,
        internal_token: impl Into<String>,
    ) -> Result<Self, CapabilityWorkerError> {
        let base_url = strict_base_url(base_url)?;
        let internal_token = internal_token.into();
        if internal_token.trim().is_empty() {
            return Err(CapabilityWorkerError::MissingInternalToken);
        }
        Ok(Self {
            client,
            base_url,
            internal_token,
            trace_context: InternalTraceContext::default(),
            run_id: None,
            execution_owner: None,
        })
    }

    pub(crate) fn with_execution_owner(mut self, owner: String, fence: i64) -> Self {
        self.execution_owner = Some((owner, fence));
        self
    }

    pub(crate) fn with_trace_context(
        mut self,
        trace_context: InternalTraceContext,
        run_id: impl Into<String>,
    ) -> Self {
        self.trace_context = trace_context;
        self.run_id = Some(run_id.into());
        self
    }

    pub async fn health(&self) -> Result<(), CapabilityWorkerError> {
        self.health_degraded().await.map(|_| ())
    }

    pub(crate) async fn health_degraded(&self) -> Result<bool, CapabilityWorkerError> {
        let mut url = self.base_url.clone();
        url.set_path("/health/ready");
        url.set_query(None);
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            self.client
                .get(url)
                .header("x-request-id", "runtime-health")
                .timeout(Duration::from_secs(2))
                .send(),
        )
        .await
        .map_err(|_| CapabilityWorkerError::Timeout)?
        .map_err(|_| CapabilityWorkerError::Unavailable)?;
        if !response.status().is_success() {
            return Err(CapabilityWorkerError::Rejected(response.status().as_u16()));
        }
        let body: WorkerHealthResponse = serde_json::from_slice(&bounded_bytes(response).await?)
            .map_err(|_| CapabilityWorkerError::InvalidResponse)?;
        if body.status != "ready" || !body.core_ready {
            return Err(CapabilityWorkerError::InvalidResponse);
        }
        Ok(body.degraded)
    }

    pub async fn catalog(
        &self,
        scope: &CapabilityScopeV2,
        capability_revision: u64,
    ) -> Result<CapabilityCatalogV2, CapabilityWorkerError> {
        let request = CapabilityCatalogRequestV2 {
            schema_version: CAPABILITY_CATALOG_SCHEMA_VERSION.to_string(),
            tenant_id: scope.tenant_id.clone(),
            user_id: scope.user_id.clone(),
            session_id: scope.session_id.clone(),
            capability_revision,
        };
        request
            .validate()
            .map_err(|_| CapabilityWorkerError::InvalidRequest)?;
        self.request(Method::POST, "catalog", scope, Some(&request), None)
            .await
    }

    pub async fn create(
        &self,
        scope: &CapabilityScopeV2,
        request: &CreateCapabilityExecutionRequestV2,
    ) -> Result<CapabilityExecutionV2, CapabilityWorkerError> {
        if request.lease.scope() != *scope {
            return Err(CapabilityWorkerError::InvalidRequest);
        }
        let execution: CapabilityExecutionV2 = self
            .request(Method::POST, "executions", scope, Some(request), None)
            .await?;
        execution
            .validate()
            .map_err(|_| CapabilityWorkerError::InvalidResponse)?;
        Ok(execution)
    }

    pub async fn get(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
    ) -> Result<CapabilityExecutionV2, CapabilityWorkerError> {
        let path = execution_path(execution_id, "")?;
        let execution: CapabilityExecutionV2 = self
            .request::<(), _>(Method::GET, &path, scope, None, None)
            .await?;
        execution
            .validate()
            .map_err(|_| CapabilityWorkerError::InvalidResponse)?;
        Ok(execution)
    }

    pub async fn events(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        after_sequence: u64,
    ) -> Result<CapabilityEventPageV2, CapabilityWorkerError> {
        let path = execution_path(execution_id, "/events")?;
        let page: CapabilityEventPageV2 = self
            .request::<(), _>(Method::GET, &path, scope, None, Some(after_sequence))
            .await?;
        validate_event_page(&page, execution_id, after_sequence)?;
        Ok(page)
    }

    pub async fn cancel(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
    ) -> Result<CapabilityExecutionV2, CapabilityWorkerError> {
        let path = execution_path(execution_id, ":cancel")?;
        let execution: CapabilityExecutionV2 = self
            .request::<(), _>(Method::POST, &path, scope, None, None)
            .await?;
        execution
            .validate()
            .map_err(|_| CapabilityWorkerError::InvalidResponse)?;
        Ok(execution)
    }

    async fn request<T: serde::Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        scope: &CapabilityScopeV2,
        body: Option<&T>,
        after_sequence: Option<u64>,
    ) -> Result<R, CapabilityWorkerError> {
        validate_scope(scope)?;
        let mut url = self
            .base_url
            .join(path)
            .map_err(|_| CapabilityWorkerError::InvalidBaseUrl)?;
        if let Some(cursor) = after_sequence {
            url.query_pairs_mut()
                .append_pair("after_sequence", &cursor.to_string());
        }
        let mut request = self
            .client
            .request(method, url)
            .header("x-ai-platform-internal-token", &self.internal_token)
            .header("x-ai-tenant-id", &scope.tenant_id)
            .header("x-ai-user-id", &scope.user_id)
            .header("x-ai-session-id", &scope.session_id)
            .timeout(REQUEST_TIMEOUT);
        if let Some((owner, fence)) = self.execution_owner.as_ref() {
            request = request
                .header("x-runtime-execution-owner", owner)
                .header("x-runtime-execution-fence", fence.to_string());
        }
        if let Some(run_id) = self.run_id.as_deref() {
            request =
                self.trace_context
                    .apply(request, run_id, run_id, execution_id_from_path(path));
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = tokio::time::timeout(REQUEST_TIMEOUT, request.send())
            .await
            .map_err(|_| CapabilityWorkerError::Timeout)?
            .map_err(|_| CapabilityWorkerError::Unavailable)?;
        if !response.status().is_success() {
            return Err(if response.status() == StatusCode::REQUEST_TIMEOUT {
                CapabilityWorkerError::Timeout
            } else {
                CapabilityWorkerError::Rejected(response.status().as_u16())
            });
        }
        let bytes = bounded_bytes(response).await?;
        serde_json::from_slice(&bytes).map_err(|_| CapabilityWorkerError::InvalidResponse)
    }
}

#[derive(Deserialize)]
struct WorkerHealthResponse {
    status: String,
    core_ready: bool,
    degraded: bool,
}

fn execution_id_from_path(path: &str) -> Option<&str> {
    let value = path.strip_prefix("executions/")?;
    let value = value.split(['/', ':']).next()?;
    uuid::Uuid::parse_str(value).ok().map(|_| value)
}

fn validate_scope(scope: &CapabilityScopeV2) -> Result<(), CapabilityWorkerError> {
    if [
        scope.tenant_id.as_str(),
        scope.user_id.as_str(),
        scope.session_id.as_str(),
    ]
    .into_iter()
    .any(|value| {
        value.is_empty() || value.len() > 255 || value.bytes().any(|byte| byte.is_ascii_control())
    }) {
        return Err(CapabilityWorkerError::InvalidRequest);
    }
    Ok(())
}

fn strict_base_url(value: &str) -> Result<Url, CapabilityWorkerError> {
    let mut url = Url::parse(value).map_err(|_| CapabilityWorkerError::InvalidBaseUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url
            .host_str()
            .is_some_and(|host| host.parse::<IpAddr>().is_ok_and(|ip| ip.is_unspecified()))
    {
        return Err(CapabilityWorkerError::InvalidBaseUrl);
    }
    let path = url.path().trim_end_matches('/');
    if path.is_empty() || path == "/internal/v2/capabilities" {
        url.set_path("/internal/v2/capabilities/");
    } else {
        return Err(CapabilityWorkerError::InvalidBaseUrl);
    }
    Ok(url)
}

fn execution_path(execution_id: &str, suffix: &str) -> Result<String, CapabilityWorkerError> {
    uuid::Uuid::parse_str(execution_id).map_err(|_| CapabilityWorkerError::InvalidRequest)?;
    Ok(format!("executions/{execution_id}{suffix}"))
}

fn validate_event_page(
    page: &CapabilityEventPageV2,
    execution_id: &str,
    after_sequence: u64,
) -> Result<(), CapabilityWorkerError> {
    if page.schema_version != ai_platform_capability_contract::CAPABILITY_EVENT_SCHEMA_VERSION
        || page.execution_id != execution_id
        || page.after_sequence != after_sequence
        || page.events.len() > 1
    {
        return Err(CapabilityWorkerError::InvalidResponse);
    }
    let mut previous = after_sequence;
    for event in &page.events {
        if event.execution_id != execution_id || event.sequence <= previous {
            return Err(CapabilityWorkerError::EventSequenceViolation);
        }
        previous = event.sequence;
    }
    if page.next_sequence != previous {
        return Err(CapabilityWorkerError::EventSequenceViolation);
    }
    Ok(())
}

async fn bounded_bytes(mut response: Response) -> Result<Vec<u8>, CapabilityWorkerError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(CapabilityWorkerError::ResponseTooLarge);
    }
    // Do not use `Response::bytes`: with a missing or forged Content-Length it
    // buffers the entire body before the size check below can run. Reading one
    // chunk at a time keeps the hard cap effective for chunked responses too.
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| CapabilityWorkerError::InvalidResponse)?
    {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
            return Err(CapabilityWorkerError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use ai_platform_capability_contract::{CAPABILITY_EVENT_SCHEMA_VERSION, CapabilityEventPageV2};

    use super::*;

    #[test]
    fn base_url_is_fixed_to_the_private_capability_prefix() {
        assert!(strict_base_url("file:///tmp").is_err());
        assert!(strict_base_url("http://user:pass@example.test").is_err());
        assert!(strict_base_url("http://0.0.0.0:8095").is_err());
        assert!(strict_base_url("http://worker.test:8095").is_ok());
        assert!(strict_base_url("http://worker.test:8095/arbitrary").is_err());
    }

    #[test]
    fn event_page_enforces_one_chunk_of_producer_ahead() {
        let execution_id = "00000000-0000-0000-0000-000000000001";
        let valid = CapabilityEventPageV2 {
            schema_version: CAPABILITY_EVENT_SCHEMA_VERSION.to_string(),
            execution_id: execution_id.to_string(),
            after_sequence: 0,
            next_sequence: 0,
            has_more: false,
            events: vec![],
        };
        assert!(validate_event_page(&valid, execution_id, 0).is_ok());
        let mut invalid = valid;
        invalid.next_sequence = 2;
        assert!(matches!(
            validate_event_page(&invalid, execution_id, 0),
            Err(CapabilityWorkerError::EventSequenceViolation)
        ));
    }

    #[tokio::test]
    async fn bounded_bytes_rejects_chunked_body_without_content_length() {
        use async_stream::stream;
        use axum::{Router, body::Body, routing::get};

        let app = Router::new().route(
            "/",
            get(|| async {
                let body = stream! {
                    yield Ok::<_, std::io::Error>(vec![0_u8; MAX_RESPONSE_BYTES]);
                    yield Ok::<_, std::io::Error>(vec![0_u8; 1]);
                };
                Body::from_stream(body)
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let address = listener.local_addr().expect("test listener address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("test server should run");
        });

        let client = Client::builder()
            .no_proxy()
            .build()
            .expect("test client should build");
        let response = client
            .get(format!("http://{address}/"))
            .send()
            .await
            .expect("test response should arrive");
        assert!(response.content_length().is_none());
        assert_eq!(
            bounded_bytes(response).await,
            Err(CapabilityWorkerError::ResponseTooLarge)
        );

        server.abort();
    }

    #[tokio::test]
    async fn arc05_health_requires_worker_core_ready_schema() {
        use axum::{Json, Router, routing::get};

        let app = Router::new().route(
            "/health/ready",
            get(|| async {
                Json(serde_json::json!({
                    "status": "ready",
                    "core_ready": true,
                    "degraded": true
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener should bind");
        let address = listener.local_addr().expect("test listener address");
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let client = CapabilityWorkerClient::new(
            Client::builder()
                .no_proxy()
                .build()
                .expect("test client should build"),
            &format!("http://{address}"),
            "internal-token",
        )
        .expect("worker client should build");
        assert_eq!(client.health_degraded().await, Ok(true));
        assert_eq!(client.health().await, Ok(()));
        server.abort();
    }
}
