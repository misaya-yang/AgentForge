use ai_platform_gateway_foundation::{
    generic_rest_adapter::{GenericRestAdapterFactory, RestRequest, RestResponse, RestTransport},
    message_queue_transport::*,
    protocol::{ServiceDefinition, TransportType},
    service_ownership::ServiceHandle,
    service_registry::AdapterFactory,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
#[derive(Default)]
struct Backend {
    state: Mutex<State>,
    pause_settle: Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>,
}
#[derive(Default)]
struct State {
    messages: VecDeque<Option<QueueMessage>>,
    pending: HashMap<String, (String, i64)>,
    stored: HashMap<String, HashMap<String, String>>,
    added: Vec<(String, Vec<(String, String)>)>,
    calls: Vec<String>,
    full: bool,
    lose_response: bool,
    closed: bool,
}
impl Backend {
    fn push(&self, id: &str, attempts: i64) {
        self.state
            .lock()
            .unwrap()
            .messages
            .push_back(Some(QueueMessage {
                id: id.into(),
                deliveries: 1,
                fields: HashMap::from([
                    ("payload".into(), "{\"text\":\"你好\"}".into()),
                    ("attempts".into(), attempts.to_string()),
                    ("original_stream".into(), "tasks".into()),
                    ("business_id".into(), "order-7".into()),
                ]),
            }));
    }
}
impl QueueBackend for Backend {
    fn ensure_group<'a>(&'a self, s: &'a str, _: &'a str) -> QueueFuture<'a, ()> {
        Box::pin(async move {
            self.state.lock().unwrap().calls.push(format!("group:{s}"));
            Ok(())
        })
    }
    fn add<'a>(
        &'a self,
        s: &'a str,
        f: &'a [(String, String)],
        _: usize,
    ) -> QueueFuture<'a, String> {
        Box::pin(async move {
            let mut x = self.state.lock().unwrap();
            if x.full {
                return Err(QueueError::Backpressure);
            }
            x.added.push((s.into(), f.into()));
            Ok("1-0".into())
        })
    }
    fn read<'a>(
        &'a self,
        s: &'a str,
        _: &'a str,
        c: &'a str,
        idle: u64,
        excluded: &'a [String],
    ) -> QueueFuture<'a, Option<QueueMessage>> {
        Box::pin(async move {
            assert!(idle > 0);
            let mut x = self.state.lock().unwrap();
            x.calls.push(format!("read:{s}"));
            let m = x.messages.pop_front().flatten();
            if let Some(m) = &m {
                assert!(!excluded.contains(&m.id));
                x.pending.insert(m.id.clone(), (c.into(), m.deliveries));
                x.stored.insert(m.id.clone(), m.fields.clone());
            }
            Ok(m)
        })
    }
    fn settle<'a>(
        &'a self,
        s: &'a str,
        _: &'a str,
        id: &'a str,
        c: &'a str,
        d: i64,
        target: Option<(&'a str, &'a [(String, String)])>,
        _: usize,
    ) -> QueueFuture<'a, i64> {
        Box::pin(async move {
            let pause = self.pause_settle.lock().unwrap().take();
            if let Some((started, resume)) = pause {
                started.notify_one();
                resume.notified().await;
            }
            let mut x = self.state.lock().unwrap();
            x.calls.push(format!("settle:{s}"));
            let Some(owner) = x.pending.get(id) else {
                return Ok(0);
            };
            if owner != &(c.into(), d) {
                return Err(QueueError::StaleDelivery);
            }
            if let Some((t, f)) = target {
                if x.full {
                    return Err(QueueError::Backpressure);
                }
                x.added.push((t.into(), f.into()));
            }
            x.pending.remove(id);
            if x.lose_response {
                x.lose_response = false;
                return Err(QueueError::Backend);
            }
            Ok(1)
        })
    }
    fn inspect<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        id: &'a str,
        c: &'a str,
        d: i64,
        fields: bool,
    ) -> QueueFuture<'a, Option<HashMap<String, String>>> {
        Box::pin(async move {
            let x = self.state.lock().unwrap();
            let Some(owner) = x.pending.get(id) else {
                return Ok(None);
            };
            if owner != &(c.into(), d) {
                return Err(QueueError::StaleDelivery);
            }
            Ok(Some(if fields {
                x.stored.get(id).cloned().unwrap_or_default()
            } else {
                HashMap::new()
            }))
        })
    }
    fn close(&self) -> QueueFuture<'_, ()> {
        Box::pin(async move {
            self.state.lock().unwrap().closed = true;
            Ok(())
        })
    }
}
fn service(extra: Value) -> ServiceHandle {
    let mut config = json!({"streams":{"normal":"tasks","high":"priority"},"max_retries":1,"drain_timeout_ms":25});
    for (k, v) in extra.as_object().unwrap() {
        config[k] = v.clone();
    }
    ServiceHandle::new(ServiceDefinition {
        service_id: "service".into(),
        connector_type: TransportType::MessageQueue,
        connector_config: config,
        ..Default::default()
    })
}
fn queue(b: Arc<Backend>) -> MessageQueueTransport {
    MessageQueueTransport::with_backend(&service(json!({})), b, false).unwrap()
}
#[tokio::test]
async fn publish_preserves_payload_and_factory_wiring() {
    let b = Arc::new(Backend::default());
    let q = queue(b.clone());
    assert_eq!(
        q.request(RestRequest {
            method: "POST".into(),
            endpoint: "/publish".into(),
            body: json!({"text":"你好"})
        })
        .await
        .unwrap(),
        RestResponse::Json(json!({"stream":"tasks","message_id":"1-0"}))
    );
    assert_eq!(
        b.state.lock().unwrap().added[0].1,
        vec![("payload".into(), "{\"text\":\"\\u4f60\\u597d\"}".into())]
    );
    assert!(GenericRestAdapterFactory.create(service(json!({}))).is_ok());
    q.close().await.unwrap();
}
#[tokio::test]
async fn retry_priority_and_exactly_one_delivery_even_if_count_large() {
    let b = Arc::new(Backend::default());
    b.state.lock().unwrap().messages.push_back(None);
    b.push("2-0", 0);
    let q = queue(b.clone());
    let r = q.consume("normal", "worker", 500, None).await.unwrap();
    assert_eq!(r["stream"], "tasks");
    assert_eq!(r["original_id"], "2-0");
    assert_eq!(
        b.state
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|s| s.starts_with("read:"))
            .cloned()
            .collect::<Vec<_>>(),
        ["read:queue:retry:tasks", "read:tasks"]
    );
    assert_eq!(q.ack("tasks", "2-0").await.unwrap(), json!({"acked":1}));
    q.close().await.unwrap();
}
#[tokio::test]
async fn atomic_retry_is_idempotent_after_lost_response_and_keeps_business_fields() {
    let b = Arc::new(Backend::default());
    b.push("2-0", 0);
    let q = queue(b.clone());
    let r = q.consume("normal", "w", 1, None).await.unwrap();
    let s = r["stream"].as_str().unwrap();
    b.state.lock().unwrap().lose_response = true;
    assert_eq!(q.fail(s, "2-0", "error").await, Err(QueueError::Backend));
    assert_eq!(
        q.fail(s, "2-0", "error").await.unwrap(),
        json!({"stream":"queue:retry:tasks","attempts":1})
    );
    let x = b.state.lock().unwrap();
    assert_eq!(x.added.len(), 1);
    let f: HashMap<_, _> = x.added[0].1.clone().into_iter().collect();
    assert_eq!(f["business_id"], "order-7");
    assert_eq!(f["original_id"], "2-0");
    assert!(x.pending.is_empty());
}
#[tokio::test]
async fn full_retry_destination_keeps_pending_then_routes_dead_letter() {
    let b = Arc::new(Backend::default());
    b.push("3-0", 1);
    let q = queue(b.clone());
    let r = q.consume("normal", "w", 1, None).await.unwrap();
    let s = r["stream"].as_str().unwrap();
    b.state.lock().unwrap().full = true;
    assert_eq!(
        q.fail(s, "3-0", "error").await,
        Err(QueueError::Backpressure)
    );
    assert!(b.state.lock().unwrap().pending.contains_key("3-0"));
    b.state.lock().unwrap().full = false;
    assert_eq!(
        q.fail(s, "3-0", "error").await.unwrap()["stream"],
        "queue:dlq:tasks"
    );
}
#[tokio::test]
async fn reclaimed_delivery_fences_old_handler() {
    let b = Arc::new(Backend::default());
    b.push("4-0", 0);
    let q = queue(b.clone());
    let r = q.consume("normal", "w", 1, None).await.unwrap();
    b.state
        .lock()
        .unwrap()
        .pending
        .insert("4-0".into(), ("new-worker".into(), 2));
    assert_eq!(
        q.fail(r["stream"].as_str().unwrap(), "4-0", "late").await,
        Err(QueueError::StaleDelivery)
    );
    assert!(b.state.lock().unwrap().added.is_empty());
    q.close().await.unwrap();
}
#[tokio::test]
async fn local_inflight_limit_backpressures_and_ack_releases_capacity() {
    let b = Arc::new(Backend::default());
    b.push("5-0", 0);
    let q =
        MessageQueueTransport::with_backend(&service(json!({"max_inflight":1})), b.clone(), false)
            .unwrap();
    let r = q.consume("normal", "w", 1, None).await.unwrap();
    assert_eq!(
        q.consume("normal", "w", 1, None).await,
        Err(QueueError::Backpressure)
    );
    assert_eq!(
        q.publish(json!({}), "normal").await,
        Err(QueueError::Backpressure)
    );
    q.ack(r["stream"].as_str().unwrap(), "5-0").await.unwrap();
    q.publish(json!({}), "normal").await.unwrap();
}
#[tokio::test]
async fn drain_rejects_new_work_but_allows_existing_ack() {
    let b = Arc::new(Backend::default());
    b.push("6-0", 0);
    let q = Arc::new(queue(b.clone()));
    let r = q.consume("normal", "w", 1, None).await.unwrap();
    let closing = {
        let q = q.clone();
        tokio::spawn(async move { q.close().await })
    };
    while !q.is_draining() {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        q.publish(json!({}), "normal").await,
        Err(QueueError::Draining)
    );
    q.ack(r["stream"].as_str().unwrap(), "6-0").await.unwrap();
    closing.await.unwrap().unwrap();
    assert!(!b.state.lock().unwrap().closed);
}
#[tokio::test]
async fn drain_timeout_does_not_ack_and_owned_backend_closes() {
    let b = Arc::new(Backend::default());
    b.push("7-0", 0);
    let q = MessageQueueTransport::with_backend(
        &service(json!({"drain_timeout_ms":1})),
        b.clone(),
        true,
    )
    .unwrap();
    q.consume("normal", "w", 1, None).await.unwrap();
    assert_eq!(q.close().await, Err(QueueError::DrainTimedOut));
    let x = b.state.lock().unwrap();
    assert!(x.closed);
    assert!(x.pending.contains_key("7-0"));
}
#[tokio::test]
async fn drain_interrupts_long_poll_and_never_reopens_closed_backend() {
    let b = Arc::new(Backend::default());
    let q = Arc::new(queue(b));
    let poll = {
        let q = q.clone();
        tokio::spawn(async move { q.consume("normal", "w", 1, Some(30000)).await })
    };
    tokio::time::sleep(Duration::from_millis(5)).await;
    tokio::time::timeout(Duration::from_millis(200), q.close())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(poll.await.unwrap(), Err(QueueError::Draining));
    assert_eq!(
        q.publish(json!({}), "normal").await,
        Err(QueueError::Draining)
    );
}
#[tokio::test]
async fn invalid_config_payload_and_unknown_settlement_fail_closed() {
    let b = Arc::new(Backend::default());
    assert!(
        MessageQueueTransport::with_backend(&service(json!({"max_inflight":0})), b.clone(), false)
            .is_err()
    );
    let q = MessageQueueTransport::with_backend(&service(json!({"max_payload_bytes":2})), b, false)
        .unwrap();
    assert_eq!(
        q.publish(json!({"x":1}), "normal").await,
        Err(QueueError::InvalidValue)
    );
    assert_eq!(
        q.fail("other", "missing", "error").await,
        Err(QueueError::StaleDelivery)
    );
    let q = MessageQueueTransport::new_handle(&service(json!({"redis_url":"invalid"}))).unwrap();
    assert_eq!(
        q.publish(json!({}), "normal").await,
        Err(QueueError::InvalidValue)
    );
}

