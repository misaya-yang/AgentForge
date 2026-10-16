use super::{
    config::server_owned_config,
    types::{Dataset, QueryError, Result},
};
use crate::{provider_cipher::NativeProviderCipher, provider_service::ProviderCipher};
use base64::Engine;
use reqwest::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

/// Secret-bearing configuration is intentionally neither Debug nor Serialize.
#[derive(Clone)]
pub struct ProviderConfig {
    pub api_key: String,
    pub base_url: Option<String>,
    pub timeout: Duration,
}
impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            base_url: None,
            timeout: Duration::from_secs(30),
        }
    }
}
#[derive(Clone)]
pub struct EmbeddingManager {
    pub document_batch_size: usize,
    client: Client,
    pool: Option<PgPool>,
    encryption_key: Arc<String>,
    defaults: Arc<BTreeMap<String, ProviderConfig>>,
    query_cache: Arc<Mutex<BTreeMap<String, (Instant, Vec<f64>)>>>,
    local_semantic: Option<Arc<LocalSemanticRuntime>>,
}
impl EmbeddingManager {
    pub fn new(
        client: Client,
        pool: Option<PgPool>,
        encryption_key: String,
        defaults: BTreeMap<String, ProviderConfig>,
    ) -> Self {
        Self {
            document_batch_size: 100,
            client,
            pool,
            encryption_key: Arc::new(encryption_key),
            defaults: Arc::new(defaults),
            query_cache: Arc::new(Mutex::new(BTreeMap::new())),
            local_semantic: None,
        }
    }
    /// Configure only the explicit local_semantic identity; regular provider
    /// credentials and tenant overrides retain their existing behavior.
    pub fn with_local_semantic(mut self, base_url: &str, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() {
            return Err(QueryError::Validation(
                "local semantic timeout must be positive".into(),
            ));
        }
        let endpoint = local_semantic_endpoint(base_url)?;
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| QueryError::Validation("local semantic HTTP client unavailable".into()))?;
        self.local_semantic = Some(Arc::new(LocalSemanticRuntime {
            client,
            endpoint,
            timeout,
        }));
        Ok(self)
    }
    /// A startup probe includes actual inference; subsequent health probes check
    /// the loaded model identity and TEI readiness without repeated inference.
    pub async fn probe_local_semantic(&self, inference: bool) -> Result<()> {
        self.local_semantic
            .as_ref()
            .ok_or_else(|| {
                QueryError::Validation("local semantic runtime is not configured".into())
            })?
            .probe(inference)
            .await
    }
    fn resolve_local_semantic(&self, dataset: &Dataset) -> Result<ResolvedEmbedding> {
        let runtime = self.local_semantic.as_ref().ok_or_else(|| {
            QueryError::Validation("local semantic runtime is not configured".into())
        })?;
        if dataset
            .embedding_model
            .as_deref()
            .is_some_and(|model| !model.is_empty() && model != LOCAL_SEMANTIC_MODEL)
            || dataset
                .embedding_dimension
                .is_some_and(|dimension| dimension != LOCAL_SEMANTIC_DIMENSION)
            || dataset
                .embedding_config
                .get("dimension")
                .is_some_and(|dimension| {
                    dimension.as_u64() != Some(LOCAL_SEMANTIC_DIMENSION as u64)
                })
        {
            return Err(QueryError::Validation(
                "local semantic requires Qwen/Qwen3-Embedding-0.6B and 1024 dimensions".into(),
            ));
        }
        // Deliberately do not consult tenant_credential for this operator-pinned identity.
        Ok(runtime.configuration())
    }
    pub async fn resolve(&self, dataset: &Dataset) -> Result<ResolvedEmbedding> {
        server_owned_config(&dataset.embedding_config)?;
        let provider = canonical_provider(dataset.embedding_provider.as_deref().unwrap_or("local"));
        if provider == "local_semantic" {
            return self.resolve_local_semantic(dataset);
        }
        if provider == "openai" {
            return Err(QueryError::Validation(
                "OpenAI embedding provider has been removed".into(),
            ));
        }
        let default_model = match provider.as_str() {
            "local" => "hash-384",
            "gemini" => "gemini-embedding-001",
            "siliconflow" => "BAAI/bge-large-zh-v1.5",
            "openai_compatible" => "Qwen/Qwen3-Embedding-0.6B",
            "dashscope_multimodal" => "multimodal-embedding-v1",
            "unified_multimodal" => "tongyi-embedding-vision-plus",
            _ => "",
        };
        let model = dataset
            .embedding_model
            .as_deref()
            .filter(|x| !x.trim().is_empty())
            .unwrap_or(default_model)
            .to_owned();
        let mut config = self.defaults.get(&provider).cloned().unwrap_or_default();
        if let Some(tenant) = self
            .tenant_credential(&dataset.tenant_id, &provider)
            .await?
        {
            if !tenant.api_key.is_empty() {
                config.api_key = tenant.api_key
            }
            if tenant.base_url.is_some() {
                config.base_url = tenant.base_url
            }
        }
        let dimension = dataset
            .embedding_dimension
            .filter(|d| *d > 0)
            .or_else(|| {
                dataset.embedding_config["dimension"]
                    .as_u64()
                    .map(|d| d as usize)
            })
            .unwrap_or_else(|| infer_dimension(&provider, &model));
        let endpoint = match provider.as_str() {
            "local" => String::new(),
            "gemini" => config
                .base_url
                .clone()
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com/v1beta/models".into())
                .trim_end_matches('/')
                .into(),
            "dashscope" | "dashscope_multimodal" | "unified_multimodal" => normalize_dashscope(
                config
                    .base_url
                    .as_deref()
                    .unwrap_or("https://dashscope.aliyuncs.com/api/v1"),
            ),
            "siliconflow" => embeddings_endpoint(
                config
                    .base_url
                    .as_deref()
                    .unwrap_or("https://api.siliconflow.cn/v1"),
            ),
            "openai_compatible" => {
                let url = config
                    .base_url
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| {
                        QueryError::Validation(
                            "openai_compatible embedding provider requires a server-owned base URL"
                                .into(),
                        )
                    })?;
                if config.api_key.trim().is_empty() {
                    config.api_key = "unused".into()
                }
                embeddings_endpoint(url)
            }
            _ => {
                return Err(QueryError::Validation(format!(
                    "Unsupported embedding provider: {provider}"
                )));
            }
        };
        if provider != "local" && config.api_key.trim().is_empty() {
            return Err(QueryError::Validation(format!(
                "server-owned {provider} embedding credentials are unavailable"
            )));
        }
        Ok(ResolvedEmbedding {
            provider,
            model,
            dimension,
            endpoint,
            api_key: config.api_key,
            timeout: config.timeout,
        })
    }
    async fn tenant_credential(
        &self,
        tenant: &str,
        provider: &str,
    ) -> Result<Option<ProviderConfig>> {
        let family = match provider {
            "dashscope" | "dashscope_multimodal" => "dashscope",
            "gemini" => "gemini",
            "siliconflow" => "siliconflow",
            _ => return Ok(None),
        };
        let Some(pool) = &self.pool else {
            return Ok(None);
        };
        if tenant.trim().is_empty() {
            return Ok(None);
        }
        let rows=match sqlx::query("SELECT provider_id,api_type,base_url,api_key_encrypted FROM llm_providers WHERE tenant_id=$1 AND is_enabled=true ORDER BY updated_at DESC,provider_id ASC").bind(tenant.trim()).fetch_all(pool).await{Ok(r)=>r,Err(_)=>return Ok(None)};
        for row in rows {
            let id: String = row.try_get("provider_id").unwrap_or_default();
            let api: String = row.try_get("api_type").unwrap_or_default();
            let base: String = row
                .try_get::<Option<String>, _>("base_url")
                .unwrap_or_default()
                .unwrap_or_default();
            if provider_family(&id, &api, &base) != Some(family) {
                continue;
            }
            let stored: String = row
                .try_get::<Option<String>, _>("api_key_encrypted")
                .unwrap_or_default()
                .unwrap_or_default();
            if stored.trim().is_empty() {
                return Ok(None);
            }
            let key = NativeProviderCipher
                .decrypt(stored.trim(), &self.encryption_key)
                .map_err(|_| {
                    QueryError::Validation(
                        "tenant embedding credential could not be decrypted".into(),
                    )
                })?;
            if key.is_empty() || key.starts_with("enc:") {
                return Err(QueryError::Validation(
                    "tenant embedding credential could not be decrypted".into(),
                ));
            }
            return Ok(Some(ProviderConfig {
                api_key: key,
                base_url: if base.trim().is_empty() {
                    None
                } else {
                    Some(base)
                },
                timeout: Duration::from_secs(30),
            }));
        }
        Ok(None)
    }
    pub async fn embed_query(&self, dataset: &Dataset, query: &str) -> Result<Vec<f64>> {
        let resolved = self.resolve(dataset).await?;
        let digest=Sha256::digest(serde_json::to_vec(&json!({"provider":resolved.provider,"model":resolved.model,"dimension":resolved.dimension,"endpoint":resolved.endpoint,"credential":format!("{:x}",Sha256::digest(resolved.api_key.as_bytes())),"text_type":"query","query":query})).map_err(|_|QueryError::InvalidProviderResponse)?);
        let key = format!("{digest:x}");
        if let Some((time, vec)) = self.query_cache.lock().await.get(&key) {
            if time.elapsed() < Duration::from_secs(1800) {
                return Ok(vec.clone());
            }
        }
        let mut vectors = self
            .embed_resolved(&resolved, &[query.to_owned()], "query")
            .await?;
        let result = vectors.remove(0);
        let mut cache = self.query_cache.lock().await;
        cache.retain(|_, (time, _)| time.elapsed() < Duration::from_secs(1800));
        if cache.len() >= 1000 {
            if let Some(old) = cache
                .iter()
                .min_by_key(|(_, v)| v.0)
                .map(|(k, _)| k.clone())
            {
                cache.remove(&old);
            }
        }
        cache.insert(key, (Instant::now(), result.clone()));
        Ok(result)
    }
    pub async fn embed_documents(
        &self,
        dataset: &Dataset,
        texts: &[String],
    ) -> Result<Vec<Vec<f64>>> {
        let resolved = self.resolve(dataset).await?;
        self.embed_resolved(&resolved, texts, "document").await
    }
    pub async fn embed_resolved(
        &self,
        config: &ResolvedEmbedding,
        texts: &[String],
        kind: &str,
    ) -> Result<Vec<Vec<f64>>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }
        if config.provider == "local" {
            return Ok(texts
                .iter()
                .map(|t| local_hash_embedding(t, config.dimension))
                .collect());
        }
        let multimodal = config.provider.ends_with("multimodal");
        let batch_size = if multimodal {
            1
        } else if config.provider == "dashscope" {
            10
        } else {
            100
        };
        let mut all = Vec::new();
        let batch_size = if kind == "document" {
            batch_size.min(self.document_batch_size.max(1))
        } else {
            batch_size
        };
        for batch in texts.chunks(batch_size) {
            let processed: Vec<String> = batch
                .iter()
                .map(|s| {
                    if config.provider == "dashscope" {
                        sanitize_dashscope(s, &config.model)
                    } else if s.trim().is_empty() {
                        "empty".into()
                    } else {
                        s.clone()
                    }
                })
                .collect();
            let vectors = match self.remote_batch(config, &processed, kind).await {
                Err(QueryError::Provider(408))
                    if config.provider == "dashscope" && batch.len() > 1 =>
                {
                    let mut singles = Vec::new();
                    for text in &processed {
                        singles.extend(self.remote_batch(config, &[text.clone()], kind).await?)
                    }
                    singles
                }
                result => result?,
            };
            validate_vectors(&vectors, batch.len(), config.dimension)?;
            all.extend(vectors)
        }
        Ok(all)
    }
    async fn remote_batch(
        &self,
        c: &ResolvedEmbedding,
        texts: &[String],
        kind: &str,
    ) -> Result<Vec<Vec<f64>>> {
        let started = Instant::now();
        let result = self.remote_batch_inner(c, texts, kind).await;
        let outcome = match &result {
            Ok(_) => "ok",
            Err(QueryError::Provider(408 | 504)) => "timeout",
            Err(_) => "error",
        };
        super::pipeline_metrics::embedding(outcome, started.elapsed().as_secs_f64());
        result
    }
    async fn remote_batch_inner(
        &self,
        c: &ResolvedEmbedding,
        texts: &[String],
        kind: &str,
    ) -> Result<Vec<Vec<f64>>> {
        if c.provider == "local_semantic" {
            return self
                .local_semantic
                .as_ref()
                .ok_or_else(|| {
                    QueryError::Validation("local semantic runtime is not configured".into())
                })?
                .embed(c, texts, kind)
                .await;
        }
        let multimodal = c.provider.ends_with("multimodal");
        let (url, payload) = if c.provider == "gemini" {
            let task = match kind {
                "query" => "RETRIEVAL_QUERY",
                "similarity" => "SEMANTIC_SIMILARITY",
                "classification" => "CLASSIFICATION",
                "clustering" => "CLUSTERING",
                _ => "RETRIEVAL_DOCUMENT",
            };
            (
                format!("{}/{}:batchEmbedContents", c.endpoint, c.model),
                json!({"requests":texts.iter().map(|t|json!({"model":format!("models/{}",c.model),"content":{"parts":[{"text":t}]},"taskType":task,"outputDimensionality":c.dimension})).collect::<Vec<_>>()}),
            )
        } else if c.provider == "dashscope" {
            (
                format!(
                    "{}/services/embeddings/text-embedding/text-embedding",
                    c.endpoint
                ),
                json!({"model":c.model,"input":{"texts":texts},"parameters":{"text_type":kind,"dimension":c.dimension}}),
            )
        } else if multimodal {
            (
                format!(
                    "{}/services/embeddings/multimodal-embedding/multimodal-embedding",
                    c.endpoint
                ),
                json!({"model":c.model,"input":{"contents":[{"text":texts[0]}]},"parameters":{"dimension":c.dimension}}),
            )
        } else {
            (c.endpoint.clone(), json!({"model":c.model,"input":texts}))
        };
        let data = self.request(c, &url, &payload).await?;
        parse_vectors(&data, &c.provider)
    }
    pub async fn embed_image_with_context(
        &self,
        dataset: &Dataset,
        image: &[u8],
        text: Option<&str>,
    ) -> Result<Vec<f64>> {
        let c = self.resolve(dataset).await?;
        if !c.provider.ends_with("multimodal") {
            return Err(QueryError::Validation(
                "embedding provider does not support image embedding".into(),
            ));
        }
        let media = if image.starts_with(b"\x89PNG") {
            "image/png"
        } else if image.starts_with(b"\xff\xd8\xff") {
            "image/jpeg"
        } else if image.starts_with(b"GIF8") {
            "image/gif"
        } else if image.starts_with(b"RIFF") {
            "image/webp"
        } else {
            "image/png"
        };
        let mut contents = vec![
            json!({"image":format!("data:{media};base64,{}",base64::engine::general_purpose::STANDARD.encode(image))}),
        ];
        if let Some(t) = text.filter(|t| !t.trim().is_empty()) {
            contents.push(json!({"text":t}))
        }
        let payload = json!({"model":c.model,"input":{"contents":contents},"parameters":{"dimension":c.dimension}});
        let data = self
            .request(
                &c,
                &format!(
                    "{}/services/embeddings/multimodal-embedding/multimodal-embedding",
                    c.endpoint
                ),
                &payload,
            )
            .await?;
        let mut vectors = parse_vectors(&data, &c.provider)?;
        validate_vectors(&vectors, 1, c.dimension)?;
        Ok(vectors.remove(0))
    }
    async fn request(&self, c: &ResolvedEmbedding, url: &str, payload: &Value) -> Result<Value> {
        let dash = c.provider.starts_with("dashscope") || c.provider == "unified_multimodal";
        let attempts = if dash { 5 } else { 3 };
        let deadline = Instant::now() + c.timeout;
        let mut last = QueryError::Provider(503);
        for attempt in 0..attempts {
            let timeout = if dash {
                deadline.saturating_duration_since(Instant::now())
            } else {
                c.timeout
            };
            if timeout.is_zero() {
                return Err(QueryError::Provider(408));
            }
            let mut request = self.client.post(url).timeout(timeout).json(payload);
            if c.provider == "gemini" {
                request = request.query(&[("key", &c.api_key)])
            } else {
                request = request.bearer_auth(&c.api_key)
            }
            match request.send().await {
                Ok(response) => {
                    let code = response.status().as_u16();
                    if response.status().is_success() {
                        return response
                            .json()
                            .await
                            .map_err(|_| QueryError::InvalidProviderResponse);
                    }
                    last = QueryError::Provider(code);
                    let retryable = match c.provider.as_str() {
                        "gemini" => [429, 500, 503].contains(&code),
                        "dashscope" => [429, 500, 502, 503, 504].contains(&code),
                        _ => [429, 500, 502, 503].contains(&code),
                    };
                    if !retryable {
                        return Err(last);
                    }
                }
                Err(e) => last = QueryError::Provider(if e.is_timeout() { 408 } else { 503 }),
            }
            if attempt + 1 < attempts {
                let delay =
                    Duration::from_secs_f64((if dash { 0.5 } else { 1.0 }) * 2f64.powi(attempt));
                let delay = if dash {
                    delay.min(deadline.saturating_duration_since(Instant::now()))
                } else {
                    delay
                };
                tokio::time::sleep(delay).await
            }
        }
        Err(last)
    }
}
pub struct ResolvedEmbedding {
    pub provider: String,
    pub model: String,
    pub dimension: usize,
    pub endpoint: String,
    api_key: String,
    pub timeout: Duration,
}
pub fn canonical_provider(provider: &str) -> String {
    match provider.trim().to_lowercase().as_str() {
        "builtin" | "hash" => "local",
        "aliyun" => "dashscope",
        "aliyun_multimodal" | "multimodal" => "dashscope_multimodal",
        "unified" | "cross_modal" => "unified_multimodal",
        "google" => "gemini",
        "silicon" | "sf" => "siliconflow",
        "vllm" | "tei" | "openai-compatible" => "openai_compatible",
        s => s,
    }
    .into()
}
fn provider_family<'a>(id: &str, api: &str, base: &str) -> Option<&'a str> {
    let i = id.to_lowercase();
    let a = api.to_lowercase();
    let b = base.to_lowercase();
    if i.starts_with("dashscope")
        || i.starts_with("aliyun")
        || ["dashscope", "aliyun"].contains(&a.as_str())
        || b.contains("dashscope.aliyuncs.com")
        || b.contains("dashscope-intl.aliyuncs.com")
    {
        Some("dashscope")
    } else if i.starts_with("gemini")
        || i.starts_with("google")
        || ["google", "google-ai-studio", "google-vertex", "vertex"].contains(&a.as_str())
    {
        Some("gemini")
    } else if i.starts_with("silicon") || b.contains("siliconflow") {
        Some("siliconflow")
    } else {
        None
    }
}
pub fn embeddings_endpoint(s: &str) -> String {
    let s = s.trim().trim_end_matches('/');
    if s.ends_with("/embeddings") {
        s.into()
    } else {
        format!("{s}/embeddings")
    }
}
pub fn normalize_dashscope(s: &str) -> String {
    let mut b = s.trim().trim_end_matches('/').to_string();
    for suffix in [
        "/compatible-mode/v1/chat/completions",
        "/compatible-mode/v1",
        "/compatible-mode",
        "/api/v1",
    ] {
        if b.ends_with(suffix) {
            b.truncate(b.len() - suffix.len());
            break;
        }
    }
    format!("{b}/api/v1")
}
fn infer_dimension(p: &str, m: &str) -> usize {
    if p == "local" {
        return m
            .rsplit('-')
            .next()
            .and_then(|x| x.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(384);
    }
    match m {
        "text-embedding-v1" | "text-embedding-v2" => 1536,
        "netease-youdao/bce-embedding-base_v1" => 512,
        "Qwen/Qwen3-Embedding-4B" => 2560,
        "Qwen/Qwen3-Embedding-8B" => 4096,
        _ => 1024,
    }
}
fn validate_vectors(v: &[Vec<f64>], count: usize, dimension: usize) -> Result<()> {
    if v.len() != count
        || v.iter()
            .any(|x| x.len() != dimension || x.iter().any(|n| !n.is_finite()))
    {
        Err(QueryError::InvalidProviderResponse)
    } else {
        Ok(())
    }
}
fn parse_vectors(data: &Value, provider: &str) -> Result<Vec<Vec<f64>>> {
    let rows = if provider == "gemini" {
        &data["embeddings"]
    } else if provider == "siliconflow" || provider == "openai_compatible" {
        &data["data"]
    } else {
        &data["output"]["embeddings"]
    };
    let rows = rows.as_array().ok_or(QueryError::InvalidProviderResponse)?;
    rows.iter()
        .map(|r| {
            let v = if r.is_array() {
                r
            } else if provider == "gemini" {
                &r["values"]
            } else {
                r.get("embedding")
                    .or(r.get("vector"))
                    .unwrap_or(&Value::Null)
            };
            v.as_array()
                .ok_or(QueryError::InvalidProviderResponse)?
                .iter()
                .map(|n| {
                    n.as_f64()
                        .filter(|n| n.is_finite())
                        .ok_or(QueryError::InvalidProviderResponse)
                })
                .collect()
        })
        .collect()
}
pub fn sanitize_dashscope(text: &str, model: &str) -> String {
    static NONPRINT: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static SPACES: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static LINES: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static LEAD: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let nonprint = NONPRINT
        .get_or_init(|| regex::Regex::new(r"[\p{C}\p{Z}]").expect("static Unicode categories"));
    let text: String = text
        .chars()
        .filter(|c| *c != '\0')
        .map(|c| {
            if c == '\n' || c == '\t' || c == ' ' || !nonprint.is_match(&c.to_string()) {
                c
            } else {
                ' '
            }
        })
        .collect();
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let text = LINES
        .get_or_init(|| regex::Regex::new(r"\n{3,}").expect("static newline regex"))
        .replace_all(&text, "\n\n");
    let text = SPACES
        .get_or_init(|| regex::Regex::new(r"[ \t]{2,}").expect("static whitespace regex"))
        .replace_all(&text, " ");
    let text = LEAD
        .get_or_init(|| regex::Regex::new(r"\n[ \t]+").expect("static whitespace regex"))
        .replace_all(&text, "\n");
    let chars: Vec<_> = text.chars().collect();
    let mut clean = String::new();
    let mut i = 0;
    while i < chars.len() {
        let mut j = i + 1;
        while j < chars.len() && chars[j] == chars[i] && chars[i] != '\n' {
            j += 1
        }
        for _ in 0..if j - i >= 21 { 3 } else { j - i } {
            clean.push(chars[i])
        }
        i = j
    }
    let text = clean.trim();
    let max = match model.to_lowercase().as_str() {
        "text-embedding-v1" | "text-embedding-v2" => 4000,
        "text-embedding-v3" | "text-embedding-v4" => 8000,
        _ => 6000,
    };
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        if let Some(index) = out.rfind(' ') {
            if out[..index].chars().count() > max * 8 / 10 {
                out.truncate(index)
            }
        }
    }
    if out.is_empty() { "empty".into() } else { out }
}

