//! Process-owned transport trust. No workload identity grants actor/tenant access.
use crate::{KnowledgeError, Result};
use ai_platform_auth::workload::{WorkloadAuth, WorkloadAuthentication};
use ai_platform_service_primitives::workload_identity::{
    Mode, PeerRules, ServiceIdentity, VerifiedPeer,
    http::WorkloadHttp,
    listener::install_peer,
    secrets::{CredentialFiles, environment_secret},
    tls::{ClientCredentials, ServerCredentials},
};
use axum::{
    Router,
    extract::{MatchedPath, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use std::{collections::BTreeMap, net::SocketAddr, sync::Arc, time::Duration};

#[derive(Clone)]
pub struct Configuration {
    gate: Arc<WorkloadAuth>,
    legacy: Option<String>,
    bind: Option<String>,
    credentials: Option<ServerCredentials>,
    outbound: Option<Arc<WorkloadHttp>>,
    health_origin: Option<reqwest::Url>,
}
pub(crate) fn file_secret(env: &BTreeMap<String, String>, key: &str) -> Result<Option<String>> {
    environment_secret(env, key)
        .map_err(|_| KnowledgeError::Configuration("workload_file_secret"))?
        .map(|secret| {
            String::from_utf8(secret.expose().to_vec())
                .map_err(|_| KnowledgeError::Configuration("workload_secret_encoding"))
        })
        .transpose()
}
fn invalid() -> KnowledgeError {
    KnowledgeError::Configuration("workload_configuration")
}
impl Configuration {
    pub fn from_values(env: &BTreeMap<String, String>) -> Result<Self> {
        let get = |key: &str| env.get(key).filter(|s| !s.is_empty()).ok_or_else(invalid);
        let mode = Mode::parse(
            env.get("AI_PLATFORM_WORKLOAD_MTLS_MODE")
                .map(String::as_str)
                .unwrap_or("disabled"),
        )
        .map_err(|_| invalid())?;
        let allowed = [
            "AI_PLATFORM_WORKLOAD_MTLS_MODE",
            "AI_PLATFORM_WORKLOAD_TLS_BIND",
            "AI_PLATFORM_WORKLOAD_CREDENTIALS_DIR",
            "AI_PLATFORM_WORKLOAD_SPIFFE_ID",
            "AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS",
            "AI_PLATFORM_WORKLOAD_ALLOWED_ORIGINS",
        ];
        if env
            .keys()
            .any(|key| key.starts_with("AI_PLATFORM_WORKLOAD_") && !allowed.contains(&key.as_str()))
        {
            return Err(invalid());
        }
        let (rules, bind, credentials, outbound) = if mode == Mode::Disabled {
            if env
                .keys()
                .any(|key| key.starts_with("AI_PLATFORM_WORKLOAD_") && key != allowed[0])
            {
                return Err(invalid());
            }
            // Unused by disabled bearer admission; there is no certificate path.
            let rules = PeerRules::new([ServiceIdentity::parse(
                "spiffe://agentforge.internal/services/knowledge-disabled",
            )
            .map_err(|_| invalid())?])
            .map_err(|_| invalid())?;
            (rules, None, None, None)
        } else {
            let identities = get(allowed[4])?
                .split(',')
                .map(ServiceIdentity::parse)
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| invalid())?;
            let rules = PeerRules::new(identities).map_err(|_| invalid())?;
            let files = CredentialFiles::new(get(allowed[2])?.into()).map_err(|_| invalid())?;
            let identity = ServiceIdentity::parse(get(allowed[3])?).map_err(|_| invalid())?;
            let credentials =
                ServerCredentials::new(files.clone(), identity.clone(), rules.clone(), mode)
                    .map_err(|_| invalid())?;
            // No default/wildcard origin: every credential-bearing destination is explicit.
            let pairs: BTreeMap<String, String> =
                serde_json::from_str(get(allowed[5])?).map_err(|_| invalid())?;
            if pairs.is_empty() {
                return Err(invalid());
            }
            let peers = pairs
                .into_iter()
                .map(|(origin, identity)| ServiceIdentity::parse(&identity).map(|id| (origin, id)))
                .collect::<std::result::Result<BTreeMap<_, _>, _>>()
                .map_err(|_| invalid())?;
            let http = WorkloadHttp::new(
                ClientCredentials::new(files, identity),
                peers,
                Duration::from_secs(5),
                Duration::from_secs(3600),
            )
            .map_err(|_| invalid())?;
            // Both actual internal callers must be admitted before resources open.
            for key in ["GATEWAY_URL", "KNOWLEDGE_MODEL_PLANE__URL"] {
                let value = env
                    .get(key)
                    .or_else(|| {
                        if key == "KNOWLEDGE_MODEL_PLANE__URL" {
                            env.get("GATEWAY_URL")
                        } else {
                            None
                        }
                    })
                    .ok_or_else(invalid)?;
                http.validate_endpoint(value).map_err(|_| invalid())?;
            }
            (
                rules,
                Some(get(allowed[1])?.to_owned()),
                Some(credentials),
                Some(Arc::new(http)),
            )
        };
        let gate = Arc::new(WorkloadAuth::from_environment(env, rules).map_err(|_| invalid())?);
        let legacy = if mode == Mode::Required {
            None
        } else {
            file_secret(env, "AI_PLATFORM_INTERNAL_TOKEN")?
        };
        let health_origin = env
            .get("KNOWLEDGE_GATEWAY_HEALTH_ORIGIN")
            .map(|value| crate::endpoint(value, "gateway_health_origin"))
            .transpose()?;
        // The private TLS facade intentionally excludes public health. Keep a
        // separately configured ordinary, certificate-free readiness client.
        if mode != Mode::Disabled && health_origin.is_none() {
            return Err(KnowledgeError::Configuration(
                "gateway_health_origin_required",
            ));
        }
        Ok(Self {
            gate,
            legacy,
            bind,
            credentials,
            outbound,
            health_origin,
        })
    }
    pub fn mode(&self) -> Mode {
        self.gate.mode()
    }
    pub(crate) fn legacy_secret(&self) -> Option<&str> {
        self.legacy.as_deref()
    }
    pub(crate) fn outbound(&self) -> Option<Arc<WorkloadHttp>> {
        self.outbound.clone()
    }
    pub(crate) fn health_origin(&self) -> Option<reqwest::Url> {
        self.health_origin.clone()
    }
    pub async fn listener(
        &self,
    ) -> Result<Option<ai_platform_service_primitives::workload_identity::listener::WorkloadListener>>
    {
        let Some(bind) = &self.bind else {
            return Ok(None);
        };
        let address = if let Ok(address) = bind.parse::<SocketAddr>() {
            address
        } else {
            let (host, port) = bind.rsplit_once(':').ok_or_else(invalid)?;
            if host.is_empty()
                || !host
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b".-".contains(&c))
                || port.parse::<u16>().is_err()
            {
                return Err(invalid());
            }
            let mut addresses = tokio::net::lookup_host(bind)
                .await
                .map_err(|_| KnowledgeError::Startup("workload_bind_dns"))?
                .collect::<Vec<_>>();
            addresses.sort();
            addresses.dedup();
            if addresses.len() != 1 {
                return Err(KnowledgeError::Startup("workload_bind_dns_ambiguous"));
            }
            addresses[0]
        };
        let credentials = self.credentials.clone().ok_or_else(invalid)?;
        ai_platform_service_primitives::workload_identity::listener::WorkloadListener::bind(
            address,
            credentials,
        )
        .await
        .map(Some)
        .map_err(|_| KnowledgeError::Startup("workload_tls_listener"))
    }
}
const CAPABILITY_ROUTE: &str = "/internal/v2/capabilities/knowledge/{dataset_id}/retrieve";
// These /api paths are the Knowledge *service's* private signed Gateway wire,
// not Gateway's public JWT API. Retain their canonical path/signature bytes on
// TLS rather than rewriting them into an alias and invalidating actor proofs.
fn business_path(path: &str) -> bool {
    path.starts_with("/internal/")
        || path == "/api/v1/knowledge"
        || path.starts_with("/api/v1/knowledge/")
        || [
            "/api/v1/internal/knowledge/datasets/authorize",
            "/api/v1/internal/knowledge/documents/authorize",
            "/api/v1/internal/knowledge/document-sources/authorize",
            "/api/v1/internal/eval/ragas",
        ]
        .contains(&path)
}
fn listed(method: &str, matched: &str, path: &str) -> bool {
    !path.contains(['%', '\\'])
        && !path.contains("//")
        && !path.split('/').any(|part| part == "." || part == "..")
        && if [
            CAPABILITY_ROUTE,
            ai_platform_service_primitives::knowledge_model::SCOPE_CHECK_PATH,
        ]
        .contains(&matched)
        {
            method == "POST"
        } else {
            // MatchedPath must come from the already assembled signed business
            // router. Unknown paths, probes, metrics and release metadata stay out.
            !matched.is_empty()
                && !matched.starts_with("/internal/")
                && business_path(matched)
                && ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"].contains(&method)
        }
}
async fn admit(State(config): State<Configuration>, mut request: Request, next: Next) -> Response {
    if !business_path(request.uri().path()) {
        return next.run(request).await;
    }
    // Disabled preserves the canonical signed-only service API protocol.
    // Capability's original transport guard becomes the typed proof below.
    if config.mode() == Mode::Disabled
        && !request
            .uri()
            .path()
            .starts_with("/internal/v2/capabilities/")
    {
        return next.run(request).await;
    }
    request.extensions_mut().remove::<WorkloadAuthentication>();
    if config.mode() == Mode::Required
        && (request.headers().contains_key("authorization")
            || request
                .headers()
                .contains_key("x-ai-platform-internal-token"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut headers = request.headers().clone();
    // Consume the historic KS transport field only at this boundary, without
    // changing the request passed to business handlers or synthesizing its proof.
    if config.mode() != Mode::Required && !headers.contains_key("authorization") {
        if headers
            .get_all("x-ai-platform-internal-token")
            .iter()
            .count()
            == 1
        {
            if let Some(token) = headers
                .get("x-ai-platform-internal-token")
                .and_then(|h| h.to_str().ok())
            {
                if let Ok(value) = format!("Bearer {token}").parse() {
                    headers.insert("authorization", value);
                }
            }
        }
    }
    match config
        .gate
        .authenticate(&headers, request.extensions().get::<VerifiedPeer>())
    {
        Ok(proof) => {
            request.extensions_mut().insert(proof);
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
}
/// Wrap *both* listeners with the same transport admission before signatures.
pub(crate) fn protect(router: Router, config: Configuration) -> Router {
    router
        .layer(middleware::from_fn_with_state(config, admit))
        .layer(middleware::from_fn(install_peer))
}
async fn internal_only(request: Request, next: Next) -> Response {
    let matched = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or("");
    if !listed(request.method().as_str(), matched, request.uri().path()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(request).await
}
/// Reuse only the assembled private, signed Knowledge business routes on TLS.
/// Public Gateway API routes are never assembled in this process.
pub fn internal_router(router: Router) -> Router {
    router.layer(middleware::from_fn(internal_only))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn facade_inventory_is_narrow() {
        assert!(listed(
            "POST",
            CAPABILITY_ROUTE,
            "/internal/v2/capabilities/knowledge/d/retrieve"
        ));
        assert!(!listed(
            "GET",
            CAPABILITY_ROUTE,
            "/internal/v2/capabilities/knowledge/d/retrieve"
        ));
        assert!(!listed(
            "POST",
            CAPABILITY_ROUTE,
            "/internal/v2/capabilities/knowledge/%64/retrieve"
        ));
        assert!(listed(
            "POST",
            "/api/v1/knowledge/{dataset_id}/retrieve",
            "/api/v1/knowledge/d/retrieve"
        ));
        assert!(!listed("POST", "/api/v1/users", "/api/v1/users"));
    }
    #[test]
    fn required_rejects_retained_bearer_configuration() {
        let mut env = required_values();
        env.insert("AI_PLATFORM_INTERNAL_TOKEN".into(), "old-bearer".into());
        assert!(Configuration::from_values(&env).is_err());
    }
    fn required() -> Configuration {
        let rules = PeerRules::new([ServiceIdentity::parse(
            "spiffe://agentforge.internal/services/gateway",
        )
        .unwrap()])
        .unwrap();
        Configuration {
            gate: Arc::new(WorkloadAuth::new(Mode::Required, None, rules).unwrap()),
            legacy: None,
            bind: None,
            credentials: None,
            outbound: None,
            health_origin: None,
        }
    }
    fn business_router() -> Router {
        Router::new()
            .route(
                CAPABILITY_ROUTE,
                axum::routing::post(|| async { StatusCode::OK }),
            )
            .route(
                ai_platform_service_primitives::knowledge_model::SCOPE_CHECK_PATH,
                axum::routing::post(|| async { StatusCode::OK }),
            )
            .route(
                "/api/v1/knowledge/{dataset_id}/retrieve",
                axum::routing::post(|| async { StatusCode::OK }),
            )
            .route(
                "/api/v1/internal/knowledge/documents/authorize",
                axum::routing::post(|| async { StatusCode::OK }),
            )
            .route(
                "/api/v1/internal/eval/ragas",
                axum::routing::post(|| async { StatusCode::OK }),
            )
            .route(
                "/health/live",
                axum::routing::get(|| async { StatusCode::OK }),
            )
    }
    #[tokio::test]
    async fn required_rejects_old_bearers_on_both_router_copies() {
        use tower::ServiceExt;
        for tls_copy in [false, true] {
            let router = protect(business_router(), required());
            let router = if tls_copy {
                internal_router(router)
            } else {
                router
            };
            for path in [
                "/internal/v2/capabilities/knowledge/d/retrieve",
                ai_platform_service_primitives::knowledge_model::SCOPE_CHECK_PATH,
                "/api/v1/knowledge/d/retrieve",
                "/api/v1/internal/knowledge/documents/authorize",
                "/api/v1/internal/eval/ragas",
            ] {
                for header in ["authorization", "x-ai-platform-internal-token"] {
                    let response = router
                        .clone()
                        .oneshot(
                            axum::http::Request::builder()
                                .method("POST")
                                .uri(path)
                                .header(header, "Bearer retained-old-token")
                                .body(axum::body::Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(
                        response.status(),
                        StatusCode::UNAUTHORIZED,
                        "{tls_copy} {path} {header}"
                    );
                }
            }
        }
    }
    #[tokio::test]
    async fn required_denies_missing_peer_and_forged_identity() {
        use tower::ServiceExt;
        let router = protect(business_router(), required());
        for hint in [None, Some("spiffe://agentforge.internal/services/gateway")] {
            let mut request = axum::http::Request::builder()
                .method("POST")
                .uri("/internal/v2/capabilities/knowledge/d/retrieve");
            if let Some(hint) = hint {
                request = request.header("x-spiffe-id", hint);
            }
            let response = router
                .clone()
                .oneshot(request.body(axum::body::Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if hint.is_some() {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::UNAUTHORIZED
                }
            );
        }
    }
    #[tokio::test]
    async fn tls_facade_excludes_public_health_but_plain_health_remains() {
        use tower::ServiceExt;
        let router = protect(business_router(), required());
        let request = || {
            axum::http::Request::builder()
                .uri("/health/live")
                .body(axum::body::Body::empty())
                .unwrap()
        };
        assert_eq!(
            router.clone().oneshot(request()).await.unwrap().status(),
            StatusCode::OK
        );
        assert_eq!(
            internal_router(router)
                .oneshot(request())
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    #[tokio::test]
    async fn disabled_historic_header_creates_proof_without_changing_business_headers() {
        use ai_platform_service_primitives::workload_identity::secrets::SecretBytes;
        use tower::ServiceExt;
        let token = "a".repeat(32);
        let rules = PeerRules::new([ServiceIdentity::parse(
            "spiffe://agentforge.internal/services/gateway",
        )
        .unwrap()])
        .unwrap();
        let config = Configuration {
            gate: Arc::new(
                WorkloadAuth::new(
                    Mode::Disabled,
                    Some(SecretBytes::from_configuration(token.as_bytes().to_vec())),
                    rules,
                )
                .unwrap(),
            ),
            legacy: Some(token.clone()),
            bind: None,
            credentials: None,
            outbound: None,
            health_origin: None,
        };
        let router = Router::new().route(
            CAPABILITY_ROUTE,
            axum::routing::post(|request: Request| async move {
                assert!(WorkloadAuthentication::require(request.extensions()).is_ok());
                assert!(!request.headers().contains_key("authorization"));
                StatusCode::OK
            }),
        );
        let response = protect(router, config)
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/internal/v2/capabilities/knowledge/d/retrieve")
                    .header("x-ai-platform-internal-token", token)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    fn required_values() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), "required".into()),
            ("AI_PLATFORM_WORKLOAD_TLS_BIND".into(), "127.0.0.1:8443".into()),
            ("AI_PLATFORM_WORKLOAD_CREDENTIALS_DIR".into(), "/synthetic/workload".into()),
            ("AI_PLATFORM_WORKLOAD_SPIFFE_ID".into(), "spiffe://agentforge.internal/services/knowledge".into()),
            ("AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS".into(), "spiffe://agentforge.internal/services/gateway".into()),
            ("AI_PLATFORM_WORKLOAD_ALLOWED_ORIGINS".into(), r#"{"https://gateway-internal:8443":"spiffe://agentforge.internal/services/gateway"}"#.into()),
            ("GATEWAY_URL".into(), "https://gateway-internal:8443".into()),
            ("KNOWLEDGE_GATEWAY_HEALTH_ORIGIN".into(), "http://gateway:8080".into()),
        ])
    }
    #[test]
    fn outbound_certificate_transport_requires_both_actual_origins_in_exact_allowlist() {
        let mut env = required_values();
        assert!(Configuration::from_values(&env).is_ok());
        env.insert(
            "KNOWLEDGE_MODEL_PLANE__URL".into(),
            "https://public-provider.example".into(),
        );
        assert!(Configuration::from_values(&env).is_err());
        env.remove("KNOWLEDGE_MODEL_PLANE__URL");
        env.insert("GATEWAY_URL".into(), "http://gateway-internal:8443".into());
        assert!(Configuration::from_values(&env).is_err());
    }
    #[test]
    fn required_rejects_retained_token_file_without_reading_it() {
        let mut env = required_values();
        env.insert(
            "AI_PLATFORM_INTERNAL_TOKEN_FILE".into(),
            "/synthetic/retired-token".into(),
        );
        assert!(Configuration::from_values(&env).is_err());
    }
}
