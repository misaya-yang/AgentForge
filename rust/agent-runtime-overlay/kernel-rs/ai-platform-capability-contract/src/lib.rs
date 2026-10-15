//! Versioned, fail-closed wire contract shared by the Agent Runtime and the
//! capability worker. This crate contains no transport, storage, provider, or
//! Agent-loop implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

pub const CAPABILITY_DESCRIPTOR_SCHEMA_VERSION: &str = "capability-descriptor/v2";
pub const RUNTIME_CAPABILITY_LEASE_SCHEMA_VERSION: &str = "runtime-capability-lease/v1";
pub const CAPABILITY_EXECUTION_SCHEMA_VERSION: &str = "capability-execution/v2";
pub const CAPABILITY_CATALOG_SCHEMA_VERSION: &str = "capability-catalog/v2";
pub const CAPABILITY_EVENT_SCHEMA_VERSION: &str = "capability-event/v2";
pub const MAX_CAPABILITY_LEASE_TTL_MS: u64 = 120_000;
/// Schema recursion and composition are deliberately bounded.  Capability
/// schemas are supplied by the control plane, so these limits are part of the
/// trust boundary rather than an implementation detail.
const MAX_SCHEMA_DEPTH: usize = 32;
const MAX_SCHEMA_BRANCHES: usize = 16;

