//! Captured Knowledge Settings / _SettingsCompat projection. No environment reads.
//! Secrets are intentionally neither Debug nor Serialize. Explicit native parser
//! paths, HTTP endpoints and independent process resources belong to the caller.
use super::{KnowledgeError, Result};
use crate::{migration_knowledge_ingest as ingest, migration_knowledge_query as query};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeRole {
    All,
    Api,
    Worker,
}
impl RuntimeRole {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "all" => Ok(Self::All),
            "api" => Ok(Self::Api),
            "worker" => Ok(Self::Worker),
            _ => Err(KnowledgeError::Configuration("runtime_role")),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Api => "api",
            Self::Worker => "worker",
        }
    }
    pub fn api(self) -> bool {
        self != Self::Worker
    }
    pub fn worker(self) -> bool {
        self != Self::Api
    }
}
#[derive(Clone, Copy)]
pub enum ReplayBackend {
    Redis,
    Memory,
}
/// Both replay and response-idempotency stores are separate configured resources.
pub struct AuthenticationSettings {
    pub environment: String,
    pub internal_token: String,
    pub keyring: BTreeMap<String, String>,
    pub outbound_key_id: Option<String>,
    pub version: String,
    pub replay_backend: ReplayBackend,
    pub idempotency_backend: ReplayBackend,
    pub idempotency_ttl_seconds: u64,
    pub allow_anonymous: bool,
    pub capability_proof_secret: Vec<u8>,
    pub gateway_origin: String,
}
pub struct QdrantSettings {
    pub prefer_grpc: bool,
    pub url: String,
    pub api_key: Option<String>,
    pub timeout_seconds: f64,
    pub bm25_v2_enabled: bool,
    pub bm25_v2_capability_ttl_seconds: f64,
    pub bm25_v2_cutover_test_tenants: String,
    pub interactive_deadline_seconds: f64,
}
impl Default for QdrantSettings {
    fn default() -> Self {
        Self {
            prefer_grpc: false,
            url: "http://localhost:6333".into(),
            api_key: None,
            timeout_seconds: 120.,
            bm25_v2_enabled: false,
            bm25_v2_capability_ttl_seconds: 300.,
            bm25_v2_cutover_test_tenants: String::new(),
            interactive_deadline_seconds: 3.,
        }
    }
}
pub struct EmbeddingSettings {
    pub provider: String,
    pub model: String,
    pub dimension: usize,
    pub api_key: String,
    pub base_url: Option<String>,
    pub google_api_key: String,
    pub dashscope_api_key: String,
    pub siliconflow_api_key: String,
    pub siliconflow_base_url: String,
    pub openai_compatible_api_key: String,
    pub openai_compatible_base_url: String,
    pub timeout_seconds: f64,
    pub encryption_key: String,
    pub multimodal_api_key: String,
    /// Explicit opt-in TEI endpoint; never inferred from missing cloud credentials.
    pub local_semantic_endpoint: Option<String>,
}
impl Default for EmbeddingSettings {
    fn default() -> Self {
        Self {
            provider: "dashscope".into(),
            model: "text-embedding-v4".into(),
            dimension: 1024,
            api_key: String::new(),
            base_url: None,
            google_api_key: String::new(),
            dashscope_api_key: String::new(),
            siliconflow_api_key: String::new(),
            siliconflow_base_url: "https://api.siliconflow.cn/v1".into(),
            openai_compatible_api_key: String::new(),
            openai_compatible_base_url: String::new(),
            timeout_seconds: 30.,
            encryption_key: String::new(),
            multimodal_api_key: String::new(),
            local_semantic_endpoint: None,
        }
    }
}
fn first(values: &[&str]) -> String {
    values
        .iter()
        .find(|v| !v.is_empty())
        .copied()
        .unwrap_or("")
        .into()
}
impl EmbeddingSettings {
    /// RAGAS uses the plain configured embedding client, with no tenant lookup.
    pub fn judge_provider(&self) -> Result<query::embedding::ProviderConfig> {
        let dedicated = match self.provider.as_str() {
            "gemini" => &self.google_api_key,
            "dashscope" => &self.dashscope_api_key,
            "siliconflow" => &self.siliconflow_api_key,
            "openai_compatible" => &self.openai_compatible_api_key,
            _ => "",
        };
        Ok(query::embedding::ProviderConfig {
            api_key: first(&[dedicated, &self.api_key]),
            base_url: self.base_url.clone(),
            timeout: seconds(self.timeout_seconds, "ragas_embedding_timeout")?,
        })
    }
    pub fn providers(
        &self,
        keys: &ProviderKeys,
    ) -> Result<BTreeMap<String, query::embedding::ProviderConfig>> {
        let timeout = seconds(self.timeout_seconds, "embedding_timeout")?;
        let selected = |provider: &str| {
            if self.provider == provider {
                self.api_key.as_str()
            } else {
                ""
            }
        };
        let vertex = if keys
            .google_embedding_backend
            .trim()
            .eq_ignore_ascii_case("vertex")
        {
            keys.vertex_embedding_api_key.as_str()
        } else {
            ""
        };
        let gemini = first(&[
            &self.google_api_key,
            selected("gemini"),
            vertex,
            &keys.gemini_api_key,
            &keys.google_api_key,
        ]);
        let dashscope = first(&[
            &self.dashscope_api_key,
            selected("dashscope"),
            &keys.dashscope_embedding_api_key,
        ]);
        let siliconflow = first(&[&self.siliconflow_api_key, selected("siliconflow")]);
        let mut rows = BTreeMap::new();
        for (provider, key, base_url) in [
            ("gemini", gemini, None),
            (
                "dashscope",
                dashscope.clone(),
                Some(keys.dashscope_embedding_base_url.clone()).filter(|s| !s.is_empty()),
            ),
            (
                "siliconflow",
                siliconflow,
                Some(first(&[
                    &self.siliconflow_base_url,
                    self.base_url.as_deref().unwrap_or(""),
                    "https://api.siliconflow.cn/v1",
                ])),
            ),
            (
                "openai_compatible",
                self.openai_compatible_api_key.clone(),
                Some(self.openai_compatible_base_url.clone()).filter(|s| !s.is_empty()),
            ),
        ] {
            rows.insert(
                provider.into(),
                query::embedding::ProviderConfig {
                    api_key: key,
                    base_url,
                    timeout,
                },
            );
        }
        let multimodal = query::embedding::ProviderConfig {
            api_key: first(&[&self.multimodal_api_key, &dashscope]),
            base_url: Some(keys.dashscope_embedding_base_url.clone()).filter(|s| !s.is_empty()),
            timeout,
        };
        rows.insert("dashscope_multimodal".into(), multimodal.clone());
        rows.insert("unified_multimodal".into(), multimodal);
        Ok(rows)
    }
}
/// Raw provider-key fallback values from Settings; never inferred from a request.
#[derive(Default)]
pub struct ProviderKeys {
    pub llm_api_key: String,
    pub deepseek_api_key: String,
    pub deepseek_key: String,
    pub gemini_api_key: String,
    pub google_api_key: String,
    pub dashscope_api_key: String,
    pub aliyun_key: String,
    /// Already resolved by the deployment's endpoint configuration, not getenv.
    pub dashscope_ocr_api_key: String,
    pub dashscope_embedding_api_key: String,
    pub dashscope_embedding_base_url: String,
    pub vertex_embedding_api_key: String,
    pub google_embedding_backend: String,
    pub dashscope_ocr_base_url: String,
}
impl ProviderKeys {
    pub fn qa_key(&self, provider: &str) -> String {
        match provider {
            "deepseek" => first(&[
                &self.llm_api_key,
                &self.deepseek_api_key,
                &self.deepseek_key,
            ]),
            "gemini" => first(&[
                &self.llm_api_key,
                &self.gemini_api_key,
                &self.google_api_key,
            ]),
            "dashscope" => first(&[&self.llm_api_key, &self.dashscope_api_key, &self.aliyun_key]),
            _ => self.llm_api_key.clone(),
        }
    }
}
pub struct QaSettings {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub timeout_seconds: f64,
    pub batch_timeout_seconds: f64,
    pub allowed_providers: BTreeSet<String>,
    pub allowed_models: BTreeSet<String>,
}
impl Default for QaSettings {
    fn default() -> Self {
        Self {
            provider: "gemini".into(),
            model: "gemini-2.0-flash".into(),
            base_url: String::new(),
            timeout_seconds: 60.,
            batch_timeout_seconds: 180.,
            allowed_providers: BTreeSet::from(["gemini".into()]),
            allowed_models: BTreeSet::from(["gemini-2.0-flash".into()]),
        }
    }
}
impl QaSettings {
    pub fn project(&self, keys: &ProviderKeys) -> Result<query::qa::QaConfig> {
        let provider = self.provider.trim().to_lowercase();
        let model = self.model.trim().to_owned();
        if !self.allowed_providers.contains(&provider) || !self.allowed_models.contains(&model) {
            return Err(KnowledgeError::Configuration("qa_allowlist"));
        }
        let base_url = if self.base_url.is_empty() {
            match provider.as_str() {
                "gemini" => "https://generativelanguage.googleapis.com/v1beta/openai",
                "dashscope" => "https://dashscope.aliyuncs.com/compatible-mode/v1",
                _ => "https://api.deepseek.com/v1",
            }
            .into()
        } else {
            self.base_url.trim_end_matches('/').into()
        };
        Ok(query::qa::QaConfig {
            api_key: keys.qa_key(&provider),
            provider,
            model,
            base_url,
            allowed_providers: self.allowed_providers.clone(),
            allowed_models: self.allowed_models.clone(),
            timeout: seconds(self.timeout_seconds, "qa_timeout")?,
        })
    }
}
pub struct OcrSettings {
    pub enabled: bool,
    pub strategy: String,
    pub provider: String,
    pub model: String,
    pub api_keys: String,
    pub base_url: Option<String>,
    pub concurrency: usize,
    pub timeout_seconds: f64,
    pub task: String,
    pub min_pixels: usize,
    pub max_pixels: usize,
    pub max_tokens: usize,
    pub enable_rotate: bool,
}
impl Default for OcrSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            strategy: "hybrid".into(),
            provider: "dashscope".into(),
            model: "qwen-vl-ocr".into(),
            api_keys: String::new(),
            base_url: None,
            concurrency: 4,
            timeout_seconds: 30.,
            task: "document_parsing".into(),
            min_pixels: 3072,
            max_pixels: 8_388_608,
            max_tokens: 8192,
            enable_rotate: true,
        }
    }
}
impl OcrSettings {
    pub fn project(
        &self,
        embedding: &EmbeddingSettings,
        keys: &ProviderKeys,
    ) -> Result<Option<ingest::ocr::OcrConfig>> {
        if !self.enabled || !["vlm", "hybrid"].contains(&self.strategy.as_str()) {
            return Ok(None);
        }
        let mut provider = self.provider.trim().to_lowercase();
        let model = self.model.trim();
        if provider == "auto" {
            provider = if model.starts_with("gemini-") {
                "gemini"
            } else if model.to_lowercase().contains("deepseek") {
                "siliconflow"
            } else {
                "dashscope"
            }
            .into();
        }
        let mut api_keys: Vec<String> = self
            .api_keys
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        let (endpoint, retries) =
            if provider == "siliconflow" || model.to_lowercase().contains("deepseek") {
                provider = "siliconflow".into();
                (
                    self.base_url
                        .clone()
                        .unwrap_or_else(|| "https://api.siliconflow.cn/v1".into()),
                    3,
                )
            } else if provider == "gemini" {
                let key = first(&[
                    &keys.gemini_api_key,
                    &keys.google_api_key,
                    &embedding.google_api_key,
                    if embedding.provider == "gemini" {
                        &embedding.api_key
                    } else {
                        ""
                    },
                ]);
                if api_keys.is_empty() && !key.is_empty() {
                    api_keys.push(key);
                }
                (
                    self.base_url.clone().unwrap_or_else(|| {
                        "https://generativelanguage.googleapis.com/v1beta".into()
                    }),
                    2,
                )
            } else {
                let key = first(&[
                    api_keys.first().map(String::as_str).unwrap_or(""),
                    &keys.dashscope_ocr_api_key,
                    &embedding.dashscope_api_key,
                    if embedding.provider == "dashscope" {
                        &embedding.api_key
                    } else {
                        ""
                    },
                ]);
                if api_keys.is_empty() && !key.is_empty() {
                    api_keys.push(key);
                }
                (
                    self.base_url
                        .clone()
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| {
                            first(&[
                                &keys.dashscope_ocr_base_url,
                                "https://dashscope.aliyuncs.com",
                            ])
                        }),
                    2,
                )
            };
        if api_keys.is_empty() {
            return Ok(None);
        }
        Ok(Some(ingest::ocr::OcrConfig {
            provider,
            model: model.into(),
            endpoint,
            api_keys,
            concurrency: self.concurrency,
            timeout: seconds(self.timeout_seconds, "ocr_timeout")?,
            max_retries: retries,
            task: self.task.clone(),
            min_pixels: self.min_pixels,
            max_pixels: self.max_pixels,
            max_tokens: self.max_tokens,
            enable_rotate: self.enable_rotate,
        }))
    }
}
pub struct StorageSettings {
    pub backend: String,
    pub local_base_path: PathBuf,
    pub key_prefix: String,
    pub url_expiry_seconds: u32,
    pub signing_key: Vec<u8>,
    pub s3_bucket: String,
    pub s3_region: String,
    pub s3_endpoint: Option<String>,
    pub s3_access_key: String,
    pub s3_secret_key: String,
}
impl Default for StorageSettings {
    fn default() -> Self {
        Self {
            backend: "local".into(),
            local_base_path: "data/files".into(),
            key_prefix: "dev".into(),
            url_expiry_seconds: 3600,
            signing_key: vec![],
            s3_bucket: String::new(),
            s3_region: "us-east-1".into(),
            s3_endpoint: None,
            s3_access_key: String::new(),
            s3_secret_key: String::new(),
        }
    }
}
impl StorageSettings {
    pub fn project(
        self,
        working_directory: &std::path::Path,
    ) -> Result<ingest::assets::AssetStorage> {
        let mut assets = if self.backend == "s3" && !self.s3_bucket.is_empty() {
            let endpoint = self
                .s3_endpoint
                .unwrap_or_else(|| format!("https://s3.{}.amazonaws.com", self.s3_region));
            ingest::assets::S3Assets::new(
                super::endpoint(&endpoint, "storage_endpoint")?,
                self.s3_bucket,
                self.s3_region,
                self.key_prefix,
                self.s3_access_key,
                self.s3_secret_key,
                None,
            )
        } else {
            let root = if self.local_base_path.is_absolute() {
                self.local_base_path
            } else {
                working_directory.join(self.local_base_path)
            };
            ingest::assets::AssetStorage::local(root, self.key_prefix)
        }
        .map_err(|_| KnowledgeError::Construction("asset_storage"))?;
        if !self.signing_key.is_empty() {
            assets = assets.with_signing_key(self.signing_key);
        }
        assets.url_expiry_seconds = self.url_expiry_seconds;
        Ok(assets)
    }
}
pub struct ProcessingSettings {
    pub worker: ingest::worker::WorkerConfig,
    pub document_worker_concurrency: usize,
    pub retrieval_query_max_concurrency: usize,
    pub retrieval_cache_ttl_seconds: f64,
    pub large_file_threshold: usize,
    pub streaming_batch_size: usize,
    pub maximum_upload_bytes: usize,
    pub maximum_batch_upload_bytes: usize,
}
impl Default for ProcessingSettings {
    fn default() -> Self {
        Self {
            worker: Default::default(),
            document_worker_concurrency: 3,
            retrieval_query_max_concurrency: 8,
            retrieval_cache_ttl_seconds: 300.,
            large_file_threshold: 50 * 1024 * 1024,
            streaming_batch_size: 20,
            maximum_upload_bytes: 16 * 1024 * 1024,
            maximum_batch_upload_bytes: 32 * 1024 * 1024,
        }
    }
}
pub struct KnowledgeConfiguration {
    pub role: RuntimeRole,
    pub authentication: AuthenticationSettings,
    pub working_directory: PathBuf,
    pub qdrant: QdrantSettings,
    pub embeddings: EmbeddingSettings,
    pub provider_keys: ProviderKeys,
    pub ocr: OcrSettings,
    pub storage: StorageSettings,
    pub processing: ProcessingSettings,
    pub parser: ingest::parser::ParserConfig,
    pub qa: Option<QaSettings>,
    pub ragas: query::ragas::RagasConfig,
    pub rerank_providers: BTreeMap<String, query::rerank::RerankProviderConfig>,
    pub local_rerank_endpoint: Option<String>,
    pub local_rerank_api_key: Option<String>,
    pub summary: Option<SummarySettings>,
    pub cors_origins: Vec<String>,
}
pub struct SummarySettings {
    pub endpoint: String,
    pub model: String,
    pub authorization: Option<String>,
}
pub(crate) fn seconds(value: f64, name: &'static str) -> Result<Duration> {
    Duration::try_from_secs_f64(value).map_err(|_| KnowledgeError::Configuration(name))
}
