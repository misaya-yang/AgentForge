//! Request scope plus the proof-bound Knowledge-to-Gateway object PDP.
use crate::migration_knowledge_query::{actor::CurrentActorStore, auth::GatewayAuth};
use ai_platform_auth::{
    cedar::{
        DecisionPoint,
        resources::{self, PolicyScope},
    },
    rbac::Rbac,
};
use ai_platform_service_primitives::object_authorization::*;
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::post,
};
use std::{collections::BTreeSet, sync::Arc};
pub async fn request_scope(
    State(rbac): State<Arc<Rbac>>,
    request: Request,
    next: Next,
) -> Response {
    let context = request
        .extensions()
        .get::<crate::migration_host::middleware::RequestContext>();
    let scope = PolicyScope {
        point: rbac.decision_point.clone(),
        request_id: context.map(|c| c.request_id.clone()).unwrap_or_default(),
        trace_id: context.map(|c| c.trace_id.clone()).unwrap_or_default(),
        ..Default::default()
    };
    resources::scoped(scope, next.run(request)).await
}
#[derive(Clone)]
pub struct DatasetPdp {
    pub point: Arc<DecisionPoint>,
    pub verifier: Arc<GatewayAuth>,
    pub actors: CurrentActorStore,
}
pub fn router(
    mut state: DatasetPdp,
) -> Result<Router, crate::migration_host::lifecycle::HostError> {
    if state.verifier.version != "v2"
        || state.verifier.caller_service != "knowledge-service"
        || state.verifier.audience != "gateway"
        || state.verifier.allow_anonymous
    {
        return Err(crate::migration_host::lifecycle::HostError::Configuration);
    }
    let mut verifier = (*state.verifier).clone();
    if !verifier
        .allowed_path_prefixes
        .iter()
        .any(|p| p == DATASET_DECISION_PATH)
    {
        verifier
            .allowed_path_prefixes
            .push(DATASET_DECISION_PATH.into());
    }
    state.verifier = Arc::new(verifier);
    Ok(Router::new()
        .route(DATASET_DECISION_PATH, post(decide))
        .route(&format!("{DATASET_DECISION_PATH}/batch"), post(batch))
        .with_state(state))
}
async fn verified_body(state: &DatasetPdp, request: Request) -> Result<Vec<u8>, StatusCode> {
    let (parts, body) = request.into_parts();
    let bytes = to_bytes(body, 1024 * 1024)
        .await
        .map_err(|_| StatusCode::PAYLOAD_TOO_LARGE)?;
    let signature = parts
        .headers
        .get("x-gateway-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    state
        .verifier
        .verify(
            signature,
            parts.method.as_str(),
            parts.uri.path(),
            parts.uri.query().unwrap_or(""),
            &bytes,
            &parts.headers,
        )
        .await
        .map_err(|_| StatusCode::FORBIDDEN)?;
    Ok(bytes.to_vec())
}
fn scope(point: &Arc<DecisionPoint>, request_id: String, trace_id: String) -> PolicyScope {
    let mut scope = resources::capture();
    scope.point = Some(point.clone());
    if scope.request_id.is_empty() {
        scope.request_id = request_id;
    }
    if scope.trace_id.is_empty() {
        scope.trace_id = trace_id;
    }
    scope
}
async fn decide(State(state): State<DatasetPdp>, request: Request) -> Response {
    let bytes = match verified_body(&state, request).await {
        Ok(b) => b,
        Err(s) => return s.into_response(),
    };
    let mut request: DatasetDecisionRequest = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(_) => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
    };
    if request.resource.kind != ResourceKind::Dataset
        || !matches!(
            request.action,
            ObjectAction::Read | ObjectAction::Write | ObjectAction::Own
        )
        || request.resource.id.is_empty()
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let current = match refresh_subject(&state, &mut request.subject).await {
        Ok(current) => current,
        Err(status) => return status.into_response(),
    };
    request.resource.identity_current &= current;
    let scope = scope(&state.point, request.request_id, request.trace_id);
    let evaluated =
        state
            .point
            .evaluate_resource(&request.subject, &request.resource, request.action);
    let allowed = state.point.decide_resource(
        &request.subject,
        &request.resource,
        request.action,
        request.legacy_allowed,
        &scope,
    );
    Json(ObjectDecisionReceipt {
        kind: ResourceKind::Dataset,
        allowed,
        legacy_allowed: request.legacy_allowed,
        cedar_allowed: matches!(evaluated, Ok(true)),
        enforced: state.point.resource_enforced(ResourceKind::Dataset),
        policy_sha256: state.point.resource_policy_sha256().into(),
        matrix_sha256: state.point.resource_matrix_sha256().into(),
    })
    .into_response()
}
async fn refresh_subject(
    state: &DatasetPdp,
    subject: &mut ObjectSubject,
) -> Result<bool, StatusCode> {
    if subject.anonymous {
        subject.roles.clear();
        subject.permissions.clear();
        subject.tier = "anonymous".into();
        return Ok(true);
    }
    let fresh = match state
        .actors
        .resolve(&subject.tenant_id, &subject.user_id)
        .await
    {
        Ok(s) => s,
        Err(crate::migration_knowledge_query::types::QueryError::Forbidden) => return Ok(false),
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let permissions=sqlx::query_scalar::<_,String>("SELECT permission_code FROM user_permissions WHERE user_id=$1 AND (expires_at IS NULL OR expires_at>NOW())").bind(&subject.user_id).fetch_all(&state.actors.pool).await.map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
    let current = fresh
        .roles
        .iter()
        .chain(&permissions)
        .cloned()
        .collect::<BTreeSet<_>>();
    let derived = if current.contains("admin") {
        "admin"
    } else if current.contains("enterprise") {
        "enterprise"
    } else if current.contains("premium") || current.contains("vip") {
        "premium"
    } else {
        fresh.user_tier.as_str()
    };
    Ok(fresh.is_authenticated
        && subject.roles.iter().all(|r| current.contains(r))
        && subject.permissions.iter().all(|r| current.contains(r))
        && (subject.tier == fresh.user_tier || subject.tier == derived))
}
fn ranks(
    point: &DecisionPoint,
    subject: &ObjectSubject,
    checks: Vec<DatasetRankCheck>,
    current: bool,
    scope: &PolicyScope,
) -> Vec<DatasetRankReceipt> {
    checks
        .into_iter()
        .map(|mut check| {
            check.resource.identity_current &= current;
            let mut rank = 0;
            for action in [ObjectAction::Read, ObjectAction::Write, ObjectAction::Own] {
                if point.decide_resource(
                    subject,
                    &check.resource,
                    action,
                    check.legacy_rank >= action.rank() as i32,
                    scope,
                ) {
                    rank = action.rank() as i32;
                }
            }
            DatasetRankReceipt {
                resource_id: check.resource.id,
                allowed_rank: rank,
                legacy_rank: check.legacy_rank,
                enforced: point.resource_enforced(ResourceKind::Dataset),
                policy_sha256: point.resource_policy_sha256().into(),
                matrix_sha256: point.resource_matrix_sha256().into(),
            }
        })
        .collect()
}
async fn batch(State(state): State<DatasetPdp>, request: Request) -> Response {
    let bytes = match verified_body(&state, request).await {
        Ok(b) => b,
        Err(s) => return s.into_response(),
    };
    let mut request: DatasetRankBatch = match serde_json::from_slice(&bytes) {
        Ok(r) => r,
        Err(_) => return StatusCode::UNPROCESSABLE_ENTITY.into_response(),
    };
    if request.checks.len() > 200
        || request.checks.iter().any(|c| {
            c.resource.kind != ResourceKind::Dataset
                || c.resource.id.is_empty()
                || !(0..=3).contains(&c.legacy_rank)
        })
    {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    let current = match refresh_subject(&state, &mut request.subject).await {
        Ok(current) => current,
        Err(status) => return status.into_response(),
    };
    let scope = scope(&state.point, request.request_id, request.trace_id);
    Json(ranks(
        &state.point,
        &request.subject,
        request.checks,
        current,
        &scope,
    ))
    .into_response()
}
#[cfg(test)]
#[path = "object_authorization_tests.rs"]
mod tests;
