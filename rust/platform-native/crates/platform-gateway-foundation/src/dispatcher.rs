//! Execution slice of `src/core/gateway/dispatcher.py`.
//!
//! This is not an HTTP router, service registry, authorization boundary, or new
//! provider client. The caller must select the tenant-authorized service, perform
//! RequestValidator/RBAC and legacy rate-limit checks, and persist the initial
//! session message before invoking this core. Session/usage recording, task
//! submission and final `stream_end` metadata remain at their original boundaries.
//! `provider_routing` remains the owner of provider/model selection; dispatcher
//! retries never select a different provider or invent an idempotency guarantee.
//!
//! One sync invocation has 1..=3 total attempts, a timeout on each adapter call,
//! and a separately acquired/released capacity lease for every attempt. Only
//! ConnectError, RemoteProtocolError and PoolTimeout on GET/HEAD/OPTIONS can be
//! replayed. Mutation timeouts/ambiguous transport failures become the sanitized
//! `UPSTREAM_OUTCOME_UNKNOWN` error. The circuit counts the final invocation,
//! not individual attempts. Streaming has no timeout or retry, and, like Python,
//! checks `allow()` without recording stream success/failure on the circuit.
//!
//! The stream is the sole owner of its producer and lease. Explicit `close()`
//! waits for producer close followed by lease release; dropping it transfers
//! cleanup to a runtime task. Cancellation of a cleanup waiter does not cancel
//! cleanup. A running Tokio runtime must outlive cleanup; process/runtime shutdown
//! cannot guarantee completion. Adapter cancellation must stop its own I/O: this
//! core does not spawn or detach provider operations.
use crate::{
    capacity::{CapacityBudget, CapacityError, CapacityRequest, CapacityResolver},
    capacity_admission::{
        AdmissionCancellation, AdmissionError, AdmissionRequest, CapacityAdmissionController,
        CapacityLease,
    },
    circuit_breaker::{CallError, CircuitBreaker, CircuitSnapshot},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinHandle;

pub type DispatchFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, DispatchError>> + Send + 'a>>;

/// Classified by the adapter, never inferred from an error string. Private
/// upstream diagnostics/URLs/credentials do not belong in this public error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterFailure {
    ConnectError,
    RemoteProtocolError,
    PoolTimeout,
    ReadTimeout,
    WriteTimeout,
    ReadError,
    WriteError,
    HttpStatus(u16),
    OperationTimeout,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DispatchError {
    Adapter(AdapterFailure),
    Capacity(CapacityError),
    Admission(AdmissionError),
    Cancelled,
    CircuitOpen { service_id: String },
    UpstreamOutcomeUnknown,
    CleanupFailed,
    AdapterBulkheadExhausted { service_id: String },
    AdapterOperationUnsupported,
    AdapterValidationFailed(&'static str),
    /// Only sanitized client-facing details may be placed in this variant.
    AdapterValidationDetail(String),
}
impl fmt::Display for DispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Adapter(_) => formatter.write_str("upstream_dispatch_failed"),
            Self::AdapterValidationFailed(message) => formatter.write_str(message),
            Self::AdapterValidationDetail(message) => formatter.write_str(message),
            Self::Capacity(error) => error.fmt(formatter),
            Self::Admission(AdmissionError::Rejected(error)) => formatter.write_str(&error.code),
            Self::Admission(error) => error.fmt(formatter),
            Self::Cancelled => formatter.write_str("dispatch_cancelled"),
            Self::CircuitOpen { .. } => formatter.write_str("CIRCUIT_BREAKER_OPEN"),
            Self::UpstreamOutcomeUnknown => formatter.write_str("UPSTREAM_OUTCOME_UNKNOWN"),
            Self::CleanupFailed => formatter.write_str("dispatch_cleanup_failed"),
            Self::AdapterBulkheadExhausted { service_id } => {
                write!(formatter, "Adapter bulkhead exhausted for {service_id}")
            }
            Self::AdapterOperationUnsupported => {
                formatter.write_str("adapter_operation_not_implemented")
            }
        }
    }
}
impl std::error::Error for DispatchError {}
impl From<AdmissionError> for DispatchError {
    fn from(error: AdmissionError) -> Self {
        Self::Admission(error)
    }
}
impl From<crate::service_registry::AdapterError> for DispatchError {
    fn from(error: crate::service_registry::AdapterError) -> Self {
        match error {
            crate::service_registry::AdapterError::Dispatch(error) => error,
            crate::service_registry::AdapterError::BulkheadExhausted { service_id } => {
                Self::AdapterBulkheadExhausted { service_id }
            }
            crate::service_registry::AdapterError::UnsupportedOperation => {
                Self::AdapterOperationUnsupported
            }
        }
    }
}
impl From<CapacityError> for DispatchError {
    fn from(error: CapacityError) -> Self {
        Self::Capacity(error)
    }
}

