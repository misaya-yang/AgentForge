//! Only installed after verified lease admission; never a public authority source.
use super::*;
use ai_platform_capability_contract::private_transport::{Budget, Edge, Trace};
use std::{future::Future, time::Instant};
pub(crate) const EXPIRY_KEY: &str = "private_transport_verified_lease_expires_ms";
tokio::task_local! {static CURRENT:Budget;}
pub(crate) fn ingress(
    headers: &axum::http::HeaderMap,
    expiry: u64,
    cap_ms: u64,
) -> Result<Budget, ReadCapabilityError> {
    let wall = wall_ms()?;
    let parent = Budget::admit(
        Duration::from_millis(cap_ms.clamp(1, 30_000)),
        expiry,
        wall,
        Instant::now(),
    )
    .map_err(|_| ReadCapabilityError::Scope)?;
    let pairs = headers
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_str().unwrap_or("").to_owned()))
        .collect::<Vec<_>>();
    parent
        .constrain_authenticated(&pairs, wall, Instant::now())
        .map_err(|_| ReadCapabilityError::Scope)
}
pub(crate) async fn scope<F: Future>(budget: Budget, future: F) -> F::Output {
    CURRENT.scope(budget, future).await
}
pub(crate) async fn verified_record<F: Future<Output = Result<Value, ReadCapabilityError>>>(
    binding: &Value,
    cap_ms: u64,
    future: F,
) -> Result<Value, ReadCapabilityError> {
    // Legacy durable rows cannot reconstruct their historical lease from HTTP.
    let expiry = binding[EXPIRY_KEY]
        .as_u64()
        .ok_or(ReadCapabilityError::Scope)?;
    let wall = wall_ms()?;
    let mut budget = Budget::admit(
        Duration::from_millis(cap_ms.clamp(1, 30_000)),
        expiry,
        wall,
        Instant::now(),
    )
    .map_err(|_| ReadCapabilityError::Scope)?;
    if let Ok(parent) = CURRENT.try_with(Clone::clone) {
        let mut headers = Vec::new();
        parent
            .append(&mut headers, wall, Instant::now())
            .map_err(|_| ReadCapabilityError::Scope)?;
        budget = budget
            .constrain_authenticated(&headers, wall, Instant::now())
            .map_err(|_| ReadCapabilityError::Scope)?;
    }
    scope(budget, future).await
}
pub(super) fn outbound(
    headers: &[(String, String)],
) -> Result<(Budget, Vec<(String, String)>), ReadCapabilityError> {
    let parent = CURRENT
        .try_with(Clone::clone)
        .map_err(|_| ReadCapabilityError::Scope)?;
    let child = parent
        .child(Edge::WorkerKnowledge.policy(), Instant::now())
        .map_err(|_| ReadCapabilityError::DownstreamUnavailable)?;
    let mut pairs = headers.to_vec();
    Trace::from_admitted_carrier(headers).append(&mut pairs);
    child
        .append(&mut pairs, wall_ms()?, Instant::now())
        .map_err(|_| ReadCapabilityError::DownstreamUnavailable)?;
    Ok((child, pairs))
}
fn wall_ms() -> Result<u64, ReadCapabilityError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|v| u64::try_from(v.as_millis()).ok())
        .ok_or(ReadCapabilityError::Configuration)
}
/// Transport admission metadata is never part of the business idempotency key.
/// Store replay keeps the ORIGINAL binding unchanged, so a replacement HTTP
/// lease cannot refresh an old durable row's authority or populate a legacy row.
pub(crate) fn bindings_match(existing: &Value, requested: &Value) -> bool {
    let mut existing = existing.clone();
    let mut requested = requested.clone();
    if let Some(object) = existing.as_object_mut() {
        object.remove(EXPIRY_KEY);
    }
    if let Some(object) = requested.as_object_mut() {
        object.remove(EXPIRY_KEY);
    }
    existing == requested
}
pub(super) fn proof_expiry_seconds() -> Result<Option<u64>, ReadCapabilityError> {
    let Ok(budget) = CURRENT.try_with(Clone::clone) else {
        return Ok(None);
    };
    let mut headers = Vec::new();
    budget
        .append(&mut headers, wall_ms()?, Instant::now())
        .map_err(|_| ReadCapabilityError::Scope)?;
    headers
        .iter()
        .find(|(key, _)| key == ai_platform_capability_contract::private_transport::DEADLINE_HEADER)
        .and_then(|(_, value)| value.parse::<u64>().ok())
        .map(|value| Some(value / 1000))
        .ok_or(ReadCapabilityError::Scope)
}
