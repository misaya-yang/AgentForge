//! Owned PostgreSQL statement supervision shared by caller-owned storage adapters.
//!
//! Construction is inert. Data and independent control pools must target the same
//! server/database/role using direct or session-pinned connections. Each operation
//! owns a transaction and a supervisor until terminal drain/rollback, even if its
//! caller drops. Aborting callers must subsequently await `drain_cancelled`.
//! Normal query deadlines remain host-owned. BEGIN establishes transaction time
//! one round-trip earlier than source autocommit; cancellation cannot undo COMMIT
//! already dispatched. Uncertain control signals quarantine the data connection.

use chrono::{DateTime, Utc};
use sqlx::{Connection, PgPool, Postgres, pool::PoolConnection};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, Notify};

pub type PgQueryFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, sqlx::Error>> + Send + 'a>>;
pub type PgOperationFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, PgOperationError>> + Send + 'a>>;

/// Retain only the machine-readable SQLSTATE, never backend text, SQL or values.
#[derive(Clone, PartialEq, Eq)]
pub enum PgOperationError {
    Database { sqlstate: Option<String> },
    Cancelled,
    InvalidCancellationPool,
    IndeterminateCancellation,
}
impl PgOperationError {
    fn database() -> Self {
        Self::Database { sqlstate: None }
    }
    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            Self::Database { sqlstate } => sqlstate.as_deref(),
            _ => None,
        }
    }
}
impl From<sqlx::Error> for PgOperationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database {
            sqlstate: error
                .as_database_error()
                .and_then(|error| error.code())
                .map(|code| code.into_owned()),
        }
    }
}
impl fmt::Display for PgOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database { .. } => "postgres_operation_unavailable",
            Self::Cancelled => "postgres_operation_cancelled",
            Self::InvalidCancellationPool => "postgres_independent_control_pool_required",
            Self::IndeterminateCancellation => "postgres_cancellation_indeterminate",
        })
    }
}
impl fmt::Debug for PgOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for PgOperationError {}

