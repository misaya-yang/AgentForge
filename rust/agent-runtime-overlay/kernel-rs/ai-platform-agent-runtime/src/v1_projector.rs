use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::TurnStatus;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

pub const ASSISTANT_TURN_CONTRACT_V1: &str = "assistant-turn-contract/v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct V1ProjectionContext {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct AssistantTurnEventV1 {
    pub schema_version: String,
    pub event_type: String,
    pub data: Value,
    pub timestamp: f64,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct SequencedAssistantTurnEventV1 {
    pub sequence: i64,
    #[serde(flatten)]
    pub event: AssistantTurnEventV1,
}

impl AssistantTurnEventV1 {
    pub(crate) fn new(event_type: &str, data: Value) -> Self {
        Self {
            schema_version: ASSISTANT_TURN_CONTRACT_V1.to_string(),
            event_type: event_type.to_string(),
            data,
            timestamp: chrono::Utc::now().timestamp_millis() as f64 / 1_000.0,
        }
    }
}

pub fn project_server_notification(
    notification: &ServerNotification,
    context: &V1ProjectionContext,
) -> Vec<AssistantTurnEventV1> {
    match notification {
        ServerNotification::TurnStarted(started) => vec![AssistantTurnEventV1::new(
            "run_started",
            lifecycle_data(
                context,
                &started.thread_id,
                &started.turn.id,
                "running",
                None,
            ),
        )],
        ServerNotification::AgentMessageDelta(delta) => vec![AssistantTurnEventV1::new(
            "text_delta",
            json!({
                "run_id": delta.turn_id,
                "session_id": context.session_id,
                "thread_id": delta.thread_id,
                "content": delta.delta,
            }),
        )],
        ServerNotification::ReasoningSummaryTextDelta(delta) => {
            vec![AssistantTurnEventV1::new(
                "thinking_delta",
                json!({
                    "run_id": delta.turn_id,
                    "session_id": context.session_id,
                    "thread_id": delta.thread_id,
                    "content": delta.delta,
                    "item_id": delta.item_id,
                    "summary_index": delta.summary_index,
                }),
            )]
        }
        ServerNotification::PlanDelta(delta) => vec![AssistantTurnEventV1::new(
            "plan_update",
            json!({
                "run_id": delta.turn_id,
                "session_id": context.session_id,
                "thread_id": delta.thread_id,
                "item_id": delta.item_id,
                "delta": delta.delta,
            }),
        )],
        ServerNotification::TurnPlanUpdated(plan) => vec![AssistantTurnEventV1::new(
            "plan_update",
            json!({
                "run_id": plan.turn_id,
                "session_id": context.session_id,
                "thread_id": plan.thread_id,
                "explanation": plan.explanation,
                "plan": plan.plan,
            }),
        )],
        ServerNotification::ContextCompacted(compacted) => vec![AssistantTurnEventV1::new(
            "context_compaction",
            json!({
                "run_id": compacted.turn_id,
                "session_id": context.session_id,
                "thread_id": compacted.thread_id,
                "compacted": true,
            }),
        )],
        // Raw reasoning is intentionally not a product event. Only provider-approved
        // reasoning summaries may reach the V1 compatibility stream.
        ServerNotification::ReasoningTextDelta(_) => Vec::new(),
        ServerNotification::ItemStarted(started) => project_tool_item(
            &started.item,
            &started.thread_id,
            &started.turn_id,
            context,
            ToolProjectionPhase::Started,
        ),
        ServerNotification::ItemCompleted(completed) => project_tool_item(
            &completed.item,
            &completed.thread_id,
            &completed.turn_id,
            context,
            ToolProjectionPhase::Completed,
        ),
        ServerNotification::TurnCompleted(completed) => {
            let (event_type, status) = match completed.turn.status {
                TurnStatus::Completed => ("run_finished", "succeeded"),
                TurnStatus::Interrupted => ("run_error", "cancelled"),
                TurnStatus::Failed => ("run_error", "failed"),
                TurnStatus::InProgress => return Vec::new(),
            };
            vec![AssistantTurnEventV1::new(
                event_type,
                lifecycle_data(
                    context,
                    &completed.thread_id,
                    &completed.turn.id,
                    status,
                    Some(completed.turn.duration_ms),
                ),
            )]
        }
        ServerNotification::Error(error) if !error.will_retry => vec![AssistantTurnEventV1::new(
            "error",
            json!({
                "run_id": error.turn_id,
                "session_id": context.session_id,
                "thread_id": error.thread_id,
                "message": "Agent runtime could not complete this request. Please try again.",
            }),
        )],
        _ => Vec::new(),
    }
}

