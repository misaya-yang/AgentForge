//! Explicit process resource and middleware assembly for the Gateway binary.
//! Calling construct opens configured connections and performs source provider/
//! model catalog initialization. No schema
//! bootstrap, credential discovery, provider inference or listener starts here.
use super::{
    BootstrapError, DatabasePools,
    configuration::{GatewayDocument, project_rates},
    graph::{GatewayBackends, GatewayGraph},
    settings_projection::SettingsProjection,
    workers::GatewayWorkers,
};
use crate::migration_host::{
    GatewayHost,
    logging::LoggingState,
    metrics,
    middleware::DrainState,
    readiness::{ReadinessConfig, ReadinessState},
    resources::ConnectionResources,
    version::VersionConfig,
};
use ai_platform_migration_authority::application_guard::ApplicationPrincipal;
use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

impl GatewayDocument {
    pub async fn construct(mut self) -> Result<(GatewayHost, SocketAddr), BootstrapError> {
        super::workload::configure_outbound(&self.environment)?;
        let process_role = crate::migration_host::worker_roles::ProcessRole::parse(
            self.environment
                .get("GATEWAY_PROCESS_ROLE")
                .map(String::as_str)
                .unwrap_or("all"),
        )
        .map_err(BootstrapError::Configuration)?;
        let ha_configuration = super::ha::Configuration::from_environment(&self.environment)?;
        let private_worker = matches!(
            process_role,
            crate::migration_host::worker_roles::ProcessRole::Worker
                | crate::migration_host::worker_roles::ProcessRole::Eval
        );
        let workload = if ha_configuration.is_none()
            && matches!(
                process_role,
                crate::migration_host::worker_roles::ProcessRole::Worker
                    | crate::migration_host::worker_roles::ProcessRole::Eval
            ) {
            None
        } else {
            super::workload::configuration(&self.environment).await?
        };
        let sdk_callback_configuration = if matches!(
            process_role,
            crate::migration_host::worker_roles::ProcessRole::Worker
                | crate::migration_host::worker_roles::ProcessRole::Eval
        ) {
            None
        } else {
            crate::migration_host::sdk_callback_tls::Configuration::from_environment(
                &self.environment,
            )
            .map_err(BootstrapError::Configuration)?
        };
        self.environment
            .retain(|name, _| !name.starts_with("SDK_AGENT_GATEWAY_TLS_"));
        let local_node_channel_config = if matches!(
            process_role,
            crate::migration_host::worker_roles::ProcessRole::Worker
                | crate::migration_host::worker_roles::ProcessRole::Eval
        ) {
            None
        } else {
            crate::migration_host::local_node_channel::LocalNodeChannelConfig::from_environment(
                &self.environment,
            )
            .map_err(BootstrapError::Configuration)?
        };
        let local_node_channel_signer = local_node_channel_config
            .as_ref()
            .map(crate::migration_host::local_node_channel::PlatformSigner::from_config)
            .transpose()
            .map_err(BootstrapError::Configuration)?;
        self.environment
            .retain(|name, _| !name.starts_with("AI_PLATFORM_LOCAL_NODE_"));
        let settings = self.settings()?;
        process_role
            .validate_delivery(&settings.task_delivery_mode)
            .map_err(BootstrapError::Configuration)?;
        let mut config = self.project(&settings)?;
        let llm_configuration = super::llm_api::Configuration::from_environment(
            &self.environment,
            &config.auth.api_key_header,
        )?;
        let enterprise_configuration =
            super::enterprise_identity::Configuration::from_environment(&self.environment)?;
        config.auth.enterprise_policy_enabled = enterprise_configuration.is_some();
        let valkey = ai_platform_service_primitives::valkey::SplitConfig::from_values(
            &self.environment,
            ai_platform_service_primitives::valkey::ServiceIdentity::Gateway,
        )
        .map_err(|_| BootstrapError::Configuration("valkey_split"))?;
        config.content.langgraph.coordination_only = valkey.is_some();
        let knowledge_model_configuration =
            super::knowledge_models::Configuration::from_environment(&self.environment)?;
        config.auth.access_keys = super::auth::access_keys(&self.environment)?;
        if self.environment.contains_key("AI_PLATFORM_BFF_ORIGINS")
            && (config.auth.access_keys.is_none() || !config.auth.jwt_enabled)
        {
            return Err(BootstrapError::Configuration(
                "bff_requires_eddsa_and_enabled_auth",
            ));
        }
        self.environment
            .retain(|name, _| !name.starts_with("AI_PLATFORM_API_EDDSA_"));
        let policies = self.policies()?;
        let integration_environment = super::configuration::provider_environment(&self.environment);
        if !settings.database.enabled || (!settings.redis.enabled && valkey.is_none()) {
            return Err(BootstrapError::Configuration(
                "durable_profile_requires_database_and_redis",
            ));
        }
        let admission_principal = match settings.task_delivery_mode.as_str() {
            "memory" => ApplicationPrincipal::Gateway,
            "durable" | "schema_admitted" => ApplicationPrincipal::GatewayDurable,
            _ => return Err(BootstrapError::Configuration("task_delivery_mode")),
        };
        let address = SocketAddr::new(
            settings
                .host
                .parse()
                .map_err(|_| BootstrapError::Configuration("listen_host_ip"))?,
            settings.port,
        );
        if local_node_channel_config
            .as_ref()
            .is_some_and(|config| config.tls_listen_address.port() == address.port())
        {
            return Err(BootstrapError::Configuration("local_node_channel_tls_port"));
        }
        let roles: BTreeMap<_, _> = config.core.roles.clone().into_iter().collect();
        let catalog = config.foundation.catalog.clone();
        let kernel_revision = config.runtime.options.kernel_revision.clone();
        let runtime_url = config.runtime.options.runtime_url.clone();
        let capability_worker_url = config.runtime.capability_worker_url.clone();
        let knowledge_url = config.runtime.knowledge_origin.clone();
        if let Some(identity) = crate::migration_agent::control::execution_transport::WorkloadIdentity::from_environment()
            .map_err(BootstrapError::Configuration)? {
            for origin in [&runtime_url, &capability_worker_url, &knowledge_url] {
                identity.validate_endpoint(origin).map_err(BootstrapError::Configuration)?;
            }
        }

        // Resolve all required health-only origins before opening resources or initializing accounts.
        let readiness_runtime_url = super::workload::health_origin(
            &self.environment,
            "AI_PLATFORM_AGENT_RUNTIME_HEALTH_ORIGIN",
            Some(&runtime_url),
            crate::migration_agent::control::execution_transport::required(),
        )?;
        let readiness_worker_url = super::workload::health_origin(
            &self.environment,
            "AI_PLATFORM_CAPABILITY_WORKER_HEALTH_ORIGIN",
            Some(&capability_worker_url),
            crate::migration_agent::control::execution_transport::required(),
        )?;
        let readiness_knowledge_url = super::workload::health_origin(
            &self.environment,
            "KNOWLEDGE_SERVICE_HEALTH_ORIGIN",
            Some(&knowledge_url),
            crate::migration_agent::control::execution_transport::required(),
        )?;
        let readiness_knowledge_worker_url = super::workload::health_origin(
            &self.environment,
            "KNOWLEDGE_WORKER_HEALTH_ORIGIN",
            None,
            crate::migration_agent::control::execution_transport::required(),
        )?;

        let database_files = super::data_connections::configuration(&self.environment)?;
        let resources = Arc::new(
            ConnectionResources::connect_split_with_database_files(
                &settings.database,
                &settings.redis,
                valkey.as_ref(),
                database_files
                    .as_ref()
                    .map(|(data, control)| (data, control)),
            )
            .await
            .map_err(|_| BootstrapError::Construction("connections"))?,
        );
        let pools = DatabasePools {
            data: resources
                .database
                .clone()
                .ok_or(BootstrapError::Construction("database"))?,
            control: resources
                .independent_control
                .clone()
                .ok_or(BootstrapError::Construction("control_database"))?,
        };
        let local_node_channel_repository = local_node_channel_config.as_ref().map(|_| {
            Arc::new(
                crate::migration_host::local_node_channel_store::PostgresLocalNodeChannelRepository::new(
                    pools.data.clone(),
                ),
            ) as Arc<dyn crate::migration_host::local_node_channel::LocalNodeChannelRepository>
        });
        // Admission precedes catalog writes, route construction and worker activation.
        // The application's only schema read is the owner-installed EXECUTE-only
        // metadata function; migrations remain operator-owned.
        {
            let mut connection = pools
                .data
                .acquire()
                .await
                .map_err(|_| BootstrapError::Construction("schema_admission_connection"))?;
            ai_platform_migration_authority::application_guard::require_compatible_schema(
                &mut connection,
                admission_principal,
            )
            .await
            .map_err(|_| BootstrapError::Construction("schema_admission"))?;
        }
        config.core.durable_tasks =
            ai_platform_gateway_foundation::task_rollout::DeliveryMode::parse(
                &settings.task_delivery_mode,
            )
            .and_then(|mode| mode.after_canonical_guard(Ok(()), config.core.worker_concurrency))
            .map_err(BootstrapError::Configuration)?;
        if let Some(repository) = &local_node_channel_repository {
            let ready = repository
                .schema_ready()
                .await
                .map_err(|_| BootstrapError::Construction("local_node_channel_schema"))?;
            if !ready {
                return Err(BootstrapError::Construction("local_node_channel_schema"));
            }
        }
        initialize_bootstrap_admin_password(
            &pools.data,
            self.environment
                .get("DEFAULT_USER_PASSWORD")
                .map(String::as_str),
            config.auth.password_concurrency,
        )
        .await?;
        let redis = resources
            .redis
            .clone()
            .ok_or(BootstrapError::Construction("redis"))?;
        let usage_coordination = resources.coordination.clone();
        let enterprise_coordination = redis.clone();
        let drain = Arc::new(DrainState::default());
        let ha = match ha_configuration {
            Some(configuration) => Some(
                super::ha::Runtime::new(
                    configuration,
                    resources.coordination.as_ref(),
                    &self.environment,
                    workload.as_ref().map(|(configuration, _)| configuration),
                    drain.clone(),
                )
                .await?,
            ),
            None => None,
        };
        let readiness = Arc::new(
            ReadinessState::new(
                resources,
                ReadinessConfig {
                    ha_lease: ha.as_ref().map(|h| h.health()),
                    auth_config_ready: settings.auth_ready(),
                    model_plane_ready: false,
                    runtime_url: readiness_runtime_url,
                    capability_worker_url: readiness_worker_url,
                    knowledge_service_url: readiness_knowledge_url,
                    knowledge_worker_url: readiness_knowledge_worker_url,
                    ..Default::default()
                },
                drain.clone(),
            )
            .map_err(|_| BootstrapError::Construction("readiness"))?,
        );
        let secrets = Arc::new(
            crate::mcp_gateway_broker::ConfiguredEnvironmentSecretResolver::from_json(
                self.environment
                    .get("MCP_SECRET_REF_MAP")
                    .map(String::as_str)
                    .unwrap_or(""),
                Arc::new(super::models::ExplicitEnvironment(
                    integration_environment.clone(),
                )),
            ),
        );
        let mut foundation = policies.foundation_policies(readiness.clone(), secrets);
        let publisher = Arc::new(super::runtime_backends::RedisConfigurationPublisher::new(
            redis.clone(),
        ));
        if policies.model_publication == super::native_policies::PublisherMode::Required {
            foundation.model_publisher = Some(publisher.clone());
        }
        if policies.provider_publication == super::native_policies::PublisherMode::Required {
            foundation.provider_publisher = Some(publisher);
        }
        let usage_bus = super::usage_event_bus::UsageEventBus::configured(
            &self.environment,
            usage_coordination,
            format!("gateway-{}", std::process::id()),
        )?;
        let backends = GatewayBackends {
            foundation,
            usage_publisher: usage_bus.as_ref().map(|bus| {
                bus.publisher.clone() as Arc<dyn crate::usage_recorder::UsageEventPublisher>
            }),
            local_node: None,
            cleanup_memory: Some(super::runtime_backends::memory_cleanup(
                &pools,
                &self.environment,
            )?),
        };
        let authorities = self.authorities(redis.clone())?;
        let models = super::models::ModelServices::native(
            &pools,
            config.provider_encryption_key.clone(),
            config.provider_environment.clone(),
            config.pricing_cache_ttl_seconds,
            config.google_auth.clone(),
        )?;
        let startup = super::startup_models::StartupModelConfiguration::from_settings(&settings)?;
        let seeded =
            super::startup_models::initialize(&models, &startup, &integration_environment).await;
        config.runtime_configured_providers =
            seeded.runtime_configured_providers.into_iter().collect();
        let runtime_providers = config.runtime_configured_providers.clone();
        let graph = GatewayGraph::from_model_services_with_ha(
            pools.clone(),
            redis,
            config,
            backends,
            authorities,
            models,
            ha.clone(),
        )
        .await?;
        let browser_auth = self
            .environment
            .get("AI_PLATFORM_BFF_ORIGINS")
            .map(|raw| {
                serde_json::from_str::<Vec<String>>(raw)
                    .map_err(|_| BootstrapError::Configuration("bff_origins"))
                    .and_then(|origins| {
                        ai_platform_auth::browser::BrowserAuth::new(graph.auth.clone(), origins)
                            .map_err(|_| BootstrapError::Configuration("bff_origins"))
                    })
            })
            .transpose()?;
        let knowledge_model_router = knowledge_model_configuration
            .map(|configuration| configuration.router(&graph))
            .transpose()?;
        let llm_router = llm_configuration
            .map(|configuration| configuration.router(&graph))
            .transpose()?;
        let enterprise_router = match enterprise_configuration {
            Some(configuration) => Some(
                configuration
                    .router(graph.auth.clone(), enterprise_coordination)
                    .await?,
            ),
            None => None,
        };
        let local_node_channel = if let Some(configuration) = local_node_channel_config {
            let signer = local_node_channel_signer
                .ok_or(BootstrapError::Configuration("local_node_channel_signer"))?;
            let repository = local_node_channel_repository
                .ok_or(BootstrapError::Construction("local_node_channel_schema"))?;
            let state = crate::migration_host::local_node_channel::LocalNodeChannelState::new(
                repository,
                signer,
                graph.compat.local_node.authorization.clone(),
            )
            .await
            .map_err(BootstrapError::Construction)?
            .ok_or(BootstrapError::Construction("local_node_channel_schema"))?;
            let state = Arc::new(state);
            Some((
                crate::migration_host::local_node_channel::local_node_worker_router(Some(
                    state.clone(),
                )),
                crate::migration_host::local_node_channel::local_node_companion_router(Some(state)),
                crate::migration_host::local_node_tls::GatewayHttpsConfiguration {
                    address: configuration.tls_listen_address,
                    certificate_path: configuration.tls_certificate_path,
                    private_key_path: configuration.tls_private_key_path,
                },
            ))
        } else {
            None
        };
        // A successfully constructed real model plane owns the validated lease
        // signer and HTTP/model authorities. External Runtime readiness is probed.
        readiness
            .config
            .write()
            .map_err(|_| BootstrapError::Construction("readiness_state"))?
            .model_plane_ready = true;
        let workers = GatewayWorkers::new(
            &pools,
            crate::billing_scheduler::UsageSchedulerConfig::default(),
        )?;
        let (extra_start, extra_stop) = usage_bus
            .as_ref()
            .map(|bus| bus.lifecycle_steps())
            .unwrap_or_default();
        let (lifecycle, worker_health) = graph
            .lifecycle(workers, extra_start, extra_stop, process_role)
            .map_err(|_| BootstrapError::Construction("worker_role_lifecycle"))?;
        {
            let mut config = readiness
                .config
                .write()
                .map_err(|_| BootstrapError::Construction("readiness_state"))?;
            config.process_role = Some(process_role);
            config.worker_health = Some(worker_health);
            config.authorization_audit = graph.core.authorization_audit.clone();
            config.authorization_enforced = graph
                .core
                .rbac
                .decision_point
                .as_ref()
                .is_some_and(|point| point.mode() == ai_platform_auth::cedar::Mode::Enforce);
        }
        ai_platform_gateway_foundation::worker_lease::enforce();
        let collector = metrics::global();
        let logging = Arc::new(LoggingState::new(
            graph.foundation_shared.metrics.clone(),
            collector.clone(),
        ));
        let metric_router = crate::migration_host::metrics_http::router(
            crate::migration_host::metrics_http::MetricsHttpState {
                identity: graph.admission.clone(),
                rbac: graph.core.rbac.clone(),
                collector,
            },
        );
        let (_, rates) = project_rates(&SettingsProjection {
            settings: &settings,
            environment: &self.environment,
        });
        let rate = crate::migration_host::rate::RateState {
            config: Arc::new(rates),
            limiter: graph.limiter.clone(),
        };
        let options = crate::GatewayOptions {
            identity: None, // GatewayGraph injects the shared native user authority.
            runtime: crate::config::RuntimeReadConfig {
                default_model: settings.default_model.clone(),
                runtime_configured_providers: runtime_providers.into_iter().collect(),
                kb_enabled: true,
                installed_catalog: None,
            },
            sessions: crate::session_history::SessionConfig {
                default_ttl_seconds: 7 * 86400,
                kernel_revision: Some(kernel_revision),
                source_access: Some(graph.history_knowledge.clone()),
                cleanup: None, // GatewayGraph injects the shared durable owner.
            },
            catalog: crate::assistant_catalog::AssistantCatalogOptions {
                catalog,
                policy_settings: crate::assistant_catalog::PolicySettings::from_lookup(|name| {
                    self.environment.get(name).cloned()
                }),
            },
            knowledge: Some(graph.history_knowledge.clone()),
        };
        let browser_telemetry = crate::browser_telemetry::BrowserTelemetryApi::new(
            graph.content.content.public.access.clone(),
            self.environment
                .get("AI_PLATFORM_BROWSER_TELEMETRY_TENANTS")
                .map(|value| serde_json::from_str::<Vec<String>>(value))
                .transpose()
                .map_err(|_| BootstrapError::Configuration("browser_telemetry_tenants"))?
                .unwrap_or_default(),
            self.environment
                .get("AI_PLATFORM_BROWSER_TELEMETRY_ORIGINS")
                .map(|value| serde_json::from_str::<Vec<String>>(value))
                .transpose()
                .map_err(|_| BootstrapError::Configuration("browser_telemetry_origins"))?
                .unwrap_or_default(),
            self.environment
                .get("AI_PLATFORM_BROWSER_TELEMETRY_SAMPLE_BPS")
                .map(|value| value.parse::<u16>())
                .transpose()
                .map_err(|_| BootstrapError::Configuration("browser_telemetry_sampling"))?
                .unwrap_or(0),
        )
        .map_err(|_| BootstrapError::Configuration("browser_telemetry"))?;
        let mut host_identity = settings.identity(&self.environment);
        let identity_auth = graph.auth.clone();
        host_identity.verify_jwt = Some(Arc::new(move |token| {
            identity_auth
                .verify_access_claims(token)
                .and_then(|claims| {
                    serde_json::to_value(claims)
                        .map_err(|_| ai_platform_auth::service::ServiceError::Unavailable)
                })
                .map_err(|e| e.to_string())
        }));
        let router = graph
            .into_router(
                options,
                roles,
                policies.cancellation_hooks(),
                crate::balancer_config_api::BalancerConfigContentTypePolicy::RequireJsonHeader,
            )?
            .merge(crate::migration_host::readiness::router(readiness))
            .merge(metric_router)
            .merge(crate::browser_telemetry::router(browser_telemetry))
            .merge(crate::migration_host::release::router(Arc::new(
                crate::migration_host::release::ReleaseIdentity::gateway(&self.environment),
            )));
        let router = if let Some(models) = knowledge_model_router {
            router.merge(models)
        } else {
            router
        };
        let router = if let Some(llm) = llm_router {
            router.merge(llm)
        } else {
            router
        };
        let router = if let Some(enterprise) = enterprise_router {
            router.merge(enterprise)
        } else {
            router
        };
        let (router, local_node_https) = match local_node_channel {
            Some((worker_router, companion_router, tls_configuration)) => (
                router.merge(worker_router),
                Some((companion_router, tls_configuration)),
            ),
            None => (router, None),
        };
        let router = if let Some(browser) = browser_auth.as_ref() {
            router.merge(browser.router())
        } else {
            router
        };
        let router = if let Some(ha) = &ha {
            ha.routing().protect(router)
        } else {
            router
        };
        let mut host = GatewayHost::assembled_with_drain(
            router,
            settings.middleware(),
            host_identity,
            settings
                .cors()
                .map_err(|_| BootstrapError::Configuration("cors"))?,
            VersionConfig::default(),
            rate,
            logging,
            lifecycle,
            drain,
        );
        if let Some(browser) = browser_auth {
            host.router = browser.bridge(host.router);
        }
        let host = if let Some((companion_router, tls_configuration)) = local_node_https {
            host.with_local_node_https(companion_router, tls_configuration)
        } else {
            host
        };
        let host = if let Some(configuration) = sdk_callback_configuration {
            let configuration = configuration
                .resolve()
                .await
                .map_err(BootstrapError::Configuration)?;
            if configuration.address.port() == address.port() {
                return Err(BootstrapError::Configuration(
                    "sdk_callback_tls_port_collision",
                ));
            }
            host.with_sdk_callback_https(configuration)
        } else {
            host
        };
        let mut host = if let Some((configuration, gate)) = workload {
            if configuration.address.port() == address.port() {
                return Err(BootstrapError::Configuration("workload_tls_port_collision"));
            }
            host.with_workload_https(
                configuration,
                gate,
                if private_worker {
                    super::ha::worker_routes()?
                } else {
                    super::workload::routes()?
                },
            )
            .map_err(BootstrapError::Configuration)?
        } else {
            host
        };
        if matches!(
            process_role,
            crate::migration_host::worker_roles::ProcessRole::Worker
                | crate::migration_host::worker_roles::ProcessRole::Eval
        ) {
            crate::migration_host::worker_roles::restrict_worker(&mut host);
        }
        Ok((host, address))
    }
}