#[derive(Clone)]
pub struct PgOperationStorage {
    pool: PgPool,
    control_pool: PgPool,
    operations: Arc<Mutex<Vec<Arc<PgOperation>>>>,
}
impl PgOperationStorage {
    /// Both pools must reach the same PostgreSQL server/database/role. The
    /// control pool must be independent and remain open until stop drains.
    /// Requires direct or session-pinned connections on both pools. Transaction
    /// pooling, backend-reassigning proxies and failover/load-balanced endpoints
    /// are unsupported: a client socket alone cannot retain a backend there.
    /// No pool, connection or task is created by this constructor.
    pub fn new(pool: PgPool, control_pool: PgPool) -> Result<Self, PgOperationError> {
        if std::ptr::eq(pool.options(), control_pool.options()) {
            return Err(PgOperationError::InvalidCancellationPool);
        }
        Ok(Self {
            pool,
            control_pool,
            operations: Arc::new(Mutex::new(Vec::new())),
        })
    }
}
struct PgOperation {
    cancelled: AtomicBool,
    cancel: Notify,
    finished: AtomicBool,
    settled: AtomicBool,
    acknowledged: AtomicBool,
    control_uncertain: AtomicBool,
    completion: Notify,
}
impl PgOperation {
    fn retain(&self) -> bool {
        !self.finished.load(Ordering::Acquire)
            || (!self.settled.load(Ordering::Acquire) && !self.acknowledged.load(Ordering::Acquire))
    }
    async fn cancelled(&self) {
        loop {
            let notified = self.cancel.notified();
            if self.cancelled.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
    async fn finished(&self) {
        loop {
            let notified = self.completion.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.finished.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}
struct CancelOperation(Option<Arc<PgOperation>>);
impl Drop for CancelOperation {
    fn drop(&mut self) {
        if let Some(operation) = &self.0 {
            operation.cancelled.store(true, Ordering::Release);
            operation.cancel.notify_one();
        }
    }
}
struct FinishOperation(Arc<PgOperation>);
impl Drop for FinishOperation {
    fn drop(&mut self) {
        self.0.finished.store(true, Ordering::Release);
        self.0.completion.notify_waiters();
    }
}
pub struct PgOperationConnection {
    connection: Arc<AsyncMutex<PoolConnection<Postgres>>>,
    pid: i32,
    backend_start: DateTime<Utc>,
    control_pool: PgPool,
    operations: Arc<Mutex<Vec<Arc<PgOperation>>>>,
    invalid: Arc<AtomicBool>,
}
#[derive(sqlx::FromRow)]
struct PgBackendIdentity {
    pid: i32,
    backend_start: DateTime<Utc>,
    server_addr: Option<String>,
    server_port: Option<i32>,
    postmaster_start: DateTime<Utc>,
    database_name: String,
    role_name: String,
}
impl PgOperationStorage {
    pub fn acquire(&self) -> PgOperationFuture<'_, PgOperationConnection> {
        Box::pin(async move {
            let mut connection = self.pool.acquire().await?;
            let identity:PgBackendIdentity =
                sqlx::query_as("SELECT pid,backend_start,inet_server_addr()::text AS server_addr,inet_server_port() AS server_port,pg_postmaster_start_time() AS postmaster_start,current_database()::text AS database_name,current_user::text AS role_name FROM pg_catalog.pg_stat_activity WHERE pid=pg_backend_pid()")
                    .fetch_one(&mut *connection).await?;
            // Validate the endpoint and exact owned backend before any mutation.
            // This also rejects a proxy whose control pool reaches another node.
            let valid: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1 AND backend_start=$2 AND inet_server_addr()::text IS NOT DISTINCT FROM $3 AND inet_server_port() IS NOT DISTINCT FROM $4 AND pg_postmaster_start_time()=$5 AND current_database()=$6 AND current_user=$7 AND datname=current_database() AND usename=current_user)")
                .bind(identity.pid).bind(identity.backend_start).bind(&identity.server_addr).bind(identity.server_port)
                .bind(identity.postmaster_start).bind(&identity.database_name).bind(&identity.role_name)
                .fetch_one(&self.control_pool).await?;
            if !valid {
                return Err(PgOperationError::InvalidCancellationPool);
            }
            Ok(PgOperationConnection {
                connection: Arc::new(AsyncMutex::new(connection)),
                pid: identity.pid,
                backend_start: identity.backend_start,
                control_pool: self.control_pool.clone(),
                operations: self.operations.clone(),
                invalid: Arc::new(AtomicBool::new(false)),
            })
        })
    }
    pub fn drain_cancelled(&self) -> PgOperationFuture<'_, ()> {
        Box::pin(async move {
            let cancelled: Vec<_> = self
                .operations
                .lock()
                .expect("postgres operations poisoned")
                .iter()
                .filter(|operation| operation.cancelled.load(Ordering::Acquire))
                .cloned()
                .collect();
            let mut settled = true;
            for operation in cancelled {
                operation.finished().await;
                settled &= operation.settled.load(Ordering::Acquire);
            }
            self.operations
                .lock()
                .expect("postgres operations poisoned")
                .retain(|operation| operation.retain());
            if settled {
                Ok(())
            } else {
                Err(PgOperationError::IndeterminateCancellation)
            }
        })
    }
}
async fn pg_drain<T>(
    future: impl Future<Output = Result<T, sqlx::Error>>,
    operation: &PgOperation,
    control: &PgPool,
    pid: i32,
    backend_start: DateTime<Utc>,
) -> Result<T, sqlx::Error> {
    if operation.cancelled.load(Ordering::Acquire) {
        return Err(sqlx::Error::Protocol("postgres_operation_cancelled".into()));
    }
    tokio::pin!(future);
    tokio::select! {
        biased;
        result = &mut future => return result,
        _ = operation.cancelled() => (),
    }
    // SQLx can still be preparing/transmitting when cancellation arrives. Keep
    // cancellation active until the owned query is terminal, rather than assuming
    // a single early request reached an already-running statement.
    loop {
        let cancel_started = AtomicBool::new(false);
        let cancel = async {
            cancel_started.store(true, Ordering::Release);
            sqlx::query_scalar::<_,bool>("SELECT COALESCE(bool_or(pg_catalog.pg_cancel_backend(pid)),FALSE) FROM pg_catalog.pg_stat_activity WHERE pid=$1 AND backend_start=$2 AND datname=current_database() AND usename=current_user")
                .bind(pid).bind(backend_start).fetch_one(control).await
        };
        tokio::pin!(cancel);
        tokio::select! {
            biased;
            result = &mut future => {
                // Never recycle the data connection while a late control request
                // could still reach its backend and hit a subsequent borrower.
                if cancel_started.load(Ordering::Acquire) && cancel.await.is_err() {
                    operation.control_uncertain.store(true,Ordering::Release);
                }
                return result;
            }
            result = &mut cancel => {
                if result.is_err() { operation.control_uncertain.store(true,Ordering::Release); }
            },
        }
        tokio::select! {
            biased;
            result = &mut future => return result,
            _ = tokio::time::sleep(Duration::from_millis(20)) => (),
        }
    }
}
impl PgOperationConnection {
    pub fn execute<T, F>(&mut self, query: F) -> PgOperationFuture<'_, T>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut sqlx::PgConnection) -> PgQueryFuture<'c, T> + Send + 'static,
    {
        Box::pin(async move {
            let operation = Arc::new(PgOperation {
                cancelled: AtomicBool::new(false),
                cancel: Notify::new(),
                finished: AtomicBool::new(false),
                settled: AtomicBool::new(false),
                acknowledged: AtomicBool::new(false),
                control_uncertain: AtomicBool::new(false),
                completion: Notify::new(),
            });
            {
                let mut operations = self
                    .operations
                    .lock()
                    .expect("postgres operations poisoned");
                operations.retain(|operation| operation.retain());
                operations.push(operation.clone());
            }
            let mut cancellation = CancelOperation(Some(operation.clone()));
            let acknowledgement = operation.clone();
            let owned_connection = self.connection.clone();
            let control = self.control_pool.clone();
            let invalid = self.invalid.clone();
            let pid = self.pid;
            let backend_start = self.backend_start;
            let task = tokio::spawn(async move {
                let _finished = FinishOperation(operation.clone());
                let mut connection = tokio::select! {
                    biased;
                    _ = operation.cancelled() => {
                        operation.settled.store(true,Ordering::Release);
                        return Err(PgOperationError::Cancelled);
                    },
                    connection = owned_connection.lock() => connection,
                };
                if invalid.load(Ordering::Acquire) {
                    operation.settled.store(true, Ordering::Release);
                    return Err(PgOperationError::database());
                }
                let mut transaction = connection.begin().await?;
                if operation.cancelled.load(Ordering::Acquire) {
                    let rollback = transaction.rollback().await;
                    if rollback.is_err() || operation.control_uncertain.load(Ordering::Acquire) {
                        invalid.store(true, Ordering::Release);
                        connection.close_on_drop();
                    }
                    rollback?;
                    operation.settled.store(true, Ordering::Release);
                    return Err(PgOperationError::Cancelled);
                }
                let result = pg_drain(
                    query(&mut transaction),
                    &operation,
                    &control,
                    pid,
                    backend_start,
                )
                .await;
                if operation.cancelled.load(Ordering::Acquire) || result.is_err() {
                    // A late-prepared query may have run after an early cancel.
                    // Rollback, rather than persisting such unobserved writes.
                    let rollback = transaction.rollback().await;
                    // On a control error its server-side signal may be uncertain.
                    // Quarantine rather than reusing this backend for new queries.
                    if rollback.is_err() || operation.control_uncertain.load(Ordering::Acquire) {
                        invalid.store(true, Ordering::Release);
                        connection.close_on_drop();
                    }
                    rollback?;
                    operation.settled.store(true, Ordering::Release);
                    if operation.cancelled.load(Ordering::Acquire) {
                        return Err(PgOperationError::Cancelled);
                    }
                    return result.map_err(PgOperationError::from);
                }
                let value = result.map_err(PgOperationError::from)?;
                // COMMIT may win a cancellation race. We drain its terminal
                // response; neither this method nor stop claims to undo it.
                let commit = pg_drain(
                    transaction.commit(),
                    &operation,
                    &control,
                    pid,
                    backend_start,
                )
                .await;
                if let Err(commit_error) = commit {
                    if operation.control_uncertain.load(Ordering::Acquire) {
                        invalid.store(true, Ordering::Release);
                        connection.close_on_drop();
                    }
                    // A failed/dropped COMMIT future queues rollback in SQLx.
                    // Explicitly drain/rollback before claiming terminal cleanup.
                    sqlx::query("ROLLBACK").execute(&mut **connection).await?;
                    operation.settled.store(true, Ordering::Release);
                    return Err(PgOperationError::from(commit_error));
                }
                if operation.control_uncertain.load(Ordering::Acquire) {
                    invalid.store(true, Ordering::Release);
                    connection.close_on_drop();
                }
                operation.settled.store(true, Ordering::Release);
                if operation.cancelled.load(Ordering::Acquire) {
                    Err(PgOperationError::Cancelled)
                } else {
                    Ok(value)
                }
            });
            let result = task.await.map_err(|_| PgOperationError::database())?;
            // The supervisor is already finished. Suppress caller-drop's flag.
            acknowledgement.acknowledged.store(true, Ordering::Release);
            cancellation.0.take();
            result
        })
    }
}
