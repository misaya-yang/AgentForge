//! Private transport attenuation, called only after native authority admission.
use ai_platform_capability_contract::private_transport::Budget;
use axum::http::HeaderMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
pub(crate) const ADMISSION_CAP: Duration = Duration::from_secs(300);
pub(crate) fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
/// A supplied expiry must come from an already scope/lease/owner-validated DB
/// row, never from body or transport headers. None explicitly means route-only.
pub(crate) fn local(cap: Duration, verified_expiry: Option<u64>) -> Result<Budget, String> {
    let wall = wall_ms();
    Budget::admit(
        cap,
        verified_expiry
            .unwrap_or_else(|| wall.saturating_add(cap.as_millis().min(u64::MAX as u128) as u64)),
        wall,
        Instant::now(),
    )
    .map_err(|_| "runtime_transport_budget_expired".into())
}
pub(crate) fn admit_authenticated(
    headers: &HeaderMap,
    cap: Duration,
    verified_expiry: Option<u64>,
) -> Result<Budget, String> {
    let local = local(cap, verified_expiry)?;
    let mut wire = vec![];
    for name in [
        ai_platform_capability_contract::private_transport::DEADLINE_HEADER,
        ai_platform_capability_contract::private_transport::BUDGET_HEADER,
        ai_platform_capability_contract::private_transport::HOPS_HEADER,
    ] {
        for value in headers.get_all(name).iter() {
            wire.push((
                name.to_owned(),
                value
                    .to_str()
                    .map_err(|_| "runtime_transport_budget_invalid")?
                    .to_owned(),
            ));
        }
    }
    local
        .constrain_authenticated(&wire, wall_ms(), Instant::now())
        .map_err(|_| "runtime_transport_budget_invalid".into())
}
pub(crate) fn intersect(local: &Budget, parent: &Budget) -> Result<Budget, String> {
    let now = Instant::now();
    let wall = wall_ms();
    let mut wire = vec![];
    parent
        .append(&mut wire, wall, now)
        .map_err(|_| "runtime_transport_budget_expired")?;
    local
        .constrain_authenticated(&wire, wall, now)
        .map_err(|_| "runtime_transport_budget_invalid".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn headers_only_shorten_verified_authority() {
        let wall = wall_ms();
        let mut h = HeaderMap::new();
        h.insert(
            "x-ai-deadline-unix-ms",
            u64::MAX.to_string().parse().unwrap(),
        );
        h.insert("x-ai-budget-ms", "3600000".parse().unwrap());
        h.insert("x-ai-hop-limit", "8".parse().unwrap());
        let b = admit_authenticated(&h, ADMISSION_CAP, Some(wall + 1000)).unwrap();
        assert!(b.remaining(Instant::now()).unwrap() <= Duration::from_secs(1));
    }
    #[test]
    fn malformed_duplicates_and_expired_verified_lease_are_rejected() {
        let mut h = HeaderMap::new();
        h.append("x-ai-budget-ms", "1000".parse().unwrap());
        h.append("x-ai-budget-ms", "1000".parse().unwrap());
        assert!(admit_authenticated(&h, ADMISSION_CAP, None).is_err());
        assert!(admit_authenticated(&HeaderMap::new(), ADMISSION_CAP, Some(1)).is_err());
    }
}
