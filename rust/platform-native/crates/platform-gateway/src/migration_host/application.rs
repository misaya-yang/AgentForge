//! Concrete router composition. Gateway and Knowledge Service remain distinct
//! authentication boundaries; no process is started by these factories.
use crate::{
    migration_agent as agent, migration_compat as compat, migration_content as content,
    migration_knowledge_query as query,
    migration_local_node as local,
};
use ai_platform_auth::service::AuthService;
use axum::Router;
use std::sync::Arc;
/// The foundation contains the established authentication, provider/model,
/// session/admin/usage/config routes, constructed by foundation::FoundationApis.
/// Knowledge forwarding stays at the signed HTTP client boundary. Knowledge
/// Service business routers belong only to the separate Knowledge service crate.
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
        let context_metrics =
            super::context_metrics::router(super::context_metrics::ContextMetricsState {
                access: access.clone(),
                sessions: self.agent.sessions.clone(),
                collector: super::context_metrics::ContextMetricsCollector::global(),
            })
            .layer(axum::middleware::from_fn_with_state(
                access.clone(),
                content::access::request_context,
            ));
        let session_tools = super::catalog::session_tools::router(
            super::catalog::session_tools::SessionToolsState {
                catalog: self.catalog.service.clone(),
                access: access.clone(),
                sessions: self.agent.sessions.clone(),
                control: self.agent.control.clone(),
                default_model: self.agent.default_model.clone(),
            },
        )
        .layer(axum::middleware::from_fn_with_state(
            access.clone(),
            content::access::request_context,
        ));
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
            .route("/openapi.json", axum::routing::get(openapi_document))
            .merge(agent::model::router(self.model_plane))
            .nest("/api/v1", content::router(self.content))
            .merge(content::capabilities::router(self.content_capabilities))
            .merge(content::read_broker::router(self.content_reads))
            .merge(super::catalog::http::router(self.catalog))
            .merge(session_tools)
            .merge(context_metrics)
            .merge(actors)
            .merge(super::knowledge_proxy::router(self.knowledge_proxy))
            .merge(compat::responses::router(self.responses))
            .merge(compat::langgraph::router(
                langgraph_admission,
                self.langgraph,
            ))
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
/// The same native declaration producer used by the offline contract gate.
pub async fn openapi_document() -> axum::Json<serde_json::Value> {
    axum::Json(super::openapi_document::extend_document(
        ai_platform_gateway_foundation::native_contract::document(),
    ))
}
