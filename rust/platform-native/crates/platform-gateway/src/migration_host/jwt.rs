//! Permissive-claim JWT decoder used by streaming identity. Account/session
//! authority is still enforced by endpoint dependencies, as in the Python host.
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::Value;
use std::str::FromStr;
pub struct JwtDecoder {
    key: DecodingKey,
    validation: Validation,
}
impl JwtDecoder {
    pub fn new(
        secret: &str,
        algorithms: &[String],
        audience: Option<&str>,
        issuer: Option<&str>,
    ) -> Result<Self, String> {
        let algorithms = if algorithms.is_empty() {
            vec![Algorithm::HS256]
        } else {
            algorithms
                .iter()
                .map(|s| {
                    Algorithm::from_str(s).map_err(|_| "JWT algorithm not supported".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut validation = Validation::new(algorithms[0]);
        validation.algorithms = algorithms.clone();
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
        let key =
            match algorithms[0] {
                Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => {
                    DecodingKey::from_secret(secret.as_bytes())
                }
                Algorithm::RS256
                | Algorithm::RS384
                | Algorithm::RS512
                | Algorithm::PS256
                | Algorithm::PS384
                | Algorithm::PS512 => DecodingKey::from_rsa_pem(secret.as_bytes())
                    .map_err(|_| "JWT RSA key invalid")?,
                Algorithm::ES256 | Algorithm::ES384 => {
                    DecodingKey::from_ec_pem(secret.as_bytes()).map_err(|_| "JWT EC key invalid")?
                }
                Algorithm::EdDSA => DecodingKey::from_ed_pem(secret.as_bytes())
                    .map_err(|_| "JWT EdDSA key invalid")?,
            };
        Ok(Self { key, validation })
    }
    pub fn decode(&self, token: &str) -> Result<Value, String> {
        let value = jsonwebtoken::decode::<Value>(token, &self.key, &self.validation)
            .map_err(|_| "Invalid token".to_owned())?
            .claims;
        let now = chrono::Utc::now().timestamp();
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
