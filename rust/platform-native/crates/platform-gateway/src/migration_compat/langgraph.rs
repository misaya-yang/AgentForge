//! LangGraph REST compatibility backed by native HTTP, Redis, auth, quota and usage.
use super::{
    common::{ByteReceiver, CompatError, CompatUser, SseDecoder, header, object, sse, text},
    langgraph_body,
};
use crate::{
    billing::{OverageStrategy, check_quota},
    model_failover::{ResolvedRuntimeSnapshot, build_runtime_model_override_config},
    quota_store::PgQuotaStore,
    rate_limits::{
        MultiDimensionConfig, QuotaRpmLimiter, RateLimitContext, SlidingWindowRateLimiter,
    },
    registry_load_balancer::{LoadBalancerRuntime, SystemLoadBalancerRuntime},
    service_registry::python_truthy,
    usage_recorder::{UsageRecord, UsageRecorder},
};
use ai_platform_auth::admission::AdmissionService;
use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use redis::{AsyncCommands, aio::ConnectionManager};
use reqwest::Client;
use serde_json::{Value, json};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub struct LangGraphInstance {
    pub id: String,
    pub url: String,
    pub healthy: bool,
    pub weight: f64,
    pub average_latency: f64,
    active: AtomicUsize,
}
impl LangGraphInstance {
    pub fn new(id: String, url: String) -> Self {
        Self {
            id,
            url,
            healthy: true,
            weight: 1.0,
            average_latency: 0.0,
            active: AtomicUsize::new(0),
        }
    }
}
struct RunLease(Arc<LangGraphInstance>);
impl Drop for RunLease {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct LangGraphGovernance {
    pub quota: Option<Arc<PgQuotaStore>>,
    pub rpm: Arc<QuotaRpmLimiter>,
    pub runtime_snapshot: Option<ResolvedRuntimeSnapshot>,
    /// Actual shared Models/Providers resolution. The immutable snapshot remains
    /// an explicit compatibility input and is never presented as a live source.
    pub live_resolution: Option<
        Arc<crate::migration_host::bootstrap::langgraph_preparation::LiveLangGraphPreparation>,
    >,
    /// Shared source-TTL loader, also invalidated by service administration.
    /// A live miss uses the canonical synthetic config, never the legacy map.
    pub live_services:
        Option<Arc<crate::migration_host::bootstrap::foundation_backends::PgProxyConfigLoader>>,
    pub services: HashMap<String, Value>,
    pub allowed_models: Vec<String>,
    pub quota_fail_closed: bool,
}
impl LangGraphGovernance {
    async fn service_config(&self, assistant_id: &str) -> Result<Value, CompatError> {
        let candidates = [assistant_id, "langgraph", "langgraph-agent"];
        if let Some(loader) = &self.live_services {
            for name in candidates.into_iter().filter(|name| !name.is_empty()) {
                let Some(loaded) = loader.load(name).await.map_err(|_| {
                    CompatError::new(503, "LangGraph service configuration unavailable")
                })?
                else {
                    continue;
                };
                // This is a request-local projection, not an independent cache.
                // Authentication secrets stay in the shared proxy configuration.
                return Ok(json!({
                    "service_id": loaded.proxy.service_id,
                    "service_name": loaded.proxy.service_name,
                    "assistant_id": loaded.proxy.assistant_id,
                    "graph_id": loaded.proxy.graph_id,
                    "default_model": loaded.proxy.default_model,
                    "default_provider": loaded.proxy.default_provider,
                    "metadata": loaded.proxy.metadata,
                    "model_override": loaded.model_override,
                }));
            }
        } else {
            for name in candidates.into_iter().filter(|name| !name.is_empty()) {
                if let Some(config) = self.services.get(name) {
                    return Ok(config.clone());
                }
            }
        }
        Ok(json!({"service_id":"langgraph","service_name":"langgraph",
            "metadata":{"adapter_type":"langgraph"}}))
    }

    pub async fn prepare(
        &self,
        payload: &mut Value,
        path: &str,
        user: &CompatUser,
        assistant: Option<&Value>,
    ) -> Result<(), CompatError> {
        self.prepare_with_allowed_models(payload, path, user, assistant, &[])
            .await
    }

