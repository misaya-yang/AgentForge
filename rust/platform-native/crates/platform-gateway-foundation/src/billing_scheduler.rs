//! Source-faithful maintenance tasks from `src/services/billing/aggregation_task.py`
//! and lifecycle/scheduling from `src/services/billing/usage_scheduler.py`.
//!
//! Construction and singleton initialization are inert. Nothing registers routes,
//! starts workers, or runs maintenance automatically. Calling maintenance methods
//! executes their storage operations; callers own authorization and access control.
//! The injected connection and runtime seams permit entirely synthetic tests.
//!
//! These are the source's full-day replacement projections, not the incremental
//! accounting in `billing_store`/`usage_recorder`. Reusing that additive SQL would
//! double count usage and lose source p95 semantics. Likewise `quota_store` ports
//! a different reset owner (next-reset timestamps, additional counters, and <=),
//! whereas this source resets only daily tokens/monthly cost with strict < and
//! database CURRENT_DATE/CURRENT_TIMESTAMP. Keep those contracts distinct.
//!
//! Preserved quirks: no catch-up after the configured hour; swallowed scheduled
//! failures still mark that date attempted; manual calls do not update status;
//! monthly checks run only on UTC day 1; unknown manual reset types mean daily.
//! Database calendar semantics remain database-owned, as in the Python SQL.
//! Rust counts are checked u64; checked calendar arithmetic enforces Python's
//! year 1..9999 date range. Driver row counts replace asyncpg status
//! string parsing. Cancellation is never converted into an ordinary error value.
//! Backend/collaborator exception text is intentionally redacted to fixed codes;
//! neither Display nor Debug retains SQL, credentials, or query values.
//! Python logging is not reproduced; one-pass seams expose ordinary outcomes.
//! Owner drop cancels workers rather than detaching maintenance into the runtime.
//!
//! PostgreSQL cancellation uses an explicitly supplied independent control pool,
//! an owned-operation supervisor, pg_cancel_backend, and a stop drain barrier.
//! A cancelled statement is drained and rolled back before its connection can be
//! reused. Cancellation cannot undo a COMMIT already sent; stop waits for that
//! result rather than claiming guaranteed rollback. Drop alone cannot await this
//! barrier; manual callers that abort must call storage.drain_cancelled().
//! Each SQL operation uses its own transaction to close the pre-dispatch cancel
//! race. Consequently CURRENT_TIMESTAMP/CURRENT_DATE use BEGIN's time, one
//! round-trip earlier than the source's autocommit statement. Cleanup still
//! commits each batch separately and holds only one data connection.
//! start/stop return a sanitized error when cancellation cannot establish terminal
//! backend cleanup. An indeterminate operation stays latched; it is never silently
//! replayed or treated as successful shutdown. A failed control connection can
//! extend shutdown while the owned data statement drains.
//! Python DatabaseStorage's DB_COMMAND_TIMEOUT_S (default 30 seconds) is outside
//! this task slice. Normal-query timeout parity requires the caller's SQLx
//! session/deadline configuration; supplying pools alone does not recreate it.
//!
//! asyncpg 0.31.0 encodes the source's naive cutoff using PROCESS-local timezone
//! (py-pgproto a29a6f6, datetime.pyx:timestamptz_encode), independent of database
//! session timezone. Preserve that source quirk with an explicit TIMESTAMPTZ
//! binding and CPython's fold=0 local-time resolution, including DST gaps.

use crate::postgres_operation::{PgOperationConnection, PgOperationError, PgOperationStorage};
use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, TimeDelta, Timelike, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Mutex as AsyncMutex, task::JoinHandle};

pub type MaintenanceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, MaintenanceError>> + Send + 'a>>;
pub type MaintenanceSleep<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceError {
    Database,
    Collaborator,
    Cancelled,
    DateOutOfRange,
    CountOverflow,
    InvalidCancellationPool,
    IndeterminateCancellation,
}
impl fmt::Display for MaintenanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database => f.write_str("billing_maintenance_storage_unavailable"),
            Self::Collaborator => f.write_str("billing_maintenance_collaborator_failed"),
            Self::Cancelled => f.write_str("billing_maintenance_cancelled"),
            Self::DateOutOfRange => f.write_str("date value out of range"),
            Self::CountOverflow => f.write_str("billing_maintenance_count_overflow"),
            Self::InvalidCancellationPool => {
                f.write_str("billing_maintenance_independent_control_pool_required")
            }
            Self::IndeterminateCancellation => {
                f.write_str("billing_maintenance_cancellation_indeterminate")
            }
        }
    }
}
impl std::error::Error for MaintenanceError {}
impl From<sqlx::Error> for MaintenanceError {
    fn from(_: sqlx::Error) -> Self {
        Self::Database
    }
}

