//! Gateway-owned Assistant config, tool, and policy projections.
//!
//! Contract sources: `src/api/v1/_assistant_routes/catalog.py`,
//! `src/core/assistant_capability_catalog.py`, and
//! `tests/api/test_assistant_control_plane_routes.py`.
//! Execution stays with the existing worker: this module only validates declarative
//! data and reads tenant policy. It never imports, starts, or contacts a runtime.
//!
//! `data/assistant_catalog_v1.json` is a byte-for-byte packaging snapshot of
//! `rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/platform_catalog_v1.json`.
//! Source SHA-256: 478175466f8c9f13e578ac0440eaaeaa5501cf56d49faf742feb37e2a2e4785d.
//! Keep that snapshot in sync with the canonical worker data; the optional source
//! equality test checks this in the full repository without requiring the overlay
//! to compile or run the independent native source bundle.

#[path = "assistant_plugins.rs"]
pub mod plugins;
#[path = "assistant_plugin_lifecycle.rs"]
pub mod lifecycle;
#[path = "assistant_plugin_mutations.rs"]
mod plugin_mutations;

use ai_platform_auth::{
    api_keys::{SelectedCredential, select_credential},
    service::{AuthService, Principal},
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, put},
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

const BUNDLED_CATALOG: &[u8] = include_bytes!("../data/assistant_catalog_v1.json");
const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
const SCHEMA_VERSION: &str = "ai-platform-capability-catalog/v1";
const ROOT_KEYS: &[&str] = &[
    "schema_version",
    "capability_revision",
    "gateway_policy",
    "capabilities",
];
const REQUIRED_RECORD_KEYS: &[&str] = &[
    "name",
    "id",
    "version",
    "schema_hash",
    "description",
    "input_schema",
    "read_only",
    "effect",
    "risk",
    "protocol",
    "kind",
    "approval",
    "category",
    "when_to_use",
    "when_not_to_use",
    "requires_confirmation",
    "required_permissions",
    "timeout_ms",
    "implementation_owner",
];
const OPTIONAL_RECORD_KEYS: &[&str] = &[
    "connector_provider",
    "content_hash",
    "permissions",
    "plugin_id",
    "plugin_version",
    "plugin_metadata",
];

/// No raw file contents, paths, database errors, or provider data in public errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogError {
    Unavailable,
    Malformed,
    AuthenticationRequired,
}
type CatalogResult<T> = Result<T, CatalogError>;

fn check(ok: bool) -> CatalogResult<()> {
    if ok {
        Ok(())
    } else {
        Err(CatalogError::Malformed)
    }
}
use crate::native_contract::{is_name, PLUGINS, PLUGIN_LIFECYCLE};
fn one_of(value: &Value, options: &[&str]) -> bool {
    value.as_str().is_some_and(|value| options.contains(&value))
}
fn valid_hash(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        value.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        })
    })
}

/// Python's JSON schema fingerprint uses sorted keys, compact separators, and
/// unescaped Unicode. Sort explicitly so a downstream `preserve_order` feature
/// cannot silently change the hash contract.
fn canonical_json(value: &Value, output: &mut String) -> CatalogResult<()> {
    match value {
        Value::Object(object) => {
            output.push('{');
            let keys: BTreeSet<_> = object.keys().collect();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).map_err(|_| CatalogError::Malformed)?);
                output.push(':');
                canonical_json(&object[key], output)?;
            }
            output.push('}');
        }
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                canonical_json(value, output)?;
            }
            output.push(']');
        }
        Value::Number(number) if number.is_f64() => {
            output.push_str(&python_float(
                number.as_f64().ok_or(CatalogError::Malformed)?,
            ));
        }
        _ => output.push_str(&serde_json::to_string(value).map_err(|_| CatalogError::Malformed)?),
    }
    Ok(())
}

