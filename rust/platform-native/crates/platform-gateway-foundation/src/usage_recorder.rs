//! Prepared usage buffering and the canonical PostgreSQL accounting writer.
//!
//! Source: ai_gateway_core/metrics/usage_recorder.py, especially record(),
//! _flush_buffer_locked(), _collapse_usage_records(), _write_records_batch(),
//! _accounting_adjustment(), and _record_reconciliation_event(). Uses migration
//! 058's existing compound request index; no new ledger or schema is introduced.
//!
//! Callers must normalize identity, bind it to authenticated request ownership,
//! validate/freeze pricing receipts, and calculate component microcent costs
//! before record_priced(). Prefer record_with_snapshot() for atomic first-receipt
//! selection, identity/hash validation and exact component microcent calculation.
//! Mutable price selection is an explicit lazy collaborator: it is called only
//! when no first buffered/flushed or supplied immutable receipt exists.
//! Once polled, record/flush/stop transfer ownership into runtime tasks: cancelling
//! a waiter does not cancel a write or blindly retry an uncertain commit. Process
//! termination and runtime shutdown still require an external durable intake.
//! Raw writes commit before independent best-effort accounting stages. Optional
//! trace/event-bus integrations are explicit collaborators, not silently recreated.
//! As in Python, prior observations are process-local: a conflict after restart
//! cannot reconstruct an accounting delta and does not charge the whole row again.
use crate::pricing_receipt::{PricingReceiptError, validate_pricing_snapshot};
use crate::{
    billing::{BillingError, MICROCENTS_PER_CENT, TokenRates},
    billing_store::{
        AccountingRecord, BillingStoreError, Granularity, apply_usage_aggregates_for_granularity,
    },
};
use chrono::{DateTime, Utc};
use rust_decimal::{Decimal, prelude::ToPrimitive};
use serde_json::{Map, Value, json};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, oneshot};

#[path = "usage_stage_receipts.rs"]
pub mod stage_receipts;

