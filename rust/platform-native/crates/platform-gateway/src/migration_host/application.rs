//! Concrete router composition. Gateway and Knowledge Service remain distinct
//! authentication boundaries; no process is started by these factories.
use crate::{
    migration_agent as agent, migration_compat as compat, migration_content as content,
    migration_knowledge_ingest as ingest, migration_knowledge_query as query,
    migration_local_node as local,
};
use ai_platform_auth::service::AuthService;
use axum::Router;
use std::sync::Arc;
/// The foundation contains the established authentication, provider/model,
/// session/admin/usage/config routes, constructed by foundation::FoundationApis.
/// Knowledge forwarding stays at the signed HTTP client boundary. Knowledge
/// Service business routers belong only to KnowledgeApplication below.
pub struct GatewayApplication {
    pub foundation: Router,
    pub auth: Arc<AuthService>,
    pub agent: agent::routes::AgentState,
    pub model_plane: Arc<agent::model::AgentModelPlane>,
    pub content: content::ContentServices,
    pub content_capabilities: Arc<content::capabilities::ContentCapabilityBroker>,
    pub content_reads: Arc<content::read_broker::ContentReadBroker>,
    pub catalog: super::catalog::http::CatalogHttpState,
    pub knowledge_actor: query::actor::GatewayActorState,
    pub knowledge_proxy: Arc<super::knowledge_proxy::KnowledgeProxy>,
    pub responses: Arc<compat::responses::ResponsesState>,
    pub langgraph: Arc<compat::langgraph::LangGraphClient>,
    pub evaluations: Arc<compat::eval::EvalService>,
    pub eval_options: compat::eval_http::EvalHttpOptions,
    pub capability_authority: Arc<compat::capability::CapabilityAuthority>,
    pub mcp: Arc<compat::mcp_invocation::McpInvocationBroker>,
    pub confluence: Arc<compat::confluence::ConfluenceBroker>,
    pub local_node: local::broker::LocalNodeBroker,
}
impl GatewayApplication {
    pub fn into_router(self) -> Result<Router, super::lifecycle::HostError> {
        let access = self.content.access.clone();
        let langgraph_admission = self.responses.auth.clone();
        let actors = query::actor::gateway_actor_router(self.knowledge_actor)
            .map_err(|_| super::lifecycle::HostError::Configuration)?;
        Ok(self
            .foundation
            .layer(axum::middleware::from_fn_with_state(
                access.clone(),
                content::access::request_context,
            ))
            .merge(
                agent::routes::router(self.agent).layer(axum::middleware::from_fn_with_state(
                    access,
                    content::access::request_context,
                )),
            )
            .merge(agent::model::router(self.model_plane))
            .nest("/api/v1", content::router(self.content))
            .merge(content::capabilities::router(self.content_capabilities))
            .merge(content::read_broker::router(self.content_reads))
            .merge(super::catalog::http::router(self.catalog))
            .merge(actors)
            .merge(super::knowledge_proxy::router(self.knowledge_proxy))
            .merge(compat::responses::router(self.responses))
            .merge(compat::langgraph::router(langgraph_admission, self.langgraph))
            .merge(compat::eval::router_with_options(
                self.auth,
                self.evaluations,
                self.eval_options,
            ))
            .merge(compat::mcp_invocation::router(
                self.capability_authority,
                self.mcp,
            ))
            .merge(compat::confluence::router(self.confluence))
            .merge(local::broker::local_node_broker_router(self.local_node)))
    }
}
/// Only signed gateway identities enter the Knowledge Service public operation
/// surface. The independently proof-authorized capability route is separate.
pub struct KnowledgeApplication {
    pub service_auth: Arc<query::auth::GatewayAuth>,
    pub retrieval: Arc<query::service::RetrievalService>,
    pub qa: Option<Arc<query::qa::QaService>>,
    pub writes: ingest::http::WriteApiState,
    pub lexical: query::lexical_http::LexicalApiState,
    pub embedding_migrations: Arc<query::migration::EmbeddingMigration>,
    pub source_store: query::store::QueryStore,
    pub capabilities: query::actor::CapabilityState,
    pub ragas: Arc<query::ragas::RagasService>,
}
impl KnowledgeApplication {
    pub fn into_router(self) -> query::types::Result<Router> {
        self.service_auth.validate_service_boundary()?;
        Ok(Router::new()
            .merge(query::ragas::knowledge_service_router(
                self.service_auth.clone(),
                self.ragas,
            )?)
            .merge(query::http::knowledge_service_router(
                self.service_auth.clone(),
                self.retrieval,
                self.qa,
            )?)
            .merge(ingest::http::service_routes(
                self.writes,
                self.service_auth.clone(),
            ))
            .merge(query::lexical_http::knowledge_service_router(
                self.lexical,
                self.service_auth.clone(),
            )?)
            .merge(query::migration_http::knowledge_service_router(
                self.embedding_migrations,
                self.service_auth.clone(),
            )?)
            .merge(query::source_authz::router(
                self.source_store,
                self.service_auth,
            )?)
            .merge(query::actor::capability_router(self.capabilities)))
    }
}
