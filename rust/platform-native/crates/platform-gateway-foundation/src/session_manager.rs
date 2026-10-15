//! Gateway legacy-session lifecycle, ported from `src/services/session/session_manager.py`
//! and `ai_gateway_core/session/{models,database_manager}.py`.
//!
//! This is separate from Assistant/Runtime history projection in `session_history`:
//! it neither starts a Runtime turn nor writes Runtime items. All ID-only methods
//! are internal and require the caller to establish ownership first. Public callers
//! should use `get_or_create`, which checks BOTH tenant and user and the service pin.
//! Returned records are snapshots, not Python's mutable object aliases.
//!
//! Memory expiry is strict (< now); durable expiry renews only on a cache miss.
//! Durable storage is injected and must implement atomic insert/append. Its local
//! cache is bounded and monotonic; a distributed Redis adapter, general database
//! full-record list APIs and immutable Agent runtime binding remain outside this module.
//! No PostgreSQL connection or HTTP route is silently created here.
use chrono::{DateTime, FixedOffset, NaiveDateTime, Timelike, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};
use std::{
    collections::{HashMap, VecDeque},
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub type SessionFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SessionError>> + Send + 'a>>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionError {
    AlreadyExists,
    PermissionDenied(&'static str),
    InvalidConfig(&'static str),
    InvalidData(&'static str),
    /// Python's strip().splitlines()[0] fails on empty/whitespace-only user text.
    EmptyTitleLine,
    StorageUnavailable,
}
impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AlreadyExists => "session already exists",
            Self::PermissionDenied(message)
            | Self::InvalidConfig(message)
            | Self::InvalidData(message) => message,
            Self::EmptyTitleLine => "list index out of range",
            Self::StorageUnavailable => "session_storage_unavailable",
        })
    }
}
impl std::error::Error for SessionError {}

