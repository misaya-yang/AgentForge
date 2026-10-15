//! Manual segment edits and durable document visibility transitions.
use super::{
    Actor, Chunk, Dataset, IngestError, IngestionService, KnowledgeWriteStore, Result,
    dataset_ingestion_identity, display_status, integer, object, require_index_writable,
    require_public_metadata, sha256, string,
};
use serde_json::{Value, json};
use sqlx::PgConnection;
const LIFECYCLE: &str = "_document_lifecycle_reindex";
fn active(doc: &Value) -> Result<()> {
    let m = object(&doc["metadata"]);
    if doc["enabled"].as_bool() == Some(false)
        || doc["archived"].as_bool() == Some(true)
        || string(doc, "status") != "completed"
        || [
            LIFECYCLE,
            "_document_upload_generation",
            "_document_upload_failed",
            "_confluence_sync_generation",
        ]
        .iter()
        .any(|k| m.get(k).is_some())
    {
        return Err(IngestError::Validation(
            "manual segment indexing requires an active completed document".into(),
        ));
    }
    Ok(())
}
fn displayed(mut doc: Value) -> Value {
    doc["display_status"] = json!(display_status(&doc));
    doc
}
async fn segment(conn: &mut PgConnection, dataset: &str, id: &str) -> Result<Chunk> {
    let row: Value = sqlx::query_scalar(
        "SELECT to_jsonb(s) FROM knowledge.segments s WHERE dataset_id=$1 AND segment_id=$2",
    )
    .bind(dataset)
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or(IngestError::NotFound)?;
    Ok(serde_json::from_value(row)?)
}
async fn state(conn: &mut PgConnection, id: &str, value: &str, error: Option<&str>) -> Result<()> {
    if sqlx::query("UPDATE knowledge.segments SET status=$2,error=$3,updated_at=NOW() WHERE segment_id=$1 RETURNING segment_id").bind(id).bind(value).bind(error).fetch_optional(conn).await?.is_none(){return Err(IngestError::GenerationChanged)}
    Ok(())
}
async fn refresh_count(conn: &mut PgConnection, doc: &str) -> Result<()> {
    sqlx::query("UPDATE knowledge.documents SET segment_count=(SELECT COUNT(*) FROM knowledge.segments WHERE document_id=$1),updated_at=NOW() WHERE document_id=$1").bind(doc).execute(conn).await?;
    Ok(())
}
/// The set of mutable columns is fixed; the supplied values never enter SQL text.
pub(super) async fn document_fields(
    conn: &mut PgConnection,
    id: &str,
    fields: &Value,
    lifecycle: bool,
) -> Result<()> {
    let mut patches = vec![];
    let mut values = object(fields);
    for key in [
        "title",
        "enabled",
        "disabled_at",
        "disabled_by",
        "archived",
        "archived_reason",
        "archived_by",
        "archived_at",
        "doc_type",
        "doc_language",
        "segment_count",
    ] {
        if values.get(key).is_some() {
            patches.push(format!(
                "{key}=(jsonb_populate_record(NULL::knowledge.documents,$2::jsonb)).{key}"
            ));
        }
    }
    if let Some(metadata) = values.get("metadata").cloned() {
        let mut metadata = object(&metadata);
        metadata
            .as_object_mut()
            .unwrap()
            .retain(|k, _| !super::source_metadata_key(k) || (lifecycle && k == LIFECYCLE));
        values["metadata"] = metadata;
        patches.push("metadata=($2::jsonb->'metadata')||COALESCE((SELECT jsonb_object_agg(e.key,e.value) FROM jsonb_each(COALESCE(knowledge.documents.metadata,'{}'::jsonb)) e WHERE e.key=ANY($3::text[])),'{}'::jsonb)".into());
    }
    if patches.is_empty() {
        return Ok(());
    }
    let reserved: Vec<String> = super::SOURCE_METADATA_KEYS
        .iter()
        .filter(|k| !(lifecycle && **k == LIFECYCLE))
        .map(|s| s.to_string())
        .collect();
    let query = format!(
        "UPDATE knowledge.documents SET {},updated_at=NOW() WHERE document_id=$1",
        patches.join(",")
    );
    // Always reference the metadata key array so PostgreSQL can infer all parameters.
    let query = format!("{query} AND cardinality($3::text[])>=0");
    sqlx::query(&query)
        .bind(id)
        .bind(values)
        .bind(reserved)
        .execute(conn)
        .await?;
    Ok(())
}
impl IngestionService {
    pub async fn update_document(
        &self,
        actor: &Actor,
        dataset: &str,
        id: &str,
        patch: &Value,
    ) -> Result<Value> {
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        require_index_writable(&d)?;
        self.store.get_document(dataset, id).await?;
        let mut fields = json!({});
        for k in ["title", "metadata", "doc_type", "doc_language"] {
            if let Some(v) = patch.get(k) {
                if k == "metadata" {
                    require_public_metadata(v)?
                }
                fields[k] = v.clone();
            }
        }
        document_fields(&mut *self.store.pool.acquire().await?, id, &fields, false).await?;
        Ok(displayed(self.store.get_document(dataset, id).await?))
    }
    async fn publish_manual_segment(
        &self,
        dataset: &Dataset,
        seg: &Chunk,
        text: &str,
    ) -> Result<()> {
        let resolved = self.indexer.embeddings.resolve(dataset).await?;
        let vectors = self
            .indexer
            .embeddings
            .embed_resolved(&resolved, &[text.to_owned()], "document")
            .await?;
        let vector = vectors
            .first()
            .ok_or_else(|| IngestError::Index("embedding returned no vector".into()))?;
        self.indexer
            .vectors
            .ensure_collection(dataset, resolved.dimension)
            .await?;
        let m = object(&seg.metadata);
        let v = serde_json::to_value(seg)?;
        let mut payload = json!({"tenant_id":dataset.tenant_id,"dataset_id":seg.dataset_id,"document_id":seg.document_id,"segment_id":seg.segment_id,"position":seg.position,"text":text,"enabled":seg.enabled,"status":"completed","level":if seg.level==0{3}else{seg.level},"metadata":m,"parent_segment_id":seg.parent_segment_id,"token_count":seg.token_count});
        for (k, fallback) in [
            ("content_type", "text"),
            ("source_type", "unknown"),
            ("language", "en"),
        ] {
            payload[k] = v
                .get(k)
                .filter(|x| x.as_str().is_some_and(|s| !s.is_empty()))
                .or_else(|| m.get(k))
                .cloned()
                .unwrap_or_else(|| json!(fallback));
        }
        for k in [
            "source_reference",
            "citation_text",
            "page_number",
            "section_header",
        ] {
            payload[k] = v
                .get(k)
                .filter(|x| !x.is_null() && **x != json!({}))
                .or_else(|| m.get(k))
                .cloned()
                .unwrap_or(Value::Null);
        }
        let id = seg
            .vector_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&seg.segment_id);
        self.indexer
            .vectors
            .upsert_index_points(
                dataset,
                &[json!({"id":id,"vector":vector,"payload":payload})],
            )
            .await?;
        Ok(())
    }
    pub async fn update_segment(
        &self,
        actor: &Actor,
        dataset: &str,
        id: &str,
        text: &str,
        answer: Option<&str>,
        keywords: Option<&[String]>,
    ) -> Result<Value> {
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        require_index_writable(&d)?;
        let before = segment(&mut *self.store.pool.acquire().await?, dataset, id).await?;
        let mut lease = self
            .store
            .segment_lease(dataset, &before.document_id, id)
            .await?;
        let current = KnowledgeWriteStore::require_identity(
            lease.connection(),
            dataset,
            Some(&dataset_ingestion_identity(&d)),
        )
        .await?;
        let mut seg = segment(lease.connection(), dataset, id).await?;
        active(
            &KnowledgeWriteStore::document_on(lease.connection(), dataset, &seg.document_id)
                .await?,
        )?;
        let clean = super::parser::sanitize_text(text)?;
        let answer = answer.map(super::parser::sanitize_text).transpose()?;
        let mut metadata = object(&seg.metadata);
        metadata.as_object_mut().unwrap().remove("source_version");
        metadata.as_object_mut().unwrap().remove("source_hash");
        metadata["manual_source_override"] = json!(true);
        seg.metadata = metadata.clone();
        state(lease.connection(), id, "indexing", None).await?;
        let result:Result<()>=async {sqlx::query("UPDATE knowledge.segments SET text=$2,answer=COALESCE($3,answer),keywords=COALESCE($4::jsonb,keywords),content_hash=$5,metadata=$6,updated_at=NOW() WHERE segment_id=$1").bind(id).bind(&clean).bind(answer).bind(keywords.map(|k|json!(k))).bind(sha256(clean.as_bytes())).bind(metadata).execute(lease.connection()).await?;self.publish_manual_segment(&serde_json::from_value(current)?,&seg,&clean).await?;state(lease.connection(),id,"completed",None).await}.await;
        if result.is_err() {
            state(
                lease.connection(),
                id,
                "error",
                Some("vector update failed"),
            )
            .await?;
            return Err(IngestError::Validation(
                "segment vector update failed; the segment remains hidden until retry".into(),
            ));
        }
        let result = serde_json::to_value(segment(lease.connection(), dataset, id).await?)?;
        lease.release().await?;
        Ok(result)
    }
    pub async fn create_segment(
        &self,
        actor: &Actor,
        dataset: &str,
        document: &str,
        text: &str,
        answer: Option<&str>,
        keywords: Option<&[String]>,
    ) -> Result<Value> {
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        require_index_writable(&d)?;
        let mut lease = self.store.document_lease(dataset, document).await?;
        let current = KnowledgeWriteStore::require_identity(
            lease.connection(),
            dataset,
            Some(&dataset_ingestion_identity(&d)),
        )
        .await?;
        active(&KnowledgeWriteStore::document_on(lease.connection(), dataset, document).await?)?;
        let position:i64=sqlx::query_scalar("SELECT COALESCE(MAX(position),-1)+1 FROM knowledge.segments WHERE dataset_id=$1 AND document_id=$2").bind(dataset).bind(document).fetch_one(lease.connection()).await?;
        let clean = super::parser::sanitize_text(text)?;
        let id = uuid::Uuid::new_v4().to_string();
        let seg: Chunk = serde_json::from_value(
            json!({"segment_id":id,"dataset_id":dataset,"document_id":document,"position":position,"level":3,"text":clean,"token_count":clean.chars().count()/4,"word_count":clean.split_whitespace().count(),"answer":answer,"keywords":keywords.unwrap_or(&[]),"created_by":actor.user_id,"enabled":true,"status":"waiting","source_type":"manual","language":"en","metadata":{"content_type":"text"}}),
        )?;
        KnowledgeWriteStore::insert_segments(lease.connection(), &[seg.clone()]).await?;
        state(lease.connection(), &id, "indexing", None).await?;
        let result: Result<()> = async {
            self.publish_manual_segment(&serde_json::from_value(current)?, &seg, &clean)
                .await?;
            sqlx::query("UPDATE knowledge.segments SET vector_id=$1 WHERE segment_id=$1")
                .bind(&id)
                .execute(lease.connection())
                .await?;
            state(lease.connection(), &id, "completed", None).await
        }
        .await;
        if result.is_err() {
            state(
                lease.connection(),
                &id,
                "error",
                Some("vector creation failed"),
            )
            .await?;
            return Err(IngestError::Validation(
                "segment vector creation failed; the segment remains hidden until retry".into(),
            ));
        }
        refresh_count(lease.connection(), document).await?;
        let result = serde_json::to_value(segment(lease.connection(), dataset, &id).await?)?;
        lease.release().await?;
        Ok(result)
    }
    pub async fn set_segment_enabled(
        &self,
        actor: &Actor,
        dataset: &str,
        id: &str,
        enabled: bool,
    ) -> Result<Value> {
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        require_index_writable(&d)?;
        let before = segment(&mut *self.store.pool.acquire().await?, dataset, id).await?;
        let mut lease = self
            .store
            .segment_lease(dataset, &before.document_id, id)
            .await?;
        let current = KnowledgeWriteStore::require_identity(
            lease.connection(),
            dataset,
            Some(&dataset_ingestion_identity(&d)),
        )
        .await?;
        let seg = segment(lease.connection(), dataset, id).await?;
        active(
            &KnowledgeWriteStore::document_on(lease.connection(), dataset, &seg.document_id)
                .await?,
        )?;
        let typed: Dataset = serde_json::from_value(current)?;
        if !enabled {
            sqlx::query("UPDATE knowledge.segments SET enabled=FALSE,disabled_at=NOW(),disabled_by=$2,updated_at=NOW() WHERE segment_id=$1").bind(id).bind(&actor.user_id).execute(lease.connection()).await?;
        }
        self.indexer
            .vectors
            .set_segment_payload_enabled(&typed, &seg.document_id, id, enabled)
            .await?;
        if enabled {
            sqlx::query("UPDATE knowledge.segments SET enabled=TRUE,disabled_at=NULL,disabled_by=NULL,updated_at=NOW() WHERE segment_id=$1").bind(id).execute(lease.connection()).await?;
        }
        let result = serde_json::to_value(segment(lease.connection(), dataset, id).await?)?;
        lease.release().await?;
        Ok(result)
    }
    pub async fn set_segments_enabled_batch(
        &self,
        actor: &Actor,
        dataset: &str,
        ids: &[String],
        enabled: bool,
    ) -> Result<Value> {
        if ids.is_empty()
            || ids.len() > 500
            || ids
                .iter()
                .any(|id| id.trim().is_empty() || id.trim().chars().count() > 256)
        {
            return Err(IngestError::Validation(
                "segment_ids must contain 1-500 IDs of 1-256 characters".into(),
            ));
        }
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        require_index_writable(&d)?;
        let mut updated = 0;
        for id in ids {
            if self
                .set_segment_enabled(actor, dataset, id.trim(), enabled)
                .await
                .is_ok()
            {
                updated += 1
            }
        }
        Ok(json!({"success":true,"updated":updated,"total":ids.len()}))
    }
    pub async fn delete_segment(&self, actor: &Actor, dataset: &str, id: &str) -> Result<bool> {
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        let expected =
            json!({"operation":"segment_delete","target_id":id,"status":"pending","version":1});
        let fence = &d["index_config"]["retrieval"]["_index_deletion_fence"];
        if !fence.is_null() && *fence != expected {
            return Err(IngestError::GenerationChanged);
        }
        let mut lease = self.store.dataset_delete_lease(dataset).await?;
        let current = KnowledgeWriteStore::dataset_on(lease.connection(), dataset).await?;
        if string(&current, "tenant_id") != string(&d, "tenant_id") {
            return Err(IngestError::GenerationChanged);
        }
        let seg = match segment(lease.connection(), dataset, id).await {
            Ok(s) => s,
            Err(IngestError::NotFound) => {
                if current["index_config"]["retrieval"]["_index_deletion_fence"] == expected {
                    KnowledgeWriteStore::clear_deletion_fence(
                        lease.connection(),
                        dataset,
                        "segment_delete",
                        id,
                    )
                    .await?;
                    lease.release().await?;
                    return Ok(true);
                }
                lease.release().await?;
                return Ok(false);
            }
            Err(e) => return Err(e),
        };
        active(
            &KnowledgeWriteStore::document_on(lease.connection(), dataset, &seg.document_id)
                .await?,
        )?;
        if seg.content_type == "image" || seg.metadata["content_type"] == "image" {
            return Err(IngestError::Validation(
                "image segment deletion requires an atomic durable object receipt".into(),
            ));
        }
        let fenced = KnowledgeWriteStore::set_deletion_fence(
            lease.connection(),
            dataset,
            "segment_delete",
            id,
        )
        .await?;
        self.indexer
            .vectors
            .delete_segment_points(
                &serde_json::from_value(fenced)?,
                &seg.document_id,
                seg.vector_id.as_deref().unwrap_or(id),
            )
            .await?;
        let deleted =
            sqlx::query("DELETE FROM knowledge.segments WHERE segment_id=$1 AND dataset_id=$2")
                .bind(id)
                .bind(dataset)
                .execute(lease.connection())
                .await?
                .rows_affected()
                > 0;
        if !KnowledgeWriteStore::clear_deletion_fence(
            lease.connection(),
            dataset,
            "segment_delete",
            id,
        )
        .await?
        {
            return Err(IngestError::GenerationChanged);
        }
        let _ = refresh_count(lease.connection(), &seg.document_id).await;
        lease.release().await?;
        Ok(deleted)
    }
    pub async fn set_document_enabled(
        &self,
        actor: &Actor,
        dataset: &str,
        id: &str,
        enabled: bool,
    ) -> Result<Value> {
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        require_index_writable(&d)?;
        let doc = self.store.get_document(dataset, id).await?;
        let updates = json!({"enabled":enabled,"disabled_by":if enabled{Value::Null}else{json!(actor.user_id)},"disabled_at":if enabled{Value::Null}else{json!(chrono::Utc::now().to_rfc3339())}});
        self.transition_document(
            actor,
            &d,
            id,
            enabled,
            doc["archived"].as_bool().unwrap_or(false),
            updates,
            if enabled { "enable" } else { "disable" },
        )
        .await
    }
    pub async fn set_document_archived(
        &self,
        actor: &Actor,
        dataset: &str,
        id: &str,
        archived: bool,
        reason: Option<&str>,
    ) -> Result<Value> {
        let d = self
            .store
            .require_dataset_access(actor, dataset, "editor")
            .await?;
        require_index_writable(&d)?;
        let doc = self.store.get_document(dataset, id).await?;
        let updates = json!({"archived":archived,"archived_by":if archived{json!(actor.user_id)}else{Value::Null},"archived_at":if archived{json!(chrono::Utc::now().to_rfc3339())}else{Value::Null},"archived_reason":if archived{json!(reason)}else{Value::Null}});
        self.transition_document(
            actor,
            &d,
            id,
            doc["enabled"].as_bool().unwrap_or(true),
            archived,
            updates,
            if archived { "archive" } else { "unarchive" },
        )
        .await
    }
    async fn transition_document(
        &self,
        actor: &Actor,
        dataset: &Value,
        id: &str,
        enabled: bool,
        archived: bool,
        mut updates: Value,
        action: &str,
    ) -> Result<Value> {
        let dataset_id = string(dataset, "dataset_id");
        let mut lease = self.store.dataset_delete_lease(dataset_id).await?;
        let d = KnowledgeWriteStore::dataset_on(lease.connection(), dataset_id).await?;
        require_index_writable(&d)?;
        if string(&d, "tenant_id").is_empty()
            || string(&d, "tenant_id") != string(dataset, "tenant_id")
        {
            return Err(IngestError::GenerationChanged);
        }
        let typed: Dataset = serde_json::from_value(d)?;
        let doc = KnowledgeWriteStore::document_on(lease.connection(), dataset_id, id).await?;
        let mut metadata = object(&doc["metadata"]);
        let pending = object(&metadata[LIFECYCLE]);
        let has_pending = !pending.as_object().unwrap().is_empty();
        let desired_active = enabled && !archived;
        if has_pending {
            let status = string(&pending, "status");
            if !["pending", "deactivating"].contains(&status) {
                return Err(IngestError::Validation(
                    "document lifecycle marker is malformed".into(),
                ));
            }
            let target = if pending.get("desired_enabled").is_some()
                && pending.get("desired_archived").is_some()
            {
                pending["desired_enabled"].as_bool() == Some(enabled)
                    && pending["desired_archived"].as_bool() == Some(archived)
            } else {
                (status == "pending") == desired_active
            };
            if !target {
                return Err(IngestError::Validation(
                    "a different document lifecycle transition is pending".into(),
                ));
            }
        }
        if desired_active
            && doc["enabled"].as_bool().unwrap_or(true)
            && !doc["archived"].as_bool().unwrap_or(false)
            && !has_pending
        {
            document_fields(lease.connection(), id, &updates, false).await?;
        } else if !desired_active {
            if !has_pending {
                metadata[LIFECYCLE] = json!({"status":"deactivating","desired_enabled":enabled,"desired_archived":archived,"requested_action":action,"requested_at":chrono::Utc::now().to_rfc3339(),"requested_by":actor.user_id})
            }
            updates["metadata"] = metadata;
            document_fields(lease.connection(), id, &updates, true).await?;
            self.indexer.vectors.delete_document_points(&typed,id).await.map_err(|_|IngestError::Validation("document vectors could not be fully deactivated; retry the same transition".into()))?;
            sqlx::query("UPDATE knowledge.datasets SET content_revision=COALESCE(content_revision,0)+1,updated_at=NOW() WHERE dataset_id=$1 AND is_deleted=FALSE").bind(dataset_id).execute(lease.connection()).await?;
            if sqlx::query("UPDATE knowledge.documents SET metadata=COALESCE(metadata,'{}'::jsonb)-'_document_lifecycle_reindex',updated_at=NOW() WHERE document_id=$1 AND metadata->'_document_lifecycle_reindex'->>'status'='deactivating' RETURNING document_id").bind(id).fetch_optional(lease.connection()).await?.is_none(){return Err(IngestError::GenerationChanged)}
        } else {
            let terminal = ["error", "completed"].contains(&string(&doc, "status"));
            let stale = chrono::DateTime::parse_from_rfc3339(string(&doc, "updated_at"))
                .map(|t| chrono::Utc::now().signed_duration_since(t).num_minutes() >= 30)
                .unwrap_or(true);
            if has_pending && !terminal && !stale {
                lease.release().await?;
                return Ok(displayed(doc));
            }
            if !has_pending || terminal {
                if !has_pending {
                    metadata[LIFECYCLE] = json!({"status":"pending","desired_enabled":true,"desired_archived":false,"requested_action":action,"requested_at":chrono::Utc::now().to_rfc3339(),"requested_by":actor.user_id});
                    document_fields(lease.connection(), id, &json!({"metadata":metadata}), true)
                        .await?;
                }
                let hot = !["multimodal", "scanned"]
                    .contains(&string(&metadata, "processing_mode"))
                    && metadata.get("images_embedded").is_none()
                    && metadata.get("embedded_image_count").is_none()
                    && integer(&metadata, "image_count") <= 0;
                if !hot {
                    sqlx::query("UPDATE knowledge.documents SET metadata=COALESCE(metadata,'{}'::jsonb)-'images_embedded'-'embedded_image_count',updated_at=NOW() WHERE document_id=$1 AND dataset_id=$2").bind(id).bind(dataset_id).execute(lease.connection()).await?;
                }
                self.indexer
                    .vectors
                    .delete_document_points(&typed, id)
                    .await
                    .map_err(|_| {
                        IngestError::Validation(
                            "document restore cleanup failed; the document remains hidden".into(),
                        )
                    })?;
                if hot {
                    sqlx::query("UPDATE knowledge.documents SET metadata=jsonb_set(COALESCE(metadata,'{}'::jsonb),'{_document_ingest_action}','\"reembed\"'::jsonb,TRUE),updated_at=NOW() WHERE document_id=$1 AND dataset_id=$2").bind(id).bind(dataset_id).execute(lease.connection()).await?;
                } else {
                    sqlx::query("DELETE FROM knowledge.segments WHERE document_id=$1")
                        .bind(id)
                        .execute(lease.connection())
                        .await?;
                    document_fields(lease.connection(), id, &json!({"segment_count":0}), false)
                        .await?;
                }
            }
            KnowledgeWriteStore::update_status(
                lease.connection(),
                id,
                "waiting",
                Some(0.0),
                Some(""),
            )
            .await?;
        }
        let result =
            displayed(KnowledgeWriteStore::document_on(lease.connection(), dataset_id, id).await?);
        lease.release().await?;
        Ok(result)
    }
}
