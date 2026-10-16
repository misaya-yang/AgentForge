//! Restart recovery loop: reclaims recoverable executions, terminalizes
//! side-effect-unknown records, and re-dispatches durable executions.

use std::collections::BTreeMap;

use ai_platform_capability_contract::{CapabilityDescriptorV2, CapabilityExecutionStatus};
use serde_json::Value;

use super::execution_run::run_execution;
use super::{ExecutionScope, WorkerState, event_id_unchecked};
use crate::{
    ExecutionRecord, durable_recovery_result, is_recoverable_execution,
    needs_side_effect_unknown_recovery,
};

async fn terminalize_recovered_side_effect(state: &WorkerState, record: &ExecutionRecord) {
    let Some(fence) = record.dispatch_fence.as_deref() else {
        return;
    };
    let execution_id = &record.execution.execution_id;
    let mut payload = BTreeMap::new();
    let (status, event_message) = if let Some(result) = durable_recovery_result(record) {
        payload.insert("result".to_string(), result);
        (
            CapabilityExecutionStatus::Succeeded,
            "recovered durable capability receipt",
        )
    } else {
        payload.insert(
            "error_code".to_string(),
            Value::String("side_effect_unknown".to_string()),
        );
        (
            CapabilityExecutionStatus::SideEffectUnknown,
            "failed to reconcile recovered side effect",
        )
    };
    payload.insert(
        "recovery".to_string(),
        Value::String("worker_restart".to_string()),
    );
    if let Err(error) = state
        .store
        .append_event(
            &record.execution.scope(),
            execution_id,
            &event_id_unchecked(execution_id, 4),
            "terminal",
            status,
            payload,
            Some(fence),
        )
        .await
    {
        tracing::error!(
            %execution_id,
            %error,
            recovery = event_message,
            "failed to persist recovered capability terminal"
        );
    }
}

async fn terminalize_rejected_recovery(state: &WorkerState, record: &ExecutionRecord) {
    let scope = record.execution.scope();
    let id = &record.execution.execution_id;
    let requested = uuid::Uuid::now_v7().to_string();
    let dispatch = match state.store.dispatch(&scope, id, &requested).await {
        Ok(value) if value.claimed => value,
        _ => return,
    };
    let fence = dispatch
        .record
        .dispatch_fence
        .as_deref()
        .unwrap_or(&requested);
    let payload = BTreeMap::from([(
        "error_code".to_string(),
        Value::String("plugin_recovery_descriptor_invalid".into()),
    )]);
    if let Err(error) = state
        .store
        .append_event(
            &scope,
            id,
            &event_id_unchecked(id, 4),
            "terminal",
            CapabilityExecutionStatus::Failed,
            payload,
            Some(fence),
        )
        .await
    {
        tracing::error!(execution_id = %id, %error, "failed to persist invalid recovery descriptor");
    }
}

pub(super) async fn recover_executions_loop(state: WorkerState) {
    loop {
        let records = match state.store.recoverable().await {
            Ok(records) => records,
            Err(error) => {
                tracing::error!(%error, "failed to load recoverable capability executions");
                Vec::new()
            }
        };
        for record in records {
            if !is_recoverable_execution(&record) {
                continue;
            }
            if needs_side_effect_unknown_recovery(&record) {
                terminalize_recovered_side_effect(&state, &record).await;
                continue;
            }
            // Durable descriptor wins. In particular, a pack may never replace
            // old work merely by reusing its capability ID in today's registry.
            let saved = record
                .resource_binding
                .get("descriptor")
                .cloned()
                .and_then(|value| serde_json::from_value::<CapabilityDescriptorV2>(value).ok());
            let pack_bound = state
                .plugin_activation
                .as_ref()
                .is_some_and(|active| active.managed.contains(&record.execution.capability_id))
                || !record.resource_binding["plugin_activation"].is_null()
                || record.resource_binding["descriptor"]["tags"]
                    .as_array()
                    .is_some_and(|tags| {
                        tags.iter()
                            .any(|tag| tag.as_str().is_some_and(|tag| tag.starts_with("plugin-")))
                    });
            if pack_bound && saved.is_none() {
                terminalize_rejected_recovery(&state, &record).await;
                continue;
            }
            let descriptor = saved.or_else(|| {
                state
                    .capability_registry
                    .get(&record.execution.capability_id)
                    .cloned()
            });
            let Some(descriptor) = descriptor else {
                terminalize_rejected_recovery(&state, &record).await;
                continue;
            };
            if descriptor.validate().is_err()
                || descriptor.id != record.execution.capability_id
                || descriptor.effect != record.execution.effect
            {
                terminalize_rejected_recovery(&state, &record).await;
                continue;
            }
            let execution_id = record.execution.execution_id.clone();
            let (_, registered) = state.register_cancellation(&execution_id).await;
            if !registered {
                continue;
            }
            tokio::spawn(crate::trace_context::InternalTraceContext::default().scope(
                run_execution(
                    state.clone(),
                    record.execution.scope(),
                    execution_id,
                    record.execution.capability_id,
                    record.arguments,
                    descriptor,
                ),
            ));
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}
