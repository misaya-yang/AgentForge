//! Durable capability execution worker. It deliberately contains no Agent
//! loop, provider client, or long-lived connector credential.
pub mod python_package_policy;
pub mod sandbox_service;
pub mod sandbox_transport;
pub mod attachment_capabilities;
pub mod confluence_write_broker;
pub mod external_write_capabilities;
pub mod http_service;
pub mod image_write_broker;
pub mod local_node_broker;
pub mod office_artifact_broker;
pub mod office_capabilities;
pub mod platform_catalog;
pub mod plugin_packs;
pub mod postgres_store;
pub mod python_artifact_broker;
pub mod python_code_execution;
pub mod quiz_capabilities;
pub mod read_capabilities;
mod trace_context;
#[path="../../ai-platform-agent-runtime/src/telemetry.rs"]
pub mod telemetry;
pub mod write_capabilities;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use ai_platform_capability_contract::{
    ApprovalPolicy, CAPABILITY_DESCRIPTOR_SCHEMA_VERSION, CAPABILITY_EVENT_SCHEMA_VERSION,
    CapabilityDescriptorV2, CapabilityEffect, CapabilityEventPageV2, CapabilityEventV2,
    CapabilityExecutionStatus, CapabilityExecutionV2, CapabilityScopeV2, ExecutionMode,
    canonical_json_hash,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::Mutex;
use uuid::Uuid;

pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 128 * 1024;
pub const EVENT_PAGE_SIZE: usize = 1;

#[derive(Clone, Debug)]
pub struct NewExecution {
    pub execution: CapabilityExecutionV2,
    pub arguments: Value,
    /// Server-derived binding from the active Runtime snapshot.  This must
    /// never be reconstructed from tool arguments during recovery.
    pub resource_binding: Value,
    pub approval_policy: ApprovalPolicy,
    pub approval_id: Option<String>,
    pub approval_status: String,
}

#[derive(Clone, Debug)]
pub struct ExecutionRecord {
    pub execution: CapabilityExecutionV2,
    pub arguments: Value,
    pub resource_binding: Value,
    /// A durable, non-secret result written atomically with an external side
    /// effect. It is normally the terminal result; before terminal admission
    /// it can also prove that restart recovery should succeed, not guess.
    pub result_summary: Option<Value>,
    pub approval_policy: ApprovalPolicy,
    pub approval_id: Option<String>,
    pub approval_status: String,
    pub dispatch_fence: Option<String>,
    pub worker_lease_until_epoch_ms: Option<u64>,
    pub last_sequence: u64,
}

#[derive(Clone, Debug)]
pub struct ReserveOutcome {
    pub record: ExecutionRecord,
    pub created: bool,
}

#[derive(Clone, Debug)]
pub struct DispatchOutcome {
    pub record: ExecutionRecord,
    pub claimed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityIdentity {
    pub capability_type: String,
    pub name: String,
    pub capability_id: String,
    pub version: String,
    pub schema_hash: String,
    pub effect: CapabilityEffect,
    pub approval_policy: ApprovalPolicy,
    pub connector_binding: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeCapabilityBinding {
    pub capability_type: String,
    pub name: String,
    pub snapshot_id: String,
    pub capability_revision: u64,
    pub capability_id: String,
    pub capability_version: String,
    pub schema_hash: String,
    pub effect: CapabilityEffect,
    pub approval_policy: ApprovalPolicy,
    pub bound_dataset_ids: std::collections::BTreeSet<String>,
    pub connector_binding: Option<RuntimeConnectorBinding>,
    pub memory_policy: Option<write_capabilities::MemoryPolicyBinding>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConnectorBinding {
    pub binding_type: String,
    pub provider: String,
    pub tool_name: String,
    pub principal_type: Option<String>,
    pub grant_id: Option<String>,
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub schema_hash: Option<String>,
    #[serde(default)]
    pub risk_level: Option<String>,
    pub channel: String,
}

impl RuntimeConnectorBinding {
    pub fn validate(&self, expected_tool: &str) -> Result<(), StoreError> {
        let valid_channel = matches!(
            self.channel.as_str(),
            "preview" | "hosted" | "hosted_private" | "hosted_public" | "embed" | "api" | "builtin"
        );
        if self.provider.is_empty()
            || self.provider.len() > 128
            || self.provider.bytes().any(|byte| byte.is_ascii_control())
            || self.tool_name != expected_tool
            || !valid_channel
        {
            return Err(StoreError::RuntimeBindingUnauthorized);
        }
        match self.binding_type.as_str() {
            "grant" if self.provider == "mcp" => {
                if self
                    .principal_type
                    .as_deref()
                    .is_none_or(|value| !matches!(value, "service_account" | "user_delegated"))
                    || self.grant_id.is_some()
                    || self
                        .connection_id
                        .as_deref()
                        .is_none_or(|value| uuid::Uuid::parse_str(value).is_err())
                    || self.schema_hash.as_deref().is_none_or(|value| {
                        value.len() != 71
                            || !value.starts_with("sha256:")
                            || value[7..]
                                .bytes()
                                .any(|byte| !matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                    })
                    || self.risk_level.as_deref().is_none_or(|value| {
                        !matches!(value, "low" | "medium" | "high" | "critical")
                    })
                {
                    Err(StoreError::RuntimeBindingUnauthorized)
                } else {
                    Ok(())
                }
            }
            "catalog" if self.principal_type.is_none() && self.grant_id.is_none() => Ok(()),
            "grant"
                if matches!(
                    self.principal_type.as_deref(),
                    Some("service_account") | Some("user_delegated")
                ) && self
                    .grant_id
                    .as_deref()
                    .is_some_and(|value| uuid::Uuid::parse_str(value).is_ok()) =>
            {
                Ok(())
            }
            _ => Err(StoreError::RuntimeBindingUnauthorized),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum StoreError {
    #[error("execution_not_found")]
    NotFound,
    #[error("execution_scope_mismatch")]
    ScopeMismatch,
    #[error("execution_idempotency_conflict")]
    IdempotencyConflict,
    #[error("execution_dispatch_fence_mismatch")]
    DispatchFenceMismatch,
    #[error("execution_approval_required")]
    ApprovalRequired,
    #[error("execution_terminal_immutable")]
    TerminalImmutable,
    #[error("execution_event_invalid")]
    InvalidEvent,
    #[error("runtime_binding_unauthorized")]
    RuntimeBindingUnauthorized,
    #[error("execution_capacity_exhausted")]
    CapacityExhausted,
    #[error("execution_store_unavailable")]
    Internal,
}

#[async_trait]
pub trait ExecutionStore: Send + Sync {
    async fn ready(&self) -> Result<(), StoreError>;
    async fn authorize_runtime_binding(
        &self,
        scope: &CapabilityScopeV2,
        run_id: &str,
        capability_revision: u64,
        identity: &CapabilityIdentity,
    ) -> Result<RuntimeCapabilityBinding, StoreError>;
    async fn reserve(&self, execution: NewExecution) -> Result<ReserveOutcome, StoreError>;
    /// Production stores validate and mutate under the same owner lock.
    async fn reserve_owned(
        &self,
        execution: NewExecution,
        _owner: Option<&str>,
        _fence: Option<i64>,
    ) -> Result<ReserveOutcome, StoreError> {
        self.reserve(execution).await
    }
    /// Run admission only for a genuinely new reservation, while holding the
    /// store's identity/owner locks. Capacity failure must leave no visible row.
    /// Callers retain any acquired RAII ticket and drop it if commit fails.
    async fn reserve_admitted_owned(
        &self,
        execution: NewExecution,
        owner: Option<&str>,
        fence: Option<i64>,
        admit: &mut (dyn FnMut() -> Result<(), StoreError> + Send),
    ) -> Result<ReserveOutcome, StoreError>;
    async fn get(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
    ) -> Result<ExecutionRecord, StoreError>;
    async fn dispatch(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        dispatch_fence: &str,
    ) -> Result<DispatchOutcome, StoreError>;
    async fn recoverable(&self) -> Result<Vec<ExecutionRecord>, StoreError>;
    async fn renew(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        dispatch_fence: &str,
        lease_ms: u64,
    ) -> Result<bool, StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn append_event(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        event_id: &str,
        event: &str,
        status: CapabilityExecutionStatus,
        payload: BTreeMap<String, Value>,
        dispatch_fence: Option<&str>,
    ) -> Result<CapabilityEventV2, StoreError>;
    async fn events(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        after_sequence: u64,
    ) -> Result<CapabilityEventPageV2, StoreError>;
    async fn cancel(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
    ) -> Result<ExecutionRecord, StoreError>;
    async fn cancel_owned(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        _owner: Option<&str>,
        _fence: Option<i64>,
    ) -> Result<ExecutionRecord, StoreError> {
        self.cancel(scope, execution_id).await
    }
}

pub type DynStore = Arc<dyn ExecutionStore>;

pub fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub fn fixture_catalog() -> Vec<CapabilityDescriptorV2> {
    let definitions = [
        (
            "platform.echo",
            "Echo",
            "Return the validated object unchanged for Runtime/Worker contract tests.",
            serde_json::json!({
                "type": "object",
                "additionalProperties": true
            }),
            serde_json::json!({
                "type": "object",
                "additionalProperties": true
            }),
        ),
        (
            "platform.read_fixture",
            "Read fixture",
            "Read one deterministic fixture value without external side effects.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "key": {"type": "string"},
                    "delay_ms": {"type": "integer", "minimum": 0, "maximum": 5000}
                },
                "additionalProperties": false
            }),
            serde_json::json!({
                "type": "object",
                "required": ["key", "value"],
                "properties": {
                    "key": {"type": "string"},
                    "value": {"type": "string"}
                },
                "additionalProperties": false
            }),
        ),
    ];
    definitions
        .into_iter()
        .map(
            |(id, _display_name, description, input_schema, output_schema)| {
                let schema_hash =
                    canonical_json_hash(&input_schema).expect("fixture input schema is an object");
                let descriptor = CapabilityDescriptorV2 {
                    schema_version: CAPABILITY_DESCRIPTOR_SCHEMA_VERSION.to_string(),
                    id: id.to_string(),
                    name: id.to_string(),
                    version: "1".to_string(),
                    description: description.to_string(),
                    schema_hash,
                    input_schema,
                    output_schema,
                    effect: CapabilityEffect::Read,
                    approval_policy: ApprovalPolicy::Never,
                    execution_mode: ExecutionMode::Inline,
                    timeout_ms: 5_000,
                    tags: vec!["fixture".to_string(), "read_only".to_string()],
                    protocol: "internal-v2".to_string(),
                    connector_binding: None,
                };
                descriptor.validate().expect("fixture descriptor is valid");
                descriptor
            },
        )
        .collect()
}

pub fn fixture_result(capability_id: &str, arguments: &Value) -> Option<Value> {
    match capability_id {
        "platform.echo" => Some(arguments.clone()),
        "platform.read_fixture" => Some(serde_json::json!({
            "key": arguments
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or("default"),
            "value": "fixture"
        })),
        _ => None,
    }
}

#[derive(Default)]
struct MemoryState {
    executions: BTreeMap<String, ExecutionRecord>,
    events: BTreeMap<String, Vec<(String, CapabilityEventV2)>>,
    runtime_bindings: BTreeMap<RuntimeBindingKey, RuntimeCapabilityBinding>,
    approved_authorities: BTreeMap<String, Value>,
}

type RuntimeBindingKey = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
);

fn runtime_binding_key(
    scope: &CapabilityScopeV2,
    run_id: &str,
    name: &str,
    capability_id: &str,
    version: &str,
    schema_hash: &str,
) -> RuntimeBindingKey {
    (
        scope.tenant_id.clone(),
        scope.user_id.clone(),
        scope.session_id.clone(),
        run_id.to_string(),
        name.to_string(),
        capability_id.to_string(),
        version.to_string(),
        schema_hash.to_string(),
    )
}

#[derive(Default)]
pub struct MemoryStore {
    state: Mutex<MemoryState>,
}

impl MemoryStore {
    /// Test/fixture-only snapshot authority. Production uses the PostgreSQL
    /// implementation; keeping this explicit prevents the memory store from
    /// silently treating request arguments as an authorization source.
    pub async fn register_runtime_binding(
        &self,
        scope: &CapabilityScopeV2,
        run_id: &str,
        binding: RuntimeCapabilityBinding,
    ) {
        let key = runtime_binding_key(
            scope,
            run_id,
            &binding.name,
            &binding.capability_id,
            &binding.capability_version,
            &binding.schema_hash,
        );
        self.state
            .lock()
            .await
            .runtime_bindings
            .insert(key, binding);
    }

    /// Explicit test authority for a one-time write approval. Production must
    /// use PostgreSQL's atomic approval-consumption function instead.
    pub async fn register_approved_authority(&self, approval_id: &str, arguments: Value) {
        self.state
            .lock()
            .await
            .approved_authorities
            .insert(approval_id.to_string(), arguments);
    }
}

fn same_scope(record: &ExecutionRecord, scope: &CapabilityScopeV2) -> bool {
    record.execution.tenant_id == scope.tenant_id
        && record.execution.user_id == scope.user_id
        && record.execution.session_id == scope.session_id
}

fn same_reservation(record: &ExecutionRecord, requested: &NewExecution) -> bool {
    let left = &record.execution;
    let right = &requested.execution;
    left.lease_id == right.lease_id
        && record.approval_policy == requested.approval_policy
        && record.approval_id == requested.approval_id
        && left.tenant_id == right.tenant_id
        && left.user_id == right.user_id
        && left.session_id == right.session_id
        && left.run_id == right.run_id
        && left.tool_call_id == right.tool_call_id
        && left.attempt_id == right.attempt_id
        && left.capability_id == right.capability_id
        && left.capability_revision == right.capability_revision
        && left.arguments_hash == right.arguments_hash
        && left.idempotency_key == right.idempotency_key
        && left.effect == right.effect
        && crate::read_capabilities::transport::bindings_match(&record.resource_binding, &requested.resource_binding)
        && record.arguments == requested.arguments
}

fn record_for_scope<'a>(
    state: &'a mut MemoryState,
    scope: &CapabilityScopeV2,
    execution_id: &str,
) -> Result<&'a mut ExecutionRecord, StoreError> {
    let record = state
        .executions
        .get_mut(execution_id)
        .ok_or(StoreError::NotFound)?;
    if !same_scope(record, scope) {
        return Err(StoreError::ScopeMismatch);
    }
    Ok(record)
}

fn terminal_payload(
    execution: &mut CapabilityExecutionV2,
    status: CapabilityExecutionStatus,
    payload: &BTreeMap<String, Value>,
) {
    if !status.is_terminal() {
        return;
    }
    execution.result = payload.get("result").cloned();
    execution.error = payload
        .get("error_code")
        .and_then(Value::as_str)
        .map(str::to_string);
}

/// A published execution has not crossed the side-effect boundary and may be
/// claimed again. Once a write/unknown execution has a dispatch fence, its
/// outcome is no longer safe to replay; recovery must terminalize it instead.
pub(crate) fn is_recoverable_execution(record: &ExecutionRecord) -> bool {
    if !matches!(
        record.execution.status,
        CapabilityExecutionStatus::Published
            | CapabilityExecutionStatus::Dispatched
            | CapabilityExecutionStatus::Running
    ) {
        return false;
    }
    match record.execution.effect {
        CapabilityEffect::Read => true,
        CapabilityEffect::Write | CapabilityEffect::Unknown => {
            if record.execution.status == CapabilityExecutionStatus::Published {
                record.dispatch_fence.is_none()
            } else {
                record.dispatch_fence.is_some()
                    && record
                        .worker_lease_until_epoch_ms
                        .is_none_or(|lease| lease <= now_epoch_ms())
            }
        }
    }
}

pub(crate) fn needs_side_effect_unknown_recovery(record: &ExecutionRecord) -> bool {
    matches!(
        record.execution.effect,
        CapabilityEffect::Write | CapabilityEffect::Unknown
    ) && matches!(
        record.execution.status,
        CapabilityExecutionStatus::Dispatched | CapabilityExecutionStatus::Running
    ) && record.dispatch_fence.is_some()
        && record
            .worker_lease_until_epoch_ms
            .is_none_or(|lease| lease <= now_epoch_ms())
}

/// Return a result only when the Worker atomically persisted an explicit
/// capability receipt with the side effect. A mere non-null summary is never
/// sufficient evidence because partial progress must remain unknown.
pub(crate) fn durable_recovery_result(record: &ExecutionRecord) -> Option<Value> {
    if !matches!(
        record.execution.effect,
        CapabilityEffect::Write | CapabilityEffect::Unknown
    ) {
        return None;
    }
    let receipt = record.result_summary.as_ref()?.as_object()?;
    if receipt.get("schema_version").and_then(Value::as_str)
        != Some("ai-platform/durable-capability-receipt/v1")
        || receipt.get("capability_id").and_then(Value::as_str)
            != Some(record.execution.capability_id.as_str())
    {
        return None;
    }
    receipt
        .get("result")
        .filter(|value| value.is_object())
        .cloned()
}

#[async_trait]
impl ExecutionStore for MemoryStore {
    async fn ready(&self) -> Result<(), StoreError> {
        Ok(())
    }