pub fn is_safe_method(method: &str) -> bool {
    matches!(method.to_uppercase().as_str(), "GET" | "HEAD" | "OPTIONS")
}

/// `mapping.method or connector.method or "POST"`, with no whitespace trimming.
/// Inputs are the already type-checked method strings from connector config.
pub fn effective_method(mapping_method: Option<&str>, connector_method: Option<&str>) -> String {
    mapping_method
        .filter(|method| !method.is_empty())
        .or_else(|| connector_method.filter(|method| !method.is_empty()))
        .unwrap_or("POST")
        .to_uppercase()
}

pub fn outcome_is_unknown(failure: AdapterFailure, method: &str) -> bool {
    !is_safe_method(method)
        && matches!(
            failure,
            AdapterFailure::ReadTimeout
                | AdapterFailure::WriteTimeout
                | AdapterFailure::ReadError
                | AdapterFailure::WriteError
                | AdapterFailure::RemoteProtocolError
                | AdapterFailure::HttpStatus(500..=u16::MAX)
        )
}

/// Python names the field `max_retries`, but it is the total attempt budget.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchPolicy {
    method: String,
    max_attempts: usize,
    pub timeout: Duration,
}
impl DispatchPolicy {
    pub fn new(method: &str, max_retries: i64, timeout: Duration) -> Self {
        Self {
            method: method.to_uppercase(),
            max_attempts: max_retries.clamp(1, 3) as usize,
            timeout,
        }
    }
    pub fn method(&self) -> &str {
        &self.method
    }
    pub fn max_attempts(&self) -> usize {
        self.max_attempts
    }
    pub fn can_retry(&self, failure: AdapterFailure) -> bool {
        self.max_attempts > 1
            && is_safe_method(&self.method)
            && matches!(
                failure,
                AdapterFailure::ConnectError
                    | AdapterFailure::RemoteProtocolError
                    | AdapterFailure::PoolTimeout
            )
    }
    pub fn classify(&self, failure: AdapterFailure) -> DispatchError {
        if outcome_is_unknown(failure, &self.method)
            || (failure == AdapterFailure::OperationTimeout && !is_safe_method(&self.method))
        {
            DispatchError::UpstreamOutcomeUnknown
        } else {
            DispatchError::Adapter(failure)
        }
    }
    /// The shared RetryPolicy uses full jitter over exponential 50..500ms bounds.
    pub fn retry_delay_ceiling(retry_index: usize) -> Duration {
        let multiplier = 1_u64 << retry_index.saturating_sub(1).min(4);
        Duration::from_millis((50 * multiplier).min(500))
    }
}
impl Default for DispatchPolicy {
    fn default() -> Self {
        Self::new("POST", 3, Duration::from_secs(60))
    }
}

/// Identity must originate from authenticated request authority, not body fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DispatchContext {
    pub request_id: String,
    pub tenant_id: String,
    pub user_id: String,
}
impl DispatchContext {
    pub fn admission_tenant_id(&self) -> &str {
        if self.tenant_id.is_empty() {
            "default"
        } else {
            &self.tenant_id
        }
    }
    pub fn admission_user_id(&self) -> &str {
        if self.user_id.is_empty() {
            "anonymous"
        } else {
            &self.user_id
        }
    }
}

