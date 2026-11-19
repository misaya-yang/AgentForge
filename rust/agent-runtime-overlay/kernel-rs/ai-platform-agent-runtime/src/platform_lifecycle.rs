//! Durable platform lifecycle hooks installed into the Agent kernel.

use std::collections::BTreeMap;
use std::sync::Arc;

use codex_core::config::Config;
use codex_extension_api::CommandStartInput;
use codex_extension_api::ExtensionFuture;
use codex_extension_api::ThreadLifecycleContributor;
use codex_extension_api::ThreadResumeInput;
use codex_extension_api::ToolCallOutcome;
use codex_extension_api::ToolDispatchError;
use codex_extension_api::ToolEffect;
use codex_extension_api::ToolFinishInput;
use codex_extension_api::ToolLifecycleContributor;
use codex_extension_api::ToolLifecycleFuture;
use codex_extension_api::ToolStartInput;
use codex_extension_api::TurnAbortInput;
use codex_extension_api::TurnLifecycleContributor;
use codex_extension_api::TurnStopInput;
use codex_protocol::ThreadId;
use codex_tools::ToolPayload;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;

use crate::PostgresThreadStore;
use crate::postgres_store::PlatformLifecycleEvent;

#[derive(Clone)]
pub(crate) struct PlatformLifecycleContributor {
    store: Arc<PostgresThreadStore>,
}

impl PlatformLifecycleContributor {
    pub(crate) fn new(store: Arc<PostgresThreadStore>) -> Self {
        Self { store }
    }

    async fn recover_unclosed(&self, thread_id: ThreadId, turn_id: &str) {
        let Ok(events) = self
            .store
            .read_platform_lifecycle_events(thread_id, turn_id)
            .await
        else {
            return;
        };
        let identity = self.store.identity_for_kernel_thread(thread_id).await.ok();
        for call in unclosed_tool_calls(&events) {
            let payload = json!({
                "schema_version": "agent-runtime-tool-lifecycle/v1",
                "turn_id": turn_id,
                "tool_call_id": call.call_id.clone(),
                "lifecycle": "terminal",
                "result_status": call.result_status,
                "detail": call.detail,
                "recovery": "runtime_restart_or_interrupt",
            });
            let _ = self
                .store
                .append_platform_lifecycle_event(PlatformLifecycleEvent {
                    kernel_thread_id: thread_id,
                    turn_id: turn_id.to_string(),
                    item_id: Some(call.call_id.clone()),
                    event_key: format!("tool-result/{turn_id}/{}", call.call_id),
                    item_type: "tool_result".to_string(),
                    status: call.result_status.to_string(),
                    payload,
                })
                .await;
            if let Some(identity) = &identity {
                let data = json!({
                    "run_id": turn_id,
                    "session_id": identity.session_id,
                    "thread_id": identity.runtime_thread_id,
                    "tool_call_id": call.call_id.clone(),
                    "status": call.result_status,
                    "success": false,
                    "detail": call.detail,
                    "recovery": "runtime_restart_or_interrupt",
                });
                for event_type in ["tool_call_result", "tool_call_end"] {
                    let event = crate::AssistantTurnEventV1::new(event_type, data.clone());
                    let event_key = format!(
                        "compat/recovery/{}/{}/{}",
                        turn_id, call.call_id, event_type
                    );
                    let _ = self
                        .store
                        .append_v1_event(thread_id, uuid::Uuid::now_v7(), &event_key, &event)
                        .await;
                }
            }
        }
    }

    async fn append(&self, event: PlatformLifecycleEvent) -> Result<(), ToolDispatchError> {
        self.store
            .append_platform_lifecycle_event(event)
            .await
            .map(|_| ())
            .map_err(|_| ToolDispatchError {
                code: "AI_PLATFORM_AGENT_RUNTIME_TOOL_RECEIPT_UNAVAILABLE".to_string(),
                message: "tool dispatch blocked because its durable receipt could not be written"
                    .to_string(),
            })
    }

