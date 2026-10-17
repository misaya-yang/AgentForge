//! One-call durable Gateway graph. Platform-specific unresolved backends are
//! explicit inputs; every service built here shares its source-owned singleton.
use super::{auth::*, compat::*, content::*, core::*, foundation::*, models::*, runtime::*, *};
use crate::{
    migration_host::{
        application::GatewayApplication,
        foundation::FoundationCancellation,
        knowledge_proxy::{KnowledgeProxy, ProxyConfig},
        observability::ObservabilityServices,
    },
    migration_knowledge_query::{
        actor::{CurrentActorStore, GatewayActorState},
        auth::GatewayAuth,
    },
    rate_limits::{MultiDimensionConfig, RedisSlidingWindowStore, SlidingWindowRateLimiter},
};
use ai_platform_auth::service::AuthService;
use axum::Router;
use redis::aio::ConnectionManager;
use std::{collections::BTreeMap, sync::Arc};

pub struct PublicConfiguration {
    pub embed_secret: String,
    pub max_attachment_bytes: usize,
}
pub struct GatewayConfiguration {
    pub auth: AuthConfiguration,
    pub core: CoreConfiguration,
    pub callback_allowlist: Vec<String>,
    pub native_foundation: foundation_backends::NativeFoundationConfiguration,
    pub provider_encryption_key: String,
    pub provider_environment: BTreeMap<String, String>,
    pub pricing_cache_ttl_seconds: f64,
    pub google_auth: crate::migration_host::google_auth::GoogleAuthConfig,
    pub runtime_configured_providers: Vec<String>,
    pub runtime: RuntimeConfiguration,
    pub content: ContentRuntimeConfiguration,
    pub pdf_backend: crate::migration_host::pdf_backend::PopplerPdfConfig,
    pub compat: CompatConfiguration,
    pub evaluation: evaluation::EvaluationConfiguration,
    pub foundation: FoundationConfiguration,
    pub foundation_shared: FoundationSharedConfiguration,
    pub rate_config: MultiDimensionConfig,
    pub usage: crate::usage_recorder::RecorderConfig,
    pub retention: crate::migration_trace::retention::RetentionConfig,
    pub knowledge_proxy: ProxyConfig,
    pub history_knowledge: crate::knowledge::KnowledgeConfig,
    pub public: Option<PublicConfiguration>,
}
pub struct GatewayBackends {
    pub foundation: foundation_backends::FoundationHostPolicies,
    pub usage_publisher: Option<Arc<dyn crate::usage_recorder::UsageEventPublisher>>,
    pub local_node: Option<LocalNodeBackends>,
    pub cleanup_memory: Option<Arc<crate::migration_agent::memory_cleanup::RuntimeMemoryCleanup>>,
}
pub struct KnowledgeAuthorities {
    /// gateway -> knowledge-service; HMAC v2, anonymous disabled.
    pub signer: Arc<GatewayAuth>,
    /// knowledge-service -> gateway; separately scoped HMAC v2 verifier.
    pub actor_verifier: Arc<GatewayAuth>,
}
pub struct GatewayGraph {
    pub pools: DatabasePools,
    pub auth: Arc<AuthService>,
    pub admission: Arc<ai_platform_auth::admission::AdmissionService>,
    pub history_knowledge: Arc<crate::knowledge::KnowledgeService>,
    pub core: CoreServices,
    pub models: ModelServices,
    pub authorities: RuntimeAuthorities,
    pub content: ContentGraph,
    pub runtime: RuntimeServices,
    pub compat: CompatServices,
    pub evaluation: Arc<evaluation::EvaluationServices>,
    pub foundation: AssembledFoundation,
    pub foundation_shared: FoundationShared,
    pub native_foundation: foundation_backends::NativeFoundationServices,
    pub langgraph_preparation: Arc<langgraph_preparation::LiveLangGraphPreparation>,
    pub image_worker_running: Arc<std::sync::atomic::AtomicBool>,
    pub observability: Arc<ObservabilityServices>,
    pub limiter: Arc<SlidingWindowRateLimiter>,
    pub knowledge_actor: GatewayActorState,
    pub knowledge_proxy: Arc<KnowledgeProxy>,
}
impl GatewayGraph {
    pub async fn new(
        pools: DatabasePools,
        redis: ConnectionManager,
        config: GatewayConfiguration,
        backends: GatewayBackends,
        knowledge: KnowledgeAuthorities,
    ) -> Result<Self, BootstrapError> {
        let models = ModelServices::native(
            &pools,
            config.provider_encryption_key.clone(),
            config.provider_environment.clone(),
            config.pricing_cache_ttl_seconds,
            config.google_auth.clone(),
        )?;
        Self::from_model_services(pools, redis, config, backends, knowledge, models).await
    }
    /// Reuse the exact live service instances initialized by explicit process startup.
    pub async fn from_model_services(
        pools: DatabasePools,
        redis: ConnectionManager,
        config: GatewayConfiguration,
        backends: GatewayBackends,
        knowledge: KnowledgeAuthorities,
        models: ModelServices,
    ) -> Result<Self, BootstrapError> {
        // Shared private capability endpoints must agree on the same authority.
        if config.content.capability_internal_token != config.compat.internal_token
            || config.content.capability_proof_secret != config.compat.proof_secret
        {
            return Err(BootstrapError::Configuration(
                "capability_authority_mismatch",
            ));
        }
        if knowledge.actor_verifier.version != "v2"
            || knowledge.actor_verifier.caller_service != "knowledge-service"
            || knowledge.actor_verifier.audience != "gateway"
            || knowledge.actor_verifier.allow_anonymous
        {
            return Err(BootstrapError::Configuration("knowledge_actor_verifier"));
        }
        let observability = ObservabilityServices::new(
            pools.data.clone(),
            pools.control.clone(),
            config.usage,
            config.retention,
            backends.usage_publisher,
        )
        .map_err(|_| BootstrapError::Construction("observability"))?;
        let security = Arc::new(foundation_backends::PgFoundationSecurityEvents::new(
            pools.data.clone(),
        ));
        let quota = Arc::new(
            foundation_backends::NativeQuotaChecker::new(&pools, security)?
                .with_shared_rpm(redis.clone()),
        );
        let langgraph_preparation = Arc::new(langgraph_preparation::LiveLangGraphPreparation::new(
            models.models.clone(),
            models.providers.clone(),
            pools.data.clone(),
            quota.clone(),
        ));
        let core = CoreServices::new(
            &pools,
            Some(redis.clone()),
            config.core,
            CoreBackends::native(
                &pools,
                langgraph_preparation.clone(),
                config.callback_allowlist,
            ),
            observability.usage.as_ref(),
            &config.provider_environment,
        )?;
        core.registry
            .configure_sdk_agent_key(config.provider_encryption_key.clone())
            .map_err(|_| BootstrapError::Construction("sdk_agent_encryption"))?;
        let auth = AuthServices::new(&pools, redis.clone(), config.auth)?;
        let limiter = Arc::new(SlidingWindowRateLimiter::new(Some(Arc::new(
            RedisSlidingWindowStore::new(redis.clone()),
        ))));
        let authorities = RuntimeAuthorities::new(
            &pools,
            &auth,
            &core,
            &models,
            &config.runtime.knowledge_origin,
            knowledge.signer.clone(),
            limiter.clone(),
            Arc::new(config.rate_config.clone()),
            config.foundation_shared.trusted_proxies.clone(),
        )?;
        let history = Arc::new(
            crate::knowledge::KnowledgeService::new(config.history_knowledge)
                .map_err(|_| BootstrapError::Configuration("history_knowledge"))?,
        );
        let proxy_loader = Arc::new(foundation_backends::PgProxyConfigLoader::new(
            pools.data.clone(),
            config.native_foundation.proxy_cache_ttl,
        ));
        let mut content_config = config.content;
        content_config.langgraph.governance.live_services = Some(proxy_loader.clone());
        content_config.langgraph.governance.live_resolution = Some(langgraph_preparation.clone());
        content_config.langgraph.governance.quota = Some(quota.store.clone());
        content_config.langgraph.governance.rpm = quota.rpm.clone();
        content_config.langgraph.governance.security = Some(quota.security.clone());
        let content = ContentGraph::new(
            &pools,
            Some(redis.clone()),
            &core,
            &authorities,
            &observability,
            limiter.clone(),
            config.rate_config.clone(),
            content_config,
            ContentBackends {
                pdf: Arc::new(
                    crate::migration_host::pdf_backend::PopplerPdfBackend::new(config.pdf_backend)
                        .map_err(|_| BootstrapError::Configuration("pdf_backend"))?,
                ),
            },
            history.clone(),
        )?;
        let public = config.public.map(|p| {
            Arc::new(crate::migration_agent::public::PublicRuntime {
                identities: auth.identities.clone(),
                redis: redis.clone(),
                embed_secret: Arc::new(p.embed_secret),
                storage: Some(content.content.attachments.uploads.clone()),
                max_attachment_bytes: p.max_attachment_bytes,
            })
        });
        let runtime = RuntimeServices::new(
            &pools,
            &auth,
            &core,
            &models,
            &authorities,
            &content.content,
            &observability,
            config.runtime,
            public,
            backends.cleanup_memory,
        )?;
        let shared = FoundationShared::new(
            &pools,
            Some(redis.clone()),
            runtime.connectors.clone(),
            runtime.mcp_database.clone(),
            models.metadata(config.runtime_configured_providers),
            config.foundation_shared,
        )?;
        let image_worker_running = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let native_foundation = foundation_backends::NativeFoundationServices::new(
            &pools,
            &shared,
            &core,
            runtime.control.clone(),
            &observability,
            image_worker_running.clone(),
            quota,
            proxy_loader,
            config.native_foundation,
        )?;
        let secrets = backends.foundation.mcp_secrets.clone();
        let foundation_backends = native_foundation.clone().into_backends(backends.foundation);
        let foundation = assemble_foundation(
            &pools,
            Some(redis.clone()),
            &core,
            &auth,
            &models,
            authorities.access.clone(),
            shared.clone(),
            config.foundation,
            foundation_backends,
            history.clone(),
        )?;
        let compat = CompatServices::new(
            &pools,
            Some(redis),
            &auth,
            &core,
            &models,
            &runtime,
            &observability,
            limiter.clone(),
            config.rate_config,
            foundation.connector_service.clone(),
            secrets,
            backends.local_node,
            config.compat,
            history.clone(),
        )?;
        let evaluation =
            evaluation::EvaluationServices::new(config.evaluation, &core, &compat, &observability)?;
        let retrieval_rpc = config.knowledge_proxy.retrieval_rpc.clone();
        let mut proxy = KnowledgeProxy::new(
            config.knowledge_proxy,
            knowledge.signer,
            authorities.access.clone(),
            content.database.clone(),
        )
        .map_err(|_| BootstrapError::Configuration("knowledge_proxy"))?;
        if let Some((origin, rpc_config)) = retrieval_rpc {
            proxy
                .configure_retrieval_rpc(origin, rpc_config)
                .await
                .map_err(|_| BootstrapError::Construction("knowledge_rpc"))?;
        }
        proxy.configure_sdk_reads(core.registry.clone(), core.session_storage.clone());
        proxy.sdk_control = Some(runtime.control.clone());
        proxy.trace_capture = Some(
            crate::migration_knowledge_query::rag_trace::RagTraceCapture::new(
                observability.traces.clone(),
            ),
        );
        let knowledge_actor = GatewayActorState {
            store: CurrentActorStore {
                pool: pools.data.clone(),
            },
            verifier: knowledge.actor_verifier,
        };
        let proxy = Arc::new(proxy);
        core.registry
            .configure_sdk_capabilities(Arc::new(
                crate::migration_host::knowledge_proxy::sdk_capabilities::Authority {
                    proxy: Arc::downgrade(&proxy),
                },
            ))
            .map_err(|_| BootstrapError::Construction("sdk_capability_authority"))?;
        Ok(Self {
            pools,
            auth: auth.auth,
            admission: auth.admission,
            history_knowledge: history,
            core,
            models,
            authorities,
            content,
            runtime,
            compat,
            evaluation,
            foundation,
            foundation_shared: shared,
            native_foundation,
            langgraph_preparation,
            image_worker_running,
            observability,
            limiter,
            knowledge_actor,
            knowledge_proxy: proxy,
        })
    }
    pub fn into_router(
        self,
        mut options: crate::GatewayOptions,
        roles: BTreeMap<String, Vec<String>>,
        cancellation: FoundationCancellation,
        content_type: crate::balancer_config_api::BalancerConfigContentTypePolicy,
    ) -> Result<Router, BootstrapError> {
        options.identity = Some(self.content.content.public.access.clone());
        // Both public session surfaces share the same durable deletion owner.
        options.sessions.cleanup = Some(self.native_foundation.runtime.clone());
        let proxy_access = self.content.content.public.access.clone();
        let proxy_store = Arc::new(
            crate::service_access_resolver::PgServiceAccessConstraintStore::new(
                self.pools.data.clone(),
                self.pools.control.clone(),
                self.native_foundation.warnings.clone(),
            )
            .map_err(|_| BootstrapError::Construction("proxy_service_access"))?,
        );
        let proxy = crate::migration_compat::proxy_http::router(Arc::new(
            crate::migration_compat::proxy_http::ProxyHttp {
                access: proxy_access.clone(),
                admission: self.admission.clone(),
                registry: self.core.registry.clone(),
                loader: self.native_foundation.proxy_loader.clone(),
                proxy: self.native_foundation.proxy.proxy.clone(),
                capacity: self.core.capacity.clone(),
                constraints: self.foundation_shared.access.clone(),
                rates: self.foundation_shared.rate_resolver.clone(),
                security: self.native_foundation.security_events.clone(),
                constraint_store: proxy_store,
                langgraph: self.content.langgraph.clone(),
            },
        ))
        .layer(axum::middleware::from_fn_with_state(
            proxy_access,
            crate::migration_content::access::request_context,
        ));
        let foundation = self.foundation.apis.into_router(
            self.auth.clone(),
            self.pools.data,
            options,
            roles,
            cancellation,
            content_type,
        );
        let foundation = foundation
            .merge(proxy)
            .layer(axum::middleware::from_fn_with_state(
                self.content.content.public.access.clone(),
                crate::migration_content::access::request_context,
            ))
            .layer(axum::middleware::from_fn(
                foundation_backends::request_effects_context,
            ));
        let application = GatewayApplication {
            foundation,
            auth: self.auth,
            agent: self.runtime.agent,
            model_plane: self.runtime.model_plane,
            content: self.content.content.public,
            content_capabilities: self.content.content.capabilities,
            content_reads: self.content.content.reads,
            catalog: self.runtime.catalog,
            knowledge_actor: self.knowledge_actor,
            knowledge_proxy: self.knowledge_proxy,
            responses: self.compat.responses,
            langgraph: self.content.langgraph,
            evaluations: self.compat.evaluations,
            eval_options: self.compat.eval_options,
            capability_authority: self.compat.capability_authority,
            mcp: self.compat.mcp,
            confluence: self.compat.confluence,
            local_node: self.compat.local_node,
        };
        let router = application
            .into_router()
            .map_err(|_| BootstrapError::Construction("gateway_router"))?;
        Ok(router)
    }
}
