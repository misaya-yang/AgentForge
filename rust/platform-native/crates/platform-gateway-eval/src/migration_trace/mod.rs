//! Tenant-scoped trace, span, event and score authority from AgentTraceRepository.
//! Evaluation dataset/experiment/outbox scheduling lives in migration_compat.
pub mod capture;
mod dashboard;
mod dashboard_sql;
mod detail;
mod reconcile;
mod reconcile_sql;
pub mod retention;
pub mod usage;
pub use reconcile::RuntimeTraceScope;
mod list;
pub use list::TraceFilter;
mod ingest;
mod scores;
mod summary;
mod summary_sql;
pub use summary::AgentOperationsFilter;
pub mod sql;
use crate::postgres_operation::{PgOperationError, PgOperationStorage, PgQueryFuture};
use serde_json::{Value, json};
use sqlx::{Acquire, PgPool};
#[derive(Clone, Debug)]
pub struct TraceError {
    pub status: u16,
    pub code: &'static str,
    pub sqlstate: Option<String>,
}
impl TraceError {
    pub fn not_found() -> Self {
        Self {
            status: 404,
            code: "Trace not found",
            sqlstate: None,
        }
    }
    pub fn invalid() -> Self {
        Self {
            status: 422,
            code: "invalid_trace_payload",
            sqlstate: None,
        }
    }
    pub fn lease_lost() -> Self {
        Self {
            status: 409,
            code: "eval_outbox_lease_lost",
            sqlstate: None,
        }
    }
}
impl std::fmt::Display for TraceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}
impl std::error::Error for TraceError {}
impl From<PgOperationError> for TraceError {
    fn from(e: PgOperationError) -> Self {
        Self {
            status: 503,
            code: "trace_storage_unavailable",
            sqlstate: e.sqlstate().map(str::to_owned),
        }
    }
}
impl From<serde_json::Error> for TraceError {
    fn from(_: serde_json::Error) -> Self {
        Self::invalid()
    }
}
#[derive(Clone)]
pub struct OutboxClaim {
    pub job_id: String,
    pub tenant_id: String,
    pub owner_id: String,
    pub claim_token: String,
}
#[derive(Clone)]
pub struct TraceRepository {
    operations: PgOperationStorage,
    claim: Option<OutboxClaim>,
}
impl TraceRepository {
    pub fn new(pool: PgPool, control_pool: PgPool) -> Result<Self, TraceError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool, control_pool)?,
            claim: None,
        })
    }
    pub fn with_outbox_claim(&self, job: &Value) -> Result<Self, TraceError> {
        let field = |key: &str| {
            job[key]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or_else(TraceError::lease_lost)
        };
        Ok(Self {
            operations: self.operations.clone(),
            claim: Some(OutboxClaim {
                job_id: field("job_id")?,
                tenant_id: field("tenant_id")?,
                owner_id: field("owner_id")?,
                claim_token: field("claim_token")?,
            }),
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), TraceError> {
        Ok(self.operations.drain_cancelled().await?)
    }
    async fn run<T, F>(&self, f: F) -> Result<T, TraceError>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut sqlx::PgConnection) -> PgQueryFuture<'c, T> + Send + 'static,
    {
        let claim = self.claim.clone();
        let mut operation = self.operations.acquire().await?;
        let result=operation.execute(move|conn|Box::pin(async move{if let Some(claim)=claim{let mut transaction=conn.begin().await?;let owned=sqlx::query_scalar::<_,String>("SELECT job_id::text FROM agent_trace_outbox WHERE job_id=$1::uuid AND tenant_id=$2 AND owner_id=$3 AND claim_token=$4::uuid AND status='running' AND lease_until>clock_timestamp() FOR UPDATE").bind(claim.job_id).bind(claim.tenant_id).bind(claim.owner_id).bind(claim.claim_token).fetch_optional(&mut *transaction).await?;if owned.is_none(){transaction.rollback().await?;return Ok(None);}let value=f(&mut transaction).await?;transaction.commit().await?;Ok(Some(value))}else{Ok(Some(f(conn).await?))}})).await?;
        result.ok_or_else(TraceError::lease_lost)
    }
    pub async fn abandon_outbox_claim(&self) -> Result<(), TraceError> {
        let job = self
            .claim
            .as_ref()
            .ok_or_else(TraceError::lease_lost)?
            .job_id
            .clone();
        self.run(move|conn|Box::pin(async move{sqlx::query("UPDATE agent_trace_outbox SET status='queued',available_at=NOW(),lease_until=NULL,updated_at=NOW() WHERE job_id=$1::uuid").bind(job).execute(conn).await?;Ok(())})).await
    }
}
fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").into()
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
        Value::Number(v) => v.as_f64() != Some(0.),
    }
}
fn string_or(v: &Value, key: &str, default: &str) -> String {
    v.get(key)
        .filter(|v| truthy(v))
        .map(pystr)
        .unwrap_or_else(|| default.into())
}
fn pystr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}
fn decode(mut row: Value, kind: &str) -> Value {
    let keys: &[&str] = match kind {
        "trace" => &["redaction_state", "metadata", "metrics", "privacy"],
        "span" => &["attributes"],
        "event" => &["payload"],
        _ => &["metadata"],
    };
    for key in keys {
        let value = row.get(*key).cloned().unwrap_or(Value::Null);
        row[*key] = if let Value::String(raw) = value {
            serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
        } else if value.is_null() {
            json!({})
        } else {
            value
        };
    }
    if kind == "trace" && row["scores_count"].is_null() {
        row["scores_count"] = json!(0);
    }
    row
}
