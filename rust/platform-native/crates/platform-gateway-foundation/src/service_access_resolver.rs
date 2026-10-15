//! Request/process cache and native read adapter for service-access constraints.
//!
//! Authority: `src/core/auth/service_access_resolver.py` and DatabaseStorage's
//! get_tenant/get_user. This module reuses the policy loader, not authentication
//! repositories: those repositories add status/tenant predicates absent here.
//! Hosts must supply already resolved identities and a stable database-object
//! identity, the database type's module, and the configured proxy TTL. No route,
//! credentials, environment, connection, task, or policy defaults are installed.
//!
//! Request snapshots survive global invalidation. Global entries are copied on
//! read and write; eviction is insertion-order FIFO, not LRU. At capacity even
//! replacing an existing key first evicts the oldest entry. Locks protect only
//! cache operations, never database awaits. Source has no single-flight or
//! invalidation generation fence: an in-flight load may repopulate after clear.
//! Negative/invalid TTL disables only global caching; request reuse remains.
//! Source's NaN and positive infinity TTL behavior is deliberately preserved.
//!
//! The PostgreSQL adapter projects only consumed fields without adding filters.
//! SQLx JSON decoding equals asyncpg JSON text followed by _row_to_dict decoding.
//! Caller-owned independent data/control pools reuse PgOperationStorage's owned
//! cancellation supervision. Abort and join callers, then drain_cancelled before
//! closing pools. Both pools must target the same direct/session-pinned endpoint.
//! That shared adapter adds a data-backend identity query, independent control
//! validation query, BEGIN, and terminal COMMIT/ROLLBACK around each source read.

use crate::{
    postgres_operation::{PgOperationError, PgOperationStorage},
    service_access_policy::{
        ServiceAccessConstraintStore, ServiceAccessConstraints, ServiceAccessError,
        ServiceAccessLoadContext, ServiceAccessLookupError, ServiceAccessLookupFailure,
        ServiceAccessLookupFuture, load_uncached_service_access_constraints,
        normalize_service_scope,
    },
    service_registry::python_truthy,
    usage_parser::{python_float_string, python_value_to_string},
};
use serde_json::Value;
use sqlx::PgPool;
use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    fmt,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

pub const SERVICE_ACCESS_CACHE_MAX_SIZE: usize = 5000;

/// JSON-facing boundary for values the Python request/app/UserContext contains.
/// database_identity must be stable for that host database object's lifetime;
/// use the same sentinel for a missing DB on every request (Python id(None)).
/// No fields except the six original key parts are added to the cache key.
#[derive(Clone, Default)]
pub struct ServiceAccessResolverContext {
    pub load: ServiceAccessLoadContext,
    pub database_identity: usize,
    pub database_type_module: String,
    /// Immutable-settings snapshot. A host that can reconfigure TTL during an
    /// awaited lookup must install ServiceAccessCacheContextProvider on its resolver.
    pub configured_ttl: Value,
    pub api_key_hash: Value,
    pub roles: Vec<Value>,
}

pub fn build_service_access_cache_key(context: &ServiceAccessResolverContext) -> String {
    let key_hash = if python_truthy(&context.api_key_hash) {
        python_value_to_string(&context.api_key_hash)
    } else {
        String::new()
    };
    let allowed = context
        .load
        .api_key_info
        .as_object()
        .and_then(|info| info.get("allowed_services"))
        .map(normalize_service_scope)
        .unwrap_or_default()
        .join(",");
    let mut roles: Vec<_> = context.roles.iter().map(python_value_to_string).collect();
    roles.sort();
    [
        context.database_identity.to_string(),
        context.load.user_id.clone(),
        context.load.tenant_id.clone(),
        key_hash,
        allowed,
        roles.join(","),
    ]
    .join("|")
}

/// Preserve max(float(ttl or 0.0), 0.0), including NaN (first max argument),
/// positive infinity, bools, Unicode decimal digits, and numeric underscores.
/// The explicit mock-module bypass never disables request-scoped reuse.
pub fn constraint_cache_ttl_seconds(database_type_module: &str, configured_ttl: &Value) -> f64 {
    if matches!(database_type_module, "types" | "unittest.mock") {
        return 0.0;
    }
    let ttl = if !python_truthy(configured_ttl) {
        0.0
    } else {
        match configured_ttl {
            Value::Bool(value) => f64::from(u8::from(*value)),
            Value::Number(value) => value.as_f64().unwrap_or(0.0),
            // str.strip accepts four control separators that float() rejects.
            Value::String(value) if !value.chars().any(|c| matches!(c, '\u{1c}'..='\u{1f}')) => {
                python_float_string(value).unwrap_or(0.0)
            }
            _ => 0.0,
        }
    };
    if ttl < 0.0 { 0.0 } else { ttl }
}

