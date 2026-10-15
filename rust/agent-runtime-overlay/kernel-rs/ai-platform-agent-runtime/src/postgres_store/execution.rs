//! Fenced execution ownership and immutable original dynamic-tool responses.

use codex_protocol::ThreadId;
use codex_protocol::dynamic_tools::DynamicToolResponse;
use codex_protocol::models::{
    FunctionCallOutputBody, FunctionCallOutputContentItem, FunctionCallOutputPayload, ResponseItem,
};
use codex_rollout::{ResponseItemEnvelope, RolloutItem};
use codex_thread_store::ThreadStoreResult;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use super::{PostgresThreadStore, store_error, thread_uuid};

#[derive(Clone, Debug)]
pub(crate) struct ExecutionClaim {
    pub run_id: Uuid,
    pub thread_id: ThreadId,
    pub owner_id: Uuid,
    pub fence: i64,
    pub context: Value,
}

impl PostgresThreadStore {
    pub(crate) async fn claim_execution(
        &self,
        run_id: Uuid,
        context: Option<Value>,
    ) -> ThreadStoreResult<ExecutionClaim> {
        let row = sqlx::query("SELECT * FROM claim_runtime_execution($1,$2,$3)")
            .bind(run_id)
            .bind(self.instance_id)
            .bind(context)
            .fetch_one(&self.pool)
            .await
            .map_err(store_error)?;
        let claim = ExecutionClaim {
            run_id,
            owner_id: self.instance_id,
            thread_id: ThreadId::from_u128(
                row.try_get::<Uuid, _>("runtime_thread_id")
                    .map_err(store_error)?
                    .as_u128(),
            ),
            fence: row.try_get("fence").map_err(store_error)?,
            context: row.try_get("context").map_err(store_error)?,
        };
        self.execution_claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(claim.thread_id, claim.clone());
        Ok(claim)
    }

    pub(crate) fn execution_claim(&self, run_id: Uuid) -> Option<ExecutionClaim> {
        self.execution_claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .find(|claim| claim.run_id == run_id)
            .cloned()
    }

    pub(crate) async fn assert_execution(&self, run_id: Uuid) -> ThreadStoreResult<ExecutionClaim> {
        let claim = self.execution_claim(run_id).ok_or_else(|| {
            codex_thread_store::ThreadStoreError::Conflict {
                message: "runtime execution is not owned by this process".to_string(),
            }
        })?;
        sqlx::query("SELECT assert_runtime_execution($1,$2,$3)")
            .bind(run_id)
            .bind(claim.owner_id)
            .bind(claim.fence)
            .execute(&self.pool)
            .await
            .map_err(store_error)?;
        Ok(claim)
    }

    pub(crate) async fn heartbeat_execution(
        &self,
        claim: &ExecutionClaim,
    ) -> ThreadStoreResult<bool> {
        let changed = sqlx::query(
            "UPDATE assistant_runtime_execution_owners o SET lease_until=NOW()+INTERVAL '30 seconds',heartbeat_at=NOW() \
             FROM assistant_runs r WHERE o.run_id=$1 AND o.owner_id=$2 AND o.fence=$3 AND o.lease_until>NOW() \
             AND r.run_id=o.run_id AND r.status IN ('running','awaiting_approval') \
             AND EXISTS (SELECT 1 FROM assistant_runtime_model_leases l WHERE l.run_id=r.run_id AND l.status='active' AND l.expires_at>NOW()) \
             AND NOT EXISTS (SELECT 1 FROM assistant_runtime_snapshot_revocations x WHERE x.snapshot_id=r.runtime_snapshot_id)")
            .bind(claim.run_id).bind(claim.owner_id).bind(claim.fence)
            .execute(&self.pool).await.map_err(store_error)?;
        Ok(changed.rows_affected() == 1)
    }

