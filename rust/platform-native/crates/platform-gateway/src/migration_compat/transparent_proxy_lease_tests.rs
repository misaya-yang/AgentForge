//! Service-free lease-loss regressions. Reqwest bodies are in-memory streams;
//! the only store is a deterministic CapacityStore implementation, never Redis.
use super::*;
use crate::capacity_admission::{
    AdmissionOptions, CapacityAcquire, CapacityPair, CapacityStore, CapacityStoreFuture,
};
use crate::usage_recorder::{
    RecorderConfig, UsageFuture, UsageRecorderError, UsageSnapshotResolver,
};
use futures_core::Stream;
use std::{
    collections::VecDeque,
    pin::Pin,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    task::{Context, Poll},
};
use tokio::sync::{Notify, mpsc};

const BOUND: Duration = Duration::from_secs(3);

#[derive(Default)]
struct LossStore {
    lose: AtomicBool,
    acquired: AtomicUsize,
    released: AtomicUsize,
}
impl CapacityStore for LossStore {
    fn acquire<'a>(&'a self, _: &'a CapacityAcquire) -> CapacityStoreFuture<'a, (i64, i64)> {
        Box::pin(async move {
            self.acquired.fetch_add(1, Ordering::SeqCst);
            Ok((0, 0))
        })
    }
    fn renew<'a>(
        &'a self,
        _: &'a CapacityPair,
        _: i64,
        _: i64,
        _: i64,
    ) -> CapacityStoreFuture<'a, bool> {
        Box::pin(async move { Ok(!self.lose.load(Ordering::SeqCst)) })
    }
    fn release<'a>(&'a self, _: &'a CapacityPair) -> CapacityStoreFuture<'a, ()> {
        Box::pin(async move {
            self.released.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
struct Fixture {
    store: Arc<LossStore>,
    controller: Arc<CapacityAdmissionController>,
    budget: CapacityBudget,
}
impl Fixture {
    fn new() -> Self {
        let store = Arc::new(LossStore::default());
        let controller = Arc::new(
            CapacityAdmissionController::new(
                AdmissionOptions {
                    lease_ttl_ms: 1000,
                    per_tenant_default_share: 1.0,
                    ..Default::default()
                },
                Some(store.clone()),
            )
            .unwrap(),
        );
        let budget = CapacityBudget {
            key: "proxy-lease-test".into(),
            limit: 1,
            queue_max: 1,
            queue_timeout_ms: 1000,
            scope: "upstream".into(),
            source: "test".into(),
            enforced: true,
            source_status: "real".into(),
            shared: true,
        };
        Self {
            store,
            controller,
            budget,
        }
    }
    async fn lease(&self) -> CapacityLease {
        bounded(self.controller.acquire(
            std::slice::from_ref(&self.budget),
            &AdmissionRequest {
                tenant_id: "tenant".into(),
                ..Default::default()
            },
        ))
        .await
        .unwrap()
    }
    fn lose(&self) {
        self.store.lose.store(true, Ordering::SeqCst);
    }
    async fn assert_released(&self) {
        wait_until(|| {
            self.store.released.load(Ordering::SeqCst) == 1
                && self.controller.snapshot()[&self.budget.key].inflight == 0
        })
        .await;
        assert_eq!(self.controller.release_failures(), 0);
    }
}
async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(BOUND, future)
        .await
        .expect("bounded regression timed out")
}
async fn wait_until(check: impl Fn() -> bool) {
    bounded(async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await;
}
fn request(budgets: Vec<CapacityBudget>) -> ProxyRequest {
    ProxyRequest {
        config: ProxyServiceConfig {
            service_id: "service".into(),
            ..Default::default()
        },
        user: CompatUser {
            tenant_id: "tenant".into(),
            request_id: "request".into(),
            ..Default::default()
        },
        api_key_id: None,
        method: Method::POST,
        path: "/runs/stream".into(),
        body: Bytes::from_static(b"{}"),
        headers: HeaderMap::new(),
        query: vec![],
        stream: true,
        langgraph_body_prepared: true,
        effective_model: None,
        effective_provider: None,
        capacity_budgets: budgets,
    }
}
fn connection(proxy: &TransparentProxy) -> ConnectionLease {
    let key = ("service".into(), "in-memory".into());
    proxy
        .balance
        .lock()
        .unwrap()
        .connections
        .insert(key.clone(), 1);
    ConnectionLease {
        balance: proxy.balance.clone(),
        key,
    }
}
fn assert_connection_released(proxy: &TransparentProxy) {
    assert_eq!(
        proxy.balance.lock().unwrap().connections[&("service".into(), "in-memory".into())],
        0
    );
}
#[derive(Default)]
struct BodyProbe {
    polls: AtomicUsize,
    drops: AtomicUsize,
}
struct TestBody {
    chunks: VecDeque<Result<Bytes, std::io::Error>>,
    pending: bool,
    probe: Arc<BodyProbe>,
}
impl Stream for TestBody {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.probe.polls.fetch_add(1, Ordering::SeqCst);
        if let Some(chunk) = self.chunks.pop_front() {
            Poll::Ready(Some(chunk))
        } else if self.pending {
            Poll::Pending
        } else {
            Poll::Ready(None)
        }
    }
}
impl Drop for TestBody {
    fn drop(&mut self) {
        self.probe.drops.fetch_add(1, Ordering::SeqCst);
    }
}
fn response(
    chunks: Vec<Result<Bytes, std::io::Error>>,
    pending: bool,
) -> (reqwest::Response, Arc<BodyProbe>) {
    let probe = Arc::new(BodyProbe::default());
    let body = reqwest::Body::wrap_stream(TestBody {
        chunks: chunks.into(),
        pending,
        probe: probe.clone(),
    });
    let response = axum::http::Response::builder()
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap();
    (reqwest::Response::from(response), probe)
}

#[tokio::test]
async fn loss_before_bind_rejects_without_polling_work() {
    let fixture = Fixture::new();
    let lease = fixture.lease().await;
    fixture.lose();
    bounded(lease.wait_lost()).await;
    let error = CapacityOwner::bind(Some(&lease))
        .err()
        .expect("lost lease must not bind");
    assert_eq!(error.status, 503);
    assert_eq!(error.detail["code"], "GATEWAY_CAPACITY_LEASE_LOST");
    assert_eq!(error.detail["budget_key"], fixture.budget.key);
    bounded(lease.release()).await;
    fixture.assert_released().await;
}

#[tokio::test]
async fn already_signalled_loss_wins_without_starting_ready_work() {
    let owner = CapacityOwner::bind(None).unwrap();
    owner.cancellation.cancel();
    let polled = AtomicBool::new(false);
    assert!(
        owner
            .run(async {
                polled.store(true, Ordering::SeqCst);
                42
            })
            .await
            .is_err()
    );
    assert!(!polled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn completed_response_survives_later_loss_and_unrelated_owner_is_not_cancelled() {
    let owner = CapacityOwner::bind(None).unwrap();
    let unrelated = AdmissionCancellation::default();
    let response = owner
        .run(async { Bytes::from_static(b"terminal response") })
        .await
        .unwrap();
    owner.cancellation.cancel();
    assert_eq!(response, Bytes::from_static(b"terminal response"));
    assert!(!unrelated.is_cancelled());
}

#[tokio::test]
async fn actual_proxy_queue_is_fenced_before_any_http_send() {
    let fixture = Fixture::new();
    let proxy = Arc::new(TransparentProxy {
        capacity: Some(fixture.controller.clone()),
        ..Default::default()
    });
    // A fresh cached availability record avoids health requests. A permanently
    // full local slot proves no network send is entered in this regression.
    proxy.availability.lock().await.insert(
        "service".into(),
        Availability {
            checked: Instant::now(),
            urls: vec!["http://unused.invalid".into()],
            error: String::new(),
        },
    );
    proxy
        .slots
        .lock()
        .unwrap()
        .insert(("service".into(), true), Arc::new(Semaphore::new(0)));
    fixture.lose();
    let result = bounded(proxy.proxy(request(vec![fixture.budget.clone()]))).await;
    let error = result
        .err()
        .expect("lease loss must exit the 30-second slot queue");
    assert_eq!(error.status, 503);
    assert_eq!(error.detail["code"], "GATEWAY_CAPACITY_LEASE_LOST");
    assert!(proxy.balance.lock().unwrap().connections.is_empty());
    fixture.assert_released().await;
}

#[tokio::test]
async fn pending_send_future_is_dropped_on_real_renewal_loss() {
    // Sending itself cannot be exercised without a transport. This tests the
    // same production fence with an owned, instrumented pending send future.
    let fixture = Fixture::new();
    let lease = fixture.lease().await;
    let owner = CapacityOwner::bind(Some(&lease)).unwrap();
    let proxy = Arc::new(TransparentProxy::default());
    let connection = connection(&proxy);
    let semaphore = Arc::new(Semaphore::new(1));
    let permit = semaphore.clone().acquire_owned().await.unwrap();
    let entered = Arc::new(AtomicBool::new(false));
    let signal = entered.clone();
    let task = tokio::spawn(async move {
        owner
            .run(async move {
                let _owners = (lease, connection, permit);
                signal.store(true, Ordering::SeqCst);
                std::future::pending::<()>().await;
            })
            .await
    });
    wait_until(|| entered.load(Ordering::SeqCst)).await;
    fixture.lose();
    assert_eq!(
        bounded(task).await.unwrap().unwrap_err().detail["code"],
        "GATEWAY_CAPACITY_LEASE_LOST"
    );
    assert_eq!(semaphore.available_permits(), 1);
    assert_connection_released(&proxy);
    fixture.assert_released().await;
}

#[tokio::test]
async fn non_stream_reqwest_body_read_is_dropped_on_loss() {
    let fixture = Fixture::new();
    let lease = fixture.lease().await;
    let owner = CapacityOwner::bind(Some(&lease)).unwrap();
    let (response, probe) = response(vec![], true);
    let task = tokio::spawn(async move {
        owner
            .run(async move {
                let result = response.bytes().await;
                lease.release().await;
                result
            })
            .await
    });
    wait_until(|| probe.polls.load(Ordering::SeqCst) > 0).await;
    fixture.lose();
    assert!(bounded(task).await.unwrap().is_err());
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    fixture.assert_released().await;
}

async fn pump_loss_case(backpressure: bool, upstream_error: bool) {
    let fixture = Fixture::new();
    let lease = fixture.lease().await;
    let owner = CapacityOwner::bind(Some(&lease)).unwrap();
    let proxy = Arc::new(TransparentProxy::default());
    let connection = connection(&proxy);
    let semaphore = Arc::new(Semaphore::new(1));
    let permit = semaphore.clone().acquire_owned().await.unwrap();
    let chunks = if upstream_error {
        vec![Err(std::io::Error::other("in-memory upstream failure"))]
    } else if backpressure {
        vec![Ok(Bytes::from_static(b"data: waiting\n\n"))]
    } else {
        vec![]
    };
    let (response, probe) = response(chunks, true);
    let (tx, mut rx) = mpsc::channel(1);
    if backpressure {
        tx.send(Ok(Bytes::from_static(b"buffered before loss")))
            .await
            .unwrap();
    }
    let task = tokio::spawn(proxy.clone().pump_stream(
        request(vec![]),
        response,
        permit,
        connection,
        Some(lease),
        owner,
        tx,
    ));
    wait_until(|| probe.polls.load(Ordering::SeqCst) > 0).await;
    fixture.lose();
    bounded(task).await.unwrap();
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert_eq!(semaphore.available_permits(), 1);
    assert_connection_released(&proxy);
    if backpressure {
        assert_eq!(
            bounded(rx.recv()).await.unwrap().unwrap(),
            Bytes::from_static(b"buffered before loss")
        );
    }
    assert!(
        bounded(rx.recv()).await.is_none(),
        "no post-loss data or success frame may be enqueued"
    );
    fixture.assert_released().await;
}
#[tokio::test]
async fn actual_stream_pump_pending_read_is_fenced() {
    pump_loss_case(false, false).await;
}
#[tokio::test]
async fn actual_stream_pump_blocked_delivery_is_fenced() {
    pump_loss_case(true, false).await;
}
#[tokio::test]
async fn actual_stream_pump_blocked_error_delivery_is_fenced() {
    pump_loss_case(true, true).await;
}

#[tokio::test]
async fn normal_pump_preserves_bytes_and_releases_once() {
    let fixture = Fixture::new();
    let lease = fixture.lease().await;
    let owner = CapacityOwner::bind(Some(&lease)).unwrap();
    let proxy = Arc::new(TransparentProxy::default());
    let connection = connection(&proxy);
    let semaphore = Arc::new(Semaphore::new(1));
    let permit = semaphore.clone().acquire_owned().await.unwrap();
    let bytes = Bytes::from_static(b"data: first\n\ndata: [DONE]\n\n");
    let (response, probe) = response(vec![Ok(bytes.clone())], false);
    let (tx, mut rx) = mpsc::channel(8);
    bounded(proxy.clone().pump_stream(
        request(vec![]),
        response,
        permit,
        connection,
        Some(lease),
        owner,
        tx,
    ))
    .await;
    assert_eq!(bounded(rx.recv()).await.unwrap().unwrap(), bytes);
    assert!(bounded(rx.recv()).await.is_none());
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert_eq!(semaphore.available_permits(), 1);
    assert_connection_released(&proxy);
    fixture.assert_released().await;
}

#[derive(Default)]
struct GatedPricing {
    entered: AtomicBool,
    finished: AtomicBool,
    finish: Notify,
}
impl UsageSnapshotResolver for GatedPricing {
    fn resolve<'a>(&'a self, _: &'a UsageRecord) -> UsageFuture<'a, Value> {
        Box::pin(async move {
            self.entered.store(true, Ordering::SeqCst);
            self.finish.notified().await;
            self.finished.store(true, Ordering::SeqCst);
            Err(UsageRecorderError::Collaborator(
                "bounded test pricing failure".into(),
            ))
        })
    }
}
#[tokio::test]
async fn pending_accounting_does_not_retain_proxy_resources_after_loss() {
    let fixture = Fixture::new();
    let lease = fixture.lease().await;
    let owner = CapacityOwner::bind(Some(&lease)).unwrap();
    let pricing = Arc::new(GatedPricing::default());
    let recorder = Arc::new(UsageRecorder::new(None, RecorderConfig::default()));
    let proxy = Arc::new(TransparentProxy {
        usage: Some(recorder.clone()),
        pricing: Some(pricing.clone()),
        ..Default::default()
    });
    let connection = connection(&proxy);
    let semaphore = Arc::new(Semaphore::new(1));
    let permit = semaphore.clone().acquire_owned().await.unwrap();
    let (response, probe) = response(vec![], false);
    let (tx, mut rx) = mpsc::channel(8);
    let task = tokio::spawn(proxy.clone().pump_stream(
        request(vec![]),
        response,
        permit,
        connection,
        Some(lease),
        owner,
        tx,
    ));
    wait_until(|| pricing.entered.load(Ordering::SeqCst)).await;
    fixture.lose();
    bounded(task).await.unwrap();
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert_eq!(semaphore.available_permits(), 1);
    assert_connection_released(&proxy);
    assert!(bounded(rx.recv()).await.is_none());
    fixture.assert_released().await;
    // Recorder work was already independently owned before loss. It is allowed
    // to finish; cancellation of its waiter is not proof billing was cancelled.
    assert!(!pricing.finished.load(Ordering::SeqCst));
    pricing.finish.notify_one();
    wait_until(|| pricing.finished.load(Ordering::SeqCst)).await;
    assert_eq!(bounded(recorder.buffered_len()).await, 0);
}

#[tokio::test]
async fn loss_in_handoff_drops_unstarted_pump_without_polling_upstream() {
    let fixture = Fixture::new();
    let lease = fixture.lease().await;
    let owner = CapacityOwner::bind(Some(&lease)).unwrap();
    fixture.lose();
    bounded(lease.wait_lost()).await;
    let proxy = Arc::new(TransparentProxy::default());
    let connection = connection(&proxy);
    let semaphore = Arc::new(Semaphore::new(1));
    let permit = semaphore.clone().acquire_owned().await.unwrap();
    let (response, probe) = response(vec![Ok(Bytes::from_static(b"must not forward"))], false);
    let (tx, mut rx) = mpsc::channel(8);
    bounded(proxy.clone().pump_stream(
        request(vec![]),
        response,
        permit,
        connection,
        Some(lease),
        owner,
        tx,
    ))
    .await;
    assert_eq!(probe.polls.load(Ordering::SeqCst), 0);
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert!(bounded(rx.recv()).await.is_none());
    assert_eq!(semaphore.available_permits(), 1);
    assert_connection_released(&proxy);
    fixture.assert_released().await;
}

#[tokio::test]
async fn aborting_pump_still_drops_local_owners_and_releases_capacity() {
    let fixture = Fixture::new();
    let lease = fixture.lease().await;
    let owner = CapacityOwner::bind(Some(&lease)).unwrap();
    let proxy = Arc::new(TransparentProxy::default());
    let connection = connection(&proxy);
    let semaphore = Arc::new(Semaphore::new(1));
    let permit = semaphore.clone().acquire_owned().await.unwrap();
    let (response, probe) = response(vec![], true);
    let (tx, mut rx) = mpsc::channel(8);
    let task = tokio::spawn(proxy.clone().pump_stream(
        request(vec![]),
        response,
        permit,
        connection,
        Some(lease),
        owner,
        tx,
    ));
    wait_until(|| probe.polls.load(Ordering::SeqCst) > 0).await;
    task.abort();
    assert!(bounded(task).await.unwrap_err().is_cancelled());
    assert_eq!(probe.drops.load(Ordering::SeqCst), 1);
    assert!(bounded(rx.recv()).await.is_none());
    assert_eq!(semaphore.available_permits(), 1);
    assert_connection_released(&proxy);
    fixture.assert_released().await;
}
