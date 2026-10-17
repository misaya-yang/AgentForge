//! Concrete Foundation API collaborators. Constructors do no I/O. Request
//! effects own their scheduled writes; the host must drain them before pools.
use super::{
    BootstrapError, DatabasePools,
    foundation::{FoundationBackends, FoundationRequestEffects, FoundationShared},
};
use crate::{
    billing, config_audit_writer, connector_admin_api,
    connector_oauth_http::{
        ConnectorDnsResolver, NativeConnectorDnsResolver, oauth_form_safe_address,
    },
    langgraph_service_validation::LangGraphValidationError,
    metrics_admin_api,
    migration_agent::{Scope, control::RuntimeControlPlane},
    migration_compat::transparent_proxy::{ProxyServiceConfig, TransparentProxy},
    migration_host::middleware::RequestContext,
    quota_admin_api,
    quota_store::PgQuotaStore,
    rate_config_api,
    rate_policy_store::RuntimeRateRules,
    registry_storage::PgRegistryStorage,
    service_access_policy::ServiceAccessLookupFailure,
    service_access_resolver, service_config_api,
    service_ownership::{ConfigHandle, ServiceHandle},
    service_registry::{RegistryError, RegistryStorage, python_truthy, service_from_value},
    services_api, sessions_api, usage_admin_api,
};
use ai_platform_auth::rbac::Capability;
use axum::{
    extract::{ConnectInfo, Request},
    http::HeaderMap,
    middleware::Next,
    response::Response,
};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::task::JoinSet;

