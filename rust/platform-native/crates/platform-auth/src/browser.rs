//! Browser BFF boundary: bearer credentials remain exclusively server-side.
// SPDX-License-Identifier: Apache-2.0
use crate::{
    accounts::AuditContext,
    browser_policy::{BrowserPolicy, cookie, session_cookie},
    contract::{LoginInput, PasswordInput},
    service::{AuthService, ServiceError},
    sessions::browser::{BrowserSession, opaque},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Clone)]
pub struct BrowserAuth {
    service: Arc<AuthService>,
    policy: BrowserPolicy,
}
struct Error(ServiceError);
impl From<ServiceError> for Error {
    fn from(e: ServiceError) -> Self {
        Self(e)
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.0.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"detail":self.0.to_string()})),
        )
            .into_response()
    }
}
fn audit(headers: &HeaderMap) -> AuditContext {
    AuditContext {
        ip: "unknown".into(),
        user_agent: headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .chars()
            .take(1024)
            .collect(),
    }
}
fn no_store(mut response: Response) -> Response {
    if let Some(cookie) = response.headers_mut().get_mut("set-cookie") {
        cookie.set_sensitive(true);
    }
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}
impl BrowserAuth {
    pub fn new(service: Arc<AuthService>, origins: Vec<String>) -> Result<Self, ServiceError> {
        if !service.codec.has_eddsa() {
            return Err(ServiceError::Invalid);
        }
        Ok(Self {
            service,
            policy: BrowserPolicy::new(origins)?,
        })
    }
    pub fn router(&self) -> Router {
        Router::new()
            .route("/api/auth/session", get(current).post(login).delete(logout))
            .route("/api/auth/session/password", post(password))
            .layer(DefaultBodyLimit::max(crate::contract::BODY_LIMIT))
            .with_state(self.clone())
    }
    /// Install after merging every application router, including WebSockets.
    /// Explicit API-key/CLI Authorization requests without cookies pass through.
    pub fn protect(self, router: Router) -> Router {
        let mounted = router.merge(self.router());
        self.bridge(mounted)
    }
    /// Bridge only, for hosts that mount BFF routes before assembling their
    /// identity/logging stack. Apply outside that stack so identity sees the
    /// authenticated short-lived request credential.
    pub fn bridge(self, router: Router) -> Router {
        router.layer(middleware::from_fn_with_state(self, protect))
    }
    async fn session(&self, headers: &HeaderMap) -> Result<(String, BrowserSession), ServiceError> {
        let id = session_cookie(headers)?.ok_or(ServiceError::Credentials)?;
        let session = self
            .service
            .sessions
            .browser(&id)
            .await
            .map_err(|_| ServiceError::Unavailable)?
            .ok_or(ServiceError::Credentials)?;
        if session.expires_at <= (self.service.clock)()? {
            return Err(ServiceError::Credentials);
        }
        Ok((id, session))
    }
}
async fn protect(State(state): State<BrowserAuth>, mut request: Request, next: Next) -> Response {
    // Fresh credential login must recover from an expired/revoked cookie.
    // It has no authenticated session yet: strict Origin+Fetch Metadata is
    // enforced by login itself, not a synchronizer token from a dead session.
    // This single OIDC redirect endpoint verifies its own one-use state, nonce,
    // PKCE proof and independent __Host flow cookie. It must work when an IdP
    // returns with an expired existing BFF cookie and cross-site Fetch Metadata.
    if credential_boundary(
        request.method(),
        request.uri().path(),
        state.service.enterprise_policy_enabled(),
    ) {
        return next.run(request).await;
    }
    let id = match session_cookie(request.headers()) {
        Ok(Some(id)) => id,
        Ok(None) => return next.run(request).await,
        Err(e) => return Error(e).into_response(),
    };
    if request.headers().contains_key("authorization") {
        return Error(ServiceError::Credentials).into_response();
    }
    let session = match state.service.sessions.browser(&id).await {
        Ok(Some(s)) => s,
        Ok(None) => return Error(ServiceError::Credentials).into_response(),
        Err(_) => return Error(ServiceError::Unavailable).into_response(),
    };
    if state
        .policy
        .check(request.method(), request.headers(), &session.csrf)
        .is_err()
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"detail":"browser_request_rejected"})),
        )
            .into_response();
    }
    let principal = match state.service.authenticate_browser(&session).await {
        Ok(p) => p,
        Err(e) => return Error(e).into_response(),
    };
    if principal.account.user_id != session.user_id || principal.expires_at != session.expires_at {
        return Error(ServiceError::Credentials).into_response();
    }
    let request_bearer = match state.service.browser_request_bearer(&principal) {
        Ok(token) => token,
        Err(e) => return Error(e).into_response(),
    };
    let mut header = match HeaderValue::from_str(&format!("Bearer {request_bearer}")) {
        Ok(h) => h,
        Err(_) => return Error(ServiceError::Unavailable).into_response(),
    };
    header.set_sensitive(true);
    request.headers_mut().insert("authorization", header);
    no_store(next.run(request).await)
}

