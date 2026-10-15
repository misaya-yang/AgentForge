//! Opt-in seven-route port of `src/api/v1/sessions.py` (generic sessions only).
//!
//! Reuses the canonical native session managers, lifecycle and supervised
//! PostgreSQL adapter. This factory is inert, opens no listener and mounts no
//! production route. The host injects identity and optional runtime cleanup.
//! Guest/disabled-auth identities are valid when produced by the host's trusted
//! resolver; this layer neither reads identity from payloads nor adds RBAC.
//! AdmissionService bridges existing current JWT/database-key authentication.
//!
//! Item ownership failures, including tenant mismatch, are uniformly 404. Only
//! create IDs are UUID-normalized; path IDs remain strings. Empty patches do
//! nothing; metadata/config patches are sequential, not a new combined atomic
//! transaction. The durable manager owns each atomic JSONB merge and cache
//! invalidation; the memory manager merges into its actual stored object. Python
//! memory create rejects a truthy config keyword, so its API adapter returns an
//! internal error rather than silently ignoring that supplied configuration.
//!
//! Backend false returns are ignored by the source route, including cleanup.
//! A host may atomically handle logical deletion and durable cleanup intent; then
//! no second manager delete runs. Durable deletion checks defeat stale caches.
//! Fallback cleanup failure prevents delete; absent cleanup is allowed. The host supplies
//! the real runtime control adapter (not a substitute runtime implementation).
//! Dropped futures stop subsequent steps; committed prior steps are not undone.
//!
//! Native boundaries: JSON values are finite serde JSON numbers/Unicode; arbitrary
//! Python integers, non-finite numbers and lone surrogates are unsupported. The
//! default serde JSON decoder depth limit (128) is retained; deeper content may
//! be accepted by Python but is rejected here before identity resolution.
//! Encoded characters in the static /api/v1/sessions prefix require host URL
//! normalization before routing. Percent decoding inside the matched suffix is
//! performed before selecting a source route. JSON
//! syntax-error wording is parser-specific except the common incomplete-object
//! case. Non-JSON body decoding and malformed URL escape diagnostics are native.
//! Canonical absolute slash redirects require an explicitly trusted public origin;
//! without one, trailing-slash paths return 404. Never trust forwarded host data.
//! Storage/cache cancellation and no-pool behavior retain the existing native
//! collaborator boundaries. No cookie minting, Redis cache or runtime transport
//! is implicitly configured by this module.
use crate::{
    session_manager::{
        CreateSession, DatabaseSessionManager, MemorySessionManager, Session, SessionError,
        SessionFuture, SessionLifecycle, SessionMessage, SessionOwner, SessionSummaryQuery,
    },
    usage_parser::{decimal_digit, python_whitespace},
};
use ai_platform_auth::admission::AdmissionService;
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Query, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