/// These are genuine unresolved policies, never replaced by an always-success
/// implementation. Native auth/catalog currently read fresh authority rows and
/// expose no cache for a stand-alone invalidator to clear.
pub struct FoundationHostPolicies {
    pub role_cache: Option<Arc<dyn crate::roles_store::RolesPermissionCache>>,
    pub tool_invalidation: Option<Arc<dyn crate::tenant_policies_api::TenantPolicyInvalidation>>,
    pub mcp_invalidation: Option<Arc<dyn crate::tenant_policies_api::TenantPolicyInvalidation>>,
    pub confluence_registrar:
        Arc<dyn crate::connector_user_activation::ConnectorConfluenceRegistrar>,
    pub mcp_secrets: Arc<dyn crate::mcp_gateway_broker::McpSecretResolver>,
    pub model_publisher: Option<Arc<dyn crate::model_admin_api::ModelConfigPublisher>>,
    pub provider_publisher: Option<Arc<dyn crate::provider_admin_api::ProviderAdminPublisher>>,
    pub health_dependencies: Arc<dyn crate::health_api::HealthDependencyState>,
    pub health_cancellation: Arc<dyn crate::health_api::HealthHttpCancellation>,
    pub architecture_runtime: Arc<dyn crate::architecture_status_api::ArchitectureRuntime>,
    pub local_channel: Option<Arc<dyn crate::local_nodes_api::LocalNodesChannelVerifier>>,
}
pub struct NativeFoundationConfiguration {
    pub local_offset: FixedOffset,
    pub proxy_cache_ttl: Duration,
    pub response_cache_enabled: bool,
}
#[derive(Clone)]
pub struct NativeFoundationServices {
    pub database: Arc<PgFoundationServiceDatabase>,
    pub proxy_loader: Arc<PgProxyConfigLoader>,
    pub proxy: Arc<NativeServicesProxy>,
    pub runtime: Arc<NativeServicesRuntime>,
    pub rate_publication: Arc<NativeRatePublication>,
    pub security_events: Arc<PgFoundationSecurityEvents>,
    pub quota: Arc<NativeQuotaChecker>,
    pub request_effects: Arc<NativeRequestEffects>,
    pub warnings: Arc<NativeAccessWarnings>,
    pub usage_clock: Arc<NativeUsageClock>,
}
impl NativeFoundationServices {
    pub fn new(
        pools: &DatabasePools,
        shared: &FoundationShared,
        core: &super::core::CoreServices,
        runtime: Arc<RuntimeControlPlane>,
        observability: &crate::migration_host::observability::ObservabilityServices,
        image_worker_running: Arc<AtomicBool>,
        quota: Arc<NativeQuotaChecker>,
        proxy_loader: Arc<PgProxyConfigLoader>,
        config: NativeFoundationConfiguration,
    ) -> Result<Self, BootstrapError> {
        let mut proxy = TransparentProxy::default();
        proxy.capacity = Some(core.admission.clone());
        proxy.usage = Some(observability.usage.clone());
        proxy.pricing = Some(core.pricing.clone());
        proxy.realtime = Some(shared.realtime.clone());
        proxy.cache = Some(Arc::new(
            crate::migration_compat::response_cache::ResponseCache {
                pool: pools.data.clone(),
                enabled: config.response_cache_enabled,
            },
        ));
        let security_events = quota.security.clone();
        Ok(Self {
            database: Arc::new(PgFoundationServiceDatabase {
                storage: PgRegistryStorage::new(pools.data.clone()),
            }),
            proxy_loader,
            proxy: Arc::new(NativeServicesProxy {
                proxy: Arc::new(proxy),
            }),
            runtime: Arc::new(NativeServicesRuntime {
                sdk_session_pool: pools.data.clone(),
                sdk_sessions: core.sessions.clone(),
                control: runtime,
                image_worker_running,
            }),
            rate_publication: Arc::new(NativeRatePublication {
                active: Mutex::new(shared.runtime_rates.clone()),
            }),
            request_effects: Arc::new(NativeRequestEffects {
                security: security_events.clone(),
            }),
            warnings: Arc::new(NativeAccessWarnings::default()),
            usage_clock: Arc::new(NativeUsageClock {
                local_offset: config.local_offset,
            }),
            security_events,
            quota,
        })
    }
    /// Keep a clone of this service bundle for orderly shutdown and to share its
    /// proxy loader/quota checker with invocation, not a second empty instance.
    pub fn into_backends(self, policies: FoundationHostPolicies) -> FoundationBackends {
        FoundationBackends {
            role_cache: policies.role_cache,
            rate_publication: self.rate_publication,
            service_database: self.database,
            proxy_loader: Some(self.proxy_loader.clone()),
            service_errors: Arc::new(SafeFoundationErrors),
            services_errors: Arc::new(SafeFoundationErrors),
            services_runtime: self.runtime.clone(),
            proxy_invalidation: Some(self.proxy_loader),
            transparent_proxy: Some(self.proxy),
            access_warnings: self.warnings,
            request_effects: self.request_effects,
            connector_destination: Arc::new(NativeConnectorDestination),
            confluence_registrar: policies.confluence_registrar,
            mcp_secrets: policies.mcp_secrets,
            tool_invalidation: policies.tool_invalidation,
            mcp_invalidation: policies.mcp_invalidation,
            model_publisher: policies.model_publisher,
            provider_publisher: policies.provider_publisher,
            usage_clock: self.usage_clock,
            quota_checker: self.quota,
            quota_denied: self.security_events.clone(),
            metrics_denied: self.security_events,
            health_dependencies: policies.health_dependencies,
            health_cancellation: policies.health_cancellation,
            architecture_runtime: policies.architecture_runtime,
            local_channel: policies.local_channel,
            sessions_cleanup: self.runtime,
        }
    }
    pub async fn drain(&self) -> Result<(), BootstrapError> {
        self.security_events.drain().await;
        self.proxy.proxy.close().await;
        self.quota
            .store
            .drain_cancelled()
            .await
            .map_err(|_| BootstrapError::Construction("foundation_quota_drain"))
    }
}

