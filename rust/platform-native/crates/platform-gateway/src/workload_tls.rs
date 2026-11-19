//! Dedicated internal TLS facade. Mount the already assembled business router;
//! this module does not replace signed tenant/actor/capability authorization.
use ai_platform_auth::workload::WorkloadAuth;
use ai_platform_service_primitives::workload_identity::{
    Mode, PeerRules, Result, ServiceIdentity, secrets::CredentialFiles, tls::ServerCredentials,
};
use axum::{
    Router,
    extract::{MatchedPath, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    sync::Arc,
};
#[derive(Clone)]
pub struct Configuration {
    pub address: SocketAddr,
    pub credentials: ServerCredentials,
}
impl Configuration {
    /// Binding is internal-only and explicit. Root may resolve a private app-only
    /// DNS alias before calling `new`; wildcard/public binds are rejected here.
    pub fn new(
        address: SocketAddr,
        credentials: ServerCredentials,
    ) -> std::result::Result<Self, &'static str> {
        let private = match address.ip() {
            std::net::IpAddr::V4(ip) => ip.is_loopback() || ip.is_private(),
            std::net::IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local(),
        };
        if !private || address.port() == 0 {
            return Err("workload_listener_private_bind_required");
        }
        Ok(Self {
            address,
            credentials,
        })
    }
    /// Resolve a configured service DNS bind once to a single private address.
    /// Never substitute a wildcard listener if DNS is absent or ambiguous.
    pub async fn resolve_from_environment(
        env: &BTreeMap<String, String>,
    ) -> std::result::Result<Option<Self>, &'static str> {
        let Some(bind) = env.get("AI_PLATFORM_WORKLOAD_TLS_BIND") else {
            return Self::from_environment(env);
        };
        if bind.parse::<SocketAddr>().is_ok() {
            return Self::from_environment(env);
        }
        let (host, port) = bind
            .rsplit_once(':')
            .ok_or("workload_listener_address_invalid")?;
        if host.is_empty()
            || !host
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
            || port.parse::<u16>().is_err()
        {
            return Err("workload_listener_address_invalid");
        }
        let mut addresses = tokio::net::lookup_host(bind)
            .await
            .map_err(|_| "workload_listener_dns_failed")?
            .collect::<Vec<_>>();
        addresses.sort();
        addresses.dedup();
        if addresses.len() != 1 {
            return Err("workload_listener_dns_ambiguous");
        }
        let mut resolved = env.clone();
        resolved.insert(
            "AI_PLATFORM_WORKLOAD_TLS_BIND".into(),
            addresses[0].to_string(),
        );
        Self::from_environment(&resolved)
    }
    pub fn from_environment(
        env: &BTreeMap<String, String>,
    ) -> std::result::Result<Option<Self>, &'static str> {
        let allowed = [
            "AI_PLATFORM_WORKLOAD_MTLS_MODE",
            "AI_PLATFORM_WORKLOAD_TLS_BIND",
            "AI_PLATFORM_WORKLOAD_CREDENTIALS_DIR",
            "AI_PLATFORM_WORKLOAD_SPIFFE_ID",
            "AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS",
        ];
        if env.keys().any(|key| {
            key.starts_with("AI_PLATFORM_WORKLOAD_")
                && !allowed.contains(&key.as_str())
                && !matches!(
                    key.as_str(),
                    "AI_PLATFORM_WORKLOAD_ALLOWED_ORIGINS"
                        | "AI_PLATFORM_WORKLOAD_KNOWLEDGE_CLIENT_IDS"
                        | "AI_PLATFORM_WORKLOAD_PEERS_JSON"
                        | "AI_PLATFORM_WORKLOAD_CA_FILE"
                        | "AI_PLATFORM_WORKLOAD_IDENTITY_FILE"
                )
        }) {
            return Err("workload_unknown_setting");
        }
        let mode = Mode::parse(
            env.get(allowed[0])
                .map(String::as_str)
                .unwrap_or("disabled"),
        )
        .map_err(|_| "workload_mode_invalid")?;
        if mode == Mode::Disabled {
            if env.keys().any(|key| {
                key.starts_with("AI_PLATFORM_WORKLOAD_") && key != "AI_PLATFORM_WORKLOAD_MTLS_MODE"
            }) {
                return Err("workload_disabled_with_tls_inputs");
            }
            return Ok(None);
        }
        let get = |name: &str| {
            env.get(name)
                .filter(|v| !v.is_empty())
                .ok_or("workload_configuration_incomplete")
        };
        let address = get(allowed[1])?
            .parse()
            .map_err(|_| "workload_listener_address_invalid")?;
        let files = CredentialFiles::new(get(allowed[2])?.into())
            .map_err(|_| "workload_credentials_invalid")?;
        let identity =
            ServiceIdentity::parse(get(allowed[3])?).map_err(|_| "workload_identity_invalid")?;
        let ids = get(allowed[4])?
            .split(',')
            .map(ServiceIdentity::parse)
            .collect::<Result<Vec<_>>>()
            .map_err(|_| "workload_allowlist_invalid")?;
        let rules = PeerRules::new(ids).map_err(|_| "workload_allowlist_invalid")?;
        let credentials = ServerCredentials::new(files, identity, rules, mode)
            .map_err(|_| "workload_credentials_invalid")?;
        Self::new(address, credentials).map(Some)
    }
}
pub use ai_platform_service_primitives::workload_identity::listener::{
    WorkloadConnection, WorkloadListener,
};
#[derive(Clone)]
pub struct Routes(BTreeSet<(String, String)>);
impl Routes {
    pub fn new(
        routes: impl IntoIterator<Item = (String, String)>,
    ) -> std::result::Result<Self, &'static str> {
        let mut admitted = BTreeSet::new();
        for (method, path) in routes {
            if method.is_empty()
                || !method.bytes().all(|b| b.is_ascii_uppercase() || b == b'-')
                || !path.starts_with("/internal/")
                || path.contains(['%', '?', '#', '\\'])
                || path.split('/').any(|part| part == "." || part == "..")
            {
                return Err("workload_internal_route_invalid");
            }
            admitted.insert((method, path));
        }
        if admitted.is_empty() {
            return Err("workload_internal_routes_required");
        }
        Ok(Self(admitted))
    }
}
async fn scope(State(routes): State<Routes>, request: Request, next: Next) -> Response {
    if request.uri().path().contains(['%', '\\'])
        || request
            .uri()
            .path()
            .split('/')
            .any(|part| part == "." || part == "..")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let method = request.method().as_str();
    let matched = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str);
    if !routes
        .0
        .contains(&(method.into(), request.uri().path().into()))
        && !matched.is_some_and(|path| routes.0.contains(&(method.into(), path.into())))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(request).await
}
/// TLS connect-info is installed before the auth gate. Original business route
/// authorization runs afterwards, without rewriting Authorization/tenant headers.
pub fn restrict(assembled: Router, gate: Arc<WorkloadAuth>, routes: Routes) -> Router {
    gate.protect(assembled)
        .layer(middleware::from_fn(
            ai_platform_service_primitives::workload_identity::listener::install_peer,
        ))
        .layer(middleware::from_fn_with_state(routes, scope))
}

