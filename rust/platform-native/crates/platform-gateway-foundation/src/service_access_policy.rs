//! Source-faithful service policy, visibility and connector masking primitives.
//!
//! Authority: `src/core/auth/service_access.py`, the pure services.py helpers,
//! and the uncached load path of `service_access_resolver.py`. Construction is
//! inert; no routes, database, cache, provider, or task is started here.
//!
//! API-key and tenant scopes intersect before exact-role `admin` bypasses only
//! the user policy. This is independent of RBAC capability authorization: callers
//! must still run the existing ServiceListRead capability gate. Empty normalized
//! scopes match all, but an explicitly empty user allowlist denies. Denials take
//! precedence. Alias matching is any-of within each source, all-of across sources.
//!
//! The uncached loader awaits an injected store, preserving source lookup order
//! and independent ordinary-exception fallback. Cancellation propagates. Its
//! warnings are emitted through the required host hook. Request caching, TTL/key construction,
//! monotonic expiry, global bounded-cache eviction/locking, and invalidation are
//! explicit resolver-adapter work; this loader must not replace that adapter.
//!
//! JSON values use existing Python truthiness/str/whitespace helpers. Python
//! tuples/sets can be supplied in their observed iteration order as JSON arrays;
//! Python-only objects, arbitrary integers and nonfinite numbers are outside the
//! JSON domain. Masking intentionally recurses through dictionaries only, as the
//! source does; dictionaries inside lists are NOT scrubbed by this helper.
use crate::{
    protocol::ServiceDefinition,
    service_registry::python_truthy,
    usage_parser::{python_value_to_string, python_whitespace},
};
use ai_platform_auth::admission::RequestPrincipal;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{future::Future, pin::Pin};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceAccessMode {
    #[default]
    All,
    Allowlist,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ServiceAccessPolicy {
    pub mode: ServiceAccessMode,
    pub allowed_services: Vec<String>,
    pub denied_services: Vec<String>,
}

fn normalize_token(value: &Value) -> String {
    if !python_truthy(value) {
        return String::new();
    }
    python_value_to_string(value)
        .trim_matches(python_whitespace)
        .to_lowercase()
}

fn normalize_strings<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        let token = value.trim_matches(python_whitespace).to_lowercase();
        if !token.is_empty() && !result.contains(&token) {
            result.push(token);
        }
    }
    result
}

/// Comma splitting occurs only for the outer string, never for list elements.
/// Unsupported outer values (including objects) normalize to an empty scope.
pub fn normalize_service_scope(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => normalize_strings(text.split(',')),
        Value::Array(items) => normalize_service_candidates(items),
        _ => Vec::new(),
    }
}

pub fn normalize_service_candidates(values: &[Value]) -> Vec<String> {
    let mut result = Vec::new();
    for value in values {
        let token = normalize_token(value);
        if !token.is_empty() && !result.contains(&token) {
            result.push(token);
        }
    }
    result
}

fn normalized_scope_matches(scope: &[String], candidates: &[String]) -> bool {
    if scope.is_empty() {
        return true;
    }
    if candidates.is_empty() {
        return false;
    }
    scope.iter().any(|rule| {
        rule == "*"
            || rule.strip_suffix('*').map_or_else(
                || candidates.contains(rule),
                |prefix| {
                    candidates
                        .iter()
                        .any(|candidate| candidate.starts_with(prefix))
                },
            )
    })
}

pub fn service_scope_matches(scope: &Value, candidates: &[Value]) -> bool {
    normalized_scope_matches(
        &normalize_service_scope(scope),
        &normalize_service_candidates(candidates),
    )
}

