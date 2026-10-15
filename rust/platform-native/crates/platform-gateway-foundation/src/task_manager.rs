//! Task domain, lifecycle and queues from `src/models/task.py` and
//! `src/services/task/{task_manager,task_queue}.py`.
//!
//! Domain service shared by the authenticated task API and its worker.
//! Callers must authenticate and enforce task ownership before get/cancel/result.
//! The durable constructor publishes task and delivery in one transaction.
//! The explicit legacy/test constructor retains its historical queue semantics.
//! Native worker and cancellation transitions use atomic status comparisons;
//! terminal outcomes cannot be resurrected through the manager. Cancellation is
//! observed by the worker and signals local dispatch cancellation, without any
//! promise that a remote effect was stopped or rolled back.
//! Callbacks are best effort and never roll back saved status. No network client
//! is provided: an explicitly injected safe transport must validate destinations,
//! pin validated DNS addresses on every attempt, and refuse redirects. The
//! default transport denies every attempt, including apparently public URLs.
//! Redis is likewise an injected collaborator; fallback is process-local and
//! neither queue promises crash recovery, acknowledgements or exactly-once work.
use crate::{task_actor::TaskActor, task_delivery::{CancelResult, DeliveryIdentity, TaskDelivery}};
use crate::protocol::{
    ContentData, ContentType, ProtocolTimestamp, UnifiedRequest, UnifiedResponse,
};
use chrono::{NaiveDateTime, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;

pub type TaskFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, TaskError>> + Send + 'a>>;
pub type SleepFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Processing,
    Completed,
    Failed,
    Cancelled,
}
impl TaskStatus {
    /// Generic task states only; never apply this graph to managed or SDK runs.
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending, Self::Processing | Self::Cancelled)
                | (
                    Self::Processing,
                    Self::Completed | Self::Failed | Self::Cancelled
                )
        )
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// A response-shaped JSON dictionary is not an actual UnifiedResponse in Python.
/// Deserializing JSON deliberately does not promote it into the Response variant.
#[derive(Clone, Debug, PartialEq)]
pub enum TaskResult {
    Response(Box<UnifiedResponse>),
    Json(Value),
}
impl Serialize for TaskResult {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Response(value) => value.serialize(serializer),
            Self::Json(value) => value.serialize(serializer),
        }
    }
}
impl<'de> Deserialize<'de> for TaskResult {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Value::deserialize(deserializer).map(Self::Json)
    }
}
impl From<UnifiedResponse> for TaskResult {
    fn from(value: UnifiedResponse) -> Self {
        Self::Response(Box::new(value))
    }
}
impl From<Value> for TaskResult {
    fn from(value: Value) -> Self {
        Self::Json(value)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Task {
    pub task_id: String,
    pub request_id: String,
    pub service_id: String,
    pub status: TaskStatus,
    #[serde(default = "task_now")]
    pub created_at: ProtocolTimestamp,
    #[serde(default)]
    pub updated_at: Option<ProtocolTimestamp>,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub callback_url: Option<String>,
    #[serde(default)]
    pub result: Option<TaskResult>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub progress: f64,
    #[serde(default)]
    pub request_data: Option<Map<String, Value>>,
    #[serde(default)]
    pub callback_sent: bool,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub retry_count: i64,
    #[serde(default = "default_max_retries")]
    pub max_retries: i64,
    #[serde(default = "default_metadata")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(default)]
    pub started_at: Option<ProtocolTimestamp>,
    #[serde(default)]
    pub completed_at: Option<ProtocolTimestamp>,
}
fn task_now() -> ProtocolTimestamp {
    SystemTaskRuntime.now().into()
}
fn default_max_retries() -> i64 {
    3
}
fn default_metadata() -> Option<Map<String, Value>> {
    Some(Map::new())
}
impl Task {
    pub fn new(task_id: String, request: &UnifiedRequest, created_at: NaiveDateTime) -> Self {
        Self {
            task_id,
            request_id: request.request_id.clone(),
            service_id: request.service_id.clone(),
            status: TaskStatus::Pending,
            created_at: created_at.into(),
            updated_at: None,
            user_id: request.user_id.clone(),
            tenant_id: request.tenant_id.clone(),
            callback_url: request.callback_url.clone(),
            result: None,
            error: None,
            progress: 0.0,
            request_data: None,
            callback_sent: false,
            priority: 0,
            retry_count: 0,
            max_retries: 3,
            metadata: Some(Map::new()),
            started_at: None,
            completed_at: None,
        }
    }
}

/// Sanitized collaborator failures. Never place callback URLs, request bodies,
/// connection strings or provider diagnostics in these public error values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskError {
    NotFound(String),
    StorageUnavailable,
    QueueUnavailable,
    StateConflict,
    InvalidEnvelope,
    InvalidUtf8,
    DurableAuthorityRequired,
    AdmissionUnconfirmed(String),
}
impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(id) => write!(f, "Task not found: {id}"),
            Self::StorageUnavailable => f.write_str("task_storage_unavailable"),
            Self::QueueUnavailable => f.write_str("task_queue_unavailable"),
            Self::StateConflict => f.write_str("task_state_conflict"),
            Self::InvalidEnvelope => f.write_str("invalid_task_envelope"),
            Self::InvalidUtf8 => f.write_str("task_data_not_utf8"),
            Self::DurableAuthorityRequired => f.write_str("task_durable_session_authority_required"),
            Self::AdmissionUnconfirmed(id) => write!(f,"task_admission_unconfirmed:{id}"),
        }
    }
}
impl std::error::Error for TaskError {}

