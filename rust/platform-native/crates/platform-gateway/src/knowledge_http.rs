//! Bounded read-only migration of the Gateway knowledge document proxy.
//!
//! The exact KS routes retain dataset/document/active-generation ACL authority.
//! Management detail and serving citation source are deliberately different routes.
//! No local SQL, ACL cache, retrieval, writes, uploads, or arbitrary proxy URLs.
use crate::knowledge::{KnowledgeActor, KnowledgeReadResponse, KnowledgeService};
use ai_platform_auth::{
    api_keys::{SelectedCredential, select_credential},
    service::AuthService,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

#[derive(Clone)]
struct ReadState {
    auth: Arc<AuthService>,
    service: Option<Arc<KnowledgeService>>,
}

/// Current-authority JWT mount. API keys remain unmounted until audited admission
/// is integrated; declaring knowledge:write never implicitly grants knowledge:read.
pub fn router(auth: Arc<AuthService>, service: Option<Arc<KnowledgeService>>) -> Router {
    Router::new()
        .route("/api/v1/knowledge/{dataset_id}/documents", get(documents))
        .route(
            "/api/v1/knowledge/{dataset_id}/documents/{document_id}",
            get(document),
        )
        .route(
            "/api/v1/knowledge/{dataset_id}/documents/{document_id}/source",
            get(active_source),
        )
        .route(
            "/api/v1/knowledge/{dataset_id}/documents/{document_id}/versions",
            get(versions),
        )
        .route(
            "/api/v1/knowledge/{dataset_id}/documents/{document_id}/versions/{version}",
            get(version),
        )
        .route(
            "/api/v1/knowledge/{dataset_id}/documents/{document_id}/versions/{version}/source",
            get(historical_source),
        )
        .with_state(ReadState { auth, service })
}

#[derive(Default, Deserialize)]
struct Pagination {
    limit: Option<u16>,
    offset: Option<u64>,
}
#[derive(Deserialize)]
struct SourceQuery {
    hash: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
enum ReadKind {
    Documents { limit: u16, offset: u64 },
    Document,
    ActiveSource,
    Versions { limit: u16, offset: u64 },
    Version(u64),
    HistoricalSource { version: u64, hash: String },
}
#[derive(Clone, Debug)]
struct ReadRequest {
    dataset: String,
    document: Option<String>,
    kind: ReadKind,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadError {
    InvalidQuery,
    NotFound,
    SourceUnavailable,
    Upstream,
}
impl IntoResponse for ReadError {
    fn into_response(self) -> Response {
        match self {
            Self::InvalidQuery => {
                crate::error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid query parameters")
            }
            Self::NotFound => crate::error(StatusCode::NOT_FOUND, "Not Found"),
            Self::SourceUnavailable => (
                StatusCode::NOT_FOUND,
                Json(json!({"detail":{"code":"SOURCE_VERSION_UNAVAILABLE"}})),
            )
                .into_response(),
            Self::Upstream => crate::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Knowledge service is unavailable",
            ),
        }
    }
}

fn checked_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 1024
        && id != "."
        && id != ".."
        && !id.chars().any(char::is_control)
        && !id.contains(['/', '\\', '?', '#', '%'])
}
fn page(
    query: Result<Query<Pagination>, QueryRejection>,
    versions: bool,
) -> Result<ReadKind, ReadError> {
    let Query(query) = query.map_err(|_| ReadError::InvalidQuery)?;
    let cap = if versions { 100 } else { 200 };
    let limit = query.limit.unwrap_or(if versions { 20 } else { 200 });
    let offset = query.offset.unwrap_or(0);
    if limit == 0 || limit > cap || offset > i64::MAX as u64 {
        return Err(ReadError::InvalidQuery);
    }
    Ok(if versions {
        ReadKind::Versions { limit, offset }
    } else {
        ReadKind::Documents { limit, offset }
    })
}
fn version_number(value: &str) -> Result<u64, ReadError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ReadError::NotFound);
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or(ReadError::NotFound)
}
fn historical_kind(
    version: &str,
    query: Result<Query<SourceQuery>, QueryRejection>,
) -> Result<ReadKind, ReadError> {
    let version = version_number(version)?;
    let Query(query) = query.map_err(|_| ReadError::InvalidQuery)?;
    // Match FastAPI's pre-handler length check before source availability masking.
    if query.hash.chars().count() != 64 {
        return Err(ReadError::InvalidQuery);
    }
    let hash = query.hash.trim().to_ascii_lowercase();
    if version == 0 || hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ReadError::SourceUnavailable);
    }
    Ok(ReadKind::HistoricalSource { version, hash })
}
impl ReadRequest {
    fn new(dataset: String, document: Option<String>, kind: ReadKind) -> Result<Self, ReadError> {
        if !checked_id(&dataset) || document.as_deref().is_some_and(|id| !checked_id(id)) {
            return Err(ReadError::NotFound);
        }
        if matches!(kind, ReadKind::Documents { .. }) != document.is_none() {
            return Err(ReadError::NotFound);
        }
        // KS reserves this static path for a streaming progress feed. It must
        // never be reached through the bounded document-detail proxy.
        if matches!(kind, ReadKind::Document) && document.as_deref() == Some("progress") {
            return Err(ReadError::NotFound);
        }
        Ok(Self {
            dataset,
            document,
            kind,
        })
    }
    fn segments(&self) -> Vec<String> {
        let mut segments = vec![
            "api".into(),
            "v1".into(),
            "knowledge".into(),
            self.dataset.clone(),
            "documents".into(),
        ];
        if let Some(document) = &self.document {
            segments.push(document.clone());
        }
        match &self.kind {
            ReadKind::Documents { .. } | ReadKind::Document => {}
            ReadKind::ActiveSource => segments.push("source".into()),
            ReadKind::Versions { .. } => segments.push("versions".into()),
            ReadKind::Version(version) => segments.extend(["versions".into(), version.to_string()]),
            ReadKind::HistoricalSource { version, .. } => {
                segments.extend(["versions".into(), version.to_string(), "source".into()])
            }
        }
        segments
    }
    fn query(&self) -> Vec<(&'static str, String)> {
        match &self.kind {
            ReadKind::Documents { limit, offset } | ReadKind::Versions { limit, offset } => {
                vec![("limit", limit.to_string()), ("offset", offset.to_string())]
            }
            ReadKind::HistoricalSource { hash, .. } => vec![("hash", hash.clone())],
            _ => vec![],
        }
    }
}

