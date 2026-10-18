//! Private single-provider model plane: immutable lease authority and budget.
//! No provider credential crosses to the Runtime or public response payload.
use super::{AgentError, AgentFuture, Scope, entry::SourceAccess, spec, text};
use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use futures_core::Stream;
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, BTreeSet},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::sync::mpsc;
#[path = "model_guarded_stream.rs"]
mod guarded_stream;
pub const LEASE_SCHEMA: &str = "agent-runtime-model-lease/v1";
#[derive(Clone)]
pub struct LeaseSigner {
    secret: Arc<Vec<u8>>,
}
impl LeaseSigner {
    pub fn new(secret: String) -> Result<Self, AgentError> {
        if secret.chars().count() < 16 && !super::lease_authority::signing_enabled()? {
            return Err(AgentError::code(503, "RUNTIME_MODEL_LEASE_SECRET_INVALID"));
        }
        Ok(Self {
            secret: Arc::new(secret.into_bytes()),
        })
    }
    pub fn sign(&self, claims: &Value) -> Result<String, AgentError> {
        validate_claims(claims)?;
        if let Some(signature) = super::lease_authority::sign_claims("model-lease", claims)? {
            return Ok(signature);
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret)
            .map_err(|_| AgentError::code(503, "RUNTIME_MODEL_LEASE_SECRET_INVALID"))?;
        mac.update(spec::canonical(claims).as_bytes());
        Ok(format!("v1:{}", spec::hex(&mac.finalize().into_bytes())))
    }
    pub fn verify(&self, signature: &str, claims: &Value) -> Result<(), AgentError> {
        validate_claims(claims)?;
        if signature.starts_with(super::lease_authority::wire::ENVELOPE_PREFIX) {
            return super::lease_authority::verify_claims(
                "model-lease",
                signature,
                claims,
                super::lease_authority::now_ms(),
            );
        }
        if !super::lease_authority::legacy_admitted(super::lease_authority::now_ms()) {
            return Err(AgentError::code(
                401,
                "RUNTIME_MODEL_LEASE_SIGNATURE_INVALID",
            ));
        }
        let s = signature
            .strip_prefix("v1:")
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| AgentError::code(401, "RUNTIME_MODEL_LEASE_SIGNATURE_INVALID"))?;
        let bytes = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| AgentError::code(401, "RUNTIME_MODEL_LEASE_SIGNATURE_INVALID"))?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret)
            .map_err(|_| AgentError::code(503, "RUNTIME_MODEL_LEASE_SECRET_INVALID"))?;
        mac.update(spec::canonical(claims).as_bytes());
        mac.verify_slice(&bytes)
            .map_err(|_| AgentError::code(401, "RUNTIME_MODEL_LEASE_SIGNATURE_INVALID"))
    }
}
fn validate_claims(v: &Value) -> Result<(), AgentError> {
    if v["schema_version"] != LEASE_SCHEMA {
        return Err(AgentError::code(
            401,
            "RUNTIME_MODEL_LEASE_VERSION_UNSUPPORTED",
        ));
    }
    for k in [
        "lease_id",
        "snapshot_id",
        "run_id",
        "runtime_thread_id",
        "tenant_id",
        "user_id",
        "session_id",
        "provider_id",
        "model_id",
    ] {
        let s = text(v, k);
        if s.is_empty() || s.trim() != s {
            return Err(AgentError::code(401, "RUNTIME_MODEL_LEASE_INVALID"));
        }
    }
    let nonce = text(v, "nonce_sha256");
    if v["capability_revision"].as_i64().unwrap_or(0) < 1
        || v["issued_at_ms"].as_i64().is_none()
        || v["expires_at_ms"].as_i64().unwrap_or(0) <= v["issued_at_ms"].as_i64().unwrap_or(0)
        || nonce.len() != 64
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(AgentError::code(401, "RUNTIME_MODEL_LEASE_INVALID"));
    }
    Ok(())
}
pub fn lease_claims(row: &Value) -> Result<Value, AgentError> {
    let mut out = json!({});
    for k in [
        "schema_version",
        "lease_id",
        "snapshot_id",
        "run_id",
        "runtime_thread_id",
        "tenant_id",
        "user_id",
        "session_id",
        "provider_id",
        "model_id",
        "capability_revision",
        "issued_at_ms",
        "expires_at_ms",
        "nonce_sha256",
    ] {
        out[k] = row[k].clone();
    }
    validate_claims(&out)?;
    Ok(out)
}
pub fn scope_hash(scope: &Scope) -> String {
    let mut h = Sha256::new();
    for value in [&scope.tenant_id, &scope.user_id, &scope.session_id] {
        h.update((value.len() as u64).to_be_bytes());
        h.update(value.as_bytes());
    }
    spec::hex(&h.finalize())
}
pub fn estimate_tokens(value: &Value) -> i64 {
    ((spec::canonical(value).len() as i64 + 3) / 4).max(1)
}
fn numeric(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0.0)
}
pub fn cost_microusd(input: i64, output: i64, pricing: &Value) -> i64 {
    ((input as f64 * numeric(&pricing["input_price_per_1k"]).max(0.0) * 1000.0)
        + (output as f64 * numeric(&pricing["output_price_per_1k"]).max(0.0) * 1000.0))
        .ceil()
        .max(0.0) as i64
}
#[derive(Clone)]
pub struct AuthorizedCall {
    pub call_id: String,
    pub lease_id: String,
    pub run_id: String,
    pub scope: Scope,
    pub model_id: String,
    pub provider_id: String,
    pub provider_revision: String,
    pub snapshot: Value,
    pub estimated_input_tokens: i64,
    pub reserved_output_tokens: i64,
}
pub trait RuntimeProviderCredentials: Send + Sync {
    fn runtime_provider<'a>(&'a self, tenant: &'a str, id: &'a str) -> AgentFuture<'a, Value>;
}
impl<D, C, H, G, E, T> RuntimeProviderCredentials
    for crate::provider_service::ProviderService<D, C, H, G, E, T>
