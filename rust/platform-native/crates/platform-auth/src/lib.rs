//! Authentication services and router factories. The Gateway owns process
//! configuration and listener startup; durable PostgreSQL/Redis authority is
//! required for protected requests.
pub mod accounts;
pub mod contract;
pub mod http;
pub mod rbac;
pub mod service;
pub mod sessions;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum AuthError {
    #[error("Invalid account identifier")]
    Email,
    #[error("Password does not meet policy")]
    Password,
    #[error("Authentication configuration is invalid")]
    Configuration,
    #[error("Invalid or expired token")]
    Token,
    #[error("Authentication operation failed")]
    Crypto,
}
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$").expect("constant regex")
});
static DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d").expect("constant regex"));
/// Matches the existing account normalization; it does not create accounts.
pub fn normalize_email(input: &str, allowed_domain: &str) -> Result<String, AuthError> {
    let domain = allowed_domain.trim().to_ascii_lowercase();
    let input = input.trim();
    let email = if input.contains('@') {
        input.to_owned()
    } else {
        format!("{input}@{domain}")
    };
    if !EMAIL.is_match(&email) || !email.to_ascii_lowercase().ends_with(&format!("@{domain}")) {
        return Err(AuthError::Email);
    }
    Ok(email.to_ascii_lowercase())
}
pub fn validate_password_change(
    current: &str,
    new: &str,
    confirmation: &str,
) -> Result<(), AuthError> {
    let specials = "!@#$%^&*(),.?\":{}|<>-_=+[]\\;'`~";
    if current.is_empty()
        || new != confirmation
        || new == current
        || new.chars().count() < 8
        || new.len() > 72
        || !new.chars().any(|c| c.is_ascii_alphabetic())
        || !DIGIT.is_match(new)
        || !new.chars().any(|c| specials.contains(c))
    {
        return Err(AuthError::Password);
    }
    Ok(())
}
/// Call from a bounded blocking pool, never the async reactor.
pub fn hash_password(password: &str) -> Result<String, AuthError> {
    if password.is_empty() || password.len() > 72 {
        return Err(AuthError::Password);
    }
    bcrypt::hash(password, 12).map_err(|_| AuthError::Crypto)
}
/// Explicit byte bound avoids bcrypt truncation differences with Python bcrypt 5.
pub fn verify_password(password: &str, hash: &str) -> bool {
    !password.is_empty() && password.len() <= 72 && bcrypt::verify(password, hash).unwrap_or(false)
}

/// Bounds CPU work even if the caller drops the pending request.
#[derive(Clone)]
pub struct PasswordEngine {
    slots: std::sync::Arc<tokio::sync::Semaphore>,
}
impl PasswordEngine {
    pub fn new(concurrency: usize) -> Result<Self, AuthError> {
        if concurrency == 0 || concurrency > 32 {
            return Err(AuthError::Configuration);
        }
        Ok(Self {
            slots: std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency)),
        })
    }
    pub async fn verify(&self, password: String, hash: String) -> Result<bool, AuthError> {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AuthError::Crypto)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            verify_password(&password, &hash)
        })
        .await
        .map_err(|_| AuthError::Crypto)
    }
    pub async fn hash(&self, password: String) -> Result<String, AuthError> {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AuthError::Crypto)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            hash_password(&password)
        })
        .await
        .map_err(|_| AuthError::Crypto)?
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Audience {
    One(String),
    Many(Vec<String>),
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub user_id: String,
    pub tenant_id: String,
    pub email: Option<String>,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub tier: String,
    pub exp: u64,
    pub iat: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nbf: Option<u64>,
    pub jti: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aud: Option<Audience>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>,
}
/// No Debug implementation: signing material must not reach diagnostics.
pub struct TokenCodec {
    encode: EncodingKey,
    decode: DecodingKey,
    audience: Option<String>,
    issuer: Option<String>,
}
impl TokenCodec {
    /// The initial native slice explicitly supports HS256 only.
    /// A minimum key length is deliberate hardening versus the Python blacklist.
    pub fn new(
        secret: &[u8],
        audience: Option<String>,
        issuer: Option<String>,
    ) -> Result<Self, AuthError> {
        if secret.len() < 32
            || audience.as_ref().is_some_and(|s| s.is_empty())
            || issuer.as_ref().is_some_and(|s| s.is_empty())
        {
            return Err(AuthError::Configuration);
        }
        Ok(Self {
            encode: EncodingKey::from_secret(secret),
            decode: DecodingKey::from_secret(secret),
            audience,
            issuer,
        })
    }
    pub fn issue(&self, mut claims: Claims, now: u64) -> Result<String, AuthError> {
        if claims.sub.is_empty() || claims.sub != claims.user_id || claims.tenant_id.is_empty() {
            return Err(AuthError::Token);
        }
        claims.iat = now;
        claims.nbf = None;
        claims.exp = now.checked_add(10_800).ok_or(AuthError::Token)?;
        claims.jti = uuid::Uuid::new_v4().to_string();
        claims.aud = self.audience.clone().map(Audience::One);
        claims.iss = self.issuer.clone();
        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.encode)
            .map_err(|_| AuthError::Crypto)
    }
    /// Verifies cryptographic claims only, not account state or Redis allowlisting.
    /// The caller must enforce those durable authorities before authorizing access.
    pub fn verify(&self, token: &str, now: u64) -> Result<Claims, AuthError> {
        if token.len() > 65_536 {
            return Err(AuthError::Token);
        }
        let mut validation = Validation::new(Algorithm::HS256);
        validation.required_spec_claims = ["exp", "sub"].into_iter().map(str::to_owned).collect();
        validation.validate_exp = false; // Injected clock; exact expiry below.
        validation.leeway = 0;
        validation.validate_aud = self.audience.is_some();
        if let Some(aud) = &self.audience {
            validation.set_audience(&[aud]);
            validation.required_spec_claims.insert("aud".into());
        }
        if let Some(iss) = &self.issuer {
            validation.set_issuer(&[iss]);
            validation.required_spec_claims.insert("iss".into());
        }
        let result = jsonwebtoken::decode::<Claims>(token, &self.decode, &validation)
            .map_err(|_| AuthError::Token)?;
        let claims = result.claims;
        if claims.sub.is_empty()
            || claims.sub != claims.user_id
            || claims.tenant_id.is_empty()
            || uuid::Uuid::parse_str(&claims.jti).is_err()
            || claims.exp <= now
            || claims.iat > now
            || claims.nbf.is_some_and(|nbf| nbf > now)
            || claims.exp <= claims.iat
            || claims.exp - claims.iat > 10_800
        {
            return Err(AuthError::Token);
        }
        Ok(claims)
    }
}

pub mod api_keys;

pub mod admission;
