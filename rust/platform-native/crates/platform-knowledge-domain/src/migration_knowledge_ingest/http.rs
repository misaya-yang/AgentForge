//! Knowledge write routes. The host must install verified UserContext as an
//! extension after its service-signature/JWT middleware; no header auth is read.
use super::request_validation::{self as validation, TypedJson, TypedQuery};
use super::{
    Actor, IngestError, IngestionService, batches::DocumentBatchStore, object,
    require_index_writable, string,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Clone)]
pub struct WriteApiState {
    pub service: Arc<IngestionService>,
    pub batches: DocumentBatchStore,
    pub maximum_upload_bytes: usize,
}
fn write_routes(state: WriteApiState) -> Router {
    Router::new()
        .route("/api/v1/knowledge/datasets",post(create_dataset))
        .route("/api/v1/knowledge/datasets/{dataset_id}",axum::routing::put(update_dataset).delete(delete_dataset_confirmed))
        .route("/api/v1/knowledge/datasets/{dataset_id}/archive",axum::routing::patch(archive_dataset))
        .route("/api/v1/knowledge/datasets/{dataset_id}/permissions",get(list_permissions).post(grant_permission).delete(revoke_permission))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/status",axum::routing::patch(document_enabled))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/enable",post(document_enabled))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/archive",axum::routing::patch(document_archived))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/segments",post(create_segment))
        .route("/api/v1/knowledge/{dataset_id}/segments/{segment_id}",axum::routing::put(update_segment).delete(delete_segment))
        .route("/api/v1/knowledge/{dataset_id}/segments/{segment_id}/status",axum::routing::patch(segment_enabled))
        .route("/api/v1/knowledge/{dataset_id}/segments/{segment_id}/enable",post(segment_enabled))
        .route("/api/v1/knowledge/{dataset_id}/segments/batch/enable",post(segments_enabled))
        .route("/api/v1/knowledge/{dataset_id}/documents",get(list_documents))
        .route("/api/v1/knowledge/{dataset_id}/segments",get(list_segments))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/source",get(active_document_source))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/statistics",get(document_statistics))
        .route("/api/v1/knowledge/{dataset_id}/documents/text",post(create_text))
        .route("/api/v1/knowledge/{dataset_id}/documents/upload",post(upload))
        .route("/api/v1/knowledge/{dataset_id}/documents/url",post(create_url))
        .route("/api/v1/knowledge/{dataset_id}/documents/batch",post(create_batch))
        .route("/api/v1/knowledge/preview",post(preview_generic))
        .route("/api/v1/knowledge/{dataset_id}/chunk/preview",post(preview_chunks))
        .route("/api/v1/knowledge/{dataset_id}/preview-chunks",post(preview_configured))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/reindex",post(reindex))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/reprocess",post(reprocess))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/retry",post(retry))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/recover",post(recover))
        .route("/api/v1/knowledge/{dataset_id}/documents/batch-reindex",post(batch_reindex))
        .route("/api/v1/knowledge/{dataset_id}/documents/batch-delete",post(batch_delete))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}",axum::routing::delete(delete_document).patch(update_document).get(get_document))
        .route("/api/v1/knowledge/{dataset_id}/document-batches/{operation_id}",get(get_batch))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/versions",get(versions))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/versions/{version_number}/restore",post(restore_version))
        .route("/api/v1/knowledge/{dataset_id}/documents/{document_id}/versions/{version_number}",get(version))
        .layer(DefaultBodyLimit::max(state.maximum_upload_bytes.clamp(1024*1024,48*1024*1024)+1024*1024))
        .with_state(state.clone())
        .merge(super::inspection_http::routes(state.clone()))
        .merge(super::catalog_http::routes(state.clone()))
        .merge(super::batch_upload::routes(state))
}
async fn create_dataset(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    TypedJson(value): TypedJson<Value, { validation::DATASET_CREATE }>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(s.service.create_dataset(&actor, &value).await?))
}
#[derive(Deserialize)]
struct TextInput {
    #[serde(default)]
    title: String,
    content: String,
    #[serde(default = "empty_object")]
    metadata: Value,
}
fn empty_object() -> Value {
    json!({})
}
async fn create_text(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
    TypedJson(input): TypedJson<TextInput, { validation::TEXT_CREATE }>,
) -> Result<Json<Value>, IngestError> {
    let doc = s
        .service
        .create_document_from_text(
            &actor,
            &dataset,
            &input.title,
            &input.content,
            input.metadata,
        )
        .await?;
    if !s
        .service
        .store
        .enqueue(
            &dataset,
            string(&doc, "document_id"),
            None,
            None,
            None,
            false,
        )
        .await?
    {
        return Err(IngestError::GenerationChanged);
    }
    Ok(Json(doc))
}
async fn upload(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
    mut multipart: Multipart,
) -> Result<Json<Value>, IngestError> {
    if actor.anonymous() {
        return Err(IngestError::Permission);
    }
    let d = s
        .service
        .store
        .require_dataset_access(&actor, &dataset, "editor")
        .await?;
    require_index_writable(&d)?;
    let limit = s.maximum_upload_bytes.clamp(1024 * 1024, 48 * 1024 * 1024);
    let mut bytes = None;
    let mut filename = String::new();
    let mut mime = String::new();
    let mut mode = "auto".to_owned();
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| IngestError::Validation("invalid multipart upload".into()))?
    {
        match field.name().unwrap_or("") {
            "processing_mode" => {
                let value = field
                    .text()
                    .await
                    .map_err(|_| IngestError::Validation("invalid processing_mode".into()))?;
                if value.len() > 64 {
                    return Err(IngestError::Validation("invalid processing_mode".into()));
                }
                // Python applies the fallback before stripping whitespace.
                mode = if value.is_empty() {
                    "auto".into()
                } else {
                    value.trim().to_lowercase()
                };
            }
            "file" => {
                if bytes.is_some() {
                    return Err(IngestError::Validation("only one file is accepted".into()));
                }
                filename = field.file_name().filter(|name| !name.is_empty()).unwrap_or("upload").to_owned();
                mime = field
                    .content_type()
                    .unwrap_or("application/octet-stream")
                    .to_owned();
                let mut body = vec![];
                while let Some(chunk) = field
                    .chunk()
                    .await
                    .map_err(|_| IngestError::Validation("upload stream failed".into()))?
                {
                    if body.len() + chunk.len() > limit {
                        return Err(IngestError::Validation(
                            "file exceeds the configured upload size limit".into(),
                        ));
                    }
                    body.extend_from_slice(&chunk)
                }
                bytes = Some(body);
            }
            _ => {}
        }
    }
    if mode == "auto" {
        mode = "text_only".into();
    }
    if mode != "text_only" {
        return Err(IngestError::Validation(
            "scanned and multimodal uploads are disabled until the unified multimodal index profile is released".into(),
        ));
    }
    let bytes = bytes.ok_or_else(|| IngestError::Validation("file is required".into()))?;
    let ext = std::path::Path::new(&filename)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !["pdf", "docx", "txt", "md", "html"].contains(&ext.as_str()) {
        return Err(IngestError::Validation(
            "unsupported document file type".into(),
        ));
    }
    let parser_limit = if ext == "html" {
        super::parser::MAX_HTML_BYTES
    } else {
        super::parser::MAX_SOURCE_BYTES
    };
    if bytes.len() > parser_limit {
        return Err(IngestError::Validation(format!(
            "{filename} exceeds the {}MB parser limit for .{ext} files",
            parser_limit / (1024 * 1024)
        )));
    }
    let doc = s
        .service
        .create_document_from_upload(&actor, &dataset, &filename, &bytes, &mime, json!({}), &mode)
        .await?;
    if !s
        .service
        .store
        .enqueue(
            &dataset,
            string(&doc, "document_id"),
            None,
            None,
            None,
            false,
        )
        .await?
    {
        return Err(IngestError::GenerationChanged);
    }
    Ok(Json(doc))
}
async fn reindex(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((d, id)): Path<(String, String)>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(s.service.submit(&a, &d, &id, "reembed").await?))
}
async fn reprocess(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((d, id)): Path<(String, String)>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(s.service.submit(&a, &d, &id, "reprocess").await?))
}
async fn retry(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((d, id)): Path<(String, String)>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(s.service.submit(&a, &d, &id, "retry").await?))
}
async fn recover(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((d, id)): Path<(String, String)>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(s.service.submit(&a, &d, &id, "recover").await?))
}
#[derive(Deserialize)]
struct BatchInput {
    #[serde(default)]
    document_ids: Vec<String>,
    #[serde(default)]
    all_documents: bool,
}
async fn batch_reindex(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path(d): Path<String>,
    TypedJson(input): TypedJson<BatchInput, { validation::BATCH_REINDEX }>,
) -> Result<(StatusCode, Json<Value>), IngestError> {
    let ds = s
        .service
        .store
        .require_dataset_access(&a, &d, "editor")
        .await?;
    require_index_writable(&ds)?;
    let value = s
        .batches
        .create_operation(
            string(&ds, "tenant_id"),
            &d,
            "reembed",
            &a.user_id,
            &a.roles,
            &input.document_ids,
            input.all_documents,
            None,
        )
        .await?;
    Ok((StatusCode::ACCEPTED, Json(value)))
}
async fn get_batch(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((d, id)): Path<(String, String)>,
) -> Result<Json<Value>, IngestError> {
    let ds = s
        .service
        .store
        .require_dataset_access(&a, &d, "viewer")
        .await?;
    let value = s
        .batches
        .get_operation(&id, string(&ds, "tenant_id"), &d)
        .await?
        .ok_or(IngestError::NotFound)?;
    Ok(Json(value))
}
#[derive(Deserialize)]
struct Page {
    #[serde(default = "twenty")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}
fn twenty() -> i64 {
    20
}
async fn versions(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((d, id)): Path<(String, String)>,
    TypedQuery(p): TypedQuery<Page, { validation::VERSIONS_QUERY }>,
) -> Response {
    if let Some(error) = page_input_error(p.limit, p.offset, 100) {
        return error;
    }
    match s
        .service
        .list_document_versions(&a, &d, &id, p.limit, p.offset)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => read_error(error, StatusCode::NOT_FOUND, false),
    }
}
async fn version(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    validation::VersionPath((d, id, n)): validation::VersionPath<true>,
) -> Response {
    match s.service.get_document_version(&a, &d, &id, n).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => read_error(error, StatusCode::NOT_FOUND, false),
    }
}

