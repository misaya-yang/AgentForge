//! Accounting authority survives price changes and revoked output permissions.
use super::*;
impl KnowledgeModelPlane {
    pub(super) async fn authorize_delivery(
        &self,
        call: &ModelCall,
        snapshot: &Value,
    ) -> PlaneResult<()> {
        self.authorize(call).await?;
        self.recheck_scope(call, snapshot).await
    }
}
pub(super) fn frozen_revision(pricing: &Value) -> PlaneResult<&str> {
    let component = if ai_platform_gateway_foundation::request_meter::is_snapshot(pricing) {
        "price_book_request"
    } else {
        "price_book_input"
    };
    pricing[component]["entry"]["key"]["book_version"]
        .as_str()
        .filter(|revision| !revision.trim().is_empty())
        .ok_or(PlaneError::new(
            503,
            "knowledge_model_frozen_revision_invalid",
        ))
}
/// Poll settlement to completion before checking mutable delivery authority.
/// Failure/uncertainty never permits delivery; ACL denial never cancels spend.
pub(super) async fn settle_before_delivery<S, A>(
    settlement: S,
    authorization: A,
) -> PlaneResult<ModelExecution>
where
    S: Future<Output = PlaneResult<ModelExecution>>,
    A: Future<Output = PlaneResult<()>>,
{
    let execution = settlement.await?;
    authorization.await?;
    Ok(execution)
}
#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;
