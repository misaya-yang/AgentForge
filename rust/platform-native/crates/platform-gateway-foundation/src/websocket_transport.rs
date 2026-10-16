//! Caller-owned JSON WebSocket transport shared by first-party protocol adapters.
//! No reader task is detached: dropping the connection drops its socket.
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config, tungstenite::Message};

/// Select a provider locally: Reqwest 0.13 uses aws-lc while Tonic/SQLx also
/// enable ring. Rustls' implicit process-wide selection is ambiguous in that
/// feature union. Preserve verified WebPKI roots without mutating global state.
fn websocket_tls_config() -> Result<Arc<rustls::ClientConfig>, WebSocketError> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|_| WebSocketError::Connect)?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSocketError {
    Connect,
    Read,
    Write,
    InvalidJson,
    UnsupportedRequest,
}
impl std::fmt::Display for WebSocketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "websocket {self:?}")
    }
}
impl std::error::Error for WebSocketError {}
#[derive(Clone)]
pub struct WebSocketConnector {
    url: String,
}
impl WebSocketConnector {
    /// Keep source concatenation rather than URL joining away a configured prefix.
    pub fn new(base_url: &str, path: &str) -> Self {
        Self {
            url: format!("{}{path}", base_url.trim_end_matches('/')),
        }
    }
    pub async fn websocket(&self) -> Result<WebSocketConnection, WebSocketError> {
        let connector = Connector::Rustls(websocket_tls_config()?);
        let (socket, _) = connect_async_tls_with_config(&self.url, None, false, Some(connector))
            .await
            .map_err(|_| WebSocketError::Connect)?;
        Ok(WebSocketConnection { socket })
    }
    pub fn health_check(&self) -> bool {
        true
    }
    /// HTTP-style request is deliberately unsupported by the source connector.
    pub async fn request(
        &self,
        _method: &str,
        _url: &str,
        _body: &Value,
    ) -> Result<Value, WebSocketError> {
        Err(WebSocketError::UnsupportedRequest)
    }
}

#[cfg(test)]
mod tls_tests {
    #[test]
    fn explicit_websocket_provider_needs_no_process_default() {
        // Construction remains valid when both aws-lc and ring features are
        // unified. Do not install a process default to make this test pass.
        let config = super::websocket_tls_config().unwrap();
        assert!(config.enable_sni);
        assert!(!config.crypto_provider().cipher_suites.is_empty());
        assert!(!webpki_roots::TLS_SERVER_ROOTS.is_empty());
    }
}
pub struct WebSocketConnection {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}
fn json_text(value: &Value) -> String {
    match value {
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    crate::python_json::python_json(&Value::String(key.clone()), true, true),
                    json_text(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(json_text).collect::<Vec<_>>().join(", ")
        ),
        value => crate::python_json::python_json(value, true, true),
    }
}
impl WebSocketConnection {
    pub async fn send_json(&mut self, value: &Value) -> Result<(), WebSocketError> {
        self.socket
            .send(Message::Text(json_text(value).into()))
            .await
            .map_err(|_| WebSocketError::Write)
    }
    /// Control frames are handled by Tungstenite; clean closure ends iteration.
    pub async fn recv_json(&mut self) -> Result<Option<Value>, WebSocketError> {
        while let Some(message) = self.socket.next().await {
            let message = message.map_err(|_| WebSocketError::Read)?;
            let bytes = match message {
                Message::Text(text) => {
                    return serde_json::from_str(&text)
                        .map(Some)
                        .map_err(|_| WebSocketError::InvalidJson);
                }
                Message::Binary(bytes) => bytes.to_vec(),
                Message::Close(_) => return Ok(None),
                Message::Ping(_) => {
                    self.socket
                        .flush()
                        .await
                        .map_err(|_| WebSocketError::Write)?;
                    continue;
                }
                _ => continue,
            };
            return match crate::generic_rest_adapter::decode_http_response_bytes(
                200,
                "application/json",
                &bytes,
            )
            .map_err(|_| WebSocketError::InvalidJson)?
            {
                crate::generic_rest_adapter::RestResponse::Json(value) => Ok(Some(value)),
                _ => Err(WebSocketError::InvalidJson),
            };
        }
        Ok(None)
    }
    pub async fn close(&mut self) -> Result<(), WebSocketError> {
        self.socket
            .close(None)
            .await
            .map_err(|_| WebSocketError::Write)
    }
}
