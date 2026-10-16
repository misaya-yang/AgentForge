//! PostgreSQL authority for the native Local Node v2 companion channel.
//!
//! This implementation is inert until the separately reviewed 023 credential
//! admission exists. Every action is reconciled with the persisted Worker
//! execution, active Runtime lease and immutable snapshot before it is queued.
use super::local_node_channel::{
    ChannelFuture, ChannelStoreError, CompanionReceipt, HeartbeatRequest, HeartbeatResult,
    LocalNodeChannelRepository, PairingRedeemRequest, PendingClaim, PendingControl, WorkerContext,
    WorkerDescriptor, WorkerDispatchRequest, WorkerReceiptEvent,
};
use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::BTreeSet;
use uuid::Uuid;

const TERMINAL: &[&str] = &[
    "succeeded",
    "failed",
    "cancelled",
    "timeout",
    "side_effect_unknown",
];

#[derive(Clone)]
pub struct PostgresLocalNodeChannelRepository {
    pub pool: PgPool,
}

impl PostgresLocalNodeChannelRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn execution_source(&self, context: &WorkerContext) -> Result<Value, ChannelStoreError> {
        let execution_id = uuid_text(&context.execution_id)?;
        let run_id = uuid_text(&context.run_id)?;
        let lease_id = uuid_text(&context.lease_id)?;
        sqlx::query_scalar::<_, Value>(
            "SELECT jsonb_build_object('execution',to_jsonb(e),'lease',to_jsonb(l),'snapshot',to_jsonb(s)) \
             FROM assistant.assistant_capability_executions e \
             JOIN assistant.assistant_runtime_model_leases l \
               ON l.lease_id=e.lease_id AND l.run_id=e.run_id \
              AND l.tenant_id=e.tenant_id AND l.user_id=e.user_id AND l.session_id=e.session_id \
             JOIN assistant.assistant_runtime_snapshots s \
               ON s.snapshot_id=l.snapshot_id AND s.run_id=l.run_id \
              AND s.tenant_id=l.tenant_id AND s.user_id=l.user_id AND s.session_id=l.session_id \
             LEFT JOIN assistant.assistant_runtime_snapshot_revocations r \
               ON r.snapshot_id=s.snapshot_id AND r.tenant_id=s.tenant_id \
              AND r.user_id=s.user_id AND r.session_id=s.session_id \
             WHERE e.execution_id=$1::uuid AND e.lease_id=$2::uuid AND e.run_id=$3::uuid \
               AND e.tenant_id=$4 AND e.user_id=$5 AND e.session_id=$6 \
               AND e.tool_call_id=$7 AND e.attempt_id=$8 \
               AND e.capability_id=$9 AND e.capability_revision=$10 \
               AND e.effect=$11 AND l.capability_revision=e.capability_revision \
               AND l.status='active' AND l.revoked_at IS NULL AND l.expires_at>now() \
               AND (s.expires_at IS NULL OR s.expires_at>now()) AND r.snapshot_id IS NULL \
               AND e.resource_binding->>'snapshot_id'=s.snapshot_id::text \
               AND e.status IN ('dispatched','running')",
        )
        .bind(execution_id)
        .bind(lease_id)
        .bind(run_id)
        .bind(&context.tenant_id)
        .bind(&context.user_id)
        .bind(&context.session_id)
        .bind(&context.tool_call_id)
        .bind(&context.attempt_id)
        .bind(&context.capability_id)
        .bind(i64::try_from(context.capability_revision).map_err(|_| ChannelStoreError::Rejected)?)
        .bind(&context.effect)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| ChannelStoreError::Unavailable)?
        .ok_or(ChannelStoreError::Rejected)
    }

    async fn current_channel(
        &self,
        tenant_id: &str,
        user_id: &str,
        device_id: &str,
    ) -> Result<String, ChannelStoreError> {
        sqlx::query_scalar::<_, String>(
            "SELECT c.channel_id FROM gateway.local_node_device_credentials c \
             JOIN gateway.local_node_devices d ON d.device_id=c.device_id \
              AND d.tenant_id=c.tenant_id AND d.user_id=c.user_id \
             JOIN gateway.local_node_channels ch ON ch.channel_id=c.channel_id \
              AND ch.device_id=c.device_id AND ch.tenant_id=c.tenant_id AND ch.user_id=c.user_id \
             WHERE c.tenant_id=$1 AND c.user_id=$2 AND c.device_id=$3 \
               AND c.revoked_at IS NULL AND c.expires_at>now() \
               AND ch.status='online' AND d.status='online' AND d.revoked_at IS NULL \
             ORDER BY c.created_at DESC LIMIT 1",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(device_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| ChannelStoreError::Unavailable)?
        .ok_or(ChannelStoreError::Rejected)
    }

    async fn current_grant(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        tenant_id: &str,
        user_id: &str,
        session_id: &str,
        device_id: &str,
        grant_id: Option<&str>,
        required_capabilities: &[&str],
    ) -> Result<Value, ChannelStoreError> {
        let capabilities = json!(required_capabilities);
        let mut query = sqlx::query_scalar::<_, Value>(
            "SELECT to_jsonb(g) FROM gateway.local_node_grants g \
             WHERE g.tenant_id=$1 AND g.user_id=$2 AND g.device_id=$3 \
               AND g.kind='workspace' AND g.status='active' AND g.revoked_at IS NULL \
               AND (g.expires_at IS NULL OR g.expires_at>now()) \
               AND (g.session_id IS NULL OR g.session_id=$4) \
               AND g.capabilities @> $5::jsonb \
               AND ($6::uuid IS NULL OR g.grant_id=$6::uuid) \
             ORDER BY g.created_at DESC LIMIT 2 FOR SHARE",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(device_id)
        .bind(session_id)
        .bind(capabilities)
        .bind(grant_id.map(uuid_text).transpose()?)
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| ChannelStoreError::Unavailable)?;
        if query.len() != 1 {
            return Err(ChannelStoreError::Rejected);
        }
        Ok(query.remove(0))
    }

    async fn validate_worker_scope(
        &self,
        context: &WorkerContext,
        device_id: &str,
    ) -> Result<(Value, String), ChannelStoreError> {
        let source = self.execution_source(context).await?;
        let binding = &source["execution"]["resource_binding"];
        if !resource_binding_device_matches(binding, device_id) {
            return Err(ChannelStoreError::Rejected);
        }
        let channel = self
            .current_channel(&context.tenant_id, &context.user_id, device_id)
            .await?;
        Ok((source, channel))
    }

    async fn append_device_receipt(
        tx: &mut Transaction<'_, Postgres>,
        receipt: &CompanionReceipt,
        tenant_id: &str,
        user_id: &str,
        session_id: &str,
        device_id: &str,
        channel_id: &str,
    ) -> Result<(), ChannelStoreError> {
        let execution_id = receipt
            .action_id
            .as_deref()
            .ok_or(ChannelStoreError::Rejected)?;
        let execution_uuid = uuid_text(execution_id)?;
        let row = sqlx::query_scalar::<_, Value>(
            "SELECT to_jsonb(e) FROM gateway.local_node_executions e \
             WHERE e.execution_id=$1::uuid AND e.tenant_id=$2 AND e.user_id=$3 \
               AND e.session_id=$4 AND e.device_id=$5 AND e.channel_id=$6 \
               AND e.dispatch_fence IS NOT NULL FOR UPDATE",
        )
        .bind(execution_uuid)
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .bind(device_id)
        .bind(channel_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| ChannelStoreError::Unavailable)?
        .ok_or(ChannelStoreError::Rejected)?;
        let previous = row["receipt_cursor"]
            .as_i64()
            .ok_or(ChannelStoreError::Rejected)?;
        let sequence = previous.checked_add(1).ok_or(ChannelStoreError::Rejected)?;
        let status = match receipt.status.as_deref() {
            Some("succeeded") => "succeeded",
            Some("failed") => "failed",
            Some("cancelled") => "cancelled",
            Some("timeout") => "timeout",
            Some("unknown" | "side_effect_unknown") => "side_effect_unknown",
            _ => "running",
        };
        if TERMINAL.contains(&row["status"].as_str().unwrap_or("")) {
            return Err(ChannelStoreError::Rejected);
        }
        let event_id = Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!(
                "local-node:{execution_id}:{sequence}:{}",
                receipt.event_type
            )
            .as_bytes(),
        )
        .to_string();
        let mut payload = json!({
            "device_event_id":receipt.event_id,
            "device_sequence":receipt.sequence,
            "occurred_at":receipt.occurred_at,
        });
        if let Some(value) = &receipt.result {
            payload["result"] = value.clone();
        }
        if let Some(value) = &receipt.result_digest {
            payload["result_digest"] = json!(value);
        }
        if let Some(value) = &receipt.error_code {
            payload["error_code"] = json!(value);
        }
        if let Some(value) = &receipt.summary {
            payload["summary"] = json!(value);
        }
        sqlx::query(
            "INSERT INTO gateway.local_node_receipts( \
                 execution_id,event_id,tenant_id,user_id,session_id,device_id,channel_id, \
                 dispatch_fence,sequence,event,status,payload \
             ) VALUES($1::uuid,$2::uuid,$3,$4,$5,$6,$7,$8::uuid,$9,$10,$11,$12)",
        )
        .bind(execution_uuid)
        .bind(&event_id)
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .bind(device_id)
        .bind(channel_id)
        .bind(
            row["dispatch_fence"]
                .as_str()
                .ok_or(ChannelStoreError::Rejected)?,
        )
        .bind(sequence)
        .bind(&receipt.event_type)
        .bind(status)
        .bind(&payload)
        .execute(&mut **tx)
        .await
        .map_err(|_| ChannelStoreError::Conflict)?;
        sqlx::query(
            "INSERT INTO gateway.local_node_events( \
                 event_id,execution_id,tenant_id,user_id,device_id,channel_id, \
                 sequence,event,status,payload \
             ) VALUES($1::uuid,$2::uuid,$3,$4,$5,$6,$7,$8,$9,$10)",
        )
        .bind(&event_id)
        .bind(execution_uuid)
        .bind(tenant_id)
        .bind(user_id)
        .bind(device_id)
        .bind(channel_id)
        .bind(sequence)
        .bind(&receipt.event_type)
        .bind(status)
        .bind(&payload)
        .execute(&mut **tx)
        .await
        .map_err(|_| ChannelStoreError::Conflict)?;
        sqlx::query(
            "UPDATE gateway.local_node_executions \
             SET receipt_cursor=$2,status=$3, \
                 result_summary=CASE WHEN $3 IN ('succeeded','failed') THEN $4::jsonb ELSE result_summary END, \
                 terminal_at=CASE WHEN $3=ANY($5::text[]) THEN now() ELSE terminal_at END,updated_at=now() \
             WHERE execution_id=$1::uuid",
        )
        .bind(execution_uuid)
        .bind(sequence)
        .bind(status)
        .bind(&payload)
        .bind(TERMINAL)
        .execute(&mut **tx)
        .await
        .map_err(|_| ChannelStoreError::Unavailable)?;
        sqlx::query(
            "UPDATE gateway.local_node_channels SET receipt_cursor=GREATEST(receipt_cursor,$2),last_seen_at=now() \
             WHERE channel_id=$1 AND tenant_id=$3 AND user_id=$4 AND device_id=$5",
        )
        .bind(channel_id)
        .bind(sequence)
        .bind(tenant_id)
        .bind(user_id)
        .bind(device_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| ChannelStoreError::Unavailable)?;
        Ok(())
    }
}

