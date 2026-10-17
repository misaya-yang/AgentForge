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
    pub model_plane: Option<Arc<crate::migration_knowledge_query::model_plane::ModelPlaneClient>>,
    pub store: KnowledgeWriteStore,
    pub(crate) vector_job_expected: Option<Value>,
    pub vector_job_actor_authority:
        Option<crate::migration_knowledge_query::actor::RuntimeActorClient>,
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
#[path = "service_construction.rs"]
mod construction;
#[path = "service_standard.rs"]
mod standard;
impl IngestionService {
    pub(crate) fn check_vector_job_generation(&self, current: &Value) -> Result<()> {
        if let Some(row) = &self.vector_job_expected {
            super::vector_jobs::check_expected(row, current)?;
        }
        Ok(())
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
        super::parsing_v2::integration::Config::resolve(&index)?;
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
        if self.indexer.vectors.is_read_only() {
            let mut queued_input = input.clone();
            queued_input["dataset_id"] = json!(id);
            return super::vector_jobs::submit(
                &self.store.pool,
                actor,
                &id,
                super::vector_jobs::Mutation::CreateDataset {
                    input: queued_input,
                },
            )
            .await;
        }
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
        self.parser
            .require_supported_source(content, filename, mime)?;
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
        let mut lease = match self.store.document_lease(dataset, document).await {
            Ok(lease) => lease,
            Err(error) => {
                if error == IngestError::LeaseBusy {
                    crate::migration_knowledge_query::pipeline_metrics::worker_claim("deferred");
                }
                return Err(error);
            }
        };
        if !KnowledgeWriteStore::claim_processing(lease.connection(), dataset, document).await? {
            crate::migration_knowledge_query::pipeline_metrics::worker_claim("contended");
            lease.release().await?;
            return Ok(vec![]);
        }
        crate::migration_knowledge_query::pipeline_metrics::worker_claim("claimed");
        let observed = KnowledgeWriteStore::document_on(lease.connection(), dataset, document)
            .await
            .ok();
        let started = std::time::Instant::now();
        let result = self.ingest_owned(&mut lease).await;
        if let Some(row) = observed {
            let metadata = &row["metadata"];
            crate::migration_knowledge_query::pipeline_metrics::ingest(
                metadata["_document_ingest_action"]
                    .as_str()
                    .unwrap_or("ingest"),
                metadata["processing_mode"].as_str().unwrap_or("text_only"),
                result.is_ok(),
                started.elapsed().as_secs_f64(),
            );
        }
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
}
pub fn with_display_status(mut value: Value) -> Value {
    if !value.is_object() {
        return value;
    }
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
        if self.indexer.vectors.is_read_only() {
            let value = super::vector_jobs::submit(
                &self.store.pool,
                actor,
                dataset,
                super::vector_jobs::Mutation::DeleteDocument {
                    document: document.into(),
                },
            )
            .await?;
            return value.as_bool().ok_or(IngestError::Database);
        }
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
        self.check_vector_job_generation(&current)?;
        if dataset_ingestion_identity(&current) != dataset_ingestion_identity(&d) {
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
        if self.indexer.vectors.is_read_only() {
            let value = super::vector_jobs::submit(
                &self.store.pool,
                actor,
                dataset,
                super::vector_jobs::Mutation::DeleteDataset {
                    confirmed: deletion_confirmed,
                    reason: reason.map(str::to_owned),
                },
            )
            .await?;
            return value.as_bool().ok_or(IngestError::Database);
        }
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
        self.check_vector_job_generation(&current)?;
        if dataset_ingestion_identity(&current) != dataset_ingestion_identity(&d) {
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
                .await?;
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
        let mut plan = match vision
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
        let docir = if let Some(profile) = super::parsing_v2::integration::Config::resolve(index)? {
            Some(
                super::parsing_v2::integration::prepare_vision(
                    self, lease, &scoped, doc, execution, &mut plan, &profile,
                )
                .await?,
            )
        } else {
            None
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
            .publish_with_docir(
                plan,
                &scoped,
                execution,
                string(doc, "content"),
                &patch,
                docir.as_ref(),
            )
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