// serde_json and Python use shortest round-trip digits but different scientific
// notation thresholds/exponent padding. Normalize those differences for schema
// numbers (including 0.0 and -0.0, which are distinct fingerprint inputs).
pub fn python_float(value: f64) -> String {
    let raw = serde_json::to_string(&value).expect("JSON values contain finite numbers");
    let (sign, unsigned) = if let Some(value) = raw.strip_prefix('-') {
        ("-", value)
    } else {
        ("", raw.as_str())
    };
    let (mantissa, exponent) = unsigned.split_once('e').map_or((unsigned, 0), |(m, e)| {
        (m, e.parse::<i32>().expect("serializer exponent"))
    });
    let decimal_at = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let mut digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let zeros = digits.bytes().take_while(|c| *c == b'0').count();
    if zeros == digits.len() {
        return format!("{sign}0.0");
    }
    let scientific_exponent = exponent + decimal_at - zeros as i32 - 1;
    digits.drain(..zeros);
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    if !(-4..16).contains(&scientific_exponent) {
        let fraction = if digits.len() == 1 {
            String::new()
        } else {
            format!(".{}", &digits[1..])
        };
        return format!("{sign}{}{fraction}e{scientific_exponent:+03}", &digits[..1]);
    }
    let decimal = scientific_exponent + 1;
    if decimal <= 0 {
        format!("{sign}0.{}{}", "0".repeat((-decimal) as usize), digits)
    } else if decimal as usize >= digits.len() {
        format!(
            "{sign}{digits}{}.0",
            "0".repeat(decimal as usize - digits.len())
        )
    } else {
        format!(
            "{sign}{}.{}",
            &digits[..decimal as usize],
            &digits[decimal as usize..]
        )
    }
}

fn schema_hash(value: &Value) -> CatalogResult<String> {
    let mut canonical = String::new();
    canonical_json(value, &mut canonical)?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())))
}

fn validate_permissions(value: &Value) -> CatalogResult<()> {
    let values = value.as_array().ok_or(CatalogError::Malformed)?;
    check(values.len() <= 32)?;
    let mut seen = BTreeSet::new();
    for value in values {
        let value = value.as_str().ok_or(CatalogError::Malformed)?;
        check(is_name(value) && seen.insert(value))?;
    }
    Ok(())
}

fn validate_record(record: &Value) -> CatalogResult<()> {
    let object = record.as_object().ok_or(CatalogError::Malformed)?;
    check(
        REQUIRED_RECORD_KEYS
            .iter()
            .all(|key| object.contains_key(*key)),
    )?;
    check(object.keys().all(|key| {
        REQUIRED_RECORD_KEYS.contains(&key.as_str()) || OPTIONAL_RECORD_KEYS.contains(&key.as_str())
    }))?;
    check(record["name"].as_str().is_some_and(is_name) && record["id"] == record["name"])?;
    check(
        record["description"]
            .as_str()
            .is_some_and(|value| !value.trim().is_empty()),
    )?;
    check(record["input_schema"].is_object())?;
    check(record["schema_hash"].as_str() == Some(schema_hash(&record["input_schema"])?.as_str()))?;
    check(record["version"].is_null() || record["version"].is_string())?;
    check(record["read_only"].is_boolean() && record["requires_confirmation"].is_boolean())?;
    for (key, options) in [
        ("effect", &["read", "write", "unknown"][..]),
        ("risk", &["low", "medium", "high", "critical"][..]),
        ("approval", &["never", "on_request", "always"][..]),
        (
            "protocol",
            &[
                "internal",
                "mcp",
                "http",
                "local",
                "local-node-capability/v2",
            ][..],
        ),
        (
            "kind",
            &[
                "tool",
                "knowledge",
                "platform_tool_discovery",
                "skill",
                "mcp",
                "fixture",
            ][..],
        ),
        (
            "category",
            &[
                "retrieval",
                "generation",
                "analysis",
                "integration",
                "utility",
                "skill",
                "mcp",
                "local",
            ][..],
        ),
        ("implementation_owner", &["runtime", "worker"][..]),
    ] {
        check(one_of(&record[key], options))?;
    }
    for key in ["when_to_use", "when_not_to_use"] {
        check(
            record[key].is_null()
                || record[key].as_str().is_some_and(|value| {
                    !value.trim().is_empty()
                        && value.chars().count() <= 20_000
                        && value.chars().all(|c| c >= ' ' || c == '\r' || c == '\n')
                }),
        )?;
    }
    validate_permissions(&record["required_permissions"])?;
    check(
        record["timeout_ms"]
            .as_u64()
            .is_some_and(|value| (1..=300_000).contains(&value)),
    )?;
    if !record["connector_provider"].is_null() {
        check(
            record["connector_provider"].as_str().is_some_and(is_name)
                && record["implementation_owner"] == "worker"
                && record["protocol"] == "internal"
                && record["kind"] == "tool",
        )?;
    }
    if !record["content_hash"].is_null() {
        check(valid_hash(&record["content_hash"]))?;
    }
    if !record["permissions"].is_null() {
        validate_permissions(&record["permissions"])?;
    }
    for key in ["plugin_id", "plugin_version"] {
        check(record[key].is_null() || record[key].as_str().is_some_and(is_name))?;
    }
    if !record["plugin_metadata"].is_null() {
        let metadata = record["plugin_metadata"]
            .as_object()
            .ok_or(CatalogError::Malformed)?;
        check(
            metadata
                .iter()
                .all(|(key, value)| is_name(key) && !value.is_array() && !value.is_object()),
        )?;
    }
    if record["effect"] == "read" {
        check(record["read_only"] == true && record["approval"] == "never")?;
    } else {
        check(record["read_only"] == false && record["approval"] != "never")?;
    }
    Ok(())
}

