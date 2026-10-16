//! Authenticated task route factory from `src/api/v1/tasks.py`.
//!
//! This factory opens no listener and is not mounted by the default gateway.
//! JWT admission uses the same current PostgreSQL account/tenant/role authority
//! and Redis revocation verifier as the existing gateway routes. API-key,
//! anonymous-cookie and disabled-auth resolver modes remain pending; no public
//! identity header, query parameter, body or unverified JWT claim grants access.
//! Exact `admin` grants same-tenant task administration only. There is no generic
//! task cross-tenant exception, including for `platform_admin`; explicit global
//! capabilities in other APIs are separate. Roles come from verified authority.
//! Listing remains owner-and-tenant scoped even for administrators. Cancellation
//! is a conditional terminal transition; the worker observes it for best-effort
//! local abort. This neither dequeues work nor proves remote effects stopped.
//! Cancelling a completed or failed task returns 409 and preserves its result.
use crate::native_contract::invocation_tasks as contract;
use crate::{
    protocol::{ProtocolTimestamp, UnifiedResponse},
    task_manager::{Task, TaskError, TaskListFilter, TaskManager, TaskResult, TaskStatus},
};
use ai_platform_auth::service::{AuthService, Principal};
use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone)]
struct TaskState {
    auth: Arc<AuthService>,
    manager: Arc<TaskManager>,
    identity: Option<Arc<dyn crate::invocation_http::InvocationIdentityResolver>>,
}

pub fn router(auth: Arc<AuthService>, manager: Arc<TaskManager>) -> Router {
    router_with_identity(auth, manager, None)
}
pub fn router_with_identity(
    auth: Arc<AuthService>,
    manager: Arc<TaskManager>,
    identity: Option<Arc<dyn crate::invocation_http::InvocationIdentityResolver>>,
) -> Router {
    Router::new()
        .route(contract::TASKS, get(list_tasks))
        .route(contract::TASK, get(get_task).delete(cancel_task))
        .route(contract::RESULT, get(get_task_result))
        .with_state(TaskState {
            auth,
            manager,
            identity,
        })
}

/// Explicit public projection; internal request bodies, callback bookkeeping,
/// priorities, retries and metadata must never escape via task serialization.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct TaskSchema {
    pub task_id: String,
    pub request_id: String,
    pub service_id: String,
    pub status: TaskStatus,
    #[serde(serialize_with = "serialize_public_task_timestamp")]
    pub created_at: ProtocolTimestamp,
    #[serde(serialize_with = "serialize_optional_public_task_timestamp")]
    pub updated_at: Option<ProtocolTimestamp>,
    pub user_id: String,
    pub tenant_id: String,
    pub callback_url: Option<String>,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub progress: f64,
}
impl TaskSchema {
    pub fn from_domain(task: &Task) -> Result<Self, TaskError> {
        let result = match &task.result {
            None => None,
            Some(TaskResult::Json(value)) => Some(value.clone()),
            // Keep task list/get aligned with /result for binary outputs.
            Some(TaskResult::Response(response)) => Some(response_value(response)),
        };
        Ok(Self {
            task_id: task.task_id.clone(),
            request_id: task.request_id.clone(),
            service_id: task.service_id.clone(),
            status: task.status,
            created_at: task.created_at,
            updated_at: task.updated_at,
            user_id: task.user_id.clone(),
            tenant_id: task.tenant_id.clone(),
            callback_url: task.callback_url.clone(),
            result,
            error: task.error.clone(),
            progress: task.progress,
        })
    }
}

fn public_task_timestamp(value: ProtocolTimestamp) -> String {
    let text = value.to_isoformat();
    match text.strip_suffix("+00:00") {
        Some(utc) => format!("{utc}Z"),
        None => text,
    }
}
fn serialize_public_task_timestamp<S: serde::Serializer>(
    value: &ProtocolTimestamp,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&public_task_timestamp(*value))
}
fn serialize_optional_public_task_timestamp<S: serde::Serializer>(
    value: &Option<ProtocolTimestamp>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    value.map(public_task_timestamp).serialize(serializer)
}

