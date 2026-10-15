//! Concrete dataset/document admission and standard text ingestion orchestration.
use super::{
    Actor, Chunk, Dataset, IngestError, ProcessingMode, Result, canonical_json,
    dataset_ingestion_identity, display_status, integer, object, require_index_writable,
    require_public_metadata, sha256, string,
};
use super::{
    assets::AssetStorage,
    chunking::{Chunker, ChunkingConfig},
    parser::{DocumentParser, require_text_budget, sanitize_text},
    store::{DocumentLease, KnowledgeWriteStore},
};
use crate::migration_knowledge_query::{Indexer, QueryError};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio::sync::Semaphore;
impl From<QueryError> for IngestError {
    fn from(e: QueryError) -> Self {
        match e {
            QueryError::Forbidden => Self::Permission,
            QueryError::NotFound => Self::NotFound,
            QueryError::GenerationChanged => Self::GenerationChanged,
            QueryError::Validation(s) => Self::Validation(s),
            _ => Self::Index(e.to_string()),
        }
    }
}
/// Explicit process-local defaults for newly created datasets. Existing rows and
/// partially specified request tuples always retain their canonical behavior.
#[derive(Clone, Debug)]
pub struct DatasetDefaults {
    pub embedding_provider: String,
    pub embedding_model: String,
    pub embedding_dimension: usize,
    pub rerank: bool,
}
#[derive(Clone)]
pub struct IngestionService {
    pub store: KnowledgeWriteStore,
    pub assets: AssetStorage,
    pub parser: DocumentParser,
    pub chunker: Chunker,
    pub indexer: Arc<Indexer>,
    pub dataset_defaults: Option<DatasetDefaults>,
    pub summary: Option<super::summary::SummaryGenerator>,
    pub vision: Option<super::vision::VisionProcessor>,
    pub streaming: Option<super::streaming::StreamingDocumentLoader>,
    pub large_file_threshold: usize,
    /// Canonical knowledge.pdf_min_text_chars_for_ocr fallback when absent.
    pub reextraction_min_chars: usize,
    /// Presence of the optional process-local multimodal embedding client.
    /// The canonical independent Knowledge bootstrap leaves it unmounted.
    pub multimodal_reextraction_enabled: bool,
    pub maximum_batch_upload_bytes: usize,
    /// Mirrors the optional Knowledge-process Confluence sync-service mount.
    pub confluence_sources_enabled: bool,
    pub(crate) worker_observer: Arc<std::sync::RwLock<super::worker::WorkerObserver>>,
    pub streaming_ocr_enabled: bool,
    pub streaming_ocr: Option<super::ocr::VlmOcrService>,
    pub streaming_ocr_strategy: String,
    pub image_extractor: super::images::DocumentImageExtractor,
    pub structured_backends: super::structured::BackendRegistry,
    cpu: Arc<Semaphore>,
}
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
                super::worker::WorkerObserver::default(),
            )),
            streaming_ocr_enabled: true,
            streaming_ocr: None,
            streaming_ocr_strategy: "hybrid".into(),
            image_extractor: super::images::DocumentImageExtractor::default(),
            structured_backends: super::structured::BackendRegistry::default(),
            cpu: Arc::new(Semaphore::new(cpu_workers.max(1))),
        })
    }
    pub async fn create_dataset(&self, actor: &Actor, input: &Value) -> Result<Value> {
        if actor.anonymous() {
            return Err(IngestError::Permission);
        }
        let embedding = object(&input["embedding_config"]);
        let defaults = self.dataset_defaults.as_ref().filter(|_| {
            [
                "embedding_provider",
                "embedding_model",
                "embedding_dimension",
            ]
            .iter()
            .all(|key| input.get(*key).is_none())
        });
        let mut index = object(&input["index_config"]);
        if input.get("index_config").is_none() {
            if let Some(defaults) = defaults {
                index["retrieval"] = json!({"rerank": {"enabled": defaults.rerank}});
            }
        }
        if contains_server_config(&embedding) {
            return Err(IngestError::Validation(
                "embedding_config credentials and endpoints are server-owned".into(),
            ));
        }
        if contains_server_config(&index["retrieval"]["rerank"]) {
            return Err(IngestError::Validation(
                "rerank credentials, headers, and endpoints are server-owned".into(),
            ));
        }
        ChunkingConfig::from_value(&index["chunking"])?;
        super::structured::resolve_config(&index)?;
        let provider = if string(input, "embedding_provider").is_empty() {
            defaults.map_or("dashscope", |d| d.embedding_provider.as_str())
        } else {
            string(input, "embedding_provider")
        };
        if [
            "unified_multimodal",
            "unified",
            "cross_modal",
            "dashscope_multimodal",
            "aliyun_multimodal",
            "multimodal",
        ]
        .contains(&provider)
        {
            return Err(IngestError::Validation(
                "multimodal dataset profiles are disabled".into(),
            ));
        }
        let dimension = if input.get("embedding_dimension").is_some() {
            input["embedding_dimension"]
                .as_u64()
                .filter(|d| *d > 0 && *d <= 8192)
                .ok_or_else(|| {
                    IngestError::Validation(
                        "embedding_dimension must be an integer between 1 and 8192".into(),
                    )
                })? as usize
        } else {
            defaults.map_or(1024, |d| d.embedding_dimension)
        };
        let requested = string(input, "dataset_id").trim();
        let id = if requested.is_empty() {
            format!("kb_{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
        } else {
            requested.into()
        };
        if self.store.dataset_exists(&id).await? {
            return Err(IngestError::Validation("dataset_id already exists".into()));
        }
        let mut d = json!({"dataset_id":id,"name":if string(input,"name").is_empty(){id.as_str()}else{string(input,"name")},"description":string(input,"description"),"tenant_id":actor.tenant_id,"visibility":if string(input,"visibility").is_empty(){"private"}else{string(input,"visibility")},"embedding_provider":provider,"embedding_model":if string(input,"embedding_model").is_empty(){defaults.map_or("text-embedding-v4", |d| d.embedding_model.as_str())}else{string(input,"embedding_model")},"embedding_dimension":dimension,"embedding_config":embedding,"index_config":index,"collection_name":input["collection_name"],"created_by":actor.user_id});
        require_index_writable(&d)?;
        let mut typed: Dataset = serde_json::from_value(d.clone())?;
        let resolved = self.indexer.embeddings.resolve(&typed).await?;
        typed.embedding_dimension = Some(resolved.dimension);
        d["embedding_dimension"] = json!(resolved.dimension);
        let collection = self
            .indexer
            .vectors
            .create_dataset_collection(&typed, resolved.dimension)
            .await?;
        typed.collection_name = Some(collection.clone());
        d["collection_name"] = json!(collection);
        match self
            .store
            .create_dataset_with_owner(&d, &actor.user_id)
            .await
        {
            Ok(true) => Ok(redact_server_config(&self.store.get_dataset(&id).await?)),
            result => {
                if !self
                    .store
                    .collection_in_use(typed.collection_name.as_deref().unwrap_or(""))
                    .await
                    .unwrap_or(true)
                {
                    let _ = self.indexer.vectors.delete_collection(&typed).await;
                }
                match result {
                    Err(e) => Err(e),
                    _ => Err(IngestError::Validation("dataset_id already exists".into())),
                }
            }
        }
    }
    pub async fn create_document_from_text(
        &self,
        actor: &Actor,
        dataset: &str,
        title: &str,
        content: &str,
        metadata: Value,
    ) -> Result<Value> {
        require_public_metadata(&metadata)?;
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        require_index_writable(&d)?;
        let id = uuid::Uuid::new_v4().to_string();
        let content = sanitize_text(content)?;
        let document = json!({"document_id":id,"dataset_id":dataset,"title":if title.is_empty(){id.as_str()}else{title},"source_type":"text","mime_type":"text/plain","size_bytes":content.len(),"status":"waiting","progress":0,"content":content,"metadata":metadata});
        self.store
            .insert_document(&document, &dataset_ingestion_identity(&d))
            .await?;
        self.get_document(actor, dataset, &id).await
    }
    pub async fn create_document_from_upload(
        &self,
        actor: &Actor,
        dataset: &str,
        filename: &str,
        content: &[u8],
        mime: &str,
        metadata: Value,
        mode: &str,
    ) -> Result<Value> {
        require_public_metadata(&metadata)?;
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        require_index_writable(&d)?;
        self.parser.require_supported_source(content, filename, mime)?;
        let id = uuid::Uuid::new_v4().to_string();
        let generation = uuid::Uuid::new_v4().to_string();
        let tenant = string(&d, "tenant_id");
        if tenant.is_empty() {
            return Err(IngestError::Validation(
                "dataset tenant identity is required for document upload".into(),
            ));
        }
        let mime = if mime.is_empty() {
            "application/octet-stream"
        } else {
            mime
        };
        let mut metadata = metadata;
        metadata["processing_mode"] = json!(ProcessingMode::parse(mode).as_str());
        metadata["_document_upload_generation"] = json!(generation);
        let mut document = json!({"document_id":id,"dataset_id":dataset,"title":if filename.is_empty(){id.as_str()}else{filename},"source_type":"upload","mime_type":mime,"size_bytes":content.len(),"status":"uploading","progress":0,"content":"","metadata":metadata});
        let identity = dataset_ingestion_identity(&d);
        self.store.insert_document(&document, &identity).await?;
        let mut lease = self.store.document_lease(dataset, &id).await?;
        let outcome = async {
            let current =
                KnowledgeWriteStore::document_on(lease.connection(), dataset, &id).await?;
            if string(&current, "status") != "uploading"
                || current["metadata"]["_document_upload_generation"] != generation
            {
                return Err(IngestError::GenerationChanged);
            }
            let key = self
                .assets
                .upload_original(tenant, &id, filename, content, mime)
                .await?;
            document["metadata"]["original_file_key"] = json!(key);
            document["metadata"]["original_filename"] = json!(filename);
            document["metadata"]["original_mime_type"] = json!(mime);
            document["metadata"]["mime_type"] = json!(mime);
            document["status"] = json!("waiting");
            if !KnowledgeWriteStore::finalize_upload(
                lease.connection(),
                &document,
                &generation,
                &identity,
            )
            .await?
            {
                return Err(IngestError::GenerationChanged);
            }
            KnowledgeWriteStore::document_on(lease.connection(), dataset, &id).await
        }
        .await;
        if outcome.is_err() {
            let _ = self.assets.delete_document_assets(tenant, &id).await;
        }
        lease.release().await?;
        outcome.map(with_display_status)
    }
    /// Submission pins configuration and its immutable process rule before the
    /// waiting transition, rather than reading mutable dataset config on replay.
    pub async fn submit(
        &self,
        actor: &Actor,
        dataset: &str,
        document: &str,
        action: &str,
    ) -> Result<Value> {
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        require_index_writable(&d)?;
        let mut lease = self.store.document_lease(dataset, document).await?;
        let doc = KnowledgeWriteStore::document_on(lease.connection(), dataset, document).await?;
        let metadata = object(&doc["metadata"]);
        let processing_mode = metadata["processing_mode"].as_str().unwrap_or("text_only");
        let index = object(&d["index_config"]);
        ChunkingConfig::from_value(&index["chunking"])?;
        let snapshot = json!({"index_config":index,"chunking":object(&index["chunking"]),"processing_mode":processing_mode});
        let mut tx = sqlx::Connection::begin(lease.connection()).await?;
        let rule = KnowledgeWriteStore::record_process_rule(
            &mut tx,
            dataset,
            processing_mode,
            &snapshot,
            Some(&actor.user_id),
        )
        .await?;
        let execution = KnowledgeWriteStore::record_execution(
            &mut tx,
            dataset,
            document,
            action,
            "api",
            Some(&actor.user_id),
            Some(&rule),
            &snapshot,
        )
        .await?;
        let stage = if action == "recover"
            && ["parsing", "splitting", "indexing"].contains(&string(&doc, "status"))
        {
            Some(string(&doc, "status"))
        } else {
            None
        };
        if !KnowledgeWriteStore::enqueue_on(
            &mut tx,
            dataset,
            document,
            Some(action),
            stage,
            Some(&execution),
            ["reprocess", "recover", "retry"].contains(&action),
        )
        .await?
        {
            return Err(IngestError::Validation(
                "document is already processing or is not eligible for this action".into(),
            ));
        }
        sqlx::query("UPDATE knowledge.documents SET process_rule_id=$2,metadata=COALESCE(metadata,'{}'::jsonb)||jsonb_build_object('_document_pipeline_execution_id',$3::text) WHERE document_id=$1").bind(document).bind(&rule).bind(&execution).execute(&mut *tx).await?;
        tx.commit().await?;
        lease.release().await?;
        Ok(
            json!({"document_id":document,"dataset_id":dataset,"execution_id":execution,"status":"waiting","action":action}),
        )
    }
    pub async fn ingest_document(&self, dataset: &str, document: &str) -> Result<Vec<String>> {
        let mut lease = self.store.document_lease(dataset, document).await?;
        if !KnowledgeWriteStore::claim_processing(lease.connection(), dataset, document).await? {
            lease.release().await?;
            return Ok(vec![]);
        }
        let result = self.ingest_owned(&mut lease).await;
        if let Err(error) = &result
            && *error != IngestError::PublicationContended
        {
            let current =
                KnowledgeWriteStore::document_on(lease.connection(), dataset, document).await?;
            if current["metadata"]
                .get("_special_publication_generation_id")
                .is_none()
            {
                let _ = KnowledgeWriteStore::update_status(
                    lease.connection(),
                    document,
                    "error",
                    Some(100.0),
                    Some(&error.to_string()),
                )
                .await;
            }
        }
        lease.release().await?;
        // Do not expose the worker-only disposition through the public error
        // contract. The same document generation is durably rediscovered.
        result.map_err(|error| match error {
            IngestError::PublicationContended => IngestError::LeaseBusy,
            other => other,
        })
    }
    pub async fn ingest_owned(&self, lease: &mut DocumentLease) -> Result<Vec<String>> {
        let dataset_id = lease.dataset_id.clone();
        let document_id = lease.document_id.clone();
        let d =
            KnowledgeWriteStore::require_identity(lease.connection(), &dataset_id, None).await?;
        let dataset: Dataset = serde_json::from_value(d.clone())?;
        let identity = dataset_ingestion_identity(&d);
        let doc =
            KnowledgeWriteStore::document_on(lease.connection(), &dataset_id, &document_id).await?;
        let metadata = object(&doc["metadata"]);
        if metadata.get("structured_parsing").is_some() {
            return Err(IngestError::Validation(
                "structured parsing is disabled until a trusted source receipt exists".into(),
            ));
        }
        let action = metadata["_document_ingest_action"]
            .as_str()
            .unwrap_or("ingest");
        let (execution, index, processing_mode) =
            self.prepare_execution(lease, &d, &doc, action).await?;
        let result = self
            .ingest_standard(
                lease,
                &dataset,
                &doc,
                &identity,
                &index,
                &processing_mode,
                action,
                &execution,
            )
            .await;
        match &result {
            Ok(ids) => {
                KnowledgeWriteStore::complete_execution(
                    lease.connection(),
                    &execution,
                    "completed",
                    None,
                    &json!({"staged_segment_ids":ids}),
                )
                .await?;
            }
            Err(IngestError::PublicationContended) => {
                // Publication admission failed before this worker mutated the
                // index. Keep the immutable rule/execution pin, and requeue only
                // if the exact running generation still belongs to this lease.
                // A refused CAS or uncertain DB result must not close either
                // the old execution or a newer document owner as failed.
                let _ = KnowledgeWriteStore::requeue_unpublished_generation(
                    lease.connection(),
                    &dataset_id,
                    &document_id,
                    &execution,
                )
                .await;
            }
            Err(error) => {
                if KnowledgeWriteStore::get_special(lease.connection(), &execution)
                    .await?
                    .is_none()
                {
                    let _ = KnowledgeWriteStore::complete_execution(
                        lease.connection(),
                        &execution,
                        "error",
                        Some(&error.to_string()),
                        &json!({}),
                    )
                    .await;
                }
            }
        }
        result
    }
    async fn prepare_execution(
        &self,
        lease: &mut DocumentLease,
        d: &Value,
        doc: &Value,
        action: &str,
    ) -> Result<(String, Value, String)> {
        let metadata = object(&doc["metadata"]);
        let linked = string(&metadata, "_document_pipeline_execution_id");
        let dataset = string(d, "dataset_id");
        let document = string(doc, "document_id");
        if !linked.is_empty() {
            let execution:Value=sqlx::query_scalar("SELECT to_jsonb(e) FROM knowledge.document_pipeline_executions e WHERE execution_id=$1").bind(linked).fetch_optional(lease.connection()).await?.ok_or_else(||IngestError::Validation("replay execution snapshot is missing".into()))?;
            if string(&execution, "status") != "running"
                || string(&execution, "action") != action
                || string(&execution, "dataset_id") != dataset
                || string(&execution, "document_id") != document
            {
                return Err(IngestError::GenerationChanged);
            }
            let rule = string(&execution, "process_rule_id");
            if rule.is_empty() || doc["process_rule_id"].as_str() != Some(rule) {
                return Err(IngestError::Validation(
                    "document process-rule pin does not match execution".into(),
                ));
            }
            let stored: Value = sqlx::query_scalar(
                "SELECT rules FROM knowledge.dataset_process_rules WHERE id=$1 AND dataset_id=$2",
            )
            .bind(rule)
            .bind(dataset)
            .fetch_optional(lease.connection())
            .await?
            .ok_or_else(|| {
                IngestError::Validation("pinned process-rule snapshot is missing".into())
            })?;
            let snapshot = normalize_snapshot(&execution["input_snapshot"])?;
            if canonical_json(&snapshot) != canonical_json(&normalize_snapshot(&stored)?) {
                return Err(IngestError::Validation(
                    "execution and process-rule snapshots disagree".into(),
                ));
            }
            return Ok((
                linked.into(),
                snapshot["index_config"].clone(),
                string(&snapshot, "processing_mode").into(),
            ));
        }
        let pinned_rule = string(doc, "process_rule_id");
        if !pinned_rule.is_empty()
            && (action == "recover" || metadata.get("_document_recover_stage").is_some())
        {
            let rules: Value = sqlx::query_scalar(
                "SELECT rules FROM knowledge.dataset_process_rules WHERE id=$1 AND dataset_id=$2",
            )
            .bind(pinned_rule)
            .bind(dataset)
            .fetch_optional(lease.connection())
            .await?
            .ok_or_else(|| {
                IngestError::Validation("recover process-rule snapshot missing".into())
            })?;
            let snapshot = normalize_snapshot(&rules)?;
            let execution = KnowledgeWriteStore::record_execution(
                lease.connection(),
                dataset,
                document,
                action,
                "worker",
                None,
                Some(pinned_rule),
                &rules,
            )
            .await?;
            return Ok((
                execution,
                snapshot["index_config"].clone(),
                string(&snapshot, "processing_mode").into(),
            ));
        }
        if ["reprocess", "retry"].contains(&action) {
            return Err(IngestError::Validation(
                "replay execution id is missing".into(),
            ));
        }
        let index = object(&d["index_config"]);
        let mode = metadata["processing_mode"].as_str().unwrap_or("text_only");
        let snapshot = json!({"index_config":index,"chunking":object(&index["chunking"]),"processing_mode":mode});
        let rule = KnowledgeWriteStore::record_process_rule(
            lease.connection(),
            dataset,
            mode,
            &snapshot,
            None,
        )
        .await?;
        sqlx::query("UPDATE knowledge.documents SET process_rule_id=$2 WHERE document_id=$1")
            .bind(document)
            .bind(&rule)
            .execute(lease.connection())
            .await?;
        let execution = KnowledgeWriteStore::record_execution(
            lease.connection(),
            dataset,
            document,
            action,
            "worker",
            None,
            Some(&rule),
            &snapshot,
        )
        .await?;
        Ok((execution, index, mode.into()))
    }
    async fn ingest_standard(
        &self,
        lease: &mut DocumentLease,
        dataset: &Dataset,
        doc: &Value,
        identity: &str,
        index: &Value,
        processing_mode: &str,
        action: &str,
        execution: &str,
    ) -> Result<Vec<String>> {
        let document_id = string(doc, "document_id");
        let mut metadata = object(&doc["metadata"]);
        if metadata.get("structured_parsing").is_some() {
            return Err(IngestError::Validation(
                "structured parsing is disabled until a trusted source receipt exists".into(),
            ));
        }
        if let Some(restored) = metadata.get("_document_restored_source_version") {
            let number = restored
                .as_i64()
                .filter(|n| *n > 0 && *n == integer(doc, "current_version"))
                .ok_or(IngestError::GenerationChanged)?;
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.document_versions WHERE document_id=$1 AND version_number=$2 AND change_type='restored' AND content=$3)").bind(document_id).bind(number as i32).bind(string(doc,"content")).fetch_one(lease.connection()).await?;
            if !valid {
                return Err(IngestError::GenerationChanged);
            }
        }
        let old =
            KnowledgeWriteStore::segments_on(lease.connection(), &dataset.dataset_id, document_id)
                .await?;
        let repair_rows: Vec<_> = old
            .iter()
            .filter(|r| {
                r.content_type == "text"
                    && (r.status == "indexing" || (r.status == "completed" && r.enabled))
            })
            .cloned()
            .collect();
        let action = if action == "reembed" && repair_rows.is_empty() {
            "ingest"
        } else {
            action
        };

        let mut effective_mode = processing_mode.to_owned();
        if effective_mode == "auto" && action != "reembed" {
            if let Some(resolved) = metadata["processing_mode"]
                .as_str()
                .filter(|m| ["text_only", "scanned", "multimodal"].contains(m))
            {
                effective_mode = resolved.into();
            }
        }

        if effective_mode == "auto" && action != "reembed" {
            if let Some(key) = metadata["original_file_key"].as_str() {
                let detection = if integer(doc, "size_bytes") > self.large_file_threshold as i64 {
                    self.detect_large_pdf(key).await
                } else {
                    let bytes = self.assets.download_original(key).await?;
                    let samples = if self.image_extractor.pdf.is_some() {
                        self.image_extractor.sample_pdf(&bytes).await.ok()
                    } else {
                        None
                    };
                    super::detection::DocumentTypeDetector::default().detect(
                        &bytes,
                        string(doc, "title"),
                        string(doc, "mime_type"),
                        samples
                            .as_ref()
                            .map(|(count, pages)| (*count, pages.as_slice())),
                    )
                };
                let (mode, receipt) = match detection {
                    Ok(result) => (
                        result.recommended_mode.as_str().to_owned(),
                        serde_json::to_value(result)?,
                    ),
                    Err(_) => (
                        "text_only".into(),
                        json!({"fallback":true,"reason":"detection_failed"}),
                    ),
                };
                let changed=sqlx::query("UPDATE knowledge.documents SET detection_result=$3::jsonb,metadata=jsonb_set(COALESCE(metadata,'{}'::jsonb),'{processing_mode}',to_jsonb($4::text),TRUE),updated_at=NOW() WHERE document_id=$1 AND dataset_id=$2 AND status='parsing' AND COALESCE(metadata->>'processing_mode','text_only')='auto'").bind(document_id).bind(&dataset.dataset_id).bind(receipt).bind(&mode).execute(lease.connection()).await?.rows_affected();
                if changed != 1 {
                    return Err(IngestError::GenerationChanged);
                }
                effective_mode = mode;
            } else {
                effective_mode = "text_only".into()
            }
        }
        let parsing_config = super::structured::resolve_config(index)?;
        let pending_owned = metadata
            .get("_document_pending_restore_version")
            .filter(|v| v.is_object())
            .cloned();
        let pending = pending_owned.as_ref();
        let mut content = string(doc, "content").to_owned();
        let mut patch = json!({});
        if let Some(pending) = pending {
            let number = integer(pending, "candidate_version");
            if integer(pending, "previous_version") <= 0
                || number <= integer(pending, "previous_version")
            {
                return Err(IngestError::GenerationChanged);
            }
            let target:Value=sqlx::query_scalar("SELECT to_jsonb(v) FROM knowledge.document_versions v WHERE document_id=$1 AND version_number=$2 AND change_type='pending_restore'").bind(document_id).bind(number as i32).fetch_optional(lease.connection()).await?.ok_or(IngestError::GenerationChanged)?;
            content = string(&target, "content").to_owned();
            if target["metadata"]["_special_source_manifest"].is_object() {
                return self
                    .ingest_special_restore(lease, dataset, doc, index, execution, &target)
                    .await;
            }
        }

        if effective_mode == "scanned" && action != "reembed" {
            return self
                .ingest_vision(lease, dataset, doc, index, execution, None)
                .await;
        }
        let reembed = action == "reembed";
        let streamed = !reembed
            && pending.is_none()
            && effective_mode != "multimodal"
            && integer(doc, "size_bytes") > self.large_file_threshold as i64;
        if streamed {
            let result = self.extract_large_file(lease, dataset, doc).await?;
            content = result.text;
            patch["total_pages"] = json!(result.total_pages);
            patch["streaming_processed"] = json!(true);
        }
        let mut chunks = if reembed {
            repair_rows
        } else {
            KnowledgeWriteStore::update_status(
                lease.connection(),
                document_id,
                "parsing",
                Some(10.0),
                None,
            )
            .await?;
            if !streamed
                && pending.is_none()
                && metadata.get("_document_restored_source_version").is_none()
                && content.trim().chars().count() < self.reextraction_min_chars
                && metadata["original_file_key"]
                    .as_str()
                    .is_some_and(|key| !key.is_empty())
            {
                let mut receipt_failed = false;
                let reextracted: Result<String> = async {
                    let key = string(&metadata, "original_file_key");
                    let source = Arc::new(self.assets.download_original(key).await?);
                    let filename = string(&metadata, "original_filename").to_owned();
                    let mime = string(&metadata, "original_mime_type").to_owned();
                    let used_multimodal = ["multimodal", "scanned"]
                        .contains(&effective_mode.as_str())
                        && self.multimodal_reextraction_enabled;
                    let mut text = if used_multimodal {
                        let result = self
                            .image_extractor
                            .extract(&filename, &source, None)
                            .await?;
                        let images: Vec<_> = result
                            .images
                            .into_iter()
                            .filter(|i| i.embeddable())
                            .collect();
                        if !images.is_empty()
                            && metadata["extracted_images"]
                                .as_array()
                                .is_none_or(|v| v.is_empty())
                        {
                            let mut receipt_doc = doc.clone();
                            receipt_doc["metadata"] = metadata.clone();
                            // Every persistence error is fatal, even if its underlying
                            // classification is storage or a missing object.
                            receipt_failed = true;
                            metadata = self
                                .persist_image_receipt(
                                    lease,
                                    dataset,
                                    &receipt_doc,
                                    &effective_mode,
                                    &images,
                                )
                                .await?;
                            receipt_failed = false;
                        }
                        result.text
                    } else {
                        let parser = self.parser.clone();
                        let permit = self
                            .cpu
                            .clone()
                            .acquire_owned()
                            .await
                            .map_err(|_| IngestError::Cancelled)?;
                        let source = source.clone();
                        let name = filename.clone();
                        let content_type = mime.clone();
                        let (text, _) = tokio::task::spawn_blocking(move || {
                            let _permit = permit;
                            parser.extract_text_from_bytes(&source, &name, &content_type)
                        })
                        .await
                        .map_err(|error| {
                            if error.is_cancelled() {
                                IngestError::Cancelled
                            } else {
                                IngestError::Storage
                            }
                        })??;
                        text
                    };
                    if super::reextraction::needs_legacy_pdf_ocr(
                        used_multimodal,
                        &text,
                        &filename,
                        &mime,
                        self.streaming_ocr_enabled,
                        self.reextraction_min_chars,
                    ) {
                        // Canonical whole-PDF OCR deliberately rejects in this release.
                        // Reuse that boundary instead of silently indexing short text.
                        let parser = self.parser.clone();
                        let ocr =
                            tokio::task::spawn_blocking(move || parser.ocr_pdf_bytes(&source))
                                .await
                                .map_err(|error| {
                                    if error.is_cancelled() {
                                        IngestError::Cancelled
                                    } else {
                                        IngestError::Storage
                                    }
                                })??;
                        if !ocr.trim().is_empty() {
                            text = ocr;
                        }
                    }
                    Ok(text)
                }
                .await;
                if let Some(text) =
                    super::reextraction::settle(reextracted, receipt_failed, &content)?
                {
                    content = text;
                }
                // Source does not mark ocr_processed here. The replacement text
                // stays private until publish_incremental's atomic activation.
            }
            require_text_budget(&content)?;
            let has_images = ["multimodal", "scanned"].contains(&effective_mode.as_str())
                && metadata["extracted_images"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty());
            if content.trim().is_empty() && !has_images {
                return Err(IngestError::Validation(
                    "document has no text to index".into(),
                ));
            }
            if let Some(config) = parsing_config {
                let hash = sha256(content.as_bytes());
                let config_hash =
                    sha256(canonical_json(&serde_json::to_value(&config)?).as_bytes());
                let cache = super::structured::PostgresPageCache {
                    pool: self.store.pool.clone(),
                    tenant_id: dataset.tenant_id.clone(),
                    dataset_id: dataset.dataset_id.clone(),
                    document_id: document_id.into(),
                    generation_key: hash.clone(),
                    parser_config_hash: config_hash.clone(),
                    page_content_hashes: BTreeMap::from([(1, hash.clone())]),
                };
                let mut cascade = super::structured::ParserCascade::new(
                    config.clone(),
                    self.structured_backends.clone(),
                )?;
                cascade.persistent = Some(cache.clone());
                let ir = if let Some(cached) = cache.load_ir(&cascade.bundle_version()).await? {
                    cached
                } else {
                    let mut job = super::structured::PageJob::text(
                        document_id,
                        &content,
                        Some(string(doc, "mime_type").into()),
                    );
                    job.options["parser_config_hash"] = json!(config_hash);
                    let ir = cascade
                        .parse_document(
                            document_id,
                            vec![job],
                            &hash,
                            string(doc, "title"),
                            Some(string(doc, "mime_type").into()),
                        )
                        .await?;
                    cache
                        .store_ir(&ir, &config, &json!({"page_count":ir.pages.len()}))
                        .await?;
                    ir
                };
                content = super::structured::render_document_markdown(&ir, false);
                patch["parsing_ir"] = json!({"generation_key":hash,"parser_bundle":cascade.bundle_version(),"parser_config_hash":config_hash});
            }
            KnowledgeWriteStore::update_status(
                lease.connection(),
                document_id,
                "splitting",
                Some(30.0),
                None,
            )
            .await?;
            let config = ChunkingConfig::from_value(&index["chunking"])?;
            if index["chunking"]["mode"]
                .as_str()
                .is_some_and(|m| m.trim().eq_ignore_ascii_case("hierarchical"))
            {
                return self
                    .ingest_hierarchy(
                        lease,
                        dataset,
                        doc,
                        index,
                        execution,
                        &content,
                        None,
                        Some(&patch),
                    )
                    .await;
            }
            let worker = self.chunker.clone();
            let text = content.clone();
            let ds = dataset.dataset_id.clone();
            let id = document_id.to_owned();
            let name = string(doc, "title").to_owned();
            let source = string(doc, "source_type").to_owned();
            let permit = self
                .cpu
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| IngestError::Cancelled)?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                worker.standard_chunks(&text, &config, &ds, &id, &name, &source)
            })
            .await
            .map_err(|_| IngestError::Parser("document chunking task failed".into()))??
        };
        let mut image_points = BTreeMap::new();
        let mut replace_images = false;
        if !reembed && pending.is_none() {
            let receipts = metadata
                .get("extracted_images")
                .cloned()
                .unwrap_or_else(|| json!([]));
            let receipts = receipts.as_array().ok_or_else(|| {
                IngestError::Validation("durable image receipt must be a list".into())
            })?;
            let required = ["multimodal", "scanned"].contains(&effective_mode.as_str())
                && (!receipts.is_empty() || integer(&metadata, "image_count") > 0);
            if required && receipts.is_empty() {
                return Err(IngestError::Validation(
                    "document declares images without a durable rebuild receipt".into(),
                ));
            }
            let image_provider = dataset.embedding_provider.as_deref().unwrap_or("local");
            let unified = [
                "unified_multimodal",
                "unified",
                "cross_modal",
                "dashscope_multimodal",
                "aliyun_multimodal",
                "multimodal",
            ]
            .contains(&image_provider);
            if required && !unified {
                if content.trim().is_empty() {
                    return Err(IngestError::Validation(
                        "image-only documents require a unified multimodal dataset".into(),
                    ));
                }
                patch["image_indexing"] = json!({"status":"skipped","reason":"text_only_dataset","receipt_count":receipts.len(),"indexed_count":0});
            }
            if required && unified && metadata["images_embedded"].as_bool() != Some(true) {
                let position = chunks
                    .iter()
                    .chain(old.iter().filter(|r| r.content_type == "text"))
                    .map(|r| r.position)
                    .max()
                    .unwrap_or(-1)
                    + 1;
                let (rows, points) = self
                    .prepare_receipt_images(dataset, document_id, receipts, &old, position)
                    .await?;
                chunks.extend(rows);
                image_points = points;
                replace_images = true;
            }
        }
        self.publish_incremental(
            lease,
            dataset,
            doc,
            identity,
            &content,
            &patch,
            pending,
            &old,
            &mut chunks,
            reembed,
            image_points,
            replace_images,
        )
        .await
    }
}
pub fn with_display_status(mut value: Value) -> Value {
    value["display_status"] = json!(display_status(&value));
    value["processing_diagnostics"] = super::diagnostics::processing_diagnostics(&value);
    super::diagnostics::sanitize_processing_error(&mut value);
    value
}
pub(super) fn contains_server_config(v: &Value) -> bool {
    match v {
        Value::Object(m) => m
            .iter()
            .any(|(k, v)| sensitive_key(k) || contains_server_config(v)),
        Value::Array(a) => a.iter().any(contains_server_config),
        _ => false,
    }
}
fn sensitive_key(key: &str) -> bool {
    let key: String = key
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    [
        "apikey",
        "key",
        "accesskey",
        "secret",
        "secretkey",
        "token",
        "bearertoken",
        "authorization",
        "auth",
        "credentials",
        "headers",
        "baseurl",
        "endpoint",
        "endpointurl",
        "apibase",
        "apiurl",
        "url",
        "host",
    ]
    .contains(&key.as_str())
        || [
            "apikey",
            "accesskey",
            "secret",
            "secretkey",
            "bearertoken",
            "authtoken",
            "accesstoken",
            "refreshtoken",
            "authorization",
            "credentials",
            "headers",
            "baseurl",
            "endpoint",
            "endpointurl",
            "apibase",
            "apiurl",
            "url",
            "host",
        ]
        .iter()
        .any(|suffix| key.ends_with(suffix))
}
pub fn redact_server_config(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        if sensitive_key(k) {
                            json!("*****")
                        } else {
                            redact_server_config(v)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(redact_server_config).collect()),
        _ => v.clone(),
    }
}
fn normalize_snapshot(input: &Value) -> Result<Value> {
    let v = object(input);
    let mut index = if v["index_config"].is_object() {
        v["index_config"].clone()
    } else if v["chunking"].is_object() {
        json!({"chunking":v["chunking"]})
    } else {
        return Err(IngestError::Validation(
            "replay snapshot has no pinned chunking config".into(),
        ));
    };
    if index.get("chunking").is_none() && v["chunking"].is_object() {
        index["chunking"] = v["chunking"].clone()
    }
    if !index["chunking"].is_object()
        || (v["chunking"].is_object() && index["chunking"] != v["chunking"])
    {
        return Err(IngestError::Validation(
            "replay snapshot chunking aliases disagree".into(),
        ));
    }
    ChunkingConfig::from_value(&index["chunking"])?;
    let mode = string(&v, "processing_mode").trim().to_lowercase();
    if !["auto", "text_only", "scanned", "multimodal"].contains(&mode.as_str()) {
        return Err(IngestError::Validation(
            "replay snapshot has no valid pinned processing mode".into(),
        ));
    }
    Ok(
        json!({"index_config":index,"processing_mode":mode,"restore_source_version":v["restore_source_version"],"restore_source_kind":v["restore_source_kind"],"restore_source_hash":v["restore_source_hash"]}),
    )
}

