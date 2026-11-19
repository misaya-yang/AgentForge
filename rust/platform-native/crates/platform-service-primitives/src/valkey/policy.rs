use super::{Instance, ServiceIdentity, ValkeyError};
use sha2::{Digest, Sha256};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoordinationDecision {
    Confirmed,
    Denied,
    Unavailable(ValkeyError),
}
/// OOM/auth/timeout/transport errors never become permission to admit a request.
pub fn coordination_decision(result: Result<bool, ValkeyError>) -> CoordinationDecision {
    match result {
        Ok(true) => CoordinationDecision::Confirmed,
        Ok(false) => CoordinationDecision::Denied,
        Err(e) => CoordinationDecision::Unavailable(e),
    }
}
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .filter_map(|l| l.trim().split_once(':'))
        .find_map(|(k, v)| (k == key).then_some(v))
}
pub fn verify_server(
    instance: Instance,
    expected_user: &str,
    server: &str,
    memory: &str,
    persistence: &str,
    client: &str,
) -> Result<(), ValkeyError> {
    if field(memory, "maxmemory")
        .and_then(|v| v.parse::<u64>().ok())
        .is_none_or(|m| m == 0)
    {
        return Err(ValkeyError::ServerPolicy);
    }
    let policy = match instance {
        Instance::Coord => "noeviction",
        Instance::Cache => "allkeys-lru",
    };
    if !field(server, "valkey_version").is_some_and(|v| v.starts_with("9."))
        || field(memory, "maxmemory_policy") != Some(policy)
        || !client
            .split_whitespace()
            .any(|f| f.strip_prefix("user=") == Some(expected_user))
    {
        return Err(ValkeyError::ServerPolicy);
    }
    match instance {
        Instance::Coord
            if field(persistence, "aof_enabled") != Some("1")
                || field(persistence, "aof_last_write_status") != Some("ok") =>
        {
            Err(ValkeyError::ServerPolicy)
        }
        Instance::Cache if field(persistence, "aof_enabled") != Some("0") => {
            Err(ValkeyError::ServerPolicy)
        }
        _ => Ok(()),
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheKind {
    Principal,
    Policy,
    Retrieval,
}
#[derive(Clone, Debug)]
pub struct CacheKey {
    pub(crate) value: String,
    pub(crate) service: ServiceIdentity,
}
impl CacheKey {
    /// Caller must derive tenant from its authenticated actor, never untrusted JSON.
    /// Hashing prevents separator/hash-tag injection and avoids putting PII in keys.
    pub fn new(
        service: ServiceIdentity,
        kind: CacheKind,
        tenant: &str,
        identity: &[u8],
    ) -> Result<Self, ValkeyError> {
        let allowed = match service {
            ServiceIdentity::Gateway => matches!(kind, CacheKind::Principal | CacheKind::Policy),
            _ => kind == CacheKind::Retrieval,
        };
        if !allowed
            || tenant.trim().is_empty()
            || tenant.len() > 512
            || identity.is_empty()
            || identity.len() > 65536
        {
            return Err(ValkeyError::CacheScope);
        }
        let tenant = format!("{:x}", Sha256::digest(tenant.as_bytes()));
        let identity = format!("{:x}", Sha256::digest(identity));
        Ok(Self {
            value: format!(
                "af-cache:{}:{kind:?}:{{{tenant}}}:{identity}",
                service.name()
            ),
            service,
        })
    }
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

/// Emit an operational warning at 70%; never infer successful admission from it.
pub fn memory_pressure_warning(memory: &str) -> Result<bool, ValkeyError> {
    let used = field(memory, "used_memory")
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or(ValkeyError::ServerPolicy)?;
    let max = field(memory, "maxmemory")
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|m| *m > 0)
        .ok_or(ValkeyError::ServerPolicy)?;
    Ok((used as u128) * 100 >= (max as u128) * 70)
}

pub(crate) fn bounded_cache_value(value: Vec<u8>) -> Result<Option<Vec<u8>>, ValkeyError> {
    if value.len() > 1_048_576 {
        return Err(ValkeyError::CacheScope);
    }
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}