where
    D: crate::provider_service::ProviderDatabase,
    C: crate::provider_service::ProviderCipher,
    H: crate::provider_service::ProviderHttp,
    G: crate::provider_service::ProviderGoogleAuth,
    E: crate::provider_service::ProviderEnvironment,
    T: crate::provider_service::ProviderClock,
{
    fn runtime_provider<'a>(&'a self, tenant: &'a str, id: &'a str) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            self.get_runtime_provider_config(tenant, id)
                .await
                .map(|v| v.into_internal_value())
                .map_err(|_| AgentError::code(503, "RUNTIME_PROVIDER_UNAVAILABLE"))
        })
    }
}
#[derive(Clone)]
pub struct AgentModelPlane {
    pub context_metrics:
        Option<Arc<crate::migration_host::context_metrics::ContextMetricsCollector>>,
    pub admission: Option<Arc<crate::capacity_admission::CapacityAdmissionController>>,
    pub capacity: Option<Arc<crate::capacity::CapacityResolver>>,
    pub pool: PgPool,
    pub signer: Arc<LeaseSigner>,
    pub providers: Arc<dyn RuntimeProviderCredentials>,
    pub sources: Arc<dyn SourceAccess>,
    pub client: reqwest::Client,
    pub internal_token: Arc<String>,
}
impl AgentModelPlane {
    async fn acquire_provider(
        &self,
        call: &AuthorizedCall,
    ) -> Result<
        Option<Arc<crate::capacity_admission::atomic::model_plane::ModelPlaneLease>>,
        AgentError,
    > {
        let (Some(admission), Some(resolver)) = (&self.admission, &self.capacity) else {
            return Err(AgentError::code(503, "MODEL_ADMISSION_UNAVAILABLE"));
        };
        let shared = admission
            .model_plane_admission()
            .ok_or_else(|| AgentError::code(503, "MODEL_ADMISSION_UNAVAILABLE"))?;
        let budgets = resolver
            .resolve(&crate::capacity::CapacityRequest {
                tenant_id: call.scope.tenant_id.clone(),
                service_id: "agent-runtime-provider".into(),
                request_class: "stream".into(),
                resource_kind: "provider".into(),
                provider_id: json!(call.provider_id),
                upstream_group: Value::Null,
                is_admin_read: false,
                service_config: Value::Null,
            })
            .map_err(|_| AgentError::code(503, "CAPACITY_UNAVAILABLE"))?;
        let lease = shared
            .acquire_assistant(
                crate::capacity_admission::atomic::model_plane::ModelCallBinding {
                    tenant: call.scope.tenant_id.clone(),
                    user: call.scope.user_id.clone(),
                    reservation_id: call.call_id.clone(),
                    provider: call.provider_id.clone(),
                    model: call.model_id.clone(),
                },
                &budgets,
            )
            .await
            .map_err(|_| AgentError::code(429, "MODEL_ADMISSION_REJECTED"))?;
        Ok(Some(lease))
    }
    pub async fn authorize_and_reserve(
        &self,
        body: &Value,
        metadata: &Value,
    ) -> Result<AuthorizedCall, AgentError> {
        let id = super::uuid(text(metadata, "ai_platform_lease_id"))
            .map_err(|_| AgentError::code(401, "RUNTIME_MODEL_LEASE_INVALID"))?;
        let tenant =
            ai_platform_gateway_foundation::tenant_database::model_lease_tenant(&self.pool, &id)
                .await
                .map_err(|error| match error {
                    sqlx::Error::RowNotFound => {
                        AgentError::code(401, "RUNTIME_MODEL_LEASE_NOT_FOUND")
                    }
                    error => AgentError::from(error),
                })?;
        let data:Value=sqlx::query_scalar("SELECT to_jsonb(l)||jsonb_build_object('snapshot',s.snapshot,'snapshot_sha256',s.snapshot_sha256,'execution_owner_id',o.owner_id,'execution_fence',o.fence,'issued_at_ms',floor(extract(epoch from l.issued_at)*1000)::bigint,'expires_at_ms',floor(extract(epoch from l.expires_at)*1000)::bigint) FROM assistant_runtime_model_leases l JOIN assistant_runtime_snapshots s ON s.snapshot_id=l.snapshot_id AND s.run_id=l.run_id AND s.tenant_id=l.tenant_id AND s.user_id=l.user_id AND s.session_id=l.session_id JOIN assistant_runs r ON r.run_id=l.run_id LEFT JOIN assistant_runtime_execution_owners o ON o.run_id=r.run_id WHERE l.lease_id=$1::uuid AND (o.run_id IS NULL OR o.lease_until>NOW()) AND l.status='active' AND l.expires_at>NOW() AND r.status='running' AND r.engine='agent_runtime' AND NOT EXISTS(SELECT 1 FROM assistant_runtime_snapshot_revocations revoked WHERE revoked.snapshot_id=l.snapshot_id)").bind(&id).fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &tenant)).await?.ok_or_else(||AgentError::code(401,"RUNTIME_MODEL_LEASE_NOT_FOUND"))?;
        let owner = &data["execution_owner_id"];
        let fence = &data["execution_fence"];
        let scalar = |v: &Value| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        };
        if !owner.is_null()
            && (text(metadata, "ai_platform_execution_owner") != scalar(owner)
                || metadata.get("ai_platform_execution_fence").map(&scalar) != Some(scalar(fence)))
        {
            return Err(AgentError::code(403, "RUNTIME_EXECUTION_FENCE_LOST"));
        }
        let claims = lease_claims(&data)?;
        self.signer
            .verify(text(metadata, "ai_platform_lease_signature"), &claims)?;
        let scope = Scope {
            tenant_id: text(&claims, "tenant_id").into(),
            user_id: text(&claims, "user_id").into(),
            session_id: text(&claims, "session_id").into(),
        };
        self.validate_thread_scope(&claims, metadata, &scope)
            .await?;
        if body["model"] != claims["model_id"] {
            return Err(AgentError::code(403, "RUNTIME_MODEL_LEASE_MODEL_MISMATCH"));
        }
        let snapshot = &data["snapshot"];
        if !snapshot.is_object() {
            return Err(AgentError::code(503, "RUNTIME_MODEL_SNAPSHOT_INVALID"));
        }
        if spec::hash(snapshot) != text(&data, "snapshot_sha256") {
            return Err(AgentError::code(
                503,
                "RUNTIME_MODEL_SNAPSHOT_HASH_MISMATCH",
            ));
        }
        if let Some(pricing) = cache_pricing(snapshot) {
            ai_platform_gateway_foundation::price_book::validate_token_snapshot(pricing)
                .map_err(|_| AgentError::code(503, "RUNTIME_CACHE_PRICING_INVALID"))?;
            let admitted:bool=sqlx::query_scalar("SELECT to_regprocedure('assistant.complete_assistant_runtime_model_call_v2(uuid,character varying,bigint,bigint,bigint,character varying,jsonb)') IS NOT NULL")
                .fetch_one(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &tenant)).await?;
            if !admitted {
                return Err(AgentError::code(503, "RUNTIME_CACHE_SCHEMA_NOT_ADMITTED"));
            }
        }
        self.sources.require_conversation(&scope, false).await?;
        let input = estimate_tokens(&body["input"]);
        let output = body["max_output_tokens"]
            .as_i64()
            .unwrap_or(
                snapshot["limits"]["max_output_tokens"]
                    .as_i64()
                    .unwrap_or(4096),
            )
            .max(1);
        let cost = cost_microusd(input, output, &snapshot["pricing"]);
        let call = uuid::Uuid::new_v4().to_string();
        let hash = spec::hash(&if owner.is_null() {
            body.clone()
        } else {
            json!({"body":body,"execution_fence":fence})
        });
        let result =
            if owner.is_null() {
                sqlx::query(
                    "SELECT reserve_assistant_runtime_model_call($1::uuid,$2::uuid,$3,$4,$5,$6)",
                )
                .bind(&call)
                .bind(&id)
                .bind(&hash)
                .bind(input)
                .bind(output)
                .bind(cost)
                .execute(ai_platform_gateway_foundation::tenant_database::executor(
                    &self.pool, &tenant,
                ))
                .await
            } else {
                sqlx::query(
                "SELECT reserve_assistant_runtime_model_call($1::uuid,$2::uuid,$3,$4,$5,$6,$7,$8)",
            )
            .bind(&call)
            .bind(&id)
            .bind(&hash)
            .bind(input)
            .bind(output)
            .bind(cost)
            .bind(text(&data, "execution_owner_id"))
            .bind(fence.as_i64().unwrap_or(0))
            .execute(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &tenant))
            .await
            };
        if let Err(error) = result {
            let message = error.as_database_error().map(|d| d.message()).unwrap_or("");
            return Err(if message.contains("MODEL_CALL_REPLAYED") {
                AgentError::code(409, "RUNTIME_MODEL_CALL_REPLAYED")
            } else if message.contains("BUDGET_EXHAUSTED") {
                AgentError::code(429, "RUNTIME_MODEL_LEASE_BUDGET_EXHAUSTED")
            } else {
                AgentError::code(403, "RUNTIME_MODEL_LEASE_REJECTED")
            });
        }
        Ok(AuthorizedCall {
            call_id: call,
            lease_id: id,
            run_id: text(&claims, "run_id").into(),
            scope,
            model_id: text(&claims, "model_id").into(),
            provider_id: text(&claims, "provider_id").into(),
            provider_revision: text(&data, "provider_revision").into(),
            snapshot: snapshot.clone(),
            estimated_input_tokens: input,
            reserved_output_tokens: output,
        })
    }
    async fn validate_thread_scope(
        &self,
        claims: &Value,
        metadata: &Value,
        scope: &Scope,
    ) -> Result<(), AgentError> {
        let denied = || AgentError::code(403, "RUNTIME_MODEL_LEASE_SCOPE_MISMATCH");
        let thread = text(metadata, "thread_id");
        let turn = text(metadata, "turn_id");
        let root_turn = text(metadata, "root_turn_id");
        let parent = text(metadata, "parent_thread_id");
        let parent_turn = text(metadata, "parent_turn_id");
        if text(metadata, "ai_platform_scope_sha256") != scope_hash(scope) {
            return Err(denied());
        }
        if thread == text(claims, "runtime_thread_id") {
            if turn != text(claims, "run_id")
                || (!root_turn.is_empty() && root_turn != text(claims, "run_id"))
                || !parent.is_empty()
                || !parent_turn.is_empty()
            {
                return Err(denied());
            }
            return Ok(());
        }
        if root_turn != text(claims, "run_id")
            || thread.is_empty()
            || turn.is_empty()
            || parent.is_empty()
            || parent == thread
        {
            return Err(denied());
        }
        for id in [thread, turn, parent] {
            super::uuid(id).map_err(|_| denied())?;
        }
        let member:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('parent_kernel_thread_id',parent_kernel_thread_id,'relation_kind',relation_kind) FROM assistant_runtime_thread_members WHERE kernel_thread_id=$1::uuid AND runtime_thread_id=$2::uuid AND tenant_id=$3 AND user_id=$4 AND session_id=$5").bind(thread).bind(text(claims,"runtime_thread_id")).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &scope.tenant_id)).await?;
        let m = member.ok_or_else(denied)?;
        if m["relation_kind"] != "subagent"
            || text(&m, "parent_kernel_thread_id") != super::uuid(parent)?
        {
            return Err(denied());
        }
        Ok(())
    }
    pub async fn complete_call(
        &self,
        call: &AuthorizedCall,
        input: i64,
        output: i64,
        provider_request_id: Option<&str>,
    ) -> Result<(), AgentError> {
        sqlx::query("SELECT complete_assistant_runtime_model_call($1::uuid,$2,$3,$4,$5)")
            .bind(&call.call_id)
            .bind(input)
            .bind(output)
            .bind(cost_microusd(input, output, &call.snapshot["pricing"]))
            .bind(provider_request_id)
            .execute(ai_platform_gateway_foundation::tenant_database::executor(
                &self.pool,
                &call.scope.tenant_id,
            ))
            .await?;
        Ok(())
    }
    async fn complete_call_versioned(
        &self,
        call: &AuthorizedCall,
        input: i64,
        output: i64,
        provider_request: Option<&str>,
        raw_usage: Value,
    ) -> Result<(), AgentError> {
        if cache_pricing(&call.snapshot).is_none() {
            return self
                .complete_call(call, input, output, provider_request)
                .await;
        }
        let (cost, receipt) = cache_settlement(call, input, output, &raw_usage)?;
        sqlx::query(
            "SELECT assistant.complete_assistant_runtime_model_call_v2($1::uuid,$2,$3,$4,$5,$6,$7)",
        )
        .bind(&call.call_id)
        .bind(&call.scope.tenant_id)
        .bind(input)
        .bind(output)
        .bind(cost)
        .bind(provider_request)
        .bind(receipt)
        .execute(ai_platform_gateway_foundation::tenant_database::executor(
            &self.pool,
            &call.scope.tenant_id,
        ))
        .await?;
        Ok(())
    }
    pub async fn fail_call(
        &self,
        id: &str,
        code: &str,
        dispatched: bool,
    ) -> Result<(), AgentError> {
        let tenant = match ai_platform_gateway_foundation::tenant_database::model_call_tenant(
            &self.pool, id,
        )
        .await
        {
            Ok(tenant) => tenant,
            Err(sqlx::Error::RowNotFound) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        sqlx::query("UPDATE assistant_runtime_model_calls SET status=$2,error_code=$3,completed_at=NOW(),updated_at=NOW() WHERE call_id=$1::uuid AND status IN ('reserved','dispatched')").bind(id).bind(if dispatched{"unknown"}else{"failed"}).bind(code).execute(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &tenant)).await?;
        Ok(())
    }
    async fn prepare(&self, body: &Value, call: &AuthorizedCall) -> Result<Prepared, AgentError> {
        let provider = self
            .providers
            .runtime_provider(&call.scope.tenant_id, &call.provider_id)
            .await?;
        if text(&provider, "updated_at") != call.provider_revision {
            return Err(AgentError::code(409, "RUNTIME_PROVIDER_REVISION_CHANGED"));
        }
        let key = text(&provider, "api_key");
        let base = text(&provider, "runtime_base_url");
        if key.is_empty() || base.is_empty() {
            return Err(AgentError::code(503, "RUNTIME_PROVIDER_UNAVAILABLE"));
        }
        let wire = text(&call.snapshot["model"], "wire_protocol");
        let native = wire == "responses_v1";
        if !native && wire != "chat_completions" {
            return Err(AgentError::code(422, "RUNTIME_PROVIDER_WIRE_UNSUPPORTED"));
        }
        let mut url = reqwest::Url::parse(base)
            .map_err(|_| AgentError::code(503, "RUNTIME_PROVIDER_ENDPOINT_INVALID"))?;
        if !matches!(url.scheme(), "https" | "http") || url.host_str().is_none() {
            return Err(AgentError::code(503, "RUNTIME_PROVIDER_ENDPOINT_INVALID"));
        }
        let path = url.path().trim_end_matches('/');
        let path = if native {
            if path.ends_with("/responses") {
                path.into()
            } else if path.ends_with("/v1") {
                format!("{path}/responses")
            } else {
                format!("{path}/v1/responses")
            }
        } else if path.ends_with("/chat/completions") {
            path.into()
        } else {
            format!("{path}/chat/completions")
        };
        url.set_path(&path);
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {key}")
                .parse()
                .map_err(|_| AgentError::code(503, "RUNTIME_PROVIDER_UNAVAILABLE"))?,
        );
        headers.insert("content-type", "application/json".parse().expect("header"));
        let profile = &call.snapshot["capabilities"];
        if profile["prompt_cache"]["adapter_id"] == "cache/dashscope-session-v1"
            && profile["prompt_cache"]["config"]["enabled"] == true
        {
            headers.insert(
                "x-dashscope-session-cache",
                "enable".parse().expect("header"),
            );
        }
        let (body, aliases, allowed, native_search) = provider_body(body, call, native)?;
        Ok(Prepared {
            url,
            headers,
            body,
            aliases,
            allowed,
            native,
            native_search,
        })
    }
}
struct Prepared {
    url: reqwest::Url,
    headers: HeaderMap,
    body: Value,
    aliases: BTreeMap<String, (String, String)>,
    allowed: BTreeSet<(Option<String>, String)>,
    native: bool,
    native_search: bool,
}
pub struct ModelStream {
    receiver: mpsc::Receiver<Result<Bytes, AgentError>>,
    task: tokio::task::JoinHandle<()>,
    first: Option<Bytes>,
}
impl Stream for ModelStream {
    type Item = Result<Bytes, AgentError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(first) = self.first.take() {
            return Poll::Ready(Some(Ok(first)));
        }
        self.receiver.poll_recv(cx)
    }
}
impl Drop for ModelStream {
    fn drop(&mut self) {
        self.task.abort();
    }
}
#[derive(Default)]
struct ProviderFrames {
    next: i64,
    response: Option<String>,
}
impl ProviderFrames {
    fn observe(&mut self, frame: &Bytes) {
        if !frame.starts_with(b"event:") {
            return;
        }
        if self.next == 0 {
            let text = String::from_utf8_lossy(frame);
            if let Some(data) = text.lines().find_map(|line| line.strip_prefix("data:")) {
                if let Ok(value) = serde_json::from_str::<Value>(data.trim()) {
                    self.response = value["response"]["id"].as_str().map(str::to_owned);
                }
            }
        }
        self.next += 1;
    }
    fn failed(&self) -> Bytes {
        let mut response = json!({"status":"failed","error":{"type":"runtime_model_plane_error","code":"RUNTIME_PROVIDER_TRANSPORT_ERROR","message":"The provider connection was interrupted. The outcome is unknown; the request was not retried."}});
        if let Some(id) = &self.response {
            response["id"] = id.clone().into();
        }
        super::model_projection::frame(
            "response.failed",
            &json!({"type":"response.failed","sequence_number":self.next,"response":response}),
        )
    }
}
struct CallCleanup {
    plane: AgentModelPlane,
    id: String,
    armed: bool,
    lease: Option<Arc<crate::capacity_admission::atomic::model_plane::ModelPlaneLease>>,
}
impl Drop for CallCleanup {
    fn drop(&mut self) {
        let plane = self.plane.clone();
        let id = self.id.clone();
        let armed = self.armed;
        let lease = self.lease.take();
        tokio::spawn(async move {
            if armed
                && let Ok(tenant) =
                    ai_platform_gateway_foundation::tenant_database::model_call_tenant(
                        &plane.pool,
                        &id,
                    )
                    .await
            {
                let _=sqlx::query("UPDATE assistant_runtime_model_calls SET status=CASE WHEN status='reserved' THEN 'failed' ELSE 'unknown' END,error_code='model_stream_interrupted',completed_at=NOW(),updated_at=NOW() WHERE call_id=$1::uuid AND status IN ('reserved','dispatched')").bind(id).execute(ai_platform_gateway_foundation::tenant_database::executor(&plane.pool, &tenant)).await;
            }
            if let Some(lease) = lease {
                lease.release().await;
            }
        });
    }
}
fn tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn kernel_tool(name: &str, namespace: Option<&str>) -> bool {
    let names = [
        "update_plan",
        "spawn_subagent",
        "spawn_agent",
        "send_input",
        "wait",
        "close_agent",
        "resume_agent",
        "send_message",
        "followup_task",
        "wait_agent",
        "list_agents",
        "interrupt_agent",
    ];
    match namespace {
        None | Some("") => {
            names.contains(&name)
                || ["collaboration", "multi_agent_v1"]
                    .iter()
                    .any(|prefix| names.iter().any(|tool| name == format!("{prefix}{tool}")))
        }
        Some("collaboration" | "multi_agent_v1") => names.contains(&name),
        _ => false,
    }
}
fn output_text(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.into());
    }
    let a = v.as_array()?;
    a.iter()
        .map(|p| {
            if matches!(text(p, "type"), "input_text" | "output_text" | "text") {
                p["text"].as_str().map(str::to_owned)
            } else {
                None
            }
        })
        .collect::<Option<Vec<_>>>()
        .map(|v| v.join(""))
}
pub fn validate_transcript(
    input: &Value,
    allowed: &BTreeSet<String>,
    namespaced: &BTreeSet<(String, String)>,
) -> Result<(), AgentError> {
    let mut pending: BTreeMap<String, (String, Option<String>, bool)> = BTreeMap::new();
    let mut complete: BTreeSet<String> = BTreeSet::new();
    let invalid = || AgentError::code(422, "RUNTIME_TOOL_TRANSCRIPT_INVALID");
    for item in input.as_array().into_iter().flatten() {
        match text(item, "type") {
            "function_call" => {
                let id = text(item, "call_id");
                let name = text(item, "name");
                let namespace = item["namespace"].as_str().map(str::to_owned);
                if id.is_empty()
                    || !tool_name(name)
                    || !item["arguments"].is_string()
                    || pending.contains_key(id)
                    || complete.contains(id)
                {
                    return Err(invalid());
                }
                let yes = kernel_tool(name, namespace.as_deref())
                    || if let Some(ns) = namespace.as_ref().filter(|s| !s.is_empty()) {
                        namespaced.contains(&(ns.clone(), name.into()))
                    } else {
                        allowed.contains(name)
                    };
                pending.insert(id.into(), (name.into(), namespace, yes));
            }
            "function_call_output" => {
                let id = text(item, "call_id");
                let Some((name, namespace, yes)) = pending.remove(id) else {
                    return Err(invalid());
                };
                let output = output_text(&item["output"]).ok_or_else(invalid)?;
                if item["name"].as_str().is_some_and(|s| s != name)
                    || item["namespace"]
                        .as_str()
                        .is_some_and(|s| Some(s) != namespace.as_deref())
                {
                    return Err(invalid());
                }
                if !yes
                    && !(namespace.as_deref().is_none_or(str::is_empty)
                        && (output == format!("unsupported call: {name}")
                            || output == format!("unsupported custom tool call: {name}")))
                {
                    return Err(invalid());
                }
                complete.insert(id.into());
            }
            _ => {}
        }
    }
    if !pending.is_empty() {
        return Err(invalid());
    }
    Ok(())
}
fn content_text(v: &Value) -> String {
    if let Some(s) = v.as_str() {
        return s.into();
    }
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| match text(p, "type") {
            "input_text" | "output_text" | "text" => p["text"].as_str(),
            "encrypted_content" => p["encrypted_content"].as_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}
pub fn responses_to_messages(body: &Value) -> Result<Value, AgentError> {
    let mut messages = Vec::<Value>::new();
    if let Some(s) = body["instructions"].as_str().filter(|s| !s.is_empty()) {
        messages.push(json!({"role":"system","content":s}));
    }
    if let Some(s) = body["input"].as_str() {
        messages.push(json!({"role":"user","content":s}));
        return Ok(json!(messages));
    }
    let input = body["input"]
        .as_array()
        .ok_or_else(|| AgentError::code(400, "RUNTIME_RESPONSES_INPUT_INVALID"))?;
    let mut pending = BTreeMap::<String, String>::new();
    for item in input {
        let kind = text(item, "type");
        match kind {
            "" | "message" | "agent_message" => {
                let role = if kind == "agent_message" {
                    "user"
                } else {
                    text(item, "role")
                };
                if !matches!(role, "user" | "assistant" | "developer" | "system") {
                    continue;
                }
                let text = content_text(&item["content"]);
                let images: Vec<_> = item["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|v| v["type"] == "input_image")
                    .map(|v| v["image_url"].clone())
                    .collect();
                if !images.is_empty()
                    && (role != "user"
                        || images.iter().any(|v| {
                            v.as_str().is_none_or(|s| {
                                !s.starts_with("data:image/") && !s.starts_with("https://")
                            })
                        }))
                {
                    return Err(AgentError::code(400, "RUNTIME_IMAGE_INPUT_INVALID"));
                }
                if !text.is_empty() || !images.is_empty() {
                    let content = if images.is_empty() {
                        json!(text)
                    } else {
                        let mut c = vec![];
                        if !text.is_empty() {
                            c.push(json!({"type":"text","text":text}));
                        }
                        c.extend(
                            images
                                .iter()
                                .map(|url| json!({"type":"image_url","image_url":{"url":url}})),
                        );
                        json!(c)
                    };
                    messages.push(
                        json!({"role":if role=="developer"{"system"}else{role},"content":content}),
                    );
                }
            }
            "function_call" => {
                let id = text(item, "call_id");
                let name = text(item, "name");
                pending.insert(id.into(), name.into());
                let tool = json!({"id":id,"type":"function","function":{"name":name,"arguments":item["arguments"]}});
                if let Some(last) = messages.last_mut().filter(|v| v["tool_calls"].is_array()) {
                    last["tool_calls"].as_array_mut().expect("array").push(tool);
                } else {
                    messages.push(json!({"role":"assistant","content":"","tool_calls":[tool]}));
                }
            }
            "function_call_output" => {
                let id = text(item, "call_id");
                let name = pending
                    .remove(id)
                    .ok_or_else(|| AgentError::code(422, "RUNTIME_TOOL_TRANSCRIPT_INVALID"))?;
                messages.push(json!({"role":"tool","tool_call_id":id,"name":name,"content":output_text(&item["output"])}));
            }
            _ => {}
        }
    }
    if !pending.is_empty() {
        return Err(AgentError::code(422, "RUNTIME_TOOL_TRANSCRIPT_INVALID"));
    }
    if messages.is_empty() {
        return Err(AgentError::code(400, "RUNTIME_RESPONSES_INPUT_INVALID"));
    }
    Ok(json!(messages))
}
pub fn provider_body(
    body: &Value,
    call: &AuthorizedCall,
    native: bool,
) -> Result<
    (
        Value,
        BTreeMap<String, (String, String)>,
        BTreeSet<(Option<String>, String)>,
        bool,
    ),
    AgentError,
> {
    // Cross-protocol cache markers have provider-specific semantics. Refuse
    // translation when this adapter cannot preserve them instead of dropping.
    if !native && has_cache_control(body) {
        return Err(AgentError::code(
            422,
            "RUNTIME_CACHE_CONTROL_TRANSLATION_UNSUPPORTED",
        ));
    }
    let profile = &call.snapshot["capabilities"];
    let invalid = || AgentError::code(422, "RUNTIME_TOOL_SCHEMA_INVALID");
    let raw = body
        .get("tools")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or(json!([]));
    let raw = raw
        .as_array()
        .filter(|a| a.len() <= 256)
        .ok_or_else(invalid)?;
    let mut tools = Vec::new();
    let mut aliases = BTreeMap::new();
    let mut wire_names = BTreeSet::new();
    let mut reverse = BTreeMap::new();
    let mut candidates: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for item in raw {
        if item["type"] == "web_search" {
            if native && profile["tools"]["web_search_wire"] == "native" {
                tools.push(json!({"type":"web_search"}));
            } else if native {
                return Err(AgentError::code(422, "RUNTIME_TOOL_SCHEMA_UNSUPPORTED"));
            }
            continue;
        }
        let pairs: Vec<(Option<&str>, &Value)> = if item["type"] == "namespace" {
            if native && profile["tools"]["namespace_wire"] != "flatten" {
                return Err(AgentError::code(422, "RUNTIME_TOOL_SCHEMA_UNSUPPORTED"));
            }
            let ns = item["name"]
                .as_str()
                .filter(|s| tool_name(s))
                .ok_or_else(invalid)?;
            item["tools"]
                .as_array()
                .filter(|a| a.len() <= 256)
                .ok_or_else(invalid)?
                .iter()
                .map(|v| (Some(ns), v))
                .collect()
        } else {
            vec![(None, item)]
        };
        for (ns, item) in pairs {
            if item["type"] != "function" {
                return Err(AgentError::code(422, "RUNTIME_TOOL_SCHEMA_UNSUPPORTED"));
            }
            let name = item["name"]
                .as_str()
                .filter(|s| tool_name(s))
                .ok_or_else(invalid)?;
            let description = item["description"]
                .as_str()
                .filter(|s| s.chars().count() <= 16384)
                .ok_or_else(invalid)?;
            if !item["parameters"].is_object() {
                return Err(invalid());
            }
            let alias = if let Some(ns) = ns {
                let hash = spec::hex(&Sha256::digest(format!("{ns}\0{name}").as_bytes()));
                let prefix = format!("ns_{}_", &hash[..10]);
                format!("{prefix}{}", &name[..name.len().min(64 - prefix.len())])
            } else {
                name.into()
            };
            if !wire_names.insert(alias.clone()) {
                return Err(invalid());
            }
            if let Some(ns) = ns {
                aliases.insert(alias.clone(), (ns.into(), name.into()));
                reverse.insert((ns.to_owned(), name.to_owned()), alias.clone());
                candidates
                    .entry(name.into())
                    .or_default()
                    .push((ns.into(), name.into()));
            }
            let mut tool = json!({"type":"function","name":alias,"description":if let Some(ns)=ns{format!("[{ns}] {description}")}else{description.into()},"parameters":item["parameters"]});
            if item["strict"].is_boolean() {
                tool["strict"] = item["strict"].clone();
            }
            tools.push(tool);
        }
    }
    if tools.len() > 256 {
        return Err(invalid());
    }
    for (name, ids) in candidates {
        if ids.len() == 1 && !wire_names.contains(&name) {
            aliases.insert(name, ids[0].clone());
        }
    }
    let mut input = body["input"].clone();
    if let Some(a) = input.as_array_mut() {
        if a.is_empty() {
            return Err(AgentError::code(400, "RUNTIME_RESPONSES_INPUT_INVALID"));
        }
        for i in a {
            if !i.is_object() {
                return Err(AgentError::code(400, "RUNTIME_RESPONSES_INPUT_INVALID"));
            }
            if matches!(text(i, "type"), "additional_tools" | "configuration_update") {
                return Err(AgentError::code(422, "RUNTIME_INPUT_CONTROL_UNSUPPORTED"));
            }
            if let Some(content) = i["content"].as_array_mut() {
                for part in content {
                    if part["type"] == "encrypted_content" && part["encrypted_content"].is_string()
                    {
                        *part = json!({"type":"input_text","text":part["encrypted_content"]});
                    }
                }
            }
            if i["type"] == "agent_message" {
                *i = json!({"type":"message","role":"user","content":i["content"]});
            }
            if matches!(text(i, "type"), "function_call" | "function_call_output") {
                let identity =
                    if let (Some(ns), Some(name)) = (i["namespace"].as_str(), i["name"].as_str()) {
                        Some((ns.into(), name.into()))
                    } else {
                        aliases.get(text(i, "name")).cloned()
                    };
                if let Some(alias) = identity.as_ref().and_then(|id| reverse.get(id)) {
                    i["name"] = alias.clone().into();
                    i.as_object_mut().expect("object").remove("namespace");
                }
            }
        }
    } else if input.as_str().is_none_or(str::is_empty) {
        return Err(AgentError::code(400, "RUNTIME_RESPONSES_INPUT_INVALID"));
    }
    if let Err(error) =
        validate_transcript(&input, &wire_names, &aliases.values().cloned().collect())
    {
        // Source native Responses first permits completed kernel/rejection
        // receipts without advertised tools. Invalid receipts in that phase
        // retain its tools-disabled error rather than granting transcript use.
        if native && wire_names.is_empty() {
            return Err(AgentError::code(422, "RUNTIME_TOOLS_NOT_ENABLED_FOR_PHASE"));
        }
        return Err(error);
    }
    let has_transcript = input.as_array().is_some_and(|a| {
        a.iter()
            .any(|v| matches!(text(v, "type"), "function_call" | "function_call_output"))
    });
    let readonly = &call.snapshot["readonly_capabilities"];
    let names = readonly["responses_tool_names"].as_array();
    let mut choice = readonly
        .get("responses_tool_choice")
        .cloned()
        .unwrap_or(json!("auto"));
    if has_transcript && choice != "none" {
        choice = json!("auto");
    }
    let parallel = readonly
        .get("responses_parallel_tool_calls")
        .cloned()
        .unwrap_or(json!(true));
    if !parallel.is_boolean() {
        return Err(AgentError::code(503, "RUNTIME_MODEL_SNAPSHOT_INVALID"));
    }
    let native_search = native
        && readonly["responses_tool_names"].is_null()
        && choice != "none"
        && readonly["items"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v["source"] == "web-search"))
        && profile["native_search"]["enabled"] == true
        && profile["tools"]["web_search_wire"] == "native";
    tools.retain(|tool| {
        if tool["type"] == "web_search" {
            native_search
        } else {
            names.is_none_or(|a| {
                let name = aliases
                    .get(text(tool, "name"))
                    .map(|(_, n)| n.as_str())
                    .unwrap_or(text(tool, "name"));
                a.iter().any(|v| v == name)
            })
        }
    });
    if native_search && !tools.iter().any(|v| v["type"] == "web_search") {
        tools.push(json!({"type":"web_search"}));
    }
    if let Some(policy) = call.snapshot.get("tool_policy") {
        let allowed = policy["allowedTools"]
            .as_array()
            .ok_or_else(|| AgentError::code(503, "RUNTIME_MODEL_SNAPSHOT_INVALID"))?;
        tools.retain(|t| {
            let name = if t["type"] == "web_search" {
                "web_search"
            } else {
                text(t, "name")
            };
            let identity = aliases
                .get(name)
                .cloned()
                .map(|(ns, n)| (Some(ns), n))
                .unwrap_or((None, name.into()));
            allowed.iter().any(|v| {
                let ns = v["namespace"]
                    .as_str()
                    .filter(|s| !s.is_empty() && *s != "functions")
                    .map(str::to_owned);
                ns == identity.0 && text(v, "name") == identity.1
            })
        });
    }
    if choice == "none" {
        tools.clear();
    }
    if choice.is_object() {
        let name = text(&choice, "name").to_owned();
        if let Some(namespace) = choice.get("namespace").filter(|value| !value.is_null()) {
            let namespace = namespace
                .as_str()
                .ok_or_else(|| AgentError::code(422, "RUNTIME_TOOL_CHOICE_INVALID"))?;
            if !namespace.is_empty() && namespace != "functions" {
                let alias = reverse
                    .get(&(namespace.to_owned(), name.clone()))
                    .ok_or_else(|| AgentError::code(422, "RUNTIME_TOOL_CHOICE_INVALID"))?;
                choice["name"] = alias.clone().into();
            }
            choice
                .as_object_mut()
                .expect("function choice")
                .remove("namespace");
        } else if let Some(alias) = aliases.get(&name).and_then(|id| reverse.get(id)) {
            // The legacy bare name is accepted only when unambiguous.
            choice["name"] = alias.clone().into();
        }
        if !tools.iter().any(|v| v["name"] == choice["name"]) {
            return Err(AgentError::code(422, "RUNTIME_TOOL_CHOICE_INVALID"));
        }
    } else if choice == "required" && tools.is_empty() {
        return Err(AgentError::code(422, "RUNTIME_TOOL_CHOICE_INVALID"));
    }
    let allowed: BTreeSet<_> = tools
        .iter()
        .filter(|t| t["type"] == "function")
        .map(|t| {
            aliases
                .get(text(t, "name"))
                .cloned()
                .map(|(ns, n)| (Some(ns), n))
                .unwrap_or((None, text(t, "name").into()))
        })
        .collect();
    let mut result = if native {
        json!({"model":call.model_id,"input":input,"stream":true,"store":false,"max_output_tokens":call.reserved_output_tokens})
    } else {
        json!({"model":call.model_id,"messages":responses_to_messages(&json!({"input":input,"instructions":body["instructions"]}))?,"stream":true,"stream_options":{"include_usage":true},"max_tokens":call.reserved_output_tokens})
    };
    // Public OpenAI cache controls are valid on both supported wires and retain
    // their original JSON values. Nested input cache_control remains untouched.
    for key in ["prompt_cache_key", "prompt_cache_retention"] {
        if let Some(value) = body.get(key) {
            result[key] = value.clone();
        }
    }
    if native {
        if let Some(instructions) = body["instructions"].as_str().filter(|s| !s.is_empty()) {
            result["instructions"] = instructions.into();
        }
    }
    if !tools.is_empty() {
        result["tools"] = if native {
            json!(tools)
        } else {
            json!(
                tools
                    .iter()
                    .filter(|t| t["type"] == "function")
                    .map(|t| {
                        let mut f = t.clone();
                        f.as_object_mut().expect("object").remove("type");
                        json!({"type":"function","function":f})
                    })
                    .collect::<Vec<_>>()
            )
        };
        result["tool_choice"] = if !native && choice.is_object() {
            json!({"type":"function","function":{"name":choice["name"]}})
        } else {
            choice
        };
        result["parallel_tool_calls"] = parallel;
    }
    for key in ["temperature", "top_p"] {
        let v = if key == "temperature" {
            call.snapshot["parameters"].get(key).unwrap_or(&body[key])
        } else {
            &body[key]
        };
        if v.is_number() {
            result[key] = v.clone();
        }
    }
    apply_reasoning_wire(
        &mut result,
        profile,
        text(&call.snapshot["reasoning"], "effective_option"),
    )?;
    Ok((result, aliases, allowed, native_search))
}
pub fn apply_reasoning_wire(
    body: &mut Value,
    profile: &Value,
    option: &str,
) -> Result<(), AgentError> {
    let resolved = super::runtime_contract::reasoning(profile, Some(option))?;
    let settings = &resolved["settings"];
    let native = body.get("input").is_some();
    let mut set = |key: &str, value: Value| -> Result<(), AgentError> {
        if body.get(key).is_some() {
            return Err(AgentError::code(422, "RUNTIME_REASONING_WIRE_INVALID"));
        }
        body[key] = value;
        Ok(())
    };
    match text(&resolved, "adapter_id") {
        "reasoning/none-v1" => {}
        "reasoning/dashscope-thinking-v1" => {
            set("enable_thinking", settings["enabled"].clone())?;
            if settings["enabled"] == true && !settings["budget_tokens"].is_null() {
                set("thinking_budget", settings["budget_tokens"].clone())?;
            }
        }
        "reasoning/dashscope-responses-effort-v1" => {
            if native {
                set("reasoning", json!({"effort":settings["effort"]}))?;
            } else {
                set("enable_thinking", settings["chat_enabled"].clone())?;
                if settings["chat_enabled"] == true && !settings["chat_budget_tokens"].is_null() {
                    set("thinking_budget", settings["chat_budget_tokens"].clone())?;
                }
            }
        }
        "reasoning/deepseek-thinking-effort-v1" => {
            let enabled = settings["enabled"] == true;
            set(
                "thinking",
                json!({"type":if enabled{"enabled"}else{"disabled"}}),
            )?;
            if enabled && !settings["effort"].is_null() {
                set("reasoning_effort", settings["effort"].clone())?;
            }
            if enabled {
                body.as_object_mut().expect("body").remove("temperature");
                body.as_object_mut().expect("body").remove("top_p");
            }
        }
        "reasoning/anthropic-adaptive-v1" => {
            let enabled = settings["enabled"] == true;
            set(
                "thinking",
                json!({"type":if enabled{"adaptive"}else{"disabled"}}),
            )?;
            if enabled && !settings["effort"].is_null() {
                set("output_config", json!({"effort":settings["effort"]}))?;
            }
            if enabled {
                body.as_object_mut().expect("body").remove("temperature");
            }
        }
        "reasoning/anthropic-budget-v1" => {
            let enabled = settings["enabled"] == true;
            set(
                "thinking",
                if enabled {
                    json!({"type":"enabled","budget_tokens":settings["budget_tokens"]})
                } else {
                    json!({"type":"disabled"})
                },
            )?;
            if enabled {
                body.as_object_mut().expect("body").remove("temperature");
            }
        }
        "reasoning/openai-responses-effort-v1" => {
            if native {
                set(
                    "reasoning",
                    json!({"effort":settings["effort"],"summary":"auto"}),
                )?;
            } else {
                set("reasoning_effort", settings["effort"].clone())?;
            }
        }
        "reasoning/xai-effort-v1" => {
            if native {
                set("reasoning", json!({"effort":settings["effort"]}))?;
            } else {
                set("reasoning_effort", settings["effort"].clone())?;
            }
        }
        "reasoning/openai-compatible-binary-v1" => {
            set("enable_thinking", settings["enabled"].clone())?;
        }
        "reasoning/gemini-thinking-v1" => {
            let mut thinking = json!({});
            if let Some(s) = settings["level"].as_str() {
                thinking["thinkingLevel"] = s.to_uppercase().into();
            }
            for (a, b) in [
                ("budget_tokens", "thinkingBudget"),
                ("include_thoughts", "includeThoughts"),
            ] {
                if !settings[a].is_null() {
                    thinking[b] = settings[a].clone();
                }
            }
            if thinking.as_object().is_some_and(|o| !o.is_empty()) {
                set("generationConfig", json!({"thinkingConfig":thinking}))?;
            }
        }
        _ => return Err(AgentError::code(422, "RUNTIME_REASONING_WIRE_INVALID")),
    }
    Ok(())
}
pub fn router(plane: Arc<AgentModelPlane>) -> Router {
    Router::new()
        .route("/internal/v1/agent-model-plane/responses", post(responses))
        .with_state(plane)
}
fn error_response(e: AgentError) -> Response {
    (StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),Json(json!({"error":{"type":"runtime_model_plane_error","code":e.detail["code"],"message":"The private model request was rejected."}}))).into_response()
}
fn require_model_transport(
    headers: &HeaderMap,
    legacy: &str,
    peer: bool,
    required: bool,
) -> Result<(), AgentError> {
    let denied = || AgentError::code(401, "RUNTIME_MODEL_PLANE_UNAUTHORIZED");
    if required {
        if !peer
            || !legacy.is_empty()
            || headers.contains_key("authorization")
            || headers.contains_key("x-ai-platform-internal-token")
        {
            return Err(denied());
        }
        return Ok(());
    }
    if peer {
        return Ok(());
    }
    if headers.get_all("authorization").iter().count() != 1 {
        return Err(denied());
    }
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let mut expected =
        Hmac::<Sha256>::new_from_slice(b"agent-model-plane-token-comparison").expect("HMAC key");
    expected.update(legacy.as_bytes());
    let mut actual =
        Hmac::<Sha256>::new_from_slice(b"agent-model-plane-token-comparison").expect("HMAC key");
    actual.update(token.as_bytes());
    if token.is_empty()
        || legacy.is_empty()
        || actual
            .verify_slice(&expected.finalize().into_bytes())
            .is_err()
    {
        return Err(denied());
    }
    Ok(())
}
async fn responses(State(plane): State<Arc<AgentModelPlane>>, request: Request) -> Response {
    let result = async {
        let peer = crate::workload_tls::mutual_tls(request.extensions());
        let headers = request.headers();
        require_model_transport(
            headers,
            &plane.internal_token,
            peer,
            super::control::execution_transport::required(),
        )?;
        let primary = headers
            .get("x-agent-turn-metadata")
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty());
        let alternatives: Vec<_> = headers
            .iter()
            .filter(|(k, _)| k.as_str().ends_with("-turn-metadata"))
            .filter_map(|(_, v)| v.to_str().ok())
            .collect();
        let raw = primary
            .or_else(|| {
                if alternatives.len() == 1 {
                    Some(alternatives[0])
                } else {
                    None
                }
            })
            .unwrap_or("");
        let metadata: Value = serde_json::from_str(raw)
            .map_err(|_| AgentError::code(401, "RUNTIME_TURN_METADATA_INVALID"))?;
        if !metadata.is_object() {
            return Err(AgentError::code(401, "RUNTIME_TURN_METADATA_INVALID"));
        }
        let bytes = to_bytes(request.into_body(), 16 * 1024 * 1024)
            .await
            .map_err(|_| AgentError::code(400, "RUNTIME_MODEL_REQUEST_INVALID"))?;
        let body: Value = serde_json::from_slice(&bytes)
            .map_err(|_| AgentError::code(400, "RUNTIME_MODEL_REQUEST_INVALID"))?;
        if !body.is_object() || body["stream"] != true {
            return Err(AgentError::code(400, "RUNTIME_MODEL_REQUEST_INVALID"));
        }
        let call = plane.authorize_and_reserve(&body, &metadata).await?;
        let stream = plane.stream(body, call).await?;
        Ok::<_, AgentError>(
            Response::builder()
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .header("x-accel-buffering", "no")
                .body(Body::from_stream(stream))
                .expect("SSE response"),
        )
    }
    .await;
    match result {
        Ok(response) => response,
        Err(error) => error_response(error),
    }
}

