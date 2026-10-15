//! Connected Redis adapter for canonical connector OAuth state.
//!
//! The service supplies the complete `oauth:state:<state>` key and 600-second
//! TTL. This adapter adds no namespace or memory fallback. Consumption uses one
//! GETDEL (Redis >= 6.2; deployment defaults use Redis 7), before decoding.
//! A connection is supplied by the host; construction does not connect or read
//! configuration. Redis failures always propagate as sanitized 503 errors.
//!
//! Direct Redis returns bytes for the service decoder. Canonical RedisStorage
//! first decodes UTF-8 and one JSON layer; JSON strings become bytes for the
//! service's second decode. Its disconnected no-op profile is explicit and is
//! NEVER selected as recovery from a connected operation failure. JSON writes
//! are semantically equivalent, with compact serde spacing rather than Python
//! json.dumps spacing/ASCII escapes. The state record has string-only values.

use crate::connector_user_types::{
    ConnectorError, ConnectorFuture, OAuthStateRaw, OAuthStateStore,
};
use redis::aio::{ConnectionLike, ConnectionManager};
use serde_json::Value;
use std::fmt;

#[derive(Clone, Copy)]
enum Profile {
    DirectRedis,
    CanonicalStorage,
}

#[derive(Clone)]
pub struct RedisOAuthStateStore {
    connection: Option<ConnectionManager>,
    profile: Profile,
}

impl RedisOAuthStateStore {
    pub fn new(connection: ConnectionManager) -> Self {
        Self::direct_redis(connection)
    }

    pub fn direct_redis(connection: ConnectionManager) -> Self {
        Self {
            connection: Some(connection),
            profile: Profile::DirectRedis,
        }
    }

    pub fn canonical_storage(connection: ConnectionManager) -> Self {
        Self {
            connection: Some(connection),
            profile: Profile::CanonicalStorage,
        }
    }

    /// Source wrapper's absent native client, not an absent service dependency.
    pub fn disconnected_canonical_storage() -> Self {
        Self {
            connection: None,
            profile: Profile::CanonicalStorage,
        }
    }
}

impl fmt::Debug for RedisOAuthStateStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RedisOAuthStateStore(<redacted>)")
    }
}

fn unavailable() -> ConnectorError {
    ConnectorError::detail(503, "OAuth state store unavailable")
}

impl OAuthStateStore for RedisOAuthStateStore {
    fn save<'a>(&'a self, key: &'a str, value: Value, ttl: u64) -> ConnectorFuture<'a, ()> {
        match self.connection.clone() {
            Some(connection) => Box::pin(save(connection, key, value, ttl, self.profile)),
            None => Box::pin(async { Ok(()) }),
        }
    }

    fn consume<'a>(&'a self, key: &'a str) -> ConnectorFuture<'a, Option<OAuthStateRaw>> {
        match self.connection.clone() {
            Some(connection) => Box::pin(consume(connection, key, self.profile)),
            None => Box::pin(async { Ok(None) }),
        }
    }
}

// Generic execution is the production path and allows offline protocol fixtures
// to prove actual commands without a Redis service or fake OAuthStateStore.
async fn save(
    mut connection: impl ConnectionLike + Send,
    key: &str,
    value: Value,
    ttl: u64,
    profile: Profile,
) -> Result<(), ConnectorError> {
    let value = serde_json::to_string(&value).map_err(|_| unavailable())?;
    let command = if matches!(profile, Profile::CanonicalStorage) && ttl == 0 {
        let mut command = redis::cmd("SET");
        command.arg(key).arg(value);
        command
    } else {
        let mut command = redis::cmd("SETEX");
        command.arg(key).arg(ttl).arg(value);
        command
    };
    command
        .query_async(&mut connection)
        .await
        .map_err(|_| unavailable())
}

async fn consume(
    mut connection: impl ConnectionLike + Send,
    key: &str,
    profile: Profile,
) -> Result<Option<OAuthStateRaw>, ConnectorError> {
    let bytes: Option<Vec<u8>> = redis::cmd("GETDEL")
        .arg(key)
        .query_async(&mut connection)
        .await
        .map_err(|_| unavailable())?;
    let Some(bytes) = bytes else { return Ok(None) };
    match profile {
        Profile::DirectRedis => Ok(Some(OAuthStateRaw::Bytes(bytes))),
        Profile::CanonicalStorage => {
            if bytes.is_empty() {
                return Ok(None);
            }
            let text = String::from_utf8(bytes).map_err(|_| unavailable())?;
            Ok(Some(match serde_json::from_str(&text) {
                Ok(Value::String(text)) => OAuthStateRaw::Bytes(text.into_bytes()),
                Ok(value) => OAuthStateRaw::Json(value),
                Err(_) => OAuthStateRaw::Bytes(text.into_bytes()),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Wire {
        commands: Arc<Mutex<Vec<Vec<u8>>>>,
        response: redis::RedisResult<redis::Value>,
    }
    impl ConnectionLike for Wire {
        fn req_packed_command<'a>(
            &'a mut self,
            command: &'a redis::Cmd,
        ) -> redis::RedisFuture<'a, redis::Value> {
            self.commands
                .lock()
                .unwrap()
                .push(command.get_packed_command());
            let response = std::mem::replace(&mut self.response, Ok(redis::Value::Nil));
            Box::pin(async move { response })
        }
        fn req_packed_commands<'a>(
            &'a mut self,
            _: &'a redis::Pipeline,
            _: usize,
            _: usize,
        ) -> redis::RedisFuture<'a, Vec<redis::Value>> {
            panic!("consumption must not pipeline GET and DEL")
        }
        fn get_db(&self) -> i64 {
            0
        }
    }
    #[tokio::test]
    async fn oauth_atomic_adapter_uses_exactly_one_getdel_for_both_profiles() {
        for profile in [Profile::DirectRedis, Profile::CanonicalStorage] {
            for response in [
                Ok(redis::Value::BulkString(br#"{"nonce":"fixture"}"#.to_vec())),
                Ok(redis::Value::Nil),
                Err(redis::RedisError::from((
                    redis::ErrorKind::ResponseError,
                    "unknown command GETDEL",
                ))),
                Err(redis::RedisError::from((
                    redis::ErrorKind::IoError,
                    "connection lost",
                ))),
            ] {
                let failing = response.is_err();
                let missing = matches!(&response, Ok(redis::Value::Nil));
                let commands = Arc::new(Mutex::new(vec![]));
                let result = consume(
                    Wire {
                        commands: commands.clone(),
                        response,
                    },
                    "oauth:state:fixture",
                    profile,
                )
                .await;
                let expected = redis::cmd("GETDEL")
                    .arg("oauth:state:fixture")
                    .get_packed_command();
                assert_eq!(*commands.lock().unwrap(), vec![expected]);
                if failing {
                    assert_eq!(result.err().unwrap().status, 503);
                } else if missing {
                    assert!(result.unwrap().is_none());
                } else {
                    match result.unwrap().unwrap() {
                        OAuthStateRaw::Bytes(bytes) => assert_eq!(bytes, br#"{"nonce":"fixture"}"#),
                        OAuthStateRaw::Json(value) => {
                            assert_eq!(value, serde_json::json!({"nonce":"fixture"}))
                        }
                    }
                }
            }
        }
    }
}