impl LocalNodeChannelRepository for PostgresLocalNodeChannelRepository {
    fn schema_ready(&self) -> ChannelFuture<'_, bool> {
        Box::pin(async move {
            sqlx::query_scalar(
                "SELECT to_regclass('gateway.local_node_device_credentials') IS NOT NULL \
                 AND to_regclass('gateway.local_node_pairing_challenges') IS NOT NULL \
                 AND to_regclass('gateway.local_node_executions') IS NOT NULL \
                 AND EXISTS (SELECT 1 FROM information_schema.columns \
                   WHERE table_schema='gateway' AND table_name='local_node_executions' \
                     AND column_name='cancel_requested_at')",
            )
            .fetch_one(&self.pool)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)
        })
    }

    fn pair<'a>(
        &'a self,
        request: &'a PairingRedeemRequest,
        channel_fingerprint: &'a str,
        credential_sha256: &'a str,
        channel_id: &'a str,
        expires_at: f64,
    ) -> ChannelFuture<'a, ()> {
        Box::pin(async move {
            if !expires_at.is_finite() || credential_sha256.len() != 64 {
                return Err(ChannelStoreError::Rejected);
            }
            let challenge_id = uuid_text(&request.challenge_id)?;
            let capabilities = serde_json::to_value(&request.capability_claims)
                .map_err(|_| ChannelStoreError::Rejected)?;
            let user_code_sha256 = sha256(request.user_code.as_bytes());
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|_| ChannelStoreError::Unavailable)?;
            let challenge_owner = sqlx::query_scalar::<_, Value>(
                "SELECT jsonb_build_object('tenant_id',tenant_id,'user_id',user_id) \
                 FROM gateway.local_node_pairing_challenges \
                 WHERE challenge_id=$1::uuid AND user_code_sha256=$2 \
                   AND consumed_at IS NULL AND expires_at>now() FOR UPDATE",
            )
            .bind(challenge_id)
            .bind(user_code_sha256)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?
            .ok_or(ChannelStoreError::Rejected)?;
            let tenant_id = value_str(&challenge_owner, "tenant_id")?;
            let user_id = value_str(&challenge_owner, "user_id")?;
            let consumed = sqlx::query_scalar::<_, i32>(
                "UPDATE gateway.local_node_pairing_challenges SET consumed_at=now() \
                 WHERE challenge_id=$1::uuid AND consumed_at IS NULL RETURNING 1",
            )
            .bind(challenge_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            if consumed.is_none() {
                return Err(ChannelStoreError::Conflict);
            }
            let key_used_elsewhere = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM gateway.local_node_channels \
                 WHERE fingerprint=$1 AND device_id<>$2)",
            )
            .bind(channel_fingerprint)
            .bind(&request.device_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            if key_used_elsewhere {
                return Err(ChannelStoreError::Conflict);
            }
            let known_fingerprint = sqlx::query_scalar::<_, String>(
                "SELECT fingerprint FROM gateway.local_node_channels \
                 WHERE device_id=$1 AND tenant_id=$2 AND user_id=$3 \
                 ORDER BY created_at DESC LIMIT 1",
            )
            .bind(&request.device_id)
            .bind(tenant_id)
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            if known_fingerprint
                .as_deref()
                .is_some_and(|known| known != channel_fingerprint)
            {
                return Err(ChannelStoreError::Conflict);
            }
            let accepted_through_sequence = sqlx::query_scalar::<_, i64>(
                "SELECT COALESCE(MAX(accepted_through_sequence),0) \
                 FROM gateway.local_node_device_credentials \
                 WHERE device_id=$1 AND tenant_id=$2 AND user_id=$3",
            )
            .bind(&request.device_id)
            .bind(tenant_id)
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            sqlx::query(
                "UPDATE gateway.local_node_device_credentials SET revoked_at=COALESCE(revoked_at,now()) \
                 WHERE device_id=$1 AND tenant_id=$2 AND user_id=$3 AND revoked_at IS NULL",
            )
            .bind(&request.device_id)
            .bind(tenant_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            sqlx::query(
                "UPDATE gateway.local_node_channels SET status='revoked' \
                 WHERE device_id=$1 AND tenant_id=$2 AND user_id=$3 AND status<>'revoked'",
            )
            .bind(&request.device_id)
            .bind(tenant_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            sqlx::query(
                "INSERT INTO gateway.local_node_devices( \
                     device_id,tenant_id,user_id,display_name,platform,node_version,protocol_version, \
                     status,capability_revision,capabilities,permission_snapshot_digest,last_seen_at \
                 ) VALUES($1,$2,$3,$4,$5,$6,$7,'online',1,$8::jsonb,$9,now()) \
                 ON CONFLICT(device_id) DO UPDATE SET \
                     display_name=EXCLUDED.display_name,platform=EXCLUDED.platform, \
                     node_version=EXCLUDED.node_version,protocol_version=EXCLUDED.protocol_version, \
                     status='online',capability_revision=gateway.local_node_devices.capability_revision+1, \
                     capabilities=EXCLUDED.capabilities,permission_snapshot_digest=EXCLUDED.permission_snapshot_digest, \
                     last_seen_at=now(),revoked_at=NULL,updated_at=now() \
                 WHERE gateway.local_node_devices.tenant_id=$2 AND gateway.local_node_devices.user_id=$3",
            )
            .bind(&request.device_id)
            .bind(tenant_id)
            .bind(user_id)
            .bind(&request.display_name)
            .bind(&request.platform)
            .bind(&request.node_version)
            .bind(super::local_node_channel::PROTOCOL_V2)
            .bind(capabilities)
            .bind(&request.permission_snapshot_digest)
            .execute(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Conflict)?;
            sqlx::query(
                "INSERT INTO gateway.local_node_channels( \
                     channel_id,device_id,tenant_id,user_id,fingerprint,status,last_seen_at \
                 ) VALUES($1,$2,$3,$4,$5,'online',now())",
            )
            .bind(channel_id)
            .bind(&request.device_id)
            .bind(tenant_id)
            .bind(user_id)
            .bind(channel_fingerprint)
            .execute(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Conflict)?;
            sqlx::query(
                "INSERT INTO gateway.local_node_device_credentials( \
                     credential_id,credential_sha256,tenant_id,user_id,device_id,channel_id,expires_at, \
                     accepted_through_sequence \
                 ) VALUES($1::uuid,$2,$3,$4,$5,$6,to_timestamp($7),$8)",
            )
            .bind(Uuid::new_v4())
            .bind(credential_sha256)
            .bind(tenant_id)
            .bind(user_id)
            .bind(&request.device_id)
            .bind(channel_id)
            .bind(expires_at)
            .bind(accepted_through_sequence)
            .execute(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Conflict)?;
            tx.commit()
                .await
                .map_err(|_| ChannelStoreError::Unavailable)?;
            Ok(())
        })
    }

    fn heartbeat<'a>(
        &'a self,
        request: &'a HeartbeatRequest,
        credential_sha256: &'a str,
    ) -> ChannelFuture<'a, HeartbeatResult> {
        Box::pin(async move {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|_| ChannelStoreError::Unavailable)?;
            let credential = sqlx::query_scalar::<_, Value>(
                "SELECT jsonb_build_object( \
                     'tenant_id',c.tenant_id,'user_id',c.user_id,'device_id',c.device_id, \
                     'channel_id',c.channel_id,'accepted_through_sequence',c.accepted_through_sequence \
                 ) FROM gateway.local_node_device_credentials c \
                 JOIN gateway.local_node_devices d ON d.device_id=c.device_id \
                  AND d.tenant_id=c.tenant_id AND d.user_id=c.user_id \
                 JOIN gateway.local_node_channels ch ON ch.channel_id=c.channel_id \
                  AND ch.device_id=c.device_id AND ch.tenant_id=c.tenant_id AND ch.user_id=c.user_id \
                 WHERE c.credential_sha256=$1 AND c.device_id=$2 AND c.revoked_at IS NULL \
                   AND c.expires_at>now() AND d.revoked_at IS NULL AND d.status='online' \
                   AND ch.status='online' FOR UPDATE OF c,d,ch",
            )
            .bind(credential_sha256)
            .bind(&request.device_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?
            .ok_or(ChannelStoreError::Rejected)?;
            let tenant_id = value_str(&credential, "tenant_id")?;
            let user_id = value_str(&credential, "user_id")?;
            let device_id = value_str(&credential, "device_id")?;
            let channel_id = value_str(&credential, "channel_id")?;
            let mut accepted = credential["accepted_through_sequence"]
                .as_i64()
                .ok_or(ChannelStoreError::Rejected)?;
            for receipt in &request.receipts {
                let sequence =
                    i64::try_from(receipt.sequence).map_err(|_| ChannelStoreError::Rejected)?;
                if sequence <= accepted {
                    continue;
                }
                if sequence != accepted.checked_add(1).ok_or(ChannelStoreError::Rejected)? {
                    return Err(ChannelStoreError::Conflict);
                }
                Self::append_device_receipt(
                    &mut tx,
                    receipt,
                    tenant_id,
                    user_id,
                    &request.device_id,
                    device_id,
                    channel_id,
                )
                .await?;
                accepted = sequence;
            }
            sqlx::query(
                "UPDATE gateway.local_node_device_credentials SET accepted_through_sequence=$2,last_seen_at=now() \
                 WHERE credential_sha256=$1 AND device_id=$3 AND revoked_at IS NULL",
            )
            .bind(credential_sha256)
            .bind(accepted)
            .bind(&request.device_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            sqlx::query(
                "UPDATE gateway.local_node_devices SET last_seen_at=now(),status='online',updated_at=now() \
                 WHERE device_id=$1 AND tenant_id=$2 AND user_id=$3 AND revoked_at IS NULL",
            )
            .bind(&request.device_id)
            .bind(tenant_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            sqlx::query(
                "UPDATE gateway.local_node_channels SET last_seen_at=now() \
                 WHERE channel_id=$1 AND device_id=$2 AND tenant_id=$3 AND user_id=$4 AND status='online'",
            )
            .bind(channel_id)
            .bind(&request.device_id)
            .bind(tenant_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;

            let claim_rows = sqlx::query_scalar::<_, Value>(
                "SELECT jsonb_build_object( \
                     'command_id',e.execution_id::text, \
                     'action',e.resource_binding->'companion_action', \
                     'normalized_arguments',e.resource_binding->'normalized_arguments' \
                 ) FROM gateway.local_node_executions e \
                 JOIN gateway.local_node_grants g ON g.grant_id=e.grant_id \
                  AND g.tenant_id=e.tenant_id AND g.user_id=e.user_id AND g.device_id=e.device_id \
                 WHERE e.tenant_id=$1 AND e.user_id=$2 AND e.device_id=$3 AND e.channel_id=$4 \
                   AND e.status='dispatched' AND e.receipt_cursor=0 \
                   AND g.kind='workspace' AND g.status='active' AND g.revoked_at IS NULL \
                   AND (g.expires_at IS NULL OR g.expires_at>now()) AND g.revision=e.grant_revision \
                   AND e.resource_binding->'companion_action' IS NOT NULL \
                 ORDER BY e.dispatched_at,e.execution_id LIMIT 100",
            )
            .bind(tenant_id)
            .bind(user_id)
            .bind(device_id)
            .bind(channel_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            let mut claims = Vec::with_capacity(claim_rows.len());
            for row in claim_rows {
                claims.push(PendingClaim {
                    command_id: value_str(&row, "command_id")?.to_owned(),
                    action: row
                        .get("action")
                        .cloned()
                        .ok_or(ChannelStoreError::Rejected)?,
                    normalized_arguments: row
                        .get("normalized_arguments")
                        .cloned()
                        .ok_or(ChannelStoreError::Rejected)?,
                });
            }
            let control_rows = sqlx::query_as::<_, (String, String, f64)>(
                "SELECT execution_id::text,device_id,extract(epoch from cancel_requested_at)::double precision \
                 FROM gateway.local_node_executions WHERE tenant_id=$1 AND user_id=$2 AND device_id=$3 \
                   AND cancel_requested_at IS NOT NULL AND status<>ALL($4::text[]) \
                 ORDER BY cancel_requested_at,execution_id LIMIT 100",
            )
            .bind(tenant_id)
            .bind(user_id)
            .bind(device_id)
            .bind(TERMINAL)
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            let now = now_seconds();
            let controls = control_rows
                .into_iter()
                .map(|(action_id, device_id, cancel_at)| PendingControl {
                    kind: "cancel".into(),
                    command_id: format!("cancel.{action_id}"),
                    action_id: Some(action_id.clone()),
                    device_id,
                    issued_at: now,
                    expires_at: now + 120.0,
                    nonce: sha256(
                        format!("local-node-cancel:{action_id}:{cancel_at:.6}").as_bytes(),
                    ),
                })
                .collect();
            tx.commit()
                .await
                .map_err(|_| ChannelStoreError::Unavailable)?;
            Ok(HeartbeatResult {
                accepted_through_sequence: u64::try_from(accepted)
                    .map_err(|_| ChannelStoreError::Rejected)?,
                claims,
                controls,
            })
        })
    }

    fn describe<'a>(
        &'a self,
        context: &'a WorkerContext,
        device_id: &'a str,
    ) -> ChannelFuture<'a, WorkerDescriptor> {
        Box::pin(async move {
            let (source, _) = self.validate_worker_scope(context, device_id).await?;
            let device = sqlx::query_scalar::<_, Value>(
                "SELECT to_jsonb(d) FROM gateway.local_node_devices d \
                 WHERE d.device_id=$1 AND d.tenant_id=$2 AND d.user_id=$3 \
                   AND d.status='online' AND d.revoked_at IS NULL \
                   AND d.last_seen_at>now()-interval '2 minutes'",
            )
            .bind(device_id)
            .bind(&context.tenant_id)
            .bind(&context.user_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?
            .ok_or(ChannelStoreError::Rejected)?;
            let grant_rows = sqlx::query_scalar::<_, Value>(
                "SELECT capabilities FROM gateway.local_node_grants \
                 WHERE tenant_id=$1 AND user_id=$2 AND device_id=$3 AND kind='workspace' \
                   AND status='active' AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at>now()) \
                   AND (session_id IS NULL OR session_id=$4)",
            )
            .bind(&context.tenant_id)
            .bind(&context.user_id)
            .bind(device_id)
            .bind(&context.session_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            let mut device_capabilities = BTreeSet::new();
            if let Some(values) = device["capabilities"].as_array() {
                for capability in values.iter().filter_map(Value::as_str) {
                    device_capabilities.insert(capability.to_owned());
                }
            }
            let grant_capabilities = grant_rows
                .into_iter()
                .map(|row| {
                    row.as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<BTreeSet<_>>()
                })
                .collect::<Vec<_>>();
            let capabilities =
                effective_file_capabilities(&device_capabilities, &grant_capabilities);
            let _trusted_execution = &source["execution"];
            Ok(WorkerDescriptor {
                device_id: device_id.to_owned(),
                capability_revision: device["capability_revision"]
                    .as_u64()
                    .ok_or(ChannelStoreError::Rejected)?,
                capabilities,
                online: true,
                expires_at_epoch_ms: (now_seconds() + 60.0) as u64 * 1000,
            })
        })
    }

    fn dispatch<'a>(
        &'a self,
        context: &'a WorkerContext,
        request: &'a WorkerDispatchRequest,
    ) -> ChannelFuture<'a, Value> {
        Box::pin(async move {
            let Some(spec) = file_operation_spec(&request.operation) else {
                return Err(ChannelStoreError::Rejected);
            };
            if !valid_file_operation_arguments(&request.operation, &request.arguments) {
                return Err(ChannelStoreError::Rejected);
            }
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|_| ChannelStoreError::Unavailable)?;
            let execution_id = uuid_text(&context.execution_id)?;
            let source = sqlx::query_scalar::<_, Value>(
                "SELECT jsonb_build_object('execution',to_jsonb(e),'lease',to_jsonb(l),'snapshot',to_jsonb(s)) \
                 FROM assistant.assistant_capability_executions e \
                 JOIN assistant.assistant_runtime_model_leases l ON l.lease_id=e.lease_id AND l.run_id=e.run_id \
                  AND l.tenant_id=e.tenant_id AND l.user_id=e.user_id AND l.session_id=e.session_id \
                 JOIN assistant.assistant_runtime_snapshots s ON s.snapshot_id=l.snapshot_id AND s.run_id=l.run_id \
                  AND s.tenant_id=l.tenant_id AND s.user_id=l.user_id AND s.session_id=l.session_id \
                 LEFT JOIN assistant.assistant_runtime_snapshot_revocations r ON r.snapshot_id=s.snapshot_id \
                  AND r.tenant_id=s.tenant_id AND r.user_id=s.user_id AND r.session_id=s.session_id \
                 WHERE e.execution_id=$1::uuid AND e.lease_id=$2::uuid AND e.run_id=$3::uuid \
                   AND e.tenant_id=$4 AND e.user_id=$5 AND e.session_id=$6 \
                   AND e.tool_call_id=$7 AND e.attempt_id=$8 AND e.capability_id='local_node_action' \
                   AND e.capability_revision=$9 AND e.effect='write' AND l.capability_revision=e.capability_revision \
                   AND l.status='active' AND l.revoked_at IS NULL AND l.expires_at>now() \
                   AND (s.expires_at IS NULL OR s.expires_at>now()) AND r.snapshot_id IS NULL \
                   AND e.resource_binding->>'snapshot_id'=s.snapshot_id::text \
                   AND e.status IN ('dispatched','running') FOR UPDATE OF e,l",
            )
            .bind(execution_id)
            .bind(uuid_text(&context.lease_id)?)
            .bind(uuid_text(&context.run_id)?)
            .bind(&context.tenant_id)
            .bind(&context.user_id)
            .bind(&context.session_id)
            .bind(&context.tool_call_id)
            .bind(&context.attempt_id)
            .bind(i64::try_from(context.capability_revision).map_err(|_| ChannelStoreError::Rejected)?)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?
            .ok_or(ChannelStoreError::Rejected)?;
            let persisted_arguments = &source["execution"]["arguments"];
            let expected_arguments =
                json!({"operation":request.operation,"arguments":request.arguments});
            let full_hash = format!("sha256:{}", digest_value(&expected_arguments));
            if persisted_arguments != &expected_arguments
                || request.arguments_hash != full_hash
                || source["execution"]["arguments_sha256"].as_str()
                    != Some(full_hash.trim_start_matches("sha256:"))
                || source["execution"]["idempotency_key"].as_str()
                    != Some(request.idempotency_key.as_str())
                || source["execution"]["approval_status"] != "consumed"
                || source["execution"]["approval_id"].as_str() != request.approval_id.as_deref()
            {
                return Err(ChannelStoreError::Rejected);
            }
            let approval_id = request
                .approval_id
                .as_deref()
                .ok_or(ChannelStoreError::Rejected)?;
            let approval = sqlx::query_scalar::<_, i32>(
                "SELECT 1 FROM assistant.assistant_tool_approvals a \
                 WHERE a.approval_id=$1::uuid AND a.tenant_id=$2 AND a.user_id=$3 AND a.session_id=$4 \
                   AND a.run_id=$5::uuid AND a.tool_call_id=$6 AND a.tool_name='local_node_action' \
                   AND a.status='consumed' AND a.arguments=$7::jsonb",
            )
            .bind(uuid_text(approval_id)?)
            .bind(&context.tenant_id)
            .bind(&context.user_id)
            .bind(&context.session_id)
            .bind(uuid_text(&context.run_id)?)
            .bind(&context.tool_call_id)
            .bind(persisted_arguments)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            if approval.is_none() {
                return Err(ChannelStoreError::Rejected);
            }
            let binding = &source["execution"]["resource_binding"];
            if !resource_binding_device_matches(binding, &request.device_id) {
                return Err(ChannelStoreError::Rejected);
            }
            let device = sqlx::query_scalar::<_, Value>(
                "SELECT to_jsonb(d) FROM gateway.local_node_devices d \
                 WHERE device_id=$1 AND tenant_id=$2 AND user_id=$3 AND status='online' AND revoked_at IS NULL",
            )
            .bind(&request.device_id)
            .bind(&context.tenant_id)
            .bind(&context.user_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?
            .ok_or(ChannelStoreError::Rejected)?;
            if !device["capabilities"].as_array().is_some_and(|values| {
                values
                    .iter()
                    .any(|value| value.as_str() == Some(spec.device_claim))
            }) {
                return Err(ChannelStoreError::Rejected);
            }
            let channel_id = sqlx::query_scalar::<_, String>(
                "SELECT ch.channel_id FROM gateway.local_node_device_credentials c \
                 JOIN gateway.local_node_channels ch ON ch.channel_id=c.channel_id \
                  AND ch.device_id=c.device_id AND ch.tenant_id=c.tenant_id AND ch.user_id=c.user_id \
                 WHERE c.device_id=$1 AND c.tenant_id=$2 AND c.user_id=$3 \
                   AND c.revoked_at IS NULL AND c.expires_at>now() AND ch.status='online' \
                 ORDER BY c.created_at DESC LIMIT 1 FOR UPDATE OF c,ch",
            )
            .bind(&request.device_id)
            .bind(&context.tenant_id)
            .bind(&context.user_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?
            .ok_or(ChannelStoreError::Rejected)?;
            let connector_grant = if binding["connector_binding"]["provider"] == "local_node" {
                binding["connector_binding"]["grant_id"].as_str()
            } else {
                binding["local_node_grant_id"].as_str()
            };
            let grant = self
                .current_grant(
                    &mut tx,
                    &context.tenant_id,
                    &context.user_id,
                    &context.session_id,
                    &request.device_id,
                    connector_grant,
                    spec.grant_capabilities,
                )
                .await?;
            let grant_id = value_str(&grant, "grant_id")?;
            if request.arguments["grant_id"].as_str() != Some(grant_id) {
                return Err(ChannelStoreError::Rejected);
            }
            let grant_revision = grant["revision"]
                .as_i64()
                .ok_or(ChannelStoreError::Rejected)?;
            let existing = sqlx::query_scalar::<_, Value>(
                "SELECT to_jsonb(e) FROM gateway.local_node_executions e \
                 WHERE execution_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 FOR UPDATE",
            )
            .bind(execution_id)
            .bind(&context.tenant_id)
            .bind(&context.user_id)
            .bind(&context.session_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            if let Some(existing) = existing {
                if existing["arguments_sha256"].as_str()
                    != Some(digest_value(&request.arguments).as_str())
                    || existing["operation"] != request.operation
                    || existing["device_id"] != request.device_id
                    || existing["channel_id"] != channel_id
                    || existing["grant_id"].as_str() != Some(grant_id)
                    || existing["grant_revision"].as_i64() != Some(grant_revision)
                    || existing["resource_binding"]["normalized_arguments"] != request.arguments
                {
                    return Err(ChannelStoreError::Conflict);
                }
                tx.commit()
                    .await
                    .map_err(|_| ChannelStoreError::Unavailable)?;
                return Ok(json!({
                    "accepted":true,
                    "execution_id":context.execution_id,
                    "action_id":context.execution_id,
                    "replayed":true,
                }));
            }
            let args = &request.arguments;
            let intent = if request.operation == "file.write" {
                file_write_intent(args)?
            } else {
                args.clone()
            };
            let path_key = if request.operation == "file.write" {
                "relative_path"
            } else {
                "path"
            };
            let relative_path = value_str(args, path_key)?;
            let now = now_seconds();
            let snapshot = &source["snapshot"];
            let snapshot_id = source["lease"]["snapshot_id"]
                .as_str()
                .ok_or(ChannelStoreError::Rejected)?;
            let snapshot_hash = snapshot["snapshot_sha256"]
                .as_str()
                .ok_or(ChannelStoreError::Rejected)?;
            let policy_snapshot_digest = format!(
                "sha256:{}",
                digest_value(&json!({
                    "snapshot_id":snapshot_id,
                    "snapshot_sha256":snapshot_hash,
                    "capability_revision":context.capability_revision,
                    "device_permission_snapshot_digest":device["permission_snapshot_digest"],
                    "grant_id":grant_id,
                    "grant_revision":grant_revision,
                }))
            );
            let target_snapshot_digest = if request.operation == "file.write" {
                intent["expected_hash"]
                    .as_str()
                    .unwrap_or("missing")
                    .to_owned()
            } else {
                format!(
                    "sha256:{}",
                    digest_value(&json!({
                        "kind":"local_node_target_scope",
                        "snapshot_id":snapshot_id,
                        "snapshot_sha256":snapshot_hash,
                        "permission_snapshot_digest":device["permission_snapshot_digest"],
                        "grant_id":grant_id,
                        "grant_revision":grant_revision,
                        "capability_revision":context.capability_revision,
                        "operation":request.operation,
                        "arguments_sha256":digest_value(args),
                    }))
                )
            };
            let nonce = Uuid::new_v4().to_string();
            let agent_id = snapshot["snapshot"]["agent_id"]
                .as_str()
                .or_else(|| snapshot["snapshot"]["agentId"].as_str())
                .unwrap_or("__builtin_assistant__");
            let agent_version = snapshot["snapshot"]["fingerprints"]["spec"]
                .as_str()
                .unwrap_or(snapshot_hash);
            let action = json!({
                "action_id":context.execution_id,
                "idempotency_key":request.idempotency_key,
                "tenant_id":context.tenant_id,
                "user_id":context.user_id,
                "session_id":context.session_id,
                "run_id":context.run_id,
                "agent_id":agent_id,
                "agent_version":agent_version,
                "call_id":context.tool_call_id,
                "device_id":request.device_id,
                "envelope_version":1,
                "capability":spec.action_capability,
                "tool_name":spec.tool_name,
                "operation":request.operation,
                "capability_lease_id":grant_id,
                "resource_refs":[grant_id,relative_path],
                "arguments_digest":digest_value(&intent),
                "target_snapshot_digest":target_snapshot_digest,
                "policy_snapshot_digest":policy_snapshot_digest,
                "nonce":nonce,
                "issued_at":now,
                "expires_at":now+120.0,
                "platform_key_id":"pending",
                "platform_signature":"pending",
                "approval":null,
                "trace_context":null,
            });
            let resource_binding = json!({
                "device_id":request.device_id,
                "channel_id":channel_id,
                "grant_id":grant_id,
                "grant_revision":grant_revision,
                "snapshot_id":snapshot_id,
                "capability_revision":context.capability_revision,
                "companion_action":action,
                "normalized_arguments":args,
            });
            let local_args_hash = digest_value(args);
            let local_approval_id = (spec.effect == "write").then_some(approval_id);
            let local_approval_status = if spec.effect == "write" {
                "approved"
            } else {
                "not_required"
            };
            let inserted = sqlx::query(
                "INSERT INTO gateway.local_node_executions( \
                     execution_id,lease_id,tenant_id,user_id,session_id,run_id,tool_call_id,attempt_id, \
                     device_id,channel_id,grant_id,grant_revision,capability_id,capability_revision, \
                     operation,arguments,arguments_sha256,idempotency_key,effect,approval_id, \
                     approval_status,resource_binding,status \
                 ) VALUES($1::uuid,$2::uuid,$3,$4,$5,$6::uuid,$7,$8,$9,$10,$11::uuid,$12, \
                     'local_node_action',$13,$14,$15::jsonb,$16,$17,$18,$19::uuid, \
                     $20,$21::jsonb,'published')",
            )
            .bind(execution_id)
            .bind(uuid_text(&context.lease_id)?)
            .bind(&context.tenant_id)
            .bind(&context.user_id)
            .bind(&context.session_id)
            .bind(uuid_text(&context.run_id)?)
            .bind(&context.tool_call_id)
            .bind(&context.attempt_id)
            .bind(&request.device_id)
            .bind(&channel_id)
            .bind(uuid_text(grant_id)?)
            .bind(grant_revision)
            .bind(i64::try_from(context.capability_revision).map_err(|_| ChannelStoreError::Rejected)?)
            .bind(&request.operation)
            .bind(args)
            .bind(local_args_hash)
            .bind(&request.idempotency_key)
            .bind(spec.effect)
            .bind(local_approval_id.map(uuid_text).transpose()?)
            .bind(local_approval_status)
            .bind(resource_binding)
            .execute(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Conflict)?;
            if inserted.rows_affected() != 1 {
                return Err(ChannelStoreError::Conflict);
            }
            let dispatch_fence = Uuid::new_v4();
            let claimed = sqlx::query_scalar::<_, bool>(
                "SELECT gateway.claim_local_node_dispatch($1::uuid,$2,$3,$4,$5::uuid,$6)",
            )
            .bind(execution_id)
            .bind(&context.tenant_id)
            .bind(&context.user_id)
            .bind(&context.session_id)
            .bind(dispatch_fence)
            .bind(spec.effect)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            if !claimed {
                return Err(ChannelStoreError::Conflict);
            }
            tx.commit()
                .await
                .map_err(|_| ChannelStoreError::Unavailable)?;
            Ok(json!({
                "accepted":true,
                "execution_id":context.execution_id,
                "action_id":context.execution_id,
                "replayed":false,
            }))
        })
    }

    fn status<'a>(
        &'a self,
        context: &'a WorkerContext,
        device_id: &'a str,
        after_sequence: u64,
    ) -> ChannelFuture<'a, Vec<WorkerReceiptEvent>> {
        Box::pin(async move {
            let _ = self.validate_worker_scope(context, device_id).await?;
            let after = i64::try_from(after_sequence).map_err(|_| ChannelStoreError::Rejected)?;
            let row = sqlx::query_scalar::<_, Value>(
                "SELECT to_jsonb(e) FROM gateway.local_node_executions e \
                 JOIN gateway.local_node_grants g ON g.grant_id=e.grant_id \
                  AND g.tenant_id=e.tenant_id AND g.user_id=e.user_id AND g.device_id=e.device_id \
                 WHERE e.execution_id=$1::uuid AND e.tenant_id=$2 AND e.user_id=$3 \
                   AND e.session_id=$4 AND e.run_id=$5::uuid AND e.tool_call_id=$6 \
                   AND e.attempt_id=$7 AND e.lease_id=$8::uuid AND e.device_id=$9 \
                   AND e.dispatch_fence IS NOT NULL AND g.kind='workspace' AND g.status='active' \
                   AND g.revoked_at IS NULL AND (g.expires_at IS NULL OR g.expires_at>now()) \
                   AND g.revision=e.grant_revision",
            )
            .bind(uuid_text(&context.execution_id)?)
            .bind(&context.tenant_id)
            .bind(&context.user_id)
            .bind(&context.session_id)
            .bind(uuid_text(&context.run_id)?)
            .bind(&context.tool_call_id)
            .bind(&context.attempt_id)
            .bind(uuid_text(&context.lease_id)?)
            .bind(device_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?
            .ok_or(ChannelStoreError::Rejected)?;
            let rows = sqlx::query_scalar::<_, Value>(
                "SELECT to_jsonb(r) FROM gateway.local_node_receipts r \
                 WHERE r.execution_id=$1::uuid AND r.tenant_id=$2 AND r.user_id=$3 \
                   AND r.session_id=$4 AND r.device_id=$5 AND r.sequence>$6 ORDER BY r.sequence LIMIT 256",
            )
            .bind(uuid_text(&context.execution_id)?)
            .bind(&context.tenant_id)
            .bind(&context.user_id)
            .bind(&context.session_id)
            .bind(device_id)
            .bind(after)
            .fetch_all(&self.pool)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            if rows.is_empty()
                && row["status"]
                    .as_str()
                    .is_some_and(|s| TERMINAL.contains(&s))
            {
                // A terminal row without a matching durable receipt is a broken ledger, not a success.
                return Err(ChannelStoreError::Conflict);
            }
            rows.into_iter()
                .map(|row| {
                    let payload = row["payload"]
                        .as_object()
                        .ok_or(ChannelStoreError::Rejected)?
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect();
                    Ok(WorkerReceiptEvent {
                        execution_id: value_str(&row, "execution_id")?.to_owned(),
                        device_id: value_str(&row, "device_id")?.to_owned(),
                        sequence: row["sequence"]
                            .as_u64()
                            .ok_or(ChannelStoreError::Rejected)?,
                        status: value_str(&row, "status")?.to_owned(),
                        event: value_str(&row, "event")?.to_owned(),
                        payload,
                    })
                })
                .collect()
        })
    }

    fn cancel<'a>(
        &'a self,
        context: &'a WorkerContext,
        request: &'a super::local_node_channel::WorkerCancelRequest,
    ) -> ChannelFuture<'a, Value> {
        Box::pin(async move {
            if request.action_id != context.execution_id || request.device_id.is_empty() {
                return Err(ChannelStoreError::Rejected);
            }
            let _ = self
                .validate_worker_scope(context, &request.device_id)
                .await?;
            let changed = sqlx::query(
                "UPDATE gateway.local_node_executions e SET cancel_requested_at=COALESCE(cancel_requested_at,now()),updated_at=now() \
                 WHERE e.execution_id=$1::uuid AND e.tenant_id=$2 AND e.user_id=$3 AND e.session_id=$4 \
                   AND e.run_id=$5::uuid AND e.tool_call_id=$6 AND e.attempt_id=$7 AND e.lease_id=$8::uuid \
                   AND e.device_id=$9 AND e.dispatch_fence IS NOT NULL AND e.status<>ALL($10::text[])",
            )
            .bind(uuid_text(&context.execution_id)?)
            .bind(&context.tenant_id)
            .bind(&context.user_id)
            .bind(&context.session_id)
            .bind(uuid_text(&context.run_id)?)
            .bind(&context.tool_call_id)
            .bind(&context.attempt_id)
            .bind(uuid_text(&context.lease_id)?)
            .bind(&request.device_id)
            .bind(TERMINAL)
            .execute(&self.pool)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            if changed.rows_affected() == 0 {
                let terminal = sqlx::query_scalar::<_, bool>(
                    "SELECT EXISTS(SELECT 1 FROM gateway.local_node_executions \
                     WHERE execution_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 \
                       AND device_id=$5 AND status=ANY($6::text[]))",
                )
                .bind(uuid_text(&context.execution_id)?)
                .bind(&context.tenant_id)
                .bind(&context.user_id)
                .bind(&context.session_id)
                .bind(&request.device_id)
                .bind(TERMINAL)
                .fetch_one(&self.pool)
                .await
                .map_err(|_| ChannelStoreError::Unavailable)?;
                if !terminal {
                    return Err(ChannelStoreError::Rejected);
                }
            }
            Ok(json!({"accepted":true,"execution_id":context.execution_id}))
        })
    }

    fn verify_device_credential<'a>(
        &'a self,
        _credential_sha256: &'a str,
        _expected_device_id: &'a str,
    ) -> ChannelFuture<'a, crate::migration_local_node::protocol::LocalNodeDeviceScope> {
        Box::pin(async { Err(ChannelStoreError::Rejected) })
    }

    fn dispatch_adapter_accepted<'a>(
        &'a self,
        action: &'a crate::migration_local_node::protocol::LocalNodeAction,
    ) -> ChannelFuture<'a, ()> {
        Box::pin(async move {
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM gateway.local_node_executions \
                 WHERE execution_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 \
                   AND device_id=$5 AND channel_id=$6 AND dispatch_fence=$7::uuid)",
            )
            .bind(uuid_text(&action.execution_id)?)
            .bind(&action.scope.tenant_id)
            .bind(&action.scope.user_id)
            .bind(&action.scope.session_id)
            .bind(&action.scope.device_id)
            .bind(&action.scope.channel_id)
            .bind(
                action
                    .dispatch_fence
                    .as_deref()
                    .ok_or(ChannelStoreError::Rejected)?,
            )
            .fetch_one(&self.pool)
            .await
            .map_err(|_| ChannelStoreError::Unavailable)?;
            if exists {
                Ok(())
            } else {
                Err(ChannelStoreError::Rejected)
            }
        })
    }
}

