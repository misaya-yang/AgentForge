//! Durable construction of the source-backed public foundation API graph.
//! The caller supplies live shared services and genuinely unported host policies.
//! Construction performs no I/O, starts no workers, and asserts no readiness.
use super::{
    BootstrapError, DatabasePools, auth::AuthServices, core::CoreServices, models::ModelServices,
};
use crate::migration_content::{ContentAccess, access::NativeContentAccess};
use crate::migration_host::foundation::FoundationApis;
use crate::*;
use axum::http::HeaderMap;
use redis::aio::ConnectionManager;
use serde_json::Value;
use std::sync::Arc;

/// Process singletons also used by middleware, model probes and recording paths.
/// Passing them here avoids an admin endpoint observing a second empty instance.
#[derive(Clone)]
pub struct FoundationShared {
    pub connectors: Arc<connector_principal_store::ConnectorPrincipalRepository>,
    pub mcp_database: Arc<mcp_registry_store::PgMcpDatabase>,
    pub audit: config_audit_writer::ConfigAuditWriter,
    pub access: service_access_resolver::ServiceAccessConstraintResolver,
    pub runtime_auth: Arc<auth_config_api::RuntimeAuthStore>,
    pub runtime_rates: Arc<rate_policy_store::RuntimeRateRules>,
    pub rate_resolver: rate_policy_store::RatePolicyResolver,
    pub balancer: Arc<balancer_config_api::BalancerConfigStore>,
    pub load_balancer: balancer_config_api::SharedConfigLoadBalancer,
    pub probes: Arc<provider_admin_api::ProviderAdminProbeState>,
    pub metrics: Arc<metrics_recorder::MetricsRecorder>,
    pub realtime: Arc<realtime_metrics::RealtimeMetricsService>,
    pub metadata: Arc<dyn health_api::HealthModelMetadata>,
}

/// Source process defaults are kept distinct from configured active services.
pub struct FoundationSharedConfiguration {
    pub runtime_rate_rules: Vec<Value>,
    pub balancer_strategy: String,
    pub latency_sample_cap: i64,
    pub trusted_proxies: client_ip::TrustedProxies,
}
struct AuditDatabase(config_audit_postgres::PgConfigAuditPool);
impl config_audit_writer::ConfigAuditDatabase for AuditDatabase {
    fn audit_service(&self) -> Option<&dyn config_audit_writer::ConfigAuditService> {
        None
    }
    fn pool(&self) -> Option<&dyn config_audit_writer::ConfigAuditPool> {
        Some(&self.0)
    }
}
impl FoundationShared {
    pub fn new(
        pools: &DatabasePools,
        redis: Option<ConnectionManager>,
        connectors: Arc<connector_principal_store::ConnectorPrincipalRepository>,
        mcp_database: Arc<mcp_registry_store::PgMcpDatabase>,
        metadata: Arc<dyn health_api::HealthModelMetadata>,
        config: FoundationSharedConfiguration,
    ) -> Result<Self, BootstrapError> {
        let audit_pool = config_audit_postgres::PgConfigAuditPool::new(
            pools.data.clone(),
            pools.control.clone(),
        )
        .map_err(|_| BootstrapError::Construction("config_audit_pool"))?;
        let audit = config_audit_writer::ConfigAuditWriter::new(
            Some(Arc::new(AuditDatabase(audit_pool))),
            Arc::new(config_audit_writer::ProxyAwareConfigAuditClientIp {
                trusted: Some(config.trusted_proxies),
            }),
        );
        let metrics_redis = redis.clone().map(|r| {
            Arc::new(metrics_recorder::RedisMetricsBackend::new(r))
                as Arc<dyn metrics_recorder::MetricsRedis>
        });
        let realtime_redis = redis.map(|r| {
            Arc::new(realtime_metrics::RedisRealtimeBackend::new(r, true))
                as Arc<dyn realtime_metrics::RealtimeRedis>
        });
        Ok(Self {
            connectors,
            mcp_database,
            metadata,
            audit,
            access: service_access_resolver::ServiceAccessConstraintResolver::new(),
            runtime_auth: Arc::new(auth_config_api::RuntimeAuthStore::default()),
            runtime_rates: Arc::new(rate_policy_store::RuntimeRateRules::new(
                config.runtime_rate_rules,
            )),
            rate_resolver: rate_policy_store::RatePolicyResolver::new(Arc::new(
                rate_policy_store::PgRateRuleSource::new(pools.data.clone()),
            )),
            balancer: Arc::new(balancer_config_api::BalancerConfigStore::default()),
            load_balancer: Arc::new(tokio::sync::Mutex::new(
                registry_load_balancer::LoadBalancer::new(config.balancer_strategy),
            )),
            probes: Arc::new(provider_admin_api::ProviderAdminProbeState::new(
                provider_health::ProviderProbeStore::new(),
                Arc::new(provider_health::SystemProviderProbeRuntime),
            )),
            metrics: Arc::new(metrics_recorder::MetricsRecorder::new(
                metrics_redis,
                Arc::new(metrics_recorder::SystemMetricsClock),
                config.latency_sample_cap,
            )),
            realtime: Arc::new(realtime_metrics::RealtimeMetricsService::new(
                realtime_redis,
                Arc::new(realtime_metrics::SystemRealtimeRuntime),
            )),
        })
    }
}

