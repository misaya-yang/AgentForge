//! Internal scope authority. Only signed Gateway workload middleware may mount
//! this route; request roles are discarded and actor membership is refreshed.
use super::{
    actor::RuntimeActorClient,
    auth::{self, GatewayAuth},
    store::QueryStore,
    types::{IndexGeneration, QueryError, Result, UserContext},
};
use ai_platform_service_primitives::knowledge_model::*;
use axum::{Json, Router, extract::State, middleware, routing::post};
use std::sync::Arc;
#[derive(Clone)]
pub struct ModelScopeAuthority {
    pub store: QueryStore,
    pub actors: RuntimeActorClient,
}
impl ModelScopeAuthority {
    pub async fn authorize(
        &self,
        request: ModelScopeCheckRequest,
    ) -> Result<ModelScopeCheckResponse> {
        if request.call_id.trim().is_empty()
            || request.request_sha256.len() != 64
            || !request
                .request_sha256
                .bytes()
                .all(|c| c.is_ascii_hexdigit())
        {
            return Err(QueryError::Validation(
                "invalid model scope proof binding".into(),
            ));
        }
        let actor = self
            .actors
            .resolve(&request.scope.actor.tenant_id, &request.scope.actor.user_id)
            .await?;
        require_actor(&actor, &request.scope.actor)?;
        let generation = if let Some(id) = request.scope.dataset_id.as_deref() {
            let permission = match request.purpose {
                ModelPurpose::QueryEmbedding
                | ModelPurpose::SimilarityEmbedding
                | ModelPurpose::Rerank => "viewer",
                _ => "editor",
            };
            let lease = super::migration::SessionLease::acquire_shared(
                &self.store.pool,
                format!("knowledge-dataset-index:{id}"),
            )
            .await
            .map_err(|_| QueryError::GenerationChanged)?;
            let dataset = self
                .store
                .require_dataset_access(&actor, id, permission)
                .await?;
            let current = dataset.generation()?;
            require_generation(request.expected_generation.as_ref(), &current)?;
            if let Some(document) = request.scope.document_id.as_deref() {
                let present:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.documents WHERE dataset_id=$1 AND document_id=$2)").bind(id).bind(document).fetch_one(&self.store.pool).await?;
                if !present {
                    return Err(QueryError::NotFound);
                }
            }
            if let Some(execution) = request.scope.execution_id.as_deref() {
                let present:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.document_pipeline_executions e JOIN knowledge.documents d ON d.document_id=e.document_id AND d.dataset_id=e.dataset_id WHERE e.execution_id=$1 AND e.dataset_id=$2 AND e.document_id=$3 AND COALESCE(e.triggered_by,d.created_by)=$4 AND e.status='running' AND d.metadata->>'_document_pipeline_execution_id'=e.execution_id) OR EXISTS(SELECT 1 FROM knowledge.vector_mutation_jobs j WHERE job_id=$1 AND tenant_id=$5 AND dataset_id=$2 AND requested_by=$4 AND state='running') OR EXISTS(SELECT 1 FROM knowledge.embedding_migration_action_jobs a JOIN knowledge.datasets ds ON ds.dataset_id=a.dataset_id WHERE a.job_id::text=$1 AND a.dataset_id=$2 AND ds.tenant_id=$5 AND a.requested_by=$4 AND a.state='running')")
                    .bind(execution).bind(id).bind(request.scope.document_id.as_deref()).bind(&actor.user_id).bind(&actor.tenant_id).fetch_one(&self.store.pool).await?;
                if !present {
                    return Err(QueryError::Forbidden);
                }
            }
            lease.release().await.map_err(|_| QueryError::Database)?;
            Some(current)
        } else {
            if request.scope.document_id.is_some()
                || request.scope.execution_id.is_some()
                || request.expected_generation.is_some()
                || !permits_unbound_input(&request.purpose)
            {
                return Err(QueryError::Forbidden);
            }
            None
        };
        Ok(ModelScopeCheckResponse {
            call_id: request.call_id,
            request_sha256: request.request_sha256,
            tenant_id: actor.tenant_id,
            user_id: actor.user_id,
            dataset_id: request.scope.dataset_id,
            document_id: request.scope.document_id,
            execution_id: request.scope.execution_id,
            purpose: request.purpose,
            generation,
        })
    }
}
fn require_actor(actual: &UserContext, claimed: &UserContext) -> Result<()> {
    if actual.anonymous()
        || claimed.anonymous()
        || actual.tenant_id != claimed.tenant_id
        || actual.user_id != claimed.user_id
    {
        Err(QueryError::Forbidden)
    } else {
        Ok(())
    }
}
fn require_generation(expected: Option<&IndexGeneration>, current: &IndexGeneration) -> Result<()> {
    if expected.is_some_and(|old| old != current) {
        Err(QueryError::GenerationChanged)
    } else {
        Ok(())
    }
}
async fn handler(
    State(authority): State<Arc<ModelScopeAuthority>>,
    Json(request): Json<ModelScopeCheckRequest>,
) -> Result<Json<ModelScopeCheckResponse>> {
    Ok(Json(authority.authorize(request).await?))
}
/// No unsigned/raw variant is exported. Signature middleware verifies full body,
/// canonical path, caller/audience and replay nonce before deserializing scope.
pub fn signed_router(
    authority: Arc<ModelScopeAuthority>,
    verifier: Arc<GatewayAuth>,
) -> Result<Router> {
    if verifier.caller_service != "gateway"
        || verifier.audience != "knowledge-service"
        || verifier.version != "v2"
        || verifier.allow_anonymous
    {
        return Err(QueryError::Forbidden);
    }
    verifier.validate_service_boundary()?;
    Ok(Router::new()
        .route(SCOPE_CHECK_PATH, post(handler))
        .with_state(authority)
        .layer(middleware::from_fn_with_state(
            verifier,
            auth::verify_gateway_signature,
        )))
}
// Only admitted actor-supplied evaluation inputs may omit Knowledge resources.
// Resource-bearing calls always follow the ACL/publication-generation branch above.
fn permits_unbound_input(purpose: &ModelPurpose) -> bool {
    matches!(
        purpose,
        ModelPurpose::Judge
            | ModelPurpose::Rerank
            | ModelPurpose::DocumentEmbedding
            | ModelPurpose::SimilarityEmbedding
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn golden_rerank_does_not_admit_other_unbound_operations() {
        assert!(permits_unbound_input(&ModelPurpose::Rerank));
        for purpose in [
            ModelPurpose::QueryEmbedding,
            ModelPurpose::Ocr,
            ModelPurpose::Vision,
            ModelPurpose::Summary,
            ModelPurpose::Qa,
        ] {
            assert!(!permits_unbound_input(&purpose));
        }
    }
    fn actor(tenant: &str) -> UserContext {
        UserContext {
            tenant_id: tenant.into(),
            user_id: "u".into(),
            roles: vec!["admin".into()],
            user_tier: "admin".into(),
            user_type: "user".into(),
            is_authenticated: true,
        }
    }
    #[test]
    fn unsigned_claimed_admin_cannot_cross_tenant_or_identity() {
        assert!(require_actor(&actor("a"), &actor("b")).is_err());
        let mut forged = actor("a");
        forged.user_id = "other".into();
        assert!(require_actor(&actor("a"), &forged).is_err());
    }
    #[test]
    fn changed_generation_never_reuses_scope_permission() {
        let a = IndexGeneration {
            tenant_id: "a".into(),
            content_revision: Some(1),
            collection_name: "c".into(),
            embedding_provider: "p".into(),
            embedding_model: "m".into(),
            embedding_model_version: "v1".into(),
            embedding_dimension: 3,
        };
        let mut b = a.clone();
        b.content_revision = Some(2);
        assert!(require_generation(Some(&a), &b).is_err());
        b = a.clone();
        b.embedding_model_version = "v2".into();
        assert!(require_generation(Some(&a), &b).is_err());
    }
}