    pub(crate) async fn check_execution_write(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        thread_id: Uuid,
    ) -> ThreadStoreResult<()> {
        let claim = self
            .execution_claims
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&ThreadId::from_u128(thread_id.as_u128()))
            .cloned();
        if let Some(claim) = claim {
            sqlx::query("SELECT assert_runtime_execution_fence($1,$2,$3)")
                .bind(claim.run_id)
                .bind(claim.owner_id)
                .bind(claim.fence)
                .execute(&mut **transaction)
                .await
                .map_err(store_error)?;
        }
        Ok(())
    }

    pub(crate) async fn begin_invocation(
        &self,
        run_id: Uuid,
        params: &codex_app_server_protocol::DynamicToolCallParams,
    ) -> ThreadStoreResult<Option<Value>> {
        let claim = self.assert_execution(run_id).await?;
        let thread_id = Uuid::parse_str(&params.thread_id).map_err(invalid_identity)?;
        let value = serde_json::to_value(params).map_err(super::projection::json_error)?;
        let mut tx = self.pool.begin().await.map_err(store_error)?;
        sqlx::query("SELECT assert_runtime_execution($1,$2,$3)")
            .bind(run_id)
            .bind(claim.owner_id)
            .bind(claim.fence)
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        sqlx::query("INSERT INTO assistant_runtime_invocations(run_id,kernel_thread_id,call_id,params) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
            .bind(run_id).bind(thread_id).bind(&params.call_id).bind(&value).execute(&mut *tx).await.map_err(store_error)?;
        let row=sqlx::query("SELECT params,response FROM assistant_runtime_invocations WHERE run_id=$1 AND kernel_thread_id=$2 AND call_id=$3 FOR UPDATE")
            .bind(run_id).bind(thread_id).bind(&params.call_id).fetch_one(&mut *tx).await.map_err(store_error)?;
        if row.try_get::<Value, _>("params").map_err(store_error)? != value {
            return Err(codex_thread_store::ThreadStoreError::Conflict {
                message: "original tool arguments changed".to_string(),
            });
        }
        let response = row.try_get("response").map_err(store_error)?;
        tx.commit().await.map_err(store_error)?;
        Ok(response)
    }

    pub(crate) async fn finish_invocation(
        &self,
        run_id: Uuid,
        params: &codex_app_server_protocol::DynamicToolCallParams,
        response: &Value,
    ) -> ThreadStoreResult<()> {
        let claim = self
            .execution_claim(run_id)
            .ok_or_else(|| invalid_identity("claim missing"))?;
        let mut tx = self.pool.begin().await.map_err(store_error)?;
        sqlx::query("SELECT assert_runtime_execution_fence($1,$2,$3)")
            .bind(run_id)
            .bind(claim.owner_id)
            .bind(claim.fence)
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        sqlx::query("UPDATE assistant_runtime_invocations SET response=$4,completed_at=COALESCE(completed_at,NOW()) WHERE run_id=$1 AND kernel_thread_id=$2 AND call_id=$3")
            .bind(run_id).bind(Uuid::parse_str(&params.thread_id).map_err(invalid_identity)?)
            .bind(&params.call_id).bind(response).execute(&mut *tx).await.map_err(store_error)?;
        tx.commit().await.map_err(store_error)
    }

    /// Idempotent Core output injection precedes cold history normalization.
    pub(crate) async fn inject_invocation_output(
        &self,
        params: &codex_app_server_protocol::DynamicToolCallParams,
        response: Value,
    ) -> ThreadStoreResult<()> {
        let thread_id = ThreadId::from_string(&params.thread_id).map_err(invalid_identity)?;
        let response: DynamicToolResponse =
            serde_json::from_value(response).map_err(super::projection::json_error)?;
        let item = RolloutItem::ResponseItem(ResponseItemEnvelope::new(
            ResponseItem::FunctionCallOutput {
                id: None,
                call_id: Some(params.call_id.clone()),
                name: None,
                namespace: None,
                output: FunctionCallOutputPayload {
                    body: FunctionCallOutputBody::ContentItems(
                        response
                            .content_items
                            .into_iter()
                            .map(FunctionCallOutputContentItem::from)
                            .collect(),
                    ),
                    success: Some(response.success),
                },
                internal_chat_message_metadata_passthrough: None,
            },
        ));
        // Core may have committed the same output before Runtime crashed.
        let exists:bool=sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assistant_runtime_items WHERE kernel_thread_id=$1 AND event_type='rollout/item' AND payload->>'type'='response_item' AND payload#>>'{payload,type}'='function_call_output' AND payload#>>'{payload,call_id}'=$2)")
            .bind(thread_uuid(thread_id)?).bind(&params.call_id).fetch_one(&self.pool).await.map_err(store_error)?;
        if !exists {
            let key = format!(
                "rollout/recovery-output/{}/{}",
                params.turn_id, params.call_id
            );
            let digest = Sha256::digest(key.as_bytes());
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(&digest[..16]);
            self.append_items_with_keys(thread_id, vec![(key, Uuid::from_bytes(bytes), item)])
                .await?;
        }
        Ok(())
    }
}

fn invalid_identity<E>(_error: E) -> codex_thread_store::ThreadStoreError {
    codex_thread_store::ThreadStoreError::InvalidRequest {
        message: "invalid execution identity".to_string(),
    }
}