/// Apply to every pre-existing listener copy as well as the public router.
/// Required mode never admits an internal route without this listener's TLS
/// peer proof. Dual mode leaves the existing, signed legacy guards in charge.
pub fn protect_cleartext(router: Router, gate: Arc<WorkloadAuth>) -> Router {
    router.layer(middleware::from_fn_with_state(gate, reject_cleartext))
}

async fn reject_cleartext(
    State(gate): State<Arc<WorkloadAuth>>,
    mut request: Request,
    next: Next,
) -> Response {
    request
        .extensions_mut()
        .remove::<ai_platform_auth::workload::WorkloadAuthentication>();
    if gate.mode() == Mode::Required && request.uri().path().starts_with("/internal/") {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(serde_json::json!({
                "detail": "workload_mutual_tls_required"
            })),
        )
            .into_response();
    }
    next.run(request).await
}

/// This typed proof originates only in the workload TLS admission middleware.
/// It replaces transport-bearer checks, never signed business authorization.
pub fn mutual_tls(extensions: &axum::http::Extensions) -> bool {
    ai_platform_auth::workload::WorkloadAuthentication::require(extensions)
        .is_ok_and(|proof| proof.kind() == ai_platform_auth::workload::CredentialKind::MutualTls)
}
#[cfg(test)]
#[path = "workload_tls/tests.rs"]
mod tests;
