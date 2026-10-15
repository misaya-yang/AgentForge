//! Canonical DynamicToolSpec projection for the pinned Codex harness.
//! Namespace identity is catalog metadata, never inferred from the tool source.
use super::{AgentError, invalid};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

type ToolIdentity = (Option<String>, String);

fn identifier(value: &str) -> bool {
    // The current model-plane provider contract supports 64-byte identifiers.
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        && value != "mcp"
        && !value.starts_with("mcp__")
}

fn namespace(descriptor: &Value) -> Result<Option<String>, AgentError> {
    let metadata = &descriptor["metadata"];
    if !metadata.is_null() && !metadata.is_object() {
        return Err(invalid());
    }
    let Some(value) = metadata.get("namespace").filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let value = value.as_str().ok_or_else(invalid)?;
    if value.is_empty() || value == "functions" {
        return Ok(None);
    }
    // These names belong to upstream or platform-owned native tools.
    if !identifier(value)
        || [
            "api_tool",
            "browser",
            "computer",
            "container",
            "file_search",
            "image_gen",
            "multi_tool_use",
            "python",
            "python_user_visible",
            "submodel_delegator",
            "terminal",
            "tool_search",
            "web",
            "collaboration",
            "multi_agent_v1",
        ]
        .contains(&value)
    {
        return Err(invalid());
    }
    Ok(Some(value.to_owned()))
}

fn selected_functions(readonly: &Value) -> Result<Vec<(ToolIdentity, Value)>, AgentError> {
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for key in ["tools", "mcp", "deferred", "attachment_tools"] {
        let Some(descriptors) = readonly.get(key).filter(|value| !value.is_null()) else {
            continue;
        };
        for descriptor in descriptors.as_array().ok_or_else(invalid)? {
            if !descriptor.is_object() {
                return Err(invalid());
            }
            if descriptor["read_only"] != true
                && readonly["capability_allowlist"]
                    .as_array()
                    .is_none_or(|allowlist| {
                        allowlist
                            .iter()
                            .filter(|entry| {
                                ["id", "name", "version", "schema_hash"]
                                    .iter()
                                    .all(|key| entry[*key] == descriptor[*key])
                            })
                            .count()
                            != 1
                    })
            {
                continue;
            }
            let name = descriptor["name"].as_str().ok_or_else(invalid)?;
            let description = descriptor["description"].as_str().ok_or_else(invalid)?;
            let namespace = namespace(descriptor)?;
            if !descriptor["schema"].is_object()
                || (namespace.is_some()
                    && (!identifier(name) || description.chars().count() > 16_384))
            {
                return Err(invalid());
            }
            // Preserve the existing flat catalog projection. Namespace-qualified
            // tools with the same leaf name are separate authorized capabilities.
            if namespace.is_none()
                && [
                    "tool_search",
                    "tool_describe",
                    "tool_call",
                    "context_compact",
                    "spawn_subagent",
                ]
                .contains(&name)
            {
                continue;
            }
            let identity = (namespace, name.to_owned());
            if !seen.insert(identity.clone()) || seen.len() > 256 {
                return Err(invalid());
            }
            result.push((
                identity,
                json!({"type":"function","name":name,
                "description":description,"inputSchema":descriptor["schema"]}),
            ));
        }
    }
    Ok(result)
}

pub fn dynamic_tools(readonly: &Value) -> Result<Vec<Value>, AgentError> {
    let mut result: Vec<Value> = Vec::new();
    let mut namespaces: BTreeMap<String, usize> = BTreeMap::new();
    for ((namespace, _), function) in selected_functions(readonly)? {
        let Some(namespace) = namespace else {
            result.push(function);
            continue;
        };
        if let Some(index) = namespaces.get(&namespace) {
            result[*index]["tools"]
                .as_array_mut()
                .expect("namespace tools")
                .push(function);
        } else {
            namespaces.insert(namespace.clone(), result.len());
            result.push(json!({"type":"namespace","name":namespace,
                "description":"","tools":[function]}));
        }
    }
    Ok(result)
}

pub fn tool_policy(
    readonly: &Value,
    native_search: bool,
    startup: bool,
    generic: bool,
) -> Result<Value, AgentError> {
    if !startup && readonly["responses_tool_choice"] == "none" {
        return Ok(json!({"allowedTools":[]}));
    }
    let mut ids: BTreeSet<ToolIdentity> = selected_functions(readonly)?
        .into_iter()
        .map(|(identity, _)| identity)
        .collect();
    if readonly["responses_tool_names"].is_null() {
        for name in [
            "update_plan",
            "spawn_agent",
            "spawn_subagent",
            "steer",
            "wait",
            "interrupt",
            "send_input",
            "close_agent",
            "resume_agent",
            "send_message",
            "followup_task",
            "wait_agent",
            "list_agents",
            "interrupt_agent",
        ] {
            ids.insert((None, name.into()));
        }
        for name in [
            "spawn_agent",
            "send_message",
            "followup_task",
            "wait_agent",
            "list_agents",
            "interrupt_agent",
        ] {
            ids.insert((Some("collaboration".into()), name.into()));
        }
        for name in [
            "spawn_agent",
            "send_input",
            "wait",
            "wait_agent",
            "close_agent",
            "resume_agent",
        ] {
            ids.insert((Some("multi_agent_v1".into()), name.into()));
        }
        if native_search {
            ids.insert((None, "web_search".into()));
        }
        if startup && generic {
            ids.insert((None, "read_attachment".into()));
        }
    }
    Ok(json!({"allowedTools":ids.into_iter().map(|(namespace,name)|
        json!({"namespace":namespace,"name":name})).collect::<Vec<_>>()}))
}
