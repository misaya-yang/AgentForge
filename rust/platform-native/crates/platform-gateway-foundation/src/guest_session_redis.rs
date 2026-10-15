//! Concrete Redis storage for `src/services/session/guest_session_manager.py`.
//!
//! `direct_redis` implements the source's native Redis SETEX fallback, while
//! `canonical_storage` implements the connected `RedisStorage` wrapper at
//! `ai_gateway_core/persistence/redis.py`. The profile is selected explicitly.
//! Construction performs no I/O. Connected profiles require an existing
//! configured connection. `disconnected_canonical_storage` explicitly models
//! the wrapper's absent client: reads are missing and writes/deletes are no-ops.
//! This adapter
//! does not select credentials, create a client, start Redis or add a namespace;
//! the caller owns connection lifecycle and the manager supplies the full key.
//! Each SETEX resets expiry atomically. GET never refreshes expiry. The wrapper
//! profile uses SET at TTL zero, removing an existing expiry; the native profile
//! sends zero to SETEX unchanged. Both send negative TTLs unchanged to Redis.
//! Wrapper reads decode UTF-8 and one JSON layer before the guest manager's
//! decoder, including the source's double-encoded JSON string behavior. Errors
//! propagate as the canonical sanitized error, with no memory fallback and no
//! automatic transition into the disconnected profile.

use crate::{
    guest_session_manager::{GuestSessionData, GuestSessionStorage},
    session_manager::{SessionError, SessionFuture},
};
use redis::aio::{ConnectionLike, ConnectionManager};

#[derive(Clone)]
pub struct RedisGuestSessionStorage {
    connection: Option<ConnectionManager>,
    profile: Profile,
}

#[derive(Clone, Copy)]
enum Profile {
    DirectRedis,
    CanonicalStorage,
}

impl RedisGuestSessionStorage {
    /// Compatibility alias for `direct_redis`; no profile is inferred at runtime.
    pub fn new(connection: ConnectionManager) -> Self {
        Self::direct_redis(connection)
    }

    pub fn direct_redis(connection: ConnectionManager) -> Self {
        Self {
            connection: Some(connection),
            profile: Profile::DirectRedis,
        }
    }

    /// Connected source `RedisStorage`: UTF-8/JSON reads and SET for TTL zero.
    /// The manager passes JSON text, so the wrapper must not JSON-encode again.
    pub fn canonical_storage(connection: ConnectionManager) -> Self {
        Self {
            connection: Some(connection),
            profile: Profile::CanonicalStorage,
        }
    }

    /// Source wrapper with no native client. Never selected after an I/O error.
    pub fn disconnected_canonical_storage() -> Self {
        Self {
            connection: None,
            profile: Profile::CanonicalStorage,
        }
    }
}

impl GuestSessionStorage for RedisGuestSessionStorage {
    fn get<'a>(&'a self, key: &'a str) -> SessionFuture<'a, Option<GuestSessionData>> {
        match self.connection.clone() {
            Some(connection) => Box::pin(get(connection, key, self.profile)),
            None => Box::pin(async { Ok(None) }),
        }
    }

    fn save<'a>(&'a self, key: &'a str, json: String, ttl: i64) -> SessionFuture<'a, ()> {
        match self.connection.clone() {
            Some(connection) => Box::pin(save(connection, key, json, ttl, self.profile)),
            None => Box::pin(async { Ok(()) }),
        }
    }

    fn delete<'a>(&'a self, key: &'a str) -> SessionFuture<'a, i64> {
        match self.connection.clone() {
            Some(connection) => Box::pin(delete(connection, key, self.profile)),
            None => Box::pin(async { Ok(0) }),
        }
    }
}

// Keep command execution generic internally so the exact production command
// path can also be checked with a no-I/O Redis protocol fixture.
async fn get(
    mut connection: impl ConnectionLike + Send,
    key: &str,
    profile: Profile,
) -> Result<Option<GuestSessionData>, SessionError> {
    let data: Option<Vec<u8>> = redis::cmd("GET")
        .arg(key)
        .query_async(&mut connection)
        .await
        .map_err(|_| SessionError::StorageUnavailable)?;
    let Some(data) = data.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    match profile {
        Profile::DirectRedis => Ok(Some(GuestSessionData::Json(data))),
        Profile::CanonicalStorage => {
            // The canonical native client has decode_responses=True. A decode
            // failure occurs during its get(), outside the manager's try/except.
            let text = String::from_utf8(data).map_err(|_| SessionError::StorageUnavailable)?;
            match serde_json::from_str(&text) {
                Ok(serde_json::Value::Object(value)) if !value.is_empty() => {
                    Ok(Some(GuestSessionData::Object(value)))
                }
                Ok(serde_json::Value::String(value)) if !value.is_empty() => {
                    Ok(Some(GuestSessionData::Json(value.into_bytes())))
                }
                // The wrapper returns parsed scalars/lists, but the guest
                // manager either sees a falsey value or rejects json.loads on
                // those non-string inputs. Normalize that result at this seam.
                Ok(_) => Ok(None),
                Err(_) => Ok(Some(GuestSessionData::Json(text.into_bytes()))),
            }
        }
    }
}

async fn save(
    mut connection: impl ConnectionLike + Send,
    key: &str,
    json: String,
    ttl: i64,
    profile: Profile,
) -> Result<(), SessionError> {
    let command = if matches!(profile, Profile::CanonicalStorage) && ttl == 0 {
        let mut command = redis::cmd("SET");
        command.arg(key).arg(json);
        command
    } else {
        let mut command = redis::cmd("SETEX");
        command.arg(key).arg(ttl).arg(json);
        command
    };
    command
        .query_async(&mut connection)
        .await
        .map_err(|_| SessionError::StorageUnavailable)
}

async fn delete(
    mut connection: impl ConnectionLike + Send,
    key: &str,
    profile: Profile,
) -> Result<i64, SessionError> {
    let count: i64 = redis::cmd("DEL")
        .arg(key)
        .query_async(&mut connection)
        .await
        .map_err(|_| SessionError::StorageUnavailable)?;
    Ok(match profile {
        Profile::DirectRedis => count,
        Profile::CanonicalStorage => i64::from(count > 0),
    })
}

