//! Explicit inert Knowledge service construction.
use super::*;
impl IngestionService {
    pub fn new(
        store: KnowledgeWriteStore,
        assets: AssetStorage,
        parser: DocumentParser,
        indexer: Arc<Indexer>,
        cpu_workers: usize,
    ) -> Result<Self> {
        Ok(Self {
            store,
            vector_job_actor_authority: None,
            vector_job_expected: None,
            model_plane: None,
            assets,
            parser,
            chunker: Chunker::new()?,
            indexer,
            dataset_defaults: None,
            summary: None,
            vision: None,
            streaming: None,
            large_file_threshold: 50 * 1024 * 1024,
            reextraction_min_chars: 200,
            multimodal_reextraction_enabled: false,
            maximum_batch_upload_bytes: 32 * 1024 * 1024,
            confluence_sources_enabled: false,
            worker_observer: Arc::new(std::sync::RwLock::new(
                crate::migration_knowledge_ingest::worker::WorkerObserver::default(),
            )),
            streaming_ocr_enabled: true,
            streaming_ocr: None,
            streaming_ocr_strategy: "hybrid".into(),
            image_extractor:
                crate::migration_knowledge_ingest::images::DocumentImageExtractor::default(),
            structured_backends:
                crate::migration_knowledge_ingest::structured::BackendRegistry::default(),
            cpu: Arc::new(Semaphore::new(cpu_workers.max(1))),
        })
    }
}