impl IngestionService {
    pub async fn delete_document(
        &self,
        actor: &Actor,
        dataset: &str,
        document: &str,
    ) -> Result<bool> {
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        let target = json!({"operation":"document_delete","target_id":document,"status":"pending","version":1});
        if let Some(fence) = d["index_config"]["retrieval"].get("_index_deletion_fence") {
            if fence != &target {
                return Err(IngestError::Validation(
                    "another dataset index deletion target is already pending".into(),
                ));
            }
        }
        let mut lease = self.store.dataset_delete_lease(dataset).await?;
        let current = KnowledgeWriteStore::dataset_on(lease.connection(), dataset).await?;
        if string(&current, "tenant_id") != string(&d, "tenant_id") {
            return Err(IngestError::GenerationChanged);
        }
        match KnowledgeWriteStore::document_on(lease.connection(), dataset, document).await {
            Ok(_) => {}
            Err(IngestError::NotFound)
                if current["index_config"]["retrieval"]["_index_deletion_fence"] == target =>
            {
                let cleared = KnowledgeWriteStore::clear_deletion_fence(
                    lease.connection(),
                    dataset,
                    "document_delete",
                    document,
                )
                .await?;
                lease.release().await?;
                return Ok(cleared);
            }
            Err(e) => return Err(e),
        }
        let current = KnowledgeWriteStore::set_deletion_fence(
            lease.connection(),
            dataset,
            "document_delete",
            document,
        )
        .await?;
        let typed: Dataset = serde_json::from_value(current)?;
        self.indexer
            .vectors
            .delete_document_points(&typed, document)
            .await?;
        self.assets
            .delete_document_assets(&typed.tenant_id, document)
            .await?;
        let deleted =
            sqlx::query("DELETE FROM knowledge.documents WHERE document_id=$1 AND dataset_id=$2")
                .bind(document)
                .bind(dataset)
                .execute(lease.connection())
                .await?
                .rows_affected()
                > 0;
        if !deleted {
            return Err(IngestError::Validation(
                "document database deletion failed; index deletion fence remains".into(),
            ));
        }
        if !KnowledgeWriteStore::clear_deletion_fence(
            lease.connection(),
            dataset,
            "document_delete",
            document,
        )
        .await?
        {
            return Err(IngestError::Validation(
                "document deletion committed but its index fence remains pending".into(),
            ));
        }
        lease.release().await?;
        Ok(true)
    }
    /// `deletion_confirmed` is the verified gateway assertion, never a public
    /// body boolean. HTTP mounting must preserve the gateway confirmation proof.
    pub async fn delete_dataset(
        &self,
        actor: &Actor,
        dataset: &str,
        deletion_confirmed: bool,
        reason: Option<&str>,
    ) -> Result<bool> {
        let d = self
            .store
            .require_dataset_access(actor, dataset, "owner")
            .await?;
        if actor.anonymous() || !deletion_confirmed {
            return Err(IngestError::Permission);
        }
        let mut lease = self.store.dataset_delete_lease(dataset).await?;
        let current = KnowledgeWriteStore::set_deletion_fence(
            lease.connection(),
            dataset,
            "dataset_delete",
            dataset,
        )
        .await?;
        if string(&current, "tenant_id") != string(&d, "tenant_id") {
            return Err(IngestError::GenerationChanged);
        }
        let typed: Dataset = serde_json::from_value(current)?;
        let documents: Vec<String> = sqlx::query_scalar(
            "SELECT document_id FROM knowledge.documents WHERE dataset_id=$1 ORDER BY document_id",
        )
        .bind(dataset)
        .fetch_all(lease.connection())
        .await?;
        for id in &documents {
            self.assets
                .delete_document_assets(&typed.tenant_id, id)
                .await?
        }
        self.indexer
            .vectors
            .delete_dataset_collections(&typed)
            .await?;
        let mut tx = sqlx::Connection::begin(lease.connection()).await?;
        let changed=sqlx::query("UPDATE knowledge.datasets SET is_deleted=TRUE,deleted_at=NOW(),deleted_by=$2,delete_reason=$3,updated_at=NOW() WHERE dataset_id=$1 AND is_deleted=FALSE").bind(dataset).bind(&actor.user_id).bind(reason).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(IngestError::GenerationChanged);
        }
        for table in [
            "confluence_space_bindings",
            "version_retention_policies",
            "dataset_keyword_tables",
            "dataset_process_rules",
            "dataset_queries",
            "child_chunks",
            "dataset_permissions",
            "documents",
        ] {
            sqlx::query(&format!(
                "DELETE FROM knowledge.{table} WHERE dataset_id=$1"
            ))
            .bind(dataset)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        lease.release().await?;
        Ok(true)
    }
}

