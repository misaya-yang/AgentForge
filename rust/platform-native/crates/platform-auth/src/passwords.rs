//! Bounded Argon2id credentials and transparent bcrypt migration.
// SPDX-License-Identifier: Apache-2.0
use crate::AuthError;
use argon2::{
    Algorithm, Argon2, Params, PasswordHash, PasswordHasher, PasswordVerifier, Version,
    password_hash::SaltString,
};
pub struct Verification {
    pub valid: bool,
    pub replacement: Option<String>,
}
fn engine() -> Result<Argon2<'static>, AuthError> {
    let params = Params::new(19_456, 2, 1, Some(32)).map_err(|_| AuthError::Configuration)?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}
pub fn hash(password: &str) -> Result<String, AuthError> {
    if password.is_empty() || password.len() > 1024 {
        return Err(AuthError::Password);
    }
    // UUID v4 is OS-random; all 122 random bits are retained in this unique salt.
    let salt =
        SaltString::encode_b64(uuid::Uuid::new_v4().as_bytes()).map_err(|_| AuthError::Crypto)?;
    engine()?
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| AuthError::Crypto)
}
pub fn verify(password: &str, encoded: &str) -> bool {
    if password.is_empty() || password.len() > 1024 || encoded.len() > 1024 {
        return false;
    }
    if encoded.starts_with("$2a$") || encoded.starts_with("$2b$") || encoded.starts_with("$2y$") {
        return password.len() <= 72 && bcrypt::verify(password, encoded).unwrap_or(false);
    }
    let Ok(parsed) = PasswordHash::new(encoded) else {
        return false;
    };
    if parsed.algorithm.as_str() != "argon2id"
        || parsed.version != Some(19)
        || !parsed
            .params
            .get_decimal("m")
            .is_some_and(|n| (19_456..=65_536).contains(&n))
        || !parsed
            .params
            .get_decimal("t")
            .is_some_and(|n| (2..=10).contains(&n))
        || !parsed
            .params
            .get_decimal("p")
            .is_some_and(|n| (1..=4).contains(&n))
    {
        return false;
    }
    engine().is_ok_and(|e| e.verify_password(password.as_bytes(), &parsed).is_ok())
}
pub fn verify_upgrade(password: &str, encoded: &str) -> Result<Verification, AuthError> {
    let valid = verify(password, encoded);
    let replacement = if valid && encoded.starts_with("$2") {
        Some(hash(password)?)
    } else {
        None
    };
    Ok(Verification { valid, replacement })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn new_credentials_are_argon2id_and_bcrypt_rehash_requires_success() {
        let old = bcrypt::hash("Strong1!", 4).unwrap();
        assert!(!verify_upgrade("wrong", &old).unwrap().valid);
        assert!(verify_upgrade("wrong", &old).unwrap().replacement.is_none());
        let checked = verify_upgrade("Strong1!", &old).unwrap();
        assert!(checked.valid);
        let new = checked.replacement.unwrap();
        assert!(new.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert!(verify("Strong1!", &new));
        assert!(!verify("wrong", &new));
        assert!(
            verify_upgrade("Strong1!", &new)
                .unwrap()
                .replacement
                .is_none()
        );
        assert!(!verify("Strong1!", &new.replace("m=19456", "m=999999999")));
    }
    #[test]
    fn malformed_phc_and_unbounded_work_are_rejected_without_fallback() {
        assert!(crate::PasswordEngine::new(0).is_err());
        assert!(crate::PasswordEngine::new(9).is_err());
        for encoded in [
            "$argon2i$v=19$m=19456,t=2,p=1$c2FsdA$invalid",
            "$argon2id$v=16$m=19456,t=2,p=1$c2FsdA$invalid",
            "$2b$garbage",
            "not a hash",
        ] {
            assert!(!verify("Strong1!", encoded));
        }
        let old = bcrypt::hash("Strong1!", 4).unwrap();
        assert!(!verify(&"x".repeat(73), &old));
    }
}
