//! Guest lifecycle port of `src/services/session/guest_session_manager.py`.
//!
//! This is an internal manager, not authentication or an HTTP route. The Python
//! source has no tenant field or ownership check: callers must establish guest
//! possession and the authorized target user before conversion. Redis adapters
//! own key isolation and expiry. Memory reads deliberately do NOT expire records;
//! explicit cleanup uses `age > ttl`, including the microsecond boundary.
//!
//! Conversion retains the legacy blank-tenant admin-context command, sequential
//! best-effort thread updates, and unconditional guest deletion after the loop.
//! The canonical proxy may bypass owner/tenant checks for this admin tier. The
//! host must authorize guest possession AND the target user before calling this
//! trusted-only operation. It never creates a native authenticated principal.
//!
//! Reuses canonical session timestamps, clock, ID and error seams. Returned
//! records/lists are snapshots, not Python's mutable aliases. Internally, memory
//! conversion iterates the original live thread list across proxy awaits, while
//! Redis conversion uses its decoded copy. Redis decoding is typed: invalid
//! records fail closed, and JSON bytes must be UTF-8. Guest timestamp decoding
//! accepts calendar dates and full ISO date-times with T/space separators and
//! RFC3339 offsets. It rejects leap seconds and truncates sub-microsecond digits
//! like Python; other `fromisoformat` extensions remain outside this slice.
//! JSON text is semantically identical; Python JSON whitespace/escaping is not a
//! wire contract. No Redis connection, proxy client, background task or route is
//! created here. `memory_enabled` remains an unused source configuration field.

