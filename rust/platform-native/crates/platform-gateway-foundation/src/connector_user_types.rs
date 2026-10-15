//! Shared contracts for the canonical eleven-route user-connector family.
//! Values crossing credential boundaries deliberately do not implement Debug.
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::{fmt, future::Future, pin::Pin};

pub type ConnectorFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ConnectorError>> + Send + 'a>>;
#[derive(Clone, PartialEq)]
pub struct ConnectorError {
    pub status: u16,
    pub detail: Value,
    pub source_unhandled: bool,
}
impl ConnectorError {
    pub fn detail(status: u16, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: Value::String(detail.into()),
            source_unhandled: false,
        }
    }
    pub fn source() -> Self {
        Self {
            status: 500,
            detail: Value::String("Internal Server Error".into()),
            source_unhandled: true,
        }
    }
    pub fn storage() -> Self {
        Self::source()
    }
}
impl fmt::Debug for ConnectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectorError")
            .field("status", &self.status)
            .field("source_unhandled", &self.source_unhandled)
            .finish_non_exhaustive()
    }
}
impl fmt::Display for ConnectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "connector_error_{}", self.status)
    }
}
impl std::error::Error for ConnectorError {}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct ConnectorUser {
    pub tenant_id: String,
    pub user_id: String,
}
#[derive(Clone, PartialEq)]
pub struct ConnectorRecord(pub Value);
impl ConnectorRecord {
    pub fn get(&self, key: &str) -> &Value {
        self.0.get(key).unwrap_or(&Value::Null)
    }
}
#[derive(Clone)]
pub struct ConnectorTokenWrite {
    pub user: ConnectorUser,
    pub provider: String,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub scopes: String,
    pub metadata: Option<Value>,
}
#[derive(Clone)]
pub enum ConnectorUserOperation {
    EffectiveEnabledConfig {
        provider: String,
        tenant_id: String,
    },
    AvailableConfigs {
        tenant_id: String,
    },
    UserStatuses {
        user: ConnectorUser,
    },
    ListMine {
        user: ConnectorUser,
    },
    GetUser {
        user: ConnectorUser,
        provider: String,
    },
    UpsertTokens(ConnectorTokenWrite),
    MarkRefreshFailed {
        id: Value,
    },
    SaveRefreshed {
        id: Value,
        access_token: Option<String>,
        refresh_token: Option<String>,
        expires_at: DateTime<Utc>,
    },
    Revoke {
        user: ConnectorUser,
        provider: String,
    },
}
pub trait ConnectorUserStore: Send + Sync {
    fn enabled(&self) -> bool {
        true
    }
    fn run(&self, operation: ConnectorUserOperation) -> ConnectorFuture<'_, Vec<ConnectorRecord>>;
}
#[derive(Clone)]
pub enum OAuthStateRaw {
    Bytes(Vec<u8>),
    Json(Value),
}
pub trait OAuthStateStore: Send + Sync {
    fn save<'a>(&'a self, key: &'a str, value: Value, ttl: u64) -> ConnectorFuture<'a, ()>;
    /// Atomically return and remove the value; never implement as GET then DELETE.
    fn consume<'a>(&'a self, key: &'a str) -> ConnectorFuture<'a, Option<OAuthStateRaw>>;
}
pub trait ConnectorClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}
pub trait ConnectorNonce: Send + Sync {
    fn nonce(&self) -> Result<String, ConnectorError>;
}
/// Captured explicitly by the host. Construction does not inspect environment.
#[derive(Clone, Default)]
pub struct ConnectorEnvironment {
    pub injected_encryption_key: Option<String>,
    pub gateway_encryption_key: String,
    pub console_public_base_url: String,
}
pub trait ConnectorTokenCipher: Send + Sync {
    fn decrypt_config(&self, value: &str) -> Result<String, ConnectorError>;
    fn encrypt_user(&self, value: Option<&str>) -> Result<Option<String>, ConnectorError>;
    fn decrypt_user(&self, value: Option<&str>) -> Result<Option<String>, ConnectorError>;
}
#[derive(Clone)]
pub struct OAuthFormRequest {
    pub url: String,
    pub fields: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
}
#[derive(Clone)]
pub struct ConnectorGetRequest {
    pub url: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
}
#[derive(Clone)]
pub struct ConnectorHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}
impl ConnectorHttpResponse {
    pub fn json(&self) -> Result<Value, ConnectorError> {
        serde_json::from_slice(&self.body).map_err(|_| ConnectorError::source())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorHttpError {
    RejectedDestination,
    /// Ordinary network failure, corresponding to source SafeFetchError.
    Transport,
    /// Collaborator failure outside the source's SafeFetchError boundary.
    SourceUnhandled,
}
pub type ConnectorHttpFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ConnectorHttpResponse, ConnectorHttpError>> + Send + 'a>>;
pub trait OAuthFormHttp: Send + Sync {
    fn post(&self, request: OAuthFormRequest) -> ConnectorHttpFuture<'_>;
}
pub trait ConnectorProviderHttp: Send + Sync {
    fn get(&self, request: ConnectorGetRequest) -> ConnectorHttpFuture<'_>;
}
/// Pydantic's unconstrained Python int, retained locally without changing serde
/// number behavior for any existing gateway family. Always canonical decimal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorSearchLimit(String);
impl ConnectorSearchLimit {
    pub fn from_decimal(raw: &str) -> Option<Self> {
        let (negative, digits) = if let Some(raw) = raw.strip_prefix('-') {
            (true, raw)
        } else {
            (false, raw.strip_prefix('+').unwrap_or(raw))
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let digits = digits.trim_start_matches('0');
        if digits.is_empty() {
            return Some(Self("0".into()));
        }
        Some(Self(format!("{}{digits}", if negative { "-" } else { "" })))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn as_i64(&self) -> Option<i64> {
        self.0.parse().ok()
    }
    /// Equivalent to records[:limit], including enormous negative integers.
    pub fn slice_end(&self, len: usize) -> usize {
        if let Some(magnitude) = self.0.strip_prefix('-') {
            len.saturating_sub(magnitude.parse::<usize>().unwrap_or(usize::MAX))
        } else {
            len.min(self.0.parse::<usize>().unwrap_or(usize::MAX))
        }
    }
}
impl From<i64> for ConnectorSearchLimit {
    fn from(value: i64) -> Self {
        Self(value.to_string())
    }
}
pub trait ConnectorSearchProviders: Send + Sync {
    fn search<'a>(
        &'a self,
        provider: &'a str,
        token: &'a str,
        metadata: &'a Value,
        query: &'a str,
        limit: i64,
    ) -> ConnectorFuture<'a, Value>;
    fn search_exact<'a>(
        &'a self,
        provider: &'a str,
        token: &'a str,
        metadata: &'a Value,
        query: &'a str,
        limit: &'a ConnectorSearchLimit,
    ) -> ConnectorFuture<'a, Value> {
        Box::pin(async move {
            self.search(
                provider,
                token,
                metadata,
                query,
                limit.as_i64().ok_or_else(ConnectorError::source)?,
            )
            .await
        })
    }
}
pub trait ConnectorActivation: Send + Sync {
    fn start_confluence<'a>(
        &'a self,
        tenant_id: &'a str,
        domain: &'a str,
        email: &'a str,
        api_token: &'a str,
    ) -> ConnectorFuture<'a, Vec<Value>>;
    fn stop<'a>(&'a self, tenant_id: &'a str, provider: &'a str) -> ConnectorFuture<'a, ()>;
    fn status(&self, tenant_id: &str, provider: &str)
    -> Result<(bool, Vec<Value>), ConnectorError>;
}
