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
    Parse,
    Embed,
    Index,
}
impl RuntimeRole {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "all" => Ok(Self::All),
            "api" => Ok(Self::Api),
            "worker" => Ok(Self::Worker),
            "parse" => Ok(Self::Parse),
            "embed" => Ok(Self::Embed),
            "index" => Ok(Self::Index),
            _ => Err(KnowledgeError::Configuration("runtime_role")),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Api => "api",
            Self::Worker => "worker",
            Self::Parse => "parse",
            Self::Embed => "embed",
            Self::Index => "index",
        }
    }
    pub fn api(self) -> bool {
        matches!(self, Self::All | Self::Api)
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
    pub max_retries: usize,
    pub retry_base_delay: f64,
    pub url: String,
    pub api_key: Option<String>,
    pub tls_ca_file: Option<PathBuf>,
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
            max_retries: 5,
            retry_base_delay: 2.,
            url: "http://localhost:6333".into(),
            api_key: None,
            tls_ca_file: None,
            timeout_seconds: 120.,
            bm25_v2_enabled: false,
            bm25_v2_capability_ttl_seconds: 300.,
            bm25_v2_cutover_test_tenants: String::new(),
            interactive_deadline_seconds: 3.,
        }
    }
}
pub struct EmbeddingSettings {
    pub batch_size: usize,
    pub provider: String,
    pub model: String,
    pub dimension: usize,
    pub timeout_seconds: f64,
    pub local_semantic_enabled: bool,
    pub local_semantic_revision: String,
}
impl Default for EmbeddingSettings {
    fn default() -> Self {
        Self {
            batch_size: 50,
            provider: "dashscope".into(),
            model: "text-embedding-v4".into(),
            dimension: 1024,
            timeout_seconds: 30.,
            local_semantic_enabled: false,
            local_semantic_revision: String::new(),
        }
    }
}
impl EmbeddingSettings {
    pub fn judge_provider(&self) -> Result<query::embedding::ProviderConfig> {
        Ok(query::embedding::ProviderConfig {
            timeout: seconds(self.timeout_seconds, "embedding_timeout")?,
            ..Default::default()
        })
    }
    pub fn providers(
        &self,
        _keys: &ProviderKeys,
    ) -> Result<BTreeMap<String, query::embedding::ProviderConfig>> {
        Ok(BTreeMap::new())
    }
}
/// Compatibility marker for old constructor signatures; contains no key fields.
#[derive(Default)]
pub struct ProviderKeys;
impl ProviderKeys {
    pub fn qa_key(&self, _provider: &str) -> String {
        String::new()
    }
}
pub struct QaSettings {
    pub provider: String,
    pub model: String,
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
            timeout_seconds: 60.,
            batch_timeout_seconds: 180.,
            allowed_providers: BTreeSet::from(["gemini".into()]),
            allowed_models: BTreeSet::from(["gemini-2.0-flash".into()]),
        }
    }
}
impl QaSettings {
    pub fn project(&self, _keys: &ProviderKeys) -> Result<query::qa::QaConfig> {
        let provider = self.provider.trim().to_lowercase();
        let model = self.model.trim().to_owned();
        if !self.allowed_providers.contains(&provider) || !self.allowed_models.contains(&model) {
            return Err(KnowledgeError::Configuration("qa_allowlist"));
        }
        Ok(query::qa::QaConfig {
            api_key: String::new(),
            provider,
            model,
            base_url: String::new(),
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
        _embedding: &EmbeddingSettings,
        _keys: &ProviderKeys,
    ) -> Result<Option<ingest::ocr::OcrConfig>> {
        if !self.enabled || !["vlm", "hybrid"].contains(&self.strategy.as_str()) {
            return Ok(None);
        }
        let model = self.model.trim();
        let provider = if self.provider == "auto" {
            if model.starts_with("gemini-") {
                "gemini"
            } else if model.to_lowercase().contains("deepseek") {
                "siliconflow"
            } else {
                "dashscope"
            }
        } else {
            self.provider.as_str()
        };
        Ok(Some(ingest::ocr::OcrConfig {
            provider: provider.into(),
            model: model.into(),
            endpoint: String::new(),
            api_keys: vec![],
            concurrency: self.concurrency,
            timeout: seconds(self.timeout_seconds, "ocr_timeout")?,
            max_retries: if provider == "siliconflow" { 3 } else { 2 },
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
    pub deployment_mode: String,
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
            deployment_mode: "single-node".into(),
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
        let backend = ai_platform_service_primitives::content::storage_policy::backend(
            &self.deployment_mode,
            Some(&self.backend),
        )
        .map_err(|_| KnowledgeError::Construction("shared_storage"))?;
        if backend == "s3"
            && (self.s3_bucket.is_empty()
                || self.s3_access_key.is_empty()
                || self.s3_secret_key.is_empty())
        {
            return Err(KnowledgeError::Construction("s3_storage_configuration"));
        }
        if backend == "oss" {
            return Err(KnowledgeError::Construction("unsupported_storage_backend"));
        }
        let mut assets = if backend == "s3" {
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
        assets.url_expiry_seconds = if backend == "s3" {
            self.url_expiry_seconds.clamp(1, 900)
        } else {
            self.url_expiry_seconds
        };
        Ok(assets)
    }
}
pub struct ProcessingSettings {
    pub pdf_upload: crate::upload::PdfUploadConfig,
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
            pdf_upload: Default::default(),
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
    pub dataset_policy: Option<ai_platform_knowledge_domain::cedar_dataset_policy::Configuration>,
    pub model_plane: crate::model_plane_identity::ModelPlaneSettings,
    pub connector_encryption_key: String,
    pub request_governance:
        ai_platform_service_primitives::request_governance::RequestGovernanceConfig,
    pub confluence_enabled: bool,
    pub confluence_acl_refresh: bool,
    pub confluence_request_timeout: f64,
    pub confluence_max_retries: u32,
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
    pub dashscope_rerank_region: Option<String>,
    pub rerank_providers: BTreeMap<String, query::rerank::RerankProviderConfig>,
    pub native_cross_encoder_directory: PathBuf,
    pub native_bge_directory: PathBuf,
    pub native_bge_large_directory: PathBuf,
    pub local_rerank_endpoint: Option<String>,
    pub summary: Option<SummarySettings>,
    pub cors_origins: Vec<String>,
}
pub struct SummarySettings {
    pub provider: String,
    pub model: String,
}
pub(crate) fn seconds(value: f64, name: &'static str) -> Result<Duration> {
    Duration::try_from_secs_f64(value).map_err(|_| KnowledgeError::Configuration(name))
}

#[cfg(test)]
mod shared_storage_configuration_tests {
    use super::*;
    #[test]
    fn shared_storage_never_falls_back_to_local() {
        for backend in ["local", "unknown", "s3", "auto"] {
            let settings = StorageSettings {
                backend: backend.into(),
                deployment_mode: "multi-instance".into(),
                ..StorageSettings::default()
            };
            assert!(settings.project(std::path::Path::new(".")).is_err());
        }
    }
    #[test]
    fn single_node_auto_retains_local_backend() {
        assert!(
            StorageSettings {
                backend: "auto".into(),
                ..StorageSettings::default()
            }
            .project(&std::env::temp_dir())
            .is_ok()
        );
    }
}
