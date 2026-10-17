//! Child of `knowledge`: reuse its canonical signed client, timeout and limits.
use super::*;
use ai_platform_service_primitives::knowledge_model::{
    ModelScopeCheckRequest, ModelScopeCheckResponse, SCOPE_CHECK_PATH,
};
impl KnowledgeService {
    pub(crate) async fn model_operation_scope(
        &self,
        actor: &KnowledgeActor,
        request: &ModelScopeCheckRequest,
    ) -> Result<ModelScopeCheckResponse, KnowledgeError> {
        let body = serde_json::to_vec(request).map_err(|_| KnowledgeError::InvalidResponse)?;
        if body.len() > MAX_RESPONSE_BYTES {
            return Err(KnowledgeError::InvalidResponse);
        }
        let url = self.endpoint(&["internal", "v2", "knowledge", "model-operation-scope"])?;
        let (payload, _, _) = self
            .request(
                Method::POST,
                url,
                SCOPE_CHECK_PATH,
                actor.headers(IdentityUse::Catalog)?,
                body,
            )
            .await?;
        serde_json::from_value(payload).map_err(|_| KnowledgeError::InvalidResponse)
    }
}