/// Preserve Python naive-versus-aware datetime ISO formatting, including six
/// fractional digits when nonzero. Storage conversion accepts RFC3339 Z input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionTimestamp {
    Naive(NaiveDateTime),
    Aware(DateTime<FixedOffset>),
}
impl SessionTimestamp {
    pub fn naive(time: NaiveDateTime) -> Self {
        Self::Naive(micros(time))
    }
    pub fn parse(text: &str) -> Result<Self, SessionError> {
        if let Ok(time) = DateTime::parse_from_rfc3339(text) {
            return Ok(Self::Aware(time));
        }
        NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
            .map(Self::Naive)
            .map_err(|_| SessionError::InvalidData("invalid_session_timestamp"))
    }
    fn local(&self) -> NaiveDateTime {
        match self {
            Self::Naive(time) => *time,
            Self::Aware(time) => time.naive_local(),
        }
    }
    fn sort_key(&self) -> NaiveDateTime {
        match self {
            Self::Naive(time) => *time,
            Self::Aware(time) => time.naive_utc(),
        }
    }
    pub fn isoformat(&self) -> String {
        let time = self.local();
        let mut text = if time.nanosecond() == 0 {
            time.format("%Y-%m-%dT%H:%M:%S").to_string()
        } else {
            time.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
        };
        if let Self::Aware(time) = self {
            text.push_str(&time.format("%:z").to_string());
        }
        text
    }
}
impl Serialize for SessionTimestamp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.isoformat())
    }
}
impl<'de> Deserialize<'de> for SessionTimestamp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
fn micros(time: NaiveDateTime) -> NaiveDateTime {
    time.with_nanosecond(time.nanosecond() / 1000 * 1000)
        .expect("microsecond timestamp")
}
fn empty_map() -> Option<Map<String, Value>> {
    Some(Map::new())
}
fn active() -> String {
    "active".into()
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SessionMessage {
    pub role: String,
    pub content: Value,
    pub timestamp: Option<SessionTimestamp>,
    pub metadata: Option<Map<String, Value>>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub session_id: String,
    pub user_id: String,
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub service_id: Option<String>,
    pub created_at: SessionTimestamp,
    #[serde(default)]
    pub updated_at: Option<SessionTimestamp>,
    #[serde(default)]
    pub metadata: Option<Map<String, Value>>,
    #[serde(default)]
    pub history: Vec<SessionMessage>,
    #[serde(default = "empty_map")]
    pub state: Option<Map<String, Value>>,
    #[serde(default = "empty_map")]
    pub config: Option<Map<String, Value>>,
    #[serde(default = "active")]
    pub status: String,
    #[serde(default)]
    pub expires_at: Option<SessionTimestamp>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_version_id: Option<String>,
    #[serde(default)]
    pub agent_draft_revision: Option<i64>,
    #[serde(default)]
    pub publication_id: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub runtime_fingerprint: Option<String>,
    #[serde(default)]
    pub agent_spec_hash: Option<String>,
}
/// Small owned snapshot used for generic session-route ownership checks.
/// Service pin enforcement remains in get_or_create; this is not authorization.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionOwner {
    pub user_id: String,
    pub tenant_id: String,
}
impl From<&Session> for SessionOwner {
    fn from(session: &Session) -> Self {
        Self {
            user_id: session.user_id.clone(),
            tenant_id: session.tenant_id.clone(),
        }
    }
}
impl Session {
    /// Exact storage projection: created_at/updated_at are owned by the database
    /// and are intentionally absent, unlike serialization of the domain record.
    pub fn to_storage_value(&self) -> Value {
        json!({"session_id":self.session_id,"service_id":self.service_id,"user_id":self.user_id,
            "tenant_id":self.tenant_id,"state":self.state.clone().unwrap_or_default(),"history":self.history,
            "metadata":self.metadata.clone().unwrap_or_default(),"config":self.config.clone().unwrap_or_default(),
            "status":self.status,"expires_at":self.expires_at,"agent_id":self.agent_id,
            "agent_version_id":self.agent_version_id,"agent_draft_revision":self.agent_draft_revision,
            "publication_id":self.publication_id,"channel":self.channel,"runtime_fingerprint":self.runtime_fingerprint,
            "agent_spec_hash":self.agent_spec_hash})
    }
    /// Database manager normalization: skip non-dictionary history entries,
    /// default missing message fields and normalize absent/null maps to {}.
    /// Annotation-invalid object/scalar values fail closed instead of coercing.
    pub fn from_storage_value(mut value: Value, now: NaiveDateTime) -> Result<Self, SessionError> {
        let row = value
            .as_object_mut()
            .ok_or(SessionError::InvalidData("invalid_session_row"))?;
        for key in ["metadata", "state", "config"] {
            if row.get(key).is_none_or(|v| v.is_null()) {
                row.insert(key.into(), json!({}));
            }
        }
        if row.get("created_at").is_none_or(|v| v.is_null()) {
            row.insert("created_at".into(), json!(SessionTimestamp::naive(now)));
        }
        row.entry("user_id").or_insert(json!(""));
        let history = row
            .entry("history")
            .or_insert(json!([]))
            .as_array_mut()
            .ok_or(SessionError::InvalidData("invalid_session_history"))?;
        history.retain(Value::is_object);
        for message in history {
            let message = message.as_object_mut().expect("retained object");
            message.entry("role").or_insert(json!("user"));
            message.entry("content").or_insert(json!(""));
            message.entry("timestamp").or_insert(Value::Null);
            message.entry("metadata").or_insert(Value::Null);
        }
        serde_json::from_value(value).map_err(|_| SessionError::InvalidData("invalid_session_row"))
    }
}

pub trait SessionClock: Send + Sync {
    fn utc_now(&self) -> NaiveDateTime;
    fn monotonic(&self) -> Duration;
}
struct SystemClock(Instant);
impl SessionClock for SystemClock {
    fn utc_now(&self) -> NaiveDateTime {
        micros(Utc::now().naive_utc())
    }
    fn monotonic(&self) -> Duration {
        self.0.elapsed()
    }
}
pub trait SessionIds: Send + Sync {
    fn new_id(&self) -> String;
}
struct RandomIds;
impl SessionIds for RandomIds {
    fn new_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}