/// Explicit captured settings; never Debug/Serialize because keys can be present.
pub struct FoundationConfiguration {
    pub timestamps: super::timestamps::DatabaseTimestampPolicy,
    pub auth: auth_config_api::AuthConfigSettings,
    pub system: system_status_api::SystemStatusSettingsSnapshot,
    pub balancer_strategy: String,
    pub connector_environment: connector_user_types::ConnectorEnvironment,
    pub connector_encryption_key: String,
    pub console_public_base_url: String,
    pub principal_enabled: bool,
    pub mcp_enabled: bool,
    pub mcp_client_ttl_seconds: f64,
    pub mcp_client_max_entries: usize,
    pub users: users_api::UsersConfiguration,
    pub password_concurrency: usize,
    pub service_access_ttl: Value,
    pub service_access_database_module: String,
    pub catalog: Result<Arc<assistant_catalog::CapabilityCatalog>, assistant_catalog::CatalogError>,
    pub usage_dimension_order: [String; 5],
}

/// No fallback implementations: these interfaces do not yet have an accepted
/// concrete native backend, or carry deployment-specific runtime/cancellation policy.
pub struct FoundationBackends {
    pub role_cache: Option<Arc<dyn roles_store::RolesPermissionCache>>,
    pub rate_publication: Arc<dyn rate_config_api::RateConfigPublication>,
    pub service_database: Arc<dyn service_config_api::ServiceConfigDatabase>,
    pub proxy_loader: Option<Arc<dyn service_config_api::ServiceConfigProxyLoader>>,
    pub service_errors: Arc<dyn service_config_api::ServiceConfigExceptionText>,
    pub services_errors: Arc<dyn services_api::ServicesExceptionText>,
    pub services_runtime: Arc<dyn services_api::ServicesRuntime>,
    pub proxy_invalidation: Option<Arc<dyn services_api::ServicesProxyInvalidation>>,
    pub transparent_proxy: Option<Arc<dyn services_api::ServicesTransparentProxy>>,
    pub access_warnings: Arc<dyn service_access_resolver::ServiceAccessWarningSink>,
    pub request_effects: Arc<dyn FoundationRequestEffects>,
    pub connector_destination: Arc<dyn connector_admin_api::ConnectorDestinationValidator>,
    pub confluence_registrar: Arc<dyn connector_user_activation::ConnectorConfluenceRegistrar>,
    pub mcp_secrets: Arc<dyn mcp_gateway_broker::McpSecretResolver>,
    pub tool_invalidation: Option<Arc<dyn tenant_policies_api::TenantPolicyInvalidation>>,
    pub mcp_invalidation: Option<Arc<dyn tenant_policies_api::TenantPolicyInvalidation>>,
    pub model_publisher: Option<Arc<dyn model_admin_api::ModelConfigPublisher>>,
    pub provider_publisher: Option<Arc<dyn provider_admin_api::ProviderAdminPublisher>>,
    pub usage_clock: Arc<dyn usage_admin_api::UsageAdminClock>,
    pub quota_checker: Arc<dyn quota_admin_api::QuotaAdminChecker>,
    pub quota_denied: Arc<dyn quota_admin_api::QuotaPermissionDenied>,
    pub metrics_denied: Arc<dyn metrics_admin_api::MetricsPermissionDeniedSink>,
    pub health_dependencies: Arc<dyn health_api::HealthDependencyState>,
    pub health_cancellation: Arc<dyn health_api::HealthHttpCancellation>,
    pub architecture_runtime: Arc<dyn architecture_status_api::ArchitectureRuntime>,
    pub local_channel: Option<Arc<dyn local_nodes_api::LocalNodesChannelVerifier>>,
    pub sessions_cleanup: Arc<dyn sessions_api::SessionsRuntimeCleanup>,
}

