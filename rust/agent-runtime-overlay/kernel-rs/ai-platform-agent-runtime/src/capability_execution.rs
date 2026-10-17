//! Runtime-side adapter for one read-only Capability Contract V2 execution.
//!
//! The production dynamic-tool path installs this bridge only when the
//! explicit capability-worker rollout flag is enabled; otherwise the legacy
//! capability plane remains the control path during migration.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_platform_capability_contract::{
    CAPABILITY_EXECUTION_SCHEMA_VERSION, CapabilityDescriptorV2, CapabilityEffect,
    CapabilityEventV2, CapabilityExecutionStatus, CapabilityScopeV2,
    CreateCapabilityExecutionRequestV2, RUNTIME_CAPABILITY_LEASE_SCHEMA_VERSION,
    RuntimeCapabilityLeaseV1, canonical_json_hash,
};
use codex_app_server_protocol::{
    DynamicToolCallOutputContentItem, DynamicToolCallParams, DynamicToolCallResponse,
};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::capability_worker::{CapabilityWorkerClient, CapabilityWorkerError};
use crate::{CapabilityAllowlistEntry, PlatformThreadIdentity};

const MAX_LEASE_TTL: Duration = Duration::from_secs(120);
const EXECUTION_DEADLINE: Duration = Duration::from_secs(30);
const CANCELLATION_GRACE_PERIOD: Duration = Duration::from_secs(2);
const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Clone, Debug)]
pub struct ReadonlyCapabilityBinding {
    pub capability_revision: u64,
    pub allowlist: Vec<CapabilityAllowlistEntry>,
    pub expected_tool: CapabilityAllowlistEntry,
    /// Complete descriptor resolved from the Runtime snapshot.  It is sent
    /// with every Worker execution request; the Worker still verifies it.
    pub descriptor: CapabilityDescriptorV2,
}

/// Transport-independent identity. SDK invocations do not fabricate Codex threads
/// or turns; the authenticated durable run and call are sufficient authority.
#[derive(Clone, Debug)]
pub(crate) struct CapabilityCallIdentity {
    pub scope: CapabilityScopeV2,
    pub run_id: String,
    pub tool_call_id: String,
    pub tool: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapabilityExecutionError {
    InvalidBinding,
    InvalidArguments,
    InvalidLeaseSecret,
    LeaseSigningFailed,
    Worker(CapabilityWorkerError),
    CancellationNotTerminal,
    CancellationTerminalMissing,
    TerminalResultInvalid,
}

/// The worker status is kept alongside the projected response.  A terminal
/// capability event is still a valid dynamic-tool response even when the
/// operation failed or was cancelled; callers must not infer durability from
/// `response.success` alone.
#[derive(Clone, Debug, PartialEq)]
pub struct CapabilityExecutionOutcome {
    pub response: Value,
    pub status: CapabilityExecutionStatus,
    /// Original Worker terminal result retained for platform event projection.
    /// It never crosses the Codex dynamic-tool response boundary directly.
    pub raw_result: Option<Value>,
}

impl CapabilityExecutionError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidBinding => "capability_execution_binding_invalid",
            Self::InvalidArguments => "capability_execution_arguments_invalid",
            Self::InvalidLeaseSecret => "capability_execution_secret_invalid",
            Self::LeaseSigningFailed => "capability_execution_lease_signing_failed",
            Self::Worker(error) => error.code(),
            Self::CancellationNotTerminal => "capability_execution_cancel_not_terminal",
            Self::CancellationTerminalMissing => "capability_execution_cancel_terminal_missing",
            Self::TerminalResultInvalid => "capability_execution_terminal_result_invalid",
        }
    }
}

/// Dispatch one read-only dynamic capability and wait for exactly one
/// terminal event. `secret` is never sent as a request field; it signs only
/// the lease consumed by the worker.
pub async fn execute_readonly_capability(
    worker: &CapabilityWorkerClient,
    identity: &PlatformThreadIdentity,
    binding: &ReadonlyCapabilityBinding,
    params: &DynamicToolCallParams,
    secret: &[u8],
    cancel: &CancellationToken,
) -> Result<CapabilityExecutionOutcome, CapabilityExecutionError> {
    execute_capability(
        worker,
        identity,
        binding,
        params,
        secret,
        CapabilityEffect::Read,
        None,
        cancel,
    )
    .await
}

pub async fn execute_capability(
    worker: &CapabilityWorkerClient,
    identity: &PlatformThreadIdentity,
    binding: &ReadonlyCapabilityBinding,
    params: &DynamicToolCallParams,
    secret: &[u8],
    expected_effect: CapabilityEffect,
    approval_id: Option<&str>,
    cancel: &CancellationToken,
) -> Result<CapabilityExecutionOutcome, CapabilityExecutionError> {
    execute_capability_with_existing(
        worker,
        identity,
        binding,
        params,
        secret,
        expected_effect,
        approval_id,
        cancel,
        None,
    )
    .await
}