#[derive(Clone, Debug, Default)]
pub struct CreateSession {
    pub user_id: String,
    pub tenant_id: String,
    pub service_id: Option<String>,
    pub session_id: Option<String>,
    pub metadata: Option<Map<String, Value>>,
    /// Used by the durable manager. The memory manager has no config argument.
    pub config: Option<Map<String, Value>>,
    pub ttl: Option<i64>,
    pub fail_if_exists: bool,
}
#[derive(Clone, Debug, Default)]
pub struct GetOrCreateSession {
    pub session_id: Option<String>,
    pub user_id: String,
    pub tenant_id: String,
    pub service_id: Option<String>,
    pub ttl: Option<i64>,
}
pub trait SessionLifecycle: Send + Sync {
    fn get_or_create(&self, request: GetOrCreateSession) -> SessionFuture<'_, Session>;
    fn add_message<'a>(
        &'a self,
        session_id: &'a str,
        role: &'a str,
        content: Value,
        metadata: Option<Map<String, Value>>,
    ) -> SessionFuture<'a, ()>;
}
#[derive(Clone, Debug)]
pub struct SessionManagerConfig {
    pub default_session_ttl: i64,
    pub anonymous_session_ttl: i64,
    pub max_sessions: usize,
    pub cleanup_interval: i64,
}
impl Default for SessionManagerConfig {
    fn default() -> Self {
        Self {
            default_session_ttl: 7 * 86400,
            anonymous_session_ttl: 86400,
            max_sessions: 10000,
            cleanup_interval: 300,
        }
    }
}
#[derive(Default)]
struct OrderedSessions {
    sessions: HashMap<String, Session>,
    order: VecDeque<String>,
}
impl OrderedSessions {
    fn remove(&mut self, id: &str) -> Option<Session> {
        self.order.retain(|key| key != id);
        self.sessions.remove(id)
    }
    fn touch(&mut self, id: &str) {
        self.order.retain(|key| key != id);
        self.order.push_back(id.into());
    }
    fn insert(&mut self, session: Session) {
        if !self.sessions.contains_key(&session.session_id) {
            self.order.push_back(session.session_id.clone());
        }
        self.sessions.insert(session.session_id.clone(), session);
    }
    fn evict(&mut self) -> Option<String> {
        let id = self.order.pop_front()?;
        self.sessions.remove(&id);
        Some(id)
    }
}
struct MemoryState {
    records: OrderedSessions,
    last_cleanup: NaiveDateTime,
}
pub struct MemorySessionManager {
    config: SessionManagerConfig,
    state: Mutex<MemoryState>,
    clock: Arc<dyn SessionClock>,
    ids: Arc<dyn SessionIds>,
}
/// Canonical memory-manager name for callers migrating the Python dependency.
pub type SessionManager = MemorySessionManager;
impl MemorySessionManager {
    pub fn new(config: SessionManagerConfig) -> Result<Self, SessionError> {
        Self::with_runtime(
            config,
            Arc::new(SystemClock(Instant::now())),
            Arc::new(RandomIds),
        )
    }
    pub fn with_runtime(
        config: SessionManagerConfig,
        clock: Arc<dyn SessionClock>,
        ids: Arc<dyn SessionIds>,
    ) -> Result<Self, SessionError> {
        if config.max_sessions == 0 {
            return Err(SessionError::InvalidConfig("max_sessions must be positive"));
        }
        let last_cleanup = clock.utc_now();
        Ok(Self {
            config,
            state: Mutex::new(MemoryState {
                records: OrderedSessions::default(),
                last_cleanup,
            }),
            clock,
            ids,
        })
    }
    pub async fn create(&self, request: CreateSession) -> Result<Session, SessionError> {
        let now = self.clock.utc_now();
        let mut state = self.state.lock().await;
        self.create_locked(&mut state, request, now)
    }
    fn create_locked(
        &self,
        state: &mut MemoryState,
        request: CreateSession,
        now: NaiveDateTime,
    ) -> Result<Session, SessionError> {
        if (now - state.last_cleanup).num_milliseconds()
            >= self.config.cleanup_interval.saturating_mul(1000)
        {
            cleanup(state, now);
        }
        let mut request = request;
        request.config = None;
        let session = build_session(request.clone(), &self.config, self.ids.as_ref(), now, false)?;
        if request.fail_if_exists && state.records.sessions.contains_key(&session.session_id) {
            return Err(SessionError::AlreadyExists);
        }
        // Preserve Python's pre-insert eviction even when replacing an existing ID.
        while state.records.sessions.len() >= self.config.max_sessions {
            state.records.evict();
        }
        state.records.insert(session.clone());
        Ok(session)
    }
    pub async fn get(&self, id: &str) -> Result<Option<Session>, SessionError> {
        Ok(memory_get(
            &mut self.state.lock().await.records,
            id,
            self.clock.utc_now(),
        ))
    }
    pub async fn delete(&self, id: &str) {
        self.state.lock().await.records.remove(id);
    }
    /// The generic Python route mutates its stored object alias. Native reads
    /// are snapshots, so apply the same shallow merge under the existing lock.
    /// Do not change updated_at or LRU order (the route's get already did that).
    pub async fn update_metadata(&self, id: &str, patch: Map<String, Value>) -> bool {
        let mut state = self.state.lock().await;
        let Some(session) = state.records.sessions.get_mut(id) else {
            return false;
        };
        session.metadata.get_or_insert_with(Map::new).extend(patch);
        true
    }
    pub async fn update_config(&self, id: &str, patch: Map<String, Value>) -> bool {
        let mut state = self.state.lock().await;
        let Some(session) = state.records.sessions.get_mut(id) else {
            return false;
        };
        session.config.get_or_insert_with(Map::new).extend(patch);
        true
    }
    pub async fn cleanup_expired(&self) -> usize {
        cleanup(&mut *self.state.lock().await, self.clock.utc_now())
    }
    pub async fn history(&self, id: &str, limit: i64) -> Vec<SessionMessage> {
        self.state
            .lock()
            .await
            .records
            .sessions
            .get(id)
            .map(|session| history_slice(&session.history, limit).to_vec())
            .unwrap_or_default()
    }
    pub async fn list_sessions(&self, query: SessionListQuery) -> Vec<Session> {
        let now = self.clock.utc_now();
        let state = self.state.lock().await;
        let mut sessions: Vec<_> = state
            .records
            .order
            .iter()
            .filter_map(|id| state.records.sessions.get(id))
            .filter(|session| !expired(session, now))
            .filter(|session| filter_matches(&query.user_id, &session.user_id))
            .filter(|session| filter_matches(&query.tenant_id, &session.tenant_id))
            .filter(|session| {
                filter_matches(
                    &query.service_id,
                    session.service_id.as_deref().unwrap_or(""),
                )
            })
            .cloned()
            .collect();
        // Stable ties retain current LRU order, as Python's stable sort does.
        sessions.sort_by_key(|session| {
            std::cmp::Reverse(
                session
                    .updated_at
                    .as_ref()
                    .unwrap_or(&session.created_at)
                    .sort_key(),
            )
        });
        sessions.truncate(prefix_len(sessions.len(), query.limit));
        sessions
    }
    pub async fn list_session_summaries(&self, query: SessionSummaryQuery) -> Vec<Value> {
        let service_id = if query.service_ids.len() == 1 {
            query.service_ids.first().cloned()
        } else {
            None
        };
        let sessions = self
            .list_sessions(SessionListQuery {
                user_id: query.user_id,
                tenant_id: query.tenant_id,
                service_id,
                limit: query.limit,
            })
            .await;
        sessions.into_iter().filter(|session| {
            (query.service_ids.len() <= 1 || query.service_ids.iter().any(|id| session.service_id.as_ref() == Some(id))
                || (query.include_null_service_id && session.service_id.as_deref().is_none_or(str::is_empty)))
                && (query.status.is_empty() || query.status == session.status)
        }).map(|s| json!({"session_id":s.session_id,"service_id":s.service_id,"user_id":s.user_id,"tenant_id":s.tenant_id,
            "metadata":s.metadata,"config":s.config,"status":s.status,"created_at":s.created_at,"updated_at":s.updated_at})).collect()
    }
    pub async fn get_stats(&self) -> SessionStats {
        let state = self.state.lock().await;
        let now = self.clock.utc_now();
        let total = state.records.sessions.len();
        let expired = state
            .records
            .sessions
            .values()
            .filter(|session| expired(session, now))
            .count();
        SessionStats {
            total_sessions: total,
            active_sessions: total - expired,
            expired_sessions: expired,
            max_sessions: self.config.max_sessions,
        }
    }
}
impl SessionLifecycle for MemorySessionManager {
    fn get_or_create(&self, request: GetOrCreateSession) -> SessionFuture<'_, Session> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            let now = self.clock.utc_now();
            if let Some(id) = request.session_id.as_deref().filter(|id| !id.is_empty())
                && let Some(mut session) = memory_get(&mut state.records, id, now)
            {
                verify_binding(&mut session, &request)?;
                state.records.sessions.insert(id.into(), session.clone());
                return Ok(session);
            }
            self.create_locked(&mut state, create_request(request), now)
        })
    }
    fn add_message<'a>(
        &'a self,
        id: &'a str,
        role: &'a str,
        content: Value,
        metadata: Option<Map<String, Value>>,
    ) -> SessionFuture<'a, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            let Some(session) = state.records.sessions.get_mut(id) else {
                return Ok(());
            };
            session.history.push(SessionMessage {
                role: role.into(),
                content: content.clone(),
                timestamp: Some(SessionTimestamp::naive(self.clock.utc_now())),
                metadata,
            });
            if role == "user" {
                let metadata = session.metadata.get_or_insert_with(Map::new);
                if !metadata.get("title").is_some_and(truthy) {
                    metadata.insert(
                        "title".into(),
                        json!(auto_title(&crate::usage_parser::python_value_to_string(
                            &content
                        ))?),
                    );
                }
            }
            session.updated_at = Some(SessionTimestamp::naive(self.clock.utc_now()));
            state.records.touch(id);
            Ok(())
        })
    }
}
#[derive(Clone, Debug)]
pub struct SessionListQuery {
    pub user_id: Option<String>,
    pub tenant_id: Option<String>,
    pub service_id: Option<String>,
    pub limit: i64,
}
impl Default for SessionListQuery {
    fn default() -> Self {
        Self {
            user_id: None,
            tenant_id: None,
            service_id: None,
            limit: 100,
        }
    }
}
#[derive(Clone, Debug)]
pub struct SessionSummaryQuery {
    pub user_id: Option<String>,
    pub tenant_id: Option<String>,
    pub service_ids: Vec<String>,
    pub include_null_service_id: bool,
    pub status: String,
    pub limit: i64,
}
impl Default for SessionSummaryQuery {
    fn default() -> Self {
        Self {
            user_id: None,
            tenant_id: None,
            service_ids: Vec::new(),
            include_null_service_id: false,
            status: active(),
            limit: 100,
        }
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct SessionStats {
    pub total_sessions: usize,
    pub active_sessions: usize,
    pub expired_sessions: usize,
    pub max_sessions: usize,
}
fn filter_matches(filter: &Option<String>, value: &str) -> bool {
    filter
        .as_ref()
        .is_none_or(|filter| filter.is_empty() || filter == value)
}
fn expired(session: &Session, now: NaiveDateTime) -> bool {
    session
        .expires_at
        .as_ref()
        .is_some_and(|expiry| expiry.local() < now)
}
fn memory_get(records: &mut OrderedSessions, id: &str, now: NaiveDateTime) -> Option<Session> {
    let session = records.sessions.get(id)?.clone();
    if expired(&session, now) {
        records.remove(id);
        return None;
    }
    records.touch(id);
    Some(session)
}
fn cleanup(state: &mut MemoryState, now: NaiveDateTime) -> usize {
    let ids: Vec<_> = state
        .records
        .sessions
        .values()
        .filter(|session| expired(session, now))
        .map(|session| session.session_id.clone())
        .collect();
    for id in &ids {
        state.records.remove(id);
    }
    state.last_cleanup = now;
    ids.len()
}
fn prefix_len(len: usize, limit: i64) -> usize {
    if limit >= 0 {
        len.min(limit as usize)
    } else {
        len.saturating_sub(limit.unsigned_abs() as usize)
    }
}
fn history_slice(history: &[SessionMessage], limit: i64) -> &[SessionMessage] {
    // Python [-0:] returns everything, while a negative limit becomes [abs(n):].
    let start = if limit > 0 {
        history.len().saturating_sub(limit as usize)
    } else if limit == 0 {
        0
    } else {
        history.len().min(limit.unsigned_abs() as usize)
    };
    &history[start..]
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64().is_some_and(|v| v != 0.),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
fn python_space(character: char) -> bool {
    character.is_whitespace() || matches!(character, '\u{1c}'..='\u{1f}')
}
fn auto_title(text: &str) -> Result<String, SessionError> {
    let text = text.trim_matches(python_space);
    if text.is_empty() {
        return Err(SessionError::EmptyTitleLine);
    }
    Ok(text
        .split([
            '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
            '\u{2029}',
        ])
        .next()
        .expect("nonempty string")
        .chars()
        .take(40)
        .collect())
}
fn verify_binding(session: &mut Session, request: &GetOrCreateSession) -> Result<(), SessionError> {
    if session.user_id != request.user_id || session.tenant_id != request.tenant_id {
        return Err(SessionError::PermissionDenied(
            "Session does not belong to current user",
        ));
    }
    if let Some(service) = request.service_id.as_deref().filter(|id| !id.is_empty()) {
        if let Some(existing) = session.service_id.as_deref().filter(|id| !id.is_empty()) {
            if existing != service {
                return Err(SessionError::PermissionDenied(
                    "Session is bound to a different service",
                ));
            }
        } else {
            session.service_id = Some(service.into());
        }
    }
    Ok(())
}
fn create_request(request: GetOrCreateSession) -> CreateSession {
    CreateSession {
        user_id: request.user_id,
        tenant_id: request.tenant_id,
        service_id: request.service_id,
        ttl: request.ttl,
        ..Default::default()
    }
}
fn build_session(
    request: CreateSession,
    config: &SessionManagerConfig,
    ids: &dyn SessionIds,
    now: NaiveDateTime,
    durable: bool,
) -> Result<Session, SessionError> {
    let ttl = request
        .ttl
        .unwrap_or(if request.user_id.starts_with("anon:") {
            config.anonymous_session_ttl
        } else {
            config.default_session_ttl
        });
    let expires_at = if ttl != 0 && (durable || ttl > 0) {
        Some(SessionTimestamp::naive(
            now.checked_add_signed(
                chrono::Duration::try_seconds(ttl)
                    .ok_or(SessionError::InvalidData("session_ttl_out_of_range"))?,
            )
            .ok_or(SessionError::InvalidData("session_ttl_out_of_range"))?,
        ))
    } else {
        None
    };
    Ok(Session {
        session_id: request
            .session_id
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| ids.new_id()),
        user_id: request.user_id,
        tenant_id: request.tenant_id,
        service_id: request.service_id,
        created_at: SessionTimestamp::naive(now),
        updated_at: Some(SessionTimestamp::naive(now)),
        metadata: Some(request.metadata.unwrap_or_default()),
        history: Vec::new(),
        state: empty_map(),
        config: Some(request.config.unwrap_or_default()),
        status: active(),
        expires_at,
        agent_id: None,
        agent_version_id: None,
        agent_draft_revision: None,
        publication_id: None,
        channel: None,
        runtime_fingerprint: None,
        agent_spec_hash: None,
    })
}

/// Internal persistence seam, matching canonical DatabaseStorage operations. It
/// carries no authentication: callers MUST verify tenant/user before exposing
/// these ID-based operations. Adapters must not implement append with a lossy
/// read-modify-write, nor insert-if-absent with a racy preflight read.
pub trait SessionStorage: Send + Sync {
    /// Lightweight source sidebar projection. Empty service_ids means no
    /// service filter; the generic API never requests an explicit empty set.
    /// Older injected stores fail explicitly until they support this extension.
    fn list_session_summaries(&self, _query: SessionSummaryQuery) -> SessionFuture<'_, Vec<Value>> {
        Box::pin(async { Err(SessionError::StorageUnavailable) })
    }
    /// Atomic shallow JSONB merges, never full-session save/read-modify-write.
    fn update_metadata<'a>(
        &'a self,
        _id: &'a str,
        _patch: Map<String, Value>,
    ) -> SessionFuture<'a, bool> {
        Box::pin(async { Err(SessionError::StorageUnavailable) })
    }
    fn update_config<'a>(
        &'a self,
        _id: &'a str,
        _patch: Map<String, Value>,
    ) -> SessionFuture<'a, bool> {
        Box::pin(async { Err(SessionError::StorageUnavailable) })
    }
    fn get<'a>(&'a self, id: &'a str) -> SessionFuture<'a, Option<Value>>;
    fn save(&self, session: Value) -> SessionFuture<'_, ()>;
    fn create_if_absent(&self, session: Value) -> SessionFuture<'_, bool>;
    fn append_message<'a>(
        &'a self,
        id: &'a str,
        message: SessionMessage,
        metadata_update: Option<Map<String, Value>>,
    ) -> SessionFuture<'a, bool>;
    fn delete<'a>(&'a self, id: &'a str) -> SessionFuture<'a, bool>;
    /// Change expiry only: never touch updated_at or history.
    fn extend_expiry<'a>(
        &'a self,
        id: &'a str,
        expires_at: SessionTimestamp,
    ) -> SessionFuture<'a, ()>;
}
#[derive(Clone, Debug)]
pub struct DatabaseSessionConfig {
    pub default_session_ttl: i64,
    pub anonymous_session_ttl: i64,
    pub cache_ttl: i64,
    pub memory_cache_max_entries: usize,
}
impl Default for DatabaseSessionConfig {
    fn default() -> Self {
        Self {
            default_session_ttl: 7 * 86400,
            anonymous_session_ttl: 86400,
            cache_ttl: 3600,
            memory_cache_max_entries: 1024,
        }
    }
}
#[derive(Default)]
struct SessionCache {
    records: OrderedSessions,
    deadlines: HashMap<String, Duration>,
}
pub struct DatabaseSessionManager {
    storage: Arc<dyn SessionStorage>,
    config: DatabaseSessionConfig,
    cache: Mutex<SessionCache>,
    clock: Arc<dyn SessionClock>,
    ids: Arc<dyn SessionIds>,
}
impl DatabaseSessionManager {
    pub fn new(storage: Arc<dyn SessionStorage>, config: DatabaseSessionConfig) -> Self {
        Self::with_runtime(
            storage,
            config,
            Arc::new(SystemClock(Instant::now())),
            Arc::new(RandomIds),
        )
    }
    pub fn with_runtime(
        storage: Arc<dyn SessionStorage>,
        mut config: DatabaseSessionConfig,
        clock: Arc<dyn SessionClock>,
        ids: Arc<dyn SessionIds>,
    ) -> Self {
        config.memory_cache_max_entries = config.memory_cache_max_entries.max(1);
        Self {
            storage,
            config,
            cache: Mutex::new(SessionCache::default()),
            clock,
            ids,
        }
    }
    pub async fn create(&self, request: CreateSession) -> Result<Session, SessionError> {
        let atomic = request.fail_if_exists || request.session_id.is_some();
        let config = SessionManagerConfig {
            default_session_ttl: self.config.default_session_ttl,
            anonymous_session_ttl: self.config.anonymous_session_ttl,
            ..Default::default()
        };
        let session = build_session(
            request,
            &config,
            self.ids.as_ref(),
            self.clock.utc_now(),
            true,
        )?;
        if atomic {
            if !self
                .storage
                .create_if_absent(session.to_storage_value())
                .await?
            {
                return Err(SessionError::AlreadyExists);
            }
        } else {
            self.storage.save(session.to_storage_value()).await?;
        }
        self.cache_session(session.clone()).await;
        Ok(session)
    }
    pub async fn get(&self, id: &str) -> Result<Option<Session>, SessionError> {
        self.get_projected(id, Clone::clone).await
    }
    /// Read ownership without copying history or other unreturned payloads.
    /// This retains get's cache/expiry lifecycle and does not bind a service.
    pub async fn get_owner(&self, id: &str) -> Result<Option<SessionOwner>, SessionError> {
        self.get_projected(id, |session| SessionOwner::from(session))
            .await
    }
    // Project only the fields the caller needs while borrowing the cached record.
    // Keep all expiry, LRU, cache-miss renewal and storage semantics shared with get.
    async fn get_projected<T>(
        &self,
        id: &str,
        project: impl FnOnce(&Session) -> T,
    ) -> Result<Option<T>, SessionError> {
        {
            let mut cache = self.cache.lock().await;
            if cache
                .deadlines
                .get(id)
                .is_some_and(|deadline| *deadline <= self.clock.monotonic())
            {
                cache.records.remove(id);
                cache.deadlines.remove(id);
            }
            if let Some(session) = cache.records.sessions.get(id) {
                let result = project(session);
                cache.records.touch(id);
                return Ok(Some(result));
            }
        }
        let Some(row) = self.storage.get(id).await? else {
            return Ok(None);
        };
        let mut session = Session::from_storage_value(row, self.clock.utc_now())?;
        if expired(&session, self.clock.utc_now()) {
            self.extend_ttl(id, None).await?;
            session.expires_at = Some(self.expiry_from_now(self.config.default_session_ttl)?);
        }
        let result = project(&session);
        self.cache_session(session).await;
        Ok(Some(result))
    }
    pub async fn history(&self, id: &str, limit: i64) -> Result<Vec<SessionMessage>, SessionError> {
        Ok(self
            .get_projected(id, |session| {
                history_slice(&session.history, limit).to_vec()
            })
            .await?
            .unwrap_or_default())
    }
    pub async fn delete(&self, id: &str) -> Result<bool, SessionError> {
        let result = self.storage.delete(id).await?;
        self.invalidate(id).await;
        Ok(result)
    }
    pub async fn list_session_summaries(
        &self,
        query: SessionSummaryQuery,
    ) -> Result<Vec<Value>, SessionError> {
        // Do not consult the session cache or renew/filter expired rows.
        self.storage.list_session_summaries(query).await
    }
    pub async fn update_metadata(
        &self,
        id: &str,
        patch: Map<String, Value>,
    ) -> Result<bool, SessionError> {
        if self.get_projected(id, |_| ()).await?.is_none() {
            return Ok(false);
        }
        // Python checks len(json.dumps(patch)), not UTF-8 length or compact JSON.
        // Default ensure_ascii=True includes six characters per UTF-16 escape.
        if python_json_length(&Value::Object(patch.clone())) > 1_048_576 {
            return Ok(false);
        }
        if !self.storage.update_metadata(id, patch).await? {
            return Ok(false);
        }
        self.invalidate(id).await;
        Ok(true)
    }
    pub async fn update_config(
        &self,
        id: &str,
        patch: Map<String, Value>,
    ) -> Result<bool, SessionError> {
        if self.get_projected(id, |_| ()).await?.is_none() {
            return Ok(false);
        }
        if !self.storage.update_config(id, patch).await? {
            return Ok(false);
        }
        self.invalidate(id).await;
        Ok(true)
    }
    pub async fn extend_ttl(&self, id: &str, seconds: Option<i64>) -> Result<(), SessionError> {
        let seconds = seconds
            .filter(|v| *v != 0)
            .unwrap_or(self.config.default_session_ttl);
        self.storage
            .extend_expiry(id, self.expiry_from_now(seconds)?)
            .await
    }
    fn expiry_from_now(&self, seconds: i64) -> Result<SessionTimestamp, SessionError> {
        let duration = chrono::Duration::try_seconds(seconds)
            .ok_or(SessionError::InvalidData("session_ttl_out_of_range"))?;
        self.clock
            .utc_now()
            .checked_add_signed(duration)
            .map(SessionTimestamp::naive)
            .ok_or(SessionError::InvalidData("session_ttl_out_of_range"))
    }
    pub async fn invalidate(&self, id: &str) {
        let mut cache = self.cache.lock().await;
        cache.records.remove(id);
        cache.deadlines.remove(id);
    }
    async fn cache_session(&self, session: Session) {
        let mut cache = self.cache.lock().await;
        let id = session.session_id.clone();
        cache.records.insert(session);
        cache.records.touch(&id);
        cache.deadlines.insert(
            id,
            self.clock
                .monotonic()
                .saturating_add(Duration::from_secs(self.config.cache_ttl.max(1) as u64)),
        );
        while cache.records.sessions.len() > self.config.memory_cache_max_entries {
            if let Some(id) = cache.records.evict() {
                cache.deadlines.remove(&id);
            }
        }
    }
    pub async fn append_message(
        &self,
        id: &str,
        role: &str,
        content: Value,
        metadata: Option<Map<String, Value>>,
    ) -> Result<bool, SessionError> {
        let timestamp = Some(SessionTimestamp::naive(self.clock.utc_now()));
        let mut update = None;
        if role == "user"
            && self
                .get_projected(id, |session| {
                    !session
                        .metadata
                        .as_ref()
                        .and_then(|m| m.get("title"))
                        .is_some_and(truthy)
                })
                .await?
                .unwrap_or(false)
        {
            update = Some(Map::from_iter([(
                "title".into(),
                json!(auto_title(&crate::usage_parser::python_value_to_string(
                    &content
                ))?),
            )]));
        }
        let result = self
            .storage
            .append_message(
                id,
                SessionMessage {
                    role: role.into(),
                    content,
                    timestamp,
                    metadata,
                },
                update,
            )
            .await?;
        if result {
            self.invalidate(id).await;
        }
        Ok(result)
    }
}
impl SessionLifecycle for DatabaseSessionManager {
    fn get_or_create(&self, request: GetOrCreateSession) -> SessionFuture<'_, Session> {
        Box::pin(async move {
            if let Some(id) = request.session_id.as_deref().filter(|id| !id.is_empty())
                && let Some(mut session) = self.get(id).await?
            {
                let previous_service = session.service_id.clone();
                verify_binding(&mut session, &request)?;
                // Python mutates only its in-process cached object, not durable
                // storage. Keep that distinction; service pin persistence is not
                // silently invented here. An evicted pin can be rebound later.
                if previous_service != session.service_id {
                    let mut cache = self.cache.lock().await;
                    if let Some(cached) = cache.records.sessions.get_mut(id) {
                        verify_binding(cached, &request)?;
                        session.service_id.clone_from(&cached.service_id);
                    }
                }
                return Ok(session);
            }
            self.create(create_request(request)).await
        })
    }
    fn add_message<'a>(
        &'a self,
        id: &'a str,
        role: &'a str,
        content: Value,
        metadata: Option<Map<String, Value>>,
    ) -> SessionFuture<'a, ()> {
        Box::pin(async move {
            self.append_message(id, role, content, metadata)
                .await
                .map(|_| ())
        })
    }
}

/// Character length of Python json.dumps with default ensure_ascii/separators.
/// Native values are finite JSON with i64/u64/f64 numbers, as elsewhere here.
fn python_json_length(value: &Value) -> usize {
    fn string_length(text: &str) -> usize {
        2 + text
            .chars()
            .map(|c| match c {
                '"' | '\\' | '\x08' | '\x0c' | '\n' | '\r' | '\t' => 2,
                '\x00'..='\x1f' => 6,
                '\x20'..='\x7e' => 1,
                _ => c.len_utf16() * 6,
            })
            .sum::<usize>()
    }
    match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(_) => crate::usage_parser::python_value_to_string(value)
            .chars()
            .count(),
        Value::String(text) => string_length(text),
        Value::Array(values) => {
            2 + values.iter().map(python_json_length).sum::<usize>()
                + values.len().saturating_sub(1) * 2
        }
        Value::Object(values) => {
            2 + values
                .iter()
                .map(|(key, value)| string_length(key) + 2 + python_json_length(value))
                .sum::<usize>()
                + values.len().saturating_sub(1) * 2
        }
    }
}

#[cfg(test)]
#[path = "session_manager_tests.rs"]
mod tests;
