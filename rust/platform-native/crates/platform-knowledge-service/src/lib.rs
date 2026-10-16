//! Separate Knowledge-service construction from its own settings and connections.
//! Gateway identity is verified over signed HTTP. No Gateway AuthService,
//! in-process actor lookup, or replacement Agent loop is constructed here.
//! Constructors do not open connections, start workers, or mark the service ready.
//! PDF engines execute natively; optional lower-layer protocol/model and telemetry
//! boundaries are tracked in the migration report.
pub mod application;
pub mod confluence;
mod http;
mod metrics;
pub mod upload;
pub use ai_platform_gateway::migration_host as host;
pub use ai_platform_gateway::{
    migration_content, migration_knowledge_ingest, migration_knowledge_query,
};
use application::KnowledgeApplication;
pub mod langgraph_tools;
pub mod lifecycle;
pub mod native_contract;
pub mod native_cross_encoder;
pub mod native_rerank;
pub mod ocr_utils;
pub mod pdf_backend;
pub mod parsing_eval;
mod pdf_readiness;
pub mod process;
pub mod rpc;
pub mod settings;
pub mod tracing;
use crate::host::{cors::CorsConfig, idempotency, middleware::DrainState};
use crate::{migration_knowledge_ingest as ingest, migration_knowledge_query as query};
use redis::aio::ConnectionManager;
use settings::{KnowledgeConfiguration, ReplayBackend};
use sqlx::PgPool;
use std::{collections::BTreeSet, sync::Arc, time::Duration};

