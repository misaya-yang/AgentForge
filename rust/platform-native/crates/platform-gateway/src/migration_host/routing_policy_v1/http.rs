//! Existing authenticated listener adapter; no independent authentication scheme.
use super::*;
use ai_platform_auth::admission::AdmissionService;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
#[derive(Clone)]
struct StateData {
    service: RoutingPolicyService,
    auth: Arc<AdmissionService>,
}
pub fn router(service: RoutingPolicyService, auth: Arc<AdmissionService>) -> Router {
    Router::new()
        .route("/api/v1/routing/decisions/{id}", get(read))
        .route("/api/v1/routing/policies", post(publish))
        .route(
            "/api/v1/routing/policies/{id}/versions/{version}",
            get(version),
        )
        .route("/api/v1/routing/policies/{id}/activate", post(activate))
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
        .with_state(StateData { service, auth })
}
struct Error(RoutingError);
impl From<RoutingError> for Error {
    fn from(e: RoutingError) -> Self {
        Self(e)
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = match self.0 {
            RoutingError::Forbidden => StatusCode::FORBIDDEN,
            RoutingError::NotFound => StatusCode::NOT_FOUND,
            RoutingError::Conflict | RoutingError::PinMismatch => StatusCode::CONFLICT,
            RoutingError::Storage | RoutingError::UncertainCommit => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            _ => StatusCode::BAD_REQUEST,
        };
        (status, Json(json!({"error":self.0.to_string()}))).into_response()
    }
}
async fn authority(
    s: &StateData,
    h: &HeaderMap,
    path: &str,
) -> std::result::Result<RoutingAuthority, Error> {
    let request = s.auth.request(h, path);
    let p = request
        .resolve()
        .await
        .map_err(|_| Error(RoutingError::Forbidden))?;
    RoutingAuthority::from_principal(p).map_err(Error)
}
fn response(v: impl serde::Serialize) -> Response {
    let mut r = Json(v).into_response();
    r.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    r
}
async fn read(
    State(s): State<StateData>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> std::result::Result<Response, Error> {
    let a = authority(&s, &headers, "/api/v1/routing/decisions").await?;
    Ok(response(s.service.read(&a, &id).await?))
}
async fn publish(
    State(s): State<StateData>,
    headers: HeaderMap,
    Json(p): Json<Policy>,
) -> std::result::Result<Response, Error> {
    let a = authority(&s, &headers, "/api/v1/routing/policies").await?;
    Ok(response(
        json!({"policy_pin":s.service.publish(&a,&p).await?}),
    ))
}
async fn version(
    State(s): State<StateData>,
    Path((id, version)): Path<(String, String)>,
    headers: HeaderMap,
) -> std::result::Result<Response, Error> {
    let a = authority(&s, &headers, "/api/v1/routing/policies").await?;
    if !a.manage {
        return Err(Error(RoutingError::Forbidden));
    }
    Ok(response(s.service.load_version(&a, &id, &version).await?))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Activation {
    version: String,
    expected_pin: Option<String>,
}
async fn activate(
    State(s): State<StateData>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<Activation>,
) -> std::result::Result<Response, Error> {
    let a = authority(&s, &headers, "/api/v1/routing/policies").await?;
    Ok(response(
        json!({"policy_pin":s.service.activate(&a,&id,&input.version,input.expected_pin.as_deref()).await?}),
    ))
}
