//! Native raw proxy lifecycle: admission before slots, upstream errors before SSE,
//! byte-preserving streams, accounting before final slot/lease release.
//! Lease loss drops owned local work, including blocked delivery/accounting. This
//! does not prove remote execution cessation or durable final billing.
use super::{
    common::{ByteReceiver, CompatError, CompatUser, object, text},
    langgraph_body,
};
use crate::{
    capacity::CapacityBudget,
    capacity_admission::{
        AdmissionCancellation, AdmissionError, AdmissionRequest, CapacityAdmissionController,
        CapacityLease,
    },
    registry_load_balancer::{LoadBalancerRuntime, SystemLoadBalancerRuntime},
    usage_parser::{
        TokenUsage, extract_assistant_id, extract_model, extract_provider, extract_token_usage,
    },
    usage_recorder::{UsageRecord, UsageRecorder},
};
use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ProxyServiceConfig {
    pub service_id: String,
    pub service_name: String,
    pub upstream_url: String,
    pub upstream_urls: Vec<String>,
    pub enabled: bool,
    pub assistant_id: Option<String>,
    pub graph_id: Option<String>,
    pub path_rewrite: Option<String>,
    pub auth_token: Option<String>,
    pub forward_auth: bool,
    pub load_balance_strategy: String,
    pub concurrency_limit: usize,
    pub streaming_concurrency_limit: Option<usize>,
    pub non_streaming_concurrency_limit: Option<usize>,
    pub concurrency_queue_timeout: f64,
    pub timeout_connect: f64,
    pub timeout_read: f64,
    pub health_check_timeout: f64,
    pub default_model: Option<String>,
    pub default_provider: Option<String>,
    pub metadata: Value,
    pub cache_enabled: bool,
    pub cache_ttl: i64,
}
impl Default for ProxyServiceConfig {
    fn default() -> Self {
        Self {
            service_id: String::new(),
            service_name: String::new(),
            upstream_url: String::new(),
            upstream_urls: vec![],
            enabled: true,
            assistant_id: None,
            graph_id: None,
            path_rewrite: None,
            auth_token: None,
            forward_auth: true,
            load_balance_strategy: "round_robin".into(),
            concurrency_limit: 32,
            streaming_concurrency_limit: None,
            non_streaming_concurrency_limit: None,
            concurrency_queue_timeout: 30.0,
            timeout_connect: 5.0,
            timeout_read: 300.0,
            health_check_timeout: 5.0,
            default_model: None,
            default_provider: None,
            metadata: json!({}),
            cache_enabled: false,
            cache_ttl: 3600,
        }
    }
}
impl ProxyServiceConfig {
    pub fn upstreams(&self) -> Vec<String> {
        let mut urls = self.upstream_urls.clone();
        if !self.upstream_url.is_empty() && !urls.contains(&self.upstream_url) {
            urls.insert(0, self.upstream_url.clone());
        }
        urls
    }
    pub fn url(&self, base: &str, path: &str) -> String {
        let path = if let Some(rewrite) = &self.path_rewrite {
            format!(
                "{}/{}",
                rewrite.trim_end_matches('/'),
                path.trim_start_matches('/')
            )
        } else {
            path.to_owned()
        };
        format!(
            "{}/{}",
            base.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }
}
pub struct ProxyRequest {
    pub config: ProxyServiceConfig,
    pub user: CompatUser,
    pub api_key_id: Option<String>,
    pub method: Method,
    pub path: String,
    pub body: Bytes,
    pub headers: HeaderMap,
    pub query: Vec<(String, String)>,
    pub stream: bool,
    pub langgraph_body_prepared: bool,
    pub effective_model: Option<String>,
    pub effective_provider: Option<String>,
    pub capacity_budgets: Vec<CapacityBudget>,
}
#[derive(Clone)]
struct Availability {
    checked: Instant,
    checked_at: f64,
    urls: Vec<String>,
    error: String,
}
#[derive(Default)]
struct Balance {
    counters: HashMap<String, usize>,
    connections: HashMap<(String, String), usize>,
}
struct ConnectionLease {
    balance: Arc<Mutex<Balance>>,
    key: (String, String),
}
impl Drop for ConnectionLease {
    fn drop(&mut self) {
        let mut balance = self.balance.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(count) = balance.connections.get_mut(&self.key) {
            *count = count.saturating_sub(1);
        }
    }
}
fn capacity_error(error: AdmissionError) -> CompatError {
    match error {
        AdmissionError::Rejected(rejected) => CompatError::value(
            rejected.status_code,
            json!({"code":rejected.code,"message":rejected.message,"budget_key":rejected.budget_key}),
        ),
        _ => CompatError::new(503, "Capacity unavailable"),
    }
}

/// One private signal spans the request-to-pump ownership transfer. It never
/// binds a caller task, and dropping the fenced future drops all of its owners.
#[derive(Clone)]
struct CapacityOwner {
    cancellation: AdmissionCancellation,
    lost_error: CompatError,
}
impl CapacityOwner {
    fn bind(capacity: Option<&CapacityLease>) -> Result<Self, CompatError> {
        let cancellation = AdmissionCancellation::default();
        let lost_error = if let Some(capacity) = capacity {
            capacity
                .bind_owner(cancellation.clone())
                .map_err(capacity_error)?;
            capacity_error(capacity.lost_error())
        } else {
            CompatError::new(503, "Capacity unavailable")
        };
        Ok(Self {
            cancellation,
            lost_error,
        })
    }
    async fn run<F: Future>(&self, work: F) -> Result<F::Output, CompatError> {
        tokio::select! { biased;
            _ = self.cancellation.cancelled() => Err(self.lost_error.clone()),
            result = work => Ok(result),
        }
    }
}

pub struct TransparentProxy {
    clients: Mutex<HashMap<String, reqwest::Client>>,
    slots: Mutex<HashMap<(String, bool), Arc<Semaphore>>>,
    balance: Arc<Mutex<Balance>>,
    availability: tokio::sync::Mutex<HashMap<String, Availability>>,
    pub capacity: Option<Arc<CapacityAdmissionController>>,
    pub usage: Option<Arc<UsageRecorder>>,
    pub pricing: Option<Arc<dyn crate::usage_recorder::UsageSnapshotResolver>>,
    pub realtime: Option<Arc<crate::realtime_metrics::RealtimeMetricsService>>,
    pub cache: Option<Arc<super::response_cache::ResponseCache>>,
}
impl Default for TransparentProxy {
    fn default() -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            slots: Mutex::new(HashMap::new()),
            balance: Arc::new(Mutex::new(Balance::default())),
            availability: tokio::sync::Mutex::new(HashMap::new()),
            capacity: None,
            usage: None,
            pricing: None,
            realtime: None,
            cache: None,
        }
    }
}
impl TransparentProxy {
    fn client(&self, config: &ProxyServiceConfig) -> Result<reqwest::Client, CompatError> {
        let mut clients = self.clients.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(client) = clients.get(&config.service_id) {
            return Ok(client.clone());
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs_f64(config.timeout_connect.max(0.1)))
            .read_timeout(Duration::from_secs_f64(config.timeout_read.max(0.1)))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()?;
        clients.insert(config.service_id.clone(), client.clone());
        Ok(client)
    }
    pub async fn availability(
        self: &Arc<Self>,
        config: &ProxyServiceConfig,
    ) -> Result<Value, CompatError> {
        let availability = self.available(config).await?;
        Ok(
            json!({"availability_status":if availability.urls.is_empty(){"unavailable"}else if availability.error.is_empty(){"available"}else{"degraded"},"available_upstreams":availability.urls,"last_health_error":availability.error,"last_health_check_at":availability.checked_at}),
        )
    }
    pub async fn health(&self, config: &ProxyServiceConfig) -> Result<Value, CompatError> {
        let availability = self.refresh(config).await?;
        Ok(
            json!({"availability_status":if availability.urls.is_empty(){"unavailable"}else if availability.error.is_empty(){"available"}else{"degraded"},"available_upstreams":availability.urls,"last_health_error":availability.error,"last_health_check_at":availability.checked_at}),
        )
    }
    async fn refresh(&self, config: &ProxyServiceConfig) -> Result<Availability, CompatError> {
        let client = self.client(config)?;
        let mut urls = vec![];
        let mut errors = vec![];
        for upstream in config.upstreams() {
            let mut healthy = false;
            let mut failure = "unreachable".to_owned();
            for (method, path) in [
                (Method::GET, "health"),
                (Method::GET, "docs"),
                (Method::POST, "assistants/search"),
            ] {
                let mut request = client
                    .request(
                        method.clone(),
                        format!("{}/{}", upstream.trim_end_matches('/'), path),
                    )
                    .timeout(Duration::from_secs_f64(
                        config.health_check_timeout.max(0.1),
                    ));
                if method == Method::POST {
                    request = request.json(&json!({}));
                }
                match request.send().await {
                    Ok(response) if response.status().as_u16() < 400 => {
                        healthy = true;
                        break;
                    }
                    Ok(response) => failure = response.status().as_u16().to_string(),
                    Err(_) => failure = "unreachable".into(),
                }
            }
            if healthy {
                urls.push(upstream);
            } else {
                errors.push(format!("{upstream}: {failure}"));
            }
        }
        let availability = Availability {
            checked: Instant::now(),
            checked_at: chrono::Utc::now().timestamp_millis() as f64 / 1000.0,
            urls,
            error: errors.join("; "),
        };
        self.availability
            .lock()
            .await
            .insert(config.service_id.clone(), availability.clone());
        Ok(availability)
    }
    async fn available(
        self: &Arc<Self>,
        config: &ProxyServiceConfig,
    ) -> Result<Availability, CompatError> {
        let cached = self
            .availability
            .lock()
            .await
            .get(&config.service_id)
            .cloned();
        if let Some(cached) = cached {
            if cached.checked.elapsed()
                < Duration::from_secs(if cached.urls.is_empty() { 5 } else { 30 })
            {
                return Ok(cached);
            }
            if !cached.urls.is_empty() {
                let this = self.clone();
                let config = config.clone();
                tokio::spawn(async move {
                    let _ = this.refresh(&config).await;
                });
                return Ok(cached);
            }
        }
        self.refresh(config).await
    }
    fn select(&self, config: &ProxyServiceConfig, urls: &[String]) -> Result<String, CompatError> {
        if urls.is_empty() {
            return Err(CompatError::new(
                502,
                format!("No upstream URLs configured for {}", config.service_name),
            ));
        }
        let mut balance = self.balance.lock().unwrap_or_else(|p| p.into_inner());
        let index = match config.load_balance_strategy.as_str() {
            "round_robin" => {
                let counter = balance
                    .counters
                    .entry(config.service_id.clone())
                    .or_insert(0);
                let index = *counter % urls.len();
                *counter += 1;
                index
            }
            "least_connections" => urls
                .iter()
                .enumerate()
                .min_by_key(|(_, url)| {
                    balance
                        .connections
                        .get(&(config.service_id.clone(), (*url).clone()))
                        .copied()
                        .unwrap_or(0)
                })
                .map(|(i, _)| i)
                .unwrap_or(0),
            "random" => SystemLoadBalancerRuntime.choice_index(urls.len()),
            _ => 0,
        };
        Ok(urls[index].clone())
    }
    pub async fn proxy(self: &Arc<Self>, request: ProxyRequest) -> Result<Response, CompatError> {
        if !request.config.enabled {
            return Err(CompatError::new(
                503,
                format!("Service disabled: {}", request.config.service_name),
            ));
        }
        let available = self.available(&request.config).await?;
        if available.urls.is_empty() {
            return Err(CompatError::new(
                503,
                format!("Service unavailable: {}", available.error),
            ));
        }
        let upstream = self.select(&request.config, &available.urls)?;
        let client = self.client(&request.config)?;
        let streaming = request.stream || is_streaming_path(&request.path);
        let headers = forward_headers(&request.headers, &request.user, &request.config)?;
        let capacity = if let Some(controller) = &self.capacity {
            Some(
                controller
                    .acquire(
                        &request.capacity_budgets,
                        &AdmissionRequest {
                            tenant_id: request.user.tenant_id.clone(),
                            user_id: request.user.user_id.clone(),
                            service_id: request.config.service_id.clone(),
                            request_class: if streaming {
                                "stream"
                            } else if request.path.contains("image") {
                                "image"
                            } else if request.path.contains("retrieve")
                                || request.path.contains("knowledge")
                            {
                                "rag"
                            } else if request.path.contains("tool") {
                                "tool"
                            } else {
                                "sync"
                            }
                            .into(),
                            request_id: request.user.request_id.clone(),
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(capacity_error)?,
            )
        } else {
            None
        };
        let owner = CapacityOwner::bind(capacity.as_ref())?;
        owner
            .run(self.proxy_admitted(request, upstream, client, headers, capacity, owner.clone()))
            .await?
    }
    async fn proxy_admitted(
        self: &Arc<Self>,
        mut request: ProxyRequest,
        upstream: String,
        client: reqwest::Client,
        headers: HeaderMap,
        capacity: Option<CapacityLease>,
        owner: CapacityOwner,
    ) -> Result<Response, CompatError> {
        let streaming = request.stream || is_streaming_path(&request.path);
        let capacity_headers = capacity
            .as_ref()
            .map(|lease| lease.headers())
            .unwrap_or_default();
        let semaphore = {
            let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
            slots
                .entry((request.config.service_id.clone(), streaming))
                .or_insert_with(|| {
                    Arc::new(Semaphore::new(
                        if streaming {
                            request.config.streaming_concurrency_limit
                        } else {
                            request.config.non_streaming_concurrency_limit
                        }
                        .unwrap_or(request.config.concurrency_limit)
                        .max(1),
                    ))
                })
                .clone()
        };
        let permit = match tokio::time::timeout(
            Duration::from_secs_f64(request.config.concurrency_queue_timeout.max(0.1)),
            semaphore.acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => permit,
            _ => {
                if let Some(capacity) = capacity {
                    capacity.release().await;
                }
                return Err(CompatError::new(503, "Proxy concurrency queue timeout"));
            }
        };
        let key = (request.config.service_id.clone(), upstream.clone());
        *self
            .balance
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .connections
            .entry(key.clone())
            .or_default() += 1;
        let connection_lease = ConnectionLease {
            balance: self.balance.clone(),
            key,
        };
        let mut parsed = serde_json::from_slice::<Value>(&request.body).unwrap_or(Value::Null);
        if !request.langgraph_body_prepared
            && langgraph_body::is_run_path(request.method.as_str(), &request.path)
            && parsed.is_object()
        {
            let config = serde_json::to_value(&request.config).unwrap_or(Value::Null);
            langgraph_body::prepare_run_payload(
                &mut parsed,
                &request.path,
                &request.user,
                &config,
                None,
            );
            request.body = Bytes::from(parsed.to_string());
        }
        let url = request.config.url(&upstream, &request.path);
        let started = Instant::now();
        let (cache_status, cache_hash, cached) = if let Some(cache) = &self.cache {
            cache.get(&request, &parsed, streaming).await
        } else {
            ("BYPASS", None, None)
        };
        if let Some(cached) = cached {
            let mut cached_headers = filter_response_headers(&cached.headers);
            cached_headers.insert("x-gateway-cache", HeaderValue::from_static("HIT"));
            drop(permit);
            drop(connection_lease);
            if let Some(capacity) = capacity {
                capacity.release().await;
            }
            for (k, v) in capacity_headers {
                if let (Ok(k), Ok(v)) = (
                    HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(&v),
                ) {
                    cached_headers.insert(k, v);
                }
            }
            return Ok((cached.status, cached_headers, cached.body).into_response());
        }
        let response = client
            .request(request.method.clone(), &url)
            .headers(headers.clone())
            .query(&request.query)
            .body(request.body.clone())
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                drop(permit);
                drop(connection_lease);
                if let Some(capacity) = capacity {
                    capacity.release().await;
                }
                return Err(error.into());
            }
        };
        let status = response.status();
        let is_sse = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("text/event-stream"));
        let mut response_headers = filter_response_headers(response.headers());
        response_headers.insert("x-gateway-cache", HeaderValue::from_static(cache_status));
        if !streaming || status.as_u16() >= 400 || !is_sse {
            let mut status = status;
            let mut bytes = response.bytes().await?;
            if !streaming
                && status == StatusCode::UNPROCESSABLE_ENTITY
                && request.path.to_lowercase().contains("/runs")
                && String::from_utf8_lossy(&bytes).contains("Invalid assistant")
            {
                if let Some(candidate) = self
                    .retry_candidate(&client, &request.config, &upstream, &headers)
                    .await
                {
                    if parsed.is_object() && text(&parsed, "assistant_id") != candidate {
                        parsed["assistant_id"] = json!(candidate);
                        let retried = client
                            .request(request.method.clone(), &url)
                            .headers(headers)
                            .query(&request.query)
                            .json(&parsed)
                            .send()
                            .await?;
                        status = retried.status();
                        response_headers = filter_response_headers(retried.headers());
                        response_headers
                            .insert("x-gateway-cache", HeaderValue::from_static(cache_status));
                        bytes = retried.bytes().await?;
                    }
                }
            }
            let value = if response_headers
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.contains("application/json"))
            {
                serde_json::from_slice(&bytes).unwrap_or(Value::Null)
            } else {
                Value::Null
            };
            self.record(
                &request,
                &value,
                started,
                status.as_u16(),
                false,
                extract_token_usage(&value).ok().flatten(),
                0,
            )
            .await;
            if cache_status == "MISS" {
                if let Some(cache) = &self.cache {
                    cache
                        .save(
                            &request,
                            &parsed,
                            cache_hash.as_deref(),
                            status,
                            &response_headers,
                            &bytes,
                            streaming,
                        )
                        .await;
                }
            }
            drop(permit);
            drop(connection_lease);
            if let Some(capacity) = capacity {
                capacity.release().await;
            }
            for (k, v) in capacity_headers {
                if let (Ok(k), Ok(v)) = (
                    HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(&v),
                ) {
                    response_headers.insert(k, v);
                }
            }
            return Ok((status, response_headers, bytes).into_response());
        }
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let this = self.clone();
        tokio::spawn(this.pump_stream(
            request,
            response,
            permit,
            connection_lease,
            capacity,
            owner,
            tx,
        ));
        let mut headers = HeaderMap::new();
        for (k, v) in [
            ("content-type", "text/event-stream"),
            ("cache-control", "no-cache"),
            ("connection", "keep-alive"),
            ("x-accel-buffering", "no"),
            ("x-gateway-cache", "BYPASS"),
        ] {
            headers.insert(HeaderName::from_static(k), HeaderValue::from_static(v));
        }
        for (k, v) in capacity_headers {
            if let (Ok(k), Ok(v)) = (
                HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(&v),
            ) {
                headers.insert(k, v);
            }
        }
        Ok((
            status,
            headers,
            Body::from_stream(super::sse_heartbeat::with_sse_heartbeat(
                ByteReceiver(rx),
                super::sse_heartbeat::DEFAULT_HEARTBEAT_INTERVAL,
            )),
        )
            .into_response())
    }
    async fn pump_stream(
        self: Arc<Self>,
        request: ProxyRequest,
        mut response: reqwest::Response,
        permit: tokio::sync::OwnedSemaphorePermit,
        connection_lease: ConnectionLease,
        capacity: Option<CapacityLease>,
        owner: CapacityOwner,
        tx: tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    ) {
        // Fence the entire pump, not just chunk reads: process_chunk,
        // channel backpressure and finalize may all suspend independently.
        // On loss the owned response, sender, permit and leases drop even
        // if accounting cannot finish. Do not synthesize a success frame.
        let _ = owner
            .run(async move {
                let input = serde_json::from_slice::<Value>(&request.body).unwrap_or(Value::Null);
                let model = request
                    .effective_model
                    .clone()
                    .or_else(|| extract_model(&input))
                    .or_else(|| request.config.default_model.clone())
                    .unwrap_or_default();
                let provider = request
                    .effective_provider
                    .clone()
                    .or_else(|| extract_provider(&input))
                    .or_else(|| request.config.default_provider.clone())
                    .unwrap_or_default();
                let mut billing = super::billing_stream::StreamAccounting::new(
                    request.user.clone(),
                    request.config.service_id.clone(),
                    request
                        .config
                        .assistant_id
                        .clone()
                        .or_else(|| extract_assistant_id(&input))
                        .unwrap_or_default(),
                    format!(
                        "proxy_{}",
                        detect_operation(request.method.as_str(), &request.path)
                    ),
                    model,
                    provider,
                    self.usage.clone(),
                    self.pricing.clone(),
                    self.realtime.clone(),
                );
                loop {
                    tokio::select! {_=tx.closed()=>break,chunk=response.chunk()=>match chunk{
                        Ok(Some(chunk))=>{billing.process_chunk(&chunk).await;if tx.send(Ok(chunk)).await.is_err(){break;}},
                        Ok(None)=>break,Err(_)=>{billing.mark_error("upstream_stream_error","Upstream stream interrupted");let _=tx.send(Ok(Bytes::from_static(b"event: error\ndata: Upstream stream interrupted\n\n"))).await;break;}
                    }}
                }
                billing.finalize().await;
                drop(response);
                drop(permit);
                drop(connection_lease);
                if let Some(capacity) = capacity {
                    capacity.release().await;
                }
            })
            .await;
    }
    async fn retry_candidate(
        &self,
        client: &reqwest::Client,
        config: &ProxyServiceConfig,
        base: &str,
        headers: &HeaderMap,
    ) -> Option<String> {
        let response = client
            .post(config.url(base, "assistants/search"))
            .headers(headers.clone())
            .json(&json!({}))
            .send()
            .await
            .ok()?;
        if response.status().as_u16() >= 400 {
            return None;
        }
        let payload = response.json::<Value>().await.ok()?;
        let records = payload.as_array().or_else(|| {
            ["items", "assistants", "data", "results"]
                .iter()
                .find_map(|key| payload[*key].as_array())
        })?;
        let mut candidates = vec![
            config.graph_id.clone().unwrap_or_default(),
            config.assistant_id.clone().unwrap_or_default(),
            config.service_name.clone(),
            config.service_id.clone(),
        ];
        candidates.extend(
            config
                .service_id
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|v| !v.is_empty() && !v.bytes().all(|b| b.is_ascii_digit()))
                .map(str::to_owned),
        );
        for candidate in candidates {
            for field in ["graph_id", "assistant_id"] {
                if let Some(found) = records.iter().find(|v| {
                    !text(v, field).is_empty()
                        && text(v, field).trim().eq_ignore_ascii_case(candidate.trim())
                }) {
                    return Some(text(found, field).trim().into());
                }
            }
        }
        None
    }
    async fn record(
        &self,
        request: &ProxyRequest,
        payload: &Value,
        start: Instant,
        status: u16,
        stream: bool,
        tokens: Option<TokenUsage>,
        first: i64,
    ) {
        let Some(recorder) = &self.usage else {
            return;
        };
        let operation = detect_operation(request.method.as_str(), &request.path);
        let run = operation.starts_with("run_");
        if tokens.is_none() && !run {
            return;
        }
        let request_data = serde_json::from_slice::<Value>(&request.body).unwrap_or(Value::Null);
        let model = extract_model(payload)
            .or_else(|| request.effective_model.clone())
            .or_else(|| extract_model(&request_data))
            .or_else(|| request.config.default_model.clone())
            .unwrap_or_else(|| "langgraph-agent".into());
        let mut record = UsageRecord::new(
            &request.user.tenant_id,
            &request.user.user_id,
            model,
            chrono::Utc::now(),
        );
        record.request_id = request.user.request_id.clone();
        record.service_id = request.config.service_id.clone();
        record.assistant_id = request
            .config
            .assistant_id
            .clone()
            .or_else(|| extract_assistant_id(&request_data))
            .or_else(|| extract_assistant_id(payload))
            .unwrap_or_default();
        record.provider = extract_provider(payload)
            .or_else(|| request.effective_provider.clone())
            .or_else(|| extract_provider(&request_data))
            .or_else(|| request.config.default_provider.clone())
            .unwrap_or_else(|| "unattributed".into());
        record.status = if status >= 400
            || payload.get("error").is_some()
            || payload.get("__error__").is_some()
        {
            "error"
        } else {
            "success"
        }
        .into();
        record.request_type = format!("proxy_{operation}");
        record.latency_ms = start.elapsed().as_millis() as i64;
        record.request_total_duration_ms = record.latency_ms;
        record.first_token_ms = first;
        record.metadata = object(
            &json!({"source":if stream{"transparent_proxy_stream"}else{"transparent_proxy_non_stream"},"path":request.path,"http_status":status,"response_has_usage":tokens.is_some(),"token_source":if tokens.is_some(){"upstream"}else if status>=400{"zero_on_failure"}else{"estimated"}}),
        );
        if let Some(tokens) = tokens {
            record.input_tokens = tokens.input_tokens;
            record.output_tokens = tokens.output_tokens;
        }
        if let Some(pricing) = &self.pricing {
            let _ = recorder.record_with_resolver(record, pricing.clone()).await;
        } else {
            recorder.record_priced(record).await;
        }
    }
    pub async fn close(&self) {
        self.clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        self.availability.lock().await.clear();
    }
}
pub fn is_streaming_path(path: &str) -> bool {
    let path = path.to_lowercase();
    path.ends_with("/stream") || path.ends_with("/sse") || path.contains("/stream")
}
pub fn detect_operation(method: &str, path: &str) -> &'static str {
    super::proxy_http::operation(method, path)
}

