//! Gateway control plane. Only issues/resumes/interrupts the existing Runtime;
//! model or tool selection is never performed in this module.
use super::instructions::{CORE_ASSISTANT_PROMPT, GENERIC_AGENT_INSTRUCTIONS};
use super::{
    Actor, AgentError, AgentFuture, Scope, entry,
    model::LeaseSigner,
    runtime_contract as contract, spec, text,
    threads::{AgentThreadStore, RuntimeThread},
};
use crate::migration_host::context;
use crate::{
    capacity::{CapacityRequest, CapacityResolver},
    capacity_admission::{AdmissionRequest, CapacityAdmissionController, CapacityLease},
};
use axum::http::{HeaderMap, HeaderName, HeaderValue};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::HashMap,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::sync::{Mutex, mpsc};

// Runtime emits both successful tombstones and idempotent absence as HTTP 200.
// Unknown statuses are not evidence of absence and must block Gateway deletion.
fn session_cleanup_result(session_id: &str, payload: &Value) -> Result<bool, AgentError> {
    if payload.get("session_id").and_then(Value::as_str) == Some(session_id) {
        match payload.get("status").and_then(Value::as_str) {
            Some("deleted") => return Ok(true),
            Some("not_found") => return Ok(false),
            _ => {}
        }
    }
    Err(AgentError::code(
        503,
        "AI_PLATFORM_AGENT_RUNTIME_SESSION_CLEANUP_INVALID",
    ))
}

fn native_search_selection(readonly: &Value, profile: &Value) -> Result<bool, AgentError> {
    let requested = readonly["items"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item["source"] == "web-search"));
    let supported = profile["native_search"]["enabled"] == true
        && profile["tools"]["web_search_wire"] == "native";
    if requested && !supported {
        return Err(AgentError::code(
            422,
            "AI_PLATFORM_AGENT_RUNTIME_NATIVE_SEARCH_UNAVAILABLE",
        ));
    }
    Ok(requested
        && readonly["responses_tool_choice"] != "none"
        && readonly["responses_tool_names"].is_null()
        && supported)
}

pub trait RuntimeModels: Send + Sync {
    fn model<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: Option<&'a str>,
    ) -> AgentFuture<'a, Value>;
    /// Read-only diagnostics need to distinguish a missing model from a failed
    /// lookup. Runtime admission continues to require `model` and fails closed.
    fn model_optional<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: Option<&'a str>,
    ) -> AgentFuture<'a, Option<Value>> {
        Box::pin(async move { self.model(tenant, model, provider).await.map(Some) })
    }
}
impl<D, P, C, O> RuntimeModels for crate::model_service::ModelService<D, P, C, O>
where
    D: crate::model_service::ModelDatabase,
    P: crate::model_service::ModelPricingCache,
    C: crate::model_service::ModelClock,
    O: crate::model_service::ModelObserver,
{
    fn model<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: Option<&'a str>,
    ) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            self.model_optional(tenant, model, provider)
                .await?
                .ok_or_else(|| AgentError::code(503, "AGENT_RUNTIME_MODEL_UNAVAILABLE"))
        })
    }
    fn model_optional<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: Option<&'a str>,
    ) -> AgentFuture<'a, Option<Value>> {
        Box::pin(async move {
            self.get_model(tenant, model, provider)
                .await
                .map(|row| row.map(Value::Object))
                .map_err(|e| {
                    let ambiguous = e == crate::model_service::ModelServiceError::ProviderAmbiguous;
                    AgentError::code(
                        if ambiguous { 409 } else { 503 },
                        if ambiguous {
                            "MODEL_PROVIDER_REQUIRED"
                        } else {
                            "AGENT_RUNTIME_MODEL_UNAVAILABLE"
                        },
                    )
                })
        })
    }
}
pub trait RuntimeProviders: Send + Sync {
    fn provider<'a>(&'a self, tenant: &'a str, id: &'a str) -> AgentFuture<'a, Value>;
    fn runtime_config<'a>(&'a self, tenant: &'a str, id: &'a str) -> AgentFuture<'a, Value>;
}
impl<D, C, H, G, E, T> RuntimeProviders
    for crate::provider_service::ProviderService<D, C, H, G, E, T>
where
    D: crate::provider_service::ProviderDatabase,
    C: crate::provider_service::ProviderCipher,
    H: crate::provider_service::ProviderHttp,
    G: crate::provider_service::ProviderGoogleAuth,
    E: crate::provider_service::ProviderEnvironment,
    T: crate::provider_service::ProviderClock,
{
    fn provider<'a>(&'a self, tenant: &'a str, id: &'a str) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            self.get_provider(tenant, id)
                .await
                .map_err(|_| {
                    AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_PROVIDER_UNAVAILABLE")
                })?
                .ok_or_else(|| {
                    AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_PROVIDER_UNAVAILABLE")
                })
        })
    }
    fn runtime_config<'a>(&'a self, tenant: &'a str, id: &'a str) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            self.get_runtime_provider_config(tenant, id)
                .await
                .map(|v| v.into_internal_value())
                .map_err(|_| AgentError::code(503, "AGENT_RUNTIME_PROVIDER_UNAVAILABLE"))
        })
    }
}
pub trait RuntimeCatalog: Send + Sync {
    fn fetch<'a>(
        &'a self,
        scope: &'a Scope,
        model: &'a str,
        revision: i64,
        allowlist: Option<&'a [Value]>,
    ) -> AgentFuture<'a, Value>;
}
pub trait RuntimeMemory: Send + Sync {
    fn context<'a>(&'a self, scope: &'a Scope, limit: i64) -> AgentFuture<'a, Value>;
}
#[derive(Clone)]
pub struct RuntimeOptions {
    pub runtime_url: String,
    pub runtime_internal_token: String,
    pub model_plane_base_url: String,
    pub kernel_revision: String,
    pub lease_ttl_seconds: i64,
    pub max_model_calls: i64,
    pub max_cost_microusd: i64,
    pub worker_writes_enabled: bool,
}
#[derive(Clone)]
pub struct RuntimeControlPlane {
    pub pool: PgPool,
    pub threads: AgentThreadStore,
    pub models: Arc<dyn RuntimeModels>,
    pub providers: Arc<dyn RuntimeProviders>,
    pub catalog: Arc<dyn RuntimeCatalog>,
    pub memory: Option<Arc<dyn RuntimeMemory>>,
    pub sources: Arc<dyn entry::SourceAccess>,
    pub signer: Arc<LeaseSigner>,
    pub options: RuntimeOptions,
    pub client: reqwest::Client,
    events: super::event_transport::EventTransport,
    pub admission: Option<Arc<CapacityAdmissionController>>,
    pub capacity: Option<Arc<CapacityResolver>>,
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    run_leases: Arc<Mutex<HashMap<String, Arc<CapacityLease>>>>,
}
#[derive(Clone, Default)]
pub struct TurnRequest {
    pub actor: Actor,
    pub session_id: String,
    pub message: String,
    pub model_id: String,
    pub provider_id: Option<String>,
    pub expected_model_ref: Option<Value>,
    pub reasoning_option: Option<String>,
    pub legacy_thinking_level: Option<String>,
    pub max_tokens: Option<i64>,
    pub temperature: Option<f64>,
    pub developer_instructions: Option<String>,
    pub style_guidance: Option<String>,
    pub readonly_capabilities: Value,
    pub memory_mode: String,
    pub memory_profile: Option<String>,
    pub enable_dynamic_tools: bool,
    pub image_inputs: Vec<String>,
    pub resolved_snapshot: Option<Value>,
    /// Server-resolved built-in Local Node grant binding. Never populated from
    /// the model-authored tool arguments or a pinned/preview AgentSpec.
    pub local_node_capability: Option<Value>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct AgentTurn {
    pub runtime_thread_id: String,
    pub run_id: String,
    pub snapshot_id: String,
    pub lease_id: String,
    pub after_sequence: i64,
    pub requested_reasoning_option: String,
    pub effective_reasoning_option: String,
    pub reasoning_adapter_id: String,
    pub capability_revision: i64,
    pub fallback_reason: Option<String>,
}

fn failed_thread_create(
    thread: Option<RuntimeThread>,
    fingerprint: &str,
) -> Result<RuntimeThread, AgentError> {
    let Some(thread) = thread else {
        return Err(AgentError::code(
            503,
            "AI_PLATFORM_AGENT_RUNTIME_THREAD_CREATE_FAILED",
        ));
    };
    match thread
        .dynamic_tool_fingerprint
        .as_deref()
        .filter(|stored| !stored.is_empty())
    {
        Some(stored) if stored == fingerprint => Ok(thread),
        Some(_) => Err(AgentError::code(
            409,
            "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_THREAD_RECREATE_REQUIRED",
        )),
        // Runtime reserves the root row before asking the kernel to start it.
        // A failed start leaves that row unbound; it is not a prior conversation
        // whose tool configuration changed.
        None => Err(AgentError::code(
            503,
            "AI_PLATFORM_AGENT_RUNTIME_THREAD_CREATE_FAILED",
        )),
    }
}

fn catalog_descriptor_matches_allowlist(descriptor: &Value, allowlist: &Value) -> bool {
    ["version", "schema_hash"].iter().all(|field| {
        descriptor[*field] == allowlist[*field]
            || (matches!(text(allowlist, "type"), "platform" | "connector")
                && allowlist[*field].is_null())
    })
}

pub struct EventReceiver {
    receiver: mpsc::Receiver<Result<Value, AgentError>>,
    task: tokio::task::JoinHandle<()>,
}
impl EventReceiver {
    pub async fn recv(&mut self) -> Option<Result<Value, AgentError>> {
        self.receiver.recv().await
    }
}
impl Stream for EventReceiver {
    type Item = Result<Value, AgentError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}
impl Drop for EventReceiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod local_node_catalog_binding_tests {
    use super::*;