use crate::session_manager::{
    SessionClock, SessionError, SessionFuture, SessionIds, SessionTimestamp,
};
use chrono::{Datelike, NaiveDate, Timelike, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestSessionConfig {
    pub session_ttl: i64,
    pub max_threads_per_session: i64,
    pub memory_enabled: bool,
    pub key_prefix: String,
}
impl Default for GuestSessionConfig {
    fn default() -> Self {
        Self {
            session_ttl: 7 * 24 * 3600,
            max_threads_per_session: 5,
            memory_enabled: false,
            key_prefix: "guest_session".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GuestSession {
    pub session_id: String,
    #[serde(deserialize_with = "deserialize_guest_timestamp")]
    pub created_at: SessionTimestamp,
    #[serde(deserialize_with = "deserialize_guest_timestamp")]
    pub last_active_at: SessionTimestamp,
    #[serde(default)]
    pub thread_ids: Vec<String>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

/// Normalize the source's ordinary calendar-date/ISO inputs locally. Other
/// session models keep their own timestamp contracts. Chrono represents leap
/// seconds, unlike Python datetime, so reject those before microsecond truncation.
fn deserialize_guest_timestamp<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<SessionTimestamp, D::Error> {
    let text = String::deserialize(deserializer)?;
    parse_guest_timestamp(&text).map_err(serde::de::Error::custom)
}

fn parse_guest_timestamp(text: &str) -> Result<SessionTimestamp, SessionError> {
    let invalid = || SessionError::InvalidData("invalid_guest_session_timestamp");
    if text.ends_with('z') {
        return Err(invalid());
    }
    if text.len() == 10 {
        return NaiveDate::parse_from_str(text, "%Y-%m-%d")
            .ok()
            .filter(|date| (1..=9999).contains(&date.year()))
            .and_then(|date| date.and_hms_opt(0, 0, 0))
            .map(SessionTimestamp::naive)
            .ok_or_else(invalid);
    }
    let mut normalized = text.to_owned();
    if normalized.as_bytes().get(10) == Some(&b' ') {
        normalized.replace_range(10..11, "T");
    }
    if let Some(dot) = normalized.find('.') {
        let digits = normalized.as_bytes()[dot + 1..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digits > 6 {
            normalized.replace_range(dot + 7..dot + 1 + digits, "");
        }
    }
    let timestamp = SessionTimestamp::parse(&normalized).map_err(|_| invalid())?;
    match timestamp {
        SessionTimestamp::Naive(time)
            if time.nanosecond() < 1_000_000_000 && (1..=9999).contains(&time.year()) =>
        {
            Ok(SessionTimestamp::naive(time))
        }
        SessionTimestamp::Aware(time)
            if time.nanosecond() < 1_000_000_000 && (1..=9999).contains(&time.year()) =>
        {
            Ok(SessionTimestamp::Aware(
                time.with_nanosecond(time.nanosecond() / 1000 * 1000)
                    .ok_or_else(invalid)?,
            ))
        }
        _ => Err(invalid()),
    }
}
impl GuestSession {
    pub fn to_dict(&self) -> Value {
        json!({
            "session_id": self.session_id,
            "created_at": self.created_at,
            "last_active_at": self.last_active_at,
            "thread_ids": self.thread_ids,
            "metadata": self.metadata,
        })
    }

    pub fn from_dict(value: Value) -> Result<Self, SessionError> {
        serde_json::from_value(value)
            .map_err(|_| SessionError::InvalidData("invalid_guest_session"))
    }
}

/// The two accepted Redis result shapes: an already-decoded dictionary, or JSON
/// text/bytes. Missing and falsey backend results should be returned as `None`.
#[derive(Clone, Debug)]
pub enum GuestSessionData {
    Object(Map<String, Value>),
    Json(Vec<u8>),
}
impl GuestSessionData {
    fn decode(self) -> Option<GuestSession> {
        let value = match self {
            Self::Object(value) => Value::Object(value),
            Self::Json(value) => serde_json::from_slice(&value).ok()?,
        };
        GuestSession::from_dict(value).ok()
    }
}

/// Injected Redis-compatible backend. `save` corresponds to Python's preferred
/// `save(key, json, ttl)`, or `setex(key, ttl, json)` when that method is absent.
/// Every write resets the backend TTL. Reads must not extend it. Backend errors
/// propagate; they must not cause an implicit in-memory fallback. Adapters redact
/// all backend diagnostics into the canonical `SessionError`.
pub trait GuestSessionStorage: Send + Sync {
    fn get<'a>(&'a self, key: &'a str) -> SessionFuture<'a, Option<GuestSessionData>>;
    fn save<'a>(&'a self, key: &'a str, json: String, ttl: i64) -> SessionFuture<'a, ()>;
    fn delete<'a>(&'a self, key: &'a str) -> SessionFuture<'a, i64>;
    /// Shared mode requires exact opaque snapshots and atomic non-resurrecting CAS.
    fn atomic_capable(&self) -> bool {
        false
    }
    fn raw_get<'a>(&'a self, _key: &'a str) -> SessionFuture<'a, Option<String>> {
        Box::pin(async { Err(SessionError::InvalidConfig("guest_atomic_storage_required")) })
    }
    fn compare_exchange<'a>(
        &'a self,
        _key: &'a str,
        _expected: String,
        _replacement: Option<String>,
        _ttl: i64,
    ) -> SessionFuture<'a, bool> {
        Box::pin(async { Err(SessionError::InvalidConfig("guest_atomic_storage_required")) })
    }
}

/// Exact legacy `UserContext` data for the migration command. It is deliberately
/// not a `RequestPrincipal`, and cannot be deserialized from untrusted input.
pub struct GuestMigrationContext<'a> {
    user_id: &'a str,
}
impl GuestMigrationContext<'_> {
    pub fn user_id(&self) -> &str {
        self.user_id
    }
    pub fn tenant_id(&self) -> &str {
        ""
    }
    pub fn tier(&self) -> &str {
        "admin"
    }
    pub fn is_authenticated(&self) -> bool {
        true
    }
    pub fn roles(&self) -> &[&str] {
        &["admin"]
    }
    pub fn to_dict(&self) -> Value {
        json!({
            "user_id": self.user_id,
            "tenant_id": "",
            "tier": "admin",
            "is_authenticated": true,
            "ip": "",
            "roles": ["admin"],
            "user_type": "user",
            "email": "",
            "name": "",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestMigrationUnavailable;
pub type GuestMigrationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), GuestMigrationUnavailable>> + Send + 'a>>;

/// Trusted internal bridge to the source `LangGraphProxy.update_thread` call.
/// Its admin tier can bypass the canonical proxy's owner/tenant checks. The host
/// must independently authorize guest possession and the target user before
/// calling conversion. The manager only records success/failure and never
/// accepts a caller-provided admin context or creates a native principal.
pub trait GuestThreadMigrator: Send + Sync {
    fn update_thread<'a>(
        &'a self,
        user: GuestMigrationContext<'a>,
        thread_id: &'a str,
        metadata: Map<String, Value>,
    ) -> GuestMigrationFuture<'a>;
}

struct GuestClock(Instant);
impl SessionClock for GuestClock {
    fn utc_now(&self) -> chrono::NaiveDateTime {
        let now = Utc::now().naive_utc();
        now.with_nanosecond(now.nanosecond() / 1000 * 1000)
            .expect("microsecond timestamp")
    }
    fn monotonic(&self) -> Duration {
        self.0.elapsed()
    }
}
struct GuestIds;
impl SessionIds for GuestIds {
    fn new_id(&self) -> String {
        uuid::Uuid::new_v4().simple().to_string()
    }
}

pub struct GuestSessionManager {
    pub config: GuestSessionConfig,
    redis: Option<Arc<dyn GuestSessionStorage>>,
    shared: bool,
    sessions: Mutex<HashMap<String, Arc<Mutex<GuestSession>>>>,
    clock: Arc<dyn SessionClock>,
    ids: Arc<dyn SessionIds>,
}
impl GuestSessionManager {
    pub fn new(config: GuestSessionConfig, redis: Option<Arc<dyn GuestSessionStorage>>) -> Self {
        Self::with_runtime(
            config,
            redis,
            Arc::new(GuestClock(Instant::now())),
            Arc::new(GuestIds),
        )
    }

    /// The injected ID source supplies the UUID hex suffix, without `guest_`.
    /// As in Python, no new validation is imposed on negative TTL/quota values.
    pub fn with_runtime(
        config: GuestSessionConfig,
        redis: Option<Arc<dyn GuestSessionStorage>>,
        clock: Arc<dyn SessionClock>,
        ids: Arc<dyn SessionIds>,
    ) -> Self {
        Self {
            config,
            redis,
            shared: false,
            sessions: Mutex::new(HashMap::new()),
            clock,
            ids,
        }
    }

    pub fn shared(
        config: GuestSessionConfig,
        storage: Arc<dyn GuestSessionStorage>,
    ) -> Result<Self, SessionError> {
        if !storage.atomic_capable()
            || config.session_ttl <= 0
            || config.session_ttl > 604800
            || config.max_threads_per_session < 1
            || config.key_prefix.is_empty()
        {
            return Err(SessionError::InvalidConfig("guest_shared_configuration"));
        }
        let mut manager = Self::new(config, Some(storage));
        manager.shared = true;
        Ok(manager)
    }
    pub fn shared_authority_configured(&self) -> bool {
        self.shared
    }
    async fn shared_snapshot(
        &self,
        id: &str,
    ) -> Result<Option<(String, GuestSession)>, SessionError> {
        let store = self
            .redis
            .as_ref()
            .ok_or(SessionError::StorageUnavailable)?;
        let Some(raw) = store.raw_get(&self.session_key(id)).await? else {
            return Ok(None);
        };
        let value: Value = serde_json::from_str(&raw)
            .map_err(|_| SessionError::InvalidData("guest_shared_corrupt"))?;
        if value.get("_native_conversion").is_some() {
            return Err(SessionError::InvalidData(
                "guest_conversion_reconciliation_required",
            ));
        }
        let session = GuestSession::from_dict(value)
            .map_err(|_| SessionError::InvalidData("guest_shared_corrupt"))?;
        if session.session_id != id {
            return Err(SessionError::InvalidData("guest_shared_binding"));
        }
        Ok(Some((raw, session)))
    }
    fn now(&self) -> SessionTimestamp {
        let now = self.clock.utc_now();
        SessionTimestamp::Aware(
            now.with_nanosecond(now.nanosecond() / 1000 * 1000)
                .expect("microsecond timestamp")
                .and_utc()
                .fixed_offset(),
        )
    }

    pub fn session_key(&self, session_id: &str) -> String {
        format!("{}:{session_id}", self.config.key_prefix)
    }

    pub async fn create_session(
        &self,
        metadata: Option<Map<String, Value>>,
    ) -> Result<GuestSession, SessionError> {
        let session_id = format!("guest_{}", self.ids.new_id());
        let now = self.now();
        let session = GuestSession {
            session_id,
            created_at: now.clone(),
            last_active_at: now,
            thread_ids: Vec::new(),
            metadata: metadata.unwrap_or_default(),
        };
        self.save_session(&session).await?;
        Ok(session)
    }

    pub async fn validate_session(&self, session_id: &str) -> Result<bool, SessionError> {
        Ok(self.get_session(session_id).await?.is_some())
    }

    pub async fn get_session(
        &self,
        session_id: &str,
    ) -> Result<Option<GuestSession>, SessionError> {
        if self.shared {
            return Ok(self
                .shared_snapshot(session_id)
                .await?
                .map(|(_, session)| session));
        }
        if let Some(redis) = &self.redis {
            let data = redis.get(&self.session_key(session_id)).await?;
            // Source catches decoding errors only, not failed Redis reads.
            Ok(data.and_then(GuestSessionData::decode))
        } else {
            let sessions = self.sessions.lock().await;
            match sessions.get(session_id) {
                Some(session) => Ok(Some(session.lock().await.clone())),
                None => Ok(None),
            }
        }
    }

    pub async fn refresh_session(&self, session_id: &str) -> Result<bool, SessionError> {
        self.mutate_session(session_id, Mutation::Refresh).await
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<bool, SessionError> {
        if let Some(redis) = &self.redis {
            Ok(redis.delete(&self.session_key(session_id)).await? > 0)
        } else {
            Ok(self.sessions.lock().await.remove(session_id).is_some())
        }
    }

    pub async fn add_thread_to_session(
        &self,
        session_id: &str,
        thread_id: &str,
    ) -> Result<bool, SessionError> {
        self.mutate_session(session_id, Mutation::Add(thread_id))
            .await
    }

    pub async fn remove_thread_from_session(
        &self,
        session_id: &str,
        thread_id: &str,
    ) -> Result<bool, SessionError> {
        self.mutate_session(session_id, Mutation::Remove(thread_id))
            .await
    }

    pub async fn get_session_threads(&self, session_id: &str) -> Result<Vec<String>, SessionError> {
        Ok(self
            .get_session(session_id)
            .await?
            .map_or_else(Vec::new, |session| session.thread_ids))
    }

    /// Caller authorizes both the guest bearer ID and target user. This source
    /// operation has no tenant input and must not be exposed as authentication.
    pub async fn convert_to_user(
        &self,
        session_id: &str,
        user_id: &str,
        migrator: Option<&dyn GuestThreadMigrator>,
    ) -> Result<Value, SessionError> {
        if self.shared {
            return self.convert_shared(session_id, user_id, migrator).await;
        }
        if self.redis.is_none() && migrator.is_none() {
            // Source's memory-only no-proxy path has no suspension point.
            // Snapshot and removal must stay atomic with respect to additions.
            let mut sessions = self.sessions.lock().await;
            let Some(session) = sessions.get(session_id).cloned() else {
                return Ok(json!({"success": false, "error": "Session not found"}));
            };
            let migrated_threads = session.lock().await.thread_ids.clone();
            // The only potentially suspending lock precedes removal, so a
            // cancelled waiter cannot delete an unreported guest session.
            sessions.remove(session_id);
            return Ok(json!({
                "success": true,
                "user_id": user_id,
                "migrated_threads": migrated_threads,
                "failed_threads": [],
            }));
        }
        let memory_session = if self.redis.is_none() {
            self.sessions.lock().await.get(session_id).cloned()
        } else {
            None
        };
        let session = if let Some(session) = &memory_session {
            Some(session.lock().await.clone())
        } else if self.redis.is_some() {
            self.get_session(session_id).await?
        } else {
            None
        };
        let Some(session) = session else {
            return Ok(json!({"success": false, "error": "Session not found"}));
        };
        let mut migrated_threads = Vec::new();
        let mut failed_threads = Vec::new();
        if let Some(migrator) = migrator {
            let mut index = 0;
            loop {
                // Python list iterators fetch the current item at each index
                // after the previous proxy await. Retain the original memory
                // object even if its map entry is deleted/replaced meanwhile.
                let thread_id = if let Some(session) = &memory_session {
                    session.lock().await.thread_ids.get(index).cloned()
                } else {
                    session.thread_ids.get(index).cloned()
                };
                let Some(thread_id) = thread_id else { break };
                index += 1;
                let metadata = Map::from_iter([
                    ("owner_id".into(), json!(user_id)),
                    ("migrated_from_guest".into(), json!(session_id)),
                    ("migrated_at".into(), json!(self.now())),
                ]);
                if migrator
                    .update_thread(GuestMigrationContext { user_id }, &thread_id, metadata)
                    .await
                    .is_ok()
                {
                    migrated_threads.push(thread_id);
                } else {
                    failed_threads.push(thread_id);
                }
            }
        } else {
            migrated_threads = session.thread_ids;
        }
        // Do not retain the guest after partial failure, and do not treat a
        // false delete result as an error. An actual storage error propagates.
        self.delete_session(session_id).await?;
        Ok(json!({
            "success": true,
            "user_id": user_id,
            "migrated_threads": migrated_threads,
            "failed_threads": failed_threads,
        }))
    }

    async fn convert_shared(
        &self,
        session_id: &str,
        user_id: &str,
        migrator: Option<&dyn GuestThreadMigrator>,
    ) -> Result<Value, SessionError> {
        let Some((raw, session)) = self.shared_snapshot(session_id).await? else {
            return Ok(json!({"success":false,"error":"Session not found"}));
        };
        let mut claimed = session.to_dict();
        claimed["_native_conversion"] =
            json!({"claim":uuid::Uuid::new_v4().to_string(),"target_user":user_id});
        let claimed = claimed.to_string();
        let store = self
            .redis
            .as_ref()
            .ok_or(SessionError::StorageUnavailable)?;
        if !store
            .compare_exchange(
                &self.session_key(session_id),
                raw,
                Some(claimed.clone()),
                self.config.session_ttl,
            )
            .await?
        {
            return Err(SessionError::InvalidData("guest_conversion_conflict"));
        }
        let mut migrated = Vec::new();
        let mut failed = Vec::new();
        for thread in session.thread_ids {
            if let Some(migrator) = migrator {
                let metadata = Map::from_iter([
                    ("owner_id".into(), json!(user_id)),
                    ("migrated_from_guest".into(), json!(session_id)),
                    ("migrated_at".into(), json!(self.now())),
                ]);
                if migrator
                    .update_thread(GuestMigrationContext { user_id }, &thread, metadata)
                    .await
                    .is_err()
                {
                    failed.push(thread);
                    continue;
                }
            }
            migrated.push(thread);
        }
        if !store
            .compare_exchange(
                &self.session_key(session_id),
                claimed,
                None,
                self.config.session_ttl,
            )
            .await?
        {
            return Err(SessionError::StorageUnavailable);
        }
        Ok(
            json!({"success":true,"user_id":user_id,"migrated_threads":migrated,"failed_threads":failed}),
        )
    }
    pub async fn cleanup_expired_sessions(&self) -> Result<usize, SessionError> {
        if self.redis.is_some() {
            return Ok(0);
        }
        let now = self.clock.utc_now();
        let now = now
            .with_nanosecond(now.nanosecond() / 1000 * 1000)
            .expect("microsecond timestamp")
            .and_utc();
        let mut sessions = self.sessions.lock().await;
        let mut expired = Vec::new();
        for (id, session) in sessions.iter() {
            let session = session.lock().await;
            let SessionTimestamp::Aware(last_active) = session.last_active_at else {
                return Err(SessionError::InvalidData("naive_guest_session_timestamp"));
            };
            let age = now.signed_duration_since(last_active);
            // Avoid narrowing the source's signed TTL or rounding away the
            // first expired microsecond. Clock rollback keeps future sessions.
            if age.num_seconds() > self.config.session_ttl
                || (age.num_seconds() == self.config.session_ttl && age.subsec_nanos() > 0)
            {
                expired.push(id.clone());
            }
        }
        for id in &expired {
            sessions.remove(id);
        }
        Ok(expired.len())
    }

    async fn save_session(&self, session: &GuestSession) -> Result<(), SessionError> {
        if let Some(redis) = &self.redis {
            redis
                .save(
                    &self.session_key(&session.session_id),
                    session.to_dict().to_string(),
                    self.config.session_ttl,
                )
                .await
        } else {
            self.sessions.lock().await.insert(
                session.session_id.clone(),
                Arc::new(Mutex::new(session.clone())),
            );
            Ok(())
        }
    }

    async fn mutate_session(
        &self,
        session_id: &str,
        mutation: Mutation<'_>,
    ) -> Result<bool, SessionError> {
        if self.shared {
            for _ in 0..16 {
                let Some((raw, mut session)) = self.shared_snapshot(session_id).await? else {
                    return Ok(false);
                };
                let (result, save) = self.apply_mutation(&mut session, mutation);
                if !save {
                    return Ok(result);
                }
                if self
                    .redis
                    .as_ref()
                    .ok_or(SessionError::StorageUnavailable)?
                    .compare_exchange(
                        &self.session_key(session_id),
                        raw,
                        Some(session.to_dict().to_string()),
                        self.config.session_ttl,
                    )
                    .await?
                {
                    return Ok(result);
                }
            }
            return Err(SessionError::StorageUnavailable);
        }
        if self.redis.is_some() {
            let Some(mut session) = self.get_session(session_id).await? else {
                return Ok(false);
            };
            let (result, save) = self.apply_mutation(&mut session, mutation);
            if save {
                self.save_session(&session).await?;
            }
            Ok(result)
        } else {
            // Python's in-memory read/mutate/write has no suspending operation.
            // Keep it atomic here as well; distributed read/write is deliberately
            // nontransactional, exactly as in the Redis branch of the source.
            let sessions = self.sessions.lock().await;
            let Some(session) = sessions.get(session_id) else {
                return Ok(false);
            };
            let mut session = session.lock().await;
            Ok(self.apply_mutation(&mut session, mutation).0)
        }
    }

    fn apply_mutation(&self, session: &mut GuestSession, mutation: Mutation<'_>) -> (bool, bool) {
        match mutation {
            Mutation::Refresh => {
                session.last_active_at = self.now();
                (true, true)
            }
            Mutation::Add(thread_id) => {
                // Quota precedes duplicate detection, even when already linked.
                if session.thread_ids.len() as i128 >= self.config.max_threads_per_session as i128 {
                    return (false, false);
                }
                if session.thread_ids.iter().any(|id| id == thread_id) {
                    return (true, false);
                }
                session.thread_ids.push(thread_id.into());
                session.last_active_at = self.now();
                (true, true)
            }
            Mutation::Remove(thread_id) => {
                if let Some(index) = session.thread_ids.iter().position(|id| id == thread_id) {
                    session.thread_ids.remove(index);
                    // This write extends Redis TTL but NOT memory last activity.
                    (true, true)
                } else {
                    (true, false)
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Mutation<'a> {
    Refresh,
    Add(&'a str),
    Remove(&'a str),
}

pub struct GuestSessionValidator<'a> {
    manager: &'a GuestSessionManager,
}
impl GuestSessionValidator<'_> {
    pub async fn validate(&self, session_id: &str) -> Result<Option<Value>, SessionError> {
        let Some(session) = self.manager.get_session(session_id).await? else {
            return Ok(None);
        };
        // Source makes a second read and ignores a false refresh result, while
        // still propagating a backend error. It returns the first read's fields.
        let refreshed = self.manager.refresh_session(session_id).await?;
        if self.manager.shared && !refreshed {
            return Ok(None);
        }
        Ok(Some(json!({
            "session_id": session.session_id,
            "created_at": session.created_at,
            "thread_count": session.thread_ids.len(),
        })))
    }
}

/// Creating the validator performs no I/O; `validate` is its async callback.
pub fn create_guest_session_validator(manager: &GuestSessionManager) -> GuestSessionValidator<'_> {
    GuestSessionValidator { manager }
}