/// A selected service's execution fields, without provider credentials. The
/// capacity config is the already-resolved dictionary shape accepted by capacity.rs.
#[derive(Clone)]
pub struct DispatchService {
    pub service_id: String,
    pub policy: DispatchPolicy,
    pub circuit_breaker_enabled: bool,
    pub failure_threshold: i64,
    pub recovery_timeout: Duration,
    pub capacity_config: Value,
}
impl DispatchService {
    pub fn new(service_id: impl Into<String>) -> Self {
        Self {
            service_id: service_id.into(),
            policy: DispatchPolicy::default(),
            circuit_breaker_enabled: true,
            failure_threshold: 5,
            recovery_timeout: Duration::from_secs(30),
            capacity_config: Value::Null,
        }
    }
}

/// Consuming release transfers ownership into a cancellation-safe cleanup task.
/// Acquisition implementations must also clean up partial acquisitions if their
/// returned future is cancelled before handing the lease to the dispatcher.
pub trait DispatchLease: Send + Sync + 'static {
    fn bind_owner(&self, cancellation: AdmissionCancellation) -> Result<(), DispatchError>;
    fn wait_lost(&self) -> DispatchFuture<'_, ()>;
    fn release(self: Box<Self>) -> DispatchFuture<'static, ()>;
}
impl DispatchLease for CapacityLease {
    fn bind_owner(&self, cancellation: AdmissionCancellation) -> Result<(), DispatchError> {
        CapacityLease::bind_owner(self, cancellation).map_err(Into::into)
    }
    fn wait_lost(&self) -> DispatchFuture<'_, ()> {
        Box::pin(async move {
            CapacityLease::wait_lost(self).await;
            Err(self.lost_error().into())
        })
    }
    fn release(self: Box<Self>) -> DispatchFuture<'static, ()> {
        Box::pin(async move {
            CapacityLease::release(&self).await;
            Ok(())
        })
    }
}
pub trait DispatchAdmission: Send + Sync {
    fn acquire<'a>(
        &'a self,
        context: &'a DispatchContext,
        service_id: &'a str,
        request_class: &'a str,
        budgets: &'a [CapacityBudget],
    ) -> DispatchFuture<'a, Box<dyn DispatchLease>>;
}

/// Exact native admission adapter; all existing budget headers/errors survive.
impl DispatchAdmission for CapacityAdmissionController {
    fn acquire<'a>(
        &'a self,
        context: &'a DispatchContext,
        service_id: &'a str,
        request_class: &'a str,
        budgets: &'a [CapacityBudget],
    ) -> DispatchFuture<'a, Box<dyn DispatchLease>> {
        Box::pin(async move {
            let request = AdmissionRequest {
                tenant_id: context.admission_tenant_id().into(),
                user_id: context.admission_user_id().into(),
                service_id: service_id.into(),
                request_class: request_class.into(),
                request_id: context.request_id.clone(),
                ..AdmissionRequest::default()
            };
            let lease = CapacityAdmissionController::acquire(self, budgets, &request).await?;
            Ok(Box::new(lease) as Box<dyn DispatchLease>)
        })
    }
}

/// Injection is only for deterministic tests; production uses full-jitter sleep.
pub trait RetryDelay: Send + Sync {
    fn wait(&self, retry_index: usize) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}
struct JitteredDelay;
impl RetryDelay for JitteredDelay {
    fn wait(&self, retry_index: usize) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        // UUID v4 supplies random bits without introducing another RNG dependency.
        let random = uuid::Uuid::new_v4();
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(&random.as_bytes()[..8]);
        // Avoid fixed version bits by using the lower 48 random bits.
        let fraction = (u64::from_be_bytes(bytes) >> 16) as f64 / ((1_u64 << 48) - 1) as f64;
        let ceiling = DispatchPolicy::retry_delay_ceiling(retry_index);
        Box::pin(tokio::time::sleep(ceiling.mul_f64(fraction)))
    }
}