/// Explicit v2 only: the immutable snapshot owns all component prices. Unknown
/// usage stays in the receipt; malformed counts or missing positive-cache terms
/// fail before completion and never become a zero-cost success.
fn cache_settlement(
    call: &AuthorizedCall,
    input: i64,
    output: i64,
    usage: &Value,
) -> Result<(i64, Value), AgentError> {
    use ai_platform_gateway_foundation::{
        usage_parser::cache::CacheUsage,
        usage_recorder::{UsageRecord, prepare_with_snapshot},
    };
    let mut record = UsageRecord::new(
        &call.scope.tenant_id,
        &call.scope.user_id,
        &call.model_id,
        chrono::Utc::now(),
    );
    record.provider = call.provider_id.clone();
    record.request_id = call.call_id.clone();
    record.input_tokens = input;
    record.output_tokens = output;
    record.metadata.insert(
        "provider_cache_usage".into(),
        serde_json::to_value(CacheUsage::observe(&call.provider_id, usage))
            .map_err(|_| AgentError::code(502, "RUNTIME_CACHE_USAGE_INVALID"))?,
    );
    prepare_with_snapshot(
        &mut record,
        cache_pricing(&call.snapshot)
            .ok_or_else(|| AgentError::code(503, "RUNTIME_CACHE_PRICING_INVALID"))?,
    )
    .map_err(|_| AgentError::code(502, "RUNTIME_CACHE_SETTLEMENT_INVALID"))?;
    // Frozen UsageRecorder storage uses 1,000,000 units/USD: these
    // historically named "microcents" are already microUSD. A second /100
    // conversion would undercharge v2 Runtime calls by up to two orders.
    // Preserve the recorder's component half-even rounding, then sum exactly.
    let microusd = record
        .input_cost_cents
        .checked_add(record.output_cost_cents)
        .ok_or_else(|| AgentError::code(502, "RUNTIME_CACHE_COST_OVERFLOW"))?;
    Ok((
        microusd,
        json!({"schema":"agentforge.runtime-cache-settlement/v2","tenant_id":call.scope.tenant_id,"provider":call.provider_id,"model":call.model_id,"input_tokens":input,"output_tokens":output,"cost_microusd":microusd,"metadata":record.metadata}),
    ))
}

