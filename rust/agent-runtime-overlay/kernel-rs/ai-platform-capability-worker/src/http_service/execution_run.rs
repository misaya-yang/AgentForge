//! Execution runner: dispatch fence claim, lease heartbeat, capability
//! dispatch per effect/kind, cancellation, and terminal event persistence.

use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use ai_platform_capability_contract::{
    CapabilityDescriptorV2, CapabilityEffect, CapabilityExecutionStatus, CapabilityScopeV2,
};
use base64::Engine as _;
use serde_json::Value;
use tokio::sync::Notify;
use uuid::Uuid;

use super::execution::lease_for_record;
use super::{WorkerState, event_id_unchecked, payload_within_limit};
use crate::external_write_capabilities::ExternalWriteContext;
use crate::fixture_result;
use crate::local_node_broker::{LocalNodeActionRequest, LocalNodeExecutionContext, LocalNodeScope};
use crate::office_capabilities::OfficeExecutionContext;
use crate::python_artifact_broker::{
    PythonArtifactBrokerError, PythonArtifactUploadContext, terminal_result,
};
use crate::python_code_execution::{
    CodeInputAttachment, PythonCodeExecutionRequest, PythonSandboxLimits,
};
use crate::read_capabilities::ReadCapabilityContext;
use crate::write_capabilities::WriteCapabilityContext;

enum OperationError {
    Failed(String),
    SideEffectUnknown,
}

