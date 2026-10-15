//! Declarative parity catalog for the built-in Assistant capabilities.
//!
//! The JSON beside this module is generated from the Python `ToolDefinition`
//! catalog and is intentionally data-only.  This loader validates the static
//! artifact before it can be projected into a worker catalog; it never routes
//! by model name, user text, or keyword.

use std::collections::{BTreeMap, BTreeSet};

use ai_platform_capability_contract::{
    ApprovalPolicy, CAPABILITY_DESCRIPTOR_SCHEMA_VERSION, CapabilityDescriptorV2, CapabilityEffect,
    ExecutionMode, canonical_json_hash,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const CATALOG_SCHEMA_VERSION: &str = "ai-platform-capability-catalog/v1";
const MIN_CAPABILITIES: usize = 1;
const MAX_CAPABILITIES: usize = 256;

/// The exact fields required to preserve the legacy Python descriptor and its
/// user-facing authorization/usage metadata.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformCapabilityRecord {
    pub name: String,
    pub id: String,
    pub version: Option<String>,
    pub schema_hash: String,
    pub description: String,
    pub input_schema: Value,
    pub read_only: bool,
    pub effect: String,
    pub risk: String,
    pub protocol: String,
    pub kind: String,
    pub approval: String,
    pub category: String,
    pub when_to_use: Option<String>,
    pub when_not_to_use: Option<String>,
    pub requires_confirmation: bool,
    pub required_permissions: Vec<String>,
    pub timeout_ms: u64,
    pub implementation_owner: String,
    #[serde(default)]
    pub connector_provider: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformCapabilityCatalog {
    pub schema_version: String,
    pub capability_revision: u64,
    pub gateway_policy: GatewayPolicy,
    pub capabilities: Vec<PlatformCapabilityRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayPolicy {
    pub high_risk_tools: Vec<String>,
    pub medium_risk_tools: Vec<String>,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum PlatformCatalogError {
    #[error("invalid catalog JSON: {0}")]
    Json(String),
    #[error("catalog schema version mismatch: {0}")]
    SchemaVersion(String),
    #[error("catalog capability revision must be positive")]
    Revision,
    #[error("catalog capability count is outside the supported range 1..=256: {0}")]
    Count(usize),
    #[error("catalog contains duplicate {0}: {1}")]
    Duplicate(&'static str, String),
    #[error("catalog capability {0} has invalid {1}")]
    Field(String, &'static str),
    #[error("catalog capability {0} schema hash mismatch: expected {1}, got {2}")]
    SchemaHash(String, String, String),
    #[error("catalog capability {0} cannot become a descriptor: {1}")]
    Descriptor(String, String),
}

/// Parse and validate the checked-in parity artifact.
///
/// The checked-in artifact is the FRC-00 baseline snapshot.  In particular,
/// the specialist IDs embedded in `spawn_subagent` are evidence from that
/// snapshot, not a permanent runtime allowlist; the running catalog is built
/// from the versioned plugin catalog and may change with its revision.
pub fn load_platform_catalog() -> Result<PlatformCapabilityCatalog, PlatformCatalogError> {
    let catalog: PlatformCapabilityCatalog =
        serde_json::from_str(include_str!("platform_catalog_v1.json"))
            .map_err(|error| PlatformCatalogError::Json(error.to_string()))?;
    validate_platform_catalog(&catalog)?;
    Ok(catalog)
}

/// Validate a catalog value.  Keeping this separate makes mutation and
/// compatibility tests able to prove that tampered data fails closed.
pub fn validate_platform_catalog(
    catalog: &PlatformCapabilityCatalog,
) -> Result<(), PlatformCatalogError> {
    if catalog.schema_version != CATALOG_SCHEMA_VERSION {
        return Err(PlatformCatalogError::SchemaVersion(
            catalog.schema_version.clone(),
        ));
    }
    if catalog.capability_revision == 0 {
        return Err(PlatformCatalogError::Revision);
    }
    if !(MIN_CAPABILITIES..=MAX_CAPABILITIES).contains(&catalog.capabilities.len()) {
        return Err(PlatformCatalogError::Count(catalog.capabilities.len()));
    }

    let mut names = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut policy_names = BTreeSet::new();
    for name in catalog
        .gateway_policy
        .high_risk_tools
        .iter()
        .chain(catalog.gateway_policy.medium_risk_tools.iter())
    {
        if !is_capability_name(name) || !policy_names.insert(name.clone()) {
            return Err(PlatformCatalogError::Duplicate(
                "gateway_policy capability name",
                name.clone(),
            ));
        }
    }
    for capability in &catalog.capabilities {
        if !names.insert(capability.name.clone()) {
            return Err(PlatformCatalogError::Duplicate(
                "name",
                capability.name.clone(),
            ));
        }
        if !ids.insert(capability.id.clone()) {
            return Err(PlatformCatalogError::Duplicate("id", capability.id.clone()));
        }
        validate_record(capability)?;
    }
    Ok(())
}

fn validate_record(record: &PlatformCapabilityRecord) -> Result<(), PlatformCatalogError> {
    if record.name.is_empty() || !is_capability_name(&record.name) {
        return Err(PlatformCatalogError::Field(record.name.clone(), "name"));
    }
    if record.id.is_empty() || !is_capability_name(&record.id) {
        return Err(PlatformCatalogError::Field(record.name.clone(), "id"));
    }
    if record.id != record.name {
        return Err(PlatformCatalogError::Field(
            record.name.clone(),
            "id/name binding",
        ));
    }
    if record.description.trim().is_empty() || record.protocol.trim().is_empty() {
        return Err(PlatformCatalogError::Field(
            record.name.clone(),
            "description/protocol",
        ));
    }
    if !record.input_schema.is_object() {
        return Err(PlatformCatalogError::Field(
            record.name.clone(),
            "input_schema",
        ));
    }
    let expected_hash = canonical_json_hash(&record.input_schema)
        .map_err(|_| PlatformCatalogError::Field(record.name.clone(), "input_schema"))?;
    if record.schema_hash != expected_hash {
        return Err(PlatformCatalogError::SchemaHash(
            record.name.clone(),
            expected_hash,
            record.schema_hash.clone(),
        ));
    }
    if !matches!(record.effect.as_str(), "read" | "write" | "unknown") {
        return Err(PlatformCatalogError::Field(record.name.clone(), "effect"));
    }
    if !matches!(record.risk.as_str(), "low" | "medium" | "high" | "critical") {
        return Err(PlatformCatalogError::Field(record.name.clone(), "risk"));
    }
    if !matches!(record.approval.as_str(), "never" | "on_request" | "always") {
        return Err(PlatformCatalogError::Field(record.name.clone(), "approval"));
    }
    if !matches!(record.implementation_owner.as_str(), "runtime" | "worker") {
        return Err(PlatformCatalogError::Field(
            record.name.clone(),
            "implementation_owner",
        ));
    }
    if let Some(provider) = record.connector_provider.as_deref()
        && (record.implementation_owner != "worker"
            || record.protocol != "internal"
            || record.kind != "tool"
            || !is_capability_name(provider))
    {
        return Err(PlatformCatalogError::Field(
            record.name.clone(),
            "connector_provider",
        ));
    }
    if !matches!(
        record.category.as_str(),
        "retrieval"
            | "generation"
            | "analysis"
            | "integration"
            | "utility"
            | "skill"
            | "mcp"
            | "local"
    ) {
        return Err(PlatformCatalogError::Field(record.name.clone(), "category"));
    }
    for (value, field) in [
        (record.when_to_use.as_ref(), "when_to_use"),
        (record.when_not_to_use.as_ref(), "when_not_to_use"),
    ] {
        if let Some(value) = value
            && (value.trim().is_empty()
                || value.len() > 20_000
                || value.chars().any(|character| {
                    character.is_control() && character != '\n' && character != '\r'
                }))
        {
            return Err(PlatformCatalogError::Field(record.name.clone(), field));
        }
    }
    let mut permissions = BTreeSet::new();
    if record.required_permissions.len() > 32
        || record.required_permissions.iter().any(|permission| {
            permission.is_empty()
                || permission.len() > 128
                || !permissions.insert(permission)
                || !permission.chars().all(|character| {
                    character.is_ascii_alphanumeric() || ":._-".contains(character)
                })
        })
    {
        return Err(PlatformCatalogError::Field(
            record.name.clone(),
            "required_permissions",
        ));
    }
    if record.timeout_ms == 0 || record.timeout_ms > 300_000 {
        return Err(PlatformCatalogError::Field(
            record.name.clone(),
            "timeout_ms",
        ));
    }
    if record.effect == "read" && (!record.read_only || record.approval != "never") {
        return Err(PlatformCatalogError::Field(
            record.name.clone(),
            "read effect binding",
        ));
    }
    if record.effect != "read" && (record.read_only || record.approval == "never") {
        return Err(PlatformCatalogError::Field(
            record.name.clone(),
            "write approval binding",
        ));
    }
    Ok(())
}

/// Convert one versioned catalog record into the wire descriptor consumed by
/// the worker.  This is intentionally a pure projection: the record remains
/// authoritative for identity, schema hash, policy, timeout, and protocol.
/// A missing legacy version is represented by the explicit `null` sentinel so
/// it cannot be confused with a worker-selected version.
pub fn to_capability_descriptor(
    record: &PlatformCapabilityRecord,
) -> Result<CapabilityDescriptorV2, PlatformCatalogError> {
    validate_record(record)?;
    let effect = match record.effect.as_str() {
        "read" => CapabilityEffect::Read,
        "write" => CapabilityEffect::Write,
        "unknown" => CapabilityEffect::Unknown,
        _ => return Err(PlatformCatalogError::Field(record.name.clone(), "effect")),
    };
    let approval_policy = match record.approval.as_str() {
        "never" => ApprovalPolicy::Never,
        "on_request" => ApprovalPolicy::OnRequest,
        "always" => ApprovalPolicy::Always,
        _ => return Err(PlatformCatalogError::Field(record.name.clone(), "approval")),
    };

    let mut tags = BTreeSet::new();
    tags.insert(format!("effect:{}", record.effect));
    tags.insert(format!("category:{}", record.category));
    tags.insert(format!("kind:{}", record.kind));
    tags.insert(format!("owner:{}", record.implementation_owner));
    tags.insert(format!("risk:{}", record.risk));
    if let Some(provider) = &record.connector_provider {
        tags.insert("binding-type:connector".to_string());
        tags.insert(format!("connector:{provider}"));
    }
    for permission in &record.required_permissions {
        tags.insert(format!("permission:{permission}"));
    }

    let descriptor = CapabilityDescriptorV2 {
        schema_version: CAPABILITY_DESCRIPTOR_SCHEMA_VERSION.to_string(),
        id: record.id.clone(),
        name: record.name.clone(),
        version: record.version.clone().unwrap_or_else(|| "null".to_string()),
        description: record.description.clone(),
        schema_hash: record.schema_hash.clone(),
        input_schema: record.input_schema.clone(),
        output_schema: serde_json::json!({"type": "object"}),
        effect,
        approval_policy,
        execution_mode: ExecutionMode::Inline,
        timeout_ms: record.timeout_ms,
        tags: tags.into_iter().collect(),
        protocol: record.protocol.clone(),
        connector_binding: None,
    };
    descriptor.validate().map_err(|error| {
        PlatformCatalogError::Descriptor(record.name.clone(), error.to_string())
    })?;
    Ok(descriptor)
}

/// Build the immutable registry used by the capability worker.
///
/// The declarative catalog is broader than the set of executors that this
/// process currently owns.  Only worker-owned, read-only capabilities with a
/// concrete implementation are projected here.  Runtime-native filesystem
/// capabilities and deferred worker capabilities therefore cannot be
/// accidentally advertised by this service.
pub fn worker_capability_catalog()
-> Result<BTreeMap<String, CapabilityDescriptorV2>, PlatformCatalogError> {
    worker_capability_catalog_with_writes(false)
}

/// Build the worker registry with the optional Phase-2 memory writers.
///
/// The flag is intentionally supplied by the process owner rather than read
/// from this module so the default registry remains read-only and cannot be
/// widened by a request or a catalog argument.
pub fn worker_capability_catalog_with_writes(
    writes_enabled: bool,
) -> Result<BTreeMap<String, CapabilityDescriptorV2>, PlatformCatalogError> {
    const EXECUTABLE_READ_CAPABILITIES: &[&str] = &[
        "confluence_read",
        "search_knowledge_base",
        "web_fetch",
        "search_web",
        "read_tool_artifact",
        "todo_read",
        "read_attachment",
        "local_node_catalog",
        "local_node_describe",
    ];
    const EXECUTABLE_WRITE_CAPABILITIES: &[&str] = &[
        "confluence_write",
        "generate_image",
        "generate_quiz",
        "mcp_docgen__generate_document",
        "todo_write",
        "update_user_memory",
        "execute_python_code",
        "local_node_action",
    ];

    let catalog = load_platform_catalog()?;
    let mut descriptors = BTreeMap::new();
    for record in catalog.capabilities.iter().filter(|record| {
        record.implementation_owner == "worker"
            && ((record.effect == "read"
                && EXECUTABLE_READ_CAPABILITIES.contains(&record.id.as_str()))
                || (record.effect == "write"
                    && writes_enabled
                    && EXECUTABLE_WRITE_CAPABILITIES.contains(&record.id.as_str())))
    }) {
        let descriptor = to_capability_descriptor(record)?;
        if descriptors
            .insert(descriptor.id.clone(), descriptor)
            .is_some()
        {
            // `load_platform_catalog` already rejects duplicate IDs.  Keep
            // this guard local as a fail-closed invariant if the projection
            // filter or source changes later.
            return Err(PlatformCatalogError::Duplicate(
                "projected id",
                record.id.clone(),
            ));
        }
    }

    for descriptor in executable_worker_descriptors(writes_enabled) {
        descriptor.validate().map_err(|error| {
            PlatformCatalogError::Descriptor(descriptor.id.clone(), error.to_string())
        })?;
        if descriptors
            .insert(descriptor.id.clone(), descriptor)
            .is_some()
        {
            return Err(PlatformCatalogError::Duplicate(
                "projected id",
                "builtin".into(),
            ));
        }
    }

    if EXECUTABLE_READ_CAPABILITIES
        .iter()
        .any(|capability_id| !descriptors.contains_key(*capability_id))
        || (writes_enabled
            && ["execute_python_code", "local_node_action"]
                .iter()
                .any(|capability_id| !descriptors.contains_key(*capability_id)))
        || (writes_enabled
            && EXECUTABLE_WRITE_CAPABILITIES
                .iter()
                .any(|capability_id| !descriptors.contains_key(*capability_id)))
    {
        return Err(PlatformCatalogError::Field(
            "worker_registry".to_string(),
            "implemented capability missing",
        ));
    }

    Ok(descriptors)
}

fn executable_worker_descriptors(writes_enabled: bool) -> Vec<CapabilityDescriptorV2> {
    let mut descriptors = Vec::new();
    if writes_enabled {
        descriptors.extend([
            builtin_descriptor(
                "mcp_docgen__modify_document",
                "Modify an existing Office document",
                CapabilityEffect::Write,
                ApprovalPolicy::OnRequest,
                ExecutionMode::Job,
                json!({
                    "type": "object",
                    "properties": {
                        "format": {"type": "string", "enum": ["docx", "pptx", "xlsx"]},
                        "source_base64": {"type": "string", "minLength": 1},
                        "title": {"type": "string", "minLength": 1, "maxLength": 300},
                        "goal": {"type": "string", "minLength": 1, "maxLength": 2000},
                        "body_markdown": {"type": "string", "maxLength": 20000},
                        "locale": {"type": "string", "minLength": 2, "maxLength": 16},
                        "design_system": {
                            "type": "string",
                            "enum": ["claude", "stripe", "carbon", "keynote", "editorial", "enterprise"]
                        },
                        "template_name": {"type": "string", "minLength": 1, "maxLength": 128}
                    },
                    "required": ["format", "source_base64", "title", "goal"],
                    "additionalProperties": false
                }),
                vec!["kind:tool", "category:office", "owner:worker"],
            ),
            builtin_descriptor(
                "mcp_docgen__preview_document",
                "Preview an existing Office document",
                CapabilityEffect::Read,
                ApprovalPolicy::Never,
                ExecutionMode::Inline,
                json!({
                    "type": "object",
                    "properties": {
                        "format": {"type": "string", "enum": ["docx", "pptx", "xlsx", "pdf"]},
                        "source_base64": {"type": "string", "minLength": 1}
                    },
                    "required": ["format", "source_base64"],
                    "additionalProperties": false
                }),
                vec!["kind:tool", "category:office", "owner:worker"],
            ),
        ]);
    }
    descriptors
}

fn builtin_descriptor(
    id: &str,
    description: &str,
    effect: CapabilityEffect,
    approval_policy: ApprovalPolicy,
    execution_mode: ExecutionMode,
    input_schema: Value,
    mut tags: Vec<&str>,
) -> CapabilityDescriptorV2 {
    tags.sort_unstable();
    let tags = tags.into_iter().map(str::to_owned).collect();
    CapabilityDescriptorV2 {
        schema_version: CAPABILITY_DESCRIPTOR_SCHEMA_VERSION.into(),
        id: id.into(),
        name: id.into(),
        version: "1".into(),
        description: description.into(),
        schema_hash: canonical_json_hash(&input_schema).unwrap_or_else(|_| "sha256:invalid".into()),
        input_schema,
        output_schema: json!({"type":"object"}),
        effect,
        approval_policy,
        execution_mode,
        timeout_ms: if execution_mode == ExecutionMode::Job {
            120_000
        } else {
            30_000
        },
        tags,
        protocol: "internal".into(),
        connector_binding: None,
    }
}

fn is_capability_name(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_in_catalog_loads() {
        let catalog = load_platform_catalog().expect("catalog must be valid");
        assert!((MIN_CAPABILITIES..=MAX_CAPABILITIES).contains(&catalog.capabilities.len()));
        assert_eq!(
            catalog.gateway_policy.high_risk_tools,
            vec!["system_run_lite", "browser_action_lite"]
        );
        assert_eq!(
            catalog.gateway_policy.medium_risk_tools,
            vec!["execute_python_code", "confluence_write"]
        );
    }

    #[test]
    fn every_checked_in_record_projects_to_a_valid_descriptor() {
        let catalog = load_platform_catalog().expect("catalog must be valid");
        for record in &catalog.capabilities {
            let descriptor = to_capability_descriptor(record).expect("descriptor must validate");
            assert_eq!(descriptor.id, record.id);
            assert_eq!(descriptor.name, record.name);
            assert_eq!(
                descriptor.version,
                record.version.as_deref().unwrap_or("null")
            );
            assert_eq!(descriptor.schema_hash, record.schema_hash);
            assert_eq!(descriptor.input_schema, record.input_schema);
            assert_eq!(descriptor.protocol, record.protocol);
            assert_eq!(descriptor.timeout_ms, record.timeout_ms);
            assert!(
                descriptor
                    .tags
                    .contains(&format!("category:{}", record.category))
            );
            assert!(descriptor.tags.windows(2).all(|pair| pair[0] < pair[1]));
        }
    }

    #[test]
    fn worker_registry_contains_only_implemented_read_capabilities() {
        let descriptors = worker_capability_catalog().expect("worker catalog must load");
        for capability_id in [
            "confluence_read",
            "read_tool_artifact",
            "search_knowledge_base",
            "todo_read",
            "web_fetch",
            "read_attachment",
        ] {
            assert!(descriptors.contains_key(capability_id), "{capability_id}");
        }
        assert!(!descriptors.contains_key("fs_read"));
        assert!(!descriptors.contains_key("fs_glob"));
        assert!(!descriptors.contains_key("fs_grep"));
        assert!(
            descriptors["confluence_read"]
                .tags
                .iter()
                .any(|tag| tag == "binding-type:connector")
        );
        assert!(descriptors.contains_key("todo_read"));
        assert!(!descriptors.contains_key("platform.echo"));
        assert!(!descriptors.contains_key("platform.read_fixture"));
        assert!(!descriptors.contains_key("execute_python_code"));
        assert!(!descriptors.contains_key("local_node_action"));
        assert_eq!(
            descriptors["read_attachment"].effect,
            CapabilityEffect::Read
        );
    }

    #[test]
    fn worker_registry_exposes_implemented_writers_only_when_enabled() {
        let default = worker_capability_catalog().expect("worker catalog must load");
        assert!(!default.contains_key("todo_write"));
        assert!(!default.contains_key("update_user_memory"));

        let enabled =
            worker_capability_catalog_with_writes(true).expect("writes catalog must load");
        assert!(enabled.contains_key("todo_write"));
        assert!(enabled.contains_key("update_user_memory"));
        assert!(enabled.contains_key("generate_quiz"));
        assert!(enabled.contains_key("confluence_write"));
        assert!(enabled.contains_key("generate_image"));
        assert!(enabled.contains_key("execute_python_code"));
        assert!(enabled.contains_key("local_node_action"));
        assert!(enabled.contains_key("mcp_docgen__generate_document"));
        for capability_id in [
            "mcp_docgen__modify_document",
            "mcp_docgen__preview_document",
        ] {
            let descriptor = &enabled[capability_id];
            descriptor
                .validate()
                .expect("Office descriptor must be valid");
            let properties = descriptor.input_schema["properties"]
                .as_object()
                .expect("Office descriptor properties");
            for required in descriptor.input_schema["required"]
                .as_array()
                .expect("Office descriptor required")
            {
                assert!(properties.contains_key(required.as_str().expect("required name")));
            }
        }
        assert!(enabled.len() >= default.len() + 6);
        assert!(
            enabled
                .values()
                .filter(|descriptor| descriptor.effect == CapabilityEffect::Write)
                .all(|descriptor| {
                    matches!(
                        descriptor.id.as_str(),
                        "execute_python_code"
                            | "local_node_action"
                            | "mcp_docgen__modify_document"
                            | "confluence_write"
                            | "generate_image"
                            | "generate_quiz"
                            | "mcp_docgen__generate_document"
                            | "todo_write"
                            | "update_user_memory"
                    )
                })
        );
    }

    #[test]
    fn metadata_category_and_permissions_fail_closed() {
        let catalog = load_platform_catalog().expect("catalog must be valid");
        let mut record = catalog.capabilities[0].clone();

        record.category = "kb".to_string();
        assert!(matches!(
            validate_record(&record),
            Err(PlatformCatalogError::Field(_, "category"))
        ));

        record.category = catalog.capabilities[0].category.clone();
        record.required_permissions = vec!["tier:unknown".to_string()];
        assert!(validate_record(&record).is_ok());
        record.required_permissions = vec!["permission with spaces".to_string()];
        assert!(matches!(
            validate_record(&record),
            Err(PlatformCatalogError::Field(_, "required_permissions"))
        ));

        record.required_permissions = vec!["role:reader".to_string(), "role:reader".to_string()];
        assert!(matches!(
            validate_record(&record),
            Err(PlatformCatalogError::Field(_, "required_permissions"))
        ));

        record.required_permissions.clear();
        record.when_to_use = Some("\u{0000}".to_string());
        assert!(matches!(
            validate_record(&record),
            Err(PlatformCatalogError::Field(_, "when_to_use"))
        ));
    }
}
