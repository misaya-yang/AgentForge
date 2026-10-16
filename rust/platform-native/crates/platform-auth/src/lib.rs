//! Authentication services and router factories. The Gateway owns process
//! configuration and listener startup; durable PostgreSQL/Redis authority is
//! required for protected requests.
pub mod accounts;
pub mod contract;
pub mod http;
pub mod rbac;
pub mod service;
pub mod sessions;
use jsonwebtoken::jwk::{
    AlgorithmParameters, EllipticCurve, Jwk, JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse,
};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use regex::Regex;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    str::FromStr,
    sync::{Arc, LazyLock},
};

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
/// Signing mode for account-session JWTs. The asymmetric mode is the secure
/// default; the single HMAC mode exists only for explicit deployment migration.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JwtSigningMode {
    AsymmetricKeyring,
    LegacySingleHs256,
}
impl Default for JwtSigningMode {
    fn default() -> Self {
        Self::AsymmetricKeyring
    }
}

/// One public verification key and, for the active key only, its private signer.
/// The public JWK is the only representation ever returned from the JWKS route.
/// This type deliberately has no Debug implementation because it may contain a
/// private signing key.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JwtKeyConfig {
    pub kid: String,
    pub algorithm: String,
    pub public_jwk: Value,
    #[serde(default)]
    pub private_key_pem: Option<String>,
    #[serde(default)]
    pub verify_until: Option<u64>,
}

struct VerificationKey {
    algorithm: Algorithm,
    decoding: DecodingKey,
    encoding: Option<EncodingKey>,
    verify_until: Option<u64>,
    public_jwk: Option<Jwk>,
}

/// Validated JWT verification and active-signing keyring. Its public methods
/// never return private key material and its fields have no Debug projection.
pub struct JwtKeyRing {
    keys: BTreeMap<String, VerificationKey>,
    active_kid: Option<String>,
    allow_missing_kid: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JwtVerificationError {
    InvalidToken,
    InvalidSignature,
    Expired,
}
impl JwtKeyRing {
    pub fn for_signing(
        mode: JwtSigningMode,
        legacy_secret: &[u8],
        active_kid: Option<&str>,
        keys: &[JwtKeyConfig],
    ) -> Result<Self, AuthError> {
        Self::build(mode, legacy_secret, active_kid, keys, true)
    }

    pub fn for_verification(
        mode: JwtSigningMode,
        legacy_secret: &[u8],
        active_kid: Option<&str>,
        keys: &[JwtKeyConfig],
    ) -> Result<Self, AuthError> {
        Self::build(mode, legacy_secret, active_kid, keys, false)
    }

