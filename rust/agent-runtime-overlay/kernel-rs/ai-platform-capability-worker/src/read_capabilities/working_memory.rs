//! Working-memory envelope validation and markdown rendering for `todo_read`.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::{
    MAX_WORKING_MEMORY_BYTES, MAX_WORKING_MEMORY_DESCRIPTION_BYTES, MAX_WORKING_MEMORY_GOAL_BYTES,
    MAX_WORKING_MEMORY_INFO_KEY_BYTES, MAX_WORKING_MEMORY_INFO_SOURCE_BYTES,
    MAX_WORKING_MEMORY_INFO_VALUE_BYTES, MAX_WORKING_MEMORY_RESULT_BYTES,
    MAX_WORKING_MEMORY_TASK_ID_BYTES, MAX_WORKING_MEMORY_TASKS, ReadCapabilityContext,
    ReadCapabilityError, WORKING_MEMORY_SCHEMA_VERSION, working_memory_key,
};

pub(super) fn todo_read_empty() -> Value {
    json!({
        "markdown": "(no tasks)",
        "task_count": 0,
        "progress": {
            "total": 0,
            "completed": 0,
            "failed": 0,
            "percentage": 0
        }
    })
}

pub(crate) fn render_working_memory(
    envelope: &Value,
    context: &ReadCapabilityContext,
) -> Result<Value, ReadCapabilityError> {
    if serde_json::to_string(envelope)
        .map_err(|_| ReadCapabilityError::WorkingMemoryInvalid)?
        .chars()
        .count()
        > MAX_WORKING_MEMORY_BYTES
    {
        return Err(ReadCapabilityError::WorkingMemoryInvalid);
    }
    let envelope = envelope
        .as_object()
        .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
    require_exact_keys(
        envelope,
        &["schema_version", "owner_scope", "working_memory"],
    )?;
    if envelope.get("schema_version").and_then(Value::as_str) != Some(WORKING_MEMORY_SCHEMA_VERSION)
    {
        return Err(ReadCapabilityError::WorkingMemoryInvalid);
    }
    let expected_scope =
        working_memory_scope(&context.tenant_id, &context.user_id, &context.session_id);
    if envelope.get("owner_scope").and_then(Value::as_str) != Some(expected_scope.as_str()) {
        return Err(ReadCapabilityError::WorkingMemoryInvalid);
    }
    let memory = envelope
        .get("working_memory")
        .and_then(Value::as_object)
        .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
    validate_working_memory(memory, &context.session_id)?;
    let tasks = memory
        .get("tasks")
        .and_then(Value::as_array)
        .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
    if tasks.is_empty() {
        return Ok(todo_read_empty());
    }

    let mut completed = 0usize;
    let mut failed = 0usize;
    let mut markdown = String::from("# Current Task State\n\n");
    if let Some(goal) = memory.get("goal").and_then(Value::as_str)
        && !goal.is_empty()
    {
        markdown.push_str("**Goal:** ");
        markdown.push_str(&markdown_inline(goal));
        markdown.push_str("\n\n");
    }
    markdown.push_str("## Tasks\n");
    for task in tasks {
        let task = task
            .as_object()
            .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
        let status = task
            .get("status")
            .and_then(Value::as_str)
            .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
        let description = task
            .get("description")
            .and_then(Value::as_str)
            .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
        let indicator = match status {
            "pending" => "[ ]",
            "in_progress" => "[~]",
            "completed" => {
                completed += 1;
                "[x]"
            }
            "failed" => {
                failed += 1;
                "[!]"
            }
            "blocked" => "[B]",
            _ => return Err(ReadCapabilityError::WorkingMemoryInvalid),
        };
        markdown.push_str("- ");
        markdown.push_str(indicator);
        markdown.push(' ');
        markdown.push_str(&markdown_inline(description));
        if status == "in_progress" {
            markdown.push_str(" <- current");
        }
        if let Some(error) = task.get("error").and_then(Value::as_str)
            && !error.is_empty()
        {
            markdown.push_str(" (error: ");
            markdown.push_str(&markdown_inline(error));
            markdown.push(')');
        }
        markdown.push('\n');
    }
    markdown.push('\n');

    if let Some(information) = memory.get("collected_info").and_then(Value::as_array)
        && !information.is_empty()
    {
        markdown.push_str("## Collected Information\n");
        for item in information {
            let item = item
                .as_object()
                .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
            let key = item
                .get("key")
                .and_then(Value::as_str)
                .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
            let value = item
                .get("value")
                .and_then(Value::as_str)
                .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
            let display = if value.chars().count() > 100 {
                format!("{}...", value.chars().take(100).collect::<String>())
            } else {
                value.to_string()
            };
            markdown.push_str("- **");
            markdown.push_str(&markdown_inline(key));
            markdown.push_str("**: ");
            markdown.push_str(&markdown_inline(&display));
            markdown.push('\n');
        }
        markdown.push('\n');
    }
    if let Some(notes) = memory.get("notes").and_then(Value::as_array)
        && !notes.is_empty()
    {
        markdown.push_str("## Notes\n");
        for note in notes {
            markdown.push_str("- ");
            markdown.push_str(&markdown_inline(
                note.as_str()
                    .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?,
            ));
            markdown.push('\n');
        }
        markdown.push('\n');
    }
    let total = tasks.len();
    Ok(json!({
        "markdown": markdown.trim_end_matches('\n'),
        "task_count": total,
        "progress": {
            "total": total,
            "completed": completed,
            "failed": failed,
            "percentage": (completed as f64 / total as f64) * 100.0
        }
    }))
}