#[derive(Clone, Debug)]
struct GatewayPolicy {
    high_risk_tools: Vec<String>,
    medium_risk_tools: Vec<String>,
}
fn validate_gateway_policy(value: &Value) -> CatalogResult<GatewayPolicy> {
    let object = value.as_object().ok_or(CatalogError::Malformed)?;
    check(
        object.len() == 2
            && object.contains_key("high_risk_tools")
            && object.contains_key("medium_risk_tools"),
    )?;
    let mut seen = BTreeSet::new();
    let mut lists = Vec::new();
    for key in ["high_risk_tools", "medium_risk_tools"] {
        let values = value[key].as_array().ok_or(CatalogError::Malformed)?;
        check(values.len() <= 256)?;
        let mut checked = Vec::new();
        for value in values {
            let name = value.as_str().ok_or(CatalogError::Malformed)?;
            check(is_name(name) && seen.insert(name))?;
            checked.push(name.to_owned());
        }
        lists.push(checked);
    }
    Ok(GatewayPolicy {
        high_risk_tools: lists.remove(0),
        medium_risk_tools: lists.remove(0),
    })
}

/// Immutable, fully validated snapshot. Only projections expose its contents.
#[derive(Clone, Debug)]
pub struct CapabilityCatalog {
    revision: u64,
    gateway_policy: GatewayPolicy,
    records: Vec<Value>,
}
impl CapabilityCatalog {
    pub fn from_bytes(bytes: &[u8]) -> CatalogResult<Self> {
        check(bytes.len() <= MAX_CATALOG_BYTES)?;
        let value: Value = serde_json::from_slice(bytes).map_err(|_| CatalogError::Malformed)?;
        let root = value.as_object().ok_or(CatalogError::Malformed)?;
        check(
            root.len() == ROOT_KEYS.len() && ROOT_KEYS.iter().all(|key| root.contains_key(*key)),
        )?;
        check(value["schema_version"] == SCHEMA_VERSION)?;
        let revision = value["capability_revision"]
            .as_u64()
            .filter(|value| *value >= 1)
            .ok_or(CatalogError::Malformed)?;
        let gateway_policy = validate_gateway_policy(&value["gateway_policy"])?;
        let records = value["capabilities"]
            .as_array()
            .ok_or(CatalogError::Malformed)?;
        check((1..=256).contains(&records.len()))?;
        let mut names = BTreeSet::new();
        for record in records {
            validate_record(record)?;
            check(names.insert(record["name"].as_str().ok_or(CatalogError::Malformed)?))?;
        }
        Ok(Self {
            revision,
            gateway_policy,
            records: records.clone(),
        })
    }
    pub fn bundled() -> CatalogResult<Self> {
        Self::from_bytes(BUNDLED_CATALOG)
    }
    /// Call during application assembly, outside an async request reactor.
    /// No fallback to bundled data when an explicitly selected file is invalid.
    pub fn from_path(path: &Path) -> CatalogResult<Self> {
        let file = File::open(path).map_err(|_| CatalogError::Unavailable)?;
        check(
            file.metadata()
                .map_err(|_| CatalogError::Unavailable)?
                .len()
                <= MAX_CATALOG_BYTES as u64,
        )?;
        let mut bytes = Vec::new();
        file.take(MAX_CATALOG_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| CatalogError::Unavailable)?;
        Self::from_bytes(&bytes)
    }
    pub fn records(&self) -> &[Value] { &self.records }
    pub fn revision(&self) -> u64 {
        self.revision
    }
}

