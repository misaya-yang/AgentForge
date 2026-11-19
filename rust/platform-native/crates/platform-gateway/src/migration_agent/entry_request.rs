//! The V1 Assistant schema defaults are intentionally distinct from V2 turns.
use super::{Actor, AgentError, AgentFuture, text};
use serde_json::{Value, json};
use std::collections::BTreeSet;
pub trait AgentRateLimit: Send + Sync {
    fn check<'a>(&'a self, actor: &'a Actor, operation: &'a str) -> AgentFuture<'a, ()>;
}
impl<T: crate::migration_content::ContentAccess> AgentRateLimit for T {
    fn check<'a>(&'a self, actor: &'a Actor, operation: &'a str) -> AgentFuture<'a, ()> {
        Box::pin(async move {
            let user = crate::migration_content::ContentIdentity {
                tenant_id: actor.tenant_id.clone(),
                user_id: actor.user_id.clone(),
                tier: actor.tier.clone(),
                roles: actor.roles.clone(),
                is_authenticated: true,
                ..Default::default()
            };
            self.rate_limit(&user, operation, None)
                .await
                .map_err(|e| AgentError {
                    status: e.status,
                    detail: e.detail,
                })
        })
    }
}
pub fn normalize_v1(raw: &Value) -> Result<Value, AgentError> {
    let invalid = || AgentError::code(422, "INVALID_ASSISTANT_REQUEST");
    if !raw.is_object() {
        return Err(invalid());
    }
    if raw["confirm_plan"] == true || raw["kb_include_images"] == true {
        return Err(invalid());
    }
    if raw["execution_profile"]
        .as_str()
        .is_some_and(|s| s != "safe")
        || raw["enable_task_planning"] == true
        || !raw["resume_run_id"].is_null()
        || !raw["resume_approval_id"].is_null()
    {
        return Err(AgentError::code(
            409,
            "AGENT_RUNTIME_CAPABILITY_NOT_MIGRATED",
        ));
    }
    for (k, max) in [("max_tokens", 32768), ("web_search_max_results", 10)] {
        if raw
            .get(k)
            .is_some_and(|v| !v.is_null() && v.as_i64().is_none_or(|v| v < 1 || v > max))
        {
            return Err(invalid());
        }
    }
    if raw["thinking_level"]
        .as_str()
        .is_some_and(|s| s.chars().count() > 16)
    {
        return Err(invalid());
    }
    if let Some(s) = raw["reasoning_option"].as_str() {
        if s.is_empty()
            || s.len() > 32
            || !s.as_bytes()[0].is_ascii_lowercase()
            || !s
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"_-".contains(&c))
        {
            return Err(invalid());
        }
    }
    for (k, max, bound) in [
        ("file_paths", 20, 1024),
        ("kb_dataset_ids", 8, 128),
        ("local_node_grant_ids", 1, 128),
    ] {
        if let Some(v) = raw.get(k) {
            let a = v.as_array().ok_or_else(invalid)?;
            if a.len() > max
                || a.iter().any(|v| {
                    v.as_str().is_none_or(|s| {
                        s.len() > bound || (k != "file_paths" && s.trim().is_empty())
                    })
                })
            {
                return Err(invalid());
            }
            if k != "file_paths"
                && a.iter()
                    .filter_map(Value::as_str)
                    .collect::<BTreeSet<_>>()
                    .len()
                    != a.len()
            {
                return Err(invalid());
            }
        }
    }
    let grants = raw["local_node_grant_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !text(raw, "local_node_device_id").is_empty() != !grants.is_empty() {
        return Err(invalid());
    }
    if raw
        .get("local_node_device_id")
        .is_some_and(|value| !value.is_null() && value.as_str().is_none())
    {
        return Err(invalid());
    }
    if grants.iter().any(|grant| {
        grant
            .as_str()
            .is_none_or(|value| uuid::Uuid::parse_str(value).is_err())
    }) {
        return Err(invalid());
    }
    for s in std::iter::once(text(raw, "local_node_device_id"))
        .chain(grants.iter().filter_map(Value::as_str))
        .filter(|s| !s.is_empty())
    {
        if s.len() > 128
            || !s.as_bytes()[0].is_ascii_alphanumeric()
            || !s
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
        {
            return Err(invalid());
        }
    }
    if raw["history"].as_array().is_some_and(|a| a.len() > 200) {
        return Err(invalid());
    }
    for (k, allowed) in [
        ("runtime_mode", &["off", "compat", "full"][..]),
        (
            "queue_mode",
            &["collect", "followup", "steer", "interrupt"][..],
        ),
        ("memory_profile", &["off", "basic", "hybrid"][..]),
    ] {
        if raw[k].as_str().is_some_and(|s| !allowed.contains(&s)) {
            return Err(invalid());
        }
    }
    let mut value = json!({"temperature":0.7,"kb_mode":"auto","kb_top_k":5,"kb_score_threshold":0.0,"web_search_enabled":false,"web_search_max_results":5,"file_paths":[],"kb_dataset_ids":[],"memory_mode":"auto","execution_profile":"safe"});
    for key in [
        "message",
        "model_id",
        "reasoning_option",
        "thinking_level",
        "temperature",
        "execution_profile",
        "memory_mode",
        "system_prompt",
        "os_agent_enabled",
        "local_node_device_id",
        "local_node_grant_ids",
        "max_tokens",
        "kb_dataset_ids",
        "kb_mode",
        "kb_top_k",
        "kb_score_threshold",
        "web_search_enabled",
        "web_search_max_results",
        "file_paths",
    ] {
        if let Some(v) = raw.get(key) {
            value[key] = v.clone();
        }
    }
    Ok(value)
}

#[cfg(test)]
mod local_node_selector_tests {
    use super::*;

    #[test]
    fn local_node_selector_keeps_one_owner_grant_and_rejects_ambiguous_bindings() {
        let valid = json!({
            "message":"List the files on my paired Local Node.",
            "local_node_device_id":"dev_aaaaaaaaaaaaaaaaaaaaaaaa",
            "local_node_grant_ids":["cfbfa9d4-18b3-4470-b3f2-f13660a993dc"]
        });
        let normalized = normalize_v1(&valid).expect("one explicit owner grant is valid");
        assert_eq!(
            normalized["local_node_device_id"],
            valid["local_node_device_id"]
        );
        assert_eq!(
            normalized["local_node_grant_ids"],
            valid["local_node_grant_ids"]
        );

        let multiple = json!({
            "message":"List files.",
            "local_node_device_id":"dev_aaaaaaaaaaaaaaaaaaaaaaaa",
            "local_node_grant_ids":[
                "cfbfa9d4-18b3-4470-b3f2-f13660a993dc",
                "d7bcf05f-45c8-4bd8-960e-81cd6972ec3f"
            ]
        });
        assert!(normalize_v1(&multiple).is_err());

        let malformed = json!({
            "message":"List files.",
            "local_node_device_id":"dev_aaaaaaaaaaaaaaaaaaaaaaaa",
            "local_node_grant_ids":["not-a-uuid"]
        });
        assert!(normalize_v1(&malformed).is_err());
    }
}
