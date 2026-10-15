//! Connected durable lane; only committed pending rows can become new work.
use crate::{
    capacity_admission::{AdmissionCancellation, AdmissionError},
    dispatcher::DispatchError,
    gateway_dispatcher::GatewayDispatchError,
    task_actor::{TaskActorError, TaskActorValidator},
    task_delivery::{DeliveryClaim, DeliveryIdentity, DeliveryPoll, DeliveryTerminal, FinishResult, TaskDelivery},
    task_manager::{Task, TaskError, TaskManager, TaskStatus},
    task_worker::{TaskDispatcher, TaskWorkerError},
};
use std::{sync::Arc, time::Duration};

pub(crate) struct DurableCollaborators {
    pub delivery: Arc<dyn TaskDelivery>,
    pub actors: Arc<dyn TaskActorValidator>,
    pub dispatcher: Arc<dyn TaskDispatcher>,
    pub manager: Arc<TaskManager>,
}
impl DurableCollaborators {
    pub async fn process_next(&self) -> Result<(), TaskWorkerError> {
        let claim = match self.delivery.claim_next().await.map_err(|_|TaskError::StorageUnavailable)? {
            DeliveryPoll::Claimed(claim) => claim,
            DeliveryPoll::Empty => {tokio::time::sleep(Duration::from_millis(250)).await;return Ok(());},
            DeliveryPoll::ReconciliationRequired {..} | DeliveryPoll::CancelledBeforeDispatch {..} => return Ok(()),
        };
        let current_roles = match self.actors.revalidate(&claim.actor, &claim.envelope).await {
            Ok(roles) => roles,
            Err(error) => {
                // No invocation has started, so this is a known rejection.
                return self.finish(&claim, DeliveryTerminal::Failed(match error {
                    TaskActorError::Revoked => "task_authority_revoked",
                    TaskActorError::Unavailable => "task_authority_unavailable",
                })).await;
            }
        };
        if !self.owns_claim(&claim).await? { return Ok(()); }
        let mut envelope = claim.envelope.clone();
        envelope.roles = current_roles;
        let cancellation = AdmissionCancellation::default();
        let mut guard = CancelOnDrop(Some(cancellation.clone()));
        let invoke = self.dispatcher.invoke(envelope, cancellation);
        tokio::pin!(invoke);
        let result = tokio::select! {
            // Poll the durable fence before the first invoke poll as well.
            biased;
            revoked = self.wait_for_revocation(&claim) => { revoked?; return Ok(()); }
            result = &mut invoke => result,
        };
        guard.0 = None;
        self.finish(&claim, classify_outcome(result)).await
    }
    async fn owns_claim(&self, claim: &DeliveryClaim) -> Result<bool, TaskWorkerError> {
        let observed = self.delivery.observe(&DeliveryIdentity::of(&claim.envelope)).await
            .map_err(|_|TaskError::StorageUnavailable)?.ok_or(TaskError::StateConflict)?;
        Ok(observed.state == "claimed_unknown" && observed.claim_id == Some(claim.claim_id))
    }
    async fn wait_for_revocation(&self, claim: &DeliveryClaim) -> Result<(), TaskWorkerError> {
        loop {
            // Ledger identity/claim is the authority, never a mutable tasks row.
            // Read failure drops invoke and signals the canonical cleanup path.
            if !self.owns_claim(claim).await? { return Ok(()); }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    async fn finish(&self, claim: &DeliveryClaim, outcome: DeliveryTerminal) -> Result<(), TaskWorkerError> {
        let applied = self.delivery.finish(claim, outcome.clone()).await.map_err(|_|TaskError::StorageUnavailable)?;
        match applied {
            FinishResult::Applied(status) if matches!(status,TaskStatus::Completed|TaskStatus::Failed) => {
                // Reconstruct callback from the protected envelope and owned
                // outcome; a broader task-projection writer cannot substitute a
                // callback URL/result between the terminal COMMIT and this read.
                let mut task = Task::new(claim.envelope.task_id.clone(), &claim.envelope.request, claim.admitted_at.naive_utc());
                task.status = status;
                task.started_at = Some(claim.claimed_at.naive_utc().into());
                task.completed_at = Some(chrono::Utc::now().naive_utc().into());
                match outcome {
                    DeliveryTerminal::Completed(response) => task.result = Some(response.into()),
                    DeliveryTerminal::Failed(code) => task.error = Some(code.into()),
                    _ => return Err(TaskError::StateConflict.into()),
                }
                self.manager.send_callback(&task).await;
            }
            FinishResult::ProjectionConflict => return Err(TaskError::StateConflict.into()),
            _ => {},
        }
        // Uncertain COMMIT is an error with no failure overwrite or callback.
        // No outcome here requeues, retries, releases or replaces the claim.
        Ok(())
    }
}
fn classify_outcome(result: Result<crate::protocol::UnifiedResponse,GatewayDispatchError>) -> DeliveryTerminal {
    match result {
        Ok(response) => DeliveryTerminal::Completed(response),
        // A persisted user cancellation is handled by the ledger watcher or
        // rejected terminal fence. Spontaneous dispatcher cancellation may be
        // capacity lease loss after provider dispatch, so preserve uncertainty.
        Err(GatewayDispatchError::Dispatch(DispatchError::Cancelled | DispatchError::CleanupFailed
            | DispatchError::Admission(AdmissionError::Cancelled | AdmissionError::TaskFailed))) => DeliveryTerminal::Unknown,
        Err(GatewayDispatchError::Dispatch(DispatchError::Admission(AdmissionError::Rejected(rejection))))
            if rejection.code == "GATEWAY_CAPACITY_LEASE_LOST" => DeliveryTerminal::Unknown,
        // Session/result metadata/cleanup errors can occur after remote success.
        Err(GatewayDispatchError::Dispatch(DispatchError::UpstreamOutcomeUnknown)
            | GatewayDispatchError::Session(_)
            | GatewayDispatchError::Metadata(_)
            | GatewayDispatchError::Cleanup) => DeliveryTerminal::Unknown,
        Err(_) => DeliveryTerminal::Failed("task_dispatch_failed"),
    }
}
struct CancelOnDrop(Option<AdmissionCancellation>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) { if let Some(signal) = self.0.take() { signal.cancel(); } }
}

#[cfg(test)]
#[path="task_durable_worker_tests.rs"]
mod tests;
