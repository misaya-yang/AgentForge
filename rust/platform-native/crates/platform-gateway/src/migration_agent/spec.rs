//! Direct port of ai_gateway_core.agents.spec; no resource authorization occurs here.
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
pub const SCHEMA: &str = "agent-spec/v1";
const ROOT: &[&str] = &[
    "schema_version",
    "identity",
    "outcome",
    "instructions",
    "model",
    "capabilities",
    "knowledge",
    "memory",
];
const IDENTITY: &[&str] = &[
    "icon_url",
    "theme_color",
    "welcome_message",
    "suggested_prompts",
];
const OUTCOME: &[&str] = &[
    "goal",
    "triggers",
    "inputs",
    "human_boundaries",
    "success_criteria",
];
const MODEL: &[&str] = &[
    "model_id",
    "provider_id",
    "temperature",
    "max_tokens",
    "thinking_mode",
];
const CAPABILITY: &[&str] = &[
    "type",
    "resource_id",
    "resource_version",
    "schema_hash",
    "config",
];
const KNOWLEDGE: &[&str] = &["dataset_id", "retrieval_config"];
const RETRIEVAL: &[&str] = &[
    "mode",
    "top_k",
    "threshold",
    "score_threshold",
    "include_images",
];
pub fn sensitive_key(key: &str) -> bool {
    let key: String = key
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    [
        "accesskey",
        "accesstoken",
        "apikey",
        "apitoken",
        "auth",
        "authentication",
        "authorization",
        "bearer",
        "bearertoken",
        "clientsecret",
        "credential",
        "credentials",
        "credentialref",
        "credentialvalue",
        "oauthrefreshtoken",
        "oauthtoken",
        "password",
        "passwordhash",
        "privatekey",
        "refreshtoken",
        "secret",
        "secretref",
        "secrets",
        "tokenhash",
        "tokenref",
    ]
    .contains(&key.as_str())
        || [
            "apikey",
            "apitoken",
            "accesstoken",
            "authorization",
            "bearertoken",
            "clientsecret",
            "password",
            "privatekey",
            "refreshtoken",
            "secret",
            "tokenhash",
        ]
        .iter()
        .any(|s| key.contains(s))
}
pub fn canonical(value: &Value) -> String {
    crate::mcp_discovery::canonical_schema(value)
}
pub fn hash(value: &Value) -> String {
    hex(&Sha256::digest(canonical(value).as_bytes()))
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn unsafe_paths(value: &Value, location: &str) -> Vec<String> {
    let mut out = Vec::new();
    match value {
        Value::Object(o) => {
            for (k, v) in o {
                let p = format!("{location}.{k}");
                if sensitive_key(k) {
                    out.push(p)
                } else {
                    out.extend(unsafe_paths(v, &p));
                }
            }
        }
        Value::Array(a) => {
            for (i, v) in a.iter().enumerate() {
                out.extend(unsafe_paths(v, &format!("{location}[{i}]")));
            }
        }
        _ => {}
    }
    out
}
fn error(field: impl Into<String>, code: &str, message: impl Into<String>) -> Value {
    json!({"field":field.into(),"code":code,"message":message.into()})
}
fn unknown(value: &Value, allowed: &[&str], location: &str, errors: &mut Vec<Value>) {
    if let Some(o) = value.as_object() {
        for key in o.keys() {
            if !allowed.contains(&key.as_str()) {
                errors.push(error(
                    format!("{location}.{key}"),
                    "AGENT_SPEC_FIELD_FORBIDDEN",
                    "field is not part of the public Agent Spec contract",
                ));
            }
        }
    }
}
pub fn safety_errors(spec: &Value) -> Vec<Value> {
    if !spec.is_object() {
        return vec![error(
            "spec",
            "AGENT_SPEC_TYPE_INVALID",
            "spec must be an object",
        )];
    }
    let mut errors = Vec::new();
    unknown(spec, ROOT, "spec", &mut errors);
    for (k, allowed) in [
        ("identity", IDENTITY),
        ("outcome", OUTCOME),
        ("model", MODEL),
    ] {
        unknown(&spec[k], allowed, &format!("spec.{k}"), &mut errors);
    }
    for (k, allowed) in [("capabilities", CAPABILITY), ("knowledge", KNOWLEDGE)] {
        if let Some(a) = spec[k].as_array() {
            for (i, v) in a.iter().enumerate() {
                let loc = format!("spec.{k}[{i}]");
                unknown(v, allowed, &loc, &mut errors);
                if k == "knowledge" {
                    unknown(
                        &v["retrieval_config"],
                        RETRIEVAL,
                        &format!("{loc}.retrieval_config"),
                        &mut errors,
                    );
                }
            }
        }
    }
    for path in unsafe_paths(spec, "spec") {
        errors.push(error(
            path,
            "AGENT_SPEC_SECRET_FORBIDDEN",
            "credentials, Secret references, and token material are forbidden",
        ));
    }
    errors
}
pub fn redact(value: &Value) -> Value {
    match value {
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(k, _)| !sensitive_key(k))
                .map(|(k, v)| (k.clone(), redact(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(redact).collect()),
        _ => value.clone(),
    }
}
fn structured(value: &Value, allowed: &[&str]) -> Value {
    Value::Object(
        value
            .as_object()
            .into_iter()
            .flat_map(|o| o.iter())
            .filter(|(k, _)| allowed.contains(&k.as_str()) && !sensitive_key(k))
            .map(|(k, v)| (k.clone(), redact(v)))
            .collect(),
    )
}
pub fn redact_for_read(value: &Value) -> Value {
    let mut out = Map::new();
    for k in ["schema_version", "instructions"] {
        if let Some(v) = value.get(k) {
            out.insert(k.into(), v.clone());
        }
    }
    for (k, keys) in [
        ("identity", IDENTITY),
        ("outcome", OUTCOME),
        ("model", MODEL),
    ] {
        if let Some(v) = value.get(k) {
            out.insert(k.into(), structured(v, keys));
        }
    }
    for (k, keys) in [("capabilities", CAPABILITY), ("knowledge", KNOWLEDGE)] {
        if let Some(a) = value[k].as_array() {
            out.insert(
                k.into(),
                Value::Array(
                    a.iter()
                        .filter(|v| v.is_object())
                        .map(|v| structured(v, keys))
                        .collect(),
                ),
            );
        }
    }
    if value["memory"].is_object() {
        out.insert("memory".into(), redact(&value["memory"]));
    }
    Value::Object(out)
}
pub fn copy_spec(value: &Value) -> Value {
    let public = redact_for_read(value);
    let mut out = json!({"schema_version":SCHEMA,"identity":structured(&public["identity"],IDENTITY),"instructions":public["instructions"].as_str().unwrap_or(""),"model":structured(&public["model"],MODEL),"capabilities":[],"knowledge":[],"memory":{}});
    if !public["outcome"].is_null() {
        out["outcome"] = structured(&public["outcome"], OUTCOME);
    }
    out
}
pub fn validate(spec: &Value) -> Vec<Value> {
    let mut errors = safety_errors(spec);
    if !spec.is_object() {
        return errors;
    }
    if spec["schema_version"] != SCHEMA {
        errors.push(error(
            "schema_version",
            "AGENT_SPEC_SCHEMA_UNSUPPORTED",
            format!("schema_version must be {SCHEMA}"),
        ));
    }
    if spec["instructions"]
        .as_str()
        .is_none_or(|s| s.trim().is_empty())
    {
        errors.push(error(
            "instructions",
            "AGENT_INSTRUCTIONS_REQUIRED",
            "instructions must be a non-empty string",
        ));
    }
    if let Some(outcome) = spec.get("outcome").filter(|v| !v.is_null()) {
        if !outcome.is_object() {
            errors.push(error(
                "outcome",
                "AGENT_OUTCOME_INVALID",
                "outcome must be an object",
            ));
        } else {
            if outcome.get("goal").is_some_and(|v| !v.is_string()) {
                errors.push(error(
                    "outcome.goal",
                    "AGENT_OUTCOME_GOAL_INVALID",
                    "outcome.goal must be a string",
                ));
            }
            for k in ["triggers", "inputs", "human_boundaries", "success_criteria"] {
                if outcome.get(k).is_some_and(|v| {
                    v.as_array().is_none_or(|a| {
                        a.iter()
                            .any(|s| s.as_str().is_none_or(|s| s.trim().is_empty()))
                    })
                }) {
                    errors.push(error(
                        format!("outcome.{k}"),
                        "AGENT_OUTCOME_LIST_INVALID",
                        format!("outcome.{k} must contain non-empty strings"),
                    ));
                }
            }
        }
    }
    if spec["model"]["model_id"]
        .as_str()
        .is_none_or(|s| s.trim().is_empty())
    {
        errors.push(error(
            "model.model_id",
            "AGENT_MODEL_REQUIRED",
            "model.model_id must be configured",
        ));
    }
    let mut seen = BTreeSet::new();
    if let Some(knowledge) = spec.get("knowledge").filter(|v| !v.is_null()) {
        if let Some(bindings) = knowledge.as_array() {
            for (i, b) in bindings.iter().enumerate() {
                let p = format!("knowledge[{i}]");
                if !b.is_object() {
                    errors.push(error(
                        &p,
                        "AGENT_KNOWLEDGE_BINDING_INVALID",
                        "Knowledge binding must be an object",
                    ));
                    continue;
                }
                match b["dataset_id"].as_str() {
                    Some(id) if !id.trim().is_empty() => {
                        if !seen.insert(id) {
                            errors.push(error(
                                format!("{p}.dataset_id"),
                                "AGENT_KNOWLEDGE_DATASET_DUPLICATE",
                                "dataset_id must be unique within an Agent spec",
                            ));
                        }
                    }
                    _ => errors.push(error(
                        format!("{p}.dataset_id"),
                        "AGENT_KNOWLEDGE_DATASET_REQUIRED",
                        "dataset_id must be a non-empty string",
                    )),
                }
                let empty = json!({});
                let cfg = b.get("retrieval_config").unwrap_or(&empty);
                let loc = format!("{p}.retrieval_config");
                if !cfg.is_object() {
                    errors.push(error(
                        loc,
                        "AGENT_KNOWLEDGE_CONFIG_INVALID",
                        "retrieval_config must be an object",
                    ));
                    continue;
                }
                if !matches!(
                    cfg.get("mode").unwrap_or(&json!("auto")).as_str(),
                    Some("auto" | "tool" | "off")
                ) {
                    errors.push(error(
                        format!("{loc}.mode"),
                        "AGENT_KNOWLEDGE_MODE_INVALID",
                        "mode must be auto, tool, or off",
                    ));
                }
                if cfg
                    .get("top_k")
                    .unwrap_or(&json!(5))
                    .as_i64()
                    .is_none_or(|n| !(1..=20).contains(&n))
                {
                    errors.push(error(
                        format!("{loc}.top_k"),
                        "AGENT_KNOWLEDGE_TOP_K_INVALID",
                        "top_k must be an integer between 1 and 20",
                    ));
                }
                if cfg
                    .get("threshold")
                    .or_else(|| cfg.get("score_threshold"))
                    .unwrap_or(&json!(0.4))
                    .as_f64()
                    .is_none_or(|n| !(0.0..=1.0).contains(&n))
                {
                    errors.push(error(
                        format!("{loc}.threshold"),
                        "AGENT_KNOWLEDGE_THRESHOLD_INVALID",
                        "threshold must be a number between 0 and 1",
                    ));
                }
                if let (Some(a), Some(b)) = (cfg.get("threshold"), cfg.get("score_threshold")) {
                    if a != b {
                        errors.push(error(
                            &loc,
                            "AGENT_KNOWLEDGE_THRESHOLD_CONFLICT",
                            "threshold and score_threshold must not conflict",
                        ));
                    }
                }
                if cfg.get("include_images").is_some_and(|v| !v.is_boolean()) {
                    errors.push(error(
                        format!("{loc}.include_images"),
                        "AGENT_KNOWLEDGE_IMAGES_INVALID",
                        "include_images must be a boolean",
                    ));
                }
            }
        } else {
            errors.push(error(
                "knowledge",
                "AGENT_KNOWLEDGE_BINDING_INVALID",
                "knowledge must be a list",
            ));
        }
    }
    errors
}
pub fn outcome_contract(spec: &Value) -> String {
    let o = &spec["outcome"];
    let goal = o["goal"].as_str().unwrap_or("").trim();
    let mut lines = Vec::new();
    if !goal.is_empty() {
        lines.push(format!("Goal: {goal}"));
    }
    for (label, key) in [
        ("Triggers", "triggers"),
        ("Required inputs", "inputs"),
        ("Human boundaries", "human_boundaries"),
        ("Success criteria", "success_criteria"),
    ] {
        let values: Vec<_> = o[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if !values.is_empty() {
            lines.push(format!("{label}:"));
            lines.extend(values.iter().map(|s| format!("- {s}")));
        }
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("[Agent outcome contract]\n{}", lines.join("\n"))
    }
}