/// Live cache-input seam: read independently before global lookup and after
/// awaited loading before publication. The returned context supplies cache key,
/// TTL and current DB type module only; it never changes the in-flight store or
/// the original trusted identity/inputs supplied to the uncached policy loader.
/// Snapshot current app/request data without side effects; the resolver applies
/// Python's module bypass before parsing the returned configured TTL.
pub trait ServiceAccessCacheContextProvider: Send + Sync {
    fn cache_context(
        &self,
        resolved: &ServiceAccessResolverContext,
    ) -> ServiceAccessResolverContext;
}

pub trait ServiceAccessClock: Send + Sync {
    /// Seconds from a monotonic origin, never wall-clock Unix timestamps.
    fn monotonic_seconds(&self) -> f64;
}
pub struct MonotonicServiceAccessClock(Instant);
impl Default for MonotonicServiceAccessClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}
impl ServiceAccessClock for MonotonicServiceAccessClock {
    fn monotonic_seconds(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }
}

/// One instance belongs to one request. Clone of the returned Arc denotes the
/// exact request snapshot; new requests must start with a fresh cache. Rust's
/// immutable policy snapshot replaces Python's mutable tuple/list container.
#[derive(Default)]
pub struct ServiceAccessRequestCache {
    constraints: Option<Arc<ServiceAccessConstraints>>,
}
impl ServiceAccessRequestCache {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn snapshot(&self) -> Option<Arc<ServiceAccessConstraints>> {
        self.constraints.clone()
    }
}
struct CacheEntry {
    expires_at: f64,
    constraints: ServiceAccessConstraints,
}
#[derive(Default)]
struct CacheState {
    entries: HashMap<String, CacheEntry>,
    insertion_order: VecDeque<String>,
}
struct ResolverInner {
    state: Mutex<CacheState>,
    clock: Arc<dyn ServiceAccessClock>,
}
fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
static PROCESS_CACHE: OnceLock<Arc<ResolverInner>> = OnceLock::new();

