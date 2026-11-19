//! Optional app-network TLS facade over the already assembled/authenticated Router.
//! This is server TLS plus existing bearer grants, not mutual TLS.
use super::local_node_tls::GatewayHttpsConfiguration;
use axum::{
    Router,
    body::Body,
    extract::Request,
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf};
pub struct Configuration {
    bind: String,
    cert: PathBuf,
    key: PathBuf,
}
impl Configuration {
    pub fn from_environment(env: &BTreeMap<String, String>) -> Result<Option<Self>, &'static str> {
        let allowed = [
            "SDK_AGENT_GATEWAY_TLS_ENABLED",
            "SDK_AGENT_GATEWAY_TLS_BIND",
            "SDK_AGENT_GATEWAY_TLS_CERT_FILE",
            "SDK_AGENT_GATEWAY_TLS_KEY_FILE",
        ];
        if env
            .keys()
            .any(|k| k.starts_with("SDK_AGENT_GATEWAY_TLS_") && !allowed.contains(&k.as_str()))
        {
            return Err("sdk_callback_tls_unknown_setting");
        }
        let enabled = match env.get(allowed[0]).map(String::as_str) {
            None | Some("false") => false,
            Some("true") => true,
            _ => return Err("sdk_callback_tls_enabled"),
        };
        if !enabled {
            if allowed[1..].iter().any(|k| env.contains_key(*k)) {
                return Err("sdk_callback_tls_disabled_with_inputs");
            }
            return Ok(None);
        }
        let get = |key: &str| {
            env.get(key)
                .filter(|v| !v.is_empty())
                .cloned()
                .ok_or("sdk_callback_tls_incomplete")
        };
        let bind = get(allowed[1])?;
        if bind != "gateway-sdk-callback:8097"
            && !bind
                .parse::<SocketAddr>()
                .is_ok_and(|a| a.ip().is_loopback() && a.port() != 0)
        {
            return Err("sdk_callback_tls_app_bind_required");
        }
        let cert = PathBuf::from(get(allowed[2])?);
        let key = PathBuf::from(get(allowed[3])?);
        if !cert.is_absolute() || !key.is_absolute() {
            return Err("sdk_callback_tls_absolute_paths_required");
        }
        Ok(Some(Self { bind, cert, key }))
    }
    pub async fn resolve(self) -> Result<GatewayHttpsConfiguration, &'static str> {
        let address = if let Ok(address) = self.bind.parse::<SocketAddr>() {
            address
        } else {
            let mut addresses: Vec<_> = tokio::net::lookup_host(&self.bind)
                .await
                .map_err(|_| "sdk_callback_tls_app_dns")?
                .collect();
            addresses.sort();
            addresses.dedup();
            // Compose assigns this alias on app only. Ambiguous/edge/public DNS
            // is refused rather than falling back to an all-interface listener.
            if addresses.len() != 1 {
                return Err("sdk_callback_tls_app_dns_ambiguous");
            }
            let address = addresses[0];
            let private = match address.ip() {
                std::net::IpAddr::V4(ip) => ip.is_private(),
                std::net::IpAddr::V6(ip) => ip.is_unique_local(),
            };
            if !private {
                return Err("sdk_callback_tls_app_dns_not_private");
            }
            address
        };
        Ok(GatewayHttpsConfiguration {
            address,
            certificate_path: self.cert,
            private_key_path: self.key,
        })
    }
}
fn allowed(method: &Method, path: &str) -> bool {
    method == Method::POST
        && matches!(
            path,
            "/internal/v1/sdk-tools/capabilities/admit"
                | "/internal/v1/sdk-tools/capabilities/execute"
                | "/internal/v1/sdk-tools/knowledge/retrieve"
                | "/internal/v1/sdk-tools/knowledge/admit"
                | "/internal/v1/sdk-tools/models/lease"
                | "/internal/v1/sdk-tools/models/execute"
                | "/internal/v1/sdk-tools/models/revocations"
        )
}
async fn scope(request: Request<Body>, next: Next) -> Response {
    if !allowed(request.method(), request.uri().path()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(request).await
}
pub fn restrict(assembled: Router) -> Router {
    assembled.layer(axum::middleware::from_fn(scope))
}
#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;
    #[test]
    fn model_plane_callbacks_preserve_exact_method_and_path_scope() {
        for path in [
            "/internal/v1/sdk-tools/models/lease",
            "/internal/v1/sdk-tools/models/execute",
            "/internal/v1/sdk-tools/models/revocations",
        ] {
            assert!(allowed(&Method::POST, path));
            assert!(!allowed(&Method::GET, path));
            assert!(!allowed(&Method::PUT, path));
            assert!(!allowed(&Method::POST, &format!("{path}/extra")));
        }
        assert!(!allowed(&Method::POST, "/internal/v1/sdk-tools/models"));
    }
    #[test]
    fn default_off_partial_unknown_public_and_relative_inputs_fail_closed() {
        assert!(
            Configuration::from_environment(&BTreeMap::new())
                .unwrap()
                .is_none()
        );
        let mut env = BTreeMap::from([("SDK_AGENT_GATEWAY_TLS_ENABLED".into(), "true".into())]);
        assert!(Configuration::from_environment(&env).is_err());
        env.insert(
            "SDK_AGENT_GATEWAY_TLS_BIND".into(),
            "gateway-sdk-callback:8097".into(),
        );
        env.insert("SDK_AGENT_GATEWAY_TLS_CERT_FILE".into(), "/cert.pem".into());
        env.insert("SDK_AGENT_GATEWAY_TLS_KEY_FILE".into(), "/key.pem".into());
        assert!(Configuration::from_environment(&env).is_ok());
        for bind in [
            "0.0.0.0:8097",
            "[::]:8097",
            "192.0.2.2:8097",
            "public.example:8097",
        ] {
            env.insert("SDK_AGENT_GATEWAY_TLS_BIND".into(), bind.into());
            assert!(Configuration::from_environment(&env).is_err());
        }
        env.insert(
            "SDK_AGENT_GATEWAY_TLS_BIND".into(),
            "gateway-sdk-callback:8097".into(),
        );
        env.insert(
            "SDK_AGENT_GATEWAY_TLS_KEY_FILE".into(),
            "relative.pem".into(),
        );
        assert!(Configuration::from_environment(&env).is_err());
        env.insert("SDK_AGENT_GATEWAY_TLS_ENABLED".into(), "false".into());
        assert!(Configuration::from_environment(&env).is_err());
        assert!(
            Configuration::from_environment(&BTreeMap::from([(
                "SDK_AGENT_GATEWAY_TLS_INSECURE".into(),
                "true".into()
            )]))
            .is_err()
        );
    }
    #[tokio::test]
    async fn exact_route_facade_preserves_existing_auth_and_excludes_public_routes() {
        async fn existing_auth(request: Request<Body>, next: Next) -> Response {
            if request.headers().get("authorization").is_none() {
                StatusCode::UNAUTHORIZED.into_response()
            } else {
                next.run(request).await
            }
        }
        let assembled = Router::new()
            .route(
                "/internal/v1/sdk-tools/knowledge/admit",
                axum::routing::post(|| async { "authorized" }),
            )
            .route("/health/ready", axum::routing::get(|| async { "ready" }))
            .layer(axum::middleware::from_fn(existing_auth));
        let facade = restrict(assembled.clone());
        for router in [assembled, facade.clone()] {
            let response = router
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/internal/v1/sdk-tools/knowledge/admit")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = facade
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/v1/sdk-tools/knowledge/admit")
                    .header("authorization", "Bearer fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        for path in [
            "/health/ready",
            "/api/admin",
            "/internal/v1/sdk-tools/knowledge/admit/extra",
            "/internal/v1/sdk-tools/../admin",
        ] {
            let response = facade
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(path)
                        .header("authorization", "Bearer fixture")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
    }
}