/// Trusted request extensions and owned audit scheduling stay with the host.
/// In particular, forwarded headers alone must not become a trusted peer address.
pub trait FoundationRequestEffects: Send + Sync {
    fn audit_context(&self, headers: &HeaderMap) -> config_audit_writer::ConfigAuditRequestContext;
    fn permission_denied(
        &self,
        identity: &services_api::ServicesIdentity,
        context: &config_audit_writer::ConfigAuditRequestContext,
        capability: ai_platform_auth::rbac::Capability,
    );
}

struct EndpointAdapters {
    content: Arc<NativeContentAccess>,
    admission: Arc<ai_platform_auth::admission::AdmissionService>,
    effects: Arc<dyn FoundationRequestEffects>,
    database_identity: usize,
    database_module: String,
    cache_ttl: Value,
}
impl tool_inventory_api::ToolInventoryIdentityResolver for EndpointAdapters {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        _path: &'a str,
    ) -> service_config_api::ServiceConfigFuture<'a, tool_inventory_api::ToolInventoryIdentity>
    {
        Box::pin(async move {
            let user =
                self.content.resolve(headers).await.map_err(|e| {
                    service_config_api::ServiceConfigError::detail(e.status, e.detail)
                })?;
            Ok(tool_inventory_api::ToolInventoryIdentity {
                is_authenticated: user.is_authenticated,
                tier: user.tier,
                roles: user.roles,
            })
        })
    }
}
impl presign_api::PresignIdentityResolver for EndpointAdapters {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        _path: &'a str,
    ) -> service_config_api::ServiceConfigFuture<'a, presign_api::PresignIdentity> {
        Box::pin(async move {
            let user =
                self.content.resolve(headers).await.map_err(|e| {
                    service_config_api::ServiceConfigError::detail(e.status, e.detail)
                })?;
            Ok(presign_api::PresignIdentity {
                is_authenticated: user.is_authenticated,
            })
        })
    }
}
impl model_admin_api::ModelAdminIdentityResolver for EndpointAdapters {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
    ) -> model_admin_api::AdminFuture<'a, model_admin_api::ResolvedModelUser> {
        Box::pin(async move {
            let user = self
                .content
                .resolve(headers)
                .await
                .map_err(|e| model_admin_api::ModelAdminError::detail(e.status, e.detail))?;
            Ok(model_admin_api::ResolvedModelUser {
                user_id: user.user_id,
                tenant_id: user.tenant_id,
                roles: user.roles,
                is_authenticated: user.is_authenticated,
            })
        })
    }
    fn audit_request(&self, headers: &HeaderMap) -> config_audit_writer::ConfigAuditRequestContext {
        self.effects.audit_context(headers)
    }
}
impl services_api::ServicesIdentityResolver for EndpointAdapters {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
        needs_user: bool,
    ) -> services_api::ServicesFuture<'a, services_api::ServicesIdentity> {
        Box::pin(async move {
            let request = self.admission.request(headers, path);
            let principal = request.resolve().await.map_err(|e| {
                services_api::ServicesError::detail(e.status(), "Authentication required")
            })?;
            let mut identity = services_api::ServicesIdentity {
                user_id: principal.user_id().into(),
                tenant_id: principal.tenant_id().into(),
                roles: principal.roles().to_vec(),
                permissions: principal.permissions().to_vec(),
                access: Default::default(),
            };
            if needs_user {
                let user = self
                    .content
                    .resolve(headers)
                    .await
                    .map_err(|e| services_api::ServicesError::detail(e.status, e.detail))?;
                identity.access = service_access_resolver::ServiceAccessResolverContext {
                    load: service_access_policy::ServiceAccessLoadContext {
                        api_key_info: principal.api_key().map_or(
                            Value::Null,
                            |key| serde_json::json!({"allowed_services":key.allowed_services}),
                        ),
                        user_id: user.user_id,
                        tenant_id: user.tenant_id,
                        is_authenticated: user.is_authenticated,
                    },
                    database_identity: self.database_identity,
                    database_type_module: self.database_module.clone(),
                    configured_ttl: self.cache_ttl.clone(),
                    api_key_hash: principal
                        .api_key()
                        .map_or(Value::Null, |key| Value::String(key.key_hash.clone())),
                    roles: user.roles.into_iter().map(Value::String).collect(),
                };
            }
            Ok(identity)
        })
    }
    fn context(&self, headers: &HeaderMap) -> config_audit_writer::ConfigAuditRequestContext {
        self.effects.audit_context(headers)
    }
    fn permission_denied(
        &self,
        identity: &services_api::ServicesIdentity,
        context: &config_audit_writer::ConfigAuditRequestContext,
        capability: ai_platform_auth::rbac::Capability,
    ) {
        self.effects
            .permission_denied(identity, context, capability)
    }
}