#[derive(Clone, Debug)]
pub struct TaskListFilter {
    pub user_id: Option<String>,
    pub tenant_id: Option<String>,
    pub service_id: Option<String>,
    pub status: Option<String>,
    pub limit: usize,
    pub offset: usize,
}
impl Default for TaskListFilter {
    fn default() -> Self {
        Self {
            user_id: None,
            tenant_id: None,
            service_id: None,
            status: None,
            limit: 100,
            offset: 0,
        }
    }
}
pub trait TaskStorage: Send + Sync {
    fn save<'a>(&'a self, task: &'a Task) -> TaskFuture<'a, ()>;
    /// Atomically replace lifecycle fields only if the stored status and immutable
    /// identity still match. No get/save fallback: unsupported stores fail closed.
    fn compare_status<'a>(
        &'a self,
        _task: &'a Task,
        _expected: TaskStatus,
    ) -> TaskFuture<'a, bool> {
        Box::pin(async { Err(TaskError::StorageUnavailable) })
    }
    fn get<'a>(&'a self, task_id: &'a str) -> TaskFuture<'a, Task>;
    fn list<'a>(&'a self, filter: &'a TaskListFilter) -> TaskFuture<'a, Vec<Task>>;
}
#[derive(Default)]
pub struct MemoryTaskStorage {
    tasks: Mutex<HashMap<String, Task>>,
}
impl MemoryTaskStorage {
    pub fn new() -> Self {
        Self::default()
    }
}
impl TaskStorage for MemoryTaskStorage {
    fn save<'a>(&'a self, task: &'a Task) -> TaskFuture<'a, ()> {
        Box::pin(async move {
            self.tasks
                .lock()
                .map_err(|_| TaskError::StorageUnavailable)?
                .insert(task.task_id.clone(), task.clone());
            Ok(())
        })
    }
    fn compare_status<'a>(&'a self, task: &'a Task, expected: TaskStatus) -> TaskFuture<'a, bool> {
        Box::pin(async move {
            if !expected.can_transition_to(task.status) {
                return Err(TaskError::StateConflict);
            }
            let mut tasks = self
                .tasks
                .lock()
                .map_err(|_| TaskError::StorageUnavailable)?;
            let Some(stored) = tasks.get_mut(&task.task_id) else {
                return Ok(false);
            };
            if stored.status != expected
                || stored.request_id != task.request_id
                || stored.service_id != task.service_id
                || stored.user_id != task.user_id
                || stored.tenant_id != task.tenant_id
            {
                return Ok(false);
            }
            stored.status = task.status;
            stored.updated_at = task.updated_at;
            stored.started_at = task.started_at;
            stored.completed_at = task.completed_at;
            stored.result = task.result.clone();
            stored.error = task.error.clone();
            Ok(true)
        })
    }
    fn get<'a>(&'a self, task_id: &'a str) -> TaskFuture<'a, Task> {
        Box::pin(async move {
            self.tasks
                .lock()
                .map_err(|_| TaskError::StorageUnavailable)?
                .get(task_id)
                .cloned()
                .ok_or_else(|| TaskError::NotFound(task_id.into()))
        })
    }
    fn list<'a>(&'a self, filter: &'a TaskListFilter) -> TaskFuture<'a, Vec<Task>> {
        Box::pin(async move {
            let matches = |filter: &Option<String>, value: &str| {
                filter
                    .as_deref()
                    .is_none_or(|filter| filter.is_empty() || filter == value)
            };
            // Keep one coherent snapshot while sorting borrowed rows. Clone only
            // the requested page, including its complete result and bookkeeping.
            // No await occurs while the guard is held.
            let stored = self.tasks.lock().map_err(|_| TaskError::StorageUnavailable)?;
            let mut tasks: Vec<_> = stored
                .values()
                .filter(|task| {
                    matches(&filter.user_id, &task.user_id)
                        && matches(&filter.tenant_id, &task.tenant_id)
                        && matches(&filter.service_id, &task.service_id)
                        && matches(&filter.status, task.status.as_str())
                })
                .collect();
            // Python datetime sorting raises TypeError for mixed naive/aware
            // values, even if either group would otherwise sort successfully.
            // Preserve that failure rather than inventing an implicit timezone.
            if let Some(first) = tasks.first() {
                let naive = matches!(first.created_at, ProtocolTimestamp::Naive(_));
                if tasks
                    .iter()
                    .any(|task| matches!(task.created_at, ProtocolTimestamp::Naive(_)) != naive)
                {
                    return Err(TaskError::StorageUnavailable);
                }
            }
            tasks.sort_by(|a, b| {
                (b.created_at.naive_utc(), &b.task_id).cmp(&(a.created_at.naive_utc(), &a.task_id))
            });
            Ok(tasks
                .into_iter()
                .skip(filter.offset)
                .take(filter.limit)
                .cloned()
                .collect())
        })
    }
}