    #[test]
    fn connector_allowlist_null_version_and_hash_are_pinned_to_catalog_output() {
        let descriptor = json!({
            "version":"2",
            "schema_hash":format!("sha256:{}", "a".repeat(64))
        });
        let allowlist = json!({
            "type":"connector",
            "version":null,
            "schema_hash":null
        });
        assert!(catalog_descriptor_matches_allowlist(
            &descriptor,
            &allowlist
        ));

        let mut stale = allowlist.clone();
        stale["version"] = json!("1");
        assert!(!catalog_descriptor_matches_allowlist(&descriptor, &stale));

        let mut mismatched_schema = allowlist;
        mismatched_schema["schema_hash"] = json!(format!("sha256:{}", "b".repeat(64)));
        assert!(!catalog_descriptor_matches_allowlist(
            &descriptor,
            &mismatched_schema
        ));
    }
}

impl RuntimeControlPlane {
    pub fn new(
        pool: PgPool,
        models: Arc<dyn RuntimeModels>,
        providers: Arc<dyn RuntimeProviders>,
        catalog: Arc<dyn RuntimeCatalog>,
        sources: Arc<dyn entry::SourceAccess>,
        signer: Arc<LeaseSigner>,
        options: RuntimeOptions,
    ) -> Result<Self, AgentError> {
        if options.runtime_url.is_empty()
            || options.runtime_internal_token.is_empty()
            || options.model_plane_base_url.is_empty()
            || options.kernel_revision.is_empty()
        {
            return Err(AgentError::code(503, "AGENT_RUNTIME_UNAVAILABLE"));
        }
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .pool_max_idle_per_host(20)
            .build()
            .map_err(|_| AgentError::code(503, "AGENT_RUNTIME_UNAVAILABLE"))?;
        Ok(Self {
            threads: AgentThreadStore::new(pool.clone()),
            pool,
            models,
            providers,
            catalog,
            sources,
            signer,
            options,
            client,
            events: super::event_transport::EventTransport::new()?,
            memory: None,
            admission: None,
            capacity: None,
            locks: Arc::new(Mutex::new(HashMap::new())),
            run_leases: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    pub fn headers(&self, scope: &Scope, run: Option<&str>) -> Result<HeaderMap, AgentError> {
        let mut h = HeaderMap::new();
        for (k, v) in [
            (
                "x-ai-platform-internal-token",
                self.options.runtime_internal_token.as_str(),
            ),
            ("x-ai-tenant-id", scope.tenant_id.as_str()),
            ("x-ai-user-id", scope.user_id.as_str()),
            ("x-ai-session-id", scope.session_id.as_str()),
        ] {
            if !v.is_empty() {
                h.insert(
                    k,
                    HeaderValue::from_str(v)
                        .map_err(|_| AgentError::code(400, "AGENT_RUNTIME_SCOPE_INVALID"))?,
                );
            }
        }
        // Identity still comes only from configured service auth and the
        // admitted Scope. The existing helper adds only bounded correlation.
        let correlation = context::internal_http_headers(Default::default(), run, run, None)
            .map_err(|_| AgentError::code(400, "AGENT_RUNTIME_SCOPE_INVALID"))?;
        for (name, value) in correlation {
            h.insert(
                HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| AgentError::code(400, "AGENT_RUNTIME_SCOPE_INVALID"))?,
                HeaderValue::from_str(&value)
                    .map_err(|_| AgentError::code(400, "AGENT_RUNTIME_SCOPE_INVALID"))?,
            );
        }
        Ok(h)
    }
    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.options.runtime_url.trim_end_matches('/'), path)
    }
    async fn acquire(
        &self,
        scope: &Scope,
        kind: &str,
        provider: Option<&str>,
    ) -> Result<Option<Arc<CapacityLease>>, AgentError> {
        let (Some(admission), Some(resolver)) = (&self.admission, &self.capacity) else {
            return Ok(None);
        };
        let service_id = format!("agent-runtime-{kind}");
        let class = if matches!(kind, "provider" | "sse") {
            "stream"
        } else {
            "sync"
        };
        let budgets = resolver
            .resolve(&CapacityRequest {
                tenant_id: scope.tenant_id.clone(),
                service_id: service_id.clone(),
                request_class: class.into(),
                resource_kind: kind.into(),
                provider_id: json!(provider),
                upstream_group: Value::Null,
                is_admin_read: false,
                service_config: Value::Null,
            })
            .map_err(|_| AgentError::code(503, "CAPACITY_UNAVAILABLE"))?;
        let lease = admission
            .acquire(
                &budgets,
                &AdmissionRequest {
                    tenant_id: scope.tenant_id.clone(),
                    user_id: scope.user_id.clone(),
                    service_id,
                    request_class: class.into(),
                    request_id: uuid::Uuid::new_v4().to_string(),
                    ..Default::default()
                },
            )
            .await
            .map_err(|_| AgentError::code(429, "CAPACITY_REJECTED"))?;
        Ok(Some(Arc::new(lease)))
    }
    pub async fn ensure_thread(
        &self,
        scope: &Scope,
        model: &str,
        provider: Option<&str>,
        readonly: Option<Value>,
        allowlist: Option<Vec<Value>>,
        native_search: bool,
        instructions: Option<&str>,
    ) -> Result<RuntimeThread, AgentError> {
        if instructions.is_some_and(|s| s.len() > 256 * 1024) {
            return Err(AgentError::code(
                409,
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_INSTRUCTIONS_INVALID",
            ));
        }
        let mut selected = if let Some(r) = readonly {
            r
        } else {
            let m = self.models.model(&scope.tenant_id, model, provider).await?;
            let revision = m["capability_revision"].as_i64().unwrap_or(1);
            let mut r = contract::readonly_payload(&json!({}), scope, revision)?;
            self.bind_catalog(scope, model, revision, &mut r, allowlist.as_deref())
                .await?;
            r
        };
        let generic = allowlist.is_none() && selected["responses_tool_names"].is_null();
        let thread_readonly = selected
            .as_object_mut()
            .and_then(|o| o.remove("_thread_capabilities"))
            .unwrap_or_else(|| selected.clone());
        let fingerprint = contract::fingerprint(&thread_readonly);
        if let Some(t) = self.threads.get_for_session(scope).await? {
            return self.bind_fingerprint(scope, t, &fingerprint, false).await;
        }
        let key = format!(
            "{}\0{}\0{}",
            scope.tenant_id, scope.user_id, scope.session_id
        );
        let lock = {
            let mut locks = self.locks.lock().await;
            locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let guard = lock.lock().await;
        let result=async{if let Some(t)=self.threads.get_for_session(scope).await?{return self.bind_fingerprint(scope,t,&fingerprint,true).await;}let mut start=json!({"model":model,"modelProvider":"ai-platform-gateway","cwd":"/workspace","approvalPolicy":"on-request","sandbox":"read-only","config":contract::runtime_model_config(&self.options.model_plane_base_url,model,native_search),"dynamicTools":contract::dynamic_tools(&thread_readonly)?});if let Some(instructions)=instructions{start["baseInstructions"]=CORE_ASSISTANT_PROMPT.into();start["developerInstructions"]=if instructions.trim().is_empty(){GENERIC_AGENT_INSTRUCTIONS}else{instructions}.into();}
    let body=json!({"tenantId":scope.tenant_id,"userId":scope.user_id,"sessionId":scope.session_id,"start":start,"toolPolicy":contract::tool_policy(if generic{&thread_readonly}else{&selected},native_search||generic,true,generic)?});
    let response=self.client.post(self.endpoint("/internal/v1/threads")).headers(self.headers(scope,None)?).json(&body).send().await.map_err(|_|AgentError::code(503,"AI_PLATFORM_AGENT_RUNTIME_THREAD_CREATE_FAILED"))?;
    if !response.status().is_success() {
        return failed_thread_create(self.threads.get_for_session(scope).await?, &fingerprint);
    }
    let payload:Value=response.json().await.map_err(|_|AgentError::code(503,"AI_PLATFORM_AGENT_RUNTIME_THREAD_CREATE_INVALID"))?;let id=super::uuid(text(&payload["thread"],"id")).map_err(|_|AgentError::code(503,"AI_PLATFORM_AGENT_RUNTIME_THREAD_CREATE_INVALID"))?;
    let t=self.threads.get_for_session(scope).await?.unwrap_or(RuntimeThread{runtime_thread_id:id,tenant_id:scope.tenant_id.clone(),user_id:scope.user_id.clone(),session_id:scope.session_id.clone(),kernel_owner:"agent_runtime".into(),source_kind:"native".into(),import_status:"ready".into(),last_sequence:0,dynamic_tool_fingerprint:None});self.bind_fingerprint(scope,t,&fingerprint,true).await}.await;
        drop(guard);
        let mut locks = self.locks.lock().await;
        if Arc::strong_count(&lock) == 2 {
            locks.remove(&key);
        }
        result
    }
    async fn bind_fingerprint(
        &self,
        scope: &Scope,
        mut thread: RuntimeThread,
        fingerprint: &str,
        created: bool,
    ) -> Result<RuntimeThread, AgentError> {
        let error = || {
            AgentError::code(
                409,
                "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_THREAD_RECREATE_REQUIRED",
            )
        };
        if let Some(stored) = thread
            .dynamic_tool_fingerprint
            .as_deref()
            .filter(|s| !s.is_empty())
        {
            return if stored == fingerprint {
                Ok(thread)
            } else {
                Err(error())
            };
        }
        if !created {
            for _ in 0..5 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if let Some(t) = self.threads.get_for_session(scope).await? {
                    if let Some(stored) = t.dynamic_tool_fingerprint.as_deref() {
                        if !stored.is_empty() {
                            return if stored == fingerprint {
                                Ok(t)
                            } else {
                                Err(error())
                            };
                        }
                    }
                }
            }
            return Err(error());
        }
        for attempt in 0..3 {
            let result=sqlx::query("UPDATE assistant_runtime_threads SET dynamic_tool_fingerprint=$1,updated_at=NOW() WHERE runtime_thread_id=$2::uuid AND tenant_id=$3 AND user_id=$4 AND session_id=$5 AND dynamic_tool_fingerprint IS NULL AND deleted_at IS NULL").bind(fingerprint).bind(&thread.runtime_thread_id).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).execute(&self.pool).await;
            if result.as_ref().is_ok_and(|r| r.rows_affected() == 1) {
                thread.dynamic_tool_fingerprint = Some(fingerprint.into());
                return Ok(thread);
            }
            if let Some(t) = self.threads.get_for_session(scope).await? {
                if t.dynamic_tool_fingerprint.as_deref() == Some(fingerprint) {
                    return Ok(t);
                }
                if t.dynamic_tool_fingerprint.is_some() {
                    return Err(error());
                }
            }
            if attempt < 2 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        Err(error())
    }
    pub async fn resume_thread(
        &self,
        scope: &Scope,
        thread: &str,
        config: &Value,
    ) -> Result<(), AgentError> {
        let response = self
            .client
            .post(self.endpoint(&format!(
                "/internal/v1/threads/{}/resume",
                super::uuid(thread)?
            )))
            .headers(self.headers(scope, None)?)
            .json(config)
            .send()
            .await
            .map_err(|_| AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_THREAD_RESUME_FAILED"))?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let p: Value = response.json().await.unwrap_or(Value::Null);
            let code = if p["error"] == "runtime_turn_recovering" {
                "AI_PLATFORM_AGENT_RUNTIME_TURN_RECOVERING"
            } else if status == 409 {
                "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_THREAD_RECREATE_REQUIRED"
            } else {
                "AI_PLATFORM_AGENT_RUNTIME_THREAD_RESUME_FAILED"
            };
            return Err(AgentError::code(
                if status == 409 { 409 } else { 503 },
                code,
            ));
        }
        Ok(())
    }
    pub async fn verify_thread(
        &self,
        scope: &Scope,
        thread: &str,
        model: &str,
    ) -> Result<(), AgentError> {
        self.resume_thread(scope,thread,&json!({"model":model,"modelPlaneBaseUrl":self.options.model_plane_base_url,"baseInstructions":null,"developerInstructions":null,"modelContextWindow":null,"autoCompactTokenLimit":null})).await
    }
    pub async fn cleanup_session(&self, scope: &Scope) -> Result<bool, AgentError> {
        if scope.session_id.is_empty()
            || scope.session_id.len() > 255
            || scope.session_id.chars().any(char::is_control)
        {
            return Err(AgentError::code(
                400,
                "AI_PLATFORM_AGENT_RUNTIME_SESSION_ID_INVALID",
            ));
        }
        let mut url = reqwest::Url::parse(&self.endpoint("/internal/v1/sessions/"))
            .map_err(|_| AgentError::code(503, "AGENT_RUNTIME_UNAVAILABLE"))?;
        url.path_segments_mut()
            .map_err(|_| AgentError::code(503, "AGENT_RUNTIME_UNAVAILABLE"))?
            .pop_if_empty()
            .push(&scope.session_id)
            .push("cleanup");
        let response = self
            .client
            .post(url)
            .headers(self.headers(scope, None)?)
            .json(&json!({}))
            .send()
            .await
            .map_err(|_| {
                AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_SESSION_CLEANUP_FAILED")
            })?;
        if response.status().as_u16() == 404 {
            return Ok(false);
        }
        if !response.status().is_success() {
            return Err(AgentError::code(
                if response.status().is_server_error() {
                    503
                } else {
                    409
                },
                "AI_PLATFORM_AGENT_RUNTIME_SESSION_CLEANUP_FAILED",
            ));
        }
        let p: Value = response.json().await.map_err(|_| {
            AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_SESSION_CLEANUP_INVALID")
        })?;
        session_cleanup_result(&scope.session_id, &p)
    }
    pub async fn recover_turn(
        &self,
        scope: &Scope,
        thread: &str,
        turn: &str,
    ) -> Result<Value, AgentError> {
        let response = self
            .client
            .post(self.endpoint(&format!(
                "/internal/v1/threads/{}/turns/{}/recover",
                super::uuid(thread)?,
                super::uuid(turn)?
            )))
            .headers(self.headers(scope, Some(turn))?)
            .json(&json!({}))
            .send()
            .await
            .map_err(|_| AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_RECOVERY_FAILED"))?;
        if !response.status().is_success() {
            return Err(AgentError::code(
                if response.status().is_server_error() {
                    503
                } else {
                    response.status().as_u16()
                },
                "AI_PLATFORM_AGENT_RUNTIME_RECOVERY_FAILED",
            ));
        }
        response
            .json()
            .await
            .map_err(|_| AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_RECOVERY_FAILED"))
    }
    pub async fn interrupt_turn(
        &self,
        scope: &Scope,
        thread: &str,
        turn: &str,
        _reason: &str,
    ) -> Result<(), AgentError> {
        let response = self
            .client
            .post(self.endpoint(&format!(
                "/internal/v1/threads/{}/turns/{}/interrupt",
                super::uuid(thread)?,
                super::uuid(turn)?
            )))
            .headers(self.headers(scope, Some(turn))?)
            .json(&json!({}))
            .send()
            .await
            .map_err(|_| AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_INTERRUPT_FAILED"))?;
        if !response.status().is_success() {
            return Err(AgentError::code(
                if response.status().is_server_error() {
                    503
                } else {
                    409
                },
                "AI_PLATFORM_AGENT_RUNTIME_INTERRUPT_FAILED",
            ));
        }
        self.complete_run(turn, "cancelled").await
    }
    pub async fn get_approval(&self, scope: &Scope, id: &str) -> Result<Option<Value>, AgentError> {
        let r = self
            .client
            .get(self.endpoint(&format!("/internal/v1/approvals/{}", super::uuid(id)?)))
            .headers(self.headers(scope, None)?)
            .send()
            .await
            .map_err(|_| {
                AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_LOOKUP_FAILED")
            })?;
        if r.status().as_u16() == 404 {
            return Ok(None);
        }
        if !r.status().is_success() {
            return Err(AgentError::code(
                503,
                "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_LOOKUP_FAILED",
            ));
        }
        let p: Value = r.json().await.map_err(|_| {
            AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_LOOKUP_FAILED")
        })?;
        Ok(p.is_object().then_some(p))
    }
    pub async fn read_approval_decision(
        &self,
        scope: &Scope,
        thread: &str,
        id: &str,
        approved: bool,
        reason: Option<&str>,
    ) -> Result<Value, AgentError> {
        self.submit_approval_decision(scope, id, approved, reason, Some(thread))
            .await
    }
    pub async fn decide_approval(
        &self,
        scope: &Scope,
        id: &str,
        approved: bool,
        reason: Option<&str>,
    ) -> Result<Value, AgentError> {
        self.submit_approval_decision(scope, id, approved, reason, None)
            .await
    }
    async fn submit_approval_decision(
        &self,
        scope: &Scope,
        id: &str,
        approved: bool,
        reason: Option<&str>,
        receipt_only_thread: Option<&str>,
    ) -> Result<Value, AgentError> {
        let mut payload = json!({"decision":if approved{"approve"}else{"reject"},"reason":reason});
        if let Some(thread) = receipt_only_thread {
            payload["receipt_only"] = json!({"thread_id":super::uuid(thread)?});
        }
        let r = self
            .client
            .post(self.endpoint(&format!(
                "/internal/v1/approvals/{}/decision",
                super::uuid(id)?
            )))
            .headers(self.headers(scope, None)?)
            .json(&payload)
            .send()
            .await
            .map_err(|_| {
                AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_DECISION_FAILED")
            })?;
        if !r.status().is_success() {
            return Err(AgentError::code(
                if [400, 404, 409].contains(&r.status().as_u16()) {
                    409
                } else {
                    503
                },
                "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_DECISION_FAILED",
            ));
        }
        let p: Value = r.json().await.map_err(|_| {
            AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_DECISION_FAILED")
        })?;
        Ok(if p.is_object() {
            p
        } else {
            json!({"approval_id":id,"status":"consumed"})
        })
    }
    pub async fn complete_run(&self, run: &str, status: &str) -> Result<(), AgentError> {
        let status = completion_status(status);
        let mut usage:Value=sqlx::query_scalar("SELECT jsonb_build_object('input_tokens',COALESCE(SUM(input_tokens),0)::bigint,'output_tokens',COALESCE(SUM(output_tokens),0)::bigint,'cost_microusd',COALESCE(SUM(cost_microusd),0)::bigint) FROM assistant_runtime_model_calls WHERE run_id=$1::uuid AND status='completed'").bind(run).fetch_one(&self.pool).await?;
        let snapshot: Option<Value> = sqlx::query_scalar(
            "SELECT snapshot FROM assistant_runtime_snapshots WHERE run_id=$1::uuid",
        )
        .bind(run)
        .fetch_optional(&self.pool)
        .await?;
        if let Some(p) = snapshot
            .as_ref()
            .and_then(|s| s["pricing"].get("snapshot"))
            .filter(|v| !v.is_null())
        {
            usage["metadata"] = json!({"pricing_snapshot":p});
        }
        sqlx::query("UPDATE assistant_runtime_model_leases SET status='revoked',revoked_at=NOW(),revoked_reason=$2,updated_at=NOW() WHERE run_id=$1::uuid AND status='active'").bind(run).bind(format!("turn_{status}")).execute(&self.pool).await?;
        sqlx::query(COMPLETE_RUN_SQL)
            .bind(run)
            .bind(status)
            .bind(usage)
            .execute(&self.pool)
            .await?;
        if let Some(lease) = self.run_leases.lock().await.remove(run) {
            lease.release().await;
        }
        // Persist recorded usage provenance independently of execution status.
        // This never promotes mixed provider/estimated records into settlement.
        sqlx::query(TERMINAL_USAGE_PROJECTION_SQL)
            .bind(run)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    pub async fn fail_run(
        &self,
        run: &str,
        snapshot: &str,
        reason: &str,
    ) -> Result<(), AgentError> {
        sqlx::query("UPDATE assistant_runtime_model_leases SET status='revoked',revoked_at=NOW(),revoked_reason=$3,updated_at=NOW() WHERE run_id=$1::uuid AND snapshot_id=$2::uuid AND status='active'").bind(run).bind(snapshot).bind(reason).execute(&self.pool).await?;
        sqlx::query("UPDATE assistant_runs SET status='failed',error=$2,finished_at=NOW(),updated_at=NOW() WHERE run_id=$1::uuid AND engine='agent_runtime' AND status='running'").bind(run).bind(reason).execute(&self.pool).await?;
        Ok(())
    }
    async fn bind_catalog(
        &self,
        scope: &Scope,
        model: &str,
        revision: i64,
        readonly: &mut Value,
        allowlist: Option<&[Value]>,
    ) -> Result<(), AgentError> {
        if allowlist == Some(&[]) {
            for k in [
                "tools",
                "mcp",
                "deferred",
                "capability_allowlist",
                "attachment_tools",
            ] {
                readonly[k] = json!([]);
            }
            let mut t = readonly.clone();
            t["responses_tool_names"] = Value::Null;
            readonly["_thread_capabilities"] = t;
            return Ok(());
        }
        let payload = self
            .catalog
            .fetch(scope, model, revision, allowlist)
            .await?;
        let invalid =
            || AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_CATALOG_INVALID");
        if !payload.is_object()
            || payload
                .get("schema_version")
                .is_some_and(|v| !v.is_null() && v != "agent-capability-catalog/v1")
            || payload
                .get("capability_revision")
                .is_some_and(|v| !v.is_null() && v.as_i64() != Some(revision))
        {
            return Err(invalid());
        }
        let mut catalog: HashMap<&str, Vec<Value>> = HashMap::new();
        for k in ["tools", "mcp", "deferred"] {
            let values = payload.get(k).cloned().unwrap_or(json!([]));
            let values = values.as_array().ok_or_else(invalid)?;
            for d in values {
                let hash = text(d, "schema_hash");
                if !d.is_object()
                    || (k != "deferred" && d["read_only"] != true)
                    || d["tenant_id"] != scope.tenant_id
                    || d["capability_revision"].as_i64() != Some(revision)
                    || !d["name"].is_string()
                    || !d["description"].is_string()
                    || !d["schema"].is_object()
                    || text(d, "id").is_empty()
                    || hash.len() != 71
                    || !hash.starts_with("sha256:")
                    || !hash[7..]
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    || !matches!(
                        text(d, "kind"),
                        "tool"
                            | "knowledge"
                            | "mcp"
                            | "connector"
                            | "office_read"
                            | "platform_tool_discovery"
                    )
                {
                    return Err(invalid());
                }
            }
            catalog.insert(k, values.clone());
        }
        if !catalog["deferred"].is_empty() && !self.options.worker_writes_enabled {
            return Err(AgentError::code(
                if allowlist.is_some() { 409 } else { 503 },
                if allowlist.is_some() {
                    "AI_PLATFORM_AGENT_RUNTIME_WRITE_CAPABILITY_NOT_MIGRATED"
                } else {
                    "AI_PLATFORM_AGENT_RUNTIME_WRITE_CAPABILITY_DEGRADED"
                },
            ));
        }
        let filter = |descriptors: &[Value]| -> Result<Vec<Value>, AgentError> {
            let Some(allow) = allowlist else {
                return Ok(descriptors.to_vec());
            };
            let mut out = Vec::new();
            for d in descriptors {
                let matching: Vec<_> = allow.iter().filter(|a| a["id"] == d["id"]).collect();
                if matching.len() != 1 {
                    continue;
                }
                let a = matching[0];
                if !catalog_descriptor_matches_allowlist(d, a) {
                    return Err(AgentError::code(
                        409,
                        "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_CATALOG_SCOPE_MISMATCH",
                    ));
                }
                out.push(d.clone());
            }
            Ok(out)
        };
        let mut thread = readonly.clone();
        for k in ["tools", "mcp", "deferred"] {
            thread[k] = json!(filter(&catalog[k])?);
        }
        thread["responses_tool_names"] = Value::Null;
        let has_attachment = readonly["items"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v["kind"] == "attachment"));
        if !has_attachment {
            catalog
                .get_mut("tools")
                .expect("catalog")
                .retain(|v| v["name"] != "read_attachment");
        }
        let bridges = if readonly["responses_tool_names"].is_null() {
            catalog["tools"]
                .iter()
                .filter(|d| {
                    matches!(
                        text(d, "name"),
                        "tool_search" | "tool_describe" | "tool_call"
                    )
                })
                .cloned()
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        if let Some(names) = readonly["responses_tool_names"].as_array() {
            let mut tools = Vec::new();
            let mut mcp = Vec::new();
            for name in names {
                let matches: Vec<_> = catalog
                    .iter()
                    .flat_map(|(k, v)| v.iter().map(move |v| (*k, v)))
                    .filter(|(_, v)| v["name"] == *name)
                    .collect();
                if matches.len() != 1 {
                    return Err(AgentError::code(
                        400,
                        "AI_PLATFORM_AGENT_RUNTIME_RESPONSE_TOOL_NOT_FOUND",
                    ));
                }
                let (k, d) = matches[0];
                if d["read_only"] != true {
                    return Err(AgentError::code(
                        409,
                        "AI_PLATFORM_AGENT_RUNTIME_RESPONSE_TOOL_NOT_READONLY",
                    ));
                }
                if k == "tools" {
                    tools.push(d.clone());
                } else {
                    mcp.push(d.clone());
                }
            }
            catalog.insert("tools", tools);
            catalog.insert("mcp", mcp);
            catalog.insert("deferred", vec![]);
        }
        catalog.get_mut("tools").expect("catalog").retain(|d| {
            !matches!(
                text(d, "name"),
                "tool_search" | "tool_describe" | "tool_call"
            )
        });
        let mut count = 0;
        for k in ["tools", "mcp", "deferred"] {
            let mut v = filter(&catalog[k])?;
            count += v.len();
            if k == "tools" {
                v.extend(bridges.clone());
            }
            readonly[k] = json!(v);
        }
        if allowlist.is_some_and(|a| a.len() != count) {
            return Err(AgentError::code(
                409,
                "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_CATALOG_SCOPE_MISMATCH",
            ));
        }
        let live = |v: &Value| {
            ["tools", "mcp", "deferred", "attachment_tools"]
                .iter()
                .flat_map(|k| v[*k].as_array().into_iter().flatten().cloned())
                .collect::<Vec<_>>()
        };
        thread["capability_allowlist"]=json!(live(&thread).iter().map(|d|json!({"id":d["id"],"name":d["name"],"version":d["version"],"schema_hash":d["schema_hash"]})).collect::<Vec<_>>());
        readonly["capability_allowlist"] = json!(
            live(readonly)
                .iter()
                .map(|d| {
                    let mut a = allowlist
                        .and_then(|a| a.iter().find(|a| a["id"] == d["id"]))
                        .cloned()
                        .unwrap_or(json!({}));
                    let capability_type = if a["type"] == "connector" {
                        json!("connector")
                    } else {
                        d["kind"].clone()
                    };
                    for (k, v) in [
                        ("type", capability_type),
                        ("id", d["id"].clone()),
                        ("name", d["name"].clone()),
                        ("version", d["version"].clone()),
                        ("schema_hash", d["schema_hash"].clone()),
                    ] {
                        a[k] = v;
                    }
                    a
                })
                .collect::<Vec<_>>()
        );
        readonly["_thread_capabilities"] = thread;
        Ok(())
    }
    async fn load_memory(&self, scope: &Scope, mode: &str) -> Result<Value, AgentError> {
        if !matches!(mode, "auto" | "strict" | "user") {
            return Ok(Value::Null);
        }
        let unavailable = |reason: &str| -> Result<Value, AgentError> {
            if matches!(mode, "strict" | "user") {
                Err(AgentError::code(
                    503,
                    "AI_PLATFORM_AGENT_RUNTIME_MEMORY_UNAVAILABLE",
                ))
            } else {
                Ok(json!({"status":"unavailable","reason":reason}))
            }
        };
        let Some(service) = &self.memory else {
            return unavailable("memory_service_unavailable");
        };
        let context = match service.context(scope, 20).await {
            Ok(c) if c.is_object() => c,
            Ok(_) => return unavailable("memory_payload_invalid"),
            Err(_) => return unavailable("memory_lookup_failed"),
        };
        let frequent:Vec<_>=context["frequent_memories"].as_array().into_iter().flatten().take(10).filter(|v|v.is_object()).map(|v|json!({"key":text(v,"key").chars().take(128).collect::<String>(),"value":v["value"],"access_count":v["access_count"].as_i64().unwrap_or(0)})).collect();
        let mut bounded = json!({"preferences":super::object(&context["preferences"]),"frequent_memories":frequent});
        if spec::canonical(&bounded).chars().count() > 64 * 1024 {
            bounded["frequent_memories"] = json!([]);
            if spec::canonical(&bounded).chars().count() > 64 * 1024 {
                bounded["preferences"] = json!({});
            }
        }
        Ok(json!({"status":"available","context":bounded}))
    }
    pub async fn start_turn(&self, request: TurnRequest) -> Result<AgentTurn, AgentError> {
        // Background launches have no HTTP scope; mint one bounded correlation
        // snapshot for admission, dispatch and this launch's detached work.
        context::scoped(context::capture(), self.start_turn_scoped(request)).await
    }
    async fn start_turn_scoped(&self, request: TurnRequest) -> Result<AgentTurn, AgentError> {
        let scope = request.actor.scope(request.session_id.clone());
        let lease = self.acquire(&scope, "run", None).await?;
        let mut guard = TurnStartGuard {
            plane: self.clone(),
            scope: scope.clone(),
            lease: lease.clone(),
            pending: None,
            armed: true,
            correlation: context::capture(),
        };
        let result = self.start_turn_inner(request, &mut guard).await;
        match result {
            Ok(turn) => {
                guard.armed = false;
                if let Some(lease) = lease {
                    let plane = self.clone();
                    let watched = turn.clone();
                    let correlation = guard.correlation.clone();
                    tokio::spawn(context::scoped(correlation, async move {
                        plane.watch_run(scope, watched, lease).await;
                    }));
                }
                Ok(turn)
            }
            Err(e) => {
                guard.cleanup().await;
                Err(e)
            }
        }
    }
    async fn start_turn_inner(
        &self,
        request: TurnRequest,
        guard: &mut TurnStartGuard,
    ) -> Result<AgentTurn, AgentError> {
        let scope = request.actor.scope(request.session_id.clone());
        self.sources.require_conversation(&scope, true).await?;
        let assignment = self.threads.require_assignment(&scope).await?;
        let provider_hint = request
            .resolved_snapshot
            .as_ref()
            .and_then(|s| s["model"]["provider"].as_str())
            .or(request.provider_id.as_deref());
        let model = self
            .models
            .model(&scope.tenant_id, &request.model_id, provider_hint)
            .await?;
        if model["is_enabled"] == false
            || !entry::user_can_access_model(
                &request.actor,
                model["access_level"].as_str().unwrap_or("public"),
            )
        {
            return Err(AgentError::code(403, "AGENT_RUNTIME_MODEL_FORBIDDEN"));
        }
        let provider_id = text(&model, "provider_id");
        if provider_id.is_empty() {
            return Err(AgentError::code(409, "AGENT_RUNTIME_MODEL_MISMATCH"));
        }
        let provider = self
            .providers
            .provider(&scope.tenant_id, provider_id)
            .await?;
        if provider["is_enabled"] != true {
            return Err(AgentError::code(
                503,
                "AI_PLATFORM_AGENT_RUNTIME_PROVIDER_UNAVAILABLE",
            ));
        }
        let profile = &model["effective_capabilities"];
        if !profile.is_object() {
            return Err(AgentError::code(
                503,
                "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_PROFILE_INVALID",
            ));
        }
        if let Some(expected) = &request.expected_model_ref {
            for (k, v) in [
                ("tenant_id", json!(scope.tenant_id)),
                ("model_id", json!(request.model_id)),
                ("provider_id", json!(provider_id)),
                (
                    "capability_revision",
                    json!(model["capability_revision"].as_i64().unwrap_or(1)),
                ),
                (
                    "price_version",
                    model["pricing_snapshot"]["version"].clone(),
                ),
            ] {
                if expected[k] != v {
                    return Err(AgentError::code(409, "EVAL_MODEL_REF_CHANGED"));
                }
            }
        }
        let memory_mode = if request.memory_mode.is_empty() {
            "auto"
        } else {
            request.memory_mode.trim()
        };
        if !matches!(memory_mode, "off" | "session" | "auto" | "strict" | "user") {
            return Err(AgentError::code(
                400,
                "AI_PLATFORM_AGENT_RUNTIME_MEMORY_MODE_INVALID",
            ));
        }
        let memory_profile = request.memory_profile.as_deref().unwrap_or("basic");
        if !matches!(memory_profile, "off" | "basic" | "hybrid") {
            return Err(AgentError::code(
                400,
                "AI_PLATFORM_AGENT_RUNTIME_MEMORY_PROFILE_INVALID",
            ));
        }
        let mut instructions = request
            .developer_instructions
            .as_deref()
            .filter(|value| !value.is_empty())
            .unwrap_or(GENERIC_AGENT_INSTRUCTIONS)
            .trim()
            .to_owned();
        if let Some(style) = request
            .style_guidance
            .as_deref()
            .filter(|s| !s.trim().is_empty())
        {
            instructions.push_str("\n\n");
            instructions.push_str(style.trim());
        }
        if instructions.is_empty() || instructions.len() > 256 * 1024 {
            return Err(AgentError::code(
                400,
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_INSTRUCTIONS_INVALID",
            ));
        }
        let policy = json!({"reasoning_option":request.reasoning_option,"legacy_thinking_level":request.legacy_thinking_level,"max_tokens":request.max_tokens,"temperature":request.temperature,"style_guidance":request.style_guidance,"memory_mode":memory_mode,"memory_profile":memory_profile,"enable_dynamic_tools":request.enable_dynamic_tools});
        if request.local_node_capability.is_some() && request.resolved_snapshot.is_some() {
            return Err(AgentError::code(
                409,
                "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_BINDING_INVALID",
            ));
        }
        let signed = if let Some(snapshot) = request.resolved_snapshot.clone() {
            if text(&snapshot, "tenant_id") != scope.tenant_id
                || snapshot["model"]["id"] != request.model_id
                || snapshot["model"]["provider"] != provider_id
                || !snapshot["agent_spec"].is_object()
            {
                return Err(AgentError::code(
                    403,
                    "AI_PLATFORM_AGENT_RUNTIME_AGENT_SNAPSHOT_SCOPE_MISMATCH",
                ));
            }
            snapshot
        } else {
            // Source launch identity omits unset options rather than hashing
            // null-valued keys into the immutable model/spec fingerprint.
            let mut parameters = serde_json::Map::new();
            if let Some(value) = request.temperature {
                parameters.insert("temperature".into(), json!(value));
            }
            if let Some(value) = request.max_tokens {
                parameters.insert("max_tokens".into(), json!(value));
            }
            if let Some(value) = request.legacy_thinking_level.as_ref() {
                parameters.insert("thinking_mode".into(), json!(value));
            }
            let model_ref =
                json!({"id":request.model_id,"provider":provider_id,"parameters":parameters});
            let mut launch = entry::builtin_launch(
                &scope,
                &model_ref,
                profile,
                &request.readonly_capabilities,
                &policy,
                &instructions,
            );
            if let Some(capability) = &request.local_node_capability {
                launch["agent_spec"]["capabilities"] = json!([capability]);
                launch["fingerprints"]["spec"] =
                    json!(format!("sha256:{}", spec::hash(&launch["agent_spec"])));
                launch["fingerprints"]["tool_schema"] = json!(format!(
                    "sha256:{}",
                    spec::hash(&launch["agent_spec"]["capabilities"])
                ));
            }
            let capabilities = launch["agent_spec"]["capabilities"].clone();
            json!({"schema_version":"agent-runtime/v1","tenant_id":scope.tenant_id,"user_id":scope.user_id,"session_id":scope.session_id,"agent_id":"__builtin_assistant__","agent_version_id":null,"model":launch["model"],"model_profile":profile,"agent_spec":launch["agent_spec"],"capabilities":capabilities,"knowledge":launch["knowledge_bindings"],"memory":launch["memory_policy"],"channel_policy":launch["channel_policy"],"fingerprints":launch["fingerprints"],"publication":{"channel":"builtin","auth_mode":"private"}})
        };
        let agent_spec = &signed["agent_spec"];
        let instructions = agent_spec["developerInstructions"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(GENERIC_AGENT_INSTRUCTIONS);
        if request.resolved_snapshot.is_some()
            && request
                .developer_instructions
                .as_ref()
                .is_some_and(|v| v != instructions)
        {
            return Err(AgentError::code(
                409,
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_INSTRUCTIONS_MISMATCH",
            ));
        }
        let params = &signed["model"]["parameters"];
        let max = request.max_tokens.or_else(|| params["max_tokens"].as_i64());
        let temperature = request
            .temperature
            .or_else(|| params["temperature"].as_f64());
        if temperature.is_some_and(|t| !(0.0..=2.0).contains(&t)) {
            return Err(AgentError::code(
                409,
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_SNAPSHOT_INVALID",
            ));
        }
        let requested = request
            .reasoning_option
            .as_deref()
            .or(request.legacy_thinking_level.as_deref())
            .or_else(|| params["thinking_mode"].as_str());
        let reasoning = contract::reasoning(profile, requested)?;
        let selected_memory = if request.resolved_snapshot.is_some() {
            signed["memory"]["mode"].as_str().unwrap_or("session")
        } else {
            memory_mode
        };
        let memory = self.load_memory(&scope, selected_memory).await?;
        let mut input = if request.readonly_capabilities.is_object() {
            request.readonly_capabilities.clone()
        } else {
            json!({})
        };
        if memory["status"] == "available" {
            input["memory_context"] = memory["context"].clone();
        }
        let allowlist = contract::snapshot_allowlist(&signed)?;
        if let Some(a) = &allowlist {
            input["capability_allowlist"] = json!(a);
        }
        let revision = model["capability_revision"].as_i64().unwrap_or(1);
        let mut readonly = contract::readonly_payload(&input, &scope, revision)?;
        let config = contract::platform_config(&signed, &scope, &readonly)?;
        readonly["platform_config"] = config.clone();
        if request.enable_dynamic_tools {
            self.bind_catalog(
                &scope,
                &request.model_id,
                revision,
                &mut readonly,
                allowlist.as_deref(),
            )
            .await?;
        }
        let search = native_search_selection(&readonly, profile)?;
        let thread = self
            .ensure_thread(
                &scope,
                &request.model_id,
                Some(provider_id),
                Some(readonly.clone()),
                allowlist,
                search,
                Some(instructions),
            )
            .await?;
        readonly
            .as_object_mut()
            .expect("normalized")
            .remove("_thread_capabilities");
        let tool_policy = contract::tool_policy(&readonly, search, false, false)?;
        let context_window = model["context_window"].as_i64().unwrap_or(128000);
        let compact = model["auto_compact_token_limit"]
            .as_i64()
            .or_else(|| profile["auto_compact_token_limit"].as_i64());
        let resume = json!({"model":request.model_id,"modelPlaneBaseUrl":self.options.model_plane_base_url,"baseInstructions":CORE_ASSISTANT_PROMPT,"developerInstructions":instructions,"modelContextWindow":context_window,"autoCompactTokenLimit":compact,"nativeWebSearchEnabled":search,"toolPolicy":tool_policy});
        self.resume_thread(&scope, &thread.runtime_thread_id, &resume)
            .await?;
        let wire = text(&profile["wire_protocols"], "preferred");
        if profile["wire_protocols"]["supported"]
            .as_array()
            .is_none_or(|a| !a.iter().any(|v| v == wire))
        {
            return Err(AgentError::code(
                503,
                "AI_PLATFORM_AGENT_RUNTIME_WIRE_CAPABILITY_INVALID",
            ));
        }
        let model_max = model["max_output_tokens"].as_i64().unwrap_or(4096).max(1);
        let output = max.unwrap_or(model_max.min(32768)).clamp(1, model_max);
        let calls = self.options.max_model_calls.clamp(1, 128);
        let run = uuid::Uuid::new_v4().to_string();
        let snapshot_id = uuid::Uuid::new_v4().to_string();
        let lease_id = uuid::Uuid::new_v4().to_string();
        let nonce = spec::hash(&json!([
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string()
        ]));
        let provider_revision = text(&provider, "updated_at");
        let mut snapshot = json!({"schema_version":"agent-runtime-snapshot/v1","tenant_id":scope.tenant_id,"user_id":scope.user_id,"session_id":scope.session_id,"runtime_thread_id":thread.runtime_thread_id,"run_id":run,"kernel_revision":assignment["kernel_revision"],"model":{"id":request.model_id,"provider_id":provider_id,"api_type":provider["api_type"],"wire_protocol":wire,"provider_revision":provider_revision},"capability_revision":revision,"capabilities":profile,"instructions":{"baseInstructions":CORE_ASSISTANT_PROMPT,"developerInstructions":instructions},"runtime_resume_config":{"model":request.model_id,"modelPlaneBaseUrl":self.options.model_plane_base_url,"baseInstructions":CORE_ASSISTANT_PROMPT,"developerInstructions":instructions,"modelContextWindow":context_window,"nativeWebSearchEnabled":search},"agent_spec":agent_spec,"memory":{"mode":selected_memory,"context_status":memory["status"].as_str().unwrap_or("not_loaded"),"policy":if selected_memory!="off"&&(request.resolved_snapshot.is_none()||selected_memory=="user"){json!({"authoritative_profile":memory_profile,"agent_memory_mode":"user","memory_principal":scope.user_id})}else{Value::Null}},"memory_context":memory,"reasoning":reasoning,"input":{"message":request.message},"limits":{"context_window":context_window,"max_output_tokens":output,"max_model_calls":calls},"parameters":{},"pricing":{"snapshot":model["pricing_snapshot"],"input_price_per_1k":model["input_price_per_1k"],"output_price_per_1k":model["output_price_per_1k"]},"tools":{"enabled":readonly["tools"].as_array().is_some_and(|a|!a.is_empty())||readonly["mcp"].as_array().is_some_and(|a|!a.is_empty()),"phase":if ["items","tools","mcp"].iter().any(|k|readonly[*k].as_array().is_some_and(|a|!a.is_empty())){"readonly"}else{"pure_text"}},"readonly_capabilities":readonly,"tool_policy":tool_policy,"platform_config":config,"platform_config_hash":config["config_hash"]});
        if let Some(t) = temperature {
            snapshot["parameters"]["temperature"] = json!(t);
        }
        if let Some(limit) = compact {
            snapshot["limits"]["auto_compact_token_limit"] = json!(limit.max(1));
        }
        snapshot["eval_fingerprint"] = json!({"system_prompt_hash":spec::hash(&snapshot["instructions"]),"tool_schema_hash":spec::hash(&json!(contract::dynamic_tools(&readonly)?))});
        let expires = chrono::Utc::now()
            + chrono::Duration::seconds(self.options.lease_ttl_seconds.clamp(30, 3600));
        sqlx::query("SELECT issue_assistant_runtime_turn($1::uuid,$2::uuid,$3::uuid,$4::uuid,$5,$6,$7,$8,'agent-runtime-snapshot/v1',$9::jsonb,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23)").bind(&snapshot_id).bind(&lease_id).bind(&run).bind(&thread.runtime_thread_id).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).bind(text(&assignment,"kernel_revision")).bind(&snapshot).bind(spec::hash(&snapshot)).bind(revision).bind(text(&reasoning,"effective_option")).bind("agent-runtime-model-lease/v1").bind(provider_id).bind(&request.model_id).bind(provider_revision).bind(&nonce).bind(i32::try_from(calls).expect("bounded model call count fits int4")).bind((context_window*calls).min(10000000)).bind((output*calls).min(1000000)).bind(self.options.max_cost_microusd.max(1)).bind(expires).bind(&request.message).execute(&self.pool).await?;
        let lease:Value=sqlx::query_scalar("SELECT to_jsonb(l)||jsonb_build_object('issued_at_ms',floor(extract(epoch from issued_at)*1000)::bigint,'expires_at_ms',floor(extract(epoch from expires_at)*1000)::bigint) FROM assistant_runtime_model_leases l WHERE lease_id=$1::uuid").bind(&lease_id).fetch_optional(&self.pool).await?.ok_or_else(||AgentError::code(503,"AI_PLATFORM_AGENT_RUNTIME_LEASE_ISSUE_FAILED"))?;
        let signature = self.signer.sign(&super::model::lease_claims(&lease)?)?;
        let effort = reasoning["canonical_effort"]
            .as_str()
            .filter(|e| ["minimal", "low", "medium", "high", "xhigh", "max", "ultra"].contains(e));
        let body = json!({"runId":run,"snapshotId":snapshot_id,"leaseId":lease_id,"leaseSignature":signature,"message":request.message,"images":request.image_inputs,"model":request.model_id,"effort":effort,"capabilityRevision":revision,"readonly":contract::turn_prompt_readonly(&readonly),"platformConfig":config});
        let pending = AgentTurn {
            runtime_thread_id: thread.runtime_thread_id.clone(),
            run_id: run.clone(),
            snapshot_id: snapshot_id.clone(),
            lease_id: lease_id.clone(),
            after_sequence: thread.last_sequence,
            requested_reasoning_option: text(&reasoning, "requested_option").into(),
            effective_reasoning_option: text(&reasoning, "effective_option").into(),
            reasoning_adapter_id: text(&reasoning, "adapter_id").into(),
            capability_revision: revision,
            fallback_reason: reasoning["fallback_reason"].as_str().map(str::to_owned),
        };
        guard.pending = Some(pending.clone());
        if let Some(lease) = &guard.lease {
            self.run_leases
                .lock()
                .await
                .insert(run.clone(), lease.clone());
        }
        let response = self
            .client
            .post(self.endpoint(&format!(
                "/internal/v1/threads/{}/turns",
                thread.runtime_thread_id
            )))
            .headers(self.headers(&scope, Some(&run))?)
            .json(&body)
            .send()
            .await
            .map_err(|_| AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_TURN_START_FAILED"))?;
        if !response.status().is_success() {
            self.fail_run(&run, &snapshot_id, "agent_turn_start_rejected")
                .await?;
            guard.pending = None;
            self.run_leases.lock().await.remove(&run);
            return Err(AgentError::code(
                503,
                "AI_PLATFORM_AGENT_RUNTIME_TURN_START_FAILED",
            ));
        }
        let payload: Value = response
            .json()
            .await
            .map_err(|_| AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_TURN_START_FAILED"))?;
        if payload["turn"]["id"] != run {
            return Err(AgentError::code(
                503,
                "AI_PLATFORM_AGENT_RUNTIME_TURN_IDENTITY_MISMATCH",
            ));
        }
        Ok(pending)
    }
    pub async fn stream_thread_events(
        &self,
        scope: Scope,
        thread: String,
        after: i64,
        limit: i64,
        turn: Option<String>,
    ) -> Result<EventReceiver, AgentError> {
        let lease = self.acquire(&scope, "sse", None).await?;
        self.open_events(scope, thread, after, limit, turn, lease, true)
            .await
    }
    async fn open_events(
        &self,
        scope: Scope,
        thread: String,
        after: i64,
        limit: i64,
        turn: Option<String>,
        lease: Option<Arc<CapacityLease>>,
        guard_sources: bool,
    ) -> Result<EventReceiver, AgentError> {
        let lease_guard = SseLeaseGuard(lease);
        let response = self
            .events
            .open(
                self.endpoint(&format!(
                    "/internal/v1/threads/{}/events",
                    super::uuid(&thread)?,
                )),
                self.headers(&scope, turn.as_deref())?,
                after,
                limit,
            )
            .await?;
        if !response.status().is_success() {
            return Err(AgentError::code(
                if response.status().is_server_error() {
                    503
                } else {
                    response.status().as_u16()
                },
                "AI_PLATFORM_AGENT_RUNTIME_EVENT_STREAM_FAILED",
            ));
        }
        let (tx, rx) = mpsc::channel(16);
        let plane = self.clone();
        let task = tokio::spawn(context::scoped(context::capture(), async move {
            let _lease_guard = lease_guard;
            let mut response = response;
            let mut pending = Vec::<u8>::new();
            let mut frame = Vec::<String>::new();
            let mut revoked = false;
            let result:Result<(),AgentError>=async {while let Some(chunk)=response.chunk().await.map_err(|_|AgentError::code(503,"AI_PLATFORM_AGENT_RUNTIME_EVENT_STREAM_FAILED"))?{pending.extend_from_slice(&chunk);while let Some(end)=pending.iter().position(|b|*b==b'\n'){let bytes:Vec<_>=pending.drain(..=end).collect();let line=std::str::from_utf8(&bytes).map_err(|_|AgentError::code(503,"AI_PLATFORM_AGENT_RUNTIME_EVENT_STREAM_FAILED"))?.trim_end_matches(['\r','\n']).to_owned();if !line.is_empty(){frame.push(line);continue;}if frame.is_empty(){continue;}let data=frame.iter().find_map(|line|line.strip_prefix("data:").map(str::trim)).unwrap_or("").to_owned();frame.clear();let Ok(mut event)=serde_json::from_str::<Value>(&data)else{continue;};if !event.is_object(){continue;}if let Some(turn)=turn.as_deref(){let Some(projected)=project_child_event(&event,turn)else{continue;};event=projected;}
            let kind=text(&event,"event_type");let run=text(&event["data"],"run_id").to_owned();let terminal=matches!(kind,"run_finished"|"run_error"|"cancelled");let mut close=false;if terminal&&uuid::Uuid::parse_str(&run).is_ok(){let scoped=if let Some(turn)=&turn{turn==&run}else{sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM assistant_runs WHERE run_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND harness_thread_id=$5::uuid AND engine='agent_runtime')").bind(&run).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).bind(&thread).fetch_one(&plane.pool).await?};if scoped{let status=terminal_event_status(kind,&event["data"]);plane.complete_run(&run,status).await?;close=turn.is_some();}}
            if guard_sources{event=plane.sources.project_event(&scope,event).await?;revoked=revoked||event["data"]["source_access_revoked"]==true;if revoked{event=entry::redact_event(&event);}}
            if tx.send(Ok(event)).await.is_err(){return Ok(());}if close{return Ok(());}
        }if pending.len()>2*1024*1024{return Err(AgentError::code(503,"AI_PLATFORM_AGENT_RUNTIME_EVENT_STREAM_FAILED"));}}Ok(())}.await;
            if let Err(error) = result {
                let _ = tx.send(Err(error)).await;
            }
        }));
        Ok(EventReceiver { receiver: rx, task })
    }
    async fn terminal_in_ledger(
        &self,
        scope: &Scope,
        turn: &AgentTurn,
    ) -> Result<bool, AgentError> {
        let status: Option<String> = sqlx::query_scalar("SELECT status FROM assistant_runs WHERE run_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND engine='agent_runtime'").bind(&turn.run_id).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).fetch_optional(&self.pool).await?;
        Ok(status.as_deref().is_some_and(terminal_run_status))
    }
    async fn stop_run(&self, scope: &Scope, turn: &AgentTurn) -> bool {
        let _=sqlx::query("UPDATE assistant_runtime_model_leases SET status='revoked',revoked_at=NOW(),revoked_reason='run_capacity_interrupted',updated_at=NOW() WHERE run_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND status='active'").bind(&turn.run_id).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).execute(&self.pool).await;
        matches!(
            tokio::time::timeout(
                Duration::from_secs(5),
                self.interrupt_turn(
                    scope,
                    &turn.runtime_thread_id,
                    &turn.run_id,
                    "run_capacity_interrupted"
                )
            )
            .await,
            Ok(Ok(()))
        )
    }
    async fn watch_run(&self, scope: Scope, turn: AgentTurn, lease: Arc<CapacityLease>) {
        let mut cursor = turn.after_sequence;
        let consume = async {
            loop {
                if self.terminal_in_ledger(&scope, &turn).await? {
                    return Ok::<_, AgentError>(());
                }
                let opened = self
                    .open_events(
                        scope.clone(),
                        turn.runtime_thread_id.clone(),
                        cursor,
                        1000,
                        Some(turn.run_id.clone()),
                        None,
                        false,
                    )
                    .await;
                match opened {
                    Ok(mut events) => {
                        while let Some(event) = events.recv().await {
                            let event = match event {
                                Ok(event) => event,
                                Err(error) if error.status == 503 => break,
                                Err(error) => return Err(error),
                            };
                            cursor = event["sequence"].as_i64().unwrap_or(cursor).max(cursor);
                            if matches!(
                                text(&event, "event_type"),
                                "run_finished" | "run_error" | "cancelled"
                            ) && event["data"]["run_id"] == turn.run_id
                            {
                                return Ok(());
                            }
                        }
                    }
                    Err(error) if error.status == 503 => {}
                    Err(error) => return Err(error),
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        };
        tokio::pin!(consume);
        let confirmed = tokio::select! {result=tokio::time::timeout(Duration::from_secs(self.options.lease_ttl_seconds.clamp(30,3600)as u64),&mut consume)=>matches!(result,Ok(Ok(()))),_=lease.wait_lost()=>false};
        if confirmed
            || self
                .terminal_in_ledger(&scope, &turn)
                .await
                .unwrap_or(false)
            || self.stop_run(&scope, &turn).await
        {
            if let Some(lease) = self.run_leases.lock().await.remove(&turn.run_id) {
                lease.release().await;
            }
        }
    }
}

struct TurnStartGuard {
    plane: RuntimeControlPlane,
    scope: Scope,
    lease: Option<Arc<CapacityLease>>,
    pending: Option<AgentTurn>,
    armed: bool,
    correlation: context::OutboundContext,
}
impl TurnStartGuard {
    async fn cleanup(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;
        if let Some(turn) = self.pending.take() {
            if self.plane.stop_run(&self.scope, &turn).await {
                self.plane.run_leases.lock().await.remove(&turn.run_id);
                if let Some(lease) = self.lease.take() {
                    lease.release().await;
                }
            }
        } else if let Some(lease) = self.lease.take() {
            lease.release().await;
        }
    }
}
impl Drop for TurnStartGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut cleanup = Self {
            plane: self.plane.clone(),
            scope: self.scope.clone(),
            lease: self.lease.take(),
            pending: self.pending.take(),
            armed: true,
            correlation: self.correlation.clone(),
        };
        self.armed = false;
        tokio::spawn(context::scoped(cleanup.correlation.clone(), async move {
            cleanup.cleanup().await;
        }));
    }
}
struct SseLeaseGuard(Option<Arc<CapacityLease>>);
impl Drop for SseLeaseGuard {
    fn drop(&mut self) {
        if let Some(lease) = self.0.take() {
            tokio::spawn(async move {
                lease.release().await;
            });
        }
    }
}
pub fn project_child_event(envelope: &Value, parent: &str) -> Option<Value> {
    let data = envelope["data"].as_object()?;
    let run = data.get("run_id")?.as_str()?;
    if run.is_empty() {
        return None;
    }
    if run == parent {
        return Some(envelope.clone());
    }
    let agent = data.get("thread_id")?.as_str()?;
    if agent.is_empty() {
        return None;
    }
    let kind = text(envelope, "event_type");
    let mut common = json!({"agent_id":agent,"agent_type":"task","call_id":run,"parent_task_id":parent,"task_id":run,"session_id":data.get("session_id"),"thread_id":agent});
    let event_type = match kind {
        "run_started" => {
            common["description"] = "Delegated child task".into();
            common["status"] = "running".into();
            "subagent_started"
        }
        "run_finished" | "run_error" | "cancelled" => {
            let status = data
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("failed")
                .to_lowercase();
            common["status"] = json!(if kind == "run_finished"
                && matches!(status.as_str(), "completed" | "succeeded")
            {
                "completed"
            } else if status == "cancelled" || kind == "cancelled" {
                "cancelled"
            } else {
                "failed"
            });
            common["result"] = data.get("exit").cloned().unwrap_or(Value::Null);
            "subagent_finished"
        }
        "text_delta" if data.get("content").is_some_and(Value::is_string) => {
            common["content"] = data["content"].clone();
            common["status"] = "running".into();
            "subagent_text_delta"
        }
        _ => return None,
    };
    let mut result = envelope.clone();
    result["event_type"] = event_type.into();
    result["data"] = common;
    Some(result)
}

// Keep completion fail-closed. Historical recovery's `completed` value is
// terminal, but may hide a Core terminal error and is not a success alias.
fn completion_status(status: &str) -> &'static str {
    match status {
        "succeeded" => "succeeded",
        "cancelled" => "cancelled",
        _ => "failed",
    }
}

fn terminal_event_status<'a>(kind: &str, data: &'a Value) -> &'a str {
    if kind == "cancelled" {
        "cancelled"
    } else {
        data["status"].as_str().unwrap_or("failed")
    }
}

fn terminal_run_status(status: &str) -> bool {
    matches!(status, "succeeded" | "failed" | "cancelled" | "completed")
}

const COMPLETE_RUN_SQL: &str = "UPDATE assistant_runs SET status=$2,usage=$3::jsonb,finished_at=NOW(),updated_at=NOW() WHERE run_id=$1::uuid AND engine='agent_runtime' AND status='running'";

// Keep in sync with Python run_ledger.TERMINAL_USAGE_PROJECTION_SQL. The shared
// fixture check compares exact SQL; real PostgreSQL validates the JSON/CAS path.
const TERMINAL_USAGE_PROJECTION_SQL: &str = r#"
WITH target AS MATERIALIZED (
    SELECT r.run_id,r.tenant_id,r.user_id,r.session_id,r.harness_thread_id,
           r.runtime_snapshot_id,r.status,r.usage
    FROM assistant_runs r
    JOIN assistant_runtime_snapshots s ON s.snapshot_id=r.runtime_snapshot_id
      AND s.run_id=r.run_id AND s.runtime_thread_id=r.harness_thread_id
      AND s.tenant_id=r.tenant_id AND s.user_id=r.user_id AND s.session_id=r.session_id
    WHERE r.run_id=$1::uuid AND r.engine='agent_runtime'
      AND r.status IN ('succeeded','failed','cancelled')
      AND jsonb_typeof(r.usage)='object'
      AND (NOT (r.usage ? 'metadata') OR jsonb_typeof(r.usage->'metadata')='object')
      AND EXISTS (
          SELECT 1 FROM assistant_runtime_items i
          WHERE i.runtime_thread_id=r.harness_thread_id
            AND i.kernel_thread_id=r.harness_thread_id
            AND i.tenant_id=r.tenant_id AND i.user_id=r.user_id AND i.session_id=r.session_id
            AND i.turn_id=r.run_id::text AND i.status=r.status
            AND i.payload->>'schema_version'='assistant-turn-contract/v1'
            AND i.payload->'data'->>'run_id'=r.run_id::text
            AND i.payload->'data'->>'thread_id'=r.harness_thread_id::text
            AND i.payload->'data'->>'session_id'=r.session_id
            AND i.payload->'data'->>'status'=r.status
            AND i.event_type='compat/v1/'||(i.payload->>'event_type')
            AND ((r.status='succeeded' AND i.event_type='compat/v1/run_finished')
              OR (r.status='failed' AND i.event_type='compat/v1/run_error')
              OR (r.status='cancelled' AND i.event_type IN ('compat/v1/run_error','compat/v1/cancelled')))
      )
), observed AS (
    SELECT t.run_id,COUNT(c.call_id)::bigint AS total_calls,
           COUNT(c.call_id) FILTER (WHERE c.status='completed')::bigint AS completed_calls,
           COUNT(c.call_id) FILTER (WHERE c.status IN ('reserved','dispatched'))::bigint AS pending_calls,
           COUNT(c.call_id) FILTER (WHERE c.status='unknown')::bigint AS unknown_calls,
           COUNT(c.call_id) FILTER (WHERE c.dispatched_at IS NOT NULL)::bigint AS dispatched_calls,
           COUNT(c.call_id) FILTER (
               WHERE c.status='completed' AND c.input_tokens IS NOT NULL
                 AND c.output_tokens IS NOT NULL AND c.cost_microusd IS NOT NULL
           )::bigint AS recorded_calls,
           SUM(c.input_tokens) FILTER (
               WHERE c.status='completed' AND c.input_tokens IS NOT NULL
                 AND c.output_tokens IS NOT NULL AND c.cost_microusd IS NOT NULL
           )::bigint AS input_tokens,
           SUM(c.output_tokens) FILTER (
               WHERE c.status='completed' AND c.input_tokens IS NOT NULL
                 AND c.output_tokens IS NOT NULL AND c.cost_microusd IS NOT NULL
           )::bigint AS output_tokens,
           SUM(c.cost_microusd) FILTER (
               WHERE c.status='completed' AND c.input_tokens IS NOT NULL
                 AND c.output_tokens IS NOT NULL AND c.cost_microusd IS NOT NULL
           )::bigint AS cost_microusd
    FROM target t LEFT JOIN assistant_runtime_model_calls c
      ON c.run_id=t.run_id AND c.snapshot_id=t.runtime_snapshot_id
     AND c.tenant_id=t.tenant_id AND c.user_id=t.user_id AND c.session_id=t.session_id
    GROUP BY t.run_id
), projection AS (
    SELECT t.*,t.usage || jsonb_build_object(
        'metadata',COALESCE(t.usage->'metadata','{}'::jsonb) || jsonb_build_object(
            'runtime_model_usage',jsonb_build_object(
                'source','persisted_model_call_ledger',
                'provider_usage_verified',false,
                'settlement','not_established',
                'reason','provider_reported_and_estimated_usage_not_distinguished',
                'total_calls',o.total_calls,'completed_calls',o.completed_calls,
                'recorded_calls',o.recorded_calls,'pending_calls',o.pending_calls,
                'unknown_calls',o.unknown_calls,'dispatched_calls',o.dispatched_calls,
                'recorded_totals_complete',o.recorded_calls>0
                    AND o.recorded_calls=o.completed_calls AND o.pending_calls=0
                    AND o.unknown_calls=0 AND o.dispatched_calls=o.recorded_calls,
                'recorded_totals',CASE WHEN o.recorded_calls>0 THEN jsonb_build_object(
                    'input_tokens',o.input_tokens,'output_tokens',o.output_tokens,
                    'cost_microusd',o.cost_microusd
                ) ELSE NULL END
            )
        )
    ) AS next_usage
    FROM target t JOIN observed o USING(run_id)
)
UPDATE assistant_runs r SET usage=p.next_usage,updated_at=NOW()
FROM projection p
WHERE r.run_id=p.run_id AND r.engine='agent_runtime' AND r.status=p.status
  AND r.tenant_id=p.tenant_id AND r.user_id=p.user_id AND r.session_id=p.session_id
  AND r.harness_thread_id=p.harness_thread_id AND r.runtime_snapshot_id=p.runtime_snapshot_id
  AND r.usage IS NOT DISTINCT FROM p.usage AND r.usage IS DISTINCT FROM p.next_usage
"#;

#[cfg(test)]
mod terminal_status_tests {
    use super::*;
    use rusqlite::{Connection, params};

    fn cases() -> Value {
        serde_json::from_str(include_str!(
            "../../../../../../tests/fixtures/agent-runtime/terminal-status-v1.json"
        ))
        .unwrap()
    }

    #[test]
    fn terminal_usage_projection_matches_the_shared_postgres_contract() {
        assert_eq!(
            TERMINAL_USAGE_PROJECTION_SQL.trim(),
            include_str!(
                "../../../../../../tests/fixtures/agent-runtime/terminal-usage-projection.sql"
            )
            .trim()
        );
        let assignments = TERMINAL_USAGE_PROJECTION_SQL
            .split("UPDATE assistant_runs r SET")
            .nth(1)
            .unwrap()
            .split("FROM projection")
            .next()
            .unwrap()
            .trim();
        assert_eq!(assignments, "usage=p.next_usage,updated_at=NOW()");
        assert!(TERMINAL_USAGE_PROJECTION_SQL.contains("'provider_usage_verified',false"));
        assert!(TERMINAL_USAGE_PROJECTION_SQL.contains("'settlement','not_established'"));
        assert!(TERMINAL_USAGE_PROJECTION_SQL.contains("r.usage IS NOT DISTINCT FROM p.usage"));
    }

    #[test]
    fn terminal_vocabulary_is_bounded_and_legacy_completion_is_not_success() {
        for case in cases()["statuses"].as_array().unwrap() {
            let input = case["input"].as_str().unwrap();
            assert_eq!(
                completion_status(input),
                case["completion_status"].as_str().unwrap(),
                "{input}"
            );
            assert_eq!(
                terminal_run_status(input),
                case["terminal_in_ledger"].as_bool().unwrap(),
                "{input}"
            );
        }
    }

    #[test]
    fn cancelled_event_kind_and_run_error_cancellation_remain_compatible() {
        for (kind, data) in [
            ("cancelled", json!({})),
            ("cancelled", json!({"status":"failed"})),
            ("run_error", json!({"status":"cancelled"})),
        ] {
            assert_eq!(
                completion_status(terminal_event_status(kind, &data)),
                "cancelled"
            );
        }
    }

    #[test]
    fn completion_sql_preserves_existing_terminal_rows_and_other_engines() {
        // Execute the production WHERE predicate; SQLite does not prove
        // PostgreSQL types, locks, transaction ordering, or crash atomicity.
        let sql = COMPLETE_RUN_SQL
            .replace("$1::uuid", "?1")
            .replace("$2", "?2")
            .replace("$3::jsonb", "?3")
            .replace("NOW()", "CURRENT_TIMESTAMP");
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE assistant_runs(run_id TEXT PRIMARY KEY, engine TEXT,
             status TEXT, usage TEXT, finished_at TEXT, updated_at TEXT);",
        )
        .unwrap();
        for case in cases()["existing_rows"].as_array().unwrap() {
            let prior = case["status"].as_str().unwrap();
            let engine = case["engine"].as_str().unwrap();
            db.execute("DELETE FROM assistant_runs", []).unwrap();
            db.execute(
                "INSERT INTO assistant_runs VALUES('run-a', ?1, ?2, '{}', 'original', 'original')",
                params![engine, prior],
            )
            .unwrap();
            let changed = db
                .execute(&sql, params!["run-a", "succeeded", "{}"])
                .unwrap();
            assert_eq!(
                changed == 1,
                case["may_complete"].as_bool().unwrap(),
                "{engine}/{prior}"
            );
            let (status, finished_at): (String, String) = db
                .query_row(
                    "SELECT status, finished_at FROM assistant_runs WHERE run_id='run-a'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            let expected = if changed == 1 { "succeeded" } else { prior };
            assert_eq!(status, expected);
            if changed == 0 {
                assert_eq!(finished_at, "original");
            }
        }
    }
}

#[cfg(test)]
mod session_cleanup_status_tests {
    use super::*;

    #[test]
    fn accepts_both_runtime_success_outcomes() {
        assert!(
            session_cleanup_result("s", &json!({"session_id":"s","status":"deleted"})).unwrap()
        );
        assert!(
            !session_cleanup_result("s", &json!({"session_id":"s","status":"not_found"})).unwrap()
        );
    }

    #[test]
    fn rejects_unknown_missing_and_non_string_status() {
        for status in [
            Value::Null,
            json!(true),
            json!(1),
            json!([]),
            json!({}),
            json!(""),
            json!("pending"),
            json!("failed"),
            json!("DELETED"),
            json!("not-found"),
        ] {
            let error = session_cleanup_result("s", &json!({"session_id":"s","status":status}))
                .unwrap_err();
            assert_eq!(error.status, 503);
            assert_eq!(
                error.detail,
                json!({"code":"AI_PLATFORM_AGENT_RUNTIME_SESSION_CLEANUP_INVALID"})
            );
        }
        assert!(session_cleanup_result("s", &json!({"session_id":"s"})).is_err());
    }

    #[test]
    fn rejects_wrong_session_and_non_object_payload() {
        for payload in [
            json!({"session_id":"other","status":"deleted"}),
            json!({"status":"not_found"}),
            json!({"session_id":1,"status":"deleted"}),
            Value::Null,
            json!([]),
            json!("deleted"),
        ] {
            assert!(session_cleanup_result("s", &payload).is_err());
        }
    }
}

#[cfg(test)]
mod failed_thread_create_tests {
    use super::*;

    fn thread(dynamic_tool_fingerprint: Option<&str>) -> RuntimeThread {
        RuntimeThread {
            runtime_thread_id: "thread-id".into(),
            tenant_id: "tenant".into(),
            user_id: "user".into(),
            session_id: "session".into(),
            kernel_owner: "agent_runtime".into(),
            source_kind: "native".into(),
            import_status: "pending".into(),
            last_sequence: 0,
            dynamic_tool_fingerprint: dynamic_tool_fingerprint.map(str::to_owned),
        }
    }

    #[test]
    fn failed_start_reservation_is_reported_as_create_failure() {
        for thread in [Some(thread(None)), Some(thread(Some(""))), None] {
            let error = failed_thread_create(thread, &"a".repeat(64)).unwrap_err();
            assert_eq!(error.status, 503);
            assert_eq!(
                error.detail,
                json!({"code":"AI_PLATFORM_AGENT_RUNTIME_THREAD_CREATE_FAILED"})
            );
        }
    }

    #[test]
    fn failed_duplicate_create_reuses_only_the_same_bound_configuration() {
        let fingerprint = "a".repeat(64);
        let bound_thread =
            failed_thread_create(Some(thread(Some(&fingerprint))), &fingerprint).unwrap();
        assert_eq!(
            bound_thread.dynamic_tool_fingerprint.as_deref(),
            Some(fingerprint.as_str())
        );

        let error =
            failed_thread_create(Some(thread(Some(&"b".repeat(64)))), &fingerprint).unwrap_err();
        assert_eq!(error.status, 409);
        assert_eq!(
            error.detail,
            json!({"code":"AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_THREAD_RECREATE_REQUIRED"})
        );
    }
}

#[cfg(test)]
mod native_search_admission_tests {
    use super::*;

    fn readonly(web_search: bool) -> Value {
        let items = if web_search {
            json!([{"source":"web-search"}])
        } else {
            json!([])
        };
        json!({
            "items": items,
            "responses_tool_choice": "auto",
            "responses_tool_names": null
        })
    }

    fn profile(enabled: bool, wire: &str) -> Value {
        json!({
            "native_search": {"enabled": enabled},
            "tools": {"web_search_wire": wire}
        })
    }

    #[test]
    fn rejects_web_search_before_dispatch_when_model_does_not_support_it() {
        for unsupported in [profile(false, "disabled"), profile(true, "disabled")] {
            let error = native_search_selection(&readonly(true), &unsupported).unwrap_err();
            assert_eq!(error.status, 422);
            assert_eq!(
                error.detail,
                json!({"code":"AI_PLATFORM_AGENT_RUNTIME_NATIVE_SEARCH_UNAVAILABLE"})
            );
        }
    }

    #[test]
    fn enables_native_search_only_for_a_supported_selected_request() {
        assert!(native_search_selection(&readonly(true), &profile(true, "native")).unwrap());
        let unsupported_profile = profile(false, "disabled");
        assert!(!native_search_selection(&readonly(false), &unsupported_profile).unwrap());

        let mut tool_choice_none = readonly(true);
        tool_choice_none["responses_tool_choice"] = json!("none");
        assert!(!native_search_selection(&tool_choice_none, &profile(true, "native")).unwrap());
    }
}
