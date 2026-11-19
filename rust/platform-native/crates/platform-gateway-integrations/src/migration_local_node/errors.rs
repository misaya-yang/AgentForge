use std::fmt;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalNodeError {
    pub code: &'static str,
    pub message: String,
}
impl LocalNodeError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub fn denied(message: impl Into<String>) -> Self {
        Self::new("capability_denied", message)
    }
    pub fn path(message: impl Into<String>) -> Self {
        Self::new("path_escape", message)
    }
    pub fn stale(message: impl Into<String>) -> Self {
        Self::new("stale_target", message)
    }
    pub fn approval(message: impl Into<String>) -> Self {
        Self::new("approval_required", message)
    }
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::new("transport_protocol_error", message)
    }
}
impl fmt::Display for LocalNodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for LocalNodeError {}
impl From<std::io::Error> for LocalNodeError {
    fn from(error: std::io::Error) -> Self {
        Self::new(
            if error.kind() == std::io::ErrorKind::NotFound {
                "file_not_found"
            } else {
                "local_io_error"
            },
            "local operation failed",
        )
    }
}
impl From<rusqlite::Error> for LocalNodeError {
    fn from(_: rusqlite::Error) -> Self {
        Self::new("ledger_integrity_error", "local database operation failed")
    }
}
impl From<serde_json::Error> for LocalNodeError {
    fn from(_: serde_json::Error) -> Self {
        Self::protocol("invalid JSON payload")
    }
}
pub type Result<T> = std::result::Result<T, LocalNodeError>;
