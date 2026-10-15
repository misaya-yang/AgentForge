//! Historical source snapshots and transactional restore admission.
use super::{
    Actor, Dataset, IngestError, IngestionService, Result, canonical_json,
    chunking::ChunkingConfig, hierarchy::CandidatePlan, integer, object, require_index_writable,
    sha256, sql, store::KnowledgeWriteStore, string,
};
use serde_json::{Value, json};
use sqlx::Connection;
use std::collections::BTreeMap;
impl IngestionService {
    pub async fn restore_version(
        &self,
        actor: &Actor,
        dataset: &str,
        document: &str,
        version: i32,
        reason: Option<&str>,
    ) -> Result<Value> {
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        require_index_writable(&d)?;
        let mut lease = self.store.document_lease(dataset, document).await?;
        let fresh = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        let authoritative = KnowledgeWriteStore::dataset_on(lease.connection(), dataset).await?;
        if fresh["tenant_id"] != authoritative["tenant_id"] {
            return Err(IngestError::GenerationChanged);
        }
        require_index_writable(&authoritative)?;
        let typed: Dataset = serde_json::from_value(authoritative.clone())?;
        let mut tx = lease.connection().begin().await?;
        let doc = KnowledgeWriteStore::document_on(&mut tx, dataset, document).await?;
        let metadata = object(&doc["metadata"]);
        if doc["enabled"].as_bool() == Some(false)
            || doc["archived"].as_bool() == Some(true)
            || string(&doc, "status") != "completed"
            || metadata.get("_document_lifecycle_reindex").is_some()
        {
            return Err(IngestError::Validation(
                "Document is not active and restore-ready".into(),
            ));
        }
        let target:Value=sqlx::query_scalar("SELECT to_jsonb(v) FROM knowledge.document_versions v WHERE document_id=$1 AND version_number=$2 AND change_type NOT IN ('pending_restore','pending_before_restore')").bind(document).bind(version).fetch_optional(&mut *tx).await?.ok_or(IngestError::NotFound)?;
        let target_meta = object(&target["metadata"]);
        let source = object(&target_meta["_special_source_manifest"]);
        let kind = string(&source, "source_kind");
        let special = target_meta.get("_special_source_manifest").is_some()
            || target_meta["processing_mode"] == "scanned"
            || integer(&target_meta, "l1_segments") != 0;
        let content = string(&target, "content");
        let digest = sha256(content.as_bytes());
        if special {
            if !["text", "hierarchy", "vision"].contains(&kind)
                || string(&target, "content_hash") != digest
                || source["content_hash"] != digest
                || string(&source, "source_hash").len() != 64
                || !string(&source, "source_hash")
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                || !source["index_config"].is_object()
                || source["index_config"] != object(&authoritative["index_config"])
                || (["text", "hierarchy"].contains(&kind) && source["source_hash"] != digest)
                || (kind == "text"
                    && (source["objects"] != json!({}) || source["page_texts"] != json!([])))
                || (kind == "vision"
                    && (!string(&source, "original_source_key").starts_with(&format!(
                        "knowledge/documents/{}/{document}/original/",
                        typed.tenant_id
                    )) || !source["page_texts"]
                        .as_array()
                        .is_some_and(|p| !p.is_empty())))
            {
                return Err(IngestError::Validation(
                    "specialized historical source cannot be verified".into(),
                ));
            }
        } else {
            let specialized:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.segments WHERE dataset_id=$1 AND document_id=$2 AND (content_type<>'text' OR level IN (1,2)))").bind(dataset).bind(document).fetch_one(&mut *tx).await?;
            let points = self
                .indexer
                .vectors
                .document_point_ids_by_collection(&typed, document)
                .await?;
            let base = typed.collection_name.as_deref().unwrap_or("");
            if metadata["_special_source_manifest"].is_object()
                || metadata["images_embedded"] == true
                || specialized
                || points
                    .iter()
                    .any(|(name, ids)| name != base && !ids.is_empty())
            {
                return Err(IngestError::Validation(
                    "specialized historical source cannot be verified".into(),
                ));
            }
        }
        let before = KnowledgeWriteStore::create_version(
            &mut tx,
            document,
            string(&doc, "content"),
            "pending_before_restore",
            Some(string(&doc, "title")),
            &metadata,
            Some(&format!("Before restore to version {version}")),
            Some(&actor.user_id),
            false,
        )
        .await?;
        let previous = integer(&before, "version_number") as i32;
        KnowledgeWriteStore::update_status(&mut tx, document, "waiting", Some(0.0), Some(""))
            .await?;
        let reason = reason
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Restored from version {version}"));
        let candidate = KnowledgeWriteStore::create_version(
            &mut tx,
            document,
            content,
            "pending_restore",
            Some(target["title"].as_str().unwrap_or(string(&doc, "title"))),
            &target_meta,
            Some(&reason),
            Some(&actor.user_id),
            false,
        )
        .await?;
        let candidate_number = integer(&candidate, "version_number") as i32;
        if previous <= 0 || candidate_number <= previous {
            return Err(IngestError::GenerationChanged);
        }
        let index = if special {
            source["index_config"].clone()
        } else {
            object(&authoritative["index_config"])
        };
        ChunkingConfig::from_value(&index["chunking"])?;
        let mut snapshot = json!({"index_config":index,"chunking":object(&index["chunking"]),"processing_mode":if kind=="vision"{"scanned"}else{"text_only"},"restore_source_version":version});
        if special {
            snapshot["restore_source_kind"] = json!(kind);
            snapshot["restore_source_hash"] = source["source_hash"].clone()
        }
        let rule = KnowledgeWriteStore::record_process_rule(
            &mut tx,
            dataset,
            index["chunking"]["mode"].as_str().unwrap_or("automatic"),
            &snapshot,
            Some(&actor.user_id),
        )
        .await?;
        sqlx::query("UPDATE knowledge.documents SET process_rule_id=$2 WHERE document_id=$1")
            .bind(document)
            .bind(&rule)
            .execute(&mut *tx)
            .await?;
        let execution = KnowledgeWriteStore::record_execution(
            &mut tx,
            dataset,
            document,
            "reprocess",
            "api",
            Some(&actor.user_id),
            Some(&rule),
            &snapshot,
        )
        .await?;
        let bound = sqlx::query(sql::SET_PENDING_RESTORE_VERSION_0)
            .bind(document)
            .bind(dataset)
            .bind("_document_pending_restore_version")
            .bind(previous)
            .bind(candidate_number)
            .bind("_document_ingest_action")
            .bind("_document_pipeline_execution_id")
            .bind(&execution)
            .fetch_optional(&mut *tx)
            .await?;
        if bound.is_none() {
            return Err(IngestError::GenerationChanged);
        }
        tx.commit().await?;
        lease.release().await?;
        Ok(
            json!({"status":"pending","document_id":document,"restored_from_version":version,"new_version":candidate_number,"message":format!("Restore of version {version} was queued; the previous version remains available until indexing succeeds.")}),
        )
    }
    pub async fn prepare_text_restore(
        &self,
        dataset: &Dataset,
        document: &str,
        generation: &str,
        content: &str,
        title: &str,
        chunking: &Value,
    ) -> Result<CandidatePlan> {
        let generation = uuid::Uuid::parse_str(generation)
            .map_err(|_| {
                IngestError::Validation("text restore generation must be a durable UUID".into())
            })?
            .to_string();
        let config = ChunkingConfig::from_value(chunking)?;
        let mut rows = self.chunker.standard_chunks(
            content.trim(),
            &config,
            &dataset.dataset_id,
            document,
            title,
            "text",
        )?;
        if rows.is_empty() {
            return Err(IngestError::Validation(
                "text restore produced no candidate segments".into(),
            ));
        }
        let resolved = self.indexer.embeddings.resolve(dataset).await?;
        let texts: Vec<_> = rows.iter().map(|r| r.text.clone()).collect();
        let vectors = self
            .indexer
            .embeddings
            .embed_resolved(&resolved, &texts, "document")
            .await?;
        if rows.len() != vectors.len() {
            return Err(IngestError::Index(
                "text restore embedding batch incomplete".into(),
            ));
        }
        let mut points = vec![];
        for (position, (row, vector)) in rows.iter_mut().zip(vectors).enumerate() {
            if vector.len() != resolved.dimension || vector.iter().any(|v| !v.is_finite()) {
                return Err(IngestError::Index(
                    "text restore embedding vector invalid".into(),
                ));
            }
            let id = uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_URL,
                format!(
                    "ai-platform:kb-text-restore:{}:{document}:{generation}:{position}",
                    dataset.dataset_id
                )
                .as_bytes(),
            )
            .to_string();
            row.segment_id = id.clone();
            row.vector_id = Some(id.clone());
            row.enabled = false;
            row.status = "indexing".into();
            row.metadata["position"] = json!(position);
            let mut payload_meta = json!({});
            for key in [
                "source_type",
                "citation_text",
                "source_reference",
                "section_title",
                "section_full_path",
                "page_number",
                "chunk_index",
                "paragraph_index",
                "source_document",
                "document_title",
                "madhab",
                "language",
            ] {
                if let Some(v) = row.metadata.get(key).filter(|v| !v.is_null()) {
                    payload_meta[key] = v.clone()
                }
            }
            points.push(json!({"id":id,"vector":{"":vector},"payload":{"tenant_id":dataset.tenant_id,"dataset_id":dataset.dataset_id,"document_id":document,"segment_id":id,"position":position,"text":row.text,"token_count":row.token_count,"source_type":payload_meta.get("source_type").cloned().unwrap_or(json!("unknown")),"language":payload_meta.get("language").cloned().unwrap_or(json!("en")),"metadata":payload_meta,"citation_text":payload_meta["citation_text"],"source_reference":payload_meta["source_reference"],"content_type":"text","enabled":true}}));
            if let Some(original) = row
                .metadata
                .as_object_mut()
                .and_then(|m| m.remove("original_text"))
                .and_then(|v| v.as_str().map(str::to_owned))
            {
                row.text = original
            }
            let hash = sha256(row.text.as_bytes());
            row.content_hash = Some(hash.clone());
            row.index_node_hash = Some(hash);
        }
        let ids = rows.iter().map(|r| r.segment_id.clone()).collect();
        let collection = dataset.collection_name.clone().ok_or_else(|| {
            IngestError::Validation("text restore dataset collection missing".into())
        })?;
        Ok(CandidatePlan {
            generation_id: generation,
            document_id: document.into(),
            dataset_id: dataset.dataset_id.clone(),
            points_by_collection: BTreeMap::from([(collection, points)]),
            segment_rows: rows,
            content: content.into(),
            source_hash: sha256(content.as_bytes()),
            segment_ids: ids,
            source_kind: "text".into(),
            object_manifest: vec![],
            summary_row: None,
            l1_count: 0,
            l2_count: 0,
            l3_count: 0,
            total_pages: None,
            extracted_texts: BTreeMap::new(),
        })
    }
}