pub(crate) async fn execute_capability_with_existing(
    worker: &CapabilityWorkerClient,
    identity: &PlatformThreadIdentity,
    binding: &ReadonlyCapabilityBinding,
    params: &DynamicToolCallParams,
    secret: &[u8],
    expected_effect: CapabilityEffect,
    approval_id: Option<&str>,
    cancel: &CancellationToken,
    existing_execution_id: Option<&str>,
) -> Result<CapabilityExecutionOutcome, CapabilityExecutionError> {
    if cancel.is_cancelled() {
        return Ok(cancelled_before_dispatch());
    }
    if params.thread_id != identity.runtime_thread_id.to_string()
        || params.turn_id.is_empty()
        || params.call_id.is_empty()
    {
        return Err(CapabilityExecutionError::InvalidBinding);
    }
    let call = CapabilityCallIdentity {
        scope: CapabilityScopeV2 {
            tenant_id: identity.tenant_id.clone(),
            user_id: identity.user_id.clone(),
            session_id: identity.session_id.clone(),
        },
        run_id: params.turn_id.clone(),
        tool_call_id: params.call_id.clone(),
        tool: params.tool.clone(),
        arguments: params.arguments.clone(),
    };
    execute_capability_call_with_existing(
        worker, &call, binding, secret, expected_effect, approval_id,
        cancel, existing_execution_id,
    ).await
}

