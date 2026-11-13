//! Authenticated provider-secret envelopes, compatible with existing Fernet
//! `enc:` records. Stored nonempty credentials require an envelope and a key;
//! missing keys, invalid tokens and crypto failures return a redacted error.
//! Construction is inert and callers supply the key explicitly. Legacy Base64
//! normalization preserves existing authenticated token bytes, never plaintext.

use crate::provider_service::{ProviderCipher, ServiceError};
use base64::{
    Engine, alphabet,
    engine::{GeneralPurpose, GeneralPurposeConfig, general_purpose::URL_SAFE},
};
use fernet::Fernet;
use sha2::{Digest, Sha256};
use std::{fmt, panic::UnwindSafe};

/// A stateless, offline cipher adapter. Neither construction nor Debug accesses
/// the environment, random generator, system clock, filesystem or a service.
#[derive(Clone, Copy, Default)]
pub struct NativeProviderCipher;

impl NativeProviderCipher {
    pub const fn new() -> Self {
        Self
    }
}

impl fmt::Debug for NativeProviderCipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeProviderCipher(<internal>)")
    }
}

impl ProviderCipher for NativeProviderCipher {
    fn encrypt(&self, value: &str, encryption_key: &str) -> Result<String, ServiceError> {
        // Preserve the established envelope format. Crypto failure must never
        // turn a successful storage write into an unencrypted credential.
        encrypt_with(value, encryption_key, |cipher, bytes| {
            Ok(cipher.encrypt(bytes))
        })
    }

    fn decrypt(&self, value: &str, encryption_key: &str) -> Result<String, ServiceError> {
        if value.is_empty() {
            return Ok(String::new());
        }
        if !is_encrypted(value) {
            return Err(ServiceError::Cipher);
        }
        let plaintext = get_fernet(encryption_key).and_then(|cipher| {
            let token = normalize_python312_base64(&value[4..])?;
            // fernet 0.2.2 checks future clock skew even with ttl=None, whereas
            // cryptography.Fernet does not. Its MAX_CLOCK_SKEW is exactly 60.
            // At u64::MAX - 60, the addition is overflow-free and EVERY u64 token
            // timestamp passes. None also avoids timestamp + ttl arithmetic.
            // Token/version/timestamp/IV/ciphertext/MAC bytes are never rewritten;
            // the library still verifies HMAC before decrypting and unpadding.
            let bytes = cipher.decrypt_at_time(&token, None, u64::MAX - 60).ok()?;
            String::from_utf8(bytes).ok()
        });
        plaintext.ok_or(ServiceError::Cipher)
    }
}

/// Prefix detection deliberately says nothing about token validity.
pub fn is_encrypted(value: &str) -> bool {
    value.starts_with("enc:")
}

fn derive_fernet_key(encryption_key: &str) -> String {
    URL_SAFE.encode(Sha256::digest(encryption_key.as_bytes()))
}

fn get_fernet(encryption_key: &str) -> Option<Fernet> {
    if encryption_key.trim().is_empty() {
        None
    } else {
        Fernet::new(&derive_fernet_key(encryption_key))
    }
}

/// Keep OS clock/entropy failures inside the credential boundary and fail closed.
fn encrypt_with<F>(value: &str, encryption_key: &str, encrypt: F) -> Result<String, ServiceError>
where
    F: FnOnce(&Fernet, &[u8]) -> Result<String, ()> + UnwindSafe,
{
    if value.is_empty() {
        return Ok(String::new());
    }
    let Some(cipher) = get_fernet(encryption_key) else {
        return Err(ServiceError::Cipher);
    };
    std::panic::catch_unwind(|| encrypt(&cipher, value.as_bytes()))
        .ok()
        .and_then(Result::ok)
        .map(|token| format!("enc:{token}"))
        .ok_or(ServiceError::Cipher)
}