pub type SessionsApiFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SessionsApiError>> + Send + 'a>>;
#[derive(Clone, Debug, PartialEq)]
pub enum SessionsApiError {
    Http { status: u16, detail: Value },
    Internal,
}
impl SessionsApiError {
    pub fn http(status: u16, detail: impl Into<Value>) -> Self {
        Self::Http {
            status,
            detail: detail.into(),
        }
    }
    fn response(self, trace: &str) -> Response {
        match self {
            Self::Http { status, detail } => (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(json!({"detail":detail})),
            )
                .into_response(),
            Self::Internal => {
                let mut body = json!({"error":{"code":2000,"message":"Internal server error"}});
                if !trace.is_empty() {
                    body["error"]["trace_id"] = trace.into();
                }
                (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
            }
        }
    }
}
impl From<SessionError> for SessionsApiError {
    fn from(_: SessionError) -> Self {
        Self::Internal
    }
}
/// Created only by a trusted resolver, never deserialized from request data.
#[derive(Clone)]
pub struct SessionsIdentity {
    user_id: String,
    tenant_id: String,
}
impl SessionsIdentity {
    pub fn from_resolved_identity(user_id: String, tenant_id: String) -> Self {
        Self { user_id, tenant_id }
    }
    pub fn user_id(&self) -> &str {
        &self.user_id
    }
    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }
}
pub trait SessionsIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> SessionsApiFuture<'a, SessionsIdentity>;
}
impl SessionsIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> SessionsApiFuture<'a, SessionsIdentity> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let principal = request
                .resolve()
                .await
                .map_err(|error| SessionsApiError::http(error.status(), error.to_string()))?;
            Ok(SessionsIdentity::from_resolved_identity(
                principal.user_id().into(),
                principal.tenant_id().into(),
            ))
        })
    }
}
/// Thin endpoint seam. Concrete implementations delegate to existing managers;
/// lifecycle append is shared with the gateway invocation path, not duplicated.
pub trait SessionsApiManager: SessionLifecycle {
    fn create(&self, request: CreateSession) -> SessionFuture<'_, Session>;
    fn get<'a>(&'a self, id: &'a str) -> SessionFuture<'a, Option<Session>>;
    /// Compatibility fallback for managers without a small ownership projection.
    fn get_owner<'a>(&'a self, id: &'a str) -> SessionFuture<'a, Option<SessionOwner>> {
        Box::pin(async move { Ok(self.get(id).await?.as_ref().map(SessionOwner::from)) })
    }
    fn list_summaries(&self, query: SessionSummaryQuery) -> SessionFuture<'_, Vec<Value>>;
    fn update_metadata<'a>(
        &'a self,
        id: &'a str,
        patch: Map<String, Value>,
    ) -> SessionFuture<'a, bool>;
    fn update_config<'a>(
        &'a self,
        id: &'a str,
        patch: Map<String, Value>,
    ) -> SessionFuture<'a, bool>;
    fn delete<'a>(&'a self, id: &'a str) -> SessionFuture<'a, bool>;
    fn history<'a>(&'a self, id: &'a str, limit: i64) -> SessionFuture<'a, Vec<SessionMessage>>;
}
impl SessionsApiManager for MemorySessionManager {
    fn create(&self, request: CreateSession) -> SessionFuture<'_, Session> {
        Box::pin(async move {
            if request.config.as_ref().is_some_and(|v| !v.is_empty()) {
                return Err(SessionError::InvalidData(
                    "memory session create does not accept config",
                ));
            }
            MemorySessionManager::create(self, request).await
        })
    }
    fn get<'a>(&'a self, id: &'a str) -> SessionFuture<'a, Option<Session>> {
        Box::pin(MemorySessionManager::get(self, id))
    }
    fn list_summaries(&self, query: SessionSummaryQuery) -> SessionFuture<'_, Vec<Value>> {
        Box::pin(async move { Ok(self.list_session_summaries(query).await) })
    }
    fn update_metadata<'a>(
        &'a self,
        id: &'a str,
        patch: Map<String, Value>,
    ) -> SessionFuture<'a, bool> {
        Box::pin(async move { Ok(MemorySessionManager::update_metadata(self, id, patch).await) })
    }
    fn update_config<'a>(
        &'a self,
        id: &'a str,
        patch: Map<String, Value>,
    ) -> SessionFuture<'a, bool> {
        Box::pin(async move { Ok(MemorySessionManager::update_config(self, id, patch).await) })
    }
    fn delete<'a>(&'a self, id: &'a str) -> SessionFuture<'a, bool> {
        Box::pin(async move {
            MemorySessionManager::delete(self, id).await;
            Ok(true)
        })
    }
    fn history<'a>(&'a self, id: &'a str, limit: i64) -> SessionFuture<'a, Vec<SessionMessage>> {
        Box::pin(async move { Ok(MemorySessionManager::history(self, id, limit).await) })
    }
}
impl SessionsApiManager for DatabaseSessionManager {
    fn create(&self, request: CreateSession) -> SessionFuture<'_, Session> {
        Box::pin(DatabaseSessionManager::create(self, request))
    }
    fn get<'a>(&'a self, id: &'a str) -> SessionFuture<'a, Option<Session>> {
        Box::pin(DatabaseSessionManager::get(self, id))
    }
    fn get_owner<'a>(&'a self, id: &'a str) -> SessionFuture<'a, Option<SessionOwner>> {
        Box::pin(DatabaseSessionManager::get_owner(self, id))
    }
    fn list_summaries(&self, query: SessionSummaryQuery) -> SessionFuture<'_, Vec<Value>> {
        Box::pin(self.list_session_summaries(query))
    }
    fn update_metadata<'a>(
        &'a self,
        id: &'a str,
        patch: Map<String, Value>,
    ) -> SessionFuture<'a, bool> {
        Box::pin(DatabaseSessionManager::update_metadata(self, id, patch))
    }
    fn update_config<'a>(
        &'a self,
        id: &'a str,
        patch: Map<String, Value>,
    ) -> SessionFuture<'a, bool> {
        Box::pin(DatabaseSessionManager::update_config(self, id, patch))
    }
    fn delete<'a>(&'a self, id: &'a str) -> SessionFuture<'a, bool> {
        Box::pin(DatabaseSessionManager::delete(self, id))
    }
    fn history<'a>(&'a self, id: &'a str, limit: i64) -> SessionFuture<'a, Vec<SessionMessage>> {
        Box::pin(DatabaseSessionManager::history(self, id, limit))
    }
}
pub trait SessionsRuntimeCleanup: Send + Sync {
    /// Native durable deletion may atomically tombstone the canonical session and
    /// commit external-cleanup intent. True means do not issue a second delete.
    fn delete_owned_session<'a>(&'a self, _tenant: &'a str, _user: &'a str, _session: &'a str) -> SessionsApiFuture<'a, bool> {
        Box::pin(async { Ok(false) })
    }
    /// Defeat stale per-process session caches after durable logical deletion.
    fn session_deleted<'a>(&'a self, _tenant: &'a str, _user: &'a str, _session: &'a str) -> SessionsApiFuture<'a, bool> {
        Box::pin(async { Ok(false) })
    }

