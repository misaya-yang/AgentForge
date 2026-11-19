//! Durable in-app outcome receipts. Listing/opening does not acknowledge a receipt.
//! Runtime and scheduler terminal state remain authoritative; no delivery is sent.
mod store;

use super::{Actor, AgentError, routes::AgentState};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Query, Request, State},
    http::header,
    response::{IntoResponse, Response},
    routing::any,
};
use chrono::{DateTime, Utc};
use ai_platform_gateway_foundation::native_contract::{NOTIFICATION_BASE as BASE, MAX_PAGE, Filter, ListQuery, ReadBatch};
use serde_json::{Value, json};
use std::collections::HashSet;

struct Page {
    unread_only: bool,
    limit: usize,
    before_at: Option<DateTime<Utc>>,
    before_id: Option<String>,
}

trait ValidateListQuery { fn validate(self) -> Result<Page, AgentError>; }
impl ValidateListQuery for ListQuery {
    fn validate(self) -> Result<Page, AgentError> {
        let limit = self.limit.unwrap_or(50);
        if !(1..=MAX_PAGE).contains(&limit) || self.before_at.is_some() != self.before_id.is_some() {
            return Err(invalid());
        }
        let before_id = self.before_id.as_deref().map(super::uuid).transpose()?;
        Ok(Page {
            unread_only: matches!(self.filter, Filter::Unread),
            limit,
            before_at: self.before_at,
            before_id,
        })
    }
}

fn invalid() -> AgentError {
    AgentError::code(422, "SCHEDULE_NOTIFICATION_INVALID")
}

fn normalize_ids(ids: Vec<String>) -> Result<Vec<String>, AgentError> {
    if ids.is_empty() || ids.len() > MAX_PAGE {
        return Err(invalid());
    }
    let mut unique = HashSet::new();
    for id in ids {
        if !unique.insert(super::uuid(&id)?) {
            return Err(invalid());
        }
    }
    let mut ids: Vec<String> = unique.into_iter().collect();
    // A consistent lock order avoids reversed-batch acknowledgement deadlocks.
    ids.sort();
    Ok(ids)
}

pub fn router(state: AgentState) -> Router {
    Router::new()
        .route(BASE, any(handle))
        .route("/api/v1/scheduled-task-notifications/{*rest}", any(handle))
        .with_state(state)
}

async fn handle(State(state): State<AgentState>, request: Request) -> Response {
    let mut response = match dispatch(&state, request).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    };
    // A shared browser/proxy must not retain another owner's inbox or count.
    response.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

async fn dispatch(state: &AgentState, request: Request) -> Result<Value, AgentError> {
    let path = request.uri().path().to_owned();
    let method = request.method().as_str().to_owned();
    super::public::reject_forgery(request.headers(), None)?;
    let actor = Actor::resolve(&state.auth, request.headers(), &path).await?;
    let pool = &state.repository.pool;
    let tail = path.trim_start_matches(BASE).trim_matches('/');
    let parts: Vec<_> = if tail.is_empty() { vec![] } else { tail.split('/').collect() };
    match (method.as_str(), parts.as_slice()) {
        ("GET", []) => {
            let page = Query::<ListQuery>::try_from_uri(request.uri())
                .map_err(|_| invalid())?.0.validate()?;
            let mut result = store::list(pool, &actor, &page).await?;
            let notifications = result["notifications"].as_array_mut()
                .ok_or_else(|| AgentError::code(503, "AGENT_STORAGE_UNAVAILABLE"))?;
            let has_more = notifications.len() > page.limit;
            notifications.truncate(page.limit);
            let cursor = if has_more {
                notifications.last().map(|row| json!({
                    "before_at": row["occurred_at"], "before_id": row["notification_id"]
                })).unwrap_or(Value::Null)
            } else { Value::Null };
            result["next_cursor"] = cursor;
            Ok(result)
        }
        ("GET", ["summary"]) => store::summary(pool, &actor).await,
        ("GET", [id]) => {
            let id = super::uuid(id)?;
            Ok(ai_platform_gateway_foundation::native_contract::responses::notification::value(store::get(pool, &actor, &id).await?))
        }
        ("POST", ["read"]) => {
            let bytes = to_bytes(request.into_body(), 16 * 1024).await.map_err(|_| invalid())?;
            let batch: ReadBatch = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            let ids = normalize_ids(batch.notification_ids)?;
            store::acknowledge(pool, &actor, &ids, true).await
        }
        ("POST", [id, action]) if matches!(*action, "read" | "unread") => {
            let ids = normalize_ids(vec![(*id).to_owned()])?;
            store::acknowledge(pool, &actor, &ids, *action == "read").await
        }
        _ => Err(AgentError::code(404, "NOT_FOUND")),
    }
}
