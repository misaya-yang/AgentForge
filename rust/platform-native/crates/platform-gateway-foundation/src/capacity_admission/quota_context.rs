//! Non-serializable dispatch-owned proof that the current logical request has
//! already consumed quota RPM in the combined admission script. Never carried
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
    pub(super) fn new(tenant: String, user: String, decision: MinuteQuotaCheck) -> Self {
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
        let proof = AdmittedQuota::new(
            "t".into(),
            "u".into(),
            MinuteQuotaCheck {
                allowed: true,
                used: 7,
                retry_after_seconds: 0,
            },
        );
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
}