impl IngestionService {
    async fn begin_special(
        &self,
        lease: &mut DocumentLease,
        execution: &str,
        hash: &str,
        keys: &[String],
    ) -> Result<String> {
        let generation = uuid::Uuid::parse_str(execution)
            .map_err(|_| {
                IngestError::Validation("special publication execution ID is not UUID".into())
            })?
            .to_string();
        let doc = lease.document_id.clone();
        let ds = lease.dataset_id.clone();
        let mut tx = sqlx::Connection::begin(lease.connection()).await?;
        KnowledgeWriteStore::record_special(&mut tx, execution, &doc, &ds, &generation, hash, keys)
            .await?;
        tx.commit().await?;
        Ok(generation)
    }
    async fn ingest_hierarchy(
        &self,
        lease: &mut DocumentLease,
        dataset: &Dataset,
        doc: &Value,
        index: &Value,
        execution: &str,
        content: &str,
        metadata: Option<&Value>,
        candidate_metadata_patch: Option<&Value>,
    ) -> Result<Vec<String>> {
        let generation = self
            .begin_special(lease, execution, &sha256(content.as_bytes()), &[])
            .await?;
        let mut scoped = dataset.clone();
        scoped.index_config = index.clone();
        let indexer = super::hierarchy::HierarchicalIndexer {
            chunker: self.chunker.clone(),
            embeddings: self.indexer.embeddings.clone(),
            summary: self.summary.clone(),
            levels: vec![1, 2, 3],
        };
        let config = ChunkingConfig::from_value(&index["chunking"])?;
        let coordinator = super::special::SpecialPublicationCoordinator {
            service: self.clone(),
        };
        let plan = match indexer
            .prepare_document(
                &scoped,
                string(doc, "document_id"),
                content,
                &generation,
                metadata.unwrap_or(&doc["metadata"]),
                Some(&config),
                None,
            )
            .await
        {
            Ok(plan) => plan,
            Err(error) => {
                let _ = coordinator.abort_preparing(&scoped, execution, false).await;
                return Err(error);
            }
        };
        let mut patch = candidate_metadata_patch
            .map(object)
            .unwrap_or_else(|| json!({}));
        patch.as_object_mut().unwrap().extend(object(&json!({"l1_segments":plan.l1_count,"l2_segments":plan.l2_count,"l3_segments":plan.l3_count,"total_vectors":plan.segment_ids.len()})).as_object().unwrap().clone());
        KnowledgeWriteStore::update_status(
            lease.connection(),
            string(doc, "document_id"),
            "indexing",
            Some(75.0),
            None,
        )
        .await?;
        coordinator
            .publish(plan, &scoped, execution, string(doc, "content"), &patch)
            .await
    }
    async fn ingest_vision(
        &self,
        lease: &mut DocumentLease,
        dataset: &Dataset,
        doc: &Value,
        index: &Value,
        execution: &str,
        target: Option<&Value>,
    ) -> Result<Vec<String>> {
        let vision=self.vision.as_ref().ok_or_else(||IngestError::Parser("canonical PyMuPDF page renderer is unavailable; scanned candidate remains unprepared".into()))?;
        let metadata = object(&doc["metadata"]);
        let source = target.map(|t| object(&t["metadata"]["_special_source_manifest"]));
        let key = source
            .as_ref()
            .map(|s| string(s, "original_source_key"))
            .unwrap_or(string(&metadata, "original_file_key"));
        if !key.starts_with(&format!(
            "knowledge/documents/{}/{}/original/",
            dataset.tenant_id,
            string(doc, "document_id")
        )) {
            return Err(IngestError::Validation(
                "scanned original source owner differs".into(),
            ));
        }
        let bytes = self.assets.download_original(key).await?;
        let hash = sha256(&bytes);
        if source.as_ref().is_some_and(|s| s["source_hash"] != hash) {
            return Err(IngestError::GenerationChanged);
        }
        let mut pinned = None;
        if let Some(source) = &source {
            let mut pages = BTreeMap::new();
            for p in source["page_texts"]
                .as_array()
                .ok_or(IngestError::GenerationChanged)?
            {
                let n = p["page_number"]
                    .as_u64()
                    .filter(|n| *n > 0)
                    .ok_or(IngestError::GenerationChanged)? as usize;
                let text = p["text"].as_str().ok_or(IngestError::GenerationChanged)?;
                if pages.insert(n, text.to_owned()).is_some() {
                    return Err(IngestError::GenerationChanged);
                }
            }
            pinned = Some(pages)
        }
        let keys = vision
            .planned_object_keys(
                &bytes,
                &dataset.tenant_id,
                string(doc, "document_id"),
                execution,
                0,
            )
            .await?;
        let generation = self.begin_special(lease, execution, &hash, &keys).await?;
        let mut scoped = dataset.clone();
        scoped.index_config = index.clone();
        let coordinator = super::special::SpecialPublicationCoordinator {
            service: self.clone(),
        };
        let plan = match vision
            .prepare_document(
                &bytes,
                &scoped,
                string(doc, "document_id"),
                &generation,
                &self.assets,
                pinned.as_ref(),
                0,
            )
            .await
        {
            Ok(plan) => plan,
            Err(error) => {
                let _ = coordinator.abort_preparing(&scoped, execution, false).await;
                return Err(error);
            }
        };
        let patch = json!({"pages_processed":plan.total_pages,"total_pages":plan.total_pages,"segments_created":plan.segment_ids.len(),"ocr_strategy":if target.is_some(){"restored_pinned_page_text"}else{"vision"}});
        KnowledgeWriteStore::update_status(
            lease.connection(),
            string(doc, "document_id"),
            "indexing",
            Some(75.0),
            None,
        )
        .await?;
        coordinator
            .publish(plan, &scoped, execution, string(doc, "content"), &patch)
            .await
    }
    async fn ingest_special_restore(
        &self,
        lease: &mut DocumentLease,
        dataset: &Dataset,
        doc: &Value,
        index: &Value,
        execution: &str,
        target: &Value,
    ) -> Result<Vec<String>> {
        let source = object(&target["metadata"]["_special_source_manifest"]);
        let content = string(target, "content");
        let hash = sha256(content.as_bytes());
        if source["content_hash"] != hash || source["index_config"] != *index {
            return Err(IngestError::GenerationChanged);
        }
        match string(&source, "source_kind") {
            "hierarchy" => {
                if source["source_hash"] != hash {
                    return Err(IngestError::GenerationChanged);
                }
                self.ingest_hierarchy(
                    lease,
                    dataset,
                    doc,
                    index,
                    execution,
                    content,
                    Some(&target["metadata"]),
                    None,
                )
                .await
            }
            "vision" => {
                self.ingest_vision(lease, dataset, doc, index, execution, Some(target))
                    .await
            }
            "text" => {
                if source["source_hash"] != hash
                    || source["objects"] != json!({})
                    || source["page_texts"] != json!([])
                {
                    return Err(IngestError::GenerationChanged);
                }
                let generation = self.begin_special(lease, execution, &hash, &[]).await?;
                let mut scoped = dataset.clone();
                scoped.index_config = index.clone();
                let coordinator = super::special::SpecialPublicationCoordinator {
                    service: self.clone(),
                };
                let plan = match self
                    .prepare_text_restore(
                        &scoped,
                        string(doc, "document_id"),
                        &generation,
                        content,
                        string(target, "title"),
                        &index["chunking"],
                    )
                    .await
                {
                    Ok(plan) => plan,
                    Err(error) => {
                        let _ = coordinator.abort_preparing(&scoped, execution, false).await;
                        return Err(error);
                    }
                };
                KnowledgeWriteStore::update_status(
                    lease.connection(),
                    string(doc, "document_id"),
                    "indexing",
                    Some(75.0),
                    None,
                )
                .await?;
                coordinator
                    .publish(
                        plan,
                        &scoped,
                        execution,
                        string(doc, "content"),
                        &json!({"l1_segments":0,"l2_segments":0,"l3_segments":0}),
                    )
                    .await
            }
            _ => Err(IngestError::Validation(
                "special restore source kind is invalid".into(),
            )),
        }
    }
}