/// `utcnow` must return naive UTC at Python datetime's microsecond precision.
/// Dropping a sleep future must cancel its pending work.
pub trait MaintenanceRuntime: Send + Sync {
    fn utcnow(&self) -> NaiveDateTime;
    fn sleep(&self, seconds: f64) -> MaintenanceSleep<'_>;
}
pub struct SystemMaintenanceRuntime;
impl MaintenanceRuntime for SystemMaintenanceRuntime {
    fn utcnow(&self) -> NaiveDateTime {
        let now = Utc::now().naive_utc();
        now.with_nanosecond(now.nanosecond() / 1_000 * 1_000)
            .unwrap()
    }
    fn sleep(&self, seconds: f64) -> MaintenanceSleep<'_> {
        Box::pin(async move {
            if seconds <= 0.0 {
                tokio::task::yield_now().await;
            } else {
                tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
            }
        })
    }
}

/// One acquisition is retained throughout a complete cleanup, including pauses.
/// Connections/queries must be cancellation-safe when their futures are dropped.
pub trait MaintenanceStorage: Send + Sync {
    fn acquire(&self) -> MaintenanceFuture<'_, Box<dyn MaintenanceConnection>>;
    /// Wait only for operations whose caller already requested cancellation.
    /// The scheduler calls this after aborting and joining both source lanes.
    fn drain_cancelled(&self) -> MaintenanceFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
pub trait MaintenanceConnection: Send {
    fn aggregate_day(&mut self, date: NaiveDate) -> MaintenanceFuture<'_, u64>;
    fn delete_old_batch(&mut self, cutoff: NaiveDateTime, limit: i64)
    -> MaintenanceFuture<'_, u64>;
    fn reset_daily(&mut self) -> MaintenanceFuture<'_, u64>;
    fn reset_monthly(&mut self) -> MaintenanceFuture<'_, u64>;
}

pub const DAILY_AGGREGATION_SQL: &str = r#"
    INSERT INTO usage_daily_aggregates (
        tenant_id, user_id, model, assistant_id, service_id,
        date, request_count, success_count, error_count,
        total_input_tokens, total_output_tokens, total_cost_cents,
        avg_latency_ms, p95_latency_ms
    )
    SELECT
        tenant_id,
        COALESCE(user_id, '') as user_id,
        COALESCE(model, '') as model,
        COALESCE(assistant_id, '') as assistant_id,
        COALESCE(service_id, '') as service_id,
        $1::date as date,
        COUNT(*) as request_count,
        COUNT(*) FILTER (WHERE status = 'success') as success_count,
        COUNT(*) FILTER (WHERE status != 'success') as error_count,
        COALESCE(SUM(input_tokens), 0) as total_input_tokens,
        COALESCE(SUM(output_tokens), 0) as total_output_tokens,
        COALESCE(SUM(total_cost_cents), 0) as total_cost_cents,
        COALESCE(AVG(latency_ms)::integer, 0) as avg_latency_ms,
        COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY latency_ms)::integer, 0) as p95_latency_ms
    FROM usage_records
    WHERE created_at >= $1::date
      AND created_at < ($1::date + interval '1 day')
    GROUP BY tenant_id, COALESCE(user_id, ''), COALESCE(model, ''), COALESCE(assistant_id, ''), COALESCE(service_id, '')
    ON CONFLICT (tenant_id, user_id, model, assistant_id, service_id, date)
    DO UPDATE SET
        request_count = EXCLUDED.request_count,
        success_count = EXCLUDED.success_count,
        error_count = EXCLUDED.error_count,
        total_input_tokens = EXCLUDED.total_input_tokens,
        total_output_tokens = EXCLUDED.total_output_tokens,
        total_cost_cents = EXCLUDED.total_cost_cents,
        avg_latency_ms = EXCLUDED.avg_latency_ms,
        p95_latency_ms = EXCLUDED.p95_latency_ms,
        updated_at = CURRENT_TIMESTAMP
"#;
pub const CLEANUP_BATCH_SQL: &str = r#"
    DELETE FROM usage_records
    WHERE id IN (
        SELECT id FROM usage_records
        WHERE created_at < $1
        LIMIT $2
    )
"#;
pub const DAILY_QUOTA_RESET_SQL: &str = r#"
    UPDATE user_quotas
    SET
        current_daily_tokens = 0,
        daily_reset_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP
    WHERE daily_reset_at < CURRENT_DATE
       OR daily_reset_at IS NULL
