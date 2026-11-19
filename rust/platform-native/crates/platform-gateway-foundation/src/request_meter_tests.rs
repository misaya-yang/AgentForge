use super::*;
use crate::price_book::{EffectiveRange, PriceBook, PriceEntry};
use crate::usage_recorder::{prepare_with_snapshot, quota_adjustments};
fn receipt(rate_nanos: u64) -> PriceReceipt {
    let key = PriceKey {
        book_version: "book-r1".into(),
        provider: "local".into(),
        model: "score".into(),
        model_version: "weights-r1".into(),
        component: Component::Request,
        currency: "USD".into(),
    };
    let book = PriceBook::new(
        vec![PriceEntry {
            key: key.clone(),
            tenant: None,
            effective: EffectiveRange {
                from_ms: 0,
                to_ms: None,
            },
            unit: Unit::Requests,
            units_per_rate: 1,
            rate_nanos,
        }],
        vec![],
    )
    .unwrap();
    book.resolve("tenant", &key, 1).unwrap()
}
fn record() -> UsageRecord {
    let mut record = UsageRecord::new("tenant", "actor", "score", chrono::Utc::now());
    record.provider = "local".into();
    record.request_id = "invocation".into();
    record.service_id = "knowledge-model-plane".into();
    record.request_type = "knowledge_model".into();
    set_requests(&mut record, 1);
    record
}
#[test]
fn request_fee_is_exact_and_receipt_has_no_token_observations() {
    let receipt = receipt(2_500_000);
    let snapshot = snapshot(&receipt).unwrap();
    let mut record = record();
    prepare_with_snapshot(&mut record, &snapshot).unwrap();
    assert_eq!(record.input_cost_cents, 2500);
    assert_eq!(record.output_cost_cents, 0);
    assert_eq!(record.metadata["request_cost_nanos"], 2_500_000);
    assert_eq!(request_count(&record).unwrap(), Some(1));
    assert_eq!(
        measured(&record).unwrap(),
        Some(MeasuredUnits::Requests {
            requests: 1,
            tokens: TokenCountState::NotApplicable
        })
    );
    assert_eq!(
        quota_adjustments(&[record])
            .unwrap()
            .values()
            .next()
            .unwrap()
            .tokens,
        0
    );
}
#[test]
fn half_even_legacy_usd_conversion_is_not_request_as_token() {
    assert_eq!(quote_microcents(&receipt(500), 1).unwrap(), 0);
    assert_eq!(quote_microcents(&receipt(1500), 1).unwrap(), 2);
    assert_eq!(
        quote_microcents(&receipt(1_000_000_000), 1).unwrap(),
        1_000_000
    );
    let mut entry = receipt(1_499_500).entry;
    entry.units_per_rate = 1000;
    let key = entry.key.clone();
    let term = PriceBook::new(vec![entry], vec![])
        .unwrap()
        .resolve("tenant", &key, 1)
        .unwrap();
    assert_eq!(quote_microcents(&term, 1).unwrap(), 1); // no double rounding at nanounit boundary
}
#[test]
fn failed_or_interrupted_dispatch_cannot_invent_completed_units() {
    assert_eq!(
        observed_requests(Some(200), true),
        Some(MeasuredUnits::Requests {
            requests: 1,
            tokens: TokenCountState::NotApplicable
        })
    );
    for status in [None, Some(200), Some(400), Some(429), Some(500)] {
        assert_eq!(observed_requests(status, false), None)
    }
    assert_eq!(observed_requests(Some(500), true), None);
    assert_eq!(observed_requests(Some(400), true), None);
}
#[test]
fn wrong_actor_model_price_unit_or_unknown_token_state_fails_closed() {
    let snapshot = snapshot(&receipt(1_000_000)).unwrap();
    for i in 0..4 {
        let mut record = record();
        match i {
            0 => record.tenant_id = "foreign".into(),
            1 => record.model = "other".into(),
            2 => record.input_tokens = 1,
            _ => {
                record.metadata.insert(
                    METADATA_KEY.into(),
                    json!(MeasuredUnits::Requests {
                        requests: 1,
                        tokens: TokenCountState::Unknown
                    }),
                );
            }
        }
        assert!(prepare_with_snapshot(&mut record, &snapshot).is_err())
    }
    let mut tampered = snapshot.clone();
    tampered["unit"] = json!("per_1k_tokens");
    assert!(prepare_with_snapshot(&mut record(), &tampered).is_err());
    let mut tampered = snapshot;
    tampered["price_book_request"]["entry"]["unit"] = json!("tokens");
    assert!(prepare_with_snapshot(&mut record(), &tampered).is_err());
}
#[test]
fn new_component_keeps_existing_token_wire_names_unchanged() {
    assert_eq!(
        serde_json::to_value(Component::Input).unwrap(),
        json!("input")
    );
    assert_eq!(
        serde_json::to_value(Component::Request).unwrap(),
        json!("request")
    );
    let old = UsageRecord::new("t", "u", "m", chrono::Utc::now());
    assert_eq!(measured(&old).unwrap(), None);
}
#[tokio::test]
#[ignore = "requires both model-plane/request-meter successors in dedicated PostgreSQL test database"]
async fn real_existing_raw_writer_summary_and_cache_reader_accept_request_facts() {
    use crate::billing_store::{PgBillingStore, UsageFilter};
    use crate::usage_recorder::{PgUsagePersistence, UsagePersistence};
    let dsn = std::env::var("AGENTFORGE_KNOWLEDGE_MODEL_TEST_DSN")
        .expect("explicit dedicated database required");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect(&dsn)
        .await
        .unwrap();
    let tenant = format!("meter-test-{}", uuid::Uuid::new_v4());
    let mut record = record();
    record.tenant_id = tenant.clone();
    record.request_id = uuid::Uuid::new_v4().to_string();
    let mut price = receipt(2_500_000);
    price = PriceBook::new(vec![price.entry.clone()], vec![])
        .unwrap()
        .resolve(&tenant, &price.entry.key, 1)
        .unwrap();
    let snapshot = snapshot(&price).unwrap();
    prepare_with_snapshot(&mut record, &snapshot).unwrap();
    sqlx::query("INSERT INTO gateway.price_book_v2_usage_receipts(tenant_id,request_id,service_id,request_type,provider_id,model_id,snapshot) VALUES($1,$2,$3,$4,$5,$6,$7)").bind(&tenant).bind(&record.request_id).bind(&record.service_id).bind(&record.request_type).bind(&record.provider).bind(&record.model).bind(&snapshot).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO gateway.user_quotas(tenant_id,user_id) VALUES($1,$2)")
        .bind(&tenant)
        .bind(&record.user_id)
        .execute(&pool)
        .await
        .unwrap();
    let persistence = PgUsagePersistence::new(pool.clone());
    let mut invented_tokens = record.clone();
    invented_tokens.input_tokens = 1;
    assert!(persistence.upsert_raw(&[invented_tokens]).await.is_err());
    let mut free_cost = record.clone();
    free_cost.input_cost_cents = 0;
    assert!(persistence.upsert_raw(&[free_cost]).await.is_err());

    persistence.upsert_raw(&[record.clone()]).await.unwrap();
    persistence.upsert_raw(&[record.clone()]).await.unwrap();
    let row:(Option<i32>,Option<i32>,String,String,i32)=sqlx::query_as("SELECT input_tokens,output_tokens,meter_kind,token_count_state,input_cost_cents FROM gateway.usage_records WHERE tenant_id=$1 AND request_id=$2").bind(&tenant).bind(&record.request_id).fetch_one(&pool).await.unwrap();
    assert_eq!(
        row,
        (None, None, "requests".into(), "not_applicable".into(), 2500)
    );
    for _ in 0..2 {
        let mut transaction = pool.begin().await.unwrap();
        crate::knowledge_model_accounting::settle_in_transaction(&mut transaction, &record)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
    }
    let quantity:(i64,i32)=sqlx::query_as("SELECT current_daily_tokens,current_daily_requests FROM gateway.user_quotas WHERE tenant_id=$1 AND user_id=$2").bind(&tenant).bind(&record.user_id).fetch_one(&pool).await.unwrap();
    assert_eq!(quantity, (0, 1));
    let today = chrono::Utc::now().date_naive();
    let mut filter = UsageFilter::new(&tenant, today, today);
    filter.provider = Some("local".into());
    let summary = PgBillingStore::new(pool.clone())
        .try_usage_summary(&filter)
        .await
        .unwrap();
    #[derive(serde::Deserialize)]
    struct LegacySummary {
        total_requests: i64,
        total_input_tokens: i64,
        total_output_tokens: i64,
        total_tokens: i64,
        total_cost_usd: f64,
    }
    let legacy: LegacySummary = serde_json::from_value(summary.clone()).unwrap();
    assert_eq!(
        (
            legacy.total_requests,
            legacy.total_input_tokens,
            legacy.total_output_tokens,
            legacy.total_tokens
        ),
        (1, 0, 0, 0)
    );
    assert_eq!(legacy.total_cost_usd, 0.0025);
    assert_eq!(summary["total_requests"], 1);
    assert_eq!(summary["total_cost_usd"], json!(0.0025));
    assert_eq!(summary["metering"]["request_priced_invocations"], 1);
    assert_eq!(summary["metering"]["tokens_not_applicable_records"], 1);
    assert_eq!(summary["metering"]["request_priced_cost_nanos"], 2_500_000);
    filter.provider = None;
    let aggregate_summary = PgBillingStore::new(pool.clone())
        .try_usage_summary(&filter)
        .await
        .unwrap();
    assert_eq!(aggregate_summary["total_requests"], 1);
    assert_eq!(aggregate_summary["total_cost_usd"], json!(0.0025));
    assert_eq!(
        aggregate_summary["metering"]["request_priced_invocations"],
        1
    );
    let from = today
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    let cache = price_book::service::PgPriceBookService::new(pool.clone())
        .cache_metrics(&tenant, from, from + 86_400_000)
        .await
        .unwrap();
    assert!(cache.is_empty());
    // Repeated sub-cent request fees cannot bypass the model-plane ceiling
    // merely because the unchanged legacy quota stage rounds each to zero cents.
    for _ in 0..3 {
        let mut another = record.clone();
        another.request_id = uuid::Uuid::new_v4().to_string();
        persistence.upsert_raw(&[another]).await.unwrap();
    }
    sqlx::query("UPDATE gateway.user_quotas SET monthly_cost_limit_cents=1,current_monthly_cost_cents=0 WHERE tenant_id=$1 AND user_id=$2").bind(&tenant).bind(&record.user_id).execute(&pool).await.unwrap();
    let lease_id = uuid::Uuid::new_v4().to_string();
    let digest = "a".repeat(64);
    let token = json!({"lease_id":lease_id,"tenant_id":tenant,"user_id":record.user_id,"call_id":"over-budget","request_sha256":digest,"expires_at_unix":chrono::Utc::now().timestamp()+60});
    let body = json!({"scope":{"actor":{"tenant_id":tenant,"user_id":record.user_id}}});
    let rejected=sqlx::query_scalar::<_,Value>("SELECT gateway.reserve_knowledge_model_call($1,$2,'over-budget',$3,$4::uuid,$5,$6,'{}',$7,0,0,2500)").bind(&tenant).bind(&record.user_id).bind(digest).bind(lease_id).bind(token).bind(body).bind(&snapshot).fetch_one(&pool).await;
    assert!(rejected.is_err());
    // Immutable price receipts/checkpoints intentionally remain as test evidence
    // in this explicitly disposable database; only this test tenant's raw row is removed.
    sqlx::query("DELETE FROM gateway.usage_records WHERE tenant_id=$1")
        .bind(&tenant)
        .execute(&pool)
        .await
        .unwrap();
}