#[derive(Clone)]
pub struct ServiceAccessConstraintResolver {
    inner: Arc<ResolverInner>,
    cache_context_provider: Option<Arc<dyn ServiceAccessCacheContextProvider>>,
}
impl Default for ServiceAccessConstraintResolver {
    fn default() -> Self {
        Self::new()
    }
}
impl ServiceAccessConstraintResolver {
    /// Shares the one in-process cache, including across independently created
    /// handles. Construction is inert and does not load any constraints.
    pub fn new() -> Self {
        Self {
            inner: PROCESS_CACHE
                .get_or_init(|| {
                    Arc::new(ResolverInner {
                        state: Mutex::new(CacheState::default()),
                        clock: Arc::new(MonotonicServiceAccessClock::default()),
                    })
                })
                .clone(),
            cache_context_provider: None,
        }
    }
    /// Explicit host-owned cache scope and monotonic clock seam. Clone/share this
    /// handle across requests and all hosts that should share invalidation.
    pub fn with_clock(clock: Arc<dyn ServiceAccessClock>) -> Self {
        Self {
            inner: Arc::new(ResolverInner {
                state: Mutex::new(CacheState::default()),
                clock,
            }),
            cache_context_provider: None,
        }
    }
    /// Attach current cache-input authority without replacing the shared cache.
    /// Cloned handles retain it. Required when settings, DB identity/module, or
    /// request key dimensions can change while an uncached lookup is awaiting.
    /// Without it the explicit context is an immutable-host snapshot boundary.
    pub fn with_cache_context_provider(
        mut self,
        provider: Arc<dyn ServiceAccessCacheContextProvider>,
    ) -> Self {
        self.cache_context_provider = Some(provider);
        self
    }
    fn current_cache_context<'a>(
        &self,
        context: &'a ServiceAccessResolverContext,
    ) -> Cow<'a, ServiceAccessResolverContext> {
        match &self.cache_context_provider {
            Some(provider) => Cow::Owned(provider.cache_context(context)),
            None => Cow::Borrowed(context),
        }
    }
    pub fn clear_cache(&self) {
        *lock(&self.inner.state) = CacheState::default();
    }
    pub fn cached_entry_count(&self) -> usize {
        lock(&self.inner.state).entries.len()
    }

    fn cached(&self, context: &ServiceAccessResolverContext) -> Option<ServiceAccessConstraints> {
        let context = self.current_cache_context(context);
        if constraint_cache_ttl_seconds(&context.database_type_module, &context.configured_ttl)
            <= 0.0
        {
            return None;
        }
        let now = self.inner.clock.monotonic_seconds();
        let key = build_service_access_cache_key(&context);
        let mut state = lock(&self.inner.state);
        let entry = state.entries.get(&key)?;
        if now >= entry.expires_at {
            state.entries.remove(&key);
            state.insertion_order.retain(|existing| existing != &key);
            return None;
        }
        Some(entry.constraints.clone())
    }
    fn publish(
        &self,
        context: &ServiceAccessResolverContext,
        constraints: &ServiceAccessConstraints,
    ) {
        let context = self.current_cache_context(context);
        let ttl =
            constraint_cache_ttl_seconds(&context.database_type_module, &context.configured_ttl);
        if ttl <= 0.0 {
            return;
        }
        let key = build_service_access_cache_key(&context);
        let entry = CacheEntry {
            expires_at: self.inner.clock.monotonic_seconds() + ttl,
            constraints: constraints.clone(),
        };
        let mut state = lock(&self.inner.state);
        // Evict before checking whether this key already exists: Python does.
        while state.entries.len() >= SERVICE_ACCESS_CACHE_MAX_SIZE {
            if let Some(oldest) = state.insertion_order.pop_front() {
                state.entries.remove(&oldest);
            }
        }
        if !state.entries.contains_key(&key) {
            state.insertion_order.push_back(key.clone());
        }
        state.entries.insert(key, entry);
    }
    pub async fn load(
        &self,
        request: &mut ServiceAccessRequestCache,
        context: &ServiceAccessResolverContext,
        database: Option<&dyn ServiceAccessConstraintStore>,
    ) -> Result<Arc<ServiceAccessConstraints>, ServiceAccessError> {
        if let Some(constraints) = &request.constraints {
            return Ok(constraints.clone());
        }
        if let Some(constraints) = self.cached(context) {
            let constraints = Arc::new(constraints);
            request.constraints = Some(constraints.clone());
            return Ok(constraints);
        }
        let load = load_uncached_service_access_constraints(&context.load, database).await?;
        let constraints = Arc::new(load.constraints);
        request.constraints = Some(constraints.clone());
        self.publish(context, &constraints);
        Ok(constraints)
    }
}

/// Same global invalidation scope as the Python test/config/user/API-key helper.
/// Does not invalidate request snapshots, cancel lookups, or set a generation.
pub fn clear_service_access_constraint_cache() {
    if let Some(cache) = PROCESS_CACHE.get() {
        *lock(&cache.state) = CacheState::default();
    }
}

/// Exact consumed projections of SELECT * with unchanged identity-only WHERE.
/// Unqualified names preserve host-owned search_path and isolated fixture use.
pub const SELECT_SERVICE_ACCESS_TENANT_SQL: &str = "SELECT jsonb_build_object('allowed_services', allowed_services) FROM tenants WHERE tenant_id = $1";
pub const SELECT_SERVICE_ACCESS_USER_SQL: &str =
    "SELECT jsonb_build_object('metadata', metadata) FROM users WHERE user_id = $1";

