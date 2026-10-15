use super::types::{QueryError, Result};
use reqwest::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
#[derive(Clone)]
pub struct RerankProviderConfig {
    pub api_key: String,
    pub endpoint: String,
    pub request_schema: String,
    pub instruct: Option<String>,
}
#[derive(Clone, Debug)]
pub struct RerankResult {
    pub index: usize,
    pub relevance_score: f64,
}
#[derive(Default)]
struct ProviderHealth {
    failures: BTreeMap<String, (Instant, u16)>,
    success: BTreeMap<String, Instant>,
}
#[derive(Clone)]
pub struct Reranker {
    local_backend: Option<Arc<dyn LocalRerankBackend>>,
    client: Client,
    providers: Arc<BTreeMap<String, RerankProviderConfig>>,
    cache: Arc<Mutex<VecDeque<(String, Vec<RerankResult>)>>>,
    health: Arc<Mutex<ProviderHealth>>,
    guards: Arc<Mutex<BTreeMap<String, Arc<Mutex<()>>>>>,
    pub timeout: Duration,
}
impl Reranker {
    pub fn new(client: Client, providers: BTreeMap<String, RerankProviderConfig>) -> Self {
        Self {
            local_backend: None,
            client,
            providers: Arc::new(providers),
            cache: Arc::new(Mutex::new(VecDeque::new())),
            health: Arc::new(Mutex::new(ProviderHealth::default())),
            guards: Arc::new(Mutex::new(BTreeMap::new())),
            timeout: Duration::from_secs(30),
        }
    }
    pub async fn rerank(
        &self,
        provider: &str,
        model: &str,
        query: &str,
        documents: &[String],
        top_n: usize,
    ) -> Result<Vec<RerankResult>> {
        if documents.is_empty() {
            return Ok(vec![]);
        }
        let p = normalize_provider(provider, model);
        if p == "bge" || p == "local" {
            return self.rerank_local(&p, model, query, documents, top_n).await;
        }
        let model = normalize_model(&p, model);
        let config = self.providers.get(&p).ok_or_else(|| {
            QueryError::Validation(format!(
                "server-owned {p} rerank credentials are unavailable"
            ))
        })?;
        if config.api_key.is_empty() || config.endpoint.is_empty() {
            return Err(QueryError::Validation(
                "server-owned rerank endpoint and credentials are required".into(),
            ));
        }
        let schema = if config.request_schema == "auto" {
            if config.endpoint.trim_end_matches('/').ends_with("/reranks") {
                "flat"
            } else {
                "legacy"
            }
        } else {
            &config.request_schema
        };
        if !["flat", "legacy"].contains(&schema) {
            return Err(QueryError::Validation(
                "DashScope rerank request_schema must be auto|legacy|flat".into(),
            ));
        }
        if config.instruct.is_some()
            && !["qwen3-rerank", "qwen3-vl-rerank"].contains(&model.as_str())
        {
            return Err(QueryError::Validation(
                "rerank instruct is not supported by this model".into(),
            ));
        }
        let profile = format!(
            "{}:{}:{}:{:x}",
            config.endpoint.trim_end_matches('/').to_lowercase(),
            schema,
            config.instruct.as_deref().unwrap_or(""),
            Sha256::digest(config.api_key.as_bytes())
        );
        let key = format!(
            "{:x}",
            Sha256::digest(
                json!([&p, &model, &profile, query, documents, top_n])
                    .to_string()
                    .as_bytes()
            )
        );
        {
            let mut cache = self.cache.lock().await;
            if let Some(index) = cache.iter().position(|(k, _)| k == &key) {
                let entry = cache
                    .remove(index)
                    .ok_or(QueryError::InvalidProviderResponse)?;
                let result = entry.1.clone();
                cache.push_back(entry);
                return Ok(result);
            }
        }
        let health_key = format!("{p}:{model}:{profile}");
        if let Some(error) = self.failure(&health_key).await {
            return Err(error);
        }
        let guard = {
            let mut guards = self.guards.lock().await;
            guards
                .entry(health_key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let recent = self
            .health
            .lock()
            .await
            .success
            .get(&health_key)
            .is_some_and(|t| t.elapsed() < Duration::from_secs(60));
        let _guard = if recent {
            None
        } else {
            Some(guard.lock().await)
        };
        if let Some(error) = self.failure(&health_key).await {
            return Err(error);
        }
        let mut payload = if p == "cohere" || schema == "flat" {
            json!({"model":model,"query":query,"documents":documents,"top_n":top_n})
        } else {
            json!({"model":model,"input":{"query":query,"documents":documents},"parameters":{"top_n":top_n,"return_documents":false}})
        };
        if p == "cohere" {
            payload["return_documents"] = json!(false)
        }
        if let Some(instruct) = &config.instruct {
            if schema == "flat" {
                payload["instruct"] = json!(instruct)
            } else {
                payload["parameters"]["instruct"] = json!(instruct)
            }
        }
        let response = match self
            .client
            .post(&config.endpoint)
            .timeout(self.timeout)
            .bearer_auth(&config.api_key)
            .json(&payload)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let status = if e.is_timeout() { 408 } else { 503 };
                self.fail(&health_key, status, Duration::from_secs(30))
                    .await;
                return Err(QueryError::Provider(status));
            }
        };
        if !response.status().is_success() {
            let code = response.status().as_u16();
            let permanent = [400, 401, 403, 404, 405, 410, 422].contains(&code);
            self.fail(
                &health_key,
                code,
                Duration::from_secs(if permanent { 300 } else { 30 }),
            )
            .await;
            return Err(QueryError::Provider(code));
        }
        let data: Value = response
            .json()
            .await
            .map_err(|_| QueryError::InvalidProviderResponse)?;
        self.health
            .lock()
            .await
            .success
            .insert(health_key, Instant::now());
        let rows = data["results"]
            .as_array()
            .or(data["output"]["results"].as_array())
            .ok_or(QueryError::InvalidProviderResponse)?;
        let mut results = Vec::new();
        for r in rows {
            if let Some(index) = r["index"].as_u64() {
                results.push(RerankResult {
                    index: index as usize,
                    relevance_score: r["relevance_score"]
                        .as_f64()
                        .filter(|n| n.is_finite())
                        .unwrap_or(0.0),
                })
            }
        }
        results.sort_by(|a, b| b.relevance_score.total_cmp(&a.relevance_score));
        let mut cache = self.cache.lock().await;
        cache.push_back((key, results.clone()));
        while cache.len() > 200 {
            cache.pop_front();
        }
        Ok(results)
    }
    async fn failure(&self, key: &str) -> Option<QueryError> {
        self.health
            .lock()
            .await
            .failures
            .get(key)
            .filter(|(until, _)| Instant::now() < *until)
            .map(|(_, code)| QueryError::Provider(*code))
    }
    async fn fail(&self, key: &str, code: u16, cooldown: Duration) {
        self.health
            .lock()
            .await
            .failures
            .insert(key.into(), (Instant::now() + cooldown, code));
    }
}
pub fn normalize_provider(provider: &str, model: &str) -> String {
    let raw = provider.trim().to_lowercase();
    match raw.as_str() {
        "dashscope" | "gte" | "aliyun" => "dashscope",
        "bge" | "flagembedding" | "baai" => "bge",
        "cohere" => "cohere",
        "local" | "cross-encoder" | "sentence-transformers" => "local",
        _ => {
            let model = model.trim().to_lowercase();
            if model.starts_with("baai/") || model.contains("bge-reranker") {
                "bge"
            } else if model.starts_with("rerank-multilingual") {
                "cohere"
            } else if model.starts_with("cross-encoder/") {
                "local"
            } else {
                "dashscope"
            }
        }
    }
    .into()
}
pub fn normalize_model(provider: &str, model: &str) -> String {
    let m = model.trim();
    match provider {
        "dashscope" => {
            if m.is_empty() || m.eq_ignore_ascii_case("default") {
                "qwen3-rerank".into()
            } else if ["gte-rerank", "gte-reranker"].contains(&m.to_lowercase().as_str()) {
                "gte-rerank-v2".into()
            } else {
                m.into()
            }
        }
        "bge" => {
            if m.is_empty() || m.eq_ignore_ascii_case("bge-reranker-v2-m3") {
                "BAAI/bge-reranker-v2-m3".into()
            } else {
                m.into()
            }
        }
        "cohere" => {
            if m.is_empty() {
                "rerank-multilingual-v3.0".into()
            } else {
                m.into()
            }
        }
        "local" => {
            if m.is_empty() {
                "cross-encoder/ms-marco-MiniLM-L-12-v2".into()
            } else {
                m.into()
            }
        }
        _ => m.into(),
    }
}
/// An installed native inference runtime must supply this boundary. No model
/// download, Python child, pseudo-score or successful stub is selected by default.
pub trait LocalRerankBackend: Send + Sync {
    fn identity(&self) -> String;
    fn scores<'a>(
        &'a self,
        provider: &'a str,
        model: &'a str,
        query: &'a str,
        documents: &'a [String],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<f64>>> + Send + 'a>>;
}
/// Concrete server-configured inference adapter. The operator supplies a trusted
/// local model service endpoint; the client cannot select an endpoint or key.
#[derive(Clone)]
pub struct HttpLocalRerankBackend {
    client: Client,
    endpoint: reqwest::Url,
    api_key: Option<String>,
    pub timeout: Duration,
    pub device: String,
    pub use_fp16: bool,
}
impl HttpLocalRerankBackend {
    pub fn new(client: Client, endpoint: reqwest::Url, api_key: Option<String>) -> Result<Self> {
        if !["http", "https"].contains(&endpoint.scheme()) || endpoint.host_str().is_none() {
            return Err(QueryError::Validation(
                "invalid local inference endpoint".into(),
            ));
        }
        Ok(Self {
            client,
            endpoint,
            api_key,
            timeout: Duration::from_secs(30),
            device: "cpu".into(),
            use_fp16: true,
        })
    }
}
impl LocalRerankBackend for HttpLocalRerankBackend {
    fn identity(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(format!(
                "{}:{}:{}:{}",
                self.endpoint,
                self.api_key.as_deref().unwrap_or(""),
                self.device,
                self.use_fp16
            ))
        )
    }
    fn scores<'a>(
        &'a self,
        provider: &'a str,
        model: &'a str,
        query: &'a str,
        documents: &'a [String],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<f64>>> + Send + 'a>> {
        Box::pin(async move {
            let pairs: Vec<_> = documents.iter().map(|d| json!([query, d])).collect();
            let payload = json!({"provider":provider,"model":model,"pairs":pairs,"normalize":provider=="bge","device":self.device,"use_fp16":self.use_fp16});
            let mut request = self
                .client
                .post(self.endpoint.clone())
                .timeout(self.timeout)
                .json(&payload);
            if let Some(key) = &self.api_key {
                request = request.bearer_auth(key)
            }
            let response = request
                .send()
                .await
                .map_err(|_| QueryError::Provider(503))?;
            if !response.status().is_success() {
                return Err(QueryError::Provider(response.status().as_u16()));
            }
            let v: Value = response
                .json()
                .await
                .map_err(|_| QueryError::InvalidProviderResponse)?;
            let scores = if let Some(n) = v["scores"].as_f64() {
                vec![n]
            } else {
                v["scores"]
                    .as_array()
                    .ok_or(QueryError::InvalidProviderResponse)?
                    .iter()
                    .map(|v| v.as_f64().ok_or(QueryError::InvalidProviderResponse))
                    .collect::<Result<Vec<_>>>()?
            };
            if scores.len() != documents.len() || scores.iter().any(|s| !s.is_finite()) {
                return Err(QueryError::InvalidProviderResponse);
            }
            Ok(scores)
        })
    }
}
impl Reranker {
    pub fn with_local_backend(mut self, backend: Arc<dyn LocalRerankBackend>) -> Self {
        self.local_backend = Some(backend);
        self
    }
    async fn rerank_local(
        &self,
        provider: &str,
        model: &str,
        query: &str,
        documents: &[String],
        top_n: usize,
    ) -> Result<Vec<RerankResult>> {
        let backend = self.local_backend.as_ref().ok_or_else(|| {
            QueryError::Validation(format!(
                "{provider} local inference backend is not installed"
            ))
        })?;
        let model = normalize_model(provider, model);
        let key = format!(
            "{:x}",
            Sha256::digest(
                json!([provider, model, backend.identity(), query, documents, top_n]).to_string()
            )
        );
        {
            let mut cache = self.cache.lock().await;
            if let Some(i) = cache.iter().position(|(k, _)| k == &key) {
                let entry = cache.remove(i).ok_or(QueryError::InvalidProviderResponse)?;
                let result = entry.1.clone();
                cache.push_back(entry);
                return Ok(result);
            }
        }
        let model_key = format!("local:{provider}:{model}:{}", backend.identity());
        let guard = {
            let mut guards = self.guards.lock().await;
            guards
                .entry(model_key)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let _guard = guard.lock().await;
        let scores = backend.scores(provider, &model, query, documents).await?;
        if scores.len() != documents.len() {
            return Err(QueryError::InvalidProviderResponse);
        }
        let mut result: Vec<_> = scores
            .into_iter()
            .enumerate()
            .map(|(index, relevance_score)| RerankResult {
                index,
                relevance_score,
            })
            .collect();
        result.sort_by(|a, b| b.relevance_score.total_cmp(&a.relevance_score));
        if top_n > 0 {
            result.truncate(top_n)
        }
        let mut cache = self.cache.lock().await;
        cache.push_back((key, result.clone()));
        while cache.len() > 200 {
            cache.pop_front();
        }
        Ok(result)
    }
}