#[derive(Clone, Copy)]
struct FileOperationSpec {
    device_claim: &'static str,
    action_capability: &'static str,
    tool_name: &'static str,
    grant_capabilities: &'static [&'static str],
    effect: &'static str,
}

fn file_operation_spec(operation: &str) -> Option<FileOperationSpec> {
    let (device_claim, action_capability, tool_name, grant_capabilities, effect) = match operation {
        // Pairing intentionally advertises the coarse `file.read` claim for
        // both list and read grants. The signed Action remains operation-specific.
        "file.list" => (
            "file.read",
            "file.list",
            "local_file_list",
            &["list"][..],
            "read",
        ),
        "file.read" => (
            "file.read",
            "file.read",
            "local_file_read",
            &["read"][..],
            "read",
        ),
        "file.hash" => (
            "file.read",
            "file.read",
            "local_file_hash",
            &["read"][..],
            "read",
        ),
        "file.search" => (
            "file.search",
            "file.search",
            "local_file_search",
            &["search", "read", "list"][..],
            "read",
        ),
        "file.watch" => (
            "file.watch",
            "file.watch",
            "local_file_watch",
            &["watch"][..],
            "read",
        ),
        "file.write" => (
            "file.write",
            "file.write",
            "local_file_write",
            &["write"][..],
            "write",
        ),
        _ => return None,
    };
    Some(FileOperationSpec {
        device_claim,
        action_capability,
        tool_name,
        grant_capabilities,
        effect,
    })
}

