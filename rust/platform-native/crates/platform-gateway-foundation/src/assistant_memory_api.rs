//! Four opt-in, owner-scoped assistant memory routes. Construction is inert.
//! JSON decoding precedes the trusted identity dependency; Pydantic-shaped
//! validation precedes the endpoint's authentication and database checks.
//! Each store call is a separate source operation, including PUT readback and
//! access statistics. A failed readback never rolls back a successful write.
use crate::{
    assistant_memory_types::{
        AssistantMemoryError, AssistantMemoryFuture, AssistantMemoryIdentity, AssistantMemoryStore,
        MEMORY_CONTROL_KEY, MEMORY_EFFECT, memory_database, memory_enabled, source_value,
    },
    assistant_memory_validation::{
        MemoryItemUpdate, MemoryPreferenceUpdate, delete_key, parse_body,
    },
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{FromRequest, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// A host resolver must preserve canonical guest/authentication semantics and
/// derive ownership from trusted authority, never caller ownership headers.
pub trait AssistantMemoryIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> AssistantMemoryFuture<'a, AssistantMemoryIdentity>;
}

#[derive(Clone, Default)]
pub struct AssistantMemoryApi {
    pub store: Option<Arc<dyn AssistantMemoryStore>>,
}
fn actor(owner: &AssistantMemoryIdentity) -> Result<(), AssistantMemoryError> {
    if !owner.is_authenticated || owner.tenant_id.is_empty() || owner.user_id.is_empty() {
        Err(AssistantMemoryError::detail(401, "Authentication required"))
    } else {
        Ok(())
    }
}
fn reserved(key: &str) -> Result<(), AssistantMemoryError> {
    if key.starts_with("__assistant_") {
        Err(AssistantMemoryError::detail(
            400,
            "This memory key is reserved",
        ))
    } else {
        Ok(())
    }
}
fn operation_failure(error: AssistantMemoryError, detail: &'static str) -> AssistantMemoryError {
    match error {
        AssistantMemoryError::Cancelled => AssistantMemoryError::Cancelled,
        _ => AssistantMemoryError::detail(503, detail),
    }
}
impl AssistantMemoryApi {
    pub async fn get(
        &self,
        owner: &AssistantMemoryIdentity,
    ) -> Result<Value, AssistantMemoryError> {
        actor(owner)?;
        let database = memory_database(self.store.as_ref())?;
        let mut rows = database
            .list_active(owner)
            .await
            .map_err(|error| operation_failure(error, "Memory storage is unavailable"))?;
        // The source awaits the control lookup after fetching, before projection.
        let enabled = memory_enabled(self.store.as_ref(), owner).await?;
        let has_more = rows.len() > 100;
        rows.truncate(100);
        Ok(json!({"enabled":enabled,"effect":MEMORY_EFFECT,"items":rows,"has_more":has_more}))
    }
    pub async fn patch(
        &self,
        owner: &AssistantMemoryIdentity,
        body: MemoryPreferenceUpdate,
    ) -> Result<Value, AssistantMemoryError> {
        actor(owner)?;
        let database = memory_database(self.store.as_ref())?;
        database
            .upsert(owner, MEMORY_CONTROL_KEY, json!(body.enabled), true)
            .await
            .map_err(|error| operation_failure(error, "Memory preference could not be saved"))?;
        Ok(json!({"enabled":memory_enabled(self.store.as_ref(), owner).await?}))
    }
    pub async fn put(
        &self,
        owner: &AssistantMemoryIdentity,
        body: MemoryItemUpdate,
    ) -> Result<Value, AssistantMemoryError> {
        actor(owner)?;
        reserved(&body.key)?;
        let database = memory_database(self.store.as_ref())?;
        database
            .upsert(owner, &body.key, json!(body.value), false)
            .await
            .map_err(|error| operation_failure(error, "Memory item could not be saved"))?;
        let raw = database.read_active(owner, &body.key).await?;
        let value = match raw {
            Some(raw) => {
                database.increment_access(owner, &body.key).await?;
                source_value(raw)
            }
            None => Value::Null,
        };
        Ok(json!({"key":body.key,"value":value}))
    }
    pub async fn delete(
        &self,
        owner: &AssistantMemoryIdentity,
        key: &str,
    ) -> Result<Value, AssistantMemoryError> {
        actor(owner)?;
        reserved(key)?;
        memory_database(self.store.as_ref())?
            .delete(owner, key)
            .await
            .map_err(|error| operation_failure(error, "Memory item could not be deleted"))?;
        Ok(json!({"key":key,"status":"deleted"}))
    }
}

pub const ASSISTANT_MEMORY_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/api/v1/assistant/memory", "get_memory"),
    (
        "PATCH",
        "/api/v1/assistant/memory",
        "update_memory_preference",
    ),
    ("PUT", "/api/v1/assistant/memory/items", "put_memory_item"),
    (
        "DELETE",
        "/api/v1/assistant/memory/items",
        "delete_memory_item",
    ),
];
#[derive(Clone)]
struct HttpState {
    api: AssistantMemoryApi,
    identity: Arc<dyn AssistantMemoryIdentityResolver>,
}
/// Deployment origin supplied by trusted host composition, never derived from
/// request Host, forwarding headers or URI authority. Without this extension,
/// slash redirects remain same-origin relative URLs.
#[derive(Clone, Debug)]
pub struct AssistantMemoryTrustedOrigin(String);
impl AssistantMemoryTrustedOrigin {
    pub fn parse(origin: &str) -> Result<Self, AssistantMemoryError> {
        let url = reqwest::Url::parse(origin).map_err(|_| AssistantMemoryError::Internal)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            return Err(AssistantMemoryError::Internal);
        }
        Ok(Self(url.origin().ascii_serialization()))
    }
}
/// No listener, environment reads, connection attempts, or global mounting.
/// The host owns request-size limits and optional trusted-origin injection.
pub fn assistant_memory_router(
    api: AssistantMemoryApi,
    identity: Arc<dyn AssistantMemoryIdentityResolver>,
) -> Router {
    Router::new()
        .route("/api/v1/assistant/memory", any(handle))
        .route("/api/v1/assistant/memory/items", any(handle))
        .route("/api/v1/assistant/memory/", any(unmatched))
        .route("/api/v1/assistant/memory/{*tail}", any(unmatched))
        .with_state(HttpState { api, identity })
}
async fn unmatched(request: Request) -> Response {
    let uri = request.uri();
    let path = uri.path().trim_end_matches('/');
    if path == uri.path() || !ASSISTANT_MEMORY_ROUTES.iter().any(|(_, p, _)| *p == path) {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    }
    let target = match uri.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_owned(),
    };
    let origin = request
        .extensions()
        .get::<AssistantMemoryTrustedOrigin>()
        .map(|origin| origin.0.as_str())
        .unwrap_or("");
    let location = format!("{origin}{target}");
    (StatusCode::TEMPORARY_REDIRECT, [("location", location)]).into_response()
}
async fn respond(result: Result<Value, AssistantMemoryError>) -> Response {
    match result {
        Ok(value) => (StatusCode::OK, Json(value)).into_response(),
        Err(AssistantMemoryError::Http { status, detail }) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"detail":detail})),
        )
            .into_response(),
        Err(AssistantMemoryError::Storage | AssistantMemoryError::Internal) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        }
        Err(AssistantMemoryError::Cancelled) => std::future::pending().await,
    }
}
async fn handle(State(state): State<HttpState>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method().as_str().to_owned();
    let Some((_, _, operation)) = ASSISTANT_MEMORY_ROUTES
        .iter()
        .find(|(m, p, _)| *m == method && *p == path)
    else {
        // Starlette returns the first partial route match, not a union of methods.
        let allow = ASSISTANT_MEMORY_ROUTES
            .iter()
            .find(|(_, p, _)| *p == path)
            .map(|(m, _, _)| *m)
            .unwrap_or("");
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", allow)],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response();
    };
    let headers = request.headers().clone();
    let query = request.uri().query().unwrap_or("").to_owned();
    let raw = if matches!(*operation, "update_memory_preference" | "put_memory_item") {
        let bytes = match Bytes::from_request(request, &state).await {
            Ok(bytes) => bytes,
            Err(error) => return error.into_response(),
        };
        match parse_body(&bytes, &headers) {
            Ok(raw) => raw,
            Err(error) => return respond(Err(error)).await,
        }
    } else {
        Default::default()
    };
    let result = async {
        let owner = state.identity.resolve(&headers, &path).await?;
        match *operation {
            "get_memory" => state.api.get(&owner).await,
            "update_memory_preference" => {
                state
                    .api
                    .patch(&owner, MemoryPreferenceUpdate::parse(&raw)?)
                    .await
            }
            "put_memory_item" => state.api.put(&owner, MemoryItemUpdate::parse(&raw)?).await,
            "delete_memory_item" => state.api.delete(&owner, &delete_key(&query)?).await,
            _ => Err(AssistantMemoryError::Internal),
        }
    }
    .await;
    respond(result).await
}
