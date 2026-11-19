//! Four unmounted service-configuration routes from `src/api/v1/config.py`.
//!
//! These routes require GatewayServiceConfigWrite, including explicit permissions;
//! they do not inherit services.py's additional platform-admin/model-override checks.
//! The registry, protocol dataclasses, capacity resolver/admission, and awaited audit
//! writer are the existing implementations. Construction is inert.
//!
//! Subsections replace with request defaults; null/omitted sections are unchanged.
//! PUT deliberately never calls register or evicts adapters. Python mutates its
//! captured object before awaiting save. Live handles retain memory/config aliases;
//! only successful save republishes that captured object. No operation mutex/CAS
//! suppresses source replacement/delete races. GET reads its captured config after
//! proxy lookup but captures capacity inputs before it. DELETE clears cache when
//! storage returns false; it retains the source adapter-cache quirk.
//!
//! Quick LangGraph catches ordinary exceptions and returns their source text as
//! HTTP 400. The mature registry/audit errors intentionally erase backend text;
//! an explicit trusted host renderer is required, rather than inventing Python messages.
//! Native adapters must retain redaction; never expose raw backend/credential text
//! solely for compatibility. Sanitized host output is an explicit native boundary.
//! Hosts merging custom-fallback factories must call reset_fallback() before merge.
//! Cancellation remains separate, including proxy-read cancellation (ordinary
//! proxy errors are swallowed). Dropping requests drops their awaited collaborator;
//! persistence already performed is not rolled back.
//!
//! HTTP JSON syntax parsing precedes identity; model validation follows identity
//! and precedes capability checks. Missing/empty Content-Type retains body bytes.
//! HEAD is rejected before collaborators. Invalid encodings, malformed JSON's exact
//! parser offsets/messages, transport limits, and trusted-origin slash redirects
//! remain host boundaries. JSON numbers fit i64 and are finite.
use crate::{
    capacity::{CapacityRequest, CapacityResolver},
    capacity_admission::CapacityAdmissionController,
    config_audit_writer::{
        ConfigAuditError, ConfigAuditIdentity, ConfigAuditRequestContext, ConfigAuditValue,
        ConfigAuditWriter, ConfigChange,
    },
    protocol::{ServiceConfig, ServiceDefinition},
    rate_config_api::{boolean, integer},
    service_ownership::{ConfigHandle, ServiceHandle},
    service_registry::{RegistryError, ServiceRegistry, python_truthy, service_from_value},
    usage_parser::{python_value_to_string, python_whitespace},
};
use ai_platform_auth::{
    admission::AdmissionService,
    rbac::{Capability, Rbac, check_capability, permission_denied_detail},
};
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{MatchedPath, Path, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, fmt, future::Future, pin::Pin, sync::Arc};

