use super::*;
use crate::usage_recorder::{
    self, AccountingStage, FlushReport, PersistedUsage, RecorderConfig, StageStatus,
    UsagePersistence, UsageRecorder,
};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Mutex;
fn publication(tenant: &str, provider: &str, version: &str, rate: u64) -> Publication {
    Publication {
        tenant: tenant.into(),
        book_version: version.into(),
        provider: provider.into(),
        model: "same".into(),
        model_version: "revision-1".into(),
        currency: "USD".into(),
        entries: [Component::Input, Component::Output]
            .into_iter()
            .map(|component| PriceEntry {
                key: PriceKey {
                    book_version: version.into(),
                    provider: provider.into(),
                    model: "same".into(),
                    model_version: "revision-1".into(),
                    component,
                    currency: "USD".into(),
                },
                tenant: Some(tenant.into()),
                effective: EffectiveRange {
                    from_ms: 0,
                    to_ms: None,
                },
                unit: Unit::Tokens,
                units_per_rate: 1000,
                rate_nanos: rate,
            })
            .collect(),
        discounts: vec![],
    }
}
fn snapshot(publication: &Publication) -> Value {
    token_pair(
        &publication.validate(&publication.tenant, false).unwrap(),
        &publication.tenant,
        &Activation {
            provider: publication.provider.clone(),
            model: publication.model.clone(),
            model_version: publication.model_version.clone(),
            book_version: publication.book_version.clone(),
            currency: publication.currency.clone(),
            expected_revision: None,
        },
        1,
    )
    .unwrap()
}
fn record(tokens: i64) -> UsageRecord {
    let mut record = UsageRecord::new("a", "u", "same", chrono::Utc::now());
    record.provider = "p".into();
    record.request_id = "request-1".into();
    record.service_id = "service".into();
    record.request_type = "chat".into();
    record.status = "success".into();
    record.input_tokens = tokens;
    record
}
#[test]
fn publication_cannot_escape_tenant_or_composite_identity() {
    let mut p = publication("a", "p", "one", 1_000_000);
    assert!(p.validate("a", true).is_ok());
    assert!(matches!(
        p.validate("b", true),
        Err(PriceBookError::Forbidden)
    ));
    p.entries[0].tenant = None;
    assert!(matches!(
        p.validate("a", true),
        Err(PriceBookError::Forbidden)
    ));
    assert!(p.validate("a", false).is_ok());
    p.entries[0].tenant = Some("foreign".into());
    assert!(matches!(
        p.validate("a", false),
        Err(PriceBookError::Forbidden)
    ));
    p.entries[0].tenant = Some("a".into());
    p.entries[0].key.provider = "same-name-other-provider".into();
    assert!(matches!(
        p.validate("a", true),
        Err(PriceBookError::Forbidden)
    ));
}
#[test]
fn distinct_providers_and_book_versions_never_overwrite_each_other() {
    let a = publication("a", "p", "one", 1_000_000);
    let b = publication("a", "other", "one", 2_000_000);
    let next = publication("a", "p", "two", 3_000_000);
    assert_ne!(a.reference().unwrap(), b.reference().unwrap());
    assert_ne!(a.reference().unwrap(), next.reference().unwrap());
    assert_ne!(
        snapshot(&a)["price_book_input"]["entry_reference"],
        snapshot(&b)["price_book_input"]["entry_reference"]
    );
    assert_ne!(
        snapshot(&a)["price_book_input"]["entry_reference"],
        snapshot(&next)["price_book_input"]["entry_reference"]
    );
}
#[test]
fn legacy_receipt_verifies_and_v2_envelope_cannot_be_stripped() {
    let r = record(1000);
    let old = crate::pricing_receipt::build_pricing_snapshot(
        "a",
        "p",
        "same",
        "0.001",
        "0.002",
        "tenant_model",
    )
    .unwrap()
    .to_value();
    assert!(validate_bound_snapshot(&r, &old).is_ok());
    let mut v2 = snapshot(&publication("a", "p", "one", 1_000_000));
    assert!(validate_bound_snapshot(&r, &v2).is_ok());
    v2.as_object_mut().unwrap().remove("price_book_schema");
    assert_eq!(
        validate_bound_snapshot(&r, &v2),
        Err(PriceBookError::InvalidReference)
    );
}
#[test]
fn cumulative_transition_replay_and_identity_change_fail_closed() {
    let snapshot = snapshot(&publication("a", "p", "one", 1_000_000));
    let mut initial = record(1000);
    usage_recorder::prepare_with_snapshot(&mut initial, &snapshot).unwrap();
    let first = usage_recorder::stage_receipts::v2_cumulative_delta(&initial, None)
        .unwrap()
        .unwrap();
    assert_eq!(first.input_cost_cents, 1000);
    let mut retry = initial.clone();
    retry.timestamp = chrono::Utc::now();
    assert!(
        usage_recorder::stage_receipts::v2_cumulative_delta(&retry, Some(&initial))
            .unwrap()
            .is_none()
    );
    retry.input_tokens = 2000;
    usage_recorder::prepare_with_snapshot(&mut retry, &snapshot).unwrap();
    let delta = usage_recorder::stage_receipts::v2_cumulative_delta(&retry, Some(&initial))
        .unwrap()
        .unwrap();
    assert_eq!(delta.input_tokens, 1000);
    assert_eq!(delta.input_cost_cents, 1000);
    retry.provider = "other".into();
    assert!(usage_recorder::stage_receipts::v2_cumulative_delta(&retry, Some(&initial)).is_err());
}
struct Frozen(Value);
impl UsageSnapshotResolver for Frozen {
    fn resolve<'a>(&'a self, _: &'a UsageRecord) -> UsageFuture<'a, Value> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}
