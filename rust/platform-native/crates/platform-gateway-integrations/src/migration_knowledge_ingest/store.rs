//! Existing PostgreSQL schema, durable claims, versions and cross-replica leases.
use super::{
    Actor, Chunk, Dataset, IngestError, Result, canonical_json, dataset_ingestion_identity,
    integer, object, require_index_writable, sha256, sql, string,
};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Postgres, pool::PoolConnection};
use std::collections::{BTreeMap, BTreeSet};

fn source_version_matches(row: &Value, content: &str) -> bool {
    !string(row, "change_type").starts_with("pending_")
        && string(row, "content") == content
        && string(row, "content_hash") == sha256(content.as_bytes())
}

pub(crate) fn dataset_permission_short_circuit(actor: &Actor, dataset: &Value) -> Option<i32> {
    if dataset["is_archived"].as_bool() == Some(true) {
        return Some(0);
    }
    let visibility = string(dataset, "visibility").to_lowercase();
    if actor.anonymous() {
        return Some(if visibility == "public" { 1 } else { 0 });
    }
    let tenant = string(dataset, "tenant_id").trim();
    let same = !tenant.is_empty()
        && !actor.tenant_id.trim().is_empty()
        && tenant == actor.tenant_id.trim();
    if (same && (actor.user_tier == "admin" || actor.roles.iter().any(|r| r == "admin")))
        || (!actor.user_id.is_empty() && string(dataset, "created_by") == actor.user_id)
    {
        return Some(3);
    }
    None
}

fn same_dataset_tenant(actor: &Actor, dataset: &Value) -> bool {
    let tenant = string(dataset, "tenant_id").trim();
    !tenant.is_empty() && !actor.tenant_id.trim().is_empty() && tenant == actor.tenant_id.trim()
}

pub(crate) fn dataset_permission_from_grants(
    actor: &Actor,
    dataset: &Value,
    permissions: &[String],
) -> i32 {
    if let Some(permission) = dataset_permission_short_circuit(actor, dataset) {
        return permission;
    }
    let explicit = permissions.iter().map(|s| rank(s)).max().unwrap_or(0);
    if permissions.iter().any(|permission| !permission.is_empty()) {
        explicit
    } else {
        let visibility = string(dataset, "visibility").to_lowercase();
        if visibility == "public" || (same_dataset_tenant(actor, dataset) && visibility == "tenant")
        {
            1
        } else {
            0
        }
    }
}

fn catalog_permission_ids<'a>(
    actor: &Actor,
    datasets: &'a [Value],
) -> Result<(Vec<&'a str>, Vec<&'a str>)> {
    if datasets.len() > 200 {
        return Err(IngestError::Validation(
            "dataset permission page exceeds 200 rows".into(),
        ));
    }
    let mut ids = Vec::new();
    let mut role_ids = Vec::new();
    for dataset in datasets {
        if dataset_permission_short_circuit(actor, dataset).is_some() {
            continue;
        }
        let id = string(dataset, "dataset_id");
        ids.push(id);
        if same_dataset_tenant(actor, dataset) {
            role_ids.push(id);
        }
    }
    Ok((ids, role_ids))
}