fn pydantic_response_timestamp(value: &mut Value) {
    // Pydantic's JSON datetime encoder uses Z for aware UTC. JSON-shaped stored
    // task results are left untouched because their timestamp is only a string.
    if let Some(timestamp) = value["timestamp"]
        .as_str()
        .and_then(|time| time.strip_suffix("+00:00"))
    {
        value["timestamp"] = format!("{timestamp}Z").into();
    }
}
fn response_value(response: &UnifiedResponse) -> Value {
    let mut value = response.to_wire_value();
    pydantic_response_timestamp(&mut value);
    value
}

fn error(status: StatusCode, detail: impl Into<String>) -> Response {
    (status, Json(json!({"detail": detail.into()}))).into_response()
}
async fn authenticate(
    state: &TaskState,
    headers: &HeaderMap,
) -> Result<crate::invocation_http::InvocationIdentity, Box<Response>> {
    crate::invocation_http::resolve_identity(
        &state.auth,
        state.identity.as_deref(),
        headers,
        "/api/v1/tasks",
    )
    .await
}
trait TaskIdentity: Sync {
    fn user_id(&self) -> &str;
    fn tenant_id(&self) -> &str;
    fn roles(&self) -> &[String];
}
impl TaskIdentity for Principal {
    fn user_id(&self) -> &str {
        &self.account.user_id
    }
    fn tenant_id(&self) -> &str {
        &self.account.tenant_id
    }
    fn roles(&self) -> &[String] {
        &self.authority.roles
    }
}
impl TaskIdentity for crate::invocation_http::InvocationIdentity {
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn tenant_id(&self) -> &str {
        &self.tenant_id
    }
    fn roles(&self) -> &[String] {
        &self.roles
    }
}
fn visible(task: &Task, principal: &impl TaskIdentity) -> bool {
    !task.tenant_id.is_empty()
        && task.tenant_id == principal.tenant_id()
        && (task.user_id == principal.user_id()
            || principal.roles().iter().any(|role| role == "admin"))
}
fn internal_error(headers: &HeaderMap) -> Response {
    let mut value = json!({"error": {"code": 2000, "message": "Internal server error"}});
    if let Some(trace) = ["x-trace-id", "x-request-id"].into_iter().find_map(|name| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
    }) {
        value["error"]["trace_id"] = trace.into();
    }
    (StatusCode::INTERNAL_SERVER_ERROR, Json(value)).into_response()
}
fn manager_error(err: TaskError, headers: &HeaderMap) -> Response {
    match err {
        TaskError::NotFound(_) => error(StatusCode::NOT_FOUND, err.to_string()),
        TaskError::StateConflict => error(StatusCode::CONFLICT, err.to_string()),
        _ => internal_error(headers),
    }
}
async fn owned_task(
    manager: &TaskManager,
    principal: &impl TaskIdentity,
    task_id: &str,
    headers: &HeaderMap,
) -> Result<Task, Box<Response>> {
    let task = manager
        .get_task(task_id)
        .await
        .map_err(|err| Box::new(manager_error(err, headers)))?;
    if !visible(&task, principal) {
        return Err(Box::new(error(StatusCode::NOT_FOUND, "task not found")));
    }
    Ok(task)
}

fn validation_error(field: &str, input: &str, kind: &str, msg: &str, ctx: Option<Value>) -> Value {
    let mut value = json!({"type": kind, "loc": ["query", field], "msg": msg, "input": input});
    if let Some(ctx) = ctx {
        value["ctx"] = ctx;
    }
    value
}

