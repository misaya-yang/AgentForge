//! Runtime-only configuration and resources for the independent Knowledge binary.
//! `from_values` is pure; only `from_environment` reads process environment.
//! No dotenv discovery, schema migration, or implicit executable discovery.
use super::{
    KnowledgeBackends, KnowledgeConnections, KnowledgeError, KnowledgeGraph, PdfBackends, Result,
    settings::*,
};
use crate::pdf_backend::{PopplerPdfBackend, PopplerPdfConfig};
use crate::{migration_knowledge_ingest as ingest, migration_knowledge_query as query};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

pub struct ProcessSettings {
    pub release_identity: Arc<crate::host::release::ReleaseIdentity>,
    pub configuration: KnowledgeConfiguration,
    pub host: String,
    pub port: u16,
    pub rpc: crate::rpc::RpcConfiguration,
    pub database_dsn: String,
    pub database_min: u32,
    pub database_max: u32,
    pub redis_url: String,
    pub poppler: Option<PopplerPdfConfig>,
    pub capability_gaps: Vec<&'static str>,
}
/// Case-insensitive legacy names, nested JSON objects, then __ leaf overrides,
/// matching pydantic-settings precedence without retaining unrelated process keys.
struct Values(BTreeMap<String, String>);
impl Values {
    fn raw(&self, name: &str) -> Option<String> {
        if let Some(value) = self.0.get(name) {
            return Some(value.clone());
        }
        let path: Vec<_> = name.split("__").collect();
        for n in (1..path.len()).rev() {
            let Some(raw) = self.0.get(&path[..n].join("__")) else {
                continue;
            };
            let Ok(mut value) = serde_json::from_str::<Value>(raw) else {
                continue;
            };
            for key in &path[n..] {
                value = value
                    .get(key.to_lowercase())
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            if !value.is_null() {
                return Some(match value {
                    Value::String(v) => v,
                    v => v.to_string(),
                });
            }
        }
        None
    }
    fn text(&self, k: &str, d: &str) -> String {
        self.raw(k).unwrap_or_else(|| d.into())
    }
    fn optional(&self, k: &str) -> Option<String> {
        self.raw(k).filter(|s| !s.is_empty() && s != "null")
    }
    fn number<T: std::str::FromStr>(&self, k: &str, d: T) -> Result<T> {
        match self.raw(k) {
            None => Ok(d),
            Some(s) => s
                .parse()
                .map_err(|_| KnowledgeError::Configuration("numeric_setting")),
        }
    }
    fn flag(&self, k: &str, d: bool) -> Result<bool> {
        match self.raw(k) {
            None => Ok(d),
            Some(s) => match s.to_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Ok(true),
                "0" | "false" | "no" | "off" => Ok(false),
                _ => Err(KnowledgeError::Configuration("boolean_setting")),
            },
        }
    }
    fn list(&self, k: &str, d: Vec<String>) -> Result<Vec<String>> {
        match self.raw(k) {
            None => Ok(d),
            Some(s) => serde_json::from_str(&s)
                .map_err(|_| KnowledgeError::Configuration("json_list_setting")),
        }
    }
    fn backend(&self, k: &str) -> Result<ReplayBackend> {
        match self.text(k, "redis").trim().to_lowercase().as_str() {
            "redis" => Ok(ReplayBackend::Redis),
            "memory" => Ok(ReplayBackend::Memory),
            _ => Err(KnowledgeError::Configuration("internal_state_backend")),
        }
    }
}
impl ProcessSettings {
    pub fn from_environment() -> Result<Self> {
        let cwd = std::env::current_dir()
            .map_err(|_| KnowledgeError::Configuration("working_directory"))?;
        let values = std::env::vars_os()
            .filter_map(|(key, value)| {
                let key = key.into_string().ok()?.to_uppercase();
                if key.starts_with("KNOWLEDGE_")
                    || [
                        "ENVIRONMENT",
                        "REDIS_URL",
                        "GATEWAY_URL",
                        "GATEWAY_ENCRYPTION_KEY",
                        "AI_PLATFORM_INTERNAL_TOKEN",
                        "AI_PLATFORM_COMPATIBILITY_MANIFEST_PATH",
                        "AI_PLATFORM_RELEASE_ID",
                        "AI_PLATFORM_GIT_SHA",
                        "AI_PLATFORM_IMAGE_DIGEST",
                        "AI_PLATFORM_CAPABILITY_PROOF_SECRET",
                    ]
                    .contains(&key.as_str())
                    || [
                        "INTERNAL_",
                        "LLM_",
                        "DEEPSEEK_",
                        "GEMINI_",
                        "GOOGLE_",
                        "DASHSCOPE_",
                        "VERTEX_",
                        "COHERE_",
                        "KB_",
                        "ALIYUN_",
                    ]
                    .iter()
                    .any(|p| key.starts_with(p))
                {
                    Some((key, value.into_string().ok()?))
                } else {
                    None
                }
            })
            .collect();
        Self::from_values(values, cwd)
    }
    pub fn from_values(
        values: BTreeMap<String, String>,
        working_directory: PathBuf,
    ) -> Result<Self> {
        if !working_directory.is_absolute() {
            return Err(KnowledgeError::Configuration("working_directory"));
        }
        let e = Values(
            values
                .into_iter()
                .map(|(k, v)| (k.to_uppercase(), v))
                .collect(),
        );
        let mut identity = crate::host::release::ReleaseIdentity::gateway(&e.0);
        identity.service_id = "knowledge-service".into();
        if identity.manifest_path.is_relative() {
            identity.manifest_path = working_directory.join(&identity.manifest_path);
        }
        let release_identity = Arc::new(identity);
        let mut gaps = Vec::new();
        let token = e.text("AI_PLATFORM_INTERNAL_TOKEN", "");
        let mut keys = BTreeMap::new();
        let mut first_key = None;
        for item in e.text("INTERNAL_AUTH_KEYS", "").split(',') {
            if let Some((kid, value)) = item.trim().split_once(':') {
                let kid = kid.trim();
                let value = value.trim();
                if !kid.is_empty() && !value.is_empty() {
                    first_key.get_or_insert_with(|| kid.to_owned());
                    keys.insert(kid.to_owned(), value.to_owned());
                }
            }
        }
        let mut active = e.text("INTERNAL_AUTH_ACTIVE_KEY_ID", "local");
        if active.trim().is_empty() {
            active = "local".into();
        } else {
            active = active.trim().into();
        }
        if keys.is_empty() {
            keys.insert(active.clone(), token.trim().into());
        } else if !keys.contains_key(&active) {
            active = first_key.ok_or(KnowledgeError::Configuration("signing_key"))?;
        }
        let authentication = AuthenticationSettings {
            environment: e.text("ENVIRONMENT", ""),
            internal_token: token,
            keyring: keys,
            outbound_key_id: Some(active),
            version: e.text("INTERNAL_AUTH_VERSION", "v2"),
            replay_backend: e.backend("INTERNAL_COMM_STATE_BACKEND")?,
            idempotency_backend: e.backend("INTERNAL_IDEMPOTENCY_BACKEND")?,
            idempotency_ttl_seconds: e.number("INTERNAL_IDEMPOTENCY_TTL_SECONDS", 86400)?,
            allow_anonymous: e.flag("KNOWLEDGE_APP__ALLOW_ANONYMOUS", false)?,
            capability_proof_secret: e
                .text("AI_PLATFORM_CAPABILITY_PROOF_SECRET", "")
                .into_bytes(),
            gateway_origin: e.text("GATEWAY_URL", "http://gateway:8080"),
        };
        let mut qdrant = QdrantSettings::default();
        let mut embeddings = EmbeddingSettings::default();
        embeddings.batch_size = e.number("KNOWLEDGE_EMBEDDINGS__BATCH_SIZE", 50)?;
        let mut provider_keys = ProviderKeys::default();
        let mut ocr = OcrSettings::default();
        let mut storage = StorageSettings::default();
        let mut processing = ProcessingSettings::default();
        let mut ragas = query::ragas::RagasConfig::default();
        qdrant.max_retries = e.number("KNOWLEDGE_QDRANT__MAX_RETRIES", 5)?;
        qdrant.retry_base_delay = e.number("KNOWLEDGE_QDRANT__RETRY_BASE_DELAY", 2.)?;
        qdrant.url = e.text("KNOWLEDGE_QDRANT__URL", &qdrant.url);
        qdrant.prefer_grpc = e.flag("KNOWLEDGE_QDRANT__PREFER_GRPC", false)?;
        qdrant.api_key = e.optional("KNOWLEDGE_QDRANT__API_KEY");
        qdrant.timeout_seconds =
            e.number("KNOWLEDGE_QDRANT__TIMEOUT_SECONDS", qdrant.timeout_seconds)?;
        qdrant.bm25_v2_capability_ttl_seconds = e.number(
            "KNOWLEDGE_QDRANT__BM25_V2_CAPABILITY_TTL_SECONDS",
            qdrant.bm25_v2_capability_ttl_seconds,
        )?;
        qdrant.bm25_v2_enabled =
            e.flag("KNOWLEDGE_QDRANT__BM25_V2_ENABLED", qdrant.bm25_v2_enabled)?;
        embeddings.provider = e.text("KNOWLEDGE_EMBEDDINGS__PROVIDER", &embeddings.provider);
        embeddings.model = e.text("KNOWLEDGE_EMBEDDINGS__MODEL", &embeddings.model);
        embeddings.api_key = e.text("KNOWLEDGE_EMBEDDINGS__API_KEY", &embeddings.api_key);
        embeddings.google_api_key = e.text(
            "KNOWLEDGE_EMBEDDINGS__GOOGLE_API_KEY",
            &embeddings.google_api_key,
        );
        embeddings.dashscope_api_key = e.text(
            "KNOWLEDGE_EMBEDDINGS__DASHSCOPE_API_KEY",
            &embeddings.dashscope_api_key,
        );
        embeddings.siliconflow_api_key = e.text(
            "KNOWLEDGE_EMBEDDINGS__SILICONFLOW_API_KEY",
            &embeddings.siliconflow_api_key,
        );
        embeddings.siliconflow_base_url = e.text(
            "KNOWLEDGE_EMBEDDINGS__SILICONFLOW_BASE_URL",
            &embeddings.siliconflow_base_url,
        );
        embeddings.openai_compatible_api_key = e.text(
            "KNOWLEDGE_EMBEDDINGS__OPENAI_COMPATIBLE_API_KEY",
            &embeddings.openai_compatible_api_key,
        );
        embeddings.openai_compatible_base_url = e.text(
            "KNOWLEDGE_EMBEDDINGS__OPENAI_COMPATIBLE_BASE_URL",
            &embeddings.openai_compatible_base_url,
        );
        embeddings.base_url = e.optional("KNOWLEDGE_EMBEDDINGS__BASE_URL");
        embeddings.dimension = e.number("KNOWLEDGE_EMBEDDINGS__DIMENSION", embeddings.dimension)?;
        embeddings.timeout_seconds = e.number(
            "KNOWLEDGE_EMBEDDINGS__TIMEOUT_SECONDS",
            embeddings.timeout_seconds,
        )?;
        ocr.strategy = e.text("KNOWLEDGE_OCR__STRATEGY", &ocr.strategy);
        ocr.enabled = e.flag("KNOWLEDGE_OCR__ENABLED", ocr.enabled)?;
        storage.backend = e.text("KNOWLEDGE_STORAGE__BACKEND", &storage.backend);
        storage.key_prefix = e.text("KNOWLEDGE_STORAGE__KEY_PREFIX", &storage.key_prefix);
        storage.url_expiry_seconds = e.number(
            "KNOWLEDGE_STORAGE__URL_EXPIRY_SECONDS",
            storage.url_expiry_seconds,
        )?;
        processing.document_worker_concurrency = e.number(
            "KNOWLEDGE_PROCESSING__DOCUMENT_WORKER_CONCURRENCY",
            processing.document_worker_concurrency,
        )?;
        processing.retrieval_query_max_concurrency = e.number(
            "KNOWLEDGE_PROCESSING__RETRIEVAL_QUERY_MAX_CONCURRENCY",
            processing.retrieval_query_max_concurrency,
        )?;
        processing.retrieval_cache_ttl_seconds = e.number(
            "KNOWLEDGE_PROCESSING__RETRIEVAL_CACHE_TTL_SECONDS",
            processing.retrieval_cache_ttl_seconds,
        )?;
        processing.large_file_threshold = e.number(
            "KNOWLEDGE_PROCESSING__LARGE_FILE_THRESHOLD",
            processing.large_file_threshold,
        )?;
        processing.streaming_batch_size = e.number(
            "KNOWLEDGE_PROCESSING__STREAMING_BATCH_SIZE",
            processing.streaming_batch_size,
        )?;
        ragas.provider = e.text("KNOWLEDGE_RAGAS_EVAL__PROVIDER", &ragas.provider);
        ragas.model = e.text("KNOWLEDGE_RAGAS_EVAL__MODEL", &ragas.model);
        ragas.base_url = e.optional("KNOWLEDGE_RAGAS_EVAL__BASE_URL");
        ragas.max_contexts = e.number("KNOWLEDGE_RAGAS_EVAL__MAX_CONTEXTS", ragas.max_contexts)?;
        ragas.max_context_chars = e.number(
            "KNOWLEDGE_RAGAS_EVAL__MAX_CONTEXT_CHARS",
            ragas.max_context_chars,
        )?;
        ragas.max_total_context_chars = e.number(
            "KNOWLEDGE_RAGAS_EVAL__MAX_TOTAL_CONTEXT_CHARS",
            ragas.max_total_context_chars,
        )?;
        ragas.max_metrics = e.number("KNOWLEDGE_RAGAS_EVAL__MAX_METRICS", ragas.max_metrics)?;
        ragas.enabled = e.flag("KNOWLEDGE_RAGAS_EVAL__ENABLED", ragas.enabled)?;
        ocr.provider = e.text("KNOWLEDGE_OCR__VLM_PROVIDER", &ocr.provider);
        ocr.model = e.text("KNOWLEDGE_OCR__VLM_MODEL", &ocr.model);
        ocr.api_keys = e.text("KNOWLEDGE_OCR__VLM_API_KEYS", &ocr.api_keys);
        ocr.task = e.text("KNOWLEDGE_OCR__VLM_TASK", &ocr.task);
        ocr.concurrency = e.number("KNOWLEDGE_OCR__VLM_CONCURRENCY", ocr.concurrency)?;
        ocr.timeout_seconds =
            e.number("KNOWLEDGE_OCR__VLM_TIMEOUT_SECONDS", ocr.timeout_seconds)?;
        ocr.min_pixels = e.number("KNOWLEDGE_OCR__VLM_MIN_PIXELS", ocr.min_pixels)?;
        ocr.max_pixels = e.number("KNOWLEDGE_OCR__VLM_MAX_PIXELS", ocr.max_pixels)?;
        ocr.max_tokens = e.number("KNOWLEDGE_OCR__VLM_MAX_TOKENS", ocr.max_tokens)?;
        ocr.base_url = e.optional("KNOWLEDGE_OCR__VLM_BASE_URL");
        ocr.enable_rotate = e.flag("KNOWLEDGE_OCR__VLM_ENABLE_ROTATE", true)?;
        qdrant.bm25_v2_cutover_test_tenants =
            e.text("KNOWLEDGE_QDRANT__BM25_V2_CUTOVER_TEST_TENANTS", "");
        qdrant.interactive_deadline_seconds =
            e.number("KNOWLEDGE_QDRANT_INTERACTIVE_DEADLINE_SECONDS", 3.)?;
        embeddings.encryption_key = e.text("GATEWAY_ENCRYPTION_KEY", "");
        embeddings.multimodal_api_key = e.text("KNOWLEDGE_MULTIMODAL__API_KEY", "");
        provider_keys.llm_api_key = e.text("LLM_API_KEY", "");
        provider_keys.deepseek_api_key = e.text("DEEPSEEK_API_KEY", "");
        provider_keys.deepseek_key = e.text("DEEPSEEK_KEY", "");
        provider_keys.gemini_api_key = e.text("GEMINI_API_KEY", "");
        provider_keys.google_api_key = e.text("GOOGLE_API_KEY", "");
        provider_keys.dashscope_api_key = e.text("DASHSCOPE_API_KEY", "");
        provider_keys.aliyun_key = e.text("ALIYUN_KEY", "");
        provider_keys.vertex_embedding_api_key = e.text("VERTEX_EMBEDDING_API_KEY", "");
        provider_keys.google_embedding_backend = e.text("GOOGLE_EMBEDDING_BACKEND", "");
        // Existing native endpoint normalization consumes the explicitly resolved
        // task URLs; preserve regional configured roots instead of guessing keys.
        let endpoint_resolver =
            ai_platform_gateway::provider_startup_seeder::ChatEndpointResolver::new(&e.0);
        let embedding_endpoint = endpoint_resolver.resolve_dashscope(
            ai_platform_gateway::provider_startup_seeder::EndpointDomain::Embedding,
        );
        let ocr_endpoint = endpoint_resolver
            .resolve_dashscope(ai_platform_gateway::provider_startup_seeder::EndpointDomain::Ocr);
        provider_keys.dashscope_embedding_api_key = embedding_endpoint.api_key;
        provider_keys.dashscope_embedding_base_url = embedding_endpoint.base_url;
        provider_keys.dashscope_ocr_api_key = ocr_endpoint.api_key;
        provider_keys.dashscope_ocr_base_url = ocr_endpoint.base_url;
        storage.local_base_path = e
            .text("KNOWLEDGE_STORAGE__LOCAL_BASE_PATH", "./data/files")
            .into();
        storage.signing_key = e.text("KNOWLEDGE_STORAGE__SIGNING_KEY", "").into_bytes();
        storage.s3_bucket = e.text("KNOWLEDGE_STORAGE__S3__BUCKET", &storage.s3_bucket);
        storage.s3_region = e.text("KNOWLEDGE_STORAGE__S3__REGION", &storage.s3_region);
        storage.s3_access_key = e.text("KNOWLEDGE_STORAGE__S3__ACCESS_KEY", &storage.s3_access_key);
        storage.s3_secret_key = e.text("KNOWLEDGE_STORAGE__S3__SECRET_KEY", &storage.s3_secret_key);
        storage.s3_endpoint = e.optional("KNOWLEDGE_STORAGE__S3__ENDPOINT_URL");
        processing.worker.concurrency = e.number("KNOWLEDGE_PROCESSING__WORKER_CONCURRENCY", 2)?;
        processing.maximum_batch_upload_bytes =
            e.number::<i64>("KB_MAX_BATCH_SIZE_MB", 32)?.clamp(1, 48) as usize * 1024 * 1024;
        processing.pdf_upload = crate::upload::PdfUploadConfig {
            threshold: e.number::<usize>("KB_PDF_SPLIT_MAX_SIZE_MB", 20)?.min(48) * 1024 * 1024,
            pages_per_part: e
                .number::<usize>("KB_PDF_SPLIT_PAGES_PER_PART", 500)?
                .clamp(1, 2000),
            max_pages: e.number::<usize>("KB_PDF_MAX_PAGES", 2000)?.clamp(1, 2000),
            max_output_bytes: e
                .number::<u64>("KB_PDF_SPLIT_MAX_OUTPUT_BYTES", 96 * 1024 * 1024)?
                .clamp(1, 96 * 1024 * 1024),
        };
        processing.maximum_upload_bytes = e
            .number::<usize>("KB_MAX_FILE_SIZE_MB", 16)?
            .checked_mul(1024 * 1024)
            .ok_or(KnowledgeError::Configuration("upload_limit"))?;
        ragas.timeout = seconds(
            e.number("KNOWLEDGE_RAGAS_EVAL__TIMEOUT_SECONDS", 60.)?,
            "ragas_timeout",
        )?;
        ragas.request_timeout = seconds(
            e.number("KNOWLEDGE_RAGAS_EVAL__REQUEST_TIMEOUT_SECONDS", 180.)?,
            "ragas_request_timeout",
        )?;
        ragas.allowed_providers = e
            .list(
                "KNOWLEDGE_RAGAS_EVAL__ALLOWED_PROVIDERS",
                vec!["dashscope".into()],
            )?
            .into_iter()
            .map(|v| v.trim().to_lowercase())
            .collect();
        ragas.allowed_models = e
            .list(
                "KNOWLEDGE_RAGAS_EVAL__ALLOWED_MODELS",
                vec!["qwen3.7-plus".into()],
            )?
            .into_iter()
            .map(|v| v.trim().to_owned())
            .collect();
        let mut qa = QaSettings::default();
        qa.model = e.text("LLM_MODEL", "gemini-2.0-flash");
        qa.base_url = e.text("LLM_BASE_URL", "");
        qa.allowed_models = BTreeSet::from([qa.model.clone()]);
        let scratch = working_directory
            .join(e.text("KNOWLEDGE_NATIVE__SCRATCH_ROOT", "data/knowledge-scratch"));
        let mut parser = ingest::parser::ParserConfig::new(scratch.clone());
        parser.ocr_languages = e.text("KNOWLEDGE_OCR__LANGUAGES", "eng+ara");
        parser.ocr_dpi = e.number("KNOWLEDGE_OCR__RENDER_DPI", 200)?;
        parser.timeout = seconds(
            e.number("KNOWLEDGE_OCR__TESSERACT_TIMEOUT_SECONDS", 60.)?,
            "parser_timeout",
        )?;
        parser.tesseract = e.optional("KNOWLEDGE_NATIVE__TESSERACT").map(PathBuf::from);
        let poppler = if e.flag("KNOWLEDGE_NATIVE__PDF_ENABLED", true)? {
            let info = e.text("KNOWLEDGE_NATIVE__PDFINFO", "/usr/bin/pdfinfo");
            let text = e.text("KNOWLEDGE_NATIVE__PDFTOTEXT", "/usr/bin/pdftotext");
            let mut pdf = PopplerPdfConfig::new(
                info.into(),
                text.into(),
                Some(PathBuf::from(
                    e.text("KNOWLEDGE_NATIVE__PDFTOPPM", "/usr/bin/pdftoppm"),
                )),
                scratch,
            );
            pdf.allow_ingestion_text_alternative = true;
            pdf.pdfimages = Some(
                e.text("KNOWLEDGE_NATIVE__PDFIMAGES", "/usr/bin/pdfimages")
                    .into(),
            );
            pdf.pdfseparate = Some(
                e.text("KNOWLEDGE_NATIVE__PDFSEPARATE", "/usr/bin/pdfseparate")
                    .into(),
            );
            pdf.pdfunite = Some(
                e.text("KNOWLEDGE_NATIVE__PDFUNITE", "/usr/bin/pdfunite")
                    .into(),
            );

            Some(pdf)
        } else {
            None
        };
        // Declared but not consumed by the authoritative Python runtime.
        let _ = e.flag("KNOWLEDGE_METADATA_LLM__ENABLED", false)?;
        gaps.push("other_local_rerank_models_require_explicit_backend");
        let mut rerank_providers = BTreeMap::new();
        {
            let endpoint = e.text("DASHSCOPE_RERANK_BASE_URL", "");
            rerank_providers.insert(
                "dashscope".into(),
                query::rerank::RerankProviderConfig {
                    api_key: provider_keys.dashscope_api_key.clone(),
                    endpoint,
                    request_schema: e.text("DASHSCOPE_RERANK_REQUEST_SCHEMA", "auto"),
                    instruct: e.optional("DASHSCOPE_RERANK_INSTRUCT"),
                },
            );
        }
        if let Some(key) = e.optional("COHERE_API_KEY") {
            rerank_providers.insert(
                "cohere".into(),
                query::rerank::RerankProviderConfig {
                    api_key: key,
                    endpoint: "https://api.cohere.com/v2/rerank".into(),
                    request_schema: "flat".into(),
                    instruct: None,
                },
            );
        }
        let redis_url = e
            .optional("INTERNAL_COMM_REDIS_URL")
            .or_else(|| e.optional("REDIS_URL"))
            .unwrap_or_default();
        let database_min = e.number("KNOWLEDGE_DATABASE__POOL_MIN_SIZE", 2)?;
        let database_max = e.number("KNOWLEDGE_DATABASE__POOL_MAX_SIZE", 10)?;
        if database_min > database_max || database_max == 0 {
            return Err(KnowledgeError::Configuration("database_pool"));
        }
        match e.text("KNOWLEDGE_NATIVE__EMBEDDING_PROFILE", "").as_str() {
            "" => {}
            "local_semantic" => {
                for (name, expected) in [
                    ("KNOWLEDGE_EMBEDDINGS__PROVIDER", "local_semantic"),
                    (
                        "KNOWLEDGE_EMBEDDINGS__MODEL",
                        query::embedding::LOCAL_SEMANTIC_MODEL,
                    ),
                    ("KNOWLEDGE_EMBEDDINGS__DIMENSION", "1024"),
                ] {
                    if e.raw(name).is_some_and(|value| value != expected) {
                        return Err(KnowledgeError::Configuration(
                            "local_semantic_profile_conflict",
                        ));
                    }
                }
                if !embeddings.api_key.is_empty() {
                    return Err(KnowledgeError::Configuration("local_semantic_is_keyless"));
                }
                let endpoint = e.text(
                    "KNOWLEDGE_NATIVE__LOCAL_SEMANTIC_ENDPOINT",
                    "http://127.0.0.1:8093/v1",
                );
                query::embedding::local_semantic_endpoint(&endpoint)
                    .map_err(|_| KnowledgeError::Configuration("local_semantic_endpoint"))?;
                embeddings.provider = "local_semantic".into();
                embeddings.model = query::embedding::LOCAL_SEMANTIC_MODEL.into();
                embeddings.dimension = query::embedding::LOCAL_SEMANTIC_DIMENSION;
                embeddings.local_semantic_endpoint = Some(endpoint);
            }
            _ => return Err(KnowledgeError::Configuration("embedding_profile")),
        }
        let confluence_timeout: f64 = e.number("KNOWLEDGE_CONFLUENCE__REQUEST_TIMEOUT", 30.)?;
        let confluence_retries: u32 = e.number("KNOWLEDGE_CONFLUENCE__MAX_RETRIES", 3)?;
        if !(1.0..=120.0).contains(&confluence_timeout) || confluence_retries > 5 {
            return Err(KnowledgeError::Configuration(
                "confluence_transport_settings",
            ));
        }
        let http_deadline = |key: &str| -> Result<Option<u64>> {
            // Explicit null in a nested settings object is a deliberate disable,
            // while a missing leaf keeps the default. A direct env leaf wins.
            if !e.0.contains_key(key) {
                if let Some(raw) = e.raw("KNOWLEDGE_HTTP") {
                    if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                        let leaf = key
                            .strip_prefix("KNOWLEDGE_HTTP__")
                            .unwrap_or(key)
                            .to_lowercase();
                        if value.get(leaf).is_some_and(Value::is_null) {
                            return Ok(None);
                        }
                    }
                }
            }
            match e.raw(key).as_deref() {
                None => Ok(Some(120_000)),
                Some("null") => Ok(None),
                Some(value) => value
                    .parse()
                    .map(Some)
                    .map_err(|_| KnowledgeError::Configuration("http_governance_deadline")),
            }
        };
        let configuration = KnowledgeConfiguration {
            request_governance:
                ai_platform_gateway::request_governance::RequestGovernanceConfig::new(
                    e.number("KNOWLEDGE_HTTP__MAX_CONCURRENT_REQUESTS", 1024)?,
                    http_deadline("KNOWLEDGE_HTTP__RESPONSE_HEADER_TIMEOUT_MS")?,
                    http_deadline("KNOWLEDGE_HTTP__RESPONSE_BODY_IDLE_TIMEOUT_MS")?,
                )
                .map_err(|_| KnowledgeError::Configuration("http_governance"))?,
            confluence_request_timeout: confluence_timeout,
            confluence_max_retries: confluence_retries,
            confluence_enabled: e.flag("KNOWLEDGE_CONFLUENCE__MANUAL_SYNC_ENABLED", true)?,
            role: RuntimeRole::parse(&e.text("KNOWLEDGE_RUNTIME_ROLE", "all"))?,
            authentication,
            native_cross_encoder_directory: e
                .optional("KNOWLEDGE_NATIVE__CROSS_ENCODER_MODEL_DIRECTORY")
                .map(PathBuf::from)
                .map(|path| {
                    if path.is_absolute() {
                        path
                    } else {
                        working_directory.join(path)
                    }
                })
                .unwrap_or_else(|| {
                    working_directory
                        .join("data/models/ms-marco-MiniLM-L-12-v2")
                        .join(crate::native_cross_encoder::MODEL_REVISION)
                }),
            native_bge_large_directory: e
                .optional("KNOWLEDGE_NATIVE__BGE_LARGE_MODEL_DIRECTORY")
                .map(PathBuf::from)
                .map(|path| {
                    if path.is_absolute() {
                        path
                    } else {
                        working_directory.join(path)
                    }
                })
                .unwrap_or_else(|| {
                    working_directory
                        .join("data/models/bge-reranker-large")
                        .join(crate::native_rerank::LARGE_MODEL_REVISION)
                }),
            native_bge_directory: e
                .optional("KNOWLEDGE_NATIVE__BGE_MODEL_DIRECTORY")
                .map(PathBuf::from)
                .map(|path| {
                    if path.is_absolute() {
                        path
                    } else {
                        working_directory.join(path)
                    }
                })
                .unwrap_or_else(|| {
                    working_directory
                        .join("data/models/bge-reranker-v2-m3")
                        .join(crate::native_rerank::MODEL_REVISION)
                }),
            working_directory,
            qdrant,
            embeddings,
            provider_keys,
            ocr,
            storage,
            processing,
            parser,
            qa: Some(qa),
            ragas,
            dashscope_rerank_region: e.optional("DASHSCOPE_BASE_URL"),
            rerank_providers,
            local_rerank_endpoint: e.optional("KNOWLEDGE_NATIVE__LOCAL_RERANK_ENDPOINT"),
            local_rerank_api_key: e.optional("KNOWLEDGE_NATIVE__LOCAL_RERANK_API_KEY"),
            summary: None,
            cors_origins: e.list(
                "KNOWLEDGE_CORS__ALLOW_ORIGINS",
                vec!["http://localhost:80".into(), "http://localhost:3000".into()],
            )?,
        };
        let rpc = crate::rpc::RpcConfiguration {
            address: e
                .optional("KNOWLEDGE_GRPC__BIND_ADDRESS")
                .map(|s| {
                    s.parse()
                        .map_err(|_| KnowledgeError::Configuration("internal_rpc_address"))
                })
                .transpose()?,
            max_concurrent_requests: e.number("KNOWLEDGE_GRPC__MAX_CONCURRENT_REQUESTS", 32)?,
            deadline: Duration::from_millis(e.number("KNOWLEDGE_GRPC__DEADLINE_MS", 30_000)?),
        };
        rpc.validate()?;
        if rpc.address.is_some() && !configuration.role.api() {
            return Err(KnowledgeError::Configuration(
                "internal_rpc_requires_api_role",
            ));
        }
        Ok(Self {
            release_identity,
            configuration,
            host: e.text("KNOWLEDGE_APP__HOST", "0.0.0.0"),
            port: e.number("KNOWLEDGE_APP__PORT", 8092)?,
            rpc,
            database_dsn: e.text(
                "KNOWLEDGE_DATABASE__DSN",
                "postgresql://localhost:5432/gateway",
            ),
            database_min,
            database_max,
            redis_url,
            poppler,
            capability_gaps: gaps,
        })
    }
    pub async fn construct(self) -> Result<(KnowledgeGraph, String, u16, Vec<&'static str>)> {
        let mut backends = KnowledgeBackends::default();
        backends.image_fetch = Some(Arc::new(
            crate::migration_content::image_adapters::PinnedImageFetch::default(),
        ));
        if let Some(config) = self.poppler {
            backends.pdf = PdfBackends::poppler(Arc::new(
                PopplerPdfBackend::new(config)
                    .map_err(|_| KnowledgeError::Construction("poppler"))?,
            ));
        }
        // Lazy construction leaves all readiness assertions to lifecycle.start.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .min_connections(self.database_min)
            .max_connections(self.database_max)
            .acquire_timeout(Duration::from_secs(10))
            .connect_lazy(&self.database_dsn)
            .map_err(|_| KnowledgeError::Configuration("database_dsn"))?;
        let replay = matches!(
            self.configuration.authentication.replay_backend,
            ReplayBackend::Redis
        );
        let idem = matches!(
            self.configuration.authentication.idempotency_backend,
            ReplayBackend::Redis
        );
        let redis = if replay || idem {
            let client = redis::Client::open(self.redis_url.as_str())
                .map_err(|_| KnowledgeError::Configuration("internal_redis_url"))?;
            Some(
                tokio::time::timeout(
                    Duration::from_secs(10),
                    redis::aio::ConnectionManager::new(client),
                )
                .await
                .map_err(|_| KnowledgeError::Startup("redis_connect_timeout"))?
                .map_err(|_| KnowledgeError::Startup("redis_connect"))?,
            )
        } else {
            None
        };
        let graph = KnowledgeGraph::new(
            KnowledgeConnections {
                database: pool.clone(),
                replay_redis: if replay { redis.clone() } else { None },
                idempotency_redis: if idem { redis } else { None },
            },
            self.configuration,
            backends,
        );
        match graph {
            Ok(mut graph) => {
                graph.release_identity = self.release_identity;
                Ok((graph, self.host, self.port, self.capability_gaps))
            }
            Err(error) => {
                pool.close().await;
                Err(error)
            }
        }
    }
}