struct SharedHealth(Arc<tokio::sync::Mutex<registry_health_monitor::HealthMonitor>>);
impl health_api::HealthStatusSource for SharedHealth {
    fn all_status(&self) -> Result<health_api::HealthRecords, health_api::HealthApiError> {
        let monitor = self
            .0
            .try_lock()
            .map_err(|_| health_api::HealthApiError::Internal)?;
        health_api::HealthStatusSource::all_status(&*monitor)
    }
    fn get_status(
        &self,
        id: &str,
    ) -> Result<Option<Arc<registry_health_monitor::HealthStatus>>, health_api::HealthApiError>
    {
        let monitor = self
            .0
            .try_lock()
            .map_err(|_| health_api::HealthApiError::Internal)?;
        health_api::HealthStatusSource::get_status(&*monitor, id)
    }
}
struct OsKeyEntropy;
impl config_api_keys_api::ConfigApiKeyRandom for OsKeyEntropy {
    fn bytes32(&self) -> Result<[u8; 32], service_config_api::ServiceConfigError> {
        let mut bytes = [0; 32];
        quiz_entropy::QuizEntropy::fill_bytes(&quiz_entropy::OsQuizEntropy, &mut bytes)
            .map_err(|_| service_config_api::ServiceConfigError::Internal)?;
        Ok(bytes)
    }
}
impl self_service_api_keys::SelfServiceApiKeyEntropy for OsKeyEntropy {
    fn bytes(
        &self,
        length: usize,
    ) -> Result<Vec<u8>, self_service_api_keys::SelfServiceApiKeyError> {
        let mut bytes = vec![0; length];
        quiz_entropy::QuizEntropy::fill_bytes(&quiz_entropy::OsQuizEntropy, &mut bytes)
            .map_err(|_| service_config_api::ServiceConfigError::Internal)?;
        Ok(bytes)
    }
}

/// The durable profile requires both independent PostgreSQL pools. Feature flags
/// such as MCP remain source settings; disabled backends are not synthesized.
pub struct AssembledFoundation {
    pub mcp_broker: Arc<mcp_gateway_broker::McpGatewayBroker>,
    pub apis: FoundationApis,
    pub connector_service: Arc<connector_user_service::ConnectorService>,
}

