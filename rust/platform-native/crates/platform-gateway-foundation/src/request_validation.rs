//! Request-validation core from `src/core/gateway/validator.py`.
//!
//! The caller supplies the already selected service and verified identity/roles.
//! This module neither authenticates identity from request data nor resolves a
//! service, tenant, API key, provider, or HTTP status. The views below contain only
//! fields read by the Python validator; they are not a new public request format.
//!
//! Validation order is required fields, content types, parameter schema, content
//! presence, service auth, then global AgentInvoke. Public/disabled service auth
//! never bypasses the final capability check. `allowed_api_keys` is intentionally
//! unused, as in the source.
//!
//! JSON Schema uses the real `jsonschema` engine (Draft 4/6/7/2019-09/2020-12).
//! Formats and content encodings/media types remain annotations, as with Python's
//! `jsonschema.validate` without a format checker. Invalid serialized schemas and
//! non-object schemas are skipped only where Python explicitly skips them.
//! Invalid object schemas are configuration errors, not client validation errors.
//! Engine diagnostic wording/error selection is not byte-identical to Python.
//!
//! Security-boundary deviation: external HTTP/file/other retrieval is always
//! denied, even if another Cargo dependency enables resolver features. In-document
//! references still work; unresolved references fail as configuration errors.
//! Unknown dialects fall back to Draft 2020-12, as in Python. Recognized Draft 3
//! has no engine support and is an explicit configuration error instead of being
//! interpreted under a newer draft. Python may retrieve remote references; this
//! core deliberately cannot.
use ai_platform_auth::rbac::{Capability, Rbac, require_capability};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;

/// Canonical `ai_gateway_core.enums.ContentType` values.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    Text,
    Image,
    Audio,
    Video,
    File,
    Json,
    ToolCall,
    ToolResult,
}
impl ContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
            Self::File => "file",
            Self::Json => "json",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
        }
    }
}
impl<'de> Deserialize<'de> for ContentType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Pydantic and ContentType(value) accept wire strings, not tagged maps.
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "text" => Ok(Self::Text),
            "image" => Ok(Self::Image),
            "audio" => Ok(Self::Audio),
            "video" => Ok(Self::Video),
            "file" => Ok(Self::File),
            "json" => Ok(Self::Json),
            "tool_call" => Ok(Self::ToolCall),
            "tool_result" => Ok(Self::ToolResult),
            _ => Err(serde::de::Error::unknown_variant(
                &value,
                &[
                    "text",
                    "image",
                    "audio",
                    "video",
                    "file",
                    "json",
                    "tool_call",
                    "tool_result",
                ],
            )),
        }
    }
}

