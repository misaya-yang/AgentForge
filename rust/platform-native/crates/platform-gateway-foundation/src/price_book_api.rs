//! Tenant-scoped v2 pricing HTTP boundary using the existing trusted identity
//! resolver. No tenant/role authority is accepted from headers or request bodies.
use super::{
    Component, PriceBookError,
    service::{Activation, PgPriceBookService, Publication},
};
use crate::model_admin_api::{ModelAdminIdentityResolver, ResolvedModelUser};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Clone)]
struct ApiState {
    service: Arc<PgPriceBookService>,
    identity: Arc<dyn ModelAdminIdentityResolver>,
}
#[derive(Debug)]
struct ApiError(StatusCode, &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"detail":self.1}))).into_response()
    }
}
impl From<PriceBookError> for ApiError {
    fn from(error: PriceBookError) -> Self {
        match error {
            PriceBookError::Forbidden => Self(StatusCode::FORBIDDEN, "price_book_tenant_forbidden"),
            PriceBookError::RevisionConflict | PriceBookError::AmbiguousPrice => {
                Self(StatusCode::CONFLICT, "price_book_revision_conflict")
            }
            PriceBookError::MissingPrice
            | PriceBookError::UnknownModel
            | PriceBookError::NotActivated => {
                Self(StatusCode::NOT_FOUND, "price_book_not_found_or_inactive")
            }
            PriceBookError::Storage | PriceBookError::CommitUncertain => Self(
                StatusCode::SERVICE_UNAVAILABLE,
                "price_book_storage_outcome_requires_reconciliation",
            ),
            _ => Self(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_price_book_terms_or_receipt",
            ),
        }
    }
}
fn authorized(user: &ResolvedModelUser, write: bool) -> Result<(), ApiError> {
    if !user.is_authenticated || user.tenant_id.trim().is_empty() || user.user_id.trim().is_empty()
    {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "authentication_required",
        ));
    }
    if write && !user.roles.iter().any(|role| role == "admin") {
        return Err(ApiError(StatusCode::FORBIDDEN, "admin_required"));
    }
    Ok(())
}
async fn principal(
    state: &ApiState,
    headers: &HeaderMap,
    write: bool,
) -> Result<ResolvedModelUser, ApiError> {
    let user = state
        .identity
        .resolve(headers)
        .await
        .map_err(|_| ApiError(StatusCode::UNAUTHORIZED, "authentication_required"))?;
    authorized(&user, write)?;
    Ok(user)
}
pub fn router(
    service: Arc<PgPriceBookService>,
    identity: Arc<dyn ModelAdminIdentityResolver>,
) -> Router {
    Router::new()
        .route("/api/v1/pricing/v2/publications", get(list).post(publish))
        .route("/api/v1/pricing/v2/activations", post(activate))
        .route("/api/v1/pricing/v2/quote", get(quote))
        .route("/api/v1/pricing/v2/token-snapshot", get(snapshot))
        .route("/api/v1/pricing/v2/cache-metrics", get(cache_metrics))
        .with_state(ApiState { service, identity })
}
async fn list(State(state): State<ApiState>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    let user = principal(&state, &headers, false).await?;
    Ok(Json(
        json!({"publications":state.service.list_publications(&user.tenant_id).await?}),
    ))
}
async fn publish(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(publication): Json<Publication>,
) -> Result<Json<Value>, ApiError> {
    let user = principal(&state, &headers, true).await?;
    publication.validate(&user.tenant_id, true)?;
    let reference = state
        .service
        .publish(&user.tenant_id, &user.user_id, publication)
        .await?;
    Ok(Json(json!({"reference":reference,"immutable":true})))
}
async fn activate(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(activation): Json<Activation>,
) -> Result<Json<Value>, ApiError> {
    let user = principal(&state, &headers, true).await?;
    let revision = state
        .service
        .activate(
            &user.tenant_id,
            &user.user_id,
            &activation,
            chrono::Utc::now().timestamp_millis(),
        )
        .await?;
    Ok(Json(json!({"revision":revision})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuoteQuery {
    provider: String,
    model: String,
    currency: String,
    component: Component,
    units: u64,
    at_ms: Option<i64>,
}
async fn quote(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<QuoteQuery>,
) -> Result<Json<Value>, ApiError> {
    let user = principal(&state, &headers, false).await?;
    Ok(Json(
        state
            .service
            .quote_component(
                &user.tenant_id,
                &query.provider,
                &query.model,
                &query.currency,
                query.component,
                query.units,
                query
                    .at_ms
                    .unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
            )
            .await?,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenQuery {
    provider: String,
    model: String,
    at_ms: Option<i64>,
}
async fn snapshot(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Result<Json<Value>, ApiError> {
    let user = principal(&state, &headers, false).await?;
    Ok(Json(
        state
            .service
            .token_quote(
                &user.tenant_id,
                &query.provider,
                &query.model,
                query
                    .at_ms
                    .unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
            )
            .await?,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_authenticated_same_tenant_admin_can_mutate() {
        let mut user = ResolvedModelUser {
            tenant_id: "a".into(),
            user_id: "u".into(),
            roles: vec![],
            is_authenticated: true,
        };
        assert!(authorized(&user, false).is_ok());
        assert!(authorized(&user, true).is_err());
        user.roles.push("admin".into());
        assert!(authorized(&user, true).is_ok());
        user.is_authenticated = false;
        assert!(authorized(&user, true).is_err());
    }
    #[test]
    fn query_cannot_supply_tenant_or_role_authority() {
        assert!(
            serde_json::from_value::<TokenQuery>(
                json!({"provider":"p","model":"m","tenant_id":"foreign"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<TokenQuery>(
                json!({"provider":"p","model":"m","roles":["admin"]})
            )
            .is_err()
        );
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CacheMetricQuery {
    from_ms: i64,
    to_ms: i64,
}
async fn cache_metrics(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<CacheMetricQuery>,
) -> Result<Json<Value>, ApiError> {
    let user = principal(&state, &headers, false).await?;
    Ok(Json(
        json!({"metrics":state.service.cache_metrics(&user.tenant_id,query.from_ms,query.to_ms).await?}),
    ))
}
