//! RFC 8628 public-client grant over the existing EdDSA/session authority.
//! Device and refresh secrets are hashed at rest. No user/tenant is client-selected.
use crate::{
    Claims,
    browser_policy::{BrowserPolicy, session_cookie},
    service::{AuthService, Principal, ServiceError},
    sessions::browser::{BrowserSession, opaque},
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
pub const DEVICE_TTL: u64 = 600;
pub const CLIENT_ID: &str = "agentforge-cli";
pub const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
#[derive(Clone)]
struct DeviceAuth {
    auth: Arc<AuthService>,
    policy: BrowserPolicy,
    origin: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Binding {
    pub parent_jti: String,
    pub user_id: String,
    pub tenant_id: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub credential_revision: Value,
    pub auth_epoch: Option<Value>,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Flow {
    pub challenge: String,
    pub client_id: String,
    pub scope: String,
    pub user_code_hash: String,
    pub expires_at: u64,
    pub last_poll: u64,
    pub interval: u64,
    pub status: String,
    pub binding: Option<Binding>,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct RefreshGrant {
    pub id: String,
    pub jti: String,
    pub client_id: String,
    pub scope: String,
    pub refresh_hash: String,
    pub revocation_hashes: Vec<String>,
    pub binding: Binding,
}
#[derive(Debug, PartialEq)]
pub(crate) enum DeviceError {
    Invalid,
    InvalidClient,
    InvalidGrant,
    Pending,
    SlowDown,
    Denied,
    Expired,
    Rate,
    Proof,
    Unavailable,
    StepUp,
}
impl IntoResponse for DeviceError {
    fn into_response(self) -> Response {
        let (status, error) = match self {
            Self::Invalid => (StatusCode::BAD_REQUEST, "invalid_request"),
            Self::InvalidClient => (StatusCode::BAD_REQUEST, "invalid_client"),
            Self::InvalidGrant => (StatusCode::BAD_REQUEST, "invalid_grant"),
            Self::Pending => (StatusCode::BAD_REQUEST, "authorization_pending"),
            Self::SlowDown => (StatusCode::BAD_REQUEST, "slow_down"),
            Self::Denied => (StatusCode::BAD_REQUEST, "access_denied"),
            Self::Expired => (StatusCode::BAD_REQUEST, "expired_token"),
            Self::Rate => (StatusCode::TOO_MANY_REQUESTS, "slow_down"),
            Self::Proof => (StatusCode::UNAUTHORIZED, "access_denied"),
            Self::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable"),
            Self::StepUp => (StatusCode::FORBIDDEN, "step_up_required"),
        };
        secure((status, Json(json!({"error":error}))).into_response())
    }
}
fn failure(e: ServiceError) -> DeviceError {
    if matches!(e, ServiceError::Unavailable) {
        DeviceError::Unavailable
    } else {
        DeviceError::Proof
    }
}
pub(crate) fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn secure(mut response: Response) -> Response {
    let h = response.headers_mut();
    h.insert("cache-control", HeaderValue::from_static("no-store"));
    h.insert("pragma", HeaderValue::from_static("no-cache"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
}
pub(crate) fn router(auth: Arc<AuthService>, origins: Vec<String>) -> Result<Router, ServiceError> {
    if !auth.codec.has_eddsa() {
        return Err(ServiceError::Invalid);
    }
    let origin = origins.first().cloned().ok_or(ServiceError::Invalid)?;
    let state = DeviceAuth {
        auth,
        policy: BrowserPolicy::new(origins)?,
        origin,
    };
    Ok(Router::new()
        .route("/api/v1/auth/device/authorize", post(authorize))
        .route("/api/v1/auth/device/token", post(token))
        .route("/api/v1/auth/device/revoke", post(revoke))
        .route("/api/auth/device", get(page).post(decide))
        .route("/api/auth/device/request", get(inspect))
        .layer(DefaultBodyLimit::max(8192))
        .with_state(state))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizeInput {
    pub client_id: String,
    #[serde(default)]
    pub scope: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenInput {
    pub grant_type: String,
    pub client_id: String,
    pub device_code: Option<String>,
    pub code_verifier: Option<String>,
    pub refresh_token: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeInput {
    pub refresh_token: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionInput {
    pub user_code: String,
    pub approve: bool,
}
#[derive(Deserialize)]
pub struct InspectInput {
    pub user_code: String,
}
fn native(headers: &HeaderMap) -> Result<(), DeviceError> {
    // These public-client endpoints never accept ambient browser credentials.
    if ["cookie", "origin", "sec-fetch-site", "authorization"]
        .iter()
        .any(|h| headers.contains_key(*h))
    {
        Err(DeviceError::Invalid)
    } else {
        Ok(())
    }
}
fn client(id: &str) -> Result<(), DeviceError> {
    if id == CLIENT_ID {
        Ok(())
    } else {
        Err(DeviceError::InvalidClient)
    }
}
fn scope(value: &str) -> Result<String, DeviceError> {
    if value.is_empty() || value == "platform" {
        Ok("platform".into())
    } else {
        Err(DeviceError::Invalid)
    }
}
fn challenge(value: &str) -> bool {
    value.len() == 43
        && URL_SAFE_NO_PAD
            .decode(value)
            .is_ok_and(|v| v.len() == 32 && URL_SAFE_NO_PAD.encode(v) == value)
}
fn pkce(verifier: &str) -> Result<String, DeviceError> {
    if !(43..=128).contains(&verifier.len())
        || !verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
    {
        return Err(DeviceError::InvalidGrant);
    }
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())))
}
fn user_code(value: &str) -> Result<String, DeviceError> {
    let normalized = value.to_ascii_uppercase();
    let normalized = normalized.replace('-', "");
    if normalized.len() != 10 || !normalized.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(DeviceError::Invalid);
    }
    Ok(normalized)
}
fn refresh_parts(value: &str) -> Result<(&str, String), DeviceError> {
    let mut p = value.split('.');
    if p.next() != Some("adfr") {
        return Err(DeviceError::InvalidGrant);
    }
    let id = p.next().ok_or(DeviceError::InvalidGrant)?;
    let secret = p.next().ok_or(DeviceError::InvalidGrant)?;
    if uuid::Uuid::parse_str(id).is_err()
        || secret.len() != 64
        || !secret
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || p.next().is_some()
    {
        return Err(DeviceError::InvalidGrant);
    }
    Ok((id, digest(value)))
}
fn device_code(value: &str) -> Result<(), DeviceError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(DeviceError::InvalidGrant)
    }
}
async fn authorize(
    State(state): State<DeviceAuth>,
    headers: HeaderMap,
    Json(input): Json<AuthorizeInput>,
) -> Result<Response, DeviceError> {
    native(&headers)?;
    client(&input.client_id)?;
    let scope = scope(&input.scope)?;
    if input.code_challenge_method != "S256" || !challenge(&input.code_challenge) {
        return Err(DeviceError::Invalid);
    }
    let store = state.auth.sessions.device_store();
    store.rate("authorize-global", 500).await?;
    store.rate(&input.code_challenge, 5).await?;
    let now = (state.auth.clock)().map_err(failure)?;
    let code = opaque();
    let user = opaque()[..10].to_ascii_uppercase();
    let shown = format!("{}-{}", &user[..5], &user[5..]);
    let flow = Flow {
        challenge: input.code_challenge,
        client_id: input.client_id,
        scope,
        user_code_hash: digest(&user),
        expires_at: now
            .checked_add(DEVICE_TTL)
            .ok_or(DeviceError::Unavailable)?,
        last_poll: 0,
        interval: 5,
        status: "pending".into(),
        binding: None,
    };
    store.create(&code, &user, &flow).await?;
    let uri = format!("{}/api/auth/device", state.origin);
    Ok(secure(Json(json!({"device_code":code,"user_code":shown,"verification_uri":uri,"verification_uri_complete":format!("{uri}?user_code={shown}"),"expires_in":DEVICE_TTL,"interval":5})).into_response()))
}
impl DeviceAuth {
    async fn browser(
        &self,
        headers: &HeaderMap,
        method: &Method,
    ) -> Result<(BrowserSession, Principal), DeviceError> {
        let id = session_cookie(headers)
            .map_err(failure)?
            .ok_or(DeviceError::Proof)?;
        let session = self
            .auth
            .sessions
            .browser(&id)
            .await
            .map_err(|_| DeviceError::Unavailable)?
            .ok_or(DeviceError::Proof)?;
        self.policy
            .check(method, headers, &session.csrf)
            .map_err(|_| DeviceError::Denied)?;
        let p = self
            .auth
            .authenticate_browser(&session)
            .await
            .map_err(failure)?;
        if p.account.force_password_change {
            return Err(DeviceError::Denied);
        }
        Ok((session, p))
    }
    async fn parent(&self, binding: &Binding) -> Result<(Principal, Value), DeviceError> {
        let p = self
            .auth
            .authenticate_claims(binding.claims())
            .await
            .map_err(failure)?;
        let record = self
            .auth
            .sessions
            .record(&binding.parent_jti)
            .await
            .map_err(|_| DeviceError::Unavailable)?
            .ok_or(DeviceError::InvalidGrant)?;
        if record.get("credential_revision") != Some(&binding.credential_revision)
            || record.get("auth_epoch") != binding.auth_epoch.as_ref()
            || p.account.force_password_change
        {
            return Err(DeviceError::InvalidGrant);
        }
        Ok((p, record))
    }
    fn access(&self, p: &Principal, jti: &str) -> Result<String, DeviceError> {
        self.auth
            .codec
            .issue_for_session(
                Claims {
                    sub: p.account.user_id.clone(),
                    user_id: p.account.user_id.clone(),
                    tenant_id: p.account.tenant_id.clone(),
                    email: p.account.email.clone(),
                    roles: p.authority.roles.clone(),
                    permissions: p.authority.permissions.clone(),
                    tier: p.authority.tier.clone(),
                    exp: 0,
                    iat: 0,
                    nbf: None,
                    jti: String::new(),
                    aud: None,
                    iss: None,
                },
                (self.auth.clock)().map_err(failure)?,
                jti,
            )
            .map_err(|_| DeviceError::Unavailable)
    }
}
impl Binding {
    fn claims(&self) -> Claims {
        Claims {
            sub: self.user_id.clone(),
            user_id: self.user_id.clone(),
            tenant_id: self.tenant_id.clone(),
            email: None,
            roles: vec![],
            permissions: vec![],
            tier: String::new(),
            exp: self.expires_at,
            iat: self.issued_at,
            nbf: None,
            jti: self.parent_jti.clone(),
            aud: None,
            iss: None,
        }
    }
}
async fn inspect(
    State(state): State<DeviceAuth>,
    headers: HeaderMap,
    Query(input): Query<InspectInput>,
) -> Result<Response, DeviceError> {
    let (_, p) = state.browser(&headers, &Method::GET).await?;
    let store = state.auth.sessions.device_store();
    store.rate(&format!("inspect:{}", p.jti), 30).await?;
    let (_, _, flow) = store.by_user(&user_code(&input.user_code)?).await?;
    if flow.expires_at <= (state.auth.clock)().map_err(failure)? || flow.status != "pending" {
        return Err(DeviceError::Expired);
    }
    Ok(secure(Json(json!({"client_id":flow.client_id,"scope":flow.scope,"user_id":p.account.user_id,"tenant_id":p.account.tenant_id,"expires_at":flow.expires_at})).into_response()))
}
async fn decide(
    State(state): State<DeviceAuth>,
    headers: HeaderMap,
    Json(input): Json<DecisionInput>,
) -> Result<Response, DeviceError> {
    let (session, p) = state.browser(&headers, &Method::POST).await?;
    let store = state.auth.sessions.device_store();
    store.rate(&format!("decision:{}", p.jti), 10).await?;
    let (id, old, mut flow) = store.by_user(&user_code(&input.user_code)?).await?;
    let now = (state.auth.clock)().map_err(failure)?;
    if input.approve {
        if state.auth.enterprise_policy_enabled() {
            let id = session_cookie(&headers)
                .map_err(failure)?
                .ok_or(DeviceError::Proof)?;
            match crate::enterprise::step_up::recent(&state.auth, &p, Some((&id, &session))).await {
                Ok(true) => {}
                Ok(false) => return Err(DeviceError::StepUp),
                Err(crate::enterprise::EnterpriseError::Unavailable) => {
                    return Err(DeviceError::Unavailable);
                }
                Err(_) => return Err(DeviceError::Proof),
            }
        }
        let record = state
            .auth
            .sessions
            .record(&p.jti)
            .await
            .map_err(|_| DeviceError::Unavailable)?
            .ok_or(DeviceError::Proof)?;
        flow.binding = Some(Binding {
            parent_jti: p.jti,
            user_id: p.account.user_id,
            tenant_id: p.account.tenant_id,
            issued_at: session.issued_at,
            expires_at: session.expires_at,
            credential_revision: session.credential_revision,
            auth_epoch: record.get("auth_epoch").cloned(),
        });
        flow.status = "approved".into();
    } else {
        flow.status = "denied".into()
    }
    store.decide(&id, &old, &flow, now).await?;
    Ok(secure(
        Json(json!({"approved":input.approve})).into_response(),
    ))
}
async fn token(
    State(state): State<DeviceAuth>,
    headers: HeaderMap,
    Json(input): Json<TokenInput>,
) -> Result<Response, DeviceError> {
    native(&headers)?;
    client(&input.client_id)?;
    let now = (state.auth.clock)().map_err(failure)?;
    let store = state.auth.sessions.device_store();
    let (access, refresh, binding, scope) = match input.grant_type.as_str() {
        DEVICE_GRANT => {
            if input.refresh_token.is_some() {
                return Err(DeviceError::Invalid);
            }
            let code = input.device_code.ok_or(DeviceError::Invalid)?;
            device_code(&code)?;
            let proof = pkce(&input.code_verifier.ok_or(DeviceError::Invalid)?)?;
            let (old, flow) = store.poll(&code, &proof, now).await?;
            if flow.client_id != input.client_id {
                return Err(DeviceError::InvalidGrant);
            }
            let binding = flow.binding.clone().ok_or(DeviceError::InvalidGrant)?;
            let (p, parent) = state.parent(&binding).await?;
            let id = uuid::Uuid::new_v4().to_string();
            let jti = uuid::Uuid::new_v4().to_string();
            let refresh = format!("adfr.{id}.{}", opaque());
            let hash = digest(&refresh);
            let grant = RefreshGrant {
                id,
                jti: jti.clone(),
                client_id: input.client_id,
                scope: flow.scope.clone(),
                refresh_hash: hash.clone(),
                revocation_hashes: vec![hash],
                binding: binding.clone(),
            };
            let access = state.access(&p, &jti)?;
            let mut record = json!({"user_id":binding.user_id,"tenant_id":binding.tenant_id,"credential_revision":binding.credential_revision,"device_parent_jti":binding.parent_jti,"device_parent_expires_at":binding.expires_at});
            if let Some(epoch) = &binding.auth_epoch {
                record["auth_epoch"] = epoch.clone()
            }
            store
                .complete(&code, &old, &flow, &grant, &record, &parent, now)
                .await?;
            (access, refresh, binding, flow.scope)
        }
        "refresh_token" => {
            if input.device_code.is_some() || input.code_verifier.is_some() {
                return Err(DeviceError::Invalid);
            }
            let token = input.refresh_token.ok_or(DeviceError::Invalid)?;
            let (id, hash) = refresh_parts(&token)?;
            store.rate(&format!("refresh:{id}"), 30).await?;
            let (old, mut grant) = store.grant(id).await?;
            if grant.client_id != input.client_id {
                return Err(DeviceError::InvalidGrant);
            }
            if grant.refresh_hash != hash {
                // Reuse of a rotated credential indicates replay. Revoke its
                // family atomically; a random wrong secret cannot revoke it.
                if grant.revocation_hashes.contains(&hash) {
                    store.revoke(id, &hash).await?;
                }
                return Err(DeviceError::InvalidGrant);
            }
            if grant.revocation_hashes.len() >= 128 {
                return Err(DeviceError::Rate);
            }
            let (p, parent) = state.parent(&grant.binding).await?;
            // Existing allowlist and current authority are checked before rotation.
            let mut claims = grant.binding.claims();
            claims.jti = grant.jti.clone();
            state
                .auth
                .authenticate_claims(claims)
                .await
                .map_err(failure)?;
            let access = state.access(&p, &grant.jti)?;
            let refresh = format!("adfr.{id}.{}", opaque());
            grant.refresh_hash = digest(&refresh);
            grant.revocation_hashes.push(grant.refresh_hash.clone());
            store.rotate(&old, &grant, &parent, now).await?;
            (access, refresh, grant.binding, grant.scope)
        }
        _ => return Err(DeviceError::Invalid),
    };
    Ok(secure(Json(json!({"access_token":access,"token_type":"Bearer","expires_in":crate::access_tokens::ACCESS_TOKEN_TTL.min(binding.expires_at.saturating_sub(now)),"refresh_token":refresh,"scope":scope,"user_id":binding.user_id,"tenant_id":binding.tenant_id})).into_response()))
}
async fn revoke(
    State(state): State<DeviceAuth>,
    headers: HeaderMap,
    Json(input): Json<RevokeInput>,
) -> Result<Response, DeviceError> {
    native(&headers)?;
    if let Ok((id, hash)) = refresh_parts(&input.refresh_token) {
        state.auth.sessions.device_store().revoke(id, &hash).await?
    }
    Ok(secure(StatusCode::NO_CONTENT.into_response()))
}
// Called by the ordinary token verifier: a child session never outlives or
// restores a revoked browser grant. Missing/malformed authority fails closed.
pub(crate) async fn validate_parent(
    auth: &AuthService,
    record: &Value,
    now: u64,
) -> Result<(), ServiceError> {
    let Some(parent) = record.get("device_parent_jti") else {
        return Ok(());
    };
    let parent = parent.as_str().ok_or(ServiceError::Credentials)?;
    if record
        .get("device_parent_expires_at")
        .and_then(Value::as_u64)
        .is_none_or(|expires| expires <= now)
    {
        return Err(ServiceError::Credentials);
    }
    let source = auth
        .sessions
        .record(parent)
        .await
        .map_err(|_| ServiceError::Unavailable)?
        .ok_or(ServiceError::Credentials)?;
    if source.get("device_parent_jti").is_some()
        || source.get("user_id") != record.get("user_id")
        || source.get("credential_revision") != record.get("credential_revision")
        || source.get("auth_epoch") != record.get("auth_epoch")
    {
        return Err(ServiceError::Credentials);
    }
    Ok(())
}
async fn page() -> Response {
    let mut response = secure(Html(include_str!("device_flow/approval.html")).into_response());
    response.headers_mut().insert("content-security-policy",HeaderValue::from_static("default-src 'none'; script-src 'sha256-86OxvkS8QpkMRSE+1u+6k9ZcTSoxRcme1394VObBk7A='; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"));
    response
}
#[cfg(test)]
#[path = "device_flow/tests.rs"]
mod tests;