#[derive(Clone)]
pub struct KnowledgeWriteStore {
    pub pool: PgPool,
}
impl KnowledgeWriteStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn get_dataset(&self, id: &str) -> Result<Value> {
        Self::dataset_on(&mut *self.pool.acquire().await?, id).await
    }
    pub async fn dataset_on(conn: &mut PgConnection, id: &str) -> Result<Value> {
        sqlx::query_scalar(
            "SELECT to_jsonb(d) FROM knowledge.datasets d WHERE dataset_id=$1 AND is_deleted=FALSE",
        )
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or(IngestError::NotFound)
    }
    pub async fn document_on(conn: &mut PgConnection, dataset_id: &str, id: &str) -> Result<Value> {
        sqlx::query_scalar(
            "SELECT to_jsonb(d) FROM knowledge.documents d WHERE document_id=$1 AND dataset_id=$2",
        )
        .bind(id)
        .bind(dataset_id)
        .fetch_optional(conn)
        .await?
        .ok_or(IngestError::NotFound)
    }
    pub async fn get_document(&self, dataset_id: &str, id: &str) -> Result<Value> {
        Self::document_on(&mut *self.pool.acquire().await?, dataset_id, id).await
    }
    pub async fn require_dataset_access(
        &self,
        actor: &Actor,
        id: &str,
        required: &str,
    ) -> Result<Value> {
        let dataset = self.get_dataset(id).await?;
        if self.effective_dataset_permission(actor, &dataset).await? < rank(required) {
            return Err(IngestError::Permission);
        }
        Ok(dataset)
    }
    pub(crate) async fn effective_dataset_permission(
        &self,
        actor: &Actor,
        dataset: &Value,
    ) -> Result<i32> {
        if let Some(permission) = dataset_permission_short_circuit(actor, dataset) {
            return Ok(permission);
        }
        let same = same_dataset_tenant(actor, dataset);
        let permissions: Vec<String> = sqlx::query_scalar("SELECT permission FROM knowledge.dataset_permissions WHERE dataset_id=$1 AND ((subject_type='user' AND subject_id=$2) OR (subject_type='tenant_role' AND subject_id=ANY($3::text[])))")
            .bind(string(dataset,"dataset_id")).bind(&actor.user_id).bind(if same {actor.roles.clone()} else {vec![]}).fetch_all(&self.pool).await?;
        Ok(dataset_permission_from_grants(actor, dataset, &permissions))
    }
    pub(crate) async fn catalog_permissions_batch(
        &self,
        actor: &Actor,
        datasets: &[Value],
    ) -> Result<BTreeMap<String, Vec<String>>> {
        let (ids, role_ids) = catalog_permission_ids(actor, datasets)?;
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        // One statement snapshot per bounded catalog page; no cross-request cache.
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT dataset_id, permission FROM knowledge.dataset_permissions WHERE dataset_id=ANY($1::text[]) AND ((subject_type='user' AND subject_id=$2) OR (subject_type='tenant_role' AND dataset_id=ANY($3::text[]) AND subject_id=ANY($4::text[])))"
        ).bind(&ids).bind(&actor.user_id).bind(&role_ids).bind(&actor.roles).fetch_all(&self.pool).await?;
        let mut grants: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (id, permission) in rows {
            grants.entry(id).or_default().push(permission);
        }
        Ok(grants)
    }
    pub async fn require_identity(
        conn: &mut PgConnection,
        id: &str,
        expected: Option<&str>,
    ) -> Result<Value> {
        let dataset = Self::dataset_on(conn, id).await?;
        require_index_writable(&dataset)?;
        if dataset["is_archived"].as_bool() == Some(true) {
            return Err(IngestError::GenerationChanged);
        }
        if expected.is_some_and(|identity| identity != dataset_ingestion_identity(&dataset)) {
            return Err(IngestError::GenerationChanged);
        }
        Ok(dataset)
    }
    pub async fn create_dataset_with_owner(&self, d: &Value, owner: &str) -> Result<bool> {
        if owner.is_empty() {
            return Err(IngestError::Validation("owner_user_id is required".into()));
        }
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(sql::INSERT_DATASET_ROW_0)
            .bind(string(d, "dataset_id"))
            .bind(string(d, "name"))
            .bind(string(d, "description"))
            .bind(string(d, "tenant_id"))
            .bind(string(d, "visibility"))
            .bind(string(d, "embedding_provider"))
            .bind(string(d, "embedding_model"))
            .bind(integer(d, "embedding_dimension") as i32)
            .bind(object(&d["embedding_config"]))
            .bind(object(&d["index_config"]))
            .bind(d["collection_name"].as_str())
            .bind(false)
            .bind(Option::<chrono::NaiveDateTime>::None)
            .bind(Option::<String>::None)
            .bind(Option::<String>::None)
            .bind(owner)
            .execute(&mut *tx)
            .await;
        if let Err(e) = result {
            if e.as_database_error()
                .and_then(|e| e.code())
                .is_some_and(|c| c == "23505")
            {
                return Ok(false);
            }
            return Err(e.into());
        }
        sqlx::query("INSERT INTO knowledge.dataset_permissions (dataset_id,subject_type,subject_id,permission) VALUES ($1,'user',$2,'owner')").bind(string(d,"dataset_id")).bind(owner).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(true)
    }
    pub async fn dataset_exists(&self, id: &str) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM knowledge.datasets WHERE dataset_id=$1)",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?)
    }
    pub async fn collection_in_use(&self, name: &str) -> Result<bool> {
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.datasets WHERE collection_name=$1 AND is_deleted=FALSE)").bind(name).fetch_one(&self.pool).await?)
    }
    pub async fn insert_document(&self, d: &Value, expected: &str) -> Result<()> {
        let dataset = string(d, "dataset_id");
        if dataset.is_empty() {
            return Err(IngestError::Validation(
                "dataset_id is required for fenced document insert".into(),
            ));
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock_shared(hashtextextended($1,0))")
            .bind(format!("knowledge-dataset-index:{dataset}"))
            .execute(&mut *tx)
            .await?;
        Self::require_identity(&mut tx, dataset, Some(expected)).await?;
        Self::insert_document_on(&mut tx, d).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn insert_document_on(conn: &mut PgConnection, d: &Value) -> Result<()> {
        sqlx::query(sql::INSERT_DOCUMENT_ROW_0)
            .bind(string(d, "document_id"))
            .bind(string(d, "dataset_id"))
            .bind(string(d, "title"))
            .bind(string(d, "source_type"))
            .bind(d["source_uri"].as_str())
            .bind(d["mime_type"].as_str())
            .bind(integer(d, "size_bytes"))
            .bind(string(d, "status"))
            .bind(d["progress"].as_f64().unwrap_or(0.0))
            .bind(d["error"].as_str())
            .bind(d["content"].as_str())
            .bind(object(&d["metadata"]))
            .bind(Option::<chrono::NaiveDateTime>::None)
            .bind(Option::<chrono::NaiveDateTime>::None)
            .execute(conn)
            .await?;
        Ok(())
    }
    pub async fn finalize_upload(
        conn: &mut PgConnection,
        d: &Value,
        generation: &str,
        expected: &str,
    ) -> Result<bool> {
        Self::require_identity(conn, string(d, "dataset_id"), Some(expected)).await?;
        let mut metadata = object(&d["metadata"]);
        metadata
            .as_object_mut()
            .unwrap()
            .remove("_document_upload_generation");
        Ok(sqlx::query(sql::FINALIZE_DOCUMENT_UPLOAD_0)
            .bind(string(d, "document_id"))
            .bind(string(d, "dataset_id"))
            .bind(string(d, "title"))
            .bind(string(d, "source_type"))
            .bind(d["source_uri"].as_str())
            .bind(d["mime_type"].as_str())
            .bind(integer(d, "size_bytes"))
            .bind(string(d, "status"))
            .bind(d["progress"].as_f64().unwrap_or(0.0))
            .bind(d["error"].as_str())
            .bind(d["content"].as_str())
            .bind(metadata)
            .bind(Option::<chrono::NaiveDateTime>::None)
            .bind(Option::<chrono::NaiveDateTime>::None)
            .bind("_document_upload_generation")
            .bind(generation)
            .fetch_optional(conn)
            .await?
            .is_some())
    }
    pub async fn document_lease(&self, dataset: &str, document: &str) -> Result<DocumentLease> {
        if dataset.trim().is_empty() || document.trim().is_empty() {
            return Err(IngestError::Validation(
                "dataset_id and document_id are required for document locking".into(),
            ));
        }
        let mut lease = DocumentLease {
            connection: Some(self.pool.acquire().await?),
            locks: vec![],
            dataset_id: dataset.into(),
            document_id: document.into(),
        };
        lease
            .lock(format!("knowledge-dataset-index:{dataset}"), true)
            .await?;
        lease
            .lock(
                format!("knowledge-document-index:{dataset}:{document}"),
                false,
            )
            .await?;
        Ok(lease)
    }
    pub async fn segment_lease(
        &self,
        dataset: &str,
        document: &str,
        segment: &str,
    ) -> Result<DocumentLease> {
        let mut lease = self.document_lease(dataset, document).await?;
        lease
            .lock(
                format!("knowledge-segment-index:{dataset}:{segment}"),
                false,
            )
            .await?;
        Ok(lease)
    }
    pub async fn enqueue(
        &self,
        dataset: &str,
        document: &str,
        action: Option<&str>,
        recover_stage: Option<&str>,
        execution: Option<&str>,
        pin_rule: bool,
    ) -> Result<bool> {
        let mut lease = self.document_lease(dataset, document).await?;
        let result = Self::enqueue_on(
            lease.connection(),
            dataset,
            document,
            action,
            recover_stage,
            execution,
            pin_rule,
        )
        .await;
        let released = lease.release().await;
        released?;
        result
    }
    pub async fn enqueue_on(
        conn: &mut PgConnection,
        dataset: &str,
        document: &str,
        action: Option<&str>,
        recover_stage: Option<&str>,
        execution: Option<&str>,
        pin_rule: bool,
    ) -> Result<bool> {
        if action
            .is_some_and(|a| !["ingest", "reprocess", "reembed", "recover", "retry"].contains(&a))
        {
            return Err(IngestError::Validation("unsupported ingest action".into()));
        }
        if recover_stage.is_some_and(|a| !["parsing", "splitting", "indexing"].contains(&a))
            || (recover_stage.is_some() && action != Some("recover"))
        {
            return Err(IngestError::Validation(
                "recover_stage is only valid with the recover action".into(),
            ));
        }
        if pin_rule
            && (!matches!(action, Some("reprocess" | "recover" | "retry")) || execution.is_none())
        {
            return Err(IngestError::Validation(
                "replay rule pin requires a replay execution".into(),
            ));
        }
        Self::require_identity(conn, dataset, None).await?;
        let patch = if action.is_none() || action == Some("ingest") {
            None
        } else {
            let mut p = json!({"_document_ingest_action":action});
            if let Some(s) = recover_stage {
                p["_document_recover_stage"] = json!(s)
            }
            if let Some(e) = execution {
                p["_document_pipeline_execution_id"] = json!(e)
            }
            Some(p)
        };
        Ok(sqlx::query(sql::CLAIM_DOCUMENT_FOR_ENQUEUE_0)
            .bind(document)
            .bind(dataset)
            .bind(patch)
            .bind(action.unwrap_or(""))
            .bind(pin_rule)
            .bind(execution)
            .fetch_optional(conn)
            .await?
            .is_some())
    }
    pub async fn list_queued(&self, limit: i64, cursor: &str) -> Result<Vec<Value>> {
        let query = format!(
            "SELECT to_jsonb(q) FROM ({}) q",
            sql::LIST_QUEUED_DOCUMENTS_0
        );
        Ok(sqlx::query_scalar(&query)
            .bind(limit.clamp(1, 1000))
            .bind(cursor)
            .bind(300.0f64)
            .bind(vec!["recover", "reembed", "reprocess", "retry"])
            .fetch_all(&self.pool)
            .await?)
    }
    pub async fn claim_processing(
        conn: &mut PgConnection,
        dataset: &str,
        document: &str,
    ) -> Result<bool> {
        Self::require_identity(conn, dataset, None).await?;
        Ok(sqlx::query(sql::CLAIM_QUEUED_DOCUMENT_FOR_PROCESSING_0)
            .bind(document)
            .bind(dataset)
            .fetch_optional(conn)
            .await?
            .is_some())
    }
    pub async fn requeue_cancelled(
        conn: &mut PgConnection,
        dataset: &str,
        document: &str,
    ) -> Result<bool> {
        Ok(sqlx::query(sql::REQUEUE_CANCELLED_DOCUMENT_GENERATION_0)
            .bind(document)
            .bind(dataset)
            .fetch_optional(conn)
            .await?
            .is_some())
    }
    pub(crate) async fn requeue_unpublished_generation(
        conn: &mut PgConnection,
        dataset: &str,
        document: &str,
        execution: &str,
    ) -> Result<bool> {
        Ok(sqlx::query(sql::REQUEUE_UNPUBLISHED_DOCUMENT_GENERATION_0)
            .bind(document)
            .bind(dataset)
            .bind(execution)
            .fetch_optional(conn)
            .await?
            .is_some())
    }
    pub async fn update_status(
        conn: &mut PgConnection,
        document: &str,
        status: &str,
        progress: Option<f64>,
        error: Option<&str>,
    ) -> Result<()> {
        let stage = match status {
            "parsing" => Some("parsing_started_at"),
            "splitting" => Some("splitting_started_at"),
            "indexing" => Some("indexing_started_at"),
            _ => None,
        };
        let mut query="UPDATE knowledge.documents SET status=$2,updated_at=NOW(),progress=COALESCE($3,progress),error=COALESCE($4,error)".to_owned();
        if let Some(stage) = stage {
            query.push_str(&format!(
                ",started_at=COALESCE(started_at,NOW()),{stage}=NOW()"
            ))
        }
        if status == "completed" {
            let pending = "COALESCE(metadata,'{}'::jsonb)->'_document_lifecycle_reindex'->>'status'='pending'";
            query.push_str(&format!(",completed_at=NOW(),enabled=CASE WHEN {pending} THEN TRUE ELSE enabled END,disabled_at=CASE WHEN {pending} THEN NULL ELSE disabled_at END,disabled_by=CASE WHEN {pending} THEN NULL ELSE disabled_by END,archived=CASE WHEN {pending} THEN FALSE ELSE archived END,archived_at=CASE WHEN {pending} THEN NULL ELSE archived_at END,archived_by=CASE WHEN {pending} THEN NULL ELSE archived_by END,archived_reason=CASE WHEN {pending} THEN NULL ELSE archived_reason END,metadata=(CASE WHEN {pending} THEN COALESCE(metadata,'{{}}'::jsonb)-'_document_lifecycle_reindex' ELSE COALESCE(metadata,'{{}}'::jsonb) END)-'_document_ingest_action'-'_document_recover_stage'-'_document_pipeline_execution_id'"));
            // Activation and authoritative cache invalidation share one SQL statement.
            query = format!(
                "WITH pending_restore AS (SELECT dataset_id FROM knowledge.documents WHERE document_id=$1 AND {pending}),revision_bump AS (UPDATE knowledge.datasets SET content_revision=COALESCE(content_revision,0)+1 WHERE dataset_id IN (SELECT dataset_id FROM pending_restore) AND is_deleted=FALSE RETURNING dataset_id) {query}"
            );
            query = query.replace(
                "error=COALESCE($4,error)",
                &format!("error=CASE WHEN {pending} THEN NULL ELSE COALESCE($4,error) END"),
            );
        } else if status == "error" {
            query.push_str(",completed_at=NOW(),metadata=COALESCE(metadata,'{}'::jsonb)-'_document_ingest_action'-'_document_recover_stage'-'_document_pipeline_execution_id'");
        }
        query.push_str(" WHERE document_id=$1");
        sqlx::query(&query)
            .bind(document)
            .bind(status)
            .bind(progress)
            .bind(error)
            .execute(conn)
            .await?;
        Ok(())
    }
    pub async fn list_documents(
        &self,
        dataset: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Value>> {
        Ok(sqlx::query_scalar("SELECT to_jsonb(d) FROM knowledge.documents d WHERE dataset_id=$1 ORDER BY created_at DESC LIMIT $2 OFFSET $3").bind(dataset).bind(limit.clamp(1,200)).bind(offset.max(0)).fetch_all(&self.pool).await?)
    }
    pub async fn segments_on(
        conn: &mut PgConnection,
        dataset: &str,
        document: &str,
    ) -> Result<Vec<Chunk>> {
        let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(s) FROM knowledge.segments s WHERE dataset_id=$1 AND document_id=$2 ORDER BY position").bind(dataset).bind(document).fetch_all(conn).await?;
        rows.into_iter()
            .map(|v| serde_json::from_value(v).map_err(Into::into))
            .collect()
    }
    pub async fn insert_segments(conn: &mut PgConnection, chunks: &[Chunk]) -> Result<()> {
        for s in chunks {
            sqlx::query(sql::INSERT_SEGMENTS_0)
                .bind(&s.segment_id)
                .bind(&s.dataset_id)
                .bind(&s.document_id)
                .bind(s.position)
                .bind(s.level)
                .bind(&s.parent_segment_id)
                .bind(&s.summary)
                .bind(s.page_start)
                .bind(s.page_end)
                .bind(&s.text)
                .bind(s.token_count)
                .bind(&s.vector_id)
                .bind(&s.metadata)
                .bind(s.enabled)
                .bind(&s.status)
                .bind(s.word_count)
                .bind(&s.keywords)
                .bind(&s.answer)
                .bind(&s.created_by)
                .bind(&s.content_hash)
                .bind(&s.source_type)
                .bind(&s.source_reference)
                .bind(&s.citation_text)
                .bind(s.page_number)
                .bind(&s.section_header)
                .bind(&s.language)
                .bind(&s.contextual_prefix)
                .bind(&s.content_type)
                .bind(&s.index_node_id)
                .bind(&s.index_node_hash)
                .bind(&s.image_url)
                .bind(&s.image_attachment_id)
                .bind(&s.image_filename)
                .bind(&s.image_media_type)
                .bind(s.image_file_size)
                .execute(&mut *conn)
                .await?;
        }
        Ok(())
    }
    pub async fn create_version(
        conn: &mut PgConnection,
        document: &str,
        content: &str,
        change_type: &str,
        title: Option<&str>,
        metadata: &Value,
        reason: Option<&str>,
        actor: Option<&str>,
        activate: bool,
    ) -> Result<Value> {
        let next: i32 = sqlx::query_scalar(sql::CREATE_DOCUMENT_VERSION_0)
            .bind(document)
            .fetch_one(&mut *conn)
            .await?;
        let id = uuid::Uuid::new_v4().to_string();
        let count = content.split_whitespace().count() as i32;
        sqlx::query(sql::CREATE_DOCUMENT_VERSION_1)
            .bind(&id)
            .bind(document)
            .bind(next)
            .bind(content)
            .bind(sha256(content.as_bytes()))
            .bind(Option::<i32>::None)
            .bind(Option::<chrono::NaiveDateTime>::None)
            .bind(title)
            .bind(metadata)
            .bind(count)
            .bind(change_type)
            .bind(reason)
            .bind(actor)
            .execute(&mut *conn)
            .await?;
        if activate {
            sqlx::query(sql::CREATE_DOCUMENT_VERSION_2)
                .bind(next)
                .bind(document)
                .execute(&mut *conn)
                .await?;
        }
        Ok(
            json!({"version_id":id,"document_id":document,"version_number":next,"change_type":change_type,"word_count":count}),
        )
    }
    pub async fn list_versions(
        &self,
        document: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Value>> {
        Ok(sqlx::query_scalar("SELECT to_jsonb(v) FROM (SELECT version_id,document_id,version_number,content_hash,confluence_version,confluence_updated_at,title,word_count,change_type,change_reason,changed_by,created_at FROM knowledge.document_versions) v WHERE document_id=$1 AND change_type NOT IN ('pending_restore','pending_before_restore') ORDER BY version_number DESC LIMIT $2 OFFSET $3").bind(document).bind(limit.clamp(1,100)).bind(offset.max(0)).fetch_all(&self.pool).await?)
    }
    pub async fn get_version(
        &self,
        document: &str,
        version: i32,
        include_pending: bool,
    ) -> Result<Value> {
        sqlx::query_scalar("SELECT to_jsonb(v) FROM knowledge.document_versions v WHERE document_id=$1 AND version_number=$2 AND ($3 OR change_type NOT IN ('pending_restore','pending_before_restore'))").bind(document).bind(version).bind(include_pending).fetch_optional(&self.pool).await?.ok_or(IngestError::NotFound)
    }
    pub async fn record_process_rule(
        conn: &mut PgConnection,
        dataset: &str,
        mode: &str,
        rules: &Value,
        actor: Option<&str>,
    ) -> Result<String> {
        if let Some(id) = sqlx::query_scalar(sql::RECORD_PROCESS_RULE_0)
            .bind(dataset)
            .bind(mode)
            .bind(rules)
            .fetch_optional(&mut *conn)
            .await?
        {
            return Ok(id);
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        sqlx::query(sql::RECORD_PROCESS_RULE_1)
            .bind(&id)
            .bind(dataset)
            .bind(mode)
            .bind(rules)
            .bind(actor)
            .execute(conn)
            .await?;
        Ok(id)
    }
    pub async fn record_execution(
        conn: &mut PgConnection,
        dataset: &str,
        document: &str,
        action: &str,
        trigger: &str,
        actor: Option<&str>,
        rule: Option<&str>,
        snapshot: &Value,
    ) -> Result<String> {
        if !["ingest", "reprocess", "reembed", "recover", "retry"].contains(&action)
            || !["upload", "api", "worker", "confluence_sync", "recover"].contains(&trigger)
        {
            return Err(IngestError::Validation(
                "unsupported pipeline execution action or trigger".into(),
            ));
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        sqlx::query(sql::RECORD_PIPELINE_EXECUTION_0)
            .bind(&id)
            .bind(document)
            .bind(dataset)
            .bind(action)
            .bind(trigger)
            .bind(actor)
            .bind(rule)
            .bind(snapshot)
            .execute(&mut *conn)
            .await?;
        sqlx::query(sql::LINK_PIPELINE_EXECUTION_0)
            .bind(document)
            .bind(&id)
            .execute(conn)
            .await?;
        Ok(id)
    }
    pub async fn complete_execution(
        conn: &mut PgConnection,
        id: &str,
        status: &str,
        error: Option<&str>,
        manifest: &Value,
    ) -> Result<bool> {
        if !["completed", "error"].contains(&status) {
            return Err(IngestError::Validation(
                "pipeline execution status must be completed or error".into(),
            ));
        }
        Ok(sqlx::query(sql::COMPLETE_PIPELINE_EXECUTION_1)
            .bind(id)
            .bind(status)
            .bind(error)
            .bind(manifest)
            .fetch_optional(conn)
            .await?
            .is_some())
    }
    pub async fn publication_lease(
        &self,
        dataset: &str,
        expected: &str,
    ) -> Result<PublicationLease> {
        self.publication_lease_bound(dataset, expected, None).await
    }
    pub async fn publication_lease_bound(
        &self,
        dataset: &str,
        expected: &str,
        execution: Option<&str>,
    ) -> Result<PublicationLease> {
        let mut lease = DocumentLease {
            connection: Some(self.pool.acquire().await?),
            locks: vec![],
            dataset_id: dataset.into(),
            document_id: String::new(),
        };
        lease
            .lock(format!("knowledge-dataset-index:{dataset}"), true)
            .await?;
        lease
            .lock(format!("knowledge-dataset-publication:{dataset}"), false)
            .await?;
        let conn = lease.connection();
        let mut tx = sqlx::Connection::begin(conn).await?;
        Self::require_identity(&mut tx, dataset, Some(expected)).await?;
        let old:i64=sqlx::query_scalar("SELECT content_revision FROM knowledge.datasets WHERE dataset_id=$1 AND is_deleted=FALSE FOR UPDATE").bind(dataset).fetch_one(&mut *tx).await?;
        let revision = if old < 0 {
            old
        } else {
            sqlx::query_scalar("UPDATE knowledge.datasets SET content_revision=-ABS(COALESCE(content_revision,0))-$2,updated_at=NOW() WHERE dataset_id=$1 AND is_deleted=FALSE RETURNING content_revision").bind(dataset).bind(1_000_000_000i64).fetch_one(&mut *tx).await?
        };
        if old < 0 {
            if let Some(execution) = execution {
                let owner = Self::active_special(&mut tx, dataset).await?;
                if string(&owner, "execution_id") != execution {
                    return Err(IngestError::GenerationChanged);
                }
            }
        }
        if old >= 0 {
            if let Some(execution) = execution {
                Self::bind_special(&mut tx, execution, dataset, revision).await?;
            }
        }
        tx.commit().await?;
        Ok(PublicationLease {
            lease,
            revision,
            recovered: old < 0,
        })
    }
    pub async fn commit_text_publication(
        publication: &mut PublicationLease,
        dataset: &str,
        document: &str,
        chunks: &[Chunk],
        keep_ids: &[String],
        staged_ids: &[String],
        expected: &str,
        content: &str,
        metadata_patch: &Value,
        expected_content: &str,
        pending: Option<&Value>,
        replace_images: bool,
        lexical: Option<(
            &crate::migration_knowledge_query::lexical_lifecycle::LexicalLifecycle,
            &crate::migration_knowledge_query::lexical_lifecycle::ActivePublicationContext,
        )>,
    ) -> Result<i64> {
        let publication_revision = publication.revision;
        let mut tx = sqlx::Connection::begin(publication.connection()).await?;
        Self::require_identity(&mut tx, dataset, Some(expected)).await?;
        let current:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM knowledge.documents d WHERE document_id=$1 AND dataset_id=$2 AND status='indexing' FOR UPDATE").bind(document).bind(dataset).fetch_optional(&mut *tx).await?.ok_or(IngestError::GenerationChanged)?;
        if string(&current, "content") != expected_content {
            return Err(IngestError::GenerationChanged);
        }
        if let Some(pending) = pending {
            let previous = integer(pending, "previous_version");
            let candidate = integer(pending, "candidate_version");
            if current["metadata"]["_document_pending_restore_version"] != *pending
                || previous <= integer(&current, "current_version")
                || candidate <= previous
            {
                return Err(IngestError::GenerationChanged);
            }
            for (number, status, next, text) in [
                (
                    previous,
                    "pending_before_restore",
                    "updated",
                    expected_content,
                ),
                (candidate, "pending_restore", "restored", content),
            ] {
                if sqlx::query("UPDATE knowledge.document_versions SET change_type=$4 WHERE document_id=$1 AND version_number=$2 AND change_type=$3 AND content=$5 RETURNING version_id").bind(document).bind(number as i32).bind(status).bind(next).bind(text).fetch_optional(&mut *tx).await?.is_none(){return Err(IngestError::GenerationChanged)}
            }
            sqlx::query("UPDATE knowledge.documents SET current_version=$2,version_count=COALESCE(version_count,0)+2,metadata=(COALESCE(metadata,'{}'::jsonb)-'_document_pending_restore_version')||jsonb_build_object('_document_restored_source_version',$2::integer) WHERE document_id=$1").bind(document).bind(candidate as i32).execute(&mut *tx).await?;
        } else if !content.trim().is_empty() {
            let current_version = integer(&current, "current_version");
            let active: Option<Value> = if current_version > 0 {
                sqlx::query_scalar("SELECT to_jsonb(v) FROM knowledge.document_versions v WHERE document_id=$1 AND version_number=$2 AND change_type NOT IN ('pending_restore','pending_before_restore') FOR UPDATE")
                    .bind(document)
                    .bind(current_version as i32)
                    .fetch_optional(&mut *tx)
                    .await?
            } else {
                None
            };
            if !active
                .as_ref()
                .is_some_and(|version| source_version_matches(version, content))
            {
                let has_published_version: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.document_versions WHERE document_id=$1 AND change_type NOT IN ('pending_restore','pending_before_restore'))")
                    .bind(document)
                    .fetch_one(&mut *tx)
                    .await?;
                let mut version_metadata = object(&current["metadata"]);
                if let (Some(target), Some(patch)) =
                    (version_metadata.as_object_mut(), metadata_patch.as_object())
                {
                    target.extend(
                        patch
                            .iter()
                            .map(|(key, value)| (key.clone(), value.clone())),
                    );
                }
                if let Some(metadata) = version_metadata.as_object_mut() {
                    for key in [
                        "_document_ingest_action",
                        "_document_recover_stage",
                        "_document_pipeline_execution_id",
                        "_document_lifecycle_reindex",
                    ] {
                        metadata.remove(key);
                    }
                }
                Self::create_version(
                    &mut tx,
                    document,
                    content,
                    if has_published_version {
                        "updated"
                    } else {
                        "created"
                    },
                    Some(string(&current, "title")),
                    &version_metadata,
                    None,
                    None,
                    true,
                )
                .await?;
            }
        }
        Self::insert_segments(&mut tx, chunks).await?;
        sqlx::query(sql::DELETE_SEGMENTS_BY_DOCUMENT_1)
            .bind(document)
            .bind(keep_ids)
            .bind("text")
            .execute(&mut *tx)
            .await?;
        if replace_images {
            sqlx::query("DELETE FROM knowledge.segments WHERE document_id=$1 AND content_type='image' AND NOT(segment_id=ANY($2::text[]))").bind(document).bind(keep_ids).execute(&mut *tx).await?;
        }
        sqlx::query(sql::ACTIVATE_STAGED_SEGMENTS_1)
            .bind(document)
            .bind(staged_ids)
            .execute(&mut *tx)
            .await?;
        let changed=sqlx::query("UPDATE knowledge.documents SET content=$3,metadata=COALESCE(metadata,'{}'::jsonb)||$4::jsonb,word_count=$5,segment_count=(SELECT COUNT(*) FROM knowledge.segments WHERE document_id=$1),updated_at=NOW() WHERE document_id=$1 AND dataset_id=$2 AND status='indexing'")
            .bind(document).bind(dataset).bind(content).bind(metadata_patch).bind(content.split_whitespace().count() as i32).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(IngestError::GenerationChanged);
        }
        Self::update_status(&mut tx, document, "completed", Some(100.0), None).await?;
        let certification = if let Some((lifecycle, context)) = lexical {
            Some(
                lifecycle
                    .recertify_active_publication(context, publication_revision, &mut tx)
                    .await?,
            )
        } else {
            None
        };
        let revision: i64 = sqlx::query_scalar(sql::FINISH_INDEX_PUBLICATION_0)
            .bind(dataset)
            .fetch_one(&mut *tx)
            .await?;
        if let (Some((lifecycle, context)), Some(certification)) = (lexical, certification) {
            if integer(&certification, "target_revision") != revision {
                return Err(IngestError::GenerationChanged);
            }
            lifecycle
                .settle_active_publication(context, &certification, &mut tx)
                .await?;
        }
        tx.commit().await?;
        Ok(revision)
    }
    pub async fn abort_publication(
        publication: &mut PublicationLease,
        dataset: &str,
    ) -> Result<i64> {
        Ok(sqlx::query_scalar(sql::FINISH_INDEX_PUBLICATION_0)
            .bind(dataset)
            .fetch_one(publication.connection())
            .await?)
    }
}
fn rank(permission: &str) -> i32 {
    match permission {
        "viewer" => 1,
        "editor" => 2,
        "owner" => 3,
        _ => 0,
    }
}
/// A dropped or cancelled lease closes its socket. A still-locked session is
/// never returned to the pool; normal release returns the healthy socket.
pub struct DocumentLease {
    connection: Option<PoolConnection<Postgres>>,
    locks: Vec<(String, bool)>,
    pub dataset_id: String,
    pub document_id: String,
}
impl DocumentLease {
    pub fn connection(&mut self) -> &mut PgConnection {
        &mut *self.connection.as_mut().expect("live document lease")
    }
    async fn lock(&mut self, name: String, shared: bool) -> Result<()> {
        let sql = if shared {
            "SELECT pg_try_advisory_lock_shared(hashtextextended($1,0))"
        } else {
            "SELECT pg_try_advisory_lock(hashtextextended($1,0))"
        };
        let locked: bool = sqlx::query_scalar(sql)
            .bind(&name)
            .fetch_one(self.connection())
            .await?;
        if !locked {
            return Err(IngestError::LeaseBusy);
        }
        self.locks.push((name, shared));
        Ok(())
    }
    pub async fn release(mut self) -> Result<()> {
        while let Some((name, shared)) = self.locks.pop() {
            let sql = if shared {
                "SELECT pg_advisory_unlock_shared(hashtextextended($1,0))"
            } else {
                "SELECT pg_advisory_unlock(hashtextextended($1,0))"
            };
            let released: bool = sqlx::query_scalar(sql)
                .bind(name)
                .fetch_one(self.connection())
                .await?;
            if !released {
                return Err(IngestError::Database);
            }
        }
        self.connection.take();
        Ok(())
    }
}
impl Drop for DocumentLease {
    fn drop(&mut self) {
        if let Some(conn) = self.connection.as_mut() {
            conn.close_on_drop();
        }
    }
}
pub struct PublicationLease {
    lease: DocumentLease,
    pub revision: i64,
    pub recovered: bool,
}
impl PublicationLease {
    pub fn connection(&mut self) -> &mut PgConnection {
        self.lease.connection()
    }
    pub async fn release(self) -> Result<()> {
        self.lease.release().await
    }
}

impl KnowledgeWriteStore {
    pub async fn dataset_delete_lease(&self, dataset: &str) -> Result<DocumentLease> {
        let mut lease = DocumentLease {
            connection: Some(self.pool.acquire().await?),
            locks: vec![],
            dataset_id: dataset.into(),
            document_id: String::new(),
        };
        lease
            .lock(format!("knowledge-dataset-index:{dataset}"), false)
            .await?;
        Ok(lease)
    }
    pub async fn set_deletion_fence(
        conn: &mut PgConnection,
        dataset: &str,
        operation: &str,
        target: &str,
    ) -> Result<Value> {
        if !["dataset_delete", "document_delete", "segment_delete"].contains(&operation)
            || target.trim().is_empty()
        {
            return Err(IngestError::Validation(
                "invalid dataset index deletion target".into(),
            ));
        }
        let marker =
            json!({"operation":operation,"target_id":target,"status":"pending","version":1});
        sqlx::query("UPDATE knowledge.datasets SET index_config=jsonb_set(COALESCE(index_config,'{}'::jsonb),'{retrieval}',(CASE WHEN jsonb_typeof(index_config->'retrieval')='object' THEN index_config->'retrieval' ELSE '{}'::jsonb END)||jsonb_build_object('_index_deletion_fence',$2::jsonb),TRUE),content_revision=COALESCE(content_revision,0)+1,updated_at=NOW() WHERE dataset_id=$1 AND is_deleted=FALSE AND jsonb_typeof(COALESCE(index_config,'{}'::jsonb))='object' AND NOT ((CASE WHEN jsonb_typeof(index_config->'retrieval')='object' THEN index_config->'retrieval' ELSE '{}'::jsonb END)?'_index_deletion_fence')")
            .bind(dataset).bind(&marker).execute(&mut *conn).await?;
        let current = Self::dataset_on(conn, dataset).await?;
        if current["index_config"]["retrieval"]["_index_deletion_fence"] != marker {
            return Err(IngestError::Validation(
                "another dataset index deletion target is already pending".into(),
            ));
        }
        Ok(current)
    }
    pub async fn clear_deletion_fence(
        conn: &mut PgConnection,
        dataset: &str,
        operation: &str,
        target: &str,
    ) -> Result<bool> {
        let marker =
            json!({"operation":operation,"target_id":target,"status":"pending","version":1});
        Ok(sqlx::query("UPDATE knowledge.datasets SET index_config=jsonb_set(COALESCE(index_config,'{}'::jsonb),'{retrieval}',(CASE WHEN jsonb_typeof(index_config->'retrieval')='object' THEN index_config->'retrieval' ELSE '{}'::jsonb END)-'_index_deletion_fence',TRUE),content_revision=COALESCE(content_revision,0)+1,updated_at=NOW() WHERE dataset_id=$1 AND is_deleted=FALSE AND jsonb_typeof(COALESCE(index_config,'{}'::jsonb))='object' AND (CASE WHEN jsonb_typeof(index_config->'retrieval')='object' THEN index_config->'retrieval' ELSE '{}'::jsonb END)->'_index_deletion_fence'=$2::jsonb RETURNING dataset_id").bind(dataset).bind(marker).fetch_optional(conn).await?.is_some())
    }
}

#[cfg(test)]
mod source_version_tests {
    use super::*;

    #[test]
    fn source_snapshot_matches_only_when_content_hash_and_publication_state_match() {
        let content = "published source";
        let hash = sha256(content.as_bytes());
        let matching = json!({
            "change_type": "created",
            "content": content,
            "content_hash": hash,
        });
        assert!(source_version_matches(&matching, content));

        let wrong_hash = json!({
            "change_type": "created",
            "content": content,
            "content_hash": "0".repeat(64),
        });
        assert!(!source_version_matches(&wrong_hash, content));

        let wrong_content = json!({
            "change_type": "created",
            "content": "older source",
            "content_hash": sha256(b"older source"),
        });
        assert!(!source_version_matches(&wrong_content, content));

        let pending = json!({
            "change_type": "pending_restore",
            "content": content,
            "content_hash": hash,
        });
        assert!(!source_version_matches(&pending, content));
    }
}

#[cfg(test)]
mod catalog_permission_tests {
    use super::*;

    fn actor() -> Actor {
        Actor {
            user_id: "reader".into(),
            tenant_id: "t".into(),
            user_tier: "normal".into(),
            user_type: "user".into(),
            roles: vec!["analyst".into()],
            is_authenticated: true,
        }
    }

    // Frozen pre-batch native decision algorithm, with its SQL result supplied.
    fn legacy(actor: &Actor, dataset: &Value, permissions: &[String]) -> i32 {
        if dataset["is_archived"].as_bool() == Some(true) {
            return 0;
        }
        let visibility = string(dataset, "visibility").to_lowercase();
        if actor.anonymous() {
            return if visibility == "public" { 1 } else { 0 };
        }
        let tenant = string(dataset, "tenant_id").trim();
        let same = !tenant.is_empty()
            && !actor.tenant_id.trim().is_empty()
            && tenant == actor.tenant_id.trim();
        if (same && (actor.user_tier == "admin" || actor.roles.iter().any(|r| r == "admin")))
            || (!actor.user_id.is_empty() && string(dataset, "created_by") == actor.user_id)
        {
            return 3;
        }
        let explicit = permissions.iter().map(|s| rank(s)).max().unwrap_or(0);
        if permissions.iter().any(|permission| !permission.is_empty()) {
            explicit
        } else if visibility == "public" || (same && visibility == "tenant") {
            1
        } else {
            0
        }
    }

    #[test]
    fn catalog_grant_snapshot_preserves_native_static_decisions() {
        let mut actors = vec![actor()];
        for (tier, kind, roles, authenticated) in [
            ("admin", "user", vec![], true),
            ("normal", "service", vec!["service"], true),
            ("normal", "user", vec!["admin"], true),
            ("normal", "user", vec!["analyst", "analyst"], true),
            ("normal", "guest", vec![], true),
            ("normal", "user", vec![], false),
            ("platform_admin", "user", vec!["platform_admin"], true),
        ] {
            let mut user = actor();
            user.user_tier = tier.into();
            user.user_type = kind.into();
            user.roles = roles.into_iter().map(String::from).collect();
            user.is_authenticated = authenticated;
            actors.push(user);
        }
        let grants = [
            vec![],
            vec![""],
            vec!["unknown"],
            vec!["viewer"],
            vec!["VIEWER"],
            vec!["editor"],
            vec!["owner"],
            vec!["unknown", "viewer"],
            vec!["editor", "owner"],
        ];
        let mut cases = 0;
        for mut user in actors {
            for tenant in ["", "t", "foreign", " t "] {
                user.tenant_id = tenant.into();
                for dataset_tenant in ["", "t", "foreign", " t "] {
                    for visibility in ["private", "tenant", "PUBLIC"] {
                        for archived in [false, true] {
                            for creator in ["", "reader", "other"] {
                                let row = json!({"dataset_id":"d", "tenant_id":dataset_tenant,
                                    "visibility":visibility, "is_archived":archived, "created_by":creator});
                                for values in &grants {
                                    let values: Vec<String> =
                                        values.iter().map(|s| s.to_string()).collect();
                                    assert_eq!(
                                        dataset_permission_from_grants(&user, &row, &values),
                                        legacy(&user, &row, &values)
                                    );
                                    cases += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(cases, 20736);
    }

    #[test]
    fn catalog_grant_plan_is_bounded_and_excludes_fast_paths_and_foreign_roles() {
        let rows = vec![
            json!({"dataset_id":"own","created_by":"reader","tenant_id":"t"}),
            json!({"dataset_id":"same","tenant_id":"t"}),
            json!({"dataset_id":"foreign","tenant_id":"other","visibility":"public"}),
            json!({"dataset_id":"archived","tenant_id":"t","is_archived":true}),
        ];
        let user = actor();
        assert_eq!(
            catalog_permission_ids(&user, &rows).unwrap(),
            (vec!["same", "foreign"], vec!["same"])
        );
        assert!(catalog_permission_ids(&user, &vec![rows[1].clone(); 201]).is_err());
        assert_eq!(
            catalog_permission_ids(&user, &vec![rows[1].clone(); 200])
                .unwrap()
                .0
                .len(),
            200
        );
        assert!(catalog_permission_ids(&user, &[]).unwrap().0.is_empty());
        assert!(
            catalog_permission_ids(&user, &[rows[0].clone(), rows[3].clone()])
                .unwrap()
                .0
                .is_empty()
        );
        let mut admin = user.clone();
        admin.user_tier = "admin".into();
        assert!(
            catalog_permission_ids(&admin, &[rows[1].clone()])
                .unwrap()
                .0
                .is_empty()
        );
        let mut anonymous = user.clone();
        anonymous.is_authenticated = false;
        assert!(
            catalog_permission_ids(&anonymous, &rows)
                .unwrap()
                .0
                .is_empty()
        );
    }
}