type HmacSha256 = Hmac<Sha256>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityEffect {
    Read,
    Write,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalPolicy {
    Never,
    OnRequest,
    Always,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Inline,
    Stream,
    Job,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityExecutionStatus {
    Published,
    AwaitingApproval,
    Dispatched,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Timeout,
    SideEffectUnknown,
}

impl CapabilityExecutionStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::Cancelled
                | Self::Timeout
                | Self::SideEffectUnknown
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDescriptorV2 {
    pub schema_version: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub schema_hash: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub effect: CapabilityEffect,
    pub approval_policy: ApprovalPolicy,
    pub execution_mode: ExecutionMode,
    pub timeout_ms: u64,
    #[serde(default)]
    pub tags: Vec<String>,
    pub protocol: String,
    /// Non-secret, snapshot-bound connector identity.  The Worker verifies
    /// this value against PostgreSQL before any argument validation or
    /// dispatch; credentials never appear in the descriptor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector_binding: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityScopeV2 {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeCapabilityLeaseV1 {
    pub schema_version: String,
    pub lease_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub attempt_id: String,
    pub capability_id: String,
    pub capability_revision: u64,
    pub arguments_hash: String,
    pub effect: CapabilityEffect,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_id: Option<String>,
    pub issued_at_epoch_ms: u64,
    pub expires_at_epoch_ms: u64,
    pub nonce: String,
    pub signature: String,
}

impl RuntimeCapabilityLeaseV1 {
    pub fn scope(&self) -> CapabilityScopeV2 {
        CapabilityScopeV2 {
            tenant_id: self.tenant_id.clone(),
            user_id: self.user_id.clone(),
            session_id: self.session_id.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityExecutionV2 {
    pub schema_version: String,
    pub execution_id: String,
    pub lease_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub attempt_id: String,
    pub capability_id: String,
    pub capability_revision: u64,
    pub arguments_hash: String,
    pub idempotency_key: String,
    pub effect: CapabilityEffect,
    pub status: CapabilityExecutionStatus,
    pub events_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityCatalogRequestV2 {
    pub schema_version: String,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub capability_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityCatalogV2 {
    pub schema_version: String,
    pub capability_revision: u64,
    pub capabilities: Vec<CapabilityDescriptorV2>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateCapabilityExecutionRequestV2 {
    pub schema_version: String,
    pub lease: RuntimeCapabilityLeaseV1,
    pub idempotency_key: String,
    /// The descriptor resolved by Runtime from the immutable capability
    /// snapshot.  Worker treats this as a claim and verifies it against the
    /// static registry or the PostgreSQL snapshot before dispatch.
    pub descriptor: CapabilityDescriptorV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector_binding: Option<Value>,
    pub arguments: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityEventV2 {
    pub schema_version: String,
    pub execution_id: String,
    pub tool_call_id: String,
    pub sequence: u64,
    pub event: String,
    pub status: CapabilityExecutionStatus,
    #[serde(default)]
    pub payload: BTreeMap<String, Value>,
    pub created_at_epoch_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityEventPageV2 {
    pub schema_version: String,
    pub execution_id: String,
    pub after_sequence: u64,
    pub next_sequence: u64,
    pub has_more: bool,
    pub events: Vec<CapabilityEventV2>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ContractError {
    InvalidField(&'static str),
    SchemaMismatch(&'static str),
    UnsupportedSchemaKeyword(String),
    Expired,
    BindingMismatch(&'static str),
    InvalidSignature,
}

impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField(field) => write!(formatter, "invalid_{field}"),
            Self::SchemaMismatch(field) => write!(formatter, "schema_mismatch_{field}"),
            Self::UnsupportedSchemaKeyword(keyword) => {
                write!(formatter, "unsupported_schema_keyword_{keyword}")
            }
            Self::Expired => formatter.write_str("lease_expired"),
            Self::BindingMismatch(field) => {
                write!(formatter, "lease_binding_mismatch_{field}")
            }
            Self::InvalidSignature => formatter.write_str("lease_signature_invalid"),
        }
    }
}

impl std::error::Error for ContractError {}

fn bounded_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && !value.bytes().any(|byte| byte.is_ascii_control())
}

fn bounded_description(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_control() && byte != b'\n')
}

fn identifier(value: &str, maximum: usize) -> bool {
    bounded_text(value, maximum)
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
        })
}

fn sha256(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_scope(scope: &CapabilityScopeV2) -> Result<(), ContractError> {
    for (field, value) in [
        ("tenant_id", scope.tenant_id.as_str()),
        ("user_id", scope.user_id.as_str()),
        ("session_id", scope.session_id.as_str()),
    ] {
        if !bounded_text(value, 255) {
            return Err(ContractError::InvalidField(field));
        }
    }
    Ok(())
}

impl CapabilityDescriptorV2 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != CAPABILITY_DESCRIPTOR_SCHEMA_VERSION {
            return Err(ContractError::SchemaMismatch("descriptor"));
        }
        if !identifier(&self.id, 160) {
            return Err(ContractError::InvalidField("capability_id"));
        }
        for (field, value, maximum) in [
            ("name", self.name.as_str(), 160),
            ("version", self.version.as_str(), 64),
            ("protocol", self.protocol.as_str(), 64),
        ] {
            if !bounded_text(value, maximum) {
                return Err(ContractError::InvalidField(field));
            }
        }
        if !bounded_description(&self.description, 2_000) {
            return Err(ContractError::InvalidField("description"));
        }
        if !sha256(&self.schema_hash)
            || !self.input_schema.is_object()
            || !self.output_schema.is_object()
        {
            return Err(ContractError::InvalidField("schema"));
        }
        if canonical_json_hash(&self.input_schema)? != self.schema_hash {
            return Err(ContractError::BindingMismatch("schema_hash"));
        }
        validate_json_schema(&self.input_schema)?;
        if !(1..=3_600_000).contains(&self.timeout_ms) {
            return Err(ContractError::InvalidField("timeout_ms"));
        }
        let mut tags = self.tags.clone();
        tags.sort_unstable();
        if self.tags.len() > 32
            || self.tags.iter().any(|tag| !identifier(tag, 80))
            || tags.windows(2).any(|pair| pair[0] == pair[1])
        {
            return Err(ContractError::InvalidField("tags"));
        }
        if !matches!(self.effect, CapabilityEffect::Read)
            && matches!(self.approval_policy, ApprovalPolicy::Never)
        {
            return Err(ContractError::InvalidField("approval_policy"));
        }
        if self.connector_binding.as_ref().is_some_and(|binding| {
            !binding.is_object()
                || serde_json::to_vec(binding).map_or(true, |encoded| encoded.len() > 16 * 1024)
        }) {
            return Err(ContractError::InvalidField("connector_binding"));
        }
        Ok(())
    }
}

impl CapabilityCatalogRequestV2 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != CAPABILITY_CATALOG_SCHEMA_VERSION {
            return Err(ContractError::SchemaMismatch("catalog_request"));
        }
        validate_scope(&CapabilityScopeV2 {
            tenant_id: self.tenant_id.clone(),
            user_id: self.user_id.clone(),
            session_id: self.session_id.clone(),
        })?;
        if self.capability_revision == 0 {
            return Err(ContractError::InvalidField("capability_revision"));
        }
        Ok(())
    }
}

impl RuntimeCapabilityLeaseV1 {
    pub fn validate(&self, now_epoch_ms: u64) -> Result<(), ContractError> {
        if self.schema_version != RUNTIME_CAPABILITY_LEASE_SCHEMA_VERSION {
            return Err(ContractError::SchemaMismatch("lease"));
        }
        validate_scope(&self.scope())?;
        for (field, value, maximum) in [
            ("lease_id", self.lease_id.as_str(), 64),
            ("run_id", self.run_id.as_str(), 64),
            ("tool_call_id", self.tool_call_id.as_str(), 160),
            ("attempt_id", self.attempt_id.as_str(), 160),
            ("capability_id", self.capability_id.as_str(), 160),
        ] {
            if !identifier(value, maximum) {
                return Err(ContractError::InvalidField(field));
            }
        }
        if self.capability_revision == 0 || !sha256(&self.arguments_hash) {
            return Err(ContractError::InvalidField("lease_binding"));
        }
        if self.issued_at_epoch_ms >= self.expires_at_epoch_ms
            || now_epoch_ms >= self.expires_at_epoch_ms
            || self.expires_at_epoch_ms - self.issued_at_epoch_ms > MAX_CAPABILITY_LEASE_TTL_MS
        {
            return Err(ContractError::Expired);
        }
        if !bounded_text(&self.nonce, 128) || self.nonce.len() < 16 || !sha256(&self.signature) {
            return Err(ContractError::InvalidField("lease_proof"));
        }
        match self.effect {
            CapabilityEffect::Read if self.approval_id.is_some() => {
                return Err(ContractError::InvalidField("approval_id"));
            }
            CapabilityEffect::Write | CapabilityEffect::Unknown
                if self
                    .approval_id
                    .as_deref()
                    .is_none_or(|value| !identifier(value, 64)) =>
            {
                return Err(ContractError::InvalidField("approval_id"));
            }
            _ => {}
        }
        Ok(())
    }

    pub fn verify_signature(&self, secret: &[u8]) -> Result<(), ContractError> {
        if secret.len() < 32 || !sha256(&self.signature) {
            return Err(ContractError::InvalidSignature);
        }
        let mut mac =
            HmacSha256::new_from_slice(secret).map_err(|_| ContractError::InvalidSignature)?;
        mac.update(&self.signing_payload()?);
        let signature =
            hex::decode(&self.signature[7..]).map_err(|_| ContractError::InvalidSignature)?;
        mac.verify_slice(&signature)
            .map_err(|_| ContractError::InvalidSignature)
    }

    pub fn sign(&mut self, secret: &[u8]) -> Result<(), ContractError> {
        if secret.len() < 32 {
            return Err(ContractError::InvalidSignature);
        }
        self.signature = format!("sha256:{}", "0".repeat(64));
        let mut mac =
            HmacSha256::new_from_slice(secret).map_err(|_| ContractError::InvalidSignature)?;
        mac.update(&self.signing_payload()?);
        self.signature = format!("sha256:{}", hex::encode(mac.finalize().into_bytes()));
        Ok(())
    }

    fn signing_payload(&self) -> Result<Vec<u8>, ContractError> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        let value =
            serde_json::to_value(unsigned).map_err(|_| ContractError::InvalidField("lease"))?;
        serde_json::to_vec(&canonicalize(&value)).map_err(|_| ContractError::InvalidField("lease"))
    }
}

impl CreateCapabilityExecutionRequestV2 {
    pub fn validate(&self, now_epoch_ms: u64) -> Result<(), ContractError> {
        if self.schema_version != CAPABILITY_EXECUTION_SCHEMA_VERSION {
            return Err(ContractError::SchemaMismatch("execution_request"));
        }
        self.lease.validate(now_epoch_ms)?;
        self.descriptor.validate()?;
        if self.descriptor.id != self.lease.capability_id
            || self.descriptor.effect != self.lease.effect
            || self.descriptor.connector_binding != self.connector_binding
        {
            return Err(ContractError::BindingMismatch("descriptor"));
        }
        if !identifier(&self.idempotency_key, 160) || !self.arguments.is_object() {
            return Err(ContractError::InvalidField("execution_request"));
        }
        if canonical_json_hash(&self.arguments)? != self.lease.arguments_hash {
            return Err(ContractError::BindingMismatch("arguments_hash"));
        }
        Ok(())
    }
}

impl CapabilityExecutionV2 {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema_version != CAPABILITY_EXECUTION_SCHEMA_VERSION {
            return Err(ContractError::SchemaMismatch("execution"));
        }
        validate_scope(&CapabilityScopeV2 {
            tenant_id: self.tenant_id.clone(),
            user_id: self.user_id.clone(),
            session_id: self.session_id.clone(),
        })?;
        for (field, value, maximum) in [
            ("execution_id", self.execution_id.as_str(), 64),
            ("lease_id", self.lease_id.as_str(), 64),
            ("run_id", self.run_id.as_str(), 64),
            ("tool_call_id", self.tool_call_id.as_str(), 160),
            ("attempt_id", self.attempt_id.as_str(), 160),
            ("capability_id", self.capability_id.as_str(), 160),
            ("idempotency_key", self.idempotency_key.as_str(), 160),
        ] {
            if !identifier(value, maximum) {
                return Err(ContractError::InvalidField(field));
            }
        }
        if self.capability_revision == 0
            || !sha256(&self.arguments_hash)
            || !self
                .events_url
                .starts_with("/internal/v2/capabilities/executions/")
            || self.events_url.contains("://")
        {
            return Err(ContractError::InvalidField("execution_binding"));
        }
        if !self.status.is_terminal() && (self.result.is_some() || self.error.is_some()) {
            return Err(ContractError::InvalidField("terminal_payload"));
        }
        Ok(())
    }
}

/// Validate the intentionally small JSON Schema subset accepted by capability
/// descriptors.  This is a schema validator, not a general JSON Schema
/// implementation: accepting a keyword here means that the worker may rely on
/// it, so unknown keywords fail closed.
pub fn validate_json_schema(schema: &Value) -> Result<(), ContractError> {
    validate_schema_node(schema, 0)
}

/// Validate an instance against the supported schema subset.  Calling this
/// entry point also validates the schema itself, so callers cannot accidentally
/// use an unsupported keyword with a permissive instance path.
pub fn validate_json_value(schema: &Value, value: &Value) -> Result<(), ContractError> {
    validate_json_schema(schema)?;
    validate_value_node(schema, value, 0)
}

fn validate_value_node(schema: &Value, value: &Value, depth: usize) -> Result<(), ContractError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(ContractError::InvalidField("schema_depth"));
    }
    let object = schema
        .as_object()
        .ok_or(ContractError::InvalidField("input_schema"))?;

    validate_composition_value(object, value, depth)?;

    if let Some(expected) = object.get("type").and_then(Value::as_str) {
        let matches = match expected {
            "object" => value.is_object(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "array" => value.is_array(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        };
        if !matches {
            return Err(ContractError::InvalidField("value_type"));
        }
    }

    if let Some(enumeration) = object.get("enum").and_then(Value::as_array) {
        let value_hash = canonical_json_hash_any(value)?;
        if !enumeration
            .iter()
            .map(canonical_json_hash_any)
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .any(|candidate| candidate == &value_hash)
        {
            return Err(ContractError::InvalidField("enum"));
        }
    }

    if value.is_object()
        && (object.get("type").and_then(Value::as_str) == Some("object")
            || object.get("type").is_none()
                && (object.contains_key("required")
                    || object.contains_key("properties")
                    || object.contains_key("additionalProperties")))
    {
        validate_object_value(object, value, depth)?;
    }
    if value.is_string()
        && (object.get("type").and_then(Value::as_str) == Some("string")
            || object.get("type").is_none()
                && (object.contains_key("minLength")
                    || object.contains_key("maxLength")
                    || object.contains_key("pattern")))
    {
        validate_string_value(
            object,
            value
                .as_str()
                .ok_or(ContractError::InvalidField("value_type"))?,
        )?;
    }
    if value.is_number()
        && matches!(
            object.get("type").and_then(Value::as_str),
            Some("number" | "integer")
        )
        || value.is_number()
            && object.get("type").is_none()
            && (object.contains_key("minimum")
                || object.contains_key("maximum")
                || object.contains_key("exclusiveMinimum")
                || object.contains_key("exclusiveMaximum"))
    {
        validate_number_value(object, value)?;
    }
    if value.is_array()
        && (object.get("type").and_then(Value::as_str) == Some("array")
            || object.get("type").is_none()
                && (object.contains_key("minItems")
                    || object.contains_key("maxItems")
                    || object.contains_key("uniqueItems")
                    || object.contains_key("items")))
    {
        validate_array_value(
            object,
            value
                .as_array()
                .ok_or(ContractError::InvalidField("value_type"))?,
            depth,
        )?;
    }
    Ok(())
}

fn validate_composition_value(
    schema: &Map<String, Value>,
    value: &Value,
    depth: usize,
) -> Result<(), ContractError> {
    if let Some(branches) = schema.get("anyOf") {
        let branches = branches
            .as_array()
            .ok_or(ContractError::InvalidField("anyOf"))?;
        let matches = branches
            .iter()
            .filter(|branch| validate_value_node(branch, value, depth + 1).is_ok())
            .count();
        if matches == 0 {
            return Err(ContractError::InvalidField("anyOf"));
        }
    }
    if let Some(branches) = schema.get("oneOf") {
        let branches = branches
            .as_array()
            .ok_or(ContractError::InvalidField("oneOf"))?;
        let matches = branches
            .iter()
            .filter(|branch| validate_value_node(branch, value, depth + 1).is_ok())
            .count();
        if matches != 1 {
            return Err(ContractError::InvalidField("oneOf"));
        }
    }
    if let Some(branch) = schema.get("not")
        && validate_value_node(branch, value, depth + 1).is_ok()
    {
        return Err(ContractError::InvalidField("not"));
    }
    Ok(())
}

fn validate_object_value(
    schema: &Map<String, Value>,
    value: &Value,
    depth: usize,
) -> Result<(), ContractError> {
    let properties = schema.get("properties").and_then(Value::as_object);
    let value = value
        .as_object()
        .ok_or(ContractError::InvalidField("value_type"))?;
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            if !value.contains_key(name) {
                return Err(ContractError::InvalidField("required"));
            }
        }
    }
    for (name, property_value) in value {
        if let Some(property_schema) = properties.and_then(|properties| properties.get(name)) {
            validate_value_node(property_schema, property_value, depth + 1)?;
        } else if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
            return Err(ContractError::InvalidField("additionalProperties"));
        }
    }
    Ok(())
}

