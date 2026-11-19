//! Closed, credential-redacted connector-principal schemas.
//! Canonical: `src/api/schemas/mcp.py::ConnectorPrincipalCreate/Response`.
//! Connector scopes are deliberately neither trimmed nor deduplicated: that
//! normalization exists on MCP connections, not connector principals.
use crate::{
    mcp_registry_store::validate_secret_ref,
    mcp_registry_validation::{datetime, field_error, uuid_value},
    rate_config_api::boolean,
    service_config_api::ServiceConfigError,
};
use serde_json::{Map, Value, json};

pub const CONNECTOR_CHANNELS: &[&str] =
    &["preview", "hosted_private", "hosted_public", "embed", "api"];
pub const CONNECTOR_METADATA_FIELDS: &[&str] = &["domain", "email", "site_name", "cloud_id"];
pub const CONNECTOR_PRINCIPAL_FIELDS: &[&str] = &[
    "grant_id",
    "provider",
    "principal_type",
    "owner_user_id",
    "scopes",
    "audience",
    "connection_metadata",
    "allowed_channels",
    "expires_at",
    "revoked_at",
    "enabled",
    "credential_configured",
    "created_at",
    "updated_at",
];
pub fn public_channel(channel: &str) -> bool {
    matches!(channel, "hosted_public" | "embed")
}
pub fn principal_type(value: &str) -> bool {
    matches!(value, "service_account" | "user_delegated")
}
fn string(value: &Value, min: usize, max: usize) -> Result<Value, &'static str> {
    let text = value.as_str().ok_or("string_type")?;
    if text.chars().count() < min {
        Err("string_too_short")
    } else if text.chars().count() > max {
        Err("string_too_long")
    } else {
        Ok(value.clone())
    }
}
fn location(field: &str) -> [String; 2] {
    ["body".into(), field.into()]
}

/// No submitted values, validator messages, or exception context enter errors.
pub fn validate_create(body: &Value) -> Result<Value, Vec<Value>> {
    let Some(input) = body.as_object() else {
        return Err(vec![field_error(
            ["body".into()],
            if body.is_null() {
                "missing"
            } else {
                "model_attributes_type"
            },
        )]);
    };
    let fields = [
        "principal_type",
        "owner_user_id",
        "secret_ref",
        "scopes",
        "audience",
        "connection_metadata",
        "allowed_channels",
        "expires_at",
    ];
    let mut output = Map::new();
    let mut errors = Vec::new();
    for field in fields {
        let Some(value) = input.get(field) else {
            let value = match field {
                "owner_user_id" | "audience" | "expires_at" => Value::Null,
                "scopes" => json!([]),
                "connection_metadata" => json!({}),
                "allowed_channels" => json!(["preview", "hosted_private"]),
                _ => {
                    errors.push(field_error(location(field), "missing"));
                    continue;
                }
            };
            output.insert(field.into(), value);
            continue;
        };
        if value.is_null() && matches!(field, "owner_user_id" | "audience" | "expires_at") {
            output.insert(field.into(), Value::Null);
            continue;
        }
        let parsed = match field {
            "principal_type" => {
                if value.as_str().is_some_and(principal_type) {
                    Ok(value.clone())
                } else {
                    Err("literal_error")
                }
            }
            "owner_user_id" => string(value, 1, 255),
            "secret_ref" => string(value, 1, 2048).and_then(|value| {
                validate_secret_ref(value.as_str()).map_err(|_| "value_error")?;
                Ok(value)
            }),
            "audience" => string(value, 0, 2048),
            "expires_at" => datetime(value),
            "connection_metadata" => {
                if value.is_object() {
                    Ok(value.clone())
                } else {
                    Err("dict_type")
                }
            }
            "scopes" | "allowed_channels" => {
                let Some(values) = value.as_array() else {
                    errors.push(field_error(location(field), "list_type"));
                    continue;
                };
                if field == "scopes" && values.len() > 128 {
                    Err("too_long")
                } else {
                    let before = errors.len();
                    for (index, item) in values.iter().enumerate() {
                        let kind = if field == "allowed_channels" {
                            if item
                                .as_str()
                                .is_some_and(|v| CONNECTOR_CHANNELS.contains(&v))
                            {
                                None
                            } else {
                                Some("literal_error")
                            }
                        } else if item.is_string() {
                            None
                        } else {
                            Some("string_type")
                        };
                        if let Some(kind) = kind {
                            errors.push(field_error(
                                ["body".into(), field.into(), index.to_string()],
                                kind,
                            ));
                        }
                    }
                    if before != errors.len() {
                        continue;
                    }
                    Ok(value.clone())
                }
            }
            _ => unreachable!(),
        };
        match parsed {
            Ok(value) => {
                output.insert(field.into(), value);
            }
            Err(kind) => errors.push(field_error(location(field), kind)),
        }
    }
    for field in input.keys() {
        if !fields.contains(&field.as_str()) {
            errors.push(field_error(location(field), "extra_forbidden"));
        }
    }
    if errors.is_empty() {
        let value = Value::Object(output.clone());
        let delegated = value["principal_type"] == "user_delegated";
        let invalid = (value["principal_type"] == "service_account"
            && !value["owner_user_id"].is_null())
            || (delegated && value["owner_user_id"].is_null())
            || (delegated
                && value["allowed_channels"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| public_channel(v.as_str().unwrap())))
            || value["connection_metadata"]
                .as_object()
                .unwrap()
                .keys()
                .any(|key| !CONNECTOR_METADATA_FIELDS.contains(&key.as_str()));
        if invalid {
            errors.push(field_error(["body".into()], "value_error"));
        }
    }
    if errors.is_empty() {
        Ok(Value::Object(output))
    } else {
        Err(errors)
    }
}