pub type UsageIdentity = (String, String, String, String);
pub type UsageFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, UsageRecorderError>> + Send + 'a>>;
#[derive(Debug)]
pub enum UsageRecorderError {
    Database(sqlx::Error),
    /// The server may have committed; retry requires explicit reconciliation.
    CommitUncertain(sqlx::Error),
    Billing(BillingStoreError),
    Receipt(PricingReceiptError),
    InvalidAccountingDelta,
    IntegerOverflow,
    Collaborator(String),
}
impl fmt::Display for UsageRecorderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(e) => e.fmt(f),
            Self::CommitUncertain(_) => f.write_str("usage_raw_commit_uncertain"),
            Self::Billing(e) => e.fmt(f),
            Self::Receipt(e) => e.fmt(f),
            Self::InvalidAccountingDelta => f.write_str("invalid_accounting_delta"),
            Self::IntegerOverflow => f.write_str("billing_integer_overflow"),
            Self::Collaborator(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for UsageRecorderError {}
impl From<sqlx::Error> for UsageRecorderError {
    fn from(e: sqlx::Error) -> Self {
        Self::Database(e)
    }
}
impl From<BillingStoreError> for UsageRecorderError {
    fn from(e: BillingStoreError) -> Self {
        Self::Billing(e)
    }
}
impl From<PricingReceiptError> for UsageRecorderError {
    fn from(error: PricingReceiptError) -> Self {
        Self::Receipt(error)
    }
}
impl From<BillingError> for UsageRecorderError {
    fn from(e: BillingError) -> Self {
        Self::Billing(e.into())
    }
}
impl UsageRecorderError {
    fn kind(&self) -> &'static str {
        match self {
            Self::Database(_) => "DatabaseError",
            Self::CommitUncertain(_) => "CommitUncertain",
            Self::Billing(_) => "BillingError",
            Self::Receipt(_) => "ValueError",
            Self::InvalidAccountingDelta => "ValueError",
            Self::IntegerOverflow => "OverflowError",
            Self::Collaborator(_) => "CollaboratorError",
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UsageRecord {
    pub tenant_id: String,
    pub user_id: String,
    pub model: String,
    pub request_id: String,
    pub service_id: String,
    pub assistant_id: String,
    pub provider: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub input_cost_cents: i64,
    pub output_cost_cents: i64,
    pub latency_ms: i64,
    pub first_token_ms: i64,
    pub request_total_duration_ms: i64,
    pub llm_inference_duration_ms: i64,
    pub retrieval_duration_ms: i64,
    pub tool_call_duration_ms: i64,
    pub agent_or_graph_overhead_ms: i64,
    pub tool_call_breakdown: Map<String, Value>,
    pub error_type: Option<String>,
    pub status: String,
    pub request_type: String,
    pub metadata: Map<String, Value>,
    pub trace_steps: Vec<Value>,
    pub timestamp: DateTime<Utc>,
}
impl UsageRecord {
    pub fn new(
        tenant: impl Into<String>,
        user: impl Into<String>,
        model: impl Into<String>,
        timestamp: DateTime<Utc>,
    ) -> Self {
        Self {
            tenant_id: tenant.into(),
            user_id: user.into(),
            model: model.into(),
            timestamp,
            request_id: String::new(),
            service_id: String::new(),
            assistant_id: String::new(),
            provider: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            input_cost_cents: 0,
            output_cost_cents: 0,
            latency_ms: 0,
            first_token_ms: 0,
            request_total_duration_ms: 0,
            llm_inference_duration_ms: 0,
            retrieval_duration_ms: 0,
            tool_call_duration_ms: 0,
            agent_or_graph_overhead_ms: 0,
            tool_call_breakdown: Map::new(),
            error_type: None,
            status: "success".into(),
            request_type: "chat".into(),
            metadata: Map::new(),
            trace_steps: vec![],
        }
    }
    pub fn identity(&self) -> Option<UsageIdentity> {
        if self.request_id.is_empty() {
            None
        } else {
            Some((
                self.tenant_id.clone(),
                self.request_id.clone(),
                self.service_id.clone(),
                self.request_type.clone(),
            ))
        }
    }
    fn request_identity(&self) -> UsageIdentity {
        (
            if self.tenant_id.is_empty() {
                "default".into()
            } else {
                self.tenant_id.clone()
            },
            self.request_id.clone(),
            self.service_id.clone(),
            self.request_type.clone(),
        )
    }
    fn total_duration(&self) -> i64 {
        if self.request_total_duration_ms > 0 {
            self.request_total_duration_ms
        } else {
            self.latency_ms
        }
    }
    fn accounting_record(&self) -> Result<AccountingRecord, UsageRecorderError> {
        Ok(AccountingRecord {
            tenant_id: self.tenant_id.clone(),
            user_id: self.user_id.clone(),
            model: self.model.clone(),
            assistant_id: self.assistant_id.clone(),
            service_id: self.service_id.clone(),
            timestamp: self.timestamp,
            status: self.status.clone(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            input_cost_cents: self.input_cost_cents,
            output_cost_cents: self.output_cost_cents,
            latency_ms: self.latency_ms,
            first_token_ms: self.first_token_ms,
            request_total_duration_ms: self.request_total_duration_ms,
            request_delta: metadata_integer(&self.metadata, "_accounting_request_delta", 1)?,
            success_delta: if self.metadata.contains_key("_accounting_success_delta") {
                Some(metadata_integer(
                    &self.metadata,
                    "_accounting_success_delta",
                    0,
                )?)
            } else {
                None
            },
            error_delta: metadata_integer(&self.metadata, "_accounting_error_delta", 0)?,
        })
    }
}
/// Validate the bound immutable receipt before mutating the record or costs.
/// Component prices are USD per 1K tokens; stored costs are integer microcents.
/// Extremely large valid receipts can still exceed TokenRates' arithmetic range.
pub fn prepare_with_snapshot(
    record: &mut UsageRecord,
    snapshot: &Value,
) -> Result<(), UsageRecorderError> {
    // Only an explicit v2 marker selects the new contract. Legacy receipts and
    // their rounding remain unchanged. Validate all references before mutation.
    let price_book_v2 = snapshot.get("price_book_schema").is_some();
    if price_book_v2 {
        crate::price_book::validate_token_snapshot(snapshot)
            .map_err(|error| UsageRecorderError::Collaborator(error.to_string()))?;
    }
    let receipt =
        validate_pricing_snapshot(snapshot, &record.tenant_id, &record.provider, &record.model)?;
    let costs = TokenRates::parse(&receipt.input_price_per_1k, &receipt.output_price_per_1k)?
        .calculate_cost(record.input_tokens, record.output_tokens)?
        .usage_storage_cost()?;
    let serialized = if price_book_v2 {
        snapshot.clone()
    } else {
        receipt.to_value()
    };
    record.input_cost_cents = costs.input_cost_cents;
    record.output_cost_cents = costs.output_cost_cents;
    record
        .metadata
        .insert("pricing_snapshot".into(), serialized);
    record
        .metadata
        .entry("provider")
        .or_insert_with(|| json!(receipt.provider_id));
    record
        .metadata
        .entry("model")
        .or_insert_with(|| json!(record.model));
    record
        .metadata
        .insert("pricing_status".into(), json!(receipt.pricing_status));
    Ok(())
}
fn metadata_integer(
    metadata: &Map<String, Value>,
    key: &str,
    default: i64,
) -> Result<i64, UsageRecorderError> {
    let Some(value) = metadata.get(key) else {
        return Ok(default);
    };
    match value {
        Value::Number(value) => value.as_i64().or_else(|| {
            value
                .as_f64()
                .filter(|v| v.is_finite() && *v >= i64::MIN as f64 && *v < i64::MAX as f64)
                .map(|v| v as i64)
        }),
        Value::String(value) => value.trim().parse().ok(),
        Value::Bool(value) => Some(i64::from(*value)),
        _ => None,
    }
    .ok_or(UsageRecorderError::InvalidAccountingDelta)
}
fn add(a: i64, b: i64) -> Result<i64, UsageRecorderError> {
    a.checked_add(b).ok_or(UsageRecorderError::IntegerOverflow)
}
fn monotonic_delta(current: i64, previous: i64) -> Result<i64, UsageRecorderError> {
    let delta = i128::from(current) - i128::from(previous);
    i64::try_from(delta.max(0)).map_err(|_| UsageRecorderError::IntegerOverflow)
}
fn pending(status: &str) -> bool {
    matches!(status, "running" | "pending")
}
/// Preserve the first observation's dimensions and non-null pricing receipt.
pub fn merge_idempotent_record(existing: &mut UsageRecord, incoming: &UsageRecord) {
    existing.input_tokens = existing.input_tokens.max(incoming.input_tokens);
    existing.output_tokens = existing.output_tokens.max(incoming.output_tokens);
    existing.input_cost_cents = existing.input_cost_cents.max(incoming.input_cost_cents);
    existing.output_cost_cents = existing.output_cost_cents.max(incoming.output_cost_cents);
    let pricing = existing
        .metadata
        .get("pricing_snapshot")
        .filter(|value| !value.is_null())
        .cloned();
    existing.metadata.extend(incoming.metadata.clone());
    if let Some(pricing) = pricing {
        existing.metadata.insert("pricing_snapshot".into(), pricing);
    }
    if pending(&existing.status) && incoming.status == "success" {
        existing.status = incoming.status.clone();
        existing.error_type = incoming.error_type.clone();
    }
    if existing.trace_steps.is_empty() && !incoming.trace_steps.is_empty() {
        existing.trace_steps = incoming.trace_steps.clone();
    }
    existing.latency_ms = existing.latency_ms.max(incoming.latency_ms);
    existing.request_total_duration_ms = existing
        .request_total_duration_ms
        .max(incoming.request_total_duration_ms);
    existing.first_token_ms = existing.first_token_ms.max(incoming.first_token_ms);
    existing.llm_inference_duration_ms = existing
        .llm_inference_duration_ms
        .max(incoming.llm_inference_duration_ms);
    existing.retrieval_duration_ms = existing
        .retrieval_duration_ms
        .max(incoming.retrieval_duration_ms);
    existing.tool_call_duration_ms = existing
        .tool_call_duration_ms
        .max(incoming.tool_call_duration_ms);
    existing.agent_or_graph_overhead_ms = existing
        .agent_or_graph_overhead_ms
        .max(incoming.agent_or_graph_overhead_ms);
}
pub fn accounting_adjustment(
    record: &UsageRecord,
    previous: &UsageRecord,
) -> Result<Option<UsageRecord>, UsageRecorderError> {
    let input = monotonic_delta(record.input_tokens, previous.input_tokens)?;
    let output = monotonic_delta(record.output_tokens, previous.output_tokens)?;
    let input_cost = monotonic_delta(record.input_cost_cents, previous.input_cost_cents)?;
    let output_cost = monotonic_delta(record.output_cost_cents, previous.output_cost_cents)?;
    let success = i64::from(pending(&previous.status) && record.status == "success");
    if input == 0 && output == 0 && input_cost == 0 && output_cost == 0 && success == 0 {
        return Ok(None);
    }
    let mut delta = record.clone();
    stage_receipts::bind_observation(&mut delta, record);
    delta.input_tokens = input;
    delta.output_tokens = output;
    delta.input_cost_cents = input_cost;
    delta.output_cost_cents = output_cost;
    delta.latency_ms = 0;
    delta.first_token_ms = 0;
    delta.request_total_duration_ms = 0;
    delta.llm_inference_duration_ms = 0;
    delta.retrieval_duration_ms = 0;
    delta.tool_call_duration_ms = 0;
    delta.agent_or_graph_overhead_ms = 0;
    delta.trace_steps.clear();
    delta
        .metadata
        .insert("_accounting_adjustment".into(), json!(true));
    delta
        .metadata
        .insert("_accounting_request_delta".into(), json!(0));
    delta
        .metadata
        .insert("_accounting_success_delta".into(), json!(success));
    delta
        .metadata
        .insert("_accounting_error_delta".into(), json!(0));
    Ok(Some(delta))
}
#[derive(Clone, Debug)]
pub struct CollapsedUsage {
    pub database: UsageRecord,
    pub accounting: UsageRecord,
}
/// Direct-writer collapse mirrors sequential ON CONFLICT metadata/error merging;
/// unlike buffer coalescing, the accounting row keeps its first metadata/receipt.
pub fn collapse_usage_records(records: &[UsageRecord]) -> Vec<CollapsedUsage> {
    let mut result: Vec<CollapsedUsage> = vec![];
    let mut positions = HashMap::new();
    for record in records {
        let identity = record.identity();
        let index = identity
            .as_ref()
            .and_then(|identity| positions.get(identity))
            .copied();
        if let Some(index) = index {
            let pair: &mut CollapsedUsage = &mut result[index];
            for current in [&mut pair.database, &mut pair.accounting] {
                current.input_tokens = current.input_tokens.max(record.input_tokens);
                current.output_tokens = current.output_tokens.max(record.output_tokens);
                current.input_cost_cents = current.input_cost_cents.max(record.input_cost_cents);
                current.output_cost_cents = current.output_cost_cents.max(record.output_cost_cents);
                if pending(&current.status) && record.status == "success" {
                    current.status = record.status.clone();
                }
            }
            if pair
                .database
                .error_type
                .as_deref()
                .is_none_or(str::is_empty)
            {
                pair.database.error_type = record.error_type.clone();
            }
            pair.database.metadata.extend(record.metadata.clone());
        } else {
            if let Some(identity) = identity {
                positions.insert(identity, result.len());
            }
            result.push(CollapsedUsage {
                database: record.clone(),
                accounting: record.clone(),
            });
        }
    }
    result
}
fn coalesce_buffer(records: Vec<UsageRecord>) -> Vec<UsageRecord> {
    let mut result: Vec<UsageRecord> = vec![];
    let mut positions = HashMap::new();
    for record in records {
        let identity = record.request_identity();
        if let Some(index) = positions.get(&identity).copied() {
            merge_idempotent_record(&mut result[index], &record);
        } else {
            positions.insert(identity, result.len());
            result.push(record);
        }
    }
    result
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountingStage {
    QuotaCounters,
    RequestTraces,
    DailyAggregates,
    HourlyAggregates,
}
impl AccountingStage {
    pub fn name(self) -> &'static str {
        match self {
            Self::QuotaCounters => "quota_counters",
            Self::RequestTraces => "request_traces",
            Self::DailyAggregates => "daily_aggregates",
            Self::HourlyAggregates => "hourly_aggregates",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageStatus {
    Applied,
    Skipped,
}
#[derive(Clone, Debug, FromRow)]
pub struct PersistedUsage {
    pub inserted: bool,
    pub tenant_id: String,
    pub request_id_key: String,
    pub service_id_key: String,
    pub request_type_key: String,
}
impl PersistedUsage {
    fn identity(&self) -> UsageIdentity {
        (
            self.tenant_id.clone(),
            self.request_id_key.clone(),
            self.service_id_key.clone(),
            self.request_type_key.clone(),
        )
    }
}
/// Implementations commit raw rows before returning success and never perform
/// quota/aggregate writes from inside upsert_raw().
pub trait UsagePersistence: Send + Sync {
    fn upsert_raw<'a>(&'a self, records: &'a [UsageRecord])
    -> UsageFuture<'a, Vec<PersistedUsage>>;
    fn post_write<'a>(
        &'a self,
        stage: AccountingStage,
        records: &'a [UsageRecord],
    ) -> UsageFuture<'a, StageStatus>;
    fn reconcile<'a>(
        &'a self,
        stage: AccountingStage,
        records: &'a [UsageRecord],
        error: &'a UsageRecorderError,
    ) -> UsageFuture<'a, ()>;
}
pub trait UsageTraceWriter: Send + Sync {
    fn write<'a>(&'a self, pool: &'a PgPool, records: &'a [UsageRecord]) -> UsageFuture<'a, ()>;
}
pub trait UsageEventPublisher: Send + Sync {
    fn publish<'a>(&'a self, record: &'a UsageRecord) -> UsageFuture<'a, ()>;
}
/// Resolve a candidate only when the recorder has no immutable receipt yet.
/// Called while the recorder lock is held. Implementations must not call back
/// into that recorder; they must not mutate the record's bound dimensions.
pub trait UsageSnapshotResolver: Send + Sync {
    fn resolve<'a>(&'a self, record: &'a UsageRecord) -> UsageFuture<'a, Value>;
}

#[derive(Clone)]
pub struct PgUsagePersistence {
    pool: PgPool,
    traces: Option<Arc<dyn UsageTraceWriter>>,
}
impl PgUsagePersistence {
    pub fn new(pool: PgPool) -> Self {
        Self { pool, traces: None }
    }
    pub fn with_trace_writer(mut self, traces: Arc<dyn UsageTraceWriter>) -> Self {
        self.traces = Some(traces);
        self
    }
}
const RAW_INSERT: &str = "INSERT INTO usage_records (tenant_id,user_id,request_id,service_id,assistant_id,model,provider,input_tokens,output_tokens,input_cost_cents,output_cost_cents,latency_ms,first_token_ms,request_total_duration_ms,llm_inference_duration_ms,retrieval_duration_ms,tool_call_duration_ms,agent_or_graph_overhead_ms,tool_call_breakdown,error_type,status,request_type,metadata,created_at) ";
const RAW_CONFLICT: &str = " ON CONFLICT (tenant_id,(COALESCE(request_id,'')),(COALESCE(service_id,'')),(COALESCE(request_type,''))) WHERE request_id IS NOT NULL AND request_id<>'' DO UPDATE SET input_tokens=GREATEST(usage_records.input_tokens,EXCLUDED.input_tokens),output_tokens=GREATEST(usage_records.output_tokens,EXCLUDED.output_tokens),input_cost_cents=GREATEST(usage_records.input_cost_cents,EXCLUDED.input_cost_cents),output_cost_cents=GREATEST(usage_records.output_cost_cents,EXCLUDED.output_cost_cents),status=CASE WHEN usage_records.status IN ('running','pending') AND EXCLUDED.status='success' THEN EXCLUDED.status ELSE usage_records.status END,error_type=COALESCE(usage_records.error_type,EXCLUDED.error_type),metadata=COALESCE(usage_records.metadata,'{}'::jsonb)||COALESCE(EXCLUDED.metadata,'{}'::jsonb) RETURNING (xmax=0) AS inserted,tenant_id,COALESCE(request_id,'') AS request_id_key,COALESCE(service_id,'') AS service_id_key,COALESCE(request_type,'') AS request_type_key";
fn optional(value: &str) -> Option<&str> {
    if value.is_empty() { None } else { Some(value) }
}
fn raw_query(records: &[UsageRecord]) -> QueryBuilder<'_, Postgres> {
    let mut query = QueryBuilder::new(RAW_INSERT);
    query.push_values(records, |mut row, record| {
        row.push_bind(&record.tenant_id)
            .push_bind(&record.user_id)
            .push_bind(&record.request_id)
            .push_bind(optional(&record.service_id))
            .push_bind(optional(&record.assistant_id))
            .push_bind(&record.model)
            .push_bind(optional(&record.provider))
            .push_bind(record.input_tokens)
            .push_bind(record.output_tokens)
            .push_bind(record.input_cost_cents)
            .push_bind(record.output_cost_cents)
            .push_bind(record.latency_ms)
            .push_bind(record.first_token_ms)
            .push_bind(if record.request_total_duration_ms != 0 {
                record.request_total_duration_ms
            } else {
                record.latency_ms
            })
            .push_bind(record.llm_inference_duration_ms)
            .push_bind(record.retrieval_duration_ms)
            .push_bind(record.tool_call_duration_ms)
            .push_bind(record.agent_or_graph_overhead_ms)
            .push_bind(Value::Object(record.tool_call_breakdown.clone()))
            .push_bind(&record.error_type)
            .push_bind(&record.status)
            .push_bind(&record.request_type)
            .push_bind(Value::Object(record.metadata.clone()))
            .push_bind(record.timestamp);
    });
    query.push(RAW_CONFLICT);
    query
}
impl UsagePersistence for PgUsagePersistence {
    fn upsert_raw<'a>(
        &'a self,
        records: &'a [UsageRecord],
    ) -> UsageFuture<'a, Vec<PersistedUsage>> {
        Box::pin(async move {
            if records.is_empty() {
                return Ok(vec![]);
            }
            let collapsed = collapse_usage_records(records);
            let records: Vec<_> = collapsed.into_iter().map(|pair| pair.database).collect();
            let mut query = raw_query(&records);
            let mut tx = self.pool.begin().await?;
            let result = query
                .build_query_as::<PersistedUsage>()
                .fetch_all(&mut *tx)
                .await?;
            tx.commit()
                .await
                .map_err(UsageRecorderError::CommitUncertain)?;
            Ok(result)
        })
    }
    fn post_write<'a>(
        &'a self,
        stage: AccountingStage,
        records: &'a [UsageRecord],
    ) -> UsageFuture<'a, StageStatus> {
        Box::pin(async move {
            match stage {
                AccountingStage::RequestTraces => {
                    let Some(traces) = &self.traces else {
                        return Ok(StageStatus::Skipped);
                    };
                    traces.write(&self.pool, records).await?;
                }
                _ => {
                    let mut tx = self.pool.begin().await?;
                    stage_receipts::apply_in_transaction(&mut tx, stage, records).await?;
                    tx.commit()
                        .await
                        .map_err(UsageRecorderError::CommitUncertain)?;
                }
            }
            Ok(StageStatus::Applied)
        })
    }
    fn reconcile<'a>(
        &'a self,
        stage: AccountingStage,
        records: &'a [UsageRecord],
        error: &'a UsageRecorderError,
    ) -> UsageFuture<'a, ()> {
        Box::pin(async move {
            for record in records {
                let metadata = stage_receipts::failure_payload(stage, record, error)?;
                let event_id = stage_receipts::failure_id(stage, record);
                let mut tx = self.pool.begin().await?;
                sqlx::query("INSERT INTO billing_events(event_id,request_id,service_id,user_id,tenant_id,input_tokens,output_tokens,total_tokens,model,assistant_id,status,cost_amount,cost_currency,metadata,event_time) VALUES($1,$2,$3,$4,$5,0,0,0,$6,$7,'reconciliation_required',NULL,'USD',$8,$9) ON CONFLICT(event_id) DO NOTHING")
                    .bind(&event_id).bind(&record.request_id).bind(if record.service_id.is_empty(){"unattributed_service"}else{&record.service_id}).bind(&record.user_id).bind(&record.tenant_id).bind(&record.model).bind(&record.assistant_id).bind(&metadata).bind(Utc::now()).execute(&mut *tx).await?;
                // Lock the stored receipt after insert/conflict. Concurrent writers
                // cannot overwrite a stricter quarantine decision with stale flags.
                let stored: Value = sqlx::query_scalar("SELECT metadata FROM billing_events WHERE event_id=$1 AND tenant_id=$2 AND request_id=$3 AND user_id=$4 FOR UPDATE")
                    .bind(&event_id).bind(&record.tenant_id).bind(&record.request_id).bind(&record.user_id).fetch_one(&mut *tx).await?;
                let merged = stage_receipts::merge_failure_quarantine(&stored, &metadata)?;
                if merged != stored {
                    sqlx::query("UPDATE billing_events SET metadata=$2 WHERE event_id=$1")
                        .bind(&event_id)
                        .bind(merged)
                        .execute(&mut *tx)
                        .await?;
                }
                tx.commit()
                    .await
                    .map_err(UsageRecorderError::CommitUncertain)?;
            }
            Ok(())
        })
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuotaAdjustment {
    pub tokens: i64,
    pub cost_cents: i64,
    pub requests: i64,
}
/// Python rounds the sum of microcents per tenant/user, after clamping each
/// record's combined cost and token delta; it never rounds each component here.
pub fn quota_adjustments(
    records: &[UsageRecord],
) -> Result<BTreeMap<(String, String), QuotaAdjustment>, UsageRecorderError> {
    let mut result: BTreeMap<(String, String), QuotaAdjustment> = BTreeMap::new();
    for record in records {
        let entry = result
            .entry((record.tenant_id.clone(), record.user_id.clone()))
            .or_default();
        entry.tokens = add(
            entry.tokens,
            add(record.input_tokens, record.output_tokens)?.max(0),
        )?;
        entry.cost_cents = add(
            entry.cost_cents,
            add(record.input_cost_cents, record.output_cost_cents)?.max(0),
        )?;
        entry.requests = add(
            entry.requests,
            metadata_integer(&record.metadata, "_accounting_request_delta", 1)?,
        )?;
    }
    for entry in result.values_mut() {
        // Preserve Python's round(int / 10000), including its binary-float input.
        let cents = entry.cost_cents as f64 / MICROCENTS_PER_CENT as f64;
        entry.cost_cents = Decimal::from_f64_retain(cents)
            .map(|value| {
                value.round_dp_with_strategy(0, rust_decimal::RoundingStrategy::MidpointNearestEven)
            })
            .and_then(|value| value.to_i64())
            .ok_or(UsageRecorderError::IntegerOverflow)?;
    }
    Ok(result)
}

