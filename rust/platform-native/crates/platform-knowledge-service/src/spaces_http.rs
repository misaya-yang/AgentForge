//! Knowledge space routes. The gateway catch-all already proxies this prefix.
use ai_platform_knowledge_domain::spaces::{
    ContentPut, ItemFilter, MemberPut, NewItem, NewSpace, SharePut, SpaceError, SpacePatch, Spaces,
    decode_cursor, page_limit, parse_view,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use crate::migration_knowledge_query::types::UserContext;
use serde::Deserialize;
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Deserialize, Default)]
struct ItemQuery {
    view: Option<String>,
    kind: Option<String>,
    q: Option<String>,
    space_id: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

pub fn routes(pool: PgPool, auth: Arc<crate::migration_knowledge_query::auth::GatewayAuth>) -> Router {
    Router::new()
        .route("/api/v1/knowledge/spaces", get(list_spaces).post(create_space))
        .route(
            "/api/v1/knowledge/spaces/{space_id}",
            get(get_space).patch(patch_space).delete(archive_space),
        )
        .route(
            "/api/v1/knowledge/spaces/{space_id}/members",
            get(list_members).put(put_members),
        )
        .route("/api/v1/knowledge/spaces/{space_id}/items", post(create_item))
        .route("/api/v1/knowledge/space-items", get(list_items))
        .route(
            "/api/v1/knowledge/space-items/{item_id}",
            get(get_item).patch(patch_item).delete(delete_item),
        )
        .route("/api/v1/knowledge/space-items/{item_id}/restore", post(restore_item))
        .route("/api/v1/knowledge/space-items/{item_id}/star", put(star_item).delete(unstar_item))
        .route("/api/v1/knowledge/space-items/{item_id}/shares", put(put_shares))
        .route(
            "/api/v1/knowledge/space-items/{item_id}/content",
            get(get_content).put(put_content),
        )
        .route("/api/v1/knowledge/space-trash", get(list_trash))
        .with_state(Spaces::new(pool))
        .layer(axum::middleware::from_fn_with_state(
            auth,
            crate::migration_knowledge_query::auth::verify_gateway,
        ))
}

async fn list_spaces(State(spaces): State<Spaces>, Extension(user): Extension<UserContext>) -> Response {
    respond(spaces.list_spaces(&user).await.map(|rows| json!({"spaces": rows})))
}

async fn create_space(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Json(input): Json<NewSpace>,
) -> Response {
    match spaces.create_space(&user, input).await {
        Ok(space) => (StatusCode::CREATED, Json(space)).into_response(),
        Err(error) => fail(error),
    }
}

async fn get_space(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(space_id): Path<String>,
) -> Response {
    respond(spaces.get_space(&user, &space_id).await.map(|space| json!(space)))
}

async fn patch_space(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(space_id): Path<String>,
    Json(patch): Json<SpacePatch>,
) -> Response {
    respond(spaces.patch_space(&user, &space_id, patch).await.map(|space| json!(space)))
}

async fn archive_space(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(space_id): Path<String>,
) -> Response {
    empty(spaces.archive_space(&user, &space_id).await)
}

async fn list_members(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(space_id): Path<String>,
) -> Response {
    respond(spaces.list_members(&user, &space_id).await.map(|rows| json!({"members": rows})))
}

async fn put_members(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(space_id): Path<String>,
    Json(input): Json<MemberPut>,
) -> Response {
    respond(spaces.put_members(&user, &space_id, input).await.map(|rows| json!({"members": rows})))
}

async fn create_item(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(space_id): Path<String>,
    Json(input): Json<NewItem>,
) -> Response {
    match spaces.create_item(&user, &space_id, input).await {
        Ok(item) => (StatusCode::CREATED, Json(item)).into_response(),
        Err(error) => fail(error),
    }
}

async fn list_items(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Query(query): Query<ItemQuery>,
) -> Response {
    items(&spaces, &user, query, false).await
}

async fn list_trash(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Query(query): Query<ItemQuery>,
) -> Response {
    items(&spaces, &user, query, true).await
}

async fn items(spaces: &Spaces, user: &UserContext, query: ItemQuery, trash: bool) -> Response {
    let filter = match item_filter(query, trash) {
        Ok(filter) => filter,
        Err(error) => return fail(error),
    };
    respond(spaces.list_items(user, filter).await.map(|page| json!(page)))
}

async fn get_item(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
) -> Response {
    respond(spaces.get_item(&user, &item_id).await.map(|item| json!(item)))
}

async fn patch_item(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
    Json(patch): Json<ai_platform_knowledge_domain::spaces::ItemPatch>,
) -> Response {
    respond(spaces.patch_item(&user, &item_id, patch).await.map(|item| json!(item)))
}

async fn delete_item(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
) -> Response {
    empty(spaces.delete_item(&user, &item_id).await)
}

async fn restore_item(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
) -> Response {
    respond(spaces.restore_item(&user, &item_id).await.map(|item| json!(item)))
}

async fn star_item(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
) -> Response {
    empty(spaces.star(&user, &item_id, true).await)
}

async fn unstar_item(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
) -> Response {
    empty(spaces.star(&user, &item_id, false).await)
}

async fn put_shares(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
    Json(input): Json<SharePut>,
) -> Response {
    respond(spaces.put_shares(&user, &item_id, input).await.map(|rows| json!({"shares": rows})))
}

async fn get_content(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
) -> Response {
    match spaces.get_content(&user, &item_id).await {
        Ok(content) => content_response(content),
        Err(error) => fail(error),
    }
}

async fn put_content(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ContentPut>,
) -> Response {
    let expected = match expected_revision(&headers) {
        Ok(value) => value,
        Err(error) => return fail(error),
    };
    match spaces.put_content(&user, &item_id, expected, input).await {
        Ok(content) => content_response(content),
        Err(error) => fail(error),
    }
}

fn item_filter(query: ItemQuery, trash: bool) -> Result<ItemFilter, SpaceError> {
    Ok(ItemFilter {
        view: parse_view(query.view.as_deref(), trash)?,
        kind: query.kind.filter(|value| !value.is_empty()),
        query: query.q.filter(|value| !value.is_empty()),
        space_id: query.space_id.filter(|value| !value.is_empty()),
        cursor: query.cursor.as_deref().map(decode_cursor).transpose()?,
        limit: page_limit(query.limit)?,
    })
}

fn expected_revision(headers: &HeaderMap) -> Result<Option<i64>, SpaceError> {
    match headers.get(axum::http::header::IF_MATCH).and_then(|value| value.to_str().ok()) {
        None => Ok(None),
        Some(value) => value
            .trim()
            .trim_matches('"')
            .parse::<i64>()
            .map(Some)
            .map_err(|_| SpaceError::Conflict),
    }
}

fn content_response(content: ai_platform_knowledge_domain::spaces::ItemContent) -> Response {
    let revision = content.revision;
    let mut response = Json(content).into_response();
    if let Ok(etag) = HeaderValue::from_str(&format!("\"{revision}\"")) {
        response.headers_mut().insert(axum::http::header::ETAG, etag);
    }
    response
}

fn respond(result: Result<serde_json::Value, SpaceError>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => fail(error),
    }
}

