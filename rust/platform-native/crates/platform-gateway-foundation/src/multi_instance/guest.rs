//! Opaque CAS snapshots prevent lost updates, deleted-session resurrection and
//! repeat conversion across replicas. No disconnected or memory fallback exists.
use crate::{
    guest_session_manager::{GuestSessionData, GuestSessionStorage},
    session_manager::{SessionError, SessionFuture},
};
use redis::aio::ConnectionManager;
#[derive(Clone)]
pub struct AtomicGuestStorage {
    redis: ConnectionManager,
}
impl AtomicGuestStorage {
    pub fn new(redis: ConnectionManager) -> Self {
        Self { redis }
    }
}
impl GuestSessionStorage for AtomicGuestStorage {
    fn get<'a>(&'a self, key: &'a str) -> SessionFuture<'a, Option<GuestSessionData>> {
        Box::pin(async move {
            Ok(self
                .raw_get(key)
                .await?
                .map(|s| GuestSessionData::Json(s.into_bytes())))
        })
    }
    fn raw_get<'a>(&'a self, key: &'a str) -> SessionFuture<'a, Option<String>> {
        Box::pin(async move {
            super::eval(
                &self.redis,
                "return redis.call('GET',KEYS[1])",
                &[key.into()],
                &[],
            )
            .await
            .map_err(|_| SessionError::StorageUnavailable)
        })
    }
    fn save<'a>(&'a self, key: &'a str, json: String, ttl: i64) -> SessionFuture<'a, ()> {
        Box::pin(async move {
            if ttl <= 0 {
                return Err(SessionError::InvalidConfig("shared_guest_ttl"));
            }
            let result:i64=super::eval(&self.redis,"if redis.call('EXISTS',KEYS[1])==1 then return 0 end;redis.call('SET',KEYS[1],ARGV[1],'EX',ARGV[2]);return 1",&[key.into()],&[json,ttl.to_string()]).await.map_err(|_|SessionError::StorageUnavailable)?;
            if result == 1 {
                Ok(())
            } else {
                Err(SessionError::AlreadyExists)
            }
        })
    }
    fn compare_exchange<'a>(
        &'a self,
        key: &'a str,
        expected: String,
        replacement: Option<String>,
        ttl: i64,
    ) -> SessionFuture<'a, bool> {
        Box::pin(async move {
            if ttl <= 0 {
                return Err(SessionError::InvalidConfig("shared_guest_ttl"));
            }
            let deleting = replacement.is_none();
            let result:i64=super::eval(&self.redis,"if redis.call('GET',KEYS[1])~=ARGV[1] then return 0 end;if ARGV[4]=='1' then redis.call('DEL',KEYS[1]) else redis.call('SET',KEYS[1],ARGV[2],'EX',ARGV[3]) end;return 1",&[key.into()],&[expected,replacement.unwrap_or_default(),ttl.to_string(),if deleting{"1"}else{"0"}.into()]).await.map_err(|_|SessionError::StorageUnavailable)?;
            Ok(result == 1)
        })
    }
    fn atomic_capable(&self) -> bool {
        true
    }
    fn delete<'a>(&'a self, key: &'a str) -> SessionFuture<'a, i64> {
        Box::pin(async move {
            super::eval(
                &self.redis,
                "return redis.call('DEL',KEYS[1])",
                &[key.into()],
                &[],
            )
            .await
            .map_err(|_| SessionError::StorageUnavailable)
        })
    }
}