    async fn authorize_runtime_binding(
        &self,
        scope: &CapabilityScopeV2,
        run_id: &str,
        capability_revision: u64,
        identity: &CapabilityIdentity,
    ) -> Result<RuntimeCapabilityBinding, StoreError> {
        let state = self.state.lock().await;
        let key = runtime_binding_key(
            scope,
            run_id,
            &identity.name,
            &identity.capability_id,
            &identity.version,
            &identity.schema_hash,
        );
        let binding = state
            .runtime_bindings
            .get(&key)
            .filter(|binding| binding.capability_revision == capability_revision)
            .cloned()
            .ok_or(StoreError::RuntimeBindingUnauthorized)?;
        if binding.capability_type != identity.capability_type
            && !(binding.capability_type == "connector" && binding.connector_binding.is_some())
        {
            return Err(StoreError::RuntimeBindingUnauthorized);
        }
        if binding.effect != identity.effect || binding.approval_policy != identity.approval_policy
        {
            return Err(StoreError::RuntimeBindingUnauthorized);
        }
        let binding_value = binding
            .connector_binding
            .as_ref()
            .map(|value| serde_json::to_value(value).unwrap_or(Value::Null));
        if binding_value != identity.connector_binding {
            return Err(StoreError::RuntimeBindingUnauthorized);
        }
        if matches!(binding.capability_type.as_str(), "connector" | "mcp") {
            let connector = binding
                .connector_binding
                .as_ref()
                .ok_or(StoreError::RuntimeBindingUnauthorized)?;
            connector.validate(&identity.name)?;
            if binding.capability_type == "mcp" && connector.provider != "mcp" {
                return Err(StoreError::RuntimeBindingUnauthorized);
            }
        } else if binding.connector_binding.is_some() {
            return Err(StoreError::RuntimeBindingUnauthorized);
        }
        if identity.capability_id == "update_user_memory" && binding.memory_policy.is_none() {
            return Err(StoreError::RuntimeBindingUnauthorized);
        }
        if let Some(policy) = &binding.memory_policy {
            policy
                .validate()
                .map_err(|_| StoreError::RuntimeBindingUnauthorized)?;
        }
        Ok(binding)
    }

