//! Durable projection of kernel notifications into the v1 event store,
//! including terminal admission recovery and stable event identity.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use codex_protocol::ThreadId;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use tokio::sync::broadcast;
use tracing::warn;
use uuid::Uuid;

use super::ReadonlyTurnBinding;
use super::RuntimeBroadcastEvent;
use super::TextProjectionState;
use crate::PostgresThreadStore;
use crate::SequencedAssistantTurnEventV1;
use crate::V1ProjectionContext;
use crate::project_server_notification;
use crate::server_notification_thread_id;

pub(super) async fn persist_projected_events(
    notification: &codex_app_server_protocol::ServerNotification,
    store: &PostgresThreadStore,
    events: &broadcast::Sender<RuntimeBroadcastEvent>,
    readonly_by_turn: &Arc<Mutex<HashMap<String, ReadonlyTurnBinding>>>,
    text_projection: &mut TextProjectionState,
) {
    let Some(kernel_thread_id) = server_notification_thread_id(notification)
        .and_then(|value| ThreadId::from_string(value).ok())
    else {
        return;
    };
    let identity = match store.identity_for_kernel_thread(kernel_thread_id).await {
        Ok(identity) => identity,
        Err(error) => {
            let _: codex_thread_store::ThreadStoreResult<()> =
                store.record_write_result(kernel_thread_id, Err(error));
            // A database outage must not strand a connected turn. Cached scope
            // is used exclusively to address a failed, non-durable notification.
            if let codex_app_server_protocol::ServerNotification::TurnCompleted(completed) =
                notification
                && let Some(identity) = store.cached_terminal_identity(kernel_thread_id)
            {
                let context = V1ProjectionContext {
                    tenant_id: identity.tenant_id,
                    user_id: identity.user_id,
                    session_id: identity.session_id,
                };
                let _ = events.send(RuntimeBroadcastEvent {
                    root_thread_id: identity.runtime_thread_id,
                    event: SequencedAssistantTurnEventV1 {
                        sequence: i64::MAX,
                        event: terminal_admission_failure_event(completed, &context),
                    },
                });
            }
            return;
        }
    };
    let context = V1ProjectionContext {
        tenant_id: identity.tenant_id.clone(),
        user_id: identity.user_id.clone(),
        session_id: identity.session_id.clone(),
    };
    let fallback = text_projection.fallback_event(notification, &context);
    if let codex_app_server_protocol::ServerNotification::TurnCompleted(completed) = notification {
        readonly_by_turn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&completed.turn.id);
        let recovered = match store
            .admit_turn_terminal(kernel_thread_id, &completed.turn.id)
            .await
        {
            Ok(recovered) => recovered,
            Err(error) => {
                warn!(%error, turn_id = %completed.turn.id, "terminal admission failed");
                // If PG has recovered, durably record the explicit failure.
                // If it is still unavailable, release the connected client via
                // the established non-durable terminal path; never emit success.
                publish_terminal_failure(
                    completed,
                    &context,
                    kernel_thread_id,
                    identity.runtime_thread_id,
                    store,
                    events,
                )
                .await;
                return;
            }
        };
        for call in recovered {
            let data = json!({
                "run_id": completed.turn.id,
                "session_id": context.session_id,
                "thread_id": completed.thread_id,
                "tool_call_id": call.call_id.clone(),
                "status": call.result_status,
                "success": false,
                "detail": call.detail,
                "recovery": "terminal_admission",
            });
            for event_type in ["tool_call_result", "tool_call_end"] {
                let event = crate::AssistantTurnEventV1::new(event_type, data.clone());
                let event_key = format!(
                    "compat/recovery/{}/{}/{}",
                    completed.turn.id, call.call_id, event_type
                );
                let event_id = stable_event_id(&event_key);
                match store
                    .append_v1_event(kernel_thread_id, event_id, &event_key, &event)
                    .await
                {
                    Ok(sequence) => {
                        let _ = events.send(RuntimeBroadcastEvent {
                            root_thread_id: identity.runtime_thread_id,
                            event: SequencedAssistantTurnEventV1 { sequence, event },
                        });
                    }
                    Err(error) => warn!(%error, "failed to persist recovered tool result"),
                }
            }
        }
    }
    let projected = fallback
        .into_iter()
        .chain(project_server_notification(notification, &context))
        .collect();
    for event in text_projection.normalize_subagent_events(projected) {
        let event_id = Uuid::now_v7();
        let event_key = format!("compat/v1/{event_id}");
        match store
            .append_v1_event(kernel_thread_id, event_id, &event_key, &event)
            .await
        {
            Ok(sequence) => {
                let _ = events.send(RuntimeBroadcastEvent {
                    root_thread_id: identity.runtime_thread_id,
                    event: SequencedAssistantTurnEventV1 { sequence, event },
                });
            }
            Err(error) => {
                warn!(%error, "failed to persist projected Agent event");
                if let codex_app_server_protocol::ServerNotification::TurnCompleted(completed) =
                    notification
                {
                    publish_terminal_failure(
                        completed,
                        &context,
                        kernel_thread_id,
                        identity.runtime_thread_id,
                        store,
                        events,
                    )
                    .await;
                    return;
                }
            }
        }
    }
}

async fn publish_terminal_failure(
    completed: &codex_app_server_protocol::TurnCompletedNotification,
    context: &V1ProjectionContext,
    kernel_thread_id: ThreadId,
    root_thread_id: ThreadId,
    store: &PostgresThreadStore,
    events: &broadcast::Sender<RuntimeBroadcastEvent>,
) {
    let mut event = terminal_admission_failure_event(completed, context);
    event.timestamp = completed.turn.completed_at.unwrap_or_default() as f64;
    event.data["durable"] = true.into();
    let key = format!(
        "compat/terminal-admission-failed/{kernel_thread_id}/{}",
        completed.turn.id
    );
    let sequence = match store
        .append_v1_event(kernel_thread_id, stable_event_id(&key), &key, &event)
        .await
    {
        Ok(sequence) => sequence,
        Err(_) => {
            event.data["durable"] = false.into();
            i64::MAX
        }
    };
    let _ = events.send(RuntimeBroadcastEvent {
        root_thread_id,
        event: SequencedAssistantTurnEventV1 { sequence, event },
    });
}

fn stable_event_id(event_key: &str) -> Uuid {
    let mut digest = Sha256::new();
    digest.update(event_key.as_bytes());
    let digest = digest.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

pub(super) fn terminal_admission_failure_event(
    completed: &codex_app_server_protocol::TurnCompletedNotification,
    context: &V1ProjectionContext,
) -> crate::AssistantTurnEventV1 {
    crate::AssistantTurnEventV1::new(
        "run_error",
        json!({
            "run_id": completed.turn.id,
            "tenant_id": context.tenant_id,
            "user_id": context.user_id,
            "session_id": context.session_id,
            "thread_id": completed.thread_id,
            "status": "failed",
            "exit": "failed",
            "durable": false,
            "terminal_envelope": {
                "schema_version": crate::ASSISTANT_TURN_CONTRACT_V1,
                "run_id": completed.turn.id,
                "tenant_id": context.tenant_id,
                "user_id": context.user_id,
                "session_id": context.session_id,
                "thread_id": completed.thread_id,
                "status": "failed",
                "exit_reason": "terminal_admission_failed",
            },
        }),
    )
}
