//! Port of src/core/gateway/circuit_breaker.py; no automatic retries or single-probe gate.
//! Python exception-class exclusions are supplied by the caller as a classification predicate.
use std::{
    future::Future,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}
#[derive(Clone, Debug, PartialEq)]
pub struct CircuitSnapshot {
    pub state: CircuitState,
    pub failure_count: u64,
    pub success_count: u64,
    pub last_failure_time: Option<f64>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum CallError<E> {
    Open { service_id: String },
    Operation(E),
}
/// State belongs to one service. Authentication and tenant-scoped service lookup belong upstream.
pub struct CircuitBreaker {
    service_id: String,
    failure_threshold: i64,
    success_threshold: i64,
    timeout: f64,
    clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    state: Mutex<CircuitSnapshot>,
}
impl CircuitBreaker {
    pub fn new(service_id: impl Into<String>) -> Self {
        Self::with_clock(
            service_id,
            5,
            3,
            30.0,
            Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs_f64())
                    .unwrap_or_else(|e| -e.duration().as_secs_f64())
            }),
        )
    }
    /// Wall-clock semantics match Python time.time(), including backward clock adjustments.
    pub fn with_clock(
        service_id: impl Into<String>,
        failure_threshold: i64,
        success_threshold: i64,
        timeout: f64,
        clock: Arc<dyn Fn() -> f64 + Send + Sync>,
    ) -> Self {
        Self {
            service_id: service_id.into(),
            failure_threshold,
            success_threshold,
            timeout,
            clock,
            state: Mutex::new(CircuitSnapshot {
                state: CircuitState::Closed,
                failure_count: 0,
                success_count: 0,
                last_failure_time: None,
            }),
        }
    }
    pub async fn snapshot(&self) -> CircuitSnapshot {
        self.state.lock().await.clone()
    }
    pub async fn allow(&self) -> bool {
        let mut state = self.state.lock().await;
        if state.state == CircuitState::Open {
            if state
                .last_failure_time
                .is_some_and(|last| (self.clock)() - last >= self.timeout)
            {
                state.state = CircuitState::HalfOpen;
            } else {
                return false;
            }
        }
        true
    }
    pub async fn on_success(&self) {
        let mut state = self.state.lock().await;
        match state.state {
            CircuitState::HalfOpen => {
                state.success_count = state.success_count.saturating_add(1);
                if self.success_threshold <= 0
                    || state.success_count >= self.success_threshold as u64
                {
                    state.state = CircuitState::Closed;
                    state.failure_count = 0;
                    state.success_count = 0;
                }
            }
            CircuitState::Closed => state.failure_count = 0,
            CircuitState::Open => {}
        }
    }
    pub async fn on_failure(&self, excluded: bool) {
        if excluded {
            return;
        }
        let mut state = self.state.lock().await;
        state.failure_count = state.failure_count.saturating_add(1);
        state.last_failure_time = Some((self.clock)());
        if self.failure_threshold <= 0 || state.failure_count >= self.failure_threshold as u64 {
            state.state = CircuitState::Open;
            state.success_count = 0;
        }
    }
    /// The factory is not invoked when OPEN. Cancellation drops the future without counting
    /// failure, matching asyncio.CancelledError's exclusion from Python's except Exception.
    pub async fn call<T, E, F, Fut, P>(&self, operation: F, excluded: P) -> Result<T, CallError<E>>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, E>>,
        P: FnOnce(&E) -> bool,
    {
        if !self.allow().await {
            return Err(CallError::Open {
                service_id: self.service_id.clone(),
            });
        }
        match operation().await {
            Ok(value) => {
                self.on_success().await;
                Ok(value)
            }
            Err(error) => {
                self.on_failure(excluded(&error)).await;
                Err(CallError::Operation(error))
            }
        }
    }
}
