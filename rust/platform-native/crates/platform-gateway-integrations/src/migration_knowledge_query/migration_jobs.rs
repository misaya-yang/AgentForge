//! Durable action ledger, claim-token fencing, lease renewal and crash recovery.
use super::migration::{EmbeddingMigration, MResult, MigrationError, MigrationStore, digest, text};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::watch;
pub fn public_job(job: &Value) -> Value {
    let fields = [
        "job_id",
        "migration_id",
        "dataset_id",
        "action",
        "request_hash",
        "state",
        "payload",
        "result",
        "error",
        "requested_by",
        "attempt_count",
        "available_at",
        "lease_expires_at",
        "last_heartbeat_at",
        "started_at",
        "finished_at",
        "created_at",
        "updated_at",
    ];
    let mut out = json!({});
    for k in fields {
        if let Some(v) = job.get(k) {
            out[k] = v.clone()
        }
    }
    out
}
pub fn normalize(action: &str, payload: &Value) -> MResult<Value> {
    if !["backfill", "verify", "gate"].contains(&action) {
        return Err(MigrationError::invalid("unsupported embedding action"));
    }
    let mut p = payload
        .as_object()
        .cloned()
        .ok_or_else(|| MigrationError::invalid("action payload must be an object"))?;
    p.retain(|_, v| !v.is_null());
    if action != "gate" && !p.is_empty() {
        return Err(MigrationError::invalid("action does not accept parameters"));
    }
    for (k, v) in &p {
        match k.as_str() {
            "sample_size" => {
                if !v.as_u64().is_some_and(|n| (1..=256).contains(&n)) {
                    return Err(MigrationError::invalid("sample_size must be 1..256"));
                }
            }
            "top_k" => {
                if !v.as_u64().is_some_and(|n| (1..=50).contains(&n)) {
                    return Err(MigrationError::invalid("top_k must be 1..50"));
                }
            }
            "tolerance" | "floor" => {
                if !v
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n))
                {
                    return Err(MigrationError::invalid("gate rate must be 0..1"));
                }
            }
            _ => return Err(MigrationError::invalid("unknown gate parameter")),
        }
    }
    Ok(Value::Object(p))
}
fn allowed(action: &str, state: &str) -> bool {
    match action {
        "backfill" => ["shadow_build", "backfilling", "failed"].contains(&state),
        "verify" => ["backfilling", "verified", "failed"].contains(&state),
        "gate" => ["verified", "gate_failed", "gating"].contains(&state),
        _ => false,
    }
}
impl MigrationStore {
    pub async fn enqueue(
        &self,
        id: &str,
        action: &str,
        payload: Value,
        user: &str,
    ) -> MResult<(Value, bool)> {
        let action = action.trim().to_lowercase();
        let payload = normalize(&action, &payload)?;
        let request_hash = digest(format!(
            "{action}\0{}",
            crate::migration_content::python_json(&payload, false, false)
        ));
        let mut tx = self.pool.begin().await?;
        let migration: Option<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(m) FROM embedding_migrations m WHERE migration_id=$1::uuid FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let m = migration.ok_or_else(|| MigrationError::conflict("migration not found"))?;
        let dataset = text(&m, "dataset_id");
        let active:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(j) FROM knowledge.embedding_migration_action_jobs j WHERE dataset_id=$1 AND state IN ('queued','running') ORDER BY created_at DESC LIMIT 1 FOR UPDATE").bind(dataset).fetch_optional(&mut *tx).await?;
        if let Some(j) = active {
            if j["migration_id"] != id
                || j["action"] != action
                || j["request_hash"] != request_hash
                || j["payload"] != payload
            {
                return Err(MigrationError::conflict(
                    "dataset already has another active action or parameters",
                ));
            }
            tx.commit().await?;
            return Ok((j, true));
        }
        let latest:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(j) FROM knowledge.embedding_migration_action_jobs j WHERE migration_id=$1::uuid AND action=$2 ORDER BY created_at DESC LIMIT 1 FOR UPDATE").bind(id).bind(&action).fetch_optional(&mut *tx).await?;
        if let Some(j) = &latest {
            if j["state"] == "succeeded" {
                let pending = Self::pending_on(&mut tx, id, dataset).await?;
                let state = text(&m, "state");
                let evidence = if action == "verify" {
                    &m["totals"]["verified_authority"]
                } else {
                    &m["gate"]["authority_snapshot"]
                };
                let revision: Option<i64> =
                    sqlx::query_scalar("SELECT content_revision FROM datasets WHERE dataset_id=$1")
                        .bind(dataset)
                        .fetch_optional(&mut *tx)
                        .await?
                        .flatten();
                let current =
                    revision.is_some() && revision == evidence["content_revision"].as_i64();
                let done = pending == 0
                    && match action.as_str() {
                        "backfill" => [
                            "backfilling",
                            "verified",
                            "gating",
                            "gate_failed",
                            "ready",
                            "completed",
                        ]
                        .contains(&state),
                        "verify" => {
                            current
                                && ["verified", "gating", "gate_failed", "ready", "completed"]
                                    .contains(&state)
                        }
                        "gate" => current && ["ready", "completed"].contains(&state),
                        _ => false,
                    };
                if done {
                    if j["request_hash"] != request_hash || j["payload"] != payload {
                        return Err(MigrationError::conflict(
                            "completed action has different parameters",
                        ));
                    }
                    tx.commit().await?;
                    return Ok((j.clone(), true));
                }
            }
        }
        if !allowed(&action, text(&m, "state")) {
            return Err(MigrationError::conflict(
                "migration state cannot enqueue action",
            ));
        }
        if action == "verify" && Self::pending_on(&mut tx, id, dataset).await? != 0 {
            return Err(MigrationError::conflict("backfill is incomplete"));
        }
        let job = if let Some(j) = latest.filter(|j| {
            j["state"] == "failed" && j["request_hash"] == request_hash && j["payload"] == payload
        }) {
            sqlx::query_scalar("UPDATE knowledge.embedding_migration_action_jobs SET state='queued',payload=$2,result=NULL,error=NULL,requested_by=$3,claimed_by=NULL,claim_token=NULL,terminal_claim_token=NULL,available_at=NOW(),lease_expires_at=NULL,last_heartbeat_at=NULL,started_at=NULL,finished_at=NULL,updated_at=NOW() WHERE job_id=$1::uuid RETURNING to_jsonb(embedding_migration_action_jobs)").bind(text(&j,"job_id")).bind(&payload).bind(user).fetch_one(&mut *tx).await?
        } else {
            sqlx::query_scalar("INSERT INTO knowledge.embedding_migration_action_jobs(migration_id,dataset_id,action,request_hash,payload,requested_by) VALUES($1::uuid,$2,$3,$4,$5,$6) RETURNING to_jsonb(embedding_migration_action_jobs)").bind(id).bind(dataset).bind(&action).bind(&request_hash).bind(&payload).bind(user).fetch_one(&mut *tx).await?
        };
        tx.commit().await?;
        Ok((job, false))
    }
    pub async fn action_job(&self, id: &str, migration: &str) -> MResult<Value> {
        if uuid::Uuid::parse_str(id).is_err() {
            return Err(MigrationError {
                status: 404,
                message: "action job not found".into(),
            });
        }
        let row:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(j) FROM knowledge.embedding_migration_action_jobs j WHERE job_id=$1::uuid AND migration_id=$2::uuid").bind(id).bind(migration).fetch_optional(&self.pool).await?;
        row.ok_or_else(|| MigrationError {
            status: 404,
            message: "action job not found".into(),
        })
    }
    pub async fn cancel_action(&self, id: &str, migration: &str) -> MResult<Value> {
        let mut tx = self.pool.begin().await?;
        let j:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(j) FROM knowledge.embedding_migration_action_jobs j WHERE job_id=$1::uuid AND migration_id=$2::uuid FOR UPDATE").bind(id).bind(migration).fetch_optional(&mut *tx).await?;
        let j = j.ok_or_else(|| MigrationError {
            status: 404,
            message: "action job not found".into(),
        })?;
        if j["state"] == "failed" && j["error"] == "cancelled by operator" {
            tx.commit().await?;
            return Ok(j);
        }
        if !["queued", "running"].contains(&text(&j, "state")) {
            return Err(MigrationError::conflict(
                "terminal action cannot be cancelled",
            ));
        }
        let j:Value=sqlx::query_scalar("UPDATE knowledge.embedding_migration_action_jobs SET state='failed',result=NULL,error='cancelled by operator',claimed_by=NULL,claim_token=NULL,terminal_claim_token=CASE WHEN state='running' THEN claim_token ELSE terminal_claim_token END,lease_expires_at=NULL,last_heartbeat_at=CASE WHEN state='running' THEN NOW() ELSE last_heartbeat_at END,finished_at=NOW(),updated_at=NOW() WHERE job_id=$1::uuid RETURNING to_jsonb(embedding_migration_action_jobs)").bind(id).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(j)
    }
    pub async fn claim_action(&self, worker: &str, lease_seconds: u64) -> MResult<Option<Value>> {
        if worker.trim().is_empty() {
            return Err(MigrationError::invalid("worker_id is required"));
        }
        Ok(sqlx::query_scalar("WITH candidate AS (SELECT job_id,state AS prior_state FROM knowledge.embedding_migration_action_jobs WHERE (state='queued' AND available_at<=NOW()) OR (state='running' AND lease_expires_at<NOW()) ORDER BY CASE WHEN state='running' THEN lease_expires_at ELSE available_at END,created_at FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE knowledge.embedding_migration_action_jobs AS job SET state='running',claimed_by=$1,claim_token=gen_random_uuid(),terminal_claim_token=NULL,lease_expires_at=NOW()+make_interval(secs=>$2),last_heartbeat_at=NOW(),started_at=COALESCE(started_at,NOW()),finished_at=NULL,error=NULL,attempt_count=attempt_count+1,updated_at=NOW() FROM candidate WHERE job.job_id=candidate.job_id RETURNING to_jsonb(job)||jsonb_build_object('prior_state',candidate.prior_state)").bind(worker).bind(lease_seconds.max(1)as f64).fetch_optional(&self.pool).await?)
    }
    pub async fn heartbeat(&self, id: &str, token: &str, lease_seconds: u64) -> MResult<bool> {
        Ok(sqlx::query("UPDATE knowledge.embedding_migration_action_jobs SET lease_expires_at=NOW()+make_interval(secs=>$3),last_heartbeat_at=NOW(),updated_at=NOW() WHERE job_id=$1::uuid AND state='running' AND claim_token=$2::uuid").bind(id).bind(token).bind(lease_seconds.max(1)as f64).execute(&self.pool).await?.rows_affected()==1)
    }
    pub async fn finish(
        &self,
        id: &str,
        token: &str,
        result: Option<Value>,
        error: Option<&str>,
    ) -> MResult<bool> {
        let state = if error.is_some() {
            "failed"
        } else {
            "succeeded"
        };
        let changed=sqlx::query("UPDATE knowledge.embedding_migration_action_jobs SET state=$3,result=$4,error=$5,claimed_by=NULL,terminal_claim_token=claim_token,claim_token=NULL,lease_expires_at=NULL,last_heartbeat_at=NOW(),finished_at=NOW(),updated_at=NOW() WHERE job_id=$1::uuid AND state='running' AND claim_token=$2::uuid").bind(id).bind(token).bind(state).bind(result).bind(error.map(|s|s.chars().take(4000).collect::<String>())).execute(&self.pool).await?.rows_affected();
        if changed == 1 {
            return Ok(true);
        }
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.embedding_migration_action_jobs WHERE job_id=$1::uuid AND state=$3 AND terminal_claim_token=$2::uuid)").bind(id).bind(token).bind(state).fetch_one(&self.pool).await?)
    }
    pub async fn requeue(&self, id: &str, token: &str) -> MResult<bool> {
        Ok(sqlx::query("UPDATE knowledge.embedding_migration_action_jobs SET state='queued',claimed_by=NULL,claim_token=NULL,terminal_claim_token=NULL,available_at=NOW(),lease_expires_at=NULL,last_heartbeat_at=NULL,updated_at=NOW() WHERE job_id=$1::uuid AND state='running' AND claim_token=$2::uuid").bind(id).bind(token).execute(&self.pool).await?.rows_affected()==1)
    }
}
struct RequeueOnCancel {
    store: MigrationStore,
    id: String,
    token: String,
    armed: bool,
}
impl Drop for RequeueOnCancel {
    fn drop(&mut self) {
        if self.armed {
            let s = self.store.clone();
            let id = self.id.clone();
            let token = self.token.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = s.requeue(&id, &token).await;
                });
            }
        }
    }
}
#[derive(Clone)]
pub struct MigrationWorker {
    pub service: Arc<EmbeddingMigration>,
    pub worker_id: String,
    pub lease_seconds: u64,
    pub heartbeat_seconds: u64,
    pub poll_interval: Duration,
}
impl MigrationWorker {
    pub fn new(service: Arc<EmbeddingMigration>, worker_id: String) -> Self {
        Self {
            service,
            worker_id,
            lease_seconds: 120,
            heartbeat_seconds: 30,
            poll_interval: Duration::from_secs(1),
        }
    }
    async fn execute(&self, j: &Value) -> MResult<Value> {
        let id = text(j, "migration_id");
        let job = text(j, "job_id");
        let action = text(j, "action");
        if j["prior_state"] == "running" {
            let m = self.service.store.migration(id).await?;
            if action == "verify"
                && ["verified", "gating", "gate_failed", "ready", "completed"]
                    .contains(&text(&m, "state"))
                && m["totals"]["verify_action_job_id"] == job
            {
                return Ok(json!({"migration":m,"recovered":true}));
            }
            if action == "gate"
                && ["gate_failed", "ready", "completed", "rolled_back"].contains(&text(&m, "state"))
                && m["gate"]["action_job_id"] == job
            {
                return Ok(
                    json!({"migration":m,"verdict":m["gate"],"passed":m["gate"]["passed"] == true,"recovered":true}),
                );
            }
        }
        match action {
            "backfill" => self.service.backfill(id, 200, 1000).await,
            "verify" => self.service.verify(id, job).await,
            "gate" => self.service.gate(id, &j["payload"], job).await,
            _ => Err(MigrationError::invalid("unsupported embedding action")),
        }
    }
    pub async fn run_once(&self) -> MResult<bool> {
        let Some(j) = self
            .service
            .store
            .claim_action(&self.worker_id, self.lease_seconds)
            .await?
        else {
            return Ok(false);
        };
        let id = text(&j, "job_id");
        let token = text(&j, "claim_token");
        let mut guard = RequeueOnCancel {
            store: self.service.store.clone(),
            id: id.into(),
            token: token.into(),
            armed: true,
        };
        let operation = self.execute(&j);
        tokio::pin!(operation);
        let heartbeat = async {
            loop {
                tokio::time::sleep(Duration::from_secs(self.heartbeat_seconds.max(1))).await;
                if !self
                    .service
                    .store
                    .heartbeat(id, token, self.lease_seconds)
                    .await?
                {
                    return Err::<(), MigrationError>(MigrationError::conflict(
                        "migration action lease lost",
                    ));
                }
            }
        };
        tokio::pin!(heartbeat);
        // Durable action completion wins a simultaneous heartbeat failure.
        let outcome =
            tokio::select! {biased;result=&mut operation=>Some(result),_= &mut heartbeat=>None};
        guard.armed = false;
        match outcome {
            Some(Ok(result)) => {
                let _ = self
                    .service
                    .store
                    .finish(id, token, Some(result), None)
                    .await?;
            }
            Some(Err(e)) => {
                let _ = self
                    .service
                    .store
                    .finish(id, token, None, Some(&e.message))
                    .await?;
            }
            None => return Err(MigrationError::conflict("migration action lease lost")),
        }
        Ok(true)
    }
    pub async fn run(&self, mut stop: watch::Receiver<bool>) -> MResult<()> {
        loop {
            if *stop.borrow() {
                return Ok(());
            }
            tokio::select! {changed=stop.changed()=>{if changed.is_err()||*stop.borrow(){return Ok(())}},result=self.run_once()=>{if !result?{tokio::select!{_=tokio::time::sleep(self.poll_interval)=>{},_=stop.changed()=>{}}}}}
        }
    }
}