#[test]
fn receipt_wire_explicitly_distinguishes_not_applicable_and_unknown() {
    use ai_platform_service_primitives::knowledge_model::{
        ModelIdentity, ModelMetering, UsageReceipt,
    };
    let mut receipt = UsageReceipt {
        receipt_id: "r".into(),
        call_id: "c".into(),
        lease_id: "l".into(),
        tenant_id: "t".into(),
        user_id: "u".into(),
        provider_id: "local".into(),
        model: ModelIdentity {
            provider: "local".into(),
            model: "score".into(),
            revision: "r1".into(),
        },
        price_book_revision: "book".into(),
        units: std::collections::BTreeMap::new(),
        settled: false,
        metering: None,
    };
    assert!(
        serde_json::to_value(&receipt)
            .unwrap()
            .get("metering")
            .is_none()
    );
    receipt.metering = Some(ModelMetering {
        kind: MeterKind::Requests,
        tokens: TokenCountState::Unknown,
        cost_nanos: None,
    });
    assert_eq!(
        serde_json::to_value(&receipt).unwrap()["metering"]["tokens"],
        "unknown"
    );
    receipt.units.insert("requests".into(), 1);
    receipt.settled = true;
    receipt.metering = Some(ModelMetering {
        kind: MeterKind::Requests,
        tokens: TokenCountState::NotApplicable,
        cost_nanos: Some(2_500_000),
    });
    let value = serde_json::to_value(&receipt).unwrap();
    assert_eq!(value["units"], json!({"requests":1}));
    assert_eq!(value["metering"]["tokens"], "not_applicable");
    assert_eq!(value["metering"]["cost_nanos"], 2_500_000);
}
