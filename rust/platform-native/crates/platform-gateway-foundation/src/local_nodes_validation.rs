//! Source-shaped Pydantic request validation for the public Local Node API.
use crate::{local_nodes_types::LocalNodesError, rate_config_api::integer};
use serde_json::{Map, Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalNodesBodyKind {
    Challenge,
    Completion,
    Grant,
    EmergencyStop,
}

fn error(field: &str, typ: &str, message: impl Into<String>, input: &Value) -> Value {
    json!({"type":typ,"loc":["body",field],"msg":message.into(),"input":input})
}
fn integer_message(typ: &str) -> &'static str {
    match typ {
        "int_parsing" => "Input should be a valid integer, unable to parse string as an integer",
        "int_from_float" => "Input should be a valid integer, got a number with a fractional part",
        "int_parsing_size" => "Unable to parse input string as an integer, exceeded maximum size",
        _ => "Input should be a valid integer",
    }
}
struct StringRule<'a> {
    default: Option<&'a str>,
    nullable: bool,
    min: usize,
    max: usize,
}
fn string_field(
    input: &Map<String, Value>,
    output: &mut Map<String, Value>,
    errors: &mut Vec<Value>,
    name: &str,
    rule: StringRule<'_>,
) {
    let StringRule {
        default,
        nullable,
        min,
        max,
    } = rule;
    let value = match input.get(name) {
        Some(value) => value.clone(),
        None => match default {
            Some(v) => json!(v),
            None if nullable => Value::Null,
            None => {
                errors.push(error(
                    name,
                    "missing",
                    "Field required",
                    &Value::Object(input.clone()),
                ));
                return;
            }
        },
    };
    if nullable && value.is_null() {
        output.insert(name.into(), value);
        return;
    }
    let Some(text) = value.as_str() else {
        errors.push(error(
            name,
            "string_type",
            "Input should be a valid string",
            &value,
        ));
        return;
    };
    let len = text.chars().count();
    if len < min {
        let mut err = error(
            name,
            "string_too_short",
            format!(
                "String should have at least {min} character{}",
                if min == 1 { "" } else { "s" }
            ),
            &value,
        );
        err["ctx"] = json!({"min_length":min});
        errors.push(err);
    } else if len > max {
        let mut err = error(
            name,
            "string_too_long",
            format!("String should have at most {max} characters"),
            &value,
        );
        err["ctx"] = json!({"max_length":max});
        errors.push(err);
    } else {
        output.insert(name.into(), value);
    }
}
fn strings_field(
    input: &Map<String, Value>,
    output: &mut Map<String, Value>,
    errors: &mut Vec<Value>,
    name: &str,
    default: bool,
    bounds: (usize, usize),
) {
    let (min, max) = bounds;
    let value = match input.get(name) {
        Some(v) => v.clone(),
        None if default => json!([]),
        None => {
            errors.push(error(
                name,
                "missing",
                "Field required",
                &Value::Object(input.clone()),
            ));
            return;
        }
    };
    let Some(items) = value.as_array() else {
        errors.push(error(
            name,
            "list_type",
            "Input should be a valid list",
            &value,
        ));
        return;
    };
    // Pydantic's max-length iterator guard takes precedence over element errors.
    if items.len() > max {
        let mut err = error(
            name,
            "too_long",
            format!(
                "List should have at most {max} items after validation, not {}",
                items.len()
            ),
            &value,
        );
        err["ctx"] = json!({"field_type":"List","max_length":max,"actual_length":items.len()});
        errors.push(err);
        return;
    }
    let mut invalid = false;
    for (i, item) in items.iter().enumerate() {
        if !item.is_string() {
            let mut err = error(name, "string_type", "Input should be a valid string", item);
            err["loc"] = json!(["body", name, i]);
            errors.push(err);
            invalid = true;
        }
    }
    if !invalid && items.len() < min {
        let mut err = error(
            name,
            "too_short",
            format!(
                "List should have at least {min} item{} after validation, not {}",
                if min == 1 { "" } else { "s" },
                items.len()
            ),
            &value,
        );
        err["ctx"] = json!({"field_type":"List","min_length":min,"actual_length":items.len()});
        errors.push(err);
    } else if !invalid {
        output.insert(name.into(), value);
    }
}
/// Returns the complete model dump, including defaults and explicit nulls.
pub fn validate_body(kind: LocalNodesBodyKind, value: &Value) -> Result<Value, LocalNodesError> {
    if value.is_null() {
        return Err(LocalNodesError::detail(
            422,
            json!([{"type":"missing","loc":["body"],"msg":"Field required","input":null}]),
        ));
    }
    let Some(input) = value.as_object() else {
        return Err(LocalNodesError::detail(
            422,
            json!([{"type":"model_attributes_type","loc":["body"],"msg":"Input should be a valid dictionary or object to extract fields from","input":value}]),
        ));
    };
    let mut out = Map::new();
    let mut errors = vec![];
    let fields: &[&str] = match kind {
        LocalNodesBodyKind::EmergencyStop => {
            if input.get("confirmation").and_then(Value::as_str) != Some("stop-all-local-actions") {
                return Err(LocalNodesError::detail(
                    422,
                    "explicit emergency-stop confirmation required",
                ));
            }
            let id = input
                .get("idempotency_key")
                .and_then(Value::as_str)
                .filter(|id| uuid::Uuid::parse_str(id).is_ok())
                .ok_or_else(|| {
                    LocalNodesError::detail(422, "emergency-stop idempotency_key must be a UUID")
                })?;
            out.insert("confirmation".into(), json!("stop-all-local-actions"));
            out.insert(
                "idempotency_key".into(),
                json!(uuid::Uuid::parse_str(id).unwrap().to_string()),
            );
            &["confirmation", "idempotency_key"]
        }
        LocalNodesBodyKind::Challenge => {
            let value = input
                .get("expires_in_seconds")
                .cloned()
                .unwrap_or(json!(600));
            match integer(&value) {
                Ok(n) if (60..=900).contains(&n) => {
                    out.insert("expires_in_seconds".into(), json!(n));
                }
                Ok(n) => {
                    let (typ, msg, ctx) = if n < 60 {
                        (
                            "greater_than_equal",
                            "Input should be greater than or equal to 60",
                            json!({"ge":60}),
                        )
                    } else {
                        (
                            "less_than_equal",
                            "Input should be less than or equal to 900",
                            json!({"le":900}),
                        )
                    };
                    let mut err = error("expires_in_seconds", typ, msg, &value);
                    err["ctx"] = ctx;
                    errors.push(err);
                }
                Err("int_parsing_size") => {
                    let negative = value
                        .as_str()
                        .map(|s| s.trim().starts_with('-'))
                        .unwrap_or_else(|| value.as_f64().is_some_and(|n| n < 0.0));
                    let (typ, msg, ctx) = if negative {
                        (
                            "greater_than_equal",
                            "Input should be greater than or equal to 60",
                            json!({"ge":60}),
                        )
                    } else {
                        (
                            "less_than_equal",
                            "Input should be less than or equal to 900",
                            json!({"le":900}),
                        )
                    };
                    let mut err = error("expires_in_seconds", typ, msg, &value);
                    err["ctx"] = ctx;
                    errors.push(err);
                }
                Err(typ) => errors.push(error(
                    "expires_in_seconds",
                    typ,
                    integer_message(typ),
                    &value,
                )),
            }
            &["expires_in_seconds"]
        }
        LocalNodesBodyKind::Completion => {
            for (name, default, min, max) in [
                ("display_name", "Local Node", 1, 160),
                ("platform", "unknown", 1, 32),
                ("node_version", "unknown", 1, 64),
                ("protocol_version", "ai-platform.local-node.v1", 1, 64),
            ] {
                string_field(
                    input,
                    &mut out,
                    &mut errors,
                    name,
                    StringRule {
                        default: Some(default),
                        nullable: false,
                        min,
                        max,
                    },
                );
            }
            strings_field(
                input,
                &mut out,
                &mut errors,
                "capability_claims",
                true,
                (0, 64),
            );
            string_field(
                input,
                &mut out,
                &mut errors,
                "permission_snapshot_digest",
                StringRule {
                    default: Some(""),
                    nullable: false,
                    min: 0,
                    max: 80,
                },
            );
            &[
                "display_name",
                "platform",
                "node_version",
                "protocol_version",
                "capability_claims",
                "permission_snapshot_digest",
            ]
        }
        LocalNodesBodyKind::Grant => {
            match input.get("kind") {
                None => errors.push(error(
                    "kind",
                    "missing",
                    "Field required",
                    &Value::Object(input.clone()),
                )),
                Some(Value::String(s)) if ["workspace", "app", "domain"].contains(&s.as_str()) => {
                    out.insert("kind".into(), json!(s));
                }
                Some(v @ Value::String(_)) => {
                    let mut err = error(
                        "kind",
                        "string_pattern_mismatch",
                        "String should match pattern '^(workspace|app|domain)$'",
                        v,
                    );
                    err["ctx"] = json!({"pattern":"^(workspace|app|domain)$"});
                    errors.push(err);
                }
                Some(v) => errors.push(error(
                    "kind",
                    "string_type",
                    "Input should be a valid string",
                    v,
                )),
            }
            string_field(
                input,
                &mut out,
                &mut errors,
                "display_name",
                StringRule {
                    default: None,
                    nullable: false,
                    min: 1,
                    max: 160,
                },
            );
            strings_field(input, &mut out, &mut errors, "capabilities", false, (1, 16));
            for name in ["resource_ref", "session_id"] {
                string_field(
                    input,
                    &mut out,
                    &mut errors,
                    name,
                    StringRule {
                        default: None,
                        nullable: true,
                        min: 0,
                        max: 255,
                    },
                );
            }
            &[
                "kind",
                "display_name",
                "capabilities",
                "resource_ref",
                "session_id",
            ]
        }
    };
    for (name, v) in input {
        if !fields.contains(&name.as_str()) {
            errors.push(error(
                name,
                "extra_forbidden",
                "Extra inputs are not permitted",
                v,
            ));
        }
    }
    if errors.is_empty() {
        Ok(Value::Object(out))
    } else {
        Err(LocalNodesError::detail(422, json!(errors)))
    }
}