pub fn assemble_foundation(
    pools: &DatabasePools,
    redis: Option<ConnectionManager>,
    core: &CoreServices,
    auth: &AuthServices,
    models: &ModelServices,
    content: Arc<NativeContentAccess>,
    shared: FoundationShared,
    config: FoundationConfiguration,
    backends: FoundationBackends,
    history: Arc<knowledge::KnowledgeService>,
) -> Result<AssembledFoundation, BootstrapError> {
    let failed = |name| BootstrapError::Construction(name);
    let timestamps = Arc::new(config.timestamps);
    let rbac = (*core.rbac).clone();
    let users_db = Arc::new(
        users_store::PgUsersDatabase::new(
            pools.data.clone(),
            pools.control.clone(),
            Arc::new(users_store::FreshUserPermissions),
        )
        .map_err(|_| failed("users_database"))?,
    );
    let access_db = Arc::new(
        service_access_resolver::PgServiceAccessConstraintStore::new(
            pools.data.clone(),
            pools.control.clone(),
            backends.access_warnings,
        )
        .map_err(|_| failed("service_access_database"))?,
    );
    let adapters = Arc::new(EndpointAdapters {
        content: content.clone(),
        admission: auth.admission.clone(),
        effects: backends.request_effects,
        database_identity: Arc::as_ptr(&access_db) as usize,
        database_module: config.service_access_database_module,
        cache_ttl: config.service_access_ttl,
    });
    let rate_db = Arc::new(rate_config_api::SqlRateConfigDatabase::new(
        true,
        Some(Arc::new(
            rate_config_api::PgRateConfigPool::new(
                pools.data.clone(),
                pools.control.clone(),
                timestamps.clone(),
            )
            .map_err(|_| failed("rate_config_pool"))?,
        )),
    ));
    let key_db = Arc::new(config_api_keys_store::SqlConfigApiKeysDatabase::new(
        true,
        Some(Arc::new(
            config_api_keys_store::PgConfigApiKeysPool::new(
                pools.data.clone(),
                pools.control.clone(),
                timestamps.clone(),
            )
            .map_err(|_| failed("config_api_keys_pool"))?,
        )),
    ));
    let self_keys = Arc::new(self_service_api_keys::SelfServiceApiKeys::new(
        Arc::new(
            self_service_api_keys_store::PgSelfServiceApiKeyStore::new(
                pools.data.clone(),
                pools.control.clone(),
                timestamps.clone(),
            )
            .map_err(|_| failed("self_service_api_keys_store"))?,
        ),
        Arc::new(OsKeyEntropy),
    ));
    let roles_db = Arc::new(roles_store::SqlRolesDatabase::new(
        true,
        Some(Arc::new(
            roles_store::PgRolesPool::new(
                pools.data.clone(),
                pools.control.clone(),
                timestamps.clone(),
            )
            .map_err(|_| failed("roles_pool"))?,
        )),
        backends.role_cache,
    ));
    let tenant_db = Arc::new(tenant_policies_store::SqlTenantPoliciesDatabase::new(
        true,
        Some(Arc::new(
            tenant_policies_store::PgTenantPoliciesPool::new(
                pools.data.clone(),
                pools.control.clone(),
            )
            .map_err(|_| failed("tenant_policies_pool"))?,
        )),
    ));
    let connector_db = Arc::new(connector_admin_store::SqlConnectorAdminDatabase::new(
        true,
        Some(Arc::new(
            connector_admin_store::PgConnectorAdminPool::new(
                pools.data.clone(),
                pools.control.clone(),
            )
            .map_err(|_| failed("connector_admin_pool"))?,
        )),
    ));
    let mcp = Arc::new(mcp_registry_store::SqlMcpRepository::new(
        true,
        Some(shared.mcp_database),
    ));
    let mcp_broker = Arc::new(mcp_gateway_broker::McpGatewayBroker::new(
        mcp.clone(),
        backends.mcp_secrets,
        Arc::new(mcp_http::NativeMcpClientFactory::native()),
        config.mcp_client_ttl_seconds,
        config.mcp_client_max_entries,
    ));
    let provider_http = Arc::new(connector_oauth_http::ReqwestConnectorProviderHttp::new());
    let connectors = Arc::new(connector_user_service::ConnectorService::new(
        connector_user_service::ConnectorServiceDependencies {
            store: Some(Arc::new(
                connector_user_store::PgConnectorUserStore::new(
                    pools.data.clone(),
                    pools.control.clone(),
                )
                .map_err(|_| failed("connector_user_store"))?,
            )),
            state: redis.clone().map(|r| {
                Arc::new(connector_oauth_state::RedisOAuthStateStore::new(r))
                    as Arc<dyn connector_user_types::OAuthStateStore>
            }),
            clock: Arc::new(connector_user_service::SystemConnectorClock),
            nonce: Arc::new(connector_user_service::OsConnectorNonce),
            cipher: Arc::new(connector_oauth_cipher::NativeConnectorTokenCipher::new(
                config.connector_environment,
            )),
            form_http: Arc::new(connector_oauth_http::ReqwestOAuthFormHttp::new()),
            provider_http: provider_http.clone(),
            search: Arc::new(
                connector_user_search::CanonicalConnectorSearchProviders::new(provider_http),
            ),
            activation: Arc::new(
                connector_user_activation::CanonicalConnectorActivation::new(
                    backends.confluence_registrar,
                ),
            ),
            console_public_base_url: config.console_public_base_url,
        },
    ));
    let usage = Arc::new(usage_admin_api::NativeUsageAdminRecorder::new(
        billing_store::PgBillingStore::new(pools.data.clone()),
        backends.usage_clock.clone(),
    ));
    let metrics_redis = redis.clone().map(|r| {
        Arc::new(metrics_admin_api::RedisMetricsAdminBackend::new(r, true))
            as Arc<dyn metrics_admin_api::MetricsAdminRedis>
    });
    let metrics_clock = Arc::new(metrics_admin_api::SystemMetricsAdminClock);
    let quota_pool = Arc::new(
        quota_admin_api::PgQuotaAdminPool::new(pools.data.clone(), pools.control.clone())
            .map_err(|_| failed("quota_admin_pool"))?,
    );
    let quota = Arc::new(
        quota_admin_api::PgQuotaAdminService::new(
            quota_store::PgQuotaStore::new_supervised(pools.data.clone(), pools.control.clone())
                .map_err(|_| failed("quota_store"))?,
            backends.quota_checker,
            timestamps.clone(),
        )
        .map_err(|_| failed("quota_service"))?,
    );
    let shares = Arc::new(
        artifact_shares_store::PgArtifactSharesDatabase::new(
            pools.data.clone(),
            pools.control.clone(),
        )
        .map_err(|_| failed("artifact_shares_database"))?,
    );
    let health_monitor = Arc::new(SharedHealth(core.health_monitor.clone()));
    let audit = shared.audit;
    let apis = FoundationApis {
        auth_config: Arc::new(auth_config_api::AuthConfigApi::new(
            auth_config_api::AuthConfigDependencies {
                identity: auth.admission.clone(),
                rbac: rbac.clone(),
                settings: Arc::new(config.auth),
                runtime: shared.runtime_auth.clone(),
                cache: redis.clone().map(|r| {
                    Arc::new(auth_config_api::RedisAuthConfigCache::connected(r))
                        as Arc<dyn auth_config_api::AuthConfigCache>
                }),
                audit: audit.clone(),
            },
        )),
        config_api_keys: Arc::new(config_api_keys_api::ConfigApiKeysApi::new(
            config_api_keys_api::ConfigApiKeysDependencies {
                identity: auth.admission.clone(),
                rbac: rbac.clone(),
                database: Some(key_db),
                runtime: shared.runtime_auth,
                access: shared.access.clone(),
                audit: audit.clone(),
                random: Arc::new(OsKeyEntropy),
                errors: Arc::new(config_api_keys_api::RedactedConfigApiKeysErrors),
            },
        )),
        connector_admin: Arc::new(connector_admin_api::ConnectorAdminApi::new(
            connector_admin_api::ConnectorAdminDependencies {
                identity: auth.admission.clone(),
                rbac: rbac.clone(),
                database: Some(connector_db),
                destination: backends.connector_destination,
                cipher: Arc::new(provider_cipher::NativeProviderCipher),
                encryption_key: config.connector_encryption_key,
            },
        )),
        connector_user: Arc::new(connector_user_api::ConnectorUserApi::new(
            connector_user_api::ConnectorUserDependencies {
                service: connectors.clone(),
                user_identity: auth.admission.clone(),
                principal_identity: auth.admission.clone(),
                rbac: rbac.clone(),
                principal_enabled: config.principal_enabled,
                principals: Some(shared.connectors),
                audit: Some(mcp.clone()),
            },
        )),
        mcp_registry: Arc::new(mcp_registry_api::McpRegistryApi::new(
            mcp_registry_api::McpRegistryDependencies {
                identity: auth.admission.clone(),
                rbac: rbac.clone(),
                enabled: config.mcp_enabled,
                repository: Some(mcp.clone()),
                audit: Some(mcp),
                discovery: Some(mcp_broker.clone()),
                destination: Arc::new(mcp_registry_api::NativeMcpDestinationValidator),
            },
        )),
        presign: Arc::new(presign_api::PresignApi::new(adapters.clone())),
        roles: Arc::new(roles_api::RolesApi::new(roles_api::RolesDependencies {
            identity: auth.admission.clone(),
            rbac: rbac.clone(),
            database: Some(roles_db),
            clock: Arc::new(roles_api::SystemRolesClock),
        })),
        self_service_api_keys: Arc::new(self_service_api_keys_api::SelfServiceKeysApi::new(
            self_service_api_keys_api::SelfServiceKeyDependencies {
                identity: auth.admission.clone(),
                service: Some(self_keys),
                invalidation: Arc::new(self_service_api_keys_api::GlobalSelfServiceKeyInvalidation),
            },
        )),
        service_config: Arc::new(service_config_api::ServiceConfigApi::new(
            service_config_api::ServiceConfigDependencies {
                identity: auth.admission.clone(),
                rbac: rbac.clone(),
                registry: core.registry.clone(),
                database: Some(backends.service_database),
                proxy_loader: backends.proxy_loader,
                capacity_resolver: core.capacity.clone(),
                admission: Some(core.admission.clone()),
                audit: audit.clone(),
                exception_text: backends.service_errors,
            },
        )),
        services: Arc::new(services_api::ServicesApi::new(
            services_api::ServicesDependencies {
                identity: adapters.clone(),
                rbac: rbac.clone(),
                registry: core.registry.clone(),
                access_resolver: shared.access,
                access_database: Some(access_db),
                providers: Some(models.providers.clone()),
                models: Some(models.models.clone()),
                runtime: backends.services_runtime,
                proxy_invalidation: backends.proxy_invalidation,
                transparent_proxy: backends.transparent_proxy,
                exception_text: backends.services_errors,
            },
        )),
        tenant_policies: Arc::new(tenant_policies_api::TenantPoliciesApi::new(
            tenant_policies_api::TenantPoliciesDependencies {
                identity: auth.admission.clone(),
                database: Some(tenant_db),
                tool_invalidation: backends.tool_invalidation,
                mcp_invalidation: backends.mcp_invalidation,
            },
        )),
        tool_inventory: Arc::new(tool_inventory_api::ToolInventoryApi::new(
            adapters.clone(),
            config.catalog,
        )),
        users: Arc::new(users_api::UsersApi::new(users_api::UsersDependencies {
            identity: auth.admission.clone(),
            profile_identity: auth.admission.clone(),
            rbac: rbac.clone(),
            database: Some(users_db.clone()),
            passwords: Arc::new(
                ai_platform_auth::PasswordEngine::new(config.password_concurrency)
                    .map_err(|_| failed("password_engine"))?,
            ),
            configuration: config.users,
            runtime: Arc::new(users_api::SystemUsersRuntimeValues),
            service_access: Arc::new(users_api::GlobalUsersServiceAccessInvalidation),
        })),
        balancer_config: Arc::new(balancer_config_api::BalancerConfigApi {
            identity: auth.admission.clone(),
            rbac: core.rbac.clone(),
            settings: Arc::new(config.balancer_strategy),
            runtime: shared.balancer.clone(),
            load_balancer: Some(shared.load_balancer),
            audit: Arc::new(audit.clone()),
        }),
        rate_config: Arc::new(rate_config_api::RateConfigApi::new(
            auth.admission.clone(),
            rbac.clone(),
            Some(rate_db.clone()),
            shared.runtime_rates.clone(),
            shared.rate_resolver,
            backends.rate_publication,
            audit.clone(),
        )),
        model_admin: Arc::new(model_admin_api::ModelAdminApi::new(
            adapters,
            Some(models.models.clone()),
            Arc::new(audit.clone()),
            backends.model_publisher,
        )),
        usage_admin: Arc::new(
            usage_admin_api::UsageAdminApi::new(
                auth.admission.clone(),
                usage.clone(),
                backends.usage_clock,
                rbac.clone(),
                config.usage_dimension_order.each_ref().map(String::as_str),
            )
            .map_err(|_| failed("usage_dimension_order"))?,
        ),
        system_status: Arc::new(system_status_api::SystemStatusApi {
            identity: auth.admission.clone(),
            rbac: core.rbac.clone(),
            settings: Arc::new(config.system),
            database: Some(system_status_api::SystemStatusDatabase {
                pool_present: true,
                rate_limits: Some(rate_db),
            }),
            redis: redis.map(|r| {
                Arc::new(system_status_api::NativeSystemStatusRedis::new(
                    true,
                    Some(r),
                )) as Arc<dyn system_status_api::SystemStatusRedis>
            }),
            runtime_rules: shared.runtime_rates,
            balancer: shared.balancer,
            admission: Some(core.admission.clone()),
            capacity_resolver: core.capacity.clone(),
        }),
        provider_admin: Arc::new(provider_admin_api::ProviderAdminApi::new(
            Some(models.providers.clone()),
            Some(Arc::new(
                provider_admin_api::NativeProviderAdminCatalog::new(
                    models.providers.clone(),
                    models.models.clone(),
                    Arc::new(catalog_http::ReqwestCatalogHttp::new()),
                ),
            )),
            Arc::new(provider_admin_api::NativeProviderAdminEffects::new(
                shared.probes.clone(),
                Arc::new(audit.clone()),
                backends.provider_publisher,
            )),
        ).with_review(Arc::new(ai_platform_gateway_foundation::model_catalog_review::CatalogReview::new(
            models.providers.clone(), Arc::new(catalog_http::ReqwestCatalogHttp::new()),
            Arc::new(ai_platform_gateway_foundation::model_catalog_review::postgres::PgReviewDatabase::new(pools.data.clone(), models.pricing.clone())),
        )))),
        quota_admin: Arc::new(quota_admin_api::QuotaAdminApi::new(
            quota,
            Some(quota_pool.clone()),
            Some(quota_pool),
            Arc::new(quota_admin_api::RbacQuotaAdminAuthorization::new(
                rbac.clone(),
                backends.quota_denied,
            )),
            Arc::new(audit),
        )),
        dashboard: Arc::new(dashboard_api::DashboardApi::new(
            dashboard_api::DashboardDependencies {
                realtime: shared.realtime,
                metrics: shared.metrics.clone(),
                usage: dashboard_api::DashboardUsageState {
                    recorder: Some(usage),
                    database_enabled: true,
                },
                redis: metrics_redis.clone(),
                clock: metrics_clock.clone(),
                rbac: rbac.clone(),
                denied: Some(backends.metrics_denied.clone()),
            },
        )),
        metrics_admin: Arc::new(metrics_admin_api::MetricsAdminApi::new(
            shared.metrics,
            metrics_redis,
            Some(Arc::new(
                metrics_admin_api::PgMetricsAdminDatabase::new(
                    pools.data.clone(),
                    pools.control.clone(),
                )
                .map_err(|_| failed("metrics_database"))?,
            )),
            metrics_clock,
            rbac.clone(),
            Some(backends.metrics_denied),
        )),
        provider_identity: auth.admission.clone(),
        quota_identity: auth.admission.clone(),
        metrics_identity: auth.admission.clone(),
        memory: assistant_memory_api::AssistantMemoryApi {
            store: Some(Arc::new(
                assistant_memory_store::PgAssistantMemoryStore::new(
                    pools.data.clone(),
                    pools.control.clone(),
                )
                .map_err(|_| failed("assistant_memory_store"))?,
            )),
        },
        memory_identity: content.clone(),
        artifact_shares: artifact_shares_api::ArtifactSharesApi {
            database: Some(shares.clone()),
            scope: Arc::new(artifact_share_scope::NativeArtifactShareScope {
                database: shares,
                accounts: Some(users_db),
                source_access: Some(history),
            }),
            random: Arc::new(quiz_entropy::OsQuizEntropy),
            clock: Arc::new(artifact_shares_api::ArtifactSharesSystemClock),
        },
        artifact_share_identity: content,
        local_nodes: local_nodes_api::LocalNodesApi {
            control_plane: Some(Arc::new(
                local_nodes_store::PgLocalNodesControlPlane::new(
                    pools.data.clone(),
                    pools.control.clone(),
                    Arc::new(local_nodes_types::LocalNodesOsRandom),
                    Arc::new(local_nodes_types::LocalNodesSystemClock),
                )
                .map_err(|_| failed("local_nodes_store"))?,
            )),
            channel_verifier: backends.local_channel,
        },
        local_node_identity: auth.admission.clone(),
        sessions: sessions_api::SessionsApi::new(
            auth.admission.clone(),
            core.sessions.clone(),
            Some(backends.sessions_cleanup),
        ),
        health: health_api::HealthHttpApi::new(
            health_api::HealthApi::new(
                core.registry.clone(),
                health_monitor.clone(),
                backends.health_dependencies.clone(),
                Some(shared.metadata),
                shared.probes,
            ),
            auth.admission.clone(),
            backends.health_cancellation.clone(),
        ),
        architecture: Arc::new(architecture_status_api::ArchitectureStatusApi {
            identity: auth.admission.clone(),
            rbac,
            runtime: backends.architecture_runtime,
            dependencies: backends.health_dependencies,
            monitor: Arc::new(architecture_status_api::NativeArchitectureMonitor(
                health_monitor,
            )),
            cancellation: backends.health_cancellation,
        }),
        tasks: core.tasks.clone(),
        dispatcher: core.dispatcher.clone(),
    };
    Ok(AssembledFoundation {
        mcp_broker,
        apis,
        connector_service: connectors,
    })
}