pub(crate) fn validate_working_memory(
    memory: &serde_json::Map<String, Value>,
    session_id: &str,
) -> Result<(), ReadCapabilityError> {
    const KEYS: &[&str] = &[
        "session_id",
        "goal",
        "goal_set_at",
        "turns_since_goal",
        "tasks",
        "collected_info",
        "notes",
        "archived",
    ];
    if memory.keys().any(|key| !KEYS.contains(&key.as_str()))
        || memory.get("session_id").and_then(Value::as_str) != Some(session_id)
    {
        return Err(ReadCapabilityError::WorkingMemoryInvalid);
    }
    if let Some(goal) = memory.get("goal")
        && !goal.is_null()
        && !goal
            .as_str()
            .is_some_and(|value| valid_memory_text(value, 1, MAX_WORKING_MEMORY_GOAL_BYTES))
    {
        return Err(ReadCapabilityError::WorkingMemoryInvalid);
    }
    for field in ["goal_set_at"] {
        if let Some(value) = memory.get(field)
            && !value.is_null()
            && value.as_str().is_none_or(|text| !valid_text(text, 1, 128))
        {
            return Err(ReadCapabilityError::WorkingMemoryInvalid);
        }
    }
    if let Some(turns) = memory.get("turns_since_goal")
        && turns.as_u64().is_none_or(|value| value > 1_000_000)
    {
        return Err(ReadCapabilityError::WorkingMemoryInvalid);
    }
    let tasks = memory
        .get("tasks")
        .and_then(Value::as_array)
        .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
    if tasks.len() > MAX_WORKING_MEMORY_TASKS {
        return Err(ReadCapabilityError::WorkingMemoryInvalid);
    }
    let mut ids = BTreeSet::new();
    for task in tasks {
        let task = task
            .as_object()
            .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
        if task.keys().any(|key| {
            ![
                "id",
                "description",
                "status",
                "result",
                "error",
                "created_at",
                "completed_at",
            ]
            .contains(&key.as_str())
        }) {
            return Err(ReadCapabilityError::WorkingMemoryInvalid);
        }
        let id = task
            .get("id")
            .and_then(Value::as_str)
            .filter(|value| valid_text(value, 1, MAX_WORKING_MEMORY_TASK_ID_BYTES))
            .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
        if !ids.insert(id.to_string()) {
            return Err(ReadCapabilityError::WorkingMemoryInvalid);
        }
        if task
            .get("description")
            .and_then(Value::as_str)
            .is_none_or(|value| !valid_memory_text(value, 1, MAX_WORKING_MEMORY_DESCRIPTION_BYTES))
        {
            return Err(ReadCapabilityError::WorkingMemoryInvalid);
        }
        if task
            .get("status")
            .and_then(Value::as_str)
            .is_none_or(|value| {
                !matches!(
                    value,
                    "pending" | "in_progress" | "completed" | "failed" | "blocked"
                )
            })
        {
            return Err(ReadCapabilityError::WorkingMemoryInvalid);
        }
        for field in ["result", "error"] {
            if let Some(value) = task.get(field)
                && !value.is_null()
                && value
                    .as_str()
                    .is_none_or(|text| !valid_memory_text(text, 1, MAX_WORKING_MEMORY_RESULT_BYTES))
            {
                return Err(ReadCapabilityError::WorkingMemoryInvalid);
            }
        }
        for field in ["created_at", "completed_at"] {
            if let Some(value) = task.get(field)
                && !value.is_null()
                && value.as_str().is_none_or(|text| !valid_text(text, 1, 128))
            {
                return Err(ReadCapabilityError::WorkingMemoryInvalid);
            }
        }
    }
    if let Some(information) = memory.get("collected_info") {
        let information = information
            .as_array()
            .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
        if information.len() > MAX_WORKING_MEMORY_TASKS {
            return Err(ReadCapabilityError::WorkingMemoryInvalid);
        }
        for item in information {
            let item = item
                .as_object()
                .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
            if item
                .keys()
                .any(|key| !["key", "value", "source", "timestamp"].contains(&key.as_str()))
            {
                return Err(ReadCapabilityError::WorkingMemoryInvalid);
            }
            for (field, max) in [
                ("key", MAX_WORKING_MEMORY_INFO_KEY_BYTES),
                ("value", MAX_WORKING_MEMORY_INFO_VALUE_BYTES),
                ("source", MAX_WORKING_MEMORY_INFO_SOURCE_BYTES),
                ("timestamp", 128),
            ] {
                let validator: fn(&str, usize, usize) -> bool = if field == "value" {
                    valid_memory_text
                } else {
                    valid_text
                };
                if item
                    .get(field)
                    .and_then(Value::as_str)
                    .is_none_or(|text| !validator(text, 1, max))
                {
                    return Err(ReadCapabilityError::WorkingMemoryInvalid);
                }
            }
        }
    }
    if let Some(notes) = memory.get("notes") {
        let notes = notes
            .as_array()
            .ok_or(ReadCapabilityError::WorkingMemoryInvalid)?;
        if notes.len() > MAX_WORKING_MEMORY_TASKS
            || notes.iter().any(|note| {
                note.as_str().is_none_or(|text| {
                    !valid_memory_text(text, 1, MAX_WORKING_MEMORY_DESCRIPTION_BYTES)
                })
            })
        {
            return Err(ReadCapabilityError::WorkingMemoryInvalid);
        }
    }
    if let Some(archived) = memory.get("archived")
        && !archived.is_null()
        && !archived.is_object()
    {
        return Err(ReadCapabilityError::WorkingMemoryInvalid);
    }
    Ok(())
}

fn require_exact_keys(
    object: &serde_json::Map<String, Value>,
    required: &[&str],
) -> Result<(), ReadCapabilityError> {
    if object.len() != required.len() || required.iter().any(|key| !object.contains_key(*key)) {
        return Err(ReadCapabilityError::WorkingMemoryInvalid);
    }
    Ok(())
}

fn valid_text(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.chars().count())
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn valid_memory_text(value: &str, min: usize, max: usize) -> bool {
    (min..=max).contains(&value.chars().count())
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_control() && byte != b'\n')
}

fn markdown_inline(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn working_memory_scope(tenant_id: &str, user_id: &str, session_id: &str) -> String {
    working_memory_key(tenant_id, user_id, session_id)
        .strip_prefix("working_memory:")
        .unwrap_or_default()
        .to_string()
}
