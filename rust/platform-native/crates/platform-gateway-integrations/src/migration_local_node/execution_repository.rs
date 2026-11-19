//! Direct port of src/services/local_node/repository.py; no in-memory authority.
use super::protocol::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{future::Future, pin::Pin};
#[derive(Clone, Debug)]
pub struct RepositoryError(pub String);
impl From<sqlx::Error> for RepositoryError {
    fn from(_: sqlx::Error) -> Self {
        Self("durable Local Node operation rejected".into())
    }
}
pub type RepositoryFuture<'a, T> =
    Pin<Box<dyn Future<Output = std::result::Result<T, RepositoryError>> + Send + 'a>>;
pub trait LocalNodeExecutionRepository: Send + Sync {
    fn reserve_execution<'a>(
        &'a self,
        action: &'a LocalNodeAction,
        binding: Value,
        approval_status: &'a str,
    ) -> RepositoryFuture<'a, ()>;
    fn claim_dispatch<'a>(&'a self, action: &'a LocalNodeAction) -> RepositoryFuture<'a, bool>;
    fn dispatch_fence_for<'a>(
        &'a self,
        action: &'a LocalNodeAction,
    ) -> RepositoryFuture<'a, String>;
    fn append_receipt<'a>(&'a self, receipt: &'a LocalNodeReceipt) -> RepositoryFuture<'a, i64>;
    fn recover_receipts<'a>(
        &'a self,
        action: &'a LocalNodeAction,
        after: i64,
    ) -> RepositoryFuture<'a, Vec<LocalNodeReceipt>>;
    fn mark_side_effect_unknown<'a>(
        &'a self,
        action: &'a LocalNodeAction,
    ) -> RepositoryFuture<'a, ()>;
    fn execution_result<'a>(
        &'a self,
        action: &'a LocalNodeAction,
    ) -> RepositoryFuture<'a, Option<Value>>;
}
pub struct PostgresLocalNodeExecutionRepository {
    pub pool: PgPool,
}
fn invalid(message: &str) -> RepositoryError {
    RepositoryError(message.into())
}
fn uuid(value: &str) -> std::result::Result<&str, RepositoryError> {
    uuid::Uuid::parse_str(value).map_err(|_| invalid("execution identity invalid"))?;
    Ok(value)
}
fn value_text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
impl LocalNodeExecutionRepository for PostgresLocalNodeExecutionRepository {
    fn reserve_execution<'a>(
        &'a self,
        a: &'a LocalNodeAction,
        binding: Value,
        approval: &'a str,
    ) -> RepositoryFuture<'a, ()> {
        Box::pin(async move {
            a.validate().map_err(|e| invalid(&e.message))?;
            if !binding.is_object()
                || (a.effect == "read" && (a.approval_id.is_some() || approval != "not_required"))
                || (a.effect != "read"
                    && (a.approval_id.is_none() || !["pending", "approved"].contains(&approval)))
            {
                return Err(invalid("execution resource binding or approval invalid"));
            }
            sqlx::query("INSERT INTO local_node_executions(execution_id,lease_id,tenant_id,user_id,session_id,run_id,tool_call_id,attempt_id,device_id,channel_id,grant_id,grant_revision,capability_id,capability_revision,operation,arguments,arguments_sha256,idempotency_key,effect,approval_id,approval_status,resource_binding,status) VALUES($1::uuid,$2::uuid,$3,$4,$5,$6::uuid,$7,$8,$9,$10,$11::uuid,$12,$13,$14,$15,$16,$17,$18,$19,$20::uuid,$21,$22,CASE WHEN $21='pending' THEN 'awaiting_approval' ELSE 'published' END)").bind(uuid(&a.execution_id)?).bind(uuid(&a.lease_id)?).bind(&a.scope.tenant_id).bind(&a.scope.user_id).bind(&a.scope.session_id).bind(uuid(&a.run_id)?).bind(&a.tool_call_id).bind(&a.attempt_id).bind(&a.scope.device_id).bind(&a.scope.channel_id).bind(a.grant_id.as_deref()).bind(a.grant_revision).bind(&a.capability_id).bind(a.capability_revision).bind(&a.operation).bind(&a.arguments).bind(a.arguments_sha256.trim_start_matches("sha256:")).bind(&a.idempotency_key).bind(&a.effect).bind(a.approval_id.as_deref()).bind(approval).bind(binding).execute(&self.pool).await?;
            Ok(())
        })
    }
    fn claim_dispatch<'a>(&'a self, a: &'a LocalNodeAction) -> RepositoryFuture<'a, bool> {
        Box::pin(async move {
            a.validate().map_err(|e| invalid(&e.message))?;
            let mut tx = self.pool.begin().await?;
            let row=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(e) FROM local_node_executions e WHERE execution_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND run_id=$5::uuid FOR UPDATE").bind(uuid(&a.execution_id)?).bind(&a.scope.tenant_id).bind(&a.scope.user_id).bind(&a.scope.session_id).bind(uuid(&a.run_id)?).fetch_optional(&mut *tx).await?.ok_or_else(||invalid("execution scope mismatch"))?;
            let binding = &row["resource_binding"];
            if value_text(&row, "lease_id") != uuid(&a.lease_id)?
                || row["tool_call_id"] != a.tool_call_id
                || row["attempt_id"] != a.attempt_id
                || value_text(&row, "arguments_sha256")
                    != a.arguments_sha256.trim_start_matches("sha256:")
                || row["capability_revision"] != a.capability_revision
                || row["capability_id"] != a.capability_id
                || row["effect"] != a.effect
                || row["idempotency_key"] != a.idempotency_key
                || binding["device_id"] != a.scope.device_id
                || binding["channel_id"] != a.scope.channel_id
                || a.grant_id
                    .as_ref()
                    .is_some_and(|g| binding["grant_id"] != *g)
                || a.grant_revision
                    .is_some_and(|r| binding["grant_revision"].as_i64() != Some(r))
                || [
                    "succeeded",
                    "failed",
                    "cancelled",
                    "timeout",
                    "side_effect_unknown",
                ]
                .contains(&value_text(&row, "status"))
            {
                return Err(invalid("execution binding rejected"));
            }
            if !row["dispatch_fence"].is_null() {
                tx.commit().await?;
                return Ok(false);
            }
            if a.effect != "read" && row["approval_status"] != "approved" {
                return Err(invalid("approval required"));
            }
            let fence = uuid::Uuid::new_v4().to_string();
            let claimed = sqlx::query_scalar::<_, bool>(
                "SELECT claim_local_node_dispatch($1::uuid,$2,$3,$4,$5::uuid,$6)",
            )
            .bind(&a.execution_id)
            .bind(&a.scope.tenant_id)
            .bind(&a.scope.user_id)
            .bind(&a.scope.session_id)
            .bind(fence)
            .bind(&a.effect)
            .fetch_one(&mut *tx)
            .await?;
            if !claimed {
                return Err(invalid("dispatch fence rejected"));
            }
            tx.commit().await?;
            Ok(true)
        })
    }
    fn dispatch_fence_for<'a>(&'a self, a: &'a LocalNodeAction) -> RepositoryFuture<'a, String> {
        Box::pin(async move {
            sqlx::query_scalar::<_,Option<String>>("SELECT dispatch_fence::text FROM local_node_executions WHERE execution_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4").bind(uuid(&a.execution_id)?).bind(&a.scope.tenant_id).bind(&a.scope.user_id).bind(&a.scope.session_id).fetch_optional(&self.pool).await?.flatten().ok_or_else(||invalid("dispatch fence unavailable"))
        })
    }
    fn append_receipt<'a>(&'a self, r: &'a LocalNodeReceipt) -> RepositoryFuture<'a, i64> {
        Box::pin(async move {
            r.validate(0).map_err(|e| invalid(&e.message))?;
            uuid(&r.dispatch_fence)?;
            let mut tx = self.pool.begin().await?;
            let row=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(e) FROM local_node_executions e WHERE execution_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 FOR UPDATE").bind(uuid(&r.execution_id)?).bind(&r.tenant_id).bind(&r.user_id).bind(&r.session_id).fetch_optional(&mut *tx).await?.ok_or_else(||invalid("receipt dispatch fence or owner mismatch"))?;
            if row["dispatch_fence"] != r.dispatch_fence
                || row["device_id"] != r.device_id
                || r.channel_id
                    .as_ref()
                    .is_some_and(|v| row["channel_id"] != *v)
            {
                return Err(invalid("receipt fence, owner or channel mismatch"));
            }
            let cursor = row["receipt_cursor"]
                .as_i64()
                .ok_or_else(|| invalid("receipt cursor invalid"))?;
            if r.sequence <= cursor {
                let duplicate = sqlx::query_scalar::<_, i32>(
                    "SELECT 1 FROM local_node_receipts WHERE execution_id=$1::uuid AND sequence=$2",
                )
                .bind(&r.execution_id)
                .bind(r.sequence)
                .fetch_optional(&mut *tx)
                .await?;
                if duplicate.is_some() {
                    tx.commit().await?;
                    return Ok(cursor);
                }
                return Err(invalid("receipt cursor moved"));
            }
            if r.sequence != cursor + 1 {
                return Err(invalid("receipt sequence gap"));
            }
            let event_id = receipt_event_id(r);
            let channel = value_text(&row, "channel_id");
            sqlx::query("INSERT INTO local_node_receipts(execution_id,event_id,tenant_id,user_id,session_id,device_id,channel_id,dispatch_fence,sequence,event,status,payload) VALUES($1::uuid,$2::uuid,$3,$4,$5,$6,$7,$8::uuid,$9,$10,$11,$12)").bind(&r.execution_id).bind(&event_id).bind(&r.tenant_id).bind(&r.user_id).bind(&r.session_id).bind(&r.device_id).bind(channel).bind(&r.dispatch_fence).bind(r.sequence).bind(&r.event).bind(r.status.as_str()).bind(&r.payload).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO local_node_events(event_id,execution_id,tenant_id,user_id,device_id,channel_id,sequence,event,status,payload) VALUES($1::uuid,$2::uuid,$3,$4,$5,$6,$7,$8,$9,$10)").bind(&event_id).bind(&r.execution_id).bind(&r.tenant_id).bind(&r.user_id).bind(&r.device_id).bind(channel).bind(r.sequence).bind(&r.event).bind(r.status.as_str()).bind(&r.payload).execute(&mut *tx).await?;
            sqlx::query("UPDATE local_node_executions SET receipt_cursor=$2,status=$3,result_summary=CASE WHEN $3 IN ('succeeded','failed') THEN $4::jsonb ELSE result_summary END,terminal_at=CASE WHEN $3 IN ('succeeded','failed','cancelled','timeout','side_effect_unknown') THEN now() ELSE terminal_at END,updated_at=now() WHERE execution_id=$1::uuid").bind(&r.execution_id).bind(r.sequence).bind(r.status.as_str()).bind(&r.payload).execute(&mut *tx).await?;
            sqlx::query("UPDATE local_node_channels SET receipt_cursor=GREATEST(receipt_cursor,$2),last_seen_at=now() WHERE channel_id=$1 AND tenant_id=$3 AND user_id=$4 AND device_id=$5").bind(channel).bind(r.sequence).bind(&r.tenant_id).bind(&r.user_id).bind(&r.device_id).execute(&mut *tx).await?;
            tx.commit().await?;
            Ok(r.sequence)
        })
    }
    fn recover_receipts<'a>(
        &'a self,
        a: &'a LocalNodeAction,
        after: i64,
    ) -> RepositoryFuture<'a, Vec<LocalNodeReceipt>> {
        Box::pin(async move {
            let rows=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(r) FROM local_node_receipts r WHERE execution_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND sequence>$5 ORDER BY sequence").bind(uuid(&a.execution_id)?).bind(&a.scope.tenant_id).bind(&a.scope.user_id).bind(&a.scope.session_id).bind(after).fetch_all(&self.pool).await?;
            rows.into_iter()
                .map(|row| {
                    serde_json::from_value(row).map_err(|_| invalid("stored receipt invalid"))
                })
                .collect()
        })
    }
    fn mark_side_effect_unknown<'a>(&'a self, a: &'a LocalNodeAction) -> RepositoryFuture<'a, ()> {
        Box::pin(async move {
            let count=sqlx::query("UPDATE local_node_executions SET status='side_effect_unknown',terminal_at=now(),updated_at=now() WHERE execution_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND dispatch_fence IS NOT NULL AND status NOT IN ('succeeded','failed','cancelled','timeout','side_effect_unknown')").bind(uuid(&a.execution_id)?).bind(&a.scope.tenant_id).bind(&a.scope.user_id).bind(&a.scope.session_id).execute(&self.pool).await?.rows_affected();
            if count == 0 {
                return Err(invalid("side effect transition rejected"));
            }
            Ok(())
        })
    }
    fn execution_result<'a>(
        &'a self,
        a: &'a LocalNodeAction,
    ) -> RepositoryFuture<'a, Option<Value>> {
        Box::pin(async move {
            let row=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(e) FROM local_node_executions e WHERE execution_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND run_id=$5::uuid").bind(uuid(&a.execution_id)?).bind(&a.scope.tenant_id).bind(&a.scope.user_id).bind(&a.scope.session_id).bind(uuid(&a.run_id)?).fetch_optional(&self.pool).await?.ok_or_else(||invalid("execution scope mismatch"))?;
            if row["device_id"] != a.scope.device_id
                || row["channel_id"] != a.scope.channel_id
                || value_text(&row, "arguments_sha256")
                    != a.arguments_sha256.trim_start_matches("sha256:")
            {
                return Err(invalid("execution scope or arguments mismatch"));
            }
            if row["dispatch_fence"].is_null()
                || ![
                    "succeeded",
                    "failed",
                    "cancelled",
                    "timeout",
                    "side_effect_unknown",
                ]
                .contains(&value_text(&row, "status"))
            {
                return Ok(None);
            }
            let mut result = if row["result_summary"].is_object() {
                row["result_summary"].clone()
            } else {
                json!({})
            };
            result["status"] = row["status"].clone();
            result["last_sequence"] = row["receipt_cursor"].clone();
            Ok(Some(result))
        })
    }
}
/// Preserve Python uuid.uuid5(uuid.NAMESPACE_URL, ...) event identities.
fn receipt_event_id(receipt: &LocalNodeReceipt) -> String {
    uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_URL,
        format!(
            "local-node:{}:{}:{}",
            receipt.execution_id, receipt.sequence, receipt.event
        )
        .as_bytes(),
    )
    .to_string()
}