/// The existing ToolRegistry permission lattice. An admin role bypasses roles
/// and named permissions, but does not bypass an explicitly required tier.
pub fn user_has_required_permissions(tier: &str, roles: &[String], required: &[String]) -> bool {
    if required.is_empty() {
        return true;
    }
    fn rank(tier: &str) -> Option<usize> {
        ["anonymous", "normal", "premium", "enterprise", "admin"]
            .iter()
            .position(|value| *value == tier)
    }
    let tier = if tier.is_empty() {
        "anonymous".to_owned()
    } else {
        tier.to_lowercase()
    };
    let Some(tier_rank) = rank(&tier) else {
        return false;
    };
    let admin = roles.iter().any(|role| role == "admin");
    required.iter().all(|permission| {
        if let Some(role) = permission.strip_prefix("role:") {
            admin || roles.iter().any(|value| value == role.trim())
        } else if let Some(tier) = permission.strip_prefix("tier:") {
            rank(&tier.trim().to_lowercase())
                .is_some_and(|required_rank| tier_rank >= required_rank)
        } else {
            admin || roles.contains(permission)
        }
    })
}

fn normalise_policy_list(value: Option<&Value>) -> CatalogResult<Vec<String>> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(Vec::new());
    };
    let values = value.as_array().ok_or(CatalogError::Malformed)?;
    check(values.len() <= 256)?;
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for value in values {
        let value = value.as_str().ok_or(CatalogError::Malformed)?.trim();
        check(is_name(value))?;
        if seen.insert(value) {
            result.push(value.to_owned());
        }
    }
    Ok(result)
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ToolInfoResponse {
    pub name: String,
    pub description: String,
    pub category: String,
    pub risk_level: String,
    pub capability_kind: String,
    pub read_only: bool,
    pub effect: String,
    pub approval_policy: String,
    pub when_to_use: Option<String>,
    pub when_not_to_use: Option<String>,
}

pub fn project_tools(
    catalog: &CapabilityCatalog,
    authenticated: bool,
    tier: &str,
    roles: &[String],
    tenant_policy: &Value,
) -> CatalogResult<Vec<ToolInfoResponse>> {
    if !authenticated {
        return Err(CatalogError::AuthenticationRequired);
    }
    let policy = tenant_policy.as_object().ok_or(CatalogError::Malformed)?;
    let allowed = normalise_policy_list(policy.get("allowed_tools"))?;
    let blocked = normalise_policy_list(policy.get("blocked_tools"))?;
    let categories = normalise_policy_list(
        policy
            .get("allowed_tool_categories")
            .or_else(|| policy.get("allowed_categories")),
    )?;
    let mut tools = Vec::new();
    for record in &catalog.records {
        // Deployment tombstones remain in the catalog for generation checking;
        // they are never executable or user-visible tool availability.
        if record["plugin_metadata"]["worker_active"] == false {
            continue;
        }
        // All accesses below are to fields checked by CapabilityCatalog.
        let name = record["name"].as_str().expect("validated name");
        let category = record["category"].as_str().expect("validated category");
        let required: Vec<String> = record["required_permissions"]
            .as_array()
            .expect("validated permissions")
            .iter()
            .map(|value| value.as_str().expect("validated permission").to_owned())
            .collect();
        if !user_has_required_permissions(tier, roles, &required)
            || (!allowed.is_empty() && !allowed.iter().any(|value| value == name))
            || blocked.iter().any(|value| value == name)
            || (!categories.is_empty() && !categories.iter().any(|value| value == category))
        {
            continue;
        }
        tools.push(ToolInfoResponse {
            name: name.to_owned(),
            description: record["description"]
                .as_str()
                .expect("validated description")
                .to_owned(),
            category: category.to_owned(),
            risk_level: record["risk"].as_str().expect("validated risk").to_owned(),
            capability_kind: record["kind"].as_str().expect("validated kind").to_owned(),
            read_only: record["read_only"].as_bool().expect("validated read flag"),
            effect: record["effect"].as_str().expect("validated effect").to_owned(),
            approval_policy: record["approval"].as_str().expect("validated approval").to_owned(),
            when_to_use: record["when_to_use"].as_str().map(str::to_owned),
            when_not_to_use: record["when_not_to_use"].as_str().map(str::to_owned),
        });
    }
    Ok(tools)
}