/// Pydantic's non-strict query integers allow surrounding whitespace, a sign,
/// digit separators and a zero-only fractional suffix. Oversized positive
/// offsets saturate at usize::MAX (an empty page) instead of inventing a cap.
fn query_integer(value: &str) -> Option<(bool, usize)> {
    let raw = value.trim();
    let (negative, raw) = if let Some(raw) = raw.strip_prefix('-') {
        (true, raw)
    } else {
        (false, raw.strip_prefix('+').unwrap_or(raw))
    };
    let (digits, fraction) = raw
        .split_once('.')
        .map_or((raw, None), |(a, b)| (a, Some(b)));
    if digits.is_empty()
        || fraction
            .is_some_and(|fraction| fraction.is_empty() || !fraction.bytes().all(|b| b == b'0'))
    {
        return None;
    }
    let bytes = digits.as_bytes();
    let mut number = 0usize;
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte == b'_' {
            if index == 0
                || !bytes[index - 1].is_ascii_digit()
                || !bytes.get(index + 1).is_some_and(u8::is_ascii_digit)
            {
                return None;
            }
        } else if byte.is_ascii_digit() {
            number = number
                .saturating_mul(10)
                .saturating_add((byte - b'0') as usize);
        } else {
            return None;
        }
    }
    Some((negative && number != 0, number))
}
fn list_filter(query: &HashMap<String, String>) -> Result<TaskListFilter, Vec<Value>> {
    let mut errors = Vec::new();
    let status = query.get("status").cloned();
    const PATTERN: &str = "^(pending|processing|completed|failed|cancelled)$";
    if let Some(status) = &status
        && !matches!(
            status.as_str(),
            "pending" | "processing" | "completed" | "failed" | "cancelled"
        )
    {
        errors.push(validation_error(
            "status",
            status,
            "string_pattern_mismatch",
            &format!("String should match pattern '{PATTERN}'"),
            Some(json!({"pattern": PATTERN})),
        ));
    }
    let mut filter = TaskListFilter {
        status,
        limit: 50,
        offset: 0,
        ..Default::default()
    };
    for (field, target, minimum, maximum) in [
        ("limit", &mut filter.limit, 1, Some(100)),
        ("offset", &mut filter.offset, 0, None),
    ] {
        let Some(raw) = query.get(field) else {
            continue;
        };
        match query_integer(raw) {
            None => errors.push(validation_error(
                field,
                raw,
                "int_parsing",
                "Input should be a valid integer, unable to parse string as an integer",
                None,
            )),
            Some((negative, number)) if negative || number < minimum => {
                errors.push(validation_error(
                    field,
                    raw,
                    "greater_than_equal",
                    &format!("Input should be greater than or equal to {minimum}"),
                    Some(json!({"ge": minimum})),
                ));
            }
            Some((_, number)) if maximum.is_some_and(|max| number > max) => {
                errors.push(validation_error(
                    field,
                    raw,
                    "less_than_equal",
                    "Input should be less than or equal to 100",
                    Some(json!({"le": 100})),
                ));
            }
            Some((_, number)) => *target = number,
        }
    }
    if errors.is_empty() {
        Ok(filter)
    } else {
        Err(errors)
    }
}

async fn list_tasks(
    State(state): State<TaskState>,
    headers: HeaderMap,
    query: Result<Query<HashMap<String, String>>, QueryRejection>,
) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(response) => return *response,
    };
    let mut filter = match query {
        Ok(Query(query)) => match list_filter(&query) {
            Ok(filter) => filter,
            Err(errors) => {
                return (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    Json(json!({"detail": errors})),
                )
                    .into_response();
            }
        },
        Err(_) => return error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid query parameters"),
    };
    filter.user_id = Some(principal.user_id().to_owned());
    filter.tenant_id = Some(principal.tenant_id().to_owned());
    match state.manager.list_tasks(&filter).await {
        Ok(tasks) => match tasks
            .iter()
            .map(TaskSchema::from_domain)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(tasks) => Json(tasks).into_response(),
            Err(err) => manager_error(err, &headers),
        },
        Err(err) => manager_error(err, &headers),
    }
}
async fn get_task(
    State(state): State<TaskState>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(response) => return *response,
    };
    get_task_for_principal(&state.manager, &principal, &task_id, &headers).await
}
async fn get_task_for_principal(
    manager: &TaskManager,
    principal: &impl TaskIdentity,
    task_id: &str,
    headers: &HeaderMap,
) -> Response {
    match owned_task(manager, principal, task_id, headers).await {
        Ok(task) => match TaskSchema::from_domain(&task) {
            Ok(task) => Json(task).into_response(),
            Err(err) => manager_error(err, headers),
        },
        Err(response) => *response,
    }
}
async fn cancel_task(
    State(state): State<TaskState>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(response) => return *response,
    };
    cancel_task_for_principal(&state.manager, &principal, &task_id, &headers).await
}
async fn cancel_task_for_principal(
    manager: &TaskManager,
    principal: &impl TaskIdentity,
    task_id: &str,
    headers: &HeaderMap,
) -> Response {
    let authorized = match owned_task(manager, principal, task_id, headers).await {
        Ok(task) => task,
        Err(response) => return *response,
    };
    match manager.cancel_expected(task_id, &authorized).await {
        Ok(()) => Json(json!({"task_id": task_id, "status": "cancelled"})).into_response(),
        Err(err) => manager_error(err, headers),
    }
}
async fn get_task_result(
    State(state): State<TaskState>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(principal) => principal,
        Err(response) => return *response,
    };
    get_task_result_for_principal(&state.manager, &principal, &task_id, &headers).await
}
async fn get_task_result_for_principal(
    manager: &TaskManager,
    principal: &impl TaskIdentity,
    task_id: &str,
    headers: &HeaderMap,
) -> Response {
    if let Err(response) = owned_task(manager, principal, task_id, headers).await {
        return *response;
    }
    match manager.get_result(task_id).await {
        // The existing protocol projection is UnifiedResponseSchema.from_domain:
        // only this explicit output projection base64-encodes binary content.
        Ok(response) => Json(response_value(&response)).into_response(),
        Err(err) => manager_error(err, headers),
    }
}