    async fn reserve(&self, requested: NewExecution) -> Result<ReserveOutcome, StoreError> {
        self.reserve_admitted_owned(requested, None, None, &mut || Ok(())).await
    }

    async fn reserve_admitted_owned(
        &self,
        requested: NewExecution,
        _owner: Option<&str>,
        _fence: Option<i64>,
        admit: &mut (dyn FnMut() -> Result<(), StoreError> + Send),
    ) -> Result<ReserveOutcome, StoreError> {
        let mut state = self.state.lock().await;
        let existing = state.executions.values().find(|record| {
            (record.execution.run_id == requested.execution.run_id
                && record.execution.tool_call_id == requested.execution.tool_call_id
                && record.execution.attempt_id == requested.execution.attempt_id)
                || (record.execution.tenant_id == requested.execution.tenant_id
                    && record.execution.user_id == requested.execution.user_id
                    && record.execution.session_id == requested.execution.session_id
                    && record.execution.idempotency_key == requested.execution.idempotency_key)
        });
        if let Some(existing) = existing {
            if !same_reservation(existing, &requested) {
                return Err(StoreError::IdempotencyConflict);
            }
            return Ok(ReserveOutcome {
                record: existing.clone(),
                created: false,
            });
        }
        admit()?;
        let execution_id = requested.execution.execution_id.clone();
        let record = ExecutionRecord {
            execution: requested.execution,
            arguments: requested.arguments,
            resource_binding: requested.resource_binding,
            result_summary: None,
            approval_policy: requested.approval_policy,
            approval_id: requested.approval_id,
            approval_status: requested.approval_status,
            dispatch_fence: None,
            worker_lease_until_epoch_ms: None,
            last_sequence: 0,
        };
        state.executions.insert(execution_id, record.clone());
        Ok(ReserveOutcome {
            record,
            created: true,
        })
    }

