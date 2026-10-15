//! Canonical VectorStore._call timing and failure policy, independent of transport.
//! A shared deadline limits interactive reads only; it never returns stale data.
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::{Instant, sleep, timeout};

tokio::task_local! {
    static INTERACTIVE_DEADLINE: Instant;
    static INTERACTIVE_CALLS: bool;
}

#[derive(Clone, Debug)]
pub(super) struct Policy {
    pub max_retries: usize,
    pub retry_base_delay: Duration,
    pub interactive_max_retries: usize,
    pub interactive_deadline: Duration,
    pub health_receipt_ttl: Duration,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            retry_base_delay: Duration::from_millis(500),
            interactive_max_retries: 2,
            interactive_deadline: Duration::from_secs(3),
            health_receipt_ttl: Duration::from_secs(2),
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Failure<E> {
    Transient(E),
    Permanent(E),
    Timeout,
    Deadline,
    CircuitOpen,
}
#[derive(Default)]
struct Health {
    success_until: Option<Instant>,
    failure_until: Option<Instant>,
}
#[derive(Clone, Default)]
pub(super) struct Resilience {
    health: Arc<Mutex<Health>>,
}
pub(super) fn interactive_calls() -> bool {
    INTERACTIVE_CALLS.try_with(|v| *v).unwrap_or(false)
}
pub(super) async fn with_interactive_calls<F: Future>(future: F) -> F::Output {
    INTERACTIVE_CALLS.scope(true, future).await
}
pub(super) fn current_deadline() -> Option<Instant> {
    INTERACTIVE_DEADLINE.try_with(|v| *v).ok()
}
pub(super) async fn with_budget<F: Future>(duration: Duration, future: F) -> F::Output {
    let local = Instant::now() + duration.max(Duration::from_millis(100));
    let deadline = current_deadline().map_or(local, |inherited| inherited.min(local));
    INTERACTIVE_DEADLINE.scope(deadline, future).await
}
impl Resilience {
    fn record(&self, policy: &Policy, success: bool) {
        let mut health = self
            .health
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let until = Instant::now() + policy.health_receipt_ttl;
        if success {
            health.success_until = Some(until);
            health.failure_until = None;
        } else {
            health.failure_until = Some(until);
            health.success_until = None;
        }
    }
    pub(super) async fn execute<T, E, F, Fut>(
        &self,
        policy: &Policy,
        request_timeout: Duration,
        interactive: bool,
        mut factory: F,
    ) -> Result<T, Failure<E>>
    where
        F: FnMut(Duration) -> Fut,
        Fut: Future<Output = Result<T, Failure<E>>>,
    {
        let mut attempts = policy.max_retries.max(1);
        let deadline = if interactive {
            let now = Instant::now();
            if self
                .health
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .failure_until
                .is_some_and(|until| until > now)
            {
                return Err(Failure::CircuitOpen);
            }
            attempts = attempts.min(policy.interactive_max_retries.clamp(1, 2));
            let local = now + policy.interactive_deadline.max(Duration::from_millis(100));
            Some(current_deadline().map_or(local, |inherited| inherited.min(local)))
        } else {
            None
        };
        let mut last_was_transient = false;
        for attempt in 0..attempts {
            let mut timeout_duration = request_timeout;
            if let Some(deadline) = deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    if last_was_transient {
                        self.record(policy, false);
                    }
                    return Err(Failure::Deadline);
                }
                timeout_duration = timeout_duration.min(remaining);
            }
            let outcome = timeout(timeout_duration, factory(timeout_duration)).await;
            let failure = match outcome {
                Ok(Ok(value)) => {
                    if interactive {
                        self.record(policy, true);
                    }
                    return Ok(value);
                }
                Ok(Err(error)) => error,
                Err(_) => Failure::Timeout,
            };
            let transient = matches!(&failure, Failure::Transient(_) | Failure::Timeout);
            if attempt + 1 >= attempts || !transient {
                if interactive && transient {
                    self.record(policy, false);
                }
                return Err(failure);
            }
            last_was_transient = transient;
            let factor = 1u32
                .checked_shl(attempt.try_into().unwrap_or(u32::MAX))
                .unwrap_or(u32::MAX);
            let mut delay = policy.retry_base_delay.saturating_mul(factor);
            if let Some(deadline) = deadline {
                delay = delay.min(deadline.saturating_duration_since(Instant::now()));
                if delay.is_zero() {
                    continue;
                }
            }
            sleep(delay).await;
        }
        unreachable!("at least one bounded vector-store attempt is required")
    }
}
