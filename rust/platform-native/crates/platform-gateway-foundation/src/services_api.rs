//! Opt-in canonical `src/api/v1/services.py` routes. No route is mounted by construction.
//!
//! Registry, access-policy/resolver and LangGraph validators are the existing
//! source-backed implementations. Required host seams supply authenticated identity,
//! actual runtime readiness, proxy invalidation and sanitized exception rendering.
//! Read visibility intersects API-key and tenant constraints even for exact `admin`;
//! only that exact role bypasses user policy and sees unmasked connector secrets.
//! Schema intentionally omits detail's visibility lookup. Writes additionally require
//! explicit platform-admin identity after ServiceConfigWrite. No allow-all adapter.
//!
//! The source's Enum string guard makes availability unreachable for ordinary typed
//! registry rows (`str(ServiceType.LANGGRAPH)` != `langgraph`). The optional proxy
//! seam explicitly marks legacy string-valued source rows; ordinary enums stay null.
//! The assistant filter intentionally does not filter registered service types or
//! apply tag filters to the virtual assistant. Captured live records retain identity;
//! source annotation-violating Python records remain the registry's typed boundary.
//!
//! Cancellation drops the current await, with no spawned work or rollback of an
//! earlier write. Hosts own database cancellation supervision. Exception text erased
//! by native services cannot be recovered: required rendering is a safe host boundary.
//! JSON parsing precedes identity; dictionary validation follows identity. Absent or
//! empty Content-Type retains bytes. Malformed JSON diagnostics, non-UTF8 diagnostics,
//! arbitrary Python objects/integers and trusted-origin slash redirects are bounded
//! host concerns. HEAD/method/encoded-slash rejection precedes collaborators.
pub use crate::service_config_api::{
    ServiceConfigCancellation as ServicesCancellation, ServiceConfigError as ServicesError,
};
use crate::{
    config_audit_writer::ConfigAuditRequestContext,
    langgraph_service_validation::{
        LangGraphModelReader, LangGraphProviderReader, LangGraphValidationError,
        normalize_langgraph_connector_config, normalize_langgraph_connector_config_live,
        validate_langgraph_model_override, validate_langgraph_model_override_live,
    },
    protocol::{ContentType, InvocationMode, ServiceDefinition, ServiceType, TransportType},
    service_access_policy::{
        ServiceAccessConstraintStore, ServiceAccessError, mask_sensitive_config,
        registered_service_visible, service_visible_for_roles,
    },
    service_access_resolver::{
        ServiceAccessConstraintResolver, ServiceAccessRequestCache, ServiceAccessResolverContext,
    },
    service_ownership::{ConnectorHandle, ServiceHandle},
    service_registry::{RegistryError, ServiceRegistry, python_truthy, service_from_value},
    usage_parser::{python_value_to_string, python_whitespace},
};
use ai_platform_auth::rbac::{Capability, Rbac, check_capability, permission_denied_detail};
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{FromRequest, MatchedPath, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use crate::native_contract::services as contract;
pub use contract::SERVICE_MUTABLE_FIELDS;
// Keep atomic registration's existing native-only route ownership here. Common
// entries reference the very same declarations consumed by the offline exporter.
pub const SERVICES_ROUTES: &[(&str, &str, &str)] = &[
    contract::SERVICES_ROUTES[0],
    ("POST", "/service-registrations", "create_service_registration"),
    contract::SERVICES_ROUTES[1],
    contract::SERVICES_ROUTES[2],
    contract::SERVICES_ROUTES[3],
    contract::SERVICES_ROUTES[4],
    contract::SERVICES_ROUTES[5],
];
fn allowed_methods(route: &str) -> Vec<&'static str> {
    if route == crate::native_contract::SERVICE_REGISTRATIONS {
        vec!["POST"]
    } else {
        contract::allowed_methods(route)
    }
}
pub type ServicesFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ServicesError>> + Send + 'a>>;
#[derive(Clone, Default)]
pub struct ServicesIdentity {
    pub user_id: String,
    pub tenant_id: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    /// Populated from get_user_context only for list/detail. AuthContext roles and
    /// UserContext roles may differ; this context retains the latter's cache key.
    pub access: ServiceAccessResolverContext,
}
pub trait ServicesIdentityResolver: Send + Sync {
    /// Source order: auth dependency, then user dependency iff needs_user.
    /// A host may reuse its request's authenticated/cached principal, not body data.
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
        needs_user: bool,
    ) -> ServicesFuture<'a, ServicesIdentity>;
    fn context(&self, headers: &HeaderMap) -> ConfigAuditRequestContext;
    /// Only write denials schedule this event in the canonical source.
    fn permission_denied(
        &self,
        identity: &ServicesIdentity,
        context: &ConfigAuditRequestContext,
        capability: Capability,
    );
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServicesRuntimeStatus {
    pub control_present: bool,
    pub image_worker_running: bool,
}
pub trait ServicesRuntime: Send + Sync {
    fn status(&self) -> ServicesRuntimeStatus;
}
pub trait ServicesProxyInvalidation: Send + Sync {
    fn invalidate(&self, service_id: &str) -> Result<(), ServicesError>;
}
/// Exactly the arguments supplied to canonical ProxyServiceConfig. Its other
/// constructor defaults belong to the host's real transparent-proxy implementation.
#[derive(Clone, Debug, PartialEq)]
pub struct ServicesProxyConfig {
    pub service_id: String,
    pub service_name: String,
    pub upstream_url: String,
    pub upstream_urls: Vec<Value>,
    pub assistant_id: Value,
    pub graph_id: Value,
    pub metadata: Value,
    pub enabled: bool,
}
pub trait ServicesTransparentProxy: Send + Sync {
    /// True only when the original host row held the literal string `langgraph`,
    /// not a ServiceType enum. This is source representation, not availability.
    fn legacy_string_service_type(&self, service: &ServiceDefinition) -> bool;
    fn availability<'a>(&'a self, config: &'a ServicesProxyConfig) -> ServicesFuture<'a, Value>;
}
pub trait ServicesExceptionText: Send + Sync {
    fn registry(&self, error: &RegistryError) -> String;
    fn validation(&self, error: &LangGraphValidationError) -> String;
    fn internal(&self) -> String;
}
pub struct ServicesDependencies {
    pub identity: Arc<dyn ServicesIdentityResolver>,
    pub rbac: Rbac,
    pub registry: Arc<ServiceRegistry>,
    pub access_resolver: ServiceAccessConstraintResolver,
    pub access_database: Option<Arc<dyn ServiceAccessConstraintStore>>,
    pub providers: Option<Arc<dyn LangGraphProviderReader>>,
    pub models: Option<Arc<dyn LangGraphModelReader>>,
    pub runtime: Arc<dyn ServicesRuntime>,
    pub proxy_invalidation: Option<Arc<dyn ServicesProxyInvalidation>>,
    pub transparent_proxy: Option<Arc<dyn ServicesTransparentProxy>>,
    pub exception_text: Arc<dyn ServicesExceptionText>,
}
pub struct ServicesApi {
    dependencies: ServicesDependencies,
}
#[derive(Clone, Debug)]
pub enum ServicesOperation {
    Register,
    CreateRegistration,
    List {
        service_type: Option<String>,
        tags: Vec<String>,
    },
    Get {
        service_id: String,
    },
    Update {
        service_id: String,
    },
    Delete {
        service_id: String,
    },
    Schema {
        service_id: String,
    },
}
impl ServicesOperation {
    fn path(&self) -> String {
        match self {
            Self::CreateRegistration => crate::native_contract::SERVICE_REGISTRATIONS.into(),
            Self::Register | Self::List { .. } => "/api/v1/services".into(),
            Self::Get { service_id }
            | Self::Update { service_id }
            | Self::Delete { service_id } => format!("/api/v1/services/{service_id}"),
            Self::Schema { service_id } => format!("/api/v1/services/{service_id}/schema"),
        }
    }
    fn write(&self) -> bool {
        matches!(
            self,
            Self::Register | Self::CreateRegistration | Self::Update { .. } | Self::Delete { .. }
        )
    }
    fn body(&self) -> bool {
        matches!(self, Self::Register | Self::CreateRegistration | Self::Update { .. })
    }
    fn needs_user(&self) -> bool {
        matches!(self, Self::List { .. } | Self::Get { .. })
    }
}
fn access_error(error: ServiceAccessError) -> ServicesError {
    if error == ServiceAccessError::Cancelled {
        ServicesError::Cancelled
    } else {
        ServicesError::Internal
    }
}
fn field<'a>(value: &'a Value, name: &str) -> &'a Value {
    value.get(name).unwrap_or(&Value::Null)
}
fn or<'a>(left: &'a Value, right: &'a Value) -> &'a Value {
    if python_truthy(left) { left } else { right }
}
fn string(value: &Value) -> String {
    if python_truthy(value) {
        python_value_to_string(value)
    } else {
        String::new()
    }
}

