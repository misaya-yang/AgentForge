//! Credential boundary for canonical `src/api/v1/connectors.py`.
//!
//! The host explicitly supplies its captured environment. Config decryption uses
//! an injected key even when empty; user tokens fall back to the gateway key when
//! the injected string is empty. No environment, service or file is read here.
//! The existing NativeProviderCipher owns Fernet compatibility. Its legacy
//! pass-through behavior is checked at this boundary: protected input must yield
//! nonempty, non-`enc:` plaintext, and new nonempty tokens must yield `enc:`.

use crate::{
    connector_user_types::{ConnectorEnvironment, ConnectorError, ConnectorTokenCipher},
    provider_cipher::{NativeProviderCipher, is_encrypted},
    provider_service::ProviderCipher,
};
use std::fmt;

#[derive(Clone)]
pub struct NativeConnectorTokenCipher {
    config_key: String,
    user_key: String,
}

impl NativeConnectorTokenCipher {
    pub fn new(environment: ConnectorEnvironment) -> Self {
        let config_key = environment
            .injected_encryption_key
            .clone()
            .unwrap_or_else(|| environment.gateway_encryption_key.clone());
        let user_key = environment
            .injected_encryption_key
            .filter(|key| !key.is_empty())
            .unwrap_or(environment.gateway_encryption_key);
        Self {
            config_key,
            user_key,
        }
    }

    fn encrypt_with(
        &self,
        value: Option<&str>,
        cipher: &dyn ProviderCipher,
    ) -> Result<Option<String>, ConnectorError> {
        let Some(value) = value else { return Ok(None) };
        if value.is_empty() || is_encrypted(value) {
            return Ok(Some(value.to_owned()));
        }
        if self.user_key.is_empty() {
            return Err(ConnectorError::detail(
                503,
                "Connector token encryption is not configured",
            ));
        }
        let encrypted = cipher
            .encrypt(value, &self.user_key)
            .map_err(|_| ConnectorError::detail(500, "Connector token encryption failed"))?;
        if !is_encrypted(&encrypted) {
            return Err(ConnectorError::detail(
                500,
                "Connector token encryption failed",
            ));
        }
        Ok(Some(encrypted))
    }
}

impl fmt::Debug for NativeConnectorTokenCipher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeConnectorTokenCipher(<redacted>)")
    }
}

fn decrypt_protected(
    value: &str,
    key: &str,
    missing: &'static str,
    failed: &'static str,
) -> Result<String, ConnectorError> {
    if !is_encrypted(value) {
        return Ok(value.to_owned());
    }
    if key.is_empty() {
        return Err(ConnectorError::detail(503, missing));
    }
    let plaintext = NativeProviderCipher::new()
        .decrypt(value, key)
        .map_err(|_| ConnectorError::detail(503, failed))?;
    if plaintext.is_empty() || is_encrypted(&plaintext) {
        return Err(ConnectorError::detail(503, failed));
    }
    Ok(plaintext)
}

impl ConnectorTokenCipher for NativeConnectorTokenCipher {
    fn decrypt_config(&self, value: &str) -> Result<String, ConnectorError> {
        decrypt_protected(
            value,
            &self.config_key,
            "Connector secret decryption is not configured",
            "Connector secret cannot be decrypted",
        )
    }

    fn encrypt_user(&self, value: Option<&str>) -> Result<Option<String>, ConnectorError> {
        self.encrypt_with(value, &NativeProviderCipher::new())
    }

    fn decrypt_user(&self, value: Option<&str>) -> Result<Option<String>, ConnectorError> {
        value
            .map(|value| {
                decrypt_protected(
                    value,
                    &self.user_key,
                    "Connector token decryption is not configured",
                    "Connector token cannot be decrypted",
                )
            })
            .transpose()
    }
}