    async fn get(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
    ) -> Result<ExecutionRecord, StoreError> {
        let mut state = self.state.lock().await;
        Ok(record_for_scope(&mut state, scope, execution_id)?.clone())
    }

    async fn dispatch(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        dispatch_fence: &str,
    ) -> Result<DispatchOutcome, StoreError> {
        let mut state = self.state.lock().await;
        let snapshot = record_for_scope(&mut state, scope, execution_id)?.clone();
        if snapshot.execution.status.is_terminal() {
            return Err(StoreError::TerminalImmutable);
        }
        if let Some(existing) = &snapshot.dispatch_fence {
            if !matches!(snapshot.execution.effect, CapabilityEffect::Read) {
                if existing != dispatch_fence {
                    return Err(StoreError::DispatchFenceMismatch);
                }
                return Ok(DispatchOutcome {
                    record: snapshot,
                    claimed: false,
                });
            }
            if snapshot
                .worker_lease_until_epoch_ms
                .is_some_and(|lease| lease > now_epoch_ms())
            {
                return Ok(DispatchOutcome {
                    record: snapshot,
                    claimed: false,
                });
            }
            let record = record_for_scope(&mut state, scope, execution_id)?;
            if existing == dispatch_fence {
                record.worker_lease_until_epoch_ms = Some(now_epoch_ms() + 30_000);
                return Ok(DispatchOutcome {
                    record: record.clone(),
                    claimed: true,
                });
            }
            if record.execution.status.is_terminal() {
                return Err(StoreError::DispatchFenceMismatch);
            }
            // A recovered read gets a new fence. This makes a stale worker's
            // later progress/terminal receipt fail instead of duplicating it.
            record.dispatch_fence = Some(dispatch_fence.to_string());
            record.worker_lease_until_epoch_ms = Some(now_epoch_ms() + 30_000);
            return Ok(DispatchOutcome {
                record: record.clone(),
                claimed: true,
            });
        }
        if !matches!(snapshot.execution.effect, CapabilityEffect::Read) {
            let Some(approval_id) = snapshot.approval_id.as_deref() else {
                return Err(StoreError::ApprovalRequired);
            };
            if snapshot.approval_status != "approved"
                || state.approved_authorities.get(approval_id) != Some(&snapshot.arguments)
            {
                return Err(StoreError::ApprovalRequired);
            }
            state.approved_authorities.remove(approval_id);
        }
        let record = record_for_scope(&mut state, scope, execution_id)?;
        if !matches!(record.execution.effect, CapabilityEffect::Read) {
            record.approval_status = "consumed".to_string();
        }
        record.dispatch_fence = Some(dispatch_fence.to_string());
        record.worker_lease_until_epoch_ms = Some(now_epoch_ms() + 30_000);
        record.execution.status = CapabilityExecutionStatus::Dispatched;
        Ok(DispatchOutcome {
            record: record.clone(),
            claimed: true,
        })
    }