#[derive(Clone, Debug)]
pub struct RecorderConfig {
    pub buffer_size: usize,
    pub flush_interval: Duration,
    pub flushed_ids_max: usize,
}
impl Default for RecorderConfig {
    fn default() -> Self {
        Self {
            buffer_size: 100,
            flush_interval: Duration::from_secs(5),
            flushed_ids_max: 5000,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct FlushReport {
    pub attempted: usize,
    pub accepted: usize,
    pub skipped_duplicates: usize,
    pub dropped: usize,
    pub requeued: usize,
    pub uncertain_records: usize,
    pub raw_error: Option<String>,
    pub stage_failures: Vec<StageFailure>,
    pub skipped_stages: Vec<AccountingStage>,
}
#[derive(Clone, Debug)]
pub struct StageFailure {
    pub stage: AccountingStage,
    pub error_type: String,
    pub error: String,
    pub reconciliation_recorded: bool,
}
/// Redacted quarantine inspection: no metadata, receipts, trace payloads or
/// connection errors are returned. These identities still require authorization
/// before exposing them through an administrative endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UncertainUsage {
    pub identity: UsageIdentity,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub input_cost_cents: i64,
    pub output_cost_cents: i64,
}
#[derive(Default)]
struct RecorderState {
    buffer: Vec<UsageRecord>,
    observations: HashMap<UsageIdentity, UsageRecord>,
    flushed_ids: HashSet<String>,
    flushed_identities: HashSet<UsageIdentity>,
    uncertain: HashMap<UsageIdentity, UsageRecord>,
}
impl RecorderState {
    fn advances(&self, record: &UsageRecord) -> bool {
        let Some(previous) = self.observations.get(&record.request_identity()) else {
            return true;
        };
        (pending(&previous.status) && record.status == "success")
            || record.input_tokens > previous.input_tokens
            || record.output_tokens > previous.output_tokens
            || record.input_cost_cents > previous.input_cost_cents
            || record.output_cost_cents > previous.output_cost_cents
    }
    fn remember(&mut self, record: &UsageRecord) -> Option<UsageRecord> {
        let identity = record.identity()?;
        let previous = self.observations.get(&identity).cloned();
        let mut merged = previous.clone().unwrap_or_else(|| record.clone());
        merge_idempotent_record(&mut merged, record);
        self.observations.insert(identity, merged);
        previous
    }
    fn completed(&mut self, records: &[UsageRecord], max: usize) {
        for record in records {
            self.flushed_ids.insert(record.request_id.clone());
            self.flushed_identities.insert(record.request_identity());
        }
        if self.flushed_ids.len() > max {
            self.flushed_ids = records.iter().map(|r| r.request_id.clone()).collect();
            self.flushed_identities = records.iter().map(UsageRecord::request_identity).collect();
            self.observations = records
                .iter()
                .map(|r| (r.request_identity(), r.clone()))
                .collect();
        }
    }
}
struct RunningFlush {
    stop: oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}
#[derive(Clone)]
pub struct UsageRecorder {
    persistence: Option<Arc<dyn UsagePersistence>>,
    config: RecorderConfig,
    state: Arc<Mutex<RecorderState>>,
    background: Arc<Mutex<Option<RunningFlush>>>,
    publisher: Option<Arc<dyn UsageEventPublisher>>,
    publish_failed: Arc<Mutex<bool>>,
}
impl UsageRecorder {
    pub fn new(persistence: Option<Arc<dyn UsagePersistence>>, config: RecorderConfig) -> Self {
        Self {
            persistence,
            config,
            state: Arc::new(Mutex::new(RecorderState::default())),
            background: Arc::new(Mutex::new(None)),
            publisher: None,
            publish_failed: Arc::new(Mutex::new(false)),
        }
    }
    pub fn with_publisher(mut self, publisher: Arc<dyn UsageEventPublisher>) -> Self {
        self.publisher = Some(publisher);
        self
    }
    pub async fn buffered_len(&self) -> usize {
        self.state.lock().await.buffer.len()
    }
    /// Batches with unacknowledged COMMIT outcomes are retained separately and
    /// never automatically replayed. The operator must reconcile durable rows
    /// and accounting stages; this memory is not durable across process loss.
    pub async fn uncertain_records(&self) -> Vec<UncertainUsage> {
        self.state
            .lock()
            .await
            .uncertain
            .iter()
            .map(|(identity, record)| UncertainUsage {
                identity: identity.clone(),
                input_tokens: record.input_tokens,
                output_tokens: record.output_tokens,
                input_cost_cents: record.input_cost_cents,
                output_cost_cents: record.output_cost_cents,
            })
            .collect()
    }
    pub async fn uncertain_count(&self) -> usize {
        self.state.lock().await.uncertain.len()
    }
    /// Prepared input boundary: normalization, authorization and immutable pricing
    /// validation must have succeeded before this method. Bus failures latch but
    /// never undo the database buffer append, matching Python's dual-write order.
    pub async fn record_priced(&self, record: UsageRecord) -> Option<FlushReport> {
        let owned = self.clone();
        tokio::spawn(async move { owned.record_priced_owned(record).await })
            .await
            .expect("owned usage recording task panicked")
    }
    async fn record_priced_owned(&self, mut record: UsageRecord) -> Option<FlushReport> {
        let mut state = self.state.lock().await;
        let report = self.enqueue_locked(&mut state, &mut record).await;
        drop(state);
        self.publish_record(&record).await;
        report
    }
    /// Select the first buffered/flushed receipt under the buffer lock, validate
    /// it against this observation's unchanged identity, and calculate costs.
    /// Identity mismatch or tampering fails before enqueue or event publication.
    /// The supplied candidate is used only when no prior/record receipt exists.
    /// Cancellation stops waiting; the owned recording task continues safely.
    pub async fn record_with_snapshot(
        &self,
        record: UsageRecord,
        candidate: &Value,
    ) -> Result<Option<FlushReport>, UsageRecorderError> {
        let owned = self.clone();
        let candidate = candidate.clone();
        tokio::spawn(async move { owned.record_with_snapshot_owned(record, &candidate).await })
            .await
            .expect("owned receipt usage recording task panicked")
    }
    async fn record_with_snapshot_owned(
        &self,
        mut record: UsageRecord,
        candidate: &Value,
    ) -> Result<Option<FlushReport>, UsageRecorderError> {
        let mut state = self.state.lock().await;
        let snapshot =
            Self::selected_snapshot(&state, &record).unwrap_or_else(|| candidate.clone());
        prepare_with_snapshot(&mut record, &snapshot)?;
        let report = self.enqueue_locked(&mut state, &mut record).await;
        drop(state);
        self.publish_record(&record).await;
        Ok(report)
    }
    /// Like record_with_snapshot(), but do not perform mutable price lookup when
    /// a receipt already exists. Invalid existing receipts fail without fallback.
    /// Dimensions must already be normalized. Resolving, preparing and enqueuing
    /// are serialized together, and cancellation does not abandon owned work.
    pub async fn record_with_resolver(
        &self,
        record: UsageRecord,
        resolver: Arc<dyn UsageSnapshotResolver>,
    ) -> Result<Option<FlushReport>, UsageRecorderError> {
        let owned = self.clone();
        tokio::spawn(async move { owned.record_with_resolver_owned(record, resolver).await })
            .await
            .expect("owned lazy receipt usage recording task panicked")
    }
    async fn record_with_resolver_owned(
        &self,
        mut record: UsageRecord,
        resolver: Arc<dyn UsageSnapshotResolver>,
    ) -> Result<Option<FlushReport>, UsageRecorderError> {
        let mut state = self.state.lock().await;
        let snapshot = match Self::selected_snapshot(&state, &record) {
            Some(snapshot) => snapshot,
            None => resolver.resolve(&record).await?,
        };
        prepare_with_snapshot(&mut record, &snapshot)?;
        let report = self.enqueue_locked(&mut state, &mut record).await;
        drop(state);
        self.publish_record(&record).await;
        Ok(report)
    }
    fn selected_snapshot(state: &RecorderState, record: &UsageRecord) -> Option<Value> {
        let identity = record.request_identity();
        let prior = state.observations.get(&identity).or_else(|| {
            if record.request_id.is_empty() {
                None
            } else {
                state
                    .buffer
                    .iter()
                    .find(|previous| previous.request_identity() == identity)
            }
        });
        prior
            .and_then(|previous| previous.metadata.get("pricing_snapshot"))
            .filter(|value| !value.is_null())
            .or_else(|| {
                record
                    .metadata
                    .get("pricing_snapshot")
                    .filter(|value| !value.is_null())
            })
            .cloned()
    }
    async fn enqueue_locked(
        &self,
        state: &mut RecorderState,
        record: &mut UsageRecord,
    ) -> Option<FlushReport> {
        if state.buffer.len().saturating_add(1) >= self.config.buffer_size
            && record.request_id.is_empty()
        {
            record.request_id = uuid::Uuid::new_v4().to_string();
        }
        state.buffer.push(record.clone());
        if state.buffer.len() >= self.config.buffer_size {
            Some(self.flush_locked(state).await)
        } else {
            None
        }
    }
    async fn publish_record(&self, record: &UsageRecord) {
        if let Some(publisher) = &self.publisher {
            let mut failed = self.publish_failed.lock().await;
            if !*failed && publisher.publish(record).await.is_err() {
                *failed = true;
            }
        }
    }
    pub async fn flush(&self) -> FlushReport {
        let owned = self.clone();
        tokio::spawn(async move { owned.flush_owned().await })
            .await
            .expect("owned usage flush task panicked")
    }
    async fn flush_owned(&self) -> FlushReport {
        let mut state = self.state.lock().await;
        self.flush_locked(&mut state).await
    }
    async fn flush_locked(&self, state: &mut RecorderState) -> FlushReport {
        if state.buffer.is_empty() {
            return FlushReport::default();
        }
        let mut records = std::mem::take(&mut state.buffer);
        for record in &mut records {
            if record.request_id.is_empty() {
                record.request_id = uuid::Uuid::new_v4().to_string();
            }
        }
        let mut records = coalesce_buffer(records);
        let before = records.len();
        records.retain(|record| state.advances(record));
        let mut report = FlushReport {
            attempted: records.len(),
            skipped_duplicates: before - records.len(),
            ..Default::default()
        };
        records.retain(|record| {
            if let Some(uncertain) = state.uncertain.get_mut(&record.request_identity()) {
                merge_idempotent_record(uncertain, record);
                report.uncertain_records += 1;
                false
            } else {
                true
            }
        });
        report.attempted = records.len();
        if records.is_empty() {
            return report;
        }
        let Some(persistence) = &self.persistence else {
            report.dropped = records.len();
            return report;
        };
        let collapsed = collapse_usage_records(&records);
        let database: Vec<_> = collapsed.iter().map(|pair| pair.database.clone()).collect();
        let rows = match persistence.upsert_raw(&database).await {
            Ok(rows) => rows,
            Err(error) => {
                report.raw_error = Some(error.to_string());
                if matches!(error, UsageRecorderError::CommitUncertain(_)) {
                    report.uncertain_records += records.len();
                    for record in records {
                        state.uncertain.insert(record.request_identity(), record);
                    }
                    return report;
                }
                if state.buffer.len().saturating_add(records.len())
                    <= self.config.buffer_size.saturating_mul(2)
                {
                    report.requeued = records.len();
                    state.buffer.extend(records);
                } else {
                    report.dropped = records.len();
                }
                return report;
            }
        };
        let inserted: HashSet<_> = rows
            .iter()
            .filter(|row| row.inserted && !row.request_id_key.is_empty())
            .map(PersistedUsage::identity)
            .collect();
        let mut accepted = vec![];
        for pair in collapsed {
            let identity = pair.database.identity();
            let previous = state.remember(&pair.database);
            if identity
                .as_ref()
                .is_none_or(|identity| inserted.contains(identity))
            {
                let mut accounting = pair.accounting;
                stage_receipts::bind_observation(&mut accounting, &pair.database);
                accepted.push(accounting);
            } else if let Some(previous) = previous {
                match accounting_adjustment(&pair.database, &previous) {
                    Ok(Some(delta)) => accepted.push(delta),
                    Ok(None) => {}
                    Err(error) => {
                        let recorded = persistence
                            .reconcile(AccountingStage::DailyAggregates, &records, &error)
                            .await
                            .is_ok();
                        report.stage_failures.push(StageFailure {
                            stage: AccountingStage::DailyAggregates,
                            error_type: error.kind().into(),
                            error: error.to_string(),
                            reconciliation_recorded: recorded,
                        });
                    }
                }
            }
        }
        report.accepted = accepted.len();
        if !accepted.is_empty() {
            for stage in [
                AccountingStage::QuotaCounters,
                AccountingStage::RequestTraces,
                AccountingStage::DailyAggregates,
                AccountingStage::HourlyAggregates,
            ] {
                match persistence.post_write(stage, &accepted).await {
                    Ok(StageStatus::Applied) => {}
                    Ok(StageStatus::Skipped) => report.skipped_stages.push(stage),
                    Err(error) => {
                        let recorded = persistence
                            .reconcile(stage, &accepted, &error)
                            .await
                            .is_ok();
                        report.stage_failures.push(StageFailure {
                            stage,
                            error_type: error.kind().into(),
                            error: error.to_string(),
                            reconciliation_recorded: recorded,
                        });
                    }
                }
            }
        }
        state.completed(&records, self.config.flushed_ids_max);
        report
    }
    /// Starts at most one periodic worker. The first flush occurs after the full
    /// configured interval, not immediately. stop() waits then drains the buffer.
    pub async fn start(self: &Arc<Self>) {
        let mut background = self.background.lock().await;
        if background.is_some() {
            return;
        }
        let (stop, mut stop_rx) = oneshot::channel();
        let recorder = Arc::downgrade(self);
        let interval = self.config.flush_interval;
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {_=&mut stop_rx=>break,_=tokio::time::sleep(interval)=> {let Some(recorder)=recorder.upgrade()else{break};recorder.flush().await;}}
            }
        });
        *background = Some(RunningFlush { stop, task });
    }
    pub async fn stop(&self) -> FlushReport {
        let owned = self.clone();
        tokio::spawn(async move { owned.stop_owned().await })
            .await
            .expect("owned usage shutdown task panicked")
    }
    async fn stop_owned(&self) -> FlushReport {
        // Keep start/stop serialized through the drain, including after a caller
        // stops awaiting shutdown. Never abort an in-flight raw commit.
        let mut background = self.background.lock().await;
        if let Some(worker) = background.take() {
            let _ = worker.stop.send(());
            let _ = worker.task.await;
        }
        self.flush_owned().await
    }
}
