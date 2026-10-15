//! Kernel event pump: projects server notifications into the durable event
//! store and dispatches dynamic capability tool calls (read path inline,
//! write path through approval + capability worker).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Instant;

use codex_app_server::in_process::InProcessServerEvent;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_protocol::ThreadId;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use sqlx::Row;
use tokio::sync::Semaphore;
use tokio::sync::broadcast;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;

use super::ReadonlyTurnBinding;
use super::RuntimeBroadcastEvent;
use super::TextProjectionState;
use super::projection::persist_projected_events;
use crate::AgentKernel;
use crate::PlatformThreadIdentity;
use crate::PostgresThreadStore;
use crate::SequencedAssistantTurnEventV1;
use crate::approval_control::ApprovalBroker;
use crate::capability_execution::{
    CapabilityExecutionOutcome, ReadonlyCapabilityBinding, execute_capability_with_existing,
};
use crate::capability_worker::CapabilityWorkerClient;
use crate::postgres_store::PlatformLifecycleEvent;
use ai_platform_capability_contract::CapabilityEffect;
use ai_platform_capability_contract::CapabilityExecutionStatus;

#[allow(clippy::too_many_arguments)]
pub(super) async fn route_kernel_events(
    mut kernel: AgentKernel,
    store: Arc<PostgresThreadStore>,
    approvals: ApprovalBroker,
    events: broadcast::Sender<RuntimeBroadcastEvent>,
    kernel_ready: Arc<AtomicBool>,
    readonly_by_turn: Arc<Mutex<HashMap<String, ReadonlyTurnBinding>>>,
    turn_cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    capability_client: reqwest::Client,
    mut shutdown_rx: oneshot::Receiver<()>,
) {
    let request_handle = kernel.request_handle();
    let dynamic_tool_slots = Arc::new(Semaphore::new(16));
    let runtime_cancel = CancellationToken::new();
    let mut dynamic_tasks = tokio::task::JoinSet::new();
    let mut text_projection = TextProjectionState::default();
    loop {
        tokio::select! {
            _ = &mut shutdown_rx => {
                break;
            },
            event = kernel.next_event() => {
                let Some(event) = event else {
                    runtime_cancel.cancel();
                    break;
                };
                match event {
                    InProcessServerEvent::ServerNotification(notification) => {
                        if let codex_app_server_protocol::ServerNotification::TurnCompleted(
                            completed,
                        ) = notification.as_ref()
                        {
                            let token = turn_cancellations
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .remove(&completed.turn.id);
                            if let Some(token) = token {
                                token.cancel();
                            }
                        }
                        persist_projected_events(
                            notification.as_ref(),
                            &store,
                            &events,
                            &readonly_by_turn,
                            &mut text_projection,
                        )
                        .await;
                    }
                    InProcessServerEvent::ServerRequest(request) => {
                        if let codex_app_server_protocol::ServerRequest::DynamicToolCall {
                            request_id,
                            params,
                        } = request.as_ref()
                        {
                            let Some(permit) = dynamic_tool_slots.clone().try_acquire_owned().ok()
                            else {
                                let _ = request_handle.reject_server_request(
                                    request_id.clone(),
                                    JSONRPCErrorError {
                                        code: -32001,
                                        message: "read-only capability concurrency limit reached".to_string(),
                                        data: None,
                                    },
                                );
                                continue;
                            };
                            let request_handle = request_handle.clone();
                            let store = Arc::clone(&store);
                            let readonly_by_turn = Arc::clone(&readonly_by_turn);
                            let capability_client = capability_client.clone();
                            let approvals = approvals.clone();
                            let events = events.clone();
                            let request_id = request_id.clone();
                            let params = params.clone();
                            let thread_id = ThreadId::from_string(&params.thread_id);
                            let root_turn_id = match thread_id {
                                Ok(thread_id) => store.root_turn_id(thread_id, &params.turn_id).await.ok(),
                                Err(_) => None,
                            };
                            let Some(root_turn_id) = root_turn_id else {
                                let _ = request_handle.reject_server_request(
                                    request_id,
                                    JSONRPCErrorError { code: -32001, message: "capability turn attribution unavailable".to_string(), data: None },
                                );
                                continue;
                            };
                            let cancel = {
                                let mut cancellations = turn_cancellations.lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                let parent = cancellations.entry(root_turn_id.clone())
                                    .or_insert_with(|| runtime_cancel.child_token()).clone();
                                cancellations.entry(params.turn_id.clone())
                                    .or_insert_with(|| parent.child_token()).clone()
                            };
                            dynamic_tasks.spawn(async move {
                                let _permit = permit;
                                let result = handle_dynamic_tool_call(
                                    &params,
                                    &root_turn_id,
                                    &store,
                                    &readonly_by_turn,
                                    &capability_client,
                                    &approvals,
                                    &events,
                                    &cancel,
                                )
                                .await;
                                match result {
                                    Ok(value) => {
                                        let _ = request_handle
                                            .resolve_server_request_async(request_id, value)
                                            .await;
                                    }
                                    Err(_) => {
                                        let _ = request_handle.reject_server_request_async(
                                            request_id,
                                            JSONRPCErrorError {
                                                code: -32001,
                                                message: "read-only capability invocation failed".to_string(),
                                                data: None,
                                            },
                                        )
                                        .await;
                                    }
                                }
                            });
                            continue;
                        }
                        match approvals.capture_server_request(request.as_ref(), &store).await {
                            Ok(Some(approval_id)) => {
                                persist_approval_required(request.as_ref(), approval_id, &store, &events).await;
                            }
                            Ok(None) | Err(_) => {
                                let _ = kernel
                                    .reject_server_request(
                                        request.id().clone(),
                                        JSONRPCErrorError {
                                            code: -32001,
                                            message: "platform approval is unavailable or unsupported".to_string(),
                                            data: None,
                                        },
                                    )
                                    .await;
                            }
                        }
                    }
                    InProcessServerEvent::Lagged { skipped } => {
                        warn!(skipped, "Agent in-process event consumer lagged");
                    }
                }
            }
        }
    }
    kernel_ready.store(false, Ordering::Release);
    dynamic_tasks.abort_all();
    while dynamic_tasks.join_next().await.is_some() {}
    let claims: Vec<_> = store
        .execution_claims
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .values()
        .cloned()
        .collect();
    let mut suspended = true;
    for claim in claims {
        let active = sqlx::query_scalar::<_, bool>(
            "SELECT status IN ('running','awaiting_approval') FROM assistant_runs WHERE run_id=$1",
        )
        .bind(claim.run_id)
        .fetch_optional(&store.pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
        if !active {
            continue;
        }
        match request_handle.suspend_thread(claim.thread_id).await {
            Ok(
                codex_protocol::turn_input::SuspendTurnOutcome::Suspended { .. }
                | codex_protocol::turn_input::SuspendTurnOutcome::NotActive,
            ) => {}
            result => {
                warn!(run_id=%claim.run_id, ?result, "turn suspension unavailable; retaining owner until process failure recovery");
                suspended = false;
            }
        }
    }
    if suspended {
        let _ = kernel.shutdown().await;
    }
}

pub(super) async fn handle_dynamic_tool_call(
    params: &codex_app_server_protocol::DynamicToolCallParams,
    root_turn_id: &str,
    store: &PostgresThreadStore,
    readonly_by_turn: &Arc<Mutex<HashMap<String, ReadonlyTurnBinding>>>,
    capability_client: &reqwest::Client,
    approvals: &ApprovalBroker,
    events: &broadcast::Sender<RuntimeBroadcastEvent>,
    cancel: &CancellationToken,
) -> Result<serde_json::Value, String> {
    let run_id = Uuid::parse_str(root_turn_id).map_err(|_| "dynamic_tool_turn_invalid")?;
    let saved = store
        .begin_invocation(run_id, params)
        .await
        .map_err(|_| "dynamic_invocation_persistence_failed")?;
    let response = if let Some(response) = saved {
        response
    } else {
        super::recovery_fault::pause("invocation_saved", params, cancel).await;
        let response = handle_dynamic_tool_call_inner(
            params,
            root_turn_id,
            store,
            readonly_by_turn,
            capability_client,
            approvals,
            events,
            cancel,
        )
        .await?;
        store
            .finish_invocation(run_id, params, &response)
            .await
            .map_err(|_| "dynamic_result_persistence_failed")?;
        response
    };
    // Cold recovery bypasses Core's live tool completion notification. Project
    // the durable fact even when reusing a journal response, never its raw error.
    let thread_id =
        ThreadId::from_string(&params.thread_id).map_err(|_| "dynamic_tool_thread_invalid")?;
    let unknown: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM assistant_runtime_items WHERE kernel_thread_id=$1 AND turn_id=$2 AND event_key=$3 AND event_type='agent-runtime/tool-lifecycle' AND item_type='tool_result' AND status='side_effect_unknown')",
    )
    .bind(Uuid::parse_str(&params.thread_id).map_err(|_| "dynamic_tool_thread_invalid")?)
    .bind(&params.turn_id)
    .bind(format!("tool-result/{}/{}", params.turn_id, params.call_id))
    .fetch_one(&store.pool)
    .await
    .map_err(|_| "dynamic_tool_result_receipt_failed")?;
    if unknown {
        let event = crate::AssistantTurnEventV1::new(
            "side_effect_unknown",
            serde_json::json!({"run_id": root_turn_id, "tool_call_id": params.call_id}),
        );
        persist_capability_projection(params, thread_id, event, store, events).await?;
    }
    super::recovery_fault::pause("invocation_result_saved", params, cancel).await;
    Ok(response)
}