    async fn recoverable(&self) -> Result<Vec<ExecutionRecord>, StoreError> {
        let state = self.state.lock().await;
        Ok(state
            .executions
            .values()
            .filter(|record| is_recoverable_execution(record))
            .cloned()
            .collect())
    }

    async fn renew(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        dispatch_fence: &str,
        lease_ms: u64,
    ) -> Result<bool, StoreError> {
        let mut state = self.state.lock().await;
        let record = record_for_scope(&mut state, scope, execution_id)?;
        if record.execution.status.is_terminal()
            || record.dispatch_fence.as_deref() != Some(dispatch_fence)
            || !record
                .worker_lease_until_epoch_ms
                .is_some_and(|lease| lease > now_epoch_ms())
        {
            return Ok(false);
        }
        record.worker_lease_until_epoch_ms = Some(now_epoch_ms() + lease_ms.clamp(1_000, 120_000));
        Ok(true)
    }

    async fn append_event(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        event_id: &str,
        event: &str,
        status: CapabilityExecutionStatus,
        payload: BTreeMap<String, Value>,
        dispatch_fence: Option<&str>,
    ) -> Result<CapabilityEventV2, StoreError> {
        match serde_json::to_vec(&payload) {
            Ok(encoded) if encoded.len() <= MAX_EVENT_PAYLOAD_BYTES => {}
            _ => return Err(StoreError::InvalidEvent),
        }
        let mut state = self.state.lock().await;
        if let Some(existing) = state
            .events
            .get(execution_id)
            .and_then(|events| events.iter().find(|(id, _)| id == event_id))
        {
            return Ok(existing.1.clone());
        }
        let record = record_for_scope(&mut state, scope, execution_id)?;
        if record.execution.status.is_terminal() {
            return Err(StoreError::TerminalImmutable);
        }
        let valid_transition = match status {
            CapabilityExecutionStatus::Published => record.last_sequence == 0,
            CapabilityExecutionStatus::Dispatched => matches!(
                record.execution.status,
                CapabilityExecutionStatus::Published
                    | CapabilityExecutionStatus::Dispatched
                    | CapabilityExecutionStatus::Running
            ),
            CapabilityExecutionStatus::Running => matches!(
                record.execution.status,
                CapabilityExecutionStatus::Dispatched | CapabilityExecutionStatus::Running
            ),
            _ => true,
        };
        if !valid_transition {
            return Err(StoreError::InvalidEvent);
        }
        if let Some(fence) = dispatch_fence
            && record.dispatch_fence.as_deref() != Some(fence)
        {
            return Err(StoreError::DispatchFenceMismatch);
        }
        if record.execution.status == CapabilityExecutionStatus::Running {
            record.worker_lease_until_epoch_ms = Some(now_epoch_ms() + 30_000);
        }
        if status.is_terminal() {
            record.worker_lease_until_epoch_ms = None;
        }
        if matches!(
            status,
            CapabilityExecutionStatus::Dispatched | CapabilityExecutionStatus::Running
        ) && record.dispatch_fence.is_none()
        {
            return Err(StoreError::InvalidEvent);
        }
        if matches!(
            status,
            CapabilityExecutionStatus::Succeeded
                | CapabilityExecutionStatus::Timeout
                | CapabilityExecutionStatus::SideEffectUnknown
        ) && record.dispatch_fence.is_none()
        {
            return Err(StoreError::InvalidEvent);
        }
        record.last_sequence += 1;
        record.execution.status = status;
        if status.is_terminal() {
            record.result_summary = payload.get("result").cloned();
        }
        terminal_payload(&mut record.execution, status, &payload);
        let output = CapabilityEventV2 {
            schema_version: CAPABILITY_EVENT_SCHEMA_VERSION.to_string(),
            execution_id: execution_id.to_string(),
            tool_call_id: record.execution.tool_call_id.clone(),
            sequence: record.last_sequence,
            event: event.to_string(),
            status,
            payload,
            created_at_epoch_ms: now_epoch_ms(),
        };
        state
            .events
            .entry(execution_id.to_string())
            .or_default()
            .push((event_id.to_string(), output.clone()));
        Ok(output)
    }

