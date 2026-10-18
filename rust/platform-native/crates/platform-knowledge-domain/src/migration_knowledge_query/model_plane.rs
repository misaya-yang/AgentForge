//! Signed, bounded Knowledge -> Gateway transport. Unknown dispatch is terminal
//! to this attempt: no vendor fallback or implicit cross-provider retry exists.
use super::{
    auth::GatewayAuth,
    types::{QueryError, Result},
};
pub use ai_platform_service_primitives::knowledge_model::*;
use axum::{
    extract::{Request, State},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::{
    future::Future,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
#[derive(Clone, Default)]
pub struct ModelResource {
    pub dataset_id: Option<String>,
    pub document_id: Option<String>,
    pub execution_id: Option<String>,
}
#[derive(Clone)]
pub struct ModelPlaneClient {
    client: reqwest::Client,
    origin: reqwest::Url,
    signer: Option<Arc<GatewayAuth>>,
    workload_signer: Option<Arc<dyn KnowledgeWorkloadSigner>>,
    model_revisions: std::collections::BTreeMap<String, String>,
    verifier: Arc<dyn ServiceModelLeaseVerifier>,
    workload_http:
        Option<Arc<ai_platform_service_primitives::workload_identity::http::WorkloadHttp>>,
    health_origin: Option<reqwest::Url>,
    legacy_transport: Option<axum::http::HeaderValue>,
}
#[derive(Clone, Copy)]
pub(crate) struct ResponseDeadline(pub tokio::time::Instant);
tokio::task_local! { static PLANE_CONTEXT: (Arc<ModelPlaneClient>,ModelResource); }
impl ModelPlaneClient {
    pub fn new(
        origin: reqwest::Url,
        signer: Arc<GatewayAuth>,
        verifier: Arc<dyn ServiceModelLeaseVerifier>,
    ) -> Result<Self> {
        if !["http", "https"].contains(&origin.scheme())
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || signer.caller_service != "knowledge-service"
            || signer.audience != "gateway"
            || signer.version != "v2"
            || signer.allow_anonymous
        {
            return Err(QueryError::Validation(
                "invalid Knowledge model-plane transport scope".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| QueryError::DependenciesFailed)?;
        Ok(Self {
            client,
            origin,
            signer: Some(signer),
            workload_signer: None,
            model_revisions: Default::default(),
            verifier,
            workload_http: None,
            health_origin: None,
            legacy_transport: None,
        })
    }
    pub fn for_workload(
        origin: reqwest::Url,
        signer: Arc<dyn KnowledgeWorkloadSigner>,
        verifier: Arc<dyn ServiceModelLeaseVerifier>,
    ) -> Result<Self> {
        if !["http", "https"].contains(&origin.scheme())
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(QueryError::Validation("invalid model-plane origin".into()));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| QueryError::DependenciesFailed)?;
        Ok(Self {
            client,
            origin,
            signer: None,
            workload_signer: Some(signer),
            model_revisions: Default::default(),
            verifier,
            workload_http: None,
            health_origin: None,
            legacy_transport: None,
        })
    }
    pub fn with_legacy_transport(mut self, token: &str) -> Result<Self> {
        if token.len() < 32
            || token
                .bytes()
                .any(|b| !b.is_ascii() || b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Err(QueryError::Forbidden);
        }
        self.legacy_transport = Some(
            format!("Bearer {token}")
                .parse()
                .map_err(|_| QueryError::Forbidden)?,
        );
        Ok(self)
    }
    pub fn with_workload_http(
        mut self,
        http: Arc<ai_platform_service_primitives::workload_identity::http::WorkloadHttp>,
        health_origin: Option<reqwest::Url>,
    ) -> Result<Self> {
        http.validate_endpoint(self.origin.as_str())
            .map_err(|_| QueryError::Validation("model-plane origin is not admitted".into()))?;
        self.workload_http = Some(http);
        self.health_origin = health_origin;
        Ok(self)
    }
    pub fn with_model_revisions(
        mut self,
        revisions: std::collections::BTreeMap<String, String>,
    ) -> Result<Self> {
        if revisions
            .iter()
            .any(|(key, value)| !key.contains('/') || value.trim().is_empty())
        {
            return Err(QueryError::Validation(
                "model revisions must pin registered provider/model identities".into(),
            ));
        }
        self.model_revisions = revisions;
        Ok(self)
    }
    async fn request<T: serde::Serialize>(
        &self,
        path: &str,
        body: &T,
        timeout: Duration,
    ) -> Result<reqwest::Response> {
        let bytes = serde_json::to_vec(body).map_err(|_| QueryError::InvalidProviderResponse)?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(QueryError::Validation(
                "model-plane request exceeds byte budget".into(),
            ));
        }
        let admitted = super::private_transport::current();
        let stream = admitted.is_some()
            && path.ends_with("/execute")
            && serde_json::from_slice::<Value>(&bytes)
                .ok()
                .is_some_and(|value| value["call"]["stream"] == true);
        let effective = match &admitted {
            Some(transport) => timeout.min(
                transport
                    .budget
                    .remaining(std::time::Instant::now())
                    .map_err(|_| QueryError::DependenciesFailed)?,
            ),
            None => timeout,
        };
        let deadline = tokio::time::Instant::now() + effective;
        let mut request = self
            .client
            .post(
                self.origin
                    .join(path)
                    .map_err(|_| QueryError::DependenciesFailed)?,
            )
            .header("content-type", "application/json");
        if !stream {
            request = request.timeout(effective);
        }
        if self.workload_http.is_none() {
            if let Some(bearer) = &self.legacy_transport {
                request = request.header("authorization", bearer);
            }
        }
        if let Some(transport) = &admitted {
            let mut headers = Vec::new();
            transport.trace.append(&mut headers);
            transport
                .budget
                .child(
                    ai_platform_capability_contract::private_transport::Edge::WorkerKnowledge
                        .policy(),
                    std::time::Instant::now(),
                )
                .map_err(|_| QueryError::DependenciesFailed)?
                .append(
                    &mut headers,
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map_err(|_| QueryError::DependenciesFailed)?
                        .as_millis() as u64,
                    std::time::Instant::now(),
                )
                .map_err(|_| QueryError::DependenciesFailed)?;
            for (key, value) in headers {
                request = request.header(key, value);
            }
        }
        if let Some(signer) = &self.workload_signer {
            let actor = verified_actor_scope().ok_or(QueryError::Forbidden)?;
            let proof = signer
                .sign(path, &bytes, &actor.tenant_id, &actor.user_id)
                .map_err(|_| QueryError::Forbidden)?;
            request = request.header("x-knowledge-workload", proof);
        } else {
            let actor = verified_actor_scope().ok_or(QueryError::Forbidden)?;
            let mut headers = HeaderMap::new();
            headers.insert(
                "x-user-id",
                actor.user_id.parse().map_err(|_| QueryError::Forbidden)?,
            );
            headers.insert(
                "x-tenant-id",
                actor.tenant_id.parse().map_err(|_| QueryError::Forbidden)?,
            );
            let proof = self
                .signer
                .as_ref()
                .ok_or(QueryError::Forbidden)?
                .sign("POST", path, "", &bytes, &headers)?;
            request = request
                .headers(headers)
                .header("x-gateway-signature", proof);
        }
        // Bound response headers without imposing a whole-body deadline on SSE.
        // Any ambiguous dispatch remains a dependency failure, never a retry.
        let mut response = if let Some(http) = &self.workload_http {
            let request = request
                .build()
                .map_err(|_| QueryError::DependenciesFailed)?;
            tokio::time::timeout(
                effective,
                http.send(
                    reqwest::Method::POST,
                    request.url().as_str(),
                    request.headers().clone(),
                    Some(bytes),
                ),
            )
            .await
            .map_err(|_| QueryError::DependenciesFailed)?
            .map_err(|_| QueryError::DependenciesFailed)?
        } else {
            tokio::time::timeout(effective, request.body(bytes).send())
                .await
                .map_err(|_| QueryError::DependenciesFailed)?
                .map_err(|_| QueryError::DependenciesFailed)?
        };
        if !stream {
            response.extensions_mut().insert(ResponseDeadline(deadline));
        }
        Ok(response)
    }
    async fn reserve(&self, call: &ModelCall) -> Result<ModelLease> {
        call.validate()
            .map_err(|e| QueryError::Validation(e.into()))?;
        let digest = call_digest(call).map_err(|_| QueryError::InvalidProviderResponse)?;
        let response = self
            .request(
                RESERVE_PATH,
                &ReserveRequest {
                    request_sha256: digest.clone(),
                    call: call.clone(),
                },
                Duration::from_secs(10),
            )
            .await?;
        if !response.status().is_success() {
            return Err(QueryError::Provider(response.status().as_u16()));
        }
        let lease: ModelLease = bounded_json(response).await?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| QueryError::DependenciesFailed)?
            .as_secs();
        if lease.call_id != call.call_id
            || lease.request_sha256 != digest
            || lease.tenant_id != call.scope.actor.tenant_id
            || lease.user_id != call.scope.actor.user_id
            || lease.model != call.model
            || lease.expires_at_unix <= now
            || lease.lease_id.is_empty()
            || !lease
                .lease_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(QueryError::InvalidProviderResponse);
        }
        self.verifier
            .verify(&lease.lease_token, &lease)
            .map_err(|_| QueryError::Forbidden)?;
        Ok(lease)
    }
    async fn dispatch(&self, call: &ModelCall) -> Result<(ModelLease, reqwest::Response)> {
        let lease = self.reserve(call).await?;
        let response = self
            .request(
                &execute_path(&lease.lease_id),
                &ExecuteRequest {
                    lease_token: lease.lease_token.clone(),
                    request_sha256: lease.request_sha256.clone(),
                    call: call.clone(),
                },
                Duration::from_millis(call.budget.deadline_ms),
            )
            .await?;
        if !response.status().is_success() {
            return Err(QueryError::Provider(response.status().as_u16()));
        }
        Ok((lease, response))
    }
}
pub(crate) async fn bounded_json<T: DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T> {
    let deadline = response.extensions().get::<ResponseDeadline>().copied();
    if deadline.is_some_and(|deadline| deadline.0 <= tokio::time::Instant::now()) {
        return Err(QueryError::DependenciesFailed);
    }
    let decode = async move {
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| QueryError::DependenciesFailed)?
        {
            if bytes.len() + chunk.len() > 32 * 1024 * 1024 {
                return Err(QueryError::InvalidProviderResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| QueryError::InvalidProviderResponse)
    };
    if let Some(deadline) = deadline {
        tokio::time::timeout_at(deadline.0, decode)
            .await
            .map_err(|_| QueryError::DependenciesFailed)?
    } else {
        decode.await
    }
}
pub(crate) fn check_execution(
    call: &ModelCall,
    lease: &ModelLease,
    result: &ModelExecution,
) -> Result<()> {
    let receipt = &result.receipt;
    if result.call_id != call.call_id
        || result.lease_id != lease.lease_id
        || result.request_sha256 != lease.request_sha256
        || receipt.call_id != call.call_id
        || receipt.lease_id != lease.lease_id
        || receipt.tenant_id != call.scope.actor.tenant_id
        || receipt.user_id != call.scope.actor.user_id
        || receipt.model != call.model
        || receipt.receipt_id.is_empty()
        || receipt.provider_id.is_empty()
        || receipt.price_book_revision.is_empty()
    {
        return Err(QueryError::InvalidProviderResponse);
    }
    if result.state == ExecutionState::DispatchUnknown {
        return Err(QueryError::DependenciesFailed);
    }
    if !receipt.settled {
        return Err(QueryError::DependenciesFailed);
    }
    Ok(())
}
fn make_call(
    provider: &str,
    model: &str,
    revision: &str,
    purpose: ModelPurpose,
    wire: ModelWire,
    payload: Value,
    timeout: Duration,
    stream: bool,
) -> Result<(Arc<ModelPlaneClient>, ModelCall)> {
    let actor = verified_actor_scope().ok_or(QueryError::Forbidden)?;
    let (client, resource) = PLANE_CONTEXT
        .try_with(Clone::clone)
        .map_err(|_| QueryError::DependenciesFailed)?;
    let revision = if revision.trim().is_empty() {
        client
            .model_revisions
            .get(&format!("{provider}/{model}"))
            .map(String::as_str)
            .ok_or_else(|| {
                QueryError::Validation(
                    "registered model revision is required for Knowledge model calls".into(),
                )
            })?
    } else {
        revision
    };
    let input_bytes = serde_json::to_vec(&payload)
        .map_err(|_| QueryError::InvalidProviderResponse)?
        .len() as u64;
    let max_output_tokens = payload["max_tokens"]
        .as_u64()
        .or_else(|| payload["generationConfig"]["maxOutputTokens"].as_u64())
        .or_else(|| payload["parameters"]["max_tokens"].as_u64())
        .unwrap_or(0);
    let call = ModelCall {
        version: 1,
        call_id: uuid::Uuid::new_v4().to_string(),
        scope: ModelScope {
            actor,
            dataset_id: resource.dataset_id,
            document_id: resource.document_id,
            execution_id: resource.execution_id,
        },
        model: ModelIdentity {
            provider: provider.into(),
            model: model.into(),
            revision: revision.into(),
        },
        purpose,
        wire,
        stream,
        budget: ModelBudget {
            input_bytes,
            max_output_tokens,
            deadline_ms: timeout.as_millis().clamp(1, 600_000) as u64,
        },
        payload,
    };
    call.validate()
        .map_err(|e| QueryError::Validation(e.into()))?;
    Ok((client, call))
}
pub async fn execute(
    provider: &str,
    model: &str,
    revision: &str,
    purpose: ModelPurpose,
    wire: ModelWire,
    payload: Value,
    timeout: Duration,
) -> Result<(u16, Value)> {
    let (client, call) = make_call(
        provider, model, revision, purpose, wire, payload, timeout, false,
    )?;
    let (lease, response) = client.dispatch(&call).await?;
    let result: ModelExecution = bounded_json(response).await?;
    check_execution(&call, &lease, &result)?;
    if result.state == ExecutionState::Completed && !(200..300).contains(&result.upstream_status) {
        return Err(QueryError::InvalidProviderResponse);
    }
    Ok((result.upstream_status, result.output))
}
pub async fn execute_stream(
    provider: &str,
    model: &str,
    revision: &str,
    purpose: ModelPurpose,
    payload: Value,
    timeout: Duration,
) -> Result<super::model_plane_stream::ModelStream> {
    let (client, call) = make_call(
        provider,
        model,
        revision,
        purpose,
        ModelWire::ChatCompletions,
        payload,
        timeout,
        true,
    )?;
    let (lease, response) = client.dispatch(&call).await?;
    super::model_plane_stream::ModelStream::new(call, lease, response)
}
pub async fn run_scoped<F: Future>(
    client: Arc<ModelPlaneClient>,
    resource: ModelResource,
    actor: super::types::UserContext,
    future: F,
) -> F::Output {
    with_actor_scope(actor, PLANE_CONTEXT.scope((client, resource), future)).await
}
/// Capture both scopes before spawning, rather than attempting task-local reads
/// after the child task has started on another executor thread.
pub fn inherit<F: Future>(future: F) -> impl Future<Output = F::Output> {
    let context = PLANE_CONTEXT.try_with(Clone::clone).ok();
    let actor = verified_actor_scope();
    async move {
        match (context, actor) {
            (Some((client, resource)), Some(actor)) => {
                run_scoped(client, resource, actor, future).await
            }
            _ => future.await,
        }
    }
}
/// Install outside authentication middleware: this scope contains no identity;
/// existing verified auth middleware establishes the actor independently inside.
pub async fn request_context(
    State(client): State<Arc<ModelPlaneClient>>,
    request: Request,
    next: Next,
) -> Response {
    let pieces = request
        .uri()
        .path()
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    let dataset_id = pieces
        .iter()
        .position(|p| *p == "datasets")
        .and_then(|i| pieces.get(i + 1))
        .map(|s| (*s).to_owned())
        .or_else(|| {
            pieces
                .iter()
                .position(|p| *p == "knowledge")
                .and_then(|i| pieces.get(i + 1))
                .filter(|s| !["evaluate", "preview", "worker"].contains(s))
                .map(|s| (*s).to_owned())
        });
    let document_id = pieces
        .iter()
        .position(|p| *p == "documents")
        .and_then(|i| pieces.get(i + 1))
        .map(|s| (*s).to_owned());
    let resource = ModelResource {
        dataset_id,
        document_id,
        execution_id: None,
    };
    PLANE_CONTEXT
        .scope((client, resource), next.run(request))
        .await
}

/// Caches contain model outputs: tenant/actor/resource/model revision scope must
/// accompany the textual cache key even when two tenants have identical text.
pub fn cache_scope_identity() -> String {
    use sha2::{Digest, Sha256};
    let actor = verified_actor_scope();
    let resource = PLANE_CONTEXT
        .try_with(|(client, resource)| {
            serde_json::json!([
                resource.dataset_id,
                resource.document_id,
                resource.execution_id,
                client.origin.as_str(),
                client.model_revisions
            ])
        })
        .ok();
    format!(
        "{:x}",
        Sha256::digest(serde_json::json!([actor, resource]).to_string())
    )
}

pub async fn run_current<F: Future>(
    resource: ModelResource,
    actor: super::types::UserContext,
    future: F,
) -> F::Output {
    let client = PLANE_CONTEXT.try_with(|(client, _)| client.clone()).ok();
    if let Some(client) = client {
        run_scoped(client, resource, actor, future).await
    } else {
        with_actor_scope(actor, future).await
    }
}
impl ModelPlaneClient {
    /// Gateway service readiness, not a claim of semantic inference quality.
    pub async fn ready(&self, timeout: Duration) -> bool {
        let origin = self.health_origin.as_ref().unwrap_or(&self.origin);
        let Ok(url) = origin.join("/health/live") else {
            return false;
        };
        matches!(self.client.get(url).timeout(timeout).send().await,Ok(response) if response.status().is_success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn certificate_transport_response_cannot_extend_original_body_deadline() {
        let mut response: reqwest::Response = axum::http::Response::builder()
            .status(200)
            .body("{}".to_owned())
            .unwrap()
            .into();
        response.extensions_mut().insert(ResponseDeadline(
            tokio::time::Instant::now() - Duration::from_millis(1),
        ));
        assert!(matches!(
            bounded_json::<Value>(response).await,
            Err(QueryError::DependenciesFailed)
        ));
    }

    fn fixture() -> (ModelCall, ModelLease, ModelExecution) {
        let actor =
            serde_json::from_value(serde_json::json!({"tenant_id":"t","user_id":"u"})).unwrap();
        let model = ModelIdentity {
            provider: "p".into(),
            model: "m".into(),
            revision: "r".into(),
        };
        let call = ModelCall {
            version: 1,
            call_id: "c".into(),
            scope: ModelScope {
                actor,
                dataset_id: Some("d".into()),
                document_id: None,
                execution_id: None,
            },
            model: model.clone(),
            purpose: ModelPurpose::QueryEmbedding,
            wire: ModelWire::Embeddings,
            stream: false,
            budget: ModelBudget {
                input_bytes: 2,
                max_output_tokens: 0,
                deadline_ms: 1000,
            },
            payload: serde_json::json!({}),
        };
        let lease = ModelLease {
            lease_id: "l".into(),
            lease_token: "fixture-not-a-signed-lease".into(),
            call_id: "c".into(),
            request_sha256: call_digest(&call).unwrap(),
            tenant_id: "t".into(),
            user_id: "u".into(),
            model: model.clone(),
            expires_at_unix: u64::MAX / 1000,
        };
        let receipt = UsageReceipt {
            receipt_id: "receipt".into(),
            call_id: "c".into(),
            lease_id: "l".into(),
            tenant_id: "t".into(),
            user_id: "u".into(),
            provider_id: "registered-provider".into(),
            model,
            price_book_revision: "book-r1".into(),
            units: Default::default(),
            settled: true,
            metering: None,
        };
        let result = ModelExecution {
            call_id: "c".into(),
            lease_id: "l".into(),
            request_sha256: lease.request_sha256.clone(),
            state: ExecutionState::Completed,
            upstream_status: 200,
            output: serde_json::json!({"data":[]}),
            receipt,
        };
        (call, lease, result)
    }
    #[test]
    fn output_requires_exact_actor_model_lease_hash_and_settlement() {
        let (call, lease, result) = fixture();
        assert!(check_execution(&call, &lease, &result).is_ok());
        for index in 0..7 {
            let mut changed = result.clone();
            match index {
                0 => changed.receipt.tenant_id = "other".into(),
                1 => changed.receipt.user_id = "other".into(),
                2 => changed.receipt.model.revision = "other".into(),
                3 => changed.request_sha256 = "other".into(),
                4 => changed.receipt.lease_id = "other".into(),
                5 => changed.receipt.settled = false,
                _ => changed.state = ExecutionState::DispatchUnknown,
            };
            assert!(check_execution(&call, &lease, &changed).is_err());
        }
    }
    #[tokio::test]
    async fn no_actor_or_client_context_cannot_dispatch() {
        assert!(matches!(
            execute(
                "p",
                "m",
                "r",
                ModelPurpose::Judge,
                ModelWire::ChatCompletions,
                serde_json::json!({"messages":[]}),
                Duration::from_secs(1)
            )
            .await,
            Err(QueryError::Forbidden)
        ));
    }
}
