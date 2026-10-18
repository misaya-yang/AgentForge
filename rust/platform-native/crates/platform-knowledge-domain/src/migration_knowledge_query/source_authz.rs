//! KS-only signed batch resource authority used by the gateway/Agent plane.
use super::{
    auth::{GatewayAuth, VerifiedGateway},
    store::QueryStore,
    types::{QueryError, Result, UserContext},
};
use axum::{Extension, Json, Router, extract::State, http::HeaderMap, routing::post};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeSet, sync::Arc};
pub fn routes(store: QueryStore) -> Router {
    Router::new()
        .route(
            "/api/v1/internal/knowledge/datasets/authorize",
            post(datasets),
        )
        .route(
            "/api/v1/internal/knowledge/documents/authorize",
            post(documents),
        )
        .route(
            "/api/v1/internal/knowledge/document-sources/authorize",
            post(sources),
        )
        .with_state(store)
}
pub fn router(store: QueryStore, verifier: Arc<GatewayAuth>) -> Result<Router> {
    verifier.validate_service_boundary()?;
    Ok(routes(store).layer(axum::middleware::from_fn_with_state(
        verifier,
        super::auth::verify_gateway,
    )))
}
#[derive(Deserialize)]
struct DatasetRequest {
    #[serde(default)]
    dataset_ids: Vec<String>,
}
#[derive(Deserialize)]
struct DocumentRequest {
    dataset_id: String,
    #[serde(default)]
    document_ids: Vec<String>,
}
#[derive(Deserialize)]
struct SourceRequest {
    dataset_id: String,
    #[serde(default)]
    references: Vec<SourceRef>,
}
#[derive(Deserialize)]
struct SourceRef {
    document_id: String,
    source_version: i64,
    source_hash: String,
}
async fn datasets(
    State(store): State<QueryStore>,
    Extension(_): Extension<VerifiedGateway>,
    Extension(user): Extension<UserContext>,
    headers: HeaderMap,
    Json(body): Json<DatasetRequest>,
) -> Result<Json<Value>> {
    if headers
        .get("x-user-type")
        .and_then(|v| v.to_str().ok())
        .is_none_or(|s| s.trim().is_empty())
    {
        return Err(QueryError::Forbidden);
    }
    if body.dataset_ids.len() > 200 {
        return Err(QueryError::Validation(
            "at most 200 dataset IDs are allowed".into(),
        ));
    }
    let mut allowed = Vec::new();
    let mut seen = BTreeSet::new();
    for id in body.dataset_ids {
        let id = id.trim();
        if id.is_empty() || !seen.insert(id.to_owned()) {
            continue;
        }
        match store.require_dataset_access(&user, id, "viewer").await {
            Ok(_) => allowed.push(id.to_owned()),
            Err(QueryError::Forbidden | QueryError::NotFound) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(Json(json!({"allowed_dataset_ids":allowed})))
}
async fn documents(
    State(store): State<QueryStore>,
    Extension(_): Extension<VerifiedGateway>,
    Extension(user): Extension<UserContext>,
    Json(body): Json<DocumentRequest>,
) -> Result<Json<Value>> {
    if body.document_ids.len() > 500 {
        return Err(QueryError::Validation(
            "at most 500 document IDs are allowed".into(),
        ));
    }
    if !access(&store, &user, &body.dataset_id).await? {
        return Ok(Json(json!({"allowed_document_ids":[]})));
    }
    let ids: Vec<_> = body
        .document_ids
        .into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let allowed = active_documents(&store, &body.dataset_id, &user.tenant_id, &ids).await?;
    Ok(Json(json!({"allowed_document_ids":allowed})))
}
async fn access(store: &QueryStore, user: &UserContext, d: &str) -> Result<bool> {
    match store.require_dataset_access(user, d, "viewer").await {
        Ok(_) => Ok(true),
        Err(QueryError::Forbidden | QueryError::NotFound) => Ok(false),
        Err(e) => Err(e),
    }
}
pub async fn active_documents(
    store: &QueryStore,
    d: &str,
    tenant: &str,
    ids: &[String],
) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT d.document_id FROM documents d JOIN datasets ds ON ds.dataset_id=d.dataset_id WHERE d.dataset_id=$1 AND ds.tenant_id=$2 AND ds.is_deleted=FALSE AND d.document_id=ANY($3::text[]) AND COALESCE(d.enabled,TRUE)=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND NOT(COALESCE(d.metadata,'{}'::jsonb) ? '_document_lifecycle_reindex') ORDER BY d.document_id").bind(d).bind(tenant).bind(ids).fetch_all(crate::database::executor(&store.pool)).await?)
}
async fn sources(
    State(store): State<QueryStore>,
    Extension(_): Extension<VerifiedGateway>,
    Extension(user): Extension<UserContext>,
    Json(body): Json<SourceRequest>,
) -> Result<Json<Value>> {
    if body.references.len() > 500
        || body.references.iter().any(|r| {
            r.source_version <= 0
                || r.source_version > i32::MAX as i64
                || r.source_hash.len() != 64
                || !r.source_hash.bytes().all(|b| b.is_ascii_hexdigit())
        })
    {
        return Err(QueryError::Validation(
            "invalid document source references".into(),
        ));
    }
    if !access(&store, &user, &body.dataset_id).await? {
        return Ok(Json(json!({"allowed_references":[]})));
    }
    let refs: BTreeSet<_> = body
        .references
        .into_iter()
        .map(|r| {
            (
                r.document_id.trim().to_owned(),
                r.source_version as i32,
                r.source_hash.to_lowercase(),
            )
        })
        .filter(|r| !r.0.is_empty())
        .collect();
    let docs: Vec<_> = refs.iter().map(|r| r.0.clone()).collect();
    let versions: Vec<_> = refs.iter().map(|r| r.1).collect();
    let hashes: Vec<_> = refs.iter().map(|r| r.2.clone()).collect();
    let allowed:Vec<(String,i32,String)>=sqlx::query_as("WITH requested(document_id,version_number,source_hash) AS(SELECT * FROM unnest($3::text[],$4::integer[],$5::text[])) SELECT DISTINCT requested.document_id,requested.version_number,requested.source_hash FROM requested JOIN documents d ON d.document_id=requested.document_id JOIN datasets ds ON ds.dataset_id=d.dataset_id JOIN document_versions v ON v.document_id=d.document_id AND v.version_number=requested.version_number WHERE d.dataset_id=$1 AND ds.tenant_id=$2 AND ds.is_deleted=FALSE AND COALESCE(d.enabled,TRUE)=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND NOT(COALESCE(d.metadata,'{}'::jsonb) ? '_document_lifecycle_reindex') AND v.change_type NOT IN ('pending_restore','pending_before_restore') AND lower(v.content_hash)=requested.source_hash AND encode(sha256(convert_to(v.content,'UTF8')),'hex')=requested.source_hash ORDER BY requested.document_id,requested.version_number,requested.source_hash").bind(&body.dataset_id).bind(&user.tenant_id).bind(docs).bind(versions).bind(hashes).fetch_all(crate::database::executor(&store.pool)).await?;
    Ok(Json(
        json!({"allowed_references":allowed.into_iter().map(|(d,v,h)|json!({"document_id":d,"source_version":v,"source_hash":h})).collect::<Vec<_>>()}),
    ))
}