async fn delete_document(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((d, id)): Path<(String, String)>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(
        json!({"success":s.service.delete_document(&a,&d,&id).await?}),
    ))
}
async fn batch_delete(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path(d): Path<String>,
    TypedJson(input): TypedJson<BatchInput, { validation::BATCH_DELETE }>,
) -> Result<(StatusCode, Json<Value>), IngestError> {
    let ds = s
        .service
        .store
        .require_dataset_access(&a, &d, "editor")
        .await?;
    let document_ids = super::batches::deletion_selection(&ds, &input.document_ids)?;
    let value = s
        .batches
        .create_operation(
            string(&ds, "tenant_id"),
            &d,
            "delete",
            &a.user_id,
            &a.roles,
            &document_ids,
            false,
            None,
        )
        .await?;
    Ok((StatusCode::ACCEPTED, Json(value)))
}

pub fn routes(state: WriteApiState, auth: Arc<dyn ai_platform_service_primitives::actor_verifier::JwtActorVerifier>) -> Router {
    write_routes(state).route_layer(axum::middleware::from_fn_with_state(
        auth,
        crate::migration_knowledge_query::auth::verify_jwt,
    ))
}
pub fn service_routes(
    state: WriteApiState,
    auth: Arc<crate::migration_knowledge_query::auth::GatewayAuth>,
) -> Router {
    write_routes(state).route_layer(axum::middleware::from_fn_with_state(
        auth,
        crate::migration_knowledge_query::auth::verify_gateway,
    ))
}
async fn restore_version(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    validation::VersionPath((d, id, n)): validation::VersionPath<false>,
    validation::OptionalTypedJson(value): validation::OptionalTypedJson<
        Value,
        { validation::RESTORE },
    >,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(
        s.service
            .restore_version(&a, &d, &id, n, value["reason"].as_str())
            .await?,
    ))
}

