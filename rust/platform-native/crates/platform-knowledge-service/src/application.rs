//! Knowledge-owned business router composition.
use crate::{migration_knowledge_ingest as ingest, migration_knowledge_query as query};
use axum::Router;
use std::sync::Arc;
/// Only signed gateway identities enter the Knowledge Service public operation
/// surface. The independently proof-authorized capability route is separate.
pub struct KnowledgeApplication {
    pub model_plane:Arc<query::model_plane::ModelPlaneClient>,
    pub model_scope:Arc<query::model_scope::ModelScopeAuthority>,
    pub upload: crate::upload::UploadState,
    pub sources: Arc<crate::confluence::Sources>,
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
            .merge(crate::confluence::routes(
                self.sources.clone(),
                self.service_auth.clone(),
            ))
            .merge(query::ragas::knowledge_service_router(
                self.service_auth.clone(),
                self.ragas,
            )?)
            .merge(query::http::knowledge_service_router(
                self.service_auth.clone(),
                self.retrieval,
                self.qa,
            )?)
            .merge(
                ingest::http::service_routes(self.writes, self.service_auth.clone())
                    .layer(axum::middleware::from_fn_with_state(
                        self.upload,
                        crate::upload::intercept,
                    ))
                    .layer(axum::middleware::from_fn_with_state(
                        self.sources,
                        crate::confluence::catalog::intercept,
                    ))
                    .layer(axum::middleware::from_fn_with_state(
                        self.service_auth.clone(),
                        query::auth::verify_gateway,
                    )),
            )
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
                self.service_auth.clone(),
            )?)
            .merge(query::actor::capability_router(self.capabilities))
            .merge(query::model_scope::signed_router(self.model_scope,self.service_auth.clone())?)
            .layer(axum::middleware::from_fn_with_state(self.model_plane,query::model_plane::request_context)))
    }
}
