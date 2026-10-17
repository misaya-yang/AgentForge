//! Existing progress, execution, version-source and gated image endpoints.
use super::request_validation::{self as validation, TypedQuery};
use super::{
    Actor, IngestError,
    http::{WriteApiState, read_error},
    object, string,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
pub(super) fn routes(state: WriteApiState) -> Router {
    Router::new()
        .route("/api/v1/knowledge/{dataset_id}/documents/images",post(upload_images))
        .route("/api/v1/knowledge/images/{segment_id}",get(image_segment))
        .route("/api/v1/knowledge/{dataset_id}/documents/progress",get(progress))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/executions/{execution_id}",get(execution))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/versions/{version_number}/source",get(historical_source))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/versions/compare",get(compare))
        .with_state(state)
}
async fn upload_images(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
) -> Response {
    match s
        .service
        .store
        .require_dataset_access(&actor, &dataset, "editor")
        .await
    {
        Ok(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail":"Multimodal image ingestion is not enabled for this release."})),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}
const ACTIVE_SEGMENT: &str = " FROM knowledge.segments s JOIN knowledge.documents d ON d.document_id=s.document_id AND d.dataset_id=s.dataset_id JOIN knowledge.datasets ds ON ds.dataset_id=s.dataset_id WHERE s.segment_id=$1 AND ds.tenant_id=$2 AND ds.is_deleted=FALSE AND COALESCE(s.enabled,TRUE)=TRUE AND s.status='completed' AND COALESCE(d.enabled,TRUE)=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND NOT(COALESCE(d.metadata,'{}'::jsonb) ? '_document_lifecycle_reindex')";
async fn image_segment(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(segment): Path<String>,
) -> Response {
    let result = async {
        if segment.trim().is_empty() || actor.tenant_id.trim().is_empty() {
            return Err(IngestError::Validation("Image not found".into()));
        }
        let query = format!("SELECT to_jsonb(s){ACTIVE_SEGMENT}");
        let row: Value = sqlx::query_scalar(&query)
            .bind(segment.trim())
            .bind(actor.tenant_id.trim())
            .fetch_optional(&s.service.store.pool)
            .await?
            .ok_or_else(|| IngestError::Validation("Image not found".into()))?;
        let id = string(&row, "dataset_id");
        if id.is_empty() {
            return Err(IngestError::Validation("Image not found".into()));
        }
        let dataset = s
            .service
            .store
            .require_dataset_access(&actor, id, "viewer")
            .await?;
        let query = format!("SELECT to_jsonb(s){ACTIVE_SEGMENT} AND s.dataset_id=$3");
        let current: Value = sqlx::query_scalar(&query)
            .bind(segment.trim())
            .bind(string(&dataset, "tenant_id").trim())
            .bind(id)
            .fetch_optional(&s.service.store.pool)
            .await?
            .ok_or_else(|| IngestError::Validation("Image not found".into()))?;
        if current["content_type"] != "image" {
            return Err(IngestError::Validation("Image not found".into()));
        }
        Ok::<_, IngestError>(())
    }
    .await;
    match result {
        Ok(()) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail":"Multimodal image serving is not enabled for this release."})),
        )
            .into_response(),
        Err(error) => read_error(error, StatusCode::NOT_FOUND, false),
    }
}
async fn execution(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path((dataset, document, execution)): Path<(String, String, String)>,
) -> Response {
    let result = async {
        s.service
            .store
            .require_dataset_access(&actor, &dataset, "viewer")
            .await?;
        let row = s
            .service
            .store
            .get_pipeline_execution(&execution)
            .await?
            .ok_or_else(|| IngestError::Validation("pipeline execution not found".into()))?;
        if row["dataset_id"] != dataset || row["document_id"] != document {
            return Err(IngestError::Validation(
                "pipeline execution not found".into(),
            ));
        }
        let mut row = row;
        super::diagnostics::sanitize_processing_error(&mut row);
        Ok::<_, IngestError>(row)
    }
    .await;
    match result {
        Ok(row) => Json(row).into_response(),
        Err(error) => read_error(error, StatusCode::NOT_FOUND, false),
    }
}
async fn progress(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
    headers: HeaderMap,
) -> Response {
    let raw = match headers.get("last-event-id").map(|v| v.to_str()).transpose() {
        Ok(value) => value,
        Err(_) => {
            return read_error(
                IngestError::Validation("Invalid document progress event cursor".into()),
                StatusCode::BAD_REQUEST,
                false,
            );
        }
    };
    if let Err(error) = super::progress::parse_cursor(&dataset, raw) {
        return read_error(error, StatusCode::BAD_REQUEST, false);
    }
    if let Err(error) = s
        .service
        .store
        .require_dataset_access(&actor, &dataset, "viewer")
        .await
    {
        return read_error(error, StatusCode::NOT_FOUND, false);
    }
    match super::progress::response(s.service.store.clone(), dataset, raw).await {
        Ok(response) => response,
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail":"Document progress stream is unavailable"})),
        )
            .into_response(),
    }
}
#[derive(Deserialize)]
struct SourceQuery {
    #[serde(rename = "hash")]
    hash: String,
}
fn source_unavailable() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"detail":{"code":"SOURCE_VERSION_UNAVAILABLE"}})),
    )
        .into_response()
}
async fn historical_source(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    validation::VersionPath((dataset, document, version)): validation::VersionPath<true>,
    TypedQuery(query): TypedQuery<SourceQuery, { validation::SOURCE_QUERY }>,
) -> Response {
    match s
        .service
        .historical_document_source(&actor, &dataset, &document, version, &query.hash)
        .await
    {
        Ok(row) => Json(row).into_response(),
        Err(IngestError::Permission | IngestError::NotFound | IngestError::Validation(_)) => {
            source_unavailable()
        }
        Err(error) => error.into_response(),
    }
}
#[derive(Deserialize)]
struct CompareQuery {
    from_version: i32,
    to_version: i32,
}
async fn compare(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path((dataset, document)): Path<(String, String)>,
    TypedQuery(query): TypedQuery<CompareQuery, { validation::COMPARE_QUERY }>,
) -> Response {
    match s
        .service
        .compare_document_versions(
            &actor,
            &dataset,
            &document,
            query.from_version,
            query.to_version,
        )
        .await
    {
        Ok(row) => Json(row).into_response(),
        Err(error) => read_error(error, StatusCode::NOT_FOUND, false),
    }
}
