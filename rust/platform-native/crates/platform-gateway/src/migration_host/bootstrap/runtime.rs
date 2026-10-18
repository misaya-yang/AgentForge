//! Gateway-owned Agent wiring. The registered runtime retains the Agent loop;
//! Knowledge authorization is signed service-to-service HTTP, never local KB SQL.
use super::{
    BootstrapError, DatabasePools, auth::AuthServices, core::CoreServices, models::ModelServices,
};
use crate::{
    assistant_catalog::CapabilityCatalog,
    client_ip::TrustedProxies,
    connector_principal_store::ConnectorPrincipalRepository,
    mcp_registry_store::PgMcpDatabase,
    migration_agent::{
        control::{RuntimeControlPlane, RuntimeOptions},
        deletion::AgentDataCleanup,
        entry::PgSourceAccess,
        model::{AgentModelPlane, LeaseSigner},
        public::PublicRuntime,
        resources::NativeRuntimeResources,
        routes::AgentState,
        studio::AgentRepository,
        threads::AgentThreadStore,
    },
    migration_content::access::NativeContentAccess,
    migration_host::{
        catalog::{CatalogService, http::CatalogHttpState},
        content::AssembledContent,
        memory::PgRuntimeMemory,
        observability::ObservabilityServices,
        studio::NativeStudioResources,
    },
    migration_knowledge_query::{
        actor::{CurrentActorStore, HttpAgentRuntimeKnowledge, HttpKnowledgeSourceAuthority},
        auth::GatewayAuth,
    },
    rate_limits::{MultiDimensionConfig, SlidingWindowRateLimiter},
};
use std::sync::Arc;

pub struct RuntimeConfiguration {
    pub options: RuntimeOptions,
    pub lease_secret: String,
    pub model_plane_internal_token: String,
    pub capability_worker_url: String,
    pub knowledge_origin: String,
    pub default_model: String,
    pub web_search_configured: bool,
    pub mcp_enabled: bool,
    pub management_enabled: bool,
    pub publish_enabled: bool,
    pub runtime_enabled: bool,
}
pub struct RuntimeAuthorities {
    pub access: Arc<NativeContentAccess>,
    pub sources: Arc<PgSourceAccess>,
    pub knowledge: Arc<HttpKnowledgeSourceAuthority>,
    pub runtime_knowledge: Arc<HttpAgentRuntimeKnowledge>,
}
impl RuntimeAuthorities {
    pub fn new(
        pools: &DatabasePools,
        auth: &AuthServices,
        core: &CoreServices,
        models: &ModelServices,
        knowledge_origin: &str,
        signer: Arc<GatewayAuth>,
        limiter: Arc<SlidingWindowRateLimiter>,
        rate_config: Arc<MultiDimensionConfig>,
        trusted_proxies: TrustedProxies,
    ) -> Result<Self, BootstrapError> {
        let origin = super::endpoint(knowledge_origin, "knowledge_origin")?;
        let knowledge = Arc::new(
            HttpKnowledgeSourceAuthority::new(
                CurrentActorStore {
                    pool: pools.data.clone(),
                },
                origin.clone(),
                signer.clone(),
            )
            .map_err(|_| BootstrapError::Configuration("knowledge_signer"))?,
        );
        let runtime_knowledge = Arc::new(
            HttpAgentRuntimeKnowledge::new(origin, signer)
                .map_err(|_| BootstrapError::Configuration("runtime_knowledge_signer"))?,
        );
        let sources = Arc::new(PgSourceAccess {
            pool: pools.data.clone(),
            authority: knowledge.clone(),
        });
        let access = Arc::new(NativeContentAccess {
            identities: auth.identities.clone(),
            rbac: core.rbac.clone(),
            threads: AgentThreadStore::new(pools.data.clone()),
            sources: sources.clone(),
            models: models.models.clone(),
            limiter: Some(limiter),
            rate_config,
            trusted_proxies,
        });
        Ok(Self {
            access,
            sources,
            knowledge,
            runtime_knowledge,
        })
    }
}
pub struct RuntimeServices {
    pub agent: AgentState,
    pub model_plane: Arc<AgentModelPlane>,
    pub catalog: CatalogHttpState,
    pub repository: Arc<AgentRepository>,
    pub control: Arc<RuntimeControlPlane>,
    pub memory: Arc<PgRuntimeMemory>,
    pub connectors: Arc<ConnectorPrincipalRepository>,
    pub mcp_database: Arc<PgMcpDatabase>,
}
impl RuntimeServices {
    /// Compatibility constructor for callers using the bundled builtin catalog.
    pub fn new(
        pools: &DatabasePools,
        auth: &AuthServices,
        core: &CoreServices,
        models: &ModelServices,
        authorities: &RuntimeAuthorities,
        content: &AssembledContent,
        observability: &ObservabilityServices,
        config: RuntimeConfiguration,
        public: Option<Arc<PublicRuntime>>,
        cleanup_memory: Option<Arc<crate::migration_agent::memory_cleanup::RuntimeMemoryCleanup>>,
    ) -> Result<Self, BootstrapError> {
        Self::new_with_catalog(
            pools,
            auth,
            core,
            models,
            authorities,
            content,
            observability,
            config,
            Arc::new(
                CapabilityCatalog::bundled()
                    .map_err(|_| BootstrapError::Construction("capability_catalog"))?,
            ),
            public,
            cleanup_memory,
        )
    }