pub(super) async fn handle_dynamic_tool_call_inner(
    params: &codex_app_server_protocol::DynamicToolCallParams,
    root_turn_id: &str,
    store: &PostgresThreadStore,
    readonly_by_turn: &Arc<Mutex<HashMap<String, ReadonlyTurnBinding>>>,
    capability_client: &reqwest::Client,
    approvals: &ApprovalBroker,
    events: &broadcast::Sender<RuntimeBroadcastEvent>,
    cancel: &CancellationToken,
) -> Result<serde_json::Value, String> {
    let thread_id = ThreadId::from_string(&params.thread_id)
        .map_err(|_| "dynamic_tool_thread_invalid".to_string())?;
    let identity = store
        .identity_for_kernel_thread(thread_id)
        .await
        .map_err(|_| "dynamic_tool_scope_unavailable".to_string())?;
    let cached_binding = readonly_by_turn
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(root_turn_id)
        .cloned();
    // Re-check the durable lease on every call. The in-memory entry is only a
    // consistency witness; using it as authority would allow a revoked lease
    // or completed run to invoke the capability plane.
    let mut binding = load_readonly_turn_binding(store, &identity, root_turn_id).await?;
    if let Some(cached_binding) = cached_binding {
        if cached_binding.snapshot_id != binding.snapshot_id
            || cached_binding.capability_revision != binding.capability_revision
        {
            return Err("dynamic_tool_binding_changed".to_string());
        }
        binding.trace_context = cached_binding.trace_context;
    }
    let execution_params = execution_params(params, &identity, root_turn_id);
    let (capability_allowlist, expected_tool, descriptor, effect) =
        crate::readonly_capabilities::resolve_dynamic_capability_descriptor(
            &binding.payload,
            params.namespace.as_deref(),
            &params.tool,
        )
        .map_err(|error| error.to_string())?;
    if params.tool == "read_attachment"
        && !selected_attachment_is_bound(&binding.payload, &params.arguments)
    {
        return Err("runtime_attachment_not_selected_for_turn".to_string());
    }
    let bound_dataset_ids = binding
        .payload
        .get("items")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("kind").and_then(serde_json::Value::as_str) == Some("knowledge"))
        .filter_map(|item| item.get("payload"))
        .filter_map(|payload| payload.get("dataset_id"))
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect::<Vec<_>>();
    let capability_plane_url = std::env::var("AI_PLATFORM_CAPABILITY_PLANE_URL")
        .unwrap_or_else(|_| "http://gateway:8080/internal/v2/agent-capabilities".to_string());
    let mut digest = Sha256::new();
    digest.update(
        serde_json::to_vec(&params.arguments).map_err(|_| "dynamic_tool_arguments_invalid")?,
    );
    let arguments_sha256 = format!("{:x}", digest.finalize());
    store
        .append_platform_lifecycle_event(PlatformLifecycleEvent {
            kernel_thread_id: thread_id,
            turn_id: params.turn_id.clone(),
            item_id: Some(params.call_id.clone()),
            event_key: format!("tool-use/{}/{}", params.turn_id, params.call_id),
            item_type: "tool_use".to_string(),
            status: if effect == CapabilityEffect::Read {
                "dispatched".to_string()
            } else {
                "awaiting_approval".to_string()
            },
            payload: serde_json::json!({
                "schema_version": "agent-runtime-tool-lifecycle/v1",
                "turn_id": params.turn_id,
                "tool_call_id": params.call_id,
                "tool_name": params.tool,
                "tool_namespace": params.namespace,
                "root_turn_id": root_turn_id,
                "execution_call_id": execution_params.call_id,
                "arguments_sha256": arguments_sha256,
                "lifecycle": if effect == CapabilityEffect::Read {
                    "dispatched"
                } else {
                    "awaiting_approval"
                },
                "effect": match effect {
                    CapabilityEffect::Read => "read",
                    CapabilityEffect::Write => "write",
                    CapabilityEffect::Unknown => "unknown",
                },
                "dispatch_state": if effect == CapabilityEffect::Read {
                    "dispatched"
                } else {
                    "awaiting_approval"
                },
            }),
        })
        .await
        .map_err(|_| "dynamic_tool_dispatch_receipt_failed")?;
    let worker_enabled = capability_worker_enabled(
        std::env::var("AI_PLATFORM_CAPABILITY_WORKER_ENABLED")
            .ok()
            .as_deref(),
    );
    let worker_writes_enabled = capability_worker_enabled(
        std::env::var("AI_PLATFORM_CAPABILITY_WORKER_WRITES_ENABLED")
            .ok()
            .as_deref(),
    );
    let worker_url = std::env::var("AI_PLATFORM_CAPABILITY_WORKER_URL").ok();
    let lease_secret = std::env::var("AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET").ok();
    if effect != CapabilityEffect::Read
        && (!worker_enabled
            || !worker_writes_enabled
            || worker_url
                .as_deref()
                .is_none_or(|value| value.trim().is_empty())
            || lease_secret
                .as_deref()
                .is_none_or(|value| value.trim().is_empty()))
    {
        persist_dynamic_terminal_receipt(
            params,
            thread_id,
            store,
            "failed",
            "capability_worker_not_ready",
        )
        .await
        .map_err(|_| "dynamic_tool_terminal_receipt_failed".to_string())?;
        return Ok(structured_capability_response(
            false,
            "capability worker is not ready for this capability",
        ));
    }
    let existing_execution_id:Option<Uuid>=sqlx::query_scalar("SELECT execution_id FROM assistant_capability_executions WHERE run_id=$1 AND tool_call_id=$2 AND attempt_id=$2 AND tenant_id=$3 AND user_id=$4 AND session_id=$5 AND arguments_sha256=$6")
        .bind(Uuid::parse_str(root_turn_id).map_err(|_|"dynamic_tool_turn_invalid")?).bind(&execution_params.call_id)
        .bind(&identity.tenant_id).bind(&identity.user_id).bind(&identity.session_id)
        .bind(ai_platform_capability_contract::canonical_json_hash(&params.arguments).map_err(|_|"dynamic_tool_arguments_invalid")?.trim_start_matches("sha256:").to_string())
        .fetch_optional(&store.pool).await.map_err(|_|"execution_receipt_lookup_failed")?;
    let existing_execution_id = existing_execution_id.map(|id| id.to_string());
    let owner = store
        .assert_execution(Uuid::parse_str(root_turn_id).map_err(|_| "dynamic_tool_turn_invalid")?)
        .await
        .map_err(|_| "execution_fence_lost")?;
    let mut approval_id = None;
    if effect != CapabilityEffect::Read {
        let (id, receiver) = approvals
            .await_dynamic_tool(&execution_params, &expected_tool.id, &identity, store)
            .await?;
        approval_id = Some(id);
        if let Err(error) = persist_dynamic_approval_required(
            &execution_params,
            id,
            effect,
            &identity,
            store,
            events,
        )
        .await
        {
            approvals
                .cancel_dynamic(
                    id,
                    &identity.tenant_id,
                    &identity.user_id,
                    &identity.session_id,
                    "approval_receipt_failed",
                    store,
                )
                .await;
            persist_dynamic_terminal_receipt(
                params,
                thread_id,
                store,
                "failed",
                "approval_receipt_failed",
            )
            .await
            .map_err(|_| "dynamic_tool_terminal_receipt_failed".to_string())?;
            return Ok(structured_capability_response(false, &error));
        }
        super::recovery_fault::pause("approval_saved", params, cancel).await;
        let decision = approvals
            .wait_dynamic_decision(id, receiver, store, cancel)
            .await;
        let Some(decision) = decision else {
            approvals
                .cancel_dynamic(
                    id,
                    &identity.tenant_id,
                    &identity.user_id,
                    &identity.session_id,
                    if cancel.is_cancelled() {
                        "runtime_cancelled"
                    } else {
                        "approval_expired"
                    },
                    store,
                )
                .await;
            persist_dynamic_terminal_receipt(
                params,
                thread_id,
                store,
                if cancel.is_cancelled() {
                    "cancelled"
                } else {
                    "failed"
                },
                if cancel.is_cancelled() {
                    "capability_execution_cancelled"
                } else {
                    "capability_execution_timeout"
                },
            )
            .await
            .map_err(|_| "dynamic_tool_terminal_receipt_failed".to_string())?;
            return Ok(structured_capability_response(
                false,
                if cancel.is_cancelled() {
                    "capability execution cancelled"
                } else {
                    "capability approval expired"
                },
            ));
        };
        if !decision.approved {
            persist_dynamic_terminal_receipt(
                params,
                thread_id,
                store,
                "failed",
                "capability_execution_rejected",
            )
            .await
            .map_err(|_| "dynamic_tool_terminal_receipt_failed".to_string())?;
            return Ok(structured_capability_response(
                false,
                decision
                    .reason
                    .as_deref()
                    .unwrap_or("capability approval rejected"),
            ));
        }
        store
            .append_platform_lifecycle_event(PlatformLifecycleEvent {
                kernel_thread_id: thread_id,
                turn_id: params.turn_id.clone(),
                item_id: Some(params.call_id.clone()),
                event_key: format!("tool-dispatch/{}/{}", params.turn_id, params.call_id),
                item_type: "tool_use".to_string(),
                status: "dispatched".to_string(),
                payload: serde_json::json!({
                    "schema_version": "agent-runtime-tool-lifecycle/v1",
                    "turn_id": params.turn_id,
                    "tool_call_id": params.call_id,
                    "tool_name": params.tool,
                    "tool_namespace": params.namespace,
                    "root_turn_id": root_turn_id,
                    "execution_call_id": execution_params.call_id,
                    "approval_id": id,
                    "lifecycle": "dispatched",
                    "dispatch_state": "dispatched",
                    "effect": "write",
                }),
            })
            .await
            .map_err(|_| "dynamic_tool_dispatch_receipt_failed")?;
    }
    let internal_token = std::env::var("AI_PLATFORM_INTERNAL_TOKEN").unwrap_or_default();
    let approval_id_string = approval_id.map(|id| id.to_string());
    super::recovery_fault::pause("before_dispatch", params, cancel).await;
    let mut result: Result<CapabilityExecutionOutcome, String> = if worker_enabled {
        let worker_url = worker_url
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "capability_worker_url_missing".to_string());
        let lease_secret = lease_secret
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "capability_worker_lease_secret_missing".to_string());
        match (worker_url, lease_secret) {
            (Ok(worker_url), Ok(lease_secret)) => {
                let worker = CapabilityWorkerClient::new(
                    capability_client.clone(),
                    &worker_url,
                    internal_token.clone(),
                )
                .map(|worker| {
                    worker
                        .with_execution_owner(owner.owner_id.to_string(), owner.fence)
                        .with_trace_context(binding.trace_context.clone(), root_turn_id.to_string())
                })
                .map_err(|error| error.code().to_string());
                match worker {
                    Ok(worker) => execute_capability_with_existing(
                        &worker,
                        &identity,
                        &ReadonlyCapabilityBinding {
                            capability_revision: binding.capability_revision as u64,
                            allowlist: capability_allowlist.clone(),
                            expected_tool: expected_tool.clone(),
                            descriptor: descriptor.clone(),
                        },
                        &execution_params,
                        lease_secret.as_bytes(),
                        effect,
                        approval_id_string.as_deref(),
                        cancel,
                        existing_execution_id.as_deref(),
                    )
                    .await
                    .map_err(|error| error.code().to_string()),
                    Err(error) => Err(error),
                }
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    } else if effect == CapabilityEffect::Read {
        crate::capability_plane::invoke_dynamic_tool(
            capability_client,
            &execution_params,
            &identity,
            &capability_plane_url,
            &internal_token,
            binding.capability_revision,
            &binding.snapshot_id,
            &bound_dataset_ids,
            &capability_allowlist,
            &expected_tool,
            &binding.trace_context,
        )
        .await
        .map(|response| CapabilityExecutionOutcome {
            status: if response.get("success").and_then(serde_json::Value::as_bool) == Some(true) {
                CapabilityExecutionStatus::Succeeded
            } else {
                CapabilityExecutionStatus::Failed
            },
            response,
            raw_result: None,
        })
    } else {
        Err("capability_worker_required_for_write".to_string())
    };
    if let Ok(outcome) = &result
        && outcome.status == CapabilityExecutionStatus::Succeeded
        && let Some(event) =
            capability_projection_event(&execution_params, &identity, outcome.raw_result.as_ref())
        && persist_capability_projection(params, thread_id, event, store, events)
            .await
            .is_err()
    {
        result = Err("capability_projection_failed".to_string());
    }
    super::recovery_fault::pause("worker_result_observed", params, cancel).await;
    let (status, detail) = match &result {
        Ok(outcome) => (
            capability_status_name(outcome.status),
            capability_result_detail(outcome),
        ),
        Err(error) => ("failed", error.as_str()),
    };
    let receipt = store
        .append_platform_lifecycle_event(PlatformLifecycleEvent {
            kernel_thread_id: thread_id,
            turn_id: params.turn_id.clone(),
            item_id: Some(params.call_id.clone()),
            event_key: format!("tool-result/{}/{}", params.turn_id, params.call_id),
            item_type: "tool_result".to_string(),
            status: status.to_string(),
            payload: serde_json::json!({
                "schema_version": "agent-runtime-tool-lifecycle/v1",
                "turn_id": params.turn_id,
                "tool_call_id": params.call_id,
                "lifecycle": "terminal",
                "result_status": status,
                "detail": detail,
            }),
        })
        .await;
    if receipt.is_err() {
        return Err("dynamic_tool_result_receipt_failed".to_string());
    }
    result.map(|outcome| outcome.response)
}

