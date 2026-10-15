//! Opt-in GET /setup/state, ported from src/api/v1/setup.py.
//! Trusted identity, RBAC/settings and cancellation are mandatory host inputs.
//! Reuses the native GatewayModelMeta configured-provider collaborator. No DB,
//! credential reads, probes or scheduler are added. Absent metadata means false;
//! ordinary metadata failures remain opaque 500, cancellation propagates.
//! Host owns guest/disabled-auth resolution, denial-event scheduling, transport
//! limits, encoded static paths and slash redirects. GET ignores body and query.
use crate::health_api::{
    HealthApiError, HealthApiFuture, HealthHttpCancellation, HealthModelMetadata,
};
use ai_platform_auth::{
    admission::AdmissionService,
    rbac::{Capability, Rbac, check_capability, permission_denied_detail},
};
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use std::sync::Arc;

pub const SETUP_PROVIDERS: &[&str] = &[
    "openai",
    "anthropic",
    "deepseek",
    "dashscope",
    "google",
    "google-vertex",
];
/// Construct only from a verified auth resolver; never deserialize request claims.
#[derive(Clone)]
pub struct SetupIdentity {
    pub user_id: String,
    pub tenant_id: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub is_authenticated: bool,
}
pub trait SetupIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> HealthApiFuture<'a, SetupIdentity>;
    /// Canonical best-effort audit scheduling is host-owned, not awaited.
    fn permission_denied(&self, _identity: &SetupIdentity, _trace: &str) {}
}
impl SetupIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> HealthApiFuture<'a, SetupIdentity> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let user = request
                .resolve()
                .await
                .map_err(|e| HealthApiError::http(e.status(), e.to_string()))?;
            Ok(SetupIdentity {
                user_id: user.user_id().into(),
                tenant_id: user.tenant_id().into(),
                roles: user.roles().to_vec(),
                permissions: user.permissions().to_vec(),
                is_authenticated: true,
            })
        })
    }
}
/// Shared source require_gateway_capability / require_platform_admin gate.
/// Architecture intentionally has no additional is_authenticated check: its
/// canonical gate checks capability then explicit normalized admin subject.
pub fn require_setup_capability(
    identity: &SetupIdentity,
    rbac: &Rbac,
    resolver: &dyn SetupIdentityResolver,
    trace: &str,
    platform_admin: bool,
) -> Result<(), HealthApiError> {
    let capability = Capability::ServiceListRead;
    if !check_capability(rbac, &identity.roles, &identity.permissions, capability).allowed() {
        resolver.permission_denied(identity, trace);
        return Err(HealthApiError::http(
            403,
            permission_denied_detail(capability, trace, None),
        ));
    }
    if platform_admin
        && !identity.roles.iter().chain(&identity.permissions).any(|s| {
            matches!(
                s.trim_matches(crate::usage_parser::python_whitespace)
                    .to_lowercase()
                    .as_str(),
                "platform_admin" | "superadmin" | "super_admin"
            )
        })
    {
        resolver.permission_denied(identity, trace);
        let mut detail = permission_denied_detail(capability, trace, None);
        detail["reason"] = json!("platform_admin_required");
        return Err(HealthApiError::http(403, detail));
    }
    Ok(())
}
pub struct SetupApi {
    pub identity: Arc<dyn SetupIdentityResolver>,
    pub rbac: Rbac,
    pub metadata: Option<Arc<dyn HealthModelMetadata>>,
    pub mode: String,
    pub default_model: String,
    pub cancellation: Arc<dyn HealthHttpCancellation>,
}
impl SetupApi {
    pub async fn state(&self, auth: &SetupIdentity, trace: &str) -> Result<Value, HealthApiError> {
        if !auth.is_authenticated {
            return Err(HealthApiError::http(401, "Authentication required"));
        }
        require_setup_capability(auth, &self.rbac, self.identity.as_ref(), trace, false)?;
        let tenant = if auth.tenant_id.is_empty() {
            "default"
        } else {
            &auth.tenant_id
        };
        let configured = match &self.metadata {
            Some(meta) => meta.configured(tenant).await?,
            None => vec![],
        };
        let missing: Vec<_> = SETUP_PROVIDERS
            .iter()
            .filter(|p| !configured.iter().any(|c| c == **p))
            .collect();
        Ok(
            json!({"configured":!configured.is_empty(),"missing":missing,"mode":self.mode,"default_model":self.default_model}),
        )
    }
}
/// Trusted middleware may override even an empty request trace.
#[derive(Clone)]
pub struct SetupRequestTrace(pub String);
pub fn setup_request_trace(request: &Request) -> String {
    request
        .extensions()
        .get::<SetupRequestTrace>()
        .map(|v| v.0.clone())
        .unwrap_or_default()
}
pub fn setup_error_response(error: HealthApiError, trace: &str) -> Response {
    let status = error.status().unwrap_or(500);
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(
            error.response_body(trace).unwrap_or_else(
                || json!({"error":{"code":2000,"message":"Internal server error"}}),
            ),
        ),
    )
        .into_response()
}
pub fn setup_method_error() -> Response {
    let mut response = setup_error_response(HealthApiError::http(405, "Method Not Allowed"), "");
    response
        .headers_mut()
        .insert(header::ALLOW, "GET".parse().expect("static method"));
    response
}
pub fn setup_router(api: Arc<SetupApi>) -> Router {
    Router::new()
        .route("/setup/state", any(setup_handler))
        .with_state(api)
}
async fn setup_handler(State(api): State<Arc<SetupApi>>, request: Request) -> Response {
    if request.method() != Method::GET {
        return setup_method_error();
    }
    let trace = setup_request_trace(&request);
    // The source ignores GET bodies. Drop it before awaiting the trusted resolver;
    // borrowing Request<Body> across await would require Body to be Sync.
    let (parts, body) = request.into_parts();
    drop(body);
    let result = async {
        let auth = api
            .identity
            .resolve(&parts.headers, parts.uri.path())
            .await?;
        api.state(&auth, &trace).await
    }
    .await;
    match result {
        Ok(value) => Json(value).into_response(),
        Err(HealthApiError::Cancelled) => api.cancellation.cancelled().await,
        Err(e) => setup_error_response(e, &trace),
    }
}
