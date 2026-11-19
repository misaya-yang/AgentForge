//! Approval read/decision endpoints bound to the authenticated runtime scope.

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::HeaderMap;
use serde_json::json;
use uuid::Uuid;

use super::RuntimeBroadcastEvent;
use super::RuntimeHttpState;
use super::security::RuntimeError;
use super::security::SESSION_HEADER;
use super::security::TENANT_HEADER;
use super::security::USER_HEADER;
use super::security::authorize;
use super::security::required_header;
use crate::approval_control::ApprovalDecisionRequest;

pub(super) async fn get_approval(
    State(state): State<RuntimeHttpState>,
    Path(approval_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<crate::approval_control::ApprovalSummary>, RuntimeError> {
    authorize(&headers, &state.internal_token)?;
    let tenant_id = required_header(&headers, TENANT_HEADER)?;
    let user_id = required_header(&headers, USER_HEADER)?;
    let session_id = required_header(&headers, SESSION_HEADER)?;
    state
        .approvals
        .summary(approval_id, &tenant_id, &user_id, &session_id, &state.store)
        .await
        .map(Json)
        .map_err(|_| RuntimeError::not_found("approval_not_found"))
}

pub(super) async fn decide_approval(
    State(state): State<RuntimeHttpState>,
    Path(approval_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ApprovalDecisionRequest>,
) -> Result<Json<serde_json::Value>, RuntimeError> {
    authorize(&headers, &state.internal_token)?;
    let tenant_id = required_header(&headers, TENANT_HEADER)?;
    let user_id = required_header(&headers, USER_HEADER)?;
    let session_id = required_header(&headers, SESSION_HEADER)?;
    let projection = state
        .approvals
        .decide(
            approval_id,
            body.decision,
            &tenant_id,
            &user_id,
            &session_id,
            body.reason.as_deref(),
            body.receipt_only.as_ref().map(|scope| scope.thread_id),
            &state.store,
            &state.requests,
        )
        .await
        .map_err(|_| RuntimeError::bad_request("approval_not_eligible"))?;
    // Receipt-only reads do not wake subscribers or publish another result.
    if body.receipt_only.is_none() {
        let _ = state.events.send(RuntimeBroadcastEvent {
            root_thread_id: projection.root_thread_id,
            event: projection.event,
        });
    }
    Ok(Json(json!({
        "approval_id": approval_id,
        "status": projection.status,
    })))
}
