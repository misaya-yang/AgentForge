//! Governed Knowledge-only model calls. No assistant turn/session is fabricated.
mod authority;
mod http;
mod identity;
#[cfg(test)]
mod postgres_tests;
mod provider;
pub mod public_contract;
mod replay;
mod scope;
#[cfg(test)]
mod tests;
pub use ai_platform_service_primitives::knowledge_model as wire;
pub use http::router;
pub use identity::{WorkloadAuthentication, WorkloadVerifier};
#[path = "../../../../../agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/lease_v3.rs"]
pub mod signature;
use crate::migration_agent::model::RuntimeProviderCredentials;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{future::Future, pin::Pin, sync::Arc};
use wire::*;
pub type PlaneResult<T> = Result<T, PlaneError>;
pub type PlaneFuture<'a, T> = Pin<Box<dyn Future<Output = PlaneResult<T>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaneError {
    pub status: u16,
    pub code: &'static str,
}
impl PlaneError {
    pub(super) fn new(status: u16, code: &'static str) -> Self {
        Self { status, code }
    }
}
impl std::fmt::Display for PlaneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}
impl std::error::Error for PlaneError {}
impl From<sqlx::Error> for PlaneError {
    fn from(_: sqlx::Error) -> Self {
        Self::new(503, "knowledge_model_authority_unavailable")
    }
}
pub(super) fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}
pub(super) fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
/// Gateway-only signing material. Construct explicitly from the Gateway's private
/// key configuration. Knowledge receives only `signature::PublicKeyring`.
pub struct LeaseAuthority {
    seed: [u8; 32],
    key_id: String,
    public: signature::PublicKeyring,
}
impl LeaseAuthority {
    /// Same Gateway-owned configuration shape as platform lease signing. No
    /// environment reads or private key copies cross the service boundary.
    pub fn from_private_keyring_file(path: &std::path::Path) -> PlaneResult<Self> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Configuration {
            active_key_id: String,
            signing_seed_hex: String,
            verification: signature::PublicKeyring,
        }
        let invalid = || PlaneError::new(503, "knowledge_model_signer_invalid");
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        let file = options.open(path).map_err(|_| invalid())?;
        let metadata = file.metadata().map_err(|_| invalid())?;
        if !metadata.is_file() || metadata.len() > 16_384 {
            return Err(invalid());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
                return Err(invalid());
            }
        }
        use std::io::Read;
        let mut bytes = Vec::new();
        file.take(16_385)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() > 16_384 {
            return Err(invalid());
        }
        let configuration: Configuration = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        let seed =
            signature::unhex::<32>(&configuration.signing_seed_hex).map_err(|_| invalid())?;
        Self::new(
            seed,
            configuration.active_key_id,
            configuration.verification,
        )
    }
    pub fn new(
        seed: [u8; 32],
        key_id: String,
        public: signature::PublicKeyring,
    ) -> PlaneResult<Self> {
        public
            .validate()
            .map_err(|_| PlaneError::new(503, "knowledge_model_signer_invalid"))?;
        let s = Self {
            seed,
            key_id,
            public,
        };
        s.sign(&json!({}), now_ms(), now_ms() + 1000)?;
        Ok(s)
    }
    fn sign(&self, claims: &Value, issued: u64, expires: u64) -> PlaneResult<String> {
        let e = signature::SignedEnvelope::issue(
            &self.seed,
            &self.key_id,
            "knowledge-model-lease",
            claims.clone(),
            issued,
            expires,
            &self.public,
        )
        .map_err(|_| PlaneError::new(503, "knowledge_model_signer_invalid"))?;
        signature::encode(signature::ENVELOPE_PREFIX, &e)
            .map_err(|_| PlaneError::new(503, "knowledge_model_signer_invalid"))
    }
    fn verify(&self, token: &str, lease: &ModelLease) -> PlaneResult<()> {
        let e: signature::SignedEnvelope = signature::decode(signature::ENVELOPE_PREFIX, token)
            .map_err(|_| PlaneError::new(401, "knowledge_model_lease_invalid"))?;
        self.public
            .verify(&e, "knowledge-model-lease", now_ms())
            .map_err(|_| PlaneError::new(401, "knowledge_model_lease_invalid"))?;
        if e.claims != claims(lease)
            || e.expires_at_ms != lease.expires_at_unix.saturating_mul(1000)
        {
            return Err(PlaneError::new(403, "knowledge_model_lease_binding"));
        }
        Ok(())
    }
}
fn claims(l: &ModelLease) -> Value {
    json!({"schema_version":"knowledge-model-lease/v1","lease_id":l.lease_id,"call_id":l.call_id,"request_sha256":l.request_sha256,"tenant_id":l.tenant_id,"user_id":l.user_id,"model":l.model,"expires_at_unix":l.expires_at_unix})
}
#[derive(Clone)]
pub struct KnowledgeModelPlane {
    pub(super) pool: PgPool,
    pub(super) signer: Arc<LeaseAuthority>,
    pub(super) workload: Arc<WorkloadVerifier>,
    pub(super) providers: Arc<dyn RuntimeProviderCredentials>,
    pub(super) admission: Arc<crate::capacity_admission::CapacityAdmissionController>,
    pub(super) capacity: Arc<crate::capacity::CapacityResolver>,
    pub(super) client: reqwest::Client,
    pub(super) price_book_revision: String,
    pub(super) knowledge: Arc<crate::knowledge::KnowledgeService>,
}
impl KnowledgeModelPlane {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        signer: Arc<LeaseAuthority>,
        workload: Arc<WorkloadVerifier>,
        providers: Arc<dyn RuntimeProviderCredentials>,
        admission: Arc<crate::capacity_admission::CapacityAdmissionController>,
        capacity: Arc<crate::capacity::CapacityResolver>,
        price_book_revision: String,
        knowledge: Arc<crate::knowledge::KnowledgeService>,
    ) -> PlaneResult<Self> {
        if price_book_revision.trim().is_empty() {
            return Err(PlaneError::new(503, "knowledge_price_book_required"));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| PlaneError::new(503, "knowledge_model_http_configuration"))?;
        Ok(Self {
            pool,
            signer,
            workload,
            providers,
            admission,
            capacity,
            client,
            price_book_revision,
            knowledge,
        })
    }
    pub async fn reserve(&self, request: ReserveRequest) -> PlaneResult<ModelLease> {
        validate(&request.call, &request.request_sha256)?;
        self.authorize(&request.call).await?;
        if let Some(row) = self.lookup(&request.call).await? {
            return existing_lease(&row, &request.request_sha256);
        }
        let mut snapshot = self.model_snapshot(&request.call).await?;
        let actor = self.current_actor(&request.call).await?;
        let proof = self
            .check_knowledge_scope(&request.call, &actor, None)
            .await?;
        snapshot["scope_check"] = serde_json::to_value(proof)
            .map_err(|_| PlaneError::new(503, "knowledge_scope_proof_encode"))?;
        let price = self.pricing(&request.call).await?;
        let issued = now_ms();
        let expires = ((issued + request.call.budget.deadline_ms + 999) / 1000) * 1000;
        let mut lease = ModelLease {
            lease_id: uuid::Uuid::new_v4().to_string(),
            lease_token: String::new(),
            call_id: request.call.call_id.clone(),
            request_sha256: request.request_sha256.clone(),
            tenant_id: request.call.scope.actor.tenant_id.clone(),
            user_id: request.call.scope.actor.user_id.clone(),
            model: request.call.model.clone(),
            expires_at_unix: expires / 1000,
        };
        lease.lease_token = self.signer.sign(&claims(&lease), issued, expires)?;
        self.reserve_row(&request, &lease, &snapshot, &price).await
    }
    /// Owned task continues after HTTP disconnect. CAS dispatch fence is committed
    /// before provider I/O; a replay never sends the request again.
    pub async fn execute(&self, id: &str, request: ExecuteRequest) -> PlaneResult<ModelExecution> {
        self.execute_events(id, request, None).await
    }
    pub(super) async fn execute_events(
        &self,
        id: &str,
        request: ExecuteRequest,
        events: Option<tokio::sync::mpsc::Sender<ModelStreamEvent>>,
    ) -> PlaneResult<ModelExecution> {
        validate(&request.call, &request.request_sha256)?;
        let row = self
            .lookup(&request.call)
            .await?
            .ok_or(PlaneError::new(404, "knowledge_model_lease_not_found"))?;
        let lease = existing_lease(&row, &request.request_sha256)?;
        if lease.lease_id != id || lease.lease_token != request.lease_token {
            return Err(PlaneError::new(403, "knowledge_model_lease_binding"));
        }
        if let Some(result) = row.get("result").filter(|v| !v.is_null()) {
            self.authorize_delivery(&request.call, &row["snapshot"])
                .await?;
            return serde_json::from_value(result.clone())
                .map_err(|_| PlaneError::new(503, "knowledge_model_receipt_invalid"));
        }
        if let Some(observation) = row.get("observation").filter(|v| !v.is_null()) {
            let response: provider::ProviderResult = serde_json::from_value(observation.clone())
                .map_err(|_| PlaneError::new(503, "knowledge_model_observation_corrupt"))?;
            return replay::settle_before_delivery(
                self.settle(&request.call, &lease, &row["pricing"], response),
                self.authorize_delivery(&request.call, &row["snapshot"]),
            )
            .await;
        }
        self.authorize_delivery(&request.call, &row["snapshot"])
            .await?;
        if text(&row, "state") != "reserved" {
            return unknown_metered(&lease, &row["pricing"]);
        }
        self.signer.verify(&request.lease_token, &lease)?;
        // Only a never-dispatched reservation consults the live price route.
        self.model_snapshot(&request.call).await?;
        let prepared = self.prepare(&request.call, &row["snapshot"]).await?;
        let capacity = self.acquire(&request.call).await?;
        if !self.claim(&lease).await? {
            return unknown_metered(&lease, &row["pricing"]);
        }
        let plane = self.clone();
        let task = tokio::spawn(async move {
            let response = capacity
                .run(
                    plane.dispatch(&request.call, &lease, prepared, events),
                    crate::capacity_admission::AdmissionCancellation::default(),
                )
                .await
                .unwrap_or_else(|_| Err(PlaneError::new(503, "knowledge_capacity_lease_lost")));
            match response {
                Ok(result) => {
                    replay::settle_before_delivery(
                        plane.settle(&request.call, &lease, &row["pricing"], result),
                        plane.authorize_delivery(&request.call, &row["snapshot"]),
                    )
                    .await
                }
                Err(error) => {
                    plane.mark_unknown(&lease, error.code).await?;
                    unknown_metered(&lease, &row["pricing"])
                }
            }
        });
        task.await
            .map_err(|_| PlaneError::new(503, "knowledge_model_dispatch_unknown"))?
    }
}
fn validate(call: &ModelCall, digest: &str) -> PlaneResult<()> {
    call.validate()
        .map_err(|_| PlaneError::new(422, "knowledge_model_call_invalid"))?;
    if call.model.revision.trim().is_empty()
        || call.budget.input_bytes > 32 * 1024 * 1024
        || call.budget.max_output_tokens > 1_000_000
        || serde_json::to_vec(&call.payload)
            .map_err(|_| PlaneError::new(422, "knowledge_model_payload_invalid"))?
            .len() as u64
            > call.budget.input_bytes
    {
        return Err(PlaneError::new(422, "knowledge_model_budget_invalid"));
    }
    if call_digest(call).map_err(|_| PlaneError::new(422, "knowledge_model_payload_invalid"))?
        != digest
    {
        return Err(PlaneError::new(409, "knowledge_model_body_conflict"));
    }
    Ok(())
}
fn existing_lease(row: &Value, digest: &str) -> PlaneResult<ModelLease> {
    if row["request_sha256"] != digest {
        return Err(PlaneError::new(409, "knowledge_model_idempotency_conflict"));
    }
    serde_json::from_value(row["lease"].clone())
        .map_err(|_| PlaneError::new(503, "knowledge_model_lease_corrupt"))
}
fn unknown(lease: &ModelLease, revision: &str) -> ModelExecution {
    ModelExecution {
        call_id: lease.call_id.clone(),
        lease_id: lease.lease_id.clone(),
        request_sha256: lease.request_sha256.clone(),
        state: ExecutionState::DispatchUnknown,
        upstream_status: 0,
        output: Value::Null,
        receipt: UsageReceipt {
            receipt_id: lease.lease_id.clone(),
            call_id: lease.call_id.clone(),
            lease_id: lease.lease_id.clone(),
            tenant_id: lease.tenant_id.clone(),
            user_id: lease.user_id.clone(),
            provider_id: lease.model.provider.clone(),
            model: lease.model.clone(),
            price_book_revision: revision.into(),
            units: Default::default(),
            settled: false,
            metering: None,
        },
    }
}

fn unknown_metered(lease: &ModelLease, pricing: &Value) -> PlaneResult<ModelExecution> {
    let mut execution = unknown(lease, replay::frozen_revision(pricing)?);
    if ai_platform_gateway_foundation::request_meter::is_snapshot(pricing) {
        execution.receipt.metering = Some(ModelMetering {
            kind: MeterKind::Requests,
            tokens: TokenCountState::Unknown,
            cost_nanos: None,
        });
    }
    Ok(execution)
}