pub fn server_notification_thread_id(notification: &ServerNotification) -> Option<&str> {
    match notification {
        ServerNotification::TurnStarted(value) => Some(&value.thread_id),
        ServerNotification::AgentMessageDelta(value) => Some(&value.thread_id),
        ServerNotification::ReasoningSummaryTextDelta(value) => Some(&value.thread_id),
        ServerNotification::PlanDelta(value) => Some(&value.thread_id),
        ServerNotification::TurnPlanUpdated(value) => Some(&value.thread_id),
        ServerNotification::ContextCompacted(value) => Some(&value.thread_id),
        ServerNotification::ReasoningTextDelta(value) => Some(&value.thread_id),
        ServerNotification::ItemStarted(value) => Some(&value.thread_id),
        ServerNotification::ItemCompleted(value) => Some(&value.thread_id),
        ServerNotification::TurnCompleted(value) => Some(&value.thread_id),
        ServerNotification::Error(value) => Some(&value.thread_id),
        _ => None,
    }
}

fn lifecycle_data(
    context: &V1ProjectionContext,
    thread_id: &str,
    run_id: &str,
    status: &str,
    duration_ms: Option<Option<i64>>,
) -> Value {
    let mut data = json!({
        "run_id": run_id,
        "tenant_id": context.tenant_id,
        "user_id": context.user_id,
        "session_id": context.session_id,
        "thread_id": thread_id,
        "status": status,
    });
    if let Some(Some(duration_ms)) = duration_ms
        && let Some(object) = data.as_object_mut()
    {
        object.insert("duration_ms".to_string(), duration_ms.into());
    }
    if matches!(status, "succeeded" | "failed" | "cancelled")
        && let Some(object) = data.as_object_mut()
    {
        object.insert("exit".to_string(), status.into());
        object.insert(
            "terminal_envelope".to_string(),
            json!({
                "schema_version": ASSISTANT_TURN_CONTRACT_V1,
                "run_id": run_id,
                "tenant_id": context.tenant_id,
                "session_id": context.session_id,
                "thread_id": thread_id,
                "status": status,
                "exit_reason": status,
            }),
        );
    }
    data
}

#[derive(Clone, Copy)]
enum ToolProjectionPhase {
    Started,
    Completed,
}

fn tool_terminal_failed(status: Option<&str>) -> bool {
    !matches!(
        status.map(str::to_ascii_lowercase).as_deref(),
        Some("completed" | "succeeded")
    )
}

fn project_tool_item(
    item: &ThreadItem,
    thread_id: &str,
    turn_id: &str,
    context: &V1ProjectionContext,
    phase: ToolProjectionPhase,
) -> Vec<AssistantTurnEventV1> {
    let mut events =
        project_collab_item(item, thread_id, turn_id, context, phase).unwrap_or_default();
    let Some(mut tool) = tool_data(item) else {
        return events;
    };
    tool.insert("run_id".to_string(), turn_id.into());
    tool.insert("session_id".to_string(), context.session_id.clone().into());
    tool.insert("thread_id".to_string(), thread_id.into());
    match phase {
        ToolProjectionPhase::Started => {
            tool.insert("status".to_string(), "started".into());
            events.push(AssistantTurnEventV1::new(
                "tool_call_start",
                Value::Object(tool),
            ));
        }
        ToolProjectionPhase::Completed => {
            let failed = tool_terminal_failed(tool.get("status").and_then(Value::as_str));
            tool.insert(
                "status".to_string(),
                if failed { "failed" } else { "completed" }.into(),
            );
            tool.insert("success".to_string(), (!failed).into());
            let data = Value::Object(tool);
            events.push(AssistantTurnEventV1::new("tool_call_result", data.clone()));
            events.push(AssistantTurnEventV1::new("tool_call_end", data));
        }
    }
    events
}