/// Shared signed Worker dispatch, polling, cancellation and recovery path.
/// Both transport wrappers supply already-persisted, authenticated identities.
pub(crate) async fn execute_capability_call_with_existing(
    worker: &CapabilityWorkerClient,
    call: &CapabilityCallIdentity,
    binding: &ReadonlyCapabilityBinding,
    secret: &[u8],
    expected_effect: CapabilityEffect,
    approval_id: Option<&str>,
    cancel: &CancellationToken,
    existing_execution_id: Option<&str>,
) -> Result<CapabilityExecutionOutcome, CapabilityExecutionError> {
    if cancel.is_cancelled() {
        return Ok(cancelled_before_dispatch());
    }
    if call.run_id.is_empty() || call.tool_call_id.is_empty() {
        return Err(CapabilityExecutionError::InvalidBinding);
    }
    validate_binding(binding, &call.tool)?;
    if secret.len() < 32 || binding.capability_revision == 0 {
        return Err(CapabilityExecutionError::InvalidLeaseSecret);
    }
    if !call.arguments.is_object() {
        return Err(CapabilityExecutionError::InvalidArguments);
    }
    let arguments_hash = canonical_json_hash(&call.arguments)
        .map_err(|_| CapabilityExecutionError::InvalidArguments)?;
    let scope = call.scope.clone();
    let now = epoch_ms()?;
    binding
        .descriptor
        .validate()
        .map_err(|_| CapabilityExecutionError::InvalidBinding)?;
    if binding.descriptor.name != call.tool
        || binding.descriptor.effect != expected_effect
        || Some(binding.descriptor.version.as_str())
            != binding.expected_tool.version.as_deref().or(Some("null"))
        || Some(binding.descriptor.schema_hash.as_str())
            != binding.expected_tool.schema_hash.as_deref()
        || binding.descriptor.id != binding.expected_tool.id
    {
        return Err(CapabilityExecutionError::InvalidBinding);
    }
    // Static descriptors remain checked against the Worker catalog. Dynamic
    // descriptors are intentionally absent there and continue to PG-bound
    // Worker validation instead of being treated as NotFound.
    let mut descriptor = binding.descriptor.clone();
    if let Ok(catalog) = worker.catalog(&scope, binding.capability_revision).await
        && let Some(static_descriptor) = catalog
            .capabilities
            .iter()
            .find(|candidate| candidate.id == binding.descriptor.id)
    {
        if static_descriptor.name != call.tool
            || static_descriptor.effect != expected_effect
            || Some(static_descriptor.version.as_str())
                != binding.expected_tool.version.as_deref().or(Some("null"))
            || Some(static_descriptor.schema_hash.as_str())
                != binding.expected_tool.schema_hash.as_deref()
        {
            return Err(CapabilityExecutionError::InvalidBinding);
        }
        descriptor = static_descriptor.clone();
        // The catalog owns static identity/schema/policy; the snapshot still
        // owns the tenant-scoped connector grant attached to this invocation.
        descriptor.connector_binding = binding.descriptor.connector_binding.clone();
    }
    let lease_id = Uuid::now_v7().to_string();
    let nonce = Uuid::now_v7().to_string();
    let mut lease = RuntimeCapabilityLeaseV1 {
        schema_version: RUNTIME_CAPABILITY_LEASE_SCHEMA_VERSION.to_string(),
        lease_id,
        tenant_id: scope.tenant_id.clone(),
        user_id: scope.user_id.clone(),
        session_id: scope.session_id.clone(),
        run_id: call.run_id.clone(),
        tool_call_id: call.tool_call_id.clone(),
        attempt_id: call.tool_call_id.clone(),
        capability_id: binding.expected_tool.id.clone(),
        capability_revision: binding.capability_revision,
        arguments_hash: arguments_hash.clone(),
        effect: expected_effect,
        approval_id: approval_id.map(str::to_string),
        issued_at_epoch_ms: now,
        expires_at_epoch_ms: now.saturating_add(MAX_LEASE_TTL.as_millis() as u64),
        nonce,
        signature: String::new(),
    };
    if !crate::http_service::lease_verification::authorize_call(secret, &mut lease, &descriptor.schema_hash, now)
        .map_err(|_| CapabilityExecutionError::LeaseSigningFailed)? {
        lease.sign(secret).map_err(|_| CapabilityExecutionError::LeaseSigningFailed)?;
    }
    lease
        .validate(now)
        .map_err(|_| CapabilityExecutionError::LeaseSigningFailed)?;
    let mut idempotency_digest = Sha256::new();
    idempotency_digest.update(call.run_id.as_bytes());
    idempotency_digest.update(b"\0");
    idempotency_digest.update(call.tool_call_id.as_bytes());
    idempotency_digest.update(b"\0");
    idempotency_digest.update(arguments_hash.as_bytes());
    let request = CreateCapabilityExecutionRequestV2 {
        schema_version: CAPABILITY_EXECUTION_SCHEMA_VERSION.to_string(),
        idempotency_key: format!("capexec:{:x}", idempotency_digest.finalize()),
        descriptor,
        connector_binding: binding
            .expected_tool
            .connector_binding
            .as_ref()
            .map(|value| serde_json::to_value(value).expect("connector binding is serializable")),
        lease,
        arguments: call.arguments.clone(),
    };
    request
        .validate(now)
        .map_err(|_| CapabilityExecutionError::InvalidBinding)?;
    if cancel.is_cancelled() {
        return Ok(cancelled_before_dispatch());
    }
    // Once create is polled it may already have committed an execution. Drop
    // the in-flight request on cancellation; never resend it under a new ID.
    let creation = tokio::select! {
        biased;
        () = cancel.cancelled() => {
            return Ok(if expected_effect == CapabilityEffect::Read {
                cancelled_before_dispatch()
            } else {
                side_effect_unknown()
            });
        }
        result = async {
            if let Some(execution_id)=existing_execution_id { worker.get(&scope,execution_id).await }
            else { worker.create(&scope,&request).await }
        } => result,
    };
    let execution = match creation {
        Ok(execution) => execution,
        Err(error) => {
            if expected_effect != CapabilityEffect::Read && create_outcome_may_be_unknown(&error) {
                return Ok(side_effect_unknown());
            }
            return Err(CapabilityExecutionError::Worker(error));
        }
    };
    if execution.tenant_id != scope.tenant_id
        || execution.user_id != scope.user_id
        || execution.session_id != scope.session_id
        || execution.run_id != call.run_id
        || execution.tool_call_id != call.tool_call_id
        || execution.attempt_id != call.tool_call_id
        || execution.capability_id != binding.expected_tool.id
        || execution.capability_revision != binding.capability_revision
        || execution.arguments_hash != arguments_hash
        || execution.effect != expected_effect
    {
        return Err(CapabilityExecutionError::InvalidBinding);
    }
    let outcome = async {
        let deadline = tokio::time::Instant::now() + EXECUTION_DEADLINE;
        let mut after_sequence = 0_u64;
        loop {
            if cancel.is_cancelled() {
                return cancel_and_confirm_terminal(
                    worker,
                    &scope,
                    &execution.execution_id,
                    &call.tool_call_id,
                    after_sequence,
                )
                .await;
            }
            if tokio::time::Instant::now() >= deadline {
                return cancel_and_confirm_terminal(
                    worker,
                    &scope,
                    &execution.execution_id,
                    &call.tool_call_id,
                    after_sequence,
                )
                .await;
            }
            let page = tokio::select! {
                () = cancel.cancelled() => {
                    return cancel_and_confirm_terminal(
                        worker,
                        &scope,
                        &execution.execution_id,
                        &call.tool_call_id,
                        after_sequence,
                    )
                    .await;
                }
                () = tokio::time::sleep_until(deadline) => {
                    return cancel_and_confirm_terminal(
                        worker,
                        &scope,
                        &execution.execution_id,
                        &call.tool_call_id,
                        after_sequence,
                    )
                    .await;
                }
                result = worker.events(&scope, &execution.execution_id, after_sequence) => {
                    result.map_err(CapabilityExecutionError::Worker)?
                }
            };
            if let Some(event) = page.events.first() {
                if event.tool_call_id != call.tool_call_id {
                    return Err(CapabilityExecutionError::TerminalResultInvalid);
                }
                after_sequence = event.sequence;
                if event.status.is_terminal() {
                    return project_terminal(event);
                }
                // A newly observed progress event advanced the cursor. Poll the
                // next page immediately; sleeping here can consume the entire
                // bounded cancellation grace period even when terminal is ready.
                continue;
            }
            tokio::time::sleep(EVENT_POLL_INTERVAL).await;
        }
    }
    .await;
    if expected_effect != CapabilityEffect::Read && outcome.is_err() {
        return Ok(side_effect_unknown());
    }
    outcome
}

fn cancelled_before_dispatch() -> CapabilityExecutionOutcome {
    CapabilityExecutionOutcome {
        status: CapabilityExecutionStatus::Cancelled,
        response: dynamic_tool_text_response(
            false,
            "capability execution cancelled before dispatch",
        ),
        raw_result: None,
    }
}

fn side_effect_unknown() -> CapabilityExecutionOutcome {
    CapabilityExecutionOutcome {
        status: CapabilityExecutionStatus::SideEffectUnknown,
        response: dynamic_tool_text_response(
            false,
            "The write may already have executed but its outcome could not be confirmed. Do not retry this action; inspect the execution receipt first.",
        ),
        raw_result: None,
    }
}