pub struct PgFoundationServiceDatabase {
    storage: PgRegistryStorage,
}
impl service_config_api::ServiceConfigDatabase for PgFoundationServiceDatabase {
    fn enabled(&self) -> bool {
        true
    }
    // Canonical DatabaseStorage has save_service, but no update_service_config.
    // config.py takes its registry-storage fallback when this hook is absent.
    fn supports_update_config(&self) -> bool {
        false
    }
    fn save_service<'a>(
        &'a self,
        definition: &'a Value,
    ) -> service_config_api::ServiceConfigFuture<'a, ()> {
        Box::pin(async move {
            let service = service_from_value(definition).map_err(|error| {
                service_config_api::ServiceConfigError::Exception(error.to_string())
            })?;
            self.storage
                .save(ServiceHandle::new(service))
                .await
                .map_err(|error| {
                    service_config_api::ServiceConfigError::Exception(error.to_string())
                })
        })
    }
    fn update_service_config<'a>(
        &'a self,
        _: &'a str,
        _: ConfigHandle,
    ) -> service_config_api::ServiceConfigFuture<'a, ()> {
        Box::pin(async {
            Err(service_config_api::ServiceConfigError::Exception(
                "database_update_service_config_not_supported".into(),
            ))
        })
    }
}
#[derive(Clone)]
pub struct LoadedProxyConfiguration {
    pub proxy: ProxyServiceConfig,
    pub capacity_config: Value,
    pub rate_config: Value,
    pub model_override: Value,
}
struct CachedProxy {
    config: LoadedProxyConfiguration,
    loaded: Instant,
}
pub struct PgProxyConfigLoader {
    pool: PgPool,
    ttl: Duration,
    cache: Mutex<HashMap<String, CachedProxy>>,
}
impl PgProxyConfigLoader {
    pub fn new(pool: PgPool, ttl: Duration) -> Self {
        Self {
            pool,
            ttl,
            cache: Mutex::new(HashMap::new()),
        }
    }
    pub async fn load(
        &self,
        name: &str,
    ) -> Result<Option<LoadedProxyConfiguration>, service_config_api::ServiceConfigError> {
        if let Some(entry) = self
            .cache
            .lock()
            .map_err(|_| service_config_api::ServiceConfigError::Internal)?
            .get(name)
            && entry.loaded.elapsed() <= self.ttl
        {
            return Ok(Some(entry.config.clone()));
        }
        // Source catches both row-query and parser failures, logs, returns None.
        let row = match sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('service_id',service_id,'name',name,'connector_config',connector_config,'service_config',service_config,'timeout',timeout,'status',status,'metadata',metadata) FROM gateway.services WHERE (service_id=$1 OR name=$1) AND status='active' LIMIT 1").bind(name).fetch_optional(&self.pool).await {
            Ok(row) => row,
            Err(_) => { eprintln!("proxy_config_load_failed"); return Ok(None); }
        };
        let Some(row) = row else {
            return Ok(None);
        };
        let Some(config) = parse_proxy_row(&row) else {
            eprintln!("proxy_config_parse_failed");
            return Ok(None);
        };
        self.cache
            .lock()
            .map_err(|_| service_config_api::ServiceConfigError::Internal)?
            .insert(
                name.into(),
                CachedProxy {
                    config: config.clone(),
                    loaded: Instant::now(),
                },
            );
        Ok(Some(config))
    }
    pub async fn list(
        &self,
    ) -> Result<Vec<LoadedProxyConfiguration>, service_config_api::ServiceConfigError> {
        let rows = sqlx::query_scalar::<_, Value>("SELECT jsonb_build_object('service_id',service_id,'name',name,'connector_config',connector_config,'service_config',service_config,'timeout',timeout,'status',status,'metadata',metadata) FROM gateway.services WHERE status='active'")
            .fetch_all(&self.pool).await.map_err(|_| service_config_api::ServiceConfigError::Internal)?;
        Ok(rows.iter().filter_map(parse_proxy_row).collect())
    }
    pub fn invalidate_config(
        &self,
        name: &str,
    ) -> Result<(), service_config_api::ServiceConfigError> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| service_config_api::ServiceConfigError::Internal)?;
        if name.is_empty() {
            cache.clear();
        } else {
            cache.remove(name);
        }
        Ok(())
    }
}
impl service_config_api::ServiceConfigProxyLoader for PgProxyConfigLoader {
    fn get_config<'a>(
        &'a self,
        id: &'a str,
    ) -> service_config_api::ServiceConfigFuture<
        'a,
        Option<service_config_api::ServiceConfigProxyConfig>,
    > {
        Box::pin(async move {
            Ok(self
                .load(id)
                .await?
                .map(|config| service_config_api::ServiceConfigProxyConfig {
                    capacity_config: config.capacity_config,
                    model_override: config.model_override,
                    default_provider: config
                        .proxy
                        .default_provider
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                }))
        })
    }
    fn invalidate(&self, id: &str) -> Result<(), service_config_api::ServiceConfigError> {
        self.invalidate_config(id)
    }
}
impl services_api::ServicesProxyInvalidation for PgProxyConfigLoader {
    fn invalidate(&self, id: &str) -> Result<(), services_api::ServicesError> {
        // ServicesApi performs its own shared access-cache invalidation after
        // this source proxy-loader hook, preserving that ordering exactly.
        self.invalidate_config(id)
    }
}
fn object(row: &Value, key: &str) -> Value {
    match row.get(key) {
        Some(Value::Object(_)) => row[key].clone(),
        Some(Value::String(text)) => serde_json::from_str(text)
            .ok()
            .filter(Value::is_object)
            .unwrap_or(json!({})),
        _ => json!({}),
    }
}
fn value_string(value: &Value) -> String {
    crate::usage_parser::python_value_to_string(value)
}
fn normalized_url(value: &Value) -> String {
    if value.is_null() {
        String::new()
    } else {
        value_string(value).trim().trim_end_matches('/').into()
    }
}
fn identity(config: &Value, metadata: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        let value = config
            .get(*key)
            .filter(|v| !v.is_null())
            .or_else(|| metadata.get(*key).filter(|v| !v.is_null()))?;
        let text = value_string(value).trim().to_owned();
        (!text.is_empty()).then_some(text)
    })
}
fn optional_string(value: &Value) -> Option<String> {
    value.as_str().map(str::to_owned)
}
fn number(value: Option<&Value>, default: f64) -> Option<f64> {
    value
        .filter(|v| !v.is_null())
        .map(|v| v.as_f64())
        .unwrap_or(Some(default))
        .filter(|n| n.is_finite() && *n >= 0.0)
}
fn nonnull<'a>(a: &'a Value, b: &'a Value, key: &str) -> Option<&'a Value> {
    a.get(key)
        .filter(|v| !v.is_null())
        .or_else(|| b.get(key).filter(|v| !v.is_null()))
}
pub fn parse_proxy_row(row: &Value) -> Option<LoadedProxyConfiguration> {
    let c = object(row, "connector_config");
    let s = object(row, "service_config");
    let m = object(row, "metadata");
    let runtime = object(&s, "proxy_runtime");
    let cache = object(&s, "cache");
    let base = normalized_url(&c["base_url"]);
    let mut upstream = normalized_url(&c["upstream_url"]);
    if upstream.is_empty()
        || (!base.is_empty() && upstream != base && c["proxy_mode"] == "transparent")
    {
        upstream = base;
    }
    let urls = if let Some(text) = c["instance_urls"].as_str().filter(|s| !s.is_empty()) {
        text.split(',')
            .map(|v| Value::String(v.trim().into()))
            .collect::<Vec<_>>()
    } else {
        c["upstream_urls"].as_array().cloned().unwrap_or_default()
    };
    let capacity = s
        .get("capacity")
        .filter(|v| python_truthy(v))
        .or_else(|| m.get("capacity"))
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or(json!({}));
    let concurrency = nonnull(&runtime, &c, "concurrency_limit")
        .map(Value::as_u64)
        .unwrap_or(Some(32))?;
    let optional_usize = |key: &str| -> Option<Option<usize>> {
        match nonnull(&runtime, &c, key) {
            None => Some(None),
            Some(v) => usize::try_from(v.as_u64()?).ok().map(Some),
        }
    };
    Some(LoadedProxyConfiguration {
        proxy: ProxyServiceConfig {
            service_id: row["service_id"].as_str().unwrap_or("").into(),
            service_name: row["name"].as_str().unwrap_or("").into(),
            upstream_url: upstream,
            upstream_urls: urls
                .iter()
                .map(normalized_url)
                .filter(|s| !s.is_empty())
                .collect(),
            assistant_id: optional_string(&c["assistant_id"]),
            graph_id: optional_string(&c["graph_id"]),
            path_rewrite: optional_string(&c["path_rewrite"]),
            auth_token: optional_string(&c["auth_token"]),
            forward_auth: c.get("forward_auth").map(python_truthy).unwrap_or(true),
            load_balance_strategy: c["load_balance_strategy"]
                .as_str()
                .unwrap_or("round_robin")
                .into(),
            concurrency_limit: usize::try_from(concurrency).ok()?,
            streaming_concurrency_limit: optional_usize("streaming_concurrency_limit")?,
            non_streaming_concurrency_limit: optional_usize("non_streaming_concurrency_limit")?,
            concurrency_queue_timeout: number(
                nonnull(&runtime, &c, "concurrency_queue_timeout"),
                30.0,
            )?,
            timeout_connect: number(c.get("timeout_connect"), 5.0)?,
            timeout_read: number(c.get("timeout_read"), 300.0)?,
            health_check_timeout: number(nonnull(&runtime, &c, "health_check_timeout"), 5.0)?,
            default_model: identity(&c, &m, &["default_model", "model", "model_id", "llm_model"]),
            default_provider: identity(
                &c,
                &m,
                &[
                    "default_provider",
                    "provider",
                    "provider_id",
                    "llm_provider",
                    "vendor",
                ],
            ),
            metadata: m,
            enabled: row["status"] == "active",
            cache_enabled: cache.get("enabled").is_some_and(python_truthy),
            cache_ttl: cache["ttl"].as_i64().unwrap_or(300),
        },
        capacity_config: capacity,
        rate_config: s.get("rate_limit").cloned().unwrap_or(Value::Null),
        model_override: c
            .get("model_override")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(Value::Null),
    })
}

