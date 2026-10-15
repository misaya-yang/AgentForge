//! Local Node capability broker.
//!
//! This is the Worker-side seam for a paired Local Node.  It intentionally
//! does not execute host operations and never contains a device credential.
//! The injected transport is responsible for the authenticated outbound
//! channel; this module only binds the request to the Runtime scope and
//! reduces device receipts into the capability execution contract.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

#[cfg(test)]
use tokio::sync::Mutex;

use ai_platform_capability_contract::{
    CapabilityDescriptorV2, CapabilityEffect, CapabilityExecutionStatus, ExecutionMode,
    canonical_json_hash,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const CATALOG_SCHEMA: &str = "local-node-capability/v2";
const MAX_RECEIPT_EVENTS: usize = 256;
const MAX_SEQUENCE_GAP: u64 = 10_000;

/// Scope and device binding copied from the verified Runtime lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalNodeScope {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub device_id: String,
}

/// A request to execute one device operation.  `approval_id` is an opaque,
/// already-authorized one-time approval; the broker never accepts a boolean
/// "approved" flag from the model or device.
#[derive(Clone, Debug)]
pub struct LocalNodeActionRequest {
    pub scope: LocalNodeScope,
    pub execution_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub attempt_id: String,
    pub capability_revision: u64,
    pub effect: CapabilityEffect,
    pub operation: String,
    pub arguments: Value,
    pub arguments_hash: String,
    pub idempotency_key: String,
    pub approval_id: Option<String>,
    pub timeout: Duration,
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
    #[error("local_node_approval_replayed")]
    ApprovalReplayed,
    #[error("local_node_idempotency_conflict")]
    IdempotencyConflict,
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
    /// Return a signed, current capability snapshot.  The transport must
    /// enforce tenant/user/device ownership before returning it.
    async fn describe(
        &self,
        scope: &LocalNodeScope,
    ) -> Result<LocalNodeDescriptor, LocalNodeBrokerError>;

    /// Dispatch once.  A successful return means only that the device accepted
    /// the command, never that the host operation completed.
    async fn dispatch(&self, request: &LocalNodeActionRequest) -> Result<(), LocalNodeBrokerError>;

    /// Read durable device receipts after `after_sequence`.
    async fn receipts(
        &self,
        request: &LocalNodeActionRequest,
        after_sequence: u64,
    ) -> Result<Vec<LocalNodeReceiptEvent>, LocalNodeBrokerError>;
}

/// Production default until a paired Local Node transport is injected. This
/// keeps descriptors and dispatch paths deterministic without inventing a
/// host-operation fallback or accepting unbound device commands.
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

    async fn receipts(
        &self,
        _: &LocalNodeActionRequest,
        _: u64,
    ) -> Result<Vec<LocalNodeReceiptEvent>, LocalNodeBrokerError> {
        Err(LocalNodeBrokerError::TransportUnavailable)
    }
}

/// Durable Worker/Gateway state seam. Implementations must use PostgreSQL;
/// this trait exists so the Worker does not invent a second execution ledger.
#[async_trait]
pub trait LocalNodeDurableState: Send + Sync {
    async fn claim_dispatch(
        &self,
        request: &LocalNodeActionRequest,
    ) -> Result<bool, LocalNodeBrokerError>;
    async fn consume_approval(
        &self,
        request: &LocalNodeActionRequest,
    ) -> Result<(), LocalNodeBrokerError>;
    async fn receipt_cursor(
        &self,
        request: &LocalNodeActionRequest,
    ) -> Result<u64, LocalNodeBrokerError>;
    async fn append_receipt(
        &self,
        request: &LocalNodeActionRequest,
        event: &LocalNodeReceiptEvent,
    ) -> Result<(), LocalNodeBrokerError>;
    async fn result(
        &self,
        request: &LocalNodeActionRequest,
    ) -> Result<Option<LocalNodeActionResult>, LocalNodeBrokerError>;
    async fn mark_side_effect_unknown(
        &self,
        request: &LocalNodeActionRequest,
    ) -> Result<(), LocalNodeBrokerError>;
}

#[cfg(not(test))]
struct UnavailableDurableState;

