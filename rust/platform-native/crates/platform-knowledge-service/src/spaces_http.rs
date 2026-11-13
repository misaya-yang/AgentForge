//! Knowledge space routes. The gateway catch-all already proxies this prefix.
use ai_platform_knowledge_domain::spaces::{
    ContentPut, ImportBody, ItemFilter, MemberPut, NewItem, NewSpace, SharePut, SpaceError, SpacePatch,
    Spaces, blocks_to_html, decode_cursor, html_to_blocks, line_diff, markdown_to_blocks, page_limit,
    parse_view, project, read_zip_pages,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use crate::migration_knowledge_query::types::UserContext;
use base64::Engine;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub enum SpaceChange {
    Item(String),
    Space(String),
}

#[derive(Clone, Default)]
pub struct SpaceIndexHook {
    pub notify: Option<Arc<dyn Fn(crate::migration_knowledge_query::types::UserContext, SpaceChange) + Send + Sync>>,
}

#[derive(Deserialize)]
struct DiffQuery {
    from: i64,
    to: i64,
}

#[derive(Deserialize, Default)]
struct ExportQuery {
    format: Option<String>,
}

#[derive(Deserialize, Default)]
struct ItemQuery {
    view: Option<String>,
    kind: Option<String>,
    q: Option<String>,
    space_id: Option<String>,
    cursor: Option<String>,
    limit: Option<i64>,
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn routes(pool: PgPool, auth: Arc<crate::migration_knowledge_query::auth::GatewayAuth>) -> Router {
    routes_with(pool, auth, SpaceIndexHook::default())
}

pub fn routes_with(
    pool: PgPool,
    auth: Arc<crate::migration_knowledge_query::auth::GatewayAuth>,
    hook: SpaceIndexHook,
) -> Router {
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
        .route("/api/v1/knowledge/spaces/{space_id}/tree", get(list_tree))
        .route("/api/v1/knowledge/spaces/{space_id}/import", post(import_pages))
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
        .route("/api/v1/knowledge/space-items/{item_id}/revisions", get(list_revisions))
        .route(
            "/api/v1/knowledge/space-items/{item_id}/revisions/{revision}/restore",
            post(restore_revision),
        )
        .route("/api/v1/knowledge/space-items/{item_id}/diff", get(diff_revisions))
        .route("/api/v1/knowledge/space-items/{item_id}/backlinks", get(list_backlinks))
        .route("/api/v1/knowledge/space-items/{item_id}/export", get(export_item))
        .route("/api/v1/knowledge/space-items/{item_id}/collab", get(crate::spaces_collab::connect))
        .route("/api/v1/knowledge/space-trash", get(list_trash))
        .with_state(Spaces::new(pool))
        .layer(axum::middleware::from_fn_with_state(
            auth,
            crate::migration_knowledge_query::auth::verify_gateway,
        ))
        .layer(Extension(hook))
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
    Extension(hook): Extension<SpaceIndexHook>,
    Path(space_id): Path<String>,
    Json(input): Json<MemberPut>,
) -> Response {
    let result = spaces.put_members(&user, &space_id, input).await.map(|rows| json!({"members": rows}));
    if result.is_ok() {
        notify(&hook, &user, SpaceChange::Space(space_id));
    }
    respond(result)
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
    Extension(hook): Extension<SpaceIndexHook>,
    Path(item_id): Path<String>,
) -> Response {
    let result = spaces.delete_item(&user, &item_id).await;
    if result.is_ok() {
        notify(&hook, &user, SpaceChange::Item(item_id.clone()));
    }
    empty(result)
}

async fn restore_item(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Extension(hook): Extension<SpaceIndexHook>,
    Path(item_id): Path<String>,
) -> Response {
    let result = spaces.restore_item(&user, &item_id).await.map(|item| json!(item));
    if result.is_ok() {
        notify(&hook, &user, SpaceChange::Item(item_id.clone()));
    }
    respond(result)
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
    Extension(hook): Extension<SpaceIndexHook>,
    Path(item_id): Path<String>,
    Json(input): Json<SharePut>,
) -> Response {
    let result = spaces.put_shares(&user, &item_id, input).await.map(|rows| json!({"shares": rows}));
    if result.is_ok() {
        notify(&hook, &user, SpaceChange::Item(item_id.clone()));
    }
    respond(result)
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
    Extension(hook): Extension<SpaceIndexHook>,
    Path(item_id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<ContentPut>,
) -> Response {
    let expected = match expected_revision(&headers) {
        Ok(value) => value,
        Err(error) => return fail(error),
    };
    match spaces.put_content(&user, &item_id, expected, input).await {
        Ok(content) => {
            notify(&hook, &user, SpaceChange::Item(item_id.clone()));
            content_response(content)
        }
        Err(error) => fail(error),
    }
}

async fn list_tree(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(space_id): Path<String>,
) -> Response {
    respond(spaces.list_tree(&user, &space_id).await.map(|items| json!({"items": items})))
}

async fn list_revisions(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
) -> Response {
    respond(spaces.list_revisions(&user, &item_id).await.map(|revisions| json!({"revisions": revisions})))
}

async fn diff_revisions(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
    Query(query): Query<DiffQuery>,
) -> Response {
    let from = match spaces.revision_markdown(&user, &item_id, query.from).await {
        Ok(value) => value,
        Err(error) => return fail(error),
    };
    let to = match spaces.revision_markdown(&user, &item_id, query.to).await {
        Ok(value) => value,
        Err(error) => return fail(error),
    };
    respond(Ok(json!({"from": query.from, "to": query.to, "diff": line_diff(&from, &to)})))
}

async fn restore_revision(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Extension(hook): Extension<SpaceIndexHook>,
    Path((item_id, revision)): Path<(String, i64)>,
    headers: HeaderMap,
) -> Response {
    let expected = match expected_revision(&headers) {
        Ok(value) => value,
        Err(error) => return fail(error),
    };
    match spaces.restore_revision(&user, &item_id, revision, expected).await {
        Ok(content) => {
            notify(&hook, &user, SpaceChange::Item(item_id.clone()));
            content_response(content)
        }
        Err(error) => fail(error),
    }
}

async fn list_backlinks(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
) -> Response {
    respond(spaces.backlinks(&user, &item_id).await.map(|items| json!({"items": items})))
}

async fn export_item(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Path(item_id): Path<String>,
    Query(query): Query<ExportQuery>,
) -> Response {
    let content = match spaces.get_content(&user, &item_id).await {
        Ok(content) => content,
        Err(error) => return fail(error),
    };
    let html = query.format.as_deref() == Some("html");
    let body = if html {
        if content.blocks.is_array() {
            blocks_to_html(&content.blocks)
        } else {
            ai_platform_knowledge_domain::spaces::markdown_to_html(&content.markdown)
        }
    } else if content.markdown.is_empty() {
        match project(&content.content_format, &content.blocks, &content.markdown) {
            Ok(value) => value,
            Err(error) => return fail(error),
        }
    } else {
        content.markdown
    };
    respond(Ok(json!({
        "item_id": item_id,
        "format": if html { "html" } else { "markdown" },
        "filename": format!("{item_id}.{}", if html { "html" } else { "md" }),
        "body": body,
    })))
}

async fn import_pages(
    State(spaces): State<Spaces>,
    Extension(user): Extension<UserContext>,
    Extension(hook): Extension<SpaceIndexHook>,
    Path(space_id): Path<String>,
    Json(body): Json<ImportBody>,
) -> Response {
    let bytes = match base64::engine::general_purpose::STANDARD.decode(body.content_base64.trim()) {
        Ok(bytes) => bytes,
        Err(_) => return fail(SpaceError::Invalid("invalid import payload")),
    };
    let pages = match import_sources(&body.format, &body.filename, &bytes) {
        Ok(pages) => pages,
        Err(error) => return fail(error),
    };
    let mut created = Vec::new();
    let mut folders = std::collections::BTreeMap::new();
    for (index, page) in pages.iter().enumerate() {
        let parent = match folder_parent(&spaces, &user, &space_id, body.parent_id.as_deref(), &page.path, &mut folders).await {
            Ok(parent) => parent,
            Err(error) => return fail(error),
        };
        let item = match spaces.create_item(&user, &space_id, NewItem {
            title: page.title.clone(),
            kind: "page".into(),
            icon: String::new(),
            parent_id: parent,
            position: Some(format!("a{index:x}")),
            content_format: "blocknote_json".into(),
            source: "upload".into(),
        }).await {
            Ok(item) => item,
            Err(error) => return fail(error),
        };
        if let Err(error) = spaces.put_content(&user, &item.item_id, Some(0), ContentPut {
            content_format: "blocknote_json".into(),
            blocks: page.blocks.clone(),
            markdown: String::new(),
        }).await {
            return fail(error);
        }
        notify(&hook, &user, SpaceChange::Item(item.item_id.clone()));
        created.push(item);
    }
    (StatusCode::CREATED, Json(json!({"items": created}))).into_response()
}

struct ImportPage {
    path: String,
    title: String,
    blocks: Value,
}

fn import_sources(format: &str, filename: &str, bytes: &[u8]) -> Result<Vec<ImportPage>, SpaceError> {
    match format {
        "markdown" | "md" => {
            let text = std::str::from_utf8(bytes).map_err(|_| SpaceError::Invalid("invalid import payload"))?;
            Ok(vec![ImportPage { path: filename.to_owned(), title: import_title(filename, text), blocks: markdown_to_blocks(text) }])
        }
        "html" => {
            let text = std::str::from_utf8(bytes).map_err(|_| SpaceError::Invalid("invalid import payload"))?;
            Ok(vec![ImportPage { path: filename.to_owned(), title: import_title(filename, text), blocks: html_to_blocks(text)? }])
        }
        "zip" => Ok(read_zip_pages(bytes)?.into_iter().map(|(path, title, blocks)| ImportPage { path, title, blocks }).collect()),
        _ => Err(SpaceError::Invalid("invalid import format")),
    }
}

fn import_title(filename: &str, text: &str) -> String {
    let heading = text.lines().find_map(|line| line.trim().strip_prefix("# ")).map(str::trim);
    let stem = filename.rsplit('/').next().unwrap_or(filename).rsplit_once('.').map(|(stem, _)| stem).unwrap_or(filename);
    let title = heading.filter(|value| !value.is_empty()).unwrap_or(stem).trim();
    if title.is_empty() { "page".into() } else { title.chars().take(500).collect() }
}

async fn folder_parent(
    spaces: &Spaces,
    user: &UserContext,
    space_id: &str,
    root: Option<&str>,
    path: &str,
    folders: &mut std::collections::BTreeMap<String, String>,
) -> Result<Option<String>, SpaceError> {
    let mut parent = root.map(str::to_owned);
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() < 2 {
        return Ok(parent);
    }
    let mut prefix = String::new();
    for part in &parts[..parts.len() - 1] {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        if let Some(existing) = folders.get(&prefix) {
            parent = Some(existing.clone());
            continue;
        }
        let folder = spaces.create_item(user, space_id, NewItem {
            title: (*part).chars().take(500).collect(),
            kind: "folder".into(),
            icon: String::new(),
            parent_id: parent.clone(),
            position: Some("a0".into()),
            content_format: "markdown".into(),
            source: "upload".into(),
        }).await?;
        folders.insert(prefix.clone(), folder.item_id.clone());
        parent = Some(folder.item_id);
    }
    Ok(parent)
}

fn notify(hook: &SpaceIndexHook, user: &UserContext, change: SpaceChange) {
    if let Some(notify) = &hook.notify {
        notify(user.clone(), change);
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

pub(crate) fn fail(error: SpaceError) -> Response {
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