pub fn service_access_policy_from_metadata(metadata: &Value) -> ServiceAccessPolicy {
    let payload = metadata
        .as_object()
        .and_then(|m| m.get("service_access"))
        .and_then(Value::as_object);
    let field = |name: &str| payload.and_then(|p| p.get(name)).unwrap_or(&Value::Null);
    let allowed_services = normalize_service_scope(field("allowed_services"));
    let denied_services = normalize_service_scope(field("denied_services"));
    let mode = match normalize_token(field("mode")).as_str() {
        "all" => ServiceAccessMode::All,
        "allowlist" => ServiceAccessMode::Allowlist,
        _ if !allowed_services.is_empty() => ServiceAccessMode::Allowlist,
        _ => ServiceAccessMode::All,
    };
    ServiceAccessPolicy {
        mode,
        allowed_services,
        denied_services,
    }
}

pub fn evaluate_service_access(
    policy: &ServiceAccessPolicy,
    candidates: &[Value],
) -> (bool, &'static str) {
    let candidates = normalize_service_candidates(candidates);
    // Check original sequence emptiness before normalization, as the dataclass
    // can be constructed with a nonempty sequence containing only blank rules.
    if !policy.denied_services.is_empty()
        && normalized_scope_matches(
            &normalize_strings(policy.denied_services.iter().map(String::as_str)),
            &candidates,
        )
    {
        return (false, "denied_by_user_policy");
    }
    if policy.mode == ServiceAccessMode::Allowlist {
        if policy.allowed_services.is_empty() {
            return (false, "user_allowlist_empty");
        }
        if !normalized_scope_matches(
            &normalize_strings(policy.allowed_services.iter().map(String::as_str)),
            &candidates,
        ) {
            return (false, "not_in_user_allowlist");
        }
    }
    (true, "allowed")
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ServiceAccessConstraints {
    /// Source order is API key, then tenant. Labels are retained, not merged.
    pub allowed_sources: Vec<(String, Vec<String>)>,
    pub user_policy: ServiceAccessPolicy,
}

fn legacy_service_visible_with_scopes(
    aliases: &[Value],
    allowed_sources: &[(String, Vec<String>)],
    user_policy: &ServiceAccessPolicy,
    is_admin: bool,
) -> bool {
    let aliases = normalize_service_candidates(aliases);
    if aliases.is_empty() {
        return false;
    }
    for (_, allowed) in allowed_sources {
        if !normalized_scope_matches(
            &normalize_strings(allowed.iter().map(String::as_str)),
            &aliases,
        ) {
            return false;
        }
    }
    if is_admin {
        return true;
    }
    evaluate_service_access(
        user_policy,
        &aliases.into_iter().map(Value::String).collect::<Vec<_>>(),
    )
    .0
}

pub fn service_visible_with_scopes(
    aliases: &[Value],
    allowed_sources: &[(String, Vec<String>)],
    user_policy: &ServiceAccessPolicy,
    is_admin: bool,
) -> bool {
    let legacy =
        legacy_service_visible_with_scopes(aliases, allowed_sources, user_policy, is_admin);
    use ai_platform_auth::cedar::resources::{
        self, ObjectAction, ObjectProfile, ObjectResource, ObjectSubject, ResourceKind,
    };
    let mut subject = resources::current_subject().unwrap_or_else(|| ObjectSubject {
        tenant_id: "public".into(),
        user_id: "anonymous".into(),
        anonymous: true,
        ..Default::default()
    });
    // is_admin is the canonical caller's exact-role fact, not its policy result.
    subject.roles.retain(|v| v != "admin");
    if is_admin {
        subject.roles.push("admin".into());
    }
    let aliases = normalize_service_candidates(aliases);
    let mut resource = ObjectResource::new(
        ResourceKind::Tool,
        aliases.first().map(String::as_str).unwrap_or("missing"),
        &subject.tenant_id,
    );
    resource.profile = ObjectProfile::ToolVisibility;
    resource.aliases = aliases.iter().cloned().collect();
    resource.allowlist_mode = user_policy.mode == ServiceAccessMode::Allowlist;
    for (index, (name, scope)) in allowed_sources.iter().enumerate() {
        let key = format!("{index}:{name}");
        resource.required_scopes.insert(key.clone());
        if normalized_scope_matches(
            &normalize_strings(scope.iter().map(String::as_str)),
            &aliases,
        ) {
            resource.satisfied_scopes.insert(key);
        }
    }
    for alias in &aliases {
        if !user_policy.denied_services.is_empty()
            && normalized_scope_matches(
                &normalize_strings(user_policy.denied_services.iter().map(String::as_str)),
                std::slice::from_ref(alias),
            )
        {
            resource.denied_aliases.insert(alias.clone());
        }
        if !user_policy.allowed_services.is_empty()
            && normalized_scope_matches(
                &normalize_strings(user_policy.allowed_services.iter().map(String::as_str)),
                std::slice::from_ref(alias),
            )
        {
            resource.allowed_aliases.insert(alias.clone());
        }
    }
    resources::compare_current(&subject, &resource, ObjectAction::Invoke, legacy) && legacy
}
/// Do not substitute an admin-like role, permission, tier or wildcard here.
pub fn service_visible_for_roles(
    aliases: &[Value],
    constraints: &ServiceAccessConstraints,
    roles: &[String],
) -> bool {
    service_visible_with_scopes(
        aliases,
        &constraints.allowed_sources,
        &constraints.user_policy,
        roles.iter().any(|role| role == "admin"),
    )
}

pub fn registered_service_visible(
    service: &ServiceDefinition,
    constraints: &ServiceAccessConstraints,
    roles: &[String],
) -> bool {
    service_visible_for_roles(
        &[json!(service.service_id), json!(service.name)],
        constraints,
        roles,
    )
}

pub const ASSISTANT_SERVICE_ALIASES: &[&str] = &["assistant", "ai 助手", "ai assistant"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAccessError {
    /// Truthy request.state.api_key_info lacks dict.get; source propagates this.
    ApiKeyInfoNotMapping,
    /// Truthy connector configuration lacks dict.items; source propagates this.
    ConnectorConfigNotMapping,
    Cancelled,
}

const SENSITIVE_CONNECTOR_FIELDS: &[&str] = &[
    "auth_token",
    "api_key",
    "api_secret",
    "password",
    "secret",
    "access_token",
    "refresh_token",
    "private_key",
    "credentials",
    "langsmith_api_key",
    "headers",
];

pub fn mask_sensitive_config(config: &Value) -> Result<Value, ServiceAccessError> {
    if !python_truthy(config) {
        return Ok(json!({}));
    }
    let config = config
        .as_object()
        .ok_or(ServiceAccessError::ConnectorConfigNotMapping)?;
    let mut masked = Map::new();
    for (key, value) in config {
        let value = if SENSITIVE_CONNECTOR_FIELDS.contains(&key.to_lowercase().as_str()) {
            match value {
                Value::String(text) if !text.is_empty() => {
                    let count = text.chars().count();
                    if count > 4 {
                        json!(format!(
                            "***{}",
                            text.chars().skip(count - 4).collect::<String>()
                        ))
                    } else {
                        json!("***")
                    }
                }
                Value::Object(_) => json!("***[hidden]***"),
                _ => json!("***"),
            }
        } else if value.is_object() {
            mask_sensitive_config(value)?
        } else {
            value.clone()
        };
        masked.insert(key.clone(), value);
    }
    Ok(Value::Object(masked))
}

/// This is resolved request data, never credential material. A trusted identity
/// resolver may use from_principal; it does not authenticate arbitrary input.
#[derive(Clone, Default)]
pub struct ServiceAccessLoadContext {
    pub api_key_info: Value,
    pub user_id: String,
    pub tenant_id: String,
    pub is_authenticated: bool,
}
impl ServiceAccessLoadContext {
    pub fn from_principal(principal: &RequestPrincipal) -> Self {
        Self {
            api_key_info: principal.api_key().map_or(
                Value::Null,
                |key| json!({"allowed_services":key.allowed_services}),
            ),
            user_id: principal.user_id().into(),
            tenant_id: principal.tenant_id().into(),
            is_authenticated: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAccessLookupError {
    Ordinary,
    Cancelled,
}
pub type ServiceAccessLookupFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Value, ServiceAccessLookupError>> + Send + 'a>>;

/// Return Null for absent records. JSON rows preserve malformed-data fallback.
/// This seam does not issue SQL or create a new database abstraction/connection.
pub trait ServiceAccessConstraintStore: Send + Sync {
    fn enabled(&self) -> bool;
    fn get_tenant<'a>(&'a self, tenant_id: &'a str) -> ServiceAccessLookupFuture<'a>;
    fn get_user<'a>(&'a self, user_id: &'a str) -> ServiceAccessLookupFuture<'a>;
    /// Emit immediately, before a later lookup can fail or be cancelled.
    fn warn_lookup_failure(&self, source: ServiceAccessLookupFailure, identity: &str);
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAccessLookupFailure {
    Tenant,
    User,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServiceAccessLoad {
    pub constraints: ServiceAccessConstraints,
    /// Already emitted through the host hook. Backend error text is absent.
    pub warnings: Vec<ServiceAccessLookupFailure>,
}

fn record_field(row: &Value, field: &str) -> Result<Value, ServiceAccessLookupError> {
    if !python_truthy(row) {
        return Ok(Value::Null);
    }
    row.as_object()
        .map(|row| row.get(field).cloned().unwrap_or(Value::Null))
        .ok_or(ServiceAccessLookupError::Ordinary)
}

/// Use only after the host resolver has checked request/global caches. Successful
/// results must be published through that resolver's cache contract, not here.
pub async fn load_uncached_service_access_constraints(
    context: &ServiceAccessLoadContext,
    database: Option<&dyn ServiceAccessConstraintStore>,
) -> Result<ServiceAccessLoad, ServiceAccessError> {
    let mut load = ServiceAccessLoad::default();
    if python_truthy(&context.api_key_info) {
        let info = context
            .api_key_info
            .as_object()
            .ok_or(ServiceAccessError::ApiKeyInfoNotMapping)?;
        let allowed = normalize_service_scope(info.get("allowed_services").unwrap_or(&Value::Null));
        if !allowed.is_empty() {
            load.constraints
                .allowed_sources
                .push(("api_key".into(), allowed));
        }
    }
    let Some(database) = database.filter(|db| db.enabled() && context.is_authenticated) else {
        return Ok(load);
    };
    if !context.tenant_id.is_empty() {
        match database
            .get_tenant(&context.tenant_id)
            .await
            .and_then(|row| record_field(&row, "allowed_services"))
        {
            Ok(value) => {
                let allowed = normalize_service_scope(&value);
                if !allowed.is_empty() {
                    load.constraints
                        .allowed_sources
                        .push(("tenant".into(), allowed));
                }
            }
            Err(ServiceAccessLookupError::Ordinary) => {
                database
                    .warn_lookup_failure(ServiceAccessLookupFailure::Tenant, &context.tenant_id);
                load.warnings.push(ServiceAccessLookupFailure::Tenant);
            }
            Err(ServiceAccessLookupError::Cancelled) => return Err(ServiceAccessError::Cancelled),
        }
    }
    if !context.user_id.is_empty() {
        match database
            .get_user(&context.user_id)
            .await
            .and_then(|row| record_field(&row, "metadata"))
        {
            Ok(metadata) => {
                load.constraints.user_policy = service_access_policy_from_metadata(&metadata)
            }
            Err(ServiceAccessLookupError::Ordinary) => {
                database.warn_lookup_failure(ServiceAccessLookupFailure::User, &context.user_id);
                load.warnings.push(ServiceAccessLookupFailure::User);
            }
            Err(ServiceAccessLookupError::Cancelled) => return Err(ServiceAccessError::Cancelled),
        }
    }
    Ok(load)
}

#[cfg(test)]
#[path = "service_access_policy_cedar_tests.rs"]
mod cedar_differential_tests;
