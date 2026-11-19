//! PostgreSQL persistence for `src/services/billing/quota_service.py`.
//!
//! Reuses the frozen billing::UserQuota policy type. HTTP identity/RBAC, RPM,
//! security-event recording and scheduler ownership remain upstream. Merely
//! constructing this adapter starts no jobs. Explicit daily/monthly maintenance
//! resets cover ALL tenants, matching Python; every other query is tenant-bound.
//! Costs here are ordinary cents; forecast input aggregate costs are microcents.
//!
//! Preserve two source quirks: normalized zero/negative setter limits become NULL
//! and COALESCE therefore cannot clear an existing finite limit. Omitted boost
//! values are bound as NULL, including on INSERT; migration 033's NOT NULL boost
//! columns reject that request. We report that database error rather than silently
//! substituting defaults or changing the schema. Get-or-create uses DB defaults.
//!
//! `new_supervised` opts into shared caller-owned statement supervision, including
//! get/create's implicit INSERT. Source statements remain separately committed;
//! this adds no cross-statement atomicity, scheduler, route, pool or policy engine.
//! Abort and join callers, then await `drain_cancelled` before shutdown. See the
//! constructor's direct/session-pinned pool requirements and sent-COMMIT caveat.
//! Supervised database errors retain SQLSTATE with safe text. Public None / false /
//! zero / empty / error-object swallowing is unchanged; legacy `new` retains raw
//! SQLx errors and cannot promise cancellation drain. Alerts/forecast read-only
//! SQL remains unsupervised; get/create keeps its one owned connection.
use crate::billing::{OverageStrategy, UserQuota, normalize_limit};
use crate::postgres_operation::{
    PgOperationConnection, PgOperationError, PgOperationStorage, PgQueryFuture,
};
use chrono::{DateTime, Datelike, Days, NaiveDate, Utc};
use rust_decimal::{Decimal, prelude::ToPrimitive};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool, Postgres, pool::PoolConnection};
use std::fmt;

