//! KS dataset facts are current object authority. Gateway runs the Cedar PDP;
//! no Gateway dependency, raw public identity input, local evaluator, or retry.
use ai_platform_service_primitives::{
    knowledge_auth::GatewayAuth, knowledge_types::UserContext, object_authorization::*,
};
use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
pub type PolicyFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ()>> + Send + 'a>>;
pub trait DatasetObjectPolicy: Send + Sync {
    fn ranks<'a>(
        &'a self,
        actor: &'a UserContext,
        checks: Vec<DatasetRankCheck>,
    ) -> PolicyFuture<'a, Vec<i32>>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rollout {
    Shadow,
    Enforce,
}
/// Explicit operator input. Absence keeps the existing ACL path. Both process
/// configurations must agree before enabling enforcement; no inferred rollout.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub origin: String,
    pub mode: Rollout,
    #[serde(default)]
    pub expected_policy_sha256: String,
    #[serde(default)]
    pub expected_matrix_sha256: String,
}
impl Configuration {
    pub fn build(self, signer: Arc<GatewayAuth>) -> Result<RemoteDatasetPolicy, ()> {
        RemoteDatasetPolicy::new(
            self.origin.parse().map_err(|_| ())?,
            signer,
            self.mode,
            self.expected_policy_sha256,
            self.expected_matrix_sha256,
        )
    }
}
pub struct RemoteDatasetPolicy {
    client: reqwest::Client,
    origin: reqwest::Url,
    signer: Arc<GatewayAuth>,
    mode: Rollout,
    policy_sha256: String,
    matrix_sha256: String,
    legacy_transport: Option<axum::http::HeaderValue>,
    workload_http:
        Option<Arc<ai_platform_service_primitives::workload_identity::http::WorkloadHttp>>,
}
impl RemoteDatasetPolicy {
    pub fn new(
        origin: reqwest::Url,
        signer: Arc<GatewayAuth>,
        mode: Rollout,
        policy_sha256: String,
        matrix_sha256: String,
    ) -> Result<Self, ()> {
        if signer.version != "v2"
            || signer.caller_service != "knowledge-service"
            || signer.audience != "gateway"
            || signer.allow_anonymous
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
            || origin.scheme() != "https"
                && !(origin.scheme() == "http"
                    && origin
                        .host_str()
                        .and_then(|h| h.parse::<std::net::IpAddr>().ok())
                        .is_some_and(|ip| ip.is_loopback()))
        {
            return Err(());
        }
        if mode == Rollout::Enforce
            && [&policy_sha256, &matrix_sha256]
                .iter()
                .any(|v| v.len() != 64 || !v.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(());
        }
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| ())?;
        Ok(Self {
            client,
            origin,
            signer,
            mode,
            policy_sha256,
            matrix_sha256,
            legacy_transport: None,
            workload_http: None,
        })
    }
    pub fn with_legacy_transport(mut self, token: &str) -> Result<Self, ()> {
        if token.len() < 32
            || token
                .bytes()
                .any(|b| !b.is_ascii() || b.is_ascii_whitespace() || b.is_ascii_control())
        {
            return Err(());
        }
        self.legacy_transport = Some(format!("Bearer {token}").parse().map_err(|_| ())?);
        Ok(self)
    }
    pub fn with_workload_http(
        mut self,
        http: Arc<ai_platform_service_primitives::workload_identity::http::WorkloadHttp>,
    ) -> Result<Self, ()> {
        http.validate_endpoint(self.origin.as_str())
            .map_err(|_| ())?;
        self.workload_http = Some(http);
        Ok(self)
    }
    async fn request(
        &self,
        actor: &UserContext,
        checks: &[DatasetRankCheck],
    ) -> Result<Vec<i32>, RemoteError> {
        self.exchange(actor, checks)
            .await
            .map_err(RemoteError::Unavailable)
            .and_then(|receipts| {
                validate_receipts(
                    self.mode,
                    &self.policy_sha256,
                    &self.matrix_sha256,
                    checks,
                    &receipts,
                )
            })
    }
    async fn exchange(
        &self,
        actor: &UserContext,
        checks: &[DatasetRankCheck],
    ) -> Result<Vec<DatasetRankReceipt>, ()> {
        if checks.len() > 200 {
            return Err(());
        }
        let subject = ObjectSubject {
            tenant_id: actor.tenant_id.clone(),
            user_id: actor.user_id.clone(),
            roles: actor.roles.clone(),
            tier: actor.user_tier.clone(),
            anonymous: actor.anonymous(),
            ..Default::default()
        };
        let path = format!("{DATASET_DECISION_PATH}/batch");
        let body = serde_json::to_vec(&DatasetRankBatch {
            subject,
            checks: checks.to_vec(),
            request_id: String::new(),
            trace_id: String::new(),
        })
        .map_err(|_| ())?;
        if body.len() > 1024 * 1024 {
            return Err(());
        }
        let signature = self
            .signer
            .sign("POST", &path, "", &body, &axum::http::HeaderMap::new())
            .map_err(|_| ())?;
        let url = self.origin.join(&path).map_err(|_| ())?;
        let mut response = if let Some(http) = &self.workload_http {
            let mut headers = axum::http::HeaderMap::new();
            if let Some(token) = &self.legacy_transport {
                headers.insert("authorization", token.clone());
            }
            headers.insert("x-gateway-secret", signature.parse().map_err(|_| ())?);
            headers.insert("content-type", "application/json".parse().map_err(|_| ())?);
            tokio::time::timeout(
                Duration::from_secs(5),
                http.send(reqwest::Method::POST, url.as_str(), headers, Some(body)),
            )
            .await
            .map_err(|_| ())?
            .map_err(|_| ())?
        } else {
            let mut request = self
                .client
                .post(url)
                .header("x-gateway-secret", signature)
                .header("content-type", "application/json")
                .body(body);
            if let Some(token) = &self.legacy_transport {
                request = request.header("authorization", token.clone());
            }
            request.send().await.map_err(|_| ())?
        };
        if !response.status().is_success() {
            return Err(());
        }
        let mut bytes = vec![];
        while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err(());
            }
            bytes.extend_from_slice(&chunk);
        }
        let receipts: Vec<DatasetRankReceipt> = serde_json::from_slice(&bytes).map_err(|_| ())?;
        Ok(receipts)
    }
}
impl DatasetObjectPolicy for RemoteDatasetPolicy {
    fn ranks<'a>(
        &'a self,
        actor: &'a UserContext,
        checks: Vec<DatasetRankCheck>,
    ) -> PolicyFuture<'a, Vec<i32>> {
        Box::pin(async move {
            match self.request(actor, &checks).await {
                Ok(ranks) => Ok(ranks),
                Err(RemoteError::Unavailable(_)) if self.mode == Rollout::Shadow => {
                    Ok(checks.iter().map(|c| c.legacy_rank).collect())
                }
                Err(_) => Err(()),
            }
        })
    }
}
pub fn facts(dataset: &Value, grants: Vec<String>, rank: i32) -> DatasetRankCheck {
    let text = |key: &str| dataset[key].as_str().unwrap_or("");
    let mut resource =
        ObjectResource::new(ResourceKind::Dataset, text("dataset_id"), text("tenant_id"));
    resource.owner_id = text("created_by").into();
    resource.visibility = text("visibility").into();
    resource.archived = dataset["is_archived"] == true;
    resource.present = dataset["is_deleted"] != true;
    resource.grants = grants;
    DatasetRankCheck {
        resource,
        legacy_rank: rank,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RemoteError {
    Unavailable(()),
    Incompatible,
}
fn validate_receipts(
    mode: Rollout,
    policy: &str,
    matrix: &str,
    checks: &[DatasetRankCheck],
    receipts: &[DatasetRankReceipt],
) -> Result<Vec<i32>, RemoteError> {
    if receipts.len() != checks.len() {
        return Err(RemoteError::Incompatible);
    }
    receipts
        .iter()
        .zip(checks)
        .map(|(r, c)| {
            if r.resource_id != c.resource.id
                || r.legacy_rank != c.legacy_rank
                || !(0..=3).contains(&c.legacy_rank)
                || !(0..=c.legacy_rank).contains(&r.allowed_rank)
                || r.enforced != (mode == Rollout::Enforce)
                || !policy.is_empty() && r.policy_sha256 != policy
                || !matrix.is_empty() && r.matrix_sha256 != matrix
            {
                return Err(RemoteError::Incompatible);
            }
            Ok(r.allowed_rank)
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn facts_preserve_current_raw_acl_owner_and_lifecycle() {
        let c = facts(
            &json!({"dataset_id":"d","tenant_id":"t","created_by":"u","visibility":"public","is_archived":true,"is_deleted":false}),
            vec!["invalid".into(), "viewer".into()],
            1,
        );
        assert_eq!(c.resource.grants, ["invalid", "viewer"]);
        assert_eq!(c.resource.owner_id, "u");
        assert!(c.resource.archived && c.resource.present);
        assert_eq!(c.legacy_rank, 1);
    }
    #[test]
    fn receipts_bind_order_facts_rollout_and_digests_and_cannot_widen_legacy() {
        let c = facts(&json!({"dataset_id":"d","tenant_id":"t"}), vec![], 1);
        let r = DatasetRankReceipt {
            resource_id: "d".into(),
            allowed_rank: 1,
            legacy_rank: 1,
            enforced: false,
            policy_sha256: "p".into(),
            matrix_sha256: "m".into(),
        };
        let validate = |r: &DatasetRankReceipt| {
            validate_receipts(
                Rollout::Shadow,
                "p",
                "m",
                std::slice::from_ref(&c),
                std::slice::from_ref(r),
            )
        };
        assert_eq!(validate(&r).unwrap(), [1]);
        for field in 0..7 {
            let mut bad = r.clone();
            match field {
                0 => bad.resource_id = "other".into(),
                1 => bad.allowed_rank = 2,
                2 => bad.allowed_rank = -1,
                3 => bad.legacy_rank = 2,
                4 => bad.enforced = true,
                5 => bad.policy_sha256 = "stale".into(),
                _ => bad.matrix_sha256 = "stale".into(),
            };
            assert_eq!(validate(&bad), Err(RemoteError::Incompatible));
        }
        assert!(validate_receipts(Rollout::Enforce, "p", "m", &[c], &[r]).is_err());
    }
    #[test]
    fn server_enforcement_never_silently_downgrades_to_client_shadow() {
        let c = facts(&json!({"dataset_id":"d"}), vec![], 3);
        let r = DatasetRankReceipt {
            resource_id: "d".into(),
            allowed_rank: 0,
            legacy_rank: 3,
            enforced: true,
            policy_sha256: "p".into(),
            matrix_sha256: "m".into(),
        };
        assert_eq!(
            validate_receipts(
                Rollout::Shadow,
                "",
                "",
                std::slice::from_ref(&c),
                std::slice::from_ref(&r)
            ),
            Err(RemoteError::Incompatible)
        );
        assert_eq!(
            validate_receipts(Rollout::Enforce, "p", "m", &[c], &[r]).unwrap(),
            [0]
        );
    }
}

/// A process-scoped policy handle may flow into fresh per-request stores. It
/// carries no caller identity; each call still supplies the verified current actor.
tokio::task_local! {static CURRENT:Option<Arc<dyn DatasetObjectPolicy>>;}
pub fn current() -> Option<Arc<dyn DatasetObjectPolicy>> {
    CURRENT.try_with(Clone::clone).unwrap_or_default()
}
pub async fn scoped<T>(
    policy: Option<Arc<dyn DatasetObjectPolicy>>,
    future: impl Future<Output = T>,
) -> T {
    CURRENT.scope(policy, future).await
}
pub async fn request_context(
    axum::extract::State(policy): axum::extract::State<Option<Arc<dyn DatasetObjectPolicy>>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    scoped(policy, next.run(request)).await
}
#[cfg(test)]
mod scope_tests {
    use super::*;
    struct Deny;
    impl DatasetObjectPolicy for Deny {
        fn ranks<'a>(
            &'a self,
            _: &'a UserContext,
            checks: Vec<DatasetRankCheck>,
        ) -> PolicyFuture<'a, Vec<i32>> {
            Box::pin(async move { Ok(vec![0; checks.len()]) })
        }
    }
    #[tokio::test]
    async fn policy_scope_is_explicit_and_cannot_leak_into_an_unrelated_request() {
        assert!(current().is_none());
        let policy: Arc<dyn DatasetObjectPolicy> = Arc::new(Deny);
        scoped(Some(policy.clone()), async {
            assert!(current().is_some());
            assert_eq!(
                current()
                    .unwrap()
                    .ranks(
                        &UserContext {
                            tenant_id: "t".into(),
                            user_id: "u".into(),
                            user_tier: "normal".into(),
                            user_type: "runtime".into(),
                            roles: vec![],
                            is_authenticated: true
                        },
                        vec![facts(&serde_json::json!({"dataset_id":"d"}), vec![], 3)]
                    )
                    .await
                    .unwrap(),
                [0]
            );
            scoped(None, async {
                assert!(current().is_none());
            })
            .await;
            assert!(current().is_some());
        })
        .await;
        assert!(current().is_none());
    }
}