fn validate_string_value(schema: &Map<String, Value>, value: &str) -> Result<(), ContractError> {
    let length = value.chars().count() as u64;
    if schema
        .get("minLength")
        .and_then(Value::as_u64)
        .is_some_and(|minimum| length < minimum)
        || schema
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| length > maximum)
    {
        return Err(ContractError::InvalidField("string_length"));
    }
    if let Some(pattern) = schema.get("pattern").and_then(Value::as_str) {
        let parsed = parse_safe_pattern(pattern).ok_or(ContractError::InvalidField("pattern"))?;
        if !pattern_matches(&parsed, value) {
            return Err(ContractError::InvalidField("pattern"));
        }
    }
    Ok(())
}

fn validate_number_value(schema: &Map<String, Value>, value: &Value) -> Result<(), ContractError> {
    let number = value
        .as_f64()
        .ok_or(ContractError::InvalidField("value_type"))?;
    if schema.get("type").and_then(Value::as_str) == Some("integer")
        && (value.as_i64().is_none() && value.as_u64().is_none())
    {
        return Err(ContractError::InvalidField("value_type"));
    }
    if schema
        .get("minimum")
        .and_then(Value::as_f64)
        .is_some_and(|minimum| number < minimum)
        || schema
            .get("maximum")
            .and_then(Value::as_f64)
            .is_some_and(|maximum| number > maximum)
        || schema
            .get("exclusiveMinimum")
            .and_then(Value::as_f64)
            .is_some_and(|minimum| number <= minimum)
        || schema
            .get("exclusiveMaximum")
            .and_then(Value::as_f64)
            .is_some_and(|maximum| number >= maximum)
    {
        return Err(ContractError::InvalidField("number_bound"));
    }
    Ok(())
}