    /// The immutable thread catalog identifies calls owned by the dynamic
    /// broker. Classify by structured namespace/name, including inherited child
    /// catalogs, before the broker applies the current turn's narrower snapshot.
    async fn is_dynamic_capability(
        &self,
        thread_id: ThreadId,
        tool_name: &codex_extension_api::ToolName,
    ) -> Result<bool, ToolDispatchError> {
        let thread_id =
            uuid::Uuid::parse_str(&thread_id.to_string()).map_err(|_| ToolDispatchError {
                code: "AI_PLATFORM_AGENT_RUNTIME_SCOPE_INVALID".to_string(),
                message: "tool scope is invalid".to_string(),
            })?;
        let catalog: Option<Value> = sqlx::query_scalar(
            "SELECT member.metadata->'dynamic_tools' FROM assistant_runtime_thread_members AS member JOIN assistant_runtime_threads AS root ON root.runtime_thread_id=member.runtime_thread_id AND root.deleted_at IS NULL WHERE member.kernel_thread_id=$1",
        )
        .bind(thread_id)
        .fetch_optional(&self.store.pool)
        .await
        .map_err(|_| catalog_unavailable())?
        .flatten();
        let raw: Vec<Value> = serde_json::from_value(catalog.unwrap_or_else(|| json!([])))
            .map_err(|_| catalog_unavailable())?;
        let tools = codex_protocol::dynamic_tools::normalize_dynamic_tool_specs(raw)
            .map_err(|_| catalog_unavailable())?;
        Ok(dynamic_catalog_contains(&tools, tool_name))
    }
}

fn catalog_unavailable() -> ToolDispatchError {
    ToolDispatchError {
        code: "AI_PLATFORM_AGENT_RUNTIME_CATALOG_UNAVAILABLE".to_string(),
        message: "tool lifecycle owner could not be verified".to_string(),
    }
}