"#;
pub const MONTHLY_QUOTA_RESET_SQL: &str = r#"
    UPDATE user_quotas
    SET
        current_monthly_cost_cents = 0,
        monthly_reset_at = CURRENT_TIMESTAMP,
        updated_at = CURRENT_TIMESTAMP
    WHERE monthly_reset_at < DATE_TRUNC('month', CURRENT_DATE)
       OR monthly_reset_at IS NULL
"#;

#[derive(Clone)]
pub struct PgMaintenanceStorage {
    operations: PgOperationStorage,
}
impl PgMaintenanceStorage {
    /// Both pools must reach the same PostgreSQL server/database/role. The
    /// control pool must be independent and remain open until stop drains.
    /// Requires direct or session-pinned connections on both pools. Transaction
    /// pooling, backend-reassigning proxies and failover/load-balanced endpoints
    /// are unsupported. Construction creates no connection or task.
    pub fn new(pool: PgPool, control_pool: PgPool) -> Result<Self, MaintenanceError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool, control_pool)?,
        })
    }
}
impl From<PgOperationError> for MaintenanceError {
    fn from(error: PgOperationError) -> Self {
        match error {
            PgOperationError::Database { .. } => Self::Database,
            PgOperationError::Cancelled => Self::Cancelled,
            PgOperationError::InvalidCancellationPool => Self::InvalidCancellationPool,
            PgOperationError::IndeterminateCancellation => Self::IndeterminateCancellation,
        }
    }
}
struct PgMaintenanceConnection {
    connection: PgOperationConnection,
}
impl MaintenanceStorage for PgMaintenanceStorage {
    fn acquire(&self) -> MaintenanceFuture<'_, Box<dyn MaintenanceConnection>> {
        Box::pin(async move {
            Ok(Box::new(PgMaintenanceConnection {
                connection: self.operations.acquire().await?,
            }) as Box<dyn MaintenanceConnection>)
        })
    }
    fn drain_cancelled(&self) -> MaintenanceFuture<'_, ()> {
        Box::pin(async move {
            self.operations
                .drain_cancelled()
                .await
                .map_err(MaintenanceError::from)
        })
    }
}
enum PgStatement {
    Aggregate(NaiveDate),
    Cleanup(AsyncpgCutoff, i64),
    Daily,
    Monthly,
}
/// Public SQLx Encode/Type customization for asyncpg's datetime sentinels.
/// No driver-private fields or cancellation protocol bytes are accessed.
enum AsyncpgCutoff {
    Finite(DateTime<Utc>),
    NegativeInfinity,
    Infinity,
}
impl sqlx::Type<Postgres> for AsyncpgCutoff {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <DateTime<Utc> as sqlx::Type<Postgres>>::type_info()
    }
}
impl<'q> sqlx::Encode<'q, Postgres> for AsyncpgCutoff {
    fn encode_by_ref(
        &self,
        buffer: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        match self {
            Self::Finite(value) => {
                <DateTime<Utc> as sqlx::Encode<'q, Postgres>>::encode_by_ref(value, buffer)
            }
            Self::NegativeInfinity => {
                <i64 as sqlx::Encode<'q, Postgres>>::encode_by_ref(&i64::MIN, buffer)
            }
            Self::Infinity => <i64 as sqlx::Encode<'q, Postgres>>::encode_by_ref(&i64::MAX, buffer),
        }
    }
    fn size_hint(&self) -> usize {
        8
    }
}
struct AsyncpgDate(NaiveDate);
impl sqlx::Type<Postgres> for AsyncpgDate {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <NaiveDate as sqlx::Type<Postgres>>::type_info()
    }
}
impl<'q> sqlx::Encode<'q, Postgres> for AsyncpgDate {
    fn encode_by_ref(
        &self,
        buffer: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        if self.0 == NaiveDate::from_ymd_opt(1, 1, 1).unwrap() {
            <i32 as sqlx::Encode<'q, Postgres>>::encode_by_ref(&i32::MIN, buffer)
        } else if self.0 == NaiveDate::from_ymd_opt(9999, 12, 31).unwrap() {
            <i32 as sqlx::Encode<'q, Postgres>>::encode_by_ref(&i32::MAX, buffer)
        } else {
            <NaiveDate as sqlx::Encode<'q, Postgres>>::encode_by_ref(&self.0, buffer)
        }
    }
    fn size_hint(&self) -> usize {
        4
    }
}
impl PgStatement {
    async fn execute(self, connection: &mut sqlx::PgConnection) -> Result<u64, sqlx::Error> {
        let result = match self {
            Self::Aggregate(date) => {
                sqlx::query(DAILY_AGGREGATION_SQL)
                    .bind(AsyncpgDate(date))
                    .execute(connection)
                    .await?
            }
            Self::Cleanup(cutoff, limit) => {
                sqlx::query(CLEANUP_BATCH_SQL)
                    .bind(cutoff)
                    .bind(limit)
                    .execute(connection)
                    .await?
            }
            Self::Daily => {
                sqlx::query(DAILY_QUOTA_RESET_SQL)
                    .execute(connection)
                    .await?
            }
            Self::Monthly => {
                sqlx::query(MONTHLY_QUOTA_RESET_SQL)
                    .execute(connection)
                    .await?
            }
        };
        Ok(result.rows_affected())
    }
}
impl PgMaintenanceConnection {
    fn execute(&mut self, statement: PgStatement) -> MaintenanceFuture<'_, u64> {
        Box::pin(async move {
            self.connection
                .execute(move |connection| Box::pin(statement.execute(connection)))
                .await
                .map_err(MaintenanceError::from)
        })
    }
}
impl MaintenanceConnection for PgMaintenanceConnection {
    fn aggregate_day(&mut self, date: NaiveDate) -> MaintenanceFuture<'_, u64> {
        self.execute(PgStatement::Aggregate(date))
    }
    fn delete_old_batch(
        &mut self,
        cutoff: NaiveDateTime,
        limit: i64,
    ) -> MaintenanceFuture<'_, u64> {
        // asyncpg checks these sentinels before calling datetime.astimezone.
        if cutoff
            == NaiveDate::from_ymd_opt(1, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
        {
            return self.execute(PgStatement::Cleanup(AsyncpgCutoff::NegativeInfinity, limit));
        }
        if cutoff
            == NaiveDate::from_ymd_opt(9999, 12, 31)
                .unwrap()
                .and_hms_micro_opt(23, 59, 59, 999_999)
                .unwrap()
        {
            return self.execute(PgStatement::Cleanup(AsyncpgCutoff::Infinity, limit));
        }
        match python_naive_cutoff_instant(cutoff) {
            Ok(cutoff) => self.execute(PgStatement::Cleanup(AsyncpgCutoff::Finite(cutoff), limit)),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
    fn reset_daily(&mut self) -> MaintenanceFuture<'_, u64> {
        self.execute(PgStatement::Daily)
    }
    fn reset_monthly(&mut self) -> MaintenanceFuture<'_, u64> {
        self.execute(PgStatement::Monthly)
    }
}

