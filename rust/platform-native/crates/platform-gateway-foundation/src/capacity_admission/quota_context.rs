//! Non-serializable dispatch-owned proof that the current logical request has
//! passed quota admission, consuming RPM only when limited. Never carried
//! in user JSON, request headers, task payloads or cross-request mutable caches.
use crate::billing::MinuteQuotaCheck;
use std::future::Future;
#[derive(Clone)]
pub struct AdmittedQuota {
    tenant: String,
    user: String,
    decision: MinuteQuotaCheck,
}
impl AdmittedQuota {
    pub(super) fn new(tenant: String, user: String, rpm_used: Option<i64>) -> Self {
        let decision = match rpm_used {
            Some(used) => MinuteQuotaCheck {
                allowed: true,
                used,
                retry_after_seconds: 0,
            },
            None => MinuteQuotaCheck::default(),
        };
        Self {
            tenant,
            user,
            decision,
        }
    }
}
tokio::task_local! {static CURRENT: AdmittedQuota;}
pub async fn scope<F: Future>(proof: Option<AdmittedQuota>, future: F) -> F::Output {
    match proof {
        Some(proof) => CURRENT.scope(proof, future).await,
        None => future.await,
    }
}
pub fn decision(tenant: &str, user: &str) -> Option<MinuteQuotaCheck> {
    CURRENT
        .try_with(|proof| (proof.tenant == tenant && proof.user == user).then_some(proof.decision))
        .ok()
        .flatten()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn proof_is_scoped_to_future_and_exact_authority() {
        assert!(decision("t", "u").is_none());
        let proof = AdmittedQuota::new("t".into(), "u".into(), Some(7));
        scope(Some(proof), async {
            assert_eq!(decision("t", "u").unwrap().used, 7);
            assert!(decision("foreign", "u").is_none());
            assert!(decision("t", "foreign").is_none());
            assert!(
                tokio::spawn(async { decision("t", "u") })
                    .await
                    .unwrap()
                    .is_none()
            );
        })
        .await;
        assert!(decision("t", "u").is_none());
    }

    #[tokio::test]
    async fn unlimited_rpm_retains_a_successful_authority_bound_proof() {
        for configured_limit in [None, Some(0), Some(-1)] {
            assert!(crate::billing::normalize_limit(configured_limit).is_none());
            let proof = AdmittedQuota::new("tenant".into(), "user".into(), None);
            scope(Some(proof), async {
                assert_eq!(
                    decision("tenant", "user"),
                    Some(MinuteQuotaCheck::default())
                );
                assert!(decision("other-tenant", "user").is_none());
                assert!(decision("tenant", "other-user").is_none());
            })
            .await;
            assert!(decision("tenant", "user").is_none());
        }
    }
}
