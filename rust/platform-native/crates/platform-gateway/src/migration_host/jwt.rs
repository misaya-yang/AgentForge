//! Permissive-claim JWT decoder used by streaming identity. Account/session
//! authority is still enforced by endpoint dependencies, as in the Python host.
use ai_platform_auth::{JwtKeyConfig, JwtKeyRing, JwtSigningMode, JwtVerificationError};
use jsonwebtoken::{Algorithm, Validation};
use serde_json::Value;
use std::sync::Arc;
pub struct JwtDecoder {
    keyring: Arc<JwtKeyRing>,
    validation: Validation,
}
impl JwtDecoder {
    /// Explicit one-key compatibility constructor for existing HS256 callers.
    /// New deployments should use from_keyring with asymmetric public JWKs.
    pub fn new(
        secret: &str,
        algorithms: &[String],
        audience: Option<&str>,
        issuer: Option<&str>,
    ) -> Result<Self, String> {
        if algorithms.iter().any(|algorithm| algorithm != "HS256") || algorithms.len() > 1 {
            return Err("JWT algorithm not supported".to_owned());
        }
        Self::from_keyring_config(
            JwtSigningMode::LegacySingleHs256,
            secret,
            None,
            &[],
            audience,
            issuer,
        )
    }

    pub fn from_keyring_config(
        mode: JwtSigningMode,
        legacy_secret: &str,
        active_kid: Option<&str>,
        keys: &[JwtKeyConfig],
        audience: Option<&str>,
        issuer: Option<&str>,
    ) -> Result<Self, String> {
        let keyring =
            JwtKeyRing::for_verification(mode, legacy_secret.as_bytes(), active_kid, keys)
                .map_err(|_| "JWT keyring configuration invalid".to_owned())?;
        Self::from_keyring(Arc::new(keyring), audience, issuer)
    }

    pub fn from_keyring(
        keyring: Arc<JwtKeyRing>,
        audience: Option<&str>,
        issuer: Option<&str>,
    ) -> Result<Self, String> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.required_spec_claims.clear();
        validation.leeway = 0;
        validation.validate_nbf = true;
        validation.validate_aud = audience.is_some();
        if let Some(audience) = audience {
            validation.set_audience(&[audience]);
            validation.required_spec_claims.insert("aud".into());
        }
        if let Some(issuer) = issuer {
            validation.set_issuer(&[issuer]);
            validation.required_spec_claims.insert("iss".into());
        }
        Ok(Self {
            keyring,
            validation,
        })
    }
    pub fn decode(&self, token: &str) -> Result<Value, String> {
        let now = chrono::Utc::now().timestamp();
        if now < 0 {
            return Err("Invalid token".to_owned());
        }
        let value = self
            .keyring
            .decode_with_error::<Value>(token, &self.validation, now as u64)
            .map_err(|error| {
                // Keep diagnostic classes observable without logging the token,
                // claims, signing material, or provider-generated error text.
                match error {
                    JwtVerificationError::Expired => {
                        tracing::warn!(event = "gateway.jwt.expired", "JWT token expired");
                    }
                    JwtVerificationError::InvalidSignature => {
                        tracing::warn!(
                            event = "gateway.jwt.invalid_signature",
                            "JWT signature invalid"
                        );
                    }
                    JwtVerificationError::InvalidToken => {
                        tracing::warn!(event = "gateway.jwt.invalid", "JWT token invalid");
                    }
                }
                "Invalid token".to_owned()
            })?;
        if let Some(iat) = value.get("iat") {
            let iat = iat
                .as_i64()
                .or_else(|| iat.as_f64().map(|v| v as i64))
                .or_else(|| iat.as_str().and_then(|s| s.parse().ok()))
                .ok_or("Invalid token")?;
            if iat > now {
                return Err("Invalid token".into());
            }
        }
        for field in ["sub", "jti"] {
            if value.get(field).is_some_and(|v| !v.is_string()) {
                return Err("Invalid token".into());
            }
        }
        Ok(value)
    }
}
