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
    let fence = dispatch.record.dispatch_fence.as_deref().unwrap_or(&requested);
    let payload = BTreeMap::from([("error_code".to_string(), Value::String("plugin_recovery_descriptor_invalid".into()))]);
    if let Err(error) = state.store.append_event(&scope, id, &event_id_unchecked(id, 4),
        "terminal", CapabilityExecutionStatus::Failed, payload, Some(fence)).await {
        tracing::error!(execution_id = %id, %error, "failed to persist invalid recovery descriptor");
    }
}

/// Recovery uses the saved descriptor for every pack-bound execution. A current
/// compiled blueprint cannot supply missing provenance for an old quiz write.
fn recovery_descriptor(state: &WorkerState, record: &ExecutionRecord) -> Option<CapabilityDescriptorV2> {
    let saved = record.resource_binding.get("descriptor").cloned()
        .and_then(|value| serde_json::from_value::<CapabilityDescriptorV2>(value).ok());
    let pack_bound = record.execution.capability_id == ai_platform_plugin_quiz::CAPABILITY_ID
        || state.plugin_activation.as_ref().is_some_and(|active|
            active.managed.contains(&record.execution.capability_id))
        || !record.resource_binding["plugin_activation"].is_null()
        || record.resource_binding["descriptor"]["tags"].as_array().is_some_and(|tags|
            tags.iter().any(|tag| tag.as_str().is_some_and(|tag| tag.starts_with("plugin-"))));
    if pack_bound && saved.is_none() { return None; }
    let descriptor = saved.or_else(|| state.capability_registry.get(&record.execution.capability_id).cloned())?;
    (descriptor.validate().is_ok()
        && descriptor.id == record.execution.capability_id
        && descriptor.effect == record.execution.effect
        && state.admits_plugin_descriptor(&record.execution.tenant_id, &descriptor)).then_some(descriptor)
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
            let Some(descriptor) = recovery_descriptor(&state, &record) else {
                terminalize_rejected_recovery(&state, &record).await;
                continue;
            };
            let ticket = match state.execution_pools.admit(
                super::pools::ExecutionClass::for_descriptor(&descriptor),
                &record.execution.tenant_id,
                super::pools::write_target(&descriptor, &record.execution.scope(), &record.resource_binding),
            ) {
                Ok(ticket) => ticket,
                // Leave durable work unchanged; retry on the next recovery pass.
                Err(_) => continue,
            };
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
                    ticket,
                ),
            ));
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}

#[cfg(test)]
mod plugin_recovery_tests {
    use super::*;
    use ai_platform_capability_contract::{ApprovalPolicy, CapabilityEffect};
    use serde_json::json;
    use std::sync::Arc;
    fn record() -> ExecutionRecord {
        ExecutionRecord {
            execution: serde_json::from_value(json!({
                "schema_version":"capability-execution/v2",
                "execution_id":"00000000-0000-0000-0000-000000000003",
                "lease_id":"00000000-0000-0000-0000-000000000001",
                "tenant_id":"tenant-a", "user_id":"user-a", "session_id":"session-a",
                "run_id":"00000000-0000-0000-0000-000000000002",
                "tool_call_id":"call-a", "attempt_id":"attempt-a", "capability_id":"generate_quiz",
                "capability_revision":1, "arguments_hash":format!("sha256:{}","a".repeat(64)),
                "idempotency_key":"idem-a", "effect":"write", "status":"published", "events_url":"/events"
            })).unwrap(),
            arguments:json!({}), resource_binding:json!({}), result_summary:None,
            approval_policy:ApprovalPolicy::OnRequest, approval_id:Some("approval".into()),
            approval_status:"approved".into(), dispatch_fence:None,
            worker_lease_until_epoch_ms:None, last_sequence:0,
        }
    }
    #[tokio::test]
    async fn retired_builtin_quiz_recovery_cannot_gain_a_current_plugin_descriptor() {
        let state=WorkerState::try_new_with_writes(Arc::new(crate::MemoryStore::default()),
            "token".into(),vec![7;32],false,true).unwrap();
        let mut record=record();
        assert!(recovery_descriptor(&state,&record).is_none());
        let blueprint=crate::platform_catalog::worker_capability_catalog_with_writes(true).unwrap()["generate_quiz"].clone();
        record.resource_binding=json!({"descriptor":blueprint});
        assert!(recovery_descriptor(&state,&record).is_none());
        record.execution.capability_id="todo_read".into();
        record.execution.effect=CapabilityEffect::Read;
        record.resource_binding=json!({});
        assert!(recovery_descriptor(&state,&record).is_some());
    }
    #[tokio::test]
    async fn dispatched_quiz_unknown_commit_is_reconciled_before_activation_denial() {
        let store=Arc::new(crate::MemoryStore::default());
        let state=WorkerState::try_new_with_writes(store.clone(),"token".into(),vec![7;32],false,true).unwrap();
        let record=record();
        let id=record.execution.execution_id.clone();
        let scope=record.execution.scope();
        store.register_approved_authority("approval", record.arguments.clone()).await;
        state.store.reserve(crate::NewExecution { execution:record.execution, arguments:record.arguments,
            resource_binding:record.resource_binding, approval_policy:record.approval_policy,
            approval_id:record.approval_id, approval_status:record.approval_status }).await.unwrap();
        let dispatch=state.store.dispatch(&scope,&id,"fixture-fence").await.unwrap();
        assert!(dispatch.claimed);
        // A live worker's thirty-second claim must not be terminalized, even
        // after its plugin activation disappears. Simulate the actual restart
        // recovery boundary: the persisted worker lease has first expired.
        assert!(!needs_side_effect_unknown_recovery(&dispatch.record));
        assert!(state.store.recoverable().await.unwrap().is_empty());
        {
            let mut memory = store.state.lock().await;
            memory.executions.get_mut(&id).unwrap().worker_lease_until_epoch_ms = Some(0);
        }
        let expired = state.store.get(&scope, &id).await.unwrap();
        assert!(is_recoverable_execution(&expired));
        assert!(needs_side_effect_unknown_recovery(&expired));
        assert!(recovery_descriptor(&state,&expired).is_none());
        terminalize_recovered_side_effect(&state,&expired).await;
        let recovered=state.store.get(&scope,&id).await.unwrap();
        assert_eq!(recovered.execution.status,CapabilityExecutionStatus::SideEffectUnknown);
    }
}
