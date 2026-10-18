//! Shared content failure/future and strict PDF extraction port only.
//! No attachment binder, storage access, account or parser implementation is owned here.
use crate::postgres_error::PgOperationError;
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin};
pub type ContentFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ContentError>> + Send + 'a>>;
#[derive(Clone, Debug)]
pub struct ContentError {
    pub status: u16,
    pub detail: Value,
    pub sqlstate: Option<String>,
    pub headers: std::collections::BTreeMap<String, String>,
}
impl ContentError {
    pub fn new(status: u16, detail: impl Into<Value>) -> Self {
        Self {
            status,
            detail: detail.into(),
            sqlstate: None,
            headers: Default::default(),
        }
    }
    pub fn code(status: u16, code: &str, message: &str) -> Self {
        Self::new(status, json!({"code":code,"message":message}))
    }
    pub fn storage() -> Self {
        Self::new(503, "Storage unavailable")
    }
    pub fn missing_schema(&self) -> bool {
        matches!(self.sqlstate.as_deref(), Some("3F000" | "42P01"))
    }
}
impl IntoResponse for ContentError {
    fn into_response(self) -> Response {
        let mut response = (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"detail":self.detail})),
        )
            .into_response();
        for (k, v) in self.headers {
            if let (Ok(k), Ok(v)) = (
                axum::http::HeaderName::try_from(k),
                axum::http::HeaderValue::try_from(v),
            ) {
                response.headers_mut().insert(k, v);
            }
        }
        response
    }
}
impl From<PgOperationError> for ContentError {
    fn from(error: PgOperationError) -> Self {
        Self {
            status: 503,
            detail: json!("Database unavailable"),
            sqlstate: error.sqlstate().map(str::to_owned),
            headers: Default::default(),
        }
    }
}
impl From<std::io::Error> for ContentError {
    fn from(e: std::io::Error) -> Self {
        Self::new(
            if e.kind() == std::io::ErrorKind::NotFound {
                404
            } else {
                500
            },
            "Storage operation failed",
        )
    }
}
impl From<serde_json::Error> for ContentError {
    fn from(_: serde_json::Error) -> Self {
        Self::new(422, "Invalid content")
    }
}
impl std::fmt::Display for ContentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "content operation failed ({})", self.status)
    }
}
impl std::error::Error for ContentError {}

/// Caller selects a strict PDF backend. Implementations enforce their own page,
/// process and output limits; this contract never silently chooses a backend.
pub trait PdfAttachmentParser: Send + Sync {
    fn extract_strict(&self, raw: &[u8], max_pages: usize)
    -> Result<(String, usize), ContentError>;
}

pub mod storage;
pub mod images;
pub mod attachments;
pub use crate::python_json::python_json;
pub fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
pub fn percent_encode(s: &str) -> String {
    s.as_bytes()
        .iter()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(b) {
                (*b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

pub mod storage_policy;
