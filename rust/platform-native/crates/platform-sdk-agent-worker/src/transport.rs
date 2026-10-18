//! Explicit private worker TLS. HTTP remains restricted to loopback development.
use crate::contract::Result;
use axum::serve::Listener;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{
    collections::BTreeMap,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

pub struct Transport {
    address: SocketAddr,
    certificate: Option<PathBuf>,
    key: Option<PathBuf>,
    ca: Option<PathBuf>,
}
fn file(path: &Path, private: bool) -> Result<Vec<u8>> {
    if !path.is_absolute() {
        return Err("sdk_worker_tls_path_invalid");
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut input = options
        .open(path)
        .map_err(|_| "sdk_worker_tls_file_invalid")?;
    let metadata = input
        .metadata()
        .map_err(|_| "sdk_worker_tls_file_invalid")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 1024 * 1024 {
        return Err("sdk_worker_tls_file_invalid");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if private && metadata.permissions().mode() & 0o077 != 0 {
            return Err("sdk_worker_tls_key_permissions");
        }
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut bytes = Vec::new();
    use std::io::Read;
    input
        .by_ref()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "sdk_worker_tls_file_invalid")?;
    if bytes.len() > 1024 * 1024 {
        return Err("sdk_worker_tls_file_invalid");
    }
    Ok(bytes)
}
pub(super) fn trust_roots(path: &Path) -> Result<Vec<reqwest::Certificate>> {
    let roots = reqwest::Certificate::from_pem_bundle(&file(path, false)?)
        .map_err(|_| "sdk_worker_tls_ca_invalid")?;
    if roots.is_empty() {
        return Err("sdk_worker_tls_ca_invalid");
    }
    Ok(roots)
}
impl Transport {
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn has_legacy_tls(&self) -> bool {
        self.certificate.is_some()
    }
    pub fn from_environment(environment: &BTreeMap<String, String>) -> Result<Self> {
        let keys = [
            "SDK_AGENT_WORKER_TLS_CERT_FILE",
            "SDK_AGENT_WORKER_TLS_KEY_FILE",
            "SDK_AGENT_WORKER_TLS_CA_FILE",
        ];
        if environment
            .keys()
            .any(|k| k.starts_with("SDK_AGENT_WORKER_TLS_") && !keys.contains(&k.as_str()))
        {
            return Err("sdk_worker_tls_unknown_setting");
        }
        let address = environment
            .get("SDK_AGENT_WORKER_BIND")
            .map(String::as_str)
            .unwrap_or("127.0.0.1:8096")
            .parse::<SocketAddr>()
            .map_err(|_| "sdk_worker_bind_invalid")?;
        if address.port() == 0 {
            return Err("sdk_worker_bind_invalid");
        }
        let values: Vec<_> = keys
            .iter()
            .map(|k| {
                environment
                    .get(*k)
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
            })
            .collect();
        let count = values.iter().filter(|v| v.is_some()).count();
        if count != 0 && count != 3 {
            return Err("sdk_worker_tls_incomplete");
        }
        if !address.ip().is_loopback() && count != 3 && !crate::workload_identity::required() {
            return Err("sdk_worker_nonlocal_requires_tls");
        }
        if values.iter().flatten().any(|p| !p.is_absolute()) {
            return Err("sdk_worker_tls_path_invalid");
        }
        Ok(Self {
            address,
            certificate: values[0].clone(),
            key: values[1].clone(),
            ca: values[2].clone(),
        })
    }
    pub fn load() -> Result<Self> {
        Self::from_environment(
            &std::env::vars()
                .filter(|(k, _)| {
                    k == "SDK_AGENT_WORKER_BIND" || k.starts_with("SDK_AGENT_WORKER_TLS_")
                })
                .collect(),
        )
    }
    fn tls(&self) -> Result<Option<TlsAcceptor>> {
        let Some(cert) = &self.certificate else {
            return Ok(None);
        };
        // Validate the configured health trust at startup, never downgrade TLS.
        let ca = file(self.ca.as_ref().ok_or("sdk_worker_tls_incomplete")?, false)?;
        if reqwest::Certificate::from_pem_bundle(&ca)
            .map_err(|_| "sdk_worker_tls_ca_invalid")?
            .is_empty()
        {
            return Err("sdk_worker_tls_ca_invalid");
        }
        let certificates = CertificateDer::pem_slice_iter(&file(cert, false)?)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| "sdk_worker_tls_certificate_invalid")?;
        if certificates.is_empty() {
            return Err("sdk_worker_tls_certificate_invalid");
        }
        let key = PrivateKeyDer::from_pem_slice(&file(
            self.key.as_ref().ok_or("sdk_worker_tls_incomplete")?,
            true,
        )?)
        .map_err(|_| "sdk_worker_tls_key_invalid")?;
        let mut server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|_| "sdk_worker_tls_config_invalid")?
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(|_| "sdk_worker_tls_pair_invalid")?;
        server.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Some(TlsAcceptor::from(Arc::new(server))))
    }
    pub async fn bind(&self) -> Result<WorkerListener> {
        let tls = self.tls()?;
        let tcp = TcpListener::bind(self.address)
            .await
            .map_err(|_| "sdk_worker_listen_failed")?;
        Ok(WorkerListener {
            tcp,
            tls,
            handshakes: JoinSet::new(),
        })
    }
    pub fn health(&self) -> Result<(reqwest::Client, String)> {
        let mut builder = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(3));
        if let Some(ca) = &self.ca {
            let roots = reqwest::Certificate::from_pem_bundle(&file(ca, false)?)
                .map_err(|_| "sdk_worker_tls_ca_invalid")?;
            if roots.is_empty() {
                return Err("sdk_worker_tls_ca_invalid");
            }
            for root in roots {
                builder = builder.add_root_certificate(root);
            }
            let ip = if self.address.is_ipv6() {
                IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
            } else {
                IpAddr::V4(Ipv4Addr::LOCALHOST)
            };
            builder = builder.resolve("sdk-agent-worker", SocketAddr::new(ip, self.address.port()));
            Ok((
                builder.build().map_err(|_| "sdk_worker_health_client")?,
                format!(
                    "https://sdk-agent-worker:{}/health/ready",
                    self.address.port()
                ),
            ))
        } else {
            Ok((
                builder.build().map_err(|_| "sdk_worker_health_client")?,
                format!("http://{}/health/ready", self.address),
            ))
        }
    }
}
pub enum Connection {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}
impl AsyncRead for Connection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}
impl AsyncWrite for Connection {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}
pub struct WorkerListener {
    tcp: TcpListener,
    tls: Option<TlsAcceptor>,
    handshakes: JoinSet<io::Result<(TlsStream<TcpStream>, SocketAddr)>>,
}
impl Listener for WorkerListener {
    type Io = Connection;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            if let Some(tls) = &self.tls {
                tokio::select! {
                    accepted=self.tcp.accept(),if self.handshakes.len()<32=>{
                        match accepted {Ok((stream,address))=>{let tls=tls.clone();self.handshakes.spawn(async move{let stream=tokio::time::timeout(Duration::from_secs(5),tls.accept(stream)).await.map_err(|_|io::Error::new(io::ErrorKind::TimedOut,"SDK TLS handshake timed out"))??;Ok((stream,address))});},Err(_)=>tokio::time::sleep(Duration::from_millis(50)).await}
                    },
                    complete=self.handshakes.join_next(),if !self.handshakes.is_empty()=>{if let Some(Ok(Ok((stream,address))))=complete{return(Connection::Tls(Box::new(stream)),address);}}
                }
            } else {
                match self.tcp.accept().await {
                    Ok((stream, address)) => return (Connection::Plain(stream), address),
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        }
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.local_addr()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loopback_default_and_explicit_tls_boundary() {
        assert!(Transport::from_environment(&BTreeMap::new()).is_ok());
        for bind in [
            "0.0.0.0:8096",
            "[::]:8096",
            "192.0.2.4:8096",
            "localhost:8096",
            "127.0.0.1:0",
        ] {
            assert!(
                Transport::from_environment(&BTreeMap::from([(
                    "SDK_AGENT_WORKER_BIND".into(),
                    bind.into()
                )]))
                .is_err()
            );
        }
        let mut env = BTreeMap::from([("SDK_AGENT_WORKER_BIND".into(), "0.0.0.0:8096".into())]);
        for key in [
            "SDK_AGENT_WORKER_TLS_CERT_FILE",
            "SDK_AGENT_WORKER_TLS_KEY_FILE",
            "SDK_AGENT_WORKER_TLS_CA_FILE",
        ] {
            env.insert(key.into(), "/does-not-exist.pem".into());
        }
        let config = Transport::from_environment(&env).unwrap();
        assert!(config.tls().is_err());
        env.remove("SDK_AGENT_WORKER_TLS_KEY_FILE");
        assert!(Transport::from_environment(&env).is_err());
    }
    #[test]
    fn typos_partial_and_relative_tls_are_refused() {
        for env in [
            BTreeMap::from([("SDK_AGENT_WORKER_TLS_INSECURE".into(), "true".into())]),
            BTreeMap::from([("SDK_AGENT_WORKER_TLS_CERT_FILE".into(), "/cert.pem".into())]),
            BTreeMap::from([
                ("SDK_AGENT_WORKER_TLS_CERT_FILE".into(), "cert.pem".into()),
                ("SDK_AGENT_WORKER_TLS_KEY_FILE".into(), "key.pem".into()),
                ("SDK_AGENT_WORKER_TLS_CA_FILE".into(), "ca.pem".into()),
            ]),
        ] {
            assert!(Transport::from_environment(&env).is_err());
        }
    }
    #[test]
    fn plaintext_health_targets_only_explicit_loopback() {
        let config = Transport::from_environment(&BTreeMap::new()).unwrap();
        assert_eq!(
            config.health().unwrap().1,
            "http://127.0.0.1:8096/health/ready"
        );
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tls_tests;
