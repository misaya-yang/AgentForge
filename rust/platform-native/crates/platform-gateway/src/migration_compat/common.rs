use ai_platform_auth::{
    api_keys::{SelectedCredential, select_credential},
    service::AuthService,
};
use axum::{
    Json,
    body::Bytes,
    http::{HeaderMap, HeaderName, StatusCode},
    response::{IntoResponse, Response},
};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    pin::Pin,
    task::{Context, Poll},
};

#[derive(Clone, Debug)]
pub struct CompatError {
    pub status: u16,
    pub detail: Value,
}
impl CompatError {
    pub fn new(status: u16, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: json!(detail.into()),
        }
    }
    pub fn value(status: u16, detail: Value) -> Self {
        Self { status, detail }
    }
}
impl std::fmt::Display for CompatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}
impl std::error::Error for CompatError {}
impl IntoResponse for CompatError {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"detail":self.detail})),
        )
            .into_response()
    }
}
impl From<sqlx::Error> for CompatError {
    fn from(_: sqlx::Error) -> Self {
        Self::new(503, "Database unavailable")
    }
}
impl From<reqwest::Error> for CompatError {
    fn from(e: reqwest::Error) -> Self {
        Self::new(
            if e.is_timeout() { 504 } else { 502 },
            "Upstream unavailable",
        )
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CompatUser {
    pub user_id: String,
    pub tenant_id: String,
    pub tier: String,
    pub authenticated: bool,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub request_id: String,
    pub trace_id: String,
}
impl CompatUser {
    pub fn from_admitted(
        principal: &ai_platform_auth::admission::RequestPrincipal,
        headers: &HeaderMap,
    ) -> Self {
        Self {
            user_id: principal.user_id().into(),
            tenant_id: principal.tenant_id().into(),
            tier: principal.tier().into(),
            authenticated: true,
            roles: principal.roles().to_vec(),
            permissions: principal.permissions().to_vec(),
            request_id: header(headers, "x-request-id").into(),
            trace_id: header(headers, "x-trace-id").into(),
        }
    }
    pub fn apply_request_context(&mut self, extensions: &axum::http::Extensions) {
        if let Some(context) = extensions.get::<crate::migration_host::middleware::RequestContext>()
        {
            self.request_id = context.request_id.clone();
            self.trace_id = context.trace_id.clone();
        }
    }

    pub async fn authenticate(
        auth: &AuthService,
        headers: &HeaderMap,
    ) -> Result<Self, CompatError> {
        let token = match select_credential(headers, &HeaderName::from_static("x-api-key")) {
            Ok(SelectedCredential::Jwt(token)) => token,
            _ => return Err(CompatError::new(401, "Authentication required")),
        };
        let principal = auth
            .authenticate(token)
            .await
            .map_err(|e| CompatError::new(e.status(), "Authentication failed"))?;
        Ok(Self {
            user_id: principal.account.user_id,
            tenant_id: principal.account.tenant_id,
            tier: principal.authority.tier,
            authenticated: true,
            roles: principal.authority.roles,
            permissions: principal.authority.permissions,
            request_id: header(headers, "x-request-id").to_owned(),
            trace_id: header(headers, "x-trace-id").to_owned(),
        })
    }
    pub fn operator(&self) -> bool {
        self.roles
            .iter()
            .any(|r| matches!(r.as_str(), "admin" | "operator"))
    }
}
pub fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
}
pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn object(value: &Value) -> serde_json::Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub struct ByteReceiver(pub tokio::sync::mpsc::Receiver<Result<Bytes, std::io::Error>>);
impl Stream for ByteReceiver {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}
/// Incremental SSE line decoder; handles CRLF and a codepoint split across HTTP chunks.
#[derive(Default)]
pub struct SseDecoder {
    bytes: Vec<u8>,
    event: String,
    data: Vec<String>,
}
impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<(String, String)> {
        self.bytes.extend_from_slice(bytes);
        let mut frames = Vec::new();
        while let Some(end) = self.bytes.iter().position(|b| *b == b'\n') {
            let raw: Vec<_> = self.bytes.drain(..=end).collect();
            let line = String::from_utf8_lossy(&raw[..raw.len() - 1]);
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                if !self.data.is_empty() {
                    frames.push((std::mem::take(&mut self.event), self.data.join("\n")));
                    self.data.clear();
                }
                self.event.clear();
            } else if let Some(value) = line.strip_prefix("event:") {
                self.event = value.trim_start_matches(' ').to_owned();
            } else if let Some(value) = line.strip_prefix("data:") {
                self.data
                    .push(value.strip_prefix(' ').unwrap_or(value).to_owned());
            }
        }
        frames
    }
}
pub fn sse(event: &str, data: &Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}