fn selected_attachment_is_bound(
    payload: &serde_json::Value,
    arguments: &serde_json::Value,
) -> bool {
    let Some(attachment_id) = arguments
        .get("attachment_id")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    payload
        .get("items")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .any(|item| {
            item.get("kind").and_then(serde_json::Value::as_str) == Some("attachment")
                && item.get("source").and_then(serde_json::Value::as_str) == Some("attachments")
                && item
                    .get("payload")
                    .and_then(|value| value.get("content_ref"))
                    .and_then(serde_json::Value::as_str)
                    == Some(attachment_id)
        })
}

fn capability_projection_event(
    params: &codex_app_server_protocol::DynamicToolCallParams,
    identity: &PlatformThreadIdentity,
    raw_result: Option<&serde_json::Value>,
) -> Option<crate::AssistantTurnEventV1> {
    let result = raw_result?.as_object()?;
    if params.tool == "search_knowledge_base" {
        let fallback_dataset = result
            .get("dataset_ids")
            .and_then(serde_json::Value::as_array)
            .and_then(|items| (items.len() == 1).then(|| items[0].as_str()).flatten());
        let chunks = result
            .get("results")
            .and_then(serde_json::Value::as_array)?
            .iter()
            .filter_map(|item| {
                let item = item.as_object()?;
                let metadata = item.get("metadata").and_then(serde_json::Value::as_object);
                let dataset_id = metadata
                    .and_then(|value| value.get("dataset_id"))
                    .and_then(serde_json::Value::as_str)
                    .or(fallback_dataset)?;
                let content = item.get("text").and_then(serde_json::Value::as_str)?;
                Some(serde_json::json!({
                    "dataset_id": dataset_id,
                    "document_id": item.get("document_id"),
                    "segment_id": item.get("segment_id"),
                    "content": content,
                    "score": item.get("score"),
                    "metadata": item.get("metadata"),
                    "source_type": item.get("source_type"),
                    "citation_text": item.get("citation_text"),
                    "source_reference": item.get("source_reference"),
                }))
            })
            .collect::<Vec<_>>();
        if chunks.is_empty() {
            return None;
        }
        return Some(crate::AssistantTurnEventV1::new(
            "context_retrieved",
            serde_json::json!({
                "run_id": params.turn_id,
                "session_id": identity.session_id,
                "thread_id": identity.runtime_thread_id.to_string(),
                "tool_call_id": params.call_id,
                "chunks": chunks,
            }),
        ));
    }
    if params.tool == "execute_python_code" {
        let status = result
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("failed");
        let stdout = result
            .get("stdout")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let stderr = result
            .get("stderr")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        return Some(crate::AssistantTurnEventV1::new(
            "code_execution_result",
            serde_json::json!({
                "run_id": params.turn_id,
                "session_id": identity.session_id,
                "thread_id": identity.runtime_thread_id.to_string(),
                "tool_call_id": params.call_id,
                "success": status == "succeeded",
                "status": status,
                "exit_code": result.get("exit_code"),
                "duration_ms": result.get("duration_ms"),
                "output": stdout,
                "result": stdout,
                "stderr": stderr,
                "error": result.get("error_message"),
                // The Gateway replaces raw base64 output with durable artifact
                // identities before this result reaches the Runtime.
                "output_files": result.get("artifacts").cloned().unwrap_or_else(|| serde_json::json!([])),
            }),
        ));
    }
    let artifact_id = result
        .get("artifact_id")
        .and_then(serde_json::Value::as_str)?;
    let filename = result
        .get("filename")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(artifact_id);
    let format = filename
        .rsplit_once('.')
        .map_or("file", |(_, suffix)| suffix);
    Some(crate::AssistantTurnEventV1::new(
        "artifact_created",
        serde_json::json!({
            "run_id": params.turn_id,
            "session_id": identity.session_id,
            "thread_id": identity.runtime_thread_id.to_string(),
            "tool_call_id": params.call_id,
            "artifact_id": artifact_id,
            "type": "file",
            "format": format,
            "title": filename,
            "filename": filename,
            "mime_type": result.get("mime_type"),
            "size_bytes": result.get("size_bytes"),
            "download_url": result.get("download_url"),
            "source": params.tool,
        }),
    ))
}

