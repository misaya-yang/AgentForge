//! Live model-control-plane bridge for remote adapters and typed REST runs.
//! Credentials stay inside server-only runtime values; no ambient configuration.
use super::models::{Models, Providers};
use crate::{
    billing::{OverageStrategy, QuotaStatus},
    dispatcher::{AdapterFailure, DispatchError},
    migration_adapters::LangGraphPayloadPreparation,
    migration_compat::{common::CompatUser, langgraph_body},
    model_failover::{
        ResolvedRuntimeSnapshot, build_runtime_candidate, normalize_failover_attempts,
    },
    protocol::UnifiedRequest,
    provider_service::ServiceError,
    quota_store::NewQuotaAlert,
    service_ownership::ServiceHandle,
    service_registry::{AdapterError, AdapterFuture, python_truthy},
    usage_parser::{estimate_tokens, extract_model, python_value_to_string},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub enum LivePreparationError {
    Invalid(String),
    Unavailable,
}
impl std::fmt::Display for LivePreparationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(code) => f.write_str(code),
            Self::Unavailable => f.write_str("MODEL_OVERRIDE_CONTROL_PLANE_UNAVAILABLE"),
        }
    }
}
impl std::error::Error for LivePreparationError {}
type CacheKey = (String, String, String, String);
type CacheCell = Arc<Mutex<Option<(Instant, Value)>>>;
#[derive(Default)]
struct RuntimeCache {
    entries: HashMap<CacheKey, CacheCell>,
    order: VecDeque<CacheKey>,
}
pub struct LiveLangGraphPreparation {
    pub models: Arc<Models>,
    pub providers: Arc<Providers>,
    quota: Arc<super::foundation_backends::NativeQuotaChecker>,
    pool: sqlx::PgPool,
    cache: Mutex<RuntimeCache>,
}
impl LiveLangGraphPreparation {
    pub fn new(
        models: Arc<Models>,
        providers: Arc<Providers>,
        pool: sqlx::PgPool,
        quota: Arc<super::foundation_backends::NativeQuotaChecker>,
    ) -> Self {
        Self {
            models,
            providers,
            quota,
            pool,
            cache: Mutex::new(RuntimeCache::default()),
        }
    }
    /// The same resolver is suitable for authenticated REST governance; that
    /// caller must retain its own API-key allowlist and quota policy context.
    pub async fn resolve_runtime(
        &self,
        tenant: &str,
        service_id: &str,
        model_override: &Value,
    ) -> Result<Value, LivePreparationError> {
        let tenant = if tenant.is_empty() { "default" } else { tenant };
        let signature = json!({"provider_id":model_override["provider_id"],"model_id":model_override["model_id"],"temperature":model_override["temperature"],"failover":model_override["failover"]});
        let digest = Sha256::digest(
            serde_json::to_vec(&signature).map_err(|_| LivePreparationError::Unavailable)?,
        );
        let signature: String = digest[..8].iter().map(|v| format!("{v:02x}")).collect();
        let key = (
            tenant.into(),
            service_id.into(),
            id_or(&model_override["cache_epoch"], "0"),
            signature,
        );
        let cell = {
            let mut cache = self.cache.lock().await;
            if let Some(cell) = cache.entries.get(&key) {
                cell.clone()
            } else {
                while cache.entries.len() >= 2048 {
                    if let Some(old) = cache.order.pop_front() {
                        cache.entries.remove(&old);
                    } else {
                        break;
                    }
                }
                let cell = Arc::new(Mutex::new(None));
                cache.entries.insert(key.clone(), cell.clone());
                cache.order.push_back(key.clone());
                cell
            }
        };
        // A per-key async lock coalesces lookups without serializing other tenants.
        let mut cached = cell.lock().await;
        if let Some((at, value)) = &*cached {
            if at.elapsed() <= Duration::from_secs(60) {
                return Ok(value.clone());
            }
        }
        let started = Instant::now();
        let result = self.build_runtime(tenant, model_override).await;
        match result {
            Ok(value) => {
                *cached = Some((started, value.clone()));
                Ok(value)
            }
            Err(error) => {
                let mut cache = self.cache.lock().await;
                if cache
                    .entries
                    .get(&key)
                    .is_some_and(|current| Arc::ptr_eq(current, &cell))
                {
                    cache.entries.remove(&key);
                    cache.order.retain(|k| k != &key);
                }
                Err(error)
            }
        }
    }
    async fn candidate(
        &self,
        tenant: &str,
        raw: &Value,
        temperature: &Value,
        epoch: &Value,
        prefix: &str,
    ) -> Result<Value, LivePreparationError> {
        let provider_id = id_or(&raw["provider_id"], "");
        let model_id = id_or(&raw["model_id"], "");
        let invalid = |suffix: &str| LivePreparationError::Invalid(format!("{prefix}_{suffix}"));
        if provider_id.is_empty() {
            return Err(invalid("PROVIDER_REQUIRED"));
        }
        if model_id.is_empty() {
            return Err(invalid("MODEL_REQUIRED"));
        }
        let provider = self
            .providers
            .get_runtime_provider_config(tenant, &provider_id)
            .await
            .map_err(|e| match e {
                ServiceError::ProviderNotFound => invalid("PROVIDER_NOT_FOUND"),
                _ => LivePreparationError::Unavailable,
            })?
            .into_internal_value();
        if !python_truthy(&provider["is_enabled"]) {
            return Err(invalid("PROVIDER_DISABLED"));
        }
        let model = self
            .models
            .get_provider_model(tenant, &provider_id, &model_id)
            .await
            .map_err(|_| LivePreparationError::Unavailable)?
            .ok_or_else(|| invalid("MODEL_NOT_FOUND"))?;
        let snapshot = ResolvedRuntimeSnapshot::new(&[provider], &[Value::Object(model)]);
        build_runtime_candidate(
            tenant,
            &json!(provider_id),
            &json!(model_id),
            temperature,
            epoch,
            &snapshot,
            prefix,
        )
        .map(|v| v.into_internal_value())
        .map_err(|e| LivePreparationError::Invalid(e.to_string()))
    }
    async fn build_runtime(
        &self,
        tenant: &str,
        config: &Value,
    ) -> Result<Value, LivePreparationError> {
        if !config.is_object() || !python_truthy(&config["enabled"]) {
            return Err(LivePreparationError::Invalid(
                "MODEL_OVERRIDE_INVALID".into(),
            ));
        }
        let primary = self
            .candidate(
                tenant,
                config,
                &config["temperature"],
                &config["cache_epoch"],
                "MODEL_OVERRIDE",
            )
            .await?;
        let mut runtime = primary.clone();
        let failover = &config["failover"];
        if !failover.is_object() || !python_truthy(&failover["enabled"]) {
            return Ok(runtime);
        }
        let mut seen = HashSet::from([(
            id_or(&primary["provider_id"], ""),
            id_or(&primary["model_id"], ""),
        )]);
        let mut candidates = vec![primary];
        let mut warnings = vec![];
        if let Some(values) = failover["candidates"].as_array() {
            for raw in values {
                if !raw.is_object() {
                    warnings.push(json!({"code":"MODEL_OVERRIDE_FAILOVER_CANDIDATE_INVALID"}));
                    continue;
                }
                let provider = id_or(&raw["provider_id"], "");
                let model = id_or(&raw["model_id"], "");
                let mut warning = json!({"provider_id":if provider.is_empty(){None}else{Some(&provider)},"model_id":if model.is_empty(){None}else{Some(&model)}});
                let code = if provider.is_empty() || model.is_empty() {
                    Some("MODEL_OVERRIDE_FAILOVER_CANDIDATE_INVALID".to_owned())
                } else if seen.contains(&(provider.clone(), model.clone())) {
                    Some("MODEL_OVERRIDE_FAILOVER_DUPLICATE".to_owned())
                } else {
                    match self
                        .candidate(
                            tenant,
                            raw,
                            &config["temperature"],
                            &config["cache_epoch"],
                            "MODEL_OVERRIDE_FAILOVER",
                        )
                        .await
                    {
                        Ok(value) => {
                            candidates.push(value);
                            seen.insert((provider, model));
                            None
                        }
                        Err(LivePreparationError::Invalid(code)) => Some(code),
                        Err(error) => return Err(error),
                    }
                };
                if let Some(code) = code {
                    warning["code"] = json!(code);
                    warnings.push(warning);
                }
            }
        }
        runtime["failover"] = json!({"enabled":true,"max_attempts":normalize_failover_attempts(&failover["max_attempts"],candidates.len(),3),"candidates":candidates});
        if !warnings.is_empty() {
            runtime["failover"]["warnings"] = json!(warnings);
        }
        Ok(runtime)
    }
    async fn quota_event(&self, request: &UnifiedRequest) {
        // Canonical recorder aggregates; metadata is deliberately not persisted.
        let _ = sqlx::query("INSERT INTO security_event_daily_aggregates (tenant_id,user_id,service_id,event_type,date,event_count) VALUES ($1,$2,$3,'quota_exceeded',$4,1) ON CONFLICT (tenant_id,COALESCE(user_id,''),COALESCE(service_id,''),event_type,date) DO UPDATE SET event_count=security_event_daily_aggregates.event_count+1,updated_at=CURRENT_TIMESTAMP")
            .bind(if request.tenant_id.is_empty(){"public"}else{&request.tenant_id}).bind((!request.user_id.is_empty()).then_some(&request.user_id)).bind(&request.service_id).bind(chrono::Utc::now().date_naive()).execute(&self.pool).await;
    }
    async fn apply_adapter_quota(
        &self,
        request: &UnifiedRequest,
        payload: &mut Value,
    ) -> Result<Option<String>, AdapterError> {
        let tenant = if request.tenant_id.is_empty() {
            "default"
        } else {
            &request.tenant_id
        };
        let user = if request.user_id.is_empty() {
            "anonymous"
        } else {
            &request.user_id
        };
        // Adapter's minimal control-plane request has default fail-open policy.
        let Ok(check) = self
            .quota
            .check_quota(tenant, user, payload_tokens(payload), false)
            .await
        else {
            return Ok(None);
        };
        if check.status == QuotaStatus::Blocked || !check.can_proceed() {
            self.quota_event(request).await;
            return Err(DispatchError::Adapter(AdapterFailure::HttpStatus(
                if check.status != QuotaStatus::Blocked
                    && check.overage_strategy == OverageStrategy::RateLimit
                {
                    429
                } else {
                    403
                },
            ))
            .into());
        }
        if check.status != QuotaStatus::Exceeded {
            return Ok(None);
        }
        self.quota_event(request).await;
        if check.overage_strategy == OverageStrategy::DowngradeModel {
            if let Some(model) = check
                .downgraded_model
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                payload["model"] = json!(model);
                if payload["input"].is_object() {
                    payload["input"]["model"] = json!(model)
                }
                if payload
                    .pointer("/config/configurable")
                    .is_some_and(Value::is_object)
                {
                    payload["config"]["configurable"]["model"] = json!(model)
                }
                return Ok(Some(model.into()));
            }
        } else if check.overage_strategy == OverageStrategy::AllowButAlert {
            // Python creates a fresh minimal request for each adapter invocation,
            // so its request-local alert dedupe map never spans invocations.
            let _ = self
                .quota
                .store
                .try_create_alert(&NewQuotaAlert {
                    tenant_id: tenant.into(),
                    user_id: user.into(),
                    alert_type: "quota_allow_but_alert".into(),
                    threshold_value: check.warning_threshold,
                    current_value: check.daily_tokens_used,
                    limit_value: check.daily_tokens_limit.unwrap_or(0),
                    message: check.message,
                })
                .await;
        }
        Ok(None)
    }
}
impl LangGraphPayloadPreparation for LiveLangGraphPreparation {
    fn prepare<'a>(
        &'a self,
        request: &'a UnifiedRequest,
        service: &'a ServiceHandle,
        path: &'a str,
        mut payload: Value,
    ) -> AdapterFuture<'a, Value> {
        Box::pin(async move {
            let definition = service.snapshot();
            let connector = &definition.connector_config;
            let assistant = payload.get("assistant_id").cloned().unwrap_or(Value::Null);
            let config = json!({"service_id":definition.service_id,"service_name":definition.name,"assistant_id":assistant,"metadata":definition.metadata});
            let user = CompatUser {
                user_id: request.user_id.clone(),
                tenant_id: request.tenant_id.clone(),
                tier: "anonymous".into(),
                authenticated: !request.user_id.is_empty(),
                roles: vec![
                    if request.user_id.is_empty() {
                        "guest"
                    } else {
                        "user"
                    }
                    .into(),
                ],
                request_id: request.request_id.clone(),
                trace_id: request.request_id.clone(),
                ..Default::default()
            };
            langgraph_body::prepare_run_payload(&mut payload, path, &user, &config, None);
            let requested = extract_model(&payload).unwrap_or_default();
            let runtime = async {
                if python_truthy(&connector["model_override"]["enabled"]) {
                    self.resolve_runtime(
                        &request.tenant_id,
                        &definition.service_id,
                        &connector["model_override"],
                    )
                    .await
                    .map(Some)
                } else {
                    Ok(None)
                }
            };
            let (runtime, downgraded) =
                tokio::join!(runtime, self.apply_adapter_quota(request, &mut payload));
            let runtime = runtime.map_err(|error| match error {
                LivePreparationError::Invalid(code) => DispatchError::AdapterValidationDetail(
                    format!("LangGraph model_override invalid: {code}"),
                ),
                LivePreparationError::Unavailable => DispatchError::AdapterValidationFailed(
                    "LangGraph model control plane is not initialized",
                ),
            })?;
            let downgraded = downgraded?;
            if let Some(runtime) = runtime {
                payload["config"]["configurable"]["gateway_model"] = runtime;
            }
            if let Some(model) = downgraded {
                langgraph_body::apply_model_downgrade(&mut payload, &model, &requested);
            }
            Ok(payload)
        })
    }
}
fn id_or(value: &Value, fallback: &str) -> String {
    if python_truthy(value) {
        python_value_to_string(value).trim().into()
    } else {
        fallback.into()
    }
}
pub(crate) fn payload_tokens(value: &Value) -> i64 {
    if value.is_null() {
        return 0;
    }
    fn walk(value: &Value, key: &str) -> i64 {
        let text = matches!(
            key,
            "prompt"
                | "query"
                | "question"
                | "instruction"
                | "text"
                | "content"
                | "message"
                | "messages"
                | "input"
        );
        match value {
            Value::String(v) if text => estimate_tokens(v),
            Value::Array(v) => v.iter().map(|v| walk(v, key)).sum(),
            Value::Object(v) => v
                .iter()
                .map(|(k, v)| walk(v, &k.trim().to_lowercase()))
                .sum(),
            _ => 0,
        }
    }
    if let Value::String(v) = value {
        return estimate_tokens(v);
    }
    let estimate = walk(value, "");
    if estimate > 0 {
        estimate
    } else {
        (value.to_string().chars().count() as i64 / 4).max(1)
    }
}
