//! Source event-bus lifecycle assertions through the production Redis consumer.
//! Explicit disposable Redis DB only. No network operation in ordinary test runs.
use ai_platform_gateway::{
    migration_host::bootstrap::usage_event_bus::{
        EventError, EventFuture, UsageEventBus, UsageHandler,
    },
    usage_recorder::{UsageEventPublisher, UsageRecord},
};
use futures_util::FutureExt;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::Mutex;
const STREAM: &str = "events:usage:recorded:v1";
const GROUP: &str = "gateway-usage-logger";
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[derive(Default)]
struct Handler {
    received: Mutex<Vec<Value>>,
    fail: AtomicBool,
}
impl UsageHandler for Handler {
    fn handle<'a>(&'a self, event: &'a Value) -> EventFuture<'a, ()> {
        Box::pin(async move {
            self.received.lock().await.push(event.clone());
            if self.fail.load(Ordering::SeqCst) {
                Err(EventError::Handler)
            } else {
                Ok(())
            }
        })
    }
}
async fn wait_count(handler: &Handler, count: usize) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if handler.received.lock().await.len() >= count {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}
async fn stop(bus: &Arc<UsageEventBus>) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(3), bus.stop())
        .await?
        .map_err(|_| "event consumer stop failed")?;
    Ok(())
}
async fn append(conn: &mut redis::aio::MultiplexedConnection, raw: &str) -> Result<String> {
    Ok(redis::cmd("XADD")
        .arg(STREAM)
        .arg("*")
        .arg("payload")
        .arg(raw)
        .query_async(conn)
        .await?)
}
async fn entries(
    conn: &mut redis::aio::MultiplexedConnection,
    key: &str,
) -> Result<redis::streams::StreamRangeReply> {
    Ok(redis::cmd("XRANGE")
        .arg(key)
        .arg("-")
        .arg("+")
        .query_async(conn)
        .await?)
}
async fn no_pending(conn: &mut redis::aio::MultiplexedConnection) -> Result<()> {
    let pending: redis::streams::StreamPendingReply = redis::cmd("XPENDING")
        .arg(STREAM)
        .arg(GROUP)
        .query_async(conn)
        .await?;
    assert_eq!(pending.count(), 0);
    Ok(())
}