/// Keep native call identity in Codex/lifecycle receipts, while the existing
/// capability contract binds authorization and billing to the authenticated root.
fn execution_params(
    params: &codex_app_server_protocol::DynamicToolCallParams,
    identity: &PlatformThreadIdentity,
    root_turn_id: &str,
) -> codex_app_server_protocol::DynamicToolCallParams {
    let mut execution = params.clone();
    if params.thread_id != identity.runtime_thread_id.to_string() {
        let mut digest = Sha256::new();
        for part in [&params.thread_id, &params.turn_id, &params.call_id] {
            digest.update(part.as_bytes());
            digest.update([0]);
        }
        execution.call_id = format!("child/{:x}", digest.finalize());
    }
    execution.thread_id = identity.runtime_thread_id.to_string();
    execution.turn_id = root_turn_id.to_string();
    execution
}

async fn persist_capability_projection(
    params: &codex_app_server_protocol::DynamicToolCallParams,
    thread_id: ThreadId,
    event: crate::AssistantTurnEventV1,
    store: &PostgresThreadStore,
    events: &broadcast::Sender<RuntimeBroadcastEvent>,
) -> Result<(), String> {
    let event_key = format!(
        "compat/capability/{}/{}/{}",
        params.turn_id, params.call_id, event.event_type
    );
    let mut digest = Sha256::new();
    digest.update(event_key.as_bytes());
    let digest = digest.finalize();
    let mut event_id = [0_u8; 16];
    event_id.copy_from_slice(&digest[..16]);
    let sequence = store
        .append_v1_event(thread_id, Uuid::from_bytes(event_id), &event_key, &event)
        .await
        .map_err(|_| "capability_projection_failed".to_string())?;
    let identity = store
        .identity_for_kernel_thread(thread_id)
        .await
        .map_err(|_| "capability_projection_scope_unavailable".to_string())?;
    let _ = events.send(RuntimeBroadcastEvent {
        root_thread_id: identity.runtime_thread_id,
        event: SequencedAssistantTurnEventV1 { sequence, event },
    });
    Ok(())
}

