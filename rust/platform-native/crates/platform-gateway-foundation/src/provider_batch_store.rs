//! Protected submission intents share generic task identity/claim authority.
use crate::{
    dispatch_preflight::{DispatchPreflight, PreflightAuthority, PreflightMode},
    postgres_operation::PgOperationStorage,
    protocol::UnifiedResponse,
    provider_batch::*,
    task_delivery::{DeliveryClaim, DeliveryError, DeliveryIdentity, DeliveryTerminal},
    task_manager::TaskEnvelope,
};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{future::Future, sync::Arc};
use uuid::Uuid;

fn spawn_hold_release(
    release: impl Future<Output = Result<(), BatchError>> + Send + 'static,
) -> tokio::task::JoinHandle<Result<(), BatchError>> {
    // Dropping the caller's JoinHandle detaches this task rather than aborting
    // a known-safe release while it waits for the shared user or row locks.
    tokio::spawn(release)
}

#[derive(Clone)]
pub struct BatchCoordinator {
    pub(crate) pool: PgPool,
    operations: PgOperationStorage,
    preflight: Arc<DispatchPreflight>,
    providers: Arc<BatchProviders>,
    accounting: Arc<dyn BatchAccounting>,
    lane: Arc<crate::provider_batch_lane::BatchLane>,
}
#[derive(Clone)]
pub(crate) struct StoredBatch {
    pub identity: DeliveryIdentity,
    pub claim: Uuid,
    pub plan: BatchPlan,
    pub nonce: Uuid,
    pub job: Option<String>,
}
pub(crate) struct VerifiedBatchOutcome {
    pub identity: DeliveryIdentity,
    pub claim: Uuid,
    pub job: String,
    pub status: &'static str,
    pub receipt_sha256: String,
    pub response: UnifiedResponse,
    pub observed_at: chrono::DateTime<chrono::Utc>,
}
impl BatchCoordinator {
    pub fn new(
        pool: PgPool,
        control: PgPool,
        preflight: Arc<DispatchPreflight>,
        providers: Arc<BatchProviders>,
        accounting: Arc<dyn BatchAccounting>,
        lane: Arc<crate::provider_batch_lane::BatchLane>,
    ) -> Result<Self, DeliveryError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool.clone(), control)
                .map_err(|_| DeliveryError::StorageUnavailable)?,
            pool,
            preflight,
            providers,
            accounting,
            lane,
        })
    }
    pub async fn admit(&self, envelope: &TaskEnvelope) -> Result<(), DeliveryError> {
        let plan = BatchPlan::from_envelope(envelope)
            .map_err(|_| DeliveryError::InvalidEnvelope)?
            .ok_or(DeliveryError::InvalidEnvelope)?;
        let authority = PreflightAuthority::from_resolved_context(
            &envelope.request.user_id,
            &envelope.request.tenant_id,
            &envelope.roles,
        );
        let ready = self
            .preflight
            .prepare(
                envelope.request.clone(),
                &authority,
                envelope.client_ip.as_deref(),
                PreflightMode::Submit,
            )
            .await
            .map_err(|_| DeliveryError::InvalidEnvelope)?;
        let service = ready.service.snapshot();
        let policy = &service.metadata["provider_batch"];
        if policy["enabled"] != true
            || policy["provider_id"] != plan.provider_id
            || policy["model_id"] != plan.model_id
            || policy["endpoint"] != plan.endpoint
        {
            return Err(DeliveryError::InvalidEnvelope);
        }
        self.providers
            .get(&envelope.request.tenant_id, &plan.provider_id)
            .map_err(|_| DeliveryError::InvalidEnvelope)?;
        let permitted:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway.task_batch_inputs WHERE tenant_id=$1 AND user_id=$2 AND service_id=$3 AND provider_id=$4 AND input_file_id=$5 AND endpoint=$6 AND model_id=$7 AND expires_at>clock_timestamp())")
            .bind(&envelope.request.tenant_id).bind(&envelope.request.user_id).bind(&envelope.request.service_id).bind(&plan.provider_id).bind(&plan.input_file_id).bind(&plan.endpoint).bind(&plan.model_id).fetch_one(&self.pool).await.map_err(|_|DeliveryError::StorageUnavailable)?;
        if !permitted {
            return Err(DeliveryError::InvalidEnvelope);
        }
        self.accounting
            .admit(&DeliveryIdentity::of(envelope), &plan)
            .await
            .map_err(|_| DeliveryError::InvalidEnvelope)?;
        Ok(())
    }
    /// INSERT is the one submission permission. It commits before HTTP. A lost
    /// INSERT/POST/confirmation response never permits another POST.
    pub async fn submit_claim(&self, claim: &DeliveryClaim) -> Result<(), DeliveryError> {
        self.submit_claim_outcome(claim).await.map(|_| ())
    }
    pub(crate) async fn submit_claim_outcome(
        &self,
        claim: &DeliveryClaim,
    ) -> Result<DeliveryTerminal, DeliveryError> {
        self.admit(&claim.envelope).await?;
        let identity = DeliveryIdentity::of(&claim.envelope);
        let plan = BatchPlan::from_envelope(&claim.envelope)
            .map_err(|_| DeliveryError::InvalidEnvelope)?
            .ok_or(DeliveryError::InvalidEnvelope)?;
        let saved = identity.clone();
        let claimed = claim.claim_id;
        let nonce = Uuid::new_v4();
        let stored = plan.clone();
        let mut operation = self
            .operations
            .acquire()
            .await
            .map_err(|_| DeliveryError::StorageUnavailable)?;
        let inserted=operation.execute(move|connection|Box::pin(async move{
            let valid:Option<String>=sqlx::query_scalar("SELECT task_id FROM gateway.task_deliveries WHERE task_id=$1 AND request_id=$2 AND service_id=$3 AND user_id=$4 AND tenant_id=$5 AND claim_id=$6 AND state='claimed_unknown' FOR UPDATE").bind(&saved.task_id).bind(&saved.request_id).bind(&saved.service_id).bind(&saved.user_id).bind(&saved.tenant_id).bind(claimed).fetch_optional(&mut *connection).await?;
            if valid.is_none(){return Ok(false)}
            let row=sqlx::query("INSERT INTO gateway.task_provider_batches(task_id,request_id,service_id,user_id,tenant_id,claim_id,provider_id,model_id,input_file_id,endpoint,input_sha256,submission_nonce) SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,i.input_sha256,$11 FROM gateway.task_batch_inputs i WHERE i.tenant_id=$5 AND i.user_id=$4 AND i.service_id=$3 AND i.provider_id=$7 AND i.model_id=$8 AND i.input_file_id=$9 AND i.endpoint=$10 AND i.expires_at>clock_timestamp() ON CONFLICT(task_id) DO NOTHING RETURNING task_id")
                .bind(&saved.task_id).bind(&saved.request_id).bind(&saved.service_id).bind(&saved.user_id).bind(&saved.tenant_id).bind(claimed).bind(&stored.provider_id).bind(&stored.model_id).bind(&stored.input_file_id).bind(&stored.endpoint).bind(nonce).fetch_optional(connection).await?;Ok(row.is_some())
        })).await.map_err(|_|DeliveryError::StorageUnavailable)?;
        if !inserted {
            let stored = self.load(&identity, claimed).await?;
            let released = crate::provider_batch_quota::was_released(
                &self.pool,
                &identity,
                claimed,
                stored.nonce,
            )
            .await
            .map_err(|_| DeliveryError::StorageUnavailable)?;
            return Ok(if released {
                DeliveryTerminal::Failed("provider_batch_not_dispatched")
            } else {
                DeliveryTerminal::Unknown
            });
        } // Retain existing intent; observation only thereafter.
        let provider = self
            .providers
            .get(&identity.tenant_id, &plan.provider_id)
            .map_err(|_| DeliveryError::StorageUnavailable)?;
        let service = self
            .preflight
            .registry()
            .get(&identity.service_id)
            .await
            .map_err(|_| DeliveryError::StorageUnavailable)?
            .ok_or(DeliveryError::InvalidEnvelope)?;
        let service = service
            .snapshot()
            .dispatch_view()
            .map_err(|_| DeliveryError::InvalidEnvelope)?;
        self.accounting
            .reserve(&identity, claimed, &plan)
            .await
            .map_err(|_| DeliveryError::StorageUnavailable)?;
        let receipt = match self
            .lane
            .submit(claim, &service, provider, &plan, nonce)
            .await
        {
            Ok(receipt) => receipt,
            Err(BatchError::NotDispatched) => {
                let pool = self.pool.clone();
                let release_identity = identity.clone();
                // Establish independent ownership before any further await:
                // worker revocation can drop this invocation at its next poll.
                let release = spawn_hold_release(async move {
                    crate::provider_batch_quota::release_not_dispatched(
                        &pool,
                        &release_identity,
                        claimed,
                        nonce,
                    )
                    .await
                });
                release
                    .await
                    .map_err(|_| DeliveryError::StorageUnavailable)?
                    .map_err(|_| DeliveryError::StorageUnavailable)?;
                return Ok(DeliveryTerminal::Failed("provider_batch_not_dispatched"));
            }
            Err(_) => return Err(DeliveryError::StorageUnavailable),
        };
        let job = verify_receipt(&receipt, &plan, nonce, None)
            .map_err(|_| DeliveryError::InvalidEnvelope)?;
        self.confirm(&identity, claimed, nonce, &job).await?;
        Ok(DeliveryTerminal::Unknown)
    }
    async fn confirm(
        &self,
        identity: &DeliveryIdentity,
        claim: Uuid,
        nonce: Uuid,
        job: &str,
    ) -> Result<(), DeliveryError> {
        let identity = identity.clone();
        let job = job.to_owned();
        let mut operation = self
            .operations
            .acquire()
            .await
            .map_err(|_| DeliveryError::StorageUnavailable)?;
        let changed=operation.execute(move|connection|Box::pin(async move{let rows=sqlx::query("UPDATE gateway.task_provider_batches SET provider_job_id=$8,confirmed_at=clock_timestamp() WHERE task_id=$1 AND request_id=$2 AND service_id=$3 AND user_id=$4 AND tenant_id=$5 AND claim_id=$6 AND submission_nonce=$7 AND (provider_job_id IS NULL OR provider_job_id=$8)").bind(&identity.task_id).bind(&identity.request_id).bind(&identity.service_id).bind(&identity.user_id).bind(&identity.tenant_id).bind(claim).bind(nonce).bind(job).execute(connection).await?;Ok(rows.rows_affected()==1)})).await.map_err(|_|DeliveryError::StorageUnavailable)?;
        if changed {
            Ok(())
        } else {
            Err(DeliveryError::FenceRejected)
        }
    }
    async fn load(
        &self,
        identity: &DeliveryIdentity,
        claim: Uuid,
    ) -> Result<StoredBatch, DeliveryError> {
        let row=sqlx::query("SELECT provider_id,model_id,input_file_id,endpoint,submission_nonce,provider_job_id FROM gateway.task_provider_batches WHERE task_id=$1 AND request_id=$2 AND service_id=$3 AND user_id=$4 AND tenant_id=$5 AND claim_id=$6").bind(&identity.task_id).bind(&identity.request_id).bind(&identity.service_id).bind(&identity.user_id).bind(&identity.tenant_id).bind(claim).fetch_optional(&self.pool).await.map_err(|_|DeliveryError::StorageUnavailable)?.ok_or(DeliveryError::FenceRejected)?;
        let result = StoredBatch {
            identity: identity.clone(),
            claim,
            plan: BatchPlan {
                provider_id: row
                    .try_get("provider_id")
                    .map_err(|_| DeliveryError::InvalidEnvelope)?,
                model_id: row
                    .try_get("model_id")
                    .map_err(|_| DeliveryError::InvalidEnvelope)?,
                input_file_id: row
                    .try_get("input_file_id")
                    .map_err(|_| DeliveryError::InvalidEnvelope)?,
                endpoint: row
                    .try_get("endpoint")
                    .map_err(|_| DeliveryError::InvalidEnvelope)?,
            },
            nonce: row
                .try_get("submission_nonce")
                .map_err(|_| DeliveryError::InvalidEnvelope)?,
            job: row
                .try_get("provider_job_id")
                .map_err(|_| DeliveryError::InvalidEnvelope)?,
        };
        result
            .plan
            .validate()
            .map_err(|_| DeliveryError::InvalidEnvelope)?;
        Ok(result)
    }
    /// A caller supplies only a job-id hint. Status/result/nonce/input identity
    /// always come from the stored intent and the authenticated provider GET.
    pub(crate) async fn verify_terminal(
        &self,
        identity: &DeliveryIdentity,
        claim: Uuid,
        hint: Option<&str>,
    ) -> Result<VerifiedBatchOutcome, DeliveryError> {
        let stored = self.load(identity, claim).await?;
        if stored.job.as_deref().zip(hint).is_some_and(|(a, b)| a != b) {
            return Err(DeliveryError::InvalidEnvelope);
        }
        let job = stored
            .job
            .as_deref()
            .or(hint)
            .ok_or(DeliveryError::InvalidEnvelope)?;
        let provider = self
            .providers
            .get(&identity.tenant_id, &stored.plan.provider_id)
            .map_err(|_| DeliveryError::StorageUnavailable)?;
        let receipt = provider
            .observe(job)
            .await
            .map_err(|_| DeliveryError::StorageUnavailable)?;
        let job = verify_receipt(&receipt, &stored.plan, stored.nonce, Some(job))
            .map_err(|_| DeliveryError::InvalidEnvelope)?;
        let status = terminal_status(&receipt).map_err(|_| DeliveryError::InvalidEnvelope)?;
        self.accounting
            .settle(identity, claim, &stored.plan, &receipt)
            .await
            .map_err(|_| DeliveryError::StorageUnavailable)?;
        self.confirm(identity, claim, stored.nonce, &job).await?;
        use sha2::{Digest, Sha256};
        let receipt_sha256 = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&receipt).map_err(|_| DeliveryError::InvalidEnvelope)?
            )
        );
        let mut response = UnifiedResponse::new(&identity.request_id, "completed", vec![]);
        response.task_id = Some(identity.task_id.clone());
        response.metadata = Some(serde_json::Map::from_iter([(
            "provider_batch".into(),
            json!({"job_id":job,"status":receipt["status"],"output_file_id":receipt["output_file_id"],"error_file_id":receipt["error_file_id"],"request_counts":receipt["request_counts"]}),
        )]));
        Ok(VerifiedBatchOutcome {
            identity: stored.identity,
            claim: stored.claim,
            job,
            status,
            receipt_sha256,
            response,
            observed_at: chrono::Utc::now(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn known_not_dispatched_release_survives_caller_cancellation_while_waiting() {
        let (entered, waiting) = oneshot::channel();
        let (unlock, locked) = oneshot::channel();
        let (completed, completion) = oneshot::channel();
        let caller = tokio::spawn(async move {
            let release = spawn_hold_release(async move {
                entered.send(()).unwrap();
                locked.await.unwrap();
                completed.send(()).unwrap();
                Ok(())
            });
            release.await.unwrap()
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        unlock
            .send(())
            .expect("owned release must retain its lock waiter");
        tokio::time::timeout(std::time::Duration::from_secs(1), completion)
            .await
            .unwrap()
            .unwrap();
    }
}
