//! Redis Streams connector port of src/transports/message_queue.py.
//! Native durability upgrade: fenced atomic settlement, bounded PEL recovery and
//! admission, graceful drain. Delivery remains at-least-once; handlers must be idempotent.
use crate::{
    dispatcher::AdapterFailure,
    generic_rest_adapter::{
        RestMappingError, RestRequest, RestResponse, RestTransport, RestTransportFuture,
    },
    quiz_grading::python_str,
    service_ownership::ServiceHandle,
    service_registry::python_truthy,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, RwLock, Semaphore};
mod redis_backend;
use redis_backend::RedisBackend;
pub type QueueFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, QueueError>> + Send + 'a>>;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueError {
    InvalidOperation,
    InvalidValue,
    Backend,
    Backpressure,
    StaleDelivery,
    Draining,
    DrainTimedOut,
}
#[derive(Clone, Debug, PartialEq)]
pub struct QueueMessage {
    pub id: String,
    pub fields: HashMap<String, String>,
    pub deliveries: i64,
}
pub trait QueueBackend: Send + Sync {
    fn ensure_group<'a>(&'a self, stream: &'a str, group: &'a str) -> QueueFuture<'a, ()>;
    fn add<'a>(
        &'a self,
        stream: &'a str,
        fields: &'a [(String, String)],
        max_len: usize,
    ) -> QueueFuture<'a, String>;
    fn read<'a>(
        &'a self,
        stream: &'a str,
        group: &'a str,
        consumer: &'a str,
        idle_ms: u64,
        excluded: &'a [String],
    ) -> QueueFuture<'a, Option<QueueMessage>>;
    /// Must atomically validate owner/delivery count, optionally append, and acknowledge.
    /// Return 0 for already settled, StaleDelivery for a reclaimed lease, Backpressure
    /// without acknowledging when the destination cannot accept the retry/dead letter.
    fn settle<'a>(
        &'a self,
        stream: &'a str,
        group: &'a str,
        id: &'a str,
        consumer: &'a str,
        deliveries: i64,
        target: Option<(&'a str, &'a [(String, String)])>,
        max_len: usize,
    ) -> QueueFuture<'a, i64>;
    fn inspect<'a>(
        &'a self,
        stream: &'a str,
        group: &'a str,
        id: &'a str,
        consumer: &'a str,
        deliveries: i64,
        fields: bool,
    ) -> QueueFuture<'a, Option<HashMap<String, String>>>;
    fn close(&self) -> QueueFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryReceipt {
    version: u8,
    service: String,
    group: String,
    stream: String,
    id: String,
    consumer: String,
    deliveries: i64,
}
#[derive(Clone)]
struct Cached {
    fields: HashMap<String, String>,
    consumer: String,
    deliveries: i64,
    _permit: Option<Arc<OwnedSemaphorePermit>>,
    attempts: i64,
    original: String,
}
pub struct MessageQueueTransport {
    backend: Mutex<(Option<Arc<dyn QueueBackend>>, bool)>,
    redis_url: String,
    streams: Value,
    group: String,
    max_retries: i64,
    block_ms: i64,
    messages: Mutex<HashMap<(String, String), Cached>>,
    max_len: usize,
    max_payload: usize,
    idle_ms: u64,
    drain_ms: u64,
    instance: String,
    service_id: String,
    reap_cursor: AtomicUsize,
    slots: Arc<Semaphore>,
    draining: AtomicBool,
    drain_notify: Notify,
    operations: RwLock<()>,
}
fn integer(v: &Value) -> Result<i64, QueueError> {
    match v {
        Value::Bool(v) => Ok(i64::from(*v)),
        Value::Number(v) => v
            .as_i64()
            .or_else(|| {
                v.as_f64()
                    .filter(|n| n.is_finite() && *n >= i64::MIN as f64 && *n < i64::MAX as f64)
                    .map(|n| n as i64)
            })
            .ok_or(QueueError::InvalidValue),
        Value::String(v) => v.trim().parse().map_err(|_| QueueError::InvalidValue),
        _ => Err(QueueError::InvalidValue),
    }
}
fn string_or(v: &Value, default: &str) -> String {
    if python_truthy(v) {
        python_str(v)
    } else {
        default.into()
    }
}
fn compact(v: &Value) -> String {
    match v {
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, v)| format!(
                    "{}:{}",
                    crate::python_json::python_json(&json!(k), true, false),
                    compact(v)
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Array(a) => format!("[{}]", a.iter().map(compact).collect::<Vec<_>>().join(",")),
        _ => crate::python_json::python_json(v, true, false),
    }
}
impl MessageQueueTransport {
    pub fn new_handle(service: &ServiceHandle) -> Result<Self, RestMappingError> {
        Self::configured(service, None, true)
    }
    pub fn with_backend(
        service: &ServiceHandle,
        backend: Arc<dyn QueueBackend>,
        owns_backend: bool,
    ) -> Result<Self, RestMappingError> {
        Self::configured(service, Some(backend), owns_backend)
    }
    fn configured(
        service: &ServiceHandle,
        backend: Option<Arc<dyn QueueBackend>>,
        owns_backend: bool,
    ) -> Result<Self, RestMappingError> {
        let config = service.connector().snapshot();
        let invalid = || RestMappingError::InvalidConfig("invalid_message_queue_config");
        let streams = config
            .get("streams")
            .filter(|v| python_truthy(v))
            .cloned()
            .unwrap_or(json!({}));
        if !streams.is_object() {
            return Err(invalid());
        }
        let positive = |name: &str, default: i64, maximum: i64| -> Result<i64, RestMappingError> {
            let n = config
                .get(name)
                .map(integer)
                .transpose()
                .map_err(|_| invalid())?
                .unwrap_or(default);
            if n < 1 || n > maximum {
                return Err(invalid());
            }
            Ok(n)
        };
        Ok(Self {
            max_len: positive("max_queue_length", 10000, 1000000)? as usize,
            max_payload: positive("max_payload_bytes", 1048576, 16777216)? as usize,
            idle_ms: positive("reclaim_idle_ms", 60000, 86400000)? as u64,
            drain_ms: positive("drain_timeout_ms", 30000, 300000)? as u64,
            instance: uuid::Uuid::new_v4().to_string(),
            service_id: service.service_id().to_owned(),
            reap_cursor: AtomicUsize::new(0),
            slots: Arc::new(Semaphore::new(positive("max_inflight", 128, 4096)? as usize)),
            draining: AtomicBool::new(false),
            drain_notify: Notify::new(),
            operations: RwLock::new(()),
            backend: Mutex::new((backend, owns_backend)),
            redis_url: string_or(&config["redis_url"], "redis://localhost:6379/0"),
            streams,
            group: string_or(
                &config["group"],
                &format!("{}:workers", service.service_id()),
            ),
            max_retries: config
                .get("max_retries")
                .map(integer)
                .transpose()
                .map_err(|_| invalid())?
                .unwrap_or(3),
            block_ms: config
                .get("block_ms")
                .map(integer)
                .transpose()
                .map_err(|_| invalid())?
                .unwrap_or(0)
                .max(0),
            messages: Mutex::new(HashMap::new()),
        })
    }
    async fn backend(&self) -> Result<Arc<dyn QueueBackend>, QueueError> {
        let mut slot = self.backend.lock().await;
        if slot.0.is_none() {
            let client = redis::Client::open(self.redis_url.as_str())
                .map_err(|_| QueueError::InvalidValue)?;
            slot.0 = Some(Arc::new(RedisBackend::new(client)));
            slot.1 = true;
        }
        Ok(slot.0.as_ref().unwrap().clone())
    }
    pub fn stream_for(&self, priority: &str) -> String {
        let value = self
            .streams
            .get(priority)
            .filter(|v| python_truthy(v))
            .unwrap_or(&self.streams["normal"]);
        string_or(value, "queue:normal")
    }
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::Acquire)
    }
    async fn reap_settled(&self) -> Result<(), QueueError> {
        let cached: Vec<_> = self
            .messages
            .lock()
            .await
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if cached.is_empty() {
            return Ok(());
        }
        let start = self.reap_cursor.fetch_add(32, Ordering::Relaxed) % cached.len();
        let backend = self.backend().await?;
        for i in 0..cached.len().min(32) {
            let ((stream, id), lease) = &cached[(start + i) % cached.len()];
            let state = backend
                .inspect(
                    stream,
                    &self.group,
                    id,
                    &lease.consumer,
                    lease.deliveries,
                    false,
                )
                .await;
            if matches!(state, Ok(None) | Err(QueueError::StaleDelivery)) {
                let mut map = self.messages.lock().await;
                if map
                    .get(&(stream.clone(), id.clone()))
                    .is_some_and(|current| {
                        current.consumer == lease.consumer && current.deliveries == lease.deliveries
                    })
                {
                    map.remove(&(stream.clone(), id.clone()));
                }
            } else {
                state?;
            }
        }
        Ok(())
    }
    fn receipt(
        &self,
        stream: &str,
        id: &str,
        consumer: &str,
        deliveries: i64,
    ) -> Result<String, QueueError> {
        let receipt = DeliveryReceipt {
            version: 1,
            service: self.service_id.clone(),
            group: self.group.clone(),
            stream: stream.into(),
            id: id.into(),
            consumer: consumer.into(),
            deliveries,
        };
        Ok(URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&receipt).map_err(|_| QueueError::InvalidValue)?))
    }
    fn parse_receipt(
        &self,
        stream: &str,
        id: &str,
        raw: &str,
    ) -> Result<DeliveryReceipt, QueueError> {
        if raw.len() > 8192 {
            return Err(QueueError::InvalidValue);
        }
        let bytes = URL_SAFE_NO_PAD
            .decode(raw)
            .map_err(|_| QueueError::InvalidValue)?;
        let r: DeliveryReceipt =
            serde_json::from_slice(&bytes).map_err(|_| QueueError::InvalidValue)?;
        let prefix = format!("{}/", URL_SAFE_NO_PAD.encode(&self.service_id));
        let allowed = std::iter::once(self.stream_for("normal"))
            .chain(
                self.streams
                    .as_object()
                    .into_iter()
                    .flat_map(|m| m.values().map(|v| string_or(v, "queue:normal"))),
            )
            .any(|s| stream == s || stream == format!("queue:retry:{s}"));
        if r.version != 1
            || r.service != self.service_id
            || r.group != self.group
            || r.stream != stream
            || r.id != id
            || !r.consumer.starts_with(&prefix)
            || r.deliveries < 1
            || !allowed
        {
            return Err(QueueError::StaleDelivery);
        }
        Ok(r)
    }
    pub async fn publish(&self, payload: Value, priority: &str) -> Result<Value, QueueError> {
        let _operation = self.operations.read().await;
        if self.is_draining() {
            return Err(QueueError::Draining);
        }
        self.reap_settled().await?;
        let _permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| QueueError::Backpressure)?;
        let raw = compact(&payload);
        if raw.len() > self.max_payload {
            return Err(QueueError::InvalidValue);
        }
        let stream = self.stream_for(priority);
        let backend = self.backend().await?;
        backend.ensure_group(&stream, &self.group).await?;
        let id = backend
            .add(&stream, &[("payload".into(), raw)], self.max_len)
            .await?;
        Ok(json!({"stream":stream,"message_id":id}))
    }
    pub async fn consume(
        &self,
        priority: &str,
        consumer: &str,
        _count: i64,
        block: Option<i64>,
    ) -> Result<Value, QueueError> {
        let _operation = self.operations.read().await;
        if self.is_draining() {
            return Err(QueueError::Draining);
        }
        self.reap_settled().await?;
        let permit = Arc::new(
            self.slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| QueueError::Backpressure)?,
        );
        let stream = self.stream_for(priority);
        if consumer.len() > 256 {
            return Err(QueueError::InvalidValue);
        }
        let consumer = format!(
            "{}/{}/{}/{}",
            URL_SAFE_NO_PAD.encode(&self.service_id),
            self.instance,
            consumer,
            uuid::Uuid::new_v4()
        );
        let backend = self.backend().await?;
        let selected = [format!("queue:retry:{stream}"), stream.clone()];
        for s in &selected {
            backend.ensure_group(s, &self.group).await?;
        }
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(block.unwrap_or(self.block_ms).clamp(0, 30000) as u64);
        loop {
            let draining = self.drain_notify.notified();
            tokio::pin!(draining);
            draining.as_mut().enable();
            if self.is_draining() {
                return Err(QueueError::Draining);
            }
            for selected in &selected {
                let excluded: Vec<String> = self
                    .messages
                    .lock()
                    .await
                    .keys()
                    .filter(|(s, _)| s == selected)
                    .map(|(_, id)| id.clone())
                    .collect();
                let Some(message) = backend
                    .read(selected, &self.group, &consumer, self.idle_ms, &excluded)
                    .await?
                else {
                    continue;
                };
                if message
                    .fields
                    .iter()
                    .map(|(k, v)| k.len() + v.len())
                    .sum::<usize>()
                    > self.max_payload + 32768
                {
                    return Err(QueueError::InvalidValue);
                }
                let raw = message
                    .fields
                    .get("payload")
                    .ok_or(QueueError::InvalidValue)?;
                if raw.len() > self.max_payload {
                    return Err(QueueError::InvalidValue);
                }
                let payload: Value =
                    serde_json::from_str(raw).map_err(|_| QueueError::InvalidValue)?;
                let attempts = message
                    .fields
                    .get("attempts")
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                let original = message
                    .fields
                    .get("original_stream")
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .unwrap_or(stream.clone());
                let original_id = message
                    .fields
                    .get("original_id")
                    .cloned()
                    .unwrap_or(message.id.clone());
                let key = (selected.clone(), message.id.clone());
                let mut messages = self.messages.lock().await;
                // A concurrent read may reclaim a lease before another read caches it.
                // Never overwrite an active local delivery; the stale handler will be fenced.
                if messages.contains_key(&key) {
                    return Err(QueueError::StaleDelivery);
                }
                messages.insert(
                    key,
                    Cached {
                        fields: message.fields,
                        consumer: consumer.clone(),
                        deliveries: message.deliveries,
                        _permit: Some(permit),
                        attempts,
                        original: original.clone(),
                    },
                );
                let receipt = self.receipt(selected, &message.id, &consumer, message.deliveries)?;
                return Ok(
                    json!({"delivery_receipt":receipt,"stream":selected,"message_id":message.id,"payload":payload,"attempts":attempts,"original_stream":original,"original_id":original_id}),
                );
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(Value::Null);
            }
            tokio::select! {
                _=draining=>return Err(QueueError::Draining),
                _=tokio::time::sleep_until(deadline.min(tokio::time::Instant::now()+Duration::from_millis(50)))=>{},
            }
        }
    }
    pub async fn ack(&self, stream: &str, id: &str) -> Result<Value, QueueError> {
        self.settle(stream, id, None, None).await
    }
    pub async fn fail(&self, stream: &str, id: &str, error: &str) -> Result<Value, QueueError> {
        self.settle(stream, id, Some(error), None).await
    }
    pub async fn ack_receipt(
        &self,
        stream: &str,
        id: &str,
        receipt: &str,
    ) -> Result<Value, QueueError> {
        self.settle(stream, id, None, Some(receipt)).await
    }
    pub async fn fail_receipt(
        &self,
        stream: &str,
        id: &str,
        error: &str,
        receipt: &str,
    ) -> Result<Value, QueueError> {
        self.settle(stream, id, Some(error), Some(receipt)).await
    }
    async fn settle(
        &self,
        stream: &str,
        id: &str,
        error: Option<&str>,
        receipt: Option<&str>,
    ) -> Result<Value, QueueError> {
        let _operation = self.operations.read().await;
        let key = (stream.to_owned(), id.to_owned());
        let local = self.messages.lock().await.get(&key).cloned();
        let cached = if let Some(receipt) = receipt {
            let r = self.parse_receipt(stream, id, receipt)?;
            let backend = self.backend().await?;
            let Some(fields) = backend
                .inspect(
                    stream,
                    &self.group,
                    id,
                    &r.consumer,
                    r.deliveries,
                    error.is_some(),
                )
                .await?
            else {
                self.messages.lock().await.remove(&key);
                return Ok(if error.is_some() {
                    json!({"already_settled":true})
                } else {
                    json!({"acked":0})
                });
            };
            let attempts = fields
                .get("attempts")
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            let original = fields
                .get("original_stream")
                .filter(|s| !s.is_empty())
                .cloned()
                .unwrap_or_else(|| stream.strip_prefix("queue:retry:").unwrap_or(stream).into());
            if fields.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>()
                > self.max_payload + 32768
            {
                return Err(QueueError::InvalidValue);
            }
            if error.is_some() && !fields.contains_key("payload") {
                return Err(QueueError::InvalidValue);
            }
            Cached {
                fields,
                consumer: r.consumer,
                deliveries: r.deliveries,
                _permit: None,
                attempts,
                original,
            }
        } else {
            local.ok_or(QueueError::StaleDelivery)?
        };
        let attempts = cached
            .attempts
            .checked_add(1)
            .ok_or(QueueError::InvalidValue)?;
        let target = format!(
            "queue:{}:{}",
            if attempts <= self.max_retries {
                "retry"
            } else {
                "dlq"
            },
            cached.original
        );
        let mut fields = cached.fields.clone();
        if let Some(error) = error {
            if error.len() > 16384 {
                return Err(QueueError::InvalidValue);
            }
            fields.insert("original_stream".into(), cached.original.clone());
            fields.entry("original_id".into()).or_insert(id.into());
            fields.insert("error".into(), error.into());
            fields.insert("attempts".into(), attempts.to_string());
        }
        let fields: Vec<_> = fields.into_iter().collect();
        let result = self
            .backend()
            .await?
            .settle(
                stream,
                &self.group,
                id,
                &cached.consumer,
                cached.deliveries,
                error.map(|_| (target.as_str(), fields.as_slice())),
                self.max_len,
            )
            .await;
        // Unknown network outcome keeps the cached lease for safe retry. Redis PEL
        // absence makes a repeated atomic transfer a no-op, never another append.
        if result.is_ok() || result == Err(QueueError::StaleDelivery) {
            let mut messages = self.messages.lock().await;
            if messages.get(&key).is_some_and(|current| {
                current.consumer == cached.consumer && current.deliveries == cached.deliveries
            }) {
                messages.remove(&key);
            }
        }
        let acked = result?;
        if error.is_some() {
            Ok(json!({"stream":target,"attempts":attempts}))
        } else {
            Ok(json!({"acked":acked}))
        }
    }
    pub async fn request_operation(
        &self,
        method: &str,
        url: &str,
        kwargs: &Value,
    ) -> Result<Value, QueueError> {
        let path = url.split('?').next().unwrap_or("").trim_end_matches('/');
        match (
            method.to_uppercase().as_str(),
            if path.is_empty() { "/" } else { path },
        ) {
            ("POST", "/publish") => {
                let payload = kwargs
                    .get("json")
                    .filter(|v| python_truthy(v))
                    .or_else(|| kwargs.get("payload").filter(|v| python_truthy(v)))
                    .cloned()
                    .unwrap_or(json!({}));
                self.publish(payload, &string_or(&kwargs["priority"], "normal"))
                    .await
            }
            ("GET", "/consume") => {
                self.consume(
                    &string_or(&kwargs["priority"], "normal"),
                    &string_or(&kwargs["consumer"], "worker-0"),
                    if python_truthy(&kwargs["count"]) {
                        integer(&kwargs["count"])?
                    } else {
                        1
                    },
                    kwargs.get("block_ms").map(integer).transpose()?,
                )
                .await
            }
            ("POST", "/ack") => {
                self.settle(
                    &python_str(kwargs.get("stream").ok_or(QueueError::InvalidValue)?),
                    &python_str(kwargs.get("message_id").ok_or(QueueError::InvalidValue)?),
                    None,
                    kwargs.get("delivery_receipt").and_then(Value::as_str),
                )
                .await
            }
            ("POST", "/fail") => {
                self.settle(
                    &python_str(kwargs.get("stream").ok_or(QueueError::InvalidValue)?),
                    &python_str(kwargs.get("message_id").ok_or(QueueError::InvalidValue)?),
                    Some(&string_or(&kwargs["error"], "handler failed")),
                    kwargs.get("delivery_receipt").and_then(Value::as_str),
                )
                .await
            }
            _ => Err(QueueError::InvalidOperation),
        }
    }
    pub async fn close(&self) -> Result<(), QueueError> {
        self.draining.store(true, Ordering::Release);
        self.drain_notify.notify_waiters();
        let deadline = tokio::time::Instant::now() + Duration::from_millis(self.drain_ms);
        // Wait for admitted publish/read operations; reads are nonblocking Redis
        // calls with a bounded timeout and interruptible local polling.
        {
            let _barrier = tokio::time::timeout_at(deadline, self.operations.write())
                .await
                .map_err(|_| QueueError::DrainTimedOut)?;
        }
        while !self.messages.lock().await.is_empty() && tokio::time::Instant::now() < deadline {
            let _ = tokio::time::timeout_at(deadline, self.reap_settled()).await;
            if self.messages.lock().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _barrier = tokio::time::timeout_at(deadline, self.operations.write())
            .await
            .map_err(|_| QueueError::DrainTimedOut)?;
        let timed_out = !self.messages.lock().await.is_empty();
        let mut slot = self.backend.lock().await;
        if slot.1 {
            if let Some(backend) = &slot.0 {
                backend.close().await?;
            }
        }
        slot.0 = None;
        // No ACK on timeout: pending Redis entries survive and are reclaimable.
        self.messages.lock().await.clear();
        if timed_out {
            Err(QueueError::DrainTimedOut)
        } else {
            Ok(())
        }
    }
}
impl RestTransport for MessageQueueTransport {
    fn shutdown(&self) -> RestTransportFuture<'_, ()> {
        Box::pin(async move { self.close().await.map_err(|_| AdapterFailure::Other) })
    }
    fn request(&self, r: RestRequest) -> RestTransportFuture<'_, RestResponse> {
        Box::pin(async move {
            let path = r
                .endpoint
                .split('?')
                .next()
                .unwrap_or("")
                .trim_end_matches('/');
            let kwargs = if path == "/publish" {
                json!({"json":r.body})
            } else {
                r.body
            };
            self.request_operation(&r.method, &r.endpoint, &kwargs)
                .await
                .map(RestResponse::Json)
                .map_err(|_| AdapterFailure::Other)
        })
    }
    fn health_check(&self) -> RestTransportFuture<'_, bool> {
        Box::pin(async { Ok(true) })
    }
}
