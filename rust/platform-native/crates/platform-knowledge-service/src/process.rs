//! Runtime-only configuration and resources for the independent Knowledge binary.
//! `from_values` consumes an explicit snapshot; file secrets are read only when configured.
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

fn retired_model_setting(key: &str) -> bool {
    ([
        "LLM_API_KEY",
        "DEEPSEEK_API_KEY",
        "DEEPSEEK_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "DASHSCOPE_API_KEY",
        "ALIYUN_KEY",
        "VERTEX_EMBEDDING_API_KEY",
        "COHERE_API_KEY",
        "GATEWAY_ENCRYPTION_KEY",
        "LLM_BASE_URL",
        "KNOWLEDGE_MULTIMODAL__API_KEY",
        "KNOWLEDGE_OCR__VLM_API_KEYS",
        "KNOWLEDGE_OCR__VLM_BASE_URL",
        "KNOWLEDGE_NATIVE__LOCAL_RERANK_API_KEY",
        "KNOWLEDGE_NATIVE__LOCAL_RERANK_ENDPOINT",
        "KNOWLEDGE_NATIVE__LOCAL_SEMANTIC_ENDPOINT",
        "KNOWLEDGE_RAGAS_EVAL__BASE_URL",
    ]
    .contains(&key))
        || (key.starts_with("DASHSCOPE_")
            && (key.ends_with("API_KEY") || key.ends_with("BASE_URL")))
        || (key.starts_with("KNOWLEDGE_EMBEDDINGS__")
            && (key.ends_with("API_KEY") || key.ends_with("BASE_URL")))
}
pub struct ProcessSettings {
    broker_environment: BTreeMap<String, String>,
    pub release_identity: Arc<crate::host::release::ReleaseIdentity>,
    pub configuration: KnowledgeConfiguration,
    pub host: String,
    pub port: u16,
    pub rpc: crate::rpc::RpcConfiguration,
    pub database: Option<ai_platform_service_primitives::data_connection::ConnectionConfig>,
    pub workload: Option<crate::workload::Configuration>,
    pub redis_url: String,
    pub valkey: Option<ai_platform_service_primitives::valkey::SplitConfig>,
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
    fn private_file_or_legacy(&self, name: &str) -> Result<String> {
        let file_name = format!("{name}_FILE");
        let Some(file) = self.raw(&file_name) else {
            return Ok(self.text(name, ""));
        };
        let mut values = BTreeMap::from([(file_name, file)]);
        if let Some(inline) = self.raw(name) {
            values.insert(name.to_owned(), inline);
        }
        // The file authority rejects inline conflicts, unreadable/private-mode
        // failures and empty paths. A selected file never falls back to inline.
        Ok(crate::workload::file_secret(&values, name)?.unwrap_or_default())
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
                    || key.starts_with("AI_PLATFORM_VALKEY_")
                    || key.starts_with("AI_PLATFORM_WORKLOAD_")
                    || key.starts_with("AI_PLATFORM_LEASE_")
                    || key.starts_with("AI_PLATFORM_BROKER_PROOF_")
                    || key == "AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE"
                    || key.starts_with("DATA_KNOWLEDGE_")
                    || key == "AI_PLATFORM_INTERNAL_TOKEN_FILE"
                    || key == "AI_PLATFORM_CAPABILITY_PROOF_SECRET_FILE"
                    || [
                        "ENVIRONMENT",
                        "REDIS_URL",
                        "GATEWAY_URL",
                        "LLM_MODEL",
                        "DASHSCOPE_RERANK_REQUEST_SCHEMA",
                        "DASHSCOPE_RERANK_INSTRUCT",
                        "GATEWAY_ENCRYPTION_KEY",
                        "AI_PLATFORM_INTERNAL_TOKEN",
                        "AI_PLATFORM_COMPATIBILITY_MANIFEST_PATH",
                        "AI_PLATFORM_RELEASE_ID",
                        "AI_PLATFORM_GIT_SHA",
                        "AI_PLATFORM_IMAGE_DIGEST",
                        "AI_PLATFORM_CAPABILITY_PROOF_SECRET",
                    ]
                    .contains(&key.as_str())
                    || ["INTERNAL_", "KB_"].iter().any(|p| key.starts_with(p))
                {
                    if retired_model_setting(&key) {
                        None
                    } else {
                        Some((key, value.into_string().ok()?))
                    }
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
        if values.iter().any(|(key, value)| {
            !value.trim().is_empty() && retired_model_setting(&key.to_uppercase())
        }) {
            return Err(KnowledgeError::Configuration(
                "knowledge_models_require_gateway",
            ));
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
        let workload = if e.0.contains_key("AI_PLATFORM_WORKLOAD_MTLS_MODE")
            || e.0.contains_key("AI_PLATFORM_INTERNAL_TOKEN_FILE")
        {
            Some(crate::workload::Configuration::from_values(&e.0)?)
        } else {
            None
        };
        // Legacy library fixtures remain parseable. Executable construction below
        // requires the file-backed transport configuration, without an inline fallback.
        let token = if let Some(workload) = &workload {
            workload.legacy_secret().unwrap_or_default().to_owned()
        } else {
            e.text("AI_PLATFORM_INTERNAL_TOKEN", "")
        };
        let signing_keys = if e.0.contains_key("INTERNAL_AUTH_KEYS_FILE") {
            crate::workload::file_secret(&e.0, "INTERNAL_AUTH_KEYS")?.unwrap_or_default()
        } else {
            e.text("INTERNAL_AUTH_KEYS", "")
        };
        let required = workload.as_ref().is_some_and(|configuration| {
            configuration.mode()
                == ai_platform_service_primitives::workload_identity::Mode::Required
        });
        let (mut keys, first_key) = domain_signing_keys(&signing_keys, required)?;
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
            // Transport credentials stay absent in required mode. Domain HMAC
            // keys use the explicit pure-v2 constructor, never this token field.
            internal_token: token,
            keyring: keys,
            outbound_key_id: Some(active),
            version: e.text("INTERNAL_AUTH_VERSION", "v2"),
            replay_backend: e.backend("INTERNAL_COMM_STATE_BACKEND")?,
            idempotency_backend: e.backend("INTERNAL_IDEMPOTENCY_BACKEND")?,
            idempotency_ttl_seconds: e.number("INTERNAL_IDEMPOTENCY_TTL_SECONDS", 86400)?,
            allow_anonymous: e.flag("KNOWLEDGE_APP__ALLOW_ANONYMOUS", false)?,
            capability_proof_secret: if e.0.contains_key("AI_PLATFORM_CAPABILITY_PROOF_SECRET_FILE")
            {
                crate::workload::file_secret(&e.0, "AI_PLATFORM_CAPABILITY_PROOF_SECRET")?
                    .unwrap_or_default()
                    .into_bytes()
            } else {
                e.text("AI_PLATFORM_CAPABILITY_PROOF_SECRET", "")
                    .into_bytes()
            },
            gateway_origin: e.text("GATEWAY_URL", "http://gateway:8080"),
        };
        let mut qdrant = QdrantSettings::default();
        let mut embeddings = EmbeddingSettings::default();
        embeddings.batch_size = e.number("KNOWLEDGE_EMBEDDINGS__BATCH_SIZE", 50)?;
        let provider_keys = ProviderKeys;
        let mut ocr = OcrSettings::default();
        let mut storage = StorageSettings::default();
        let mut processing = ProcessingSettings::default();
        let mut ragas = query::ragas::RagasConfig::default();
        qdrant.max_retries = e.number("KNOWLEDGE_QDRANT__MAX_RETRIES", 5)?;
        qdrant.retry_base_delay = e.number("KNOWLEDGE_QDRANT__RETRY_BASE_DELAY", 2.)?;
        qdrant.url = e.text("KNOWLEDGE_QDRANT__URL", &qdrant.url);
        qdrant.prefer_grpc = e.flag("KNOWLEDGE_QDRANT__PREFER_GRPC", false)?;
        qdrant.api_key = e.optional("KNOWLEDGE_QDRANT__API_KEY");
        qdrant.tls_ca_file = e
            .optional("KNOWLEDGE_QDRANT__TLS_CA_FILE")
            .map(PathBuf::from);
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
        embeddings.dimension = e.number("KNOWLEDGE_EMBEDDINGS__DIMENSION", embeddings.dimension)?;
        embeddings.timeout_seconds = e.number(
            "KNOWLEDGE_EMBEDDINGS__TIMEOUT_SECONDS",
            embeddings.timeout_seconds,
        )?;
        ocr.strategy = e.text("KNOWLEDGE_OCR__STRATEGY", &ocr.strategy);
        ocr.enabled = e.flag("KNOWLEDGE_OCR__ENABLED", ocr.enabled)?;
        storage.deployment_mode = e.text("AI_PLATFORM_DEPLOYMENT_MODE", "single-node");
        storage.backend = e.text(
            "KNOWLEDGE_STORAGE__BACKEND",
            if storage.deployment_mode == "multi-instance" {
                "s3"
            } else {
                "local"
            },
        );
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
        ocr.task = e.text("KNOWLEDGE_OCR__VLM_TASK", &ocr.task);
        ocr.concurrency = e.number("KNOWLEDGE_OCR__VLM_CONCURRENCY", ocr.concurrency)?;
        ocr.timeout_seconds =
            e.number("KNOWLEDGE_OCR__VLM_TIMEOUT_SECONDS", ocr.timeout_seconds)?;
        ocr.min_pixels = e.number("KNOWLEDGE_OCR__VLM_MIN_PIXELS", ocr.min_pixels)?;
        ocr.max_pixels = e.number("KNOWLEDGE_OCR__VLM_MAX_PIXELS", ocr.max_pixels)?;
        ocr.max_tokens = e.number("KNOWLEDGE_OCR__VLM_MAX_TOKENS", ocr.max_tokens)?;
        ocr.enable_rotate = e.flag("KNOWLEDGE_OCR__VLM_ENABLE_ROTATE", true)?;
        qdrant.bm25_v2_cutover_test_tenants =
            e.text("KNOWLEDGE_QDRANT__BM25_V2_CUTOVER_TEST_TENANTS", "");
        qdrant.interactive_deadline_seconds =
            e.number("KNOWLEDGE_QDRANT_INTERACTIVE_DEADLINE_SECONDS", 3.)?;
        storage.local_base_path = e
            .text("KNOWLEDGE_STORAGE__LOCAL_BASE_PATH", "./data/files")
            .into();
        storage.signing_key = e
            .private_file_or_legacy("KNOWLEDGE_STORAGE__SIGNING_KEY")?
            .into_bytes();
        storage.s3_bucket = e.text("KNOWLEDGE_STORAGE__S3__BUCKET", &storage.s3_bucket);
        storage.s3_region = e.text("KNOWLEDGE_STORAGE__S3__REGION", &storage.s3_region);
        storage.s3_access_key = e.text("KNOWLEDGE_STORAGE__S3__ACCESS_KEY", &storage.s3_access_key);
        storage.s3_secret_key = e.text("KNOWLEDGE_STORAGE__S3__SECRET_KEY", &storage.s3_secret_key);
        storage.s3_endpoint = e.optional("KNOWLEDGE_STORAGE__S3__ENDPOINT_URL");
        processing.worker.concurrency = e.number("KNOWLEDGE_PROCESSING__WORKER_CONCURRENCY", 2)?;
        let stage = e.text("KNOWLEDGE_RUNTIME_ROLE", "all");
        processing.worker.pipeline = ingest::pipeline::Configuration {
            enabled: e.flag("KNOWLEDGE_PIPELINE__ENABLED", false)?
                || ["parse", "embed", "index"].contains(&stage.as_str()),
            stage: if stage == "api" {
                ingest::pipeline::Stage::Worker
            } else {
                ingest::pipeline::Stage::parse(&stage)
                    .map_err(|_| KnowledgeError::Configuration("pipeline_stage"))?
            },
            lease: seconds(
                e.number("KNOWLEDGE_PIPELINE__LEASE_SECONDS", 60.)?,
                "pipeline_lease",
            )?,
            renew: seconds(
                e.number("KNOWLEDGE_PIPELINE__RENEW_SECONDS", 20.)?,
                "pipeline_renew",
            )?,
        };
        processing
            .worker
            .pipeline
            .validate()
            .map_err(|_| KnowledgeError::Configuration("pipeline_lease"))?;
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
        qa.allowed_models = BTreeSet::from([qa.model.clone()]);
        let scratch = working_directory
            .join(e.text("KNOWLEDGE_NATIVE__SCRATCH_ROOT", "data/knowledge-scratch"));
        let sandbox = match (
            e.optional("KNOWLEDGE_PARSER_SANDBOX__LAUNCHER"),
            e.optional("KNOWLEDGE_PARSER_SANDBOX__ROOTFS"),
        ) {
            (None, None) => None,
            (Some(launcher), Some(rootfs)) => {
                let policy = ingest::parser_sandbox::SandboxPolicy {
                    launcher: launcher.into(),
                    rootfs: rootfs.into(),
                    scratch: scratch.clone(),
                };
                policy
                    .validate()
                    .map_err(|_| KnowledgeError::Configuration("parser_sandbox"))?;
                Some(policy)
            }
            _ => return Err(KnowledgeError::Configuration("parser_sandbox")),
        };
        let mut parser = ingest::parser::ParserConfig::new(scratch.clone());
        parser.sandbox = sandbox.clone();
        parser.converters_enabled =
            ["all", "worker", "parse"].contains(&e.text("KNOWLEDGE_RUNTIME_ROLE", "all").as_str());
        parser.ocr_languages = e.text("KNOWLEDGE_OCR__LANGUAGES", "eng+ara");
        parser.ocr_dpi = e.number("KNOWLEDGE_OCR__RENDER_DPI", 200)?;
        parser.timeout = seconds(
            e.number("KNOWLEDGE_OCR__TESSERACT_TIMEOUT_SECONDS", 60.)?,
            "parser_timeout",
        )?;
        parser.tesseract = e.optional("KNOWLEDGE_NATIVE__TESSERACT").map(PathBuf::from);
        if sandbox.is_none() {
            gaps.push("credential_free_parser_sandbox_not_configured");
            parser.tesseract = None;
        }
        let poppler = if e.flag("KNOWLEDGE_NATIVE__PDF_ENABLED", true)? && sandbox.is_some() {
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
            pdf.sandbox = sandbox;
            pdf.execution_enabled = parser.converters_enabled;
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
            let endpoint = String::new();
            rerank_providers.insert(
                "dashscope".into(),
                query::rerank::RerankProviderConfig {
                    api_key: String::new(),
                    endpoint,
                    request_schema: e.text("DASHSCOPE_RERANK_REQUEST_SCHEMA", "auto"),
                    instruct: e.optional("DASHSCOPE_RERANK_INSTRUCT"),
                },
            );
        }
        rerank_providers.insert(
            "cohere".into(),
            query::rerank::RerankProviderConfig {
                api_key: String::new(),
                endpoint: String::new(),
                request_schema: "flat".into(),
                instruct: None,
            },
        );
        let valkey = ai_platform_service_primitives::valkey::SplitConfig::from_values(
            &e.0,
            if RuntimeRole::parse(&e.text("KNOWLEDGE_RUNTIME_ROLE", "all"))?.api() {
                ai_platform_service_primitives::valkey::ServiceIdentity::KnowledgeApi
            } else {
                ai_platform_service_primitives::valkey::ServiceIdentity::KnowledgeWorker
            },
        )
        .map_err(|_| KnowledgeError::Configuration("valkey_split"))?;
        let redis_url = e
            .optional("INTERNAL_COMM_REDIS_URL")
            .or_else(|| e.optional("REDIS_URL"))
            .unwrap_or_default();
        if e.optional("KNOWLEDGE_DATABASE__DSN").is_some() {
            return Err(KnowledgeError::Configuration("inline_database_dsn_retired"));
        }
        let database = if e.0.keys().any(|key| key.starts_with("DATA_KNOWLEDGE_")) {
            use ai_platform_service_primitives::data_connection::{
                ConnectionConfig, ConnectionPurpose, DatabaseRole,
            };
            // Session advisory leases and backend cancellation must retain their
            // backend. Never route this mixed control/data pool through tx PgBouncer.
            let role = if RuntimeRole::parse(&e.text("KNOWLEDGE_RUNTIME_ROLE", "all"))?
                == RuntimeRole::Api
            {
                DatabaseRole::KnowledgeApi
            } else {
                DatabaseRole::KnowledgeWorker
            };
            let config = ConnectionConfig::from_environment(
                "DATA_KNOWLEDGE",
                role,
                ConnectionPurpose::BackendPinned,
                &e.0,
            )
            .map_err(|_| KnowledgeError::Configuration("knowledge_database_trust"))?;
            if [
                &config.dsn_file,
                &config.tls.ca,
                &config.tls.certificate,
                &config.tls.key,
            ]
            .iter()
            .any(|path| !path.is_absolute())
                || config
                    .password_file
                    .as_ref()
                    .is_some_and(|path| !path.is_absolute())
            {
                return Err(KnowledgeError::Configuration(
                    "knowledge_database_absolute_files",
                ));
            }
            Some(config)
        } else {
            None
        };
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
                embeddings.provider = "local_semantic".into();
                embeddings.model = query::embedding::LOCAL_SEMANTIC_MODEL.into();
                embeddings.dimension = query::embedding::LOCAL_SEMANTIC_DIMENSION;
                embeddings.local_semantic_enabled = true;
                embeddings.local_semantic_revision = e
                    .optional("KNOWLEDGE_NATIVE__LOCAL_SEMANTIC_REVISION")
                    .filter(|s| !s.trim().is_empty() && s.len() <= 100)
                    .ok_or(KnowledgeError::Configuration("local_semantic_revision"))?;
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
            dataset_policy: e
                .optional("KNOWLEDGE_CEDAR_DATASET_CONFIGURATION")
                .map(|raw| serde_json::from_str(&raw))
                .transpose()
                .map_err(|_| KnowledgeError::Configuration("dataset_policy_configuration"))?,
            model_plane: crate::model_plane_identity::ModelPlaneSettings {
                origin: e.text("KNOWLEDGE_MODEL_PLANE__URL", &authentication.gateway_origin),
                workload_key_file: e
                    .optional("KNOWLEDGE_MODEL_PLANE__WORKLOAD_KEY_FILE")
                    .map(PathBuf::from),
                gateway_public_keys_file: e
                    .optional("KNOWLEDGE_MODEL_PLANE__GATEWAY_PUBLIC_KEYS_FILE")
                    .map(PathBuf::from),
                model_revisions: serde_json::from_str(
                    &e.text("KNOWLEDGE_MODEL_PLANE__MODEL_REVISIONS", "{}"),
                )
                .map_err(|_| KnowledgeError::Configuration("knowledge_model_revisions"))?,
            },
            connector_encryption_key: e.text("KNOWLEDGE_CONNECTOR_ENCRYPTION_KEY", ""),
            request_governance:
                ai_platform_service_primitives::request_governance::RequestGovernanceConfig::new(
                    e.number("KNOWLEDGE_HTTP__MAX_CONCURRENT_REQUESTS", 1024)?,
                    http_deadline("KNOWLEDGE_HTTP__RESPONSE_HEADER_TIMEOUT_MS")?,
                    http_deadline("KNOWLEDGE_HTTP__RESPONSE_BODY_IDLE_TIMEOUT_MS")?,
                )
                .map_err(|_| KnowledgeError::Configuration("http_governance"))?,
            confluence_request_timeout: confluence_timeout,
            confluence_max_retries: confluence_retries,
            confluence_enabled: e.flag("KNOWLEDGE_CONFLUENCE__MANUAL_SYNC_ENABLED", true)?,
            confluence_acl_refresh: e.flag("KNOWLEDGE_CONFLUENCE__ACL_REFRESH_ENABLED", false)?,
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
        if rpc.address.is_some()
            && workload.as_ref().is_some_and(|config| {
                config.mode() != ai_platform_service_primitives::workload_identity::Mode::Disabled
            })
        {
            // The existing tonic adapter deliberately drops bearer metadata and
            // has no TLS-derived WorkloadConnection. Never synthesize its peer.
            return Err(KnowledgeError::Configuration("mtls_plain_rpc_forbidden"));
        }
        Ok(Self {
            broker_environment: e
                .0
                .iter()
                .filter(|(k, _)| {
                    k.starts_with("AI_PLATFORM_LEASE_")
                        || k.starts_with("AI_PLATFORM_BROKER_PROOF_")
                        || k.starts_with("AI_PLATFORM_WORKLOAD_")
                        || k.as_str() == "AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE"
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            release_identity,
            configuration,
            host: e.text("KNOWLEDGE_APP__HOST", "0.0.0.0"),
            port: e.number("KNOWLEDGE_APP__PORT", 8092)?,
            rpc,
            database,
            workload,
            redis_url,
            valkey,
            poppler,
            capability_gaps: gaps,
        })
    }
    pub async fn construct(self) -> Result<(KnowledgeGraph, String, u16, Vec<&'static str>)> {
        use crate::migration_content::capabilities::broker_proof_v3 as broker_proof;
        broker_proof::configure_environment(&self.broker_environment)
            .map_err(KnowledgeError::Configuration)?;
        if self
            .broker_environment
            .contains_key("AI_PLATFORM_GATEWAY_ED25519_KEYRING_FILE")
            || (broker_proof::verify_only_required()
                && !self
                    .configuration
                    .authentication
                    .capability_proof_secret
                    .is_empty())
        {
            return Err(KnowledgeError::Configuration(
                "consumer_broker_signing_secret_forbidden",
            ));
        }
        if broker_proof::verify_only_required() {
            broker_proof::keys().map_err(KnowledgeError::Configuration)?;
        }

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
        let database = self.database.as_ref().ok_or(KnowledgeError::Configuration(
            "data_knowledge_file_configuration_required",
        ))?;
        let workload = self.workload.as_ref().ok_or(KnowledgeError::Configuration(
            "workload_file_configuration_required",
        ))?;
        if workload.mode() != ai_platform_service_primitives::workload_identity::Mode::Disabled
            && self.rpc.address.is_some()
        {
            return Err(KnowledgeError::Configuration("mtls_plain_rpc_forbidden"));
        }
        let pool = database
            .connect()
            .await
            .map_err(|_| KnowledgeError::Startup("knowledge_database_role_mtls"))?;
        ai_platform_knowledge_domain::database::configure(
            database.role,
            database.statement_timeout_ms,
        )
        .map_err(|_| KnowledgeError::Configuration("knowledge_database_policy"))?;
        backends.workload_http = workload.outbound();
        let mut model_plane = (*self.configuration.model_plane.construct()?).clone();
        if let Some(token) = workload.legacy_secret() {
            backends.internal_transport_bearer = Some(token.to_owned());
            model_plane = model_plane
                .with_legacy_transport(token)
                .map_err(|_| KnowledgeError::Configuration("model_plane_legacy_transport"))?;
        }
        if let Some(http) = workload.outbound() {
            model_plane = model_plane
                .with_workload_http(http, workload.health_origin())
                .map_err(|_| KnowledgeError::Configuration("model_plane_workload_origin"))?;
        }
        let replay = matches!(
            self.configuration.authentication.replay_backend,
            ReplayBackend::Redis
        );
        let idem = matches!(
            self.configuration.authentication.idempotency_backend,
            ReplayBackend::Redis
        );
        let redis = if let Some(config) = &self.valkey {
            use ai_platform_service_primitives::valkey::{CacheAvailability, SplitConnections};
            let split = SplitConnections::connect(config)
                .await
                .map_err(|_| KnowledgeError::Startup("valkey_split"))?;
            match split.cache {
                CacheAvailability::Ready(cache) => backends.result_cache = Some(cache),
                CacheAvailability::Unavailable(error) => {
                    tracing::warn!(error=%error,"optional Knowledge result cache unavailable")
                }
            }
            Some(split.coord.coordination_manager())
        } else if replay || idem {
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
                model_plane: Some(Arc::new(model_plane)),
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
                graph.workload = self.workload;
                if let Some(app) = graph.application.as_mut() {
                    app.capabilities =
                        app.capabilities
                            .clone()
                            .with_transport_guard(Arc::new(|extensions| {
                                ai_platform_auth::workload::WorkloadAuthentication::require(
                                    extensions,
                                )
                                .is_ok()
                            }));
                }
                Ok((graph, self.host, self.port, self.capability_gaps))
            }
            Err(error) => {
                pool.close().await;
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod valkey_split_tests {
    use super::*;
    use ai_platform_service_primitives::valkey::ServiceIdentity;
    fn values(role: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ENVIRONMENT".into(), "test".into()),
            (
                "AI_PLATFORM_INTERNAL_TOKEN".into(),
                "offline-signing-material".into(),
            ),
            (
                "AI_PLATFORM_CAPABILITY_PROOF_SECRET".into(),
                "offline-proof-material".into(),
            ),
            ("INTERNAL_COMM_STATE_BACKEND".into(), "memory".into()),
            ("INTERNAL_IDEMPOTENCY_BACKEND".into(), "memory".into()),
            ("KNOWLEDGE_RUNTIME_ROLE".into(), role.into()),
        ])
    }
    fn split(role: &str) -> BTreeMap<String, String> {
        let mut values = values(role);
        let service = if role == "worker" {
            ServiceIdentity::KnowledgeWorker
        } else {
            ServiceIdentity::KnowledgeApi
        };
        values.insert("AI_PLATFORM_VALKEY_MODE".into(), "split".into());
        for (kind, port) in [("COORD", 6380), ("CACHE", 6381)] {
            for (suffix, value) in [
                (
                    "ENDPOINT",
                    format!("rediss://valkey-{}.internal:{port}/0", kind.to_lowercase()),
                ),
                (
                    "USERNAME",
                    format!("{}-{}", service.name(), kind.to_lowercase()),
                ),
                (
                    "PASSWORD_FILE",
                    format!("/nonexistent/{}.password", kind.to_lowercase()),
                ),
                ("CA_FILE", "/nonexistent/ca.pem".into()),
            ] {
                values.insert(format!("AI_PLATFORM_VALKEY_{kind}_{suffix}"), value);
            }
        }
        values
    }
    #[test]
    fn pure_split_parse_selects_api_or_worker_without_opening_files() {
        for role in ["api", "worker"] {
            let settings =
                ProcessSettings::from_values(split(role), PathBuf::from("/tmp")).unwrap();
            let config = settings.valkey.unwrap();
            let service = if role == "api" {
                ServiceIdentity::KnowledgeApi
            } else {
                ServiceIdentity::KnowledgeWorker
            };
            assert_eq!(config.coord().service(), service);
            assert_eq!(config.cache().service(), service);
        }
    }
    #[test]
    fn partial_split_never_falls_back_to_legacy_redis() {
        let mut partial = values("api");
        partial.insert("REDIS_URL".into(), "redis://localhost".into());
        partial.insert("AI_PLATFORM_VALKEY_MODE".into(), "split".into());
        assert!(ProcessSettings::from_values(partial, PathBuf::from("/tmp")).is_err());
        let mut wrong = split("api");
        wrong.insert(
            "AI_PLATFORM_VALKEY_CACHE_USERNAME".into(),
            "knowledge-worker-cache".into(),
        );
        assert!(ProcessSettings::from_values(wrong, PathBuf::from("/tmp")).is_err());
    }
}

#[cfg(test)]
mod trust_tests {
    use super::*;
    use ai_platform_service_primitives::data_connection::{
        ConnectionMode, ConnectionPurpose, DatabaseRole,
    };
    fn values(role: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ENVIRONMENT".into(), "test".into()),
            ("KNOWLEDGE_RUNTIME_ROLE".into(), role.into()),
            (
                "DATA_KNOWLEDGE_DSN_FILE".into(),
                "/secrets/knowledge.dsn".into(),
            ),
            (
                "DATA_KNOWLEDGE_PASSWORD_FILE".into(),
                "/secrets/knowledge.password".into(),
            ),
            (
                "DATA_KNOWLEDGE_TLS_CA_FILE".into(),
                "/secrets/postgres-ca.pem".into(),
            ),
            (
                "DATA_KNOWLEDGE_TLS_CERT_FILE".into(),
                "/secrets/knowledge-cert.pem".into(),
            ),
            (
                "DATA_KNOWLEDGE_TLS_KEY_FILE".into(),
                "/secrets/knowledge-key.pem".into(),
            ),
            ("DATA_KNOWLEDGE_MODE".into(), "direct".into()),
            (
                "DATA_KNOWLEDGE_EXPECTED_HOST".into(),
                "postgres-internal".into(),
            ),
            ("DATA_KNOWLEDGE_DATABASE_ALIAS".into(), "gateway".into()),
            ("DATA_KNOWLEDGE_PHYSICAL_DATABASE".into(), "gateway".into()),
            ("DATA_KNOWLEDGE_POOL_MAX".into(), "4".into()),
        ])
    }
    #[test]
    fn executable_configuration_uses_role_checked_file_database() {
        for role in ["api", "worker", "parse", "embed", "index", "all"] {
            let settings = ProcessSettings::from_values(values(role), "/tmp".into()).unwrap();
            let config = settings.database.unwrap();
            assert_eq!(
                config.role,
                if role == "api" {
                    DatabaseRole::KnowledgeApi
                } else {
                    DatabaseRole::KnowledgeWorker
                }
            );
            assert_eq!(config.purpose, ConnectionPurpose::BackendPinned);
            assert_eq!(config.mode, ConnectionMode::Direct);
            assert!(!format!("{config:?}").contains("knowledge.password"));
        }
    }
    #[test]
    fn transaction_pool_cannot_back_session_advisory_leases() {
        let mut values = values("api");
        values.insert("DATA_KNOWLEDGE_MODE".into(), "transaction".into());
        values.insert(
            "DATA_KNOWLEDGE_DATABASE_ALIAS".into(),
            "knowledge_api_tx".into(),
        );
        assert!(ProcessSettings::from_values(values, "/tmp".into()).is_err());
    }
    #[test]
    fn inline_dsn_password_and_relative_file_migration_are_rejected() {
        for (key, value) in [
            ("KNOWLEDGE_DATABASE__DSN", "postgres://old/inline"),
            ("DATA_KNOWLEDGE_DSN", "postgres://old/inline"),
            ("DATA_KNOWLEDGE_PASSWORD", "old-inline-password"),
            ("DATA_KNOWLEDGE_PASSWORD_FILE", "relative-secret"),
        ] {
            let mut env = values("api");
            env.insert(key.into(), value.into());
            assert!(
                ProcessSettings::from_values(env, "/tmp".into()).is_err(),
                "{key}"
            );
        }
    }
    #[test]
    fn session_alias_is_tied_to_selected_database_role() {
        let mut env = values("api");
        env.insert("DATA_KNOWLEDGE_MODE".into(), "session".into());
        env.insert(
            "DATA_KNOWLEDGE_DATABASE_ALIAS".into(),
            "knowledge_api_session".into(),
        );
        assert!(ProcessSettings::from_values(env.clone(), "/tmp".into()).is_ok());
        env.insert("KNOWLEDGE_RUNTIME_ROLE".into(), "worker".into());
        assert!(ProcessSettings::from_values(env, "/tmp".into()).is_err());
    }
}

#[cfg(all(test, unix))]
mod storage_signing_file_tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};
    #[test]
    fn selected_private_key_file_rejects_conflicts_and_never_falls_back() {
        let path = std::env::temp_dir().join(format!("knowledge-signing-{}", uuid::Uuid::new_v4()));
        fs::write(&path, "purpose-specific-signing-fixture\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let name = "KNOWLEDGE_STORAGE__SIGNING_KEY";
        let mut values = Values(BTreeMap::from([(
            format!("{name}_FILE"),
            path.to_string_lossy().into_owned(),
        )]));
        assert_eq!(
            values.private_file_or_legacy(name).unwrap(),
            "purpose-specific-signing-fixture"
        );
        values.0.insert(
            "KNOWLEDGE_STORAGE".into(),
            r#"{"signing_key":"nested-inline-conflict"}"#.into(),
        );
        assert!(values.private_file_or_legacy(name).is_err());
        values.0.remove("KNOWLEDGE_STORAGE");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(values.private_file_or_legacy(name).is_err());
        fs::remove_file(&path).unwrap();
        assert!(values.private_file_or_legacy(name).is_err());
    }
}

// Required workload transport keeps domain HMAC roots explicit and never uses a
// bearer or an empty key as the signing-keyring fallback.
fn domain_signing_keys(
    raw: &str,
    required: bool,
) -> Result<(BTreeMap<String, String>, Option<String>)> {
    let mut keys = BTreeMap::new();
    let mut first = None;
    for item in raw.split(',').filter(|item| !item.trim().is_empty()) {
        let Some((id, key)) = item.trim().split_once(':') else {
            if required {
                return Err(KnowledgeError::Configuration("signing_key"));
            }
            continue;
        };
        let (id, key) = (id.trim(), key.trim());
        if id.is_empty() || key.is_empty() {
            if required {
                return Err(KnowledgeError::Configuration("signing_key"));
            }
            continue;
        }
        if required && (keys.contains_key(id) || key.chars().count() < 16) {
            return Err(KnowledgeError::Configuration("signing_key"));
        }
        first.get_or_insert_with(|| id.to_owned());
        keys.insert(id.to_owned(), key.to_owned());
    }
    if required && keys.is_empty() {
        return Err(KnowledgeError::Configuration("signing_key"));
    }
    Ok((keys, first))
}
#[cfg(test)]
mod required_domain_keys_tests {
    use super::*;
    #[test]
    fn required_domain_signing_keys_are_explicit_rotatable_and_not_a_bearer_fallback() {
        let (keys, first) = domain_signing_keys(
            "new:independent-domain-key-one,old:independent-domain-key-two",
            true,
        )
        .unwrap();
        assert_eq!(first.as_deref(), Some("new"));
        assert_eq!(keys.len(), 2);
        for input in [
            "",
            "malformed",
            "a:short",
            "a:independent-domain-key,malformed",
            "a:independent-domain-key,a:duplicate-domain-key",
        ] {
            assert!(domain_signing_keys(input, true).is_err());
        }
        // The disabled/dual parser's existing fallback belongs to its owner.
        assert!(
            domain_signing_keys("malformed", false)
                .unwrap()
                .0
                .is_empty()
        );
    }
}