fn validate_array_value(
    schema: &Map<String, Value>,
    value: &[Value],
    depth: usize,
) -> Result<(), ContractError> {
    let length = value.len() as u64;
    if schema
        .get("minItems")
        .and_then(Value::as_u64)
        .is_some_and(|minimum| length < minimum)
        || schema
            .get("maxItems")
            .and_then(Value::as_u64)
            .is_some_and(|maximum| length > maximum)
    {
        return Err(ContractError::InvalidField("array_length"));
    }
    if schema.get("uniqueItems").and_then(Value::as_bool) == Some(true) {
        let mut hashes = value
            .iter()
            .map(canonical_json_hash_any)
            .collect::<Result<Vec<_>, _>>()?;
        hashes.sort_unstable();
        if hashes.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ContractError::InvalidField("uniqueItems"));
        }
    }
    if let Some(items) = schema.get("items") {
        for item in value {
            validate_value_node(items, item, depth + 1)?;
        }
    }
    Ok(())
}

fn validate_schema_node(schema: &Value, depth: usize) -> Result<(), ContractError> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err(ContractError::InvalidField("schema_depth"));
    }
    let object = schema
        .as_object()
        .ok_or(ContractError::InvalidField("input_schema"))?;
    if object.is_empty() {
        return Err(ContractError::InvalidField("input_schema"));
    }

    for keyword in object.keys() {
        if !matches!(
            keyword.as_str(),
            "type"
                | "title"
                | "description"
                | "required"
                | "properties"
                | "additionalProperties"
                | "minLength"
                | "maxLength"
                | "pattern"
                | "minimum"
                | "maximum"
                | "exclusiveMinimum"
                | "exclusiveMaximum"
                | "minItems"
                | "maxItems"
                | "uniqueItems"
                | "items"
                | "enum"
                | "anyOf"
                | "oneOf"
                | "not"
        ) {
            return Err(ContractError::UnsupportedSchemaKeyword(keyword.clone()));
        }
    }

    let declared_type = match object.get("type") {
        Some(Value::String(value)) if is_supported_type(value) => Some(value.as_str()),
        Some(Value::String(_)) => return Err(ContractError::InvalidField("schema_type")),
        Some(_) => return Err(ContractError::InvalidField("schema_type")),
        None => None,
    };

    if let Some(value) = object.get("enum") {
        validate_enum(value)?;
    }

    if let Some(value) = object.get("title") {
        let value = value.as_str().ok_or(ContractError::InvalidField("title"))?;
        if !bounded_text(value, 2_000) {
            return Err(ContractError::InvalidField("title"));
        }
    }
    if let Some(value) = object.get("description") {
        let value = value
            .as_str()
            .ok_or(ContractError::InvalidField("description"))?;
        if !bounded_description(value, 2_000) {
            return Err(ContractError::InvalidField("description"));
        }
    }

    for keyword in ["anyOf", "oneOf"] {
        if let Some(value) = object.get(keyword) {
            let branches = value
                .as_array()
                .ok_or(ContractError::InvalidField(keyword))?;
            if branches.is_empty() || branches.len() > MAX_SCHEMA_BRANCHES {
                return Err(ContractError::InvalidField(keyword));
            }
            for branch in branches {
                validate_schema_node(branch, depth + 1)?;
            }
        }
    }
    if let Some(branch) = object.get("not") {
        validate_schema_node(branch, depth + 1)?;
    }

    let has_object_keywords = object.contains_key("required")
        || object.contains_key("properties")
        || object.contains_key("additionalProperties");
    if matches!(declared_type, Some("object")) || (declared_type.is_none() && has_object_keywords) {
        validate_object_keywords(object, depth)?;
    } else if has_object_keywords {
        return Err(ContractError::InvalidField("object_schema_keyword"));
    }

    let has_string_keywords = object.contains_key("minLength")
        || object.contains_key("maxLength")
        || object.contains_key("pattern");
    if matches!(declared_type, Some("string")) || (declared_type.is_none() && has_string_keywords) {
        validate_string_keywords(object)?;
    } else if has_string_keywords {
        return Err(ContractError::InvalidField("string_schema_keyword"));
    }

    let has_number_keywords = object.contains_key("minimum")
        || object.contains_key("maximum")
        || object.contains_key("exclusiveMinimum")
        || object.contains_key("exclusiveMaximum");
    if matches!(declared_type, Some("number" | "integer"))
        || (declared_type.is_none() && has_number_keywords)
    {
        validate_number_keywords(object, declared_type)?;
    } else if has_number_keywords {
        return Err(ContractError::InvalidField("number_schema_keyword"));
    }

    let has_array_keywords = object.contains_key("minItems")
        || object.contains_key("maxItems")
        || object.contains_key("uniqueItems")
        || object.contains_key("items");
    if matches!(declared_type, Some("array")) || (declared_type.is_none() && has_array_keywords) {
        validate_array_keywords(object, depth)?;
    } else if has_array_keywords {
        return Err(ContractError::InvalidField("array_schema_keyword"));
    }

    Ok(())
}