/// Project the kernel's collaboration item into the stable child-agent
/// lifecycle already consumed by the Assistant workbench. Receiver thread IDs
/// are the child identity; unlike the parent tool-call ID they remain stable
/// across reconnects and history replay.
fn project_collab_item(
    item: &ThreadItem,
    thread_id: &str,
    turn_id: &str,
    context: &V1ProjectionContext,
    phase: ToolProjectionPhase,
) -> Option<Vec<AssistantTurnEventV1>> {
    let value = serde_json::to_value(item).ok()?;
    let object = value.as_object()?;
    if object.get("type")?.as_str()? == "subAgentActivity" {
        if matches!(phase, ToolProjectionPhase::Started) {
            return Some(Vec::new());
        }
        let (event_type, status) = match object.get("kind")?.as_str()? {
            "started" => ("subagent_started", "running"),
            "completed" => ("subagent_finished", "completed"),
            "interrupted" => ("subagent_finished", "cancelled"),
            "interacted" => ("subagent_step", "running"),
            _ => return None,
        };
        return Some(vec![AssistantTurnEventV1::new(
            event_type,
            json!({
                "agent_id": object.get("agentThreadId")?,
                "agent_path": object.get("agentPath"),
                "agent_type": "task",
                "call_id": object.get("id")?,
                "parent_task_id": thread_id,
                "task_id": turn_id,
                "session_id": context.session_id,
                "thread_id": thread_id,
                "status": status,
            }),
        )]);
    }
    if object.get("type")?.as_str()? != "collabAgentToolCall" {
        return None;
    }
    let call_id = object.get("id")?.as_str()?;
    let tool = object
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("collaboration");
    let prompt = object.get("prompt").cloned().unwrap_or(Value::Null);
    let mut receiver_ids = object
        .get("receiverThreadIds")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if receiver_ids.is_empty() {
        receiver_ids = object
            .get("agentsStates")
            .and_then(Value::as_object)
            .map(|states| states.keys().cloned().collect())
            .unwrap_or_default();
    }
    // A spawn item without a receiver identity must remain a normal tool
    // event. Using the parent call ID here would create a phantom child that
    // cannot be correlated with the later receiver thread.
    if receiver_ids.is_empty() {
        return None;
    }
    let raw_status = object
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("failed");
    let status = match raw_status {
        "completed" => "completed",
        "inProgress" => "running",
        _ => "failed",
    };
    let agent_type = "task";
    Some(
        receiver_ids
            .into_iter()
            .flat_map(|agent_id| {
                let child_status = object.get("agentsStates")
                    .and_then(|states| states.get(&agent_id))
                    .and_then(|state| state.get("status"))
                    .and_then(Value::as_str);
                let child_terminal = matches!(child_status, Some("completed" | "errored" | "interrupted" | "shutdown"));
                let data = json!({
                    "agent_id": agent_id,
                    "agent_type": agent_type,
                    "description": prompt,
                    "call_id": call_id,
                    "parent_task_id": thread_id,
                    "task_id": turn_id,
                    "session_id": context.session_id,
                    "thread_id": thread_id,
                    "tool": tool,
                    "status": if tool == "spawnAgent" { child_status.unwrap_or("running") } else { status },
                });
                let event_type = match (tool, phase) {
                    ("spawnAgent", ToolProjectionPhase::Started) => "subagent_started",
                    ("spawnAgent", ToolProjectionPhase::Completed) if child_terminal => "subagent_finished",
                    // A successful spawn only means the child was accepted.
                    ("spawnAgent", ToolProjectionPhase::Completed) => "subagent_step",
                    (_, _) => "subagent_step",
                };
                let data = if tool == "spawnAgent" {
                    data
                } else {
                    let mut step = data;
                    step["step"] = prompt.clone();
                    step["status"] = if matches!(phase, ToolProjectionPhase::Started) {
                        "running".into()
                    } else if status == "failed" {
                        "failed".into()
                    } else {
                        "completed".into()
                    };
                    step
                };
                vec![AssistantTurnEventV1::new(event_type, data)]
            })
            .collect(),
    )
}