/// Defense in depth: explicit allowlist independently of the SQL projection.
/// This function is never suitable for the internal credential-resolution row.
pub fn project_principal(row: &Value) -> Result<Value, ServiceConfigError> {
    let object = row.as_object().ok_or(ServiceConfigError::Internal)?;
    let mut output = Map::new();
    for &key in CONNECTOR_PRINCIPAL_FIELDS {
        let value = match object.get(key) {
            Some(value) => value.clone(),
            None => match key {
                "owner_user_id" | "audience" | "expires_at" | "revoked_at" => Value::Null,
                "scopes" | "allowed_channels" => json!([]),
                "connection_metadata" => json!({}),
                _ => return Err(ServiceConfigError::Internal),
            },
        };
        output.insert(key.into(), value);
    }
    let mut output = Value::Object(output);
    output["grant_id"] =
        uuid_value(&output["grant_id"]).map_err(|_| ServiceConfigError::Internal)?;
    if output["provider"] != "confluence"
        || !output["principal_type"]
            .as_str()
            .is_some_and(principal_type)
    {
        return Err(ServiceConfigError::Internal);
    }
    for key in ["owner_user_id", "audience"] {
        if !output[key].is_null() && !output[key].is_string() {
            return Err(ServiceConfigError::Internal);
        }
    }
    for key in ["scopes", "allowed_channels"] {
        if !output[key].as_array().is_some_and(|items| {
            items.iter().all(|v| {
                v.is_string()
                    && (key != "allowed_channels"
                        || CONNECTOR_CHANNELS.contains(&v.as_str().unwrap()))
            })
        }) {
            return Err(ServiceConfigError::Internal);
        }
    }
    if !output["connection_metadata"].is_object() {
        return Err(ServiceConfigError::Internal);
    }
    for key in ["enabled", "credential_configured"] {
        output[key] = json!(boolean(&output[key]).map_err(|_| ServiceConfigError::Internal)?);
    }
    for key in ["expires_at", "revoked_at", "created_at", "updated_at"] {
        if output[key].is_null() && matches!(key, "expires_at" | "revoked_at") {
            continue;
        }
        output[key] = datetime(&output[key]).map_err(|_| ServiceConfigError::Internal)?;
    }
    Ok(output)
}
