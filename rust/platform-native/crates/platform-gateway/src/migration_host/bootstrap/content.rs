//! Content and LangGraph constructors sharing the gateway's authority and usage.
use super::{BootstrapError, DatabasePools, core::CoreServices, runtime::RuntimeAuthorities};
use crate::{
    guest_session_manager::{GuestSessionConfig, GuestSessionManager, GuestSessionStorage},
    guest_session_redis::RedisGuestSessionStorage,
    migration_compat::langgraph::{LangGraphClient, LangGraphGovernance, LangGraphInstance},
    migration_content::{
        ContentDatabase,
        attachments::PdfAttachmentParser,
        capabilities::{CapabilityAuthorization, HttpSandboxTransport},
        image_native::NativeImageRasterBackend,
        read_broker::TavilySearch,
    },
    migration_host::{
        content::{AssembledContent, ContentConfiguration, ContentDependencies},
        observability::ObservabilityServices,
    },
    rate_limits::{MultiDimensionConfig, SlidingWindowRateLimiter},
};
use redis::aio::ConnectionManager;
use std::{sync::Arc, time::Duration};
pub struct LangGraphEndpoint {
    pub id: String,
    pub url: String,
    pub weight: f64,
}
pub struct LangGraphConfiguration {
    pub instances: Vec<LangGraphEndpoint>,
    pub strategy: String,
    pub token: String,
    pub governance: LangGraphGovernance,
    pub coordination_only: bool,
}
pub struct ContentBackends {
    pub shared_coordination: Option<ConnectionManager>,
    /// Must implement the source PDF text extraction, never a fake empty parse.
    pub pdf: Arc<dyn PdfAttachmentParser>,
}
pub struct ContentRuntimeConfiguration {
    pub artifacts: super::storage::ObjectStorageConfiguration,
    pub uploads: super::storage::ObjectStorageConfiguration,
    pub attachment_fallbacks: Vec<super::storage::ObjectStorageConfiguration>,
    pub content: ContentConfiguration,
    pub langgraph: LangGraphConfiguration,
    pub guests: GuestSessionConfig,
    pub sandbox_url: String,
    pub sandbox_token: String,
    pub capability_internal_token: String,
    pub capability_proof_secret: Vec<u8>,
    pub tavily_api_key: String,
}
pub struct ContentGraph {
    pub content: AssembledContent,
    pub langgraph: Arc<LangGraphClient>,
    pub guests: Arc<GuestSessionManager>,
    pub database: ContentDatabase,
}
impl ContentGraph {
    pub fn new(
        pools: &DatabasePools,
        redis: Option<ConnectionManager>,
        core: &CoreServices,
        authorities: &RuntimeAuthorities,
        observability: &ObservabilityServices,
        limiter: Arc<SlidingWindowRateLimiter>,
        rate_config: MultiDimensionConfig,
        config: ContentRuntimeConfiguration,
        backends: ContentBackends,
        knowledge: Arc<crate::knowledge::KnowledgeService>,
    ) -> Result<Self, BootstrapError> {
        let mut instances = Vec::new();
        for configured in config.langgraph.instances {
            super::endpoint(&configured.url, "langgraph_instance")?;
            if !configured.weight.is_finite() || configured.weight <= 0.0 {
                return Err(BootstrapError::Configuration("langgraph_weight"));
            }
            let mut instance = LangGraphInstance::new(configured.id, configured.url);
            instance.weight = configured.weight;
            instances.push(Arc::new(instance));
        }
        let mut langgraph = LangGraphClient::new(
            instances,
            config.langgraph.token,
            redis.clone(),
            config.langgraph.governance,
        )
        .map_err(|_| BootstrapError::Construction("langgraph"))?;
        if config.langgraph.coordination_only {
            langgraph = langgraph.with_coordination_only();
        }
        langgraph.strategy = config.langgraph.strategy;
        langgraph.usage = Some(observability.usage.clone());
        langgraph.pricing = Some(core.pricing.clone());
        langgraph.traces = Some(observability.traces.clone());
        langgraph.limiter = Some(limiter);
        langgraph.rate_config = rate_config;
        let langgraph = Arc::new(langgraph);
        let guests = Arc::new(if let Some(coordination) = backends.shared_coordination {
            GuestSessionManager::shared(
                config.guests,
                Arc::new(
                    ai_platform_gateway_foundation::multi_instance::guest::AtomicGuestStorage::new(
                        coordination,
                    ),
                ),
            )
            .map_err(|_| BootstrapError::Construction("ha_guest_cas"))?
        } else {
            let guest_storage = redis.map(|r| {
                Arc::new(RedisGuestSessionStorage::canonical_storage(r))
                    as Arc<dyn GuestSessionStorage>
            });
            GuestSessionManager::new(config.guests, guest_storage)
        });
        let database = ContentDatabase::new(pools.data.clone(), pools.control.clone())
            .map_err(|_| BootstrapError::Construction("content_database"))?;
        let sandbox = if config.sandbox_url.is_empty() && config.sandbox_token.is_empty() {
            None
        } else {
            super::endpoint(&config.sandbox_url, "sandbox_url")?;
            let client = reqwest::Client::builder()
                .retry(reqwest::retry::never())
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(120))
                .build()
                .map_err(|_| BootstrapError::Construction("sandbox_client"))?;
            Some(Arc::new(
                HttpSandboxTransport::new(client, &config.sandbox_url, config.sandbox_token)
                    .map_err(|_| BootstrapError::Configuration("sandbox"))?,
            )
                as Arc<
                    dyn crate::migration_content::capabilities::SandboxTransport,
                >)
        };
        let search = Arc::new(
            TavilySearch::new(config.tavily_api_key)
                .map_err(|_| BootstrapError::Construction("search"))?,
        );
        let authorization = CapabilityAuthorization::new(
            config.capability_internal_token,
            config.capability_proof_secret,
        );
        let content = crate::migration_host::content::assemble(
            config.content,
            ContentDependencies {
                database: database.clone(),
                access: authorities.access.clone(),
                artifacts_backend: config.artifacts.build()?,
                uploads_backend: config.uploads.build()?,
                attachment_fallbacks: config
                    .attachment_fallbacks
                    .into_iter()
                    .map(|v| v.build())
                    .collect::<Result<Vec<_>, _>>()?,
                share_sources: Arc::new(crate::artifact_share_scope::NativeArtifactShareScope {
                    database: Arc::new(
                        crate::artifact_shares_store::PgArtifactSharesDatabase::new(
                            pools.data.clone(),
                            pools.control.clone(),
                        )
                        .map_err(|_| BootstrapError::Construction("share_sources"))?,
                    ),
                    accounts: Some(Arc::new(
                        crate::users_store::PgUsersDatabase::new(
                            pools.data.clone(),
                            pools.control.clone(),
                            Arc::new(crate::users_store::FreshUserPermissions),
                        )
                        .map_err(|_| BootstrapError::Construction("share_accounts"))?,
                    )),
                    source_access: Some(knowledge),
                }),
                langgraph: langgraph.clone(),
                guests: guests.clone(),
                authorization,
                sandbox,
                pdf: backends.pdf,
                raster: Arc::new(NativeImageRasterBackend::default()),
                search,
            },
        )
        .map_err(|_| BootstrapError::Construction("content"))?;
        Ok(Self {
            content,
            langgraph,
            guests,
            database,
        })
    }
}