    // Verified key restrictions are request-local; never store them on the shared client.
    async fn prepare_with_allowed_models(
        &self,
        payload: &mut Value,
        path: &str,
        user: &CompatUser,
        assistant: Option<&Value>,
        allowed_models: &[String],
    ) -> Result<(), CompatError> {
        let assistant_id = text(payload, "assistant_id").trim().to_owned();
        let mut config = self.service_config(&assistant_id).await?;
        if !assistant_id.is_empty() {
            config["assistant_id"] = json!(assistant_id);
        }
        langgraph_body::prepare_run_payload(payload, path, user, &config, assistant);
        let requested = crate::usage_parser::extract_model(payload)
            .unwrap_or_else(|| text(&config, "default_model").to_owned());
        for restriction in [self.allowed_models.as_slice(), allowed_models] {
            if !model_allowed(restriction, &requested) {
                let requested = requested.trim_matches(crate::usage_parser::python_whitespace);
                return Err(CompatError::new(
                    403,
                    format!(
                        "Permission denied: model '{}' not allowed for this API key",
                        if requested.is_empty() {
                            "<empty>"
                        } else {
                            requested
                        }
                    ),
                ));
            }
        }
        let runtime = if config["model_override"]["enabled"].as_bool() == Some(true) {
            Some(if let Some(live) = &self.live_resolution {
                live.resolve_runtime(
                    &user.tenant_id,
                    text(&config, "service_id"),
                    &config["model_override"],
                )
                .await
                .map_err(|error| {
                    use crate::migration_host::bootstrap::langgraph_preparation::LivePreparationError;
                    match error {
                        LivePreparationError::Invalid(detail) => CompatError::new(422, detail),
                        LivePreparationError::Unavailable => CompatError::new(
                            503, "MODEL_OVERRIDE_CONTROL_PLANE_UNAVAILABLE",
                        ),
                    }
                })?
            } else {
                let snapshot = self.runtime_snapshot.as_ref().ok_or_else(|| {
                    CompatError::new(503, "MODEL_OVERRIDE_CONTROL_PLANE_UNAVAILABLE")
                })?;
                build_runtime_model_override_config(
                    &user.tenant_id,
                    &config["model_override"],
                    snapshot,
                )
                .map_err(|error| CompatError::new(422, error.to_string()))?
                .into_internal_value()
            })
        } else {
            None
        };
        let mut downgraded = None;
        if let Some(quota) = &self.quota {
            let stored = quota
                .try_get_or_create_quota_at(&user.tenant_id, &user.user_id, chrono::Utc::now())
                .await;
            match stored {
                Ok(stored) => {
                    let estimated = ((payload.to_string().chars().count() + 3) / 4) as i64;
                    let check = check_quota(
                        stored.as_ref().map(|s| &s.quota),
                        estimated,
                        chrono::Utc::now(),
                        |limit| self.rpm.check(&user.tenant_id, &user.user_id, Some(limit)),
                    )
                    .map_err(|_| CompatError::new(503, "QUOTA_CHECK_UNAVAILABLE"))?;
                    if !check.can_proceed() {
                        return Err(CompatError::value(
                            if check.overage_strategy == OverageStrategy::RateLimit {
                                429
                            } else {
                                403
                            },
                            json!({"message":check.message,"status":check.to_dict()["status"],"policy":check.to_dict()["overage_strategy"]}),
                        ));
                    }
                    downgraded = check.downgraded_model;
                    if let Some(model) = &downgraded {
                        payload["model"] = json!(model);
                    }
                }
                Err(_) if self.quota_fail_closed => {
                    return Err(CompatError::new(503, "QUOTA_CHECK_UNAVAILABLE"));
                }
                Err(_) => {}
            }
        }
        if let Some(runtime) = runtime {
            payload["config"]["configurable"]["gateway_model"] = runtime;
        }
        if let Some(model) = downgraded {
            langgraph_body::apply_model_downgrade(payload, &model, &requested);
        }
        Ok(())
    }
}

// Source: src/proxy/langgraph_governance.py normalize_allowed_models/is_model_allowed.
fn model_allowed(allowed_models: &[String], model: &str) -> bool {
    let patterns: Vec<_> = allowed_models
        .iter()
        .map(|s| s.trim_matches(crate::usage_parser::python_whitespace))
        .filter(|s| !s.is_empty())
        .collect();
    if patterns.is_empty() {
        return true;
    }
    let model = model
        .trim_matches(crate::usage_parser::python_whitespace)
        .to_lowercase();
    if model.is_empty() {
        return false;
    }
    patterns.iter().any(|pattern| {
        let pattern = pattern.to_lowercase();
        pattern
            .strip_suffix('*')
            .map_or_else(|| model == pattern, |prefix| model.starts_with(prefix))
    })
}

pub struct LangGraphClient {
    client: Client,
    pub instances: Vec<Arc<LangGraphInstance>>,
    pub strategy: String,
    token: String,
    rr: AtomicUsize,
    redis: Option<ConnectionManager>,
    cache: Mutex<HashMap<String, (Instant, Value)>>,
    pub governance: LangGraphGovernance,
    pub usage: Option<Arc<UsageRecorder>>,
    pub pricing: Option<Arc<dyn crate::usage_recorder::UsageSnapshotResolver>>,
    pub traces: Option<Arc<crate::migration_trace::TraceRepository>>,
    pub limiter: Option<Arc<SlidingWindowRateLimiter>>,
    pub rate_config: MultiDimensionConfig,
}
impl LangGraphClient {
    pub fn new(
        instances: Vec<Arc<LangGraphInstance>>,
        token: String,
        redis: Option<ConnectionManager>,
        governance: LangGraphGovernance,
    ) -> Result<Self, CompatError> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(300))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()?;
        Ok(Self {
            client,
            instances,
            strategy: "round_robin".into(),
            token,
            rr: AtomicUsize::new(0),
            redis,
            cache: Mutex::new(HashMap::new()),
            governance,
            usage: None,
            pricing: None,
            traces: None,
            limiter: None,
            rate_config: MultiDimensionConfig::default(),
        })
    }
    fn select(&self) -> Result<Arc<LangGraphInstance>, CompatError> {
        let healthy: Vec<_> = self
            .instances
            .iter()
            .filter(|i| i.healthy)
            .cloned()
            .collect();
        if healthy.is_empty() {
            return Err(CompatError::new(
                503,
                "No healthy LangGraph instance available",
            ));
        }
        let selected = match self.strategy.as_str() {
            "least_connections" => healthy
                .iter()
                .min_by_key(|i| i.active.load(Ordering::Relaxed))
                .unwrap()
                .clone(),
            "latency_based" => healthy
                .iter()
                .min_by(|a, b| a.average_latency.total_cmp(&b.average_latency))
                .unwrap()
                .clone(),
            "weighted" => {
                let total: f64 = healthy.iter().map(|i| i.weight).sum();
                let draw = SystemLoadBalancerRuntime.random_unit() * total;
                let mut cumulative = 0.0;
                healthy
                    .iter()
                    .find(|i| {
                        cumulative += i.weight;
                        draw <= cumulative
                    })
                    .unwrap_or_else(|| healthy.last().unwrap())
                    .clone()
            }
            _ => healthy[self.rr.fetch_add(1, Ordering::Relaxed) % healthy.len()].clone(),
        };
        Ok(selected)
    }
    pub fn headers(&self, user: &CompatUser) -> Result<HeaderMap, CompatError> {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("content-type", "application/json"),
            ("x-user-id", user.user_id.as_str()),
            (
                "x-user-type",
                if user.authenticated { "user" } else { "guest" },
            ),
            ("x-tenant-id", user.tenant_id.as_str()),
            ("x-user-tier", user.tier.as_str()),
        ] {
            headers.insert(
                HeaderName::from_static(name),
                HeaderValue::from_str(value)
                    .map_err(|_| CompatError::new(400, "Invalid context header"))?,
            );
        }
        if !self.token.is_empty() {
            headers.insert(
                "x-api-key",
                HeaderValue::from_str(&self.token)
                    .map_err(|_| CompatError::new(503, "Invalid upstream authentication"))?,
            );
            headers.insert(
                "authorization",
                HeaderValue::from_str(&format!("Bearer {}", self.token))
                    .map_err(|_| CompatError::new(503, "Invalid upstream authentication"))?,
            );
        }
        Ok(headers)
    }
    async fn raw(
        &self,
        user: &CompatUser,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
        instance: Option<&LangGraphInstance>,
    ) -> Result<reqwest::Response, CompatError> {
        let selected = if instance.is_none() {
            Some(self.select()?)
        } else {
            None
        };
        let instance = instance.unwrap_or_else(|| selected.as_ref().unwrap());
        let mut request = self
            .client
            .request(
                method,
                format!("{}{}", instance.url.trim_end_matches('/'), path),
            )
            .headers(self.headers(user)?)
            .query(query);
        if let Some(body) = body {
            request = request.json(body);
        }
        Ok(request.send().await?)
    }
    async fn json(
        &self,
        user: &CompatUser,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Value, CompatError> {
        let response = self.raw(user, method, path, query, body, None).await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if status.is_client_error() || status.is_server_error() {
            return Err(CompatError::new(
                if status == StatusCode::NOT_FOUND {
                    404
                } else {
                    500
                },
                String::from_utf8_lossy(&bytes)
                    .chars()
                    .take(500)
                    .collect::<String>(),
            ));
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|_| CompatError::new(502, "Invalid upstream JSON"))
    }
    async fn cached(&self, key: &str, ttl: u64, redis_key: &str) -> Option<Value> {
        if let Some((at, value)) = self
            .cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(key)
        {
            if at.elapsed() < Duration::from_secs(ttl) {
                return Some(value.clone());
            }
        }
        if let Some(mut redis) = self.redis.clone() {
            if let Ok(Some(raw)) = redis.get::<_, Option<String>>(redis_key).await {
                if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                    if python_truthy(&value) {
                        self.cache
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .insert(key.into(), (Instant::now(), value.clone()));
                        return Some(value);
                    }
                }
            }
        }
        None
    }
    async fn save_cache(&self, key: String, redis_key: String, value: &Value, ttl: u64) {
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, (Instant::now(), value.clone()));
        if let Some(mut redis) = self.redis.clone() {
            let _: Result<(), _> = redis.set_ex(redis_key, value.to_string(), ttl).await;
        }
    }
    async fn invalidate_thread(&self, user: &CompatUser, id: &str) {
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&format!("thread:{id}:{}", user.user_id));
        if let Some(mut redis) = self.redis.clone() {
            let _: Result<usize, _> = redis.del(format!("lg:thread:{id}")).await;
        }
    }
    pub async fn get_thread(&self, user: &CompatUser, id: &str) -> Result<Value, CompatError> {
        let key = format!("thread:{id}:{}", user.user_id);
        let redis_key = format!("lg:thread:{id}");
        if let Some(value) = self.cached(&key, 60, &redis_key).await {
            verify_thread(&value, user)?;
            return Ok(value);
        }
        let value = self
            .json(user, Method::GET, &format!("/threads/{id}"), &[], None)
            .await?;
        verify_thread(&value, user)?;
        self.save_cache(key, redis_key, &value, 60).await;
        Ok(value)
    }
    pub async fn get_assistant(
        &self,
        user: &CompatUser,
        id: &str,
        write: bool,
    ) -> Result<Value, CompatError> {
        let key = format!("assistant:{id}:{}", user.user_id);
        let redis_key = format!("lg:assistant:{id}");
        let value = match self.cached(&key, 300, &redis_key).await {
            Some(value) => value,
            None => {
                self.json(user, Method::GET, &format!("/assistants/{id}"), &[], None)
                    .await?
            }
        };
        verify_assistant(&value, user, write)?;
        self.save_cache(key, redis_key, &value, 300).await;
        Ok(value)
    }
    pub async fn list_assistants(
        &self,
        user: &CompatUser,
        limit: usize,
        offset: usize,
        metadata: Option<Value>,
    ) -> Result<Value, CompatError> {
        let cacheable =
            limit == 10 && offset == 0 && metadata.as_ref().is_none_or(|v| !python_truthy(v));
        let key = format!("assistants:{}", user.user_id);
        let redis_key = format!("lg:assistants_list:{}", user.user_id);
        if cacheable {
            if let Some(value) = self.cached(&key, 60, &redis_key).await {
                return Ok(value);
            }
        }
        let mut query = json!({"limit":if user.tier=="admin"{limit}else{limit*3},"offset":offset});
        if let Some(metadata) = metadata.filter(python_truthy) {
            query["metadata"] = json!(metadata.to_string());
        }
        let raw = self
            .json(user, Method::POST, "/assistants/search", &[], Some(&query))
            .await?;
        let result = filter_assistants(raw, user, limit);
        if cacheable {
            self.save_cache(key, redis_key, &result, 60).await;
        }
        Ok(result)
    }
    pub async fn create_thread(
        &self,
        user: &CompatUser,
        body: &Value,
    ) -> Result<Value, CompatError> {
        if !matches!(user.tier.as_str(), "admin" | "enterprise") {
            let limit = match user.tier.as_str() {
                "normal" => 10,
                "premium" => 100,
                _ => 1,
            };
            let count = if let Some(mut redis) = self.redis.clone() {
                redis
                    .get::<_, Option<i64>>(format!("lg:quota:{}:threads", user.user_id))
                    .await
                    .ok()
                    .map(|v| v.unwrap_or(0))
            } else {
                None
            };
            if count.is_none_or(|count| count >= limit) {
                return Err(CompatError::new(
                    429,
                    format!(
                        "Thread quota exceeded: {}/{}",
                        count.map(|v| v.to_string()).unwrap_or_else(|| "inf".into()),
                        limit
                    ),
                ));
            }
        }
        let mut metadata = object(&body["metadata"]);
        metadata.extend(object(&json!({"owner_id":user.user_id,"tenant_id":user.tenant_id,"user_tier":user.tier,"created_at":chrono::Utc::now().naive_utc().format("%Y-%m-%dT%H:%M:%S%.6f").to_string()})));
        let value=self.json(user,Method::POST,"/threads",&[],Some(&json!({"metadata":metadata,"if_exists":body.get("if_exists").cloned().unwrap_or(json!("do_nothing"))}))).await?;
        if !text(&value, "thread_id").is_empty() {
            self.save_cache(
                format!("thread:{}:{}", text(&value, "thread_id"), user.user_id),
                format!("lg:thread:{}", text(&value, "thread_id")),
                &value,
                60,
            )
            .await;
        }
        if let Some(mut redis) = self.redis.clone() {
            let _: Result<i64, _> = redis
                .incr(format!("lg:quota:{}:threads", user.user_id), 1)
                .await;
        }
        Ok(value)
    }
    async fn record(
        &self,
        user: &CompatUser,
        assistant: &str,
        start: Instant,
        kind: &str,
        status: &str,
        tokens: (i64, i64),
    ) {
        if let Some(usage) = &self.usage {
            let mut record = UsageRecord::new(
                &user.tenant_id,
                &user.user_id,
                "langgraph-agent",
                chrono::Utc::now(),
            );
            record.service_id = "langgraph".into();
            record.assistant_id = assistant.into();
            record.request_type = kind.into();
            record.status = status.into();
            record.input_tokens = tokens.0;
            record.output_tokens = tokens.1;
            record.latency_ms = start.elapsed().as_millis() as i64;
            if let Some(pricing) = &self.pricing {
                let _ = usage.record_with_resolver(record, pricing.clone()).await;
            } else {
                usage.record_priced(record).await;
            }
        }
    }
    pub async fn run(
        self: &Arc<Self>,
        user: CompatUser,
        path: &str,
        payload: Value,
        streaming: bool,
    ) -> Result<Response, CompatError> {
        self.run_with_allowed_models(user, path, payload, streaming, &[])
            .await
    }

    async fn run_with_allowed_models(
        self: &Arc<Self>,
        user: CompatUser,
        path: &str,
        mut payload: Value,
        streaming: bool,
        allowed_models: &[String],
    ) -> Result<Response, CompatError> {
        let assistant_id = text(&payload, "assistant_id").to_owned();
        if assistant_id.is_empty() {
            return Err(CompatError::new(422, "assistant_id required"));
        }
        let assistant = if let Some(id) = langgraph_body::thread_id(path) {
            let (_, assistant) = tokio::try_join!(
                self.get_thread(&user, id),
                self.get_assistant(&user, &assistant_id, false)
            )?;
            assistant
        } else {
            self.get_assistant(&user, &assistant_id, false).await?
        };
        let instance = self.select()?;
        instance.active.fetch_add(1, Ordering::Relaxed);
        let lease = RunLease(instance.clone());
        let start = Instant::now();
        let prepared = self
            .governance
            .prepare_with_allowed_models(
                &mut payload,
                path,
                &user,
                Some(&assistant),
                allowed_models,
            )
            .await;
        if let Err(error) = prepared {
            self.record(&user, &assistant_id, start, "run", "error", (0, 0))
                .await;
            return Err(error);
        }
        if !streaming {
            let kind = if path.ends_with("/runs") {
                "run_async"
            } else if path == "/runs/wait" {
                "run_stateless"
            } else {
                "run"
            };
            let outcome = async {
                let response = self
                    .raw(
                        &user,
                        Method::POST,
                        path,
                        &[],
                        Some(&payload),
                        Some(&instance),
                    )
                    .await?;
                let status = response.status();
                let body = response.bytes().await?;
                if !status.is_success() {
                    return Err(CompatError::new(
                        500,
                        String::from_utf8_lossy(&body).to_string(),
                    ));
                }
                serde_json::from_slice::<Value>(&body)
                    .map_err(|_| CompatError::new(502, "Invalid upstream JSON"))
            }
            .await;
            let tokens = if kind == "run_async" {
                (0, 0)
            } else {
                outcome
                    .as_ref()
                    .map(|v| usage_tokens(&v["usage"]))
                    .unwrap_or_default()
            };
            drop(lease);
            self.record(
                &user,
                &assistant_id,
                start,
                kind,
                if outcome.is_ok() { "success" } else { "error" },
                tokens,
            )
            .await;
            return outcome.map(|v| Json(v).into_response());
        }
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let client = self.clone();
        let path = path.to_owned();
        tokio::spawn(async move {
            let mut tokens = (0, 0);
            let mut status = "success";
            let result:Result<(),CompatError>=async{
                let response=tokio::select!{_ = tx.closed()=>return Ok(()),result=client.raw(&user,Method::POST,&path,&[],Some(&payload),Some(&instance))=>result?};
                if !response.status().is_success(){return Err(CompatError::new(500,"LangGraph upstream request failed"));}
                let mut response=response;let mut decoder=SseDecoder::default();
                let mut heartbeat=tokio::time::interval(Duration::from_secs(15));heartbeat.tick().await;
                loop{tokio::select!{
                    _=tx.closed()=>break,
                    _=heartbeat.tick()=>{if tx.send(Ok(Bytes::from_static(b": heartbeat\n\n"))).await.is_err(){break;}},
                    chunk=response.chunk()=>{let Some(chunk)=chunk?else{break;};for(event,data)in decoder.push(&chunk){if data=="[DONE]"{continue;}if let Ok(value)=serde_json::from_str::<Value>(&data){let usage=extract_usage(&value);tokens.0+=usage.0;tokens.1+=usage.1;if tx.send(Ok(sse(&event,&value))).await.is_err(){return Ok(());}}}}
                }}Ok(())
            }.await;
            if let Err(error) = result {
                status = "error";
                let _ = tx
                    .send(Ok(sse("error", &json!({"error":error.to_string()}))))
                    .await;
            }
            drop(lease);
            client
                .record(&user, &assistant_id, start, "run", status, tokens)
                .await;
        });
        Ok((
            [
                ("content-type", "text/event-stream"),
                ("cache-control", "no-cache"),
                ("x-accel-buffering", "no"),
            ],
            Body::from_stream(ByteReceiver(rx)),
        )
            .into_response())
    }
    pub async fn dispatch(
        self: &Arc<Self>,
        user: CompatUser,
        method: Method,
        path: &str,
        query: Vec<(String, String)>,
        body: Value,
    ) -> Result<Response, CompatError> {
        self.dispatch_with_headers(user, method, path, query, body, HeaderMap::new())
            .await
    }
    pub async fn dispatch_with_headers(
        self: &Arc<Self>,
        user: CompatUser,
        method: Method,
        path: &str,
        query: Vec<(String, String)>,
        body: Value,
        incoming_headers: HeaderMap,
    ) -> Result<Response, CompatError> {
        self.dispatch_with_allowed_models(user, method, path, query, body, incoming_headers, &[])
            .await
    }

    async fn dispatch_with_allowed_models(
        self: &Arc<Self>,
        user: CompatUser,
        method: Method,
        path: &str,
        query: Vec<(String, String)>,
        mut body: Value,
        incoming_headers: HeaderMap,
        allowed_models: &[String],
    ) -> Result<Response, CompatError> {
        // SDK search controls live in the JSON body. Normalize before the
        // legacy query-only pagination used by the other compatibility routes.
        if method == Method::POST && path.trim_matches('/') == "threads/search" {
            let payload = thread_search_payload(&body, &user, &query)?;
            return Ok(Json(
                self.json(&user, method, "/threads/search", &[], Some(&payload))
                    .await?,
            )
            .into_response());
        }
        let get_query = |key: &str| {
            query
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        let limit = get_query("limit")
            .unwrap_or("10")
            .parse::<usize>()
            .ok()
            .filter(|v| (1..=100).contains(v))
            .ok_or_else(|| CompatError::new(422, "Invalid limit"))?;
        let offset = get_query("offset")
            .unwrap_or("0")
            .parse::<usize>()
            .map_err(|_| CompatError::new(422, "Invalid offset"))?;
        let parts: Vec<_> = path.trim_matches('/').split('/').collect();
        if langgraph_body::is_run_path(method.as_str(), path) && method == Method::POST {
            return self
                .run_with_allowed_models(
                    user,
                    path,
                    body,
                    path.ends_with("/stream"),
                    allowed_models,
                )
                .await;
        }
        let result = match (method.as_str(), parts.as_slice()) {
            ("GET", ["assistants"]) => self.list_assistants(&user, limit, offset, None).await?,
            ("GET", ["assistants", id]) => self.get_assistant(&user, id, false).await?,
            ("POST", ["assistants"]) => {
                if !matches!(user.tier.as_str(), "admin" | "enterprise") {
                    return Err(CompatError::new(
                        403,
                        "Custom assistants require enterprise plan",
                    ));
                }
                if text(&body, "graph_id").is_empty() {
                    return Err(CompatError::new(422, "graph_id required"));
                }
                let mut metadata = object(&body["metadata"]);
                metadata.insert("created_by".into(), json!(user.user_id));
                metadata.insert("tenant_id".into(), json!(user.tenant_id));
                body["metadata"] = json!(metadata);
                self.json(&user, method, path, &[], Some(&body)).await?
            }
            ("POST", ["threads"]) => self.create_thread(&user, &body).await?,
            ("GET", ["threads", id]) => self.get_thread(&user, id).await?,
            ("PATCH" | "DELETE", ["threads", id]) => {
                self.get_thread(&user, id).await?;
                let result = self
                    .json(
                        &user,
                        method.clone(),
                        path,
                        &query,
                        if method == Method::DELETE {
                            None
                        } else {
                            Some(&body)
                        },
                    )
                    .await?;
                self.invalidate_thread(&user, id).await;
                if method == Method::DELETE {
                    if let Some(mut redis) = self.redis.clone() {
                        let _: Result<i64, _> = redis
                            .decr(format!("lg:quota:{}:threads", user.user_id), 1)
                            .await;
                    }
                    json!({"status":"ok"})
                } else {
                    result
                }
            }
            (_, ["store", "items"]) => {
                let namespace = if method == Method::POST {
                    body["namespace"].clone()
                } else {
                    serde_json::from_str(get_query("namespace").unwrap_or(""))
                        .map_err(|_| CompatError::new(400, "Invalid namespace format"))?
                };
                let ns: Vec<String> = serde_json::from_value(namespace)
                    .map_err(|_| CompatError::new(400, "Invalid namespace format"))?;
                if !namespace_access(&ns, &user, method != Method::GET) {
                    return Err(CompatError::new(403, "Access denied to this namespace"));
                }
                let result = self
                    .json(
                        &user,
                        method.clone(),
                        path,
                        &query,
                        if method == Method::POST {
                            Some(&body)
                        } else {
                            None
                        },
                    )
                    .await?;
                if method == Method::GET {
                    result
                } else {
                    json!({"status":"ok"})
                }
            }
            ("GET", ["store", "namespaces"]) => {
                let mut prefix: Vec<String> = match get_query("prefix") {
                    Some(value) => serde_json::from_str(value)
                        .map_err(|_| CompatError::new(400, "Invalid prefix format"))?,
                    None => vec![],
                };
                if user.tier != "admin" {
                    prefix.splice(..0, ["user".into(), user.user_id.clone()]);
                }
                self.json(
                    &user,
                    method,
                    path,
                    &[("prefix".into(), json!(prefix).to_string())],
                    None,
                )
                .await?
            }
            _ => {
                if parts.len() >= 2
                    && parts[0] == "threads"
                    && !matches!(parts[1], "search" | "count")
                {
                    self.get_thread(&user, parts[1]).await?;
                }
                if parts.len() >= 2
                    && parts[0] == "assistants"
                    && !matches!(parts[1], "search" | "count")
                {
                    self.get_assistant(&user, parts[1], false).await?;
                }
                return self
                    .passthrough(
                        user,
                        method.clone(),
                        path,
                        query,
                        if matches!(method, Method::POST | Method::PUT | Method::PATCH) {
                            Some(body)
                        } else {
                            None
                        },
                        incoming_headers,
                    )
                    .await;
            }
        };
        Ok(Json(result).into_response())
    }
    async fn passthrough(
        self: &Arc<Self>,
        user: CompatUser,
        method: Method,
        path: &str,
        query: Vec<(String, String)>,
        body: Option<Value>,
        incoming: HeaderMap,
    ) -> Result<Response, CompatError> {
        let instance = self.select()?;
        let started = chrono::Utc::now();
        let streaming =
            path.contains("/stream") || header(&incoming, "accept").contains("text/event-stream");
        let traceparent = incoming
            .get("traceparent")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut headers = self.headers(&user)?;
        for key in [
            "accept",
            "last-event-id",
            "traceparent",
            "tracestate",
            "x-request-id",
        ] {
            if let Some(value) = incoming.get(key) {
                headers.insert(
                    HeaderName::from_bytes(key.as_bytes()).unwrap(),
                    value.clone(),
                );
            }
        }
        let mut request = self
            .client
            .request(
                method.clone(),
                format!("{}{}", instance.url.trim_end_matches('/'), path),
            )
            .headers(headers)
            .query(&query);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let capture = {
            let traces = self.traces.clone();
            let user = user.clone();
            let method = method.to_string();
            let path = path.to_owned();
            move |status: &str, upstream: Option<u16>, error: Option<&str>| {
                if let Some(traces) = &traces {
                    traces.schedule_gateway_trace_ingest(
                        super::proxy_trace::build_langgraph_trace_payload(
                            &user,
                            &method,
                            &path,
                            started,
                            chrono::Utc::now(),
                            status,
                            upstream,
                            error,
                            traceparent.as_deref(),
                            streaming,
                        ),
                        90,
                        false,
                    );
                }
            }
        };
        if streaming {
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            tokio::spawn(async move {
                let mut upstream = None;
                let result:Result<(),CompatError>=async{let mut response=request.send().await?;upstream=Some(response.status().as_u16());if response.status().as_u16()>=400{let status=response.status().as_u16();let body=response.bytes().await?;return Err(CompatError::new(status,String::from_utf8_lossy(&body).chars().take(500).collect::<String>()));}let mut heartbeat=tokio::time::interval(Duration::from_secs(15));heartbeat.tick().await;loop{tokio::select!{_=tx.closed()=>break,_=heartbeat.tick()=>{if tx.send(Ok(Bytes::from_static(b": heartbeat\n\n"))).await.is_err(){break;}},chunk=response.chunk()=>{let Some(chunk)=chunk?else{break;};if tx.send(Ok(chunk)).await.is_err(){break;}}}}Ok(())}.await;
                match result {
                    Ok(()) => capture("succeeded", upstream, None),
                    Err(error) => {
                        capture("failed", upstream, Some(&error.to_string()));
                        let _ = tx.send(Err(std::io::Error::other(error.to_string()))).await;
                    }
                }
            });
            return Ok((
                [
                    ("content-type", "text/event-stream"),
                    ("cache-control", "no-cache"),
                    ("x-accel-buffering", "no"),
                ],
                Body::from_stream(ByteReceiver(rx)),
            )
                .into_response());
        }
        let response = request.send().await?;
        let status = response.status();
        let content_type = response.headers().get("content-type").cloned();
        let bytes = response.bytes().await?;
        let error = String::from_utf8_lossy(&bytes)
            .chars()
            .take(500)
            .collect::<String>();
        capture(
            if status.as_u16() < 400 {
                "succeeded"
            } else {
                "failed"
            },
            Some(status.as_u16()),
            if status.as_u16() < 400 {
                None
            } else {
                Some(&error)
            },
        );
        if content_type
            .as_ref()
            .is_some_and(|v| v.to_str().unwrap_or("").contains("application/json"))
        {
            return Ok(Json(
                serde_json::from_slice::<Value>(&bytes)
                    .map_err(|_| CompatError::new(502, "Invalid upstream JSON"))?,
            )
            .into_response());
        }
        let mut result = (status, bytes).into_response();
        if let Some(content_type) = content_type {
            result.headers_mut().insert("content-type", content_type);
        }
        Ok(result)
    }
}