#[cfg(not(test))]
#[async_trait]
impl LocalNodeDurableState for UnavailableDurableState {
    async fn claim_dispatch(
        &self,
        _: &LocalNodeActionRequest,
    ) -> Result<bool, LocalNodeBrokerError> {
        Err(LocalNodeBrokerError::TransportUnavailable)
    }
    async fn consume_approval(
        &self,
        _: &LocalNodeActionRequest,
    ) -> Result<(), LocalNodeBrokerError> {
        Err(LocalNodeBrokerError::TransportUnavailable)
    }
    async fn receipt_cursor(
        &self,
        _: &LocalNodeActionRequest,
    ) -> Result<u64, LocalNodeBrokerError> {
        Err(LocalNodeBrokerError::TransportUnavailable)
    }
    async fn append_receipt(
        &self,
        _: &LocalNodeActionRequest,
        _: &LocalNodeReceiptEvent,
    ) -> Result<(), LocalNodeBrokerError> {
        Err(LocalNodeBrokerError::TransportUnavailable)
    }
    async fn result(
        &self,
        _: &LocalNodeActionRequest,
    ) -> Result<Option<LocalNodeActionResult>, LocalNodeBrokerError> {
        Err(LocalNodeBrokerError::TransportUnavailable)
    }
    async fn mark_side_effect_unknown(
        &self,
        _: &LocalNodeActionRequest,
    ) -> Result<(), LocalNodeBrokerError> {
        Err(LocalNodeBrokerError::TransportUnavailable)
    }
}

#[cfg(test)]
struct TestDurableState {
    results: Mutex<BTreeMap<String, LocalNodeActionResult>>,
    cursors: Mutex<BTreeMap<String, u64>>,
}

#[cfg(test)]
impl TestDurableState {
    fn new() -> Self {
        Self {
            results: Mutex::new(BTreeMap::new()),
            cursors: Mutex::new(BTreeMap::new()),
        }
    }
}

#[cfg(test)]
#[async_trait]
impl LocalNodeDurableState for TestDurableState {
    async fn claim_dispatch(
        &self,
        request: &LocalNodeActionRequest,
    ) -> Result<bool, LocalNodeBrokerError> {
        Ok(!self
            .results
            .lock()
            .await
            .contains_key(&request.execution_id))
    }
    async fn consume_approval(
        &self,
        _: &LocalNodeActionRequest,
    ) -> Result<(), LocalNodeBrokerError> {
        Ok(())
    }
    async fn receipt_cursor(
        &self,
        request: &LocalNodeActionRequest,
    ) -> Result<u64, LocalNodeBrokerError> {
        Ok(*self
            .cursors
            .lock()
            .await
            .get(&request.execution_id)
            .unwrap_or(&0))
    }
    async fn append_receipt(
        &self,
        request: &LocalNodeActionRequest,
        event: &LocalNodeReceiptEvent,
    ) -> Result<(), LocalNodeBrokerError> {
        self.cursors
            .lock()
            .await
            .insert(request.execution_id.clone(), event.sequence);
        if event.terminal() {
            self.results.lock().await.insert(
                request.execution_id.clone(),
                LocalNodeActionResult {
                    status: event.status,
                    result: event.payload.get("result").cloned(),
                    last_sequence: event.sequence,
                },
            );
        }
        Ok(())
    }
    async fn result(
        &self,
        request: &LocalNodeActionRequest,
    ) -> Result<Option<LocalNodeActionResult>, LocalNodeBrokerError> {
        Ok(self
            .results
            .lock()
            .await
            .get(&request.execution_id)
            .cloned())
    }
    async fn mark_side_effect_unknown(
        &self,
        request: &LocalNodeActionRequest,
    ) -> Result<(), LocalNodeBrokerError> {
        self.results.lock().await.insert(
            request.execution_id.clone(),
            LocalNodeActionResult {
                status: CapabilityExecutionStatus::SideEffectUnknown,
                result: None,
                last_sequence: 0,
            },
        );
        Ok(())
    }
}

/// Broker state contains only transport plus an injected durable state seam;
/// it never becomes an execution or approval authority of its own.
#[derive(Clone)]
pub struct LocalNodeBroker {
    transport: Arc<dyn LocalNodeTransport>,
    durable: Arc<dyn LocalNodeDurableState>,
}

impl LocalNodeBroker {
    /// Fail-closed constructor retained for source compatibility. Production
    /// composition must call `with_durable_state` with a PostgreSQL adapter.
    pub fn new(transport: Arc<dyn LocalNodeTransport>) -> Self {
        #[cfg(test)]
        return Self::with_durable_state(transport, Arc::new(TestDurableState::new()));
        #[cfg(not(test))]
        Self::with_durable_state(transport, Arc::new(UnavailableDurableState))
    }