fn tool_data(item: &ThreadItem) -> Option<serde_json::Map<String, Value>> {
    let value = serde_json::to_value(item).ok()?;
    let object = value.as_object()?;
    let item_type = object.get("type")?.as_str()?;
    let tool_name = match item_type {
        "mcpToolCall" => format!(
            "{}.{}",
            object.get("server")?.as_str()?,
            object.get("tool")?.as_str()?
        ),
        "dynamicToolCall" => object.get("tool")?.as_str()?.to_string(),
        "commandExecution" => "shell".to_string(),
        "fileChange" => "apply_patch".to_string(),
        "collabAgentToolCall" => "collaboration".to_string(),
        "webSearch" => "web_search".to_string(),
        _ => return None,
    };
    let mut projected = serde_json::Map::new();
    let item_id = object.get("id")?.as_str()?;
    projected.insert("tool_call_id".to_string(), item_id.into());
    projected.insert("tool_name".to_string(), tool_name.into());
    if let Some(namespace) = object.get("namespace") {
        projected.insert("tool_namespace".to_string(), namespace.clone());
    }
    if let Some(arguments) = object.get("arguments") {
        projected.insert("arguments".to_string(), arguments.clone());
    }
    if let Some(status) = object.get("status") {
        projected.insert("status".to_string(), status.clone());
    } else if item_type == "webSearch" {
        // WebSearchItem has no status field. Receiving it through
        // ItemCompleted is the authoritative terminal-success signal; the
        // Started projection overwrites this value with `started` above.
        projected.insert("status".to_string(), "completed".into());
    }
    if let Some(duration_ms) = object.get("durationMs") {
        projected.insert("duration_ms".to_string(), duration_ms.clone());
    }
    Some(projected)
}

#[cfg(test)]
mod tests {
    use super::tool_terminal_failed;

    #[test]
    fn native_subagent_completion_is_separate_from_spawn_acceptance() {
        let item: codex_app_server_protocol::ThreadItem =
            serde_json::from_value(serde_json::json!({
                "type": "subAgentActivity", "id": "activity-1", "kind": "completed",
                "agentThreadId": "child-1", "agentPath": "/root/child",
            }))
            .expect("native child activity");
        let context = super::V1ProjectionContext {
            tenant_id: "tenant".to_string(),
            user_id: "user".to_string(),
            session_id: "session".to_string(),
        };
        let events = super::project_collab_item(
            &item,
            "root",
            "turn",
            &context,
            super::ToolProjectionPhase::Completed,
        )
        .expect("native activity projection");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "subagent_finished");
        assert_eq!(events[0].data["agent_id"], "child-1");
    }

    #[test]
    fn same_named_dynamic_tools_keep_distinct_namespaces() {
        for namespace in ["alpha", "beta"] {
            let item: codex_app_server_protocol::ThreadItem =
                serde_json::from_value(serde_json::json!({
                    "type": "dynamicToolCall", "id": "call-1", "namespace": namespace,
                    "tool": "lookup", "arguments": {}, "status": "completed",
                    "contentItems": [], "success": true, "durationMs": 1,
                }))
                .expect("target dynamic tool item");
            let projected = super::tool_data(&item).expect("tool projection");
            assert_eq!(projected["tool_name"], "lookup");
            assert_eq!(projected["tool_namespace"], namespace);
        }
    }

    #[test]
    fn non_successful_tool_statuses_are_projected_as_failures() {
        assert!(!tool_terminal_failed(Some("completed")));
        assert!(!tool_terminal_failed(Some("succeeded")));
        assert!(tool_terminal_failed(Some("failed")));
        assert!(tool_terminal_failed(Some("declined")));
        assert!(tool_terminal_failed(Some("cancelled")));
        assert!(tool_terminal_failed(None));
    }
}