pub trait TaskRuntime: Send + Sync {
    fn now(&self) -> NaiveDateTime;
    fn new_id(&self) -> String;
    fn sleep(&self, delay: Duration) -> SleepFuture<'_>;
}
pub struct SystemTaskRuntime;
impl TaskRuntime for SystemTaskRuntime {
    fn now(&self) -> NaiveDateTime {
        let now = Utc::now().naive_utc();
        now.with_nanosecond(now.nanosecond() / 1000 * 1000)
            .expect("microsecond timestamp")
    }
    fn new_id(&self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
    fn sleep(&self, delay: Duration) -> SleepFuture<'_> {
        Box::pin(tokio::time::sleep(delay))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct TaskEnvelope {
    pub task_id: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub client_ip: Option<String>,
    pub request: UnifiedRequest,
}
pub fn serialize_task_envelope(envelope: &TaskEnvelope) -> Result<String, TaskError> {
    let mut encoded = envelope.clone();
    for content in &mut encoded.request.inputs {
        if let Some(ContentData::Bytes(bytes)) = &content.data {
            content.data = Some(ContentData::Text(
                std::str::from_utf8(bytes)
                    .map_err(|_| TaskError::InvalidUtf8)?
                    .to_owned(),
            ));
        }
    }
    // Preserve insertion order and Python json.dumps' spaces: Redis members are
    // compared lexically on equal scores and deduplicated by exact member bytes.
    let compact = serde_json::to_string(&encoded).map_err(|_| TaskError::InvalidEnvelope)?;
    let mut spaced = String::with_capacity(compact.len());
    let (mut quoted, mut escaped) = (false, false);
    for ch in compact.chars() {
        spaced.push(ch);
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        } else if !quoted && matches!(ch, ',' | ':') {
            spaced.push(' ');
        }
    }
    Ok(spaced)
}
pub fn deserialize_task_envelope(raw: &str, now: NaiveDateTime) -> Result<TaskEnvelope, TaskError> {
    let mut payload: Value = serde_json::from_str(raw).map_err(|_| TaskError::InvalidEnvelope)?;
    let req = payload
        .get_mut("request")
        .and_then(Value::as_object_mut)
        .ok_or(TaskError::InvalidEnvelope)?;
    let inputs = req
        .entry("inputs")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or(TaskError::InvalidEnvelope)?;
    for input in inputs {
        let object = input.as_object_mut().ok_or(TaskError::InvalidEnvelope)?;
        let content_type = object
            .get("type")
            .cloned()
            .and_then(|value| serde_json::from_value::<ContentType>(value).ok())
            .unwrap_or(ContentType::Text);
        object.insert("type".into(), content_type.as_str().into());
    }
    let timestamp = req.get("timestamp").and_then(Value::as_str).unwrap_or("");
    let timestamp = ProtocolTimestamp::parse_isoformat(timestamp).unwrap_or_else(|_| now.into());
    req.insert(
        "timestamp".into(),
        serde_json::to_value(timestamp).map_err(|_| TaskError::InvalidEnvelope)?,
    );
    let priority = match req.get("priority") {
        None => 0,
        Some(Value::Bool(value)) => i64::from(*value),
        Some(Value::String(value)) => {
            let value = value.trim();
            let bytes = value.as_bytes();
            if bytes.iter().enumerate().any(|(index, byte)| {
                *byte == b'_'
                    && (index == 0
                        || !bytes[index - 1].is_ascii_digit()
                        || !bytes.get(index + 1).is_some_and(u8::is_ascii_digit))
            }) {
                return Err(TaskError::InvalidEnvelope);
            }
            value
                .replace('_', "")
                .parse()
                .map_err(|_| TaskError::InvalidEnvelope)?
        }
        Some(value) if value.as_i64().is_some() => value.as_i64().expect("checked integer"),
        Some(Value::Number(value)) => {
            let number = value.as_f64().ok_or(TaskError::InvalidEnvelope)?;
            if !number.is_finite() || number < i64::MIN as f64 || number >= -(i64::MIN as f64) {
                return Err(TaskError::InvalidEnvelope);
            }
            number.trunc() as i64
        }
        _ => return Err(TaskError::InvalidEnvelope),
    };
    req.insert("priority".into(), priority.into());
    let input_values = req
        .get("inputs")
        .and_then(Value::as_array)
        .expect("validated array")
        .iter()
        .map(|item| item.get("data").cloned())
        .collect::<Vec<_>>();
    let mut envelope: TaskEnvelope =
        serde_json::from_value(payload).map_err(|_| TaskError::InvalidEnvelope)?;
    // This format has no binary marker: bytes were already UTF-8 decoded on
    // enqueue. A JSON array from Python must not be reinterpreted as bytes by
    // ContentData's broader, untagged domain deserializer.
    for (content, raw) in envelope.request.inputs.iter_mut().zip(input_values) {
        if let Some(value) = raw.filter(|value| !value.is_null() && !value.is_string()) {
            content.data = Some(ContentData::Json(value));
        }
    }
    Ok(envelope)
}

pub trait TaskQueue: Send + Sync {
    fn enqueue(&self, envelope: TaskEnvelope) -> TaskFuture<'_, ()>;
    fn dequeue(&self) -> TaskFuture<'_, TaskEnvelope>;
    /// Default mirrors a fallback without Python's inspectable `_queue` member.
    fn try_dequeue(&self) -> TaskFuture<'_, Option<TaskEnvelope>> {
        Box::pin(async { Ok(None) })
    }
}
#[derive(Default)]
pub struct MemoryTaskQueue {
    queue: Mutex<Vec<TaskEnvelope>>,
    ready: Notify,
}
impl MemoryTaskQueue {
    pub fn new() -> Self {
        Self::default()
    }
}
impl TaskQueue for MemoryTaskQueue {
    fn enqueue(&self, envelope: TaskEnvelope) -> TaskFuture<'_, ()> {
        Box::pin(async move {
            self.queue
                .lock()
                .map_err(|_| TaskError::QueueUnavailable)?
                .push(envelope);
            self.ready.notify_one();
            Ok(())
        })
    }
    fn try_dequeue(&self) -> TaskFuture<'_, Option<TaskEnvelope>> {
        Box::pin(async move {
            let mut queue = self.queue.lock().map_err(|_| TaskError::QueueUnavailable)?;
            let index = queue
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    b.request
                        .priority
                        .cmp(&a.request.priority)
                        .then(a.task_id.cmp(&b.task_id))
                })
                .map(|(index, _)| index);
            Ok(index.map(|index| queue.remove(index)))
        })
    }
    fn dequeue(&self) -> TaskFuture<'_, TaskEnvelope> {
        Box::pin(async move {
            loop {
                let notified = self.ready.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if let Some(envelope) = self.try_dequeue().await? {
                    return Ok(envelope);
                }
                notified.await;
            }
        })
    }
}

