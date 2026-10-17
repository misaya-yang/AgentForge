//! Capability route budgets are admitted only after token, proof and actor checks.
use ai_platform_capability_contract::private_transport::{Budget, Edge, Trace};
use axum::http::HeaderMap;
use std::{
    future::Future,
    time::{Duration, Instant},
};
#[derive(Clone)]
pub struct AdmittedTransport {
    pub budget: Budget,
    pub trace: Trace,
}
tokio::task_local! {static CURRENT:AdmittedTransport;}
impl AdmittedTransport {
    pub(super) fn after_authenticated(
        headers: &HeaderMap,
        proof_expiry_ms: u64,
    ) -> Result<Self, ()> {
        let wall = chrono::Utc::now().timestamp_millis().max(0) as u64;
        let local = Budget::admit(
            Duration::from_secs(30),
            proof_expiry_ms,
            wall,
            Instant::now(),
        )
        .map_err(|_| ())?;
        let pairs = headers
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_str().unwrap_or("").to_owned()))
            .collect::<Vec<_>>();
        Ok(Self {
            budget: local
                .constrain_authenticated(&pairs, wall, Instant::now())
                .map_err(|_| ())?,
            trace: Trace::from_admitted_carrier(&pairs),
        })
    }
    pub(super) async fn run<F: Future>(self, future: F) -> Result<F::Output, ()> {
        let timeout = Edge::WorkerKnowledge
            .policy()
            .total_timeout(&self.budget, Instant::now())
            .map_err(|_| ())?;
        CURRENT
            .scope(self, tokio::time::timeout(timeout, future))
            .await
            .map_err(|_| ())
    }
}
/// Only installed by this authenticated capability ingress, never public headers.
pub fn current() -> Option<AdmittedTransport> {
    CURRENT.try_with(Clone::clone).ok()
}
