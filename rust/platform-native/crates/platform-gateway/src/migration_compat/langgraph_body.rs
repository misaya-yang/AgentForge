//! Request mutations shared by typed LangGraph routes and transparent passthrough.
use super::common::{CompatUser, object, text};
use crate::service_registry::python_truthy;
use serde_json::{Value, json};

pub fn is_run_path(method: &str, path: &str) -> bool {
    if !matches!(
        method.to_ascii_uppercase().as_str(),
        "POST" | "PUT" | "PATCH"
    ) {
        return false;
    }
    let lower = path.trim().to_ascii_lowercase();
    let segments: Vec<_> = lower.trim_start_matches('/').split('/').collect();
    let core = if matches!(segments.last(), Some(&"stream") | Some(&"wait")) {
        &segments[..segments.len() - 1]
    } else {
        &segments[..]
    };
    core.last() == Some(&"runs")
        && (core.len() == 1
            || (core.len() >= 3
                && core[core.len() - 3] == "threads"
                && !core[core.len() - 2].is_empty())
            || lower.ends_with("/runs"))
}
pub fn thread_id(path: &str) -> Option<&str> {
    let parts: Vec<_> = path
        .trim_matches('/')
        .split('/')
        .filter(|v| !v.is_empty())
        .collect();
    parts
        .windows(3)
        .find(|p| p[0].eq_ignore_ascii_case("threads") && p[2].eq_ignore_ascii_case("runs"))
        .map(|p| p[1])
}
pub fn sanitize_configurable(value: &Value) -> Value {
    let mut result = object(value);
    result.retain(|key, _| {
        let key = key.trim().to_ascii_lowercase().replace('-', "_");
        !matches!(
            key.as_str(),
            "user_id"
                | "tenant_id"
                | "checkpoint_ns"
                | "gateway_model"
                | "_api_key"
                | "api_key"
                | "apikey"
                | "provider_api_key"
                | "provider_credentials"
                | "credential"
                | "credentials"
                | "password"
                | "secret"
                | "token"
                | "auth_token"
                | "access_token"
                | "authorization"
                | "service_id"
                | "assistant_id"
                | "thread_id"
        ) && !["_api_key", "_token", "_secret", "_credentials"]
            .iter()
            .any(|suffix| key.ends_with(suffix))
    });
    Value::Object(result)
}
pub fn normalize_domain_policy(value: &Value) -> String {
    let policy = value.as_str().unwrap_or("").trim().to_lowercase();
    if policy.is_empty()
        || policy.chars().count() > 64
        || !policy
            .chars()
            .all(|c| c.is_alphanumeric() || "._-".contains(c))
    {
        "none".into()
    } else {
        policy
    }
}
pub fn prepare_run_payload(
    payload: &mut Value,
    path: &str,
    user: &CompatUser,
    service: &Value,
    assistant: Option<&Value>,
) {
    if !payload.is_object() {
        return;
    }
    let mut policy = normalize_domain_policy(&service["metadata"]["domain_policy"]);
    if policy == "none" {
        policy = assistant
            .map(|a| normalize_domain_policy(&a["metadata"]["domain_policy"]))
            .unwrap_or_else(|| "none".into());
    }
    if policy != "none" {
        let mut metadata = object(&payload["metadata"]);
        let mut gateway = object(metadata.get("gateway").unwrap_or(&Value::Null));
        if !gateway.get("domain_policy").is_some_and(python_truthy) {
            gateway.insert("domain_policy".into(), json!(policy));
        }
        metadata.insert("gateway".into(), json!(gateway));
        payload["metadata"] = json!(metadata);
    }
    let mut config = object(&payload["config"]);
    let mut configurable = object(&sanitize_configurable(
        config.get("configurable").unwrap_or(&Value::Null),
    ));
    let tenant = if user.tenant_id.is_empty() {
        "default"
    } else {
        &user.tenant_id
    };
    configurable.insert(
        "user_id".into(),
        json!(if user.user_id.is_empty() {
            "anonymous"
        } else {
            &user.user_id
        }),
    );
    configurable.insert("tenant_id".into(), json!(tenant));
    configurable.insert("checkpoint_ns".into(), json!(tenant));
    if !user.tier.trim().is_empty() {
        configurable.insert("user_tier".into(), json!(user.tier.trim()));
    }
    let fallback = if !text(payload, "thread_id").is_empty() {
        text(payload, "thread_id")
    } else if !user.request_id.is_empty() {
        &user.request_id
    } else {
        &user.trace_id
    };
    let thread = thread_id(path).unwrap_or(fallback);
    if !thread.is_empty() {
        configurable.insert("thread_id".into(), json!(thread));
    }
    let mut metadata = object(config.get("metadata").unwrap_or(&Value::Null));
    if !user.request_id.is_empty() {
        metadata.insert("gateway_request_id".into(), json!(user.request_id));
    }
    if !user.trace_id.is_empty() {
        metadata.insert("gateway_trace_id".into(), json!(user.trace_id));
    }
    config.insert("metadata".into(), json!(metadata));
    config.insert("configurable".into(), json!(configurable));
    payload["config"] = json!(config);
    if !text(service, "assistant_id").trim().is_empty() {
        payload["assistant_id"] = json!(text(service, "assistant_id").trim());
    }
    if path.to_ascii_lowercase().ends_with("/runs/stream") {
        if !python_truthy(&payload["stream_mode"]) {
            payload["stream_mode"] = json!(["messages", "updates", "custom"]);
        }
        let wants_messages = match &payload["stream_mode"] {
            Value::String(v) => v
                .split(',')
                .any(|s| matches!(s.trim(), "messages" | "messages-tuple")),
            Value::Array(v) => v
                .iter()
                .any(|s| matches!(s.as_str(), Some("messages" | "messages-tuple"))),
            _ => false,
        };
        if wants_messages && payload.get("stream_subgraphs").is_none() {
            payload["stream_subgraphs"] = json!(true);
        }
    }
}
pub fn billing_snapshot(payload: &Value) -> Value {
    let mut result = payload.clone();
    if let Some(configurable) = result
        .get_mut("config")
        .and_then(|v| v.get_mut("configurable"))
        .and_then(Value::as_object_mut)
    {
        if let Some(model) = configurable.remove("gateway_model") {
            let mut hints = object(&model);
            hints.retain(|key, value| {
                matches!(
                    key.as_str(),
                    "model_id"
                        | "model"
                        | "provider_id"
                        | "provider"
                        | "cache_epoch"
                        | "api_key_fingerprint"
                ) && !value.is_null()
            });
            if !hints.is_empty() {
                configurable.insert("gateway_model".into(), json!(hints));
            }
        }
    }
    result
}
pub fn apply_model_downgrade(payload: &mut Value, model: &str, requested: &str) -> bool {
    if model.trim().is_empty() || model.trim() == requested.trim() {
        return false;
    }
    let Some(gateway) = payload
        .pointer_mut("/config/configurable/gateway_model")
        .and_then(Value::as_object_mut)
    else {
        return false;
    };
    if gateway.get("model_id") == Some(&json!(model.trim()))
        && gateway.get("model") == Some(&json!(model.trim()))
    {
        return false;
    }
    gateway.insert("model_id".into(), json!(model.trim()));
    gateway.insert("model".into(), json!(model.trim()));
    true
}