fn hop(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailers"
            | "transfer-encoding"
            | "upgrade"
    )
}
pub fn filter_response_headers(headers: &HeaderMap) -> HeaderMap {
    headers
        .iter()
        .filter(|(name, _)| !hop(name.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
pub fn forward_headers(
    headers: &HeaderMap,
    user: &CompatUser,
    config: &ProxyServiceConfig,
) -> Result<HeaderMap, CompatError> {
    forward_headers_with_client_ip(
        headers,
        user,
        config,
        &crate::migration_content::access::context().client_ip,
    )
}
/// The IP argument is already resolved by trusted-proxy middleware, never copied
/// from the incoming forwarding headers. Used by the production wrapper above.
pub fn forward_headers_with_client_ip(
    headers: &HeaderMap,
    user: &CompatUser,
    config: &ProxyServiceConfig,
    trusted_client_ip: &str,
) -> Result<HeaderMap, CompatError> {
    let mut result = HeaderMap::new();
    for (name, value) in headers {
        let name_str = name.as_str();
        if hop(name_str)
            || matches!(
                name_str,
                "host"
                    | "content-length"
                    | "cookie"
                    | "x-api-key"
                    | "x-forwarded-for"
                    | "x-real-ip"
            )
            || name_str.starts_with("x-user-")
            || name_str.starts_with("x-tenant-")
            || name_str.starts_with("x-gw-")
        {
            continue;
        }
        if name_str == "authorization" && !config.forward_auth {
            continue;
        }
        result.insert(name.clone(), value.clone());
    }
    for (name, value) in [
        ("x-user-id", user.user_id.as_str()),
        ("x-gw-user-id", user.user_id.as_str()),
        ("x-tenant-id", user.tenant_id.as_str()),
        ("x-gw-tenant-id", user.tenant_id.as_str()),
        ("x-user-tier", user.tier.as_str()),
        ("x-gw-user-tier", user.tier.as_str()),
        (
            "x-user-type",
            if user.authenticated {
                "user"
            } else if user.user_id.is_empty() {
                "anonymous"
            } else {
                "guest"
            },
        ),
        (
            "x-gw-authenticated",
            if user.authenticated { "true" } else { "false" },
        ),
        ("x-gw-request-id", user.request_id.as_str()),
        ("x-trace-id", user.trace_id.as_str()),
    ] {
        result.insert(
            HeaderName::from_static(name),
            HeaderValue::from_str(value)
                .map_err(|_| CompatError::new(400, "Invalid context header"))?,
        );
    }
    let mut permissions = vec!["read"];
    if user.authenticated {
        permissions.push("write");
    }
    if user.roles.iter().any(|r| r == "admin") || user.tier == "admin" {
        permissions.extend(["admin", "delete"]);
    }
    result.insert(
        "x-user-permissions",
        HeaderValue::from_str(&permissions.join(","))
            .map_err(|_| CompatError::new(400, "Invalid context header"))?,
    );
    if !trusted_client_ip.is_empty() {
        result.insert(
            "x-forwarded-for",
            HeaderValue::from_str(trusted_client_ip)
                .map_err(|_| CompatError::new(400, "Invalid context header"))?,
        );
    }
    if !config.forward_auth {
        if let Some(token) = config.auth_token.as_ref().filter(|v| !v.is_empty()) {
            if token.contains(['\r', '\n'])
                || (!token.starts_with("Bearer ") && token.contains(':'))
            {
                return Err(CompatError::new(
                    503,
                    "Invalid upstream authentication: service_auth_token",
                ));
            }
            let name = if token.starts_with("Bearer ") {
                "authorization"
            } else {
                "x-api-key"
            };
            result.insert(
                name,
                HeaderValue::from_str(token).map_err(|_| {
                    CompatError::new(503, "Invalid upstream authentication: service_auth_token")
                })?,
            );
        }
    }
    Ok(result)
}

#[cfg(test)]
#[path = "transparent_proxy_lease_tests.rs"]
mod lease_tests;

#[cfg(test)]
#[path = "transparent_proxy_source_matrix.rs"]
mod source_matrix;