fn effective_file_capabilities(
    device_claims: &BTreeSet<String>,
    grants: &[BTreeSet<String>],
) -> BTreeSet<String> {
    let can = |device_claim: &str, required: &[&str]| {
        device_claims.contains(device_claim)
            && grants.iter().any(|grant| {
                required
                    .iter()
                    .all(|permission| grant.contains(*permission))
            })
    };
    let mut effective = BTreeSet::new();
    if can("file.read", &["list"]) {
        effective.insert("file.list".to_owned());
    }
    if can("file.read", &["read"]) {
        effective.insert("file.read".to_owned());
        effective.insert("file.hash".to_owned());
    }
    if can("file.search", &["search", "read", "list"]) {
        effective.insert("file.search".to_owned());
    }
    if can("file.watch", &["watch"]) {
        effective.insert("file.watch".to_owned());
    }
    if can("file.write", &["write"]) {
        effective.insert("file.write".to_owned());
    }
    effective
}

fn valid_file_operation_arguments(operation: &str, arguments: &Value) -> bool {
    if operation == "file.write" {
        return valid_file_write_arguments(arguments);
    }
    let Some(object) = arguments.as_object() else {
        return false;
    };
    let allowed = match operation {
        "file.list" => &["grant_id", "path", "limit"][..],
        "file.read" | "file.hash" => &["grant_id", "path", "max_bytes"][..],
        "file.search" => &["grant_id", "path", "query", "limit"][..],
        "file.watch" => &["grant_id", "path", "after_revision", "timeout_ms"][..],
        _ => return false,
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return false;
    }
    let Some(grant_id) = arguments["grant_id"].as_str() else {
        return false;
    };
    let Some(path) = arguments["path"].as_str() else {
        return false;
    };
    if !valid_grant_id(grant_id) || !valid_relative_path(path) {
        return false;
    }
    match operation {
        "file.list" => valid_optional_bounded(arguments, "limit", 500),
        "file.read" | "file.hash" => {
            path != "." && valid_optional_bounded(arguments, "max_bytes", 8 * 1024 * 1024)
        }
        "file.search" => {
            arguments["query"].as_str().is_some_and(|query| {
                !query.is_empty() && query.chars().count() <= 500 && !query.contains('\0')
            }) && valid_optional_bounded(arguments, "limit", 200)
        }
        "file.watch" => {
            let after_revision_valid = match arguments.get("after_revision") {
                None | Some(Value::Null) => true,
                Some(Value::String(value)) => value == "0",
                _ => false,
            };
            after_revision_valid && valid_optional_bounded(arguments, "timeout_ms", 30_000)
        }
        _ => false,
    }
}

