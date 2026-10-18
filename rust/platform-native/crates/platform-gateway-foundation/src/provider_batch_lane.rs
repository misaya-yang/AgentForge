//! Internal-only Batch admission marker; no HTTP field selects zero-token admission.
use crate::{
    capacity::{CapacityRequest, CapacityResolver},
    capacity_admission::AdmissionCancellation,
    dispatcher::{DispatchAdmission, DispatchContext, DispatchService, DispatchSettlement},
    provider_batch::{BatchError, BatchPlan, BatchProvider},
    task_delivery::DeliveryClaim,
};
use serde_json::Value;
use std::sync::Arc;
/// Only this module constructs the marker after a protected claimed Batch intent.
pub struct BatchAdmission {
    _private: (),
}
pub struct BatchLane {
    admission: Arc<dyn DispatchAdmission>,
    capacity: Arc<CapacityResolver>,
}
impl BatchLane {
    pub fn new(admission: Arc<dyn DispatchAdmission>, capacity: Arc<CapacityResolver>) -> Self {
        Self {
            admission,
            capacity,
        }
    }
    pub async fn submit(
        &self,
        claim: &DeliveryClaim,
        service: &DispatchService,
        provider: Arc<dyn BatchProvider>,
        plan: &BatchPlan,
        nonce: uuid::Uuid,
    ) -> Result<Value, BatchError> {
        let request = &claim.envelope.request;
        let context = DispatchContext {
            request_id: request.request_id.clone(),
            tenant_id: request.tenant_id.clone(),
            user_id: request.user_id.clone(),
        };
        let budgets = self
            .capacity
            .resolve(&CapacityRequest {
                tenant_id: request.tenant_id.clone(),
                service_id: request.service_id.clone(),
                request_class: "provider_batch".into(),
                upstream_group: Value::Null,
                provider_id: Value::String(plan.provider_id.clone()),
                is_admin_read: false,
                service_config: service.capacity_config.clone(),
                resource_kind: "request".into(),
            })
            .map_err(|_| BatchError::NotDispatched)?;
        let lease = self
            .admission
            .acquire_provider_batch(
                &BatchAdmission { _private: () },
                &context,
                service,
                &budgets,
                request,
                &claim.claim_id.to_string(),
            )
            .await
            .map_err(|_| BatchError::NotDispatched)?;
        let cancellation = AdmissionCancellation::default();
        if lease.bind_owner(cancellation.clone()).is_err() {
            let _ = lease.release().await;
            return Err(BatchError::UnknownSubmission);
        }
        let result = tokio::select! {
            biased;
            _ = lease.wait_lost() => Err(BatchError::UnknownSubmission),
            _ = cancellation.cancelled() => Err(BatchError::UnknownSubmission),
            result = async {
                lease.mark_dispatched().await.map_err(|_| BatchError::UnknownSubmission)?;
                // Only this lane can prove NotDispatched. Once polled, even a
                // provider returning that variant must retain the unknown hold.
                provider.submit(plan, nonce).await.map_err(|_| BatchError::UnknownSubmission)
            } => result,
        };
        // Even an accepted asynchronous job has no durable terminal usage yet.
        let marked = lease.mark_completion(DispatchSettlement::Unknown).await;
        let released = lease.release().await;
        if marked.is_err() || released.is_err() {
            return Err(BatchError::UnknownSubmission);
        }
        result
    }
}
#[cfg(test)]
#[path = "provider_batch_lane_tests.rs"]
mod tests;