fn dynamic_catalog_contains(
    tools: &[codex_protocol::dynamic_tools::DynamicToolSpec],
    name: &codex_extension_api::ToolName,
) -> bool {
    use codex_protocol::dynamic_tools::{DynamicToolNamespaceTool, DynamicToolSpec};
    tools.iter().any(|tool| match tool {
        DynamicToolSpec::Function(function) => {
            name.is_default_namespace() && name.name == function.name
        }
        DynamicToolSpec::Namespace(namespace) => {
            let namespace_matches = name.namespace.as_deref() == Some(namespace.name.as_str())
                || (name.is_default_namespace() && namespace.name == "functions");
            namespace_matches
                && namespace.tools.iter().any(|tool| match tool {
                    DynamicToolNamespaceTool::Function(function) => name.name == function.name,
                })
        }
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecoveredToolCall {
    pub(crate) call_id: String,
    pub(crate) result_status: &'static str,
    pub(crate) detail: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PersistedPhase {
    Published,
    AwaitingApproval,
    Dispatched,
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PersistedEffect {
    Read,
    Write,
    Unknown,
}

#[derive(Clone, Copy, Debug)]
struct PersistedCall {
    phase: PersistedPhase,
    effect: PersistedEffect,
}

fn persisted_effect(event: &Value) -> Option<PersistedEffect> {
    match event
        .get("effect")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("read" | "readonly" | "read_only") => Some(PersistedEffect::Read),
        Some("write") => Some(PersistedEffect::Write),
        Some("unknown") => Some(PersistedEffect::Unknown),
        _ => None,
    }
}

fn persisted_calls(events: &[Value]) -> BTreeMap<String, PersistedCall> {
    let mut calls = BTreeMap::<String, PersistedCall>::new();
    for event in events {
        let Some(call_id) = event.get("tool_call_id").and_then(Value::as_str) else {
            continue;
        };
        let lifecycle = event
            .get("lifecycle")
            .and_then(Value::as_str)
            .or_else(|| event.get("dispatch_state").and_then(Value::as_str));
        let phase = match lifecycle {
            Some("published") => PersistedPhase::Published,
            Some("awaiting_approval") => PersistedPhase::AwaitingApproval,
            Some("dispatched") => PersistedPhase::Dispatched,
            Some("terminal") => PersistedPhase::Terminal,
            _ => continue,
        };
        let effect = persisted_effect(event).unwrap_or(PersistedEffect::Unknown);
        let current = calls
            .entry(call_id.to_string())
            .or_insert(PersistedCall { phase, effect });
        if current.phase != PersistedPhase::Terminal {
            current.phase = phase;
            if persisted_effect(event).is_some() {
                current.effect = effect;
            }
        }
    }
    calls
}

/// Reconstructs every published tool call that lacks one durable terminal
/// result. This is intentionally pure so Runtime restart, live interrupt, and
/// terminal admission apply the same effect-aware recovery decision.
pub(crate) fn unclosed_tool_calls(events: &[Value]) -> Vec<RecoveredToolCall> {
    persisted_calls(events)
        .into_iter()
        .filter_map(|(call_id, call)| {
            let (result_status, detail) = match (call.phase, call.effect) {
                (PersistedPhase::Terminal, _) => return None,
                (PersistedPhase::Published | PersistedPhase::AwaitingApproval, _) => {
                    ("cancelled", "not_dispatched")
                }
                (PersistedPhase::Dispatched, PersistedEffect::Read) => {
                    ("timeout", "dispatch_interrupted")
                }
                (PersistedPhase::Dispatched, PersistedEffect::Write | PersistedEffect::Unknown) => {
                    ("side_effect_unknown", "dispatch_effect_unknown")
                }
            };
            Some(RecoveredToolCall {
                call_id,
                result_status,
                detail,
            })
        })
        .collect()
}

fn terminal_outcome(
    outcome: ToolCallOutcome,
    persisted: Option<PersistedCall>,
) -> (&'static str, &'static str) {
    match outcome {
        ToolCallOutcome::Completed { success: true } => ("succeeded", "completed"),
        ToolCallOutcome::Completed { success: false } => ("failed", "completed_failed"),
        ToolCallOutcome::Blocked => ("cancelled", "blocked_before_handler"),
        ToolCallOutcome::Failed {
            handler_executed: true,
        } if persisted
            .as_ref()
            .is_some_and(|call| call.effect == PersistedEffect::Read) =>
        {
            ("failed", "read_handler_failed")
        }
        ToolCallOutcome::Failed {
            handler_executed: true,
        } => ("side_effect_unknown", "handler_failed_after_dispatch"),
        ToolCallOutcome::Failed {
            handler_executed: false,
        } => ("failed", "handler_not_executed"),
        ToolCallOutcome::Aborted
            if persisted.as_ref().is_some_and(|call| {
                call.phase == PersistedPhase::Dispatched && call.effect != PersistedEffect::Read
            }) =>
        {
            ("side_effect_unknown", "dispatch_interrupted")
        }
        ToolCallOutcome::Aborted => ("cancelled", "aborted"),
    }
}

fn command_argument(payload: &ToolPayload) -> Option<String> {
    let raw = payload.log_payload();
    serde_json::from_str::<Value>(raw.as_ref())
        .ok()?
        .get("cmd")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn is_internal_agent_control_tool(tool_name: &str) -> bool {
    let unnamespaced = matches!(
        tool_name,
        "spawn_agent"
            | "spawn_subagent"
            | "steer"
            | "wait"
            | "interrupt"
            | "send_input"
            | "close_agent"
            | "resume_agent"
            | "send_message"
            | "followup_task"
            | "wait_agent"
            | "list_agents"
            | "interrupt_agent"
            | "update_plan"
    );
    let collaboration_v2 = matches!(
        tool_name,
        "collaborationspawn_agent"
            | "collaborationsend_message"
            | "collaborationfollowup_task"
            | "collaborationwait_agent"
            | "collaborationlist_agents"
            | "collaborationinterrupt_agent"
    );
    let collaboration_v1 = matches!(
        tool_name,
        "multi_agent_v1spawn_agent"
            | "multi_agent_v1send_input"
            | "multi_agent_v1wait"
            | "multi_agent_v1close_agent"
            | "multi_agent_v1resume_agent"
    );
    unnamespaced || collaboration_v2 || collaboration_v1
}

impl ThreadLifecycleContributor<Config> for PlatformLifecycleContributor {
    fn resolve_host_thread_startup<'a>(
        &'a self,
        thread_id: Option<ThreadId>,
        parent_thread_id: Option<ThreadId>,
        forked_from_thread_id: Option<ThreadId>,
    ) -> ExtensionFuture<
        'a,
        Result<Option<codex_extension_api::HostThreadStartupData>, ToolDispatchError>,
    > {
        Box::pin(async move {
            self.store
                .resolve_host_startup(thread_id, parent_thread_id, forked_from_thread_id)
                .await
                .map(Some)
                .map_err(|_| ToolDispatchError {
                    code: "AI_PLATFORM_AGENT_RUNTIME_STARTUP_AUTHORITY_UNAVAILABLE".to_string(),
                    message: "authenticated thread startup authority could not be restored"
                        .to_string(),
                })
        })
    }

    fn on_thread_resume<'a>(&'a self, input: ThreadResumeInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Ok(thread_id) = ThreadId::from_string(input.thread_store.level_id()) else {
                return;
            };
            // A process kill can happen before the previous turn's abort hook
            // runs. Revisit every durable turn receipt on resume and close
            // every published call with the phase/effect-specific terminal.
            let Ok(turn_ids) = self.store.read_platform_lifecycle_turn_ids(thread_id).await else {
                return;
            };
            for turn_id in turn_ids {
                let durable:bool=sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assistant_runtime_execution_owners WHERE run_id::text=$1)")
                    .bind(&turn_id).fetch_one(&self.store.pool).await.unwrap_or(true);
                if durable {
                    continue;
                }
                self.recover_unclosed(thread_id, &turn_id).await;
            }
        })
    }
}