pub fn python_naive_cutoff_instant(
    cutoff: NaiveDateTime,
) -> Result<DateTime<Utc>, MaintenanceError> {
    python_naive_cutoff_instant_with(cutoff, |seconds| {
        DateTime::from_timestamp(seconds, 0)
            .map(|utc| utc.with_timezone(&Local).naive_local())
            .ok_or(MaintenanceError::DateOutOfRange)
    })
}
/// CPython _mktime/_local_timezone with fold=0, using an injected epoch->local
/// conversion for deterministic timezone tests. Cutoff arithmetic resets fold.
pub fn python_naive_cutoff_instant_with(
    cutoff: NaiveDateTime,
    utc_to_local: impl Fn(i64) -> Result<NaiveDateTime, MaintenanceError>,
) -> Result<DateTime<Utc>, MaintenanceError> {
    let t = cutoff.and_utc().timestamp();
    let local = |seconds| {
        let value = utc_to_local(seconds)?;
        if !(1..=9999).contains(&value.year()) {
            return Err(MaintenanceError::DateOutOfRange);
        }
        Ok(value.and_utc().timestamp())
    };
    let mktime = |fold: bool| -> Result<i64, MaintenanceError> {
        let a = local(t)? - t;
        let u1 = t - a;
        let t1 = local(u1)?;
        let b = if t1 == t {
            let u2 = u1 + if fold { 86_400 } else { -86_400 };
            let b = local(u2)? - u2;
            if a == b {
                return Ok(u1);
            }
            b
        } else {
            t1 - u1
        };
        let u2 = t - b;
        let t2 = local(u2)?;
        if t2 == t {
            Ok(u2)
        } else if t1 == t {
            Ok(u1)
        } else if fold {
            Ok(u1.min(u2))
        } else {
            Ok(u1.max(u2))
        }
    };
    // _local_timezone switches to the earlier candidate in fold/gap for fold=0.
    let seconds = mktime(false)?.min(mktime(true)?);
    let offset = local(seconds)? - seconds;
    DateTime::from_timestamp(t - offset, cutoff.nanosecond())
        .filter(|value| (1..=9999).contains(&value.year()))
        .ok_or(MaintenanceError::DateOutOfRange)
}

