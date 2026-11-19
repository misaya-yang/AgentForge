//! Tenant/revision-bound read-only capability projection for the platform host.
//!
//! This is intentionally a data bridge. It does not select a tool from user
//! text, execute a tool, or introduce another Agent loop. The host resolves
//! authorized contributors and supplies their immutable snapshot metadata;
//! this module validates and projects that data for one Agent turn.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt::Display;
use std::fmt::Formatter;

use ai_platform_capability_contract::{
    ApprovalPolicy, CAPABILITY_DESCRIPTOR_SCHEMA_VERSION, CapabilityDescriptorV2, CapabilityEffect,
    ExecutionMode, canonical_json_hash,
};
use serde::Deserialize;
use serde::Serialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

pub const READONLY_CAPABILITY_SCHEMA_VERSION: &str = "agent-readonly-capability/v1";
pub const PLATFORM_CONFIG_SCHEMA_VERSION: &str = "agent-runtime-platform-config/v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeCapabilityScope {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub capability_revision: i64,
    #[serde(default)]
    pub snapshot_id: String,
}

impl RuntimeCapabilityScope {
    pub fn validate(&self) -> Result<(), ReadonlyCapabilityError> {
        if self.tenant_id.is_empty() || self.user_id.is_empty() || self.session_id.is_empty() {
            return Err(ReadonlyCapabilityError::new(
                "runtime_capability_scope_invalid",
            ));
        }
        if self.capability_revision < 1 {
            return Err(ReadonlyCapabilityError::new(
                "runtime_capability_revision_invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadonlyItemKind {
    Context,
    TurnInput,
    Knowledge,
    Attachment,
    Citation,
    Artifact,
    OfficeRead,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilityItem {
    pub item_id: String,
    pub kind: ReadonlyItemKind,
    pub tenant_id: String,
    pub capability_revision: i64,
    pub source: String,
    pub payload: Value,
    #[serde(default = "default_untrusted")]
    pub untrusted: bool,
    #[serde(default = "default_authority")]
    pub authority: String,
}

fn default_untrusted() -> bool {
    true
}

fn default_authority() -> String {
    "non_authoritative".to_string()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    pub name: String,
    pub description: String,
    pub schema: Value,
    pub tenant_id: String,
    pub capability_revision: i64,
    pub source: String,
    #[serde(default = "default_tool_kind")]
    pub kind: String,
    #[serde(default = "default_read_only")]
    pub read_only: bool,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_internal_protocol")]
    pub protocol: String,
    #[serde(default = "default_general_category")]
    pub category: String,
    /// Stable AgentSpec capability identity. These fields are optional for
    /// legacy catalog entries, but dynamic invocation rejects a descriptor
    /// that cannot be bound to the signed allowlist.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_hash: Option<String>,
    #[serde(default, flatten)]
    pub metadata: Map<String, Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorBinding {
    pub binding_type: String,
    pub provider: String,
    pub tool_name: String,
    pub principal_type: Option<String>,
    pub grant_id: Option<String>,
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub schema_hash: Option<String>,
    #[serde(default)]
    pub risk_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_snapshot_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_capability_revision: Option<u64>,
    pub channel: String,
}

impl ConnectorBinding {
    fn validate(&self, expected_tool: &str) -> Result<(), ReadonlyCapabilityError> {
        let valid_channel = matches!(
            self.channel.as_str(),
            "preview" | "hosted" | "hosted_private" | "hosted_public" | "embed" | "api" | "builtin"
        );
        if self.provider.is_empty()
            || self.provider.len() > 128
            || self.provider.bytes().any(|byte| byte.is_ascii_control())
            || self.tool_name != expected_tool
            || !valid_channel
        {
            return Err(ReadonlyCapabilityError::new(
                "runtime_connector_binding_invalid",
            ));
        }
        match self.binding_type.as_str() {
            "catalog"
                if self.provider != "local_node"
                    && self.principal_type.is_none()
                    && self.grant_id.is_none()
                    && self.permission_snapshot_digest.is_none()
                    && self.device_capability_revision.is_none() =>
            {
                Ok(())
            }
            "grant"
                if self.provider == "mcp"
                    && matches!(
                        self.principal_type.as_deref(),
                        Some("service_account") | Some("user_delegated")
                    )
                    && self.grant_id.is_none()
                    && self.connection_id.as_deref().is_some_and(valid_uuid_string)
                    && self
                        .schema_hash
                        .as_deref()
                        .is_some_and(valid_schema_hash_value)
                    && self.risk_level.as_deref().is_some_and(|value| {
                        matches!(value, "low" | "medium" | "high" | "critical")
                    })
                    && self.permission_snapshot_digest.is_none()
                    && self.device_capability_revision.is_none() =>
            {
                Ok(())
            }
            "grant"
                if self.provider == "local_node"
                    && self.tool_name == "local_node_action"
                    && self.principal_type.as_deref() == Some("user_delegated")
                    && self.grant_id.as_deref().is_some_and(valid_uuid_string)
                    && self
                        .connection_id
                        .as_deref()
                        .is_some_and(valid_local_node_id)
                    && self
                        .permission_snapshot_digest
                        .as_deref()
                        .is_some_and(valid_sha256_hex)
                    && self
                        .device_capability_revision
                        .is_some_and(|revision| revision > 0)
                    && self.schema_hash.is_none()
                    && self.risk_level.is_none()
                    && self.channel == "builtin" =>
            {
                Ok(())
            }
            "grant"
                if self.provider != "local_node"
                    && matches!(
                        self.principal_type.as_deref(),
                        Some("service_account") | Some("user_delegated")
                    )
                    && self.grant_id.as_deref().is_some_and(valid_uuid_string)
                    && self.permission_snapshot_digest.is_none()
                    && self.device_capability_revision.is_none() =>
            {
                Ok(())
            }
            _ => Err(ReadonlyCapabilityError::new(
                "runtime_connector_binding_invalid",
            )),
        }
    }
}

fn valid_local_node_id(value: &str) -> bool {
    value.strip_prefix("dev_").is_some_and(|suffix| {
        suffix.len() == 24
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_schema_hash_value(value: &str) -> bool {
    valid_schema_hash(Some(value))
}

fn valid_uuid_string(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityAllowlistEntry {
    #[serde(rename = "type")]
    pub capability_type: String,
    pub name: String,
    pub id: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub schema_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector_binding: Option<ConnectorBinding>,
}

impl CapabilityAllowlistEntry {
    fn validate(&self) -> Result<(), ReadonlyCapabilityError> {
        if self.capability_type.is_empty()
            || self.name.is_empty()
            || self.id.is_empty()
            || self.version.as_deref().is_some_and(str::is_empty)
            || !valid_schema_hash(self.schema_hash.as_deref())
        {
            return Err(ReadonlyCapabilityError::new(
                "runtime_capability_allowlist_entry_invalid",
            ));
        }
        if matches!(self.capability_type.as_str(), "mcp" | "skill")
            && (self.version.is_none() || self.schema_hash.is_none())
        {
            return Err(ReadonlyCapabilityError::new(
                "runtime_capability_allowlist_binding_invalid",
            ));
        }
        if matches!(self.capability_type.as_str(), "connector" | "mcp") {
            let binding = self
                .connector_binding
                .as_ref()
                .ok_or_else(|| ReadonlyCapabilityError::new("runtime_connector_binding_missing"))?;
            binding.validate(&self.name)?;
            if self.capability_type == "mcp" && binding.provider != "mcp" {
                return Err(ReadonlyCapabilityError::new(
                    "runtime_connector_binding_invalid",
                ));
            }
        } else if self.connector_binding.is_some() {
            return Err(ReadonlyCapabilityError::new(
                "runtime_connector_binding_unexpected",
            ));
        }
        Ok(())
    }
}

fn valid_schema_hash(value: Option<&str>) -> bool {
    value
        .and_then(|value| value.strip_prefix("sha256:"))
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

fn default_tool_kind() -> String {
    "tool".to_string()
}

fn default_read_only() -> bool {
    // Missing effect metadata must not silently become executable authority.
    false
}

fn default_internal_protocol() -> String {
    "internal".to_string()
}

fn default_general_category() -> String {
    "general".to_string()
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataFilter {
    pub kind: Option<String>,
    pub source: Option<String>,
    pub tags: BTreeSet<String>,
    pub protocol: Option<String>,
    pub category: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadonlyCapabilityError(String);

impl ReadonlyCapabilityError {
    fn new(code: &str) -> Self {
        Self(code.to_string())
    }
}

impl Display for ReadonlyCapabilityError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ReadonlyCapabilityError {}

fn validate_binding(
    scope: &RuntimeCapabilityScope,
    tenant_id: &str,
    capability_revision: i64,
) -> Result<(), ReadonlyCapabilityError> {
    scope.validate()?;
    if tenant_id != scope.tenant_id || capability_revision != scope.capability_revision {
        return Err(ReadonlyCapabilityError::new(
            "runtime_capability_revision_mismatch",
        ));
    }
    Ok(())
}

fn descriptor_is_valid(
    scope: &RuntimeCapabilityScope,
    descriptor: &CapabilityDescriptor,
) -> Result<(), ReadonlyCapabilityError> {
    validate_binding(scope, &descriptor.tenant_id, descriptor.capability_revision)?;
    if descriptor.name.is_empty()
        || descriptor.id.is_empty()
        || descriptor.source.is_empty()
        || descriptor.description.is_empty()
        || !valid_schema_hash(descriptor.schema_hash.as_deref())
        || matches!(descriptor.kind.as_str(), "mcp" | "skill") && descriptor.version.is_none()
    {
        return Err(ReadonlyCapabilityError::new(
            "runtime_capability_descriptor_invalid",
        ));
    }
    if !descriptor.read_only {
        return Err(ReadonlyCapabilityError::new(
            "runtime_readonly_capability_required",
        ));
    }
    if !descriptor.schema.is_object() {
        return Err(ReadonlyCapabilityError::new(
            "runtime_capability_schema_invalid",
        ));
    }
    Ok(())
}

fn descriptor_shape_is_valid(
    scope: &RuntimeCapabilityScope,
    descriptor: &CapabilityDescriptor,
) -> Result<(), ReadonlyCapabilityError> {
    validate_binding(scope, &descriptor.tenant_id, descriptor.capability_revision)?;
    if descriptor.name.is_empty()
        || descriptor.id.is_empty()
        || descriptor.source.is_empty()
        || descriptor.description.is_empty()
        || !valid_schema_hash(descriptor.schema_hash.as_deref())
        || matches!(descriptor.kind.as_str(), "mcp" | "skill") && descriptor.version.is_none()
        || !descriptor.schema.is_object()
    {
        return Err(ReadonlyCapabilityError::new(
            "runtime_capability_descriptor_invalid",
        ));
    }
    Ok(())
}

fn capability_allowlist(
    payload: &Value,
) -> Result<Vec<CapabilityAllowlistEntry>, ReadonlyCapabilityError> {
    let entries = payload
        .get("capability_allowlist")
        .and_then(Value::as_array)
        .ok_or_else(|| ReadonlyCapabilityError::new("runtime_capability_allowlist_missing"))?;
    let mut result = Vec::with_capacity(entries.len());
    let mut identities = BTreeSet::new();
    for value in entries {
        let entry: CapabilityAllowlistEntry =
            serde_json::from_value(value.clone()).map_err(|_| {
                ReadonlyCapabilityError::new("runtime_capability_allowlist_entry_invalid")
            })?;
        entry.validate()?;
        let identity = (
            entry.capability_type.clone(),
            entry.name.clone(),
            entry.id.clone(),
            entry.version.clone(),
            entry.schema_hash.clone(),
        );
        if !identities.insert(identity) {
            return Err(ReadonlyCapabilityError::new(
                "runtime_capability_allowlist_collision",
            ));
        }
        result.push(entry);
    }
    Ok(result)
}

/// Validate the Gateway's immutable Agent/tenant/Skill/Plugin projection.
/// Dynamic data is intentionally not interpreted here; it remains a lower
/// trust turn item after the stable instruction layer.
pub fn validate_platform_config(
    scope: &RuntimeCapabilityScope,
    value: &Value,
) -> Result<(), ReadonlyCapabilityError> {
    let object = value
        .as_object()
        .ok_or_else(|| ReadonlyCapabilityError::new("runtime_platform_config_invalid"))?;
    if object.get("schema_version").and_then(Value::as_str) != Some(PLATFORM_CONFIG_SCHEMA_VERSION)
        || object.get("tenant_id").and_then(Value::as_str) != Some(scope.tenant_id.as_str())
        || object.get("user_id").and_then(Value::as_str) != Some(scope.user_id.as_str())
        || object.get("session_id").and_then(Value::as_str) != Some(scope.session_id.as_str())
    {
        return Err(ReadonlyCapabilityError::new(
            "runtime_platform_config_scope_mismatch",
        ));
    }
    let allowed = [
        "schema_version",
        "config_hash",
        "tenant_id",
        "user_id",
        "session_id",
        "agent_spec",
        "instructions",
        "tenant_grants",
        "skills",
        "plugins",
        "attachments",
    ];
    if object.keys().any(|key| !allowed.contains(&key.as_str()))
        || object
            .get("agent_spec")
            .is_none_or(|value| !value.is_object())
        || object
            .get("instructions")
            .is_none_or(|value| !value.is_object())
    {
        return Err(ReadonlyCapabilityError::new(
            "runtime_platform_config_invalid",
        ));
    }
    for key in ["tenant_grants", "skills", "plugins", "attachments"] {
        if object.get(key).is_none_or(|value| !value.is_array()) {
            return Err(ReadonlyCapabilityError::new(
                "runtime_platform_config_invalid",
            ));
        }
    }
    let instructions = object
        .get("instructions")
        .and_then(Value::as_object)
        .ok_or_else(|| ReadonlyCapabilityError::new("runtime_platform_config_invalid"))?;
    if instructions
        .get("prompt_hash")
        .and_then(Value::as_str)
        .is_none_or(|hash| !valid_schema_hash(Some(hash)))
        || instructions
            .get("dynamic_data_after_instructions")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Err(ReadonlyCapabilityError::new(
            "runtime_platform_config_instructions_invalid",
        ));
    }
    let config_hash = object
        .get("config_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| ReadonlyCapabilityError::new("runtime_platform_config_hash_missing"))?;
    if !valid_schema_hash(Some(config_hash)) {
        return Err(ReadonlyCapabilityError::new(
            "runtime_platform_config_hash_invalid",
        ));
    }
    let mut unhashed = object.clone();
    unhashed.remove("config_hash");
    if canonical_json_hash(&Value::Object(unhashed))
        .ok()
        .as_deref()
        != Some(config_hash)
    {
        return Err(ReadonlyCapabilityError::new(
            "runtime_platform_config_hash_mismatch",
        ));
    }
    for skill in object
        .get("skills")
        .and_then(Value::as_array)
        .expect("validated skills array")
    {
        let skill = skill
            .as_object()
            .ok_or_else(|| ReadonlyCapabilityError::new("runtime_platform_skill_invalid"))?;
        for key in ["id", "version", "content_hash", "permissions", "manifest"] {
            if skill.get(key).is_none() {
                return Err(ReadonlyCapabilityError::new(
                    "runtime_platform_skill_invalid",
                ));
            }
        }
        if skill
            .get("id")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
            || skill
                .get("version")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            || skill
                .get("content_hash")
                .and_then(Value::as_str)
                .is_none_or(|hash| !valid_schema_hash(Some(hash)))
            || skill
                .get("permissions")
                .is_none_or(|value| !value.is_array())
            || skill.get("manifest").is_none_or(|value| !value.is_object())
        {
            return Err(ReadonlyCapabilityError::new(
                "runtime_platform_skill_invalid",
            ));
        }
    }
    for plugin in object
        .get("plugins")
        .and_then(Value::as_array)
        .expect("validated plugins array")
    {
        let plugin = plugin
            .as_object()
            .ok_or_else(|| ReadonlyCapabilityError::new("runtime_platform_plugin_invalid"))?;
        if plugin
            .get("id")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
            || plugin
                .get("version")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            || plugin
                .get("capability_id")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            || plugin
                .get("metadata")
                .is_none_or(|value| !value.is_object())
        {
            return Err(ReadonlyCapabilityError::new(
                "runtime_platform_plugin_invalid",
            ));
        }
    }
    for grant in object
        .get("tenant_grants")
        .and_then(Value::as_array)
        .expect("validated grants array")
    {
        let grant = grant
            .as_object()
            .ok_or_else(|| ReadonlyCapabilityError::new("runtime_platform_grant_invalid"))?;
        if grant
            .get("capability_id")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
            || grant
                .get("capability_type")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            || !grant.keys().all(|key| {
                matches!(
                    key.as_str(),
                    "capability_id"
                        | "capability_type"
                        | "grant_id"
                        | "connection_id"
                        | "principal_type"
                        | "provider"
                )
            })
        {
            return Err(ReadonlyCapabilityError::new(
                "runtime_platform_grant_invalid",
            ));
        }
    }
    for attachment in object
        .get("attachments")
        .and_then(Value::as_array)
        .expect("validated attachments array")
    {
        let attachment = attachment
            .as_object()
            .ok_or_else(|| ReadonlyCapabilityError::new("runtime_platform_attachment_invalid"))?;
        if attachment
            .get("ref")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
            || attachment.get("descriptor").and_then(Value::as_str) != Some("read_attachment")
            || attachment.get("version").and_then(Value::as_str) != Some("v1")
        {
            return Err(ReadonlyCapabilityError::new(
                "runtime_platform_attachment_invalid",
            ));
        }
    }
    Ok(())
}

fn descriptor_allowlist_entry(
    descriptor: &CapabilityDescriptor,
    allowlist: &[CapabilityAllowlistEntry],
) -> Result<CapabilityAllowlistEntry, ReadonlyCapabilityError> {
    let mut candidates = allowlist.iter().filter(|entry| {
        entry.capability_type.as_str() == descriptor.kind.as_str()
            && entry.name.as_str() == descriptor.name.as_str()
            && entry.id.as_str() == descriptor.id.as_str()
            && entry.version.as_ref() == descriptor.version.as_ref()
            && entry.schema_hash.as_ref() == descriptor.schema_hash.as_ref()
    });
    let candidate = candidates.next().cloned().ok_or_else(|| {
        ReadonlyCapabilityError::new("runtime_capability_allowlist_scope_mismatch")
    })?;
    if candidates.next().is_some() {
        return Err(ReadonlyCapabilityError::new(
            "runtime_capability_allowlist_scope_mismatch",
        ));
    }
    Ok(candidate)
}

/// Resolve a dynamic tool against the immutable snapshot descriptor and its
/// signed AgentSpec allowlist. The model supplies only the tool name and
/// arguments; all identity/version/schema fields come from this payload.
pub fn resolve_dynamic_tool(
    payload: &Value,
    namespace: Option<&str>,
    tool_name: &str,
) -> Result<(Vec<CapabilityAllowlistEntry>, CapabilityAllowlistEntry), ReadonlyCapabilityError> {
    let (allowlist, expected, effect) = resolve_dynamic_capability(payload, namespace, tool_name)?;
    if effect != CapabilityEffect::Read {
        return Err(ReadonlyCapabilityError::new(
            "runtime_readonly_capability_required",
        ));
    }
    Ok((allowlist, expected))
}

pub fn resolve_dynamic_capability(
    payload: &Value,
    namespace: Option<&str>,
    tool_name: &str,
) -> Result<
    (
        Vec<CapabilityAllowlistEntry>,
        CapabilityAllowlistEntry,
        CapabilityEffect,
    ),
    ReadonlyCapabilityError,
> {
    let (allowlist, expected, _descriptor, effect) =
        resolve_dynamic_capability_descriptor(payload, namespace, tool_name)?;
    Ok((allowlist, expected, effect))
}

/// Resolve the immutable snapshot descriptor and project it into the V2 wire
/// contract used by the capability Worker. The model supplies only the name;
/// every identity and policy field comes from the snapshot payload.
pub fn resolve_dynamic_capability_descriptor(
    payload: &Value,
    namespace: Option<&str>,
    tool_name: &str,
) -> Result<
    (
        Vec<CapabilityAllowlistEntry>,
        CapabilityAllowlistEntry,
        CapabilityDescriptorV2,
        CapabilityEffect,
    ),
    ReadonlyCapabilityError,
> {
    if tool_name.is_empty() {
        return Err(ReadonlyCapabilityError::new("runtime_dynamic_tool_invalid"));
    }
    if is_native_agent_control_tool(namespace, tool_name) {
        return Err(ReadonlyCapabilityError::new(
            "runtime_native_agent_control_required",
        ));
    }
    let mut candidates = Vec::new();
    for key in ["tools", "mcp", "deferred", "attachment_tools"] {
        for value in payload
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let descriptor: CapabilityDescriptor =
                serde_json::from_value(value.clone()).map_err(|_| {
                    ReadonlyCapabilityError::new("runtime_capability_descriptor_invalid")
                })?;
            if descriptor.name == tool_name && descriptor_namespace_matches(&descriptor, namespace)
            {
                candidates.push(descriptor);
            }
        }
    }
    if candidates.len() != 1 {
        return Err(ReadonlyCapabilityError::new(
            "runtime_dynamic_tool_not_authorized",
        ));
    }
    let resolved = &candidates[0];
    let allowlist = capability_allowlist(payload)?;
    let expected = descriptor_allowlist_entry(resolved, &allowlist)?;
    let effect = match resolved
        .metadata
        .get("effect")
        .or_else(|| resolved.metadata.get("operation_kind"))
        .and_then(Value::as_str)
    {
        Some("unknown") => CapabilityEffect::Unknown,
        Some("write") => CapabilityEffect::Write,
        _ if resolved.read_only => CapabilityEffect::Read,
        _ => CapabilityEffect::Write,
    };
    let descriptor = project_capability_descriptor_v2(resolved, &expected, effect)?;
    Ok((allowlist, expected, descriptor, effect))
}

fn project_capability_descriptor_v2(
    descriptor: &CapabilityDescriptor,
    expected: &CapabilityAllowlistEntry,
    effect: CapabilityEffect,
) -> Result<CapabilityDescriptorV2, ReadonlyCapabilityError> {
    let schema = descriptor.schema.clone();
    let schema_hash = expected
        .schema_hash
        .clone()
        .or_else(|| descriptor.schema_hash.clone())
        .ok_or_else(|| ReadonlyCapabilityError::new("runtime_capability_schema_hash_missing"))?;
    if descriptor.id != expected.id
        || descriptor.name != expected.name
        || descriptor.version != expected.version
        || descriptor.schema_hash.as_deref() != Some(schema_hash.as_str())
        || canonical_json_hash(&schema)
            .map_err(|_| ReadonlyCapabilityError::new("runtime_capability_descriptor_invalid"))?
            != schema_hash
    {
        return Err(ReadonlyCapabilityError::new(
            "runtime_capability_allowlist_scope_mismatch",
        ));
    }
    let mut tags = descriptor
        .tags
        .iter()
        .filter(|tag| {
            !tag.is_empty()
                && tag.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'/' | b'-')
                })
        })
        .cloned()
        .collect::<Vec<_>>();
    tags.push(format!("kind:{}", descriptor.kind));
    tags.sort();
    tags.dedup();
    let approval_policy = match descriptor
        .metadata
        .get("approval_policy")
        .or_else(|| descriptor.metadata.get("approval"))
        .and_then(Value::as_str)
    {
        Some("on_request") => ApprovalPolicy::OnRequest,
        Some("always") => ApprovalPolicy::Always,
        Some("never") | None if effect == CapabilityEffect::Read => ApprovalPolicy::Never,
        Some("never") => {
            return Err(ReadonlyCapabilityError::new(
                "runtime_capability_approval_policy_invalid",
            ));
        }
        _ => ApprovalPolicy::Always,
    };
    let execution_mode = match descriptor
        .metadata
        .get("execution_mode")
        .and_then(Value::as_str)
    {
        Some("stream") => ExecutionMode::Stream,
        Some("job") => ExecutionMode::Job,
        Some("inline") | None => ExecutionMode::Inline,
        _ => {
            return Err(ReadonlyCapabilityError::new(
                "runtime_capability_execution_mode_invalid",
            ));
        }
    };
    let timeout_ms = descriptor
        .metadata
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(120_000);
    let output_schema = descriptor
        .metadata
        .get("output_schema")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"type": "object"}));
    let projected = CapabilityDescriptorV2 {
        schema_version: CAPABILITY_DESCRIPTOR_SCHEMA_VERSION.to_string(),
        id: expected.id.clone(),
        name: expected.name.clone(),
        version: expected
            .version
            .clone()
            .unwrap_or_else(|| "null".to_string()),
        description: descriptor.description.clone(),
        schema_hash,
        input_schema: schema,
        output_schema,
        effect,
        approval_policy,
        execution_mode,
        timeout_ms,
        tags,
        protocol: descriptor.protocol.clone(),
        connector_binding: expected
            .connector_binding
            .as_ref()
            .map(|value| serde_json::to_value(value).expect("connector binding is serializable")),
    };
    projected
        .validate()
        .map_err(|_| ReadonlyCapabilityError::new("runtime_capability_descriptor_invalid"))?;
    Ok(projected)
}

fn matches_filter(descriptor: &CapabilityDescriptor, filter: &MetadataFilter) -> bool {
    filter
        .kind
        .as_deref()
        .is_none_or(|kind| kind == descriptor.kind)
        && filter
            .source
            .as_deref()
            .is_none_or(|source| source == descriptor.source)
        && filter
            .protocol
            .as_deref()
            .is_none_or(|protocol| protocol == descriptor.protocol)
        && filter
            .category
            .as_deref()
            .is_none_or(|category| category == descriptor.category)
        && filter
            .tags
            .iter()
            .all(|tag| descriptor.tags.iter().any(|value| value == tag))
}

/// Validate, metadata-filter, deduplicate, and stably order read-only schemas.
///
/// The typed [`MetadataFilter`] deliberately has no prompt/query field. The
/// caller cannot route capabilities through user text at this boundary.
pub fn discover_readonly(
    scope: &RuntimeCapabilityScope,
    descriptors: impl IntoIterator<Item = CapabilityDescriptor>,
    filter: &MetadataFilter,
) -> Result<Vec<CapabilityDescriptor>, ReadonlyCapabilityError> {
    let mut selected = BTreeMap::<(String, String), CapabilityDescriptor>::new();
    for mut descriptor in descriptors {
        descriptor_is_valid(scope, &descriptor)?;
        descriptor.tags.sort();
        descriptor.tags.dedup();
        if !matches_filter(&descriptor, filter) {
            continue;
        }
        let key = (descriptor.source.clone(), descriptor.name.clone());
        if selected.insert(key, descriptor).is_some() {
            return Err(ReadonlyCapabilityError::new(
                "runtime_capability_name_collision",
            ));
        }
    }
    Ok(selected.into_values().collect())
}

/// Validate a gateway payload and render turn **items** for Codex
/// `additional_context`. Tool catalogs authorize execution on the snapshot and
/// are registered as `dynamicTools`; they must not become model-visible text.
pub fn render_turn_input(
    scope: &RuntimeCapabilityScope,
    payload: &Value,
) -> Result<Option<String>, ReadonlyCapabilityError> {
    let object = payload
        .as_object()
        .ok_or_else(|| ReadonlyCapabilityError::new("runtime_readonly_payload_invalid"))?;
    if object.get("schema_version").and_then(Value::as_str)
        != Some(READONLY_CAPABILITY_SCHEMA_VERSION)
        || object.get("tenant_id").and_then(Value::as_str) != Some(scope.tenant_id.as_str())
        || object.get("capability_revision").and_then(Value::as_i64)
            != Some(scope.capability_revision)
    {
        return Err(ReadonlyCapabilityError::new(
            "runtime_capability_revision_mismatch",
        ));
    }
    for key in ["items", "tools", "mcp", "deferred", "attachment_tools"] {
        if !object.get(key).is_none_or(Value::is_array) {
            return Err(ReadonlyCapabilityError::new(
                "runtime_readonly_payload_invalid",
            ));
        }
    }
    if let Some(platform_config) = object.get("platform_config") {
        validate_platform_config(scope, platform_config)?;
    }
    if object.get("capability_allowlist").is_some() {
        capability_allowlist(payload)?;
    }
    if let Some(items) = object.get("items").and_then(Value::as_array) {
        for item in items {
            let item_object = item
                .as_object()
                .ok_or_else(|| ReadonlyCapabilityError::new("runtime_capability_item_invalid"))?;
            validate_binding(
                scope,
                item_object
                    .get("tenant_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        ReadonlyCapabilityError::new("runtime_capability_item_invalid")
                    })?,
                item_object
                    .get("capability_revision")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| {
                        ReadonlyCapabilityError::new("runtime_capability_item_invalid")
                    })?,
            )?;
            if item_object
                .get("payload")
                .is_none_or(|value| !value.is_object())
            {
                return Err(ReadonlyCapabilityError::new(
                    "runtime_capability_item_invalid",
                ));
            }
        }
    }
    for key in ["tools", "mcp", "deferred", "attachment_tools"] {
        if let Some(descriptors) = object.get(key).and_then(Value::as_array) {
            for descriptor in descriptors {
                let descriptor: CapabilityDescriptor = serde_json::from_value(descriptor.clone())
                    .map_err(|_| {
                    ReadonlyCapabilityError::new("runtime_capability_descriptor_invalid")
                })?;
                descriptor_shape_is_valid(scope, &descriptor)?;
                if !descriptor.read_only {
                    let allowlist = capability_allowlist(payload)?;
                    descriptor_allowlist_entry(&descriptor, &allowlist)?;
                }
            }
        }
    }
    let items = object
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if items.is_empty() {
        return Ok(None);
    }
    let encoded = serde_json::to_string(&json!({
        "schema_version": object.get("schema_version"),
        "tenant_id": object.get("tenant_id"),
        "capability_revision": object.get("capability_revision"),
        "items": items,
    }))
    .map_err(|_| ReadonlyCapabilityError::new("runtime_readonly_payload_invalid"))?;
    Ok(Some(encoded))
}

/// Explicit namespace metadata is authoritative. Legacy descriptors without it
/// retain source aliases, but cannot steal calls addressed to another namespace.
fn descriptor_namespace_matches(
    descriptor: &CapabilityDescriptor,
    requested: Option<&str>,
) -> bool {
    fn normalized(namespace: &str) -> Option<&str> {
        (!namespace.is_empty() && namespace != "functions").then_some(namespace)
    }
    let requested = requested.and_then(normalized);
    match descriptor.metadata.get("namespace") {
        Some(Value::String(namespace)) => normalized(namespace) == requested,
        None | Some(Value::Null) => {
            requested.is_none_or(|namespace| descriptor.source == namespace)
        }
        _ => false,
    }
}

/// Returns whether a dynamic tool name is explicitly present in the
/// immutable, revision-bound read-only descriptor set. An absent descriptor
/// is never treated as authorized. Namespaces are accepted from descriptor
/// metadata, with source aliases retained only for legacy descriptors that lack
/// explicit namespace metadata. Empty and `functions` denote the default namespace.
pub fn allows_dynamic_tool(payload: &Value, namespace: Option<&str>, tool_name: &str) -> bool {
    if tool_name.is_empty() || is_native_agent_control_tool(namespace, tool_name) {
        return false;
    }
    ["tools", "mcp", "deferred", "attachment_tools"]
        .iter()
        .filter_map(|key| payload.get(*key).and_then(Value::as_array))
        .flatten()
        .filter(|value| {
            let Ok(descriptor) = serde_json::from_value::<CapabilityDescriptor>((*value).clone())
            else {
                return false;
            };
            if !descriptor.read_only || descriptor.name != tool_name {
                return false;
            }
            descriptor_namespace_matches(&descriptor, namespace)
        })
        .count()
        == 1
}

fn is_native_agent_control_tool(namespace: Option<&str>, tool_name: &str) -> bool {
    matches!(
        namespace,
        None | Some("" | "functions" | "collaboration" | "multi_agent_v1")
    ) && matches!(
        tool_name,
        "spawn_agent"
            | "steer"
            | "wait"
            | "interrupt"
            | "spawn_subagent"
            | "send_input"
            | "wait_agent"
            | "interrupt_agent"
    )
}

/// Copy one item into the immutable runtime scope and mark it non-authoritative.
pub fn project_item(
    scope: &RuntimeCapabilityScope,
    item_id: impl Into<String>,
    kind: ReadonlyItemKind,
    source: impl Into<String>,
    payload: Value,
) -> Result<CapabilityItem, ReadonlyCapabilityError> {
    validate_binding(scope, &scope.tenant_id, scope.capability_revision)?;
    let item_id = item_id.into();
    let source = source.into();
    if item_id.is_empty() || source.is_empty() || !payload.is_object() {
        return Err(ReadonlyCapabilityError::new(
            "runtime_capability_item_invalid",
        ));
    }
    Ok(CapabilityItem {
        item_id,
        kind,
        tenant_id: scope.tenant_id.clone(),
        capability_revision: scope.capability_revision,
        source,
        payload,
        untrusted: true,
        authority: default_authority(),
    })
}

pub fn project_knowledge(
    scope: &RuntimeCapabilityScope,
    source_id: impl Into<String>,
    content: impl Into<String>,
) -> Result<CapabilityItem, ReadonlyCapabilityError> {
    let source_id = source_id.into();
    project_item(
        scope,
        format!("knowledge:{source_id}"),
        ReadonlyItemKind::Knowledge,
        source_id,
        serde_json::json!({"content": content.into()}),
    )
}

pub fn project_attachment(
    scope: &RuntimeCapabilityScope,
    attachment_id: impl Into<String>,
    content_ref: impl Into<String>,
) -> Result<CapabilityItem, ReadonlyCapabilityError> {
    let attachment_id = attachment_id.into();
    project_item(
        scope,
        format!("attachment:{attachment_id}"),
        ReadonlyItemKind::Attachment,
        attachment_id,
        serde_json::json!({"content_ref": content_ref.into()}),
    )
}

pub fn project_citation(
    scope: &RuntimeCapabilityScope,
    citation_id: impl Into<String>,
    source_id: impl Into<String>,
    locator: impl Into<String>,
) -> Result<CapabilityItem, ReadonlyCapabilityError> {
    let citation_id = citation_id.into();
    project_item(
        scope,
        format!("citation:{citation_id}"),
        ReadonlyItemKind::Citation,
        source_id,
        serde_json::json!({"locator": locator.into()}),
    )
}

pub fn project_artifact(
    scope: &RuntimeCapabilityScope,
    artifact_id: impl Into<String>,
    content_ref: impl Into<String>,
    media_type: impl Into<String>,
) -> Result<CapabilityItem, ReadonlyCapabilityError> {
    let artifact_id = artifact_id.into();
    project_item(
        scope,
        format!("artifact:{artifact_id}"),
        ReadonlyItemKind::Artifact,
        artifact_id,
        serde_json::json!({"content_ref": content_ref.into(), "media_type": media_type.into()}),
    )
}

pub fn project_office_read(
    scope: &RuntimeCapabilityScope,
    artifact_id: impl Into<String>,
    format: impl Into<String>,
    extracted: Value,
) -> Result<CapabilityItem, ReadonlyCapabilityError> {
    let artifact_id = artifact_id.into();
    project_item(
        scope,
        format!("office-read:{artifact_id}"),
        ReadonlyItemKind::OfficeRead,
        artifact_id,
        serde_json::json!({"format": format.into(), "extracted": extracted}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_node_connector_binding_requires_current_device_snapshot_identity() {
        let mut binding = ConnectorBinding {
            binding_type: "grant".into(),
            provider: "local_node".into(),
            tool_name: "local_node_action".into(),
            principal_type: Some("user_delegated".into()),
            grant_id: Some("cfbfa9d4-18b3-4470-b3f2-f13660a993dc".into()),
            connection_id: Some("dev_aaaaaaaaaaaaaaaaaaaaaaaa".into()),
            schema_hash: None,
            risk_level: None,
            permission_snapshot_digest: Some("a".repeat(64)),
            device_capability_revision: Some(2),
            channel: "builtin".into(),
        };
        assert!(binding.validate("local_node_action").is_ok());

        binding.device_capability_revision = None;
        assert!(binding.validate("local_node_action").is_err());
        binding.device_capability_revision = Some(2);
        binding.connection_id = Some("dev_short".into());
        assert!(binding.validate("local_node_action").is_err());
    }

    fn scope() -> RuntimeCapabilityScope {
        RuntimeCapabilityScope {
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
            capability_revision: 7,
            snapshot_id: "snapshot-a".to_string(),
        }
    }

    fn descriptor(name: &str, source: &str) -> CapabilityDescriptor {
        let schema = serde_json::json!({"type": "object"});
        let schema_hash = canonical_json_hash(&schema).expect("canonical schema hash");
        CapabilityDescriptor {
            name: name.to_string(),
            description: "read-only capability".to_string(),
            schema,
            tenant_id: "tenant-a".to_string(),
            capability_revision: 7,
            source: source.to_string(),
            kind: "knowledge".to_string(),
            read_only: true,
            tags: vec!["retrieval".to_string(), "read".to_string()],
            protocol: "internal".to_string(),
            category: "retrieval".to_string(),
            id: format!("{source}:{name}"),
            version: Some("v1".to_string()),
            schema_hash: Some(schema_hash),
            metadata: Map::new(),
        }
    }

    #[test]
    fn discovery_is_metadata_bound_and_stably_sorted() {
        let filter = MetadataFilter {
            tags: BTreeSet::from(["read".to_string()]),
            ..Default::default()
        };
        let values = discover_readonly(
            &scope(),
            [
                descriptor("z.read", "knowledge"),
                descriptor("a.read", "mcp:docs"),
            ],
            &filter,
        )
        .expect("read-only descriptors should validate");
        assert_eq!(values[0].source, "knowledge");
        assert_eq!(values[1].source, "mcp:docs");
        assert_eq!(values[0].tags, vec!["read", "retrieval"]);
    }

    #[test]
    fn tenant_and_revision_mismatch_fail_closed() {
        let mut foreign = descriptor("foreign.read", "foreign");
        foreign.tenant_id = "tenant-b".to_string();
        let error = discover_readonly(&scope(), [foreign], &MetadataFilter::default())
            .expect_err("foreign tenant must be rejected");
        assert_eq!(error.to_string(), "runtime_capability_revision_mismatch");
    }

    #[test]
    fn unified_projection_keeps_external_data_non_authoritative() {
        let values = [
            project_knowledge(&scope(), "source-a", "retrieved"),
            project_attachment(&scope(), "attachment-a", "blob:a"),
            project_citation(&scope(), "citation-a", "source-a", "p.1"),
            project_artifact(&scope(), "artifact-a", "blob:b", "text/plain"),
            project_office_read(&scope(), "artifact-a", "docx", serde_json::json!({"p": 1})),
        ];
        assert!(values.iter().all(|value| {
            value.as_ref().is_ok_and(|item| {
                item.tenant_id == "tenant-a"
                    && item.capability_revision == 7
                    && item.untrusted
                    && item.authority == "non_authoritative"
            })
        }));
    }

    #[test]
    fn write_descriptor_is_not_visible() {
        let mut value = descriptor("write.danger", "danger");
        value.read_only = false;
        let error = discover_readonly(&scope(), [value], &MetadataFilter::default())
            .expect_err("write capability must fail closed");
        assert_eq!(error.to_string(), "runtime_readonly_capability_required");
    }

    #[test]
    fn missing_effect_metadata_is_not_visible() {
        let mut value = serde_json::to_value(descriptor("unknown", "external")).unwrap();
        value.as_object_mut().unwrap().remove("read_only");
        let value: CapabilityDescriptor = serde_json::from_value(value).unwrap();
        assert!(!value.read_only);
        let error = discover_readonly(&scope(), [value], &MetadataFilter::default())
            .expect_err("missing effect metadata must fail closed");
        assert_eq!(error.to_string(), "runtime_readonly_capability_required");
    }

    #[test]
    fn render_turn_input_rejects_foreign_payload_and_marks_context() {
        let payload = serde_json::json!({
            "schema_version": READONLY_CAPABILITY_SCHEMA_VERSION,
            "tenant_id": "tenant-a",
            "capability_revision": 7,
            "items": [{
                "item_id": "knowledge:source-a",
                "kind": "knowledge",
                "tenant_id": "tenant-a",
                "capability_revision": 7,
                "source": "knowledge",
                "payload": {"dataset_id": "source-a"}
            }],
            "tools": [],
            "mcp": []
        });
        let rendered = render_turn_input(&scope(), &payload)
            .expect("readonly input should validate")
            .expect("non-empty readonly input should render");
        assert!(rendered.contains("knowledge:source-a"));
        assert!(!rendered.contains("capability_allowlist"));
        assert!(!rendered.contains("[AI_PLATFORM_READONLY_CONTEXT_V1]"));

        let mut foreign = payload;
        foreign["tenant_id"] = Value::String("tenant-b".to_string());
        let error = render_turn_input(&scope(), &foreign).expect_err("foreign input must fail");
        assert_eq!(error.to_string(), "runtime_capability_revision_mismatch");
    }

    #[test]
    fn render_turn_input_does_not_dump_tool_catalogs() {
        let payload = serde_json::json!({
            "schema_version": READONLY_CAPABILITY_SCHEMA_VERSION,
            "tenant_id": "tenant-a",
            "capability_revision": 7,
            "items": [],
            "tools": [serde_json::to_value(descriptor("search", "knowledge")).unwrap()],
            "mcp": [],
            "deferred": [],
            "attachment_tools": []
        });
        let rendered =
            render_turn_input(&scope(), &payload).expect("catalog-only payload should validate");
        assert_eq!(rendered, None);
    }

    #[test]
    fn dynamic_tools_require_an_explicit_readonly_descriptor() {
        let mut payload = serde_json::json!({
            "tools": [serde_json::to_value(descriptor("search", "knowledge")).unwrap()]
        });
        assert!(allows_dynamic_tool(&payload, None, "search"));
        assert!(allows_dynamic_tool(&payload, Some("knowledge"), "search"));
        assert!(!allows_dynamic_tool(&payload, None, "write"));
        payload["tools"][0]["read_only"] = Value::Bool(false);
        assert!(!allows_dynamic_tool(&payload, None, "search"));
        payload["tools"][0]["read_only"] = Value::Bool(true);
        payload["mcp"] =
            serde_json::json!([serde_json::to_value(descriptor("search", "mcp:docs")).unwrap()]);
        assert!(!allows_dynamic_tool(&payload, None, "search"));
    }

    #[test]
    fn dynamic_tool_resolution_requires_snapshot_allowlist_identity() {
        let descriptor = descriptor("search", "knowledge");
        let schema_hash = descriptor.schema_hash.clone().expect("schema hash");
        let mut payload = serde_json::json!({
            "tools": [serde_json::to_value(&descriptor).unwrap()],
            "capability_allowlist": [{
                "type": "knowledge",
                "name": "search",
                "id": "knowledge:search",
                "version": "v1",
                "schema_hash": schema_hash
            }]
        });
        let (allowlist, expected) =
            resolve_dynamic_tool(&payload, None, "search").expect("allowlisted tool");
        assert_eq!(allowlist.len(), 1);
        assert_eq!(expected.id, "knowledge:search");
        assert_eq!(expected.schema_hash, descriptor.schema_hash);

        payload["capability_allowlist"][0]["schema_hash"] =
            Value::String(format!("sha256:{}", "b".repeat(64)));
        assert_eq!(
            resolve_dynamic_tool(&payload, None, "search")
                .expect_err("schema drift must fail closed")
                .to_string(),
            "runtime_capability_allowlist_scope_mismatch"
        );
        payload
            .as_object_mut()
            .unwrap()
            .remove("capability_allowlist");
        assert_eq!(
            resolve_dynamic_tool(&payload, None, "search")
                .expect_err("missing allowlist must fail closed")
                .to_string(),
            "runtime_capability_allowlist_missing"
        );
    }

    #[test]
    fn deferred_write_resolution_requires_exact_allowlist_identity() {
        let mut descriptor = descriptor("document_write", "docs");
        let schema_hash = descriptor.schema_hash.clone().expect("schema hash");
        descriptor.read_only = false;
        descriptor
            .metadata
            .insert("effect".to_string(), Value::String("write".to_string()));
        let mut payload = serde_json::json!({
            "deferred": [serde_json::to_value(&descriptor).unwrap()],
            "capability_allowlist": [{
                "type": "knowledge",
                "name": "document_write",
                "id": "docs:document_write",
                "version": "v1",
                "schema_hash": schema_hash
            }]
        });
        let (_, expected, effect) = resolve_dynamic_capability(&payload, None, "document_write")
            .expect("allowlisted deferred write should resolve");
        assert_eq!(expected.name, "document_write");
        assert_eq!(effect, CapabilityEffect::Write);
        payload["capability_allowlist"][0]["schema_hash"] =
            Value::String(format!("sha256:{}", "b".repeat(64)));
        assert_eq!(
            resolve_dynamic_capability(&payload, None, "document_write")
                .expect_err("schema mismatch must fail closed")
                .to_string(),
            "runtime_capability_allowlist_scope_mismatch"
        );
    }

    #[test]
    fn native_child_controls_never_resolve_as_dynamic_worker_tools() {
        let payload = serde_json::json!({"tools": []});
        for name in [
            "spawn_agent",
            "steer",
            "wait",
            "interrupt",
            "spawn_subagent",
        ] {
            assert!(!allows_dynamic_tool(&payload, None, name));
            assert_eq!(
                resolve_dynamic_capability(&payload, None, name)
                    .expect_err("native control must not enter Worker")
                    .to_string(),
                "runtime_native_agent_control_required"
            );
        }
    }
}