fn create_outcome_may_be_unknown(error: &CapabilityWorkerError) -> bool {
    !matches!(
        error,
        CapabilityWorkerError::InvalidBaseUrl
            | CapabilityWorkerError::MissingInternalToken
            | CapabilityWorkerError::InvalidRequest
            | CapabilityWorkerError::Rejected(400 | 401 | 403 | 404 | 409 | 422)
    )
}

/// Cancellation is a durable protocol, not a local projection.  The worker
/// may have completed the execution just before the cancellation request, so
/// its response is only an acknowledgement that the execution is terminal.
/// The event cursor remains authoritative for the single tool result.
async fn cancel_and_confirm_terminal(
    worker: &CapabilityWorkerClient,
    scope: &CapabilityScopeV2,
    execution_id: &str,
    tool_call_id: &str,
    after_sequence: u64,
) -> Result<CapabilityExecutionOutcome, CapabilityExecutionError> {
    cancel_and_confirm_terminal_with_grace(
        worker,
        scope,
        execution_id,
        tool_call_id,
        after_sequence,
        CANCELLATION_GRACE_PERIOD,
    )
    .await
}

async fn cancel_and_confirm_terminal_with_grace(
    worker: &CapabilityWorkerClient,
    scope: &CapabilityScopeV2,
    execution_id: &str,
    tool_call_id: &str,
    mut after_sequence: u64,
    grace_period: Duration,
) -> Result<CapabilityExecutionOutcome, CapabilityExecutionError> {
    let execution = worker
        .cancel(scope, execution_id)
        .await
        .map_err(CapabilityExecutionError::Worker)?;
    if execution.execution_id != execution_id || execution.tool_call_id != tool_call_id {
        return Err(CapabilityExecutionError::TerminalResultInvalid);
    }
    if !execution.status.is_terminal() {
        return Err(CapabilityExecutionError::CancellationNotTerminal);
    }

    let grace_deadline = tokio::time::Instant::now() + grace_period;
    loop {
        let page = match tokio::time::timeout_at(
            grace_deadline,
            worker.events(scope, execution_id, after_sequence),
        )
        .await
        {
            Ok(result) => result.map_err(CapabilityExecutionError::Worker)?,
            Err(_) => return Err(CapabilityExecutionError::CancellationTerminalMissing),
        };
        if let Some(event) = page.events.first() {
            if event.tool_call_id != tool_call_id {
                return Err(CapabilityExecutionError::TerminalResultInvalid);
            }
            after_sequence = event.sequence;
            if event.status.is_terminal() {
                return project_terminal(event);
            }
            // The cursor advanced, so the worker may already have the terminal
            // event ready on the next page. Poll it immediately instead of
            // spending part of the bounded cancellation grace period asleep.
            continue;
        }
        if tokio::time::Instant::now() >= grace_deadline {
            return Err(CapabilityExecutionError::CancellationTerminalMissing);
        }
        tokio::time::sleep(EVENT_POLL_INTERVAL).await;
    }
}

fn validate_binding(
    binding: &ReadonlyCapabilityBinding,
    tool_name: &str,
) -> Result<(), CapabilityExecutionError> {
    if binding.expected_tool.name != tool_name
        || binding.expected_tool.capability_type.is_empty()
        || binding.expected_tool.id.is_empty()
        || binding.expected_tool.schema_hash.is_none()
        || binding
            .allowlist
            .iter()
            .filter(|item| *item == &binding.expected_tool)
            .count()
            != 1
        || binding.descriptor.id != binding.expected_tool.id
        || binding.descriptor.connector_binding
            != binding
                .expected_tool
                .connector_binding
                .as_ref()
                .map(|value| {
                    serde_json::to_value(value).expect("connector binding is serializable")
                })
    {
        return Err(CapabilityExecutionError::InvalidBinding);
    }
    Ok(())
}

fn project_terminal(
    event: &CapabilityEventV2,
) -> Result<CapabilityExecutionOutcome, CapabilityExecutionError> {
    if !event.status.is_terminal() {
        return Err(CapabilityExecutionError::TerminalResultInvalid);
    }
    Ok(CapabilityExecutionOutcome {
        response: project_terminal_response(event)?,
        status: event.status,
        raw_result: event.payload.get("result").cloned(),
    })
}

fn project_terminal_response(event: &CapabilityEventV2) -> Result<Value, CapabilityExecutionError> {
    let success = event.status == CapabilityExecutionStatus::Succeeded;
    if let Some(result) = event.payload.get("result") {
        return project_result_value(result, success);
    }
    if let Some(content_items) = event.payload.get("content_items") {
        if !content_items.is_array() {
            return Err(CapabilityExecutionError::TerminalResultInvalid);
        }
        return project_dynamic_response(content_items, success);
    }
    if success {
        return Err(CapabilityExecutionError::TerminalResultInvalid);
    }
    Ok(error_response(
        event
            .payload
            .get("error_code")
            .or_else(|| event.payload.get("error"))
            .and_then(Value::as_str)
            .unwrap_or("capability execution failed"),
    ))
}

