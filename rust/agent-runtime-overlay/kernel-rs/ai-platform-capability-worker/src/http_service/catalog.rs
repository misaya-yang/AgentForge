//! Capability catalog endpoint: registry-authoritative descriptor listing
//! bound to the forwarded runtime scope.

use ai_platform_capability_contract::{
    CAPABILITY_CATALOG_SCHEMA_VERSION, CapabilityCatalogRequestV2, CapabilityCatalogV2,
    CapabilityScopeV2,
};
use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;

use super::{HttpError, WorkerState, authorize, error, scope_matches};

pub(super) async fn catalog(
    State(state): State<WorkerState>,
    headers: HeaderMap,
    Json(request): Json<CapabilityCatalogRequestV2>,
) -> Result<impl IntoResponse, HttpError> {
    let scope = authorize(&headers, &state)?;
    request.validate().map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "catalog_request_invalid",
            "catalog request is invalid",
        )
    })?;
    if !scope_matches(
        &scope,
        &CapabilityScopeV2 {
            tenant_id: request.tenant_id,
            user_id: request.user_id,
            session_id: request.session_id,
        },
    ) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "scope_mismatch",
            "catalog scope does not match forwarded identity",
        ));
    }
    let activation = state
        .plugin_activation
        .as_ref()
        .map(|active| active.receipt.activation_id.clone())
        .unwrap_or_else(|| "none".into());
    Ok((
        [("x-ai-platform-plugin-activation", activation)],
        Json(CapabilityCatalogV2 {
            schema_version: CAPABILITY_CATALOG_SCHEMA_VERSION.to_string(),
            capability_revision: request.capability_revision,
            capabilities: state.capability_registry.values().cloned().collect(),
        }),
    ))
}