fn valid_optional_bounded(arguments: &Value, key: &str, maximum: u64) -> bool {
    match arguments.get(key) {
        None | Some(Value::Null) => true,
        Some(value) => value
            .as_u64()
            .is_some_and(|number| number >= 1 && number <= maximum),
    }
}

fn valid_grant_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 255 && Uuid::parse_str(value).is_ok()
}

fn valid_relative_path(path: &str) -> bool {
    if path.is_empty()
        || path.chars().count() > 1024
        || path.starts_with('/')
        || path.contains(['\0', '\\', ':'])
    {
        return false;
    }
    path.split('/').all(|part| part != "..")
}

fn valid_file_write_arguments(arguments: &Value) -> bool {
    let Some(object) = arguments.as_object() else {
        return false;
    };
    let fields = [
        "grant_id",
        "relative_path",
        "content_sha256",
        "expected_hash",
        "content_base64",
    ];
    if object.len() != fields.len() || object.keys().any(|key| !fields.contains(&key.as_str())) {
        return false;
    }
    let Some(content) = arguments["content_base64"].as_str() else {
        return false;
    };
    let Some(expected) = arguments["content_sha256"].as_str() else {
        return false;
    };
    let Some(path) = arguments["relative_path"].as_str() else {
        return false;
    };
    let Some(grant) = arguments["grant_id"].as_str() else {
        return false;
    };
    if content.len() > 699_052
        || path == "."
        || !valid_relative_path(path)
        || !valid_grant_id(grant)
        || expected.len() != 64
        || !expected
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return false;
    }
    if !arguments["expected_hash"].is_null()
        && arguments["expected_hash"].as_str().is_none_or(|v| {
            v != "missing"
                && (v.len() != 64
                    || !v
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
        })
    {
        return false;
    }
    let Ok(decoded) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(content) else {
        return false;
    };
    decoded.len() <= 512 * 1024 && sha256(&decoded) == expected
}