fn project_result_value(result: &Value, success: bool) -> Result<Value, CapabilityExecutionError> {
    if let (Some(content_items), Some(result_success)) =
        (result.get("contentItems"), result.get("success"))
    {
        if !content_items.is_array() || !result_success.is_boolean() {
            return Err(CapabilityExecutionError::TerminalResultInvalid);
        }
        if success && result_success.as_bool() != Some(true) {
            return Err(CapabilityExecutionError::TerminalResultInvalid);
        }
        return project_dynamic_response(content_items, success);
    }
    if let Some(content_items) = result.get("content_items") {
        if !content_items.is_array() {
            return Err(CapabilityExecutionError::TerminalResultInvalid);
        }
        return project_dynamic_response(content_items, success);
    }
    let text = serde_json::to_string(result)
        .map_err(|_| CapabilityExecutionError::TerminalResultInvalid)?;
    Ok(dynamic_tool_text_response(success, &text))
}

fn error_response(message: &str) -> Value {
    dynamic_tool_text_response(false, message)
}

pub(crate) fn dynamic_tool_text_response(success: bool, message: &str) -> Value {
    serde_json::to_value(DynamicToolCallResponse {
        content_items: vec![DynamicToolCallOutputContentItem::InputText {
            text: message.to_string(),
        }],
        success,
    })
    .expect("dynamic tool text response is serializable")
}

