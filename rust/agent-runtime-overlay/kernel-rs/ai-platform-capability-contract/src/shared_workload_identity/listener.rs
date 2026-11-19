//! Shared native TLS listener for Gateway and Knowledge. No process startup,
//! public bind, credential fallback or forwarded-header identity extraction.
use super::{VerifiedPeer, tls::ServerCredentials};
use axum::{
    extract::{ConnectInfo, Request, connect_info::Connected},
    middleware::Next,
    response::Response,
    serve::{IncomingStream, Listener},
};
use std::{io, net::SocketAddr, time::Duration};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::{TlsAcceptor, server::TlsStream};
#[derive(Clone, Debug)]
pub struct WorkloadConnection {
    pub address: SocketAddr,
    pub peer: Option<VerifiedPeer>,
}
impl<'a> Connected<IncomingStream<'a, WorkloadListener>> for WorkloadConnection {
    fn connect_info(stream: IncomingStream<'a, WorkloadListener>) -> Self {
        stream.remote_addr().clone()
    }
}
pub struct WorkloadListener {
    tcp: TcpListener,
    credentials: ServerCredentials,
    handshakes: tokio::task::JoinSet<io::Result<(TlsStream<TcpStream>, WorkloadConnection)>>,
}
fn tls_error() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "workload TLS rejected")
}
impl WorkloadListener {
    pub async fn bind(address: SocketAddr, credentials: ServerCredentials) -> io::Result<Self> {
        let private = match address.ip() {
            std::net::IpAddr::V4(ip) => ip.is_private() || ip.is_loopback(),
            std::net::IpAddr::V6(ip) => ip.is_unique_local() || ip.is_loopback(),
        };
        if !private || address.port() == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workload private listener bind required",
            ));
        }
        credentials.load().map_err(|_| tls_error())?;
        let tcp = TcpListener::bind(address).await?;
        Ok(Self {
            tcp,
            credentials,
            handshakes: tokio::task::JoinSet::new(),
        })
    }
}
impl Listener for WorkloadListener {
    type Io = TlsStream<TcpStream>;
    type Addr = WorkloadConnection;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            tokio::select! {
                accepted=self.tcp.accept(),if self.handshakes.len()<64=>match accepted{Ok((stream,address))=>{let credentials=self.credentials.clone();self.handshakes.spawn(async move{let loader=credentials.clone();let config=tokio::task::spawn_blocking(move||loader.load()).await.map_err(|_|tls_error())?.map_err(|_|tls_error())?;let stream=tokio::time::timeout(Duration::from_secs(10),TlsAcceptor::from(config).accept(stream)).await.map_err(|_|tls_error())??;let peer=credentials.verified_peer(stream.get_ref().1).map_err(|_|tls_error())?;Ok((stream,WorkloadConnection{address,peer}))});},Err(_)=>tokio::time::sleep(Duration::from_millis(50)).await},
                completed=self.handshakes.join_next(),if !self.handshakes.is_empty()=>if let Some(Ok(Ok(connection)))=completed{return connection;}
            }
        }
    }
    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(WorkloadConnection {
            address: self.tcp.local_addr()?,
            peer: None,
        })
    }
}
pub async fn install_peer(mut request: Request, next: Next) -> Response {
    request.extensions_mut().remove::<VerifiedPeer>();
    if let Some(ConnectInfo(info)) = request
        .extensions()
        .get::<ConnectInfo<WorkloadConnection>>()
        .cloned()
    {
        if let Some(peer) = info.peer {
            request.extensions_mut().insert(peer);
        }
        request.extensions_mut().insert(ConnectInfo(info.address));
    }
    next.run(request).await
}