#[derive(Debug)]
pub enum QuotaStoreError {
    DatabaseUnavailable,
    QuotaNotFound,
    Database(sqlx::Error),
    /// Supervised mode retains only SQLSTATE, never backend text or bound values.
    SupervisedDatabase {
        sqlstate: Option<String>,
    },
    Cancelled,
    InvalidCancellationPool,
    IndeterminateCancellation,
    SupervisionRequired,
    DateOutOfRange,
    IntegerOverflow,
}
impl fmt::Display for QuotaStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DatabaseUnavailable => f.write_str("Database not available"),
            Self::QuotaNotFound => f.write_str("Quota not found"),
            Self::Database(error) => error.fmt(f),
            Self::SupervisedDatabase { .. } => f.write_str("quota_database_unavailable"),
            Self::Cancelled => f.write_str("quota_operation_cancelled"),
            Self::InvalidCancellationPool => f.write_str("quota_independent_control_pool_required"),
            Self::IndeterminateCancellation => f.write_str("quota_cancellation_indeterminate"),
            Self::SupervisionRequired => f.write_str("quota_supervised_store_required"),
            Self::DateOutOfRange => f.write_str("quota_date_out_of_range"),
            Self::IntegerOverflow => f.write_str("billing_integer_overflow"),
        }
    }
}
impl std::error::Error for QuotaStoreError {}
impl From<sqlx::Error> for QuotaStoreError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}
impl From<PgOperationError> for QuotaStoreError {
    fn from(error: PgOperationError) -> Self {
        match error {
            PgOperationError::Database { sqlstate } => Self::SupervisedDatabase { sqlstate },
            PgOperationError::Cancelled => Self::Cancelled,
            PgOperationError::InvalidCancellationPool => Self::InvalidCancellationPool,
            PgOperationError::IndeterminateCancellation => Self::IndeterminateCancellation,
        }
    }
}
impl QuotaStoreError {
    /// Machine-readable code for both legacy and redacted supervised failures.
    pub fn sqlstate(&self) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            Self::Database(error) => error.as_database_error().and_then(|error| error.code()),
            Self::SupervisedDatabase { sqlstate } => sqlstate.as_deref().map(Into::into),
            _ => None,
        }
    }
}
type Result<T> = std::result::Result<T, QuotaStoreError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredQuota {
    pub quota: UserQuota,
    pub daily_reset_at: Option<DateTime<Utc>>,
    pub monthly_reset_at: Option<DateTime<Utc>>,
}
impl StoredQuota {
    pub fn to_value(&self) -> Value {
        let q = &self.quota;
        json!({"tenant_id":q.tenant_id,"user_id":q.user_id,"limits":{"daily_tokens":q.daily_token_limit,"monthly_tokens":q.monthly_token_limit,"monthly_cost_cents":q.monthly_cost_limit_cents,"requests_per_minute":q.requests_per_minute,"requests_per_day":q.requests_per_day},"current_usage":{"daily_tokens":q.current_daily_tokens,"monthly_tokens":q.current_monthly_tokens,"monthly_cost_cents":q.current_monthly_cost_cents,"daily_requests":q.current_daily_requests},"status":{"is_blocked":q.is_blocked,"blocked_reason":q.blocked_reason},"policy":{"overage_strategy":strategy_name(q.overage_strategy),"downgraded_model":q.downgraded_model},"resets":{"daily_reset_at":self.daily_reset_at.map(timestamp),"monthly_reset_at":self.monthly_reset_at.map(timestamp)},"temporary_boost":{"extra_tokens":q.temporary_extra_tokens,"extra_cost_cents":q.temporary_extra_cost_cents,"expires_at":q.temporary_expires_at.map(timestamp)},"warning_threshold":q.warning_threshold})
    }
}
fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(
        if value.timestamp_subsec_micros() == 0 {
            chrono::SecondsFormat::Secs
        } else {
            chrono::SecondsFormat::Micros
        },
        false,
    )
}
fn strategy_name(value: OverageStrategy) -> &'static str {
    match value {
        OverageStrategy::HardBlock => "hard_block",
        OverageStrategy::RateLimit => "rate_limit",
        OverageStrategy::DowngradeModel => "downgrade_model",
        OverageStrategy::AllowButAlert => "allow_but_alert",
    }
}
#[derive(Debug, Clone)]
pub struct QuotaUpdate {
    pub daily_token_limit: Option<i64>,
    pub monthly_token_limit: Option<i64>,
    pub monthly_cost_limit_cents: Option<i64>,
    pub requests_per_minute: Option<i64>,
    pub requests_per_day: Option<i64>,
    pub warning_threshold: i64,
    pub overage_strategy: Option<String>,
    pub downgraded_model: Option<String>,
    pub temporary_extra_tokens: Option<i64>,
    pub temporary_extra_cost_cents: Option<i64>,
    pub temporary_expires_at: Option<DateTime<Utc>>,
}
impl Default for QuotaUpdate {
    fn default() -> Self {
        Self {
            daily_token_limit: None,
            monthly_token_limit: None,
            monthly_cost_limit_cents: None,
            requests_per_minute: None,
            requests_per_day: None,
            warning_threshold: 80,
            overage_strategy: None,
            downgraded_model: None,
            temporary_extra_tokens: None,
            temporary_extra_cost_cents: None,
            temporary_expires_at: None,
        }
    }
}
#[derive(Debug, Clone)]
pub struct NewQuotaAlert {
    pub tenant_id: String,
    pub user_id: String,
    pub alert_type: String,
    pub threshold_value: i64,
    pub current_value: i64,
    pub limit_value: i64,
    pub message: String,
}
#[derive(FromRow)]
struct QuotaRow {
    tenant_id: String,
    user_id: String,
    daily_token_limit: Option<i64>,
    monthly_token_limit: Option<i64>,
    monthly_cost_limit_cents: Option<i64>,
    requests_per_minute: Option<i64>,
    requests_per_day: Option<i64>,
    current_daily_tokens: Option<i64>,
    current_monthly_tokens: Option<i64>,
    current_monthly_cost_cents: Option<i64>,
    current_daily_requests: Option<i64>,
    daily_reset_at: Option<DateTime<Utc>>,
    monthly_reset_at: Option<DateTime<Utc>>,
    is_blocked: Option<bool>,
    blocked_reason: Option<String>,
    warning_threshold: Option<i64>,
    overage_strategy: Option<String>,
    downgraded_model: Option<String>,
    temporary_extra_tokens: Option<i64>,
    temporary_extra_cost_cents: Option<i64>,
    temporary_expires_at: Option<DateTime<Utc>>,
}
impl QuotaRow {
    fn into_quota(self) -> StoredQuota {
        let mut q = UserQuota::new(self.tenant_id, self.user_id);
        q.daily_token_limit = self.daily_token_limit;
        q.monthly_token_limit = self.monthly_token_limit;
        q.monthly_cost_limit_cents = self.monthly_cost_limit_cents;
        q.requests_per_minute = self.requests_per_minute;
        q.requests_per_day = self.requests_per_day;
        q.current_daily_tokens = self.current_daily_tokens.unwrap_or(0);
        q.current_monthly_tokens = self.current_monthly_tokens.unwrap_or(0);
        q.current_monthly_cost_cents = self.current_monthly_cost_cents.unwrap_or(0);
        q.current_daily_requests = self.current_daily_requests.unwrap_or(0);
        q.is_blocked = self.is_blocked.unwrap_or(false);
        q.blocked_reason = self.blocked_reason;
        q.warning_threshold = self
            .warning_threshold
            .filter(|value| *value != 0)
            .unwrap_or(80);
        q.overage_strategy = OverageStrategy::coerce(self.overage_strategy.as_deref());
        q.downgraded_model = self.downgraded_model;
        q.temporary_extra_tokens = self.temporary_extra_tokens.unwrap_or(0);
        q.temporary_extra_cost_cents = self.temporary_extra_cost_cents.unwrap_or(0);
        q.temporary_expires_at = self.temporary_expires_at;
        StoredQuota {
            quota: q,
            daily_reset_at: self.daily_reset_at,
            monthly_reset_at: self.monthly_reset_at,
        }
    }
}
const SELECT_QUOTA: &str = "SELECT tenant_id,user_id,daily_token_limit,monthly_token_limit,monthly_cost_limit_cents,requests_per_minute::bigint AS requests_per_minute,requests_per_day::bigint AS requests_per_day,current_daily_tokens,current_monthly_tokens,current_monthly_cost_cents,current_daily_requests::bigint AS current_daily_requests,daily_reset_at,monthly_reset_at,is_blocked,blocked_reason,warning_threshold::bigint AS warning_threshold,overage_strategy,downgraded_model,temporary_extra_tokens,temporary_extra_cost_cents,temporary_expires_at FROM user_quotas WHERE tenant_id=$1 AND user_id=$2";
const INSERT_DEFAULT: &str = "INSERT INTO user_quotas(tenant_id,user_id,daily_reset_at,monthly_reset_at) VALUES($1,$2,$3::timestamptz,$4::timestamptz) ON CONFLICT(tenant_id,user_id) DO NOTHING";
const SET_QUOTA: &str = "INSERT INTO user_quotas(tenant_id,user_id,daily_token_limit,monthly_token_limit,monthly_cost_limit_cents,requests_per_minute,requests_per_day,warning_threshold,overage_strategy,downgraded_model,temporary_extra_tokens,temporary_extra_cost_cents,temporary_expires_at,daily_reset_at,monthly_reset_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13::timestamptz,$14::timestamptz,$15::timestamptz) ON CONFLICT(tenant_id,user_id) DO UPDATE SET daily_token_limit=COALESCE($3,user_quotas.daily_token_limit),monthly_token_limit=COALESCE($4,user_quotas.monthly_token_limit),monthly_cost_limit_cents=COALESCE($5,user_quotas.monthly_cost_limit_cents),requests_per_minute=COALESCE($6,user_quotas.requests_per_minute),requests_per_day=COALESCE($7,user_quotas.requests_per_day),warning_threshold=$8,overage_strategy=$9,downgraded_model=COALESCE($10,user_quotas.downgraded_model),temporary_extra_tokens=COALESCE($11,user_quotas.temporary_extra_tokens),temporary_extra_cost_cents=COALESCE($12,user_quotas.temporary_extra_cost_cents),temporary_expires_at=COALESCE($13::timestamptz,user_quotas.temporary_expires_at),updated_at=CURRENT_TIMESTAMP";
const RESET_DAILY: &str = "UPDATE user_quotas SET current_daily_tokens=0,current_daily_requests=0,daily_reset_at=$1::timestamptz,updated_at=CURRENT_TIMESTAMP WHERE daily_reset_at IS NULL OR daily_reset_at<=$2::timestamptz";
const RESET_MONTHLY: &str = "UPDATE user_quotas SET current_monthly_tokens=0,current_monthly_cost_cents=0,monthly_reset_at=$1::timestamptz,updated_at=CURRENT_TIMESTAMP WHERE monthly_reset_at IS NULL OR monthly_reset_at<=$2::timestamptz";
const BLOCK_USER: &str = "UPDATE user_quotas SET is_blocked=TRUE,blocked_reason=$3,blocked_at=CURRENT_TIMESTAMP,updated_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND user_id=$2";
const UNBLOCK_USER: &str = "UPDATE user_quotas SET is_blocked=FALSE,blocked_reason=NULL,blocked_at=NULL,updated_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND user_id=$2";
const FORECAST_TOTALS: &str = "SELECT COALESCE(SUM(total_input_tokens+total_output_tokens),0)::text AS tokens,COALESCE(SUM(total_cost_cents),0)::text AS microcents FROM usage_daily_aggregates WHERE tenant_id=$1 AND user_id=$2 AND date>=$3 AND date<=$4";