    fn cleanup_session<'a>(
        &'a self,
        tenant_id: &'a str,
        user_id: &'a str,
        session_id: &'a str,
    ) -> SessionsApiFuture<'a, bool>;
}
/// Trace comes from trusted host middleware, not body/query identity claims.
#[derive(Clone, Default)]
pub struct SessionsRequestContext {
    pub trace_id: String,
}
#[derive(Clone)]
pub struct SessionsApi {
    identity: Arc<dyn SessionsIdentityResolver>,
    manager: Arc<dyn SessionsApiManager>,
    cleanup: Option<Arc<dyn SessionsRuntimeCleanup>>,
    redirect_origin: Option<String>,
}
impl SessionsApi {
    pub fn new(
        identity: Arc<dyn SessionsIdentityResolver>,
        manager: Arc<dyn SessionsApiManager>,
        cleanup: Option<Arc<dyn SessionsRuntimeCleanup>>,
    ) -> Self {
        Self {
            identity,
            manager,
            cleanup,
            redirect_origin: None,
        }
    }
    /// Host configuration only. A request Host or forwarding header is not authority.
    pub fn with_redirect_origin(mut self, origin: &str) -> Result<Self, SessionsApiError> {
        let parsed = reqwest::Url::parse(origin).map_err(|_| SessionsApiError::Internal)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(SessionsApiError::Internal);
        }
        self.redirect_origin = Some(parsed.as_str().trim_end_matches('/').into());
        Ok(self)
    }
    pub fn router(self) -> Router {
        Router::new()
            .route("/api/v1/sessions", any(dispatch))
            .route("/api/v1/sessions/", any(dispatch))
            .route("/api/v1/sessions/{*rest}", any(dispatch))
            .with_state(self)
    }
    async fn owned(&self, id: &str, user: &SessionsIdentity) -> Result<Session, SessionsApiError> {
        let session = self
            .manager
            .get(id)
            .await?
            .ok_or_else(|| SessionsApiError::http(404, "session not found"))?;
        if session.user_id != user.user_id || session.tenant_id != user.tenant_id {
            return Err(SessionsApiError::http(404, "session not found"));
        }
        if let Some(cleanup) = &self.cleanup {
            if cleanup.session_deleted(&user.tenant_id, &user.user_id, id).await? {
                return Err(SessionsApiError::http(404, "session not found"));
            }
        }
        Ok(session)
    }
    async fn owned_history(&self, id: &str, user: &SessionsIdentity) -> Result<(), SessionsApiError> {
        let owner = self
            .manager
            .get_owner(id)
            .await?
            .ok_or_else(|| SessionsApiError::http(404, "session not found"))?;
        if owner.user_id != user.user_id || owner.tenant_id != user.tenant_id {
            return Err(SessionsApiError::http(404, "session not found"));
        }
        if let Some(cleanup) = &self.cleanup {
            if cleanup.session_deleted(&user.tenant_id, &user.user_id, id).await? {
                return Err(SessionsApiError::http(404, "session not found"));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Route {
    Collection,
    Item,
    Messages,
    History,
}
fn path_route(path: &str) -> Option<(Route, String)> {
    let path = decode_path(path)?;
    if path == "/api/v1/sessions" {
        return Some((Route::Collection, String::new()));
    }
    let parts = path
        .strip_prefix("/api/v1/sessions/")?
        .split('/')
        .collect::<Vec<_>>();
    if parts[0].is_empty() {
        return None;
    }
    match parts.as_slice() {
        [id] => Some((Route::Item, (*id).into())),
        [id, "messages"] => Some((Route::Messages, (*id).into())),
        [id, "history"] => Some((Route::History, (*id).into())),
        _ => None,
    }
}
fn decode_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut result = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let high = (bytes[i + 1] as char).to_digit(16);
            let low = (bytes[i + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                result.push((high * 16 + low) as u8);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(result).ok()
}
fn method_allowed(route: Route, method: &Method) -> bool {
    match route {
        Route::Collection => method == Method::GET || method == Method::POST,
        Route::Item => method == Method::GET || method == Method::PATCH || method == Method::DELETE,
        Route::Messages => method == Method::POST,
        Route::History => method == Method::GET,
    }
}
async fn dispatch(State(api): State<SessionsApi>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path();
    let Some((route, id)) = path_route(path) else {
        if path.ends_with('/')
            && path_route(path.trim_end_matches('/')).is_some()
            && let Some(origin) = &api.redirect_origin
        {
            let suffix = parts
                .uri
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default();
            return (
                StatusCode::TEMPORARY_REDIRECT,
                [(
                    header::LOCATION,
                    format!("{origin}{}{suffix}", path.trim_end_matches('/')),
                )],
            )
                .into_response();
        }
        return SessionsApiError::http(404, "Not Found").response("");
    };
    if !method_allowed(route, &parts.method) {
        let mut response = SessionsApiError::http(405, "Method Not Allowed").response("");
        response.headers_mut().insert(
            header::ALLOW,
            if route == Route::Messages {
                "POST"
            } else {
                "GET"
            }
            .parse()
            .expect("static method"),
        );
        // Axum sets Content-Length before suppressing the HEAD representation.
        return response;
    }
    // ExceptionHandler._get_trace_id checks state attribute presence first.
    // An explicitly empty trusted value suppresses all header fallback.
    // Starlette decodes HTTP header bytes as Latin-1, not UTF-8.
    let trace_id = if let Some(context) = parts.extensions.get::<SessionsRequestContext>() {
        context.trace_id.clone()
    } else {
        ["x-trace-id", "x-request-id"]
            .into_iter()
            .find_map(|name| {
                parts
                    .headers
                    .get(name)
                    .filter(|value| !value.as_bytes().is_empty())
                    .map(|value| {
                        value
                            .as_bytes()
                            .iter()
                            .copied()
                            .map(char::from)
                            .collect::<String>()
                    })
            })
            .unwrap_or_default()
    };
    let trace = trace_id.as_str();
    let needs_body = parts.method == Method::POST || parts.method == Method::PATCH;
    // Canonical FastAPI strict_content_type defaults to true: missing/empty
    // Content-Type keeps raw bytes and does not trigger JSON syntax validation.
    let json_content_type = crate::http_content_type::is_json_content_type(&parts.headers);
    let value = if needs_body {
        let bytes = match to_bytes(body, usize::MAX).await {
            Ok(bytes) => bytes,
            Err(_) => {
                return SessionsApiError::http(400, "There was an error parsing the body")
                    .response(trace);
            }
        };
        if bytes.is_empty() {
            Value::Null
        } else if !json_content_type {
            match String::from_utf8(bytes.to_vec()) {
                Ok(value) => Value::String(value),
                Err(_) => return SessionsApiError::Internal.response(trace),
            }
        } else {
            match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) => value,
                Err(error) => {
                    let message = if bytes.as_ref() == b"{" {
                        "Expecting property name enclosed in double quotes".into()
                    } else {
                        format!("{}", error)
                    };
                    let offset = if bytes.as_ref() == b"{" {
                        1
                    } else {
                        error.column().saturating_sub(1)
                    };
                    return SessionsApiError::http(422,json!([{"type":"json_invalid","loc":["body",offset],"msg":"JSON decode error","input":{},"ctx":{"error":message}}])).response(trace);
                }
            }
        }
    } else {
        Value::Null
    };
    let user = match api.identity.resolve(&parts.headers, path).await {
        Ok(user) => user,
        Err(error) => return error.response(trace),
    };
    let query = match Query::<HashMap<String, String>>::try_from_uri(&parts.uri) {
        Ok(Query(query)) => query,
        Err(_) => return SessionsApiError::http(422, "Invalid query parameters").response(trace),
    };
    let result = handle(&api, route, &id, &parts.method, &user, &query, value).await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.response(trace),
    }
}
async fn handle(
    api: &SessionsApi,
    route: Route,
    id: &str,
    method: &Method,
    user: &SessionsIdentity,
    query: &HashMap<String, String>,
    value: Value,
) -> Result<Value, SessionsApiError> {
    if route == Route::Collection && method == Method::GET {
        let limit = query_limit(query, 100)?;
        let service = query.get("service_id").filter(|v| !v.is_empty());
        let (service_ids, include_null_service_id) = match service.map(String::as_str) {
            Some("__builtin_assistant__") => (
                vec!["__builtin_assistant__".into(), "assistant".into()],
                true,
            ),
            Some(s) => (vec![s.into()], false),
            None => (vec![], false),
        };
        let rows = api
            .manager
            .list_summaries(SessionSummaryQuery {
                user_id: Some(user.user_id.clone()),
                tenant_id: Some(user.tenant_id.clone()),
                service_ids,
                include_null_service_id,
                limit,
                ..Default::default()
            })
            .await?;
        return Ok(Value::Array(
            rows.iter()
                .map(|row| {
                    project(
                        row,
                        &[
                            "session_id",
                            "user_id",
                            "tenant_id",
                            "service_id",
                            "created_at",
                            "updated_at",
                            "metadata",
                            "config",
                        ],
                    )
                })
                .collect(),
        ));
    }
    if route == Route::Collection {
        let object = validate_body(value, BodyKind::Create)?;
        let session_id = object
            .get("session_id")
            .and_then(Value::as_str)
            .map(normalize_uuid)
            .transpose()?
            .flatten();
        if let (Some(id), Some(cleanup)) = (session_id.as_deref(), api.cleanup.as_ref()) {
            if cleanup.session_deleted(&user.tenant_id, &user.user_id, id).await? {
                return Err(SessionsApiError::http(409, "session already exists"));
            }
        }
        let request = CreateSession {
            user_id: user.user_id.clone(),
            tenant_id: user.tenant_id.clone(),
            service_id: object
                .get("service_id")
                .and_then(Value::as_str)
                .map(Into::into),
            fail_if_exists: session_id.is_some(),
            session_id,
            metadata: map_value(&object, "metadata"),
            config: map_value(&object, "config").filter(|v| !v.is_empty()),
            ..Default::default()
        };
        let session = api.manager.create(request).await.map_err(|error| {
            if error == SessionError::AlreadyExists {
                SessionsApiError::http(409, "session already exists")
            } else {
                SessionsApiError::Internal
            }
        })?;
        return Ok(json!({"session_id":session.session_id}));
    }
    // FastAPI schema/query validation happens before entering the endpoint and
    // before its get/ownership check, but after identity dependencies resolve.
    let object = if method == Method::PATCH {
        Some(validate_body(value, BodyKind::Update)?)
    } else if route == Route::Messages {
        Some(validate_body(value, BodyKind::Message)?)
    } else {
        None
    };
    let limit = if route == Route::History {
        query_limit(query, 50)?
    } else {
        0
    };
    if route == Route::History {
        api.owned_history(id, user).await?;
        let history = api.manager.history(id, limit).await?;
        return Ok(Value::Array(history.into_iter().map(|message|json!({"role":message.role,"content":message.content,"timestamp":message.timestamp,"metadata":message.metadata.filter(|v|!v.is_empty())})).collect()));
    }
    let session = api.owned(id, user).await?;
    match (route, method.as_str()) {
        (Route::Item, "GET") => Ok(project(
            &serde_json::to_value(session).map_err(|_| SessionsApiError::Internal)?,
            &[
                "session_id",
                "service_id",
                "user_id",
                "tenant_id",
                "created_at",
                "updated_at",
                "metadata",
                "config",
            ],
        )),
        (Route::Item, "PATCH") => {
            let object = object.expect("validated update");
            if let Some(metadata) = map_value(&object, "metadata").filter(|v| !v.is_empty()) {
                api.manager.update_metadata(id, metadata).await?;
            }
            if let Some(config) = map_value(&object, "config").filter(|v| !v.is_empty()) {
                api.manager.update_config(id, config).await?;
            }
            Ok(json!({"session_id":id,"status":"updated"}))
        }
        (Route::Item, "DELETE") => {
            if let Some(cleanup) = &api.cleanup {
                if cleanup.delete_owned_session(&user.tenant_id, &user.user_id, id).await? {
                    return Ok(json!({"session_id":id,"status":"deleted"}));
                }
                cleanup
                    .cleanup_session(&user.tenant_id, &user.user_id, id)
                    .await?;
            }
            api.manager.delete(id).await?;
            Ok(json!({"session_id":id,"status":"deleted"}))
        }
        (Route::Messages, "POST") => {
            let object = object.expect("validated message");
            let role = object["role"].as_str().expect("validated string");
            api.manager
                .add_message(
                    id,
                    role,
                    object["content"].clone(),
                    map_value(&object, "metadata"),
                )
                .await?;
            Ok(json!({"session_id":id,"status":"added"}))
        }
        _ => Err(SessionsApiError::Internal),
    }
}
fn project(value: &Value, fields: &[&str]) -> Value {
    Value::Object(
        fields
            .iter()
            .map(|key| {
                (
                    (*key).into(),
                    value.get(key).cloned().unwrap_or(Value::Null),
                )
            })
            .collect(),
    )
}
fn map_value(value: &Map<String, Value>, key: &str) -> Option<Map<String, Value>> {
    value.get(key).and_then(Value::as_object).cloned()
}
fn validation(loc: Value, input: Value, kind: &str, msg: &str, ctx: Option<Value>) -> Value {
    let mut value = json!({"type":kind,"loc":loc,"msg":msg,"input":input});
    if let Some(ctx) = ctx {
        value["ctx"] = ctx;
    }
    value
}
#[derive(Clone, Copy, PartialEq)]
enum BodyKind {
    Create,
    Update,
    Message,
}
fn validate_body(mut value: Value, kind: BodyKind) -> Result<Map<String, Value>, SessionsApiError> {
    if value.is_null() && kind == BodyKind::Create {
        value = json!({});
    }
    let Some(object) = value.as_object() else {
        let (kind, msg) = if value.is_null() {
            ("missing", "Field required")
        } else {
            (
                "model_attributes_type",
                "Input should be a valid dictionary or object to extract fields from",
            )
        };
        return Err(SessionsApiError::http(
            422,
            json!([validation(json!(["body"]), value, kind, msg, None)]),
        ));
    };
    let fields: &[(&str, bool, bool)] = match kind {
        BodyKind::Create => &[
            ("session_id", true, false),
            ("service_id", true, false),
            ("metadata", false, false),
            ("config", false, false),
        ],
        BodyKind::Update => &[("metadata", false, false), ("config", false, false)],
        BodyKind::Message => &[
            ("role", true, true),
            ("content", false, true),
            ("metadata", false, false),
        ],
    };
    let mut errors = vec![];
    for (field, string, required) in fields {
        let loc = json!(["body", field]);
        let Some(input) = object.get(*field) else {
            if *required {
                errors.push(validation(
                    loc,
                    value.clone(),
                    "missing",
                    "Field required",
                    None,
                ));
            }
            continue;
        };
        if *field == "content" {
            continue;
        }
        if input.is_null() && !required {
            continue;
        }
        if *string && !input.is_string() {
            errors.push(validation(
                loc,
                input.clone(),
                "string_type",
                "Input should be a valid string",
                None,
            ));
        } else if !string && !input.is_object() {
            errors.push(validation(
                loc,
                input.clone(),
                "dict_type",
                "Input should be a valid dictionary",
                None,
            ));
        } else if *field == "session_id"
            && let Some(text) = input.as_str()
            && let Err(SessionsApiError::Http { detail, .. }) = normalize_uuid(text)
        {
            errors.extend(detail.as_array().expect("validation list").iter().cloned());
        }
    }
    if errors.is_empty() {
        Ok(object.clone())
    } else {
        Err(SessionsApiError::http(422, Value::Array(errors)))
    }
}
fn normalize_uuid(text: &str) -> Result<Option<String>, SessionsApiError> {
    let trimmed = text.trim_matches(python_whitespace);
    if trimmed.is_empty() {
        return Ok(None);
    }
    // uuid.UUID(hex=...) replaces these strings, strips any edge braces and
    // removes hyphens, including unusual but accepted non-canonical spellings.
    let hex = trimmed
        .replace("urn:", "")
        .replace("uuid:", "")
        .trim_matches(['{', '}'])
        .replace('-', "");
    let error = if hex.chars().count() != 32 {
        Some("badly formed hexadecimal UUID string".into())
    } else {
        None
    };
    let normalized: String = hex
        .chars()
        .map(|c| decimal_digit(c).map(|d| char::from(b'0' + d)).unwrap_or(c))
        .collect();
    let integer = normalized.trim();
    let integer = integer.strip_prefix('+').unwrap_or(integer);
    let (integer, prefix) = integer
        .strip_prefix("0x")
        .or_else(|| integer.strip_prefix("0X"))
        .map_or((integer, false), |v| (v, true));
    let integer = if prefix {
        integer.strip_prefix('_').unwrap_or(integer)
    } else {
        integer
    };
    let bytes = integer.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.iter().enumerate().all(|(i, b)| {
            b.is_ascii_hexdigit()
                || (*b == b'_'
                    && i > 0
                    && bytes[i - 1].is_ascii_hexdigit()
                    && bytes.get(i + 1).is_some_and(u8::is_ascii_hexdigit))
        });
    let parsed = if error.is_none() && valid {
        u128::from_str_radix(&integer.replace('_', ""), 16).ok()
    } else {
        None
    };
    if let Some(number) = parsed {
        return Ok(Some(uuid::Uuid::from_u128(number).to_string()));
    }
    let error = error.unwrap_or_else(|| {
        format!(
            "invalid literal for int() with base 16: {}",
            crate::usage_parser::python_value_to_string(&json!([hex]))
                .trim_start_matches('[')
                .trim_end_matches(']')
        )
    });
    Err(SessionsApiError::http(
        422,
        json!([validation(
            json!(["body", "session_id"]),
            json!(text),
            "value_error",
            &format!("Value error, {error}"),
            Some(json!({"error":{}}))
        )]),
    ))
}
fn query_limit(query: &HashMap<String, String>, default: i64) -> Result<i64, SessionsApiError> {
    let Some(input) = query.get("limit") else {
        return Ok(default);
    };
    let value = input.trim();
    let (negative, digits) = if let Some(v) = value.strip_prefix('-') {
        (true, v)
    } else {
        (false, value.strip_prefix('+').unwrap_or(value))
    };
    let (integer, fraction) = digits
        .split_once('.')
        .map_or((digits, None), |(a, b)| (a, Some(b)));
    let bytes = integer.as_bytes();
    let mut number = 0i64;
    let valid = !bytes.is_empty()
        && fraction.is_none_or(|v| !v.is_empty() && v.bytes().all(|b| b == b'0'))
        && bytes.iter().enumerate().all(|(i, b)| {
            if *b == b'_' {
                i > 0
                    && bytes[i - 1].is_ascii_digit()
                    && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
            } else if b.is_ascii_digit() {
                number = number.saturating_mul(10).saturating_add((b - b'0') as i64);
                true
            } else {
                false
            }
        });
    let (kind, msg, ctx) = if !valid {
        (
            "int_parsing",
            "Input should be a valid integer, unable to parse string as an integer",
            None,
        )
    } else if negative || number < 1 {
        (
            "greater_than_equal",
            "Input should be greater than or equal to 1",
            Some(json!({"ge":1})),
        )
    } else if number > 500 {
        (
            "less_than_equal",
            "Input should be less than or equal to 500",
            Some(json!({"le":500})),
        )
    } else {
        return Ok(number);
    };
    Err(SessionsApiError::http(
        422,
        json!([validation(
            json!(["query", "limit"]),
            json!(input),
            kind,
            msg,
            ctx
        )]),
    ))
}

#[cfg(test)]
#[path = "sessions_api_tests.rs"]
mod tests;