#[derive(Clone, Copy, Debug)]
pub enum AggregationDate {
    Date(NaiveDate),
    DateTime(NaiveDateTime),
}
impl From<NaiveDate> for AggregationDate {
    fn from(value: NaiveDate) -> Self {
        Self::Date(value)
    }
}
impl From<NaiveDateTime> for AggregationDate {
    fn from(value: NaiveDateTime) -> Self {
        Self::DateTime(value)
    }
}
impl<Tz: chrono::TimeZone> From<chrono::DateTime<Tz>> for AggregationDate {
    // Python calls target_date.date() without first converting its timezone.
    fn from(value: chrono::DateTime<Tz>) -> Self {
        Self::Date(value.date_naive())
    }
}

#[derive(Clone)]
pub struct AggregationTask {
    storage: Arc<dyn MaintenanceStorage>,
    runtime: Arc<dyn MaintenanceRuntime>,
    pub retention_days: i64,
}
impl AggregationTask {
    pub fn new(storage: Arc<dyn MaintenanceStorage>, retention_days: i64) -> Self {
        Self::with_runtime(storage, retention_days, Arc::new(SystemMaintenanceRuntime))
    }
    pub fn with_runtime(
        storage: Arc<dyn MaintenanceStorage>,
        retention_days: i64,
        runtime: Arc<dyn MaintenanceRuntime>,
    ) -> Self {
        Self {
            storage,
            runtime,
            retention_days,
        }
    }
    pub async fn run_daily_aggregation(
        &self,
        target: Option<AggregationDate>,
    ) -> Result<Value, MaintenanceError> {
        let date = match target {
            None => self
                .runtime
                .utcnow()
                .date()
                .pred_opt()
                .ok_or(MaintenanceError::DateOutOfRange)?,
            Some(AggregationDate::Date(date)) => date,
            Some(AggregationDate::DateTime(datetime)) => datetime.date(),
        };
        if !(1..=9999).contains(&date.year()) {
            return Err(MaintenanceError::DateOutOfRange);
        }
        let mut connection = self.storage.acquire().await?;
        let count = connection.aggregate_day(date).await?;
        Ok(json!({"date":date.to_string(),"aggregations_created":count,"status":"success"}))
    }
    pub async fn cleanup_old_records(&self) -> Result<Value, MaintenanceError> {
        let retention =
            TimeDelta::try_days(self.retention_days).ok_or(MaintenanceError::DateOutOfRange)?;
        let cutoff = self
            .runtime
            .utcnow()
            .checked_sub_signed(retention)
            .filter(|cutoff| (1..=9999).contains(&cutoff.year()))
            .ok_or(MaintenanceError::DateOutOfRange)?;
        let mut connection = self.storage.acquire().await?;
        let mut total = 0_u64;
        loop {
            let deleted = connection.delete_old_batch(cutoff, 10_000).await?;
            total = total
                .checked_add(deleted)
                .ok_or(MaintenanceError::CountOverflow)?;
            if deleted < 10_000 {
                break;
            }
            self.runtime.sleep(0.1).await;
        }
        Ok(
            json!({"cutoff_date":cutoff.date().to_string(),"records_deleted":total,"status":"success"}),
        )
    }
    pub async fn run_full_maintenance(&self) -> Result<Value, MaintenanceError> {
        let aggregation = ordinary_outcome(self.run_daily_aggregation(None).await)?;
        let cleanup = ordinary_outcome(self.cleanup_old_records().await)?;
        Ok(json!({"aggregation":aggregation,"cleanup":cleanup}))
    }
}
fn ordinary_outcome(result: Result<Value, MaintenanceError>) -> Result<Value, MaintenanceError> {
    match result {
        Ok(value) => Ok(value),
        Err(MaintenanceError::Cancelled) => Err(MaintenanceError::Cancelled),
        Err(error) => Ok(json!({"status":"error","error":error.to_string()})),
    }
}