fn empty(result: Result<(), SpaceError>) -> Response {
    match result {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => fail(error),
    }
}

fn fail(error: SpaceError) -> Response {
    let (status, code, message) = match error {
        SpaceError::NotFound => (StatusCode::NOT_FOUND, "not_found", "Space or item was not found"),
        SpaceError::Conflict => (StatusCode::PRECONDITION_FAILED, "conflict", "Content revision does not match"),
        SpaceError::Precondition => (
            StatusCode::PRECONDITION_REQUIRED,
            "precondition_required",
            "If-Match revision is required",
        ),
        SpaceError::Invalid(message) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid", message),
        SpaceError::SchemaUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "schema_unavailable",
            "Knowledge spaces schema is not installed",
        ),
        SpaceError::Unavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "Knowledge spaces are temporarily unavailable",
        ),
    };
    (status, Json(json!({"code": code, "message": message}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, routing::get};
    use crate::migration_knowledge_query::auth::{GatewayAuth, ReplayStore};
    use std::collections::BTreeMap;
    use tower::ServiceExt;

    #[tokio::test]
    async fn spaces_route_is_not_captured_as_a_dataset() {
        let auth = Arc::new(
            GatewayAuth::signed_v2(
                BTreeMap::from([("test".into(), "0123456789abcdef".into())]),
                ReplayStore::memory(),
            )
            .unwrap(),
        );
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://synthetic:synthetic@127.0.0.1:1/unused")
            .unwrap();
        let app = Router::new()
            .route("/api/v1/knowledge/{dataset_id}", get(|| async { "DATASET" }))
            .merge(routes(pool, auth));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/knowledge/spaces")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(response.into_body(), 2048).await.unwrap();
        assert!(!bytes.windows(7).any(|window| window == b"DATASET"));
    }
}