pub(crate) fn project_dynamic_response(
    content_items: &Value,
    success: bool,
) -> Result<Value, CapabilityExecutionError> {
    let items = content_items
        .as_array()
        .ok_or(CapabilityExecutionError::TerminalResultInvalid)?
        .iter()
        .map(|item| {
            let item_type = item
                .get("type")
                .and_then(Value::as_str)
                .ok_or(CapabilityExecutionError::TerminalResultInvalid)?;
            match item_type {
                "input_text" | "inputText" => Ok(DynamicToolCallOutputContentItem::InputText {
                    text: item
                        .get("text")
                        .and_then(Value::as_str)
                        .ok_or(CapabilityExecutionError::TerminalResultInvalid)?
                        .to_string(),
                }),
                "input_image" | "inputImage" => Ok(DynamicToolCallOutputContentItem::InputImage {
                    image_url: item
                        .get("image_url")
                        .or_else(|| item.get("imageUrl"))
                        .and_then(Value::as_str)
                        .ok_or(CapabilityExecutionError::TerminalResultInvalid)?
                        .to_string(),
                }),
                "input_audio" | "inputAudio" => Ok(DynamicToolCallOutputContentItem::InputAudio {
                    audio_url: item
                        .get("audio_url")
                        .or_else(|| item.get("audioUrl"))
                        .and_then(Value::as_str)
                        .ok_or(CapabilityExecutionError::TerminalResultInvalid)?
                        .to_string(),
                }),
                _ => Err(CapabilityExecutionError::TerminalResultInvalid),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    serde_json::to_value(DynamicToolCallResponse {
        content_items: items,
        success,
    })
    .map_err(|_| CapabilityExecutionError::TerminalResultInvalid)
}

fn epoch_ms() -> Result<u64, CapabilityExecutionError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .map_err(|_| CapabilityExecutionError::InvalidLeaseSecret)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use super::*;
    use ai_platform_capability_contract::{CapabilityEventPageV2, CapabilityExecutionV2};
    use axum::{
        Json, Router,
        extract::{Path, Query, State},
        routing::{get, post},
    };
    use reqwest::Client;
    use serde::Deserialize;

    const EXECUTION_ID: &str = "00000000-0000-0000-0000-000000000001";

    #[derive(Clone)]
    struct FakeWorkerState {
        cancel_response: CapabilityExecutionV2,
        event_pages: Arc<Mutex<VecDeque<CapabilityEventPageV2>>>,
        event_calls: Arc<Mutex<usize>>,
    }

    #[derive(Deserialize)]
    struct EventCursor {
        after_sequence: u64,
    }

    async fn fake_cancel(State(state): State<FakeWorkerState>) -> Json<CapabilityExecutionV2> {
        Json(state.cancel_response)
    }

    async fn fake_events(
        Path(_execution_id): Path<String>,
        Query(cursor): Query<EventCursor>,
        State(state): State<FakeWorkerState>,
    ) -> Json<CapabilityEventPageV2> {
        *state.event_calls.lock().expect("event call counter") += 1;
        let mut page = state
            .event_pages
            .lock()
            .expect("event pages")
            .pop_front()
            .unwrap_or(CapabilityEventPageV2 {
                schema_version: ai_platform_capability_contract::CAPABILITY_EVENT_SCHEMA_VERSION
                    .to_string(),
                execution_id: EXECUTION_ID.to_string(),
                after_sequence: cursor.after_sequence,
                next_sequence: cursor.after_sequence,
                has_more: false,
                events: vec![],
            });
        page.after_sequence = cursor.after_sequence;
        Json(page)
    }

    fn execution(status: CapabilityExecutionStatus) -> CapabilityExecutionV2 {
        CapabilityExecutionV2 {
            schema_version: CAPABILITY_EXECUTION_SCHEMA_VERSION.to_string(),
            execution_id: EXECUTION_ID.to_string(),
            lease_id: "lease-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            user_id: "user-1".to_string(),
            session_id: "session-1".to_string(),
            run_id: "turn-1".to_string(),
            tool_call_id: "call-1".to_string(),
            attempt_id: "call-1".to_string(),
            capability_id: "knowledge:search".to_string(),
            capability_revision: 1,
            arguments_hash: format!("sha256:{}", "a".repeat(64)),
            idempotency_key: "idempotency-1".to_string(),
            effect: CapabilityEffect::Read,
            status,
            events_url: format!("/internal/v2/capabilities/executions/{EXECUTION_ID}/events"),
            result: None,
            error: None,
        }
    }

    fn terminal_event(status: CapabilityExecutionStatus) -> CapabilityEventV2 {
        CapabilityEventV2 {
            schema_version: ai_platform_capability_contract::CAPABILITY_EVENT_SCHEMA_VERSION
                .to_string(),
            execution_id: EXECUTION_ID.to_string(),
            tool_call_id: "call-1".to_string(),
            sequence: 1,
            event: "terminal".to_string(),
            status,
            payload: BTreeMap::from([(
                "result".to_string(),
                json!({
                    "contentItems": [{"type": "input_text", "text": "done"}],
                    "success": status == CapabilityExecutionStatus::Succeeded
                }),
            )]),
            created_at_epoch_ms: 1,
        }
    }

    async fn fake_worker(
        cancel_response: CapabilityExecutionV2,
        event_pages: Vec<CapabilityEventPageV2>,
    ) -> (
        CapabilityWorkerClient,
        Arc<Mutex<usize>>,
        tokio::task::JoinHandle<()>,
    ) {
        let event_calls = Arc::new(Mutex::new(0));
        let state = FakeWorkerState {
            cancel_response,
            event_pages: Arc::new(Mutex::new(event_pages.into_iter().collect())),
            event_calls: event_calls.clone(),
        };
        let app = Router::new()
            .route(
                "/internal/v2/capabilities/executions/{id}",
                post(fake_cancel),
            )
            .route(
                "/internal/v2/capabilities/executions/{id}/events",
                get(fake_events),
            )
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake worker listener");
        let address = listener.local_addr().expect("fake worker address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("fake worker server");
        });
        let client = Client::builder()
            .no_proxy()
            .build()
            .expect("fake worker client");
        let worker = CapabilityWorkerClient::new(
            client,
            &format!("http://{address}/internal/v2/capabilities"),
            "internal-token",
        )
        .expect("fake worker client config");
        (worker, event_calls, server)
    }

    fn terminal(status: CapabilityExecutionStatus) -> CapabilityEventV2 {
        CapabilityEventV2 {
            schema_version: "capability-event/v2".to_string(),
            execution_id: "execution-1".to_string(),
            tool_call_id: "call-1".to_string(),
            sequence: 1,
            event: "terminal".to_string(),
            status,
            payload: BTreeMap::from([(
                "error".to_string(),
                Value::String("operation stopped".to_string()),
            )]),
            created_at_epoch_ms: 1,
        }
    }

    #[test]
    fn every_terminal_status_projects_one_dynamic_tool_result() {
        for status in [
            CapabilityExecutionStatus::Succeeded,
            CapabilityExecutionStatus::Failed,
            CapabilityExecutionStatus::Cancelled,
            CapabilityExecutionStatus::Timeout,
            CapabilityExecutionStatus::SideEffectUnknown,
        ] {
            let mut event = terminal(status);
            if status == CapabilityExecutionStatus::Succeeded {
                event.payload = BTreeMap::from([(
                    "result".to_string(),
                    json!({
                        "contentItems": [{"type": "input_text", "text": "ok"}],
                        "success": true
                    }),
                )]);
            }
            let outcome = project_terminal(&event).expect("terminal event projects");
            assert_eq!(outcome.status, status);
            assert_eq!(
                outcome.response["contentItems"].as_array().map(Vec::len),
                Some(1)
            );
            assert_eq!(
                outcome.response["success"],
                status == CapabilityExecutionStatus::Succeeded
            );
            assert_eq!(outcome.response["contentItems"][0]["type"], "inputText");
        }
    }

    #[test]
    fn worker_content_items_are_normalized_to_app_server_wire_shape() {
        let response = project_dynamic_response(
            &json!([
                {"type": "input_text", "text": "ok"},
                {"type": "input_image", "image_url": "data:image/png;base64,AA=="},
                {"type": "input_audio", "audio_url": "data:audio/wav;base64,AA=="}
            ]),
            true,
        )
        .expect("worker output projects");

        assert_eq!(response["contentItems"][0]["type"], "inputText");
        assert_eq!(response["contentItems"][1]["type"], "inputImage");
        assert_eq!(
            response["contentItems"][1]["imageUrl"],
            "data:image/png;base64,AA=="
        );
        assert_eq!(response["contentItems"][2]["type"], "inputAudio");
        assert_eq!(
            response["contentItems"][2]["audioUrl"],
            "data:audio/wav;base64,AA=="
        );
    }

    #[test]
    fn succeeded_event_with_failed_response_is_rejected() {
        let mut event = terminal(CapabilityExecutionStatus::Succeeded);
        event.payload = BTreeMap::from([(
            "result".to_string(),
            json!({
                "contentItems": [{"type": "input_text", "text": "no"}],
                "success": false
            }),
        )]);
        assert_eq!(
            project_terminal(&event),
            Err(CapabilityExecutionError::TerminalResultInvalid)
        );
    }

    #[test]
    fn versionless_platform_capability_binds_to_null_wire_version() {
        let expected = CapabilityAllowlistEntry {
            capability_type: "tool".to_string(),
            name: "generate_document".to_string(),
            id: "generate_document".to_string(),
            version: None,
            schema_hash: Some(format!("sha256:{}", "a".repeat(64))),
            connector_binding: None,
        };
        let binding = ReadonlyCapabilityBinding {
            capability_revision: 1,
            allowlist: vec![expected.clone()],
            expected_tool: expected,
            descriptor: CapabilityDescriptorV2 {
                schema_version:
                    ai_platform_capability_contract::CAPABILITY_DESCRIPTOR_SCHEMA_VERSION
                        .to_string(),
                id: "generate_document".to_string(),
                name: "generate_document".to_string(),
                version: "null".to_string(),
                description: "Generate a document".to_string(),
                schema_hash: format!("sha256:{}", "a".repeat(64)),
                input_schema: json!({"type": "object"}),
                output_schema: json!({"type": "object"}),
                effect: CapabilityEffect::Write,
                approval_policy: ai_platform_capability_contract::ApprovalPolicy::Always,
                execution_mode: ai_platform_capability_contract::ExecutionMode::Inline,
                timeout_ms: 30_000,
                tags: vec!["kind:tool".to_string()],
                protocol: "internal".to_string(),
                connector_binding: None,
            },
        };
        let params = DynamicToolCallParams {
            thread_id: "thread-a".to_string(),
            turn_id: "turn-a".to_string(),
            call_id: "call-a".to_string(),
            namespace: None,
            tool: "generate_document".to_string(),
            arguments: json!({}),
        };
        assert_eq!(validate_binding(&binding, &params.tool), Ok(()));
    }

    #[tokio::test]
    async fn pre_cancelled_write_never_reaches_worker_create() {
        let identity = PlatformThreadIdentity::new(
            codex_protocol::ThreadId::new(),
            "tenant",
            "user",
            "session",
        );
        let expected = CapabilityAllowlistEntry {
            capability_type: "tool".to_string(),
            name: "generate_document".to_string(),
            id: "generate_document".to_string(),
            version: None,
            schema_hash: Some(format!("sha256:{}", "a".repeat(64))),
            connector_binding: None,
        };
        let binding = ReadonlyCapabilityBinding {
            capability_revision: 1,
            allowlist: vec![expected.clone()],
            expected_tool: expected,
            descriptor: CapabilityDescriptorV2 {
                schema_version:
                    ai_platform_capability_contract::CAPABILITY_DESCRIPTOR_SCHEMA_VERSION
                        .to_string(),
                id: "generate_document".to_string(),
                name: "generate_document".to_string(),
                version: "null".to_string(),
                description: "Generate a document".to_string(),
                schema_hash: format!("sha256:{}", "a".repeat(64)),
                input_schema: json!({"type": "object"}),
                output_schema: json!({"type": "object"}),
                effect: CapabilityEffect::Write,
                approval_policy: ai_platform_capability_contract::ApprovalPolicy::Always,
                execution_mode: ai_platform_capability_contract::ExecutionMode::Inline,
                timeout_ms: 30_000,
                tags: vec!["kind:tool".to_string()],
                protocol: "internal".to_string(),
                connector_binding: None,
            },
        };
        let params = DynamicToolCallParams {
            thread_id: identity.runtime_thread_id.to_string(),
            turn_id: "turn-a".to_string(),
            call_id: "call-a".to_string(),
            namespace: None,
            tool: "generate_document".to_string(),
            arguments: json!({}),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener");
        let worker = CapabilityWorkerClient::new(
            Client::builder().no_proxy().build().unwrap(),
            &format!(
                "http://{}/internal/v2/capabilities",
                listener.local_addr().unwrap()
            ),
            "internal",
        )
        .unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            execute_capability(
                &worker,
                &identity,
                &binding,
                &params,
                &[0; 32],
                CapabilityEffect::Write,
                Some("approval"),
                &cancel,
            ),
        )
        .await
        .expect("pre-cancel is immediate")
        .expect("cancelled outcome");
        assert_eq!(outcome.status, CapabilityExecutionStatus::Cancelled);
        // The SDK-facing entrypoint needs no Codex thread or dynamic-call type.
        let sdk_call = CapabilityCallIdentity {
            scope: CapabilityScopeV2 {
                tenant_id: "tenant".into(), user_id: "user".into(), session_id: "session".into(),
            },
            run_id: Uuid::now_v7().to_string(),
            tool_call_id: "sdk-call-1".into(),
            tool: "generate_document".into(),
            arguments: json!({}),
        };
        let sdk_outcome = execute_capability_call_with_existing(
            &worker, &sdk_call, &binding, &[0; 32], CapabilityEffect::Write,
            Some("approval"), &cancel, None,
        ).await.expect("pre-cancelled SDK call is terminal");
        assert_eq!(sdk_outcome.status, CapabilityExecutionStatus::Cancelled);
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
    }

    #[test]
    fn uncertain_worker_acceptance_is_never_an_ordinary_write_failure() {
        for error in [
            CapabilityWorkerError::Timeout,
            CapabilityWorkerError::Unavailable,
            CapabilityWorkerError::InvalidResponse,
            CapabilityWorkerError::Rejected(500),
        ] {
            assert!(create_outcome_may_be_unknown(&error));
        }
        assert!(!create_outcome_may_be_unknown(
            &CapabilityWorkerError::Rejected(403)
        ));
        assert_eq!(
            side_effect_unknown().status,
            CapabilityExecutionStatus::SideEffectUnknown
        );
    }

    #[tokio::test]
    async fn cancellation_confirms_worker_and_projects_the_terminal_event() {
        let mut progress = terminal_event(CapabilityExecutionStatus::Cancelled);
        progress.event = "progress".to_string();
        progress.status = CapabilityExecutionStatus::Running;
        let mut terminal = terminal_event(CapabilityExecutionStatus::Cancelled);
        terminal.sequence = 2;
        let first_page = CapabilityEventPageV2 {
            schema_version: ai_platform_capability_contract::CAPABILITY_EVENT_SCHEMA_VERSION
                .to_string(),
            execution_id: EXECUTION_ID.to_string(),
            after_sequence: 0,
            next_sequence: 1,
            has_more: false,
            events: vec![progress],
        };
        let second_page = CapabilityEventPageV2 {
            schema_version: ai_platform_capability_contract::CAPABILITY_EVENT_SCHEMA_VERSION
                .to_string(),
            execution_id: EXECUTION_ID.to_string(),
            after_sequence: 1,
            next_sequence: 2,
            has_more: false,
            events: vec![terminal],
        };
        let (worker, event_calls, server) = fake_worker(
            execution(CapabilityExecutionStatus::Cancelled),
            vec![first_page, second_page],
        )
        .await;
        let scope = CapabilityScopeV2 {
            tenant_id: "tenant-1".to_string(),
            user_id: "user-1".to_string(),
            session_id: "session-1".to_string(),
        };
        let outcome = cancel_and_confirm_terminal_with_grace(
            &worker,
            &scope,
            EXECUTION_ID,
            "call-1",
            0,
            Duration::from_millis(50),
        )
        .await
        .expect("durable cancellation should project");
        assert_eq!(outcome.status, CapabilityExecutionStatus::Cancelled);
        assert_eq!(outcome.response["success"], false);
        assert_eq!(*event_calls.lock().expect("event call counter"), 2);
        server.abort();
    }

    #[tokio::test]
    async fn completion_race_returns_worker_success_instead_of_overriding_it() {
        let page = CapabilityEventPageV2 {
            schema_version: ai_platform_capability_contract::CAPABILITY_EVENT_SCHEMA_VERSION
                .to_string(),
            execution_id: EXECUTION_ID.to_string(),
            after_sequence: 0,
            next_sequence: 1,
            has_more: false,
            events: vec![terminal_event(CapabilityExecutionStatus::Succeeded)],
        };
        let (worker, _, server) =
            fake_worker(execution(CapabilityExecutionStatus::Succeeded), vec![page]).await;
        let scope = CapabilityScopeV2 {
            tenant_id: "tenant-1".to_string(),
            user_id: "user-1".to_string(),
            session_id: "session-1".to_string(),
        };
        let outcome = cancel_and_confirm_terminal_with_grace(
            &worker,
            &scope,
            EXECUTION_ID,
            "call-1",
            0,
            Duration::from_millis(50),
        )
        .await
        .expect("completed execution should project");
        assert_eq!(outcome.status, CapabilityExecutionStatus::Succeeded);
        assert_eq!(outcome.response["success"], true);
        server.abort();
    }

    #[tokio::test]
    async fn nonterminal_cancel_response_is_rejected_without_event_drain() {
        let (worker, event_calls, server) =
            fake_worker(execution(CapabilityExecutionStatus::Running), vec![]).await;
        let scope = CapabilityScopeV2 {
            tenant_id: "tenant-1".to_string(),
            user_id: "user-1".to_string(),
            session_id: "session-1".to_string(),
        };
        assert_eq!(
            cancel_and_confirm_terminal_with_grace(
                &worker,
                &scope,
                EXECUTION_ID,
                "call-1",
                0,
                Duration::from_millis(50),
            )
            .await,
            Err(CapabilityExecutionError::CancellationNotTerminal)
        );
        assert_eq!(*event_calls.lock().expect("event call counter"), 0);
        server.abort();
    }

    #[tokio::test]
    async fn missing_terminal_event_is_rejected_after_grace_period() {
        let (worker, event_calls, server) =
            fake_worker(execution(CapabilityExecutionStatus::Cancelled), vec![]).await;
        let scope = CapabilityScopeV2 {
            tenant_id: "tenant-1".to_string(),
            user_id: "user-1".to_string(),
            session_id: "session-1".to_string(),
        };
        assert_eq!(
            cancel_and_confirm_terminal_with_grace(
                &worker,
                &scope,
                EXECUTION_ID,
                "call-1",
                0,
                Duration::from_millis(20),
            )
            .await,
            Err(CapabilityExecutionError::CancellationTerminalMissing)
        );
        assert!(*event_calls.lock().expect("event call counter") > 0);
        server.abort();
    }
}
