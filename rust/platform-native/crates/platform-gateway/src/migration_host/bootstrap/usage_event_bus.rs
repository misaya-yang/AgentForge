//! Opt-in source usage Redis Streams dual-write and logging consumer.
//! Mirrors events/{bus,consumer,idempotency}.py; no Redis operation at construction.
//! UsageRecorder owns the publish-failure latch. Consumers reclaim on startup, deduplicate per
//! consumer, retain failed deliveries, and park poison/exhausted records in DLQ.
use super::BootstrapError;
use crate::{
    migration_host::lifecycle::{HostError, LifecycleStep},
    usage_recorder::{UsageEventPublisher, UsageFuture, UsageRecord, UsageRecorderError},
};
use redis::{
    aio::ConnectionManager,
    streams::{StreamAutoClaimReply, StreamId, StreamReadReply},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, watch},
    task::JoinHandle,
};
const STREAM: &str = ai_platform_gateway_foundation::event_envelope::STREAM_NAMES[0].1;
const GROUP: &str = "gateway-usage-logger";
const COUNTERS: &str = "events:deliveries:events:usage:recorded:v1:gateway-usage-logger";
#[derive(Clone, Copy, Debug)]
pub enum EventError {
    Redis,
    Envelope,
    Payload,
    Handler,
}
type EventResult<T> = Result<T, EventError>;
pub type EventFuture<'a, T> = Pin<Box<dyn Future<Output = EventResult<T>> + Send + 'a>>;
pub trait UsageHandler: Send + Sync {
    fn handle<'a>(&'a self, envelope: &'a Value) -> EventFuture<'a, ()>;
}
pub struct LoggingUsageHandler;
impl UsageHandler for LoggingUsageHandler {
    fn handle<'a>(&'a self, envelope: &'a Value) -> EventFuture<'a, ()> {
        Box::pin(async move {
            let p = &envelope["payload"];
            let safe=crate::metrics_redaction::redact_sensitive_text(&json!({"tenant":envelope["tenant_id"],"user":p["user_id"],"model":p["model"],"input_tokens":p["input_tokens"],"output_tokens":p["output_tokens"],"latency_ms":p["latency_ms"],"status":p["status"],"request_id":envelope["request_id"],"event_id":envelope["event_id"]}).to_string());
            eprintln!("usage_event {safe}");
            Ok(())
        })
    }
}
#[derive(Clone)]
enum ConnectionSource {
    Legacy(redis::Client),
    Coordination(ai_platform_service_primitives::valkey::CoordConnection),
}
impl ConnectionSource {
    async fn connect(&self) -> EventResult<ConnectionManager> {
        match self {
            Self::Legacy(client) => ConnectionManager::new(client.clone())
                .await
                .map_err(|_| EventError::Redis),
            Self::Coordination(coord) => coord
                .dedicated_manager()
                .await
                .map_err(|_| EventError::Redis),
        }
    }
}
pub struct UsageBusPublisher {
    source: ConnectionSource,
    connection: Mutex<Option<ConnectionManager>>,
}
impl UsageBusPublisher {
    fn new(source: ConnectionSource) -> Self {
        Self {
            source,
            connection: Mutex::new(None),
        }
    }
    async fn publish_record(&self, record: &UsageRecord) -> EventResult<()> {
        let envelope = json!({
            "event_id":uuid::Uuid::new_v4().to_string(),"event_type":"usage.recorded.v1","schema_version":1,
            "occurred_at":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros,true),"producer":"usage-recorder",
            "tenant_id":record.tenant_id,"request_id":record.request_id,"traceparent":Value::Null,
            "payload":{"tenant_id":record.tenant_id,"user_id":record.user_id,"model":record.model,"provider":record.provider,"service_id":record.service_id,"assistant_id":record.assistant_id,"input_tokens":record.input_tokens,"output_tokens":record.output_tokens,"latency_ms":record.latency_ms,"first_token_ms":record.first_token_ms,"request_total_duration_ms":record.request_total_duration_ms,"error_type":record.error_type,"status":if record.status.is_empty(){"success"}else{&record.status},"request_type":if record.request_type.is_empty(){"chat"}else{&record.request_type},"timestamp":record.timestamp.timestamp_micros() as f64/1_000_000.0,"metadata":record.metadata}
        });
        let envelope = decode_envelope(envelope.to_string().as_bytes())?;
        let result = async {
            let mut guard = self.connection.lock().await;
            if guard.is_none() {
                *guard = Some(self.source.connect().await?);
            }
            let connection = guard.as_mut().ok_or(EventError::Redis)?;
            let _: String = redis::cmd("XADD")
                .arg(STREAM)
                .arg("MAXLEN")
                .arg("~")
                .arg(50_000)
                .arg("*")
                .arg("payload")
                .arg(envelope.to_string())
                .query_async(connection)
                .await
                .map_err(|_| EventError::Redis)?;
            Ok(())
        }
        .await;
        result
    }
    pub async fn close(&self) {
        self.connection.lock().await.take();
    }
}
impl UsageEventPublisher for UsageBusPublisher {
    fn publish<'a>(&'a self, record: &'a UsageRecord) -> UsageFuture<'a, ()> {
        Box::pin(async move {
            self.publish_record(record).await.map_err(|_| {
                UsageRecorderError::Collaborator("usage_event_publish_unavailable".into())
            })
        })
    }
}
struct RunningConsumer {
    stop: watch::Sender<bool>,
    handle: JoinHandle<EventResult<()>>,
}
pub struct UsageEventBus {
    pub publisher: Arc<UsageBusPublisher>,
    source: ConnectionSource,
    consumer_name: String,
    handler: Arc<dyn UsageHandler>,
    running: Mutex<Option<RunningConsumer>>,
    alive: Arc<AtomicBool>,
}
impl UsageEventBus {
    /// Split mode cannot open a legacy URL outside the validated TLS/ACL boundary.
    pub fn configured(
        environment: &BTreeMap<String, String>,
        coordination: Option<ai_platform_service_primitives::valkey::CoordConnection>,
        consumer_name: String,
    ) -> Result<Option<Arc<Self>>, BootstrapError> {
        match configuration(environment, coordination.is_some())? {
            BusConfiguration::Disabled => Ok(None),
            BusConfiguration::Legacy(url) => Self::new(url, consumer_name).map(Some),
            BusConfiguration::Coordination => Self::with_coordination(
                coordination.ok_or(BootstrapError::Configuration("usage_event_coordination"))?,
                consumer_name,
            )
            .map(Some),
        }
    }
    pub fn new(url: &str, consumer_name: String) -> Result<Arc<Self>, BootstrapError> {
        Self::with_handler(url, consumer_name, Arc::new(LoggingUsageHandler))
    }
    pub fn with_handler(
        url: &str,
        consumer_name: String,
        handler: Arc<dyn UsageHandler>,
    ) -> Result<Arc<Self>, BootstrapError> {
        if consumer_name.is_empty() {
            return Err(BootstrapError::Configuration("usage_event_consumer_name"));
        }
        let client = redis::Client::open(url)
            .map_err(|_| BootstrapError::Configuration("EVENT_BUS_REDIS_URL"))?;
        Self::from_source(ConnectionSource::Legacy(client), consumer_name, handler)
    }
    pub fn with_coordination(
        connection: ai_platform_service_primitives::valkey::CoordConnection,
        consumer_name: String,
    ) -> Result<Arc<Self>, BootstrapError> {
        Self::from_source(
            ConnectionSource::Coordination(connection),
            consumer_name,
            Arc::new(LoggingUsageHandler),
        )
    }
    fn from_source(
        source: ConnectionSource,
        consumer_name: String,
        handler: Arc<dyn UsageHandler>,
    ) -> Result<Arc<Self>, BootstrapError> {
        if consumer_name.is_empty() {
            return Err(BootstrapError::Configuration("usage_event_consumer_name"));
        }
        Ok(Arc::new(Self {
            publisher: Arc::new(UsageBusPublisher::new(source.clone())),
            source,
            consumer_name,
            handler,
            running: Mutex::new(None),
            alive: Arc::new(AtomicBool::new(false)),
        }))
    }
    pub fn running(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
    pub async fn start(self: &Arc<Self>) {
        let mut running = self.running.lock().await;
        if running
            .as_ref()
            .is_some_and(|worker| !worker.handle.is_finished())
        {
            return;
        }
        if let Some(previous) = running.take() {
            let _ = previous.handle.await;
        }
        let (stop, receive) = watch::channel(false);
        let owner = self.clone();
        let handle = tokio::spawn(async move {
            struct Alive(Arc<AtomicBool>);
            impl Drop for Alive {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::Release);
                }
            }
            owner.alive.store(true, Ordering::Release);
            let _alive = Alive(owner.alive.clone());
            let result = owner.consume(receive).await;
            if result.is_err() {
                ai_platform_gateway_foundation::worker_lease::invalidate("usage_event_consumer");
                eprintln!("usage_event_consumer_stopped_on_error");
            }
            result
        });
        *running = Some(RunningConsumer { stop, handle });
    }
    pub async fn stop(&self) -> EventResult<()> {
        if let Some(running) = self.running.lock().await.take() {
            let _ = running.stop.send(true);
            running.handle.await.map_err(|_| EventError::Handler)??;
        }
        Ok(())
    }
    pub fn lifecycle_steps(self: &Arc<Self>) -> (Vec<LifecycleStep>, Vec<LifecycleStep>) {
        let start = self.clone();
        let stop = self.clone();
        let publisher = self.publisher.clone();
        (
            vec![LifecycleStep {
                name: "usage_event_consumer",
                best_effort: true,
                operation: Arc::new(move || {
                    let owner = start.clone();
                    Box::pin(async move {
                        owner.start().await;
                        Ok(())
                    })
                }),
            }],
            vec![
                LifecycleStep {
                    name: "usage_event_consumer",
                    best_effort: true,
                    operation: Arc::new(move || {
                        let owner = stop.clone();
                        Box::pin(async move {
                            owner
                                .stop()
                                .await
                                .map_err(|_| HostError::Shutdown("usage_event_consumer"))
                        })
                    }),
                },
                LifecycleStep {
                    name: "usage_event_publisher",
                    best_effort: true,
                    operation: Arc::new(move || {
                        let publisher = publisher.clone();
                        Box::pin(async move {
                            publisher.close().await;
                            Ok(())
                        })
                    }),
                },
            ],
        )
    }
    async fn ensure_group(connection: &mut ConnectionManager) -> EventResult<()> {
        let result: redis::RedisResult<String> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(STREAM)
            .arg(GROUP)
            .arg("0")
            .arg("MKSTREAM")
            .query_async(connection)
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if error.code() == Some("BUSYGROUP") => Ok(()),
            Err(_) => Err(EventError::Redis),
        }
    }
    async fn consume(&self, stop: watch::Receiver<bool>) -> EventResult<()> {
        let mut connection = self.source.connect().await?;
        let reclaimed = ai_platform_gateway_foundation::worker_lease::with_effect(
            "usage_event_consumer", async {
                Self::ensure_group(&mut connection).await?;
                self.reclaim(&mut connection).await
            },
        ).await.map_err(|_| EventError::Handler)??;
        for message in reclaimed {
            if *stop.borrow() {
                return Ok(());
            }
            ai_platform_gateway_foundation::worker_lease::with_effect(
                "usage_event_consumer", self.handle_one(&mut connection, message),
            ).await.map_err(|_| EventError::Handler)??;
        }
        while !*stop.borrow() {
            ai_platform_gateway_foundation::worker_lease::with_effect("usage_event_consumer", async {
            let entries: redis::RedisResult<StreamReadReply> = redis::cmd("XREADGROUP")
                .arg("GROUP")
                .arg(GROUP)
                .arg(&self.consumer_name)
                .arg("COUNT")
                .arg(32)
                .arg("BLOCK")
                .arg(1000)
                .arg("STREAMS")
                .arg(STREAM)
                .arg(">")
                .query_async(&mut connection)
                .await;
            let entries = match entries {
                Ok(v) => v,
                Err(e)
                    if e.kind()
                        == redis::ErrorKind::Server(redis::ServerErrorKind::ResponseError) =>
                {
                    Self::ensure_group(&mut connection).await?;
                    StreamReadReply::default()
                }
                Err(_) => return Err(EventError::Redis),
            };
            if entries.keys.is_empty() {
                tokio::time::sleep(Duration::from_millis(50)).await;
                return Ok(());
            }
            for key in entries.keys {
                for message in key.ids {
                    if *stop.borrow() {
                        return Ok(());
                    }
                    self.handle_one(&mut connection, message).await?;
                }
            }
                Ok::<(), EventError>(())
            }).await.map_err(|_| EventError::Handler)??;
        }
        Ok(())
    }
    async fn reclaim(&self, connection: &mut ConnectionManager) -> EventResult<Vec<StreamId>> {
        let mut cursor = "0-0".to_owned();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for _ in 0..10 {
            let result: redis::RedisResult<StreamAutoClaimReply> = redis::cmd("XAUTOCLAIM")
                .arg(STREAM)
                .arg(GROUP)
                .arg(&self.consumer_name)
                .arg(60_000)
                .arg(&cursor)
                .arg("COUNT")
                .arg(32)
                .query_async(connection)
                .await;
            let result = match result {
                Ok(v) => v,
                Err(e)
                    if e.kind()
                        == redis::ErrorKind::Server(redis::ServerErrorKind::ResponseError) =>
                {
                    return Ok(out);
                }
                Err(_) => return Err(EventError::Redis),
            };
            let mut added = 0;
            for message in result.claimed {
                if seen.insert(message.id.clone()) {
                    out.push(message);
                    added += 1;
                }
            }
            if added == 0 || result.next_stream_id == "0-0" || result.next_stream_id == cursor {
                return Ok(out);
            }
            cursor = result.next_stream_id;
        }
        Ok(out)
    }
    async fn finish(connection: &mut ConnectionManager, id: &str) -> EventResult<()> {
        let _: i64 = redis::cmd("XACK")
            .arg(STREAM)
            .arg(GROUP)
            .arg(id)
            .query_async(connection)
            .await
            .map_err(|_| EventError::Redis)?;
        let _: i64 = redis::cmd("HDEL")
            .arg(COUNTERS)
            .arg(id)
            .query_async(connection)
            .await
            .map_err(|_| EventError::Redis)?;
        Ok(())
    }
    async fn dlq(
        &self,
        connection: &mut ConnectionManager,
        id: &str,
        error: &str,
        attempts: i64,
    ) -> EventResult<()> {
        let _: String = redis::cmd("XADD")
            .arg(format!("{STREAM}:dlq"))
            .arg("MAXLEN")
            .arg("~")
            .arg(10_000)
            .arg("*")
            .arg("original_id")
            .arg(id)
            .arg("error")
            .arg(error)
            .arg("attempts")
            .arg(attempts)
            .arg("stream")
            .arg(STREAM)
            .arg("group")
            .arg(GROUP)
            .arg("consumer")
            .arg(&self.consumer_name)
            .query_async(connection)
            .await
            .map_err(|_| EventError::Redis)?;
        Ok(())
    }
    async fn handle_one(
        &self,
        connection: &mut ConnectionManager,
        message: StreamId,
    ) -> EventResult<()> {
        let raw = message
            .map
            .get("payload")
            .and_then(|v| redis::from_redis_value_ref::<Vec<u8>>(v).ok());
        let Some(raw) = raw else {
            self.dlq(connection, &message.id, "missing payload field", 1)
                .await?;
            return Self::finish(connection, &message.id).await;
        };
        let envelope = match decode_envelope(&raw) {
            Ok(v) => v,
            Err(EventError::Envelope) => {
                self.dlq(
                    connection,
                    &message.id,
                    "deserialize: malformed envelope",
                    1,
                )
                .await?;
                return Self::finish(connection, &message.id).await;
            }
            Err(error) => return Err(error),
        };
        let event_id = envelope["event_id"].as_str().ok_or(EventError::Envelope)?;
        let claim = format!("events:idem:{}:{event_id}", self.consumer_name);
        let claimed: Option<String> = redis::cmd("SET")
            .arg(&claim)
            .arg("1")
            .arg("NX")
            .arg("EX")
            .arg(86_400)
            .query_async(connection)
            .await
            .map_err(|_| EventError::Redis)?;
        if claimed.is_none() {
            return Self::finish(connection, &message.id).await;
        }
        let attempts: i64 = redis::cmd("HINCRBY")
            .arg(COUNTERS)
            .arg(&message.id)
            .arg(1)
            .query_async(connection)
            .await
            .map_err(|_| EventError::Redis)?;
        if self.handler.handle(&envelope).await.is_err() {
            let _: i64 = redis::cmd("DEL")
                .arg(&claim)
                .query_async(connection)
                .await
                .map_err(|_| EventError::Redis)?;
            if attempts >= 3 {
                self.dlq(
                    connection,
                    &message.id,
                    "EventHandlerError: handler failed",
                    attempts,
                )
                .await?;
                Self::finish(connection, &message.id).await?;
            } else {
                eprintln!("usage_event_handler_failed_pending");
            }
            return Ok(());
        }
        Self::finish(connection, &message.id).await
    }
}

