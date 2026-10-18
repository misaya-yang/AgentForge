//! Knowledge model-plane v1: no provider credentials, endpoints or signing keys.
use crate::knowledge_types::UserContext;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
pub const RESERVE_PATH: &str = "/internal/v2/knowledge/model-leases";
pub fn execute_path(id: &str) -> String {
    format!("{RESERVE_PATH}/{id}/execute")
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelScope {
    pub actor: UserContext,
    pub dataset_id: Option<String>,
    pub document_id: Option<String>,
    pub execution_id: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelIdentity {
    pub provider: String,
    pub model: String,
    pub revision: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelPurpose {
    QueryEmbedding,
    DocumentEmbedding,
    SimilarityEmbedding,
    Rerank,
    Ocr,
    Vision,
    Summary,
    Qa,
    Judge,
}
/// Input/output codec only. Gateway selects the actual registered endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelWire {
    Embeddings,
    GeminiEmbedding,
    DashscopeEmbedding,
    DashscopeMultimodal,
    ChatCompletions,
    GeminiGenerateContent,
    DashscopeVision,
    Rerank,
    DashscopeRerank,
    PairScores,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelBudget {
    pub input_bytes: u64,
    pub max_output_tokens: u64,
    pub deadline_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCall {
    pub version: u32,
    pub call_id: String,
    pub scope: ModelScope,
    pub model: ModelIdentity,
    pub purpose: ModelPurpose,
    pub wire: ModelWire,
    pub stream: bool,
    pub budget: ModelBudget,
    pub payload: Value,
}
pub fn call_digest(call: &ModelCall) -> Result<String, serde_json::Error> {
    // Hash the exact typed JSON encoding. Both hops reuse the immutable call.
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(call)?)))
}
impl ModelCall {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1
            || self.call_id.trim().is_empty()
            || self.call_id.len() > 128
            || self.scope.actor.anonymous()
            || self.scope.actor.tenant_id.trim().is_empty()
            || self.scope.actor.user_id.trim().is_empty()
            || self.model.provider.trim().is_empty()
            || self.model.model.trim().is_empty()
            || self.budget.input_bytes == 0
            || self.budget.deadline_ms == 0
            || self.budget.deadline_ms > 600_000
        {
            return Err("invalid_model_call_scope");
        }
        if !self.payload.is_object()
            || [
                "api_key",
                "base_url",
                "endpoint",
                "authorization",
                "access_token",
            ]
            .iter()
            .any(|key| self.payload.get(*key).is_some())
        {
            return Err("model_call_credentials_or_endpoint_forbidden");
        }
        if self.stream && self.wire != ModelWire::ChatCompletions {
            return Err("unsupported_model_stream_codec");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReserveRequest {
    pub request_sha256: String,
    pub call: ModelCall,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelLease {
    pub lease_id: String,
    /// Gateway-signed Ed25519 service lease; opaque to callers except their verifier.
    pub lease_token: String,
    pub call_id: String,
    pub request_sha256: String,
    pub tenant_id: String,
    pub user_id: String,
    pub model: ModelIdentity,
    pub expires_at_unix: u64,
}
/// Signs only the Knowledge workload identity with its own workload key. This
/// is never the Gateway lease-signing key or a provider credential.
pub trait KnowledgeWorkloadSigner: Send + Sync {
    fn sign(
        &self,
        path: &str,
        body: &[u8],
        tenant_id: &str,
        user_id: &str,
    ) -> Result<String, &'static str>;
}
/// A host supplies a verification-only adapter with pinned public keys/key IDs.
/// This interface cannot sign or access provider credentials.
pub trait ServiceModelLeaseVerifier: Send + Sync {
    fn verify(&self, token: &str, expected: &ModelLease) -> Result<(), &'static str>;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteRequest {
    pub lease_token: String,
    pub request_sha256: String,
    pub call: ModelCall,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Completed,
    Rejected,
    DispatchUnknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeterKind {
    Tokens,
    Requests,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenCountState {
    Reported,
    Unknown,
    NotApplicable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelMetering {
    pub kind: MeterKind,
    pub tokens: TokenCountState,
    /// Exact price-book nano-USD quote; absent when spend is unknown or a legacy token meter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_nanos: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageReceipt {
    pub receipt_id: String,
    pub call_id: String,
    pub lease_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub provider_id: String,
    pub model: ModelIdentity,
    pub price_book_revision: String,
    /// Exact metered components; an absent quantity is unknown, never invented zero.
    pub units: BTreeMap<String, u64>,
    pub settled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metering: Option<ModelMetering>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelExecution {
    pub call_id: String,
    pub lease_id: String,
    pub request_sha256: String,
    pub state: ExecutionState,
    pub upstream_status: u16,
    pub output: Value,
    pub receipt: UsageReceipt,
}
/// SSE data frames encode this enum. Delta output retains the selected wire codec;
/// a completed, settled Receipt is mandatory before semantic stream success.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelStreamEvent {
    Delta { output: Value },
    Receipt { execution: ModelExecution },
    Error { code: String },
}
#[cfg(test)]
mod tests {
    use super::*;
    fn call() -> ModelCall {
        ModelCall {
            version: 1,
            call_id: "c".into(),
            scope: ModelScope {
                actor: UserContext {
                    tenant_id: "t".into(),
                    user_id: "u".into(),
                    roles: vec![],
                    user_tier: "normal".into(),
                    user_type: "user".into(),
                    is_authenticated: true,
                },
                dataset_id: Some("d".into()),
                document_id: None,
                execution_id: None,
            },
            model: ModelIdentity {
                provider: "p".into(),
                model: "m".into(),
                revision: "r".into(),
            },
            purpose: ModelPurpose::Judge,
            wire: ModelWire::ChatCompletions,
            stream: false,
            budget: ModelBudget {
                input_bytes: 1,
                max_output_tokens: 10,
                deadline_ms: 1000,
            },
            payload: serde_json::json!({"messages":[]}),
        }
    }
    #[test]
    fn hash_binds_actor_model_operation_payload_and_budget() {
        let c = call();
        let hash = call_digest(&c).unwrap();
        for mut other in [c.clone(), c.clone(), c.clone(), c.clone(), c.clone()]
            .into_iter()
            .enumerate()
        {
            match other.0 {
                0 => other.1.scope.actor.tenant_id = "x".into(),
                1 => other.1.model.revision = "next".into(),
                2 => other.1.purpose = ModelPurpose::Qa,
                3 => other.1.payload = serde_json::json!({"messages":["changed"]}),
                _ => other.1.budget.max_output_tokens = 11,
            };
            assert_ne!(hash, call_digest(&other.1).unwrap());
        }
    }
    #[test]
    fn secrets_anonymous_and_unknown_operations_fail_closed() {
        let mut c = call();
        c.payload["api_key"] = serde_json::json!("synthetic");
        assert!(c.validate().is_err());
        c = call();
        c.scope.actor.is_authenticated = false;
        assert!(c.validate().is_err());
        assert!(
            serde_json::from_value::<ModelPurpose>(serde_json::json!("unbudgeted_direct_vendor"))
                .is_err()
        );
    }
}

tokio::task_local! { static VERIFIED_MODEL_ACTOR: UserContext; }
/// Call only after existing workload/JWT verification. Tokio restores the prior
/// task-local scope on completion, cancellation and unwind.
pub async fn with_actor_scope<F: std::future::Future>(actor: UserContext, future: F) -> F::Output {
    use crate::data_connection::{TenantScope, current_tenant_scope, with_tenant_scope};
    let tenant = if actor.anonymous() || current_tenant_scope().is_ok() {
        None
    } else {
        TenantScope::new(&actor.tenant_id).ok()
    };
    match tenant {
        Some(tenant) => {
            VERIFIED_MODEL_ACTOR
                .scope(actor, with_tenant_scope(tenant, future))
                .await
        }
        // Keep an existing database scope: the transaction binder must reject
        // a nested actor for a different tenant instead of silently replacing it.
        None => VERIFIED_MODEL_ACTOR.scope(actor, future).await,
    }
}
pub fn verified_actor_scope() -> Option<UserContext> {
    VERIFIED_MODEL_ACTOR.try_with(Clone::clone).ok()
}

#[cfg(test)]
mod database_scope_tests {
    use super::*;
    use crate::data_connection::{TenantScope, current_tenant_scope, validate_tenant_binding};

    fn actor(tenant: &str) -> UserContext {
        UserContext {
            tenant_id: tenant.into(),
            user_id: "user".into(),
            roles: vec![],
            user_tier: "normal".into(),
            user_type: "user".into(),
            is_authenticated: true,
        }
    }

    #[tokio::test]
    async fn verified_request_installs_and_restores_database_tenant() {
        assert!(current_tenant_scope().is_err());
        with_actor_scope(actor("tenant-a"), async {
            tokio::task::yield_now().await;
            assert_eq!(current_tenant_scope().unwrap().as_str(), "tenant-a");
            assert!(validate_tenant_binding(&TenantScope::new("tenant-a").unwrap()).is_ok());
            assert!(validate_tenant_binding(&TenantScope::new("tenant-b").unwrap()).is_err());
        })
        .await;
        assert!(current_tenant_scope().is_err());
        assert!(verified_actor_scope().is_none());
    }

    #[tokio::test]
    async fn nested_actor_cannot_replace_the_request_database_tenant() {
        with_actor_scope(actor("tenant-a"), async {
            with_actor_scope(actor("tenant-b"), async {
                assert_eq!(verified_actor_scope().unwrap().tenant_id, "tenant-b");
                assert_eq!(current_tenant_scope().unwrap().as_str(), "tenant-a");
                assert!(validate_tenant_binding(&TenantScope::new("tenant-b").unwrap()).is_err());
            })
            .await;
            assert_eq!(verified_actor_scope().unwrap().tenant_id, "tenant-a");
            assert_eq!(current_tenant_scope().unwrap().as_str(), "tenant-a");
        })
        .await;
        assert!(current_tenant_scope().is_err());
    }
}

pub const SCOPE_CHECK_PATH: &str = "/internal/v2/knowledge/model-operation-scope";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelScopeCheckRequest {
    pub call_id: String,
    pub request_sha256: String,
    pub scope: ModelScope,
    pub purpose: ModelPurpose,
    pub expected_generation: Option<crate::knowledge_types::IndexGeneration>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelScopeCheckResponse {
    pub call_id: String,
    pub request_sha256: String,
    pub tenant_id: String,
    pub user_id: String,
    pub dataset_id: Option<String>,
    pub document_id: Option<String>,
    pub execution_id: Option<String>,
    pub purpose: ModelPurpose,
    pub generation: Option<crate::knowledge_types::IndexGeneration>,
}

#[cfg(test)]
mod actor_scope_tests {
    use super::*;
    #[tokio::test]
    async fn actor_scopes_are_isolated_between_concurrent_tenants_and_restored() {
        assert!(verified_actor_scope().is_none());
        let mut tasks = Vec::new();
        for index in 0..32 {
            tasks.push(tokio::spawn(async move {
                let actor:UserContext=serde_json::from_value(serde_json::json!({"tenant_id":format!("tenant-{index}"),"user_id":format!("user-{index}")})).unwrap();
                let tenant=actor.tenant_id.clone();
                with_actor_scope(actor,async {tokio::task::yield_now().await;assert_eq!(verified_actor_scope().unwrap().tenant_id,tenant);}).await;
                assert!(verified_actor_scope().is_none());
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert!(verified_actor_scope().is_none());
    }
}
