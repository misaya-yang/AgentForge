//! Existing Gateway billing persistence, without a second ledger or schema.
//!
//! Source map: `src/services/billing/model_pricing.py` (tenant lookup),
//! `src/services/llm/model_service.py` (provider-scoped price mutation), and
//! `packages/ai-gateway-core/src/ai_gateway_core/metrics/usage_recorder.py`
//! (summary, breakdown, time series, and daily/hourly accounting adjustments).
//! Freshness follows the core persistence `get_usage_last_ingested_at` method.
//! `*_cost_cents` in usage storage are microcents (1,000,000 per USD), not cents.
//! Query errors remain observable through `try_*`; compatibility methods retain
//! Python's empty/collection_error responses. Authentication/RBAC belongs to the
//! caller. Every tenant-facing SQL statement includes the supplied tenant.
use crate::billing::{BillingError, PricingStatus, TokenRates, microcents_to_usd, resolve_pricing};
use chrono::{DateTime, NaiveDate, Timelike, Utc};
use rust_decimal::{Decimal, prelude::ToPrimitive};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection, PgPool, Postgres, QueryBuilder};
use std::{collections::BTreeMap, fmt};

#[derive(Debug)]
pub enum BillingStoreError {
    ModelProviderRequired,
    Billing(BillingError),
    Database(sqlx::Error),
}
impl fmt::Display for BillingStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ModelProviderRequired => f.write_str("model_provider_required"),
            Self::Billing(error) => error.fmt(f),
            Self::Database(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for BillingStoreError {}
impl From<sqlx::Error> for BillingStoreError {
    fn from(value: sqlx::Error) -> Self {
        Self::Database(value)
    }
}
impl From<BillingError> for BillingStoreError {
    fn from(value: BillingError) -> Self {
        Self::Billing(value)
    }
}
type Result<T> = std::result::Result<T, BillingStoreError>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPrice {
    pub model: String,
    pub provider: String,
    pub rates: TokenRates,
    pub pricing_status: PricingStatus,
}
impl ModelPrice {
    fn fallback(model: &str) -> Self {
        let price = resolve_pricing(model);
        Self {
            model: model.into(),
            provider: price.provider.into(),
            rates: price.rates,
            pricing_status: price.status,
        }
    }
    pub fn calculate_cost(&self, input_tokens: i64, output_tokens: i64) -> Result<Value> {
        let status = match self.pricing_status {
            PricingStatus::Catalog => "catalog",
            PricingStatus::ProviderModel => "provider_model",
            PricingStatus::Unknown => "unknown",
            PricingStatus::TenantModel => "tenant_model",
        };
        let mut result = self
            .rates
            .calculate_cost(input_tokens, output_tokens)?
            .model_price_response(status)?;
        result["model"] = json!(self.model);
        result["provider"] = json!(self.provider);
        Ok(result)
    }
    /// Tenant pricing lookup intentionally returns ModelPrice's capability defaults,
    /// just as Python does; model management owns actual capability metadata.
    pub fn to_dict(&self) -> Value {
        json!({"model":self.model,"provider":self.provider,"display_name":self.model,
            "pricing":{"input_per_1k_usd":self.rates.input_price_per_1k().to_f64(),
                "output_per_1k_usd":self.rates.output_price_per_1k().to_f64()},
            "capabilities":{"context_window":0,"max_output_tokens":0,"supports_vision":false,"supports_tools":true},
            "is_active":true})
    }
}
#[derive(FromRow)]
struct PriceRow {
    model_id: String,
    provider_id: String,
    input_price: String,
    output_price: String,
}
impl PriceRow {
    fn into_price(self) -> Result<ModelPrice> {
        Ok(ModelPrice {
            model: self.model_id,
            provider: self.provider_id,
            rates: TokenRates::parse(&self.input_price, &self.output_price)?,
            pricing_status: PricingStatus::TenantModel,
        })
    }
}
const FIND_PRICE: &str = "SELECT model_id,provider_id,input_price_per_1k::text AS input_price,output_price_per_1k::text AS output_price FROM llm_models WHERE tenant_id=$1 AND model_id=$2 AND ($3::text IS NULL OR provider_id=$3) LIMIT 2";

#[derive(Clone)]
pub struct PgBillingStore {
    pool: PgPool,
}
impl PgBillingStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
    /// No global model_pricing mirror or process-global tenant cache is consulted.
    pub async fn get_model_pricing(
        &self,
        tenant_id: &str,
        model: &str,
        provider_id: Option<&str>,
    ) -> Result<ModelPrice> {
        let rows = sqlx::query_as::<_, PriceRow>(FIND_PRICE)
            .bind(tenant_id)
            .bind(model)
            .bind(provider_id)
            .fetch_all(&self.pool)
            .await?;
        select_price(rows, model)
    }
    /// Update prices of an existing compound model identity. None leaves that rate
    /// unchanged; no row returns None, and ambiguous provider omission is an error.
    /// Numeric strings are bound as text then cast by PostgreSQL, avoiding f64 loss.
    pub async fn update_tenant_pricing(
        &self,
        tenant_id: &str,
        model: &str,
        provider_id: Option<&str>,
        input: Option<&str>,
        output: Option<&str>,
    ) -> Result<Option<ModelPrice>> {
        let rates = TokenRates::parse(input.unwrap_or("0"), output.unwrap_or("0"))?;
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query_as::<_, PriceRow>(FIND_PRICE)
            .bind(tenant_id)
            .bind(model)
            .bind(provider_id)
            .fetch_all(&mut *tx)
            .await?;
        if rows.len() > 1 {
            return Err(BillingStoreError::ModelProviderRequired);
        }
        let Some(current) = rows.into_iter().next() else {
            return Ok(None);
        };
        if input.is_none() && output.is_none() {
            return Ok(Some(current.into_price()?));
        }
        let row = sqlx::query_as::<_,PriceRow>("UPDATE llm_models SET input_price_per_1k=COALESCE($4::text::numeric,input_price_per_1k),output_price_per_1k=COALESCE($5::text::numeric,output_price_per_1k),updated_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND model_id=$2 AND provider_id=$3 RETURNING model_id,provider_id,input_price_per_1k::text AS input_price,output_price_per_1k::text AS output_price")
            .bind(tenant_id).bind(model).bind(current.provider_id)
            .bind(input.map(|_|rates.input_price_per_1k().to_string())).bind(output.map(|_|rates.output_price_per_1k().to_string()))
            .fetch_optional(&mut *tx).await?;
        let result = row.map(PriceRow::into_price).transpose()?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn try_usage_summary(&self, filter: &UsageFilter) -> Result<Value> {
        let mut query = summary_query(filter);
        let row = query
            .build_query_as::<SummaryRow>()
            .fetch_one(&self.pool)
            .await?;
        row.into_value(filter)
    }
    /// Preserve Python's collection_error empty summary (including today's dates).
    pub async fn get_usage_summary(&self, filter: &UsageFilter) -> Value {
        self.try_usage_summary(filter)
            .await
            .unwrap_or_else(|_| empty_summary(Utc::now().date_naive(), "collection_error"))
    }
    pub async fn try_usage_breakdown(
        &self,
        filter: &UsageFilter,
        dimension: UsageDimension,
        limit: i64,
    ) -> Result<Vec<Value>> {
        let mut query = breakdown_query(filter, dimension, limit);
        let rows = query
            .build_query_as::<BreakdownRow>()
            .fetch_all(&self.pool)
            .await?;
        let total = rows
            .iter()
            .try_fold(0_i64, |sum, row| checked_add(sum, integer(&row.cost)?))?;
        rows.into_iter()
            .map(|row| row.into_value(dimension, total))
            .collect()
    }
    pub async fn get_usage_breakdown(
        &self,
        filter: &UsageFilter,
        dimension: &str,
        limit: i64,
    ) -> Vec<Value> {
        let Some(dimension) = UsageDimension::parse(dimension) else {
            return vec![];
        };
        self.try_usage_breakdown(filter, dimension, limit)
            .await
            .unwrap_or_default()
    }
    pub async fn try_usage_timeseries(
        &self,
        filter: &UsageFilter,
        granularity: Granularity,
    ) -> Result<Vec<Value>> {
        let mut query = timeseries_query(filter, granularity);
        let rows = query
            .build_query_as::<SeriesRow>()
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(SeriesRow::into_value).collect()
    }
    pub async fn get_usage_timeseries(
        &self,
        filter: &UsageFilter,
        granularity: &str,
    ) -> Vec<Value> {
        self.try_usage_timeseries(filter, Granularity::from_python(granularity))
            .await
            .unwrap_or_default()
    }
    pub async fn get_last_ingested_at(
        &self,
        tenant_id: &str,
        start: Option<NaiveDate>,
        end: Option<NaiveDate>,
        granularity: Granularity,
    ) -> Result<Option<DateTime<Utc>>> {
        let mut query = QueryBuilder::<Postgres>::new(format!(
            "SELECT MAX(updated_at) FROM {} WHERE tenant_id=",
            granularity.table()
        ));
        query.push_bind(tenant_id.to_owned());
        if let Some(date) = start {
            query.push(" AND date>=").push_bind(date);
        }
        if let Some(date) = end {
            query.push(" AND date<=").push_bind(date);
        }
        Ok(query.build_query_scalar().fetch_one(&self.pool).await?)
    }
}
fn select_price(rows: Vec<PriceRow>, model: &str) -> Result<ModelPrice> {
    if rows.len() > 1 {
        return Err(BillingStoreError::ModelProviderRequired);
    }
    rows.into_iter()
        .next()
        .map(PriceRow::into_price)
        .transpose()
        .map(|price| price.unwrap_or_else(|| ModelPrice::fallback(model)))
}

#[derive(Clone, Debug)]
pub struct UsageFilter {
    pub tenant_id: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub user_id: Option<String>,
    pub model: Option<String>,
    pub service_id: Option<String>,
    pub assistant_id: Option<String>,
    pub provider: Option<String>,
}
impl UsageFilter {
    pub fn new(tenant_id: impl Into<String>, start_date: NaiveDate, end_date: NaiveDate) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            start_date,
            end_date,
            user_id: None,
            model: None,
            service_id: None,
            assistant_id: None,
            provider: None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Granularity {
    Day,
    Hour,
}
impl Granularity {
    /// Python treats every value except the exact string "hour" as daily.
    pub fn from_python(value: &str) -> Self {
        if value == "hour" {
            Self::Hour
        } else {
            Self::Day
        }
    }
    fn table(self) -> &'static str {
        match self {
            Self::Day => "usage_daily_aggregates",
            Self::Hour => "usage_hourly_aggregates",
        }
    }
    fn unit(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Hour => "hour",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageDimension {
    Model,
    User,
    Assistant,
    Service,
    Provider,
}
impl UsageDimension {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "model" => Some(Self::Model),
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "service" => Some(Self::Service),
            "provider" => Some(Self::Provider),
            _ => None,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Service => "service",
            Self::Provider => "provider",
        }
    }
    fn column(self) -> &'static str {
        match self {
            Self::Model => "u.model",
            Self::User => "u.user_id",
            Self::Assistant => "u.assistant_id",
            Self::Service => "u.service_id",
            Self::Provider => "provider",
        }
    }
}
fn present(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|value| !value.is_empty())
}
fn filter_query(
    query: &mut QueryBuilder<'static, Postgres>,
    filter: &UsageFilter,
    raw: bool,
    half_open: bool,
    dimensions: &[(&str, &Option<String>)],
) {
    let prefix = if raw { "" } else { "u." };
    query
        .push(format!(" WHERE {prefix}tenant_id="))
        .push_bind(filter.tenant_id.clone());
    if raw && half_open {
        query
            .push(" AND created_at >= ")
            .push_bind(filter.start_date)
            .push("::date AND created_at < (")
            .push_bind(filter.end_date)
            .push("::date + interval '1 day')");
    } else {
        let date = if raw { "created_at::date" } else { "u.date" };
        query
            .push(format!(" AND {date}>="))
            .push_bind(filter.start_date)
            .push(format!(" AND {date}<="))
            .push_bind(filter.end_date);
    }
    for (column, value) in dimensions {
        if let Some(value) = present(value) {
            query
                .push(format!(" AND {prefix}{column}="))
                .push_bind(value.to_owned());
        }
    }
}
fn summary_query(filter: &UsageFilter) -> QueryBuilder<'static, Postgres> {
    let raw = present(&filter.provider).is_some();
    let mut query = QueryBuilder::new(if raw {
        "SELECT COUNT(*)::text AS requests,COUNT(*) FILTER(WHERE status='success')::text AS successes,COALESCE(SUM(input_tokens),0)::text AS input,COALESCE(SUM(output_tokens),0)::text AS output,COALESCE(SUM(total_cost_cents),0)::text AS cost,COALESCE(AVG(COALESCE(NULLIF(request_total_duration_ms,0),latency_ms,0)),0)::text AS latency FROM usage_records"
    } else {
        "SELECT COALESCE(SUM(request_count),0)::text AS requests,COALESCE(SUM(success_count),0)::text AS successes,COALESCE(SUM(total_input_tokens),0)::text AS input,COALESCE(SUM(total_output_tokens),0)::text AS output,COALESCE(SUM(total_cost_cents),0)::text AS cost,COALESCE(AVG(avg_latency_ms),0)::text AS latency FROM usage_daily_aggregates u"
    });
    let mut dimensions = vec![
        ("user_id", &filter.user_id),
        ("model", &filter.model),
        ("service_id", &filter.service_id),
        ("assistant_id", &filter.assistant_id),
    ];
    if raw {
        dimensions.push(("provider", &filter.provider));
    }
    filter_query(&mut query, filter, raw, raw, &dimensions);
    query
}
fn breakdown_query(
    filter: &UsageFilter,
    dimension: UsageDimension,
    limit: i64,
) -> QueryBuilder<'static, Postgres> {
    let raw = dimension == UsageDimension::Provider;
    let (expression, label, join) = match dimension {
        UsageDimension::Provider => (
            "COALESCE(NULLIF(provider,''),'unattributed')",
            "COALESCE(NULLIF(provider,''),'unattributed')",
            "",
        ),
        UsageDimension::Service => (
            "u.service_id",
            "COALESCE(NULLIF(s.name,''),u.service_id)",
            " LEFT JOIN services s ON s.service_id=u.service_id",
        ),
        UsageDimension::User => (
            "u.user_id",
            "COALESCE(NULLIF(usr.display_name,''),NULLIF(usr.username,''),CASE WHEN u.user_id LIKE 'apikey:%' THEN 'API Key · ' || COALESCE(NULLIF(ak.name,''),SUBSTRING(u.user_id FROM 8 FOR 6)) WHEN u.user_id LIKE 'anon:%' THEN 'Anonymous · ' || RIGHT(u.user_id,6) ELSE u.user_id END)",
            " LEFT JOIN users usr ON usr.user_id=u.user_id AND usr.tenant_id=u.tenant_id LEFT JOIN api_keys ak ON ak.tenant_id=u.tenant_id AND ('apikey:' || SUBSTRING(ak.key_hash,1,16))=u.user_id",
        ),
        other => (other.column(), other.column(), ""),
    };
    let totals = if raw {
        "COUNT(*)::text AS requests,COALESCE(SUM(input_tokens),0)::text AS input,COALESCE(SUM(output_tokens),0)::text AS output,COALESCE(SUM(total_cost_cents),0)::text AS cost FROM usage_records"
    } else {
        "SUM(u.request_count)::text AS requests,SUM(u.total_input_tokens)::text AS input,SUM(u.total_output_tokens)::text AS output,SUM(u.total_cost_cents)::text AS cost FROM usage_daily_aggregates u"
    };
    let mut query = QueryBuilder::new(format!(
        "SELECT {expression} AS dimension_value,{label} AS dimension_label,{totals}{join}"
    ));
    // The Python breakdown only accepts user/service filters, unlike summary.
    filter_query(
        &mut query,
        filter,
        raw,
        false,
        &[
            ("user_id", &filter.user_id),
            ("service_id", &filter.service_id),
        ],
    );
    if !raw {
        query.push(format!(" AND {expression} IS NOT NULL"));
    }
    query
        .push(format!(
            " GROUP BY {expression},dimension_label ORDER BY SUM({}) DESC LIMIT ",
            if raw {
                "total_cost_cents"
            } else {
                "u.total_cost_cents"
            }
        ))
        .push_bind(limit);
    query
}
fn timeseries_query(
    filter: &UsageFilter,
    granularity: Granularity,
) -> QueryBuilder<'static, Postgres> {
    let mut query = QueryBuilder::new(format!(
        "SELECT date_trunc('{}',created_at) AS bucket,COUNT(*)::text AS requests,COALESCE(SUM(input_tokens),0)::text AS input,COALESCE(SUM(output_tokens),0)::text AS output,COALESCE(SUM(total_cost_cents),0)::text AS cost,COALESCE(AVG(COALESCE(NULLIF(request_total_duration_ms,0),latency_ms,0)),0)::text AS latency FROM usage_records",
        granularity.unit()
    ));
    filter_query(
        &mut query,
        filter,
        true,
        false,
        &[
            ("provider", &filter.provider),
            ("user_id", &filter.user_id),
            ("model", &filter.model),
            ("service_id", &filter.service_id),
        ],
    );
    query.push(" GROUP BY bucket ORDER BY bucket");
    query
}
fn checked_add(left: i64, right: i64) -> Result<i64> {
    left.checked_add(right)
        .ok_or(BillingError::IntegerOverflow.into())
}
fn integer(value: &str) -> Result<i64> {
    value
        .parse()
        .map_err(|_| BillingError::IntegerOverflow.into())
}
fn truncated(value: &str) -> Result<i64> {
    Decimal::from_str_exact(value)
        .ok()
        .and_then(|value| value.trunc().to_i64())
        .ok_or(BillingError::IntegerOverflow.into())
}
fn usd(value: i64) -> f64 {
    microcents_to_usd(value)
        .to_f64()
        .expect("i64 microcents fit f64 range")
}
fn rounded(value: f64, places: u32) -> f64 {
    Decimal::from_f64_retain(value)
        .map(|value| {
            value
                .round_dp_with_strategy(places, rust_decimal::RoundingStrategy::MidpointNearestEven)
        })
        .and_then(|value| value.to_f64())
        .unwrap_or(value)
}
#[derive(FromRow)]
struct SummaryRow {
    requests: String,
    successes: String,
    input: String,
    output: String,
    cost: String,
    latency: String,
}
impl SummaryRow {
    fn into_value(self, filter: &UsageFilter) -> Result<Value> {
        let requests = integer(&self.requests)?;
        let successes = integer(&self.successes)?;
        let input = integer(&self.input)?;
        let output = integer(&self.output)?;
        Ok(
            json!({"total_requests":requests,"success_rate":if requests>0 {Some(rounded(successes as f64 / requests as f64 *100.0,2))}else{None},"total_input_tokens":input,"total_output_tokens":output,"total_tokens":checked_add(input,output)?,"total_cost_usd":usd(integer(&self.cost)?),"avg_latency_ms":truncated(&self.latency)?,"start_date":filter.start_date.to_string(),"end_date":filter.end_date.to_string()}),
        )
    }
}
#[derive(FromRow)]
struct BreakdownRow {
    dimension_value: String,
    dimension_label: String,
    requests: String,
    input: String,
    output: String,
    cost: String,
}
impl BreakdownRow {
    fn into_value(self, dimension: UsageDimension, total: i64) -> Result<Value> {
        let input = integer(&self.input)?;
        let output = integer(&self.output)?;
        let cost = integer(&self.cost)?;
        let mut value = json!({"label":self.dimension_label,"requests":integer(&self.requests)?,"input_tokens":input,"output_tokens":output,"total_tokens":checked_add(input,output)?,"cost_usd":usd(cost),"percentage":if total>0 {rounded(cost as f64 / total as f64 *100.0,1)} else {0.0}});
        value[dimension.name()] = json!(self.dimension_value);
        Ok(value)
    }
}
#[derive(FromRow)]
struct SeriesRow {
    bucket: DateTime<Utc>,
    requests: String,
    input: String,
    output: String,
    cost: String,
    latency: String,
}
impl SeriesRow {
    fn into_value(self) -> Result<Value> {
        let input = integer(&self.input)?;
        let output = integer(&self.output)?;
        Ok(
            json!({"date":self.bucket.to_rfc3339_opts(chrono::SecondsFormat::Secs,false),"requests":integer(&self.requests)?,"input_tokens":input,"output_tokens":output,"total_tokens":checked_add(input,output)?,"cost_usd":usd(integer(&self.cost)?),"avg_latency_ms":truncated(&self.latency)?}),
        )
    }
}
pub fn empty_summary(today: NaiveDate, status: &str) -> Value {
    json!({"total_requests":0,"success_rate":null,"data_status":status,"total_input_tokens":0,"total_output_tokens":0,"total_tokens":0,"total_cost_usd":0.0,"avg_latency_ms":0,"start_date":today.to_string(),"end_date":today.to_string()})
}

