//! Shared bounded recent observations. Event IDs are immutable and deduplicated;
//! counters remain diagnostics and never reconstruct accounting authority.
use super::*;
use serde::{Serialize, de::DeserializeOwned};
#[derive(Clone)]
pub struct ContextStore {
    redis: ConnectionManager,
}
impl ContextStore {
    pub fn new(redis: ConnectionManager) -> Self {
        Self { redis }
    }
    pub async fn record<T: Serialize>(
        &self,
        tenant: &str,
        session: &str,
        event: &str,
        metric: &T,
    ) -> Result<bool, Error> {
        bounded(tenant)?;
        bounded(session)?;
        bounded(event)?;
        let id =
            hash(&serde_json::to_string(&(tenant, session, event)).map_err(|_| Error::Invalid)?);
        let payload = serde_json::to_string(metric).map_err(|_| Error::Invalid)?;
        if payload.len() > 32 * 1024 {
            return Err(Error::Invalid);
        }
        let result: i64 = eval(&self.redis, RECORD, &keys(), &[id, payload]).await?;
        match result {
            1 => Ok(true),
            0 => Ok(false),
            _ => Err(Error::Conflict),
        }
    }
    pub async fn read<T: DeserializeOwned>(&self) -> Result<Vec<T>, Error> {
        let rows:Vec<String>=eval(&self.redis,"local ids=redis.call('ZRANGE',KEYS[1],0,-1);local out={};for _,id in ipairs(ids) do local value=redis.call('HGET',KEYS[2],id);if value then table.insert(out,value) end end;return out",&keys(),&[]).await?;
        rows.into_iter()
            .map(|s| serde_json::from_str(&s).map_err(|_| Error::Unavailable))
            .collect()
    }
}
fn keys() -> [String; 2] {
    [
        "gateway:ha:{context}:index".into(),
        "gateway:ha:{context}:data".into(),
    ]
}
const RECORD: &str = r#"local old=redis.call('HGET',KEYS[2],ARGV[1]);if old then if old~=ARGV[2] then return -1 end;return 0 end
local time=redis.call('TIME');local now=tonumber(time[1])*1000000+tonumber(time[2]);redis.call('HSET',KEYS[2],ARGV[1],ARGV[2]);redis.call('ZADD',KEYS[1],now,ARGV[1]);local n=redis.call('ZCARD',KEYS[1]);if n>100 then local trim=redis.call('ZRANGE',KEYS[1],0,n-101);for _,id in ipairs(trim) do redis.call('HDEL',KEYS[2],id);redis.call('ZREM',KEYS[1],id) end end;redis.call('EXPIRE',KEYS[1],604800);redis.call('EXPIRE',KEYS[2],604800);return 1"#;