/// Non-secret settings only. Accept a lookup for deterministic tests and explicit
/// application wiring instead of reading unrelated environment or credentials.
#[derive(Clone, Debug)]
pub struct PolicySettings {
    configured_execution_profile_supported: bool,
    default_memory_mode: String,
    os_agent_default_enabled: bool,
    gateway_enabled: bool,
}
impl Default for PolicySettings {
    fn default() -> Self {
        Self {
            configured_execution_profile_supported: true,
            default_memory_mode: "auto".to_owned(),
            os_agent_default_enabled: false,
            gateway_enabled: true,
        }
    }
}
impl PolicySettings {
    pub fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> CatalogResult<Self> {
        fn boolean(value: Option<String>, default: bool) -> CatalogResult<bool> {
            match value
                .as_deref()
                .map(str::trim)
                .map(str::to_lowercase)
                .as_deref()
            {
                None | Some("") => Ok(default),
                Some("1" | "true" | "yes" | "on") => Ok(true),
                Some("0" | "false" | "no" | "off") => Ok(false),
                _ => Err(CatalogError::Malformed),
            }
        }
        let configured_execution_profile_supported = lookup("ASSISTANT_DEFAULT_EXECUTION_PROFILE")
            .unwrap_or_else(|| "safe".to_owned())
            .trim()
            .to_lowercase()
            == "safe";
        let default_memory_mode = lookup("ASSISTANT_DEFAULT_MEMORY_MODE")
            .unwrap_or_else(|| "auto".to_owned())
            .trim()
            .to_lowercase();
        check(["auto", "on", "off"].contains(&default_memory_mode.as_str()))?;
        Ok(Self {
            configured_execution_profile_supported,
            default_memory_mode,
            os_agent_default_enabled: boolean(lookup("ASSISTANT_OS_AGENT_LITE"), false)?,
            gateway_enabled: boolean(lookup("ASSISTANT_GATEWAY_ENABLED"), true)?,
        })
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AssistantPolicies {
    pub default_execution_profile: &'static str,
    pub supported_execution_profiles: Vec<&'static str>,
    pub configured_execution_profile_supported: bool,
    pub default_memory_mode: String,
    pub os_agent_default_enabled: bool,
    pub high_risk_tools: Vec<String>,
    pub medium_risk_tools: Vec<String>,
    pub gateway_enabled: bool,
    pub allowed_tools: Vec<String>,
    pub blocked_tools: Vec<String>,
    pub allowed_tool_categories: Vec<String>,
    pub max_calls_per_session: i64,
    pub max_calls_per_minute: i64,
}

// Production columns are nullable integers. Preserve Python's `value or default`
// for NULL/zero while rejecting dirty, negative, or excessive stored limits.
fn policy_limit(value: Option<&Value>, default: i64, max: i64) -> CatalogResult<i64> {
    let limit = match value {
        None | Some(Value::Null) => default,
        Some(value) => match value.as_i64() {
            Some(0) => default,
            Some(value) => value,
            None => return Err(CatalogError::Malformed),
        },
    };
    check((1..=max).contains(&limit))?;
    Ok(limit)
}

pub fn project_policies(
    catalog: &CapabilityCatalog,
    tenant_policy: &Value,
    settings: &PolicySettings,
) -> CatalogResult<AssistantPolicies> {
    let policy = tenant_policy.as_object().ok_or(CatalogError::Malformed)?;
    Ok(AssistantPolicies {
        // A legacy setting never advertises an execution mode the Runtime cannot enforce.
        default_execution_profile: "safe",
        supported_execution_profiles: vec!["safe"],
        configured_execution_profile_supported: settings.configured_execution_profile_supported,
        default_memory_mode: settings.default_memory_mode.clone(),
        os_agent_default_enabled: settings.os_agent_default_enabled,
        high_risk_tools: catalog.gateway_policy.high_risk_tools.clone(),
        medium_risk_tools: catalog.gateway_policy.medium_risk_tools.clone(),
        gateway_enabled: settings.gateway_enabled,
        allowed_tools: normalise_policy_list(policy.get("allowed_tools"))?,
        blocked_tools: normalise_policy_list(policy.get("blocked_tools"))?,
        allowed_tool_categories: normalise_policy_list(policy.get("allowed_categories"))?,
        max_calls_per_session: policy_limit(policy.get("max_calls_per_session"), 100, 100_000)?,
        max_calls_per_minute: policy_limit(policy.get("max_calls_per_minute"), 20, 10_000)?,
    })
}

#[derive(Clone, Copy)]
pub struct ConfigModel<'a> {
    pub id: &'a str,
    pub provider: &'a str,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct AssistantConfigResponse {
    pub default_model_id: String,
    pub available_providers: Vec<String>,
    pub kb_enabled: bool,
    pub web_search_enabled: bool,
    pub tools_available: Vec<String>,
}

/// Normalize only the already-installed safe getter's result. Missing or failed
/// getters must be represented by `None`; never discover/import a runtime here.
pub fn installed_tool_names(value: Option<&Value>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    let value = if let Some(object) = value.as_object() {
        object
            .get("tools")
            .or_else(|| object.get("capabilities"))
            .unwrap_or(&Value::Null)
    } else {
        value
    };
    let Some(values) = value.as_array() else {
        return Vec::new();
    };
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    for value in values {
        let name = if value.is_object() {
            value.get("name").and_then(Value::as_str)
        } else {
            value.as_str()
        };
        if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty())
            && seen.insert(name)
        {
            names.push(name.to_owned());
        }
    }
    names
}

