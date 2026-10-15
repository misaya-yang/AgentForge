//! Canonical private broker routes, enabled only by explicit trusted composition.
use super::{
    execution_repository::LocalNodeExecutionRepository, models::digest_payload, protocol::*,
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, sync::Arc};
pub const ACTION_PATH: &str = "/internal/v2/agent-capabilities/local-node/action";
pub const RECEIPTS_PATH: &str = "/internal/v2/agent-capabilities/local-node/receipts";
#[derive(Clone, Debug)]
pub struct BrokerError {
    pub status: u16,
    pub detail: String,
}
impl BrokerError {
    pub fn new(status: u16, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
        }
    }
}
impl IntoResponse for BrokerError {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"detail":self.detail})),
        )
            .into_response()
    }
}
pub type BrokerFuture<'a, T> =
    Pin<Box<dyn Future<Output = std::result::Result<T, BrokerError>> + Send + 'a>>;
#[derive(Clone, Debug)]
pub struct BrokerScope {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub execution_id: String,
    pub run_id: String,
}
/// Must verify the independently configured internal token AND request-bound
/// capability proof for exact POST path/body and all five scope headers.
pub trait BrokerAuthorization: Send + Sync {
    fn authorize(
        &self,
        headers: &HeaderMap,
        path: &str,
        body: &Value,
    ) -> std::result::Result<BrokerScope, BrokerError>;
}
pub trait LocalNodeDeviceAdapter: Send + Sync {
    fn dispatch<'a>(&'a self, action: &'a LocalNodeAction) -> BrokerFuture<'a, ()>;
}
/// Authentication of the device channel is external to receipt JSON. No body
/// field may supply a principal, device credential or verifier authority.
pub trait LocalNodeDeviceChannelVerifier: Send + Sync {
    fn verify<'a>(
        &'a self,
        headers: &'a HeaderMap,
        purpose: &'a str,
        body_digest: &'a str,
        expected_device: &'a str,
    ) -> BrokerFuture<'a, LocalNodeDeviceScope>;
}
#[derive(Clone)]
pub struct LocalNodeBroker {
    pub repository: Option<Arc<dyn LocalNodeExecutionRepository>>,
    pub device: Option<Arc<dyn LocalNodeDeviceAdapter>>,
    pub authorization: Arc<dyn BrokerAuthorization>,
    pub channel_verifier: Option<Arc<dyn LocalNodeDeviceChannelVerifier>>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalNodeActionBody {
    pub lease_id: String,
    pub execution_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub attempt_id: String,
    pub device_id: String,
    pub channel_id: String,
    pub capability_revision: i64,
    pub capability_id: String,
    pub effect: String,
    pub operation: String,
    pub arguments: Value,
    pub arguments_sha256: String,
    pub idempotency_key: String,
    #[serde(default)]
    pub approval_id: Option<String>,
    #[serde(default)]
    pub grant_id: Option<String>,
    #[serde(default)]
    pub grant_revision: Option<i64>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalNodeReceiptBody {
    pub execution_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub device_id: String,
    pub channel_id: String,
    pub dispatch_fence: String,
    pub sequence: i64,
    pub status: LocalNodeReceiptStatus,
    pub event: String,
    #[serde(default = "empty_object")]
    pub payload: Value,
}
fn empty_object() -> Value {
    json!({})
}
#[doc(hidden)]
pub fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}
pub fn local_node_broker_router(broker: LocalNodeBroker) -> Router {
    Router::new()
        .route(ACTION_PATH, post(dispatch_action))
        .route(RECEIPTS_PATH, post(record_receipt))
        .with_state(broker)
}
async fn dispatch_action(
    State(broker): State<LocalNodeBroker>,
    headers: HeaderMap,
    Json(payload): Json<LocalNodeActionBody>,
) -> std::result::Result<Json<Value>, BrokerError> {
    let body =
        serde_json::to_value(&payload).map_err(|_| BrokerError::new(422, "invalid action body"))?;
    let scope = broker
        .authorization
        .authorize(&headers, ACTION_PATH, &body)?;
    if payload.execution_id != scope.execution_id
        || payload.run_id != scope.run_id
        || payload.lease_id != header(&headers, "x-ai-lease-id")
        || payload.tool_call_id != header(&headers, "x-ai-tool-call-id")
    {
        return Err(BrokerError::new(
            403,
            "execution scope does not match headers",
        ));
    }
    let repository = broker
        .repository
        .as_ref()
        .ok_or_else(|| BrokerError::new(503, "Local Node broker unavailable"))?;
    let device = broker
        .device
        .as_ref()
        .ok_or_else(|| BrokerError::new(503, "Local Node broker unavailable"))?;
    let mut action = LocalNodeAction {
        scope: LocalNodeDeviceScope {
            tenant_id: scope.tenant_id,
            user_id: scope.user_id,
            session_id: scope.session_id,
            device_id: payload.device_id,
            channel_id: payload.channel_id,
        },
        lease_id: payload.lease_id,
        execution_id: payload.execution_id,
        run_id: payload.run_id,
        tool_call_id: payload.tool_call_id,
        attempt_id: payload.attempt_id,
        capability_revision: payload.capability_revision,
        capability_id: payload.capability_id,
        effect: payload.effect,
        operation: payload.operation,
        arguments: payload.arguments,
        arguments_sha256: payload.arguments_sha256,
        idempotency_key: payload.idempotency_key,
        approval_id: payload.approval_id,
        grant_id: payload.grant_id,
        grant_revision: payload.grant_revision,
        dispatch_fence: None,
    };
    action
        .validate()
        .map_err(|e| BrokerError::new(422, e.message))?;
    if action.capability_id.chars().count() > 160
        || action.operation.chars().count() > 160
        || action.idempotency_key.chars().count() > 160
    {
        return Err(BrokerError::new(422, "action field exceeds bound"));
    }
    let claimed = repository
        .claim_dispatch(&action)
        .await
        .map_err(|e| BrokerError::new(409, e.0))?;
    if !claimed {
        let existing = repository
            .execution_result(&action)
            .await
            .map_err(|e| BrokerError::new(409, e.0))?
            .ok_or_else(|| BrokerError::new(409, "dispatch already claimed; receipt required"))?;
        return Ok(Json(
            json!({"accepted":false,"replayed":true,"execution_id":action.execution_id,"result":existing}),
        ));
    }
    action.dispatch_fence = Some(
        repository
            .dispatch_fence_for(&action)
            .await
            .map_err(|e| BrokerError::new(409, e.0))?,
    );
    if device.dispatch(&action).await.is_err() {
        let _ = repository.mark_side_effect_unknown(&action).await;
        return Err(BrokerError::new(502, "Local Node dispatch outcome unknown"));
    }
    Ok(Json(
        json!({"accepted":true,"execution_id":action.execution_id,"device_id":action.scope.device_id}),
    ))
}
async fn record_receipt(
    State(broker): State<LocalNodeBroker>,
    headers: HeaderMap,
    Json(payload): Json<LocalNodeReceiptBody>,
) -> std::result::Result<Json<Value>, BrokerError> {
    let verifier = broker
        .channel_verifier
        .as_ref()
        .ok_or_else(|| BrokerError::new(503, "Local Node receipt adapter unavailable"))?;
    let repository = broker
        .repository
        .as_ref()
        .ok_or_else(|| BrokerError::new(503, "Local Node receipt adapter unavailable"))?;
    let body = serde_json::to_value(&payload)
        .map_err(|_| BrokerError::new(422, "invalid receipt body"))?;
    let digest = format!("sha256:{}", digest_payload(&body));
    let principal = verifier
        .verify(&headers, "capability.receipt", &digest, &payload.device_id)
        .await
        .map_err(|_| BrokerError::new(403, "device receipt rejected"))?;
    if principal.tenant_id != payload.tenant_id
        || principal.user_id != payload.user_id
        || principal.session_id != payload.session_id
        || principal.device_id != payload.device_id
    {
        return Err(BrokerError::new(403, "receipt scope mismatch"));
    }
    for value in [
        &payload.execution_id,
        &payload.tenant_id,
        &payload.user_id,
        &payload.session_id,
        &payload.device_id,
        &payload.channel_id,
        &payload.dispatch_fence,
    ] {
        if value.is_empty() || value.chars().count() > 255 {
            return Err(BrokerError::new(422, "receipt field invalid"));
        }
    }
    if payload.event.is_empty()
        || payload.event.chars().count() > 64
        || !payload.payload.is_object()
    {
        return Err(BrokerError::new(422, "receipt event or payload invalid"));
    }
    let receipt = LocalNodeReceipt {
        execution_id: payload.execution_id,
        tenant_id: payload.tenant_id,
        user_id: payload.user_id,
        session_id: payload.session_id,
        device_id: payload.device_id,
        channel_id: Some(payload.channel_id),
        dispatch_fence: payload.dispatch_fence,
        sequence: payload.sequence,
        status: payload.status,
        event: payload.event,
        payload: payload.payload,
    };
    let sequence = repository
        .append_receipt(&receipt)
        .await
        .map_err(|e| BrokerError::new(409, e.0))?;
    Ok(Json(
        json!({"accepted":true,"accepted_through_sequence":sequence}),
    ))
}