    async fn events(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        after_sequence: u64,
    ) -> Result<CapabilityEventPageV2, StoreError> {
        let mut state = self.state.lock().await;
        let record = record_for_scope(&mut state, scope, execution_id)?.clone();
        let matching: Vec<_> = state
            .events
            .get(execution_id)
            .into_iter()
            .flatten()
            .filter(|(_, event)| event.sequence > after_sequence)
            .map(|(_, event)| event.clone())
            .collect();
        let events: Vec<_> = matching.iter().take(EVENT_PAGE_SIZE).cloned().collect();
        let next_sequence = events.last().map_or(after_sequence, |event| event.sequence);
        Ok(CapabilityEventPageV2 {
            schema_version: CAPABILITY_EVENT_SCHEMA_VERSION.to_string(),
            execution_id: record.execution.execution_id,
            after_sequence,
            next_sequence,
            has_more: matching.len() > events.len(),
            events,
        })
    }

    async fn cancel(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
    ) -> Result<ExecutionRecord, StoreError> {
        let current = self.get(scope, execution_id).await?;
        if current.execution.status.is_terminal() {
            return Ok(current);
        }
        let status = if current.dispatch_fence.is_some()
            && !matches!(current.execution.effect, CapabilityEffect::Read)
        {
            CapabilityExecutionStatus::SideEffectUnknown
        } else {
            CapabilityExecutionStatus::Cancelled
        };
        let mut payload = BTreeMap::new();
        payload.insert(
            "error_code".to_string(),
            Value::String(
                if matches!(status, CapabilityExecutionStatus::SideEffectUnknown) {
                    "side_effect_unknown"
                } else {
                    "cancelled"
                }
                .to_string(),
            ),
        );
        self.append_event(
            scope,
            execution_id,
            &Uuid::now_v7().to_string(),
            "terminal",
            status,
            payload,
            current.dispatch_fence.as_deref(),
        )
        .await?;
        self.get(scope, execution_id).await
    }
}

#[cfg(test)]
mod tests {
    use ai_platform_capability_contract::{
        CAPABILITY_EXECUTION_SCHEMA_VERSION, CapabilityExecutionStatus,
    };
    use serde_json::json;

    use super::*;

    fn new_execution(id: &str) -> NewExecution {
        NewExecution {
            execution: CapabilityExecutionV2 {
                schema_version: CAPABILITY_EXECUTION_SCHEMA_VERSION.to_string(),
                execution_id: id.to_string(),
                lease_id: "00000000-0000-0000-0000-000000000001".to_string(),
                tenant_id: "tenant-a".to_string(),
                user_id: "user-a".to_string(),
                session_id: "session-a".to_string(),
                run_id: "00000000-0000-0000-0000-000000000002".to_string(),
                tool_call_id: "call-a".to_string(),
                attempt_id: "attempt-a".to_string(),
                capability_id: "platform.echo".to_string(),
                capability_revision: 1,
                arguments_hash: canonical_json_hash(&serde_json::json!({})).unwrap(),
                idempotency_key: "idem-a".to_string(),
                effect: CapabilityEffect::Read,
                status: CapabilityExecutionStatus::Published,
                events_url: format!("/internal/v2/capabilities/executions/{id}/events"),
                result: None,
                error: None,
            },
            arguments: serde_json::json!({}),
            resource_binding: serde_json::json!({
                "snapshot_id": "snapshot-a",
                "capability_revision": 1,
                "capability_id": "platform.echo",
                "capability_version": "1",
                "schema_hash": "fixture",
                "bound_dataset_ids": [],
            }),
            approval_policy: ApprovalPolicy::Never,
            approval_id: None,
            approval_status: "not_required".to_string(),
        }
    }

    #[tokio::test]
    async fn reserve_is_idempotent_and_scope_is_closed() {
        let store = MemoryStore::default();
        let first = store
            .reserve(new_execution("00000000-0000-0000-0000-000000000003"))
            .await
            .unwrap();
        assert!(first.created);
        let replay = store
            .reserve(new_execution("00000000-0000-0000-0000-000000000004"))
            .await
            .unwrap();
        assert!(!replay.created);
        let error = store
            .get(
                &CapabilityScopeV2 {
                    tenant_id: "tenant-b".to_string(),
                    user_id: "user-a".to_string(),
                    session_id: "session-a".to_string(),
                },
                &first.record.execution.execution_id,
            )
            .await
            .unwrap_err();
        assert_eq!(error, StoreError::ScopeMismatch);
    }