#[tokio::test]
async fn generic_rest_ack_body_reaches_delivery_settlement_and_shutdown_hook() {
    let b = Arc::new(Backend::default());
    b.push("8-0", 0);
    let q = MessageQueueTransport::with_backend(&service(json!({})), b.clone(), true).unwrap();
    let r = q
        .request(RestRequest {
            method: "GET".into(),
            endpoint: "/consume".into(),
            body: json!({"consumer":"handler","count":1}),
        })
        .await
        .unwrap();
    let RestResponse::Json(r) = r else {
        panic!("json response expected")
    };
    assert_eq!(
        q.request(RestRequest {
            method: "POST".into(),
            endpoint: "/ack".into(),
            body: json!({"stream":r["stream"],"message_id":r["message_id"]})
        })
        .await
        .unwrap(),
        RestResponse::Json(json!({"acked":1}))
    );
    q.shutdown().await.unwrap();
    assert!(b.state.lock().unwrap().closed);
}

#[tokio::test]
async fn receipt_settles_on_another_replica_and_reaps_original_capacity() {
    let b = Arc::new(Backend::default());
    b.push("9-0", 0);
    let a =
        MessageQueueTransport::with_backend(&service(json!({"max_inflight":1})), b.clone(), false)
            .unwrap();
    let other = queue(b.clone());
    let r = a.consume("normal", "worker", 1, None).await.unwrap();
    let stream = r["stream"].as_str().unwrap();
    let receipt = r["delivery_receipt"].as_str().unwrap();
    assert_eq!(
        other.ack_receipt(stream, "9-0", receipt).await.unwrap(),
        json!({"acked":1})
    );
    assert_eq!(
        other.ack_receipt(stream, "9-0", receipt).await.unwrap(),
        json!({"acked":0})
    );
    a.publish(json!({}), "normal").await.unwrap();
    a.close().await.unwrap();
}
#[tokio::test]
async fn receipt_fail_reloads_durable_payload_and_is_idempotent_after_response_loss() {
    let b = Arc::new(Backend::default());
    b.push("10-0", 1);
    let a = queue(b.clone());
    let other = queue(b.clone());
    let r = a.consume("normal", "worker", 1, None).await.unwrap();
    b.state.lock().unwrap().lose_response = true;
    assert_eq!(
        other
            .fail_receipt(
                r["stream"].as_str().unwrap(),
                "10-0",
                "failed",
                r["delivery_receipt"].as_str().unwrap()
            )
            .await,
        Err(QueueError::Backend)
    );
    assert_eq!(
        other
            .fail_receipt(
                r["stream"].as_str().unwrap(),
                "10-0",
                "failed",
                r["delivery_receipt"].as_str().unwrap()
            )
            .await
            .unwrap(),
        json!({"already_settled":true})
    );
    {
        let x = b.state.lock().unwrap();
        assert_eq!(x.added.len(), 1);
        assert_eq!(x.added[0].0, "queue:dlq:tasks");
        let f: HashMap<_, _> = x.added[0].1.clone().into_iter().collect();
        assert_eq!(f["business_id"], "order-7");
        assert_eq!(f["original_id"], "10-0");
    }
    a.close().await.unwrap();
}
#[tokio::test]
async fn receipt_forgery_wrong_binding_and_reclaimed_owner_fail_closed() {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let b = Arc::new(Backend::default());
    b.push("11-0", 0);
    let a = queue(b.clone());
    let other = queue(b.clone());
    let r = a.consume("normal", "worker", 1, None).await.unwrap();
    let s = r["stream"].as_str().unwrap();
    let receipt = r["delivery_receipt"].as_str().unwrap();
    assert_eq!(
        other.ack_receipt("other", "11-0", receipt).await,
        Err(QueueError::StaleDelivery)
    );
    let value: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(receipt).unwrap()).unwrap();
    for field in ["service", "group", "consumer", "deliveries"] {
        let mut forged = value.clone();
        forged[field] = if field == "deliveries" {
            json!(999)
        } else {
            json!("forged")
        };
        let forged = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged).unwrap());
        assert_eq!(
            other.ack_receipt(s, "11-0", &forged).await,
            Err(QueueError::StaleDelivery)
        );
    }
    b.state
        .lock()
        .unwrap()
        .pending
        .insert("11-0".into(), ("new-owner".into(), 2));
    assert_eq!(
        other.fail_receipt(s, "11-0", "stale", receipt).await,
        Err(QueueError::StaleDelivery)
    );
    assert!(b.state.lock().unwrap().added.is_empty());
    a.close().await.unwrap();
}

#[tokio::test]
async fn delayed_stale_settlement_does_not_remove_newer_local_lease() {
    let b = Arc::new(Backend::default());
    b.push("12-0", 0);
    let q = Arc::new(queue(b.clone()));
    let old = q.consume("normal", "old", 1, None).await.unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    *b.pause_settle.lock().unwrap() = Some((started.clone(), resume.clone()));
    let pending = {
        let q = q.clone();
        let stream = old["stream"].as_str().unwrap().to_owned();
        tokio::spawn(async move { q.fail(&stream, "12-0", "late").await })
    };
    started.notified().await;
    b.state
        .lock()
        .unwrap()
        .pending
        .insert("12-0".into(), ("other-owner".into(), 2));
    b.push("12-0", 0);
    let new = q.consume("normal", "new", 1, None).await.unwrap();
    resume.notify_one();
    assert_eq!(pending.await.unwrap(), Err(QueueError::StaleDelivery));
    assert_eq!(
        q.ack(new["stream"].as_str().unwrap(), "12-0")
            .await
            .unwrap(),
        json!({"acked":1})
    );
    q.close().await.unwrap();
}