#[derive(Debug, Clone, Copy)]
pub enum KnowledgeError {
    Configuration(&'static str),
    Construction(&'static str),
    Startup(&'static str),
    Shutdown(&'static str),
}
impl std::fmt::Display for KnowledgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "knowledge_bootstrap: {self:?}")
    }
}
impl std::error::Error for KnowledgeError {}
pub type Result<T> = std::result::Result<T, KnowledgeError>;
pub(crate) fn endpoint(value: &str, name: &'static str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value).map_err(|_| KnowledgeError::Configuration(name))?;
    if !["http", "https"].contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(KnowledgeError::Configuration(name));
    }
    Ok(url)
}
/// Connections must belong to this Knowledge process. stop() closes its DB pool;
/// Redis handles are shared only within this process and are released on drop.
pub struct KnowledgeConnections {
    pub database: PgPool,
    pub replay_redis: Option<ConnectionManager>,
    pub idempotency_redis: Option<ConnectionManager>,
}
#[derive(Default)]
pub struct PdfBackends {
    pub split: Option<Arc<dyn ingest::pdf_splitter::PdfSplitBackend>>,
    pub text: Option<Arc<dyn ingest::parser::PdfTextBackend>>,
    pub raster: Option<Arc<dyn ingest::vision::PdfRasterBackend>>,
    pub streaming: Option<Arc<dyn ingest::streaming::StreamingPdfBackend>>,
    pub images: Option<Arc<dyn ingest::images::PdfImageBackend>>,
    /// Whether the selected streaming backend supports embedded xref extraction.
    pub streaming_images: bool,
}
impl PdfBackends {
    /// Native Poppler operations; engine-specific output is fixture-tested.
    pub fn poppler(backend: Arc<crate::pdf_backend::PopplerPdfBackend>) -> Self {
        let available = backend.availability();
        Self {
            split: available
                .pdf_page_serialization
                .then(|| backend.clone() as Arc<dyn ingest::pdf_splitter::PdfSplitBackend>),
            text: available
                .text_alternative
                .then(|| backend.clone() as Arc<dyn ingest::parser::PdfTextBackend>),
            raster: available
                .rasterization
                .then(|| backend.clone() as Arc<dyn ingest::vision::PdfRasterBackend>),
            streaming: available
                .text_alternative
                .then(|| backend.clone() as Arc<dyn ingest::streaming::StreamingPdfBackend>),
            images: available
                .text_alternative
                .then(|| backend as Arc<dyn ingest::images::PdfImageBackend>),
            streaming_images: available.embedded_image_extraction,
        }
    }
}
/// Decoder/installed-model capabilities absent from the accepted native libraries
/// remain optional real inputs. None leaves that feature explicitly unavailable.
#[derive(Default)]
pub struct KnowledgeBackends {
    pub pdf: PdfBackends,
    pub image_fetch: Option<Arc<dyn crate::migration_content::images::ImageReferenceFetch>>,
    pub structured: ingest::structured::BackendRegistry,
}
pub struct KnowledgeGraph {
    pub request_governance: ai_platform_gateway::request_governance::RequestGovernance,
    pub release_identity: Arc<crate::host::release::ReleaseIdentity>,
    pub application: Option<KnowledgeApplication>,
    pub ingestion: Arc<ingest::IngestionService>,
    pub retrieval: Arc<query::service::RetrievalService>,
    pub indexer: Arc<query::index::Indexer>,
    pub embeddings: query::embedding::EmbeddingManager,
    pub lifecycle: Arc<lifecycle::KnowledgeLifecycle>,
    pub service_auth: Arc<query::auth::GatewayAuth>,
    pub idempotency: Arc<idempotency::IdempotencyConfig>,
    pub cors: Arc<CorsConfig>,
    pub maximum_upload_bytes: usize,
    /// Optional startup facilities that could not be constructed. These are
    /// capability gaps, not evidence that core services are healthy.
    pub degraded_components: Vec<&'static str>,
}
impl KnowledgeGraph {
    pub fn new(
        mut connections: KnowledgeConnections,
        config: KnowledgeConfiguration,
        mut backends: KnowledgeBackends,
    ) -> Result<Self> {
        if !config.working_directory.is_absolute() {
            return Err(KnowledgeError::Configuration("working_directory"));
        }
        if config.authentication.allow_anonymous {
            return Err(KnowledgeError::Configuration(
                "signed_service_profile_requires_authentication",
            ));
        }
        if config.authentication.version.trim().to_lowercase() != "v2" {
            return Err(KnowledgeError::Configuration("internal_auth_version"));
        }
        let local = ["local", "dev", "development", "test", "testing"].contains(
            &config
                .authentication
                .environment
                .trim()
                .to_lowercase()
                .as_str(),
        );
        let replay = match config.authentication.replay_backend {
            ReplayBackend::Memory if local => query::auth::ReplayStore::memory(),
            ReplayBackend::Memory => {
                return Err(KnowledgeError::Configuration("redis_replay_required"));
            }
            ReplayBackend::Redis => query::auth::ReplayStore::Redis(
                connections
                    .replay_redis
                    .clone()
                    .ok_or(KnowledgeError::Configuration("replay_redis"))?,
            ),
        };
        let service_auth = Arc::new(
            query::auth::GatewayAuth::for_knowledge_service(
                config.authentication.internal_token.trim().into(),
                config.authentication.keyring.clone(),
                &config.authentication.environment,
                replay,
            )
            .map_err(|_| KnowledgeError::Configuration("gateway_signature"))?,
        );
        let idem_store: Arc<dyn idempotency::IdempotencyStore> =
            match config.authentication.idempotency_backend {
                ReplayBackend::Memory if local => {
                    Arc::new(idempotency::InMemoryIdempotencyStore::default())
                }
                ReplayBackend::Memory => {
                    return Err(KnowledgeError::Configuration("redis_idempotency_required"));
                }
                ReplayBackend::Redis => Arc::new(idempotency::RedisIdempotencyStore::new(
                    connections
                        .idempotency_redis
                        .clone()
                        .ok_or(KnowledgeError::Configuration("idempotency_redis"))?,
                    "ai-gateway:internal:idem",
                )),
            };
        let idempotency = Arc::new(idempotency::IdempotencyConfig {
            store: idem_store,
            ttl_seconds: config.authentication.idempotency_ttl_seconds,
            ..Default::default()
        });
        if matches!(config.authentication.replay_backend, ReplayBackend::Memory) {
            connections.replay_redis = None;
        }
        if matches!(
            config.authentication.idempotency_backend,
            ReplayBackend::Memory
        ) {
            connections.idempotency_redis = None;
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| KnowledgeError::Construction("http_client"))?;
        let qdrant_origin = endpoint(&config.qdrant.url, "qdrant_origin")?;
        let mut vectors = query::qdrant::QdrantStore::new(
            client.clone(),
            qdrant_origin.clone(),
            config.qdrant.api_key.clone(),
        )
        .map_err(|_| KnowledgeError::Construction("qdrant"))?;
        vectors.max_retries = config.qdrant.max_retries;
        vectors.retry_base_delay =
            settings::seconds(config.qdrant.retry_base_delay, "qdrant_retry_base_delay")?;
        vectors.timeout = settings::seconds(config.qdrant.timeout_seconds, "qdrant_timeout")?;
        vectors.set_prefer_grpc(config.qdrant.prefer_grpc)
            .map_err(|_| KnowledgeError::Construction("qdrant_grpc"))?;
        vectors.interactive_deadline = settings::seconds(
            config.qdrant.interactive_deadline_seconds,
            "interactive_deadline",
        )?;
        vectors.bm25_v2_enabled = config.qdrant.bm25_v2_enabled;
        vectors.bm25_v2_capability_ttl = settings::seconds(
            config.qdrant.bm25_v2_capability_ttl_seconds,
            "bm25_capability_ttl",
        )?;
        let providers = config.embeddings.providers(&config.provider_keys)?;
        let mut embeddings = query::embedding::EmbeddingManager::new(
            client.clone(),
            Some(connections.database.clone()),
            config.embeddings.encryption_key.clone(),
            providers.clone(),
        );
        embeddings.document_batch_size = config.embeddings.batch_size.max(1);
        if let Some(endpoint) = &config.embeddings.local_semantic_endpoint {
            if config.embeddings.provider != "local_semantic"
                || config.embeddings.model != query::embedding::LOCAL_SEMANTIC_MODEL
                || config.embeddings.dimension != query::embedding::LOCAL_SEMANTIC_DIMENSION
                || !config.embeddings.api_key.is_empty()
            {
                return Err(KnowledgeError::Configuration("local_semantic_profile"));
            }
            embeddings = embeddings
                .with_local_semantic(
                    endpoint,
                    settings::seconds(config.embeddings.timeout_seconds, "local_semantic_timeout")?,
                )
                .map_err(|_| KnowledgeError::Construction("local_semantic"))?;
        } else if config.embeddings.provider == "local_semantic" {
            return Err(KnowledgeError::Configuration("local_semantic_endpoint"));
        }
        let store = query::store::QueryStore::new(connections.database.clone());
        let mut reranker = query::rerank::Reranker::new(client.clone(), config.rerank_providers);
        reranker.dashscope_base_url = config.dashscope_rerank_region;
        if let Some(endpoint_url) = config.local_rerank_endpoint {
            let local = query::rerank::HttpLocalRerankBackend::new(
                client.clone(),
                endpoint(&endpoint_url, "local_rerank_endpoint")?,
                config.local_rerank_api_key,
            )
            .map_err(|_| KnowledgeError::Construction("local_rerank"))?;
            reranker = reranker.with_local_backend(Arc::new(local));
        } else {
            let local = native_rerank::NativeLocalReranker {
                cross_encoder: native_cross_encoder::NativeCrossEncoder::new(
                    config.native_cross_encoder_directory,
                )
                .map_err(|_| KnowledgeError::Construction("native_cross_encoder"))?,
                m3: native_rerank::NativeBgeReranker::new(config.native_bge_directory)
                    .map_err(|_| KnowledgeError::Construction("native_bge_rerank"))?,
                large: native_rerank::NativeBgeReranker::large(config.native_bge_large_directory)
                    .map_err(|_| KnowledgeError::Construction("native_bge_large_rerank"))?,
            };
            reranker = reranker.with_local_backend(Arc::new(local));
        }
        let mut retrieval = query::service::RetrievalService::new(
            store.clone(),
            vectors.clone(),
            embeddings.clone(),
            Some(reranker),
        );
        retrieval.cache_ttl = settings::seconds(
            config.processing.retrieval_cache_ttl_seconds,
            "retrieval_cache_ttl",
        )?;
        retrieval.interactive_deadline = settings::seconds(
            config.qdrant.interactive_deadline_seconds,
            "interactive_deadline",
        )?;
        retrieval.max_query_concurrency = config.processing.retrieval_query_max_concurrency;
        let retrieval = Arc::new(retrieval);
        let indexer = Arc::new(query::index::Indexer::new(
            store.clone(),
            vectors.clone(),
            embeddings.clone(),
        ));
        let assets = config.storage.project(&config.working_directory)?;
        let mut parser = ingest::parser::DocumentParser::new(config.parser);
        parser.pdf_backend = backends.pdf.text;
        let mut ingestion = ingest::IngestionService::new(
            ingest::store::KnowledgeWriteStore::new(connections.database.clone()),
            assets.clone(),
            parser.clone(),
            indexer.clone(),
            config.processing.document_worker_concurrency,
        )
        .map_err(|_| KnowledgeError::Construction("ingestion"))?;
        if config.embeddings.local_semantic_endpoint.is_some() {
            ingestion.dataset_defaults = Some(ingest::service::DatasetDefaults {
                embedding_provider: "local_semantic".into(),
                embedding_model: query::embedding::LOCAL_SEMANTIC_MODEL.into(),
                embedding_dimension: query::embedding::LOCAL_SEMANTIC_DIMENSION,
                rerank: false,
            });
        }
        let mut degraded_components = vec![];
        let ocr = match config
            .ocr
            .project(&config.embeddings, &config.provider_keys)
        {
            Ok(Some(ocr)) => match ingest::ocr::VlmOcrService::new(ocr) {
                Ok(service) => Some(service),
                Err(_) => {
                    degraded_components.push("vlm_ocr");
                    None
                }
            },
            Ok(None) => {
                if config.ocr.enabled && ["vlm", "hybrid"].contains(&config.ocr.strategy.as_str()) {
                    degraded_components.push("vlm_ocr_credentials");
                }
                None
            }
            Err(_) => {
                degraded_components.push("vlm_ocr");
                None
            }
        };
        if let Some(service) = &ocr {
            service.register(&mut backends.structured);
        }
        ingestion.structured_backends = backends.structured;
        ingestion.streaming_ocr_enabled = config.ocr.enabled;
        ingestion.streaming_ocr_strategy = config.ocr.strategy.clone();
        ingestion.streaming_ocr = ocr.clone();
        ingestion.large_file_threshold = config.processing.large_file_threshold;
        ingestion.maximum_batch_upload_bytes = config.processing.maximum_batch_upload_bytes;
        ingestion.image_extractor.pdf = backends.pdf.images;
        ingestion.image_extractor.safe_fetch = backends.image_fetch;
        if let Some(backend) = backends.pdf.streaming {
            let mut streaming = ingest::streaming::StreamingDocumentLoader::new(backend);
            streaming.batch_size = config.processing.streaming_batch_size;
            streaming.extract_images = backends.pdf.streaming_images;
            streaming.extract_images_if_no_text = backends.pdf.streaming_images;
            streaming.storage = Some(assets);
            ingestion.streaming = Some(streaming);
        }
        if let Some(backend) = backends.pdf.raster {
            let mut vision =
                ingest::vision::VisionProcessor::new(backend, embeddings.clone(), parser);
            vision.vlm_ocr = ocr;
            vision.ocr_strategy = config.ocr.strategy;
            ingestion.vision = Some(vision);
        }
        if !ingestion.parser.supports_pdf() {
            degraded_components.push("pdf_text_backend_unconfigured");
        }
        if let Some(summary) = config.summary {
            ingestion.summary = Some(
                ingest::summary::SummaryGenerator::new(
                    summary.endpoint,
                    summary.model,
                    summary.authorization,
                )
                .map_err(|_| KnowledgeError::Construction("summary"))?,
            );
        }
        ingestion.confluence_sources_enabled = config.confluence_enabled && config.role.api();
        let ingestion = Arc::new(ingestion);
        let migrations = Arc::new(query::migration::EmbeddingMigration::new(
            connections.database.clone(),
            vectors.clone(),
            embeddings.clone(),
        ));
        let lifecycle = Arc::new(lifecycle::KnowledgeLifecycle::new(
            connections,
            config.role,
            client,
            qdrant_origin,
            config.qdrant.api_key,
            vectors.timeout,
            ingestion.clone(),
            config.processing.worker,
            migrations.clone(),
            Arc::new(DrainState::default()),
            config
                .embeddings
                .local_semantic_endpoint
                .as_ref()
                .map(|_| embeddings.clone()),
        ));
        let application = if config.role.api() {
            let qa = config
                .qa
                .map(|s| -> Result<_> {
                    let mut service = query::qa::QaService::new(
                        retrieval.clone(),
                        lifecycle.client.clone(),
                        s.project(&config.provider_keys)?,
                    );
                    service.batch_timeout =
                        settings::seconds(s.batch_timeout_seconds, "qa_batch_timeout")?;
                    Ok(Arc::new(service))
                })
                .transpose()?;
            let mut judge_embeddings = query::embedding::EmbeddingManager::new(
                lifecycle.client.clone(),
                None,
                String::new(),
                std::collections::BTreeMap::from([(
                    config.embeddings.provider.clone(),
                    config.embeddings.judge_provider()?,
                )]),
            );
            if let Some(endpoint) = &config.embeddings.local_semantic_endpoint {
                judge_embeddings = judge_embeddings
                    .with_local_semantic(
                        endpoint,
                        settings::seconds(
                            config.embeddings.timeout_seconds,
                            "local_semantic_timeout",
                        )?,
                    )
                    .map_err(|_| KnowledgeError::Construction("local_semantic_judge_embedding"))?;
            }
            let judge_providers = ["dashscope", "gemini", "deepseek"]
                .into_iter()
                .map(|provider| {
                    (
                        provider.to_owned(),
                        query::embedding::ProviderConfig {
                            api_key: config.provider_keys.qa_key(provider),
                            base_url: None,
                            timeout: config.ragas.timeout,
                        },
                    )
                })
                .collect();
            let ragas = Arc::new(
                query::ragas::RagasService::new(
                    lifecycle.client.clone(),
                    config.ragas,
                    judge_providers,
                    Some(query::ragas::RagasEmbedding {
                        manager: judge_embeddings,
                        provider: config.embeddings.provider,
                        model: config.embeddings.model,
                        dimension: Some(config.embeddings.dimension),
                    }),
                )
                .map_err(|_| KnowledgeError::Construction("ragas"))?,
            );
            let mut actor_signer = query::auth::GatewayAuth::new(
                config.authentication.internal_token.trim().into(),
                match &config.authentication.outbound_key_id {
                    Some(key) => std::collections::BTreeMap::from([(
                        key.clone(),
                        config
                            .authentication
                            .keyring
                            .get(key)
                            .cloned()
                            .ok_or(KnowledgeError::Configuration("active_signing_key"))?,
                    )]),
                    None => config.authentication.keyring,
                },
                "v2".into(),
                service_auth.replay.clone(),
            )
            .map_err(|_| KnowledgeError::Configuration("actor_signer"))?;
            actor_signer.caller_service = "knowledge-service".into();
            actor_signer.audience = "gateway".into();
            actor_signer.allowed_path_prefixes = vec![query::actor::ACTOR_PATH.into()];
            let actors = query::actor::RuntimeActorClient::new(
                endpoint(&config.authentication.gateway_origin, "gateway_origin")?,
                Arc::new(actor_signer),
            )
            .map_err(|_| KnowledgeError::Construction("runtime_actor_client"))?;
            let sources = Arc::new(
                confluence::Sources::new(
                    lifecycle.pool.clone(),
                    actors.clone(),
                    config.embeddings.encryption_key.clone(),
                    ingestion.clone(),
                    config.confluence_enabled,
                    settings::seconds(config.confluence_request_timeout, "confluence_timeout")?,
                    config.confluence_max_retries,
                )
                .map_err(|_| KnowledgeError::Construction("confluence_sources"))?,
            );
            *lifecycle
                .confluence
                .write()
                .map_err(|_| KnowledgeError::Construction("confluence_lifecycle"))? =
                Some(sources.clone());
            Some(KnowledgeApplication {
                upload: crate::upload::UploadState {
                    service: ingestion.clone(),
                    backend: backends.pdf.split,
                    maximum_upload_bytes: config.processing.maximum_upload_bytes,
                    config: config.processing.pdf_upload.clone(),
                },
                sources,
                service_auth: service_auth.clone(),
                retrieval: retrieval.clone(),
                qa,
                writes: ingest::http::WriteApiState {
                    service: ingestion.clone(),
                    batches: ingest::batches::DocumentBatchStore::new(lifecycle.pool.clone()),
                    maximum_upload_bytes: config.processing.maximum_upload_bytes,
                },
                lexical: query::lexical_http::LexicalApiState {
                    service: Arc::new(query::lexical_lifecycle::LexicalLifecycle::new(
                        lifecycle.pool.clone(),
                        vectors,
                    )),
                    cutover_test_tenants: config
                        .qdrant
                        .bm25_v2_cutover_test_tenants
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect::<BTreeSet<_>>(),
                },
                embedding_migrations: migrations,
                source_store: store,
                capabilities: query::actor::CapabilityState::new(
                    retrieval.clone(),
                    actors,
                    config.authentication.internal_token,
                    config.authentication.capability_proof_secret,
                ),
                ragas,
            })
        } else {
            None
        };
        let cors = Arc::new(
            CorsConfig {
                allow_credentials: !config.cors_origins.iter().any(|s| s == "*"),
                allow_origins: config.cors_origins,
                ..Default::default()
            }
            .normalized(),
        );
        let mut release_identity =
            crate::host::release::ReleaseIdentity::gateway(&Default::default());
        release_identity.service_id = "knowledge-service".into();
        Ok(Self {
            request_governance: ai_platform_gateway::request_governance::RequestGovernance::new(
                config.request_governance,
            ),
            release_identity: Arc::new(release_identity),
            application,
            ingestion,
            retrieval,
            indexer,
            embeddings,
            lifecycle,
            service_auth,
            idempotency,
            cors,
            maximum_upload_bytes: config.processing.maximum_upload_bytes,
            degraded_components,
        })
    }
    pub fn into_router(self) -> Result<axum::Router> {
        http::router(self)
    }
}