    #[tokio::test]
    async fn runtime_binding_requires_exact_identity_and_scope() {
        let store = MemoryStore::default();
        let scope = CapabilityScopeV2 {
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
        };
        let binding = RuntimeCapabilityBinding {
            snapshot_id: "snapshot-a".to_string(),
            capability_revision: 7,
            capability_type: "tool".to_string(),
            name: "platform.read_fixture".to_string(),
            capability_id: "platform.read_fixture".to_string(),
            capability_version: "1".to_string(),
            schema_hash: "sha256:fixture".to_string(),
            effect: CapabilityEffect::Read,
            approval_policy: ApprovalPolicy::Never,
            bound_dataset_ids: std::collections::BTreeSet::from(["docs".to_string()]),
            connector_binding: None,
            memory_policy: None,
        };
        store
            .register_runtime_binding(&scope, "run-a", binding.clone())
            .await;
        let identity = CapabilityIdentity {
            capability_type: binding.capability_type.clone(),
            name: binding.name.clone(),
            capability_id: binding.capability_id.clone(),
            version: binding.capability_version.clone(),
            schema_hash: binding.schema_hash.clone(),
            effect: binding.effect,
            approval_policy: binding.approval_policy,
            connector_binding: None,
        };
        assert_eq!(
            store
                .authorize_runtime_binding(&scope, "run-a", 7, &identity)
                .await
                .unwrap(),
            binding
        );
        assert_eq!(
            store
                .authorize_runtime_binding(&scope, "run-a", 8, &identity)
                .await
                .unwrap_err(),
            StoreError::RuntimeBindingUnauthorized
        );
        let other_scope = CapabilityScopeV2 {
            tenant_id: "tenant-b".to_string(),
            ..scope
        };
        assert_eq!(
            store
                .authorize_runtime_binding(&other_scope, "run-a", 7, &identity)
                .await
                .unwrap_err(),
            StoreError::RuntimeBindingUnauthorized
        );
    }

    #[tokio::test]
    async fn event_pages_never_advance_more_than_one_event() {
        let store = MemoryStore::default();
        let outcome = store
            .reserve(new_execution("00000000-0000-0000-0000-000000000003"))
            .await
            .unwrap();
        let scope = CapabilityScopeV2 {
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
        };
        store
            .append_event(
                &scope,
                &outcome.record.execution.execution_id,
                "00000000-0000-0000-0000-000000000011",
                "published",
                CapabilityExecutionStatus::Published,
                BTreeMap::new(),
                None,
            )
            .await
            .unwrap();
        store
            .append_event(
                &scope,
                &outcome.record.execution.execution_id,
                "00000000-0000-0000-0000-000000000012",
                "cancelled",
                CapabilityExecutionStatus::Cancelled,
                BTreeMap::new(),
                None,
            )
            .await
            .unwrap();
        let page = store
            .events(&scope, &outcome.record.execution.execution_id, 0)
            .await
            .unwrap();
        assert_eq!(page.events.len(), 1);
        assert!(page.has_more);
    }

    #[tokio::test]
    async fn an_active_read_claim_cannot_be_replayed_until_its_lease_expires() {
        let store = MemoryStore::default();
        let outcome = store
            .reserve(new_execution("00000000-0000-0000-0000-000000000013"))
            .await
            .unwrap();
        let scope = CapabilityScopeV2 {
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
        };
        let first = store
            .dispatch(
                &scope,
                &outcome.record.execution.execution_id,
                "00000000-0000-0000-0000-000000000014",
            )
            .await
            .unwrap();
        assert!(first.claimed);
        let replay = store
            .dispatch(
                &scope,
                &outcome.record.execution.execution_id,
                "00000000-0000-0000-0000-000000000015",
            )
            .await
            .unwrap();
        assert!(!replay.claimed);
    }