/// BLAKE2b-64, matching hashlib.blake2b(token, digest_size=8), not SHA/FNV.
fn blake2b64(input: &[u8]) -> [u8; 8] {
    const IV: [u64; 8] = [
        0x6a09e667f3bcc908,
        0xbb67ae8584caa73b,
        0x3c6ef372fe94f82b,
        0xa54ff53a5f1d36f1,
        0x510e527fade682d1,
        0x9b05688c2b3e6c1f,
        0x1f83d9abfb41bd6b,
        0x5be0cd19137e2179,
    ];
    const S: [[usize; 16]; 12] = [
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
        [11, 8, 12, 0, 5, 2, 15, 13, 10, 14, 3, 6, 7, 1, 9, 4],
        [7, 9, 3, 1, 13, 12, 11, 14, 2, 6, 5, 10, 4, 0, 15, 8],
        [9, 0, 5, 7, 2, 4, 10, 15, 14, 1, 11, 12, 6, 8, 3, 13],
        [2, 12, 6, 10, 0, 11, 8, 3, 4, 13, 7, 5, 15, 14, 1, 9],
        [12, 5, 1, 15, 14, 13, 4, 10, 0, 7, 6, 3, 9, 2, 8, 11],
        [13, 11, 7, 14, 12, 1, 3, 9, 5, 0, 15, 4, 8, 6, 2, 10],
        [6, 15, 14, 9, 11, 3, 0, 8, 12, 2, 13, 7, 1, 4, 10, 5],
        [10, 2, 8, 4, 7, 6, 1, 5, 15, 11, 9, 14, 3, 12, 13, 0],
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
        [14, 10, 4, 8, 9, 15, 13, 6, 1, 12, 0, 2, 11, 7, 5, 3],
    ];
    fn g(v: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, x: u64, y: u64) {
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(x);
        v[d] = (v[d] ^ v[a]).rotate_right(32);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(24);
        v[a] = v[a].wrapping_add(v[b]).wrapping_add(y);
        v[d] = (v[d] ^ v[a]).rotate_right(16);
        v[c] = v[c].wrapping_add(v[d]);
        v[b] = (v[b] ^ v[c]).rotate_right(63)
    }
    let mut h = IV;
    h[0] ^= 0x01010008;
    let chunks = input.len().max(1).div_ceil(128);
    for block in 0..chunks {
        let start = block * 128;
        let end = (start + 128).min(input.len());
        let mut bytes = [0u8; 128];
        bytes[..end - start].copy_from_slice(&input[start..end]);
        let mut m = [0u64; 16];
        for (i, c) in bytes.chunks_exact(8).enumerate() {
            m[i] = u64::from_le_bytes(c.try_into().unwrap_or([0; 8]))
        }
        let mut v = [0u64; 16];
        v[..8].copy_from_slice(&h);
        v[8..].copy_from_slice(&IV);
        v[12] ^= end as u64;
        if block + 1 == chunks {
            v[14] = !v[14]
        }
        for s in S {
            g(&mut v, 0, 4, 8, 12, m[s[0]], m[s[1]]);
            g(&mut v, 1, 5, 9, 13, m[s[2]], m[s[3]]);
            g(&mut v, 2, 6, 10, 14, m[s[4]], m[s[5]]);
            g(&mut v, 3, 7, 11, 15, m[s[6]], m[s[7]]);
            g(&mut v, 0, 5, 10, 15, m[s[8]], m[s[9]]);
            g(&mut v, 1, 6, 11, 12, m[s[10]], m[s[11]]);
            g(&mut v, 2, 7, 8, 13, m[s[12]], m[s[13]]);
            g(&mut v, 3, 4, 9, 14, m[s[14]], m[s[15]])
        }
        for i in 0..8 {
            h[i] ^= v[i] ^ v[i + 8]
        }
    }
    h[0].to_le_bytes()
}
pub fn local_hash_embedding(text: &str, dimension: usize) -> Vec<f64> {
    let dim = dimension.max(1);
    let mut vector = vec![0.0; dim];
    let mut token = String::new();
    let mut tokens = Vec::new();
    for c in text.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || super::lexical::is_cjk(c) {
            token.push(c)
        } else if !token.is_empty() {
            tokens.push(std::mem::take(&mut token))
        }
    }
    if !token.is_empty() {
        tokens.push(token)
    }
    for t in tokens {
        let hash = blake2b64(t.as_bytes());
        let index = u32::from_le_bytes([hash[0], hash[1], hash[2], hash[3]]) as usize % dim;
        vector[index] += if hash[4] & 1 == 0 { 1.0 } else { -1.0 }
    }
    let norm = vector.iter().map(|v| v * v).sum::<f64>().sqrt();
    if norm > 0.0 {
        for v in &mut vector {
            *v /= norm
        }
    }
    vector
}
impl EmbeddingManager {
    /// DashScope multimodal source limits image batches to one item; preserving
    /// ordering here also preserves failures rather than fabricating vectors.
    pub async fn embed_images(
        &self,
        dataset: &Dataset,
        images: &[Vec<u8>],
    ) -> Result<Vec<Vec<f64>>> {
        let mut vectors = Vec::with_capacity(images.len());
        for image in images {
            vectors.push(self.embed_image_with_context(dataset, image, None).await?)
        }
        Ok(vectors)
    }
}