/// Source detail projection, not ServiceDefinition's full serialization. Masking
/// recurses into dictionaries only; do not add a list-recursive sanitizer here.
pub fn service_to_dict(
    service: &ServiceDefinition,
    mask_secrets: bool,
) -> Result<Value, ServicesError> {
    let mut data = serde_json::to_value(service).map_err(|_| ServicesError::Internal)?;
    if service.service_config.is_none() {
        data.as_object_mut().unwrap().remove("service_config");
    }
    for key in [
        "connector_config",
        "input_schema",
        "output_schema",
        "metadata",
    ] {
        if !python_truthy(&data[key]) {
            data[key] = json!({});
        }
    }
    if mask_secrets {
        data["connector_config"] =
            mask_sensitive_config(&data["connector_config"]).map_err(access_error)?;
    }
    // SDK provider credentials are write-only even for administrator detail reads.
    if let Some(config) = data["connector_config"].as_object_mut() {
        let raw = config.remove("sdk_provider_api_key");
        let sealed = config.remove("sdk_provider_api_key_encrypted");
        let configured = raw.or(sealed).is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()));
        if configured { data["metadata"]["sdk_provider_key_configured"] = json!(true); }
    }
    if service.service_type == ServiceType::Langgraph {
        let adapter = string(or(
            &data["metadata"]["adapter_type"],
            &data["connector_config"]["adapter_type"],
        ));
        if adapter.trim_matches(python_whitespace).is_empty() {
            data["metadata"]["adapter_type"] = json!("langgraph");
        }
        let proxy = string(or(
            &data["metadata"]["proxy_mode"],
            &data["connector_config"]["proxy_mode"],
        ));
        let proxy = proxy.trim_matches(python_whitespace);
        if !proxy.is_empty() && !python_truthy(&data["metadata"]["proxy_mode"]) {
            data["metadata"]["proxy_mode"] = json!(proxy);
        }
    }
    Ok(data)
}
impl ServicesApi {
    pub fn new(dependencies: ServicesDependencies) -> Self {
        Self { dependencies }
    }
    pub async fn handle(
        &self,
        operation: ServicesOperation,
        headers: &HeaderMap,
        body: &Value,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, ServicesError> {
        let identity = self
            .dependencies
            .identity
            .resolve(headers, &operation.path(), operation.needs_user())
            .await?;
        self.execute(operation, &identity, body, context).await
    }
    pub async fn execute(
        &self,
        operation: ServicesOperation,
        identity: &ServicesIdentity,
        body: &Value,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, ServicesError> {
        if matches!(operation, ServicesOperation::CreateRegistration) && !body.is_object() {
            return Err(ServicesError::detail(422, "invalid_sdk_service_registration"));
        }
        if operation.body() {
            validate_dictionary(body)?;
        }
        let d = &self.dependencies;
        let capability = if operation.write() {
            Capability::ServiceConfigWrite
        } else {
            Capability::ServiceListRead
        };
        let roles: Vec<_> = identity
            .roles
            .iter()
            .map(|v| v.trim_matches(python_whitespace).to_owned())
            .collect();
        let permissions: Vec<_> = identity
            .permissions
            .iter()
            .map(|v| v.trim_matches(python_whitespace).to_owned())
            .collect();
        let allowed = check_capability(&d.rbac, &roles, &permissions, capability).allowed();
        let platform = roles.iter().chain(&permissions).any(|s| {
            matches!(
                s.to_lowercase().as_str(),
                "platform_admin" | "superadmin" | "super_admin"
            )
        });
        if !allowed || (operation.write() && !platform) {
            if operation.write() {
                d.identity.permission_denied(identity, context, capability);
            }
            let trace = context
                .request_id
                .as_ref()
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| context.trace_id.as_ref().and_then(Value::as_str))
                .unwrap_or("");
            let mut detail = permission_denied_detail(capability, trace, None);
            if allowed && operation.write() {
                detail["reason"] = json!("platform_admin_required");
            }
            return Err(ServicesError::detail(403, detail));
        }
        match operation {
            ServicesOperation::Register => self.register(body, identity, false).await,
            ServicesOperation::CreateRegistration => self.register(body, identity, true).await,
            ServicesOperation::List { service_type, tags } => {
                self.list(service_type.as_deref(), &tags, identity).await
            }
            ServicesOperation::Get { service_id } => {
                let service = self.service(&service_id).await?;
                let constraints = d
                    .access_resolver
                    .load(
                        &mut ServiceAccessRequestCache::new(),
                        &identity.access,
                        d.access_database.as_deref(),
                    )
                    .await
                    .map_err(access_error)?;
                let service = service.snapshot();
                if !registered_service_visible(&service, &constraints, &identity.roles) {
                    return Err(ServicesError::detail(403, "service not accessible"));
                }
                service_to_dict(&service, !identity.roles.iter().any(|v| v == "admin"))
            }
            ServicesOperation::Update { service_id } => {
                self.update(&service_id, body, identity).await
            }
            ServicesOperation::Delete { service_id } => {
                let deleted = d
                    .registry
                    .storage
                    .delete(&service_id)
                    .await
                    .map_err(|_| ServicesError::Internal)?;
                d.registry.invalidate_service_cache(&service_id);
                self.invalidate(&service_id)?;
                if !deleted {
                    return Err(ServicesError::detail(404, "service not found"));
                }
                Ok(contract::changed(&service_id))
            }
            ServicesOperation::Schema { service_id } => {
                let s = self.service(&service_id).await?.snapshot();
                Ok(contract::schema_value(&s.input_schema, &s.output_schema,
                    s.accepted_content_types.iter().map(|v| v.as_str()).collect(),
                    s.output_content_types.iter().map(|v| v.as_str()).collect()))
            }
        }
    }
    async fn service(&self, id: &str) -> Result<ServiceHandle, ServicesError> {
        self.dependencies
            .registry
            .get(id)
            .await
            .map_err(|_| ServicesError::Internal)?
            .ok_or_else(|| ServicesError::detail(404, "service not found"))
    }
    fn invalidate(&self, id: &str) -> Result<(), ServicesError> {
        if let Some(proxy) = &self.dependencies.proxy_invalidation {
            proxy.invalidate(id)?;
        }
        self.dependencies.access_resolver.clear_cache();
        Ok(())
    }
    fn caught(&self, error: ServicesError) -> ServicesError {
        match error {
            ServicesError::Http { .. } | ServicesError::Cancelled => error,
            ServicesError::Exception(text) => ServicesError::detail(400, text),
            ServicesError::Internal => {
                ServicesError::detail(400, self.dependencies.exception_text.internal())
            }
        }
    }
    async fn validate(
        &self,
        definition: &mut Value,
        previous: Option<&Value>,
        identity: &ServicesIdentity,
    ) -> Result<(), ServicesError> {
        let d = &self.dependencies;
        validate_langgraph_model_override(
            if identity.tenant_id.is_empty() {
                "default"
            } else {
                &identity.tenant_id
            },
            definition,
            previous,
            d.providers.as_deref(),
            d.models.as_deref(),
        )
        .await
        .map_err(|e| match e {
            LangGraphValidationError::Http { status, detail } => {
                ServicesError::detail(status, detail)
            }
            LangGraphValidationError::Cancelled => ServicesError::Cancelled,
            _ => ServicesError::Exception(d.exception_text.validation(&e)),
        })
    }
    async fn validate_live(
        &self,
        definition: &mut Value,
        previous: Option<&ConnectorHandle>,
        connector: &ConnectorHandle,
        identity: &ServicesIdentity,
    ) -> Result<(), ServicesError> {
        let d = &self.dependencies;
        validate_langgraph_model_override_live(
            if identity.tenant_id.is_empty() {
                "default"
            } else {
                &identity.tenant_id
            },
            definition,
            connector,
            previous,
            d.providers.as_deref(),
            d.models.as_deref(),
        )
        .await
        .map_err(|e| match e {
            LangGraphValidationError::Http { status, detail } => {
                ServicesError::detail(status, detail)
            }
            LangGraphValidationError::Cancelled => ServicesError::Cancelled,
            _ => ServicesError::Exception(d.exception_text.validation(&e)),
        })
    }
    async fn persist(
        &self,
        definition: &Value,
        connector: Option<ConnectorHandle>,
        create_only: bool,
    ) -> Result<String, ServicesError> {
        let d = &self.dependencies;
        // _service_from_dict indexes this field; preserve its safe KeyError text
        // before the mature registry's missing-ID ValidationFailed boundary.
        if definition.get("service_id").is_none() && missing_service_id_prechecks_pass(definition) {
            return Err(ServicesError::Exception("'service_id'".into()));
        }
        let service = service_from_value(definition)
            .map_err(|e| ServicesError::Exception(d.exception_text.registry(&e)))?;
        let id = service.service_id.clone();
        let service = match connector {
            Some(connector) => {
                ServiceHandle::with_connector(service, connector.dictionary_or_empty())
            }
            None => ServiceHandle::new(service),
        };
        let result = if create_only { d.registry.create_handle(service).await }
            else { d.registry.register_handle(service).await };
        result.map_err(|e| match e {
            RegistryError::AlreadyExists => ServicesError::detail(409, "service_id_conflict"),
            RegistryError::AtomicInsertUnsupported => ServicesError::detail(501, "atomic_service_registration_unsupported"),
            _ => ServicesError::Exception(d.exception_text.registry(&e)),
        })?;
        self.invalidate(&id)?;
        Ok(id)
    }
    async fn register(
        &self,
        body: &Value,
        identity: &ServicesIdentity,
        create_only: bool,
    ) -> Result<Value, ServicesError> {
        let result = async {
            if create_only && !jsonschema::is_valid(&crate::native_contract::service_registration_schema(), body) {
                return Err(ServicesError::detail(422, "invalid_sdk_service_registration"));
            }
            if body["connector_config"].get("sdk_provider_api_key_encrypted").is_some() {
                return Err(ServicesError::detail(400,"sdk_ciphertext_input_forbidden"));
            }
            let mut definition = body.clone();
            normalize_langgraph_connector_config(&mut definition);
            self.validate(&mut definition, None, identity).await?;
            let id = self.persist(&definition, None, create_only).await?;
            Ok(contract::registered(&id))
        }
        .await;
        result.map_err(|e| self.caught(e))
    }
    async fn update(
        &self,
        id: &str,
        body: &Value,
        identity: &ServicesIdentity,
    ) -> Result<Value, ServicesError> {
        if body["connector_config"].get("sdk_provider_api_key_encrypted").is_some() {
            return Err(ServicesError::detail(400,"sdk_ciphertext_input_forbidden"));
        }
        let service = self.service(id).await?;
        if body
            .get("service_id")
            .is_some_and(|v| python_value_to_string(v) != id)
        {
            return Err(ServicesError::detail(400, "service_id is immutable"));
        }
        let filtered: Vec<_> = body
            .as_object()
            .expect("validated dictionary")
            .iter()
            .filter(|(key, _)| SERVICE_MUTABLE_FIELDS.contains(&key.as_str()))
            .collect();
        if filtered.is_empty() {
            return Err(ServicesError::detail(400, "no mutable fields provided"));
        }
        // Internal update seed retains write-only values; public projection never does.
        let mut definition = serde_json::to_value(service.snapshot()).map_err(|_| ServicesError::Internal)?;
        for (key, value) in filtered {
            definition[key] = value.clone();
        }
        // Keep the previous dictionary itself, never a registry lookup by ID.
        let previous = service.connector();
        let connector = match body.get("connector_config") {
            Some(value) => {
                let mut value = value.clone();
                if matches!(definition["metadata"]["adapter_type"].as_str(), Some("openai_agents" | "claude_agent_sdk")) {
                    if let Some(config) = value.as_object_mut() {
                        if !config.contains_key("sdk_provider_api_key") {
                            if let Some(key) = previous.snapshot().get("sdk_provider_api_key_encrypted") {
                                config.insert("sdk_provider_api_key_encrypted".into(), key.clone());
                            }
                        }
                    }
                }
                ConnectorHandle::new(value)
            },
            None => previous.dictionary_or_empty(),
        };
        normalize_langgraph_connector_config_live(&mut definition, &connector);
        // These awaits precede Python's try block. Failed validation retains the
        // already-applied normalization on a shared old connector.
        self.validate_live(&mut definition, Some(&previous), &connector, identity)
            .await?;
        self.persist(&definition, Some(connector), false)
            .await
            .map_err(|e| self.caught(e))?;
        Ok(contract::changed(id))
    }
    async fn list(
        &self,
        service_type: Option<&str>,
        tags: &[String],
        identity: &ServicesIdentity,
    ) -> Result<Value, ServicesError> {
        let d = &self.dependencies;
        let constraints = d
            .access_resolver
            .load(
                &mut ServiceAccessRequestCache::new(),
                &identity.access,
                d.access_database.as_deref(),
            )
            .await
            .map_err(access_error)?;
        let mut output = Vec::new();
        if (service_type.is_none() || service_type == Some("assistant"))
            && service_visible_for_roles(
                &[json!("assistant"), json!("ai 助手"), json!("ai assistant")],
                &constraints,
                &identity.roles,
            )
        {
            let status = d.runtime.status();
            output.push(contract::virtual_assistant(status.control_present, status.image_worker_running));
        }
        let kind = match service_type.filter(|s| !s.is_empty() && *s != "assistant") {
            Some(s) => Some(
                serde_json::from_value::<ServiceType>(json!(s))
                    .map_err(|_| ServicesError::Internal)?,
            ),
            None => None,
        };
        for s in d
            .registry
            .list(kind, tags, None)
            .await
            .map_err(|_| ServicesError::Internal)?
        {
            let initial = s.snapshot();
            if !registered_service_visible(&initial, &constraints, &identity.roles) {
                continue;
            }
            let availability = self.availability(&initial).await?;
            let s = s.snapshot();
            output.push(contract::listed_service(&s.service_id, &s.name, &s.description, &s.version,
                s.service_type.as_str(), s.supported_modes.iter().map(|v| v.as_str()).collect(),
                s.accepted_content_types.iter().map(|v| v.as_str()).collect(),
                s.output_content_types.iter().map(|v| v.as_str()).collect(),
                &s.status, &s.tags, &s.metadata, &availability["availability_status"],
                &availability["last_health_check_at"], &availability["last_health_error"]));
        }
        Ok(json!(output))
    }
    async fn availability(&self, s: &ServiceDefinition) -> Result<Value, ServicesError> {
        let Some(proxy) = &self.dependencies.transparent_proxy else {
            return Ok(Value::Null);
        };
        let connector = &s.connector_config;
        if s.service_type != ServiceType::Langgraph
            || !proxy.legacy_string_service_type(s)
            || string(or(
                field(connector, "proxy_mode"),
                field(&s.metadata, "proxy_mode"),
            ))
            .to_lowercase()
                != "transparent"
        {
            return Ok(Value::Null);
        }
        let urls = or(field(connector, "upstream_urls"), &Value::Null);
        let urls = match urls {
            Value::Null => Vec::new(),
            Value::Array(v) => v.clone(),
            Value::String(v) => v.chars().map(|c| json!(c.to_string())).collect(),
            Value::Object(v) => v.keys().map(|k| json!(k)).collect(),
            _ => return Ok(Value::Null), // Python list(...) raises inside swallowed try.
        };
        let config = ServicesProxyConfig {
            service_id: s.service_id.clone(),
            service_name: s.name.clone(),
            upstream_url: string(or(
                field(connector, "upstream_url"),
                field(connector, "base_url"),
            )),
            upstream_urls: urls,
            assistant_id: field(connector, "assistant_id").clone(),
            graph_id: field(connector, "graph_id").clone(),
            metadata: s.metadata.clone(),
            enabled: s.status.to_lowercase() == "active",
        };
        match proxy.availability(&config).await {
            Err(ServicesError::Cancelled) => Err(ServicesError::Cancelled),
            Err(_) => Ok(Value::Null),
            // Truthy non-mapping availability fails on .get outside the try.
            Ok(v) if python_truthy(&v) && !v.is_object() => Err(ServicesError::Internal),
            Ok(v) => Ok(v),
        }
    }
}
// The shared converter checks ID before conversion; Python reaches its KeyError
// after enums and nested config. Only restore KeyError when those earlier reads
// are known valid. Compound-invalid inputs retain the shared converter's safe
// diagnostic boundary; they are not claimed as equivalent exception-text replay.
fn missing_service_id_prechecks_pass(definition: &Value) -> bool {
    fn valid_enum<T: serde::de::DeserializeOwned>(value: &Value) -> bool {
        value.is_null() || serde_json::from_value::<T>(value.clone()).is_ok()
    }
    fn valid_sequence<T: serde::de::DeserializeOwned>(value: &Value) -> bool {
        let values = match value {
            Value::Array(v) => v.clone(),
            Value::String(s) => s.chars().map(|c| json!(c.to_string())).collect(),
            Value::Object(v) => v.keys().map(|s| json!(s)).collect(),
            _ => return false,
        };
        values.iter().all(valid_enum::<T>)
    }
    if !valid_enum::<ServiceType>(field(definition, "service_type")) {
        return false;
    }
    if definition
        .get("supported_modes")
        .is_some_and(|v| !valid_sequence::<InvocationMode>(v))
    {
        return false;
    }
    if !valid_enum::<TransportType>(field(definition, "connector_type")) {
        return false;
    }
    for key in ["accepted_content_types", "output_content_types"] {
        if definition
            .get(key)
            .is_some_and(|v| !valid_sequence::<ContentType>(v))
        {
            return false;
        }
    }
    let config = field(definition, "service_config");
    if python_truthy(config) {
        if !config.is_object() {
            return false;
        }
        for key in ["rate_limit", "auth", "cache", "priority", "capacity"] {
            if config.get(key).is_some_and(|v| !v.is_object()) {
                return false;
            }
        }
    }
    true
}
fn validate_dictionary(body: &Value) -> Result<(), ServicesError> {
    if contract::is_dictionary(body) {
        return Ok(());
    }
    let missing = body.is_null();
    Err(ServicesError::validation(vec![
        json!({"type":if missing {"missing"} else {"dict_type"},
        "loc":["body"],"msg":if missing {"Field required"} else {"Input should be a valid dictionary"},"input":body}),
    ]))
}
#[derive(Clone)]
struct HttpState {
    api: Arc<ServicesApi>,
    cancellation: Arc<dyn ServicesCancellation>,
}
pub fn router(api: Arc<ServicesApi>, cancellation: Arc<dyn ServicesCancellation>) -> Router {
    let mut routes = vec!["/service-registrations"];
    let mut router = Router::new()
        .route(crate::native_contract::SERVICE_REGISTRATIONS, any(dispatch));
    for (_, route, _) in SERVICES_ROUTES {
        if !routes.contains(route) {
            routes.push(*route);
            router = router.route(&format!("/api/v1{route}"), any(dispatch));
        }
    }
    router
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState { api, cancellation })
}
async fn response(
    state: &HttpState,
    result: Result<Value, ServicesError>,
    context: &ConfigAuditRequestContext,
) -> Response {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(ServicesError::Cancelled) => state.cancellation.cancelled().await,
        Err(e) => {
            let (status, body) = e.response(context).expect("noncancelled error");
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response()
        }
    }
}
fn routed_operation(
    route: &str, method: &str, id: String, uri: &axum::http::Uri,
) -> Result<ServicesOperation, Response> {
    Ok(match (route, method) {
        (crate::native_contract::SERVICE_REGISTRATIONS, "POST") => ServicesOperation::CreateRegistration,
        ("/api/v1/services", "POST") => ServicesOperation::Register,
        ("/api/v1/services", "GET") => {
            let pairs = match Query::<Vec<(String, String)>>::try_from_uri(uri) {
                Ok(Query(v)) => v,
                Err(e) => return Err(e.into_response()),
            };
            ServicesOperation::List {
                service_type: pairs
                    .iter()
                    .rev()
                    .find(|(k, _)| k == "service_type")
                    .map(|(_, v)| v.clone()),
                tags: pairs
                    .into_iter()
                    .filter(|(k, _)| k == "tags")
                    .map(|(_, v)| v)
                    .collect(),
            }
        }
        (_, "PUT") => ServicesOperation::Update { service_id: id },
        (_, "DELETE") => ServicesOperation::Delete { service_id: id },
        ("/api/v1/services/{service_id}", _) => ServicesOperation::Get { service_id: id },
        _ => ServicesOperation::Schema { service_id: id },
    })
}

