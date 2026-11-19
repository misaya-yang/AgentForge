//! Canonical dataset catalog/config/metadata/operator HTTP surface.
use super::request_validation::{self as validation, SourceJson};
use super::{
    Actor, IngestError,
    http::{WriteApiState, read_error},
    object, require_index_writable, string,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, RawQuery, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
pub(super) fn routes(state: WriteApiState) -> Router {
    Router::new()
        .route("/api/v1/knowledge/datasets", get(datasets))
        .route("/api/v1/knowledge/datasets/{dataset_id}", get(dataset))
        .route(
            "/api/v1/knowledge/{dataset_id}/metadata-schema",
            get(registry).put(update_registry),
        )
        .route(
            "/api/v1/knowledge/{dataset_id}/documents/metadata/batch",
            axum::routing::patch(patch_metadata),
        )
        .route("/api/v1/knowledge/{dataset_id}/statistics", get(statistics))
        .route("/api/v1/knowledge/{dataset_id}/debug", get(debug))
        .route(
            "/api/v1/knowledge/{dataset_id}/config",
            get(config).put(update_config),
        )
        .route("/api/v1/knowledge/{dataset_id}/sources", get(sources))
        .route(
            "/api/v1/knowledge/{dataset_id}/maintenance/dedupe",
            post(dedupe),
        )
        .route("/api/v1/knowledge/worker/status", get(worker_status))
        .route(
            "/api/v1/knowledge/{dataset_id}/documents/{document_id}/force-complete",
            post(force_complete),
        )
        .with_state(state)
}
fn output(result: super::Result<Value>, status: StatusCode) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => read_error(error, status, false),
    }
}
pub(super) fn metadata_error(error: IngestError, status: StatusCode) -> Response {
    if matches!(&error,IngestError::Validation(message) if message==super::metadata::REVISION_CONFLICT)
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({"detail":super::metadata::REVISION_CONFLICT})),
        )
            .into_response();
    }
    if error == IngestError::LeaseBusy {
        let mut response = error.into_response();
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from_static("1"));
        return response;
    }
    read_error(error, status, false)
}
async fn datasets(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    RawQuery(raw): RawQuery,
) -> Response {
    let query = match validation::page(raw.as_deref(), 200, true) {
        Ok(query) => query,
        Err(error) => return error,
    };
    match s
        .service
        .list_datasets_page(
            &actor,
            query["limit"].as_i64().unwrap(),
            query["cursor"].as_str(),
            query["archived"].as_bool().unwrap(),
        )
        .await
    {
        Ok(page) => {
            let mut response = Json(page["items"].clone()).into_response();
            if let Some(cursor) = page["next_cursor"].as_str() {
                if let Ok(cursor) = HeaderValue::from_str(cursor) {
                    response.headers_mut().insert("x-next-cursor", cursor);
                }
            }
            response
        }
        Err(error) => read_error(error, StatusCode::BAD_REQUEST, false),
    }
}
async fn dataset(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
) -> Response {
    output(
        s.service.get_dataset_response(&actor, &dataset).await,
        StatusCode::NOT_FOUND,
    )
}
async fn registry(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
) -> Response {
    output(
        s.service.get_metadata_registry(&actor, &dataset).await,
        StatusCode::NOT_FOUND,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryInput {
    expected_revision: i64,
    #[serde(default)]
    fields: Vec<Value>,
}
async fn update_registry(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
    SourceJson(input): SourceJson,
) -> Response {
    let input = match validation::registry(input) {
        Ok(value) => serde_json::from_value::<RegistryInput>(value).unwrap(),
        Err(error) => return error,
    };
    match s
        .service
        .update_metadata_registry(&actor, &dataset, input.expected_revision, &input.fields)
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => metadata_error(error, StatusCode::BAD_REQUEST),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataInput {
    document_ids: Vec<String>,
    #[serde(default = "empty")]
    metadata_patch: Value,
    #[serde(default)]
    metadata_remove: Vec<String>,
    metadata_schema_revision: i64,
}
fn empty() -> Value {
    json!({})
}
async fn patch_metadata(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
    SourceJson(input): SourceJson,
) -> Response {
    let input = match validation::metadata_batch(input) {
        Ok(value) => serde_json::from_value::<MetadataInput>(value).unwrap(),
        Err(error) => return error,
    };
    match s
        .service
        .patch_documents_metadata(
            &actor,
            &dataset,
            &input.document_ids,
            input.metadata_schema_revision,
            &input.metadata_patch,
            &input.metadata_remove,
        )
        .await
    {
        Ok(value) => Json(value).into_response(),
        Err(error) => metadata_error(error, StatusCode::BAD_REQUEST),
    }
}
async fn statistics(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
) -> Response {
    output(
        s.service.get_dataset_statistics(&actor, &dataset).await,
        StatusCode::BAD_REQUEST,
    )
}
async fn debug(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
) -> Response {
    output(
        s.service.debug_dataset(&actor, &dataset).await,
        StatusCode::BAD_REQUEST,
    )
}
async fn config(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
) -> Response {
    output(
        s.service.get_dataset_config(&actor, &dataset).await,
        StatusCode::NOT_FOUND,
    )
}
async fn update_config(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
    SourceJson(input): SourceJson,
) -> Response {
    let input = match validation::dataset_config(input) {
        Ok(value) => value,
        Err(error) => return error,
    };
    output(
        s.service
            .update_dataset_config(&actor, &dataset, &input)
            .await,
        StatusCode::BAD_REQUEST,
    )
}
async fn sources(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
) -> Response {
    output(
        s.service.dataset_sources(&actor, &dataset).await,
        StatusCode::BAD_REQUEST,
    )
}
#[derive(Deserialize)]
struct DedupeQuery {
    #[serde(default = "yes")]
    dry_run: bool,
}
fn yes() -> bool {
    true
}
async fn dedupe(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path(dataset): Path<String>,
    validation::TypedQuery(query): validation::TypedQuery<
        DedupeQuery,
        { validation::DEDUPE_QUERY },
    >,
) -> Response {
    match s
        .service
        .dedupe_segments(&actor, &dataset, query.dry_run)
        .await
    {
        Ok(value) if value["incomplete"] == true => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"detail":value})),
        )
            .into_response(),
        result => output(result, StatusCode::BAD_REQUEST),
    }
}
fn admin(actor: &Actor) -> bool {
    actor.is_authenticated
        && !actor.user_id.trim().is_empty()
        && !actor
            .roles
            .iter()
            .any(|r| r.trim().eq_ignore_ascii_case("guest"))
        && (actor.user_tier.trim().eq_ignore_ascii_case("admin")
            || actor
                .roles
                .iter()
                .any(|r| r.trim().eq_ignore_ascii_case("admin")))
}
fn admin_required() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"detail":"Admin role required"})),
    )
        .into_response()
}
async fn worker_status(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
) -> Response {
    if !admin(&actor) {
        return admin_required();
    }
    output(s.service.worker_status(), StatusCode::BAD_REQUEST)
}
async fn force_complete(
    State(s): State<WriteApiState>,
    Extension(actor): Extension<Actor>,
    Path((dataset, document)): Path<(String, String)>,
) -> Response {
    if !admin(&actor) {
        return admin_required();
    }
    let result = async {
        let d = s
            .service
            .store
            .require_dataset_access(&actor, &dataset, "editor")
            .await?;
        require_index_writable(&d)?;
        let doc = s.service.require_active_document(&d, &document).await?;
        if doc["status"] != "completed" {
            return Err(IngestError::Validation(
                "Document is not active and completed; repair it through the durable worker".into(),
            ));
        }
        Ok(json!({"status":"completed","document_id":document}))
    }
    .await;
    output(result, StatusCode::BAD_REQUEST)
}