    pub fn with_durable_state(
        transport: Arc<dyn LocalNodeTransport>,
        durable: Arc<dyn LocalNodeDurableState>,
    ) -> Self {
        Self { transport, durable }
    }

    /// The only catalog entries exposed by this broker.  Device-specific
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
        self.transport.describe(scope).await
    }

    pub async fn execute(
        &self,
        request: LocalNodeActionRequest,
        after_sequence: u64,
    ) -> Result<LocalNodeActionResult, LocalNodeBrokerError> {
        validate_request(&request, after_sequence)?;
        if request.approval_id.is_none()
            && matches!(
                request.effect,
                CapabilityEffect::Write | CapabilityEffect::Unknown
            )
        {
            return Err(LocalNodeBrokerError::ApprovalRequired);
        }
        let claimed = self.durable.claim_dispatch(&request).await?;
        if !claimed {
            return self
                .durable
                .result(&request)
                .await?
                .ok_or(LocalNodeBrokerError::IdempotencyConflict);
        }
        if !matches!(request.effect, CapabilityEffect::Read) {
            self.durable.consume_approval(&request).await?;
        }

        // The dispatch result is not a successful action result.  If this
        // call fails after the device accepted the command, its outcome is
        // unknowable and must never be retried by this broker.
        if self.transport.dispatch(&request).await.is_err() {
            let _ = self.durable.mark_side_effect_unknown(&request).await;
            return Err(LocalNodeBrokerError::SideEffectUnknown);
        }
        let mut cursor = self
            .durable
            .receipt_cursor(&request)
            .await?
            .max(after_sequence);
        let deadline = tokio::time::Instant::now() + request.timeout;
        loop {
            let events = match self.transport.receipts(&request, cursor).await {
                Ok(events) => events,
                Err(_) => {
                    let _ = self.durable.mark_side_effect_unknown(&request).await;
                    return Err(LocalNodeBrokerError::SideEffectUnknown);
                }
            };
            if events.len() > MAX_RECEIPT_EVENTS {
                return Err(LocalNodeBrokerError::ReceiptInvalid);
            }
            for event in events {
                validate_receipt(&request, &event, cursor)?;
                self.durable.append_receipt(&request, &event).await?;
                cursor = event.sequence;
                if event.terminal() {
                    let result = LocalNodeActionResult {
                        status: event.status,
                        result: event.payload.get("result").cloned(),
                        last_sequence: cursor,
                    };
                    return Ok(result);
                }
            }
            if tokio::time::Instant::now() >= deadline {
                let _ = self.durable.mark_side_effect_unknown(&request).await;
                return Err(LocalNodeBrokerError::SideEffectUnknown);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    /// Classify a lost device channel after dispatch.  This helper is used by
    /// recovery code rather than pretending a transport error was a denial.
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
    if [
        &scope.tenant_id,
        &scope.user_id,
        &scope.session_id,
        &scope.device_id,
    ]
    .iter()
    .any(|value| value.is_empty() || value.len() > 255 || value.bytes().any(|b| b < 0x20))
    {
        return Err(LocalNodeBrokerError::ScopeInvalid);
    }
    Ok(())
}

fn validate_request(
    request: &LocalNodeActionRequest,
    after_sequence: u64,
) -> Result<(), LocalNodeBrokerError> {
    validate_scope(&request.scope)?;
    if request.execution_id.is_empty()
        || request.run_id.is_empty()
        || request.tool_call_id.is_empty()
        || request.attempt_id.is_empty()
        || request.operation.is_empty()
        || request.idempotency_key.is_empty()
        || request.capability_revision == 0
        || request.timeout.is_zero()
        || request.timeout > Duration::from_secs(120)
        || after_sequence > u64::MAX - MAX_SEQUENCE_GAP
    {
        return Err(LocalNodeBrokerError::ArgumentsInvalid);
    }
    let expected = canonical_json_hash(&request.arguments)
        .map_err(|_| LocalNodeBrokerError::ArgumentsInvalid)?;
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
    if event.execution_id != request.execution_id
        || event.device_id != request.scope.device_id
        || event.sequence <= cursor
        || event.sequence - cursor > MAX_SEQUENCE_GAP
    {
        return Err(LocalNodeBrokerError::ReceiptSequenceInvalid);
    }
    if event.terminal()
        && event.status == CapabilityExecutionStatus::Succeeded
        && event.payload.get("result").is_none()
    {
        return Err(LocalNodeBrokerError::ReceiptInvalid);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn scope() -> LocalNodeScope {
        LocalNodeScope {
            tenant_id: "t".into(),
            user_id: "u".into(),
            session_id: "s".into(),
            device_id: "d".into(),
        }
    }
    fn request() -> LocalNodeActionRequest {
        let args = json!({"path":"notes.txt"});
        LocalNodeActionRequest {
            scope: scope(),
            execution_id: "e".into(),
            run_id: "r".into(),
            tool_call_id: "c".into(),
            attempt_id: "a".into(),
            capability_revision: 1,
            effect: CapabilityEffect::Write,
            operation: "file.read".into(),
            arguments_hash: canonical_json_hash(&args).unwrap(),
            arguments: args,
            idempotency_key: "idem".into(),
            approval_id: Some("approval".into()),
            timeout: Duration::from_millis(100),
        }
    }
    struct Fake {
        dispatched: AtomicBool,
        disconnected: bool,
    }
    #[async_trait]
    impl LocalNodeTransport for Fake {
        async fn describe(
            &self,
            scope: &LocalNodeScope,
        ) -> Result<LocalNodeDescriptor, LocalNodeBrokerError> {
            Ok(LocalNodeDescriptor {
                device_id: scope.device_id.clone(),
                capability_revision: 1,
                capabilities: ["file.read".into()].into_iter().collect(),
                online: true,
                expires_at_epoch_ms: 1,
            })
        }
        async fn dispatch(
            &self,
            _request: &LocalNodeActionRequest,
        ) -> Result<(), LocalNodeBrokerError> {
            self.dispatched.store(true, Ordering::SeqCst);
            Ok(())
        }
        async fn receipts(
            &self,
            request: &LocalNodeActionRequest,
            _after: u64,
        ) -> Result<Vec<LocalNodeReceiptEvent>, LocalNodeBrokerError> {
            if self.disconnected {
                return Err(LocalNodeBrokerError::TransportUnavailable);
            }
            Ok(vec![LocalNodeReceiptEvent {
                execution_id: request.execution_id.clone(),
                device_id: request.scope.device_id.clone(),
                sequence: 1,
                status: CapabilityExecutionStatus::Succeeded,
                event: "action.succeeded".into(),
                payload: [("result".into(), json!({"content":"ok"}))]
                    .into_iter()
                    .collect(),
            }])
        }
    }
    #[tokio::test]
    async fn successful_receipt_is_the_only_success_signal() {
        let broker = LocalNodeBroker::new(Arc::new(Fake {
            dispatched: AtomicBool::new(false),
            disconnected: false,
        }));
        let result = broker.execute(request(), 0).await.unwrap();
        assert_eq!(result.status, CapabilityExecutionStatus::Succeeded);
        assert_eq!(result.last_sequence, 1);
    }
    #[tokio::test]
    async fn disconnect_after_dispatch_is_not_retryable_success() {
        let broker = LocalNodeBroker::new(Arc::new(Fake {
            dispatched: AtomicBool::new(false),
            disconnected: true,
        }));
        assert_eq!(
            broker.execute(request(), 0).await,
            Err(LocalNodeBrokerError::SideEffectUnknown)
        );
        assert_eq!(
            LocalNodeBroker::classify_disconnect_after_dispatch(
                CapabilityExecutionStatus::Dispatched
            ),
            CapabilityExecutionStatus::SideEffectUnknown
        );
    }
    #[tokio::test]
    async fn approval_and_arguments_are_bound_before_dispatch() {
        let mut req = request();
        req.approval_id = None;
        let broker = LocalNodeBroker::new(Arc::new(Fake {
            dispatched: AtomicBool::new(false),
            disconnected: false,
        }));
        assert_eq!(
            broker.execute(req, 0).await,
            Err(LocalNodeBrokerError::ApprovalRequired)
        );
        let mut req = request();
        req.arguments = json!({"path":"other.txt"});
        let broker = LocalNodeBroker::new(Arc::new(Fake {
            dispatched: AtomicBool::new(false),
            disconnected: false,
        }));
        assert_eq!(
            broker.execute(req, 0).await,
            Err(LocalNodeBrokerError::ArgumentsHashMismatch)
        );
    }
    #[test]
    fn catalog_is_exactly_three_platform_capabilities() {
        let catalog = LocalNodeBroker::catalog();
        assert_eq!(catalog.len(), 3);
        assert!(
            catalog.iter().any(
                |item| item.id == "local_node_action" && item.effect == CapabilityEffect::Write
            )
        );
    }
}
