//! Native recorder shutdown guarantees corresponding to old interceptor flush ownership.
//! Uses real recorder enqueue/flush/stop; persistence is a recording, gated collaborator.
use ai_platform_gateway_foundation::usage_recorder::*;
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, Semaphore};
struct Persistence {
    records: Mutex<Vec<UsageRecord>>,
    calls: AtomicUsize,
    gate: Option<Semaphore>,
    entered: Notify,
}
impl Persistence {
    fn new(block: bool) -> Arc<Self> {
        Arc::new(Self {
            records: Mutex::new(vec![]),
            calls: AtomicUsize::new(0),
            gate: block.then(|| Semaphore::new(0)),
            entered: Notify::new(),
        })
    }
}
impl UsagePersistence for Persistence {
    fn upsert_raw<'a>(
        &'a self,
        records: &'a [UsageRecord],
    ) -> UsageFuture<'a, Vec<PersistedUsage>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                self.entered.notify_one();
                if let Some(gate) = &self.gate {
                    gate.acquire().await.unwrap().forget();
                }
            }
            self.records.lock().unwrap().extend_from_slice(records);
            Ok(records
                .iter()
                .map(|r| PersistedUsage {
                    inserted: true,
                    tenant_id: r.tenant_id.clone(),
                    request_id_key: r.request_id.clone(),
                    service_id_key: r.service_id.clone(),
                    request_type_key: r.request_type.clone(),
                })
                .collect())
        })
    }
    fn post_write<'a>(
        &'a self,
        _: AccountingStage,
        _: &'a [UsageRecord],
    ) -> UsageFuture<'a, StageStatus> {
        Box::pin(async { Ok(StageStatus::Applied) })
    }
    fn reconcile<'a>(
        &'a self,
        _: AccountingStage,
        _: &'a [UsageRecord],
        _: &'a UsageRecorderError,
    ) -> UsageFuture<'a, ()> {
        panic!("successful fixture must not require reconciliation")
    }
}
fn usage(i: usize) -> UsageRecord {
    let mut r = UsageRecord::new("t", "u", "m", chrono::Utc::now());
    r.input_tokens = 1;
    r.output_tokens = 1;
    r.request_id = format!("req-{i}");
    r.service_id = "svc".into();
    r
}
fn recorder(persistence: Arc<Persistence>) -> Arc<UsageRecorder> {
    Arc::new(UsageRecorder::new(
        Some(persistence),
        RecorderConfig {
            buffer_size: 1000,
            flush_interval: Duration::from_secs(60),
            ..Default::default()
        },
    ))
}
#[tokio::test]
async fn buffered_records_are_flushed_in_order_on_stop() {
    let persistence = Persistence::new(false);
    let recorder = recorder(persistence.clone());
    for i in 0..5 {
        assert!(recorder.record_priced(usage(i)).await.is_none());
    }
    assert_eq!(recorder.buffered_len().await, 5);
    let report = recorder.stop().await;
    assert_eq!(report.accepted, 5);
    let records = persistence.records.lock().unwrap();
    assert_eq!(
        records
            .iter()
            .map(|r| r.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["req-0", "req-1", "req-2", "req-3", "req-4"]
    );
}
#[tokio::test]
async fn buffer_flush_preserves_billing_metadata_on_stop() {
    let persistence = Persistence::new(false);
    let recorder = recorder(persistence.clone());
    let mut record = usage(7);
    record.tenant_id = "tenant-a".into();
    record.user_id = "user-a".into();
    record.service_id = "svc-a".into();
    record.model = "gemini-3-flash-preview".into();
    record.provider = "google".into();
    record.metadata=serde_json::from_value(json!({"source":"transparent_proxy_stream","token_source":"upstream","pricing_status":"catalog"})).unwrap();
    recorder.record_priced(record.clone()).await;
    recorder.stop().await;
    let records = persistence.records.lock().unwrap();
    assert_eq!(records.len(), 1);
    let row = &records[0];
    assert_eq!(
        (
            &row.tenant_id,
            &row.user_id,
            &row.service_id,
            &row.model,
            &row.provider
        ),
        (
            &record.tenant_id,
            &record.user_id,
            &record.service_id,
            &record.model,
            &record.provider
        )
    );
    assert_eq!(row.metadata["token_source"], "upstream");
}
#[tokio::test]
async fn stop_awaits_inflight_flush_and_repeated_stop_does_not_repeat_writes() {
    let persistence = Persistence::new(true);
    let recorder = recorder(persistence.clone());
    recorder.record_priced(usage(0)).await;
    let flushing = tokio::spawn({
        let recorder = recorder.clone();
        async move { recorder.flush().await }
    });
    persistence.entered.notified().await;
    let mut stopping = tokio::spawn({
        let recorder = recorder.clone();
        async move { recorder.stop().await }
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut stopping)
            .await
            .is_err()
    );
    persistence.gate.as_ref().unwrap().add_permits(1);
    assert_eq!(flushing.await.unwrap().accepted, 1);
    stopping.await.unwrap();
    recorder.stop().await;
    assert_eq!(recorder.buffered_len().await, 0);
    assert_eq!(persistence.records.lock().unwrap().len(), 1);
    assert_eq!(persistence.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn inflight_flush_then_final_flush_does_not_drop_later_records() {
    let persistence = Persistence::new(true);
    let recorder = recorder(persistence.clone());
    for i in 0..2 {
        recorder.record_priced(usage(i)).await;
    }
    let flushing = tokio::spawn({
        let recorder = recorder.clone();
        async move { recorder.flush().await }
    });
    persistence.entered.notified().await;
    let appending = tokio::spawn({
        let recorder = recorder.clone();
        async move {
            for i in 2..5 {
                recorder.record_priced(usage(i)).await;
            }
        }
    });
    persistence.gate.as_ref().unwrap().add_permits(1);
    flushing.await.unwrap();
    appending.await.unwrap();
    recorder.stop().await;
    assert_eq!(recorder.buffered_len().await, 0);
    assert_eq!(
        persistence
            .records
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.request_id.clone())
            .collect::<Vec<_>>(),
        (0..5).map(|i| format!("req-{i}")).collect::<Vec<_>>()
    );
    assert_eq!(persistence.calls.load(Ordering::SeqCst), 2);
}