pub(super) fn capability_worker_enabled(value: Option<&str>) -> bool {
    value.is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

pub(super) fn capability_status_name(status: CapabilityExecutionStatus) -> &'static str {
    match status {
        CapabilityExecutionStatus::Succeeded => "succeeded",
        CapabilityExecutionStatus::Failed => "failed",
        CapabilityExecutionStatus::Cancelled => "cancelled",
        CapabilityExecutionStatus::Timeout => "timeout",
        CapabilityExecutionStatus::SideEffectUnknown => "side_effect_unknown",
        _ => "failed",
    }
}

pub(super) fn capability_result_detail(outcome: &CapabilityExecutionOutcome) -> &'static str {
    match outcome.status {
        CapabilityExecutionStatus::Succeeded => "completed",
        CapabilityExecutionStatus::Failed => "capability_execution_failed",
        CapabilityExecutionStatus::Cancelled => "capability_execution_cancelled",
        CapabilityExecutionStatus::Timeout => "capability_execution_timeout",
        CapabilityExecutionStatus::SideEffectUnknown => "capability_execution_side_effect_unknown",
        _ => "capability_execution_failed",
    }
}

async fn load_readonly_turn_binding(
    store: &PostgresThreadStore,
    identity: &PlatformThreadIdentity,
    turn_id: &str,
) -> Result<ReadonlyTurnBinding, String> {
    let turn_uuid =
        Uuid::parse_str(turn_id).map_err(|_| "dynamic_tool_turn_invalid".to_string())?;
    let row = sqlx::query(
        r#"
        SELECT snapshot.snapshot_id,
               snapshot.capability_revision,
               snapshot.snapshot
          FROM assistant_runtime_snapshots AS snapshot
          JOIN assistant_runtime_model_leases AS lease
            ON lease.snapshot_id = snapshot.snapshot_id
           AND lease.run_id = snapshot.run_id
           AND lease.tenant_id = snapshot.tenant_id
           AND lease.user_id = snapshot.user_id
           AND lease.session_id = snapshot.session_id
         JOIN assistant_runs AS run ON run.run_id = snapshot.run_id
         WHERE snapshot.run_id = $1
           AND snapshot.runtime_thread_id = $2
           AND snapshot.tenant_id = $3
           AND snapshot.user_id = $4
           AND snapshot.session_id = $5
           AND lease.status = 'active'
           AND lease.expires_at > NOW()
           AND run.status = 'running'
           AND run.engine = 'agent_runtime'
           AND NOT EXISTS (
               SELECT 1 FROM assistant_runtime_snapshot_revocations AS revoked
                WHERE revoked.snapshot_id = snapshot.snapshot_id
           )
         ORDER BY snapshot.created_at DESC
         LIMIT 1
        "#,
    )
    .bind(turn_uuid)
    .bind(
        identity
            .runtime_thread_id
            .to_string()
            .parse::<Uuid>()
            .map_err(|_| "dynamic_tool_thread_invalid")?,
    )
    .bind(&identity.tenant_id)
    .bind(&identity.user_id)
    .bind(&identity.session_id)
    .fetch_optional(&store.pool)
    .await
    .map_err(|_| "dynamic_tool_binding_unavailable".to_string())?
    .ok_or_else(|| "dynamic_tool_binding_missing".to_string())?;
    let snapshot: serde_json::Value = row
        .try_get("snapshot")
        .map_err(|_| "dynamic_tool_snapshot_invalid".to_string())?;
    let payload = snapshot
        .get("readonly_capabilities")
        .cloned()
        .ok_or_else(|| "dynamic_tool_binding_missing".to_string())?;
    Ok(ReadonlyTurnBinding {
        snapshot_id: row
            .try_get::<Uuid, _>("snapshot_id")
            .map_err(|_| "dynamic_tool_snapshot_invalid".to_string())?
            .to_string(),
        capability_revision: row
            .try_get("capability_revision")
            .map_err(|_| "dynamic_tool_snapshot_invalid".to_string())?,
        payload,
        trace_context: crate::trace_context::InternalTraceContext::default(),
        created_at: Instant::now(),
    })
}

