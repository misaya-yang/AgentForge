//! One-use, binding-aware server-only challenges and atomic rate limits.
// SPDX-License-Identifier: Apache-2.0
use super::*;
use serde::{Serialize, de::DeserializeOwned};
#[derive(Clone)]
pub(super) struct Transient {
    redis: redis::aio::ConnectionManager,
}
impl Transient {
    pub fn new(redis: redis::aio::ConnectionManager) -> Self {
        Self { redis }
    }
    fn key(kind: &str, id: &str) -> Result<String> {
        bounded(kind, 32)?;
        bounded(id, 2048)?;
        Ok(format!(
            "{{auth}}:enterprise:{kind}:{}",
            digest(id.as_bytes())
        ))
    }
    pub async fn put<T: Serialize>(
        &self,
        kind: &str,
        id: &str,
        binding: &str,
        value: &T,
    ) -> Result<()> {
        let data =
            serde_json::to_string(&json!({"binding":digest(binding.as_bytes()),"value":value}))
                .map_err(|_| EnterpriseError::Invalid)?;
        if data.len() > 256 * 1024 {
            return Err(EnterpriseError::Invalid);
        }
        let ok: Option<String> = redis::cmd("SET")
            .arg(Self::key(kind, id)?)
            .arg(data)
            .arg("EX")
            .arg(300)
            .arg("NX")
            .query_async(&mut self.redis.clone())
            .await
            .map_err(|_| EnterpriseError::Unavailable)?;
        if ok.is_none() {
            return Err(EnterpriseError::Conflict);
        }
        Ok(())
    }
    pub async fn take<T: DeserializeOwned>(
        &self,
        kind: &str,
        id: &str,
        binding: &str,
    ) -> Result<T> {
        // Binding is checked inside the same atomic operation that consumes the
        // challenge. An unrelated browser cannot burn somebody else's flow.
        let script = "local raw=redis.call('GET',KEYS[1]);if not raw then return nil end;local v=cjson.decode(raw);if v.binding~=ARGV[1] then return nil end;redis.call('DEL',KEYS[1]);return raw";
        let raw: Option<String> = redis::cmd("EVAL")
            .arg(script)
            .arg(1)
            .arg(Self::key(kind, id)?)
            .arg(digest(binding.as_bytes()))
            .query_async(&mut self.redis.clone())
            .await
            .map_err(|_| EnterpriseError::Unavailable)?;
        let v: Value = serde_json::from_str(&raw.ok_or(EnterpriseError::Proof)?)
            .map_err(|_| EnterpriseError::Unavailable)?;
        serde_json::from_value(v["value"].clone()).map_err(|_| EnterpriseError::Unavailable)
    }
    pub async fn rate(&self, kind: &str, identity: &str, max: u64, seconds: u64) -> Result<()> {
        let count:u64=redis::cmd("EVAL").arg("local n=redis.call('INCR',KEYS[1]);if n==1 then redis.call('EXPIRE',KEYS[1],ARGV[1]) end;return n").arg(1).arg(Self::key(kind,identity)?).arg(seconds).query_async(&mut self.redis.clone()).await.map_err(|_|EnterpriseError::Unavailable)?;
        if count > max {
            return Err(EnterpriseError::Rate);
        }
        Ok(())
    }
}