fn cache_pricing(snapshot: &Value) -> Option<&Value> {
    let pricing = &snapshot["pricing"];
    if pricing.get("price_book_schema").is_some() {
        Some(pricing)
    } else if pricing["snapshot"].get("price_book_schema").is_some() {
        Some(&pricing["snapshot"])
    } else {
        None
    }
}

fn has_cache_control(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.contains_key("cache_control") || map.values().any(has_cache_control)
        }
        Value::Array(values) => values.iter().any(has_cache_control),
        _ => false,
    }
}
#[cfg(test)]
mod cache_contract_tests {
    use super::*;
    #[test]
    fn cache_settlement_uses_actual_legacy_microdollars_without_rescaling() {
        use ai_platform_gateway_foundation::price_book::{
            Component, EffectiveRange, PriceBook, PriceEntry, PriceKey, Unit, token_snapshot,
        };
        // The fixture is shared in meaning with the foundation settlement test:
        // input=$.10/1K, cached=$.01/1K, output=$.20/1K. 20/80/10 tokens
        // cost $.0048 = 4800 microUSD, not ceil(4800/100)=48.
        // Raising input to $.10005 makes ordinary input cost $.002001;
        // total 4801 microUSD must remain 4801, not ceil(4801/100)=49.
        for (input_rate, expected) in [(100_000_000, 4800), (100_050_000, 4801)] {
            let entry = |component, rate_nanos| PriceEntry {
                key: PriceKey {
                    book_version: "book".into(),
                    provider: "p".into(),
                    model: "m".into(),
                    model_version: "revision".into(),
                    component,
                    currency: "USD".into(),
                },
                tenant: None,
                effective: EffectiveRange {
                    from_ms: 0,
                    to_ms: None,
                },
                unit: Unit::Tokens,
                units_per_rate: 1000,
                rate_nanos,
            };
            let input = entry(Component::Input, input_rate);
            let output = entry(Component::Output, 200_000_000);
            let cached = entry(Component::CachedInput, 10_000_000);
            let book = PriceBook::new(vec![input.clone(), output.clone(), cached.clone()], vec![])
                .unwrap();
            let mut pricing = token_snapshot(
                &book.resolve("tenant", &input.key, 1).unwrap(),
                &book.resolve("tenant", &output.key, 1).unwrap(),
            )
            .unwrap();
            pricing["price_book_cached_input"] =
                serde_json::to_value(book.resolve("tenant", &cached.key, 1).unwrap()).unwrap();
            let call = AuthorizedCall {
                call_id: "call".into(),
                lease_id: "lease".into(),
                run_id: "run".into(),
                scope: Scope {
                    tenant_id: "tenant".into(),
                    user_id: "user".into(),
                    session_id: "session".into(),
                },
                model_id: "m".into(),
                provider_id: "p".into(),
                provider_revision: "revision".into(),
                snapshot: json!({"pricing":{"snapshot":pricing}}),
                estimated_input_tokens: 100,
                reserved_output_tokens: 10,
            };
            let (cost,receipt)=cache_settlement(&call,100,10,&json!({"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":80}})).unwrap();
            assert_eq!(cost, expected);
            assert_eq!(receipt["cost_microusd"], expected);
            assert_eq!(
                receipt["metadata"]["cache_pricing_state"],
                "reported_components"
            );
        }
    }
    #[test]
    fn cache_pricing_requires_explicit_version_and_keeps_legacy() {
        assert!(cache_pricing(&json!({"pricing":{"input_price_per_1k":1}})).is_none());
        assert!(
            cache_pricing(
                &json!({"pricing":{"snapshot":{"price_book_schema":"agentforge.price_book/v2"}}})
            )
            .is_some()
        );
        assert!(has_cache_control(
            &json!({"input":[{"content":[{"text":"x","cache_control":{"type":"ephemeral"}}]}]})
        ));
        assert!(!has_cache_control(
            &json!({"prompt_cache_key":"public-key"})
        ));
    }
}