#[derive(Default)]
struct DurableFake {
    seen: AtomicBool,
    checkpoints: Mutex<HashMap<(usage_recorder::UsageIdentity, String), UsageRecord>>,
    charged: Mutex<(i64, i64)>,
}
impl UsagePersistence for DurableFake {
    fn upsert_raw<'a>(
        &'a self,
        records: &'a [UsageRecord],
    ) -> UsageFuture<'a, Vec<PersistedUsage>> {
        Box::pin(async move {
            Ok(records
                .iter()
                .map(|r| PersistedUsage {
                    inserted: !self.seen.swap(true, Ordering::SeqCst),
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
        stage: AccountingStage,
        records: &'a [UsageRecord],
    ) -> UsageFuture<'a, StageStatus> {
        Box::pin(async move {
            if stage == AccountingStage::RequestTraces {
                return Ok(StageStatus::Skipped);
            }
            let mut checkpoints = self.checkpoints.lock().await;
            for r in records {
                let current: UsageRecord =
                    serde_json::from_value(r.metadata["_price_book_v2_cumulative"].clone())
                        .unwrap();
                let key = (current.identity().unwrap(), stage.name().to_owned());
                let delta = usage_recorder::stage_receipts::v2_cumulative_delta(
                    &current,
                    checkpoints.get(&key),
                )?;
                let mut next = checkpoints
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| current.clone());
                usage_recorder::merge_idempotent_record(&mut next, &current);
                checkpoints.insert(key, next);
                if stage == AccountingStage::QuotaCounters {
                    if let Some(delta) = delta {
                        let mut charged = self.charged.lock().await;
                        charged.0 += delta.input_tokens;
                        charged.1 += delta.input_cost_cents;
                    }
                }
            }
            Ok(StageStatus::Applied)
        })
    }
    fn reconcile<'a>(
        &'a self,
        _: AccountingStage,
        _: &'a [UsageRecord],
        _: &'a UsageRecorderError,
    ) -> UsageFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
fn assert_flushed(report: Option<FlushReport>) {
    let report = report.unwrap();
    assert!(report.raw_error.is_none());
    assert!(report.stage_failures.is_empty());
}
#[tokio::test]
async fn recorder_restart_replay_charges_only_cumulative_increment() {
    let persistence = Arc::new(DurableFake::default());
    let frozen = snapshot(&publication("a", "p", "one", 1_000_000));
    let resolver = Arc::new(Frozen(frozen.clone()));
    let first = UsageRecorder::new(
        Some(persistence.clone()),
        RecorderConfig {
            buffer_size: 1,
            ..Default::default()
        },
    );
    assert_flushed(
        first
            .record_with_resolver(record(1000), resolver.clone())
            .await
            .unwrap(),
    );
    // A new recorder has no in-memory observations. The durable checkpoint is
    // shared by the actual production algorithm and this persistence contract.
    let restarted = UsageRecorder::new(
        Some(persistence.clone()),
        RecorderConfig {
            buffer_size: 1,
            ..Default::default()
        },
    );
    assert_flushed(
        restarted
            .record_with_resolver(record(1000), resolver.clone())
            .await
            .unwrap(),
    );
    assert_flushed(
        restarted
            .record_with_resolver(record(2000), resolver)
            .await
            .unwrap(),
    );
    assert_eq!(*persistence.charged.lock().await, (2000, 2000));
}
#[tokio::test]
async fn recorder_retains_first_v2_snapshot_after_publication_changes() {
    let persistence = Arc::new(DurableFake::default());
    let recorder = UsageRecorder::new(
        Some(persistence.clone()),
        RecorderConfig {
            buffer_size: 1,
            ..Default::default()
        },
    );
    let old = snapshot(&publication("a", "p", "one", 1_000_000));
    let new = snapshot(&publication("a", "p", "two", 9_000_000));
    assert_flushed(
        recorder
            .record_with_resolver(record(1000), Arc::new(Frozen(old)))
            .await
            .unwrap(),
    );
    assert_flushed(
        recorder
            .record_with_resolver(record(2000), Arc::new(Frozen(new)))
            .await
            .unwrap(),
    );
    assert_eq!(*persistence.charged.lock().await, (2000, 2000));
}
