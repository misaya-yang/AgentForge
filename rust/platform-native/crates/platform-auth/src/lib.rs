//! Authentication services and router factories. The Gateway owns process
//! configuration and listener startup; durable PostgreSQL/Redis authority is
//! required for protected requests.
pub mod accounts;
pub mod contract;
pub mod http;
mod knowledge_actor_verifier;
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
    passwords::hash(password)
}
/// Legacy bcrypt accepts at most 72 bytes; new Argon2id hashes remain bounded.
pub fn verify_password(password: &str, hash: &str) -> bool {
    passwords::verify(password, hash)
}

/// Bounds CPU work even if the caller drops the pending request.
#[derive(Clone)]
pub struct PasswordEngine {
    slots: std::sync::Arc<tokio::sync::Semaphore>,
}
impl PasswordEngine {
    pub fn new(concurrency: usize) -> Result<Self, AuthError> {
        if concurrency == 0 || concurrency > 8 {
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
    pub async fn verify_upgrade(
        &self,
        password: String,
        hash: String,
    ) -> Result<passwords::Verification, AuthError> {
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| AuthError::Crypto)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            passwords::verify_upgrade(&password, &hash)
        })
        .await
        .map_err(|_| AuthError::Crypto)?
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
    eddsa: Option<std::sync::Arc<access_tokens::EdDsaKeyRing>>,
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
            eddsa: None,
        })
    }
    pub fn with_eddsa(mut self, keys: access_tokens::EdDsaKeyRing) -> Result<Self, AuthError> {
        if self.audience.is_none() || self.issuer.is_none() {
            return Err(AuthError::Configuration);
        }
        self.eddsa = Some(std::sync::Arc::new(keys));
        Ok(self)
    }
    pub(crate) fn has_eddsa(&self) -> bool {
        self.eddsa.is_some()
    }
    pub fn jwks(&self, now: u64) -> serde_json::Value {
        self.eddsa
            .as_ref()
            .map(|keys| keys.jwks(now))
            .unwrap_or_else(|| serde_json::json!({"keys":[]}))
    }
    pub fn issue(&self, claims: Claims, now: u64) -> Result<String, AuthError> {
        self.issue_for_session(claims, now, &uuid::Uuid::new_v4().to_string())
    }
    pub(crate) fn issue_for_session(
        &self,
        mut claims: Claims,
        now: u64,
        jti: &str,
    ) -> Result<String, AuthError> {
        uuid::Uuid::parse_str(jti).map_err(|_| AuthError::Token)?;
        if claims.sub.is_empty() || claims.sub != claims.user_id || claims.tenant_id.is_empty() {
            return Err(AuthError::Token);
        }
        claims.iat = now;
        claims.nbf = None;
        claims.exp = now
            .checked_add(if self.eddsa.is_some() {
                access_tokens::ACCESS_TOKEN_TTL
            } else {
                10_800
            })
            .ok_or(AuthError::Token)?;
        claims.jti = jti.to_owned();
        claims.aud = self.audience.clone().map(Audience::One);
        claims.iss = self.issuer.clone();
        if let Some(keys) = &self.eddsa {
            return keys.issue(&claims, now);
        }
        jsonwebtoken::encode(&Header::new(Algorithm::HS256), &claims, &self.encode)
            .map_err(|_| AuthError::Crypto)
    }
    /// Verifies cryptographic claims only, not account state or Redis allowlisting.
    /// The caller must enforce those durable authorities before authorizing access.
    pub fn verify(&self, token: &str, now: u64) -> Result<Claims, AuthError> {
        if token.len() > 65_536 {
            return Err(AuthError::Token);
        }
        let algorithm = jsonwebtoken::decode_header(token)
            .map_err(|_| AuthError::Token)?
            .alg;
        if algorithm != Algorithm::HS256 && algorithm != Algorithm::EdDSA {
            return Err(AuthError::Token);
        }
        let mut validation = Validation::new(algorithm);
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
        let claims = if algorithm == Algorithm::EdDSA {
            self.eddsa
                .as_ref()
                .ok_or(AuthError::Token)?
                .verify(token, &validation, now)?
        } else {
            jsonwebtoken::decode::<Claims>(token, &self.decode, &validation)
                .map_err(|_| AuthError::Token)?
                .claims
        };
        if claims.sub.is_empty()
            || claims.sub != claims.user_id
            || claims.tenant_id.is_empty()
            || uuid::Uuid::parse_str(&claims.jti).is_err()
            || claims.exp <= now
            || claims.iat > now
            || claims.nbf.is_some_and(|nbf| nbf > now)
            || claims.exp <= claims.iat
            || claims.exp - claims.iat
                > if algorithm == Algorithm::EdDSA {
                    access_tokens::ACCESS_TOKEN_TTL
                } else {
                    10_800
                }
        {
            return Err(AuthError::Token);
        }
        Ok(claims)
    }
}

pub mod api_keys;

pub mod admission;

mod password_rehash;
pub mod passwords;

pub mod access_tokens;
pub mod browser;
pub mod browser_policy;

mod browser_authority;
pub mod browser_contract;
pub mod enterprise;
pub mod enterprise_contract;