pub(super) async fn run_execution(
    state: WorkerState,
    scope: CapabilityScopeV2,
    execution_id: String,
    capability_id: String,
    arguments: Value,
    descriptor: CapabilityDescriptorV2,
    mut ticket: super::pools::ExecutionTicket,
) {
    let mut telemetry=crate::trace_context::telemetry_operation();
    let cancellation = state.register_cancellation(&execution_id).await.0;
    tokio::select! {
        _ = cancellation.notified() => {
            state.forget_cancellation(&execution_id).await;
            return;
        }
        slot = ticket.wait() => match slot {
            Ok(()) => (),
            Err(_) => {
                state.forget_cancellation(&execution_id).await;
                return;
            }
        }
    };
    if let Ok(current) = state.store.get(&scope, &execution_id).await
        && current.execution.status.is_terminal()
    {
        state.forget_cancellation(&execution_id).await;
        return;
    }
    let requested_fence = Uuid::now_v7().to_string();
    let dispatch = match state
        .store
        .dispatch(&scope, &execution_id, &requested_fence)
        .await
    {
        Ok(dispatch) => dispatch,
        Err(error) => {
            tracing::error!(%execution_id, %error, "failed to claim capability execution");
            state.forget_cancellation(&execution_id).await;
            return;
        }
    };
    if !dispatch.claimed {
        state.forget_cancellation(&execution_id).await;
        return;
    }
    let fence = dispatch
        .record
        .dispatch_fence
        .clone()
        .unwrap_or(requested_fence);
    // Check again at actual dispatch, including restart recovery. Never replay
    // an old descriptor using a replacement plugin activation or implementation.
    let pack_bound = descriptor.tags.iter().any(|tag| tag.starts_with("plugin-"))
        || state.plugin_activation.as_ref().is_some_and(|active| active.managed.contains(&descriptor.id))
        || !dispatch.record.resource_binding["plugin_activation"].is_null();
    let saved_descriptor = dispatch.record.resource_binding.get("descriptor").cloned()
        .and_then(|value| serde_json::from_value::<CapabilityDescriptorV2>(value).ok());
    if descriptor.validate().is_err()
        || descriptor.id != capability_id
        || descriptor.id != dispatch.record.execution.capability_id
        || descriptor.effect != dispatch.record.execution.effect
        || !state.admits_plugin_descriptor(&descriptor)
        || (pack_bound && saved_descriptor.as_ref() != Some(&descriptor))
    {
        let payload = BTreeMap::from([(
            "error_code".to_string(), Value::String("plugin_activation_changed".to_string()),
        )]);
        if let Err(error) = state.store.append_event(
            &scope, &execution_id, &event_id_unchecked(&execution_id, 4), "terminal",
            CapabilityExecutionStatus::Failed, payload, Some(&fence),
        ).await {
            tracing::error!(%execution_id, %error, "failed to persist plugin activation denial");
        }
        state.forget_cancellation(&execution_id).await;
        return;
    }
    let lease_lost = Arc::new(Notify::new());
    let heartbeat_state = state.clone();
    let heartbeat_scope = scope.clone();
    let heartbeat_execution_id = execution_id.clone();
    let heartbeat_fence = fence.clone();
    let heartbeat_lease_lost = lease_lost.clone();
    let heartbeat = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        interval.tick().await;
        loop {
            interval.tick().await;
            match heartbeat_state
                .store
                .renew(
                    &heartbeat_scope,
                    &heartbeat_execution_id,
                    &heartbeat_fence,
                    30_000,
                )
                .await
            {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(%heartbeat_execution_id, "capability worker lease was lost");
                    heartbeat_lease_lost.notify_one();
                    return;
                }
                Err(error) => {
                    tracing::error!(%heartbeat_execution_id, %error, "failed to renew capability worker lease");
                    heartbeat_lease_lost.notify_one();
                    return;
                }
            }
        }
    });
    if let Err(error) = state
        .store
        .append_event(
            &scope,
            &execution_id,
            &event_id_unchecked(&execution_id, 2),
            "dispatched",
            CapabilityExecutionStatus::Dispatched,
            BTreeMap::new(),
            Some(&fence),
        )
        .await
    {
        tracing::error!(%execution_id, %error, "failed to persist capability dispatch event");
        heartbeat.abort();
        state.forget_cancellation(&execution_id).await;
        return;
    }
    if let Err(error) = state
        .store
        .append_event(
            &scope,
            &execution_id,
            &event_id_unchecked(&execution_id, 3),
            "progress",
            CapabilityExecutionStatus::Running,
            BTreeMap::new(),
            Some(&fence),
        )
        .await
    {
        tracing::error!(%execution_id, %error, "failed to persist capability running event");
        heartbeat.abort();
        state.forget_cancellation(&execution_id).await;
        return;
    }
    let timeout_ms = descriptor.timeout_ms.clamp(1, 120_000);
    let effect = descriptor.effect;
    let python_cancelled = Arc::new(AtomicBool::new(false));
    let python_running = AtomicBool::new(false);
    let operation = async {
        match effect {
            CapabilityEffect::Read => {
                if capability_id == "read_attachment" {
                    let Some(executor) = &state.attachment_executor else {
                        return Err(OperationError::Failed(
                            "attachment_executor_unavailable".into(),
                        ));
                    };
                    let lease = lease_for_record(&dispatch.record.execution);
                    return executor
                        .read_scoped_value(lease, execution_id.clone(), arguments.clone())
                        .await
                        .map(|value| serde_json::to_value(value).unwrap_or(Value::Null))
                        .map_err(|error| OperationError::Failed(error.to_string()));
                }
                if capability_id == "local_node_catalog" || capability_id == "local_node_describe" {
                    let Some(broker) = &state.local_node_broker else {
                        return Err(OperationError::Failed(
                            "local_node_broker_unavailable".into(),
                        ));
                    };
                    let local_scope = local_node_scope(&dispatch.record).ok_or_else(|| {
                        OperationError::Failed("local_node_device_required".into())
                    })?;
                    let descriptor = broker
                        .describe(&local_scope)
                        .await
                        .map_err(|error| OperationError::Failed(error.to_string()))?;
                    return Ok(serde_json::to_value(descriptor).unwrap_or(Value::Null));
                }
                if capability_id == "mcp_docgen__preview_document" {
                    let Some(executor) = &state.office_executor else {
                        return Err(OperationError::Failed(
                            "office_capability_executor_unavailable".into(),
                        ));
                    };
                    let format: ai_platform_office::DocumentFormat =
                        serde_json::from_value(arguments.get("format").cloned().ok_or_else(
                            || OperationError::Failed("office_format_required".into()),
                        )?)
                        .map_err(|_| OperationError::Failed("office_format_invalid".into()))?;
                    let encoded = arguments
                        .get("source_base64")
                        .and_then(Value::as_str)
                        .ok_or_else(|| OperationError::Failed("office_source_required".into()))?;
                    let source = base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .map_err(|_| OperationError::Failed("office_source_invalid".into()))?;
                    return executor
                        .preview_existing_document(format, &source)
                        .map(|value| serde_json::to_value(value).unwrap_or(Value::Null))
                        .map_err(|error| OperationError::Failed(error.to_string()));
                }
                if capability_id == "platform.read_fixture"
                    && let Some(delay_ms) = arguments.get("delay_ms").and_then(Value::as_u64)
                {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms.min(5_000))).await;
                }
                if let Some(result) = fixture_result(&capability_id, &arguments) {
                    Ok(result)
                } else if let Some(executor) = &state.read_executor {
                    let bound_dataset_ids = dispatch
                        .record
                        .resource_binding
                        .get("bound_dataset_ids")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect();
                    crate::read_capabilities::transport::verified_record(&dispatch.record.resource_binding,timeout_ms,executor
                        .execute(
                            &capability_id,
                            &ReadCapabilityContext {
                                tenant_id: scope.tenant_id.clone(),
                                user_id: scope.user_id.clone(),
                                session_id: scope.session_id.clone(),
                                execution_id: execution_id.clone(),
                                tool_call_id: dispatch.record.execution.tool_call_id.clone(),
                                run_id: dispatch.record.execution.run_id.clone(),
                                capability_revision: dispatch.record.execution.capability_revision,
                                bound_dataset_ids,
                                connector_binding: dispatch
                                    .record
                                    .resource_binding
                                    .get("connector_binding")
                                    .filter(|value| !value.is_null())
                                    .cloned()
                                    .map(serde_json::from_value)
                                    .transpose()
                                    .map_err(|_| {
                                        OperationError::Failed(
                                            "runtime_connector_binding_invalid".to_string(),
                                        )
                                    })?,
                            },
                            arguments,
                        )
                        ).await
                        .map_err(|error| OperationError::Failed(error.to_string()))
                } else {
                    Err(OperationError::Failed(
                        "read_capability_executor_unavailable".to_string(),
                    ))
                }
            }
            CapabilityEffect::Write => {
                if capability_id == "execute_python_code" {
                    let Some(executor) = &state.remote_python_executor else {
                        return Err(OperationError::Failed("python_executor_unavailable".into()));
                    };
                    let lease = lease_for_record(&dispatch.record.execution);
                    let code = arguments
                        .get("code")
                        .and_then(Value::as_str)
                        .ok_or_else(|| OperationError::Failed("python_code_required".into()))?
                        .to_owned();
                    let inputs: Vec<CodeInputAttachment> = serde_json::from_value(
                        arguments
                            .get("inputs")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!([])),
                    )
                    .map_err(|_| OperationError::Failed("python_inputs_invalid".into()))?;
                    let limits: PythonSandboxLimits = serde_json::from_value(
                        arguments.get("limits").cloned().unwrap_or_else(|| {
                            serde_json::to_value(PythonSandboxLimits::default())
                                .unwrap_or(Value::Null)
                        }),
                    )
                    .map_err(|_| OperationError::Failed("python_limits_invalid".into()))?;
                    let request = PythonCodeExecutionRequest {
                        lease,
                        arguments_hash: dispatch.record.execution.arguments_hash.clone(),
                        code,
                        inputs,
                        limits,
                    };
                    python_running.store(true, Ordering::Release);
                    let result = executor
                        .execute_cancellable(request, python_cancelled.clone())
                        .await;
                    python_running.store(false, Ordering::Release);
                    if python_cancelled.load(Ordering::Acquire) {
                        return Err(OperationError::Failed("cancelled".into()));
                    }
                    let result = result.map_err(|error| match error {
                        crate::python_code_execution::CodeExecutionError::TimedOut => {
                            OperationError::Failed("capability_timeout".into())
                        }
                        crate::python_code_execution::CodeExecutionError::Cancelled => {
                            OperationError::Failed("cancelled".into())
                        }
                        crate::python_code_execution::CodeExecutionError::SideEffectUnknown => {
                            OperationError::SideEffectUnknown
                        }
                        _ => OperationError::Failed(error.to_string()),
                    })?;
                    let Some(artifact_store) = &state.python_artifact_store else {
                        return Err(OperationError::Failed(
                            "python_artifact_store_unavailable".into(),
                        ));
                    };
                    let context = PythonArtifactUploadContext {
                        tenant_id: scope.tenant_id.clone(),
                        user_id: scope.user_id.clone(),
                        session_id: scope.session_id.clone(),
                        // This is the durable execution record id, not the
                        // capability lease id emitted by the local sandbox.
                        execution_id: execution_id.clone(),
                        run_id: dispatch.record.execution.run_id.clone(),
                        tool_call_id: dispatch.record.execution.tool_call_id.clone(),
                        arguments_hash: dispatch.record.execution.arguments_hash.clone(),
                    };
                    return terminal_result(artifact_store.as_ref(), &context, result)
                        .await
                        .map_err(|error| match error {
                            PythonArtifactBrokerError::SideEffectUnknown => {
                                OperationError::SideEffectUnknown
                            }
                            PythonArtifactBrokerError::Failed => {
                                OperationError::Failed("python_artifact_upload_rejected".into())
                            }
                            PythonArtifactBrokerError::Configuration
                            | PythonArtifactBrokerError::InvalidMetadata => {
                                OperationError::Failed("python_artifact_metadata_invalid".into())
                            }
                        });
                }
                if capability_id == "local_node_action" {
                    let Some(broker) = &state.local_node_broker else {
                        return Err(OperationError::Failed(
                            "local_node_broker_unavailable".into(),
                        ));
                    };
                    let local_scope = local_node_scope(&dispatch.record).ok_or_else(|| {
                        OperationError::Failed("local_node_device_required".into())
                    })?;
                    let operation_name = arguments
                        .get("operation")
                        .and_then(Value::as_str)
                        .ok_or_else(|| {
                            OperationError::Failed("local_node_operation_required".into())
                        })?;
                    let nested = arguments.get("arguments").cloned().unwrap_or(Value::Null);
                    let request = LocalNodeActionRequest {
                        scope: local_scope,
                        operation: operation_name.into(),
                        arguments_hash: dispatch.record.execution.arguments_hash.clone(),
                        arguments: nested,
                        idempotency_key: dispatch.record.execution.idempotency_key.clone(),
                        approval_id: dispatch.record.approval_id.clone(),
                        timeout: std::time::Duration::from_millis(
                            descriptor.timeout_ms.min(120_000),
                        ),
                        cancel_requested: python_cancelled.clone(),
                    };
                    return broker
                        .execute(request)
                        .await
                        .map_err(|error| match error {
                            crate::local_node_broker::LocalNodeBrokerError::SideEffectUnknown
                            | crate::local_node_broker::LocalNodeBrokerError::Timeout => {
                                OperationError::SideEffectUnknown
                            }
                            crate::local_node_broker::LocalNodeBrokerError::CancelledBeforeDispatch => {
                                OperationError::Failed("cancelled".into())
                            }
                            _ => OperationError::Failed(error.to_string()),
                        })
                        .and_then(|result| match result.status {
                            CapabilityExecutionStatus::Succeeded => Ok(
                                serde_json::to_value(result).unwrap_or(Value::Null),
                            ),
                            CapabilityExecutionStatus::Failed => Err(OperationError::Failed(
                                result
                                    .result
                                    .as_ref()
                                    .and_then(|value| value.get("error_code"))
                                    .and_then(Value::as_str)
                                    .unwrap_or("local_node_action_failed")
                                    .into(),
                            )),
                            CapabilityExecutionStatus::Cancelled => {
                                Err(OperationError::Failed("cancelled".into()))
                            }
                            CapabilityExecutionStatus::Timeout => {
                                Err(OperationError::Failed("capability_timeout".into()))
                            }
                            CapabilityExecutionStatus::SideEffectUnknown => {
                                Err(OperationError::SideEffectUnknown)
                            }
                            _ => Err(OperationError::SideEffectUnknown),
                        });
                }
                if capability_id == "mcp_docgen__modify_document" {
                    let Some(executor) = &state.office_executor else {
                        return Err(OperationError::Failed(
                            "office_capability_executor_unavailable".into(),
                        ));
                    };
                    let format: ai_platform_office::DocumentFormat =
                        serde_json::from_value(arguments.get("format").cloned().ok_or_else(
                            || OperationError::Failed("office_format_required".into()),
                        )?)
                        .map_err(|_| OperationError::Failed("office_format_invalid".into()))?;
                    let encoded = arguments
                        .get("source_base64")
                        .and_then(Value::as_str)
                        .ok_or_else(|| OperationError::Failed("office_source_required".into()))?;
                    let source = base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .map_err(|_| OperationError::Failed("office_source_invalid".into()))?;
                    return executor
                        .modify_existing_document(
                            &OfficeExecutionContext {
                                tenant_id: scope.tenant_id.clone(),
                                user_id: scope.user_id.clone(),
                                session_id: scope.session_id.clone(),
                                run_id: dispatch.record.execution.run_id.clone(),
                                execution_id: execution_id.clone(),
                                tool_call_id: dispatch.record.execution.tool_call_id.clone(),
                                arguments_hash: dispatch.record.execution.arguments_hash.clone(),
                            },
                            format,
                            &source,
                            arguments,
                        )
                        .await
                        .map(|value| serde_json::to_value(value).unwrap_or(Value::Null))
                        .map_err(|error| {
                            if error.is_side_effect_unknown() {
                                OperationError::SideEffectUnknown
                            } else {
                                OperationError::Failed(error.to_string())
                            }
                        });
                }
                let bound_dataset_ids = dispatch
                    .record
                    .resource_binding
                    .get("bound_dataset_ids")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                let memory_policy = dispatch
                    .record
                    .resource_binding
                    .get("memory_policy")
                    .filter(|value| !value.is_null())
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|_| {
                        OperationError::Failed("runtime_memory_policy_invalid".to_string())
                    })?;
                let write_context = WriteCapabilityContext {
                    tenant_id: scope.tenant_id.clone(),
                    user_id: scope.user_id.clone(),
                    session_id: scope.session_id.clone(),
                    execution_id: execution_id.clone(),
                    run_id: dispatch.record.execution.run_id.clone(),
                    capability_revision: dispatch.record.execution.capability_revision,
                    bound_dataset_ids,
                    memory_policy,
                };
                if matches!(
                    capability_id.as_str(),
                    "confluence_write" | "generate_image"
                ) {
                    let Some(executor) = &state.external_write_executor else {
                        return Err(OperationError::Failed(
                            "external_write_executor_unavailable".to_string(),
                        ));
                    };
                    let connector_binding = dispatch
                        .record
                        .resource_binding
                        .get("connector_binding")
                        .filter(|value| !value.is_null())
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|_| {
                            OperationError::Failed("runtime_connector_binding_invalid".to_string())
                        })?;
                    executor
                        .execute(
                            &capability_id,
                            &ExternalWriteContext {
                                write: write_context,
                                tool_call_id: dispatch.record.execution.tool_call_id.clone(),
                                arguments_hash: dispatch
                                    .record
                                    .execution
                                    .arguments_hash
                                    .clone(),
                                connector_binding,
                            },
                            arguments,
                        )
                        .await
                        .map_err(|error| {
                            if matches!(
                                error,
                                crate::external_write_capabilities::ExternalWriteError::SideEffectUnknown
                            ) {
                                OperationError::SideEffectUnknown
                            } else {
                                OperationError::Failed(error.to_string())
                            }
                        })
                        .and_then(|result| {
                            serde_json::to_value(result).map_err(|_| {
                                OperationError::Failed(
                                    "external_write_result_invalid".to_string(),
                                )
                            })
                        })
                } else if capability_id == "generate_quiz" {
                    let Some(executor) = &state.quiz_executor else {
                        return Err(OperationError::Failed(
                            "quiz_capability_executor_unavailable".to_string(),
                        ));
                    };
                    executor
                        .persist(
                            &write_context,
                            &dispatch.record.execution.tool_call_id,
                            &dispatch.record.execution.arguments_hash,
                            arguments,
                        )
                        .await
                        .map_err(|error| {
                            if error.is_side_effect_unknown() {
                                OperationError::SideEffectUnknown
                            } else {
                                OperationError::Failed(error.to_string())
                            }
                        })
                        .and_then(|result| {
                            serde_json::to_value(result).map_err(|_| {
                                OperationError::Failed("quiz_capability_result_invalid".to_string())
                            })
                        })
                } else if capability_id == "mcp_docgen__generate_document" {
                    let Some(executor) = &state.office_executor else {
                        return Err(OperationError::Failed(
                            "office_capability_executor_unavailable".to_string(),
                        ));
                    };
                    executor
                        .generate_document(
                            &OfficeExecutionContext {
                                tenant_id: scope.tenant_id.clone(),
                                user_id: scope.user_id.clone(),
                                session_id: scope.session_id.clone(),
                                run_id: dispatch.record.execution.run_id.clone(),
                                execution_id: execution_id.clone(),
                                tool_call_id: dispatch.record.execution.tool_call_id.clone(),
                                arguments_hash: dispatch.record.execution.arguments_hash.clone(),
                            },
                            arguments,
                        )
                        .await
                        .map_err(|error| {
                            if error.is_side_effect_unknown() {
                                OperationError::SideEffectUnknown
                            } else {
                                OperationError::Failed(error.to_string())
                            }
                        })
                        .and_then(|result| {
                            serde_json::to_value(result).map_err(|_| {
                                OperationError::Failed(
                                    "office_capability_result_invalid".to_string(),
                                )
                            })
                        })
                } else {
                    let Some(executor) = &state.write_executor else {
                        return Err(OperationError::Failed(
                            "write_capability_executor_unavailable".to_string(),
                        ));
                    };
                    executor
                        .execute(&capability_id, &write_context, arguments)
                        .await
                        .map_err(|error| {
                            if error.outcome_unknown() {
                                OperationError::SideEffectUnknown
                            } else {
                                OperationError::Failed(error.to_string())
                            }
                        })
                }
            }
            CapabilityEffect::Unknown => Err(OperationError::Failed(
                "capability_effect_unknown".to_string(),
            )),
        }
    };
    tokio::pin!(operation);
    let result = tokio::select! {
        _ = cancellation.notified() => {
            python_cancelled.store(true, Ordering::Release);
            if capability_id == "local_node_action" {
                (&mut operation).await
            } else if python_running.load(Ordering::Acquire) {
                let _ = (&mut operation).await;
                if matches!(effect, CapabilityEffect::Read) {
                    Err(OperationError::Failed("cancelled".to_string()))
                } else {
                    Err(OperationError::SideEffectUnknown)
                }
            } else if matches!(effect, CapabilityEffect::Read) {
                Err(OperationError::Failed("cancelled".to_string()))
            } else {
                Err(OperationError::SideEffectUnknown)
            }
        },
        _ = lease_lost.notified() => {
            python_cancelled.store(true, Ordering::Release);
            if capability_id == "local_node_action" {
                (&mut operation).await
            } else if python_running.load(Ordering::Acquire) {
                let _ = (&mut operation).await;
                if matches!(effect, CapabilityEffect::Read) {
                    Err(OperationError::Failed("capability_lease_lost".to_string()))
                } else {
                    Err(OperationError::SideEffectUnknown)
                }
            } else if matches!(effect, CapabilityEffect::Read) {
                Err(OperationError::Failed("capability_lease_lost".to_string()))
            } else {
                Err(OperationError::SideEffectUnknown)
            }
        },
        _ = tokio::time::sleep(std::time::Duration::from_millis(timeout_ms)) => {
            python_cancelled.store(true, Ordering::Release);
            if capability_id == "local_node_action" {
                (&mut operation).await
            } else if python_running.load(Ordering::Acquire) {
                let _ = (&mut operation).await;
                if matches!(effect, CapabilityEffect::Read) {
                    Err(OperationError::Failed("capability_timeout".to_string()))
                } else {
                    Err(OperationError::SideEffectUnknown)
                }
            } else if matches!(effect, CapabilityEffect::Read) {
                Err(OperationError::Failed("capability_timeout".to_string()))
            } else { Err(OperationError::SideEffectUnknown) }
        },
        result = &mut operation => result,

    };
    heartbeat.abort();
    let mut payload = BTreeMap::new();
    let mut status = match result {
        Ok(result) => { telemetry.success();
            payload.insert("result".to_string(), result);
            CapabilityExecutionStatus::Succeeded
        }
        Err(OperationError::SideEffectUnknown) => {
            payload.insert(
                "error_code".to_string(),
                Value::String("side_effect_unknown".to_string()),
            );
            CapabilityExecutionStatus::SideEffectUnknown
        }
        Err(OperationError::Failed(error_code)) => {
            payload.insert("error_code".to_string(), Value::String(error_code.clone()));
            match error_code.as_str() {
                "cancelled" => CapabilityExecutionStatus::Cancelled,
                "capability_timeout" => CapabilityExecutionStatus::Timeout,
                _ => CapabilityExecutionStatus::Failed,
            }
        }
    };
    if !payload_within_limit(&payload) {
        payload.clear();
        payload.insert(
            "error_code".to_string(),
            Value::String("capability_result_too_large".to_string()),
        );
        status = CapabilityExecutionStatus::Failed;
    }
    if let Err(error) = state
        .store
        .append_event(
            &scope,
            &execution_id,
            &event_id_unchecked(&execution_id, 4),
            "terminal",
            status,
            payload,
            Some(&fence),
        )
        .await
    {
        tracing::error!(%execution_id, %error, "failed to persist capability terminal event");
    }
    state.forget_cancellation(&execution_id).await;
}

fn local_node_scope(record: &crate::ExecutionRecord) -> Option<LocalNodeScope> {
    let binding = &record.resource_binding;
    let connector_binding = binding.get("connector_binding");
    let device_id = binding
        .get("device_id")
        .and_then(Value::as_str)
        .or_else(|| {
            connector_binding
                .and_then(|value| {
                    value
                        .get("device_id")
                        .or_else(|| value.get("connection_id"))
                })
                .and_then(Value::as_str)
        })?;
    if device_id.trim().is_empty() {
        return None;
    }
    let execution = &record.execution;
    Some(LocalNodeScope {
        context: LocalNodeExecutionContext {
            tenant_id: execution.tenant_id.clone(),
            user_id: execution.user_id.clone(),
            session_id: execution.session_id.clone(),
            execution_id: execution.execution_id.clone(),
            run_id: execution.run_id.clone(),
            tool_call_id: execution.tool_call_id.clone(),
            lease_id: execution.lease_id.clone(),
            attempt_id: execution.attempt_id.clone(),
            capability_id: execution.capability_id.clone(),
            capability_revision: execution.capability_revision,
            effect: execution.effect,
        },
        device_id: device_id.into(),
    })
}