#[cfg(test)]
mod transport_auth_tests {
    use super::*;
    #[test]
    fn required_model_transport_requires_verified_peer_and_no_retired_bearer() {
        let mut headers = HeaderMap::new();
        assert!(require_model_transport(&headers, "", false, true).is_err());
        headers.insert(
            "x-spiffe-id",
            "spiffe://agentforge.local/service/agent-runtime"
                .parse()
                .unwrap(),
        );
        assert!(require_model_transport(&headers, "", false, true).is_err());
        assert!(require_model_transport(&headers, "", true, true).is_ok());
        assert!(require_model_transport(&headers, "retained-secret", true, true).is_err());
        for name in ["authorization", "x-ai-platform-internal-token"] {
            let mut headers = headers.clone();
            headers.insert(name, "Bearer retained-secret".parse().unwrap());
            assert!(require_model_transport(&headers, "", true, true).is_err());
        }
    }
    #[test]
    fn legacy_model_transport_preserves_exact_single_bearer_validation() {
        let mut headers = HeaderMap::new();
        assert!(require_model_transport(&headers, "legacy-secret", false, false).is_err());
        headers.insert("authorization", "Bearer legacy-secret".parse().unwrap());
        assert!(require_model_transport(&headers, "legacy-secret", false, false).is_ok());
        assert!(require_model_transport(&headers, "wrong-secret", false, false).is_err());
        headers.append("authorization", "Bearer legacy-secret".parse().unwrap());
        assert!(require_model_transport(&headers, "legacy-secret", false, false).is_err());
        assert!(require_model_transport(&headers, "legacy-secret", true, false).is_ok());
    }
}