pub struct NativeServicesProxy {
    pub proxy: Arc<TransparentProxy>,
}
impl services_api::ServicesTransparentProxy for NativeServicesProxy {
    fn legacy_string_service_type(&self, _: &crate::protocol::ServiceDefinition) -> bool {
        // Native ServiceDefinition is always typed. Source str(Enum) deliberately
        // does not match the legacy literal-string branch.
        false
    }
    fn availability<'a>(
        &'a self,
        c: &'a services_api::ServicesProxyConfig,
    ) -> services_api::ServicesFuture<'a, Value> {
        Box::pin(async move {
            self.proxy
                .health(&ProxyServiceConfig {
                    service_id: c.service_id.clone(),
                    service_name: c.service_name.clone(),
                    upstream_url: c.upstream_url.clone(),
                    upstream_urls: c.upstream_urls.iter().map(value_string).collect(),
                    assistant_id: optional_string(&c.assistant_id),
                    graph_id: optional_string(&c.graph_id),
                    metadata: c.metadata.clone(),
                    enabled: c.enabled,
                    ..Default::default()
                })
                .await
                .map_err(|error| {
                    service_config_api::ServiceConfigError::detail(error.status, error.detail)
                })
        })
    }
}
pub struct NativeServicesRuntime {
    pub sdk_session_pool: PgPool,
    pub sdk_sessions: Arc<crate::session_manager::DatabaseSessionManager>,
    pub control: Arc<RuntimeControlPlane>,
    pub image_worker_running: Arc<AtomicBool>,
}
impl services_api::ServicesRuntime for NativeServicesRuntime {
    fn status(&self) -> services_api::ServicesRuntimeStatus {
        services_api::ServicesRuntimeStatus {
            control_present: true,
            image_worker_running: self.image_worker_running.load(Ordering::Acquire),
        }
    }
}
impl sessions_api::SessionsRuntimeCleanup for NativeServicesRuntime {
    fn delete_owned_session<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        session: &'a str,
    ) -> sessions_api::SessionsApiFuture<'a, bool> {
        Box::pin(async move {
            use crate::migration_adapters::sdk_session_cleanup::{self, CleanupError};
            sdk_session_cleanup::delete_owned(
                &self.sdk_session_pool,
                tenant,
                user,
                session,
                || async move {
                    self.control
                        .cleanup_session(&Scope {
                            tenant_id: tenant.into(),
                            user_id: user.into(),
                            session_id: session.into(),
                        })
                        .await
                        .map(|_| ())
                        .map_err(|e| CleanupError::Runtime(e.status))
                },
            )
            .await
            .map_err(|e| match e {
                CleanupError::SessionUnavailable => {
                    sessions_api::SessionsApiError::http(404, "session not found")
                }
                CleanupError::Runtime(status) => {
                    sessions_api::SessionsApiError::http(status, "runtime_session_cleanup_failed")
                }
                _ => sessions_api::SessionsApiError::http(503, "sdk_session_cleanup_unavailable"),
            })?;
            self.sdk_sessions.invalidate(session).await;
            Ok(true)
        })
    }
    fn session_deleted<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        session: &'a str,
    ) -> sessions_api::SessionsApiFuture<'a, bool> {
        Box::pin(async move {
            crate::migration_adapters::sdk_session_cleanup::is_deleted(
                &self.sdk_session_pool,
                tenant,
                user,
                session,
            )
            .await
            .map_err(|_| {
                sessions_api::SessionsApiError::http(503, "sdk_session_cleanup_unavailable")
            })
        })
    }
    fn cleanup_session<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        session: &'a str,
    ) -> sessions_api::SessionsApiFuture<'a, bool> {
        Box::pin(async move {
            self.control
                .cleanup_session(&Scope {
                    tenant_id: tenant.into(),
                    user_id: user.into(),
                    session_id: session.into(),
                })
                .await
                .map_err(|e| sessions_api::SessionsApiError::http(e.status, e.detail))
        })
    }
}
pub struct SafeFoundationErrors;
impl service_config_api::ServiceConfigExceptionText for SafeFoundationErrors {
    fn registry(&self, e: &RegistryError) -> String {
        crate::metrics_redaction::redact_sensitive_text(&e.to_string())
    }
    fn audit(&self, e: &config_audit_writer::ConfigAuditError) -> String {
        e.to_string()
    }
    fn internal(&self) -> String {
        "service_configuration_failed".into()
    }
}
impl services_api::ServicesExceptionText for SafeFoundationErrors {
    fn registry(&self, e: &RegistryError) -> String {
        crate::metrics_redaction::redact_sensitive_text(&e.to_string())
    }
    fn validation(&self, e: &LangGraphValidationError) -> String {
        crate::metrics_redaction::redact_sensitive_text(&e.to_string())
    }
    fn internal(&self) -> String {
        "service_operation_failed".into()
    }
}
pub struct NativeRatePublication {
    active: Mutex<Arc<RuntimeRateRules>>,
}
impl NativeRatePublication {
    pub fn current(&self) -> Arc<RuntimeRateRules> {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}
impl rate_config_api::RateConfigPublication for NativeRatePublication {
    fn publish(&self, rules: Arc<RuntimeRateRules>) {
        *self.active.lock().unwrap_or_else(|e| e.into_inner()) = rules;
    }
}
#[derive(Default)]
pub struct NativeAccessWarnings {
    failures: AtomicU64,
}
impl NativeAccessWarnings {
    pub fn failure_count(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }
}
impl service_access_resolver::ServiceAccessWarningSink for NativeAccessWarnings {
    fn warn_lookup_failure(&self, source: ServiceAccessLookupFailure, _identity: &str) {
        self.failures.fetch_add(1, Ordering::Relaxed);
        eprintln!(
            "service_access_{}_lookup_failed",
            match source {
                ServiceAccessLookupFailure::Tenant => "tenant",
                ServiceAccessLookupFailure::User => "user",
            }
        );
    }
}
pub struct NativeConnectorDestination;
impl connector_admin_api::ConnectorDestinationValidator for NativeConnectorDestination {
    fn validate<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<(), connector_admin_api::ConnectorDestinationError>,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            let addresses = NativeConnectorDnsResolver
                .resolve(host, port)
                .await
                .map_err(|_| connector_admin_api::ConnectorDestinationError::Unresolved)?;
            if addresses.is_empty() {
                return Err(connector_admin_api::ConnectorDestinationError::Unresolved);
            }
            if addresses.into_iter().any(|ip| !oauth_form_safe_address(ip)) {
                return Err(connector_admin_api::ConnectorDestinationError::Disallowed);
            }
            Ok(())
        })
    }
}
pub struct NativeUsageClock {
    pub local_offset: FixedOffset,
}
impl usage_admin_api::UsageAdminClock for NativeUsageClock {
    fn today(&self) -> NaiveDate {
        Utc::now().with_timezone(&self.local_offset).date_naive()
    }
    fn utc_now(&self) -> DateTime<Utc> {
        Utc::now()
    }
    fn local_now(&self) -> NaiveDateTime {
        Utc::now().with_timezone(&self.local_offset).naive_local()
    }
}