enum BusConfiguration<'a> {
    Disabled,
    Legacy(&'a str),
    Coordination,
}
fn configuration(
    environment: &BTreeMap<String, String>,
    split: bool,
) -> Result<BusConfiguration<'_>, BootstrapError> {
    let legacy = environment
        .get("EVENT_BUS_REDIS_URL")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    if split && legacy.is_some() {
        return Err(BootstrapError::Configuration("split_usage_bus_legacy_url"));
    }
    let enabled = match environment
        .get("AI_PLATFORM_USAGE_EVENT_BUS_ENABLED")
        .map(String::as_str)
    {
        None => legacy.is_some(),
        Some("true" | "1") => true,
        Some("false" | "0") => false,
        _ => {
            return Err(BootstrapError::Configuration(
                "AI_PLATFORM_USAGE_EVENT_BUS_ENABLED",
            ));
        }
    };
    if !enabled {
        return Ok(BusConfiguration::Disabled);
    }
    if split {
        return Ok(BusConfiguration::Coordination);
    }
    Ok(BusConfiguration::Legacy(legacy.ok_or(
        BootstrapError::Configuration("EVENT_BUS_REDIS_URL"),
    )?))
}

#[cfg(test)]
mod valkey_source_tests {
    use super::*;
    #[test]
    fn split_bus_reuses_only_validated_coordination_and_rejects_legacy_urls() {
        let mut env = BTreeMap::new();
        assert!(matches!(
            configuration(&env, true).unwrap(),
            BusConfiguration::Disabled
        ));
        env.insert("AI_PLATFORM_USAGE_EVENT_BUS_ENABLED".into(), "true".into());
        assert!(matches!(
            configuration(&env, true).unwrap(),
            BusConfiguration::Coordination
        ));
        assert!(configuration(&env, false).is_err());
        env.insert(
            "EVENT_BUS_REDIS_URL".into(),
            "redis://localhost:6379/0".into(),
        );
        assert!(configuration(&env, true).is_err());
        assert!(matches!(
            configuration(&env, false).unwrap(),
            BusConfiguration::Legacy(_)
        ));
        env.insert(
            "AI_PLATFORM_USAGE_EVENT_BUS_ENABLED".into(),
            "invalid".into(),
        );
        assert!(configuration(&env, false).is_err());
    }
}
/// Preserve source's two phases: malformed outer envelopes go to DLQ;
/// typed payload validation errors stop the loop without acknowledging the record.
fn decode_envelope(raw: &[u8]) -> EventResult<Value> {
    use ai_platform_gateway_foundation::event_envelope::{
        EventDeserializationError, parse_envelope,
    };
    let envelope = parse_envelope(raw).map_err(|error| match error {
        EventDeserializationError::PayloadValidation => EventError::Payload,
        _ => EventError::Envelope,
    })?;
    serde_json::to_value(envelope).map_err(|_| EventError::Envelope)
}