/// Normalize the standard SDK JSON envelope and the historical flat-metadata
/// body through one authority. Explicit query pagination retains precedence.
/// Reserved search keys identify an SDK envelope; metadata using those names
/// must be nested under `metadata` to avoid the inherently ambiguous flat form.
fn thread_search_payload(
    body: &Value,
    user: &CompatUser,
    query: &[(String, String)],
) -> Result<Value, CompatError> {
    let body = match body {
        Value::Null => serde_json::Map::new(),
        Value::Object(body) => body.clone(),
        _ => return Err(CompatError::new(422, "Thread search body must be an object")),
    };
    let envelope = [
        "ids", "metadata", "values", "status", "limit", "offset", "sort_by",
        "sort_order", "select", "extract",
    ]
    .iter()
    .any(|key| body.contains_key(*key));
    let mut payload = if envelope { body } else { object(&json!({"metadata":body})) };
    let mut metadata = match payload.get("metadata") {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(metadata)) => metadata.clone(),
        _ => return Err(CompatError::new(422, "Thread search metadata must be an object")),
    };
    let pagination = |key: &str, default: u64| -> Result<u64, CompatError> {
        let invalid = || CompatError::new(422, format!("Invalid {key}"));
        if let Some((_, value)) = query.iter().find(|(name, _)| name == key) {
            value.parse::<u64>().map_err(|_| invalid())
        } else {
            payload.get(key).map_or(Ok(default), |value| value.as_u64().ok_or_else(invalid))
        }
    };
    let limit = pagination("limit", 10)?;
    let offset = pagination("offset", 0)?;
    // Preserve the Gateway's existing bounded search contract, which is
    // intentionally narrower than the upstream server's maximum page size.
    if !(1..=100).contains(&limit) {
        return Err(CompatError::new(422, "Invalid limit"));
    }
    if user.tier != "admin"
        && user.user_id != "admin"
        && !user.roles.iter().any(|role| role == "admin")
        && !user.user_id.is_empty()
    {
        metadata.insert("owner_id".into(), json!(user.user_id));
    }
    payload.insert("metadata".into(), json!(metadata));
    payload.insert("limit".into(), json!(limit));
    payload.insert("offset".into(), json!(offset));
    Ok(Value::Object(payload))
}

