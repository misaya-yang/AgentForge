//! Knowledge remains the single dataset/document/job/generation authority.
use super::*;
use ai_platform_service_primitives::knowledge_types::{IndexGeneration, UserContext};
impl KnowledgeModelPlane {
    pub(super) async fn check_knowledge_scope(
        &self,
        call: &ModelCall,
        actor: &UserContext,
        expected: Option<IndexGeneration>,
    ) -> PlaneResult<ModelScopeCheckResponse> {
        let identity = crate::migration_content::ContentIdentity {
            user_id: actor.user_id.clone(),
            tenant_id: actor.tenant_id.clone(),
            tier: actor.user_tier.clone(),
            roles: actor.roles.clone(),
            permissions: vec![],
            is_authenticated: true,
        };
        let knowledge_actor = crate::knowledge::KnowledgeActor::from_identity(&identity)
            .map_err(|_| PlaneError::new(403, "knowledge_scope_actor_invalid"))?;
        let mut scope = call.scope.clone();
        scope.actor = actor.clone();
        let request = ModelScopeCheckRequest {
            call_id: call.call_id.clone(),
            request_sha256: call_digest(call)
                .map_err(|_| PlaneError::new(422, "knowledge_model_call_invalid"))?,
            scope,
            purpose: call.purpose,
            expected_generation: expected,
        };
        let proof = self
            .knowledge
            .model_operation_scope(&knowledge_actor, &request)
            .await
            .map_err(|_| PlaneError::new(403, "knowledge_model_resource_scope_denied"))?;
        validate_proof(&request, &proof)?;
        Ok(proof)
    }
    pub(super) async fn recheck_scope(
        &self,
        call: &ModelCall,
        snapshot: &Value,
    ) -> PlaneResult<()> {
        let proof: ModelScopeCheckResponse =
            serde_json::from_value(snapshot["scope_check"].clone())
                .map_err(|_| PlaneError::new(503, "knowledge_scope_proof_missing"))?;
        let actor = self.current_actor(call).await?;
        self.check_knowledge_scope(call, &actor, proof.generation)
            .await?;
        Ok(())
    }
}
fn validate_proof(
    request: &ModelScopeCheckRequest,
    proof: &ModelScopeCheckResponse,
) -> PlaneResult<()> {
    if proof.call_id != request.call_id
        || proof.request_sha256 != request.request_sha256
        || proof.tenant_id != request.scope.actor.tenant_id
        || proof.user_id != request.scope.actor.user_id
        || proof.dataset_id != request.scope.dataset_id
        || proof.document_id != request.scope.document_id
        || proof.execution_id != request.scope.execution_id
        || proof.purpose != request.purpose
        || request
            .expected_generation
            .as_ref()
            .is_some_and(|expected| proof.generation.as_ref() != Some(expected))
        || request.scope.dataset_id.is_some() != proof.generation.is_some()
    {
        return Err(PlaneError::new(
            403,
            "knowledge_scope_proof_binding_invalid",
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn foreign_or_changed_resource_scope_is_never_admitted() {
        let c = super::super::tests::call();
        let request = ModelScopeCheckRequest {
            call_id: c.call_id.clone(),
            request_sha256: call_digest(&c).unwrap(),
            scope: c.scope.clone(),
            purpose: c.purpose,
            expected_generation: None,
        };
        let p = ModelScopeCheckResponse {
            call_id: request.call_id.clone(),
            request_sha256: request.request_sha256.clone(),
            tenant_id: "t".into(),
            user_id: "u".into(),
            dataset_id: None,
            document_id: None,
            execution_id: None,
            purpose: c.purpose,
            generation: None,
        };
        assert!(validate_proof(&request, &p).is_ok());
        for i in 0..6 {
            let mut p = p.clone();
            match i {
                0 => p.tenant_id = "foreign".into(),
                1 => p.user_id = "other".into(),
                2 => p.dataset_id = Some("foreign".into()),
                3 => p.document_id = Some("foreign".into()),
                4 => p.execution_id = Some("foreign".into()),
                _ => p.request_sha256 = "f".repeat(64),
            }
            assert!(validate_proof(&request, &p).is_err())
        }
    }
}