async fn dispatch(
    State(state): State<HttpState>,
    matched: MatchedPath,
    params: Result<Path<HashMap<String, String>>, axum::extract::rejection::PathRejection>,
    request: Request,
) -> Response {
    let params = match params {
        Ok(Path(v)) => v,
        Err(e) => return e.into_response(),
    };
    if params.values().any(|v| v.contains('/')) {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    }
    let allowed = allowed_methods(matched.as_str());
    let method = request.method().as_str();
    if !allowed.contains(&method) {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", allowed[0])],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response();
    }
    let id = params.get("service_id").cloned().unwrap_or_default();
    let operation = match routed_operation(matched.as_str(), method, id, request.uri()) {
        Ok(operation) => operation,
        Err(response) => return response,
    };
    let headers = request.headers().clone();
    let mut context = request
        .extensions()
        .get::<ConfigAuditRequestContext>()
        .cloned()
        .or_else(|| {
            request
                .extensions()
                .get::<Extension<ConfigAuditRequestContext>>()
                .map(|Extension(v)| v.clone())
        })
        .unwrap_or_else(|| state.api.dependencies.identity.context(&headers));
    context.headers = headers.clone();
    let body = if operation.body() {
        let bytes = match Bytes::from_request(request, &state).await {
            Ok(v) => v,
            Err(e) => return e.into_response(),
        };
        if bytes.is_empty() {
            Value::Null
        } else {
            let is_json = crate::http_content_type::is_json_content_type(&headers);
            if is_json {
                match serde_json::from_slice(&bytes) {
                    Ok(v) => v,
                    Err(_) => return response(
                        &state,
                        Err(ServicesError::validation(vec![
                            json!({"type":"json_invalid","loc":["body",0],"msg":"JSON decode error",
                    "input":{},"ctx":{"error":"Invalid JSON"}}),
                        ])),
                        &context,
                    )
                    .await,
                }
            } else {
                json!(String::from_utf8_lossy(&bytes))
            }
        }
    } else {
        Value::Null
    };
    let result = state.api.handle(operation, &headers, &body, &context).await;
    response(&state, result, &context).await
}

#[cfg(test)]
#[path = "atomic_registration_tests.rs"]
mod atomic_registration_tests;

#[cfg(test)]
#[path = "services_contract_tests.rs"]
mod contract_tests;
