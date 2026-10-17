//! Explicit HTTPS listener for deployments that expose the native companion
//! control endpoint directly from Gateway. No global TLS provider or trust
//! override is installed.
use axum::{body::Body, extract::Request, middleware::Next, response::Response};
use axum::{
    extract::connect_info::Connected,
    serve::{IncomingStream, Listener},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::{io, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, server::TlsStream};

#[derive(Clone, Copy)]
pub struct HttpsRequest;

/// Source TCP peer captured by the TLS listener, never by forwarded headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TlsPeerAddress(pub SocketAddr);

impl<'a> Connected<IncomingStream<'a, GatewayHttpsListener>> for TlsPeerAddress {
    fn connect_info(stream: IncomingStream<'a, GatewayHttpsListener>) -> Self {
        Self(*stream.remote_addr())
    }
}

pub async fn mark_https(mut request: Request<Body>, next: Next) -> Response {
    if let Some(axum::extract::ConnectInfo(TlsPeerAddress(address))) = request
        .extensions()
        .get::<axum::extract::ConnectInfo<TlsPeerAddress>>()
        .copied()
    {
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(address));
    }
    request.extensions_mut().insert(HttpsRequest);
    next.run(request).await
}

#[derive(Clone)]
pub struct GatewayHttpsConfiguration {
    pub address: SocketAddr,
    pub certificate_path: PathBuf,
    pub private_key_path: PathBuf,
}

pub struct GatewayHttpsListener {
    tcp: TcpListener,
    tls: TlsAcceptor,
    handshakes: tokio::task::JoinSet<io::Result<(TlsStream<TcpStream>, SocketAddr)>>,
}

impl GatewayHttpsListener {
    pub async fn bind(config: &GatewayHttpsConfiguration) -> io::Result<Self> {
        let certificates = CertificateDer::pem_file_iter(&config.certificate_path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "TLS certificate invalid"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "TLS certificate invalid"))?;
        if certificates.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TLS certificate missing",
            ));
        }
        let private_key = PrivateKeyDer::from_pem_file(&config.private_key_path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "TLS private key invalid"))?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
            .map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "TLS protocol config invalid")
            })?;
        let mut server = builder
            .with_no_client_auth()
            .with_single_cert(certificates, private_key)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "TLS key pair invalid"))?;
        server.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let tcp = TcpListener::bind(config.address).await?;
        Ok(Self {
            tcp,
            tls: TlsAcceptor::from(Arc::new(server)),
            handshakes: tokio::task::JoinSet::new(),
        })
    }
}

impl Listener for GatewayHttpsListener {
    type Io = TlsStream<TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            // A silent TCP peer must not serialize every companion handshake.
            // Tasks and sockets stay bounded and are aborted when this listener
            // drops; each handshake retains the existing ten-second deadline.
            tokio::select! {
                accepted = self.tcp.accept(), if self.handshakes.len() < 64 => {
                    match accepted {
                        Ok((stream,address)) => {
                            let tls=self.tls.clone();
                            self.handshakes.spawn(async move {
                                let stream=tokio::time::timeout(Duration::from_secs(10), tls.accept(stream))
                                    .await.map_err(|_|io::Error::new(io::ErrorKind::TimedOut,"TLS handshake timed out"))??;
                                Ok((stream,address))
                            });
                        }
                        Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                    }
                }
                completed = self.handshakes.join_next(), if !self.handshakes.is_empty() => {
                    if let Some(Ok(Ok(connection)))=completed { return connection; }
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

#[cfg(test)]
#[path = "local_node_tls_tests.rs"]
mod tests;
