//! Owner-gated operator API. Expensive steps only enqueue durable actions.
use super::{
    auth,
    migration::{EmbeddingMigration, MigrationError, text},
    migration_jobs::public_job,
    types::UserContext,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
impl IntoResponse for MigrationError {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"detail":self.message})),
        )
            .into_response()
    }
}
pub fn routes(service: Arc<EmbeddingMigration>) -> Router {
    let base = "/api/v1/knowledge/datasets/{dataset_id}/embedding-migration";
    Router::new()
        .route(base, get(describe))
        .route(&format!("{base}/start"), post(start))
        .route(&format!("{base}/{{migration_id}}/backfill"), post(backfill))
        .route(&format!("{base}/{{migration_id}}/verify"), post(verify))
        .route(&format!("{base}/{{migration_id}}/gate"), post(gate))
        .route(&format!("{base}/{{migration_id}}/cutover"), post(cutover))
        .route(&format!("{base}/{{migration_id}}/rollback"), post(rollback))
        .route(&format!("{base}/{{migration_id}}/abort"), post(abort))
        .route(
            &format!("{base}/{{migration_id}}/jobs/{{job_id}}"),
            get(job).delete(cancel),
        )
        .with_state(service)
}
pub fn gateway_router(
    service: Arc<EmbeddingMigration>,
    auth: Arc<dyn ai_platform_service_primitives::actor_verifier::JwtActorVerifier>,
) -> Router {
    routes(service).layer(middleware::from_fn_with_state(
        auth,
        super::auth::verify_jwt,
    ))
}
pub fn knowledge_service_router(
    service: Arc<EmbeddingMigration>,
    verifier: Arc<auth::GatewayAuth>,
) -> super::types::Result<Router> {
    verifier.validate_service_boundary()?;
    Ok(routes(service).layer(middleware::from_fn_with_state(
        verifier,
        auth::verify_gateway,
    )))
}
async fn scoped(
    s: &EmbeddingMigration,
    u: &UserContext,
    d: &str,
    id: &str,
) -> std::result::Result<(), MigrationError> {
    s.query.require_dataset_access(u, d, "owner").await?;
    let m = s.store.migration(id).await?;
    if m["dataset_id"] != d {
        return Err(MigrationError {
            status: 404,
            message: "migration not found".into(),
        });
    }
    Ok(())
}
async fn worker_mutation(s: &EmbeddingMigration,u: &UserContext,d: &str,operation: crate::migration_knowledge_ingest::vector_jobs::Mutation) -> std::result::Result<Value,MigrationError> {
    crate::migration_knowledge_ingest::vector_jobs::submit(&s.store.pool,u,d,operation).await.map_err(|error| MigrationError {
        status: match error { crate::migration_knowledge_ingest::IngestError::Permission => 403, crate::migration_knowledge_ingest::IngestError::NotFound => 404, crate::migration_knowledge_ingest::IngestError::Validation(_) => 400, crate::migration_knowledge_ingest::IngestError::GenerationChanged | crate::migration_knowledge_ingest::IngestError::LeaseBusy => 409, _ => 503 },
        message: error.to_string(),
    })
}
async fn describe(
    State(s): State<Arc<EmbeddingMigration>>,
    Extension(u): Extension<UserContext>,
    Path(d): Path<String>,
) -> std::result::Result<Json<Value>, MigrationError> {
    let d = s.query.require_dataset_access(&u, &d, "owner").await?;
    Ok(Json(s.describe(&d).await?))
}
#[derive(Deserialize, Serialize)]
struct Start {
    embedding_provider: String,
    embedding_model: String,
    #[serde(default)]
    embedding_model_version: String,
    embedding_dimension: usize,
    capabilities: Option<Vec<String>>,
}
async fn start(
    State(s): State<Arc<EmbeddingMigration>>,
    Extension(u): Extension<UserContext>,
    Path(d): Path<String>,
    Json(p): Json<Start>,
) -> std::result::Result<Json<Value>, MigrationError> {
    let d = s.query.require_dataset_access(&u, &d, "owner").await?;
    let i = serde_json::to_value(&p)
        .map_err(|_| MigrationError::invalid("invalid embedding identity"))?;
    let capabilities = p.capabilities.map(|v| json!(v));
    if s.vectors.is_read_only() {
        return Ok(Json(worker_mutation(&s,&u,&d.dataset_id,crate::migration_knowledge_ingest::vector_jobs::Mutation::EmbeddingStart { target:i,capabilities }).await?));
    }
    Ok(Json(s.start(&d,i,capabilities,"").await?))
}
fn receipt(dataset: &str, migration: &str, job: &Value, reused: bool) -> Value {
    let mut v = public_job(job);
    let id = text(job, "job_id");
    v["reused"] = json!(reused);
    v["execution_id"] = json!(id);
    v["job_url"] = json!(format!(
        "/api/v1/knowledge/datasets/{dataset}/embedding-migration/{migration}/jobs/{id}"
    ));
    v
}
async fn enqueue(
    s: Arc<EmbeddingMigration>,
    u: UserContext,
    d: String,
    id: String,
    action: &'static str,
    payload: Value,
) -> std::result::Result<(StatusCode, Json<Value>), MigrationError> {
    scoped(&s, &u, &d, &id).await?;
    let jid = id.clone();
    let job = tokio::spawn(async move { s.store.enqueue(&jid, action, payload, &u.user_id).await })
        .await
        .map_err(|_| MigrationError {
            status: 503,
            message: "embedding enqueue unavailable".into(),
        })??;
    Ok((StatusCode::ACCEPTED, Json(receipt(&d, &id, &job.0, job.1))))
}
async fn backfill(
    State(s): State<Arc<EmbeddingMigration>>,
    Extension(u): Extension<UserContext>,
    Path((d, id)): Path<(String, String)>,
) -> std::result::Result<(StatusCode, Json<Value>), MigrationError> {
    enqueue(s, u, d, id, "backfill", json!({})).await
}
async fn verify(
    State(s): State<Arc<EmbeddingMigration>>,
    Extension(u): Extension<UserContext>,
    Path((d, id)): Path<(String, String)>,
) -> std::result::Result<(StatusCode, Json<Value>), MigrationError> {
    enqueue(s, u, d, id, "verify", json!({})).await
}
async fn gate(
    State(s): State<Arc<EmbeddingMigration>>,
    Extension(u): Extension<UserContext>,
    Path((d, id)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> std::result::Result<(StatusCode, Json<Value>), MigrationError> {
    enqueue(
        s,
        u,
        d,
        id,
        "gate",
        body.map(|v| v.0).unwrap_or_else(|| json!({})),
    )
    .await
}
async fn job(
    State(s): State<Arc<EmbeddingMigration>>,
    Extension(u): Extension<UserContext>,
    Path((d, id, j)): Path<(String, String, String)>,
) -> std::result::Result<Json<Value>, MigrationError> {
    scoped(&s, &u, &d, &id).await?;
    Ok(Json(receipt(
        &d,
        &id,
        &s.store.action_job(&j, &id).await?,
        false,
    )))
}
async fn cancel(
    State(s): State<Arc<EmbeddingMigration>>,
    Extension(u): Extension<UserContext>,
    Path((d, id, j)): Path<(String, String, String)>,
) -> std::result::Result<Json<Value>, MigrationError> {
    scoped(&s, &u, &d, &id).await?;
    Ok(Json(receipt(
        &d,
        &id,
        &s.store.cancel_action(&j, &id).await?,
        false,
    )))
}
async fn cutover(
    State(s): State<Arc<EmbeddingMigration>>,
    Extension(u): Extension<UserContext>,
    Path((d, id)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> std::result::Result<Json<Value>, MigrationError> {
    scoped(&s, &u, &d, &id).await?;
    let p = body.map(|v| v.0).unwrap_or(Value::Null);
    let retention = if p["retention_seconds"].is_null() {
        604800
    } else {
        p["retention_seconds"]
            .as_i64()
            .filter(|n| *n >= 0)
            .ok_or_else(|| MigrationError::invalid("invalid retention_seconds"))?
    };
    if s.vectors.is_read_only() {
        return Ok(Json(worker_mutation(&s,&u,&d,crate::migration_knowledge_ingest::vector_jobs::Mutation::EmbeddingCutover { migration:id.clone(), retention }).await?));
    }
    Ok(Json(s.cutover(&id, retention).await?))
}
async fn rollback(
    State(s): State<Arc<EmbeddingMigration>>,
    Extension(u): Extension<UserContext>,
    Path((d, id)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> std::result::Result<Json<Value>, MigrationError> {
    scoped(&s, &u, &d, &id).await?;
    let p = body.map(|v| v.0).unwrap_or(Value::Null);
    let keep = if p["keep_shadow"].is_null() {
        true
    } else {
        p["keep_shadow"]
            .as_bool()
            .ok_or_else(|| MigrationError::invalid("invalid keep_shadow"))?
    };
    if s.vectors.is_read_only() {
        return Ok(Json(worker_mutation(&s,&u,&d,crate::migration_knowledge_ingest::vector_jobs::Mutation::EmbeddingRollback { migration:id.clone(), keep_shadow:keep }).await?));
    }
    Ok(Json(s.rollback(&id, keep).await?))
}
async fn abort(
    State(s): State<Arc<EmbeddingMigration>>,
    Extension(u): Extension<UserContext>,
    Path((d, id)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> std::result::Result<Json<Value>, MigrationError> {
    scoped(&s, &u, &d, &id).await?;
    let p = body.map(|v| v.0).unwrap_or(Value::Null);
    let reason = p["reason"].as_str().unwrap_or("aborted");
    let purge = if p["purge_shadow"].is_null() {
        true
    } else {
        p["purge_shadow"]
            .as_bool()
            .ok_or_else(|| MigrationError::invalid("invalid purge_shadow"))?
    };
    if s.vectors.is_read_only() {
        return Ok(Json(worker_mutation(&s,&u,&d,crate::migration_knowledge_ingest::vector_jobs::Mutation::EmbeddingAbort { migration:id.clone(), reason:reason.into(), purge }).await?));
    }
    Ok(Json(s.abort(&id, reason, purge).await?))
}
