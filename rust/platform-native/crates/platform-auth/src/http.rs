//! Router factory only; never opens a listener or loads production configuration.
use crate::contract::{self, CurrentUserResponse, LoginInput, PasswordInput, ValidationResponse};
use crate::{
    JwtKeyRing,
    accounts::AuditContext,
    service::{AuthService, ServiceError},
};
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    sync::Arc,
};
#[derive(Clone)]
struct HttpState {
    service: Arc<AuthService>,
    configured_roles: Arc<BTreeMap<String, Vec<String>>>,
    introspection: Option<Arc<dyn CredentialIntrospectionResolver>>,
}
pub fn router(
    service: Arc<AuthService>,
    configured_roles: BTreeMap<String, Vec<String>>,
) -> Router {
    build_router(service, configured_roles, None)
}
/// Read-only credential metadata. This must never produce a request Principal.
pub trait CredentialIntrospectionResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<CurrentUserResponse, ServiceError>> + Send + 'a,
        >,
    >;
}
pub fn router_with_introspection(
    service: Arc<AuthService>,
    configured_roles: BTreeMap<String, Vec<String>>,
    resolver: Arc<dyn CredentialIntrospectionResolver>,
) -> Router {
    build_router(service, configured_roles, Some(resolver))
}
fn build_router(
    service: Arc<AuthService>,
    configured_roles: BTreeMap<String, Vec<String>>,
    introspection: Option<Arc<dyn CredentialIntrospectionResolver>>,
) -> Router {
    let jwks = jwks_router(service.jwt_keyring());
    Router::new()
        .route(contract::LOGIN, post(login))
        .route(contract::LOGOUT, post(logout))
        .route(contract::ME, get(me))
        .route(contract::VALIDATE, post(validate))
        .route(contract::CHANGE_PASSWORD, post(change_password))
        .layer(DefaultBodyLimit::max(contract::BODY_LIMIT))
        .with_state(HttpState {
            service,
            introspection,
            configured_roles: Arc::new(configured_roles),
        })
        .merge(jwks)
}

/// Public standards-compatible key discovery. The verifier ring owns no public
/// signing API, and this projection serializes only normalized public JWKs.
pub fn jwks_router(keyring: Arc<JwtKeyRing>) -> Router {
    Router::new()
        .route(contract::JWKS, get(jwks))
        .with_state(keyring)
}

