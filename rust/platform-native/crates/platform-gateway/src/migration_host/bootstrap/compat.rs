//! Compatibility surfaces use the same runtime, sessions, current-account and
//! credential authorities as their native counterparts.
use super::{
    BootstrapError, DatabasePools, auth::AuthServices, core::CoreServices, models::ModelServices,
    runtime::RuntimeServices,
};
use crate::{
    connector_user_service::ConnectorService,
    knowledge::KnowledgeService,
    mcp_gateway_broker::McpSecretResolver,
    migration_compat::{
        capability::CapabilityAuthority,
        confluence::ConfluenceBroker,
        eval::EvalService,
        eval_clients::KbRagasClient,
        eval_http::EvalHttpOptions,
        mcp_invocation::McpInvocationBroker,
        responses::{ResponsesIdempotency, ResponsesState},
    },
    migration_content::capabilities::CapabilityAuthorization,
    migration_host::observability::ObservabilityServices,
    migration_local_node::{
        broker::{LocalNodeBroker, LocalNodeDeviceAdapter, LocalNodeDeviceChannelVerifier},
        execution_repository::PostgresLocalNodeExecutionRepository,
    },
    rate_limits::{MultiDimensionConfig, SlidingWindowRateLimiter},
};
use redis::aio::ConnectionManager;
use std::{sync::Arc, time::Duration};
pub struct CompatConfiguration {
    pub internal_token: String,
    pub proof_secret: Vec<u8>,
    pub ragas_origin: String,
    pub ragas_secret: Vec<u8>,
    pub ragas_key_id: String,
    pub default_model: String,
    pub confluence_enabled: bool,
}
pub struct LocalNodeBackends {
    pub device: Arc<dyn LocalNodeDeviceAdapter>,
    pub channel_verifier: Arc<dyn LocalNodeDeviceChannelVerifier>,
}
pub struct CompatServices {
    pub responses: Arc<ResponsesState>,
    pub evaluations: Arc<EvalService>,
    pub eval_options: EvalHttpOptions,
    pub capability_authority: Arc<CapabilityAuthority>,
    pub mcp: Arc<McpInvocationBroker>,
    pub confluence: Arc<ConfluenceBroker>,
    pub local_node: LocalNodeBroker,
}
impl CompatServices {
    pub fn new(
        pools: &DatabasePools,
        redis: Option<ConnectionManager>,
        auth: &AuthServices,
        core: &CoreServices,
        models: &ModelServices,
        runtime: &RuntimeServices,
        observability: &ObservabilityServices,
        limiter: Arc<SlidingWindowRateLimiter>,
        rate_config: MultiDimensionConfig,
        connector_service: Arc<ConnectorService>,
        secrets: Arc<dyn McpSecretResolver>,
        local_node: Option<LocalNodeBackends>,
        config: CompatConfiguration,
        knowledge: Arc<KnowledgeService>,
    ) -> Result<Self, BootstrapError> {
        let responses = Arc::new(ResponsesState {
            auth: auth.admission.clone(),
            sessions: core.sessions.clone(),
            control: runtime.control.clone(),
            models: models.models.clone(),
            idempotency: ResponsesIdempotency::new(redis),
            limiter: Some(limiter),
            rate_config,
        });
        let evaluations = Arc::new(EvalService::new(
            pools.data.clone(),
            Some(Arc::new(
                crate::migration_host::eval_adapters::KnowledgeAdapter(knowledge),
            )),
        ));
        let ragas = Arc::new(
            KbRagasClient::new(
                &config.ragas_origin,
                Some(config.ragas_secret),
                config.ragas_key_id,
                Duration::from_secs(300),
            )
            .map_err(|_| BootstrapError::Configuration("ragas_client"))?
            .with_private_http(Arc::new(
                crate::migration_host::eval_adapters::PrivateTransport,
            )),
        );
        let eval_options = EvalHttpOptions {
            agent: Some(Arc::new(runtime.agent.clone())),
            default_model: config.default_model,
            traces: Some(observability.traces.clone()),
            ragas: Some(ragas),
            admission: Some(auth.admission.clone()),
            rbac: Some(core.rbac.clone()),
        };
        let authority = Arc::new(CapabilityAuthority::new(
            pools.data.clone(),
            config.internal_token.clone(),
            config.proof_secret.clone(),
        ));
        let mcp = Arc::new(McpInvocationBroker::new(
            pools.data.clone(),
            secrets.clone(),
        ));
        let confluence = Arc::new(ConfluenceBroker {
            authority: authority.clone(),
            principals: runtime.connectors.clone(),
            connectors: connector_service,
            secrets,
            enabled: config.confluence_enabled,
        });
        // None represents source-disabled device forwarding, not a fabricated device.
        let (device, channel_verifier) = match local_node {
            Some(v) => (Some(v.device), Some(v.channel_verifier)),
            None => (None, None),
        };
        let local_node = LocalNodeBroker {
            repository: Some(Arc::new(PostgresLocalNodeExecutionRepository {
                pool: pools.data.clone(),
            })),
            device,
            channel_verifier,
            authorization: Arc::new(CapabilityAuthorization::new(
                config.internal_token,
                config.proof_secret,
            )),
        };
        Ok(Self {
            responses,
            evaluations,
            eval_options,
            capability_authority: authority,
            mcp,
            confluence,
            local_node,
        })
    }
}