fn file_write_intent(arguments: &Value) -> Result<Value, ChannelStoreError> {
    if !valid_file_write_arguments(arguments) {
        return Err(ChannelStoreError::Rejected);
    }
    let mut intent = arguments.clone();
    intent
        .as_object_mut()
        .ok_or(ChannelStoreError::Rejected)?
        .remove("content_base64");
    Ok(intent)
}

fn value_str<'a>(value: &'a Value, key: &str) -> Result<&'a str, ChannelStoreError> {
    value[key].as_str().ok_or(ChannelStoreError::Rejected)
}

fn resource_binding_device_matches(binding: &Value, device_id: &str) -> bool {
    let direct = binding["device_id"].as_str();
    let connector = binding["connector_binding"]["connection_id"].as_str();
    (direct == Some(device_id) || connector == Some(device_id))
        && direct
            .zip(connector)
            .is_none_or(|(direct, connector)| direct == connector)
}

fn uuid_text(value: &str) -> Result<&str, ChannelStoreError> {
    Uuid::parse_str(value).map_err(|_| ChannelStoreError::Rejected)?;
    Ok(value)
}

fn digest_value(value: &Value) -> String {
    sha256(super::local_node_channel::canonical_json(value).as_bytes())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn now_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_file_operations_match_current_companion_contracts() {
        let grant_id = Uuid::new_v4().to_string();
        let cases = [
            (
                "file.list",
                json!({"grant_id":grant_id.clone(),"path":".","limit":500}),
                "file.read",
                "file.list",
                "local_file_list",
                "read",
            ),
            (
                "file.read",
                json!({"grant_id":grant_id.clone(),"path":"docs/readme.txt","max_bytes":4096}),
                "file.read",
                "file.read",
                "local_file_read",
                "read",
            ),
            (
                "file.hash",
                json!({"grant_id":grant_id.clone(),"path":"docs/readme.txt"}),
                "file.read",
                "file.read",
                "local_file_hash",
                "read",
            ),
            (
                "file.search",
                json!({"grant_id":grant_id.clone(),"path":".","query":"needle","limit":200}),
                "file.search",
                "file.search",
                "local_file_search",
                "read",
            ),
            (
                "file.watch",
                json!({"grant_id":grant_id.clone(),"path":".","after_revision":"0","timeout_ms":30000}),
                "file.watch",
                "file.watch",
                "local_file_watch",
                "read",
            ),
        ];

        for (operation, arguments, claim, action_capability, tool, effect) in cases {
            let spec = file_operation_spec(operation).expect("supported read operation");
            assert_eq!(spec.device_claim, claim);
            assert_eq!(spec.action_capability, action_capability);
            assert_eq!(spec.tool_name, tool);
            assert_eq!(spec.effect, effect);
            assert!(valid_file_operation_arguments(operation, &arguments));
        }
    }

    #[test]
    fn file_operation_arguments_reject_extra_fields_unsafe_paths_and_bad_bounds() {
        let grant_id = Uuid::new_v4().to_string();
        let mut read =
            json!({"grant_id":grant_id.clone(),"path":"docs/readme.txt","max_bytes":4096});
        assert!(valid_file_operation_arguments("file.read", &read));
        read["unexpected"] = json!(true);
        assert!(!valid_file_operation_arguments("file.read", &read));

        assert!(!valid_file_operation_arguments(
            "file.list",
            &json!({"grant_id":grant_id.clone(),"path":"../outside"})
        ));
        assert!(!valid_file_operation_arguments(
            "file.read",
            &json!({"grant_id":grant_id.clone(),"path":"docs/readme.txt","max_bytes":0})
        ));
        assert!(!valid_file_operation_arguments(
            "file.watch",
            &json!({"grant_id":grant_id,"path":".","after_revision":"1"})
        ));
    }

    #[test]
    fn bound_device_may_come_from_direct_or_connector_binding_but_never_conflict() {
        let direct = json!({"device_id":"dev_fixture"});
        assert!(resource_binding_device_matches(&direct, "dev_fixture"));

        let connector = json!({"connector_binding":{"connection_id":"dev_fixture"}});
        assert!(resource_binding_device_matches(&connector, "dev_fixture"));

        let conflict = json!({
            "device_id":"dev_other",
            "connector_binding":{"connection_id":"dev_fixture"}
        });
        assert!(!resource_binding_device_matches(&conflict, "dev_fixture"));
        assert!(!resource_binding_device_matches(&json!({}), "dev_fixture"));
    }

    #[test]
    fn effective_capabilities_preserve_coarse_claims_without_combining_grants() {
        let device_claims = BTreeSet::from(["file.search".to_owned(), "file.read".to_owned()]);
        let grant_a = BTreeSet::from(["search".to_owned(), "read".to_owned()]);
        let grant_b = BTreeSet::from(["list".to_owned()]);
        let split =
            effective_file_capabilities(&device_claims, &[grant_a.clone(), grant_b.clone()]);
        assert!(split.contains("file.list"));
        assert!(split.contains("file.read"));
        assert!(split.contains("file.hash"));
        assert!(!split.contains("file.search"));

        let complete = BTreeSet::from(["search".to_owned(), "read".to_owned(), "list".to_owned()]);
        let effective = effective_file_capabilities(&device_claims, &[complete]);
        assert!(effective.contains("file.list"));
        assert!(effective.contains("file.read"));
        assert!(effective.contains("file.hash"));
        assert!(effective.contains("file.search"));
        assert!(!effective.contains("file.rollback"));
    }

    #[test]
    fn file_write_keeps_separate_payload_and_signed_intent() {
        let content = b"native file write";
        let arguments = json!({
            "grant_id":Uuid::new_v4().to_string(),
            "relative_path":"notes/result.txt",
            "content_sha256":sha256(content),
            "expected_hash":null,
            "content_base64":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(content),
        });
        assert!(valid_file_operation_arguments("file.write", &arguments));
        let intent = file_write_intent(&arguments).unwrap();
        assert!(intent.get("content_base64").is_none());
        assert_eq!(intent["relative_path"], "notes/result.txt");

        let mut tampered = arguments;
        tampered["content_base64"] =
            json!(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"tampered"));
        assert!(!valid_file_operation_arguments("file.write", &tampered));
    }
}