struct LeaseOwner {
    lease: Option<Box<dyn DispatchLease>>,
    runtime: tokio::runtime::Handle,
}
impl LeaseOwner {
    fn new(lease: Box<dyn DispatchLease>) -> Self {
        Self {
            lease: Some(lease),
            runtime: tokio::runtime::Handle::current(),
        }
    }
    fn inner(&self) -> &dyn DispatchLease {
        self.lease.as_deref().expect("live lease")
    }
    async fn release(mut self) -> Result<(), DispatchError> {
        let lease = self.lease.take().expect("lease released once");
        await_cleanup(self.runtime.spawn(async move { lease.release().await })).await
    }
}
impl Drop for LeaseOwner {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            // Ownership of the release survives cancellation of the caller.
            self.runtime.spawn(async move {
                let _ = lease.release().await;
            });
        }
    }
}
async fn await_cleanup(task: JoinHandle<Result<(), DispatchError>>) -> Result<(), DispatchError> {
    task.await.map_err(|_| DispatchError::CleanupFailed)?
}

pub struct DispatchExecutor {
    admission: Arc<dyn DispatchAdmission>,
    capacity: CapacityResolver,
    circuits: Mutex<HashMap<String, Arc<CircuitBreaker>>>,
    delay: Arc<dyn RetryDelay>,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
}
impl DispatchExecutor {
    pub fn new(admission: Arc<dyn DispatchAdmission>, capacity: CapacityResolver) -> Self {
        Self::with_runtime(
            admission,
            capacity,
            Arc::new(JitteredDelay),
            Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_secs_f64())
                    .unwrap_or_else(|error| -error.duration().as_secs_f64())
            }),
        )
    }
    pub fn with_runtime(
        admission: Arc<dyn DispatchAdmission>,
        capacity: CapacityResolver,
        delay: Arc<dyn RetryDelay>,
        clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    ) -> Self {
        Self {
            admission,
            capacity,
            circuits: Mutex::new(HashMap::new()),
            delay,
            clock,
        }
    }
    fn circuit(&self, service: &DispatchService) -> Arc<CircuitBreaker> {
        // Configuration is captured the first time the service is seen, as in Python.
        self.circuits
            .lock()
            .expect("circuit map poisoned")
            .entry(service.service_id.clone())
            .or_insert_with(|| {
                Arc::new(CircuitBreaker::with_clock(
                    &service.service_id,
                    service.failure_threshold,
                    3,
                    service.recovery_timeout.as_secs_f64(),
                    self.clock.clone(),
                ))
            })
            .clone()
    }
    pub async fn circuit_snapshot(&self, service_id: &str) -> Option<CircuitSnapshot> {
        let circuit = self
            .circuits
            .lock()
            .expect("circuit map poisoned")
            .get(service_id)
            .cloned();
        match circuit {
            Some(circuit) => Some(circuit.snapshot().await),
            None => None,
        }
    }
    async fn admit(
        &self,
        context: &DispatchContext,
        service: &DispatchService,
        request_class: &str,
    ) -> Result<LeaseOwner, DispatchError> {
        let budgets = self.capacity.resolve(&CapacityRequest {
            tenant_id: context.admission_tenant_id().into(),
            service_id: service.service_id.clone(),
            request_class: request_class.into(),
            upstream_group: Value::Null,
            provider_id: Value::Null,
            is_admin_read: false,
            service_config: service.capacity_config.clone(),
            resource_kind: "request".into(),
        })?;
        let lease = self
            .admission
            .acquire(context, &service.service_id, request_class, &budgets)
            .await?;
        Ok(LeaseOwner::new(lease))
    }
    /// Adapter futures remain on the caller's task and are dropped on cancellation.
    /// No provider operation is detached. All acquired leases still own cleanup.
    pub async fn invoke<T, F, Fut>(
        &self,
        context: &DispatchContext,
        service: &DispatchService,
        operation: F,
    ) -> Result<T, DispatchError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, AdapterFailure>>,
    {
        self.invoke_with_cancel(
            context,
            service,
            operation,
            AdmissionCancellation::default(),
        )
        .await
    }
    /// Cooperative cancellation waits for release before returning. Arbitrary
    /// future/task drop starts owned cleanup, but cannot await it in Drop.
    pub async fn invoke_with_cancel<T, F, Fut>(
        &self,
        context: &DispatchContext,
        service: &DispatchService,
        mut operation: F,
        cancellation: AdmissionCancellation,
    ) -> Result<T, DispatchError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, AdapterFailure>>,
    {
        self.invoke_errors_with_cancel(
            context,
            service,
            || {
                let work = operation();
                async move { work.await.map_err(DispatchError::Adapter) }
            },
            cancellation,
        )
        .await
    }
    /// Invoke an injected registry adapter without erasing bulkhead/unsupported
    /// errors. Only classified transport errors may enter the retry policy.
    pub async fn invoke_adapter(
        &self,
        context: &DispatchContext,
        service: &DispatchService,
        adapter: Arc<dyn crate::service_registry::ProtocolAdapter>,
        request: crate::protocol::UnifiedRequest,
    ) -> Result<crate::protocol::UnifiedResponse, DispatchError> {
        self.invoke_adapter_with_cancel(
            context,
            service,
            adapter,
            request,
            AdmissionCancellation::default(),
        )
        .await
    }
    pub async fn invoke_adapter_with_cancel(
        &self,
        context: &DispatchContext,
        service: &DispatchService,
        adapter: Arc<dyn crate::service_registry::ProtocolAdapter>,
        request: crate::protocol::UnifiedRequest,
        cancellation: AdmissionCancellation,
    ) -> Result<crate::protocol::UnifiedResponse, DispatchError> {
        let request = Arc::new(request);
        self.invoke_errors_with_cancel(
            context,
            service,
            move || {
                let request = request.clone();
                let adapter = adapter.clone();
                async move { adapter.invoke(&request).await.map_err(Into::into) }
            },
            cancellation,
        )
        .await
    }
    async fn invoke_errors_with_cancel<T, F, Fut>(
        &self,
        context: &DispatchContext,
        service: &DispatchService,
        mut operation: F,
        cancellation: AdmissionCancellation,
    ) -> Result<T, DispatchError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, DispatchError>>,
    {
        let circuit = self.circuit(service);
        let attempts = async {
            for attempt in 0..service.policy.max_attempts() {
                let lease = tokio::select! { biased;
                    _ = cancellation.cancelled() => return Err(DispatchError::Cancelled),
                    lease = self.admit(context, service, "sync") => lease?,
                };
                if let Err(error) = lease.inner().bind_owner(cancellation.clone()) {
                    lease.release().await?;
                    return Err(error);
                }
                let result = tokio::select! { biased;
                    loss = lease.inner().wait_lost() => Err(loss.err().unwrap_or(DispatchError::CleanupFailed)),
                    _ = cancellation.cancelled() => Err(DispatchError::Cancelled),
                    result = async {
                        // asyncio.wait_for(timeout <= 0) cancels a newly created
                        // coroutine before polling it. Tokio timeout(0) may poll
                        // ready work first, which could dispatch a mutation.
                        if service.policy.timeout.is_zero() {
                            return Err(service.policy.classify(AdapterFailure::OperationTimeout));
                        }
                        match tokio::time::timeout(service.policy.timeout, operation()).await {
                            Ok(result) => result.map_err(|error| match error {
                                DispatchError::Adapter(failure) => service.policy.classify(failure),
                                error => error,
                            }),
                            Err(_) => Err(service.policy.classify(AdapterFailure::OperationTimeout)),
                        }
                    } => result,
                };
                // Release is outside the adapter timeout and completes before retry.
                lease.release().await?;
                if cancellation.is_cancelled() {
                    return Err(DispatchError::Cancelled);
                }
                match result {
                    Ok(value) => return Ok(value),
                    Err(error) => {
                        let retryable = matches!(error, DispatchError::Adapter(failure) if service.policy.can_retry(failure));
                        if attempt + 1 >= service.policy.max_attempts() || !retryable {
                            return Err(error);
                        }
                        tokio::select! { biased;
                            _ = cancellation.cancelled() => return Err(DispatchError::Cancelled),
                            _ = self.delay.wait(attempt + 1) => {},
                        }
                    }
                }
            }
            unreachable!("attempt budget is at least one")
        };
        if service.circuit_breaker_enabled {
            circuit
                .call(
                    || attempts,
                    |error| matches!(error, DispatchError::Cancelled),
                )
                .await
                .map_err(|error| match error {
                    CallError::Open { service_id } => DispatchError::CircuitOpen { service_id },
                    CallError::Operation(error) => error,
                })
        } else {
            attempts.await
        }
    }
    /// The registry's stream-open future runs under the same owned capacity lease
    /// as its producer. Bulkhead queue errors retain identity and are never retried.
    pub async fn open_adapter_stream(
        &self,
        context: &DispatchContext,
        service: &DispatchService,
        adapter: Arc<dyn crate::service_registry::ProtocolAdapter>,
        request: crate::protocol::UnifiedRequest,
    ) -> Result<DispatchStream<crate::protocol::StreamChunk>, DispatchError> {
        self.open_adapter_stream_with_cancel(
            context,
            service,
            adapter,
            request,
            AdmissionCancellation::default(),
        )
        .await
    }
    pub async fn open_adapter_stream_with_cancel(
        &self,
        context: &DispatchContext,
        service: &DispatchService,
        adapter: Arc<dyn crate::service_registry::ProtocolAdapter>,
        request: crate::protocol::UnifiedRequest,
        cancellation: AdmissionCancellation,
    ) -> Result<DispatchStream<crate::protocol::StreamChunk>, DispatchError> {
        self.check_stream_circuit(service).await?;
        self.open_adapter_stream_after_circuit(context, service, adapter, request, cancellation)
            .await
    }
    /// Python stream checks its circuit before persisting the user message.
    /// The composing dispatcher calls this once before that side effect.
    pub async fn check_stream_circuit(
        &self,
        service: &DispatchService,
    ) -> Result<(), DispatchError> {
        let circuit = self.circuit(service);
        if service.circuit_breaker_enabled && !circuit.allow().await {
            return Err(DispatchError::CircuitOpen {
                service_id: service.service_id.clone(),
            });
        }
        Ok(())
    }
    /// Crate-private continuation for the composing gateway dispatcher after its
    /// required circuit check. Do not expose this as an authorization/admission bypass.
    pub(crate) async fn open_adapter_stream_after_circuit(
        &self,
        context: &DispatchContext,
        service: &DispatchService,
        adapter: Arc<dyn crate::service_registry::ProtocolAdapter>,
        request: crate::protocol::UnifiedRequest,
        cancellation: AdmissionCancellation,
    ) -> Result<DispatchStream<crate::protocol::StreamChunk>, DispatchError> {
        let lease = tokio::select! { biased;
            _ = cancellation.cancelled() => return Err(DispatchError::Cancelled),
            lease = self.admit(context, service, "stream") => lease?,
        };
        if let Err(error) = lease.inner().bind_owner(cancellation.clone()) {
            lease.release().await?;
            return Err(error);
        }
        let producer = Box::new(LazyAdapterStream::new(adapter, request))
            as Box<dyn DispatchProducer<crate::protocol::StreamChunk>>;
        Ok(DispatchStream {
            producer: Some(producer),
            runtime: lease.runtime.clone(),
            lease: Some(lease),
            cleanup: None,
            cleanup_error: None,
            terminal_error: None,
            status: StreamStatus::InProgress,
            defer_success_cleanup: false,
            producer_ended: false,
            cancellation,
        })
    }
    /// No retry or per-chunk timeout, including before the first chunk. The factory
    /// runs only after the circuit allows and capacity has been acquired.
    pub async fn open_stream<T, F>(
        &self,
        context: &DispatchContext,
        service: &DispatchService,
        producer: F,
    ) -> Result<DispatchStream<T>, DispatchError>
    where
        T: Send + 'static,
        F: FnOnce() -> Box<dyn DispatchProducer<T>>,
    {
        self.check_stream_circuit(service).await?;
        let lease = self.admit(context, service, "stream").await?;
        let cancellation = AdmissionCancellation::default();
        if let Err(error) = lease.inner().bind_owner(cancellation.clone()) {
            lease.release().await?;
            return Err(error);
        }
        Ok(DispatchStream {
            producer: Some(producer()),
            runtime: lease.runtime.clone(),
            lease: Some(lease),
            cleanup: None,
            cleanup_error: None,
            terminal_error: None,
            status: StreamStatus::InProgress,
            defer_success_cleanup: false,
            producer_ended: false,
            cancellation,
        })
    }
}