async fn persist_approval_required(
    request: &codex_app_server_protocol::ServerRequest,
    approval_id: Uuid,
    store: &PostgresThreadStore,
    events: &broadcast::Sender<RuntimeBroadcastEvent>,
) {
    let Ok(raw) = serde_json::to_value(request) else {
        return;
    };
    let Some(params) = raw.get("params") else {
        return;
    };
    let Some(thread_id) = params
        .get("threadId")
        .or_else(|| params.get("conversationId"))
        .and_then(serde_json::Value::as_str)
        .and_then(|value| ThreadId::from_string(value).ok())
    else {
        return;
    };
    let turn_id = params
        .get("turnId")
        .or_else(|| params.get("turn_id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let Ok(identity) = store.identity_for_kernel_thread(thread_id).await else {
        return;
    };
    let event = crate::AssistantTurnEventV1::new(
        "approval_required",
        json!({
            "run_id": turn_id,
            "session_id": identity.session_id,
            "thread_id": identity.runtime_thread_id.to_string(),
            "approval_id": approval_id,
            "tool_call_id": params.get("itemId").or_else(|| params.get("callId")),
            "status": "approval_required",
            "approval_required": true,
        }),
    );
    let event_key = format!("compat/approval/{approval_id}/required");
    match store
        .append_v1_event(thread_id, approval_id, &event_key, &event)
        .await
    {
        Ok(sequence) => {
            let _ = events.send(RuntimeBroadcastEvent {
                root_thread_id: identity.runtime_thread_id,
                event: SequencedAssistantTurnEventV1 { sequence, event },
            });
        }
        Err(error) => warn!(%error, "failed to project approval_required"),
    }
}

async fn persist_dynamic_approval_required(
    params: &codex_app_server_protocol::DynamicToolCallParams,
    approval_id: Uuid,
    effect: CapabilityEffect,
    identity: &PlatformThreadIdentity,
    store: &PostgresThreadStore,
    events: &broadcast::Sender<RuntimeBroadcastEvent>,
) -> Result<(), String> {
    let arguments_hash = ai_platform_capability_contract::canonical_json_hash(&params.arguments)
        .unwrap_or_else(|_| "sha256:invalid".to_string());
    let effect = match effect {
        CapabilityEffect::Read => "read",
        CapabilityEffect::Write => "write",
        CapabilityEffect::Unknown => "unknown",
    };
    let event = crate::AssistantTurnEventV1::new(
        "approval_required",
        json!({
            "run_id": params.turn_id,
            "session_id": identity.session_id,
            "thread_id": identity.runtime_thread_id.to_string(),
            "approval_id": approval_id,
            "tool_id": params.call_id,
            "tool_call_id": params.call_id,
            "tool_name": params.tool,
            "arguments_hash": arguments_hash,
            "effect": effect,
            "status": "approval_required",
            "approval_required": true,
        }),
    );
    let event_key = format!("compat/approval/{approval_id}/required");
    match store
        .append_v1_event(identity.runtime_thread_id, approval_id, &event_key, &event)
        .await
    {
        Ok(sequence) => {
            let _ = events.send(RuntimeBroadcastEvent {
                root_thread_id: identity.runtime_thread_id,
                event: SequencedAssistantTurnEventV1 { sequence, event },
            });
            Ok(())
        }
        Err(error) => {
            warn!(%error, "failed to project dynamic approval_required");
            Err("approval_receipt_failed".to_string())
        }
    }
}

fn structured_capability_response(success: bool, message: &str) -> serde_json::Value {
    crate::capability_execution::dynamic_tool_text_response(success, message)
}

async fn persist_dynamic_terminal_receipt(
    params: &codex_app_server_protocol::DynamicToolCallParams,
    thread_id: ThreadId,
    store: &PostgresThreadStore,
    status: &str,
    detail: &str,
) -> Result<(), ()> {
    store
        .append_platform_lifecycle_event(PlatformLifecycleEvent {
            kernel_thread_id: thread_id,
            turn_id: params.turn_id.clone(),
            item_id: Some(params.call_id.clone()),
            event_key: format!("tool-result/{}/{}", params.turn_id, params.call_id),
            item_type: "tool_result".to_string(),
            status: status.to_string(),
            payload: serde_json::json!({
                "schema_version": "agent-runtime-tool-lifecycle/v1",
                "turn_id": params.turn_id,
                "tool_call_id": params.call_id,
                "lifecycle": "terminal",
                "result_status": status,
                "detail": detail,
            }),
        })
        .await
        .map(|_| ())
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        tool: &str,
    ) -> (
        codex_app_server_protocol::DynamicToolCallParams,
        PlatformThreadIdentity,
    ) {
        let thread_id = ThreadId::new();
        (
            codex_app_server_protocol::DynamicToolCallParams {
                thread_id: thread_id.to_string(),
                turn_id: "00000000-0000-4000-8000-000000000001".to_string(),
                call_id: "call-a".to_string(),
                namespace: None,
                tool: tool.to_string(),
                arguments: serde_json::json!({}),
            },
            PlatformThreadIdentity::new(thread_id, "tenant-a", "user-a", "session-a"),
        )
    }

    #[test]
    fn child_dispatches_use_root_authority_and_distinct_execution_identity() {
        let (mut first, identity) = request("lookup");
        let root_turn = first.turn_id.clone();
        first.thread_id = ThreadId::new().to_string();
        let mut second = first.clone();
        second.thread_id = ThreadId::new().to_string();
        let first_execution = execution_params(&first, &identity, &root_turn);
        let second_execution = execution_params(&second, &identity, &root_turn);
        assert_eq!(
            first_execution.thread_id,
            identity.runtime_thread_id.to_string()
        );
        assert_eq!(first_execution.turn_id, root_turn);
        assert_ne!(first_execution.call_id, second_execution.call_id);
        assert_eq!(first.call_id, "call-a");
        assert!(first_execution.call_id.len() < 255);
    }

    #[test]
    fn knowledge_result_projects_citation_identity() {
        let (params, identity) = request("search_knowledge_base");
        let event = capability_projection_event(
            &params,
            &identity,
            Some(&serde_json::json!({
                "dataset_ids": ["dataset-a"],
                "results": [{
                    "text": "grounded",
                    "document_id": "document-a",
                    "segment_id": "segment-a",
                    "score": 0.9,
                    "metadata": {"dataset_id": "dataset-a"}
                }]
            })),
        )
        .expect("knowledge result should project");
        assert_eq!(event.event_type, "context_retrieved");
        assert_eq!(event.data["chunks"][0]["dataset_id"], "dataset-a");
        assert_eq!(event.data["chunks"][0]["document_id"], "document-a");
        assert_eq!(event.data["chunks"][0]["segment_id"], "segment-a");
        assert_eq!(event.data["chunks"][0]["content"], "grounded");
    }

    #[test]
    fn attachment_dispatch_accepts_only_this_turns_selected_id() {
        let payload = serde_json::json!({"items": [{
            "kind": "attachment", "source": "attachments",
            "payload": {"content_ref": "art_1111111111111111"}
        }]});
        assert!(selected_attachment_is_bound(
            &payload,
            &serde_json::json!({"attachment_id": "art_1111111111111111"})
        ));
        assert!(!selected_attachment_is_bound(
            &payload,
            &serde_json::json!({"attachment_id": "art_2222222222222222"})
        ));
        assert!(!selected_attachment_is_bound(
            &payload,
            &serde_json::json!({})
        ));
    }

    #[test]
    fn document_result_projects_downloadable_artifact() {
        let (params, identity) = request("mcp_docgen__generate_document");
        let event = capability_projection_event(
            &params,
            &identity,
            Some(&serde_json::json!({
                "artifact_id": "art_12345678",
                "filename": "report.docx",
                "size_bytes": 1024,
                "download_url": "/api/v1/assistant/artifacts/art_12345678/download"
            })),
        )
        .expect("artifact result should project");
        assert_eq!(event.event_type, "artifact_created");
        assert_eq!(event.data["artifact_id"], "art_12345678");
        assert_eq!(event.data["format"], "docx");
        assert_eq!(event.data["filename"], "report.docx");
    }

    #[test]
    fn code_result_projects_stdout_and_durable_artifacts() {
        let (params, identity) = request("execute_python_code");
        let event = capability_projection_event(
            &params,
            &identity,
            Some(&serde_json::json!({
                "status": "succeeded",
                "stdout": "CODE-OK\n",
                "stderr": "",
                "exit_code": 0,
                "duration_ms": 12,
                "artifacts": [{
                    "artifact_id": "art_12345678",
                    "filename": "result.txt",
                    "download_url": "/api/v1/assistant/artifacts/art_12345678/download"
                }]
            })),
        )
        .expect("code result should project");
        assert_eq!(event.event_type, "code_execution_result");
        assert_eq!(event.data["success"], true);
        assert_eq!(event.data["result"], "CODE-OK\n");
        assert_eq!(event.data["exit_code"], 0);
        assert_eq!(event.data["output_files"][0]["artifact_id"], "art_12345678");
    }
}