    fn build(
        mode: JwtSigningMode,
        legacy_secret: &[u8],
        active_kid: Option<&str>,
        configs: &[JwtKeyConfig],
        require_signer: bool,
    ) -> Result<Self, AuthError> {
        match mode {
            JwtSigningMode::LegacySingleHs256 => {
                if legacy_secret.len() < 32
                    || !configs.is_empty()
                    || active_kid.is_some_and(|kid| kid != "legacy")
                {
                    return Err(AuthError::Configuration);
                }
                let mut keys = BTreeMap::new();
                keys.insert(
                    "legacy".to_owned(),
                    VerificationKey {
                        algorithm: Algorithm::HS256,
                        decoding: DecodingKey::from_secret(legacy_secret),
                        encoding: require_signer.then(|| EncodingKey::from_secret(legacy_secret)),
                        verify_until: None,
                        public_jwk: None,
                    },
                );
                Ok(Self {
                    keys,
                    active_kid: Some("legacy".to_owned()),
                    allow_missing_kid: true,
                })
            }
            JwtSigningMode::AsymmetricKeyring => {
                if !legacy_secret.is_empty()
                    || configs.is_empty()
                    || configs.len() > 64
                    || active_kid.is_none_or(|kid| !valid_kid(kid))
                {
                    return Err(AuthError::Configuration);
                }
                let active_kid = active_kid.expect("validated active kid").to_owned();
                let mut keys = BTreeMap::new();
                for config in configs {
                    if !valid_kid(&config.kid) || keys.contains_key(&config.kid) {
                        return Err(AuthError::Configuration);
                    }
                    let active = config.kid == active_kid;
                    if (active && config.verify_until.is_some())
                        || (!active && config.verify_until.is_none())
                    {
                        return Err(AuthError::Configuration);
                    }
                    if (!active && config.private_key_pem.is_some())
                        || (!require_signer && config.private_key_pem.is_some())
                        || (active && require_signer && config.private_key_pem.is_none())
                    {
                        return Err(AuthError::Configuration);
                    }

                    let algorithm = asymmetric_algorithm(&config.algorithm)?;
                    let mut jwk = public_jwk(config, algorithm)?;
                    let decoding =
                        DecodingKey::from_jwk(&jwk).map_err(|_| AuthError::Configuration)?;
                    let encoding = if active && require_signer {
                        let pem = config
                            .private_key_pem
                            .as_deref()
                            .filter(|pem| !pem.trim().is_empty())
                            .ok_or(AuthError::Configuration)?;
                        let encoding = asymmetric_encoding_key(algorithm, pem)?;
                        verify_key_pair(&config.kid, algorithm, &encoding, &decoding)?;
                        Some(encoding)
                    } else {
                        None
                    };
                    // Normalize public metadata so the JWKS advertises verify-only use.
                    jwk.common.public_key_use = Some(PublicKeyUse::Signature);
                    jwk.common.key_operations = Some(vec![KeyOperations::Verify]);
                    keys.insert(
                        config.kid.clone(),
                        VerificationKey {
                            algorithm,
                            decoding,
                            encoding,
                            verify_until: config.verify_until,
                            public_jwk: Some(jwk),
                        },
                    );
                }
                if !keys.contains_key(&active_kid) {
                    return Err(AuthError::Configuration);
                }
                Ok(Self {
                    keys,
                    active_kid: Some(active_kid),
                    allow_missing_kid: false,
                })
            }
        }
    }

    /// Decode a JWT with the key selected by its exact kid and algorithm.
    /// Missing kid is accepted only for the explicit single-HS256 compatibility
    /// mode; unknown kid never falls back to another key.
    pub fn decode<T: DeserializeOwned>(
        &self,
        token: &str,
        validation: &Validation,
        now: u64,
    ) -> Result<T, AuthError> {
        self.decode_with_error(token, validation, now)
            .map_err(|_| AuthError::Token)
    }

    pub fn decode_with_error<T: DeserializeOwned>(
        &self,
        token: &str,
        validation: &Validation,
        now: u64,
    ) -> Result<T, JwtVerificationError> {
        if token.len() > 65_536 {
            return Err(JwtVerificationError::InvalidToken);
        }
        let header =
            jsonwebtoken::decode_header(token).map_err(|_| JwtVerificationError::InvalidToken)?;
        let kid = match header.kid.as_deref() {
            Some(kid) => kid,
            None if self.allow_missing_kid => self
                .active_kid
                .as_deref()
                .ok_or(JwtVerificationError::InvalidToken)?,
            None => return Err(JwtVerificationError::InvalidToken),
        };
        let key = self
            .keys
            .get(kid)
            .ok_or(JwtVerificationError::InvalidToken)?;
        if header.alg != key.algorithm {
            return Err(JwtVerificationError::InvalidToken);
        }
        if key.verify_until.is_some_and(|until| now >= until) {
            return Err(JwtVerificationError::Expired);
        }
        let mut validation = validation.clone();
        validation.algorithms = vec![key.algorithm];
        jsonwebtoken::decode::<T>(token, &key.decoding, &validation)
            .map(|decoded| decoded.claims)
            .map_err(|error| match error.kind() {
                jsonwebtoken::errors::ErrorKind::ExpiredSignature => JwtVerificationError::Expired,
                jsonwebtoken::errors::ErrorKind::InvalidSignature => {
                    JwtVerificationError::InvalidSignature
                }
                _ => JwtVerificationError::InvalidToken,
            })
    }

