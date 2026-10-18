//! Platform-only outbound mTLS. Keys come from private workload-mounted files;
//! public web fetches use a separate credential-free client. Inbound identity is
//! enforced by the admitted workload TLS listener/proxy, never trusted HTTP hints.
use std::{io::Read, path::Path, time::Duration};

// Canonical native workload sources are synchronized byte-for-byte into this
// small controlled copy by root's generator and source receipt.
#[path = "shared_workload_identity/mod.rs"]
pub mod tls_core;
use std::collections::BTreeMap;
static EXPLICIT_ENVIRONMENT: std::sync::OnceLock<BTreeMap<String, String>> =
    std::sync::OnceLock::new();
/// Native Gateway's stdin document is authoritative. Never synthesize process
/// environment variables or mutate another task's process-global environment.
pub fn configure_environment(environment: &BTreeMap<String, String>) -> Result<(), &'static str> {
    let selected = environment
        .iter()
        .filter(|(k, _)| {
            k.starts_with("AI_PLATFORM_WORKLOAD_")
                || matches!(
                    k.as_str(),
                    "AI_PLATFORM_INTERNAL_TOKEN" | "AI_PLATFORM_INTERNAL_TOKEN_FILE"
                )
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect::<BTreeMap<_, _>>();
    if let Some(current) = EXPLICIT_ENVIRONMENT.get() {
        return if *current == selected {
            Ok(())
        } else {
            Err("workload_configuration_already_frozen")
        };
    }
    EXPLICIT_ENVIRONMENT
        .set(selected)
        .map_err(|_| "workload_configuration_already_frozen")
}
fn setting(name: &str) -> Result<String, std::env::VarError> {
    if let Some(environment) = EXPLICIT_ENVIRONMENT.get() {
        return environment
            .get(name)
            .cloned()
            .ok_or(std::env::VarError::NotPresent);
    }
    std::env::var(name)
}
pub fn tls_enabled() -> bool {
    matches!(
        setting("AI_PLATFORM_WORKLOAD_MTLS_MODE").as_deref(),
        Ok("required" | "dual")
    )
}
pub fn required() -> bool {
    setting("AI_PLATFORM_WORKLOAD_MTLS_MODE").as_deref() == Ok("required")
}
pub struct WorkloadIdentity {
    transport: tls_core::http::WorkloadHttp,
}
impl WorkloadIdentity {
    pub fn from_environment() -> Result<Option<Self>, &'static str> {
        let mode = setting("AI_PLATFORM_WORKLOAD_MTLS_MODE").unwrap_or_else(|_| "disabled".into());
        if mode == "disabled" {
            return Ok(None);
        }
        if !matches!(mode.as_str(), "required" | "dual") {
            return Err("workload_mode_invalid");
        }
        if required()
            && [
                "AI_PLATFORM_INTERNAL_TOKEN",
                "AI_PLATFORM_INTERNAL_TOKEN_FILE",
            ]
            .iter()
            .any(|k| setting(k).is_ok_and(|v| !v.is_empty()))
        {
            return Err("workload_shared_bearer_forbidden");
        }
        let root = setting("AI_PLATFORM_WORKLOAD_CREDENTIALS_DIR")
            .map_err(|_| "workload_credentials_required")?;
        let identity =
            setting("AI_PLATFORM_WORKLOAD_SPIFFE_ID").map_err(|_| "workload_identity_required")?;
        let text =
            setting("AI_PLATFORM_WORKLOAD_PEERS_JSON").map_err(|_| "workload_peers_required")?;
        if text.len() > 32768 {
            return Err("workload_peers_invalid");
        }
        let map: BTreeMap<String, String> =
            serde_json::from_str(&text).map_err(|_| "workload_peers_invalid")?;
        let mut peers = Vec::new();
        for (origin, id) in map {
            parse_origins(&origin)?;
            peers.push((
                origin,
                tls_core::ServiceIdentity::parse(&id)
                    .map_err(|_| "workload_peer_identity_invalid")?,
            ));
        }
        let credentials = tls_core::tls::ClientCredentials::new(
            tls_core::secrets::CredentialFiles::new(root.into())
                .map_err(|_| "workload_credentials_invalid")?,
            tls_core::ServiceIdentity::parse(&identity).map_err(|_| "workload_identity_invalid")?,
        );
        let transport = tls_core::http::WorkloadHttp::new(
            credentials,
            peers,
            Duration::from_secs(2),
            Duration::from_secs(3600),
        )
        .map_err(|_| "workload_client_invalid")?;
        Ok(Some(Self { transport }))
    }
    pub fn validate_endpoint(&self, endpoint: &str) -> Result<(), &'static str> {
        self.transport
            .validate_endpoint(endpoint)
            .map(|_| ())
            .map_err(|_| "workload_peer_not_admitted")
    }
    pub fn apply(self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        // Credential-free request construction; only WorkloadSend can dispatch
        // it with TLS credentials after checking its final URL and peer identity.
        builder.https_only(true).pool_max_idle_per_host(0)
    }
}
#[derive(Debug)]
pub enum TransportError {
    Configuration(&'static str),
    Request(reqwest::Error),
}
impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(s) => f.write_str(s),
            Self::Request(_) => f.write_str("workload_http_request_failed"),
        }
    }
}
impl std::error::Error for TransportError {}
impl TransportError {
    pub fn is_timeout(&self) -> bool {
        matches!(self,Self::Request(e) if e.is_timeout())
            || matches!(
                self,
                Self::Configuration("workload_http_timeout" | "workload_http_request_failed")
            )
    }
}
pub trait WorkloadSend {
    fn workload_send(
        self,
    ) -> impl std::future::Future<Output = Result<reqwest::Response, TransportError>> + Send;
}
impl WorkloadSend for reqwest::RequestBuilder {
    async fn workload_send(self) -> Result<reqwest::Response, TransportError> {
        let Some(identity) =
            WorkloadIdentity::from_environment().map_err(TransportError::Configuration)?
        else {
            return self.send().await.map_err(TransportError::Request);
        };
        let mut request = self.build().map_err(TransportError::Request)?;
        identity
            .validate_endpoint(request.url().as_str())
            .map_err(TransportError::Configuration)?;
        // Transport bearers are retired; run/model/tool bearer grants are signed
        // domain authority and intentionally remain present.
        request.headers_mut().remove("x-ai-platform-internal-token");
        let bytes = request
            .body()
            .map(|b| {
                b.as_bytes()
                    .map(<[u8]>::to_vec)
                    .ok_or(TransportError::Configuration(
                        "workload_streaming_request_body_forbidden",
                    ))
            })
            .transpose()?;
        let timeout = request
            .timeout()
            .copied()
            .unwrap_or(Duration::from_secs(300));
        tokio::time::timeout(
            timeout,
            identity.transport.send(
                request.method().clone(),
                request.url().as_str(),
                request.headers().clone(),
                bytes,
            ),
        )
        .await
        .map_err(|_| TransportError::Configuration("workload_http_timeout"))?
        .map_err(|_| TransportError::Configuration("workload_http_request_failed"))
    }
}
tokio::task_local! { static AUTHENTICATED_PEER: bool; }
pub fn authenticated_peer() -> bool {
    AUTHENTICATED_PEER.try_with(|v| *v).unwrap_or(false)
}
async fn admit(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<tls_core::listener::WorkloadConnection>>()
        .and_then(|v| v.0.peer.as_ref())
        .is_some();
    let local = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .is_some_and(|v| v.0.ip().is_loopback());
    let path = request.uri().path();
    let health = matches!(path, "/health/live" | "/health/ready");
    let kernel_proxy = path.starts_with("/internal/v1/model-proxy/") && local;
    if (required() && !peer && !health && !kernel_proxy)
        || (path.starts_with("/internal/v1/model-proxy/") && !local)
    {
        return axum::http::StatusCode::UNAUTHORIZED.into_response();
    }
    if required()
        && [
            "x-spiffe-id",
            "x-workload-identity",
            "x-forwarded-client-cert",
            "x-ssl-client-cert",
            "x-client-cert",
        ]
        .iter()
        .any(|k| request.headers().contains_key(*k))
    {
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    AUTHENTICATED_PEER.scope(peer, next.run(request)).await
}
pub async fn serve<F: std::future::Future<Output = ()> + Send + 'static>(
    address: std::net::SocketAddr,
    router: axum::Router,
    shutdown: F,
) -> std::io::Result<()> {
    let router = router.layer(axum::middleware::from_fn(admit));
    if !tls_enabled() {
        let listener = tokio::net::TcpListener::bind(address).await?;
        return axum::serve(listener, router)
            .with_graceful_shutdown(shutdown)
            .await;
    }
    // Resolve an explicit private bind. The listener validates the address and
    // credentials; no public/wildcard/plaintext fallback exists.
    let bind = setting("AI_PLATFORM_WORKLOAD_TLS_BIND")
        .map_err(|_| std::io::Error::other("workload_tls_bind_required"))?;
    let mut addresses = tokio::net::lookup_host(&bind).await?.collect::<Vec<_>>();
    addresses.sort();
    addresses.dedup();
    if addresses.len() != 1 {
        return Err(std::io::Error::other("workload_tls_bind_ambiguous"));
    }
    let root = setting("AI_PLATFORM_WORKLOAD_CREDENTIALS_DIR")
        .map_err(|_| std::io::Error::other("workload_credentials_required"))?;
    let identity = tls_core::ServiceIdentity::parse(
        &setting("AI_PLATFORM_WORKLOAD_SPIFFE_ID")
            .map_err(|_| std::io::Error::other("workload_identity_required"))?,
    )
    .map_err(std::io::Error::other)?;
    let peers = setting("AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS")
        .map_err(|_| std::io::Error::other("workload_clients_required"))?
        .split(',')
        .map(tls_core::ServiceIdentity::parse)
        .collect::<tls_core::Result<Vec<_>>>()
        .map_err(std::io::Error::other)?;
    let server = tls_core::tls::ServerCredentials::new(
        tls_core::secrets::CredentialFiles::new(root.into()).map_err(std::io::Error::other)?,
        identity,
        tls_core::PeerRules::new(peers).map_err(std::io::Error::other)?,
        if required() {
            tls_core::Mode::Required
        } else {
            tls_core::Mode::Dual
        },
    )
    .map_err(std::io::Error::other)?;
    let tls = tls_core::listener::WorkloadListener::bind(addresses[0], server).await?;
    // Preserve the original HTTP port for credential-free health/readiness.
    // Required mode rejects every business route on it; the kernel bridge is
    // additionally restricted by actual loopback peer address and its local token.
    let plain = tokio::net::TcpListener::bind(address).await?;
    // A watch value retains shutdown even if it arrives before either Axum
    // graceful-shutdown future was first polled.
    let (stop, mut tls_stop) = tokio::sync::watch::channel(false);
    let mut plain_stop = tls_stop.clone();
    tokio::pin!(shutdown);
    let tls_server = axum::serve(
        tls,
        router
            .clone()
            .into_make_service_with_connect_info::<tls_core::listener::WorkloadConnection>(),
    )
    .with_graceful_shutdown(async move {
        if !*tls_stop.borrow() {
            let _ = tls_stop.changed().await;
        }
    });
    let plain_server = axum::serve(
        plain,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        if !*plain_stop.borrow() {
            let _ = plain_stop.changed().await;
        }
    });
    let servers = async {
        tokio::try_join!(async { tls_server.await }, async { plain_server.await }).map(|_| ())
    };
    tokio::pin!(servers);
    tokio::select! {result=&mut servers=>result,_=&mut shutdown=>{let _=stop.send(true);servers.await}}
}
fn parse_origins(origins: &str) -> Result<Vec<reqwest::Url>, &'static str> {
    let values: Vec<_> = origins.split(',').collect();
    if values.is_empty() || values.len() > 64 {
        return Err("workload_peer_invalid");
    }
    let mut result = Vec::new();
    for value in values {
        let url = reqwest::Url::parse(value).map_err(|_| "workload_peer_invalid")?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || value.trim() != value
            || value.chars().any(char::is_control)
            || value.contains('\\')
            || result
                .iter()
                .any(|other: &reqwest::Url| other.origin() == url.origin())
        {
            return Err("workload_peer_invalid");
        }
        result.push(url);
    }
    Ok(result)
}
fn read_bounded(path: &Path, private: bool) -> Result<Vec<u8>, &'static str> {
    if !path.is_absolute() {
        return Err("workload_identity_file_invalid");
    }
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "workload_identity_file_unavailable")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 128 * 1024 {
        return Err("workload_identity_file_invalid");
    }
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("workload_identity_file_not_private");
        }
    }
    let mut bytes = vec![];
    std::fs::File::open(path)
        .map_err(|_| "workload_identity_file_unavailable")?
        .take(128 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "workload_identity_file_unavailable")?;
    if bytes.len() > 128 * 1024 {
        return Err("workload_identity_file_invalid");
    }
    Ok(bytes)
}
pub fn internal_client(
    endpoints: &[&str],
    connect: Duration,
    total: Duration,
) -> Result<reqwest::Client, &'static str> {
    internal_client_with_ca(endpoints, connect, total, None)
}
pub fn internal_client_with_ca(
    endpoints: &[&str],
    connect: Duration,
    total: Duration,
    ca: Option<&Path>,
) -> Result<reqwest::Client, &'static str> {
    let mut builder = reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(connect)
        .timeout(total);
    if let Some(identity) = WorkloadIdentity::from_environment()? {
        for endpoint in endpoints.iter().filter(|s| !s.is_empty()) {
            identity.validate_endpoint(endpoint)?;
        }
        builder = identity.apply(builder);
    } else if let Some(ca) = ca {
        let roots = reqwest::Certificate::from_pem_bundle(&read_bounded(ca, false)?)
            .map_err(|_| "workload_ca_invalid")?;
        if roots.is_empty() || roots.len() > 32 {
            return Err("workload_ca_invalid");
        }
        for root in roots {
            builder = builder.add_root_certificate(root);
        }
    }
    builder.build().map_err(|_| "workload_http_client_invalid")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workload_peers_require_exact_https_origins_without_ambient_authority() {
        assert_eq!(
            parse_origins("https://gateway:8080,https://worker:8095")
                .unwrap()
                .len(),
            2
        );
        for origin in [
            "",
            "http://worker:8095",
            "https://user:secret@worker",
            "https://worker/path",
            "https://worker?secret=x",
            "https://worker,https://worker",
            " https://worker",
        ] {
            assert!(parse_origins(origin).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn identity_files_reject_public_permissions_and_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = std::env::temp_dir().join(format!(
            "workload-identity-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let key = root.join("synthetic-key");
        std::fs::write(&key, b"synthetic-test-only").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_bounded(&key, true).is_err());
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_bounded(&key, true).is_ok());
        let link = root.join("link");
        symlink(&key, &link).unwrap();
        assert!(read_bounded(&link, true).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