/// Canonical update-only password bootstrap; installation owns the admin row.
/// Never create an account, replace an existing hash, or log credential material.
async fn initialize_bootstrap_admin_password(
    pool: &sqlx::PgPool,
    password: Option<&str>,
    password_concurrency: usize,
) -> Result<(), BootstrapError> {
    let Some(password) = password.filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    // Avoid hashing on later starts. The UPDATE repeats this predicate so a
    // concurrent bootstrap or password change can never be overwritten.
    let missing: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM gateway.users WHERE user_id = 'admin' AND (password_hash IS NULL OR password_hash = ''))",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| BootstrapError::Construction("bootstrap_admin_password_lookup"))?;
    if !missing {
        return Ok(());
    }
    let passwords = ai_platform_auth::PasswordEngine::new(password_concurrency)
        .map_err(|_| BootstrapError::Configuration("password_concurrency"))?;
    let hash = passwords
        .hash(password.to_owned())
        .await
        .map_err(|_| BootstrapError::Construction("bootstrap_admin_password_hash"))?;
    sqlx::query(
        "UPDATE gateway.users SET password_hash = $1, updated_at = NOW() WHERE user_id = 'admin' AND (password_hash IS NULL OR password_hash = '')",
    )
    .bind(hash)
    .execute(pool)
    .await
    .map_err(|_| BootstrapError::Construction("bootstrap_admin_password_update"))?;
    Ok(())
}