async fn update_dataset(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path(id): Path<String>,
    TypedJson(v): TypedJson<Value, { validation::DATASET_UPDATE }>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(s.service.update_dataset(&a, &id, &v).await?))
}
#[derive(Deserialize)]
struct ArchiveInput {
    archived: bool,
    reason: Option<String>,
}
async fn archive_dataset(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path(id): Path<String>,
    TypedJson(v): TypedJson<ArchiveInput, { validation::DATASET_ARCHIVE }>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(
        s.service
            .set_dataset_archived(&a, &id, v.archived, v.reason.as_deref())
            .await?,
    ))
}
#[derive(Deserialize)]
struct PermissionInput {
    subject_type: String,
    subject_id: String,
    permission: String,
}
async fn list_permissions(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Value>>, IngestError> {
    Ok(Json(s.service.list_dataset_permissions(&a, &id).await?))
}
async fn grant_permission(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path(id): Path<String>,
    TypedJson(v): TypedJson<PermissionInput, { validation::PERMISSION_GRANT }>,
) -> Result<Json<Value>, IngestError> {
    s.service
        .grant_dataset_permission(&a, &id, &v.subject_type, &v.subject_id, &v.permission)
        .await?;
    Ok(Json(json!({"status":"success"})))
}
#[derive(Deserialize)]
struct PermissionSubject {
    subject_type: String,
    subject_id: String,
}
async fn revoke_permission(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path(id): Path<String>,
    TypedQuery(v): TypedQuery<PermissionSubject, { validation::PERMISSION_QUERY }>,
) -> Result<Json<Value>, IngestError> {
    let ok = s
        .service
        .revoke_dataset_permission(&a, &id, &v.subject_type, &v.subject_id)
        .await?;
    Ok(Json(json!({"status":if ok{"success"}else{"not_found"}})))
}
async fn update_document(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((ds, id)): Path<(String, String)>,
    super::request_validation::SourceJson(v): super::request_validation::SourceJson,
) -> Response {
    let v = match super::request_validation::document_update(v) {
        Ok(value) => value,
        Err(error) => return error,
    };
    let uses_patch = v.get("metadata_patch").is_some_and(|v| !v.is_null())
        || v.get("metadata_remove").is_some_and(|v| !v.is_null());
    let result = if uses_patch {
        let patch = v
            .get("metadata_patch")
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(json!({}));
        let remove: std::result::Result<Vec<String>, _> = serde_json::from_value(
            v.get("metadata_remove")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or(json!([])),
        );
        match remove {
            Ok(remove) => {
                s.service
                    .patch_document_metadata(
                        &a,
                        &ds,
                        &id,
                        v["metadata_schema_revision"].as_i64().unwrap_or(0),
                        &patch,
                        &remove,
                    )
                    .await
            }
            Err(_) => Err(IngestError::Validation(
                "metadata_remove must be a list of field names".into(),
            )),
        }
    } else {
        let mut patch = v;
        patch
            .as_object_mut()
            .unwrap()
            .retain(|_, value| !value.is_null());
        s.service.update_document(&a, &ds, &id, &patch).await
    };
    match result {
        Ok(value) => Json(value).into_response(),
        Err(IngestError::NotFound) if uses_patch => (
            StatusCode::NOT_FOUND,
            Json(json!({"detail":"document not found"})),
        )
            .into_response(),
        Err(error) => super::catalog_http::metadata_error(error, StatusCode::BAD_REQUEST),
    }
}

#[derive(Deserialize)]
struct EnabledInput {
    enabled: bool,
}
async fn document_enabled(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((ds, id)): Path<(String, String)>,
    TypedJson(v): TypedJson<EnabledInput, { validation::ENABLED }>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(
        s.service
            .set_document_enabled(&a, &ds, &id, v.enabled)
            .await?,
    ))
}
async fn document_archived(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((ds, id)): Path<(String, String)>,
    TypedJson(v): TypedJson<ArchiveInput, { validation::DOCUMENT_ARCHIVE }>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(
        s.service
            .set_document_archived(&a, &ds, &id, v.archived, v.reason.as_deref())
            .await?,
    ))
}
#[derive(Deserialize)]
struct SegmentUpdate {
    text: String,
    answer: Option<String>,
    keywords: Option<Vec<String>>,
}
async fn update_segment(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((ds, id)): Path<(String, String)>,
    TypedJson(v): TypedJson<SegmentUpdate, { validation::SEGMENT_UPDATE }>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(
        s.service
            .update_segment(
                &a,
                &ds,
                &id,
                &v.text,
                v.answer.as_deref(),
                v.keywords.as_deref(),
            )
            .await?,
    ))
}
#[derive(Deserialize)]
struct SegmentCreate {
    content: String,
    answer: Option<String>,
    keywords: Option<Vec<String>>,
}
async fn create_segment(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((ds, id)): Path<(String, String)>,
    TypedJson(v): TypedJson<SegmentCreate, { validation::SEGMENT_CREATE }>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(
        s.service
            .create_segment(
                &a,
                &ds,
                &id,
                &v.content,
                v.answer.as_deref(),
                v.keywords.as_deref(),
            )
            .await?,
    ))
}
async fn delete_segment(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((ds, id)): Path<(String, String)>,
) -> Result<Json<Value>, IngestError> {
    let ok = s.service.delete_segment(&a, &ds, &id).await?;
    Ok(Json(json!({"status":if ok{"success"}else{"not_found"}})))
}
async fn segment_enabled(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path((ds, id)): Path<(String, String)>,
    TypedJson(v): TypedJson<EnabledInput, { validation::ENABLED }>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(
        s.service
            .set_segment_enabled(&a, &ds, &id, v.enabled)
            .await?,
    ))
}
#[derive(Deserialize)]
struct SegmentsEnabled {
    segment_ids: Vec<String>,
    enabled: bool,
}
async fn segments_enabled(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path(ds): Path<String>,
    TypedJson(v): TypedJson<SegmentsEnabled, { validation::SEGMENTS_ENABLED }>,
) -> Result<Json<Value>, IngestError> {
    Ok(Json(
        s.service
            .set_segments_enabled_batch(&a, &ds, &v.segment_ids, v.enabled)
            .await?,
    ))
}