/// Python adapter.stream() is a lazy generator. Retain its opening future across
/// cancelled next() polls so a subsequent poll cannot submit the request again.
struct LazyAdapterStream {
    opening: Option<
        crate::service_registry::AdapterFuture<
            'static,
            Box<dyn DispatchProducer<crate::protocol::StreamChunk>>,
        >,
    >,
    producer: Option<Box<dyn DispatchProducer<crate::protocol::StreamChunk>>>,
    ended: bool,
}
impl LazyAdapterStream {
    fn new(
        adapter: Arc<dyn crate::service_registry::ProtocolAdapter>,
        request: crate::protocol::UnifiedRequest,
    ) -> Self {
        Self {
            opening: Some(Box::pin(async move { adapter.stream(request).await })),
            producer: None,
            ended: false,
        }
    }
}
impl DispatchProducer<crate::protocol::StreamChunk> for LazyAdapterStream {
    fn next(&mut self) -> DispatchFuture<'_, Option<crate::protocol::StreamChunk>> {
        Box::pin(async move {
            if self.ended {
                return Ok(None);
            }
            if let Some(opening) = self.opening.as_mut() {
                let result = opening.await;
                self.opening = None;
                match result {
                    Ok(producer) => self.producer = Some(producer),
                    Err(error) => {
                        self.ended = true;
                        return Err(error.into());
                    }
                }
            }
            let result = self
                .producer
                .as_mut()
                .expect("opened stream producer")
                .next()
                .await;
            if !matches!(result, Ok(Some(_))) {
                self.ended = true;
            }
            result
        })
    }
    fn close(mut self: Box<Self>) -> DispatchFuture<'static, ()> {
        Box::pin(async move {
            // Dropping a pending open future cancels its owned work/bulkhead wait.
            drop(self.opening.take());
            match self.producer.take() {
                Some(producer) => producer.close().await,
                None => Ok(()),
            }
        })
    }
}