impl ToolLifecycleContributor for PlatformLifecycleContributor {
    fn before_tool_dispatch<'a>(
        &'a self,
        input: codex_extension_api::ToolStartInput<'a>,
    ) -> codex_extension_api::ToolDispatchFuture<'a> {
        Box::pin(async move {
            let tool_name = input.tool_name.to_string();
            let mut digest = Sha256::new();
            digest.update(input.payload.log_payload().as_bytes());
            let arguments_sha256 = format!("{:x}", digest.finalize());
            let thread_id = ThreadId::from_string(input.thread_store.level_id()).map_err(|_| {
                ToolDispatchError {
                    code: "AI_PLATFORM_AGENT_RUNTIME_SCOPE_INVALID".to_string(),
                    message: "tool scope is invalid".to_string(),
                }
            })?;
            self.store
                .check_write_health(thread_id)
                .map_err(|_| ToolDispatchError {
                    code: "AI_PLATFORM_AGENT_RUNTIME_PERSISTENCE_FAILED".to_string(),
                    message: "tool dispatch blocked after thread persistence failure".to_string(),
                })?;
            if self
                .is_dynamic_capability(thread_id, input.tool_name)
                .await?
            {
                return Ok(());
            }
            self.append(PlatformLifecycleEvent {
                kernel_thread_id: thread_id,
                turn_id: input.turn_id.to_string(),
                item_id: Some(input.call_id.to_string()),
                event_key: format!("tool-use/{}/{}", input.turn_id, input.call_id),
                item_type: "tool_use".to_string(),
                status: "published".to_string(),
                payload: json!({
                    "schema_version": "agent-runtime-tool-lifecycle/v1",
                    "turn_id": input.turn_id,
                    "tool_call_id": input.call_id,
                    "tool_name": tool_name,
                    "tool_namespace": input.tool_name.namespace,
                    "originating_item_id": input.originating_item_id,
                    "root_turn_id": input.root_turn_id,
                    "arguments_sha256": arguments_sha256,
                    "lifecycle": "published",
                    "dispatch_state": "published",
                    "effect": format!("{:?}", input.effect),
                }),
            })
            .await?;
            if matches!(input.effect, ToolEffect::Write | ToolEffect::Unknown)
                && !is_internal_agent_control_tool(&tool_name)
            {
                let run_id =
                    uuid::Uuid::parse_str(input.turn_id).map_err(|_| ToolDispatchError {
                        code: "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_REQUIRED".to_string(),
                        message: "write-capable tool requires an approved runtime action"
                            .to_string(),
                    })?;
                let identity = self
                    .store
                    .identity_for_kernel_thread(thread_id)
                    .await
                    .map_err(|_| ToolDispatchError {
                        code: "AI_PLATFORM_AGENT_RUNTIME_SCOPE_INVALID".to_string(),
                        message: "tool scope is unavailable".to_string(),
                    })?;
                let tool_kind = match tool_name.as_str() {
                    "shell" | "exec_command" | "shell_command" => "command_execution",
                    "apply_patch" => "file_change",
                    other => other,
                };
                let expected_command = if tool_kind == "command_execution" {
                    Some(command_argument(input.payload).ok_or_else(|| {
                        ToolDispatchError {
                            code: "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_REQUIRED".to_string(),
                            message: "write-capable tool arguments could not be bound to approval"
                                .to_string(),
                        }
                    })?)
                } else {
                    None
                };
                let claimed = sqlx::query(
                    "UPDATE assistant_tool_approvals SET status='consumed', approved_at=COALESCE(approved_at, NOW()) WHERE approval_id IN (SELECT approval_id FROM assistant_tool_approvals WHERE run_id=$1 AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND tool_call_id=$6 AND tool_name=$5 AND arguments->>'itemId'=$6 AND ($7::text IS NULL OR arguments->>'command'=$7) AND status='approved' AND expires_at > NOW() ORDER BY approved_at NULLS LAST, approval_id LIMIT 1) AND status='approved' RETURNING approval_id",
                )
                .bind(run_id)
                .bind(identity.tenant_id)
                .bind(identity.user_id)
                .bind(identity.session_id)
                .bind(tool_kind)
                .bind(input.call_id)
                .bind(expected_command)
                .fetch_optional(&self.store.pool)
                .await
                .map_err(|_| ToolDispatchError {
                    code: "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_UNAVAILABLE".to_string(),
                    message: "tool dispatch approval could not be verified".to_string(),
                })?;
                if claimed.is_none() {
                    return Err(ToolDispatchError {
                        code: "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_REQUIRED".to_string(),
                        message: "write-capable tool requires an approved runtime action"
                            .to_string(),
                    });
                }
            }
            self.append(PlatformLifecycleEvent {
                kernel_thread_id: thread_id,
                turn_id: input.turn_id.to_string(),
                item_id: Some(input.call_id.to_string()),
                event_key: format!("tool-dispatch/{}/{}", input.turn_id, input.call_id),
                item_type: "tool_use".to_string(),
                status: "dispatched".to_string(),
                payload: json!({
                    "schema_version": "agent-runtime-tool-lifecycle/v1",
                    "turn_id": input.turn_id,
                    "tool_call_id": input.call_id,
                    "tool_name": input.tool_name.to_string(),
                    "tool_namespace": input.tool_name.namespace,
                    "originating_item_id": input.originating_item_id,
                    "root_turn_id": input.root_turn_id,
                    "arguments_sha256": arguments_sha256,
                    "lifecycle": "dispatched",
                    "dispatch_state": "dispatched",
                    "effect": format!("{:?}", input.effect),
                }),
            })
            .await
        })
    }

    fn on_tool_start<'a>(&'a self, _input: ToolStartInput<'a>) -> ToolLifecycleFuture<'a> {
        Box::pin(async {})
    }

    fn on_command_start<'a>(&'a self, input: CommandStartInput<'a>) -> ToolLifecycleFuture<'a> {
        Box::pin(async move {
            let Ok(thread_id) = ThreadId::from_string(input.thread_store.level_id()) else {
                return;
            };
            // Hash the final executor-qualified command without reading the
            // executor's paths on the Runtime host or persisting plaintext argv.
            let Ok(command) = serde_json::to_vec(input.command) else {
                return;
            };
            let argv_sha256 = format!("{:x}", Sha256::digest(command));
            let cwd_sha256 = format!("{:x}", Sha256::digest(input.cwd.to_string().as_bytes()));
            let _ = self
                .append(PlatformLifecycleEvent {
                    kernel_thread_id: thread_id,
                    turn_id: input.turn_id.to_string(),
                    item_id: Some(input.call_id.to_string()),
                    event_key: format!("command-executor/{}/{}", input.turn_id, input.call_id),
                    item_type: "command_executor".to_string(),
                    status: "prepared".to_string(),
                    payload: json!({
                        "schema_version": "agent-runtime-command-executor/v1",
                        "turn_id": input.turn_id,
                        "tool_call_id": input.call_id,
                        "argv_sha256": argv_sha256,
                        "executor_cwd_sha256": cwd_sha256,
                    }),
                })
                .await;
        })
    }

    fn on_tool_finish<'a>(&'a self, input: ToolFinishInput<'a>) -> ToolLifecycleFuture<'a> {
        Box::pin(async move {
            let Ok(thread_id) = ThreadId::from_string(input.thread_store.level_id()) else {
                return;
            };
            match self.is_dynamic_capability(thread_id, input.tool_name).await {
                Ok(false) => {}
                Ok(true) | Err(_) => return,
            }
            let persisted = if matches!(
                input.outcome,
                ToolCallOutcome::Failed {
                    handler_executed: true
                } | ToolCallOutcome::Aborted
            ) {
                self.store
                    .read_platform_lifecycle_events(thread_id, input.turn_id)
                    .await
                    .ok()
                    .and_then(|events| persisted_calls(&events).get(input.call_id).copied())
            } else {
                None
            };
            let (result_status, detail) = terminal_outcome(input.outcome, persisted);
            let payload = json!({
                "schema_version": "agent-runtime-tool-lifecycle/v1",
                "turn_id": input.turn_id,
                "tool_call_id": input.call_id,
                "tool_name": input.tool_name.to_string(),
                "lifecycle": "terminal",
                "result_status": result_status,
                "detail": detail,
            });
            let _ = self
                .append(PlatformLifecycleEvent {
                    kernel_thread_id: thread_id,
                    turn_id: input.turn_id.to_string(),
                    item_id: Some(input.call_id.to_string()),
                    event_key: format!("tool-result/{}/{}", input.turn_id, input.call_id),
                    item_type: "tool_result".to_string(),
                    status: result_status.to_string(),
                    payload,
                })
                .await;
        })
    }
}