/// Input models must come from the shared, tenant-authorized model projection.
/// Provider state is only a tenant-scoped boolean: never pass credentials, keys,
/// provider rows, or endpoint configuration into this public projection.
pub fn project_config(
    visible_models: &[ConfigModel<'_>],
    requested_default: &str,
    configured_providers: &BTreeMap<String, bool>,
    kb_enabled: bool,
    installed_catalog: Option<&Value>,
) -> AssistantConfigResponse {
    let requested_default = requested_default.trim();
    let default_model_id = visible_models
        .iter()
        .find(|model| model.id == requested_default)
        .or_else(|| visible_models.first())
        .map_or("", |model| model.id)
        .to_owned();
    let mut seen = BTreeSet::new();
    let available_providers = visible_models
        .iter()
        .filter_map(|model| {
            if !model.provider.is_empty()
                && seen.insert(model.provider)
                && configured_providers.get(model.provider) == Some(&true)
            {
                Some(model.provider.to_owned())
            } else {
                None
            }
        })
        .collect();
    AssistantConfigResponse {
        default_model_id,
        available_providers,
        kb_enabled,
        web_search_enabled: true,
        tools_available: installed_tool_names(installed_catalog),
    }
}

/// Assembly-time immutable options. Invalid options are retained so each route
/// returns its stable 503 response rather than silently substituting defaults.
#[derive(Clone)]
pub struct AssistantCatalogOptions {
    pub catalog: CatalogResult<Arc<CapabilityCatalog>>,
    pub policy_settings: CatalogResult<PolicySettings>,
}
impl Default for AssistantCatalogOptions {
    fn default() -> Self {
        Self {
            catalog: CapabilityCatalog::bundled().map(Arc::new),
            policy_settings: Ok(PolicySettings::default()),
        }
    }
}
impl AssistantCatalogOptions {
    /// Reads only the explicit catalog path and the four non-secret policy
    /// controls used by Python. Call at application assembly, not per request.
    pub fn from_environment() -> Self {
        let path = std::env::var("AI_PLATFORM_CAPABILITY_CATALOG_PATH").unwrap_or_default();
        let path = path.trim();
        let catalog = if path.is_empty() {
            CapabilityCatalog::bundled()
        } else {
            let expanded = if path == "~" || path.starts_with("~/") {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(path.strip_prefix("~/").unwrap_or("")))
                    .unwrap_or_else(|| PathBuf::from(path))
            } else {
                PathBuf::from(path)
            };
            CapabilityCatalog::from_path(&expanded)
        };
        Self {
            catalog: catalog.map(Arc::new),
            policy_settings: PolicySettings::from_lookup(|name| std::env::var(name).ok()),
        }
    }
}

#[derive(Clone)]
struct CatalogState {
    auth: Arc<AuthService>,
    pool: PgPool,
    options: AssistantCatalogOptions,
}

/// Mount alongside the existing Gateway router; no listener or worker is started.
/// JWT-only authentication matches the native Gateway's current auth integration.
pub fn router(auth: Arc<AuthService>, pool: PgPool, options: AssistantCatalogOptions) -> Router {
    Router::new()
        .route("/api/v1/assistant/tools", get(list_tools))
        .route(PLUGINS, get(plugins::list_plugins))
        .route(PLUGIN_LIFECYCLE, put(plugin_mutations::update_plugin))
        .route("/api/v1/assistant/policies", get(get_policies))
        .with_state(CatalogState {
            auth,
            pool,
            options,
        })
}