/// Values outside PostgreSQL int8 must reach the control plane: source performs
/// ownership lookup before the database rejects their integer binding. Positive
/// values beyond serde_json's u64 are carried as decimal strings solely to retain
/// that failure ordering; this is not an arbitrary-precision JSON wire encoder.
pub fn validate_after_sequence(raw: Option<&str>) -> Result<Value, LocalNodesError> {
    let Some(raw) = raw else {
        return Ok(json!(0));
    };
    let value = json!(raw);
    match integer(&value) {
        Ok(n) if n < 0 => Err(LocalNodesError::detail(
            422,
            "after_sequence must be non-negative",
        )),
        Ok(n) => Ok(json!(n)),
        Err("int_parsing_size") => {
            let trimmed = raw.trim();
            if trimmed.starts_with('-') {
                return Err(LocalNodesError::detail(
                    422,
                    "after_sequence must be non-negative",
                ));
            }
            let digits = trimmed
                .split('.')
                .next()
                .unwrap_or(trimmed)
                .trim_start_matches('+')
                .replace('_', "");
            match digits.parse::<u64>() {
                Ok(n) => Ok(json!(n)),
                Err(_) => Ok(json!(digits)),
            }
        }
        Err(typ) => Err(LocalNodesError::detail(
            422,
            json!([{"type":typ,"loc":["query","after_sequence"],"msg":integer_message(typ),"input":raw}]),
        )),
    }
}

#[cfg(test)]
mod emergency_stop_tests {
    use super::*;
    #[test]
    fn stop_intent_requires_exact_confirmation_unique_request_and_no_scope_injection() {
        let good = json!({"confirmation":"stop-all-local-actions","idempotency_key":uuid::Uuid::new_v4().to_string()});
        assert!(validate_body(LocalNodesBodyKind::EmergencyStop, &good).is_ok());
        for bad in [
            json!({}),
            json!({"confirmation":true,"idempotency_key":good["idempotency_key"]}),
            json!({"confirmation":"stop-all-local-actions","idempotency_key":"not-a-uuid"}),
        ] {
            assert!(validate_body(LocalNodesBodyKind::EmergencyStop, &bad).is_err());
        }
        for key in [
            "tenant_id",
            "user_id",
            "device_id",
            "nonce",
            "platform_signature",
        ] {
            let mut bad = good.clone();
            bad[key] = json!("injected");
            assert!(validate_body(LocalNodesBodyKind::EmergencyStop, &bad).is_err());
        }
    }
}