pub trait DispatchProducer<T>: Send + 'static {
    /// Cancelling this future must stop/pause the owned I/O, not detach a producer.
    fn next(&mut self) -> DispatchFuture<'_, Option<T>>;
    fn close(self: Box<Self>) -> DispatchFuture<'static, ()>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamStatus {
    InProgress,
    Success,
    Error,
    Unknown,
}
impl StreamStatus {
    pub fn usage_status(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::InProgress | Self::Unknown => "unknown",
        }
    }
}

/// Pull-based and unbuffered: the caller cannot accidentally detach production.
/// Dropping a pending next() alone cancels that poll; dropping/closing this owner
/// ends the stream and closes the producer before releasing the lease.
pub struct DispatchStream<T: Send + 'static> {
    producer: Option<Box<dyn DispatchProducer<T>>>,
    lease: Option<LeaseOwner>,
    cleanup: Option<JoinHandle<Result<(), DispatchError>>>,
    // A composed stream closes again during its finally block. Retain failure
    // after consuming the join handle so that cleanup still suppresses accounting.
    cleanup_error: Option<DispatchError>,
    // Retain the producer result while joined cleanup yields. Cancelling that
    // poll must not turn the next poll into a successful EOF.
    terminal_error: Option<DispatchError>,
    status: StreamStatus,
    defer_success_cleanup: bool,
    producer_ended: bool,
    cancellation: AdmissionCancellation,
    runtime: tokio::runtime::Handle,
}
impl<T: Send + 'static> DispatchStream<T> {
    /// Outer dispatcher must keep capacity through its final stream_end yield.
    /// Only success EOF is deferred; errors and explicit close still clean up.
    pub(crate) fn defer_success_cleanup_until_close(&mut self) {
        self.defer_success_cleanup = true;
    }
    pub fn status(&self) -> StreamStatus {
        self.status
    }
    /// Also signalled on lease loss; consumers waiting between next() calls should
    /// watch this token and call close() to join ordered producer/lease cleanup.
    pub fn cancellation(&self) -> AdmissionCancellation {
        self.cancellation.clone()
    }
    fn begin_cleanup(&mut self) {
        if let Some(producer) = self.producer.take() {
            let lease = self.lease.take().expect("stream producer owns lease");
            self.cleanup = Some(self.runtime.spawn(async move {
                let closed = producer.close().await;
                // Even a producer-close error must release capacity first.
                lease.release().await?;
                closed
            }));
        }
    }
    async fn finish_cleanup(&mut self) -> Result<(), DispatchError> {
        if let Some(error) = &self.cleanup_error {
            return Err(error.clone());
        }
        self.begin_cleanup();
        let result = match self.cleanup.as_mut() {
            Some(cleanup) => cleanup.await.unwrap_or(Err(DispatchError::CleanupFailed)),
            None => return Ok(()),
        };
        self.cleanup = None;
        self.cleanup_error = result.as_ref().err().cloned();
        result
    }
    pub async fn next(&mut self) -> Result<Option<T>, DispatchError> {
        if self.terminal_error.is_some() {
            self.finish_cleanup().await?;
            return Err(self.terminal_error.take().expect("pending terminal error"));
        }
        if self.producer_ended {
            if !self.defer_success_cleanup {
                self.finish_cleanup().await?;
            }
            return Ok(None);
        }
        let result = match self.producer.as_mut() {
            Some(producer) => {
                let lease = self.lease.as_ref().expect("stream producer owns lease");
                tokio::select! { biased;
                    loss = lease.inner().wait_lost() => Err(loss.err().unwrap_or(DispatchError::CleanupFailed)),
                    _ = self.cancellation.cancelled() => Err(DispatchError::Cancelled),
                    chunk = producer.next() => chunk,
                }
            }
            None => {
                self.finish_cleanup().await?;
                return Ok(None);
            }
        };
        match result {
            Ok(Some(chunk)) => Ok(Some(chunk)),
            Ok(None) => {
                self.status = StreamStatus::Success;
                self.producer_ended = true;
                if !self.defer_success_cleanup {
                    self.finish_cleanup().await?;
                }
                Ok(None)
            }
            Err(error) => {
                self.terminal_error = Some(error.clone());
                self.status = if matches!(
                    &error,
                    DispatchError::UpstreamOutcomeUnknown | DispatchError::Cancelled
                ) || self.cancellation.is_cancelled()
                    || matches!(&error, DispatchError::Adapter(failure) if outcome_is_unknown(*failure, "POST"))
                {
                    StreamStatus::Unknown
                } else {
                    StreamStatus::Error
                };
                self.finish_cleanup().await?;
                // No yield after consumption: only a delivered error exhausts it.
                self.terminal_error = None;
                Err(error)
            }
        }
    }
    pub async fn close(&mut self) -> Result<(), DispatchError> {
        if self.status == StreamStatus::InProgress {
            self.status = StreamStatus::Unknown;
        }
        self.finish_cleanup().await
    }
}
impl<T: Send + 'static> Drop for DispatchStream<T> {
    fn drop(&mut self) {
        // JoinHandle::drop detaches cleanup, never the provider's next() future.
        self.begin_cleanup();
    }
}