fn is_supported_type(value: &str) -> bool {
    matches!(
        value,
        "object" | "string" | "number" | "integer" | "array" | "boolean" | "null"
    )
}

fn validate_object_keywords(
    object: &Map<String, Value>,
    depth: usize,
) -> Result<(), ContractError> {
    let properties = object.get("properties").and_then(Value::as_object);
    if let Some(required) = object.get("required") {
        let required = required
            .as_array()
            .ok_or(ContractError::InvalidField("required"))?;
        if depth == 0 && !required.is_empty() && properties.is_none() {
            return Err(ContractError::InvalidField("required"));
        }
        let mut names = BTreeSet::new();
        for value in required {
            let name = value
                .as_str()
                .ok_or(ContractError::InvalidField("required"))?;
            if !bounded_text(name, 255) || !names.insert(name) {
                return Err(ContractError::InvalidField("required"));
            }
        }
    }

    if object.contains_key("properties") && properties.is_none() {
        return Err(ContractError::InvalidField("properties"));
    }
    if let Some(properties) = properties {
        if properties.len() > 256 {
            return Err(ContractError::InvalidField("properties"));
        }
        for (name, schema) in properties {
            if !bounded_text(name, 255) {
                return Err(ContractError::InvalidField("property_name"));
            }
            validate_schema_node(schema, depth + 1)?;
        }
        if let Some(required) = object.get("required").and_then(Value::as_array)
            && required.iter().any(|name| {
                name.as_str()
                    .is_some_and(|name| !properties.contains_key(name))
            })
        {
            return Err(ContractError::InvalidField("required"));
        }
    }

    if let Some(additional) = object.get("additionalProperties")
        && !additional.is_boolean()
    {
        return Err(ContractError::InvalidField("additionalProperties"));
    }
    Ok(())
}

fn validate_string_keywords(object: &Map<String, Value>) -> Result<(), ContractError> {
    let min = schema_bound(object, "minLength")?;
    let max = schema_bound(object, "maxLength")?;
    if min.zip(max).is_some_and(|(min, max)| min > max) {
        return Err(ContractError::InvalidField("string_length"));
    }
    if let Some(pattern) = object.get("pattern") {
        let pattern = pattern
            .as_str()
            .ok_or(ContractError::InvalidField("pattern"))?;
        if parse_safe_pattern(pattern).is_none() {
            return Err(ContractError::InvalidField("pattern"));
        }
    }
    Ok(())
}

fn validate_number_keywords(
    object: &Map<String, Value>,
    declared_type: Option<&str>,
) -> Result<(), ContractError> {
    let bounds = ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"];
    let mut values = BTreeMap::new();
    for keyword in bounds {
        if let Some(value) = object.get(keyword) {
            let number = value
                .as_f64()
                .ok_or(ContractError::InvalidField("number_bound"))?;
            if !number.is_finite() || (declared_type == Some("integer") && number.fract() != 0.0) {
                return Err(ContractError::InvalidField("number_bound"));
            }
            values.insert(keyword, number);
        }
    }
    if values
        .get("minimum")
        .zip(values.get("maximum"))
        .is_some_and(|(min, max)| min > max)
        || values
            .get("exclusiveMinimum")
            .zip(values.get("exclusiveMaximum"))
            .is_some_and(|(min, max)| min >= max)
    {
        return Err(ContractError::InvalidField("number_bound"));
    }
    Ok(())
}

fn validate_array_keywords(object: &Map<String, Value>, depth: usize) -> Result<(), ContractError> {
    let min = schema_bound(object, "minItems")?;
    let max = schema_bound(object, "maxItems")?;
    if min.zip(max).is_some_and(|(min, max)| min > max) {
        return Err(ContractError::InvalidField("array_length"));
    }
    if let Some(unique) = object.get("uniqueItems")
        && !unique.is_boolean()
    {
        return Err(ContractError::InvalidField("uniqueItems"));
    }
    if let Some(items) = object.get("items") {
        if !items.is_object() {
            return Err(ContractError::InvalidField("items"));
        }
        validate_schema_node(items, depth + 1)?;
    }
    Ok(())
}

fn schema_bound(
    object: &Map<String, Value>,
    keyword: &'static str,
) -> Result<Option<u64>, ContractError> {
    object
        .get(keyword)
        .map(|value| {
            let value = value.as_u64().ok_or(ContractError::InvalidField(keyword))?;
            if value > 1_000_000 {
                return Err(ContractError::InvalidField(keyword));
            }
            Ok(value)
        })
        .transpose()
}

fn validate_enum(value: &Value) -> Result<(), ContractError> {
    let values = value
        .as_array()
        .ok_or(ContractError::InvalidField("enum"))?;
    if values.is_empty() || values.len() > 256 {
        return Err(ContractError::InvalidField("enum"));
    }
    let mut hashes = values
        .iter()
        .map(canonical_json_hash_any)
        .collect::<Result<Vec<_>, _>>()?;
    hashes.sort_unstable();
    if hashes.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(ContractError::InvalidField("enum"));
    }
    Ok(())
}