async fn dispatch(
    state: ReadState,
    headers: HeaderMap,
    request: Result<ReadRequest, ReadError>,
) -> Response {
    let token = match select_credential(&headers, &HeaderName::from_static("x-api-key")) {
        Ok(SelectedCredential::Jwt(token)) => token,
        _ => return crate::error(StatusCode::UNAUTHORIZED, "Authentication required"),
    };
    let principal = match state.auth.authenticate(token).await {
        Ok(principal) => principal,
        Err(error) => {
            return crate::error(
                StatusCode::from_u16(error.status()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
                "Authentication failed",
            );
        }
    };
    let actor = match KnowledgeActor::from_principal(&principal) {
        Ok(actor) => actor,
        Err(_) => return crate::error(StatusCode::UNAUTHORIZED, "Authentication required"),
    };
    let request = match request {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let Some(service) = state.service else {
        return ReadError::Upstream.into_response();
    };
    let segments = request.segments();
    let parts: Vec<&str> = segments.iter().map(String::as_str).collect();
    // Every read uses the final serving/management KS route under fresh signed
    // authority. Preflight grants or a cached catalog cannot authorize this body.
    match service
        .document_read_response(&actor, &parts, &request.query())
        .await
    {
        Ok(response) => match checked_response(&request, response) {
            Ok(response) => response,
            Err(error) => error.into_response(),
        },
        Err(_) => ReadError::Upstream.into_response(),
    }
}

async fn documents(
    State(state): State<ReadState>,
    Path(dataset): Path<String>,
    headers: HeaderMap,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Response {
    let request = page(query, false).and_then(|kind| ReadRequest::new(dataset, None, kind));
    dispatch(state, headers, request).await
}
async fn document(
    State(state): State<ReadState>,
    Path((dataset, document)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    dispatch(
        state,
        headers,
        ReadRequest::new(dataset, Some(document), ReadKind::Document),
    )
    .await
}
async fn active_source(
    State(state): State<ReadState>,
    Path((dataset, document)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    dispatch(
        state,
        headers,
        ReadRequest::new(dataset, Some(document), ReadKind::ActiveSource),
    )
    .await
}
async fn versions(
    State(state): State<ReadState>,
    Path((dataset, document)): Path<(String, String)>,
    headers: HeaderMap,
    query: Result<Query<Pagination>, QueryRejection>,
) -> Response {
    let request =
        page(query, true).and_then(|kind| ReadRequest::new(dataset, Some(document), kind));
    dispatch(state, headers, request).await
}
async fn version(
    State(state): State<ReadState>,
    Path((dataset, document, version)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let request = version_number(&version)
        .and_then(|version| ReadRequest::new(dataset, Some(document), ReadKind::Version(version)));
    dispatch(state, headers, request).await
}
async fn historical_source(
    State(state): State<ReadState>,
    Path((dataset, document, version)): Path<(String, String, String)>,
    headers: HeaderMap,
    query: Result<Query<SourceQuery>, QueryRejection>,
) -> Response {
    let request = historical_kind(&version, query)
        .and_then(|kind| ReadRequest::new(dataset, Some(document), kind));
    dispatch(state, headers, request).await
}

fn document_identity(row: &Value, dataset: &str, document: Option<&str>) -> bool {
    row.is_object()
        && row.get("dataset_id").and_then(Value::as_str) == Some(dataset)
        && row
            .get("document_id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty() && document.is_none_or(|expected| expected == id))
}
fn version_identity(row: &Value, document: &str, version: Option<u64>) -> bool {
    row.is_object()
        && row.get("document_id").and_then(Value::as_str) == Some(document)
        && row
            .get("version_number")
            .and_then(Value::as_u64)
            .is_some_and(|number| version.is_none_or(|expected| expected == number))
}
fn decimal_header(value: &HeaderValue) -> Result<u64, ReadError> {
    let value = value.to_str().map_err(|_| ReadError::Upstream)?;
    if value.is_empty() || value.len() > 20 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ReadError::Upstream);
    }
    value.parse().map_err(|_| ReadError::Upstream)
}
fn validate_payload(
    request: &ReadRequest,
    upstream: &KnowledgeReadResponse,
) -> Result<(), ReadError> {
    let payload = &upstream.payload;
    let document = request.document.as_deref().unwrap_or("");
    let valid = match &request.kind {
        ReadKind::Documents { limit, .. } => {
            let total = upstream
                .total_count
                .as_ref()
                .ok_or(ReadError::Upstream)
                .and_then(decimal_header)?;
            payload.as_array().is_some_and(|rows| {
                rows.len() <= usize::from(*limit)
                    && rows.len() as u64 <= total
                    && rows
                        .iter()
                        .all(|row| document_identity(row, &request.dataset, None))
            })
        }
        ReadKind::Document | ReadKind::ActiveSource => {
            document_identity(payload, &request.dataset, Some(document))
        }
        ReadKind::Versions { limit, .. } => {
            let total = payload.get("total").and_then(Value::as_u64);
            payload
                .get("versions")
                .and_then(Value::as_array)
                .is_some_and(|rows| {
                    rows.len() <= usize::from(*limit)
                        && total.is_some_and(|total| rows.len() as u64 <= total)
                        && rows.iter().all(|row| version_identity(row, document, None))
                })
                && payload
                    .get("current_version")
                    .is_some_and(|value| value.is_null() || value.as_u64().is_some())
        }
        ReadKind::Version(version) => version_identity(payload, document, Some(*version)),
        ReadKind::HistoricalSource { version, hash } => {
            version_identity(payload, document, Some(*version))
                && payload.get("source_hash").and_then(Value::as_str) == Some(hash)
                && payload.get("title").is_some_and(Value::is_string)
                && payload
                    .get("content")
                    .and_then(Value::as_str)
                    .is_some_and(|content| {
                        format!("{:x}", Sha256::digest(content.as_bytes())) == *hash
                    })
        }
    };
    if valid {
        Ok(())
    } else {
        Err(ReadError::Upstream)
    }
}
fn checked_response(
    request: &ReadRequest,
    upstream: KnowledgeReadResponse,
) -> Result<Response, ReadError> {
    if upstream.status == StatusCode::OK {
        validate_payload(request, &upstream)?;
    } else if upstream.status == StatusCode::UNAUTHORIZED
        || !(upstream.status.is_client_error() || upstream.status.is_server_error())
        || !upstream.payload.is_object()
        || upstream.payload.get("detail").is_none()
    {
        return Err(ReadError::Upstream);
    }
    let mut response = (upstream.status, Json(upstream.payload)).into_response();
    // Successful source content is private to the current actor; no browser/shared
    // cache may reuse the bytes after an ACL/active-version change.
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    if upstream.status == StatusCode::OK
        && matches!(request.kind, ReadKind::Documents { .. })
        && let Some(total) = upstream.total_count
    {
        response.headers_mut().insert("x-total-count", total);
    }
    if let Some(retry_after) = upstream.retry_after {
        decimal_header(&retry_after)?;
        response.headers_mut().insert("retry-after", retry_after);
    }
    Ok(response)
}