/// An already-accepted usage accounting record, not a new raw observation.
/// The recorder's idempotent raw writer must supply only its inserted records or
/// incremental adjustments. Replaying this delta would double-count usage.
#[derive(Clone, Debug)]
pub struct AccountingRecord {
    pub tenant_id: String,
    pub user_id: String,
    pub model: String,
    pub assistant_id: String,
    pub service_id: String,
    pub timestamp: DateTime<Utc>,
    pub status: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub input_cost_cents: i64,
    pub output_cost_cents: i64,
    pub latency_ms: i64,
    pub first_token_ms: i64,
    pub request_total_duration_ms: i64,
    /// `_accounting_request_delta`, or 1 for a newly accepted observation.
    pub request_delta: i64,
    /// `_accounting_success_delta`; None derives counts from status.
    pub success_delta: Option<i64>,
    pub error_delta: i64,
}
impl AccountingRecord {
    pub fn new(tenant_id: impl Into<String>, timestamp: DateTime<Utc>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            timestamp,
            user_id: String::new(),
            model: String::new(),
            assistant_id: String::new(),
            service_id: String::new(),
            status: "success".into(),
            input_tokens: 0,
            output_tokens: 0,
            input_cost_cents: 0,
            output_cost_cents: 0,
            latency_ms: 0,
            first_token_ms: 0,
            request_total_duration_ms: 0,
            request_delta: 1,
            success_delta: None,
            error_delta: 0,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AggregateKey {
    pub tenant_id: String,
    pub user_id: String,
    pub model: String,
    pub assistant_id: String,
    pub service_id: String,
    pub bucket_start: DateTime<Utc>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageAggregate {
    pub request_count: i64,
    pub success_count: i64,
    pub error_count: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost_cents: i64,
    pub latency_sum: i64,
    pub first_token_sum: i64,
}
impl UsageAggregate {
    pub fn avg_latency_ms(&self) -> i64 {
        if self.request_count > 0 {
            self.latency_sum.div_euclid(self.request_count)
        } else {
            0
        }
    }
    pub fn avg_first_token_ms(&self) -> i64 {
        if self.request_count > 0 {
            self.first_token_sum.div_euclid(self.request_count)
        } else {
            0
        }
    }
}
pub fn group_accounting_records(
    records: &[AccountingRecord],
    granularity: Granularity,
) -> Result<BTreeMap<AggregateKey, UsageAggregate>> {
    let mut aggregates: BTreeMap<AggregateKey, UsageAggregate> = BTreeMap::new();
    for record in records {
        let hour = record
            .timestamp
            .with_minute(0)
            .and_then(|time| time.with_second(0))
            .and_then(|time| time.with_nanosecond(0))
            .expect("zero clock components are valid");
        let bucket_start = if granularity == Granularity::Day {
            hour.with_hour(0).expect("midnight is valid UTC")
        } else {
            hour
        };
        let key = AggregateKey {
            tenant_id: record.tenant_id.clone(),
            user_id: record.user_id.clone(),
            model: record.model.clone(),
            assistant_id: record.assistant_id.clone(),
            service_id: record.service_id.clone(),
            bucket_start,
        };
        let aggregate = aggregates.entry(key).or_default();
        aggregate.request_count = checked_add(aggregate.request_count, record.request_delta)?;
        let (success, error) = if let Some(success) = record.success_delta {
            (success, record.error_delta)
        } else if record.status == "success" {
            (1, 0)
        } else if matches!(record.status.as_str(), "running" | "pending") {
            (0, 0)
        } else {
            (0, 1)
        };
        aggregate.success_count = checked_add(aggregate.success_count, success)?;
        aggregate.error_count = checked_add(aggregate.error_count, error)?;
        aggregate.total_input_tokens =
            checked_add(aggregate.total_input_tokens, record.input_tokens)?;
        aggregate.total_output_tokens =
            checked_add(aggregate.total_output_tokens, record.output_tokens)?;
        aggregate.total_cost_cents = checked_add(
            aggregate.total_cost_cents,
            checked_add(record.input_cost_cents, record.output_cost_cents)?,
        )?;
        if record.request_delta != 0 {
            let duration = if record.request_total_duration_ms > 0 {
                record.request_total_duration_ms
            } else {
                record.latency_ms
            };
            aggregate.latency_sum = checked_add(aggregate.latency_sum, duration)?;
            aggregate.first_token_sum =
                checked_add(aggregate.first_token_sum, record.first_token_ms)?;
        }
    }
    Ok(aggregates)
}
/// Apply both aggregate projections within the caller's existing transaction.
/// Does not write raw records, quota counters, or an invented idempotency ledger.
/// The transaction must also own acceptance of these deltas; commit only after all
/// accounting stages succeed. UPDATE-then-INSERT intentionally mirrors Python's
/// nullable historical-dimension handling and its unique-conflict error behavior.
pub async fn apply_usage_aggregates(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    records: &[AccountingRecord],
) -> Result<()> {
    for granularity in [Granularity::Day, Granularity::Hour] {
        apply_usage_aggregates_for_granularity(tx, records, granularity).await?;
    }
    Ok(())
}
/// Apply one projection so the recorder can retain Python's independent,
/// best-effort daily/hourly stages after committing accepted raw usage.
pub async fn apply_usage_aggregates_for_granularity(
    tx: &mut sqlx::Transaction<'_, Postgres>,
    records: &[AccountingRecord],
    granularity: Granularity,
) -> Result<()> {
    for (key, aggregate) in group_accounting_records(records, granularity)? {
        apply_aggregate(&mut *tx, &key, &aggregate, granularity).await?;
    }
    Ok(())
}
async fn apply_aggregate(
    connection: &mut PgConnection,
    key: &AggregateKey,
    aggregate: &UsageAggregate,
    granularity: Granularity,
) -> Result<()> {
    let hour = granularity == Granularity::Hour;
    let offset = if hour { 8 } else { 7 };
    let requests = format!("${offset}");
    let latency = format!("${}", offset + 6);
    let first_token = format!("${}", offset + 7);
    let average = |field: &str, param: &str| {
        if hour {
            format!(
                "(({field}*request_count+{param}*{requests})/NULLIF(request_count+{requests},0))::integer"
            )
        } else {
            format!(
                "CASE WHEN request_count+{requests}>0 THEN (({field}*request_count+{param}*{requests})/(request_count+{requests}))::integer ELSE 0 END"
            )
        }
    };
    let sql = format!(
        "UPDATE {} SET request_count=request_count+{requests},success_count=success_count+${},error_count=error_count+${},total_input_tokens=total_input_tokens+${},total_output_tokens=total_output_tokens+${},total_cost_cents=total_cost_cents+${},avg_latency_ms={},avg_first_token_ms={},updated_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND COALESCE(user_id,'')=$2 AND COALESCE(model,'')=$3 AND COALESCE(assistant_id,'')=$4 AND COALESCE(service_id,'')=$5 AND {} RETURNING id::text",
        granularity.table(),
        offset + 1,
        offset + 2,
        offset + 3,
        offset + 4,
        offset + 5,
        average("avg_latency_ms", &latency),
        average("avg_first_token_ms", &first_token),
        if hour {
            "bucket_start=$6 AND date=$7"
        } else {
            "date=$6"
        }
    );
    let mut query = sqlx::query_scalar::<_, String>(&sql)
        .bind(&key.tenant_id)
        .bind(&key.user_id)
        .bind(&key.model)
        .bind(&key.assistant_id)
        .bind(&key.service_id);
    if hour {
        query = query.bind(key.bucket_start);
    }
    let updated = query
        .bind(key.bucket_start.date_naive())
        .bind(aggregate.request_count)
        .bind(aggregate.success_count)
        .bind(aggregate.error_count)
        .bind(aggregate.total_input_tokens)
        .bind(aggregate.total_output_tokens)
        .bind(aggregate.total_cost_cents)
        .bind(aggregate.avg_latency_ms())
        .bind(aggregate.avg_first_token_ms())
        .fetch_optional(&mut *connection)
        .await?;
    if updated.is_some() {
        return Ok(());
    }
    let mut query = QueryBuilder::<Postgres>::new(format!(
        "INSERT INTO {} (tenant_id,user_id,model,assistant_id,service_id,{}date,request_count,success_count,error_count,total_input_tokens,total_output_tokens,total_cost_cents,avg_latency_ms,avg_first_token_ms) VALUES (",
        granularity.table(),
        if hour { "bucket_start," } else { "" }
    ));
    let mut values = query.separated(",");
    values
        .push_bind(&key.tenant_id)
        .push_bind(&key.user_id)
        .push_bind(&key.model)
        .push_bind(&key.assistant_id)
        .push_bind(&key.service_id);
    if hour {
        values.push_bind(key.bucket_start);
    }
    values
        .push_bind(key.bucket_start.date_naive())
        .push_bind(aggregate.request_count)
        .push_bind(aggregate.success_count)
        .push_bind(aggregate.error_count)
        .push_bind(aggregate.total_input_tokens)
        .push_bind(aggregate.total_output_tokens)
        .push_bind(aggregate.total_cost_cents)
        .push_bind(aggregate.avg_latency_ms())
        .push_bind(aggregate.avg_first_token_ms());
    query.push(")").build().execute(connection).await?;
    Ok(())
}

