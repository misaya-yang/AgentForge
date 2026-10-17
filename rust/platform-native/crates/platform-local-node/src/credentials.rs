//! Secure storage is explicit; there is no plaintext fallback.
use crate::{Error, Result};
pub const DEVICE_IDENTITY_KEYCHAIN_SERVICE: &str = "ai-platform.local-node.device-identity.v1";
pub const DEVICE_CREDENTIAL_KEYCHAIN_SERVICE: &str = "ai-platform.local-node.device-credential.v2";
pub trait SecureCredentialStore: Send + Sync {
    fn ready(&self) -> bool;
    fn load(&self, reference: &str) -> Result<Option<Vec<u8>>>;
    fn store(&self, reference: &str, secret: &[u8]) -> Result<()>;
}
pub struct UnavailableStore;
impl SecureCredentialStore for UnavailableStore {
    fn ready(&self) -> bool {
        false
    }
    fn load(&self, _: &str) -> Result<Option<Vec<u8>>> {
        Err(Error::Denied("secure credential storage unavailable"))
    }
    fn store(&self, _: &str, _: &[u8]) -> Result<()> {
        Err(Error::Denied("secure credential storage unavailable"))
    }
}
#[cfg(target_os = "macos")]
pub struct MacKeychainStore {
    service: String,
}
#[cfg(target_os = "macos")]
impl MacKeychainStore {
    pub fn new(service: String) -> Result<Self> {
        if service.is_empty() || service.len() > 512 {
            return Err(Error::Denied("keychain service"));
        }
        Ok(Self { service })
    }
}
#[cfg(target_os = "macos")]
impl SecureCredentialStore for MacKeychainStore {
    fn ready(&self) -> bool {
        true
    }
    fn load(&self, reference: &str) -> Result<Option<Vec<u8>>> {
        match security_framework::passwords::get_generic_password(&self.service, reference) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.code() == -25300 => Ok(None),
            Err(_) => Err(Error::Denied("keychain unavailable")),
        }
    }
    fn store(&self, reference: &str, secret: &[u8]) -> Result<()> {
        security_framework::passwords::set_generic_password(&self.service, reference, secret)
            .map_err(|_| Error::Denied("keychain store failed"))
    }
}

#[cfg(target_os = "linux")]
#[path = "credential_linux.rs"]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::LinuxSecretServiceStore;
#[cfg(target_os = "windows")]
#[path = "credential_windows.rs"]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::WindowsCredentialStore;

fn validate_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 512
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(Error::Denied("invalid secure credential namespace"));
    }
    Ok(())
}
/// Select the platform vault. Construction never falls back to plaintext storage.
pub fn native_secure_store(service: &str) -> Result<Box<dyn SecureCredentialStore>> {
    validate_name(service)?;
    #[cfg(target_os = "macos")]
    {
        return Ok(Box::new(MacKeychainStore::new(service.into())?));
    }
    #[cfg(target_os = "linux")]
    {
        return Ok(Box::new(LinuxSecretServiceStore::new(service.into())?));
    }
    #[cfg(target_os = "windows")]
    {
        return Ok(Box::new(WindowsCredentialStore::new(service.into())?));
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Err(Error::Denied("secure credential storage unavailable"))
    }
}
#[cfg(test)]
mod namespace_tests {
    use super::*;
    #[test]
    fn rejects_aliases_and_control_characters() {
        for invalid in ["", "a/b", "a\0b", "a\nb", "a b"] {
            assert!(validate_name(invalid).is_err());
        }
        assert!(validate_name(DEVICE_IDENTITY_KEYCHAIN_SERVICE).is_ok());
        assert!(validate_name("device_identity_7f38-1234").is_ok());
    }
}