    pub fn jwks_at(&self, now: u64) -> Value {
        let keys = self
            .keys
            .values()
            .filter(|key| key.verify_until.is_none_or(|until| now < until))
            .filter_map(|key| key.public_jwk.clone())
            .collect();
        serde_json::to_value(JwkSet { keys }).unwrap_or_else(|_| serde_json::json!({"keys":[]}))
    }

    fn active_signer(&self) -> Result<(&str, Algorithm, &EncodingKey), AuthError> {
        let kid = self.active_kid.as_deref().ok_or(AuthError::Configuration)?;
        let key = self.keys.get(kid).ok_or(AuthError::Configuration)?;
        let encoding = key.encoding.as_ref().ok_or(AuthError::Configuration)?;
        Ok((kid, key.algorithm, encoding))
    }
}

fn valid_kid(kid: &str) -> bool {
    !kid.is_empty()
        && kid.len() <= 128
        && kid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn asymmetric_algorithm(name: &str) -> Result<Algorithm, AuthError> {
    match name {
        "RS256" => Ok(Algorithm::RS256),
        "ES256" => Ok(Algorithm::ES256),
        "EdDSA" => Ok(Algorithm::EdDSA),
        _ => Err(AuthError::Configuration),
    }
}

fn public_jwk(config: &JwtKeyConfig, algorithm: Algorithm) -> Result<Jwk, AuthError> {
    let object = config
        .public_jwk
        .as_object()
        .ok_or(AuthError::Configuration)?;
    if ["d", "p", "q", "dp", "dq", "qi", "oth", "k"]
        .iter()
        .any(|field| object.contains_key(*field))
    {
        return Err(AuthError::Configuration);
    }
    let mut jwk: Jwk =
        serde_json::from_value(config.public_jwk.clone()).map_err(|_| AuthError::Configuration)?;
    if jwk.common.key_id.as_deref() != Some(config.kid.as_str())
        || jwk
            .common
            .key_algorithm
            .is_none_or(|key_algorithm| format!("{key_algorithm:?}") != config.algorithm)
        || jwk
            .common
            .public_key_use
            .as_ref()
            .is_some_and(|usage| usage != &PublicKeyUse::Signature)
        || jwk
            .common
            .key_operations
            .as_ref()
            .is_some_and(|operations| {
                operations
                    .iter()
                    .any(|operation| operation == &KeyOperations::Sign)
                    || !operations.contains(&KeyOperations::Verify)
            })
    {
        return Err(AuthError::Configuration);
    }
    let algorithm_matches_key = match (&jwk.algorithm, algorithm) {
        (AlgorithmParameters::RSA(_), Algorithm::RS256) => true,
        (AlgorithmParameters::EllipticCurve(parameters), Algorithm::ES256) => {
            parameters.curve == EllipticCurve::P256
        }
        (AlgorithmParameters::OctetKeyPair(parameters), Algorithm::EdDSA) => {
            parameters.curve == EllipticCurve::Ed25519
        }
        _ => false,
    };
    let expected_key_algorithm =
        KeyAlgorithm::from_str(&config.algorithm).map_err(|_| AuthError::Configuration)?;
    if !algorithm_matches_key || jwk.common.key_algorithm != Some(expected_key_algorithm) {
        return Err(AuthError::Configuration);
    }
    jwk.common.public_key_use = Some(PublicKeyUse::Signature);
    jwk.common.key_operations = Some(vec![KeyOperations::Verify]);
    Ok(jwk)
}

fn asymmetric_encoding_key(algorithm: Algorithm, pem: &str) -> Result<EncodingKey, AuthError> {
    match algorithm {
        Algorithm::RS256 => EncodingKey::from_rsa_pem(pem.as_bytes()),
        Algorithm::ES256 => EncodingKey::from_ec_pem(pem.as_bytes()),
        Algorithm::EdDSA => EncodingKey::from_ed_pem(pem.as_bytes()),
        _ => return Err(AuthError::Configuration),
    }
    .map_err(|_| AuthError::Configuration)
}

fn verify_key_pair(
    kid: &str,
    algorithm: Algorithm,
    encoding: &EncodingKey,
    decoding: &DecodingKey,
) -> Result<(), AuthError> {
    let mut header = Header::new(algorithm);
    header.kid = Some(kid.to_owned());
    let token = jsonwebtoken::encode(
        &header,
        &serde_json::json!({"key_pair_check":true}),
        encoding,
    )
    .map_err(|_| AuthError::Configuration)?;
    let mut validation = Validation::new(algorithm);
    validation.required_spec_claims.clear();
    validation.validate_exp = false;
    validation.validate_aud = false;
    jsonwebtoken::decode::<Value>(&token, decoding, &validation)
        .map(|_| ())
        .map_err(|_| AuthError::Configuration)
}

/// Narrow signing capability for internal service tokens. It exposes no key
/// bytes and always selects the configured active kid and algorithm.
pub struct JwtSigner {
    keyring: Arc<JwtKeyRing>,
}
impl JwtSigner {
    pub fn sign<T: Serialize>(&self, claims: &T) -> Result<String, AuthError> {
        let (kid, algorithm, encoding) = self.keyring.active_signer()?;
        let mut header = Header::new(algorithm);
        header.kid = Some(kid.to_owned());
        jsonwebtoken::encode(&header, claims, encoding).map_err(|_| AuthError::Crypto)
    }
}

/// No Debug implementation: signing material must not reach diagnostics.
pub struct TokenCodec {
    keyring: Arc<JwtKeyRing>,
    audience: Option<String>,
    issuer: Option<String>,
}
impl TokenCodec {
    /// Explicit single-key HS256 compatibility constructor. New Gateway
    /// configuration should use from_keyring with asymmetric keys.
    pub fn new(
        secret: &[u8],
        audience: Option<String>,
        issuer: Option<String>,
    ) -> Result<Self, AuthError> {
        Self::from_keyring(
            JwtSigningMode::LegacySingleHs256,
            secret,
            None,
            &[],
            audience,
            issuer,
        )
    }

    pub fn from_keyring(
        mode: JwtSigningMode,
        legacy_secret: &[u8],
        active_kid: Option<&str>,
        keys: &[JwtKeyConfig],
        audience: Option<String>,
        issuer: Option<String>,
    ) -> Result<Self, AuthError> {
        if audience.as_ref().is_some_and(|value| value.is_empty())
            || issuer.as_ref().is_some_and(|value| value.is_empty())
        {
            return Err(AuthError::Configuration);
        }
        Ok(Self {
            keyring: Arc::new(JwtKeyRing::for_signing(
                mode,
                legacy_secret,
                active_kid,
                keys,
            )?),
            audience,
            issuer,
        })
    }

    pub fn keyring(&self) -> Arc<JwtKeyRing> {
        Arc::clone(&self.keyring)
    }

    pub fn jwks_at(&self, now: u64) -> Value {
        self.keyring.jwks_at(now)
    }

    pub fn signer(&self) -> JwtSigner {
        JwtSigner {
            keyring: Arc::clone(&self.keyring),
        }
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
        self.signer().sign(&claims)
    }
    /// Verifies cryptographic claims only, not account state or Redis allowlisting.
    /// The caller must enforce those durable authorities before authorizing access.
    pub fn verify(&self, token: &str, now: u64) -> Result<Claims, AuthError> {
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
        let claims = self.keyring.decode::<Claims>(token, &validation, now)?;
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

#[cfg(test)]
#[path = "jwt_keyring_tests.rs"]
mod jwt_keyring_tests;