fn error(status: StatusCode, detail: &'static str) -> Response {
    (status, Json(json!({ "detail": detail }))).into_response()
}
#[allow(
    clippy::result_large_err,
    reason = "Axum response is returned directly on the cold authentication error path"
)]
async fn authenticate(state: &CatalogState, headers: &HeaderMap) -> Result<Principal, Response> {
    let token = match select_credential(headers, &HeaderName::from_static("x-api-key")) {
        Ok(SelectedCredential::Jwt(token)) => token,
        _ => return Err(error(StatusCode::UNAUTHORIZED, "authentication required")),
    };
    let principal = state.auth.authenticate(token).await.map_err(|failure| {
        error(
            StatusCode::from_u16(failure.status()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            "Authentication failed",
        )
    })?;
    if principal.account.user_id.is_empty() || principal.account.tenant_id.is_empty() {
        return Err(error(StatusCode::UNAUTHORIZED, "authentication required"));
    }
    Ok(principal)
}

#[derive(FromRow)]
struct TenantPolicyRow {
    allowed_tools: Option<Value>,
    blocked_tools: Option<Value>,
    allowed_categories: Option<Value>,
    max_calls_per_session: Option<i32>,
    max_calls_per_minute: Option<i32>,
}
const TENANT_POLICY_SQL: &str = "SELECT to_jsonb(allowed_tools) AS allowed_tools, to_jsonb(blocked_tools) AS blocked_tools, to_jsonb(allowed_categories) AS allowed_categories, max_calls_per_session, max_calls_per_minute FROM gateway.tenant_tool_policies WHERE tenant_id = $1";
async fn load_tenant_policy(pool: &PgPool, tenant_id: &str) -> CatalogResult<Value> {
    // Any disabled/closed/unavailable storage stays an error, never allow-all.
    let row = sqlx::query_as::<_, TenantPolicyRow>(TENANT_POLICY_SQL)
        .bind(if tenant_id.is_empty() {
            "default"
        } else {
            tenant_id
        })
        .fetch_optional(pool)
        .await
        .map_err(|_| CatalogError::Unavailable)?;
    Ok(match row {
        None => json!({}),
        Some(row) => json!({
            "allowed_tools": row.allowed_tools,
            "blocked_tools": row.blocked_tools,
            "allowed_categories": row.allowed_categories,
            "max_calls_per_session": row.max_calls_per_session,
            "max_calls_per_minute": row.max_calls_per_minute,
        }),
    })
}

async fn resolved_policies(
    state: &CatalogState,
    principal: &Principal,
    catalog: &CapabilityCatalog,
) -> CatalogResult<AssistantPolicies> {
    let settings = state
        .options
        .policy_settings
        .as_ref()
        .map_err(|error| *error)?;
    let policy = load_tenant_policy(&state.pool, &principal.account.tenant_id).await?;
    project_policies(catalog, &policy, settings)
}

async fn list_tools(State(state): State<CatalogState>, headers: HeaderMap) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let catalog = match &state.options.catalog {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Assistant tool catalog is unavailable",
            );
        }
    };
    let policies = match resolved_policies(&state, &principal, catalog).await {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Assistant policy is unavailable",
            );
        }
    };
    let policy = match serde_json::to_value(policies) {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Assistant policy is unavailable",
            );
        }
    };
    let denied = match lifecycle::load_bindings(&state.pool, &principal.account.tenant_id).await
        .and_then(|bindings| lifecycle::denied_capabilities(catalog, &bindings)) {
        Ok(denied) => denied,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "Assistant plugin policy is unavailable"),
    };
    match project_tools(
        catalog,
        true,
        &principal.authority.tier,
        &principal.authority.roles,
        &policy,
    ) {
        Ok(tools) => {
            let tools: Vec<_> = tools.into_iter().filter(|tool| !denied.contains(&tool.name)).collect();
            Json(json!({ "tools": tools })).into_response()
        },
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Assistant tool catalog is unavailable",
        ),
    }
}
async fn get_policies(State(state): State<CatalogState>, headers: HeaderMap) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let catalog = match &state.options.catalog {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Assistant policy is unavailable",
            );
        }
    };
    match resolved_policies(&state, &principal, catalog).await {
        Ok(policies) => Json(json!({ "policies": policies })).into_response(),
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Assistant policy is unavailable",
        ),
    }
}