pub const SERVICE_CONFIG_ROUTES: &[(&str, &str, &str)] = &[
    (
        "POST",
        "/config/services/langgraph",
        "create_langgraph_service",
    ),
    (
        "GET",
        "/config/services/{service_id}/config",
        "get_service_config",
    ),
    (
        "PUT",
        "/config/services/{service_id}/config",
        "update_service_config",
    ),
    ("DELETE", "/config/services/{service_id}", "delete_service"),
];
pub type ServiceConfigFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ServiceConfigError>> + Send + 'a>>;
#[derive(Clone, PartialEq)]
pub enum ServiceConfigError {
    Http {
        status: u16,
        body: Value,
    },
    /// Exact ordinary Python exception text supplied by a trusted collaborator.
    Exception(String),
    Internal,
    Cancelled,
}
impl fmt::Debug for ServiceConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http { status, .. } => write!(f, "ServiceConfigError::Http({status})"),
            Self::Exception(_) => f.write_str("ServiceConfigError::Exception"),
            Self::Internal => f.write_str("ServiceConfigError::Internal"),
            Self::Cancelled => f.write_str("ServiceConfigError::Cancelled"),
        }
    }
}
impl ServiceConfigError {
    pub fn detail(status: u16, detail: impl Into<Value>) -> Self {
        Self::Http {
            status,
            body: json!({"detail":detail.into()}),
        }
    }
    pub fn validation(errors: Vec<Value>) -> Self {
        Self::detail(422, json!(errors))
    }
    pub fn response(&self, context: &ConfigAuditRequestContext) -> Option<(u16, Value)> {
        match self {
            Self::Cancelled => None,
            Self::Http { status, body } => Some((*status, body.clone())),
            Self::Exception(_) | Self::Internal => {
                let mut body = json!({"error":{"code":2000,"message":"Internal server error"}});
                let trace = context.trace_id.clone().unwrap_or_else(|| {
                    ["x-trace-id", "x-request-id"]
                        .iter()
                        .find_map(|name| {
                            context
                                .headers
                                .get(*name)
                                .filter(|v| !v.is_empty())
                                .map(|v| {
                                    v.as_bytes()
                                        .iter()
                                        .copied()
                                        .map(char::from)
                                        .collect::<String>()
                                })
                        })
                        .map_or(Value::Null, |s| json!(s))
                });
                if python_truthy(&trace) {
                    body["error"]["trace_id"] = trace;
                }
                Some((500, body))
            }
        }
    }
}
#[derive(Clone, Default)]
pub struct ServiceConfigIdentity {
    pub user_id: String,
    pub tenant_id: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
}
impl ConfigAuditIdentity for ServiceConfigIdentity {
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn tenant_id(&self) -> &str {
        &self.tenant_id
    }
}
pub trait ServiceConfigIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, ServiceConfigIdentity>;
    fn context(&self, headers: &HeaderMap) -> ConfigAuditRequestContext {
        ConfigAuditRequestContext {
            headers: headers.clone(),
            ..Default::default()
        }
    }
    /// Host owns canonical best-effort denial-event scheduling and lifetime.
    fn permission_denied(
        &self,
        _identity: &ServiceConfigIdentity,
        _context: &ConfigAuditRequestContext,
    ) {
    }
}
impl ServiceConfigIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, ServiceConfigIdentity> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let user = request
                .resolve()
                .await
                .map_err(|e| ServiceConfigError::Http {
                    status: e.status(),
                    body: json!({"error": e.to_string()}),
                })?;
            Ok(ServiceConfigIdentity {
                user_id: user.user_id().into(),
                tenant_id: user.tenant_id().into(),
                roles: user.roles().to_vec(),
                permissions: user.permissions().to_vec(),
            })
        })
    }
}
/// Legacy database hooks are separate from the registry's actual storage. A quick
/// registration can therefore persist twice, exactly as the source does.
pub trait ServiceConfigDatabase: Send + Sync {
    fn enabled(&self) -> bool;
    fn supports_update_config(&self) -> bool;
    fn save_service<'a>(&'a self, definition: &'a Value) -> ServiceConfigFuture<'a, ()>;
    fn update_service_config<'a>(
        &'a self,
        service_id: &'a str,
        config: ConfigHandle,
    ) -> ServiceConfigFuture<'a, ()>;
}
#[derive(Clone, Default)]
pub struct ServiceConfigProxyConfig {
    pub capacity_config: Value,
    pub model_override: Value,
    pub default_provider: Value,
}
pub trait ServiceConfigProxyLoader: Send + Sync {
    fn get_config<'a>(
        &'a self,
        service_id: &'a str,
    ) -> ServiceConfigFuture<'a, Option<ServiceConfigProxyConfig>>;
    fn invalidate(&self, service_id: &str) -> Result<(), ServiceConfigError>;
}
/// Required only because Python quick-registration exposes str(e), whereas the
/// mature collaborators erase backend text. Hosts must not silently claim exact
/// error-text parity if their safe boundary has erased it. Never recover raw
/// database/credential text merely for compatibility. No default formatter is used.
pub trait ServiceConfigExceptionText: Send + Sync {
    fn registry(&self, error: &RegistryError) -> String;
    fn audit(&self, error: &ConfigAuditError) -> String;
    fn internal(&self) -> String;
}
pub struct ServiceConfigDependencies {
    pub identity: Arc<dyn ServiceConfigIdentityResolver>,
    pub rbac: Rbac,
    pub registry: Arc<ServiceRegistry>,
    pub database: Option<Arc<dyn ServiceConfigDatabase>>,
    pub proxy_loader: Option<Arc<dyn ServiceConfigProxyLoader>>,
    pub capacity_resolver: Arc<CapacityResolver>,
    pub admission: Option<Arc<CapacityAdmissionController>>,
    pub audit: ConfigAuditWriter,
    pub exception_text: Arc<dyn ServiceConfigExceptionText>,
}
pub struct ServiceConfigApi {
    dependencies: ServiceConfigDependencies,
}
#[derive(Clone, Debug)]
pub enum ServiceConfigOperation {
    CreateLangGraph,
    Get { service_id: String },
    Update { service_id: String },
    Delete { service_id: String },
}
impl ServiceConfigOperation {
    fn path(&self) -> String {
        match self {
            Self::CreateLangGraph => "/api/v1/config/services/langgraph".into(),
            Self::Get { service_id } | Self::Update { service_id } => {
                format!("/api/v1/config/services/{service_id}/config")
            }
            Self::Delete { service_id } => format!("/api/v1/config/services/{service_id}"),
        }
    }
    fn model(&self) -> Option<&'static str> {
        match self {
            Self::CreateLangGraph => Some("LangGraphServiceCreate"),
            Self::Update { .. } => Some("ServiceConfigUpdate"),
            _ => None,
        }
    }
}
impl ServiceConfigApi {
    pub fn new(dependencies: ServiceConfigDependencies) -> Self {
        Self { dependencies }
    }
    pub async fn handle(
        &self,
        operation: ServiceConfigOperation,
        headers: &HeaderMap,
        body: &Value,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, ServiceConfigError> {
        let identity = self
            .dependencies
            .identity
            .resolve(headers, &operation.path())
            .await?;
        self.execute(operation, &identity, body, context).await
    }
    pub async fn execute(
        &self,
        operation: ServiceConfigOperation,
        identity: &ServiceConfigIdentity,
        body: &Value,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, ServiceConfigError> {
        let body = match operation.model() {
            Some(model) => validate_model(model, body)?,
            None => Value::Null,
        };
        let d = &self.dependencies;
        let roles: Vec<_> = identity
            .roles
            .iter()
            .map(|s| s.trim_matches(python_whitespace).to_owned())
            .collect();
        let permissions: Vec<_> = identity
            .permissions
            .iter()
            .map(|s| s.trim_matches(python_whitespace).to_owned())
            .collect();
        let capability = Capability::GatewayServiceConfigWrite;
        if !check_capability(&d.rbac, &roles, &permissions, capability).allowed() {
            d.identity.permission_denied(identity, context);
            let trace = context
                .request_id
                .as_ref()
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    context
                        .trace_id
                        .as_ref()
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or("");
            return Err(ServiceConfigError::detail(
                403,
                permission_denied_detail(capability, trace, None),
            ));
        }
        match operation {
            ServiceConfigOperation::CreateLangGraph => self.create(&body, identity, context).await,
            ServiceConfigOperation::Get { service_id } => self.get(&service_id).await,
            ServiceConfigOperation::Update { service_id } => {
                self.update(&service_id, &body, identity, context).await
            }
            ServiceConfigOperation::Delete { service_id } => {
                self.delete(&service_id, identity, context).await
            }
        }
    }
    fn caught(&self, error: ServiceConfigError) -> ServiceConfigError {
        let text = match error {
            ServiceConfigError::Cancelled => return ServiceConfigError::Cancelled,
            ServiceConfigError::Exception(s) => s,
            ServiceConfigError::Http { status, body } => {
                format!("{status}: {}", python_value_to_string(&body["detail"]))
            }
            ServiceConfigError::Internal => self.dependencies.exception_text.internal(),
        };
        ServiceConfigError::detail(400, text)
    }
    async fn audit(
        &self,
        identity: &ServiceConfigIdentity,
        context: &ConfigAuditRequestContext,
        resource: (&str, &str),
        payload: (Option<Value>, Option<Value>),
        caught: bool,
    ) -> Result<(), ServiceConfigError> {
        let (service_id, action) = resource;
        let before = payload.0.map(ConfigAuditValue::from);
        let after = payload.1.map(ConfigAuditValue::from);
        self.dependencies
            .audit
            .record_config_change(
                context,
                identity,
                ConfigChange {
                    resource_type: "service",
                    resource_id: service_id,
                    action,
                    before: before.as_ref(),
                    after: after.as_ref(),
                },
            )
            .await
            .map_err(|e| {
                if e == ConfigAuditError::Cancelled {
                    ServiceConfigError::Cancelled
                } else if caught {
                    ServiceConfigError::Exception(self.dependencies.exception_text.audit(&e))
                } else {
                    ServiceConfigError::Internal
                }
            })
    }
    async fn create(
        &self,
        body: &Value,
        identity: &ServiceConfigIdentity,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, ServiceConfigError> {
        let service_id = body["service_id"].as_str().expect("validated service id");
        let proxy_mode = body["proxy_mode"].as_str().expect("validated proxy mode");
        let url = body["deployment_url"]
            .as_str()
            .expect("validated URL")
            .trim_end_matches('/');
        let mut connector = json!({"base_url":url,"graph_id":body["graph_id"],"assistant_id":body["graph_id"],
            "proxy_mode":proxy_mode,"upstream_url":url,"forward_auth":true,"timeout_connect":5,
            "timeout_read":300,"timeout_write":60,"load_balance_strategy":"round_robin"});
        if python_truthy(&body["langsmith_api_key"]) {
            connector["auth_token"] = body["langsmith_api_key"].clone();
            connector["headers"] = json!({"X-Api-Key":body["langsmith_api_key"]});
        }
        let description = if python_truthy(&body["description"]) {
            body["description"].clone()
        } else {
            json!(format!(
                "LangGraph service: {}",
                body["name"].as_str().unwrap()
            ))
        };
        let definition = json!({"service_id":service_id,"name":body["name"],"description":description,
            "service_type":"langgraph","connector_type":"http","connector_config":connector,
            "supported_modes":["sync","stream"],"accepted_content_types":["text"],"output_content_types":["text"],
            "session_enabled":body["session_enabled"],"status":"active","metadata":{"adapter_type":"langgraph","proxy_mode":proxy_mode}});
        let result: Result<(), ServiceConfigError> = async {
            let service = service_from_value(&definition).map_err(|e| {
                ServiceConfigError::Exception(self.dependencies.exception_text.registry(&e))
            })?;
            self.dependencies
                .registry
                .register(service)
                .await
                .map_err(|e| {
                    ServiceConfigError::Exception(self.dependencies.exception_text.registry(&e))
                })?;
            if let Some(db) = self
                .dependencies
                .database
                .as_ref()
                .filter(|db| db.enabled())
            {
                db.save_service(&definition).await?;
            }
            self.audit(
                identity,
                context,
                (service_id, "create_langgraph"),
                (None, Some(definition)),
                true,
            )
            .await
        }
        .await;
        if let Err(e) = result {
            return Err(self.caught(e));
        }
        let route = (proxy_mode == "transparent").then(|| format!("/api/v1/proxy/{service_id}"));
        let endpoints = route.as_ref().map(|route| json!({"list_assistants":format!("{route}/assistants"),
            "create_thread":format!("{route}/threads"),"stream_run":format!("{route}/threads/{{thread_id}}/runs/stream")}));
        Ok(
            json!({"status":"success","service_id":service_id,"message":"服务注册成功","proxy_mode":proxy_mode,
            "proxy_route":route,"example_endpoints":endpoints}),
        )
    }
    async fn service(&self, service_id: &str) -> Result<ServiceHandle, ServiceConfigError> {
        self.dependencies
            .registry
            .get(service_id)
            .await
            .map_err(|_| ServiceConfigError::Internal)?
            .ok_or_else(|| ServiceConfigError::detail(404, format!("服务 {service_id} 不存在")))
    }
    async fn get(&self, service_id: &str) -> Result<Value, ServiceConfigError> {
        let live = self.service(service_id).await?;
        let captured_config = live.materialize_config();
        let initial = live.snapshot();
        let initial_config = captured_config.snapshot();
        let status = self
            .capacity_status(service_id, &initial, &initial_config)
            .await?;
        let service = live.snapshot();
        let config = captured_config.snapshot();
        let mut projection =
            serde_json::to_value(&config).map_err(|_| ServiceConfigError::Internal)?;
        projection["priority"]["enforced"] = json!(false);
        projection["priority"]["scheduler"] = json!("not_configured");
        projection["capacity"]["source_status"] = json!(if config.capacity.enforced {
            "real"
        } else {
            "disabled"
        });
        Ok(
            json!({"service_id":service_id,"name":service.name,"config":projection,
            "legacy":{"rate_limit":service.rate_limit,"concurrency_limit":service.concurrency_limit,
                "timeout":service.timeout,"max_retries":service.max_retries,"circuit_breaker_enabled":service.circuit_breaker_enabled,
                "failure_threshold":service.failure_threshold,"recovery_timeout":service.recovery_timeout},"capacity_status":status}),
        )
    }
    async fn capacity_status(
        &self,
        service_id: &str,
        service: &ServiceDefinition,
        config: &ServiceConfig,
    ) -> Result<Value, ServiceConfigError> {
        let mut configured = json!({"upstream_group":config.capacity.upstream_group,"concurrency_limit":config.capacity.concurrency_limit,
            "queue_max":config.capacity.queue_max,"queue_timeout_ms":config.capacity.queue_timeout_ms});
        let mut provider = find_service_provider_id(service);
        if let Some(loader) = &self.dependencies.proxy_loader {
            match loader.get_config(service_id).await {
                Ok(Some(proxy)) => {
                    if python_truthy(&proxy.capacity_config) {
                        configured = proxy.capacity_config;
                    }
                    if let Some(value) = proxy
                        .model_override
                        .as_object()
                        .and_then(|v| v.get("provider_id"))
                        .filter(|v| python_truthy(v))
                    {
                        provider = value.clone();
                    } else if python_truthy(&proxy.default_provider) {
                        provider = proxy.default_provider;
                    }
                }
                Err(ServiceConfigError::Cancelled) => return Err(ServiceConfigError::Cancelled),
                Ok(None) | Err(_) => {}
            }
        }
        if !configured.is_object() {
            return Err(ServiceConfigError::Internal);
        }
        let resolver = &self.dependencies.capacity_resolver;
        let budgets = resolver
            .resolve(&CapacityRequest {
                tenant_id: "default".into(),
                service_id: service_id.into(),
                request_class: "stream".into(),
                upstream_group: configured["upstream_group"].clone(),
                provider_id: provider.clone(),
                service_config: json!({"capacity":configured}),
                resource_kind: "request".into(),
                is_admin_read: false,
            })
            .map_err(|_| ServiceConfigError::Internal)?;
        let snapshot = self
            .dependencies
            .admission
            .as_ref()
            .map(|a| a.snapshot())
            .unwrap_or_default();
        let rows: Vec<_> = budgets
            .iter()
            .map(|b| {
                let usage = snapshot.get(&b.key);
                b.to_status(
                    usage.map_or(0, |v| v.inflight),
                    usage.map_or(0, |v| v.queue_depth),
                )
            })
            .collect();
        Ok(json!({"request_class":"stream","provider_id":provider,
            "gateway_instance_id":self.dependencies.admission.as_ref().map_or("gateway-1", |a| a.gateway_instance_id()),
            "cluster_epoch":resolver.cluster_epoch,"mode":resolver.mode,"budgets":rows}))
    }
    async fn update(
        &self,
        service_id: &str,
        body: &Value,
        identity: &ServiceConfigIdentity,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, ServiceConfigError> {
        let service = self.service(service_id).await?;
        let config = service.materialize_config();
        let (before, after) = service.edit_config(|config, legacy_rate_limit| -> Result<_, ServiceConfigError> {
        let before = serde_json::to_value(&*config).map_err(|_| ServiceConfigError::Internal)?;
        let mut after = Map::new();
        for (section, value) in body.as_object().expect("validated update") {
            if value.is_null() {
                continue;
            }
            let mut audit_section = value.clone();
            if let Some(fields) = audit_section.as_object_mut() {
                fields.retain(|_, value| !value.is_null());
            }
            after.insert(section.clone(), audit_section);
            match section.as_str() {
                "rate_limit" => {
                    config.rate_limit = serde_json::from_value(value.clone())
                        .map_err(|_| ServiceConfigError::Internal)?;
                    *legacy_rate_limit = config.rate_limit.enabled.then(|| json!({"requests":config.rate_limit.requests,
                        "window":config.rate_limit.window,"burst":config.rate_limit.burst,"strategy":config.rate_limit.strategy}));
                }
                "auth" => {
                    config.auth = serde_json::from_value(value.clone())
                        .map_err(|_| ServiceConfigError::Internal)?
                }
                "cache" => {
                    config.cache = serde_json::from_value(value.clone())
                        .map_err(|_| ServiceConfigError::Internal)?
                }
                "priority" => {
                    config.priority = serde_json::from_value(value.clone())
                        .map_err(|_| ServiceConfigError::Internal)?
                }
                "capacity" => {
                    config.capacity = serde_json::from_value(value.clone())
                        .map_err(|_| ServiceConfigError::Internal)?;
                    config.capacity.enforced = true;
                }
                _ => unreachable!("validated subsection"),
            }
        }
        Ok((before, after))
        })?;
        self.dependencies
            .registry
            .storage
            .save(service.clone())
            .await
            .map_err(|_| ServiceConfigError::Internal)?;
        self.dependencies.registry.replace_service_cache(service);
        if let Some(db) = self
            .dependencies
            .database
            .as_ref()
            .filter(|db| db.enabled() && db.supports_update_config())
        {
            db.update_service_config(service_id, config).await?;
        }
        if let Some(loader) = &self.dependencies.proxy_loader {
            loader.invalidate(service_id)?;
        }
        self.audit(
            identity,
            context,
            (service_id, "update_config"),
            (Some(before), Some(json!(after))),
            false,
        )
        .await?;
        Ok(
            json!({"status":"success","message":format!("服务 {service_id} 配置已更新"),"service_id":service_id}),
        )
    }
    async fn delete(
        &self,
        service_id: &str,
        identity: &ServiceConfigIdentity,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, ServiceConfigError> {
        let service = self.service(service_id).await?;
        // RegistryStorage is the mature delete boundary. Legacy _services-pop
        // implementations adapt there; no alternate storage is created here.
        let deleted = self
            .dependencies
            .registry
            .storage
            .delete(service_id)
            .await
            .map_err(|_| ServiceConfigError::Internal)?;
        self.dependencies
            .registry
            .invalidate_service_cache(service_id);
        if !deleted {
            return Err(ServiceConfigError::detail(
                404,
                format!("服务 {service_id} 不存在"),
            ));
        }
        self.audit(
            identity,
            context,
            (service_id, "delete"),
            (
                Some(json!({"service_id":service_id,"name":service.snapshot().name})),
                None,
            ),
            false,
        )
        .await?;
        Ok(json!({"status":"success","message":format!("服务 {service_id} 已删除")}))
    }
}
pub fn find_service_provider_id(service: &ServiceDefinition) -> Value {
    for source in [&service.connector_config, &service.metadata] {
        let Some(source) = source.as_object() else {
            continue;
        };
        if let Some(value) = source
            .get("model_override")
            .and_then(Value::as_object)
            .and_then(|v| v.get("provider_id"))
            .filter(|v| python_truthy(v))
        {
            return json!(python_value_to_string(value));
        }
        for key in [
            "default_provider",
            "provider",
            "provider_id",
            "llm_provider",
            "vendor",
        ] {
            if let Some(value) = source.get(key).filter(|v| python_truthy(v)) {
                return json!(python_value_to_string(value));
            }
        }
    }
    Value::Null
}

fn validation(kind: &str, loc: Vec<Value>, input: &Value, model: &str) -> Value {
    let message = match kind {
        "missing" => "Field required".into(),
        "model_type" => format!("Input should be a valid dictionary or instance of {model}"),
        "model_attributes_type" => {
            "Input should be a valid dictionary or object to extract fields from".into()
        }
        "string_type" => "Input should be a valid string".into(),
        "list_type" => "Input should be a valid list".into(),
        "int_type" => "Input should be a valid integer".into(),
        "int_parsing" => {
            "Input should be a valid integer, unable to parse string as an integer".into()
        }
        "int_from_float" => {
            "Input should be a valid integer, got a number with a fractional part".into()
        }
        "int_parsing_size" => {
            "Unable to parse input string as an integer, exceeded maximum size".into()
        }
        "bool_type" => "Input should be a valid boolean".into(),
        "bool_parsing" => "Input should be a valid boolean, unable to interpret input".into(),
        _ => "Invalid value".into(),
    };
    let mut error = json!({"type":kind,"loc":loc,"msg":message,"input":input});
    if kind == "model_type" {
        error["ctx"] = json!({"class_name":model});
    }
    error
}
fn defaults(model: &str) -> Value {
    match model {
        "LangGraphServiceCreate" => {
            json!({"langsmith_api_key":null,"session_enabled":true,"description":null,"proxy_mode":"transparent"})
        }
        "ServiceConfigUpdate" => {
            json!({"rate_limit":null,"auth":null,"cache":null,"priority":null,"capacity":null})
        }
        "ServiceRateLimitConfigUpdate" => {
            json!({"enabled":false,"requests":100,"window":60,"burst":0,"strategy":"sliding_window"})
        }
        "ServiceAuthConfigUpdate" => {
            json!({"enabled":false,"require_auth":true,"allowed_roles":[],"allowed_api_keys":[],"public":false})
        }
        "ServiceCacheConfigUpdate" => json!({"enabled":false,"ttl":300,"semantic_cache":false}),
        "ServicePriorityConfigUpdate" => json!({"priority":5,"weight":1,"max_queue_size":100}),
        "ServiceCapacityConfigUpdate" => {
            json!({"upstream_group":null,"concurrency_limit":null,"queue_max":16,"queue_timeout_ms":3000})
        }
        _ => unreachable!("known schema"),
    }
}
fn fields(model: &str) -> &'static [&'static str] {
    match model {
        "LangGraphServiceCreate" => &[
            "service_id",
            "name",
            "deployment_url",
            "graph_id",
            "langsmith_api_key",
            "session_enabled",
            "description",
            "proxy_mode",
        ],
        "ServiceConfigUpdate" => &["rate_limit", "auth", "cache", "priority", "capacity"],
        "ServiceRateLimitConfigUpdate" => &["enabled", "requests", "window", "burst", "strategy"],
        "ServiceAuthConfigUpdate" => &[
            "enabled",
            "require_auth",
            "allowed_roles",
            "allowed_api_keys",
            "public",
        ],
        "ServiceCacheConfigUpdate" => &["enabled", "ttl", "semantic_cache"],
        "ServicePriorityConfigUpdate" => &["priority", "weight", "max_queue_size"],
        "ServiceCapacityConfigUpdate" => &[
            "upstream_group",
            "concurrency_limit",
            "queue_max",
            "queue_timeout_ms",
        ],
        _ => unreachable!("known schema"),
    }
}
fn parse_model(model: &str, body: &Value, loc: &[Value], errors: &mut Vec<Value>) -> Value {
    let Some(object) = body.as_object() else {
        errors.push(validation("model_type", loc.to_vec(), body, model));
        return Value::Null;
    };
    let mut out = defaults(model);
    for field in fields(model) {
        let mut at = loc.to_vec();
        at.push(json!(field));
        let Some(value) = object.get(*field) else {
            if out.get(*field).is_none() {
                errors.push(validation("missing", at, body, model));
            }
            continue;
        };
        if model == "ServiceConfigUpdate" {
            let child = match *field {
                "rate_limit" => "ServiceRateLimitConfigUpdate",
                "auth" => "ServiceAuthConfigUpdate",
                "cache" => "ServiceCacheConfigUpdate",
                "priority" => "ServicePriorityConfigUpdate",
                "capacity" => "ServiceCapacityConfigUpdate",
                _ => unreachable!(),
            };
            out[*field] = if value.is_null() {
                Value::Null
            } else {
                parse_model(child, value, &at, errors)
            };
            continue;
        }
        let parsed = match *field {
            "langsmith_api_key" | "description" | "upstream_group" | "concurrency_limit"
                if value.is_null() =>
            {
                Ok(Value::Null)
            }
            "service_id" | "name" | "deployment_url" | "graph_id" | "langsmith_api_key"
            | "description" | "proxy_mode" | "upstream_group" | "strategy" => {
                value.as_str().map(|v| json!(v)).ok_or("string_type")
            }
            "session_enabled" | "enabled" | "require_auth" | "public" | "semantic_cache" => {
                boolean(value).map(|v| json!(v))
            }
            "allowed_roles" | "allowed_api_keys" => {
                if let Some(array) = value.as_array() {
                    for (index, item) in array.iter().enumerate() {
                        if !item.is_string() {
                            let mut item_loc = at.clone();
                            item_loc.push(json!(index));
                            errors.push(validation("string_type", item_loc, item, model));
                        }
                    }
                    Ok(value.clone())
                } else {
                    Err("list_type")
                }
            }
            _ => integer(value).map(|v| json!(v)),
        };
        match parsed {
            Ok(value) => out[*field] = value,
            Err(kind) => errors.push(validation(kind, at, value, model)),
        }
    }
    out
}
pub fn validate_model(model: &str, body: &Value) -> Result<Value, ServiceConfigError> {
    let mut errors = Vec::new();
    let value = parse_model(model, body, &[json!("body")], &mut errors);
    if errors.is_empty() {
        Ok(value)
    } else {
        Err(ServiceConfigError::validation(errors))
    }
}

pub type ServiceConfigCancellationFuture<'a> = Pin<Box<dyn Future<Output = Response> + Send + 'a>>;
pub trait ServiceConfigCancellation: Send + Sync {
    fn cancelled(&self) -> ServiceConfigCancellationFuture<'_>;
}
#[derive(Clone)]
struct HttpState {
    api: Arc<ServiceConfigApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
}
pub fn router(
    api: Arc<ServiceConfigApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
) -> Router {
    Router::new()
        .route(
            "/api/v1/config/services/langgraph",
            any(method_fallback)
                .post(http_create)
                .delete(http_delete_langgraph),
        )
        .route(
            "/api/v1/config/services/{service_id}/config",
            any(method_fallback).get(http_get).put(http_update),
        )
        .route(
            "/api/v1/config/services/{service_id}",
            any(method_fallback).delete(http_delete),
        )
        .route_layer(middleware::from_fn(route_guard))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState { api, cancellation })
}
async fn method_fallback() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}
async fn route_guard(
    matched: MatchedPath,
    params: Result<Path<HashMap<String, String>>, axum::extract::rejection::PathRejection>,
    request: Request,
    next: Next,
) -> Response {
    let params = match params {
        Ok(Path(v)) => v,
        Err(e) => return e.into_response(),
    };
    let invalid = params.values().any(|v| v.contains('/'));
    let allowed: &[&str] = match matched.as_str() {
        "/api/v1/config/services/langgraph" => &["POST", "DELETE"],
        "/api/v1/config/services/{service_id}/config" => &["GET", "PUT"],
        _ => &["DELETE"],
    };
    if !invalid && allowed.contains(&request.method().as_str()) {
        return next.run(request).await;
    }
    // Axum computes the original JSON Content-Length before suppressing HEAD.
    if invalid {
        (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response()
    } else {
        (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", allowed[0])],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response()
    }
}
async fn result(
    state: &HttpState,
    value: Result<Value, ServiceConfigError>,
    context: &ConfigAuditRequestContext,
) -> Response {
    match value {
        Ok(v) => Json(v).into_response(),
        Err(ServiceConfigError::Cancelled) => state.cancellation.cancelled().await,
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
fn http_context(
    state: &HttpState,
    headers: &HeaderMap,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
) -> ConfigAuditRequestContext {
    let mut context = trusted
        .map(|Extension(v)| v)
        .unwrap_or_else(|| state.api.dependencies.identity.context(headers));
    context.headers = headers.clone();
    context
}
async fn read(
    state: HttpState,
    operation: ServiceConfigOperation,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
) -> Response {
    let context = http_context(&state, &headers, trusted);
    result(
        &state,
        state
            .api
            .handle(operation, &headers, &Value::Null, &context)
            .await,
        &context,
    )
    .await
}
async fn http_get(
    State(state): State<HttpState>,
    Path(service_id): Path<String>,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
) -> Response {
    read(
        state,
        ServiceConfigOperation::Get { service_id },
        trusted,
        headers,
    )
    .await
}
async fn http_delete(
    State(state): State<HttpState>,
    Path(service_id): Path<String>,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
) -> Response {
    read(
        state,
        ServiceConfigOperation::Delete { service_id },
        trusted,
        headers,
    )
    .await
}
async fn http_delete_langgraph(
    State(state): State<HttpState>,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
) -> Response {
    read(
        state,
        ServiceConfigOperation::Delete {
            service_id: "langgraph".into(),
        },
        trusted,
        headers,
    )
    .await
}
async fn write(
    state: HttpState,
    operation: ServiceConfigOperation,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let context = http_context(&state, &headers, trusted);
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        let is_json = crate::http_content_type::is_json_content_type(&headers);
        if is_json {
            match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => return result(&state,Err(ServiceConfigError::validation(vec![json!({"type":"json_invalid","loc":["body",0],"msg":"JSON decode error","input":{},"ctx":{"error":"Invalid JSON"}})])),&context).await,
        }
        } else {
            json!(String::from_utf8_lossy(&bytes))
        }
    };
    let identity = match state
        .api
        .dependencies
        .identity
        .resolve(&headers, &operation.path())
        .await
    {
        Ok(v) => v,
        Err(e) => return result(&state, Err(e), &context).await,
    };
    if !body.is_object() {
        let kind = if body.is_null() {
            "missing"
        } else {
            "model_attributes_type"
        };
        return result(
            &state,
            Err(ServiceConfigError::validation(vec![validation(
                kind,
                vec![json!("body")],
                &body,
                operation.model().unwrap(),
            )])),
            &context,
        )
        .await;
    }
    // FastAPI's from_attributes applies to nested BaseModel fields as well.
    let value = state.api.execute(operation,&identity,&body,&context).await.map_err(|error| {
        if let ServiceConfigError::Http { status:422, mut body } = error {
            if let Some(errors) = body["detail"].as_array_mut() { for error in errors {
                if error["type"] == "model_type" { error["type"] = json!("model_attributes_type");
                    error["msg"] = json!("Input should be a valid dictionary or object to extract fields from");
                    error.as_object_mut().unwrap().remove("ctx"); }
            }}
            ServiceConfigError::Http { status:422, body }
        } else { error }
    });
    result(&state, value, &context).await
}
async fn http_create(
    State(state): State<HttpState>,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    write(
        state,
        ServiceConfigOperation::CreateLangGraph,
        trusted,
        headers,
        bytes,
    )
    .await
}
async fn http_update(
    State(state): State<HttpState>,
    Path(service_id): Path<String>,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    write(
        state,
        ServiceConfigOperation::Update { service_id },
        trusted,
        headers,
        bytes,
    )
    .await
}