#[derive(Clone)]
pub struct QuotaResetTask {
    storage: Arc<dyn MaintenanceStorage>,
    runtime: Arc<dyn MaintenanceRuntime>,
}
impl QuotaResetTask {
    pub fn new(storage: Arc<dyn MaintenanceStorage>) -> Self {
        Self::with_runtime(storage, Arc::new(SystemMaintenanceRuntime))
    }
    pub fn with_runtime(
        storage: Arc<dyn MaintenanceStorage>,
        runtime: Arc<dyn MaintenanceRuntime>,
    ) -> Self {
        Self { storage, runtime }
    }
    pub async fn reset_daily_quotas(&self) -> Result<Value, MaintenanceError> {
        let mut connection = self.storage.acquire().await?;
        let count = connection.reset_daily().await?;
        Ok(json!({"users_reset":count,"reset_type":"daily","status":"success"}))
    }
    pub async fn reset_monthly_quotas(&self) -> Result<Value, MaintenanceError> {
        let mut connection = self.storage.acquire().await?;
        let count = connection.reset_monthly().await?;
        Ok(json!({"users_reset":count,"reset_type":"monthly","status":"success"}))
    }
    pub async fn check_and_reset_quotas(&self) -> Result<Value, MaintenanceError> {
        let daily = ordinary_outcome(self.reset_daily_quotas().await)?;
        let mut results = json!({"daily":daily});
        // Sample after the daily query, including a failed query, as Python does.
        if self.runtime.utcnow().day() == 1 {
            results["monthly"] = ordinary_outcome(self.reset_monthly_quotas().await)?;
        }
        Ok(results)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageSchedulerConfig {
    pub retention_days: i64,
    pub aggregation_hour: i64,
    pub aggregation_minute: i64,
    pub cleanup_hour: i64,
    pub cleanup_minute: i64,
    pub quota_check_interval: i64,
}
impl Default for UsageSchedulerConfig {
    fn default() -> Self {
        Self {
            retention_days: 30,
            aggregation_hour: 0,
            aggregation_minute: 30,
            cleanup_hour: 1,
            cleanup_minute: 0,
            quota_check_interval: 3600,
        }
    }
}
#[derive(Default)]
struct LastRuns {
    aggregation: Option<NaiveDate>,
    cleanup: Option<NaiveDate>,
    quota: Option<NaiveDateTime>,
}
#[derive(Default)]
struct TaskInstances {
    aggregation: Option<AggregationTask>,
    quota: Option<QuotaResetTask>,
}
struct SchedulerInner {
    storage: Arc<dyn MaintenanceStorage>,
    runtime: Arc<dyn MaintenanceRuntime>,
    config: RwLock<UsageSchedulerConfig>,
    initialized: Mutex<TaskInstances>,
    last: Mutex<LastRuns>,
    running: AtomicBool,
}
impl SchedulerInner {
    fn aggregation_task(&self) -> AggregationTask {
        let mut initialized = self.initialized.lock().expect("billing tasks poisoned");
        initialized
            .aggregation
            .get_or_insert_with(|| {
                AggregationTask::with_runtime(
                    self.storage.clone(),
                    self.config
                        .read()
                        .expect("billing config poisoned")
                        .retention_days,
                    self.runtime.clone(),
                )
            })
            .clone()
    }
    fn quota_task(&self) -> QuotaResetTask {
        self.initialized
            .lock()
            .expect("billing tasks poisoned")
            .quota
            .get_or_insert_with(|| {
                QuotaResetTask::with_runtime(self.storage.clone(), self.runtime.clone())
            })
            .clone()
    }
    fn should_run(&self, now: NaiveDateTime, aggregation: bool) -> bool {
        let date = {
            let last = self.last.lock().expect("billing last runs poisoned");
            if aggregation {
                last.aggregation
            } else {
                last.cleanup
            }
        };
        if date == Some(now.date()) {
            return false;
        }
        let config = self.config.read().expect("billing config poisoned");
        let (hour, minute) = if aggregation {
            (config.aggregation_hour, config.aggregation_minute)
        } else {
            (config.cleanup_hour, config.cleanup_minute)
        };
        i64::from(now.hour()) == hour && i64::from(now.minute()) >= minute
    }
    async fn daily_once(&self) -> Result<Value, MaintenanceError> {
        let now = self.runtime.utcnow();
        let mut report = json!({});
        if self.should_run(now, true) {
            let task = self
                .initialized
                .lock()
                .expect("billing tasks poisoned")
                .aggregation
                .clone();
            if let Some(task) = task {
                report["aggregation"] = ordinary_outcome(task.run_daily_aggregation(None).await)?;
            }
            // Scheduled failures are swallowed before source updates this date.
            self.last
                .lock()
                .expect("billing last runs poisoned")
                .aggregation = Some(now.date());
        }
        if self.should_run(now, false) {
            let task = self
                .initialized
                .lock()
                .expect("billing tasks poisoned")
                .aggregation
                .clone();
            if let Some(task) = task {
                report["cleanup"] = ordinary_outcome(task.cleanup_old_records().await)?;
            }
            self.last
                .lock()
                .expect("billing last runs poisoned")
                .cleanup = Some(now.date());
        }
        Ok(report)
    }
    async fn quota_once(&self) -> Result<Option<Value>, MaintenanceError> {
        let task = self
            .initialized
            .lock()
            .expect("billing tasks poisoned")
            .quota
            .clone();
        let Some(task) = task else {
            return Ok(None);
        };
        match task.check_and_reset_quotas().await {
            Ok(result) => {
                self.last.lock().expect("billing last runs poisoned").quota =
                    Some(self.runtime.utcnow());
                Ok(Some(result))
            }
            Err(MaintenanceError::Cancelled) => Err(MaintenanceError::Cancelled),
            Err(error) => Ok(Some(json!({"status":"error","error":error.to_string()}))),
        }
    }
}

pub struct UsageScheduler {
    inner: Arc<SchedulerInner>,
    workers: AsyncMutex<Vec<JoinHandle<()>>>,
}
impl UsageScheduler {
    pub fn new(storage: Arc<dyn MaintenanceStorage>, config: UsageSchedulerConfig) -> Self {
        Self::with_runtime(storage, config, Arc::new(SystemMaintenanceRuntime))
    }
    pub fn with_runtime(
        storage: Arc<dyn MaintenanceStorage>,
        config: UsageSchedulerConfig,
        runtime: Arc<dyn MaintenanceRuntime>,
    ) -> Self {
        Self {
            inner: Arc::new(SchedulerInner {
                storage,
                runtime,
                config: RwLock::new(config),
                initialized: Mutex::new(TaskInstances::default()),
                last: Mutex::new(LastRuns::default()),
                running: AtomicBool::new(false),
            }),
            workers: AsyncMutex::new(Vec::new()),
        }
    }
    /// Config fields are mutable in Python. Already-created tasks retain their
    /// captured retention until start reconstructs them, matching that behavior.
    pub fn set_config(&self, config: UsageSchedulerConfig) {
        *self.inner.config.write().expect("billing config poisoned") = config;
    }
    pub fn should_run_aggregation(&self, now: NaiveDateTime) -> bool {
        self.inner.should_run(now, true)
    }
    pub fn should_run_cleanup(&self, now: NaiveDateTime) -> bool {
        self.inner.should_run(now, false)
    }
    pub async fn start(&self) -> Result<(), MaintenanceError> {
        let mut workers = self.workers.lock().await;
        if self.inner.running.load(Ordering::Acquire) {
            return Ok(());
        }
        // A cancelled stop waiter leaves aborted handles owned here. Reap them
        // before the cancellation snapshot so every dropped operation publishes.
        while let Some(worker) = workers.last_mut() {
            let _ = worker.await;
            workers.pop();
        }
        self.inner.storage.drain_cancelled().await?;
        self.inner.running.store(true, Ordering::Release);
        let config = self
            .inner
            .config
            .read()
            .expect("billing config poisoned")
            .clone();
        *self
            .inner
            .initialized
            .lock()
            .expect("billing tasks poisoned") = TaskInstances {
            aggregation: Some(AggregationTask::with_runtime(
                self.inner.storage.clone(),
                config.retention_days,
                self.inner.runtime.clone(),
            )),
            quota: Some(QuotaResetTask::with_runtime(
                self.inner.storage.clone(),
                self.inner.runtime.clone(),
            )),
        };
        let quota = self.inner.clone();
        let daily = self.inner.clone();
        workers.push(tokio::spawn(async move {
            while quota.running.load(Ordering::Acquire) {
                if matches!(quota.quota_once().await, Err(MaintenanceError::Cancelled)) {
                    break;
                }
                let seconds = quota
                    .config
                    .read()
                    .expect("billing config poisoned")
                    .quota_check_interval;
                quota.runtime.sleep(seconds as f64).await;
            }
        }));
        workers.push(tokio::spawn(async move {
            while daily.running.load(Ordering::Acquire) {
                if matches!(daily.daily_once().await, Err(MaintenanceError::Cancelled)) {
                    break;
                }
                daily.runtime.sleep(60.0).await;
            }
        }));
        Ok(())
    }
    pub async fn stop(&self) -> Result<(), MaintenanceError> {
        let mut workers = self.workers.lock().await;
        self.inner.running.store(false, Ordering::Release);
        for worker in workers.iter() {
            worker.abort();
        }
        // Never transfer handles into this waiter's locals: its cancellation
        // must not lose the ability to await operation-drop publication.
        while let Some(worker) = workers.last_mut() {
            let _ = worker.await;
            workers.pop();
        }
        self.inner.storage.drain_cancelled().await
    }
    /// One source daily-loop body, without sleeping. Returned outcomes are a
    /// native observation seam only; get_status retains the Python response.
    pub async fn daily_tasks_once(&self) -> Result<Value, MaintenanceError> {
        self.inner.daily_once().await
    }
    pub async fn quota_check_once(&self) -> Result<Option<Value>, MaintenanceError> {
        self.inner.quota_once().await
    }
    pub async fn trigger_aggregation(
        &self,
        target: Option<AggregationDate>,
    ) -> Result<Value, MaintenanceError> {
        self.inner
            .aggregation_task()
            .run_daily_aggregation(target)
            .await
    }
    pub async fn trigger_cleanup(&self) -> Result<Value, MaintenanceError> {
        self.inner.aggregation_task().cleanup_old_records().await
    }
    pub async fn trigger_quota_reset(&self, reset_type: &str) -> Result<Value, MaintenanceError> {
        let task = self.inner.quota_task();
        if reset_type == "monthly" {
            task.reset_monthly_quotas().await
        } else {
            task.reset_daily_quotas().await
        }
    }
    pub fn get_status(&self) -> Value {
        let config = self.inner.config.read().expect("billing config poisoned");
        let last = self.inner.last.lock().expect("billing last runs poisoned");
        json!({"running":self.inner.running.load(Ordering::Acquire),
            "retention_days":config.retention_days,
            "aggregation_time":format!("{:02}:{:02} UTC",config.aggregation_hour,config.aggregation_minute),
            "cleanup_time":format!("{:02}:{:02} UTC",config.cleanup_hour,config.cleanup_minute),
            "quota_check_interval_seconds":config.quota_check_interval,
            "last_aggregation_date":last.aggregation.map(|date|date.to_string()),
            "last_cleanup_date":last.cleanup.map(|date|date.to_string()),
            "last_quota_check":last.quota.map(python_isoformat)})
    }
}
fn python_isoformat(value: NaiveDateTime) -> String {
    if value.nanosecond() == 0 {
        value.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        value.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
    }
}
impl Drop for UsageScheduler {
    fn drop(&mut self) {
        self.inner.running.store(false, Ordering::Release);
        for worker in self.workers.get_mut().drain(..) {
            worker.abort();
        }
    }
}

static AGGREGATION_TASK: OnceLock<RwLock<Option<Arc<AggregationTask>>>> = OnceLock::new();
static QUOTA_RESET_TASK: OnceLock<RwLock<Option<Arc<QuotaResetTask>>>> = OnceLock::new();
static USAGE_SCHEDULER: OnceLock<RwLock<Option<Arc<UsageScheduler>>>> = OnceLock::new();
pub fn init_aggregation_task(
    storage: Arc<dyn MaintenanceStorage>,
    retention_days: i64,
) -> Arc<AggregationTask> {
    let task = Arc::new(AggregationTask::new(storage, retention_days));
    *AGGREGATION_TASK
        .get_or_init(|| RwLock::new(None))
        .write()
        .expect("aggregation singleton poisoned") = Some(task.clone());
    task
}
pub fn get_aggregation_task() -> Option<Arc<AggregationTask>> {
    AGGREGATION_TASK
        .get_or_init(|| RwLock::new(None))
        .read()
        .expect("aggregation singleton poisoned")
        .clone()
}
pub fn init_quota_reset_task(storage: Arc<dyn MaintenanceStorage>) -> Arc<QuotaResetTask> {
    let task = Arc::new(QuotaResetTask::new(storage));
    *QUOTA_RESET_TASK
        .get_or_init(|| RwLock::new(None))
        .write()
        .expect("quota singleton poisoned") = Some(task.clone());
    task
}
pub fn get_quota_reset_task() -> Option<Arc<QuotaResetTask>> {
    QUOTA_RESET_TASK
        .get_or_init(|| RwLock::new(None))
        .read()
        .expect("quota singleton poisoned")
        .clone()
}
pub fn init_usage_scheduler(
    storage: Arc<dyn MaintenanceStorage>,
    config: UsageSchedulerConfig,
) -> Arc<UsageScheduler> {
    let scheduler = Arc::new(UsageScheduler::new(storage, config));
    *USAGE_SCHEDULER
        .get_or_init(|| RwLock::new(None))
        .write()
        .expect("scheduler singleton poisoned") = Some(scheduler.clone());
    scheduler
}
pub fn get_usage_scheduler() -> Option<Arc<UsageScheduler>> {
    USAGE_SCHEDULER
        .get_or_init(|| RwLock::new(None))
        .read()
        .expect("scheduler singleton poisoned")
        .clone()
}