/// Required host logging hook. No built-in allow-all store or silent warning sink
/// exists. Do not include SQL/backend messages or credentials in diagnostics.
pub trait ServiceAccessWarningSink: Send + Sync {
    fn warn_lookup_failure(&self, source: ServiceAccessLookupFailure, identity: &str);
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServiceAccessPostgresStage {
    Acquire,
    Query,
    Drain,
}
#[derive(Clone, PartialEq, Eq)]
pub enum ServiceAccessPostgresError {
    Database {
        stage: ServiceAccessPostgresStage,
        sqlstate: Option<String>,
    },
    Cancelled,
    InvalidCancellationPool,
    IndeterminateCancellation,
}
impl ServiceAccessPostgresError {
    fn operation(error: PgOperationError, stage: ServiceAccessPostgresStage) -> Self {
        match error {
            PgOperationError::Database { sqlstate } => Self::Database { stage, sqlstate },
            PgOperationError::Cancelled => Self::Cancelled,
            PgOperationError::InvalidCancellationPool => Self::InvalidCancellationPool,
            PgOperationError::IndeterminateCancellation => Self::IndeterminateCancellation,
        }
    }
    pub fn stage(&self) -> Option<ServiceAccessPostgresStage> {
        match self {
            Self::Database { stage, .. } => Some(*stage),
            _ => None,
        }
    }
    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            Self::Database { sqlstate, .. } => sqlstate.as_deref(),
            _ => None,
        }
    }
    fn lookup(self) -> ServiceAccessLookupError {
        if self == Self::Cancelled {
            ServiceAccessLookupError::Cancelled
        } else {
            ServiceAccessLookupError::Ordinary
        }
    }
}
impl fmt::Display for ServiceAccessPostgresError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database { .. } => "service_access_postgres_unavailable",
            Self::Cancelled => "service_access_postgres_cancelled",
            Self::InvalidCancellationPool => "service_access_independent_control_pool_required",
            Self::IndeterminateCancellation => "service_access_cancellation_indeterminate",
        })
    }
}
impl fmt::Debug for ServiceAccessPostgresError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for ServiceAccessPostgresError {}

#[derive(Clone)]
pub struct PgServiceAccessConstraintStore {
    operations: PgOperationStorage,
    warnings: Arc<dyn ServiceAccessWarningSink>,
}
impl PgServiceAccessConstraintStore {
    pub fn new(
        pool: PgPool,
        control_pool: PgPool,
        warnings: Arc<dyn ServiceAccessWarningSink>,
    ) -> Result<Self, ServiceAccessPostgresError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool, control_pool).map_err(|error| {
                ServiceAccessPostgresError::operation(error, ServiceAccessPostgresStage::Acquire)
            })?,
            warnings,
        })
    }
    async fn lookup(
        &self,
        sql: &'static str,
        identity: &str,
    ) -> Result<Value, ServiceAccessPostgresError> {
        let identity = identity.to_owned();
        let mut connection = self.operations.acquire().await.map_err(|error| {
            ServiceAccessPostgresError::operation(error, ServiceAccessPostgresStage::Acquire)
        })?;
        connection
            .execute(move |connection| {
                Box::pin(async move {
                    let row: Option<Value> = sqlx::query_scalar(sql)
                        .bind(identity)
                        .fetch_optional(connection)
                        .await?;
                    Ok(row.unwrap_or(Value::Null))
                })
            })
            .await
            .map_err(|error| {
                ServiceAccessPostgresError::operation(error, ServiceAccessPostgresStage::Query)
            })
    }
    pub async fn tenant_record(
        &self,
        tenant_id: &str,
    ) -> Result<Value, ServiceAccessPostgresError> {
        self.lookup(SELECT_SERVICE_ACCESS_TENANT_SQL, tenant_id)
            .await
    }
    pub async fn user_record(&self, user_id: &str) -> Result<Value, ServiceAccessPostgresError> {
        self.lookup(SELECT_SERVICE_ACCESS_USER_SQL, user_id).await
    }
    pub async fn drain_cancelled(&self) -> Result<(), ServiceAccessPostgresError> {
        self.operations.drain_cancelled().await.map_err(|error| {
            ServiceAccessPostgresError::operation(error, ServiceAccessPostgresStage::Drain)
        })
    }
}
impl ServiceAccessConstraintStore for PgServiceAccessConstraintStore {
    fn enabled(&self) -> bool {
        true
    }
    fn get_tenant<'a>(&'a self, tenant_id: &'a str) -> ServiceAccessLookupFuture<'a> {
        Box::pin(async move {
            self.tenant_record(tenant_id)
                .await
                .map_err(ServiceAccessPostgresError::lookup)
        })
    }
    fn get_user<'a>(&'a self, user_id: &'a str) -> ServiceAccessLookupFuture<'a> {
        Box::pin(async move {
            self.user_record(user_id)
                .await
                .map_err(ServiceAccessPostgresError::lookup)
        })
    }
    fn warn_lookup_failure(&self, source: ServiceAccessLookupFailure, identity: &str) {
        self.warnings.warn_lookup_failure(source, identity);
    }
}