/// Both empty strings and empty byte buffers are data, not missing content.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ContentData {
    Text(String),
    Bytes(Vec<u8>),
    /// Python dataclass annotations are not runtime constraints; adapters also
    /// supply structured JSON. Every non-null value counts as present data.
    Json(Value),
}
impl<'de> Deserialize<'de> for ContentData {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // JSON has no binary type. In particular, numeric arrays remain arrays,
        // matching Python dataclasses loaded from task/config JSON. Binary data
        // must be constructed explicitly by an adapter as ContentData::Bytes.
        Ok(match Value::deserialize(deserializer)? {
            Value::String(text) => Self::Text(text),
            value => Self::Json(value),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ContentItem {
    #[serde(rename = "type")]
    pub content_type: ContentType,
    #[serde(default)]
    pub data: Option<ContentData>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ValidationRequest {
    pub service_id: String,
    pub inputs: Vec<ContentItem>,
    /// JSON boundary; falsey values become `{}` exactly as `parameters or {}`.
    pub parameters: Value,
    /// Must be populated from authenticated authority, never trusted body input.
    pub user_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ServiceAuthConfig {
    pub enabled: bool,
    pub require_auth: bool,
    pub allowed_roles: Vec<String>,
    pub allowed_api_keys: Vec<String>,
    pub public: bool,
}
impl Default for ServiceAuthConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            require_auth: true,
            allowed_roles: Vec::new(),
            allowed_api_keys: Vec::new(),
            public: false,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct ValidationService {
    pub service_id: String,
    pub accepted_content_types: Vec<ContentType>,
    /// May contain a serialized object due to legacy database deserialization.
    pub input_schema: Value,
    /// The auth member of `ServiceDefinition.get_service_config()`.
    pub auth: ServiceAuthConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestValidationError {
    ValidationFailed(String),
    InvalidContentType {
        content_type: ContentType,
        /// A set in Python. Rust retains first occurrence order for diagnostics.
        accepted: Vec<ContentType>,
    },
    AuthenticationRequired(String),
    PermissionDenied(String),
    /// Invalid schema/dialect/reference, analogous to uncaught Python SchemaError
    /// or reference-resolution errors. Do not map this to a bad client request.
    SchemaConfiguration(String),
}
impl RequestValidationError {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ValidationFailed(_) => "ValidationFailedError",
            Self::InvalidContentType { .. } => "InvalidContentTypeError",
            Self::AuthenticationRequired(_) => "AuthenticationRequiredError",
            Self::PermissionDenied(_) => "PermissionDeniedError",
            Self::SchemaConfiguration(_) => "SchemaError",
        }
    }
}
impl fmt::Display for RequestValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ValidationFailed(message)
            | Self::AuthenticationRequired(message)
            | Self::PermissionDenied(message)
            | Self::SchemaConfiguration(message) => formatter.write_str(message),
            Self::InvalidContentType {
                content_type,
                accepted,
            } => write!(
                formatter,
                "Invalid content type {}, accepted: {}",
                content_type.as_str(),
                python_string_list(accepted.iter().map(|item| item.as_str())),
            ),
        }
    }
}
impl std::error::Error for RequestValidationError {}

pub struct RequestValidator<'a> {
    rbac: &'a Rbac,
}
impl<'a> RequestValidator<'a> {
    pub fn new(rbac: &'a Rbac) -> Self {
        Self { rbac }
    }

    /// Synchronous because every check is local and side-effect-free.
    pub fn validate(
        &self,
        request: &ValidationRequest,
        service: &ValidationService,
        roles: &[String],
    ) -> Result<(), RequestValidationError> {
        self.validate_with_auth(request, service, roles, || service.auth.clone())
    }

    /// Resolve live service auth only after every earlier request check passes.
    /// The callback is synchronous; it may materialize shared configuration but
    /// must not retain a lock guard or perform asynchronous work.
    pub fn validate_with_auth(
        &self,
        request: &ValidationRequest,
        service: &ValidationService,
        roles: &[String],
        resolve_auth: impl FnOnce() -> ServiceAuthConfig,
    ) -> Result<(), RequestValidationError> {
        self.validate_required_fields(request)?;
        self.validate_content_types(&request.inputs, service)?;
        let empty = json!({});
        let parameters = if python_truthy(&request.parameters) {
            &request.parameters
        } else {
            &empty
        };
        self.validate_schema(parameters, &service.input_schema)?;
        self.validate_content_items(&request.inputs)?;
        let mut auth_service = service.clone();
        auth_service.auth = resolve_auth();
        self.validate_service_auth(request, &auth_service, roles)?;
        require_capability(self.rbac, roles, &[], Capability::AgentInvoke)
            .map_err(RequestValidationError::PermissionDenied)?;
        Ok(())
    }

    pub fn validate_required_fields(
        &self,
        request: &ValidationRequest,
    ) -> Result<(), RequestValidationError> {
        if request.service_id.is_empty() {
            return Err(RequestValidationError::ValidationFailed(
                "service_id is required".into(),
            ));
        }
        if request.inputs.is_empty() {
            return Err(RequestValidationError::ValidationFailed(
                "inputs is required".into(),
            ));
        }
        Ok(())
    }

    pub fn validate_content_types(
        &self,
        inputs: &[ContentItem],
        service: &ValidationService,
    ) -> Result<(), RequestValidationError> {
        if service.accepted_content_types.is_empty() {
            return Ok(());
        }
        for item in inputs {
            if !service.accepted_content_types.contains(&item.content_type) {
                let mut accepted = Vec::new();
                for content_type in &service.accepted_content_types {
                    if !accepted.contains(content_type) {
                        accepted.push(*content_type);
                    }
                }
                return Err(RequestValidationError::InvalidContentType {
                    content_type: item.content_type,
                    accepted,
                });
            }
        }
        Ok(())
    }

