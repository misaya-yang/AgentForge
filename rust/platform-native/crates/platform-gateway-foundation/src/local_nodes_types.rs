//! Shared contracts for the canonical public Local Node control plane.
//!
//! These types do not mount routes, start workers, connect to a database or open
//! an entropy source. The host supplies trusted user resolution and (optionally)
//! a real channel verifier. No device execution or channel verifier is invented.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::{fmt, future::Future, pin::Pin};

pub type LocalNodesFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, LocalNodesError>> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq)]
pub enum LocalNodesError {
    /// Source LocalNodeRepositoryError. Translate to 409 only inside API _call;
    /// the same error from a channel verifier is an unexpected server error.
    Repository(String),
    /// Raw HTTP detail, wrapped once as {"detail": detail} by the transport.
    Http {
        status: u16,
        detail: Value,
    },
    Internal,
    Cancelled,
}
impl LocalNodesError {
    pub fn repository(message: impl Into<String>) -> Self {
        Self::Repository(message.into())
    }
    pub fn detail(status: u16, detail: impl Into<Value>) -> Self {
        Self::Http {
            status,
            detail: detail.into(),
        }
    }
}
impl fmt::Display for LocalNodesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Repository(message) => f.write_str(message),
            Self::Http { status, .. } => write!(f, "local_node_http_{status}"),
            Self::Internal => f.write_str("local_node_control_plane_internal"),
            Self::Cancelled => f.write_str("local_node_operation_cancelled"),
        }
    }
}
impl std::error::Error for LocalNodesError {}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalNodesIdentity {
    pub tenant_id: String,
    pub user_id: String,
}

/// Arguments contain canonical validated, default-filled body values and path
/// values. `channel` is supplied by a trusted verifier, never parsed from a body.
pub trait LocalNodesControlPlane: Send + Sync {
    fn call<'a>(
        &'a self,
        operation: &'a str,
        owner: &'a LocalNodesIdentity,
        args: Value,
    ) -> LocalNodesFuture<'a, Value>;
}

/// Each byte must be independent cryptographically secure entropy. Do not adapt
/// UUID bytes: their fixed version/variant bits bias the source token alphabet.
pub trait LocalNodesRandom: Send + Sync {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), LocalNodesError>;
}
pub trait LocalNodesClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalNodesSystemClock;
impl LocalNodesClock for LocalNodesSystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Inert Unix host adapter. The OS entropy source is opened only when `fill` is
/// invoked. Failure is closed; no time seed, UUID token, retry or fallback exists.
/// Non-Unix hosts must inject their own cryptographically secure byte provider.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalNodesOsRandom;
impl LocalNodesRandom for LocalNodesOsRandom {
    fn fill(&self, bytes: &mut [u8]) -> Result<(), LocalNodesError> {
        #[cfg(unix)]
        {
            use std::io::Read;
            std::fs::File::open("/dev/urandom")
                .and_then(|mut file| file.read_exact(bytes))
                .map_err(|_| LocalNodesError::Internal)
        }
        #[cfg(not(unix))]
        {
            let _ = bytes;
            Err(LocalNodesError::detail(
                500,
                "Local Node secure randomness requires a host provider on this platform",
            ))
        }
    }
}

/// Exactly secrets.token_urlsafe(9).replace('_', '-')[:12].upper(). All nine
/// bytes come from the secure provider. Tests supply isolated synthetic bytes.
pub fn pairing_user_code(bytes: &[u8; 9]) -> String {
    URL_SAFE_NO_PAD
        .encode(bytes)
        .replace('_', "-")
        .to_ascii_uppercase()
}

/// Python uuid.UUID(str), not Pydantic UUID validation. Python strips every
/// lowercase `urn:`/`uuid:` occurrence, braces at both ends, then every hyphen,
/// before checking 32 Unicode characters and parsing int(hex, 16). This accepts
/// unusual separators, signs, base prefixes and Unicode decimal digits too.
pub fn parse_uuid(value: &str, label: &str) -> Result<String, LocalNodesError> {
    let error = || LocalNodesError::repository(format!("{label} identity is invalid"));
    let normalized = value.replace("urn:", "").replace("uuid:", "");
    let normalized = normalized.trim_matches(['{', '}']).replace('-', "");
    if normalized.chars().count() != 32 {
        return Err(error());
    }
    // CPython int() does not accept the four ASCII separator controls that
    // str.strip() additionally treats as whitespace.
    let normalized = normalized.trim_matches(char::is_whitespace);
    let normalized = normalized.strip_prefix('+').unwrap_or(normalized);
    let ascii: String = normalized
        .chars()
        .map(|c| {
            crate::usage_parser::decimal_digit(c)
                .map(|d| char::from(b'0' + d))
                .unwrap_or(c)
        })
        .collect();
    let (digits, prefixed) = ascii
        .strip_prefix("0x")
        .or_else(|| ascii.strip_prefix("0X"))
        .map_or((ascii.as_str(), false), |v| (v, true));
    let digits = if prefixed {
        digits.strip_prefix('_').unwrap_or(digits)
    } else {
        digits
    };
    if digits.is_empty() {
        return Err(error());
    }
    let mut previous_digit = false;
    let mut result: u128 = 0;
    for c in digits.chars() {
        if c == '_' && previous_digit {
            previous_digit = false;
            continue;
        }
        let digit = c.to_digit(16).ok_or_else(error)?;
        result = result
            .checked_mul(16)
            .and_then(|v| v.checked_add(u128::from(digit)))
            .ok_or_else(error)?;
        previous_digit = true;
    }
    if !previous_digit {
        return Err(error());
    }
    Ok(uuid::Uuid::from_u128(result).to_string())
}
