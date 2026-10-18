//! Worker-side client for Gateway-owned Local Node actions.
//!
//! The worker sends one authenticated dispatch request and then reads the
//! Gateway's durable receipts. It does not keep a second action, approval, or
//! receipt ledger, and it never talks directly to the companion device.

use crate::workload_identity::WorkloadSend;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_platform_capability_contract::{
    CapabilityDescriptorV2, CapabilityEffect, CapabilityExecutionStatus, ExecutionMode,
    canonical_json_hash,
};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use reqwest::{Client, Response, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::Sha256;
use uuid::Uuid;

const CATALOG_SCHEMA: &str = "local-node-capability/v2";
const WORKER_SCHEMA: &str = "ai-platform.local-node.worker.v1";
const PROOF_SCHEMA: &str = "ai-platform-capability-proof/v1";
const PROOF_TTL_SECONDS: u64 = 30;
const MIN_SECRET_BYTES: usize = 32;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_ARGUMENT_BYTES: usize = crate::MAX_REQUEST_BYTES;
const MAX_RECEIPT_EVENTS: usize = 256;
const MAX_SEQUENCE_GAP: u64 = 10_000;
const MAX_CONTEXT_FIELD: usize = 255;
const MAX_OPERATION: usize = 160;
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const CANCEL_RECONCILE_TIMEOUT: Duration = Duration::from_secs(2);
const PATH_DESCRIBE: &str = "/internal/v2/agent-capabilities/local-node/describe";
const PATH_DISPATCH: &str = "/internal/v2/agent-capabilities/local-node/dispatch";
const PATH_STATUS: &str = "/internal/v2/agent-capabilities/local-node/status";
const PATH_CANCEL: &str = "/internal/v2/agent-capabilities/local-node/cancel";
type HmacSha256 = Hmac<Sha256>;

/// Verified execution facts copied from the Worker dispatch record. These
/// values are not reconstructed from the model's action arguments.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalNodeExecutionContext {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub execution_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub lease_id: String,
    pub attempt_id: String,
    pub capability_id: String,
    pub capability_revision: u64,
    pub effect: CapabilityEffect,
}

/// Execution context and the device handle resolved from the persisted
/// Runtime resource binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalNodeScope {
    pub context: LocalNodeExecutionContext,
    pub device_id: String,
}

