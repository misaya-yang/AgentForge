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
    settings::DatabaseApiKeyVerifierAvailability,
    version::VersionConfig,
};
use ai_platform_migration_authority::application_guard::ApplicationPrincipal;
use std::{collections::BTreeMap, net::SocketAddr, sync::Arc};

impl GatewayDocument {
    pub async fn construct(mut self) -> Result<(GatewayHost, SocketAddr), BootstrapError> {
        let scrape_token = match self.environment.remove("METRICS_SCRAPE_TOKEN") {
            Some(token)
                if token.len() >= 32 && token.bytes().all(|byte| byte.is_ascii_graphic()) =>
            {
                Some(Arc::new(token))
            }
            Some(_) => return Err(BootstrapError::Configuration("metrics_scrape_token")),
            None => None,
        };
        let local_node_channel_config =
            crate::migration_host::local_node_channel::LocalNodeChannelConfig::from_environment(
                &self.environment,
            )
            .map_err(BootstrapError::Configuration)?;
        let local_node_channel_signer = local_node_channel_config
            .as_ref()
            .map(crate::migration_host::local_node_channel::PlatformSigner::from_config)
            .transpose()
            .map_err(BootstrapError::Configuration)?;
        self.environment
            .retain(|name, _| !name.starts_with("AI_PLATFORM_LOCAL_NODE_"));
        let settings = self.settings()?;
        let mut config = self.project(&settings)?;
        let policies = self.policies()?;
        let integration_environment = super::configuration::provider_environment(&self.environment);
        if !settings.database.enabled || !settings.redis.enabled {
            return Err(BootstrapError::Configuration(
                "durable_profile_requires_database_and_redis",
            ));
        }
        let delivery_principal = match settings.task_delivery_mode.as_str() {
            "memory" => ApplicationPrincipal::Gateway,
            "durable" => ApplicationPrincipal::GatewayDurable,
            _ => return Err(BootstrapError::Configuration("task_delivery_mode")),
        };
        let admission_principal = if local_node_channel_config.is_some() {
            ApplicationPrincipal::GatewayLocalNode
        } else {
            delivery_principal
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

        let resources = Arc::new(
            ConnectionResources::connect(&settings.database, &settings.redis)
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
        let database_api_key_verifier = if settings.authentication.api_key.enabled
            && !pools.data.is_closed()
        {
            DatabaseApiKeyVerifierAvailability::Available
        } else {
            DatabaseApiKeyVerifierAvailability::Unavailable
        };
        let drain = Arc::new(DrainState::default());
        let readiness = Arc::new(
            ReadinessState::new(
                resources,
                ReadinessConfig {
                    auth_config_ready: settings
                        .auth_ready_with_database_api_key_verifier(database_api_key_verifier),
                    model_plane_ready: false,
                    runtime_url: Some(runtime_url),
                    capability_worker_url: Some(capability_worker_url),
                    knowledge_service_url: Some(knowledge_url),
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
        let usage_bus = self
            .environment
            .get("EVENT_BUS_REDIS_URL")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(|url| {
                super::usage_event_bus::UsageEventBus::new(
                    url,
                    format!("gateway-{}", std::process::id()),
                )
            })
            .transpose()?;
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
        super::models::validate_stored_credentials(&pools, &config.provider_encryption_key).await?;
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
        let graph = GatewayGraph::from_model_services(
            pools.clone(),
            redis,
            config,
            backends,
            authorities,
            models,
        )
        .await?;
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
        let lifecycle = graph.lifecycle(workers, extra_start, extra_stop);
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
                scrape_token,
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
        let router = graph
            .into_router(
                options,
                roles,
                policies.cancellation_hooks(),
                crate::balancer_config_api::BalancerConfigContentTypePolicy::RequireJsonHeader,
            )?
            .merge(crate::migration_host::readiness::router(readiness))
            .merge(metric_router)
            .merge(crate::migration_host::release::router(Arc::new(
                crate::migration_host::release::ReleaseIdentity::gateway(&self.environment),
            )));
        let (router, local_node_https) = match local_node_channel {
            Some((worker_router, companion_router, tls_configuration)) => (
                router.merge(worker_router),
                Some((companion_router, tls_configuration)),
            ),
            None => (router, None),
        };
        let host = GatewayHost::assembled_with_drain(
            router,
            settings.middleware(),
            settings.identity(&self.environment),
            settings
                .cors()
                .map_err(|_| BootstrapError::Configuration("cors"))?,
            VersionConfig::default(),
            rate,
            logging,
            lifecycle,
            drain,
        );
        let host = if let Some((companion_router, tls_configuration)) = local_node_https {
            host.with_local_node_https(companion_router, tls_configuration)
        } else {
            host
        };
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
