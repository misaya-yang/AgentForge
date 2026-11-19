//! Protocol-only loopback Gateway fixture. Provider outputs and receipts here are
//! synthetic; no quality, authority/database, billing or acceptance claim follows.
use ai_platform_knowledge_service::migration_knowledge_query::{
    model_plane::{self, ModelPlaneClient, ModelResource},
    types::UserContext,
};
use ai_platform_service_primitives::knowledge_model::*;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Clone)]
struct Vendor {
    endpoint: String,
    key: String,
}
struct FixtureProof;
impl KnowledgeWorkloadSigner for FixtureProof {
    fn sign(&self, _: &str, _: &[u8], _: &str, _: &str) -> Result<String, &'static str> {
        Ok("fixture-workload".into())
    }
}
impl ServiceModelLeaseVerifier for FixtureProof {
    fn verify(&self, token: &str, lease: &ModelLease) -> Result<(), &'static str> {
        if token == format!("fixture-lease:{}", lease.call_id) {
            Ok(())
        } else {
            Err("fixture_lease_binding_invalid")
        }
    }
}
pub struct Plane {
    pub client: Arc<ModelPlaneClient>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Plane {
    fn drop(&mut self) {
        self.server.abort();
    }
}
fn signed(headers: &HeaderMap) -> Result<(), StatusCode> {
    if headers
        .get("x-knowledge-workload")
        .and_then(|v| v.to_str().ok())
        == Some("fixture-workload")
    {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}
async fn reserve(
    headers: HeaderMap,
    Json(request): Json<ReserveRequest>,
) -> Result<Json<ModelLease>, StatusCode> {
    signed(&headers)?;
    request
        .call
        .validate()
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if call_digest(&request.call).unwrap() != request.request_sha256 {
        return Err(StatusCode::CONFLICT);
    }
    let call = request.call;
    Ok(Json(ModelLease {
        lease_id: call.call_id.clone(),
        lease_token: format!("fixture-lease:{}", call.call_id),
        call_id: call.call_id,
        request_sha256: request.request_sha256,
        tenant_id: call.scope.actor.tenant_id,
        user_id: call.scope.actor.user_id,
        model: call.model,
        expires_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60,
    }))
}
async fn execute(
    State(vendor): State<Vendor>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ExecuteRequest>,
) -> Result<Json<ModelExecution>, StatusCode> {
    signed(&headers)?;
    if request.call.call_id != id
        || request.lease_token != format!("fixture-lease:{id}")
        || call_digest(&request.call).unwrap() != request.request_sha256
    {
        return Err(StatusCode::CONFLICT);
    }
    let response = reqwest::Client::new()
        .post(&vendor.endpoint)
        .bearer_auth(vendor.key)
        .json(&request.call.payload)
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let status = response.status().as_u16();
    let output = response.json().await.map_err(|_| StatusCode::BAD_GATEWAY)?;
    let call = request.call;
    Ok(Json(ModelExecution {
        call_id: call.call_id.clone(),
        lease_id: id.clone(),
        request_sha256: request.request_sha256,
        state: if (200..300).contains(&status) {
            ExecutionState::Completed
        } else {
            ExecutionState::Rejected
        },
        upstream_status: status,
        output,
        receipt: UsageReceipt {
            receipt_id: format!("fixture-{id}"),
            call_id: call.call_id,
            lease_id: id,
            tenant_id: call.scope.actor.tenant_id,
            user_id: call.scope.actor.user_id,
            provider_id: "fixture-provider".into(),
            model: call.model,
            price_book_revision: "fixture-not-a-pricebook".into(),
            units: BTreeMap::from([("fixture_requests".into(), 1)]),
            settled: true,
        metering: None,
        },
    }))
}
impl Plane {
    pub async fn new(endpoint: String, key: String) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .route(RESERVE_PATH, post(reserve))
            .route(&format!("{RESERVE_PATH}/{{id}}/execute"), post(execute))
            .with_state(Vendor { endpoint, key });
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let revisions = [
            ("dashscope/text-embedding-v4", "fixture-r1"),
            ("dashscope_multimodal/multimodal-embedding-v1", "fixture-r1"),
            ("unified_multimodal/multimodal-embedding-v1", "fixture-r1"),
            ("dashscope/gte-rerank-v2", "fixture-r1"),
            ("dashscope/qwen3-rerank", "fixture-r1"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        let client = Arc::new(
            ModelPlaneClient::for_workload(
                origin.parse().unwrap(),
                Arc::new(FixtureProof),
                Arc::new(FixtureProof),
            )
            .unwrap()
            .with_model_revisions(revisions)
            .unwrap(),
        );
        Self { client, server }
    }
}
pub async fn run<F: Future>(client: Arc<ModelPlaneClient>, future: F) -> F::Output {
    let actor = UserContext {
        tenant_id: "tenant-fixture".into(),
        user_id: "user-fixture".into(),
        roles: vec!["user".into()],
        user_tier: "normal".into(),
        user_type: "user".into(),
        is_authenticated: true,
    };
    model_plane::run_scoped(
        client,
        ModelResource {
            dataset_id: Some("fixture".into()),
            document_id: None,
            execution_id: None,
        },
        actor,
        future,
    )
    .await
}