/// A request to execute one operation on the bound device. `approval_id` is
/// an opaque one-time approval handle; Gateway validates and consumes it.
#[derive(Clone, Debug)]
pub struct LocalNodeActionRequest {
    pub scope: LocalNodeScope,
    pub operation: String,
    pub arguments: Value,
    pub arguments_hash: String,
    pub idempotency_key: String,
    pub approval_id: Option<String>,
    pub timeout: Duration,
    /// Local Worker cancellation signal. It is never serialized to Gateway.
    pub cancel_requested: Arc<AtomicBool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalNodeDescriptor {
    pub device_id: String,
    pub capability_revision: u64,
    pub capabilities: BTreeSet<String>,
    pub online: bool,
    pub expires_at_epoch_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalNodeReceiptEvent {
    pub execution_id: String,
    pub device_id: String,
    pub sequence: u64,
    pub status: CapabilityExecutionStatus,
    pub event: String,
    #[serde(default)]
    pub payload: BTreeMap<String, Value>,
}

impl LocalNodeReceiptEvent {
    fn terminal(&self) -> bool {
        self.status.is_terminal()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct LocalNodeActionResult {
    pub status: CapabilityExecutionStatus,
    pub result: Option<Value>,
    pub last_sequence: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LocalNodeBrokerError {
    #[error("local_node_scope_invalid")]
    ScopeInvalid,
    #[error("local_node_arguments_invalid")]
    ArgumentsInvalid,
    #[error("local_node_arguments_hash_mismatch")]
    ArgumentsHashMismatch,
    #[error("local_node_approval_required")]
    ApprovalRequired,
    #[error("local_node_dispatch_rejected")]
    DispatchRejected,
    #[error("local_node_cancelled_before_dispatch")]
    CancelledBeforeDispatch,
    #[error("local_node_receipt_invalid")]
    ReceiptInvalid,
    #[error("local_node_receipt_sequence_invalid")]
    ReceiptSequenceInvalid,
    #[error("local_node_transport_unavailable")]
    TransportUnavailable,
    #[error("local_node_side_effect_unknown")]
    SideEffectUnknown,
    #[error("local_node_timeout")]
    Timeout,
}

#[async_trait]
pub trait LocalNodeTransport: Send + Sync {
    /// Return a current capability snapshot for the execution's device.
    async fn describe(
        &self,
        scope: &LocalNodeScope,
    ) -> Result<LocalNodeDescriptor, LocalNodeBrokerError>;

    /// Make the single dispatch POST. Success means Gateway durably accepted
    /// the action; completion still requires a terminal receipt.
    async fn dispatch(&self, request: &LocalNodeActionRequest) -> Result<(), LocalNodeBrokerError>;

    /// Read Gateway-owned durable receipts after the given sequence.
    async fn status(
        &self,
        request: &LocalNodeActionRequest,
        after_sequence: u64,
    ) -> Result<Vec<LocalNodeReceiptEvent>, LocalNodeBrokerError>;

    /// Request cancellation of the already accepted action. This never
    /// re-dispatches it and may return before the device reports terminal state.
    async fn cancel(&self, request: &LocalNodeActionRequest) -> Result<(), LocalNodeBrokerError>;
}

/// Fail-closed transport for legacy or deliberately unconfigured composition.
pub struct UnavailableLocalNodeTransport;

#[async_trait]
impl LocalNodeTransport for UnavailableLocalNodeTransport {
    async fn describe(
        &self,
        _: &LocalNodeScope,
    ) -> Result<LocalNodeDescriptor, LocalNodeBrokerError> {
        Err(LocalNodeBrokerError::TransportUnavailable)
    }

    async fn dispatch(&self, _: &LocalNodeActionRequest) -> Result<(), LocalNodeBrokerError> {
        Err(LocalNodeBrokerError::TransportUnavailable)
    }

    async fn status(
        &self,
        _: &LocalNodeActionRequest,
        _: u64,
    ) -> Result<Vec<LocalNodeReceiptEvent>, LocalNodeBrokerError> {
        Err(LocalNodeBrokerError::TransportUnavailable)
    }

    async fn cancel(&self, _: &LocalNodeActionRequest) -> Result<(), LocalNodeBrokerError> {
        Err(LocalNodeBrokerError::TransportUnavailable)
    }
}

#[derive(Clone, Debug)]
pub struct LocalNodeGatewayConfig {
    /// Operator-configured Gateway origin. Request bodies never select a host.
    pub gateway_url: String,
    pub internal_token: String,
    pub proof_secret: String,
}

impl LocalNodeGatewayConfig {
    fn endpoint(&self, path: &str) -> Result<Url, LocalNodeBrokerError> {
        let mut base = Url::parse(&self.gateway_url)
            .map_err(|_| LocalNodeBrokerError::TransportUnavailable)?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || (!crate::workload_identity::required() && self.internal_token.len() < MIN_SECRET_BYTES)
            || (!crate::broker_proof::required() && self.proof_secret.len() < MIN_SECRET_BYTES)
            || !is_worker_path(path)
        {
            return Err(LocalNodeBrokerError::TransportUnavailable);
        }
        base.set_path(path);
        Ok(base)
    }
}

/// Fixed-origin, proof-authenticated HTTP client for the Gateway Local Node
/// worker API. Redirects should be disabled on the supplied client.
#[derive(Clone)]
pub struct ReqwestLocalNodeTransport {
    client: Client,
    config: LocalNodeGatewayConfig,
}

impl ReqwestLocalNodeTransport {
    pub fn new(
        client: Client,
        config: LocalNodeGatewayConfig,
    ) -> Result<Self, LocalNodeBrokerError> {
        config.endpoint(PATH_DESCRIBE)?;
        config.endpoint(PATH_DISPATCH)?;
        config.endpoint(PATH_STATUS)?;
        config.endpoint(PATH_CANCEL)?;
        Ok(Self { client, config })
    }

    fn proof(
        &self,
        method: &str,
        path: &str,
        context: &LocalNodeExecutionContext,
        body: &Value,
        now: u64,
    ) -> Result<String, LocalNodeBrokerError> {
        if method != "POST" || !is_worker_path(path) {
            return Err(LocalNodeBrokerError::TransportUnavailable);
        }
        let expires_at = now
            .checked_add(PROOF_TTL_SECONDS)
            .ok_or(LocalNodeBrokerError::TransportUnavailable)?;
        let body_sha256 = canonical_json_hash(body)
            .map_err(|_| LocalNodeBrokerError::ArgumentsInvalid)?
            .strip_prefix("sha256:")
            .ok_or(LocalNodeBrokerError::ArgumentsInvalid)?
            .to_string();
        let mut unsigned = Map::new();
        unsigned.insert("body_sha256".into(), Value::String(body_sha256));
        unsigned.insert(
            "execution_id".into(),
            Value::String(context.execution_id.clone()),
        );
        unsigned.insert("expires_at".into(), json!(expires_at));
        unsigned.insert("method".into(), Value::String(method.into()));
        unsigned.insert(
            "nonce".into(),
            Value::String(Uuid::now_v7().simple().to_string()),
        );
        unsigned.insert("path".into(), Value::String(path.into()));
        unsigned.insert("run_id".into(), Value::String(context.run_id.clone()));
        unsigned.insert("schema_version".into(), Value::String(PROOF_SCHEMA.into()));
        unsigned.insert(
            "session_id".into(),
            Value::String(context.session_id.clone()),
        );
        unsigned.insert("tenant_id".into(), Value::String(context.tenant_id.clone()));
        unsigned.insert("user_id".into(), Value::String(context.user_id.clone()));
        let bytes = serde_json::to_vec(&Value::Object(unsigned.clone()))
            .map_err(|_| LocalNodeBrokerError::TransportUnavailable)?;
        let mut mac = HmacSha256::new_from_slice(self.config.proof_secret.as_bytes())
            .map_err(|_| LocalNodeBrokerError::TransportUnavailable)?;
        mac.update(&bytes);
        unsigned.insert(
            "signature".into(),
            Value::String(hex::encode(mac.finalize().into_bytes())),
        );
        Ok(format!(
            "v1.{}",
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&Value::Object(unsigned))
                    .map_err(|_| LocalNodeBrokerError::TransportUnavailable)?
            )
        ))
    }

    async fn post<B>(
        &self,
        path: &str,
        context: &LocalNodeExecutionContext,
        body: &B,
        timeout: Duration,
    ) -> Result<(u16, Vec<u8>), LocalNodeBrokerError>
    where
        B: Serialize,
    {
        // Serialize once so the proof hash and sent JSON cover the same value.
        let body_value =
            serde_json::to_value(body).map_err(|_| LocalNodeBrokerError::ArgumentsInvalid)?;
        let endpoint = self.config.endpoint(path)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| LocalNodeBrokerError::TransportUnavailable)?
            .as_secs();
        let proof = if crate::broker_proof::required() { crate::broker_proof::issue(path, &body_value).await.map_err(|_| LocalNodeBrokerError::TransportUnavailable)? } else { self.proof("POST", path, context, &body_value, now)? };
        let request_builder = self
            .client
            .post(endpoint)
            .header("x-ai-platform-internal-token", &self.config.internal_token)
            .header("x-ai-tenant-id", &context.tenant_id)
            .header("x-ai-user-id", &context.user_id)
            .header("x-ai-session-id", &context.session_id)
            // The proof covers the body context; Gateway also checks this
            // lease header against that signed body value.
            .header("x-ai-lease-id", &context.lease_id)
            .header("x-ai-tool-call-id", &context.tool_call_id)
            .header("x-ai-capability-proof", proof);
        let response = crate::trace_context::apply(
            request_builder,
            &context.run_id,
            &context.run_id,
            &context.execution_id,
        )
        .json(&body_value)
        .timeout(timeout)
        .workload_send()
        .await
        .map_err(|_| LocalNodeBrokerError::TransportUnavailable)?;
        let status = response.status().as_u16();
        let bytes = bounded_response(response).await?;
        Ok((status, bytes))
    }
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct DescribeBody<'a> {
    schema_version: &'static str,
    context: &'a LocalNodeExecutionContext,
    device_id: &'a str,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct DispatchBody<'a> {
    schema_version: &'static str,
    context: &'a LocalNodeExecutionContext,
    device_id: &'a str,
    operation: &'a str,
    arguments: &'a Value,
    arguments_hash: &'a str,
    idempotency_key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    approval_id: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct StatusBody<'a> {
    schema_version: &'static str,
    context: &'a LocalNodeExecutionContext,
    device_id: &'a str,
    after_sequence: u64,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct CancelBody<'a> {
    schema_version: &'static str,
    context: &'a LocalNodeExecutionContext,
    device_id: &'a str,
    action_id: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchResponse {
    accepted: bool,
    execution_id: String,
    action_id: String,
    replayed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusResponse {
    events: Vec<LocalNodeReceiptEvent>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancelResponse {
    accepted: bool,
    execution_id: String,
}

#[async_trait]
impl LocalNodeTransport for ReqwestLocalNodeTransport {
    async fn describe(
        &self,
        scope: &LocalNodeScope,
    ) -> Result<LocalNodeDescriptor, LocalNodeBrokerError> {
        validate_scope(scope)?;
        let body = DescribeBody {
            schema_version: WORKER_SCHEMA,
            context: &scope.context,
            device_id: &scope.device_id,
        };
        let (status, bytes) = self
            .post(PATH_DESCRIBE, &scope.context, &body, READ_TIMEOUT)
            .await?;
        if !(200..300).contains(&status) {
            return Err(LocalNodeBrokerError::TransportUnavailable);
        }
        serde_json::from_slice(&bytes).map_err(|_| LocalNodeBrokerError::ReceiptInvalid)
    }

    async fn dispatch(&self, request: &LocalNodeActionRequest) -> Result<(), LocalNodeBrokerError> {
        validate_request(request)?;
        let body = DispatchBody {
            schema_version: WORKER_SCHEMA,
            context: &request.scope.context,
            device_id: &request.scope.device_id,
            operation: &request.operation,
            arguments: &request.arguments,
            arguments_hash: &request.arguments_hash,
            idempotency_key: &request.idempotency_key,
            approval_id: request.approval_id.as_deref(),
        };
        let (status, bytes) = self
            .post(
                PATH_DISPATCH,
                &request.scope.context,
                &body,
                request.timeout.min(DISPATCH_TIMEOUT),
            )
            .await
            .map_err(|_| LocalNodeBrokerError::SideEffectUnknown)?;
        if (400..500).contains(&status) {
            return Err(LocalNodeBrokerError::DispatchRejected);
        }
        if !(200..300).contains(&status) {
            return Err(LocalNodeBrokerError::SideEffectUnknown);
        }
        let response: DispatchResponse =
            serde_json::from_slice(&bytes).map_err(|_| LocalNodeBrokerError::SideEffectUnknown)?;
        if !response.accepted
            || response.execution_id != request.scope.context.execution_id
            || response.action_id != request.scope.context.execution_id
        {
            return Err(LocalNodeBrokerError::SideEffectUnknown);
        }
        let _gateway_idempotent_replay = response.replayed;
        Ok(())
    }

    async fn status(
        &self,
        request: &LocalNodeActionRequest,
        after_sequence: u64,
    ) -> Result<Vec<LocalNodeReceiptEvent>, LocalNodeBrokerError> {
        let body = StatusBody {
            schema_version: WORKER_SCHEMA,
            context: &request.scope.context,
            device_id: &request.scope.device_id,
            after_sequence,
        };
        let (status, bytes) = self
            .post(PATH_STATUS, &request.scope.context, &body, READ_TIMEOUT)
            .await?;
        if !(200..300).contains(&status) {
            return Err(LocalNodeBrokerError::TransportUnavailable);
        }
        let response: StatusResponse =
            serde_json::from_slice(&bytes).map_err(|_| LocalNodeBrokerError::ReceiptInvalid)?;
        if response.events.len() > MAX_RECEIPT_EVENTS {
            return Err(LocalNodeBrokerError::ReceiptInvalid);
        }
        Ok(response.events)
    }

    async fn cancel(&self, request: &LocalNodeActionRequest) -> Result<(), LocalNodeBrokerError> {
        let execution_id = &request.scope.context.execution_id;
        let body = CancelBody {
            schema_version: WORKER_SCHEMA,
            context: &request.scope.context,
            device_id: &request.scope.device_id,
            action_id: execution_id,
        };
        let (status, bytes) = self
            .post(PATH_CANCEL, &request.scope.context, &body, READ_TIMEOUT)
            .await?;
        if !(200..300).contains(&status) {
            return Err(LocalNodeBrokerError::TransportUnavailable);
        }
        let response: CancelResponse =
            serde_json::from_slice(&bytes).map_err(|_| LocalNodeBrokerError::ReceiptInvalid)?;
        if !response.accepted || response.execution_id != *execution_id {
            return Err(LocalNodeBrokerError::ReceiptInvalid);
        }
        Ok(())
    }
}

async fn bounded_response(mut response: Response) -> Result<Vec<u8>, LocalNodeBrokerError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(LocalNodeBrokerError::ReceiptInvalid);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| LocalNodeBrokerError::TransportUnavailable)?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(LocalNodeBrokerError::ReceiptInvalid);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Broker state has no device-side execution database. Gateway is the only
/// source of truth for dispatch, approval consumption, receipts, and results.
#[derive(Clone)]
pub struct LocalNodeBroker {
    transport: Arc<dyn LocalNodeTransport>,
}

impl LocalNodeBroker {
    pub fn new(transport: Arc<dyn LocalNodeTransport>) -> Self {
        Self { transport }
    }

    /// The only catalog entries exposed by this broker. Device-specific
    /// actions are data returned by `describe`, never model-name branches.
    pub fn catalog() -> Vec<CapabilityDescriptorV2> {
        vec![
            descriptor(
                "local_node_catalog",
                "List paired Local Node capabilities",
                CapabilityEffect::Read,
                ExecutionMode::Inline,
                json!({"type":"object","required":["device_id"],"properties":{"device_id":{"type":"string"}},"additionalProperties":false}),
                json!({"type":"array"}),
            ),
            descriptor(
                "local_node_describe",
                "Describe one paired Local Node",
                CapabilityEffect::Read,
                ExecutionMode::Inline,
                json!({"type":"object","required":["device_id"],"properties":{"device_id":{"type":"string"}},"additionalProperties":false}),
                json!({"type":"object"}),
            ),
            descriptor(
                "local_node_action",
                "Dispatch an explicitly authorized operation to a paired Local Node",
                CapabilityEffect::Write,
                ExecutionMode::Job,
                json!({
                    "type": "object",
                    "properties": {
                        "operation": {"type": "string", "minLength": 1, "maxLength": 160},
                        "arguments": {"type": "object"}
                    },
                    "required": ["operation", "arguments"],
                    "additionalProperties": false
                }),
                json!({
                    "type": "object",
                    "properties": {"status": {"type": "string"}},
                    "required": ["status"]
                }),
            ),
        ]
    }

    pub async fn describe(
        &self,
        scope: &LocalNodeScope,
    ) -> Result<LocalNodeDescriptor, LocalNodeBrokerError> {
        validate_scope(scope)?;
        let descriptor = self.transport.describe(scope).await?;
        if descriptor.device_id != scope.device_id {
            return Err(LocalNodeBrokerError::ReceiptInvalid);
        }
        Ok(descriptor)
    }

    pub async fn execute(
        &self,
        request: LocalNodeActionRequest,
    ) -> Result<LocalNodeActionResult, LocalNodeBrokerError> {
        validate_request(&request)?;
        if request.approval_id.is_none()
            && matches!(
                request.scope.context.effect,
                CapabilityEffect::Write | CapabilityEffect::Unknown
            )
        {
            return Err(LocalNodeBrokerError::ApprovalRequired);
        }
        if request.cancel_requested.load(Ordering::Acquire) {
            return Err(LocalNodeBrokerError::CancelledBeforeDispatch);
        }

        // Exactly one dispatch attempt. An ambiguous result is reconciled only
        // through readonly status; the action POST is never repeated.
        let deadline = tokio::time::Instant::now() + request.timeout;
        match self.transport.dispatch(&request).await {
            Ok(()) => {}
            Err(LocalNodeBrokerError::DispatchRejected) => {
                return Err(LocalNodeBrokerError::DispatchRejected);
            }
            Err(_) => {}
        }

        let mut cursor = 0;
        loop {
            if request.cancel_requested.load(Ordering::Acquire)
                || tokio::time::Instant::now() >= deadline
            {
                return self.cancel_then_reconcile(&request, cursor).await;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let events = match tokio::time::timeout(
                remaining.min(READ_TIMEOUT),
                self.transport.status(&request, cursor),
            )
            .await
            {
                Ok(Ok(events)) if events.len() <= MAX_RECEIPT_EVENTS => events,
                _ => return self.cancel_then_reconcile(&request, cursor).await,
            };
            for event in events {
                if validate_receipt(&request, &event, cursor).is_err() {
                    return Err(LocalNodeBrokerError::SideEffectUnknown);
                }
                cursor = event.sequence;
                if event.terminal() {
                    if event.status == CapabilityExecutionStatus::Succeeded
                        && event.payload.get("result").is_none()
                    {
                        return Err(LocalNodeBrokerError::SideEffectUnknown);
                    }
                    return Ok(LocalNodeActionResult {
                        status: event.status,
                        result: event.payload.get("result").cloned(),
                        last_sequence: cursor,
                    });
                }
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            tokio::time::sleep(remaining.min(Duration::from_millis(25))).await;
        }
        self.cancel_then_reconcile(&request, cursor).await
    }

    async fn cancel_then_reconcile(
        &self,
        request: &LocalNodeActionRequest,
        mut cursor: u64,
    ) -> Result<LocalNodeActionResult, LocalNodeBrokerError> {
        let _ = tokio::time::timeout(READ_TIMEOUT, self.transport.cancel(request)).await;
        let deadline = tokio::time::Instant::now() + CANCEL_RECONCILE_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let events = match tokio::time::timeout(
                remaining.min(READ_TIMEOUT),
                self.transport.status(request, cursor),
            )
            .await
            {
                Ok(Ok(events)) if events.len() <= MAX_RECEIPT_EVENTS => events,
                _ => return Err(LocalNodeBrokerError::SideEffectUnknown),
            };
            for event in events {
                if validate_receipt(request, &event, cursor).is_err() {
                    return Err(LocalNodeBrokerError::SideEffectUnknown);
                }
                cursor = event.sequence;
                if event.terminal() {
                    if event.status == CapabilityExecutionStatus::Succeeded
                        && event.payload.get("result").is_none()
                    {
                        return Err(LocalNodeBrokerError::SideEffectUnknown);
                    }
                    return Ok(LocalNodeActionResult {
                        status: event.status,
                        result: event.payload.get("result").cloned(),
                        last_sequence: cursor,
                    });
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        Err(LocalNodeBrokerError::SideEffectUnknown)
    }

    /// Classify a lost device channel after dispatch. This helper preserves
    /// terminal evidence and otherwise reports an unknowable side effect.
    pub fn classify_disconnect_after_dispatch(
        status: CapabilityExecutionStatus,
    ) -> CapabilityExecutionStatus {
        if status.is_terminal() {
            status
        } else {
            CapabilityExecutionStatus::SideEffectUnknown
        }
    }
}

fn descriptor(
    id: &str,
    description: &str,
    effect: CapabilityEffect,
    execution_mode: ExecutionMode,
    input_schema: Value,
    output_schema: Value,
) -> CapabilityDescriptorV2 {
    CapabilityDescriptorV2 {
        schema_version: "capability-descriptor/v2".into(),
        id: id.into(),
        name: id.replace('_', " "),
        version: "2".into(),
        description: description.into(),
        schema_hash: canonical_json_hash(&input_schema).unwrap_or_else(|_| "sha256:invalid".into()),
        input_schema,
        output_schema,
        effect,
        approval_policy: if id == "local_node_action" {
            ai_platform_capability_contract::ApprovalPolicy::Always
        } else {
            ai_platform_capability_contract::ApprovalPolicy::Never
        },
        execution_mode,
        timeout_ms: if id == "local_node_action" {
            30_000
        } else {
            5_000
        },
        tags: vec!["local_node".into(), "device_bound".into()],
        protocol: CATALOG_SCHEMA.into(),
        connector_binding: None,
    }
}

fn validate_scope(scope: &LocalNodeScope) -> Result<(), LocalNodeBrokerError> {
    let context = &scope.context;
    if [
        &context.tenant_id,
        &context.user_id,
        &context.session_id,
        &context.execution_id,
        &context.run_id,
        &context.tool_call_id,
        &context.lease_id,
        &context.attempt_id,
        &context.capability_id,
        &scope.device_id,
    ]
    .iter()
    .any(|value| !valid_text(value, MAX_CONTEXT_FIELD))
        || context.capability_revision == 0
        || !matches!(
            context.capability_id.as_str(),
            "local_node_action" | "local_node_catalog" | "local_node_describe"
        )
        || (context.capability_id == "local_node_action"
            && context.effect != CapabilityEffect::Write)
        || (context.capability_id != "local_node_action"
            && context.effect != CapabilityEffect::Read)
    {
        return Err(LocalNodeBrokerError::ScopeInvalid);
    }
    Ok(())
}

fn validate_request(request: &LocalNodeActionRequest) -> Result<(), LocalNodeBrokerError> {
    validate_scope(&request.scope)?;
    if !valid_text(&request.operation, MAX_OPERATION)
        || !valid_text(&request.idempotency_key, MAX_CONTEXT_FIELD)
        || request.timeout.is_zero()
        || request.timeout > Duration::from_secs(120)
        || !request.arguments.is_object()
        || request
            .approval_id
            .as_ref()
            .is_some_and(|value| !valid_text(value, MAX_CONTEXT_FIELD))
    {
        return Err(LocalNodeBrokerError::ArgumentsInvalid);
    }
    let encoded = serde_json::to_vec(&request.arguments)
        .map_err(|_| LocalNodeBrokerError::ArgumentsInvalid)?;
    if encoded.len() > MAX_ARGUMENT_BYTES {
        return Err(LocalNodeBrokerError::ArgumentsInvalid);
    }
    let full_arguments = json!({
        "operation": &request.operation,
        "arguments": &request.arguments,
    });
    let expected =
        canonical_json_hash(&full_arguments).map_err(|_| LocalNodeBrokerError::ArgumentsInvalid)?;
    if expected != request.arguments_hash {
        return Err(LocalNodeBrokerError::ArgumentsHashMismatch);
    }
    Ok(())
}

fn validate_receipt(
    request: &LocalNodeActionRequest,
    event: &LocalNodeReceiptEvent,
    cursor: u64,
) -> Result<(), LocalNodeBrokerError> {
    if event.execution_id != request.scope.context.execution_id
        || event.device_id != request.scope.device_id
        || event.sequence <= cursor
        || event.sequence - cursor > MAX_SEQUENCE_GAP
        || event.event.is_empty()
        || event.event.len() > 160
    {
        return Err(LocalNodeBrokerError::ReceiptSequenceInvalid);
    }
    Ok(())
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn is_worker_path(path: &str) -> bool {
    matches!(
        path,
        PATH_DESCRIBE | PATH_DISPATCH | PATH_STATUS | PATH_CANCEL
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn context(capability_id: &str) -> LocalNodeExecutionContext {
        LocalNodeExecutionContext {
            tenant_id: "t".into(),
            user_id: "u".into(),
            session_id: "s".into(),
            execution_id: "e".into(),
            run_id: "r".into(),
            tool_call_id: "c".into(),
            lease_id: "lease".into(),
            attempt_id: "a".into(),
            capability_id: capability_id.into(),
            capability_revision: 1,
            effect: CapabilityEffect::Write,
        }
    }

    fn scope() -> LocalNodeScope {
        LocalNodeScope {
            context: context("local_node_action"),
            device_id: "d".into(),
        }
    }

    fn request() -> LocalNodeActionRequest {
        let args = json!({"path":"notes.txt"});
        let full_arguments = json!({"operation":"file.read","arguments":&args});
        LocalNodeActionRequest {
            scope: scope(),
            operation: "file.read".into(),
            arguments_hash: canonical_json_hash(&full_arguments).unwrap(),
            arguments: args,
            idempotency_key: "idem".into(),
            approval_id: Some("approval".into()),
            timeout: Duration::from_millis(100),
            cancel_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    struct Fake {
        dispatch_calls: AtomicUsize,
        status_calls: AtomicUsize,
        cancel_calls: AtomicUsize,
        dispatch_error: Option<LocalNodeBrokerError>,
        status_error_after_cancel: bool,
    }

    impl Fake {
        fn ready() -> Self {
            Self {
                dispatch_calls: AtomicUsize::new(0),
                status_calls: AtomicUsize::new(0),
                cancel_calls: AtomicUsize::new(0),
                dispatch_error: None,
                status_error_after_cancel: false,
            }
        }
    }

    #[async_trait]
    impl LocalNodeTransport for Fake {
        async fn describe(
            &self,
            scope: &LocalNodeScope,
        ) -> Result<LocalNodeDescriptor, LocalNodeBrokerError> {
            Ok(LocalNodeDescriptor {
                device_id: scope.device_id.clone(),
                capability_revision: scope.context.capability_revision,
                capabilities: ["file.read".into()].into_iter().collect(),
                online: true,
                expires_at_epoch_ms: 1,
            })
        }

        async fn dispatch(&self, _: &LocalNodeActionRequest) -> Result<(), LocalNodeBrokerError> {
            self.dispatch_calls.fetch_add(1, Ordering::SeqCst);
            match &self.dispatch_error {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }

        async fn status(
            &self,
            request: &LocalNodeActionRequest,
            after_sequence: u64,
        ) -> Result<Vec<LocalNodeReceiptEvent>, LocalNodeBrokerError> {
            self.status_calls.fetch_add(1, Ordering::SeqCst);
            if self.dispatch_error.is_some() {
                return Err(LocalNodeBrokerError::TransportUnavailable);
            }
            if self.status_error_after_cancel {
                return if self.cancel_calls.load(Ordering::SeqCst) > 0 {
                    Err(LocalNodeBrokerError::TransportUnavailable)
                } else {
                    // This fixture models an unresolved action, so it must not
                    // publish a successful receipt before its timeout/cancel.
                    Ok(Vec::new())
                };
            }
            if after_sequence > 0 {
                return Ok(Vec::new());
            }
            Ok(vec![LocalNodeReceiptEvent {
                execution_id: request.scope.context.execution_id.clone(),
                device_id: request.scope.device_id.clone(),
                sequence: 1,
                status: CapabilityExecutionStatus::Succeeded,
                event: "action.succeeded".into(),
                payload: [("result".into(), json!({"content":"ok"}))]
                    .into_iter()
                    .collect(),
            }])
        }

        async fn cancel(&self, _: &LocalNodeActionRequest) -> Result<(), LocalNodeBrokerError> {
            self.cancel_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn successful_receipt_is_the_only_success_signal() {
        let fake = Arc::new(Fake::ready());
        let broker = LocalNodeBroker::new(fake.clone());
        let result = broker.execute(request()).await.unwrap();
        assert_eq!(result.status, CapabilityExecutionStatus::Succeeded);
        assert_eq!(result.last_sequence, 1);
        assert_eq!(fake.dispatch_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ambiguous_dispatch_is_never_replayed_or_polled_as_success() {
        let fake = Arc::new(Fake {
            dispatch_error: Some(LocalNodeBrokerError::TransportUnavailable),
            ..Fake::ready()
        });
        let broker = LocalNodeBroker::new(fake.clone());
        assert_eq!(
            broker.execute(request()).await,
            Err(LocalNodeBrokerError::SideEffectUnknown)
        );
        assert_eq!(fake.dispatch_calls.load(Ordering::SeqCst), 1);
        assert!(fake.status_calls.load(Ordering::SeqCst) > 0);
        assert_eq!(fake.cancel_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn timeout_requests_cancel_once_then_returns_unknown_if_unreconciled() {
        let fake = Arc::new(Fake {
            status_error_after_cancel: true,
            ..Fake::ready()
        });
        let broker = LocalNodeBroker::new(fake.clone());
        let mut req = request();
        req.timeout = Duration::from_millis(1);
        assert_eq!(
            broker.execute(req).await,
            Err(LocalNodeBrokerError::SideEffectUnknown)
        );
        assert_eq!(fake.dispatch_calls.load(Ordering::SeqCst), 1);
        assert_eq!(fake.cancel_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn approval_and_arguments_are_bound_before_dispatch() {
        let fake = Arc::new(Fake::ready());
        let broker = LocalNodeBroker::new(fake.clone());
        let mut req = request();
        req.approval_id = None;
        assert_eq!(
            broker.execute(req).await,
            Err(LocalNodeBrokerError::ApprovalRequired)
        );
        let mut req = request();
        req.arguments = json!({"path":"other.txt"});
        assert_eq!(
            broker.execute(req).await,
            Err(LocalNodeBrokerError::ArgumentsHashMismatch)
        );
        assert_eq!(fake.dispatch_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn cancellation_before_dispatch_does_not_reserve_an_action() {
        let fake = Arc::new(Fake::ready());
        let broker = LocalNodeBroker::new(fake.clone());
        let mut req = request();
        req.cancel_requested.store(true, Ordering::Release);
        assert_eq!(
            broker.execute(req).await,
            Err(LocalNodeBrokerError::CancelledBeforeDispatch)
        );
        assert_eq!(fake.dispatch_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn gateway_config_forces_worker_routes_and_proof_binds_full_request() {
        let config = LocalNodeGatewayConfig {
            gateway_url: "http://gateway.internal:8080/untrusted/base".into(),
            internal_token: "x".repeat(32),
            proof_secret: "y".repeat(32),
        };
        let transport = ReqwestLocalNodeTransport::new(Client::new(), config).unwrap();
        let endpoint = transport.config.endpoint(PATH_DISPATCH).unwrap();
        assert_eq!(endpoint.path(), PATH_DISPATCH);
        let body = json!({
            "schema_version": WORKER_SCHEMA,
            "context": context("local_node_action"),
            "device_id": "d",
            "operation": "file.read",
            "arguments": {"path":"notes.txt"},
            "arguments_hash": "sha256:args",
            "idempotency_key": "idem",
            "approval_id": "approval"
        });
        assert_eq!(body["context"]["lease_id"], "lease");
        let proof = transport
            .proof(
                "POST",
                PATH_DISPATCH,
                &context("local_node_action"),
                &body,
                100,
            )
            .unwrap();
        let encoded = proof.strip_prefix("v1.").unwrap();
        let decoded: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded).expect("proof is base64url"))
                .unwrap();
        assert_eq!(decoded["path"], PATH_DISPATCH);
        assert_eq!(decoded["method"], "POST");
        assert_eq!(decoded["schema_version"], PROOF_SCHEMA);
        assert_eq!(decoded["execution_id"], "e");
        assert_eq!(decoded["expires_at"], 100 + PROOF_TTL_SECONDS);
        assert_eq!(
            decoded["body_sha256"],
            canonical_json_hash(&body)
                .unwrap()
                .strip_prefix("sha256:")
                .unwrap()
        );
        let mut unsigned = decoded.as_object().unwrap().clone();
        let signature =
            hex::decode(unsigned.remove("signature").unwrap().as_str().unwrap()).unwrap();
        let mut mac = HmacSha256::new_from_slice("y".repeat(32).as_bytes()).unwrap();
        mac.update(&serde_json::to_vec(&Value::Object(unsigned)).unwrap());
        mac.verify_slice(&signature).unwrap();
        assert!(
            transport
                .proof(
                    "POST",
                    "/internal/v2/agent-capabilities/other",
                    &context("local_node_action"),
                    &body,
                    100
                )
                .is_err()
        );
    }

    #[test]
    fn catalog_is_exactly_three_platform_capabilities() {
        let catalog = LocalNodeBroker::catalog();
        assert_eq!(catalog.len(), 3);
        assert!(catalog.iter().any(|item| {
            item.id == "local_node_action" && item.effect == CapabilityEffect::Write
        }));
    }
}
