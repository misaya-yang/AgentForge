use super::{LocalNodeError as Error, Result};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialStorageStatus {
    Ready,
    Unavailable,
    TestOnlyInsecure,
}
pub trait SecureCredentialStore: Send + Sync {
    fn status(&self) -> CredentialStorageStatus;
    fn load(&self, reference: &str) -> Result<Option<Vec<u8>>>;
    fn store(&self, reference: &str, secret: &[u8]) -> Result<()>;
}
pub struct UnavailableSecureCredentialStore;
impl SecureCredentialStore for UnavailableSecureCredentialStore {
    fn status(&self) -> CredentialStorageStatus {
        CredentialStorageStatus::Unavailable
    }
    fn load(&self, _: &str) -> Result<Option<Vec<u8>>> {
        Err(Error::new(
            "pairing_error",
            "native secure credential storage unavailable",
        ))
    }
    fn store(&self, _: &str, _: &[u8]) -> Result<()> {
        Err(Error::new(
            "pairing_error",
            "native secure credential storage unavailable",
        ))
    }
}
