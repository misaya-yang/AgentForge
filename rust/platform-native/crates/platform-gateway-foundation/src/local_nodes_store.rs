//! PostgreSQL closure of services/local_node/control_plane.py.
//!
//! Construction is inert. The host supplies independent, session-pinned data
//! and cancellation pools, their search_path, clock, and secure byte provider.
//! Pairing completion is one supervised transaction; the other source pool
//! acquisitions remain independent operations. JSONB read columns deliberately
//! become strings: the canonical asyncpg pool has no JSON decoder configured.
//! See packages/ai-gateway-core/src/ai_gateway_core/persistence/database.py,
//! Database.connect(), and src/main.py's binding of container.database._pool.
//! A host with custom source JSON codecs needs a separately reviewed adapter;
//! this constructor deliberately reproduces the canonical default pool.
use crate::{
    local_nodes_types::{
        LocalNodesClock, LocalNodesControlPlane, LocalNodesError, LocalNodesFuture,
        LocalNodesIdentity, LocalNodesRandom, pairing_user_code, parse_uuid,
    },
    postgres_operation::{PgOperationError, PgOperationStorage},
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use uuid::Uuid;

#[derive(Clone)]
pub struct PgLocalNodesControlPlane {
    operations: PgOperationStorage,
    random: Arc<dyn LocalNodesRandom>,
    clock: Arc<dyn LocalNodesClock>,
}
fn pg_error(error: PgOperationError) -> LocalNodesError {
    match error {
        PgOperationError::Cancelled => LocalNodesError::Cancelled,
        _ => LocalNodesError::Internal,
    }
}
fn owner_required(owner: &LocalNodesIdentity) -> Result<(), LocalNodesError> {
    if owner.tenant_id.is_empty() || owner.user_id.is_empty() {
        Err(LocalNodesError::repository(
            "tenant and user ownership are required",
        ))
    } else {
        Ok(())
    }
}
fn text_arg(args: &Value, key: &str, default: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_owned()
}
fn optional_text(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(str::to_owned)
}
fn channel_text(args: &Value, key: &str) -> Option<String> {
    args.get("channel")
        .and_then(|c| c.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}
// datetime.isoformat() uses exactly six fractional digits when microseconds
// are nonzero and omits the fraction otherwise; asyncpg timestamps are UTC.
fn python_datetime(value: DateTime<Utc>) -> String {
    if value.timestamp_subsec_micros() == 0 {
        value.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    } else {
        value.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
    }
}
fn source_row(mut value: Value) -> Value {
    if let Some(row) = value.as_object_mut() {
        for key in [
            "created_at",
            "updated_at",
            "last_seen_at",
            "expires_at",
            "revoked_at",
            "consumed_at",
        ] {
            if let Some(Value::String(text)) = row.get_mut(key)
                && let Ok(date) = DateTime::parse_from_rfc3339(text)
            {
                *text = python_datetime(date.with_timezone(&Utc));
            }
        }
    }
    value
}
impl PgLocalNodesControlPlane {
    pub fn new(
        pool: PgPool,
        independent_control_pool: PgPool,
        random: Arc<dyn LocalNodesRandom>,
        clock: Arc<dyn LocalNodesClock>,
    ) -> Result<Self, LocalNodesError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool, independent_control_pool)
                .map_err(pg_error)?,
            random,
            clock,
        })
    }
    /// Await after aborting callers, before the host closes its control pool.
    pub fn drain_cancelled(&self) -> LocalNodesFuture<'_, ()> {
        Box::pin(async move { self.operations.drain_cancelled().await.map_err(pg_error) })
    }
    async fn device(
        &self,
        owner: &LocalNodesIdentity,
        device_id: &str,
    ) -> Result<Value, LocalNodesError> {
        let owner = owner.clone();
        let device_id = device_id.to_owned();
        let row: Option<Value> = self.operations.acquire().await.map_err(pg_error)?.execute(move |connection| Box::pin(async move {
            sqlx::query_scalar("SELECT to_jsonb(d) || jsonb_build_object('capabilities', d.capabilities::text) FROM local_node_devices d WHERE device_id=$1 AND tenant_id=$2 AND user_id=$3")
                .bind(device_id).bind(owner.tenant_id).bind(owner.user_id).fetch_optional(connection).await
        })).await.map_err(pg_error)?;
        match row {
            Some(row) if row.get("revoked_at").is_some_and(Value::is_null) => Ok(source_row(row)),
            _ => Err(LocalNodesError::repository(
                "device is not owned or does not exist",
            )),
        }
    }
    async fn create_challenge(
        &self,
        owner: &LocalNodesIdentity,
        args: Value,
    ) -> Result<Value, LocalNodesError> {
        owner_required(owner)?;
        let challenge_id = Uuid::new_v4().to_string();
        let mut bytes = [0u8; 9];
        self.random.fill(&mut bytes)?;
        let code = pairing_user_code(&bytes);
        let expires_at = self
            .clock
            .now()
            .checked_add_signed(Duration::seconds(
                args.get("expires_in_seconds")
                    .and_then(Value::as_i64)
                    .unwrap_or(600),
            ))
            .ok_or(LocalNodesError::Internal)?;
        let hash = format!("{:x}", Sha256::digest(code.as_bytes()));
        let id = challenge_id.clone();
        let owner = owner.clone();
        self.operations.acquire().await.map_err(pg_error)?.execute(move |connection| Box::pin(async move {
            sqlx::query("INSERT INTO local_node_pairing_challenges(challenge_id,tenant_id,user_id,user_code_sha256,expires_at) VALUES($1::text::uuid,$2,$3,$4,$5)")
                .bind(id).bind(owner.tenant_id).bind(owner.user_id).bind(hash).bind(expires_at).execute(connection).await?;
            Ok(())
        })).await.map_err(pg_error)?;
        Ok(
            json!({"challenge_id":challenge_id,"user_code":code,"expires_at":python_datetime(expires_at)}),
        )
    }
    async fn complete(
        &self,
        owner: &LocalNodesIdentity,
        args: Value,
    ) -> Result<Value, LocalNodesError> {
        owner_required(owner)?;
        let challenge_id = parse_uuid(&text_arg(&args, "challenge_id", ""), "pairing challenge")?;
        let device_id =
            channel_text(&args, "device_id").unwrap_or_else(|| Uuid::new_v4().to_string());
        let channel_id =
            channel_text(&args, "channel_id").unwrap_or_else(|| Uuid::new_v4().to_string());
        let fingerprint =
            channel_text(&args, "fingerprint").unwrap_or_else(|| "unavailable".to_owned());
        let display_name = text_arg(&args, "display_name", "Local Node");
        let platform = text_arg(&args, "platform", "unknown");
        let version = text_arg(&args, "node_version", "unknown");
        let protocol = text_arg(&args, "protocol_version", "ai-platform.local-node.v1");
        let claims = args
            .get("capability_claims")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or_else(|| json!([]));
        let digest = text_arg(&args, "permission_snapshot_digest", "");
        let response = json!({"device":{"device_id":device_id,"display_name":display_name,"platform":platform,"node_version":version,"status":"online","channel_id":channel_id}});
        let owner = owner.clone();
        let missing = Arc::new(AtomicBool::new(false));
        let missing_in_transaction = missing.clone();
        let result = self.operations.acquire().await.map_err(pg_error)?.execute(move |connection| Box::pin(async move {
            let consumed: Option<String> = sqlx::query_scalar("UPDATE local_node_pairing_challenges SET consumed_at=now() WHERE challenge_id=$1::text::uuid AND tenant_id=$2 AND user_id=$3 AND consumed_at IS NULL AND expires_at>now() RETURNING challenge_id::text")
                .bind(challenge_id).bind(&owner.tenant_id).bind(&owner.user_id).fetch_optional(&mut *connection).await?;
            if consumed.is_none() {
                missing_in_transaction.store(true, Ordering::Release);
                // The supervisor must see Err and roll back, never Ok(domain_error).
                // A dedicated SQLSTATE distinguishes this sentinel from a failed
                // rollback or a disconnected backend; neither becomes a 409.
                sqlx::query("DO $$ BEGIN RAISE SQLSTATE 'PZ001'; END $$").execute(&mut *connection).await?;
                return Err(sqlx::Error::RowNotFound);
            }
            sqlx::query("INSERT INTO local_node_devices(device_id,tenant_id,user_id,display_name,platform,node_version,protocol_version,status,capability_revision,capabilities,permission_snapshot_digest,last_seen_at) VALUES($1,$2,$3,$4,$5,$6,$7,'online',1,$8::jsonb,$9,now()) ON CONFLICT(device_id) DO UPDATE SET display_name=EXCLUDED.display_name,platform=EXCLUDED.platform,node_version=EXCLUDED.node_version,protocol_version=EXCLUDED.protocol_version,status='online',capability_revision=local_node_devices.capability_revision+1,capabilities=EXCLUDED.capabilities,permission_snapshot_digest=EXCLUDED.permission_snapshot_digest,last_seen_at=now(),revoked_at=NULL WHERE local_node_devices.tenant_id=$2 AND local_node_devices.user_id=$3")
                .bind(&device_id).bind(&owner.tenant_id).bind(&owner.user_id).bind(display_name).bind(platform).bind(version).bind(protocol).bind(claims).bind(digest).execute(&mut *connection).await?;
            // Canonical source ignores affected rows for both conditional
            // conflict updates. Do not silently turn a no-op into rejection.
            sqlx::query("INSERT INTO local_node_channels(channel_id,device_id,tenant_id,user_id,fingerprint,status,last_seen_at) VALUES($1,$2,$3,$4,$5,'online',now()) ON CONFLICT(channel_id) DO UPDATE SET status='online',last_seen_at=now() WHERE local_node_channels.device_id=$2 AND local_node_channels.tenant_id=$3 AND local_node_channels.user_id=$4")
                .bind(channel_id).bind(device_id).bind(owner.tenant_id).bind(owner.user_id).bind(fingerprint).execute(&mut *connection).await?;
            Ok(())
        })).await;
        match result {
            Ok(()) => Ok(response),
            Err(error) if missing.load(Ordering::Acquire) && error.sqlstate() == Some("PZ001") => {
                Err(LocalNodesError::repository(
                    "pairing challenge is expired, consumed, or not owned",
                ))
            }
            Err(error) => Err(pg_error(error)),
        }
    }
    async fn list_devices(&self, owner: &LocalNodesIdentity) -> Result<Value, LocalNodesError> {
        owner_required(owner)?;
        let owner = owner.clone();
        let rows: Vec<Value> = self.operations.acquire().await.map_err(pg_error)?.execute(move |connection| Box::pin(async move {
            sqlx::query_scalar("SELECT to_jsonb(d) FROM (SELECT device_id,display_name,platform,node_version,status,last_seen_at,capability_revision FROM local_node_devices WHERE tenant_id=$1 AND user_id=$2 AND revoked_at IS NULL ORDER BY created_at DESC) d")
                .bind(owner.tenant_id).bind(owner.user_id).fetch_all(connection).await
        })).await.map_err(pg_error)?;
        Ok(json!({"devices": rows.into_iter().map(source_row).collect::<Vec<_>>()}))
    }
    async fn list_grants(
        &self,
        owner: &LocalNodesIdentity,
        device_id: String,
    ) -> Result<Value, LocalNodesError> {
        self.device(owner, &device_id).await?;
        let owner = owner.clone();
        let rows: Vec<Value> = self.operations.acquire().await.map_err(pg_error)?.execute(move |connection| Box::pin(async move {
            sqlx::query_scalar("SELECT to_jsonb(g) FROM (SELECT grant_id,device_id,kind,display_name,capabilities::text AS capabilities,resource_ref,session_id,revision,status,created_at,expires_at FROM local_node_grants WHERE tenant_id=$1 AND user_id=$2 AND device_id=$3 ORDER BY created_at DESC) g")
                .bind(owner.tenant_id).bind(owner.user_id).bind(device_id).fetch_all(connection).await
        })).await.map_err(pg_error)?;
        Ok(json!({"grants": rows.into_iter().map(source_row).collect::<Vec<_>>()}))
    }
    async fn create_grant(
        &self,
        owner: &LocalNodesIdentity,
        args: Value,
    ) -> Result<Value, LocalNodesError> {
        let device_id = text_arg(&args, "device_id", "");
        self.device(owner, &device_id).await?;
        let owner = owner.clone();
        let grant_id = Uuid::new_v4().to_string();
        let kind = text_arg(&args, "kind", "");
        let display = text_arg(&args, "display_name", "");
        let capabilities = args
            .get("capabilities")
            .cloned()
            .ok_or(LocalNodesError::Internal)?;
        let resource = optional_text(&args, "resource_ref");
        let session = optional_text(&args, "session_id");
        let row: Value = self.operations.acquire().await.map_err(pg_error)?.execute(move |connection| Box::pin(async move {
            sqlx::query_scalar("WITH inserted AS (INSERT INTO local_node_grants(grant_id,tenant_id,user_id,device_id,kind,display_name,capabilities,resource_ref,session_id,revision,status) VALUES($1::text::uuid,$2,$3,$4,$5,$6,$7::jsonb,$8,$9,1,'active') RETURNING *) SELECT to_jsonb(g) || jsonb_build_object('capabilities',g.capabilities::text) FROM inserted g")
                .bind(grant_id).bind(owner.tenant_id).bind(owner.user_id).bind(device_id).bind(kind).bind(display).bind(capabilities).bind(resource).bind(session).fetch_one(connection).await
        })).await.map_err(pg_error)?;
        Ok(json!({"grant":source_row(row)}))
    }
    async fn revoke_grant(
        &self,
        owner: &LocalNodesIdentity,
        args: Value,
    ) -> Result<Value, LocalNodesError> {
        let grant_id = parse_uuid(&text_arg(&args, "grant_id", ""), "grant")?;
        let device_id = text_arg(&args, "device_id", "");
        let owner = owner.clone();
        let row: Option<Value> = self.operations.acquire().await.map_err(pg_error)?.execute(move |connection| Box::pin(async move {
            sqlx::query_scalar("WITH revoked AS (UPDATE local_node_grants SET status='revoked',revision=revision+1,revoked_at=now() WHERE grant_id=$1::text::uuid AND tenant_id=$2 AND user_id=$3 AND device_id=$4 RETURNING grant_id,device_id,revision,revoked_at) SELECT to_jsonb(g) FROM revoked g")
                .bind(grant_id).bind(owner.tenant_id).bind(owner.user_id).bind(device_id).fetch_optional(connection).await
        })).await.map_err(pg_error)?;
        let mut row =
            source_row(row.ok_or_else(|| {
                LocalNodesError::repository("grant is not owned or does not exist")
            })?);
        row["revoked"] = json!(true);
        Ok(row)
    }
    async fn list_events(
        &self,
        owner: &LocalNodesIdentity,
        args: Value,
    ) -> Result<Value, LocalNodesError> {
        let device_id = text_arg(&args, "device_id", "");
        self.device(owner, &device_id).await?;
        // Python accepts arbitrary-size query integers; asyncpg then rejects
        // values outside int64 after the ownership read. Never reset to zero.
        let after = match args.get("after_sequence") {
            None => 0,
            Some(value) => value.as_i64().ok_or(LocalNodesError::Internal)?,
        };
        let owner = owner.clone();
        let selected_device = device_id.clone();
        let rows: Vec<Value> = self.operations.acquire().await.map_err(pg_error)?.execute(move |connection| Box::pin(async move {
            // Source uses per-execution sequence, not device_sequence. Retain
            // its duplicate-sequence cursor limitation and fixed 500 row cap.
            sqlx::query_scalar("SELECT to_jsonb(e) FROM (SELECT event_id,sequence,channel_id,execution_id,event,status,payload::text AS payload,created_at FROM local_node_events WHERE tenant_id=$1 AND user_id=$2 AND device_id=$3 AND sequence>$4 ORDER BY sequence LIMIT 500) e")
                .bind(owner.tenant_id).bind(owner.user_id).bind(selected_device).bind(after).fetch_all(connection).await
        })).await.map_err(pg_error)?;
        Ok(
            json!({"device_id":device_id,"events":rows.into_iter().map(source_row).collect::<Vec<_>>(),"after_sequence":after}),
        )
    }
}
impl LocalNodesControlPlane for PgLocalNodesControlPlane {
    fn call<'a>(
        &'a self,
        operation: &'a str,
        owner: &'a LocalNodesIdentity,
        args: Value,
    ) -> LocalNodesFuture<'a, Value> {
        Box::pin(async move {
            match operation {
                "pairing.create" => self.create_challenge(owner, args).await,
                "pairing.complete" => self.complete(owner, args).await,
                "devices.list" => self.list_devices(owner).await,
                "device.status" => {
                    Ok(json!({"device":self.device(owner,&text_arg(&args,"device_id","")).await?}))
                }
                "device.capabilities" => {
                    let id = text_arg(&args, "device_id", "");
                    let device = self.device(owner, &id).await?;
                    Ok(
                        json!({"device_id":id,"revision":device["capability_revision"],"capabilities":device.get("capabilities").cloned().unwrap_or_else(||json!([]))}),
                    )
                }
                "device.doctor" => {
                    let id = text_arg(&args, "device_id", "");
                    self.device(owner, &id).await?;
                    Ok(json!({"device_id":id,"status":"ready","permissions":[]}))
                }
                "grants.list" => {
                    self.list_grants(owner, text_arg(&args, "device_id", ""))
                        .await
                }
                "grants.create" => self.create_grant(owner, args).await,
                "grants.revoke" => self.revoke_grant(owner, args).await,
                "events.list" => self.list_events(owner, args).await,
                _ => Err(LocalNodesError::repository(
                    "unsupported Local Node operation",
                )),
            }
        })
    }
}
