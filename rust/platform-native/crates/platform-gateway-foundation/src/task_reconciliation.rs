//! Explicit same-tenant operator reconciliation. No API can reset a claim to pending.
use super::*;
use serde::Deserialize;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcileRequest {
    pub claim_id: Uuid,
    pub provider_job_id: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileResult {
    Applied(TaskStatus),
    AlreadyResolved,
    FenceRejected,
    ProjectionConflict,
}
impl PgTaskDeliveryLedger {
    pub async fn reconcile(
        &self,
        identity: &DeliveryIdentity,
        operator: &str,
        request: ReconcileRequest,
    ) -> Result<ReconcileResult, DeliveryError> {
        if operator.is_empty() || operator.len() > 255 || !identity.valid() {
            return Err(DeliveryError::InvalidEnvelope);
        }
        let coordinator = self
            .provider_batches
            .get()
            .ok_or(DeliveryError::InvalidEnvelope)?;
        // Proof fields cannot be deserialized from the operator request.
        let proof = coordinator
            .verify_terminal(
                identity,
                request.claim_id,
                request.provider_job_id.as_deref(),
            )
            .await?;
        let identity = proof.identity.clone();
        let claim = proof.claim;
        let operator = operator.to_owned();
        let (status, result, error) = if proof.status == "completed" {
            (
                TaskStatus::Completed,
                Some(TaskResult::from(proof.response)),
                None,
            )
        } else {
            (
                TaskStatus::Failed,
                None,
                Some("provider_batch_terminal_failure".to_owned()),
            )
        };
        let value = saved_result(&result).map_err(|_| DeliveryError::InvalidEnvelope)?;
        let format = result_format(&result);
        let mut operation = self
            .operations
            .acquire()
            .await
            .map_err(|_| DeliveryError::StorageUnavailable)?;
        operation.execute(move|connection|Box::pin(async move{
            let Some(row)=locked_delivery(connection,&identity.task_id).await?else{return Ok(ReconcileResult::FenceRejected)};
            if !matches_claim(&row,&identity,claim){
                let audited:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway.task_reconciliation_audit WHERE task_id=$1 AND claim_id=$2 AND tenant_id=$3 AND provider_job_id=$4 AND receipt_sha256=$5 AND outcome=$6)").bind(&identity.task_id).bind(claim).bind(&identity.tenant_id).bind(&proof.job).bind(&proof.receipt_sha256).bind(status.as_str()).fetch_one(&mut *connection).await?;
                return Ok(if audited{ReconcileResult::AlreadyResolved}else{ReconcileResult::FenceRejected})
            }
            if task_status(connection,&identity).await?.as_deref()!=Some("processing"){return Ok(ReconcileResult::ProjectionConflict)}
            sqlx::query("INSERT INTO gateway.task_reconciliation_audit(task_id,claim_id,tenant_id,operator_user_id,provider_job_id,receipt_sha256,outcome,observed_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8)").bind(&identity.task_id).bind(claim).bind(&identity.tenant_id).bind(&operator).bind(&proof.job).bind(&proof.receipt_sha256).bind(status.as_str()).bind(proof.observed_at).execute(&mut *connection).await?;
            sqlx::query("UPDATE gateway.tasks SET status=$2,result=$3,error=$4,completed_at=clock_timestamp(),updated_at=clock_timestamp(),metadata=(CASE WHEN jsonb_typeof(metadata)='object' THEN metadata ELSE '{}'::jsonb END)||jsonb_build_object('gateway.native_task_result.v1',$5::jsonb) WHERE task_id=$1")
                .bind(&identity.task_id).bind(status.as_str()).bind(value).bind(error).bind(format).execute(&mut *connection).await?;
            sqlx::query("UPDATE gateway.task_deliveries SET state=$3,finished_at=clock_timestamp() WHERE task_id=$1 AND claim_id=$2 AND state='claimed_unknown'").bind(&identity.task_id).bind(claim).bind(status.as_str()).execute(connection).await?;
            Ok(ReconcileResult::Applied(status))
        })).await.map_err(|_|DeliveryError::StorageUnavailable)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operator_cannot_submit_status_result_nonce_or_replay_instruction() {
        for field in [
            "status",
            "result",
            "submission_nonce",
            "reset_pending",
            "retry",
        ] {
            let mut value = json!({"claim_id":Uuid::new_v4()});
            value[field] = json!("forged");
            assert!(serde_json::from_value::<ReconcileRequest>(value).is_err());
        }
    }
}