    pub fn new_with_catalog(
        pools: &DatabasePools,
        auth: &AuthServices,
        core: &CoreServices,
        models: &ModelServices,
        authorities: &RuntimeAuthorities,
        content: &AssembledContent,
        observability: &ObservabilityServices,
        config: RuntimeConfiguration,
        records: Arc<CapabilityCatalog>,
        public: Option<Arc<PublicRuntime>>,
        cleanup_memory: Option<Arc<crate::migration_agent::memory_cleanup::RuntimeMemoryCleanup>>,
    ) -> Result<Self, BootstrapError> {
        if crate::migration_agent::control::execution_transport::required()
            && !config.model_plane_internal_token.is_empty()
        {
            return Err(BootstrapError::Configuration(
                "workload_shared_bearer_forbidden",
            ));
        }
        super::endpoint(&config.options.runtime_url, "agent_runtime_url")?;
        let model_plane =
            super::endpoint(&config.options.model_plane_base_url, "model_plane_base_url")?;
        if crate::migration_agent::control::execution_transport::required()
            && (model_plane.scheme() != "https"
                || model_plane.path() != "/internal/v1/agent-model-plane")
        {
            return Err(BootstrapError::Configuration("model_plane_workload_origin"));
        }
        super::endpoint(&config.capability_worker_url, "capability_worker_url")?;
        if config.model_plane_internal_token.is_empty()
            && !crate::migration_agent::control::execution_transport::required()
        {
            return Err(BootstrapError::Configuration("model_plane_internal_token"));
        }
        let signer = Arc::new(
            LeaseSigner::new(config.lease_secret)
                .map_err(|_| BootstrapError::Configuration("lease_secret"))?,
        );
        let catalog = Arc::new(
            CatalogService::new(
                pools.data.clone(),
                config.capability_worker_url,
                config.options.runtime_internal_token.clone(),
                records,
                config.web_search_configured,
            )
            .map_err(|_| BootstrapError::Construction("capability_catalog"))?,
        );
        let catalog_http = CatalogHttpState {
            service: catalog.clone(),
            internal_token: Arc::new(config.options.runtime_internal_token.clone()),
        };
        let memory = Arc::new(
            PgRuntimeMemory::new(pools.data.clone(), pools.control.clone())
                .map_err(|_| BootstrapError::Construction("runtime_memory"))?,
        );
        let mut control = RuntimeControlPlane::new(
            pools.data.clone(),
            models.models.clone(),
            models.providers.clone(),
            catalog,
            authorities.sources.clone(),
            signer.clone(),
            config.options,
        )
        .map_err(|_| BootstrapError::Configuration("runtime_control"))?;
        control.memory = Some(memory.clone());
        control.admission = Some(core.admission.clone());
        control.capacity = Some(core.capacity.clone());
        let control = Arc::new(control);
        let model_plane = Arc::new(AgentModelPlane {
            context_metrics: None,
            pool: pools.data.clone(),
            signer,
            providers: models.providers.clone(),
            sources: authorities.sources.clone(),
            client: control.client.clone(),
            internal_token: Arc::new(config.model_plane_internal_token),
            admission: Some(core.admission.clone()),
            capacity: Some(core.capacity.clone()),
        });
        let repository = Arc::new(
            AgentRepository::new(
                pools.data.clone(),
                Arc::new(NativeStudioResources {
                    pool: pools.data.clone(),
                    knowledge: authorities.knowledge.clone(),
                }),
            )
            .with_object_policy(core.rbac.decision_point.clone()),
        );
        let mcp_database = Arc::new(
            PgMcpDatabase::new(pools.data.clone(), pools.control.clone())
                .map_err(|_| BootstrapError::Construction("mcp_database"))?,
        );
        let connectors = Arc::new(ConnectorPrincipalRepository::new(
            true,
            Some(mcp_database.clone()),
        ));
        let resources = Arc::new(NativeRuntimeResources {
            pool: pools.data.clone(),
            connectors: connectors.clone(),
            skills: content.skills.clone(),
            mcp_enabled: config.mcp_enabled,
        });
        let cleanup = cleanup_memory.map(|memory| {
            Arc::new(AgentDataCleanup {
                repository: repository.clone(),
                storage: content.artifacts.backend.clone(),
                memory,
            })
        });
        let agent = AgentState {
            auth: auth.admission.clone(),
            repository: repository.clone(),
            control: control.clone(),
            sessions: core.sessions.clone(),
            sources: authorities.sources.clone(),
            attachments: content.attachments.clone(),
            traces: observability.traces.clone(),
            edge_rate: authorities.access.clone(),
            cleanup,
            runtime_resources: resources,
            runtime_knowledge: authorities.runtime_knowledge.clone(),
            default_model: config.default_model,
            management_enabled: config.management_enabled,
            publish_enabled: config.publish_enabled,
            runtime_enabled: config.runtime_enabled,
            public,
        };
        Ok(Self {
            agent,
            model_plane,
            catalog: catalog_http,
            repository,
            control,
            memory,
            connectors,
            mcp_database,
        })
    }
}
