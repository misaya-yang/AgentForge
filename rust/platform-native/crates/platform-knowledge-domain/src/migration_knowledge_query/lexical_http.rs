//! Pre-release lexical control plane. Cutover stays test-tenant/operator gated;
//! emergency rollback ignores both the release allowlist and service kill switch.
use super::{
    auth,
    lexical_lifecycle::LexicalLifecycle,
    migration::{MResult, MigrationError},
    types::{QueryError, UserContext},
};
use crate::migration_knowledge_ingest::{IngestError, batches::DocumentBatchStore};
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};
#[derive(Clone)]
pub struct LexicalApiState {
    pub service: Arc<LexicalLifecycle>,
    pub cutover_test_tenants: BTreeSet<String>,
}
pub fn routes(state: LexicalApiState) -> Router {
    let p = "/api/v1/knowledge/datasets/{dataset_id}/bm25-v2";
    Router::new()
        .route(p, get(state_view))
        .route(&format!("{p}/verify"), post(verify))
        .route(&format!("{p}/cutover/dry-run"), post(dry_run))
        .route(&format!("{p}/cutover"), post(cutover))
        .route(&format!("{p}/rollback"), post(rollback))
        .route(&format!("{p}/rebuild/jobs"), post(rebuild))
        .route(
            &format!("{p}/rebuild/jobs/{{job_id}}"),
            get(job).delete(cancel),
        )
        .with_state(state)
}
pub fn gateway_router(
    state: LexicalApiState,
    jwt: Arc<dyn ai_platform_service_primitives::actor_verifier::JwtActorVerifier>,
) -> Router {
    routes(state).layer(axum::middleware::from_fn_with_state(jwt, auth::verify_jwt))
}
pub fn knowledge_service_router(
    state: LexicalApiState,
    hmac: Arc<auth::GatewayAuth>,
) -> super::types::Result<Router> {
    hmac.validate_service_boundary()?;
    Ok(routes(state).layer(axum::middleware::from_fn_with_state(
        hmac,
        auth::verify_gateway,
    )))
}
fn mapped(e: QueryError) -> MigrationError {
    MigrationError {
        status: match e {
            QueryError::NotFound => 404,
            QueryError::Forbidden => 403,
            QueryError::CollectionAuthority(_)
            | QueryError::Validation(_)
            | QueryError::GenerationChanged
            | QueryError::PublicationInProgress => 409,
            _ => 503,
        },
        message: e.to_string(),
    }
}
fn batch_error(e: IngestError) -> MigrationError {
    MigrationError {
        status: match e {
            IngestError::NotFound => 404,
            IngestError::Permission => 403,
            IngestError::Validation(_)
            | IngestError::LeaseBusy
            | IngestError::GenerationChanged => 409,
            _ => 503,
        },
        message: e.to_string(),
    }
}
fn operator(u: &UserContext) -> MResult<()> {
    if u.user_tier.trim().eq_ignore_ascii_case("admin")
        || u.roles
            .iter()
            .any(|r| ["admin", "operator"].contains(&r.trim().to_lowercase().as_str()))
    {
        Ok(())
    } else {
        Err(MigrationError {
            status: 403,
            message: "BM25 v2 operator role required".into(),
        })
    }
}
fn blocked(mut v: Value) -> Json<Value> {
    v["release_status"] = json!("BLOCKED");
    Json(v)
}
async fn state_view(
    State(s): State<LexicalApiState>,
    Extension(u): Extension<UserContext>,
    Path(id): Path<String>,
) -> MResult<Json<Value>> {
    s.service
        .store
        .require_dataset_access(&u, &id, "owner")
        .await?;
    Ok(blocked(
        s.service.get_lifecycle_state(&id).await.map_err(mapped)?,
    ))
}
async fn verify(
    State(s): State<LexicalApiState>,
    Extension(u): Extension<UserContext>,
    Path(id): Path<String>,
) -> MResult<Json<Value>> {
    s.service
        .store
        .require_dataset_access(&u, &id, "owner")
        .await?;
    Ok(blocked(
        s.service
            .verify_cross_authority(&id)
            .await
            .map_err(mapped)?,
    ))
}
#[derive(Default, Deserialize)]
struct Cutover {
    expected_manifest_sha256: Option<String>,
}
fn manifest(p: &Cutover) -> MResult<Option<&str>> {
    if p.expected_manifest_sha256
        .as_ref()
        .is_some_and(|s| s.chars().count() != 64)
    {
        return Err(MigrationError::invalid(
            "expected manifest must contain 64 characters",
        ));
    }
    Ok(p.expected_manifest_sha256.as_deref())
}
async fn dry_run(
    State(s): State<LexicalApiState>,
    Extension(u): Extension<UserContext>,
    Path(id): Path<String>,
    p: Option<Json<Cutover>>,
) -> MResult<Json<Value>> {
    s.service
        .store
        .require_dataset_access(&u, &id, "owner")
        .await?;
    let p = p.map(|p| p.0).unwrap_or_default();
    Ok(blocked(
        s.service
            .cutover(&id, false, manifest(&p)?)
            .await
            .map_err(mapped)?,
    ))
}
async fn cutover(
    State(s): State<LexicalApiState>,
    Extension(u): Extension<UserContext>,
    Path(id): Path<String>,
    p: Option<Json<Cutover>>,
) -> MResult<Json<Value>> {
    let d = s
        .service
        .store
        .require_dataset_access(&u, &id, "owner")
        .await?;
    operator(&u)?;
    if d.tenant_id.is_empty() || !s.cutover_test_tenants.contains(&d.tenant_id) {
        return Err(MigrationError {
            status: 403,
            message: "BM25 v2 cutover is restricted to configured test tenants".into(),
        });
    }
    let p = p.map(|p| p.0).unwrap_or_default();
    if s.service.vectors.is_read_only() {
        let mut result = crate::migration_knowledge_ingest::vector_jobs::submit(&s.service.store.pool, &u, &id,
            crate::migration_knowledge_ingest::vector_jobs::Mutation::LexicalCutover { manifest: manifest(&p)?.map(str::to_owned) })
            .await.map_err(|e| MigrationError { status: match e { crate::migration_knowledge_ingest::IngestError::Permission => 403, crate::migration_knowledge_ingest::IngestError::NotFound => 404, crate::migration_knowledge_ingest::IngestError::Database => 503, _ => 409 }, message: e.to_string() })?;
        result["scope"] = json!("test_tenant_only");
        return Ok(blocked(result));
    }
    let mut result = s
        .service
        .cutover(&id, true, manifest(&p)?)
        .await
        .map_err(mapped)?;
    result["scope"] = json!("test_tenant_only");
    Ok(blocked(result))
}
#[derive(Deserialize)]
struct Rollback {
    #[serde(default = "yes")]
    keep_shadow_writes: bool,
}
fn yes() -> bool {
    true
}
async fn rollback(
    State(s): State<LexicalApiState>,
    Extension(u): Extension<UserContext>,
    Path(id): Path<String>,
    p: Option<Json<Rollback>>,
) -> MResult<Json<Value>> {
    s.service
        .store
        .require_dataset_access(&u, &id, "owner")
        .await?;
    operator(&u)?;
    let keep_shadow = p.map(|p| p.keep_shadow_writes).unwrap_or(true);
    if s.service.vectors.is_read_only() {
        return Ok(Json(crate::migration_knowledge_ingest::vector_jobs::submit(&s.service.store.pool, &u, &id,
            crate::migration_knowledge_ingest::vector_jobs::Mutation::LexicalRollback { keep_shadow })
            .await.map_err(|e| MigrationError { status: match e { crate::migration_knowledge_ingest::IngestError::Permission => 403, crate::migration_knowledge_ingest::IngestError::NotFound => 404, crate::migration_knowledge_ingest::IngestError::Database => 503, _ => 409 }, message: e.to_string() })?));
    }
    Ok(Json(
        s.service
            .rollback(&id, true, keep_shadow)
            .await
            .map_err(mapped)?,
    ))
}
fn receipt(id: &str, mut operation: Value, revision: Option<i64>) -> Value {
    let job = operation["operation_id"].as_str().unwrap_or("").to_owned();
    operation["job_id"] = json!(job);
    operation["execution_id"] = json!(job);
    operation["job_url"] = json!(format!(
        "/api/v1/knowledge/datasets/{id}/bm25-v2/rebuild/jobs/{job}"
    ));
    operation["job_kind"] = json!("bm25_v2_rebuild");
    if let Some(r) = revision {
        operation["content_revision"] = json!(r)
    }
    operation
}
async fn rebuild(
    State(s): State<LexicalApiState>,
    Extension(u): Extension<UserContext>,
    Path(id): Path<String>,
) -> MResult<(StatusCode, Json<Value>)> {
    let d = s
        .service
        .store
        .require_dataset_access(&u, &id, "owner")
        .await?;
    let lifecycle = s.service.get_lifecycle_state(&id).await.map_err(mapped)?;
    let revision = lifecycle["content_revision"].as_i64().unwrap_or(0);
    let namespace = uuid::Uuid::from_u128(0x109ba33d538e46fabd292b088e30b9e6);
    let job = uuid::Uuid::new_v5(
        &namespace,
        format!("{}:{id}:{revision}", d.tenant_id).as_bytes(),
    )
    .to_string();
    let store = DocumentBatchStore::new(s.service.store.pool.clone());
    let op = store
        .create_operation(
            &d.tenant_id,
            &id,
            "reembed",
            &u.user_id,
            &u.roles,
            &[],
            true,
            Some(&job),
        )
        .await
        .map_err(batch_error)?;
    Ok((StatusCode::ACCEPTED, Json(receipt(&id, op, Some(revision)))))
}
async fn job(
    State(s): State<LexicalApiState>,
    Extension(u): Extension<UserContext>,
    Path((id, job)): Path<(String, String)>,
) -> MResult<Json<Value>> {
    let d = s
        .service
        .store
        .require_dataset_access(&u, &id, "owner")
        .await?;
    uuid::Uuid::parse_str(&job).map_err(|_| MigrationError {
        status: 404,
        message: "BM25 rebuild job not found".into(),
    })?;
    let op = DocumentBatchStore::new(s.service.store.pool.clone())
        .get_operation(&job, &d.tenant_id, &id)
        .await
        .map_err(batch_error)?
        .ok_or_else(|| MigrationError {
            status: 404,
            message: "BM25 rebuild job not found".into(),
        })?;
    Ok(Json(receipt(&id, op, None)))
}
async fn cancel(
    State(s): State<LexicalApiState>,
    Extension(u): Extension<UserContext>,
    Path((id, job)): Path<(String, String)>,
) -> MResult<Json<Value>> {
    let d = s
        .service
        .store
        .require_dataset_access(&u, &id, "owner")
        .await?;
    uuid::Uuid::parse_str(&job).map_err(|_| MigrationError {
        status: 404,
        message: "BM25 rebuild job not found".into(),
    })?;
    let op = DocumentBatchStore::new(s.service.store.pool.clone())
        .cancel_operation(&job, &d.tenant_id, &id)
        .await
        .map_err(batch_error)?
        .ok_or_else(|| MigrationError {
            status: 404,
            message: "BM25 rebuild job not found".into(),
        })?;
    Ok(Json(receipt(&id, op, None)))
}
