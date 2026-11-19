//! Short-lived WebAuthn assurance for one existing opaque browser binding.
use super::{
    RedisSessions,
    browser::{BrowserSession, key},
};
use crate::accounts::StoreError;
use serde_json::Value;

impl RedisSessions {
    pub(crate) async fn step_up(&self, jti: &str) -> Result<Option<Value>, StoreError> {
        let raw: Option<String> = redis::cmd("GET")
            .arg(format!("{}:step-up", Self::token_key(jti)?))
            .query_async(&mut self.connection.clone())
            .await
            .map_err(|_| StoreError::Unavailable)?;
        raw.map(|v| serde_json::from_str(&v).map_err(|_| StoreError::Unavailable))
            .transpose()
    }

    pub(crate) async fn save_step_up(
        &self,
        id: &str,
        session: &BrowserSession,
        record: &Value,
        receipt: &Value,
        ttl: u64,
    ) -> Result<(), StoreError> {
        if ttl == 0 || ttl > 300 {
            return Err(StoreError::InvalidInput);
        }
        // Compare both live authorities inside the write. A logout, replacement
        // session or credential revision during the ceremony cannot revive it.
        let written: i64 = redis::cmd("EVAL")
            .arg("if redis.call('GET',KEYS[1])~=ARGV[1] or redis.call('GET',KEYS[2])~=ARGV[2] then return 0 end;redis.call('SET',KEYS[3],ARGV[3],'EX',ARGV[4]);return 1")
            .arg(3).arg(key(id)?).arg(Self::token_key(&session.jti)?)
            .arg(format!("{}:step-up", Self::token_key(&session.jti)?))
            .arg(serde_json::to_string(session).map_err(|_| StoreError::Unavailable)?)
            .arg(serde_json::to_string(record).map_err(|_| StoreError::Unavailable)?)
            .arg(serde_json::to_string(receipt).map_err(|_| StoreError::Unavailable)?)
            .arg(ttl).query_async(&mut self.connection.clone()).await.map_err(|_| StoreError::Unavailable)?;
        if written != 1 {
            return Err(StoreError::Changed);
        }
        Ok(())
    }
}
