//! Opaque browser aliases over the existing Redis-backed token authority.
// SPDX-License-Identifier: Apache-2.0
use super::RedisSessions;
use crate::accounts::StoreError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[derive(Serialize, Deserialize)]
pub struct BrowserSession {
    pub jti: String,
    pub tenant_id: String,
    pub issued_at: u64,
    pub idle_expires_at: u64,
    pub csrf: String,
    pub credential_revision: serde_json::Value,
    pub user_id: String,
    pub expires_at: u64,
}
pub fn opaque() -> String {
    format!(
        "{:x}",
        Sha256::digest(
            [
                uuid::Uuid::new_v4().as_bytes().as_slice(),
                uuid::Uuid::new_v4().as_bytes().as_slice()
            ]
            .concat()
        )
    )
}
fn key(id: &str) -> Result<String, StoreError> {
    if id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(StoreError::InvalidInput);
    }
    Ok(format!("auth:bff:{:x}", Sha256::digest(id.as_bytes())))
}
impl RedisSessions {
    /// Extend an already-live login allowlist, never recreate a revoked JTI.
    pub async fn bind_browser_lifetime(
        &self,
        jti: &str,
        expected: &serde_json::Value,
        ttl: u64,
    ) -> Result<(), StoreError> {
        if ttl == 0 || ttl > 10800 {
            return Err(StoreError::InvalidInput);
        }
        let payload = serde_json::to_string(expected).map_err(|_| StoreError::Unavailable)?;
        let result:i64=redis::cmd("EVAL").arg("if redis.call('GET',KEYS[1]) ~= ARGV[1] then return 0 end; return redis.call('EXPIRE',KEYS[1],ARGV[2])").arg(1).arg(Self::token_key(jti)?).arg(payload).arg(ttl)
            .query_async(&mut self.connection.clone()).await.map_err(|_|StoreError::Unavailable)?;
        if result != 1 {
            return Err(StoreError::Changed);
        }
        Ok(())
    }
    pub async fn save_browser(
        &self,
        id: &str,
        session: &BrowserSession,
        ttl: u64,
    ) -> Result<(), StoreError> {
        if ttl == 0
            || ttl > 10800
            || session.user_id.is_empty()
            || uuid::Uuid::parse_str(&session.jti).is_err()
            || session.tenant_id.is_empty()
            || session.csrf.len() != 64
        {
            return Err(StoreError::InvalidInput);
        }
        let payload = serde_json::to_string(session).map_err(|_| StoreError::Unavailable)?;
        let set: Option<String> = redis::cmd("SET")
            .arg(key(id)?)
            .arg(payload)
            .arg("NX")
            .arg("EX")
            .arg(ttl)
            .query_async(&mut self.connection.clone())
            .await
            .map_err(|_| StoreError::Unavailable)?;
        if set.is_none() {
            return Err(StoreError::Unavailable);
        }
        Ok(())
    }
    pub async fn browser(&self, id: &str) -> Result<Option<BrowserSession>, StoreError> {
        let payload: Option<String> = redis::cmd("GET")
            .arg(key(id)?)
            .query_async(&mut self.connection.clone())
            .await
            .map_err(|_| StoreError::Unavailable)?;
        payload
            .map(|v| serde_json::from_str(&v).map_err(|_| StoreError::Unavailable))
            .transpose()
    }
    pub async fn revoke_browser(&self, id: &str) -> Result<(), StoreError> {
        redis::cmd("DEL")
            .arg(key(id)?)
            .query_async::<()>(&mut self.connection.clone())
            .await
            .map_err(|_| StoreError::Unavailable)
    }
}