#[tokio::test]
#[ignore = "requires dedicated empty loopback Redis DB via AI_PLATFORM_RUNTIME_EVENT_TEST_REDIS_URL"]
async fn publish_envelope_duplicate_poison_retry_and_dlq_lifecycle() -> Result<()> {
    let origin = std::env::var("AI_PLATFORM_RUNTIME_EVENT_TEST_REDIS_URL")
        .map_err(|_| "explicit disposable Redis test URL required")?;
    let url = url::Url::parse(&origin).map_err(|_| "invalid disposable Redis test URL")?;
    if url.scheme() != "redis"
        || !matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        || url
            .path()
            .trim_start_matches('/')
            .parse::<u32>()
            .ok()
            .is_none_or(|db| db == 0)
    {
        return Err("test requires loopback Redis and dedicated nonzero database".into());
    }
    let mut conn = redis::Client::open(origin.as_str())?
        .get_multiplexed_async_connection()
        .await?;
    let size: i64 = redis::cmd("DBSIZE").query_async(&mut conn).await?;
    if size != 0 {
        return Err(
            "disposable Redis database must be empty; no existing data will be removed".into(),
        );
    }
    let lock: Option<String> = redis::cmd("SET")
        .arg("runtime-parity-test-owner")
        .arg("reserved")
        .arg("NX")
        .arg("EX")
        .arg(120)
        .query_async(&mut conn)
        .await?;
    if lock.is_none() {
        return Err("disposable Redis database already reserved".into());
    }
    let handler = Arc::new(Handler::default());
    let consumer = format!("parity-{}", uuid::Uuid::new_v4());
    let bus = UsageEventBus::with_handler(&origin, consumer.clone(), handler.clone())
        .map_err(|_| "event bus configuration failed")?;
    let result = std::panic::AssertUnwindSafe(async {
        // Source envelope factories + publishing: the actual producer generates
        // UUID4/UTC and puts a JSON envelope into the registered stream.
        let mut record = UsageRecord::new(
            "t1",
            "u1",
            "qwen-3.6",
            chrono::DateTime::from_timestamp(1714200000, 0).unwrap(),
        );
        record.request_id = "req-abc".into();
        record.provider = "dashscope".into();
        record.service_id = "svc-x".into();
        record.input_tokens = 100;
        record.output_tokens = 200;
        record.latency_ms = 1500;
        record.first_token_ms = 300;
        record.request_total_duration_ms = 1500;
        record.metadata.insert("k".into(), json!("v"));
        bus.publisher.publish(&record).await?;
        let rows = entries(&mut conn, STREAM).await?;
        assert_eq!(rows.ids.len(), 1);
        assert!(rows.ids[0].id.contains('-'));
        let raw: String = redis::from_redis_value_ref(rows.ids[0].map.get("payload").unwrap())?;
        let envelope: Value = serde_json::from_str(&raw)?;
        assert_eq!(
            uuid::Uuid::parse_str(envelope["event_id"].as_str().unwrap())?.get_version_num(),
            4
        );
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(envelope["occurred_at"].as_str().unwrap())?
                .offset()
                .local_minus_utc(),
            0
        );
        assert_eq!(envelope["event_type"], "usage.recorded.v1");
        assert_eq!(envelope["tenant_id"], "t1");
        assert_eq!(envelope["request_id"], "req-abc");
        // Real duplicate entry with the same event identity, not a second
        // publisher invocation (which correctly mints a new event identity).
        append(&mut conn, &raw).await?;
        bus.start().await;
        wait_count(&handler, 1).await?;
        stop(&bus).await?;
        assert_eq!(handler.received.lock().await.len(), 1);
        no_pending(&mut conn).await?;
        let observed = handler.received.lock().await[0].clone();
        for key in [
            "event_id",
            "event_type",
            "producer",
            "tenant_id",
            "request_id",
            "traceparent",
            "schema_version",
        ] {
            assert_eq!(observed[key], envelope[key]);
        }
        assert_eq!(observed["payload"]["input_tokens"], 100);
        assert_eq!(observed["payload"]["output_tokens"], 200);
        assert_eq!(observed["payload"]["metadata"], json!({"k":"v"}));
        assert_eq!(observed["payload"]["timestamp"], 1714200000.0);
        let mut traced = envelope.clone();
        traced["event_id"] = json!(uuid::Uuid::new_v4().to_string());
        traced["traceparent"] = json!("00-aabb-ccdd-01");
        append(&mut conn, &traced.to_string()).await?;
        bus.start().await;
        wait_count(&handler, 2).await?;
        stop(&bus).await?;
        assert_eq!(
            handler.received.lock().await[1]["traceparent"],
            traced["traceparent"]
        );
        // Unknown discriminator, malformed JSON, and missing discriminator
        // are rejected before handler invocation and parked for inspection.
        let mut unknown = envelope.clone();
        unknown["event_type"] = json!("unknown.future.v9");
        for raw in [
            unknown.to_string(),
            "{not-json".into(),
            r#"{"producer":"x","tenant_id":"t","request_id":"r","payload":{}}"#.into(),
        ] {
            append(&mut conn, &raw).await?;
        }
        bus.start().await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if entries(&mut conn, &format!("{STREAM}:dlq"))
                    .await
                    .unwrap()
                    .ids
                    .len()
                    == 3
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        stop(&bus).await?;
        assert_eq!(handler.received.lock().await.len(), 2);
        no_pending(&mut conn).await?;
        // Handler failure releases idempotency claim. Reclaim uses Redis's
        // pending idle age rather than changing production retry constants.
        handler.fail.store(true, Ordering::SeqCst);
        let mut failed = envelope.clone();
        failed["event_id"] = json!(uuid::Uuid::new_v4().to_string());
        let id = append(&mut conn, &failed.to_string()).await?;
        for attempt in 1..=3 {
            bus.start().await;
            wait_count(&handler, 2 + attempt).await?;
            stop(&bus).await?;
            if attempt < 3 {
                let _: redis::Value = redis::cmd("XCLAIM")
                    .arg(STREAM)
                    .arg(GROUP)
                    .arg(&consumer)
                    .arg(0)
                    .arg(&id)
                    .arg("IDLE")
                    .arg(60_001)
                    .arg("JUSTID")
                    .query_async(&mut conn)
                    .await?;
            }
        }
        no_pending(&mut conn).await?;
        let dlq = entries(&mut conn, &format!("{STREAM}:dlq")).await?;
        assert_eq!(dlq.ids.len(), 4);
        let last = &dlq.ids.last().unwrap().map;
        assert_eq!(
            redis::from_redis_value_ref::<String>(&last["original_id"])?,
            id
        );
        assert!(redis::from_redis_value_ref::<i64>(&last["attempts"])? >= 3);
        assert!(
            redis::from_redis_value_ref::<String>(&last["error"])?.contains("EventHandlerError")
        );
        assert_eq!(handler.received.lock().await.len(), 5);
        bus.publisher.close().await;
        bus.publisher.close().await;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    })
    .catch_unwind()
    .await;
    let stopped = stop(&bus).await;
    bus.publisher.close().await;
    // Only fixed bus keys and this unique consumer's idempotency keys are removed.
    let mut keys: Vec<String> = redis::cmd("KEYS")
        .arg(format!("events:idem:{consumer}:*"))
        .query_async(&mut conn)
        .await?;
    keys.extend([
        STREAM.into(),
        format!("{STREAM}:dlq"),
        format!("events:deliveries:{STREAM}:{GROUP}"),
        "runtime-parity-test-owner".into(),
    ]);
    let _: i64 = redis::cmd("DEL").arg(keys).query_async(&mut conn).await?;
    stopped?;
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