    pub fn validate_schema(
        &self,
        parameters: &Value,
        schema: &Value,
    ) -> Result<(), RequestValidationError> {
        if !python_truthy(schema) {
            return Ok(());
        }
        let decoded;
        let schema = if let Value::String(serialized) = schema {
            let Ok(value) = serde_json::from_str::<Value>(serialized) else {
                return Ok(());
            };
            decoded = value;
            &decoded
        } else {
            schema
        };
        if !schema.is_object() {
            return Ok(());
        }
        // Python validator_for falls back to its latest draft for an unknown
        // $schema. Removing that root annotation prevents the Rust registry from
        // trying to retrieve the unknown dialect. Do not reinterpret Draft 3,
        // which Python recognizes but this engine does not implement.
        let normalized;
        let schema = if schema
            .get("$schema")
            .and_then(Value::as_str)
            .is_some_and(|dialect| {
                dialect.trim_end_matches('#') != "http://json-schema.org/draft-03/schema"
            })
            && jsonschema::Draft::Draft202012.detect(schema).is_err()
        {
            let mut object = schema.as_object().expect("schema is an object").clone();
            object.remove("$schema");
            normalized = Value::Object(object);
            &normalized
        } else {
            schema
        };
        let validator = jsonschema::options()
            .should_validate_formats(false)
            .without_content_encoding_support("base64")
            .without_content_media_type_support("application/json")
            .with_retriever(DenyExternalRetrieval)
            .build(schema)
            .map_err(|error| RequestValidationError::SchemaConfiguration(error.to_string()))?;
        validator
            .validate(parameters)
            .map_err(|error| RequestValidationError::ValidationFailed(error.to_string()))
    }

    pub fn validate_content_items(
        &self,
        inputs: &[ContentItem],
    ) -> Result<(), RequestValidationError> {
        for item in inputs {
            let has_data = item
                .data
                .as_ref()
                .is_some_and(|data| !matches!(data, ContentData::Json(Value::Null)));
            if !has_data && item.url.as_deref().is_none_or(str::is_empty) {
                return Err(RequestValidationError::ValidationFailed(
                    "content item must have data or url".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn validate_service_auth(
        &self,
        request: &ValidationRequest,
        service: &ValidationService,
        roles: &[String],
    ) -> Result<(), RequestValidationError> {
        let auth = &service.auth;
        if !auth.enabled || auth.public {
            return Ok(());
        }
        if auth.require_auth {
            let user_id = request.user_id.as_deref().unwrap_or_default();
            let is_anon = user_id.starts_with("anon:")
                || (!roles.is_empty() && roles.iter().all(|role| role == "guest"));
            if is_anon || user_id.is_empty() {
                return Err(RequestValidationError::AuthenticationRequired(format!(
                    "服务 {} 需要鉴权",
                    service.service_id,
                )));
            }
        }
        if !auth.allowed_roles.is_empty()
            && !roles.iter().any(|role| auth.allowed_roles.contains(role))
        {
            return Err(RequestValidationError::PermissionDenied(format!(
                "服务 {} 需要以下角色之一: {}",
                service.service_id,
                python_string_list(auth.allowed_roles.iter().map(String::as_str)),
            )));
        }
        Ok(())
    }
}

struct DenyExternalRetrieval;
impl jsonschema::Retrieve for DenyExternalRetrieval {
    fn retrieve(
        &self,
        _uri: &jsonschema::Uri<String>,
    ) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        Err("external JSON Schema retrieval is disabled".into())
    }
}

fn python_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn python_string_list<'a>(values: impl Iterator<Item = &'a str>) -> String {
    let values: Vec<String> = values
        .map(|value| {
            let quote = if value.contains('\'') && !value.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut result = String::from(quote);
            for character in value.chars() {
                match character {
                    '\\' => result.push_str("\\\\"),
                    '\n' => result.push_str("\\n"),
                    '\r' => result.push_str("\\r"),
                    '\t' => result.push_str("\\t"),
                    c if c == quote => {
                        result.push('\\');
                        result.push(c);
                    }
                    c if c.is_control() && (c as u32) < 256 => {
                        result.push_str(&format!("\\x{:02x}", c as u32));
                    }
                    c => result.push(c),
                }
            }
            result.push(quote);
            result
        })
        .collect();
    format!("[{}]", values.join(", "))
}