pub fn verify_thread(value: &Value, user: &CompatUser) -> Result<(), CompatError> {
    let metadata = &value["metadata"];
    if user.tier == "admin"
        || (user.tier == "enterprise" && metadata["tenant_id"] == json!(user.tenant_id))
        || metadata["owner_id"] == json!(user.user_id)
    {
        Ok(())
    } else {
        Err(CompatError::new(403, "Access denied to this thread"))
    }
}
pub fn verify_assistant(value: &Value, user: &CompatUser, write: bool) -> Result<(), CompatError> {
    let metadata = &value["metadata"];
    let shared = match &metadata["shared_with"] {
        Value::String(v) => v == &user.user_id,
        Value::Array(v) => v.iter().any(|v| v == &json!(user.user_id)),
        _ => false,
    };
    let public = match &metadata["is_public"] {
        Value::String(v) => matches!(v.trim().to_lowercase().as_str(), "true" | "1" | "yes" | "y"),
        v => python_truthy(v),
    };
    if user.tier == "admin"
        || metadata["created_by"] == json!(user.user_id)
        || (!write && (shared || public))
    {
        Ok(())
    } else {
        Err(CompatError::new(
            403,
            format!(
                "Access denied to assistant: {}",
                text(value, "assistant_id")
            ),
        ))
    }
}
pub fn filter_assistants(mut value: Value, user: &CompatUser, limit: usize) -> Value {
    if let Some(list) = value.as_array() {
        return json!(
            list.iter()
                .filter(|v| verify_assistant(v, user, false).is_ok())
                .take(limit)
                .cloned()
                .collect::<Vec<_>>()
        );
    }
    for key in ["items", "assistants", "data", "results"] {
        if value[key].is_array() {
            value[key] = filter_assistants(value[key].clone(), user, limit);
            let count = value[key].as_array().unwrap().len();
            if value.get("count").is_some() {
                value["count"] = json!(count);
            }
            if value["total"].is_i64() || value["total"].is_u64() {
                value["total"] = json!(count);
            }
            break;
        }
    }
    value
}
pub fn namespace_access(namespace: &[String], user: &CompatUser, write: bool) -> bool {
    if namespace.is_empty() {
        return false;
    }
    if user.tier == "admin" {
        return true;
    }
    match namespace[0].as_str() {
        "global" => !write,
        "user" => namespace.get(1) == Some(&user.user_id),
        "tenant" => {
            matches!(user.tier.as_str(), "premium" | "enterprise")
                && namespace.get(1) == Some(&user.tenant_id)
        }
        _ => false,
    }
}
fn usage_tokens(value: &Value) -> (i64, i64) {
    let token = |a: &str, b: &str| {
        value[a]
            .as_i64()
            .filter(|v| *v != 0)
            .or_else(|| value[b].as_i64())
            .unwrap_or(0)
    };
    (
        token("prompt_tokens", "input_tokens"),
        token("completion_tokens", "output_tokens"),
    )
}
fn extract_usage(value: &Value) -> (i64, i64) {
    let direct = usage_tokens(if python_truthy(&value["usage"]) {
        &value["usage"]
    } else {
        &value["data"]["usage"]
    });
    let metadata = usage_tokens(&value["metadata"]["usage"]);
    (direct.0.max(metadata.0), direct.1.max(metadata.1))
}
#[derive(Clone)]
struct HttpState {
    auth: Arc<AdmissionService>,
    client: Arc<LangGraphClient>,
}
pub fn router(auth: Arc<AdmissionService>, client: Arc<LangGraphClient>) -> Router {
    Router::new()
        .route("/api/v1/langgraph/{*path}", any(http))
        .with_state(HttpState { auth, client })
}
async fn http(State(state): State<HttpState>, request: Request) -> Response {
    let result = async {
        let admission = state.auth.request(request.headers(), request.uri().path());
        let principal = admission
            .resolve()
            .await
            .map_err(|error| CompatError::new(error.status(), error.to_string()))?;
        let mut user = CompatUser::from_admitted(principal, request.headers());
        user.apply_request_context(request.extensions());
        let allowed_models = principal
            .api_key()
            .map(|key| key.allowed_models.clone())
            .unwrap_or_default();
        drop(admission);
        let path = request
            .uri()
            .path()
            .trim_start_matches("/api/v1/langgraph")
            .to_owned();
        let method = request.method().clone();
        let query = reqwest::Url::parse(&format!("http://gateway{}", request.uri()))
            .map_err(|_| CompatError::new(400, "Invalid URL"))?
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect::<Vec<_>>();
        let incoming_headers = request.headers().clone();
        let bytes = to_bytes(request.into_body(), 16 * 1024 * 1024)
            .await
            .map_err(|_| CompatError::new(413, "Request body too large"))?;
        let body = if bytes.is_empty() {
            json!({})
        } else {
            serde_json::from_slice::<Value>(&bytes)
                .map_err(|_| CompatError::new(422, "Invalid JSON body"))?
        };
        let operation = if langgraph_body::is_run_path(method.as_str(), &path) {
            Some("run_create")
        } else if method == Method::POST && path == "/threads" {
            Some("thread_create")
        } else if method == Method::POST && path == "/store/items" {
            Some("store_write")
        } else {
            None
        };
        let mut rate_headers = HeaderMap::new();
        if let (Some(operation), Some(limiter)) = (operation, &state.client.limiter) {
            let context = RateLimitContext {
                user_id: Some(user.user_id.clone()),
                tenant_id: Some(user.tenant_id.clone()),
                user_tier: user.tier.clone(),
                assistant_id: body
                    .get("assistant_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                operation: Some(operation.into()),
                ..Default::default()
            };
            let policies = state
                .client
                .rate_config
                .policies(&context, &BTreeSet::new())
                .map_err(|_| CompatError::new(503, "Rate limit unavailable"))?;
            let result = limiter
                .check_http_many(&policies)
                .await
                .map_err(|_| CompatError::new(503, "Rate limit unavailable"))?;
            for (k, v) in result.result.headers() {
                if let (Ok(k), Ok(v)) = (
                    HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(&v),
                ) {
                    rate_headers.insert(k, v);
                }
            }
            if !result.result.allowed {
                let mut response =
                    CompatError::value(429, result.result.exceeded_response()).into_response();
                response.headers_mut().extend(rate_headers);
                return Ok(response);
            }
        }
        let mut response = state
            .client
            .dispatch_with_allowed_models(
                user,
                method,
                &path,
                query,
                body,
                incoming_headers,
                &allowed_models,
            )
            .await?;
        if path.ends_with("/stream") {
            response.headers_mut().extend(rate_headers);
        }
        Ok::<_, CompatError>(response)
    }
    .await;
    result.unwrap_or_else(IntoResponse::into_response)
}

impl LangGraphClient {
    pub async fn search_threads(
        &self,
        user: &CompatUser,
        metadata: Value,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Value>, CompatError> {
        let payload = thread_search_payload(
            &json!({"metadata":metadata,"limit":limit,"offset":offset}),
            user,
            &[],
        )?;
        let value = self
            .json(user, Method::POST, "/threads/search", &[], Some(&payload))
            .await?;
        value
            .as_array()
            .cloned()
            .ok_or_else(|| CompatError::new(502, "Invalid upstream thread list"))
    }
    pub async fn delete_thread(&self, user: &CompatUser, id: &str) -> Result<(), CompatError> {
        self.get_thread(user, id).await?;
        self.json(user, Method::DELETE, &format!("/threads/{id}"), &[], None)
            .await?;
        self.invalidate_thread(user, id).await;
        if let Some(mut redis) = self.redis.clone() {
            let _: Result<i64, _> = redis
                .decr(format!("lg:quota:{}:threads", user.user_id), 1)
                .await;
        }
        Ok(())
    }
    pub async fn thread_history(
        &self,
        user: &CompatUser,
        id: &str,
        limit: i64,
        before: Option<&str>,
    ) -> Result<Vec<Value>, CompatError> {
        self.get_thread(user, id).await?;
        let mut query = vec![("limit".into(), limit.to_string())];
        if let Some(before) = before.filter(|v| !v.is_empty()) {
            query.push(("before".into(), before.into()));
        }
        let value = self
            .json(
                user,
                Method::GET,
                &format!("/threads/{id}/history"),
                &query,
                None,
            )
            .await?;
        value
            .as_array()
            .cloned()
            .ok_or_else(|| CompatError::new(502, "Invalid upstream thread history"))
    }
    pub async fn run_wait(
        self: &Arc<Self>,
        user: CompatUser,
        id: &str,
        assistant: &str,
        input: Value,
        metadata: Option<Value>,
    ) -> Result<Value, CompatError> {
        let mut payload = json!({"assistant_id":assistant,"input":input,"config":{}});
        if let Some(metadata) = metadata {
            payload["metadata"] = metadata;
        }
        let response = self
            .run(user, &format!("/threads/{id}/runs/wait"), payload, false)
            .await?;
        let bytes = to_bytes(response.into_body(), 64 * 1024 * 1024)
            .await
            .map_err(|_| CompatError::new(502, "Invalid upstream run response"))?;
        serde_json::from_slice(&bytes)
            .map_err(|_| CompatError::new(502, "Invalid upstream run response"))
    }
}

#[cfg(test)]
mod thread_search_tests {
    use super::*;

    fn user() -> CompatUser {
        CompatUser {
            user_id: "sdk-user".into(),
            tenant_id: "sdk-tenant".into(),
            tier: "normal".into(),
            authenticated: true,
            ..Default::default()
        }
    }

    #[test]
    fn sdk_default_search_does_not_turn_pagination_into_metadata() {
        assert_eq!(thread_search_payload(&json!({"limit":10,"offset":0}), &user(), &[]).unwrap(),
            json!({"metadata":{"owner_id":"sdk-user"},"limit":10,"offset":0}));
    }

    #[test]
    fn sdk_controls_and_nested_filters_remain_at_their_original_level() {
        let body = json!({"metadata":{"project":"alpha"},"limit":3,"offset":2,
            "status":"idle","sort_by":"updated_at","sort_order":"desc",
            "ids":["00000000-0000-4000-8000-000000000001"],"values":{"topic":"sdk"},
            "select":["thread_id","metadata"],"extract":{"last":"values.messages[-1]"}});
        let mut expected = body.clone();
        expected["metadata"]["owner_id"] = json!("sdk-user");
        assert_eq!(thread_search_payload(&body, &user(), &[]).unwrap(), expected);
        assert_eq!(body["metadata"], json!({"project":"alpha"}));
    }

    #[test]
    fn non_admin_owner_is_authoritative_for_both_body_shapes() {
        for body in [json!({"owner_id":"other","project":"alpha"}),
            json!({"metadata":{"owner_id":"other","project":"alpha"},"limit":3})] {
            let payload = thread_search_payload(&body, &user(), &[]).unwrap();
            assert_eq!(payload["metadata"], json!({"owner_id":"sdk-user","project":"alpha"}));
        }
    }

    #[test]
    fn each_existing_admin_recognition_path_is_preserved() {
        let mut users = [user(), user(), user()];
        users[0].tier = "admin".into();
        users[1].user_id = "admin".into();
        users[2].roles = vec!["admin".into()];
        for user in users {
            let payload = thread_search_payload(&json!({"metadata":{"owner_id":"other"}}), &user, &[]).unwrap();
            assert_eq!(payload["metadata"], json!({"owner_id":"other"}));
        }
    }

    #[test]
    fn legacy_flat_metadata_and_query_pagination_are_retained() {
        let query = vec![("limit".into(),"7".into()),("offset".into(),"12".into())];
        let payload = thread_search_payload(&json!({"project":"alpha","custom":{"nested":true}}), &user(), &query).unwrap();
        assert_eq!(payload, json!({"metadata":{"project":"alpha","custom":{"nested":true},"owner_id":"sdk-user"},"limit":7,"offset":12}));
    }

    #[test]
    fn explicit_query_pagination_overrides_sdk_body_pagination() {
        let query = vec![("limit".into(),"7".into()),("offset".into(),"12".into())];
        let payload = thread_search_payload(&json!({"metadata":{},"limit":3,"offset":2}), &user(), &query).unwrap();
        assert_eq!(payload["limit"], json!(7));
        assert_eq!(payload["offset"], json!(12));
    }

    #[test]
    fn metadata_can_still_contain_reserved_sdk_control_names() {
        let metadata = json!({"limit":7,"offset":9,"status":"custom","metadata":{"x":true}});
        let payload = thread_search_payload(&json!({"metadata":metadata,"limit":1}), &user(), &[]).unwrap();
        let mut expected = metadata;
        expected["owner_id"] = json!("sdk-user");
        assert_eq!(payload["metadata"], expected);
        assert_eq!(payload["limit"], json!(1));
    }

    #[test]
    fn omitted_and_null_metadata_keep_authenticated_owner_filter() {
        for body in [Value::Null, json!({}), json!({"metadata":null}), json!({"status":"idle"})] {
            let payload = thread_search_payload(&body, &user(), &[]).unwrap();
            assert_eq!(payload["metadata"], json!({"owner_id":"sdk-user"}));
            assert_eq!(payload["limit"], json!(10));
            assert_eq!(payload["offset"], json!(0));
        }
    }

    #[test]
    fn invalid_body_or_nested_metadata_fails_before_upstream() {
        for body in [json!([]),json!(true),json!(1),json!("bad"),
            json!({"metadata":[]}),json!({"metadata":"bad"}),json!({"metadata":1})] {
            assert_eq!(thread_search_payload(&body, &user(), &[]).unwrap_err().status,422);
        }
    }

    #[test]
    fn invalid_body_pagination_fails_before_upstream() {
        for body in [json!({"limit":0}),json!({"limit":101}),json!({"limit":-1}),
            json!({"limit":true}),json!({"limit":1.5}),json!({"limit":"2"}),json!({"limit":null}),
            json!({"offset":-1}),json!({"offset":false}),json!({"offset":1.5}),json!({"offset":"2"})] {
            assert_eq!(thread_search_payload(&body, &user(), &[]).unwrap_err().status,422);
        }
    }

    #[test]
    fn invalid_query_pagination_fails_before_upstream() {
        for (key,value) in [("limit","0"),("limit","101"),("limit","no"),("limit","1.5"),
            ("offset","-1"),("offset","18446744073709551616")] {
            let query = vec![(key.into(),value.into())];
            assert_eq!(thread_search_payload(&json!({}), &user(), &query).unwrap_err().status,422);
        }
    }

    #[test]
    fn page_boundaries_and_first_duplicate_query_value_are_retained() {
        for limit in [1,100] {
            let body=json!({"limit":limit,"offset":0});
            let payload=thread_search_payload(&body,&user(),&[]).unwrap();
            assert_eq!(payload["limit"],json!(limit));
        }
        let query=vec![("limit".into(),"7".into()),("limit".into(),"0".into())];
        assert_eq!(thread_search_payload(&json!({}),&user(),&query).unwrap()["limit"],json!(7));
    }
}