fn canonical_json_hash_any(value: &Value) -> Result<String, ContractError> {
    let bytes = serde_json::to_vec(&canonicalize(value))
        .map_err(|_| ContractError::InvalidField("enum"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[derive(Clone, Debug)]
enum PatternAtom {
    Literal(char),
    Any,
    Class {
        ranges: Vec<(char, char)>,
        negated: bool,
    },
    Digit,
    NotDigit,
    Word,
    NotWord,
    Space,
    NotSpace,
}

#[derive(Clone, Debug)]
struct PatternToken {
    atom: PatternAtom,
    minimum: usize,
    maximum: Option<usize>,
}

#[derive(Clone, Debug)]
struct SafePattern {
    tokens: Vec<PatternToken>,
    anchored_start: bool,
    anchored_end: bool,
}

fn parse_safe_pattern(pattern: &str) -> Option<SafePattern> {
    if pattern.is_empty()
        || pattern.len() > 256
        || pattern.bytes().any(|byte| byte.is_ascii_control())
    {
        return None;
    }
    let chars: Vec<char> = pattern.chars().collect();
    let mut index = 0;
    let mut anchored_start = false;
    let mut anchored_end = false;
    let mut tokens = Vec::new();

    while index < chars.len() {
        if index == 0 && chars[index] == '^' {
            anchored_start = true;
            index += 1;
            continue;
        }
        if index + 1 == chars.len() && chars[index] == '$' {
            anchored_end = true;
            index += 1;
            continue;
        }
        let atom = match chars[index] {
            '[' => {
                index += 1;
                let negated = chars.get(index) == Some(&'^');
                if negated {
                    index += 1;
                }
                let mut ranges = Vec::new();
                while index < chars.len() && chars[index] != ']' {
                    let start = parse_pattern_class_char(&chars, &mut index)?;
                    if chars.get(index) == Some(&'-') && chars.get(index + 1) != Some(&']') {
                        index += 1;
                        let end = parse_pattern_class_char(&chars, &mut index)?;
                        if start > end {
                            return None;
                        }
                        ranges.push((start, end));
                    } else {
                        ranges.push((start, start));
                    }
                }
                if chars.get(index) != Some(&']') || ranges.is_empty() {
                    return None;
                }
                index += 1;
                PatternAtom::Class { ranges, negated }
            }
            '\\' => parse_pattern_escape(&chars, &mut index)?,
            '.' => {
                index += 1;
                PatternAtom::Any
            }
            '(' | ')' | '|' | '{' | '}' => return None,
            value => {
                if value.is_control() {
                    return None;
                }
                index += 1;
                PatternAtom::Literal(value)
            }
        };

        let (minimum, maximum) = match chars.get(index).copied() {
            Some('*') => {
                index += 1;
                (0, None)
            }
            Some('+') => {
                index += 1;
                (1, None)
            }
            Some('?') => {
                index += 1;
                (0, Some(1))
            }
            Some('{') => parse_bounded_quantifier(&chars, &mut index)?,
            _ => (1, Some(1)),
        };
        if matches!(chars.get(index), Some('*' | '+' | '?' | '{')) {
            return None;
        }
        tokens.push(PatternToken {
            atom,
            minimum,
            maximum,
        });
    }
    if tokens.is_empty() && !(anchored_start && anchored_end) {
        return None;
    }
    Some(SafePattern {
        tokens,
        anchored_start,
        anchored_end,
    })
}

fn parse_pattern_class_char(chars: &[char], index: &mut usize) -> Option<char> {
    let value = *chars.get(*index)?;
    if value == '\\' {
        *index += 1;
        let escaped = *chars.get(*index)?;
        *index += 1;
        return match escaped {
            'd' | 'D' | 'w' | 'W' | 's' | 'S' | '.' | '\\' | '-' | ']' | '[' => Some(escaped),
            _ => None,
        };
    }
    *index += 1;
    Some(value)
}

fn parse_pattern_escape(chars: &[char], index: &mut usize) -> Option<PatternAtom> {
    *index += 1;
    let escaped = *chars.get(*index)?;
    *index += 1;
    Some(match escaped {
        'd' => PatternAtom::Digit,
        'D' => PatternAtom::NotDigit,
        'w' => PatternAtom::Word,
        'W' => PatternAtom::NotWord,
        's' => PatternAtom::Space,
        'S' => PatternAtom::NotSpace,
        '.' | '\\' | '-' => PatternAtom::Literal(escaped),
        _ => return None,
    })
}

fn parse_bounded_quantifier(chars: &[char], index: &mut usize) -> Option<(usize, Option<usize>)> {
    *index += 1;
    let minimum = parse_quantifier_number(chars, index)?;
    let maximum = match chars.get(*index) {
        Some('}') => Some(minimum),
        Some(',') => {
            *index += 1;
            let maximum = parse_quantifier_number(chars, index)?;
            if maximum < minimum {
                return None;
            }
            Some(maximum)
        }
        _ => return None,
    };
    if chars.get(*index) != Some(&'}') || maximum.is_some_and(|maximum| maximum > 256) {
        return None;
    }
    *index += 1;
    Some((minimum, maximum))
}

fn parse_quantifier_number(chars: &[char], index: &mut usize) -> Option<usize> {
    let start = *index;
    let mut value = 0usize;
    while let Some(character) = chars.get(*index).copied() {
        if !character.is_ascii_digit() {
            break;
        }
        value = value
            .checked_mul(10)?
            .checked_add(character as usize - '0' as usize)?;
        *index += 1;
    }
    (*index > start).then_some(value)
}

fn pattern_matches(pattern: &SafePattern, value: &str) -> bool {
    let characters: Vec<char> = value.chars().collect();
    let mut positions = BTreeSet::new();
    if pattern.anchored_start {
        positions.insert(0);
    } else {
        positions.extend(0..=characters.len());
    }
    for token in &pattern.tokens {
        let mut next_positions = BTreeSet::new();
        for start in positions {
            let mut frontier = BTreeSet::from([start]);
            let maximum = token
                .maximum
                .unwrap_or_else(|| characters.len().saturating_sub(start));
            for count in 0..=maximum {
                if count >= token.minimum {
                    next_positions.extend(frontier.iter().copied());
                }
                if count == maximum {
                    break;
                }
                frontier = frontier
                    .into_iter()
                    .filter_map(|position| {
                        characters
                            .get(position)
                            .filter(|character| pattern_atom_matches(&token.atom, **character))
                            .map(|_| position + 1)
                    })
                    .collect();
                if frontier.is_empty() {
                    break;
                }
            }
        }
        positions = next_positions;
        if positions.is_empty() {
            return false;
        }
    }
    !pattern.anchored_end || positions.contains(&characters.len())
}

fn pattern_atom_matches(atom: &PatternAtom, value: char) -> bool {
    match atom {
        PatternAtom::Literal(expected) => *expected == value,
        PatternAtom::Any => true,
        PatternAtom::Class { ranges, negated } => {
            let found = ranges
                .iter()
                .any(|(start, end)| (*start..=*end).contains(&value));
            found != *negated
        }
        PatternAtom::Digit => value.is_ascii_digit(),
        PatternAtom::NotDigit => !value.is_ascii_digit(),
        PatternAtom::Word => value.is_ascii_alphanumeric() || value == '_',
        PatternAtom::NotWord => !(value.is_ascii_alphanumeric() || value == '_'),
        PatternAtom::Space => value.is_ascii_whitespace(),
        PatternAtom::NotSpace => !value.is_ascii_whitespace(),
    }
}

pub fn canonical_json_hash(value: &Value) -> Result<String, ContractError> {
    if !value.is_object() {
        return Err(ContractError::InvalidField("arguments"));
    }
    let bytes = serde_json::to_vec(&canonicalize(value))
        .map_err(|_| ContractError::InvalidField("arguments"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), canonicalize(value)))
                .collect::<Map<_, _>>(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        scalar => scalar.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(input_schema: Value) -> CapabilityDescriptorV2 {
        CapabilityDescriptorV2 {
            schema_version: CAPABILITY_DESCRIPTOR_SCHEMA_VERSION.to_string(),
            id: "fixture.echo".to_string(),
            name: "Echo".to_string(),
            version: "1.0.0".to_string(),
            description: "A fixture capability".to_string(),
            schema_hash: canonical_json_hash(&input_schema).unwrap(),
            input_schema,
            output_schema: serde_json::json!({"type": "object"}),
            effect: CapabilityEffect::Read,
            approval_policy: ApprovalPolicy::Never,
            execution_mode: ExecutionMode::Inline,
            timeout_ms: 1_000,
            tags: vec!["fixture".to_string(), "read".to_string()],
            protocol: "internal-v2".to_string(),
            connector_binding: None,
        }
    }

    fn lease(arguments: &Value) -> RuntimeCapabilityLeaseV1 {
        RuntimeCapabilityLeaseV1 {
            schema_version: RUNTIME_CAPABILITY_LEASE_SCHEMA_VERSION.to_string(),
            lease_id: "00000000-0000-0000-0000-000000000001".to_string(),
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
            run_id: "00000000-0000-0000-0000-000000000002".to_string(),
            tool_call_id: "call-a".to_string(),
            attempt_id: "attempt-a".to_string(),
            capability_id: "fixture.echo".to_string(),
            capability_revision: 1,
            arguments_hash: canonical_json_hash(arguments).unwrap(),
            effect: CapabilityEffect::Read,
            approval_id: None,
            issued_at_epoch_ms: 1,
            expires_at_epoch_ms: 100,
            nonce: "nonce-with-16bytes".to_string(),
            signature: String::new(),
        }
    }

    #[test]
    fn canonical_hash_is_independent_of_object_key_order() {
        let left = serde_json::json!({"b": 2, "a": {"z": true, "y": [1, 2]}});
        let right = serde_json::json!({"a": {"y": [1, 2], "z": true}, "b": 2});
        assert_eq!(canonical_json_hash(&left), canonical_json_hash(&right));
    }

    #[test]
    fn lease_signature_binds_scope_and_arguments() {
        let secret = [7_u8; 32];
        let arguments = serde_json::json!({"message": "hello"});
        let mut signed = lease(&arguments);
        signed.sign(&secret).unwrap();
        assert!(signed.verify_signature(&secret).is_ok());
        signed.tenant_id = "tenant-b".to_string();
        assert!(matches!(
            signed.verify_signature(&secret),
            Err(ContractError::InvalidSignature)
        ));
    }

    #[test]
    fn write_lease_requires_bound_approval() {
        let mut write = lease(&serde_json::json!({}));
        write.effect = CapabilityEffect::Write;
        write.signature = format!("sha256:{}", "0".repeat(64));
        assert!(matches!(
            write.validate(2),
            Err(ContractError::InvalidField("approval_id"))
        ));
    }

    #[test]
    fn unknown_fields_fail_closed() {
        let raw = r#"{"schema_version":"capability-catalog/v2","tenant_id":"t","user_id":"u","session_id":"s","capability_revision":1,"extra":true}"#;
        assert!(serde_json::from_str::<CapabilityCatalogRequestV2>(raw).is_err());
    }

    #[test]
    fn signature_matches_the_python_cross_language_fixture() {
        let arguments = serde_json::json!({"b": 2, "a": {"z": true, "y": [1, 2]}});
        let mut value = RuntimeCapabilityLeaseV1 {
            schema_version: RUNTIME_CAPABILITY_LEASE_SCHEMA_VERSION.to_string(),
            lease_id: "lease-a".to_string(),
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
            run_id: "run-a".to_string(),
            tool_call_id: "call-a".to_string(),
            attempt_id: "attempt-a".to_string(),
            capability_id: "fixture.echo".to_string(),
            capability_revision: 3,
            arguments_hash: canonical_json_hash(&arguments).unwrap(),
            effect: CapabilityEffect::Read,
            approval_id: None,
            issued_at_epoch_ms: 1_000,
            expires_at_epoch_ms: 2_000,
            nonce: "nonce-with-16bytes".to_string(),
            signature: String::new(),
        };
        value.sign(&[b's'; 32]).unwrap();
        assert_eq!(
            value.signature,
            "sha256:8da76c35a714b9d32464df841017ce5523fd2f19ec15bb440542e406cc5dd68d"
        );
    }

    #[test]
    fn descriptor_binds_schema_hash_and_requires_stable_tags() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"message": {"type": "string"}},
            "required": ["message"],
            "additionalProperties": false
        });
        assert!(descriptor(schema.clone()).validate().is_ok());

        let mut mismatch = descriptor(schema.clone());
        mismatch.schema_hash = canonical_json_hash(&serde_json::json!({"type": "object"})).unwrap();
        assert!(matches!(
            mismatch.validate(),
            Err(ContractError::BindingMismatch("schema_hash"))
        ));

        let mut duplicate_tags = descriptor(schema);
        duplicate_tags.tags.push("read".to_string());
        assert!(matches!(
            duplicate_tags.validate(),
            Err(ContractError::InvalidField("tags"))
        ));
    }

    #[test]
    fn schema_subset_accepts_nested_object_string_number_and_array() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 80,
                    "pattern": "^[a-zA-Z][a-zA-Z0-9_-]*$"
                },
                "count": {"type": "integer", "minimum": 0, "maximum": 100},
                "items": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 8,
                    "uniqueItems": true,
                    "items": {"type": "string", "enum": ["a", "b"]}
                }
            },
            "required": ["count", "name"],
            "additionalProperties": false
        });
        assert!(validate_json_schema(&schema).is_ok());
    }

    #[test]
    fn schema_subset_rejects_unknown_keywords_and_unsupported_forms() {
        let cases = [
            serde_json::json!({"type": "object", "oneOf": []}),
            serde_json::json!({"type": "object", "properties": {"x": true}}),
            serde_json::json!({"type": "object", "additionalProperties": {}}),
            serde_json::json!({"type": "array", "items": [{"type": "string"}]}),
            serde_json::json!({"type": "string", "pattern": "(?=unsafe)"}),
            serde_json::json!({"type": "string", "minLength": 3, "maxLength": 2}),
            serde_json::json!({"type": "integer", "minimum": 0.5}),
        ];
        for schema in cases {
            assert!(
                validate_json_schema(&schema).is_err(),
                "schema accepted: {schema}"
            );
        }
    }

    #[test]
    fn schema_subset_requires_unique_required_and_enum_values() {
        let unsorted_required = serde_json::json!({
            "type": "object",
            "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
            "required": ["b", "a"]
        });
        assert!(validate_json_schema(&unsorted_required).is_ok());

        let duplicate_required = serde_json::json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "required": ["a", "a"]
        });
        assert!(validate_json_schema(&duplicate_required).is_err());

        let duplicate_enum = serde_json::json!({"type": "string", "enum": ["same", "same"]});
        assert!(validate_json_schema(&duplicate_enum).is_err());

        let missing_required_property = serde_json::json!({
            "type": "object",
            "properties": {"known": {"type": "string"}},
            "required": ["missing"]
        });
        assert!(validate_json_schema(&missing_required_property).is_err());

        let required_without_properties = serde_json::json!({
            "type": "object",
            "required": ["missing"],
            "additionalProperties": false
        });
        assert!(validate_json_schema(&required_without_properties).is_err());
    }

    #[test]
    fn schema_subset_validates_object_string_number_and_array_instances() {
        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "id": {"type": "string", "pattern": "^art_[A-Za-z0-9]{8,64}$"},
                "count": {"type": "integer", "minimum": 1, "maximum": 5},
                "labels": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 2,
                    "uniqueItems": true,
                    "items": {"type": "string", "minLength": 2, "maxLength": 4}
                }
            },
            "required": ["count", "id"],
            "additionalProperties": false
        });
        let valid = serde_json::json!({
            "id": "art_Abc12345",
            "count": 3,
            "labels": ["one", "two"]
        });
        assert!(validate_json_value(&schema, &valid).is_ok());

        for invalid in [
            serde_json::json!({"id": "bad", "count": 3}),
            serde_json::json!({"id": "art_Abc12345", "count": 0}),
            serde_json::json!({"id": "art_Abc12345", "count": 3, "extra": true}),
            serde_json::json!({"id": "art_Abc12345", "count": 3, "labels": ["same", "same"]}),
            serde_json::json!({"id": "art_Abc12345", "labels": ["ok"]}),
        ] {
            assert!(
                validate_json_value(&schema, &invalid).is_err(),
                "value accepted: {invalid}"
            );
        }

        let number_schema = serde_json::json!({
            "type": "number",
            "exclusiveMinimum": 0.0,
            "maximum": 10.5
        });
        assert!(validate_json_value(&number_schema, &serde_json::json!(0.1)).is_ok());
        assert!(validate_json_value(&number_schema, &serde_json::json!(0.0)).is_err());
        assert!(validate_json_value(&number_schema, &serde_json::json!(11)).is_err());
    }

    #[test]
    fn safe_pattern_supports_bounded_quantifiers_but_rejects_unsafe_forms() {
        let schema = serde_json::json!({
            "type": "string",
            "pattern": "^art_[A-Za-z0-9]{8,64}$"
        });
        assert!(validate_json_value(&schema, &serde_json::json!("art_Abc12345")).is_ok());
        assert!(validate_json_value(&schema, &serde_json::json!("art_short")).is_err());

        for pattern in [
            "(unsafe)",
            "(?=unsafe)",
            "(a)\\1",
            "a{8,3}",
            "a{257}",
            "a**",
        ] {
            assert!(validate_json_schema(&serde_json::json!({"pattern": pattern})).is_err());
        }
    }

    #[test]
    fn schema_subset_supports_annotations_and_composition() {
        let schema = serde_json::json!({
            "title": "Either input",
            "description": "A deliberately small composition fixture.",
            "type": "object",
            "properties": {
                "agent_type": {"type": "string"},
                "agent_id": {"type": "string"}
            },
            "oneOf": [
                {
                    "required": ["agent_type"],
                    "not": {"required": ["agent_id"]}
                },
                {
                    "required": ["agent_id"],
                    "not": {"required": ["agent_type"]}
                }
            ],
            "additionalProperties": false
        });
        assert!(validate_json_schema(&schema).is_ok());
        assert!(
            validate_json_value(&schema, &serde_json::json!({"agent_type": "research"})).is_ok()
        );
        assert!(validate_json_value(&schema, &serde_json::json!({"agent_id": "agent-1"})).is_ok());
        assert!(
            validate_json_value(
                &schema,
                &serde_json::json!({"agent_type": "research", "agent_id": "agent-1"})
            )
            .is_err()
        );
        assert!(validate_json_value(&schema, &serde_json::json!({})).is_err());

        let any_of = serde_json::json!({
            "anyOf": [{"required": ["left"]}, {"required": ["right"]}]
        });
        assert!(validate_json_value(&any_of, &serde_json::json!({"right": true})).is_ok());
        assert!(validate_json_value(&any_of, &serde_json::json!({})).is_err());

        assert!(
            validate_json_schema(&serde_json::json!({
                "type": "string",
                "description": "First line.\nSecond line."
            }))
            .is_ok()
        );
        assert!(
            validate_json_schema(&serde_json::json!({
                "type": "string",
                "description": "unsafe\u{0000}description"
            }))
            .is_err()
        );
    }

    #[test]
    fn schema_subset_rejects_ambiguous_composition_and_bounds_complexity() {
        let ambiguous = serde_json::json!({
            "oneOf": [{"type": "object"}, {"required": ["value"]}]
        });
        assert!(validate_json_schema(&ambiguous).is_ok());
        assert!(validate_json_value(&ambiguous, &serde_json::json!({"value": 1})).is_err());

        let too_many_branches = serde_json::json!({
            "anyOf": [
                {"type": "string"}, {"type": "string"}, {"type": "string"},
                {"type": "string"}, {"type": "string"}, {"type": "string"},
                {"type": "string"}, {"type": "string"}, {"type": "string"},
                {"type": "string"}, {"type": "string"}, {"type": "string"},
                {"type": "string"}, {"type": "string"}, {"type": "string"},
                {"type": "string"}, {"type": "string"}
            ]
        });
        assert!(validate_json_schema(&too_many_branches).is_err());

        let mut too_deep = serde_json::json!({"type": "string"});
        for _ in 0..=MAX_SCHEMA_DEPTH {
            too_deep = serde_json::json!({"anyOf": [too_deep]});
        }
        assert!(validate_json_schema(&too_deep).is_err());
        assert!(
            validate_json_schema(&serde_json::json!({
                "type": "string",
                "$ref": "#/definitions/unsafe"
            }))
            .is_err()
        );
    }
}