/// Normalize the *encoding*, not the token. CPython 3.12 urlsafe_b64decode first
/// translates -_ to +/, discards non-alphabet UTF-8 bytes, ignores nonterminal
/// padding, and stops at enough padding after 2 or 3 symbols in a quartet. Missing
/// required padding still fails. Unused pad bits are not required to be zero.
/// The Base64 crate decodes the selected symbols and re-encodes the resulting
/// bytes canonically, so Fernet sees precisely the bytes Python authenticates.
fn normalize_python312_base64(input: &str) -> Option<String> {
    let mut symbols = Vec::with_capacity(input.len());
    let mut padding = 0;
    let mut terminated = false;
    for byte in input.bytes() {
        match byte {
            b'=' => {
                let quartet_position = symbols.len() % 4;
                if quartet_position >= 2 {
                    padding += 1;
                    if quartet_position + padding >= 4 {
                        symbols.resize(symbols.len() + 4 - quartet_position, b'=');
                        terminated = true;
                        break;
                    }
                }
            }
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'+' | b'/' => {
                symbols.push(match byte {
                    b'+' => b'-',
                    b'/' => b'_',
                    other => other,
                });
                padding = 0;
            }
            _ => {}
        }
    }
    if !terminated && symbols.len() % 4 != 0 {
        return None;
    }
    let decoder = GeneralPurpose::new(
        &alphabet::URL_SAFE,
        GeneralPurposeConfig::new().with_decode_allow_trailing_bits(true),
    );
    decoder
        .decode(symbols)
        .ok()
        .map(|bytes| URL_SAFE.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_plaintext_and_invalid_envelopes_are_rejected_without_echoing_input() {
        let cipher = NativeProviderCipher::new();
        for value in ["synthetic-secret", "enc:", "enc:invalid-token"] {
            let error = cipher.decrypt(value, "synthetic-master-key").unwrap_err();
            assert_eq!(error, ServiceError::Cipher);
            assert!(!error.to_string().contains(value));
        }
        assert_eq!(cipher.decrypt("", "").unwrap(), "");
    }

    #[test]
    fn existing_envelope_roundtrip_requires_the_matching_key() {
        let cipher = NativeProviderCipher::new();
        let sealed = cipher.encrypt("synthetic-密钥", "matching-key").unwrap();
        assert!(is_encrypted(&sealed));
        assert_eq!(
            cipher.decrypt(&sealed, "matching-key").unwrap(),
            "synthetic-密钥"
        );
        assert_eq!(
            cipher.decrypt(&sealed, "another-key"),
            Err(ServiceError::Cipher)
        );
        assert_eq!(cipher.decrypt(&sealed, ""), Err(ServiceError::Cipher));
        let mut tampered = sealed.into_bytes();
        tampered[20] = if tampered[20] == b'A' { b'B' } else { b'A' };
        assert_eq!(
            cipher.decrypt(std::str::from_utf8(&tampered).unwrap(), "matching-key"),
            Err(ServiceError::Cipher)
        );
    }

    #[test]
    fn failed_encryption_never_returns_the_original_credential() {
        let secret = "synthetic-secret";
        assert_eq!(
            encrypt_with(secret, "", |_, _| unreachable!()),
            Err(ServiceError::Cipher)
        );
        assert_eq!(
            encrypt_with(secret, "key", |_, _| Err(())),
            Err(ServiceError::Cipher)
        );
        assert_eq!(
            encrypt_with(secret, "key", |_, _| panic!("synthetic crypto failure")),
            Err(ServiceError::Cipher)
        );
        assert_eq!(
            NativeProviderCipher::new().encrypt(secret, "   "),
            Err(ServiceError::Cipher)
        );
    }

    #[test]
    fn authenticated_non_utf8_credentials_are_rejected() {
        let token = get_fernet("key").unwrap().encrypt(&[0xff, 0xfe]);
        assert_eq!(
            NativeProviderCipher::new().decrypt(&format!("enc:{token}"), "key"),
            Err(ServiceError::Cipher)
        );
    }
}
