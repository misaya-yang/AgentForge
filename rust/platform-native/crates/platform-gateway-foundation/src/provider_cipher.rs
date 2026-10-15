//! Native adapter for `ai_gateway_core.security.secrets`, re-exported by
//! `src/core/crypto.py`. This module owns no configuration or long-lived secret.
//! Construction is inert; callers pass the configured key to each operation.
//!
//! SHA-256 key derivation and the `enc:` envelope match the source. Fernet 0.2.2
//! (RustCrypto backend) owns all encryption, authentication and padding. No TTL
//! is imposed. Python 3.12's permissive Base64 decoding is normalized before
//! Fernet authenticates the exact decoded bytes; invalid tokens and invalid UTF-8
//! preserve the original input. Python 3.13 differs on malformed padding; the
//! deployment target here follows the repository Dockerfile's Python 3.12.
//!
//! Fernet's encryption API panics on clock/entropy errors rather than returning
//! a Result. A narrow unwind boundary preserves the source's input fallback in
//! unwind builds. Its synthetic seam is tested, not actual OS entropy failure.
//! Panic-abort builds, allocator aborts and process termination cannot recover.
//! No panic hook is changed and no secret-bearing diagnostic is emitted here.
//! Master-key generation and the source's LRU cache are outside this adapter.

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
        // The upstream API owns its clock and OS entropy. Catch its documented
        // unwrap/expect failures only at this call boundary, never across service
        // or database work. Existing enc: strings are deliberately encrypted again.
        Ok(encrypt_with(value, encryption_key, |cipher, bytes| {
            Ok(cipher.encrypt(bytes))
        }))
    }

    fn decrypt(&self, value: &str, encryption_key: &str) -> Result<String, ServiceError> {
        if !is_encrypted(value) {
            return Ok(value.to_owned());
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
        Ok(plaintext.unwrap_or_else(|| value.to_owned()))
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
    if encryption_key.is_empty() {
        None
    } else {
        Fernet::new(&derive_fernet_key(encryption_key))
    }
}

/// The private Result seam exercises source exception fallback without replacing
/// crypto, exposing deterministic IVs, or claiming an actual OS failure occurred.
fn encrypt_with<F>(value: &str, encryption_key: &str, encrypt: F) -> String
where
    F: FnOnce(&Fernet, &[u8]) -> Result<String, ()> + UnwindSafe,
{
    if value.is_empty() {
        return value.to_owned();
    }
    let Some(cipher) = get_fernet(encryption_key) else {
        return value.to_owned();
    };
    std::panic::catch_unwind(|| encrypt(&cipher, value.as_bytes()))
        .ok()
        .and_then(Result::ok)
        .map(|token| format!("enc:{token}"))
        .unwrap_or_else(|| value.to_owned())
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