/// UTC boundary calculation is injectable for deterministic maintenance/tests.
pub fn reset_boundaries(now: DateTime<Utc>) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let today = now.date_naive();
    let tomorrow = today.succ_opt().ok_or(QuotaStoreError::DateOutOfRange)?;
    let month_start = today.with_day(1).ok_or(QuotaStoreError::DateOutOfRange)?;
    let next_month = month_start
        .checked_add_days(Days::new(32))
        .and_then(|date| date.with_day(1))
        .ok_or(QuotaStoreError::DateOutOfRange)?;
    Ok((
        tomorrow.and_hms_opt(0, 0, 0).unwrap().and_utc(),
        next_month.and_hms_opt(0, 0, 0).unwrap().and_utc(),
    ))
}
// A get/create sequence retains one connection, as in the source, but each
// statement commits separately. In supervised mode even its bracketing SELECTs
// use the owned supervisor; no transaction spans SELECT / INSERT / SELECT.
enum QuotaConnection {
    Legacy(PoolConnection<Postgres>),
    Supervised(PgOperationConnection),
}
impl QuotaConnection {
    async fn execute<T, F>(&mut self, query: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut sqlx::PgConnection) -> PgQueryFuture<'c, T> + Send + 'static,
    {
        match self {
            Self::Legacy(connection) => query(connection).await.map_err(Into::into),
            Self::Supervised(connection) => connection.execute(query).await.map_err(Into::into),
        }
    }
    async fn get(&mut self, tenant: &str, user: &str) -> Result<Option<QuotaRow>> {
        let tenant = tenant.to_owned();
        let user = user.to_owned();
        self.execute(move |connection| {
            Box::pin(async move {
                sqlx::query_as::<_, QuotaRow>(SELECT_QUOTA)
                    .bind(tenant)
                    .bind(user)
                    .fetch_optional(connection)
                    .await
            })
        })
        .await
    }
}
#[derive(Clone)]
pub struct PgQuotaStore {
    pool: Option<PgPool>,
    operations: Option<PgOperationStorage>,
}
impl PgQuotaStore {
    /// Legacy autocommit adapter. Caller-drop cancellation is NOT supervised;
    /// never use this constructor for a host promising drained native writes.
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool: Some(pool),
            operations: None,
        }
    }
    /// Opt-in owned statement supervision. Both independent pools must reach the
    /// same server/database/role using direct or session-pinned connections.
    /// This constructor opens no connection and starts no tasks. Keep both pools
    /// and the runtime alive until callers have been aborted, joined and drained.
    /// SQL and source commit boundaries are preserved. BEGIN establishes database
    /// transaction time one round-trip earlier than source autocommit; a COMMIT
    /// already sent may have succeeded and is never promised to be undone.
    pub fn new_supervised(pool: PgPool, control_pool: PgPool) -> Result<Self> {
        let operations = PgOperationStorage::new(pool.clone(), control_pool)?;
        Ok(Self {
            pool: Some(pool),
            operations: Some(operations),
        })
    }
    pub fn unavailable() -> Self {
        Self {
            pool: None,
            operations: None,
        }
    }
    /// A native HTTP bridge must require true, rather than silently wrapping new().
    pub fn is_supervised(&self) -> bool {
        self.operations.is_some()
    }
    /// Abort and JOIN caller tasks first, then drain before releasing test locks,
    /// closing either pool or stopping the runtime. Clones share the registry.
    /// Legacy adapters fail explicitly rather than claiming to have drained.
    pub async fn drain_cancelled(&self) -> Result<()> {
        self.operations
            .as_ref()
            .ok_or(QuotaStoreError::SupervisionRequired)?
            .drain_cancelled()
            .await
            .map_err(Into::into)
    }
    fn pool(&self) -> Result<&PgPool> {
        self.pool
            .as_ref()
            .ok_or(QuotaStoreError::DatabaseUnavailable)
    }
    fn database_error(&self, error: sqlx::Error) -> QuotaStoreError {
        if self.is_supervised() {
            PgOperationError::from(error).into()
        } else {
            error.into()
        }
    }
    async fn acquire(&self) -> Result<QuotaConnection> {
        match &self.operations {
            Some(operations) => Ok(QuotaConnection::Supervised(operations.acquire().await?)),
            None => Ok(QuotaConnection::Legacy(self.pool()?.acquire().await?)),
        }
    }
    pub async fn try_get_or_create_quota_at(
        &self,
        tenant: &str,
        user: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<StoredQuota>> {
        let mut connection = self.acquire().await?;
        let mut row = connection.get(tenant, user).await?;
        if row.is_none() {
            let (tomorrow, next_month) = reset_boundaries(now)?;
            let tenant_owned = tenant.to_owned();
            let user_owned = user.to_owned();
            connection
                .execute(move |connection| {
                    Box::pin(async move {
                        sqlx::query(INSERT_DEFAULT)
                            .bind(tenant_owned)
                            .bind(user_owned)
                            .bind(tomorrow)
                            .bind(next_month)
                            .execute(connection)
                            .await?;
                        Ok(())
                    })
                })
                .await?;
            row = connection.get(tenant, user).await?;
        }
        Ok(row.map(QuotaRow::into_quota))
    }
    pub async fn get_or_create_quota_at(
        &self,
        tenant: &str,
        user: &str,
        now: DateTime<Utc>,
    ) -> Option<StoredQuota> {
        self.try_get_or_create_quota_at(tenant, user, now)
            .await
            .ok()
            .flatten()
    }
    pub async fn get_or_create_quota(&self, tenant: &str, user: &str) -> Option<StoredQuota> {
        self.get_or_create_quota_at(tenant, user, Utc::now()).await
    }
    pub async fn get_user_quota(&self, tenant: &str, user: &str) -> Option<Value> {
        self.get_or_create_quota(tenant, user)
            .await
            .map(|quota| quota.to_value())
    }
    async fn write_quota_at(
        &self,
        tenant: &str,
        user: &str,
        update: &QuotaUpdate,
        now: DateTime<Utc>,
    ) -> Result<()> {
        // Preserve the legacy unavailable-pool check before date validation.
        self.pool()?;
        let (tomorrow, next_month) = reset_boundaries(now)?;
        let tenant = tenant.to_owned();
        let user = user.to_owned();
        let update = update.clone();
        self.acquire()
            .await?
            .execute(move |connection| {
                Box::pin(async move {
                    sqlx::query(SET_QUOTA)
                        .bind(tenant)
                        .bind(user)
                        .bind(normalize_limit(update.daily_token_limit))
                        .bind(normalize_limit(update.monthly_token_limit))
                        .bind(normalize_limit(update.monthly_cost_limit_cents))
                        .bind(normalize_limit(update.requests_per_minute))
                        .bind(normalize_limit(update.requests_per_day))
                        .bind(update.warning_threshold)
                        .bind(strategy_name(OverageStrategy::coerce(
                            update.overage_strategy.as_deref(),
                        )))
                        .bind(&update.downgraded_model)
                        .bind(update.temporary_extra_tokens)
                        .bind(update.temporary_extra_cost_cents)
                        .bind(update.temporary_expires_at)
                        .bind(tomorrow)
                        .bind(next_month)
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .await
    }
    pub async fn try_set_user_quota_at(
        &self,
        tenant: &str,
        user: &str,
        update: &QuotaUpdate,
        now: DateTime<Utc>,
    ) -> Result<Option<StoredQuota>> {
        self.write_quota_at(tenant, user, update, now).await?;
        self.try_get_or_create_quota_at(tenant, user, now).await
    }
    /// Source-shaped setter for native HTTP integration. A successful write is
    /// already committed before readback; ordinary read failure becomes {}.
    /// Preserve cancellation as control flow, rather than a response/audit value.
    /// Unlike the legacy Value API, this requires explicit supervised construction.
    pub async fn supervised_set_user_quota_at(
        &self,
        tenant: &str,
        user: &str,
        update: &QuotaUpdate,
        now: DateTime<Utc>,
    ) -> Result<Value> {
        if !self.is_supervised() {
            return Err(QuotaStoreError::SupervisionRequired);
        }
        match self.write_quota_at(tenant, user, update, now).await {
            Err(QuotaStoreError::Cancelled) => Err(QuotaStoreError::Cancelled),
            Err(error) => Ok(json!({"error":error.to_string()})),
            Ok(()) => match self.try_get_or_create_quota_at(tenant, user, now).await {
                Err(QuotaStoreError::Cancelled) => Err(QuotaStoreError::Cancelled),
                Ok(Some(quota)) => Ok(quota.to_value()),
                Ok(None) | Err(_) => Ok(json!({})),
            },
        }
    }
    pub async fn set_user_quota_at(
        &self,
        tenant: &str,
        user: &str,
        update: &QuotaUpdate,
        now: DateTime<Utc>,
    ) -> Value {
        match self.write_quota_at(tenant, user, update, now).await {
            Err(error) => json!({"error":error.to_string()}),
            Ok(()) => self
                .get_or_create_quota_at(tenant, user, now)
                .await
                .map(|quota| quota.to_value())
                .unwrap_or_else(|| json!({})),
        }
    }
    pub async fn set_user_quota(&self, tenant: &str, user: &str, update: &QuotaUpdate) -> Value {
        self.set_user_quota_at(tenant, user, update, Utc::now())
            .await
    }
    pub async fn try_block_user(&self, tenant: &str, user: &str, reason: &str) -> Result<u64> {
        let tenant = tenant.to_owned();
        let user = user.to_owned();
        let reason = reason.to_owned();
        self.acquire()
            .await?
            .execute(move |connection| {
                Box::pin(async move {
                    Ok(sqlx::query(BLOCK_USER)
                        .bind(tenant)
                        .bind(user)
                        .bind(reason)
                        .execute(connection)
                        .await?
                        .rows_affected())
                })
            })
            .await
    }
    /// Like Python, successful UPDATE of zero rows is true and creates no quota.
    pub async fn block_user(&self, tenant: &str, user: &str, reason: &str) -> bool {
        self.try_block_user(tenant, user, reason).await.is_ok()
    }
    pub async fn try_unblock_user(&self, tenant: &str, user: &str) -> Result<u64> {
        let tenant = tenant.to_owned();
        let user = user.to_owned();
        self.acquire()
            .await?
            .execute(move |connection| {
                Box::pin(async move {
                    Ok(sqlx::query(UNBLOCK_USER)
                        .bind(tenant)
                        .bind(user)
                        .execute(connection)
                        .await?
                        .rows_affected())
                })
            })
            .await
    }
    pub async fn unblock_user(&self, tenant: &str, user: &str) -> bool {
        self.try_unblock_user(tenant, user).await.is_ok()
    }
    /// ALL-TENANT maintenance entry point. Never authorize this from tenant input.
    pub async fn try_reset_daily_quotas_at(&self, now: DateTime<Utc>) -> Result<u64> {
        let (tomorrow, _) = reset_boundaries(now)?;
        self.acquire()
            .await?
            .execute(move |connection| {
                Box::pin(async move {
                    Ok(sqlx::query(RESET_DAILY)
                        .bind(tomorrow)
                        .bind(now)
                        .execute(connection)
                        .await?
                        .rows_affected())
                })
            })
            .await
    }
    pub async fn reset_daily_quotas_at(&self, now: DateTime<Utc>) -> u64 {
        self.try_reset_daily_quotas_at(now).await.unwrap_or(0)
    }
    pub async fn reset_daily_quotas(&self) -> u64 {
        self.reset_daily_quotas_at(Utc::now()).await
    }
    /// ALL-TENANT maintenance entry point. Does not clear boosts or block status.
    pub async fn try_reset_monthly_quotas_at(&self, now: DateTime<Utc>) -> Result<u64> {
        let (_, next_month) = reset_boundaries(now)?;
        self.acquire()
            .await?
            .execute(move |connection| {
                Box::pin(async move {
                    Ok(sqlx::query(RESET_MONTHLY)
                        .bind(next_month)
                        .bind(now)
                        .execute(connection)
                        .await?
                        .rows_affected())
                })
            })
            .await
    }
    pub async fn reset_monthly_quotas_at(&self, now: DateTime<Utc>) -> u64 {
        self.try_reset_monthly_quotas_at(now).await.unwrap_or(0)
    }
    pub async fn reset_monthly_quotas(&self) -> u64 {
        self.reset_monthly_quotas_at(Utc::now()).await
    }
    pub async fn try_get_quota_alerts(
        &self,
        tenant: &str,
        limit: i64,
        unacknowledged_only: bool,
    ) -> Result<Vec<Value>> {
        let query = format!(
            "SELECT id::text AS id,user_id,alert_type,threshold_value,current_value,limit_value,message,is_acknowledged,created_at FROM quota_alerts WHERE tenant_id=$1{} ORDER BY created_at DESC LIMIT $2",
            if unacknowledged_only {
                " AND is_acknowledged=FALSE"
            } else {
                ""
            }
        );
        Ok(sqlx::query_as::<_, AlertRow>(&query)
            .bind(tenant)
            .bind(limit)
            .fetch_all(self.pool()?)
            .await
            .map_err(|error| self.database_error(error))?
            .into_iter()
            .map(AlertRow::into_value)
            .collect())
    }
    pub async fn get_quota_alerts(
        &self,
        tenant: &str,
        limit: i64,
        unacknowledged_only: bool,
    ) -> Vec<Value> {
        self.try_get_quota_alerts(tenant, limit, unacknowledged_only)
            .await
            .unwrap_or_default()
    }
    pub async fn try_create_alert(&self, alert: &NewQuotaAlert) -> Result<()> {
        let alert = alert.clone();
        self.acquire().await?.execute(move |connection| {
            Box::pin(async move {
                sqlx::query("INSERT INTO quota_alerts(tenant_id,user_id,alert_type,threshold_value,current_value,limit_value,message) VALUES($1,$2,$3,$4,$5,$6,$7)")
                    .bind(&alert.tenant_id)
                    .bind(&alert.user_id)
                    .bind(&alert.alert_type)
                    .bind(alert.threshold_value)
                    .bind(alert.current_value)
                    .bind(alert.limit_value)
                    .bind(&alert.message)
                    .execute(connection)
                    .await?;
                Ok(())
            })
        }).await
    }
    pub async fn create_alert(&self, alert: &NewQuotaAlert) {
        let _ = self.try_create_alert(alert).await;
    }
    pub async fn try_get_quota_forecast_at(
        &self,
        tenant: &str,
        user: &str,
        lookback_days: i64,
        now: DateTime<Utc>,
    ) -> Result<Value> {
        let pool = self.pool()?;
        // Python maps any unavailable get/create to the public "Quota not found".
        let quota = match self.try_get_or_create_quota_at(tenant, user, now).await {
            Ok(Some(quota)) => quota,
            Err(QuotaStoreError::Cancelled) => return Err(QuotaStoreError::Cancelled),
            Ok(None) | Err(_) => return Err(QuotaStoreError::QuotaNotFound),
        };
        let window = forecast_window(now.date_naive(), lookback_days)?;
        let mut connection = pool
            .acquire()
            .await
            .map_err(|error| self.database_error(error))?;
        let month = sqlx::query_as::<_, TotalsRow>(FORECAST_TOTALS)
            .bind(tenant)
            .bind(user)
            .bind(window.month_start)
            .bind(window.today)
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| self.database_error(error))?;
        let recent = sqlx::query_as::<_, TotalsRow>(FORECAST_TOTALS)
            .bind(tenant)
            .bind(user)
            .bind(window.window_start)
            .bind(window.today)
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| self.database_error(error))?;
        quota_forecast(
            &quota.quota,
            ForecastTotals {
                month_tokens: integer(&month.tokens)?,
                month_cost_microcents: integer(&month.microcents)?,
                recent_tokens: integer(&recent.tokens)?,
                recent_cost_microcents: integer(&recent.microcents)?,
            },
            lookback_days,
            now,
        )
    }
    pub async fn get_quota_forecast_at(
        &self,
        tenant: &str,
        user: &str,
        lookback_days: i64,
        now: DateTime<Utc>,
    ) -> Value {
        self.try_get_quota_forecast_at(tenant, user, lookback_days, now)
            .await
            .unwrap_or_else(|error| json!({"error":error.to_string()}))
    }
    pub async fn get_quota_forecast(&self, tenant: &str, user: &str, lookback_days: i64) -> Value {
        self.get_quota_forecast_at(tenant, user, lookback_days, Utc::now())
            .await
    }
}
#[derive(FromRow)]
struct AlertRow {
    id: String,
    user_id: String,
    alert_type: String,
    threshold_value: Option<i64>,
    current_value: Option<i64>,
    limit_value: Option<i64>,
    message: Option<String>,
    is_acknowledged: Option<bool>,
    created_at: Option<DateTime<Utc>>,
}
impl AlertRow {
    fn into_value(self) -> Value {
        json!({"id":self.id,"user_id":self.user_id,"alert_type":self.alert_type,"threshold_value":self.threshold_value,"current_value":self.current_value,"limit_value":self.limit_value,"message":self.message,"is_acknowledged":self.is_acknowledged,"created_at":self.created_at.map(timestamp)})
    }
}
#[derive(FromRow)]
struct TotalsRow {
    tokens: String,
    microcents: String,
}
fn integer(value: &str) -> Result<i64> {
    value.parse().map_err(|_| QuotaStoreError::IntegerOverflow)
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
fn rounded_integer(value: f64) -> Result<i64> {
    Decimal::from_f64_retain(value)
        .map(|value| {
            value.round_dp_with_strategy(0, rust_decimal::RoundingStrategy::MidpointNearestEven)
        })
        .and_then(|value| value.to_i64())
        .ok_or(QuotaStoreError::IntegerOverflow)
}
fn boosted_limit(limit: Option<i64>, extra: i64) -> Result<Option<i64>> {
    limit
        .map(|limit| {
            limit
                .checked_add(extra)
                .ok_or(QuotaStoreError::IntegerOverflow)
        })
        .transpose()
}
struct ForecastWindow {
    month_start: NaiveDate,
    today: NaiveDate,
    window_start: NaiveDate,
    days_elapsed: i64,
    days_remaining: i64,
    lookback_days: i64,
}
fn forecast_window(today: NaiveDate, lookback_days: i64) -> Result<ForecastWindow> {
    let lookback_days = lookback_days.max(1);
    let month_start = today.with_day(1).ok_or(QuotaStoreError::DateOutOfRange)?;
    let (_, next_month) = reset_boundaries(today.and_hms_opt(0, 0, 0).unwrap().and_utc())?;
    let days_in_month = (next_month.date_naive() - month_start).num_days();
    let days_elapsed = (today - month_start).num_days() + 1;
    let window_start = today
        .checked_sub_days(Days::new((lookback_days - 1) as u64))
        .ok_or(QuotaStoreError::DateOutOfRange)?;
    Ok(ForecastWindow {
        month_start,
        today,
        window_start,
        days_elapsed,
        days_remaining: (days_in_month - days_elapsed).max(0),
        lookback_days,
    })
}
pub fn projected_breach_date(
    current: f64,
    limit: Option<f64>,
    average_daily_delta: f64,
    today: NaiveDate,
    days_remaining: i64,
) -> Option<NaiveDate> {
    let limit = limit.filter(|limit| *limit > 0.0)?;
    if current >= limit {
        return Some(today);
    }
    if average_daily_delta <= 0.0 {
        return None;
    }
    let days = ((limit - current) / average_daily_delta).ceil();
    if days <= 0.0 {
        return Some(today);
    }
    if !days.is_finite() || days > days_remaining as f64 {
        return None;
    }
    today.checked_add_days(Days::new((days as u64).saturating_sub(1)))
}
#[derive(Clone, Copy, Debug, Default)]
pub struct ForecastTotals {
    pub month_tokens: i64,
    pub month_cost_microcents: i64,
    pub recent_tokens: i64,
    pub recent_cost_microcents: i64,
}
pub fn quota_forecast(
    quota: &UserQuota,
    totals: ForecastTotals,
    lookback_days: i64,
    now: DateTime<Utc>,
) -> Result<Value> {
    let window = forecast_window(now.date_naive(), lookback_days)?;
    let month_cost = totals.month_cost_microcents as f64 / 10000.0;
    let avg_tokens = totals.recent_tokens as f64 / window.lookback_days as f64;
    let avg_cost = (totals.recent_cost_microcents as f64 / 10000.0) / window.lookback_days as f64;
    let projected_tokens =
        rounded_integer(totals.month_tokens as f64 + avg_tokens * window.days_remaining as f64)?;
    let projected_cost = rounded_integer(month_cost + avg_cost * window.days_remaining as f64)?;
    let active = quota
        .temporary_expires_at
        .is_some_and(|expiry| expiry > now);
    let token_limit = boosted_limit(
        quota.monthly_token_limit,
        if active {
            quota.temporary_extra_tokens
        } else {
            0
        },
    )?;
    let cost_limit = boosted_limit(
        quota.monthly_cost_limit_cents,
        if active {
            quota.temporary_extra_cost_cents
        } else {
            0
        },
    )?;
    let percentage = |used: i64, limit: Option<i64>| {
        limit
            .filter(|limit| *limit != 0)
            .map(|limit| rounded(used as f64 / limit as f64 * 100.0, 2))
    };
    Ok(
        json!({"tenant_id":quota.tenant_id,"user_id":quota.user_id,"window":{"month_start":window.month_start.to_string(),"today":window.today.to_string(),"days_elapsed":window.days_elapsed,"days_remaining":window.days_remaining,"lookback_days":window.lookback_days},"tokens":{"current":totals.month_tokens,"avg_daily":rounded(avg_tokens,2),"projected_month_end":projected_tokens,"limit":token_limit,"projected_usage_pct":percentage(projected_tokens,token_limit),"predicted_breach_date":projected_breach_date(totals.month_tokens as f64,token_limit.map(|value|value as f64),avg_tokens,window.today,window.days_remaining).map(|date|date.to_string())},"cost":{"current_cents":rounded(month_cost,2),"current_usd":rounded(month_cost/100.0,4),"avg_daily_cents":rounded(avg_cost,2),"projected_month_end_cents":projected_cost,"projected_month_end_usd":rounded(projected_cost as f64/100.0,4),"limit_cents":cost_limit,"limit_usd":cost_limit.filter(|limit|*limit!=0).map(|limit|rounded(limit as f64/100.0,4)),"projected_usage_pct":percentage(projected_cost,cost_limit),"predicted_breach_date":projected_breach_date(month_cost,cost_limit.map(|value|value as f64),avg_cost,window.today,window.days_remaining).map(|date|date.to_string())}}),
    )
}

#[path = "quota_store/reservations.rs"]
pub mod reservations;

#[path = "quota_store/reservation_receipts.rs"]
pub mod reservation_receipts;