#[derive(Deserialize)]
struct UrlInput {
    url: String,
    title: Option<String>,
    #[serde(default = "empty_object")]
    metadata: Value,
}
async fn create_url(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path(id): Path<String>,
    TypedJson(v): TypedJson<UrlInput, { validation::URL_CREATE }>,
) -> Result<Json<Value>, IngestError> {
    let doc = s
        .service
        .create_document_from_url(&a, &id, &v.url, v.title.as_deref(), v.metadata)
        .await?;
    if !s
        .service
        .store
        .enqueue(&id, string(&doc, "document_id"), None, None, None, false)
        .await?
    {
        return Err(IngestError::GenerationChanged);
    }
    Ok(Json(doc))
}
#[derive(Deserialize)]
struct BatchCreate {
    documents: Vec<Value>,
    process_rule: Option<Value>,
    batch_name: Option<String>,
}
async fn create_batch(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path(id): Path<String>,
    TypedJson(v): TypedJson<BatchCreate, { validation::DOCUMENT_BATCH }>,
) -> Result<Json<Value>, IngestError> {
    let mut result = s
        .service
        .batch_create_documents(
            &a,
            &id,
            &v.documents,
            v.process_rule.as_ref(),
            v.batch_name.as_deref(),
        )
        .await?;
    let mut queued = vec![];
    let mut skipped = vec![];
    for doc in result["documents"].as_array().cloned().unwrap_or_default() {
        if s.service
            .store
            .enqueue(&id, string(&doc, "document_id"), None, None, None, false)
            .await?
        {
            queued.push(doc)
        } else {
            skipped.push(doc["document_id"].clone())
        }
    }
    result["queued_count"] = json!(queued.len());
    result["documents"] = json!(queued);
    result["status"] = json!(if skipped.is_empty() {
        "queuing"
    } else {
        "partial"
    });
    result["skipped_document_ids"] = json!(skipped);
    Ok(Json(result))
}
#[derive(Deserialize)]
struct PreviewInput {
    text: String,
    config: Option<Value>,
    chunking_config: Option<Value>,
}
fn safe_preview(text: &str, config: Option<&Value>) -> Result<(), IngestError> {
    if text.is_empty() || text.chars().count() > 200000 {
        return Err(IngestError::Validation(
            "preview text must contain 1-200000 characters".into(),
        ));
    }
    if let Some(config) = config {
        if string(config, "mode").eq_ignore_ascii_case("regex")
            || ["regex_pattern", "regex", "page_marker"]
                .iter()
                .any(|k| config[*k].as_str().is_some_and(|v| !v.is_empty()))
            || config["heading_patterns"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
        {
            return Err(IngestError::Validation(
                "custom regex chunk preview is disabled".into(),
            ));
        }
    }
    Ok(())
}
async fn preview_generic(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    TypedJson(v): TypedJson<PreviewInput, { validation::PREVIEW }>,
) -> Result<Json<Value>, IngestError> {
    if a.anonymous() {
        return Err(IngestError::Permission);
    }
    safe_preview(&v.text, v.config.as_ref())?;
    let rows = s
        .service
        .preview_chunking(&a, "temp_preview", &v.text, v.config.as_ref())
        .await?;
    Ok(Json(json!({"total_chunks":rows.len(),"chunks":rows})))
}
async fn preview_chunks(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path(id): Path<String>,
    TypedJson(v): TypedJson<PreviewInput, { validation::PREVIEW }>,
) -> Result<Json<Value>, IngestError> {
    if a.anonymous() {
        return Err(IngestError::Permission);
    }
    s.service
        .store
        .require_dataset_access(&a, &id, "editor")
        .await?;
    safe_preview(&v.text, v.config.as_ref())?;
    let rows = s
        .service
        .preview_chunking(&a, &id, &v.text, v.config.as_ref())
        .await?;
    Ok(Json(json!({"total_chunks":rows.len(),"chunks":rows})))
}
async fn preview_configured(
    State(s): State<WriteApiState>,
    Extension(a): Extension<Actor>,
    Path(id): Path<String>,
    TypedJson(v): TypedJson<PreviewInput, { validation::PREVIEW_CONFIGURED }>,
) -> Result<Json<Value>, IngestError> {
    if a.anonymous() {
        return Err(IngestError::Permission);
    }
    let dataset = s
        .service
        .store
        .require_dataset_access(&a, &id, "editor")
        .await?;
    safe_preview(&v.text, v.chunking_config.as_ref())?;
    let selected = v
        .chunking_config
        .filter(|c| c.as_object().is_some_and(|m| !m.is_empty()))
        .unwrap_or(dataset["index_config"]["chunking"].clone());
    let config = super::chunking::ChunkingConfig::from_value(&selected)?;
    let engine = s.service.chunker.clone();
    let rows=tokio::task::spawn_blocking(move||{let rows=engine.process_document(&v.text,&config,&id,"","")?;let parents:std::collections::BTreeSet<_>=rows.iter().filter_map(|r|r.parent_segment_id.clone()).collect();let rows=rows.into_iter().filter(|r|!parents.contains(&r.segment_id)).collect();let rows=engine.merge_small_with_tokens(rows,config.min_chunk_size,config.max_chunk_size,if config.use_token_count{config.min_chunk_tokens}else{None},if config.use_token_count{config.max_chunk_tokens}else{None})?;let items:Vec<_>=rows.into_iter().enumerate().map(|(i,r)|json!({"index":i,"text":r.text,"char_count":r.text.chars().count(),"token_count":r.token_count,"word_count":r.word_count})).collect();Ok::<_,IngestError>(json!({"total_chunks":items.len(),"chunks":items,"config_used":config.to_value()}))}).await.map_err(|_|IngestError::Parser("chunk preview task failed".into()))??;
    Ok(Json(rows))
}

fn page_input_error(limit: i64, offset: i64, cap: i64) -> Option<Response> {
    let mut errors = vec![];
    for (key, value, min, max) in [("limit", limit, 1, Some(cap)), ("offset", offset, 0, None)] {
        if value < min {
            errors.push(super::request_validation::error(
                "greater_than_equal",
                json!(["query", key]),
                format!("Input should be greater than or equal to {min}"),
                json!(value),
                Some(json!({"ge": min})),
            ));
        } else if let Some(max) = max.filter(|max| value > *max) {
            errors.push(super::request_validation::error(
                "less_than_equal",
                json!(["query", key]),
                format!("Input should be less than or equal to {max}"),
                json!(value),
                Some(json!({"le": max})),
            ));
        }
    }
    (!errors.is_empty()).then(|| super::request_validation::response(errors))
}
fn document_page_limit() -> i64 {
    super::document_reads::DOCUMENT_LIST_PAGE_CAP
}
fn segment_page_limit() -> i64 {
    super::document_reads::SEGMENT_LIST_PAGE_CAP
}
pub(super) fn read_error(
    error: IngestError,
    validation_status: StatusCode,
    retry_changed: bool,
) -> Response {
    if retry_changed
        && matches!(&error,IngestError::Validation(message) if message == super::document_reads::CONTENT_CHANGED)
    {
        let mut response = (
            StatusCode::CONFLICT,
            Json(json!({"detail":super::document_reads::CONTENT_CHANGED})),
        )
            .into_response();
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from_static("1"));
        return response;
    }
    match error {
        IngestError::Validation(message) => {
            (validation_status, Json(json!({"detail":message}))).into_response()
        }
        IngestError::NotFound => (
            validation_status,
            Json(json!({"detail":"document or dataset not found"})),
        )
            .into_response(),
        error => error.into_response(),
    }
}
fn page_response(page: Value) -> Response {
    let mut response = Json(page["items"].clone()).into_response();
    if let Ok(total) = HeaderValue::from_str(&page["total"].as_i64().unwrap_or(0).to_string()) {
        response.headers_mut().insert("x-total-count", total);
    }
    response
}
async fn list_documents(
    State(state): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> Response {
    let query = match super::request_validation::page(raw.as_deref(), document_page_limit(), false)
    {
        Ok(value) => value,
        Err(error) => return error,
    };
    match state
        .service
        .list_documents_page(
            &actor,
            &dataset,
            query["limit"].as_i64().unwrap(),
            query["offset"].as_i64().unwrap(),
        )
        .await
    {
        Ok(page) => page_response(page),
        Err(error) => read_error(error, StatusCode::BAD_REQUEST, true),
    }
}
async fn list_segments(
    State(state): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
    axum::extract::RawQuery(raw): axum::extract::RawQuery,
) -> Response {
    let query = match super::request_validation::page(raw.as_deref(), segment_page_limit(), false) {
        Ok(value) => value,
        Err(error) => return error,
    };
    match state
        .service
        .list_segments_page(
            &actor,
            &dataset,
            query["document_id"].as_str(),
            query["q"].as_str(),
            query["limit"].as_i64().unwrap(),
            query["offset"].as_i64().unwrap(),
        )
        .await
    {
        Ok(page) => page_response(page),
        Err(error) => read_error(error, StatusCode::BAD_REQUEST, true),
    }
}
async fn get_document(
    State(state): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path((dataset, document)): Path<(String, String)>,
) -> Response {
    match state
        .service
        .get_document(&actor, &dataset, &document)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => read_error(error, StatusCode::NOT_FOUND, false),
    }
}
async fn active_document_source(
    State(state): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path((dataset, document)): Path<(String, String)>,
) -> Response {
    match state
        .service
        .get_active_document_source(&actor, &dataset, &document)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => read_error(error, StatusCode::NOT_FOUND, false),
    }
}
async fn document_statistics(
    State(state): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path((dataset, document)): Path<(String, String)>,
) -> Response {
    match state
        .service
        .get_document_statistics(&actor, &dataset, &document)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => read_error(error, StatusCode::BAD_REQUEST, false),
    }
}
#[derive(Deserialize)]
struct DatasetDeleteInput {
    #[serde(rename = "_gateway_delete_confirmation")]
    confirmation: Option<serde_json::Map<String, Value>>,
    reason: Option<String>,
}
fn confirmation_error(code: &str) -> Response {
    (StatusCode::FORBIDDEN, Json(json!({"detail":{"code":code}}))).into_response()
}
async fn delete_dataset_confirmed(
    State(state): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    proof: Option<Extension<crate::migration_knowledge_query::auth::VerifiedGateway>>,
    Path(dataset): Path<String>,
    headers: HeaderMap,
    TypedJson(input): TypedJson<DatasetDeleteInput, { validation::DATASET_DELETE }>,
) -> Response {
    // The marker has no public constructor. Only the shared v2 verifier can
    // mint it after checking the exact body, actor, route and replay nonce.
    // The public JWT-only router therefore cannot satisfy this assertion.
    if proof.is_none()
        || !headers
            .get("x-gateway-secret")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("v2:"))
        || !actor.is_authenticated
        || actor.tenant_id.is_empty()
        || actor.tenant_id == "public"
    {
        return confirmation_error("KB_DELETE_CONFIRMATION_REQUIRED");
    }
    let claims = input
        .confirmation
        .map(Value::Object)
        .unwrap_or_else(|| json!({}));
    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(now) => now.as_secs() as i64,
        Err(_) => return IngestError::Storage.into_response(),
    };
    if claims["action"] != "dataset.delete"
        || claims["dataset_id"] != dataset
        || claims["user_id"] != actor.user_id
        || claims["tenant_id"] != actor.tenant_id
        || !claims["expires_at"]
            .as_i64()
            .is_some_and(|expires| expires >= now - 60 && expires <= now + 120)
    {
        return confirmation_error("KB_DELETE_CONFIRMATION_INVALID");
    }
    match state
        .service
        .delete_dataset(&actor, &dataset, true, input.reason.as_deref())
        .await
    {
        Ok(deleted) => {
            Json(json!({"status":if deleted{"success"}else{"not_found"},"dataset_id":dataset}))
                .into_response()
        }
        Err(IngestError::LeaseBusy) => {
            let mut response = IngestError::LeaseBusy.into_response();
            response
                .headers_mut()
                .insert("retry-after", HeaderValue::from_static("1"));
            response
        }
        Err(error) => read_error(error, StatusCode::BAD_REQUEST, false),
    }
}