async fn jwks(State(keyring): State<Arc<JwtKeyRing>>) -> Response {
    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"detail":"Authentication authority unavailable"})),
            )
                .into_response();
        }
    };
    let mut response = Json(keyring.jwks_at(now)).into_response();
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60"),
    );
    response
}
#[derive(Debug)]
enum ApiError {
    Service(ServiceError),
    Body(StatusCode),
}
fn json_body<T>(
    body: Result<Json<T>, axum::extract::rejection::JsonRejection>,
) -> Result<T, ApiError> {
    body.map(|Json(value)| value)
        .map_err(|error| ApiError::Body(error.status()))
}
impl From<ServiceError> for ApiError {
    fn from(e: ServiceError) -> Self {
        Self::Service(e)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, detail) = match self {
            Self::Service(error) => (
                StatusCode::from_u16(error.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                error.to_string(),
            ),
            Self::Body(status) => (status, "Invalid request body".to_owned()),
        };
        (status, Json(json!({"detail":detail}))).into_response()
    }
}
fn bearer(headers: &HeaderMap) -> Result<&str, ApiError> {
    if headers.get_all("authorization").iter().count() != 1 {
        return Err(ServiceError::Credentials.into());
    }
    let header = headers
        .get("authorization")
        .and_then(|s| s.to_str().ok())
        .ok_or(ServiceError::Credentials)?;
    let mut parts = header.split_whitespace();
    let scheme = parts.next().ok_or(ServiceError::Credentials)?;
    let token = parts.next().ok_or(ServiceError::Credentials)?;
    if !scheme.eq_ignore_ascii_case("bearer") || parts.next().is_some() {
        return Err(ServiceError::Credentials.into());
    }
    Ok(token)
}
fn audit(headers: &HeaderMap, peer: Option<ConnectInfo<SocketAddr>>) -> AuditContext {
    AuditContext {
        ip: peer
            .map(|p| p.0.ip().to_string())
            .unwrap_or_else(|| "unknown".into()),
        user_agent: headers
            .get("user-agent")
            .and_then(|s| s.to_str().ok())
            .unwrap_or("")
            .chars()
            .take(1024)
            .collect(),
    }
}
async fn login(
    State(s): State<HttpState>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
    headers: HeaderMap,
    body: Result<Json<LoginInput>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<crate::service::LoginResponse>, ApiError> {
    let body = json_body(body)?;
    Ok(Json(
        s.service
            .login(&body.email, body.password, &audit(&headers, peer.ok()))
            .await?,
    ))
}
async fn logout(
    State(s): State<HttpState>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    if !headers.contains_key("authorization") {
        return Ok(Json(contract::logout_response(false)));
    }
    s.service
        .logout(bearer(&headers)?, &audit(&headers, peer.ok()))
        .await?;
    Ok(Json(contract::logout_response(true)))
}
fn effective_permissions(
    explicit: &[String],
    roles: &[String],
    configured: &BTreeMap<String, Vec<String>>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut result = Vec::new();
    for value in explicit
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .chain(roles.iter().map(|r| r.trim()).filter(|r| r.contains(':')))
        .chain(
            roles
                .iter()
                .flat_map(|r| configured.get(r).into_iter().flatten())
                .map(String::as_str),
        )
    {
        if seen.insert(value.to_owned()) {
            result.push(value.to_owned());
        }
    }
    result
}
async fn me(State(s): State<HttpState>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    if let Some(resolver) = &s.introspection {
        let mut response = resolver.resolve(&headers).await?;
        response.effective_permissions =
            effective_permissions(&response.permissions, &response.roles, &s.configured_roles);
        return Ok(Json(json!(response)));
    }
    let p = s.service.authenticate(bearer(&headers)?).await?;
    let effective = effective_permissions(
        &p.authority.permissions,
        &p.authority.roles,
        &s.configured_roles,
    );
    Ok(Json(json!(CurrentUserResponse {
        user_id: p.account.user_id,
        tenant_id: p.account.tenant_id,
        email: p.account.email,
        display_name: p.account.display_name,
        department: p.account.department,
        roles: p.authority.roles,
        permissions: p.authority.permissions,
        effective_permissions: effective,
        tier: p.authority.tier,
        force_password_change: p.account.force_password_change,
    })))
}
fn validated_projection(authority: crate::accounts::Authority) -> (Vec<String>, String) {
    let mut roles = authority.roles;
    for permission in authority.permissions {
        if !roles.contains(&permission) {
            roles.push(permission);
        }
    }
    (roles, authority.tier)
}
async fn validate(State(s): State<HttpState>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    let p = s.service.authenticate(bearer(&headers)?).await?;
    let (roles, tier) = validated_projection(p.authority);
    Ok(Json(json!(ValidationResponse {
        valid: true,
        user_id: p.account.user_id,
        roles,
        tier
    })))
}
async fn change_password(
    State(s): State<HttpState>,
    peer: Result<ConnectInfo<SocketAddr>, axum::extract::rejection::ExtensionRejection>,
    headers: HeaderMap,
    body: Result<Json<PasswordInput>, axum::extract::rejection::JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let body = json_body(body)?;
    s.service
        .change_password(
            bearer(&headers)?,
            body.current_password,
            body.new_password,
            &body.confirm_password,
            &audit(&headers, peer.ok()),
        )
        .await?;
    Ok(Json(contract::password_changed_response()))
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn bearer_parser_preserves_optional_logout_and_strict_other_admission() {
        let mut headers = HeaderMap::new();
        assert!(bearer(&headers).is_err());
        for value in ["Basic token", "Bearer", "Bearer one two"] {
            headers.insert("authorization", value.parse().unwrap());
            assert!(bearer(&headers).is_err());
        }
        headers.insert("authorization", "bEaReR fixture-token".parse().unwrap());
        assert_eq!(bearer(&headers).unwrap(), "fixture-token");
        headers.append("authorization", "Bearer another".parse().unwrap());
        assert!(bearer(&headers).is_err());
    }

    #[test]
    fn body_error_status_and_effective_authority_projection_are_unchanged() {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::PAYLOAD_TOO_LARGE,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            StatusCode::UNPROCESSABLE_ENTITY,
        ] {
            assert_eq!(ApiError::Body(status).into_response().status(), status);
        }
        let configured = BTreeMap::from([("user".into(), vec!["read".into(), "write".into()])]);
        assert_eq!(
            effective_permissions(
                &[" read ".into()],
                &["user".into(), "tools:use".into()],
                &configured
            ),
            vec!["read", "tools:use", "write"]
        );
    }

    #[test]
    fn current_role_does_not_promote_the_account_tier_in_validation() {
        let (roles, tier) = validated_projection(crate::accounts::Authority {
            roles: vec!["admin".into()],
            tier: "normal".into(),
            permissions: vec!["console:users:view".into()],
        });

        assert_eq!(roles, vec!["admin", "console:users:view"]);
        assert_eq!(tier, "normal");
    }

    #[tokio::test]
    async fn jwks_handler_returns_only_public_keys() {
        let config = crate::JwtKeyConfig {
            kid: "http-test-key".into(),
            algorithm: "EdDSA".into(),
            public_jwk: json!({
                "kty":"OKP",
                "crv":"Ed25519",
                "alg":"EdDSA",
                "use":"sig",
                "key_ops":["verify"],
                "kid":"http-test-key",
                "x":"pfn5-ZeMp4J4fOvlMv-I86va0mSjqPxX0xdwvEHc_oI"
            }),
            private_key_pem: Some(
                include_str!("../tests/fixtures/jwt-rotation-old-ed25519-private.pem").to_owned(),
            ),
            verify_until: None,
        };
        let codec = crate::TokenCodec::from_keyring(
            crate::JwtSigningMode::AsymmetricKeyring,
            b"",
            Some("http-test-key"),
            &[config],
            None,
            None,
        )
        .unwrap();
        let response = jwks(State(codec.keyring())).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "public, max-age=60");
        let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024)
            .await
            .unwrap();
        let document: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(document["keys"].as_array().unwrap().len(), 1);
        assert_eq!(document["keys"][0]["kid"], "http-test-key");
        assert_eq!(document["keys"][0]["key_ops"], json!(["verify"]));
        for private_field in ["d", "p", "q", "dp", "dq", "qi", "oth", "k"] {
            assert!(document["keys"][0].get(private_field).is_none());
        }
    }
}
