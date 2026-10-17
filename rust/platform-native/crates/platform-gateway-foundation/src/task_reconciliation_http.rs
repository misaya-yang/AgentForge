//! Operator hints are not evidence. Proof discovery and terminal fencing live in the ledger.
use super::*;
use crate::task_delivery::{DeliveryIdentity, ReconcileRequest, ReconcileResult};
pub(super) async fn reconcile_task(
    State(state): State<TaskState>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ReconcileRequest>,
) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    if !principal.session_jwt_selected
        || principal.session_principal.is_none()
        || !principal.roles.iter().any(|role| role == "admin")
    {
        return error(
            StatusCode::FORBIDDEN,
            "same-tenant operator session required",
        );
    }
    let task = match owned_task(&state.manager, &principal, &task_id, &headers).await {
        Ok(task) => task,
        Err(response) => return *response,
    };
    let identity = DeliveryIdentity {
        task_id: task.task_id,
        request_id: task.request_id,
        service_id: task.service_id,
        user_id: task.user_id,
        tenant_id: task.tenant_id,
    };
    let Some(delivery) = state.manager.delivery() else {
        return error(StatusCode::CONFLICT, "durable delivery required");
    };
    match delivery
        .reconcile(&identity, &principal.user_id, request)
        .await
    {
        Ok(ReconcileResult::Applied(status)) => {
            Json(json!({"status":status,"reconciled":true})).into_response()
        }
        Ok(ReconcileResult::AlreadyResolved) => {
            Json(json!({"reconciled":true,"already_resolved":true})).into_response()
        }
        Ok(ReconcileResult::FenceRejected | ReconcileResult::ProjectionConflict) => {
            error(StatusCode::CONFLICT, "task reconciliation fence rejected")
        }
        Err(crate::task_delivery::DeliveryError::InvalidEnvelope) => error(
            StatusCode::CONFLICT,
            "verified terminal provider evidence required",
        ),
        Err(_) => internal_error(&headers),
    }
}
pub(super) async fn delivery_state(
    State(state): State<TaskState>,
    Path(task_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(value) => value,
        Err(response) => return *response,
    };
    if !principal.session_jwt_selected
        || principal.session_principal.is_none()
        || !principal.roles.iter().any(|r| r == "admin")
    {
        return error(
            StatusCode::FORBIDDEN,
            "same-tenant operator session required",
        );
    }
    let task = match owned_task(&state.manager, &principal, &task_id, &headers).await {
        Ok(task) => task,
        Err(response) => return *response,
    };
    let id = DeliveryIdentity {
        task_id: task.task_id,
        request_id: task.request_id,
        service_id: task.service_id,
        user_id: task.user_id,
        tenant_id: task.tenant_id,
    };
    let Some(delivery) = state.manager.delivery() else {
        return error(StatusCode::CONFLICT, "durable delivery required");
    };
    match delivery.observe(&id).await{Ok(Some(row))=>Json(json!({"state":row.state,"claim_id":row.claim_id,"reconciliation_reason":row.reconciliation_reason,"replay_allowed":false})).into_response(),Ok(None)=>error(StatusCode::NOT_FOUND,"delivery not found"),Err(_)=>internal_error(&headers)}
}