fn credential_boundary(method: &axum::http::Method, path: &str, enterprise: bool) -> bool {
    use axum::http::Method;
    if method == Method::POST && path == "/api/auth/session" {
        return true;
    }
    if !enterprise {
        return false;
    }
    if method == Method::GET && path == "/api/auth/enterprise/oidc/callback" {
        return true;
    }
    if method != Method::POST {
        return false;
    }
    // These are fresh credential verification endpoints. Each enforces strict
    // same-origin Fetch Metadata before using its own one-use proof. Enrollment,
    // recovery-code rotation, linking and SCIM administration keep the BFF gate.
    if matches!(
        path,
        "/api/auth/enterprise/passkeys/authenticate/start"
            | "/api/auth/enterprise/passkeys/authenticate/finish"
            | "/api/auth/enterprise/recovery"
    ) {
        return true;
    }
    path.strip_prefix("/api/auth/enterprise/oidc/")
        .and_then(|path| path.strip_suffix("/begin"))
        .is_some_and(|provider| !provider.is_empty() && !provider.contains('/'))
}

#[cfg(test)]
mod credential_boundary_tests {
    use super::*;
    use axum::http::Method;
    #[test]
    fn fresh_credentials_recover_from_expired_cookies_without_exempting_enrollment_or_admin() {
        assert!(credential_boundary(
            &Method::POST,
            "/api/auth/session",
            false
        ));
        assert!(credential_boundary(
            &Method::GET,
            "/api/auth/enterprise/oidc/callback",
            true
        ));
        for path in [
            "/api/auth/enterprise/oidc/keycloak/begin",
            "/api/auth/enterprise/passkeys/authenticate/start",
            "/api/auth/enterprise/passkeys/authenticate/finish",
            "/api/auth/enterprise/recovery",
        ] {
            assert!(credential_boundary(&Method::POST, path, true));
            assert!(!credential_boundary(&Method::POST, path, false));
            assert!(!credential_boundary(&Method::GET, path, true));
        }
        for path in [
            "/api/auth/enterprise/passkeys/register/start",
            "/api/auth/enterprise/passkeys/register/finish",
            "/api/auth/enterprise/recovery/rotate",
            "/api/auth/enterprise/scim-credentials",
            "/api/auth/enterprise/identities",
            "/api/auth/enterprise/oidc/a/b/begin",
            "/api/auth/enterprise/oidc//begin",
            "/api/auth/enterprise/oidc/callback/",
        ] {
            assert!(!credential_boundary(&Method::POST, path, true));
            assert!(!credential_boundary(&Method::GET, path, true));
        }
        assert!(!credential_boundary(
            &Method::POST,
            "/api/auth/enterprise/oidc/callback",
            true
        ));
    }
}
async fn login(
    State(state): State<BrowserAuth>,
    headers: HeaderMap,
    Json(input): Json<LoginInput>,
) -> Result<Response, Error> {
    if state.policy.check_origin(&headers, true).is_err() {
        return Ok(no_store(
            (
                StatusCode::FORBIDDEN,
                Json(json!({"detail":"browser_request_rejected"})),
            )
                .into_response(),
        ));
    }
    let login = state
        .service
        .login(&input.email, input.password, &audit(&headers))
        .await?;
    let principal = state.service.authenticate(&login.access_token).await?;
    let id = opaque();
    let csrf = opaque();
    let now = (state.service.clock)()?;
    let expires_at = now
        .checked_add(crate::browser_authority::BROWSER_ABSOLUTE_TTL)
        .ok_or(ServiceError::Unavailable)?;
    let existing = state
        .service
        .sessions
        .record(&principal.jti)
        .await
        .map_err(|_| ServiceError::Unavailable)?
        .ok_or(ServiceError::Credentials)?;
    state
        .service
        .sessions
        .bind_browser_lifetime(
            &principal.jti,
            &existing,
            crate::browser_authority::BROWSER_ABSOLUTE_TTL,
        )
        .await
        .map_err(|_| ServiceError::Credentials)?;
    let session = BrowserSession {
        jti: principal.jti.clone(),
        tenant_id: principal.account.tenant_id,
        issued_at: now,
        idle_expires_at: expires_at,
        csrf: csrf.clone(),
        credential_revision: existing
            .get("credential_revision")
            .cloned()
            .ok_or(ServiceError::Credentials)?,
        user_id: principal.account.user_id,
        expires_at,
    };
    if state
        .service
        .sessions
        .save_browser(&id, &session, crate::browser_authority::BROWSER_IDLE_TTL)
        .await
        .is_err()
    {
        let _ = state
            .service
            .sessions
            .revoke(&principal.jti, &session.user_id)
            .await;
        return Err(ServiceError::Unavailable.into());
    }
    let mut response=Json(json!({"user":login.user,"force_password_change":login.force_password_change,"expires_in":crate::browser_authority::BROWSER_ABSOLUTE_TTL,"session_type":"cookie","csrf_token":csrf})).into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&cookie(&id, crate::browser_authority::BROWSER_ABSOLUTE_TTL))
            .map_err(|_| ServiceError::Unavailable)?,
    );
    Ok(no_store(response))
}
async fn current(State(state): State<BrowserAuth>, headers: HeaderMap) -> Result<Response, Error> {
    let (_, session) = state.session(&headers).await?;
    let p = state.service.authenticate_browser(&session).await?;
    Ok(no_store(Json(json!({"user":{"user_id":p.account.user_id,"tenant_id":p.account.tenant_id,"email":p.account.email,"display_name":p.account.display_name,"department":p.account.department,"roles":p.authority.roles,"permissions":p.authority.permissions,"effective_permissions":p.authority.permissions,"tier":p.authority.tier,"force_password_change":p.account.force_password_change},"csrf_token":session.csrf,"expires_at":session.expires_at})).into_response()))
}
async fn logout(State(state): State<BrowserAuth>, headers: HeaderMap) -> Result<Response, Error> {
    let (id, session) = state.session(&headers).await?;
    let principal = state.service.authenticate_browser(&session).await?;
    let token = state.service.browser_request_bearer(&principal)?;
    state.service.logout(&token, &audit(&headers)).await?;
    state
        .service
        .sessions
        .revoke_browser(&id)
        .await
        .map_err(|_| ServiceError::Unavailable)?;
    let mut response = Json(json!({"message":"Logged out"})).into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&cookie("", 0)).map_err(|_| ServiceError::Unavailable)?,
    );
    Ok(no_store(response))
}

async fn password(
    State(state): State<BrowserAuth>,
    headers: HeaderMap,
    Json(input): Json<PasswordInput>,
) -> Result<Response, Error> {
    let (id, session) = state.session(&headers).await?;
    state
        .service
        .change_password(
            &state
                .service
                .browser_request_bearer(&state.service.authenticate_browser(&session).await?)?,
            input.current_password,
            input.new_password,
            &input.confirm_password,
            &audit(&headers),
        )
        .await?;
    state
        .service
        .sessions
        .revoke_browser(&id)
        .await
        .map_err(|_| ServiceError::Unavailable)?;
    let mut response = Json(json!({"message":"Password changed; sign in again"})).into_response();
    response.headers_mut().insert(
        "set-cookie",
        HeaderValue::from_str(&cookie("", 0)).map_err(|_| ServiceError::Unavailable)?,
    );
    Ok(no_store(response))
}