impl TurnLifecycleContributor for PlatformLifecycleContributor {
    fn on_turn_abort<'a>(&'a self, input: TurnAbortInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Ok(thread_id) = ThreadId::from_string(input.thread_store.level_id()) else {
                return;
            };
            self.recover_unclosed(thread_id, input.turn_store.level_id())
                .await;
        })
    }

    fn on_turn_stop<'a>(&'a self, input: TurnStopInput<'a>) -> ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let Ok(thread_id) = ThreadId::from_string(input.thread_store.level_id()) else {
                return;
            };
            self.recover_unclosed(thread_id, input.turn_store.level_id())
                .await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::PersistedEffect;
    use super::PersistedPhase;
    use super::RecoveredToolCall;
    use super::command_argument;
    use super::is_internal_agent_control_tool;
    use super::persisted_calls;
    use super::terminal_outcome;
    use super::unclosed_tool_calls;
    use codex_extension_api::ToolCallOutcome;
    use codex_tools::ToolPayload;
    use serde_json::json;

    #[test]
    fn lifecycle_owner_matches_nested_and_legacy_dynamic_namespaces() {
        for raw in [
            vec![
                json!({"type":"namespace", "name":"alpha", "description":"", "tools":[
                    {"type":"function", "name":"lookup", "description":"", "inputSchema":{}}
                ]}),
            ],
            vec![json!({"namespace":"alpha", "name":"lookup", "description":"", "inputSchema":{}})],
        ] {
            let catalog =
                codex_protocol::dynamic_tools::normalize_dynamic_tool_specs(raw).expect("catalog");
            assert!(super::dynamic_catalog_contains(
                &catalog,
                &codex_extension_api::ToolName::namespaced("alpha", "lookup")
            ));
            assert!(!super::dynamic_catalog_contains(
                &catalog,
                &codex_extension_api::ToolName::namespaced("beta", "lookup")
            ));
            assert!(!super::dynamic_catalog_contains(
                &catalog,
                &codex_extension_api::ToolName::plain("lookup")
            ));
        }
    }

    #[test]
    fn restart_recovery_classifies_every_unpaired_published_call() {
        let events = vec![
            json!({"tool_call_id":"read-1", "lifecycle":"published", "effect":"read"}),
            json!({"tool_call_id":"read-1", "lifecycle":"dispatched", "effect":"read"}),
            json!({"tool_call_id":"read-1", "lifecycle":"terminal"}),
            json!({"tool_call_id":"write-1", "lifecycle":"published", "effect":"write"}),
            json!({"tool_call_id":"write-1", "dispatch_state":"dispatched", "effect":"write"}),
            json!({"tool_call_id":"pending", "lifecycle":"published", "effect":"read"}),
            json!({"tool_call_id":"approval", "lifecycle":"awaiting_approval", "effect":"unknown"}),
            json!({"tool_call_id":"read-2", "lifecycle":"dispatched", "effect":"Read"}),
        ];
        assert_eq!(
            unclosed_tool_calls(&events),
            vec![
                RecoveredToolCall {
                    call_id: "approval".to_string(),
                    result_status: "cancelled",
                    detail: "not_dispatched",
                },
                RecoveredToolCall {
                    call_id: "pending".to_string(),
                    result_status: "cancelled",
                    detail: "not_dispatched",
                },
                RecoveredToolCall {
                    call_id: "read-2".to_string(),
                    result_status: "timeout",
                    detail: "dispatch_interrupted",
                },
                RecoveredToolCall {
                    call_id: "write-1".to_string(),
                    result_status: "side_effect_unknown",
                    detail: "dispatch_effect_unknown",
                },
            ]
        );
    }

    #[test]
    fn persisted_call_reducer_preserves_effect_and_terminal_fence() {
        let calls = persisted_calls(&[
            json!({"tool_call_id":"read", "lifecycle":"published", "effect":"ReadOnly"}),
            json!({"tool_call_id":"read", "lifecycle":"dispatched"}),
            json!({"tool_call_id":"read", "lifecycle":"terminal"}),
            json!({"tool_call_id":"read", "lifecycle":"dispatched", "effect":"write"}),
        ]);
        let read = calls.get("read").expect("persisted call");
        assert_eq!(read.phase, PersistedPhase::Terminal);
        assert_eq!(read.effect, PersistedEffect::Read);
    }

    #[test]
    fn abnormal_terminal_status_respects_dispatch_effect() {
        let read = Some(super::PersistedCall {
            phase: PersistedPhase::Dispatched,
            effect: PersistedEffect::Read,
        });
        let write = Some(super::PersistedCall {
            phase: PersistedPhase::Dispatched,
            effect: PersistedEffect::Write,
        });
        assert_eq!(
            terminal_outcome(
                ToolCallOutcome::Failed {
                    handler_executed: true,
                },
                read,
            ),
            ("failed", "read_handler_failed")
        );
        assert_eq!(
            terminal_outcome(ToolCallOutcome::Aborted, write),
            ("side_effect_unknown", "dispatch_interrupted")
        );
        assert_eq!(
            terminal_outcome(
                ToolCallOutcome::Failed {
                    handler_executed: false,
                },
                None,
            ),
            ("failed", "handler_not_executed")
        );
    }

    #[test]
    fn approval_binding_extracts_the_exact_exec_command_argument() {
        let payload = ToolPayload::Function {
            arguments: r#"{"cmd":"rm -rf /tmp/example"}"#.to_string(),
        };
        assert_eq!(
            command_argument(&payload).as_deref(),
            Some("rm -rf /tmp/example")
        );
        let malformed = ToolPayload::Function {
            arguments: "not-json".to_string(),
        };
        assert!(command_argument(&malformed).is_none());
    }

    #[test]
    fn native_agent_control_tools_do_not_require_external_action_approval() {
        for name in [
            "spawn_agent",
            "spawn_subagent",
            "steer",
            "wait",
            "interrupt",
            "collaborationspawn_agent",
            "send_message",
            "wait_agent",
            "interrupt_agent",
            "update_plan",
        ] {
            assert!(is_internal_agent_control_tool(name));
        }
        for name in [
            "exec_command",
            "apply_patch",
            "skillsread",
            "unknown_external_tool",
        ] {
            assert!(!is_internal_agent_control_tool(name));
        }
    }
}