#[derive(Clone)]
struct SecurityEvent {
    tenant: String,
    user: Option<String>,
    service: Option<String>,
    kind: String,
}
pub struct PgFoundationSecurityEvents {
    pool: PgPool,
    tasks: Mutex<JoinSet<()>>,
    failures: Arc<AtomicU64>,
}
impl PgFoundationSecurityEvents {
    pub async fn record_decision(&self, tenant: &str, user: &str, service: &str, kind: &str) {
        if Self::record(
            &self.pool,
            SecurityEvent {
                tenant: if tenant.is_empty() {
                    "public".into()
                } else {
                    tenant.into()
                },
                user: Some(user.into()),
                service: Some(service.into()),
                kind: kind.into(),
            },
        )
        .await
        .is_err()
        {
            self.failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            tasks: Mutex::new(JoinSet::new()),
            failures: Arc::new(AtomicU64::new(0)),
        }
    }
    pub fn failure_count(&self) -> u64 {
        self.failures.load(Ordering::Relaxed)
    }
    async fn record(pool: &PgPool, event: SecurityEvent) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO security_event_daily_aggregates (tenant_id,user_id,service_id,event_type,date,event_count) VALUES ($1,$2,$3,$4,$5,1) ON CONFLICT (tenant_id,COALESCE(user_id,''),COALESCE(service_id,''),event_type,date) DO UPDATE SET event_count=security_event_daily_aggregates.event_count+1,updated_at=CURRENT_TIMESTAMP")
            .bind(event.tenant).bind(event.user.filter(|v|!v.is_empty())).bind(event.service.filter(|v|!v.is_empty())).bind(event.kind).bind(Utc::now().date_naive()).execute(pool).await?;
        Ok(())
    }
    fn schedule_event(&self, event: SecurityEvent) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.failures.fetch_add(1, Ordering::Relaxed);
            eprintln!("security_event_runtime_unavailable");
            return;
        };
        let pool = self.pool.clone();
        let failures = self.failures.clone();
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        while let Some(result) = tasks.try_join_next() {
            if result.is_err() {
                self.failures.fetch_add(1, Ordering::Relaxed);
            }
        }
        tasks.spawn_on(
            async move {
                if Self::record(&pool, event).await.is_err() {
                    failures.fetch_add(1, Ordering::Relaxed);
                    eprintln!("security_event_write_failed");
                }
            },
            &runtime,
        );
    }
    /// Call only after request/task admission is stopped and callers are joined.
    pub async fn drain(&self) {
        let mut tasks = {
            let mut guard = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::replace(&mut *guard, JoinSet::new())
        };
        while let Some(result) = tasks.join_next().await {
            if result.is_err() {
                self.failures.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    fn denied(&self, tenant: &str, user: &str) {
        self.schedule_event(SecurityEvent {
            tenant: tenant.into(),
            user: Some(user.into()),
            service: None,
            kind: "permission_denied".into(),
        })
    }
}
impl metrics_admin_api::MetricsPermissionDeniedSink for PgFoundationSecurityEvents {
    fn schedule(&self, event: metrics_admin_api::MetricsPermissionDeniedEvent) {
        self.schedule_event(SecurityEvent {
            tenant: event.tenant_id,
            user: event.user_id,
            service: event.service_id,
            kind: event.event_type,
        })
    }
}
impl quota_admin_api::QuotaPermissionDenied for PgFoundationSecurityEvents {
    fn schedule(
        &self,
        _: &quota_admin_api::QuotaAdminContext,
        auth: &quota_admin_api::QuotaAdminIdentity,
        _: Capability,
    ) {
        self.denied(auth.tenant_id(), auth.user_id())
    }
}

#[derive(Clone)]
struct TrustedRequest {
    audit: config_audit_writer::ConfigAuditRequestContext,
}
tokio::task_local! { static FOUNDATION_REQUEST: TrustedRequest; }
/// Mount inside host request_context, so IDs come from its validated extensions.
/// Forwarded headers never become the socket peer and cannot establish trust.
pub async fn request_effects_context(mut request: Request, next: Next) -> Response {
    let context = request.extensions().get::<RequestContext>();
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|p| p.0.ip().to_string());
    let audit = config_audit_writer::ConfigAuditRequestContext {
        request_id: context.map(|c| json!(c.request_id)),
        trace_id: context.map(|c| json!(c.trace_id)),
        headers: request.headers().clone(),
        client_host: peer,
    };
    let trace = context.map(|c| c.trace_id.clone()).unwrap_or_default();
    request
        .extensions_mut()
        .insert(crate::setup_api::SetupRequestTrace(trace));
    FOUNDATION_REQUEST
        .scope(TrustedRequest { audit }, next.run(request))
        .await
}
pub struct NativeRequestEffects {
    pub security: Arc<PgFoundationSecurityEvents>,
}
impl FoundationRequestEffects for NativeRequestEffects {
    fn audit_context(&self, headers: &HeaderMap) -> config_audit_writer::ConfigAuditRequestContext {
        FOUNDATION_REQUEST
            .try_with(|context| context.audit.clone())
            .unwrap_or_else(|_| config_audit_writer::ConfigAuditRequestContext {
                request_id: None,
                trace_id: None,
                headers: headers.clone(),
                client_host: None,
            })
    }
    fn permission_denied(
        &self,
        identity: &services_api::ServicesIdentity,
        _: &config_audit_writer::ConfigAuditRequestContext,
        _: Capability,
    ) {
        self.security.denied(&identity.tenant_id, &identity.user_id)
    }
}

pub struct NativeQuotaChecker {
    pub store: Arc<PgQuotaStore>,
    pub rpm: Arc<crate::rate_limits::QuotaRpmLimiter>,
    pub security: Arc<PgFoundationSecurityEvents>,
}
impl NativeQuotaChecker {
    pub fn new(
        pools: &DatabasePools,
        security: Arc<PgFoundationSecurityEvents>,
    ) -> Result<Self, BootstrapError> {
        Ok(Self {
            store: Arc::new(
                PgQuotaStore::new_supervised(pools.data.clone(), pools.control.clone())
                    .map_err(|_| BootstrapError::Construction("foundation_quota"))?,
            ),
            rpm: Arc::new(crate::rate_limits::QuotaRpmLimiter::default()),
            security,
        })
    }
    pub fn with_shared_rpm(mut self, redis: redis::aio::ConnectionManager) -> Self {
        self.rpm = Arc::new(crate::rate_limits::QuotaRpmLimiter::shared(redis));
        self
    }
    pub async fn check_quota(
        &self,
        tenant: &str,
        user: &str,
        estimated_tokens: i64,
        record_security_event: bool,
    ) -> Result<billing::QuotaCheckResult, quota_admin_api::QuotaAdminError> {
        let now = Utc::now();
        // Canonical _get_or_create_quota logs ordinary storage failure and
        // returns None; that branch intentionally means unrestricted.
        let loaded = self.store.get_or_create_quota_at(tenant, user, now).await;
        let result = self
            .rpm
            .check_quota_shared(
                tenant,
                user,
                loaded.as_ref().map(|row| &row.quota),
                estimated_tokens,
                now,
            )
            .await
            .map_err(|_| quota_admin_api::QuotaAdminError::Internal)?;
        if record_security_event
            && matches!(
                result.status,
                billing::QuotaStatus::Exceeded | billing::QuotaStatus::Blocked
            )
        {
            if PgFoundationSecurityEvents::record(
                &self.security.pool,
                SecurityEvent {
                    tenant: tenant.into(),
                    user: Some(user.into()),
                    service: None,
                    kind: "quota_exceeded".into(),
                },
            )
            .await
            .is_err()
            {
                self.security.failures.fetch_add(1, Ordering::Relaxed);
                eprintln!("quota_security_event_write_failed");
            }
        }
        Ok(result)
    }
}
impl quota_admin_api::QuotaAdminChecker for NativeQuotaChecker {
    fn check<'a>(
        &'a self,
        tenant: &'a str,
        user: &'a str,
        estimated_tokens: i64,
    ) -> quota_admin_api::QuotaAdminFuture<'a, billing::QuotaCheckResult> {
        Box::pin(self.check_quota(tenant, user, estimated_tokens, true))
    }
}
