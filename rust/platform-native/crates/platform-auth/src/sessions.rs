//! Interoperable Redis allowlist. Unavailable authority is an error, never authenticated.
use crate::accounts::StoreError;
use redis::aio::ConnectionManager;
#[derive(Clone)]
pub struct RedisSessions {
    connection: ConnectionManager,
}
impl RedisSessions {
    pub fn new(connection: ConnectionManager) -> Self {
        Self { connection }
    }
    fn token_key(jti: &str) -> Result<String, StoreError> {
        uuid::Uuid::parse_str(jti).map_err(|_| StoreError::InvalidInput)?;
        Ok(format!("auth:token:{jti}"))
    }
    pub async fn save(
        &self,
        jti: &str,
        user: &str,
        data: &serde_json::Value,
        ttl: u64,
    ) -> Result<(), StoreError> {
        if user.is_empty() || ttl == 0 || ttl > 10_800 || data["user_id"] != user {
            return Err(StoreError::InvalidInput);
        }
        let key = Self::token_key(jti)?;
        let users = format!("auth:user_tokens:{user}");
        let payload = serde_json::to_string(data).map_err(|_| StoreError::Unavailable)?;
        redis::pipe()
            .atomic()
            .cmd("SETEX")
            .arg(key)
            .arg(ttl)
            .arg(payload)
            .ignore()
            .cmd("SADD")
            .arg(&users)
            .arg(jti)
            .ignore()
            .cmd("EXPIRE")
            .arg(users)
            .arg(10_800)
            .ignore()
            .query_async::<()>(&mut self.connection.clone())
            .await
            .map_err(|_| StoreError::Unavailable)
    }
    pub async fn contains(&self, jti: &str) -> Result<bool, StoreError> {
        redis::cmd("EXISTS")
            .arg(Self::token_key(jti)?)
            .query_async(&mut self.connection.clone())
            .await
            .map_err(|_| StoreError::Unavailable)
    }
    /// Read the existing allowlist record without logging credential metadata.
    pub async fn record(&self, jti: &str) -> Result<Option<serde_json::Value>, StoreError> {
        let payload: Option<String> = redis::cmd("GET")
            .arg(Self::token_key(jti)?)
            .query_async(&mut self.connection.clone())
            .await
            .map_err(|_| StoreError::Unavailable)?;
        payload.map(|value| serde_json::from_str(&value).map_err(|_| StoreError::Unavailable)).transpose()
    }
    pub async fn revoke(&self, jti: &str, user: &str) -> Result<(), StoreError> {
        if user.is_empty() {
            return Err(StoreError::InvalidInput);
        }
        redis::pipe()
            .atomic()
            .cmd("DEL")
            .arg(Self::token_key(jti)?)
            .ignore()
            .cmd("SREM")
            .arg(format!("auth:user_tokens:{user}"))
            .arg(jti)
            .ignore()
            .query_async::<()>(&mut self.connection.clone())
            .await
            .map_err(|_| StoreError::Unavailable)
    }
}

// MULTI/EXEC is not rollback-on-runtime-error. Any save error or timeout may have
// partially written Redis. The service must never disclose a minted token after
// save failure; a failed call does not prove absence of a token key.