/// Operator-only TEI profile. This identity never uses hashing, tenant provider
/// overrides, credentials, proxy settings, or redirect destinations.
pub const LOCAL_SEMANTIC_MODEL: &str = "Qwen/Qwen3-Embedding-0.6B";
pub const LOCAL_SEMANTIC_DIMENSION: usize = 1024;

pub fn local_semantic_endpoint(value: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(value)
        .map_err(|_| QueryError::Validation("invalid local semantic endpoint".into()))?;
    let loopback = url
        .host_str()
        .and_then(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .ok()
        })
        .is_some_and(|address| address.is_loopback());
    if !loopback
        || !["http", "https"].contains(&url.scheme())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !["/v1", "/v1/", "/v1/embeddings"].contains(&url.path())
    {
        return Err(QueryError::Validation("local semantic endpoint must be a literal loopback HTTP(S) /v1 URL without credentials".into()));
    }
    url.set_path("/v1/embeddings");
    Ok(url)
}

struct LocalSemanticRuntime {
    client: Client,
    endpoint: reqwest::Url,
    timeout: Duration,
}
impl LocalSemanticRuntime {
    fn configuration(&self) -> ResolvedEmbedding {
        ResolvedEmbedding {
            provider: "local_semantic".into(),
            model: LOCAL_SEMANTIC_MODEL.into(),
            dimension: LOCAL_SEMANTIC_DIMENSION,
            endpoint: self.endpoint.to_string(),
            api_key: String::new(),
            timeout: self.timeout,
        }
    }
    async fn embed(
        &self,
        config: &ResolvedEmbedding,
        texts: &[String],
        kind: &str,
    ) -> Result<Vec<Vec<f64>>> {
        if config.provider != "local_semantic"
            || config.model != LOCAL_SEMANTIC_MODEL
            || config.dimension != LOCAL_SEMANTIC_DIMENSION
            || config.endpoint != self.endpoint.as_str()
            || !config.api_key.is_empty()
        {
            return Err(QueryError::Validation(
                "local semantic model profile mismatch".into(),
            ));
        }
        let inputs: Vec<String> = texts.iter().map(|text| {
            if kind == "query" {
                format!("Instruct: Given a web search query, retrieve relevant passages that answer the query\nQuery:{text}")
            } else { text.clone() }
        }).collect();
        let response = self
            .client
            .post(self.endpoint.clone())
            .timeout(self.timeout)
            .json(&json!({"model":LOCAL_SEMANTIC_MODEL,"input":inputs,"encoding_format":"float"}))
            .send()
            .await
            .map_err(|_| QueryError::Provider(503))?;
        if !response.status().is_success() {
            return Err(QueryError::Provider(response.status().as_u16()));
        }
        let value: Value = response
            .json()
            .await
            .map_err(|_| QueryError::InvalidProviderResponse)?;
        let vectors = parse_vectors(&value, "openai_compatible")?;
        validate_vectors(&vectors, texts.len(), LOCAL_SEMANTIC_DIMENSION)?;
        if vectors
            .iter()
            .any(|vector| vector.iter().all(|value| *value == 0.))
        {
            return Err(QueryError::InvalidProviderResponse);
        }
        Ok(vectors)
    }
    async fn probe(&self, inference: bool) -> Result<()> {
        let mut url = self.endpoint.clone();
        url.set_path("/health");
        let response = self
            .client
            .get(url.clone())
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|_| QueryError::Provider(503))?;
        if !response.status().is_success() {
            return Err(QueryError::Provider(response.status().as_u16()));
        }
        url.set_path("/info");
        let response = self
            .client
            .get(url)
            .timeout(self.timeout)
            .send()
            .await
            .map_err(|_| QueryError::Provider(503))?;
        if !response.status().is_success() {
            return Err(QueryError::Provider(response.status().as_u16()));
        }
        let info: Value = response
            .json()
            .await
            .map_err(|_| QueryError::InvalidProviderResponse)?;
        if info["model_id"].as_str() != Some(LOCAL_SEMANTIC_MODEL) {
            return Err(QueryError::Validation(
                "local semantic server model identity mismatch".into(),
            ));
        }
        if inference {
            self.embed(
                &self.configuration(),
                &["Semantic embedding model readiness".into()],
                "document",
            )
            .await?;
        }
        Ok(())
    }
}