pub trait DurablePriorityQueue: Send + Sync {
    fn zadd<'a>(&'a self, key: &'a str, member: String, score: f64) -> TaskFuture<'a, ()>;
    fn zpopmin<'a>(&'a self, key: &'a str) -> TaskFuture<'a, Option<String>>;
}
/// Concrete Redis commands, using an already configured connection. This type
/// never opens a listener, selects credentials, or constructs a connection URL.
pub struct RedisPriorityStore {
    connection: redis::aio::ConnectionManager,
}
impl RedisPriorityStore {
    pub fn new(connection: redis::aio::ConnectionManager) -> Self {
        Self { connection }
    }
}
impl DurablePriorityQueue for RedisPriorityStore {
    fn zadd<'a>(&'a self, key: &'a str, member: String, score: f64) -> TaskFuture<'a, ()> {
        Box::pin(async move {
            redis::cmd("ZADD")
                .arg(key)
                .arg(score)
                .arg(member)
                .query_async::<usize>(&mut self.connection.clone())
                .await
                .map(|_| ())
                .map_err(|_| TaskError::QueueUnavailable)
        })
    }
    fn zpopmin<'a>(&'a self, key: &'a str) -> TaskFuture<'a, Option<String>> {
        Box::pin(async move {
            let members: Vec<(String, f64)> = redis::cmd("ZPOPMIN")
                .arg(key)
                .arg(1)
                .query_async(&mut self.connection.clone())
                .await
                .map_err(|_| TaskError::QueueUnavailable)?;
            Ok(members.into_iter().next().map(|(member, _)| member))
        })
    }
}
pub struct RedisTaskQueue {
    pub queue_key: String,
    redis: Option<Arc<dyn DurablePriorityQueue>>,
    fallback: Arc<dyn TaskQueue>,
    runtime: Arc<dyn TaskRuntime>,
}
impl RedisTaskQueue {
    pub fn new(redis: Option<Arc<dyn DurablePriorityQueue>>, fallback: Arc<dyn TaskQueue>) -> Self {
        Self {
            queue_key: "gateway:tasks:priority_queue".into(),
            redis,
            fallback,
            runtime: Arc::new(SystemTaskRuntime),
        }
    }
    pub fn with_runtime(mut self, runtime: Arc<dyn TaskRuntime>) -> Self {
        self.runtime = runtime;
        self
    }
}
impl TaskQueue for RedisTaskQueue {
    fn enqueue(&self, envelope: TaskEnvelope) -> TaskFuture<'_, ()> {
        Box::pin(async move {
            if let Some(redis) = &self.redis {
                // Python serializes before entering its Redis try/except.
                let member = serialize_task_envelope(&envelope)?;
                if redis
                    .zadd(&self.queue_key, member, -(envelope.request.priority as f64))
                    .await
                    .is_ok()
                {
                    return Ok(());
                }
            }
            self.fallback.enqueue(envelope).await
        })
    }
    fn dequeue(&self) -> TaskFuture<'_, TaskEnvelope> {
        Box::pin(async move {
            if let Some(redis) = &self.redis {
                loop {
                    match redis.zpopmin(&self.queue_key).await {
                        Ok(Some(raw)) => {
                            match deserialize_task_envelope(&raw, self.runtime.now()) {
                                Ok(envelope) => return Ok(envelope),
                                Err(_) => break,
                            }
                        }
                        Ok(None) => match self.fallback.try_dequeue().await {
                            Ok(Some(envelope)) => return Ok(envelope),
                            Ok(None) => self.runtime.sleep(Duration::from_millis(100)).await,
                            Err(_) => break,
                        },
                        Err(_) => break,
                    }
                }
            }
            self.fallback.dequeue().await
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallbackGuard {
    Unconfigured,
    UnsafeDestination,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallbackFailure {
    Guard(CallbackGuard),
    Timeout,
    HttpStatus(u16),
    Serialization,
    Other,
}
pub type CallbackFuture<'a> =
    Pin<Box<dyn Future<Output = Result<u16, CallbackFailure>> + Send + 'a>>;
#[derive(Clone, Debug, PartialEq)]
pub struct CallbackPayload {
    pub task_id: String,
    pub status: TaskStatus,
    pub result: Option<TaskResult>,
    pub error: Option<String>,
}
impl CallbackPayload {
    /// Canonical public response JSON also handles binary outputs. Safe
    /// transports must still validate and pin destinations before posting.
    pub fn to_json(&self) -> Result<Value, CallbackFailure> {
        let result = match &self.result {
            Some(TaskResult::Response(response)) => response.to_wire_value(),
            Some(TaskResult::Json(value)) => value.clone(),
            None => Value::Null,
        };
        Ok(json!({"task_id": self.task_id, "status": self.status, "result": result, "error": self.error}))
    }
}
/// A trusted composition boundary, not a generic HTTP adapter. Implementations
/// must validate scheme/host/all resolved addresses and pin approved DNS before
/// posting; never follow redirects. SafeFetch-like guard failures must remain
/// Guard, never be downgraded to a retryable network failure. Python's active
/// safe_callback_post uses its default 10 s timeout, not callback_timeout (30 s).
/// Implementations must not perform a POST if payload.to_json() fails.
pub trait SafeCallbackTransport: Send + Sync {
    fn post<'a>(&'a self, url: &'a str, payload: &'a CallbackPayload) -> CallbackFuture<'a>;
}
pub struct UnconfiguredCallbackTransport;
impl SafeCallbackTransport for UnconfiguredCallbackTransport {
    fn post<'a>(&'a self, _url: &'a str, _payload: &'a CallbackPayload) -> CallbackFuture<'a> {
        Box::pin(async { Err(CallbackFailure::Guard(CallbackGuard::Unconfigured)) })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallbackOutcome {
    NotRequested,
    Delivered,
    Refused(CallbackGuard),
    Exhausted(Option<CallbackFailure>),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallbackReport {
    pub attempts: u64,
    pub outcome: CallbackOutcome,
}
#[derive(Clone, Debug)]
pub struct CallbackOptions {
    pub callback_timeout: Duration,
    pub retries: i64,
    pub retry_delay: Duration,
}
impl Default for CallbackOptions {
    fn default() -> Self {
        Self {
            callback_timeout: Duration::from_secs(30),
            retries: 3,
            retry_delay: Duration::from_secs(1),
        }
    }
}

pub struct TaskManager {
    storage: Arc<dyn TaskStorage>,
    queue: Option<Arc<dyn TaskQueue>>,
    delivery: Option<Arc<dyn TaskDelivery>>,
    runtime: Arc<dyn TaskRuntime>,
    callback: Arc<dyn SafeCallbackTransport>,
    pub callback_options: CallbackOptions,
}
impl TaskManager {
    pub fn new(storage: Arc<dyn TaskStorage>, queue: Arc<dyn TaskQueue>) -> Self {
        Self {
            storage,
            queue: Some(queue),
            delivery: None,
            runtime: Arc::new(SystemTaskRuntime),
            callback: Arc::new(UnconfiguredCallbackTransport),
            callback_options: CallbackOptions::default(),
        }
    }
    pub fn durable(storage: Arc<dyn TaskStorage>, delivery: Arc<dyn TaskDelivery>) -> Self {
        Self { storage, queue: None, delivery: Some(delivery), runtime: Arc::new(SystemTaskRuntime),
            callback: Arc::new(UnconfiguredCallbackTransport), callback_options: CallbackOptions::default() }
    }
    pub(crate) fn delivery(&self) -> Option<Arc<dyn TaskDelivery>> { self.delivery.clone() }
    pub fn requires_durable_actor(&self) -> bool { self.delivery.is_some() }
    pub async fn drain_cancelled_deliveries(&self) -> Result<(), TaskError> {
        if let Some(delivery) = &self.delivery {
            delivery.drain_cancelled().await.map_err(|_|TaskError::StorageUnavailable)?;
        }
        Ok(())
    }
    pub fn with_runtime(mut self, runtime: Arc<dyn TaskRuntime>) -> Self {
        self.runtime = runtime;
        self
    }
    pub fn with_safe_callback_transport(
        mut self,
        callback: Arc<dyn SafeCallbackTransport>,
    ) -> Self {
        self.callback = callback;
        self
    }
    pub async fn create_task(
        &self,
        request: UnifiedRequest,
        roles: Vec<String>,
        client_ip: Option<String>,
    ) -> Result<Task, TaskError> {
        self.create_task_with_actor(request, roles, client_ip, None).await
    }
    pub async fn create_task_with_actor(
        &self, request: UnifiedRequest, roles: Vec<String>, client_ip: Option<String>, actor: Option<TaskActor>,
    ) -> Result<Task, TaskError> {
        if self.delivery.is_some() && actor.as_ref().is_none_or(|value| !value.valid()) {
            return Err(TaskError::DurableAuthorityRequired);
        }
        let task = Task::new(self.runtime.new_id(), &request, self.runtime.now());
        let envelope = TaskEnvelope { task_id: task.task_id.clone(), request, roles, client_ip };
        if let Some(delivery) = &self.delivery {
            let actor = actor.ok_or(TaskError::DurableAuthorityRequired)?;
            return delivery.publish(envelope, actor).await.map_err(|error| match error {
                crate::task_delivery::DeliveryError::InvalidEnvelope => TaskError::InvalidEnvelope,
                // A timeout or lost COMMIT response may have admitted the task.
                // Return its original stable ID and never try a queue fallback.
                _ => TaskError::AdmissionUnconfirmed(task.task_id),
            });
        }
        self.storage.save(&task).await?;
        self.queue.as_ref().ok_or(TaskError::QueueUnavailable)?.enqueue(envelope).await?;
        Ok(task)
    }
    pub async fn get_task(&self, task_id: &str) -> Result<Task, TaskError> {
        self.storage.get(task_id).await
    }
    pub async fn list_tasks(&self, filter: &TaskListFilter) -> Result<Vec<Task>, TaskError> {
        self.storage.list(filter).await
    }
    pub async fn update_status(
        &self,
        task_id: &str,
        status: TaskStatus,
        result: Option<TaskResult>,
        error: Option<String>,
    ) -> Result<(), TaskError> {
        if self
            .transition_status(task_id, status, result, error)
            .await?
        {
            Ok(())
        } else {
            Err(TaskError::StateConflict)
        }
    }

    /// A false result means another transition won or the state is terminal.
    /// Dispatch only after a successful pending -> processing transition. Never
    /// retry invocation after an error or unknown outcome from this method.
    pub async fn transition_status(
        &self,
        task_id: &str,
        status: TaskStatus,
        result: Option<TaskResult>,
        error: Option<String>,
    ) -> Result<bool, TaskError> {
        if self.delivery.is_some() { return Err(TaskError::StateConflict); }
        let task = self.storage.get(task_id).await?;
        self.transition_task(task, status, result, error).await
    }

    /// Bind a queued invocation to the exact persisted task before claiming it.
    /// CAS uses this same snapshot, not a fresh identity-only read after validation.
    pub async fn claim_task(&self, envelope: &TaskEnvelope) -> Result<bool, TaskError> {
        if self.delivery.is_some() { return Err(TaskError::StateConflict); }
        let task = self.storage.get(&envelope.task_id).await?;
        let request = &envelope.request;
        if task.request_id != request.request_id
            || task.service_id != request.service_id
            || task.user_id != request.user_id
            || task.tenant_id != request.tenant_id
        {
            return Err(TaskError::InvalidEnvelope);
        }
        self.transition_task(task, TaskStatus::Processing, None, None).await
    }

    async fn transition_task(
        &self,
        mut task: Task,
        status: TaskStatus,
        result: Option<TaskResult>,
        error: Option<String>,
    ) -> Result<bool, TaskError> {
        let expected = task.status;
        if !expected.can_transition_to(status) {
            return Ok(false);
        }
        task.status = status;
        let now = self.runtime.now().into();
        task.updated_at = Some(now);
        if status == TaskStatus::Processing {
            task.started_at = Some(now);
        } else {
            task.completed_at = Some(now);
        }
        if let Some(result) =
            result.filter(|result| !matches!(result, TaskResult::Json(Value::Null)))
        {
            task.result = Some(result);
        }
        if let Some(error) = error {
            task.error = Some(error);
        }
        if !self.storage.compare_status(&task, expected).await? {
            return Ok(false);
        }
        if matches!(status, TaskStatus::Completed | TaskStatus::Failed) {
            self.send_callback(&task).await;
        }
        Ok(true)
    }
    pub async fn cancel(&self, task_id: &str) -> Result<(), TaskError> {
        if let Some(delivery) = &self.delivery {
            let task = self.storage.get(task_id).await?;
            let identity = DeliveryIdentity { task_id:task.task_id, request_id:task.request_id,
                service_id:task.service_id, user_id:task.user_id, tenant_id:task.tenant_id };
            match delivery.cancel(&identity).await.map_err(|_|TaskError::StorageUnavailable)? {
                CancelResult::Applied | CancelResult::AlreadyCancelled => return Ok(()),
                CancelResult::TerminalConflict | CancelResult::ProjectionConflict => return Err(TaskError::StateConflict),
                // Pre-cutover rows have no executable ledger. Cancellation is
                // allowed; importing/replaying those envelopes is never allowed.
                CancelResult::NotManaged => {},
            }
        }
        loop {
            let task = self.storage.get(task_id).await?;
            match task.status {
                TaskStatus::Cancelled => return Ok(()),
                TaskStatus::Completed | TaskStatus::Failed => return Err(TaskError::StateConflict),
                TaskStatus::Pending | TaskStatus::Processing => {}
            }
            // A concurrent pending -> processing claim is harmless: reload and
            // cancel its current state. A winning terminal transition is kept.
            if self
                .transition_task(task, TaskStatus::Cancelled, None, None)
                .await?
            {
                return Ok(());
            }
        }
    }
    pub async fn get_result(&self, task_id: &str) -> Result<UnifiedResponse, TaskError> {
        match self.storage.get(task_id).await?.result {
            Some(TaskResult::Response(response)) => Ok(*response),
            _ => Err(TaskError::NotFound(task_id.into())),
        }
    }
    pub async fn send_callback(&self, task: &Task) -> CallbackReport {
        let Some(url) = task.callback_url.as_deref().filter(|url| !url.is_empty()) else {
            return CallbackReport {
                attempts: 0,
                outcome: CallbackOutcome::NotRequested,
            };
        };
        let payload = CallbackPayload {
            task_id: task.task_id.clone(),
            status: task.status,
            result: task.result.clone(),
            error: task.error.clone(),
        };
        let mut attempts = 0;
        let mut last = None;
        let mut delay = self.callback_options.retry_delay;
        for attempt in 0..self.callback_options.retries {
            attempts += 1;
            let outcome = self.callback.post(url, &payload).await;
            last = Some(match outcome {
                Ok(status) if (200..300).contains(&status) => {
                    return CallbackReport {
                        attempts,
                        outcome: CallbackOutcome::Delivered,
                    };
                }
                Ok(status) => CallbackFailure::HttpStatus(status),
                Err(CallbackFailure::Guard(guard)) => {
                    return CallbackReport {
                        attempts,
                        outcome: CallbackOutcome::Refused(guard),
                    };
                }
                Err(failure) => failure,
            });
            if attempt < self.callback_options.retries - 1 {
                self.runtime.sleep(delay).await;
                delay = delay.saturating_mul(2);
            }
        }
        CallbackReport {
            attempts,
            outcome: CallbackOutcome::Exhausted(last),
        }
    }
    /// No shared HTTP client is owned here; injected transports own their resources.
    pub async fn close(&self) {}
}

#[cfg(test)]
#[path = "task_manager_tests.rs"]
mod tests;