    #[tokio::test]
    async fn lease_renewal_is_bound_to_the_current_dispatch_fence() {
        let store = MemoryStore::default();
        let outcome = store
            .reserve(new_execution("00000000-0000-0000-0000-000000000016"))
            .await
            .unwrap();
        let scope = CapabilityScopeV2 {
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
        };
        let fence = "00000000-0000-0000-0000-000000000017";
        assert!(
            store
                .dispatch(&scope, &outcome.record.execution.execution_id, fence)
                .await
                .unwrap()
                .claimed
        );
        assert!(
            store
                .renew(
                    &scope,
                    &outcome.record.execution.execution_id,
                    fence,
                    30_000,
                )
                .await
                .unwrap()
        );
        assert!(
            !store
                .renew(
                    &scope,
                    &outcome.record.execution.execution_id,
                    "00000000-0000-0000-0000-000000000018",
                    30_000,
                )
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn expired_worker_lease_cannot_be_renewed_by_the_same_fence() {
        let store = MemoryStore::default();
        let outcome = store
            .reserve(new_execution("00000000-0000-0000-0000-000000000025"))
            .await
            .unwrap();
        let scope = CapabilityScopeV2 {
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
        };
        let fence = "00000000-0000-0000-0000-000000000026";
        assert!(
            store
                .dispatch(&scope, &outcome.record.execution.execution_id, fence)
                .await
                .unwrap()
                .claimed
        );
        assert!(
            store
                .renew(
                    &scope,
                    &outcome.record.execution.execution_id,
                    fence,
                    30_000,
                )
                .await
                .unwrap()
        );
        {
            let mut state = store.state.lock().await;
            let record = state
                .executions
                .get_mut(&outcome.record.execution.execution_id)
                .unwrap();
            record.worker_lease_until_epoch_ms = Some(0);
        }
        assert!(
            !store
                .renew(
                    &scope,
                    &outcome.record.execution.execution_id,
                    fence,
                    30_000,
                )
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn published_write_is_claimable_but_active_claim_is_not_recovered() {
        let store = MemoryStore::default();
        let mut requested = new_execution("00000000-0000-0000-0000-000000000019");
        requested.execution.effect = CapabilityEffect::Write;
        requested.approval_id = Some("00000000-0000-0000-0000-000000000019".to_string());
        requested.approval_status = "approved".to_string();
        let approval_arguments = requested.arguments.clone();
        let approval_id = requested.approval_id.clone().unwrap();
        store
            .register_approved_authority(&approval_id, approval_arguments)
            .await;
        let outcome = store.reserve(requested).await.unwrap();
        let recoverable = store.recoverable().await.unwrap();
        assert_eq!(recoverable.len(), 1);
        assert_eq!(recoverable[0].dispatch_fence, None);
        assert!(!needs_side_effect_unknown_recovery(&recoverable[0]));
        let scope = CapabilityScopeV2 {
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
        };
        let dispatch = store
            .dispatch(
                &scope,
                &outcome.record.execution.execution_id,
                "00000000-0000-0000-0000-000000000020",
            )
            .await
            .unwrap();
        assert!(dispatch.claimed);
        assert!(store.recoverable().await.unwrap().is_empty());
        assert!(!needs_side_effect_unknown_recovery(&dispatch.record));
    }

    #[tokio::test]
    async fn claimed_write_is_terminalized_once_as_side_effect_unknown() {
        let store = MemoryStore::default();
        let mut requested = new_execution("00000000-0000-0000-0000-000000000021");
        requested.execution.effect = CapabilityEffect::Write;
        requested.approval_id = Some("00000000-0000-0000-0000-000000000021".to_string());
        requested.approval_status = "approved".to_string();
        let approval_arguments = requested.arguments.clone();
        let approval_id = requested.approval_id.clone().unwrap();
        store
            .register_approved_authority(&approval_id, approval_arguments)
            .await;
        let outcome = store.reserve(requested).await.unwrap();
        let scope = CapabilityScopeV2 {
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
        };
        let fence = "00000000-0000-0000-0000-000000000022";
        let dispatch = store
            .dispatch(&scope, &outcome.record.execution.execution_id, fence)
            .await
            .unwrap();
        assert!(dispatch.claimed);
        {
            let mut state = store.state.lock().await;
            let record = state
                .executions
                .get_mut(&outcome.record.execution.execution_id)
                .unwrap();
            record.worker_lease_until_epoch_ms = Some(now_epoch_ms() + 30_000);
        }
        assert!(store.recoverable().await.unwrap().is_empty());
        {
            let mut state = store.state.lock().await;
            let record = state
                .executions
                .get_mut(&outcome.record.execution.execution_id)
                .unwrap();
            record.worker_lease_until_epoch_ms = Some(0);
        }
        let recovered = store.recoverable().await.unwrap();
        assert_eq!(recovered.len(), 1);
        assert!(needs_side_effect_unknown_recovery(&recovered[0]));
        assert!(durable_recovery_result(&recovered[0]).is_none());
        let mut receipted = recovered[0].clone();
        receipted.result_summary = Some(json!({
            "schema_version": "ai-platform/durable-capability-receipt/v1",
            "capability_id": receipted.execution.capability_id,
            "result": {"receipt_id": "receipt-1"},
        }));
        assert_eq!(
            durable_recovery_result(&receipted),
            Some(json!({"receipt_id": "receipt-1"}))
        );
        let mut payload = BTreeMap::new();
        payload.insert(
            "error_code".to_string(),
            Value::String("side_effect_unknown".to_string()),
        );
        let event_id = "00000000-0000-0000-0000-000000000023";
        let first = store
            .append_event(
                &scope,
                &outcome.record.execution.execution_id,
                event_id,
                "terminal",
                CapabilityExecutionStatus::SideEffectUnknown,
                payload.clone(),
                Some(fence),
            )
            .await
            .unwrap();
        let duplicate = store
            .append_event(
                &scope,
                &outcome.record.execution.execution_id,
                event_id,
                "terminal",
                CapabilityExecutionStatus::SideEffectUnknown,
                payload,
                Some(fence),
            )
            .await
            .unwrap();
        assert_eq!(first.sequence, duplicate.sequence);
        assert_eq!(
            store
                .append_event(
                    &scope,
                    &outcome.record.execution.execution_id,
                    "00000000-0000-0000-0000-000000000024",
                    "terminal",
                    CapabilityExecutionStatus::SideEffectUnknown,
                    BTreeMap::new(),
                    Some(fence),
                )
                .await
                .unwrap_err(),
            StoreError::TerminalImmutable
        );
        assert_eq!(
            store
                .get(&scope, &outcome.record.execution.execution_id)
                .await
                .unwrap()
                .execution
                .status,
            CapabilityExecutionStatus::SideEffectUnknown
        );
        assert_eq!(
            store
                .events(&scope, &outcome.record.execution.execution_id, 0)
                .await
                .unwrap()
                .events
                .len(),
            1
        );
    }
}
