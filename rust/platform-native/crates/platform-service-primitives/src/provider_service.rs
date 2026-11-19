//! Stateless cipher contract only; no provider registry, SQL or policy.
use std::fmt;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceError {
    Database,
    /// Redacted PostgreSQL unique violation, retained for source HTTP 409 mapping.
    UniqueConflict,
    Cipher,
    InvalidRow,
    ProviderNotFound,
    Timeout,
    Connect,
    GoogleAuth,
    Http,
}
impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if matches!(self, Self::UniqueConflict) {
            f.write_str("duplicate key")
        } else {
            write!(f, "provider_service_{:?}", self)
        }
    }
}
impl std::error::Error for ServiceError {}

pub trait ProviderCipher: Send + Sync {
    fn encrypt(&self, value: &str, encryption_key: &str) -> Result<String, ServiceError>;
    fn decrypt(&self, value: &str, encryption_key: &str) -> Result<String, ServiceError>;
}
