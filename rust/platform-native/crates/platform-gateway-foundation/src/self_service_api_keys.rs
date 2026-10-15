//! Self-service API-key service, distinct from legacy config key management.
//!
//! Authority: `src/services/auth/api_key_service.py` and api/v1/api_keys.py.
//! No route, task, entropy read, connection, or environment read on construction.
//! The host supplies cryptographic entropy; both independent source draws (32,
//! then 12 bytes) happen before schema discovery. No UUID/weak RNG fallback.
//! Plaintext is returned only from create; no secret-bearing type implements Debug.
//! Validation/authority live at the API boundary. This service intentionally allows
//! an empty scopes list and preserves duplicate scopes, matching the source.
use crate::{protocol::ProtocolTimestamp, service_config_api::ServiceConfigError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{future::Future, pin::Pin, sync::Arc};

pub use crate::service_config_api::ServiceConfigError as SelfServiceApiKeyError;
pub type KeyFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SelfServiceApiKeyError>> + Send + 'a>>;
pub const DEFAULT_SCOPES: &[&str] = &["knowledge:read", "knowledge:write"];
pub const CREATE_WARNING: &str = "请妥善保存此 API Key，它只会显示一次";

/// Required cryptographically secure entropy collaborator. Implementations must
/// fail closed on unavailable entropy. Called only during an authorized create.
pub trait SelfServiceApiKeyEntropy: Send + Sync {
    fn bytes(&self, length: usize) -> Result<Vec<u8>, SelfServiceApiKeyError>;
}
/// Deliberately no Debug/Serialize: holds the single-use response credential.
pub struct GeneratedKey {
    pub full_key: String,
    pub key_id: String,
    pub key_prefix: String,
    pub key_hash: String,
}
pub fn generate_api_key(
    entropy: &dyn SelfServiceApiKeyEntropy,
) -> Result<GeneratedKey, SelfServiceApiKeyError> {
    let random = entropy.bytes(32)?;
    if random.len() != 32 {
        return Err(SelfServiceApiKeyError::Internal);
    }
    let id = entropy.bytes(12)?;
    if id.len() != 12 {
        return Err(SelfServiceApiKeyError::Internal);
    }
    let hex = |bytes: &[u8]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let full_key = format!("agk_{}", hex(&random));
    Ok(GeneratedKey {
        key_prefix: full_key[..12].into(),
        key_id: format!("ak_{}", hex(&id)),
        key_hash: format!("{:x}", Sha256::digest(full_key.as_bytes())),
        full_key,
    })
}
#[derive(Clone)]
pub struct CreateKey {
    pub name: String,
    pub description: String,
    pub user_id: String,
    /// Source create preserves the authenticated raw tenant. Read filters map
    /// empty/null legacy rows to default. It must not be replaced with body data.
    pub tenant_id: String,
    pub scopes: Vec<String>,
    pub tier: String,
    pub expires_at: Option<ProtocolTimestamp>,
}
/// Persistence input has a hash, never the full credential. No Debug/Serialize.
#[derive(Clone)]
pub struct StoredCreateKey {
    pub key_id: String,
    pub key_hash: String,
    pub key_prefix: String,
    pub request: CreateKey,
}
#[derive(Clone, Default)]
pub struct KeyFilter {
    pub user_id: Option<String>,
    pub tenant_id: Option<String>,
    pub include_inactive: bool,
}
pub trait SelfServiceApiKeyStore: Send + Sync {
    fn create<'a>(&'a self, key: &'a StoredCreateKey) -> KeyFuture<'a, Value>;
    fn list<'a>(&'a self, filter: &'a KeyFilter) -> KeyFuture<'a, Vec<Value>>;
    fn get<'a>(
        &'a self,
        key_id: &'a str,
        tenant_id: Option<&'a str>,
    ) -> KeyFuture<'a, Option<Value>>;
    fn revoke<'a>(&'a self, key_id: &'a str, filter: &'a KeyFilter) -> KeyFuture<'a, bool>;
    fn delete<'a>(&'a self, key_id: &'a str, filter: &'a KeyFilter) -> KeyFuture<'a, bool>;
}
/// Matches the endpoint's service calls; suitable for a host-owned service or the
/// native supervised implementation below. Database presence is separately checked.
pub trait SelfServiceApiKeyService: Send + Sync {
    fn create<'a>(&'a self, key: &'a CreateKey) -> KeyFuture<'a, Value>;
    fn list<'a>(&'a self, filter: &'a KeyFilter) -> KeyFuture<'a, Vec<Value>>;
    fn get<'a>(
        &'a self,
        key_id: &'a str,
        tenant_id: Option<&'a str>,
    ) -> KeyFuture<'a, Option<Value>>;
    fn revoke<'a>(&'a self, key_id: &'a str, filter: &'a KeyFilter) -> KeyFuture<'a, bool>;
    fn delete<'a>(&'a self, key_id: &'a str, filter: &'a KeyFilter) -> KeyFuture<'a, bool>;
}
pub struct SelfServiceApiKeys {
    store: Arc<dyn SelfServiceApiKeyStore>,
    entropy: Arc<dyn SelfServiceApiKeyEntropy>,
}
impl SelfServiceApiKeys {
    pub fn new(
        store: Arc<dyn SelfServiceApiKeyStore>,
        entropy: Arc<dyn SelfServiceApiKeyEntropy>,
    ) -> Self {
        Self { store, entropy }
    }
}
impl SelfServiceApiKeyService for SelfServiceApiKeys {
    fn create<'a>(&'a self, key: &'a CreateKey) -> KeyFuture<'a, Value> {
        Box::pin(async move {
            let generated = generate_api_key(self.entropy.as_ref())?;
            let input = StoredCreateKey {
                key_id: generated.key_id,
                key_hash: generated.key_hash,
                key_prefix: generated.key_prefix,
                request: key.clone(),
            };
            let mut result = self.store.create(&input).await?;
            let object = result.as_object_mut().ok_or(ServiceConfigError::Internal)?;
            let enabled = object.remove("enabled").unwrap_or(json!(true));
            object.insert("is_active".into(), enabled);
            object.insert("api_key".into(), json!(generated.full_key));
            object.insert("warning".into(), json!(CREATE_WARNING));
            Ok(result)
        })
    }
    fn list<'a>(&'a self, filter: &'a KeyFilter) -> KeyFuture<'a, Vec<Value>> {
        self.store.list(filter)
    }
    fn get<'a>(
        &'a self,
        key_id: &'a str,
        tenant_id: Option<&'a str>,
    ) -> KeyFuture<'a, Option<Value>> {
        self.store.get(key_id, tenant_id)
    }
    fn revoke<'a>(&'a self, key_id: &'a str, filter: &'a KeyFilter) -> KeyFuture<'a, bool> {
        self.store.revoke(key_id, filter)
    }
    fn delete<'a>(&'a self, key_id: &'a str, filter: &'a KeyFilter) -> KeyFuture<'a, bool> {
        self.store.delete(key_id, filter)
    }
}
