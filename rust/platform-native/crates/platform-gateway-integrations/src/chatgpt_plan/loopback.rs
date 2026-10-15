//! Explicit local sign-in listener. Binding never opens a browser or authorizes a grant.
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

pub struct LoopbackLogin {
    listener: TcpListener,
    redirect_uri: String,
}
impl LoopbackLogin {
    /// Port 0 selects an available port. Only the documented callback path is used.
    pub async fn bind(port: u16) -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|_| PlanError::local("callback_bind"))?;
        let port = listener
            .local_addr()
            .map_err(|_| PlanError::local("callback_bind"))?
            .port();
        Ok(Self {
            listener,
            redirect_uri: format!("http://127.0.0.1:{port}/auth/callback"),
        })
    }
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }
    /// Wait is bounded. The returned URL is secret-bearing and must go only to exchange().
    /// OAuth state/identity checks remain mandatory in exchange(), not in HTTP parsing.
    pub async fn wait_callback(self) -> Result<SecretString> {
        tokio::time::timeout(Duration::from_secs(600), async {
            loop {
                let (mut socket, peer) = self.listener.accept().await.map_err(|_| PlanError::local("callback_accept"))?;
                if !peer.ip().is_loopback() { continue; }
                let mut request = Vec::new();
                let read = tokio::time::timeout(Duration::from_secs(5), async {
                    loop {
                        let mut buf = [0u8;1024];
                        let n = socket.read(&mut buf).await.map_err(|_| PlanError::local("callback_read"))?;
                        if n == 0 || request.len() + n > 16384 { return Err(PlanError::local("invalid_callback_request")); }
                        request.extend_from_slice(&buf[..n]);
                        if request.windows(4).any(|w| w == b"\r\n\r\n") { return Ok(()); }
                    }
                }).await;
                if !matches!(read,Ok(Ok(()))) { continue; }
                let text = match std::str::from_utf8(&request) { Ok(t) => t, Err(_) => continue };
                let mut parts = text.lines().next().unwrap_or_default().split_whitespace();
                if parts.next() != Some("GET") { continue; }
                let Some(target) = parts.next() else { continue; };
                if !target.starts_with("/auth/callback?") { continue; }
                let callback = format!("{}{}",self.redirect_uri,target.strip_prefix("/auth/callback").unwrap_or_default());
                let body = "Sign-in response received. Return to AI Platform to see whether authorization succeeded.";
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",body.len(),body);
                let _ = tokio::time::timeout(Duration::from_secs(2),socket.write_all(response.as_bytes())).await;
                return Ok(SecretString::from_callback(callback));
            }
        }).await.map_err(|_| PlanError::local("authorization_expired"))?
    }
}