#[cfg(test)]
mod task_schema_tests {
    use super::*;
    use crate::protocol::{ContentData, ContentItem, UnifiedRequest};
    #[test]
    fn non_utf8_binary_task_projection_matches_result_endpoint() {
        let timestamp = ProtocolTimestamp::parse_isoformat("2026-10-12T09:00:00.123456Z").unwrap();
        let request = UnifiedRequest::new("request", "service", vec![]);
        let mut task = Task::new("task".into(), &request, timestamp.naive_utc());
        let mut binary = ContentItem::text("binary");
        binary.data = Some(ContentData::Bytes(vec![255, 0, 1]));
        let mut response = UnifiedResponse::new(
            "request",
            "completed",
            vec![binary, ContentItem::text("text")],
        );
        response.timestamp = timestamp;
        task.status = TaskStatus::Completed;
        task.result = Some(response.clone().into());
        let schema = TaskSchema::from_domain(&task).unwrap();
        let result = schema.result.unwrap();
        assert_eq!(result, response_value(&response));
        assert_eq!(result["outputs"][0]["data"], "/wAB");
        assert_eq!(result["outputs"][1]["data"], "text");
        assert_eq!(result["timestamp"], "2026-10-12T09:00:00.123456Z");
    }
    #[test]
    fn legacy_json_task_result_is_not_reencoded() {
        let request = UnifiedRequest::new("request", "service", vec![]);
        let mut task = Task::new(
            "task".into(),
            &request,
            ProtocolTimestamp::now().naive_utc(),
        );
        let raw = json!({"request_id":"legacy","outputs":[{"type":"text","data":[255,0,1]}],"timestamp":"kept verbatim"});
        task.result = Some(raw.clone().into());
        assert_eq!(TaskSchema::from_domain(&task).unwrap().result, Some(raw));
    }
}

#[cfg(test)]
#[path = "task_http_authorization_tests.rs"]
mod task_authorization_tests;

#[cfg(test)]
mod canonical_context_tests {
    use super::*;
    fn identity(
        user: &str,
        tenant: &str,
        roles: &[&str],
    ) -> crate::invocation_http::InvocationIdentity {
        crate::invocation_http::InvocationIdentity {
            user_id: user.into(),
            tenant_id: tenant.into(),
            is_authenticated: true,
            tier: "public".into(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            permissions: vec![],
            allowed_models: None,
            session_principal: None,
            session_jwt_selected: false,
        }
    }
    #[test]
    fn api_key_and_guest_contexts_keep_exact_task_owner_scope() {
        let mut request = crate::protocol::UnifiedRequest::new("request", "service", vec![]);
        request.user_id = "key-owner".into();
        request.tenant_id = "tenant".into();
        let task = Task::new(
            "task".into(),
            &request,
            crate::protocol::ProtocolTimestamp::now().naive_utc(),
        );
        assert!(visible(&task, &identity("key-owner", "tenant", &["user"])));
        assert!(!visible(&task, &identity("other", "tenant", &["user"])));
        assert!(visible(&task, &identity("other", "tenant", &["admin"])));
        assert!(!visible(
            &task,
            &identity("key-owner", "foreign", &["admin"])
        ));
        assert!(!visible(&task, &identity("key-owner", "", &["admin"])));
    }
    #[test]
    fn non_session_identity_does_not_create_session_authority() {
        let user = identity("anon:cookie", "public", &["guest"]);
        assert!(user.session_principal.is_none());
        assert!(!user.session_jwt_selected);
    }
}
