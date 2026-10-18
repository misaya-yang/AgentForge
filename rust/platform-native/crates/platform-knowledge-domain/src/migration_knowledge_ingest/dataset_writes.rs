//! Dataset configuration CAS, lexical compensation, owner grants and archive proof.
use super::{
    Actor, Dataset, IngestError, IngestionService, KnowledgeWriteStore, Result,
    dataset_ingestion_identity, integer, object, require_index_writable, string,
};
use crate::migration_knowledge_query::{config, lexical_v2::LexicalProfile};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection};
const CONFIG: &[&str] = &[
    "embedding_provider",
    "embedding_model",
    "embedding_model_version",
    "embedding_dimension",
    "embedding_config",
    "index_config",
    "collection_name",
];
fn projection(v: &Value) -> Value {
    let mut out = json!({});
    for k in CONFIG {
        out[*k] = if ["embedding_config", "index_config"].contains(k) {
            object(&v[*k])
        } else {
            v[*k].clone()
        }
    }
    out
}
fn transition_identity(v: &Value) -> Value {
    json!({"tenant_id":v["tenant_id"],"collection_name":v["collection_name"],"embedding_dimension":v["embedding_dimension"],"content_revision":v["content_revision"],"lexical":v["index_config"]["retrieval"]["lexical"]})
}
async fn patch_on(
    conn: &mut PgConnection,
    id: &str,
    patch: &Value,
    expected: Option<&Value>,
    no_docs: bool,
) -> Result<Option<Value>> {
    let allowed = [
        "name",
        "description",
        "visibility",
        "embedding_provider",
        "embedding_model",
        "embedding_model_version",
        "embedding_dimension",
        "embedding_config",
        "index_config",
        "collection_name",
    ];
    let selected: Vec<_> = allowed
        .into_iter()
        .filter(|k| patch.get(k).is_some())
        .collect();
    if selected.is_empty() {
        return Ok(Some(KnowledgeWriteStore::dataset_on(conn, id).await?));
    }
    let mut assignments = vec![];
    for k in &selected {
        assignments.push(format!(
            "{k}=(jsonb_populate_record(NULL::knowledge.datasets,$2::jsonb)).{k}"
        ))
    }
    let mut predicates = vec![
        "dataset_id=$1".to_owned(),
        "is_deleted=FALSE".into(),
        "is_archived=FALSE".into(),
        "$3::jsonb IS NOT NULL".into(),
    ];
    if selected.iter().any(|k| CONFIG.contains(k)) {
        let expected = expected.ok_or(IngestError::GenerationChanged)?;
        if CONFIG.iter().any(|k| expected.get(k).is_none()) {
            return Err(IngestError::GenerationChanged);
        }
        for k in CONFIG {
            predicates.push(format!("{k} IS NOT DISTINCT FROM (jsonb_populate_record(NULL::knowledge.datasets,$3::jsonb)).{k}"))
        }
    }
    if no_docs {
        predicates.push("NOT EXISTS (SELECT 1 FROM knowledge.documents WHERE documents.dataset_id=datasets.dataset_id)".into())
    }
    let sql = format!(
        "UPDATE knowledge.datasets SET {},updated_at=NOW() WHERE {} RETURNING to_jsonb(datasets)",
        assignments.join(","),
        predicates.join(" AND ")
    );
    Ok(sqlx::query_scalar(&sql)
        .bind(id)
        .bind(patch)
        .bind(expected.cloned().unwrap_or_else(|| json!({})))
        .fetch_optional(crate::database::connection(&mut *conn))
        .await?)
}
impl IngestionService {
    async fn reconcile_lexical_selection(&self, id: &str) -> Result<()> {
        for _ in 0..3 {
            let value = self.store.get_dataset(id).await?;
            let d: Dataset = serde_json::from_value(value.clone())?;
            let dimension = d.embedding_dimension.filter(|n| *n > 0).ok_or_else(|| {
                IngestError::Validation(
                    "Dataset collection/dimension is missing during lexical reconciliation".into(),
                )
            })?;
            if d.collection_name.as_deref().unwrap_or("").is_empty() {
                return Err(IngestError::Validation(
                    "Dataset collection/dimension is missing during lexical reconciliation".into(),
                ));
            }
            self.indexer
                .vectors
                .reconcile_lexical_collection(&d, dimension)
                .await?;
            if transition_identity(&value)
                == transition_identity(&self.store.get_dataset(id).await?)
            {
                return Ok(());
            }
        }
        Err(IngestError::Validation(
            "Concurrent lexical updates did not converge after three reconciliations".into(),
        ))
    }
    pub async fn update_dataset(&self, actor: &Actor, id: &str, input: &Value) -> Result<Value> {
        if self.indexer.vectors.is_read_only() {
            return super::vector_jobs::submit(
                &self.store.pool,
                actor,
                id,
                super::vector_jobs::Mutation::UpdateDataset {
                    input: input.clone(),
                },
            )
            .await;
        }
        let before = self
            .store
            .require_dataset_access(actor, id, "owner")
            .await?;
        self.check_vector_job_generation(&before)?;
        require_index_writable(&before)?;
        let mut patch = json!({});
        for k in [
            "name",
            "description",
            "visibility",
            "embedding_provider",
            "embedding_model",
            "embedding_dimension",
            "embedding_config",
            "index_config",
        ] {
            if let Some(v) = input.get(k).filter(|v| !v.is_null()) {
                patch[k] = v.clone()
            }
        }
        if patch.as_object().unwrap().is_empty() {
            return Ok(super::service::redact_server_config(&before));
        }
        if let Some(dim) = patch.get("embedding_dimension") {
            if !dim.as_u64().is_some_and(|n| (1..=8192).contains(&n)) {
                return Err(IngestError::Validation(
                    "embedding_dimension must be an integer between 1 and 8192".into(),
                ));
            }
        }
        if patch.get("embedding_config").is_some()
            && super::service::contains_server_config(&patch["embedding_config"])
        {
            return Err(IngestError::Validation(
                "embedding credentials and endpoints are server-owned".into(),
            ));
        }
        if patch.get("index_config").is_some() {
            if patch["index_config"]["retrieval"]
                .get("_index_deletion_fence")
                .is_some()
            {
                return Err(IngestError::Validation(
                    "index_config contains a reserved lifecycle field".into(),
                ));
            }
            config::validate_bounds(&patch["index_config"]["retrieval"])?;
            crate::migration_knowledge_query::retrieval_v2::Profile::from_index_config(
                &patch["index_config"],
            )?;
            if super::service::contains_server_config(&patch["index_config"]["retrieval"]["rerank"])
            {
                return Err(IngestError::Validation(
                    "rerank credentials and endpoints are server-owned".into(),
                ));
            }
            super::chunking::ChunkingConfig::from_value(&patch["index_config"]["chunking"])?;
            super::structured::resolve_config(&patch["index_config"])?;
        }
        let mut candidate = before.clone();
        for (k, v) in patch.as_object().unwrap() {
            candidate[k] = v.clone()
        }
        if [
            "unified_multimodal",
            "unified",
            "cross_modal",
            "dashscope_multimodal",
            "aliyun_multimodal",
            "multimodal",
        ]
        .contains(&string(&candidate, "embedding_provider"))
            || candidate["index_config"]["multimodal"]["enabled"].as_bool() == Some(true)
        {
            if !crate::migration_knowledge_query::serving_profile::multimodal(
                &serde_json::from_value(candidate.clone())?,
            )? {
                return Err(IngestError::Validation(
                    "multimodal dataset profiles are disabled".into(),
                ));
            }
        }
        crate::migration_knowledge_query::serving_profile::require(&serde_json::from_value(
            candidate.clone(),
        )?)?;
        let identity_changed =
            dataset_ingestion_identity(&candidate) != dataset_ingestion_identity(&before);
        if identity_changed
            && sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM knowledge.documents WHERE dataset_id=$1)",
            )
            .bind(id)
            .fetch_one(crate::database::executor(&self.store.pool))
            .await?
        {
            let message = if [
                "embedding_provider",
                "embedding_model",
                "embedding_dimension",
            ]
            .iter()
            .any(|k| candidate[*k] != before[*k])
            {
                format!(
                    "Cannot change embedding or ingestion index identity when documents exist; use POST /api/v1/knowledge/datasets/{id}/embedding-migration/start"
                )
            } else {
                "Cannot change embedding or ingestion index identity when documents exist; create a reindexed generation".into()
            };
            return Err(IngestError::Validation(message));
        }
        let typed: Dataset = serde_json::from_value(candidate.clone())?;
        let profile = LexicalProfile::from_dataset(&typed)?;
        if profile.active {
            return Err(IngestError::Validation("bm25_v2 active cutover is hard-disabled; use lexical_v1 with bm25_v2 shadow writes".into()));
        }
        let embedding_change = [
            "embedding_provider",
            "embedding_model",
            "embedding_dimension",
            "embedding_config",
        ]
        .iter()
        .any(|k| patch.get(k).is_some());
        if embedding_change {
            let resolved = self.indexer.embeddings.resolve(&typed).await?;
            let collection = self
                .indexer
                .vectors
                .ensure_collection(&typed, resolved.dimension)
                .await?;
            candidate["embedding_dimension"] = json!(resolved.dimension);
            candidate["collection_name"] = json!(collection);
            patch["embedding_dimension"] = candidate["embedding_dimension"].clone();
            patch["collection_name"] = candidate["collection_name"].clone();
        }
        if patch.get("index_config").is_some() && !embedding_change {
            let dim = typed
                .embedding_dimension
                .filter(|n| *n > 0)
                .ok_or_else(|| {
                    IngestError::Validation(
                        "Dataset collection/dimension is missing; re-index before lexical rollout"
                            .into(),
                    )
                })?;
            if typed.collection_name.as_deref().unwrap_or("").is_empty() {
                return Err(IngestError::Validation(
                    "Dataset collection/dimension is missing; re-index before lexical rollout"
                        .into(),
                ));
            }
            let old: Dataset = serde_json::from_value(before.clone())?;
            if profile.shadow && !LexicalProfile::from_dataset(&old)?.shadow {
                self.indexer
                    .vectors
                    .reconcile_lexical_collection(&typed, dim)
                    .await?;
            }
        }
        let expected = projection(&before);
        let saved = if identity_changed {
            let mut lease = self.store.dataset_delete_lease(id).await?;
            let result = patch_on(lease.connection(), id, &patch, Some(&expected), true).await;
            lease.release().await?;
            result?
        } else {
            patch_on(
                &mut *self.store.pool.acquire().await?,
                id,
                &patch,
                Some(&expected),
                false,
            )
            .await?
        };
        let Some(saved) = saved else {
            let _ = self.reconcile_lexical_selection(id).await;
            return Err(IngestError::GenerationChanged);
        };
        if patch.get("index_config").is_some() {
            if self.reconcile_lexical_selection(id).await.is_err() {
                let mut rollback = json!({});
                for k in CONFIG {
                    if patch.get(k).is_some() {
                        rollback[*k] = before[*k].clone()
                    }
                }
                let compensated = if identity_changed {
                    let mut lease = self.store.dataset_delete_lease(id).await?;
                    let r = patch_on(
                        lease.connection(),
                        id,
                        &rollback,
                        Some(&projection(&saved)),
                        true,
                    )
                    .await;
                    lease.release().await?;
                    r
                } else {
                    patch_on(
                        &mut *self.store.pool.acquire().await?,
                        id,
                        &rollback,
                        Some(&projection(&saved)),
                        false,
                    )
                    .await
                };
                if compensated.is_err() || self.reconcile_lexical_selection(id).await.is_err() {
                    return Err(IngestError::Validation("Lexical runtime reconciliation failed and compensating rollback did not converge; bm25_v2 remains fail-closed".into()));
                }
                return Err(IngestError::Validation("Lexical runtime reconciliation failed; dataset selection was compensated to its previous configuration".into()));
            }
        }
        Ok(super::service::redact_server_config(
            &self.store.get_dataset(id).await?,
        ))
    }
    pub async fn set_dataset_archived(
        &self,
        actor: &Actor,
        id: &str,
        archived: bool,
        reason: Option<&str>,
    ) -> Result<Value> {
        if actor.anonymous() || actor.user_id.is_empty() {
            return Err(IngestError::Permission);
        }
        let reason = reason.map(str::trim).filter(|s| !s.is_empty());
        if reason.is_some_and(|s| s.chars().count() > 500) {
            return Err(IngestError::Validation(
                "archive reason exceeds 500 characters".into(),
            ));
        }
        let mut lease = self.store.dataset_delete_lease(id).await?;
        let mut tx = lease.connection().begin().await?;
        crate::database::bind_current(&mut tx).await?;
        let d:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM knowledge.datasets d WHERE dataset_id=$1 AND is_deleted=FALSE FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?.ok_or(IngestError::NotFound)?;
        let same = !actor.tenant_id.is_empty() && string(&d, "tenant_id") == actor.tenant_id;
        let grant:bool=sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM knowledge.dataset_permissions WHERE dataset_id=$1 AND permission='owner' AND ((subject_type='user' AND subject_id=$2) OR (subject_type='tenant_role' AND $3::boolean AND subject_tenant_id=$5 AND subject_id=ANY($4::text[]))) FOR SHARE)").bind(id).bind(&actor.user_id).bind(same).bind(&actor.roles).bind(&actor.tenant_id).fetch_one(&mut *tx).await?;
        if !((same && (actor.user_tier == "admin" || actor.roles.iter().any(|r| r == "admin")))
            || string(&d, "created_by") == actor.user_id
            || grant)
        {
            return Err(IngestError::Permission);
        }
        if d["is_archived"].as_bool().unwrap_or(false) == archived {
            tx.commit().await?;
            lease.release().await?;
            let mut result = super::service::redact_server_config(&d);
            result["my_permission"] = json!("owner");
            return Ok(result);
        }
        if archived {
            sqlx::query("UPDATE knowledge.document_pipeline_executions AS execution SET status=CASE WHEN document.metadata->>'_document_pipeline_execution_id'=execution.execution_id AND document.status='completed' THEN 'completed' ELSE 'error' END,error=CASE WHEN document.metadata->>'_document_pipeline_execution_id'=execution.execution_id AND document.status='completed' THEN NULL ELSE 'Execution was not active when dataset was archived' END,completed_at=NOW() FROM knowledge.documents AS document WHERE execution.dataset_id=$1 AND execution.document_id=document.document_id AND execution.status='running' AND COALESCE(execution.manifest->'special_publication'->>'phase','') NOT IN ('preparing','prepared','points_written') AND (document.metadata->>'_document_pipeline_execution_id' IS DISTINCT FROM execution.execution_id OR document.status IN ('completed','error'))").bind(id).execute(&mut *tx).await?;
            let running:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.document_pipeline_executions WHERE dataset_id=$1 AND status='running')").bind(id).fetch_one(&mut *tx).await?;
            if running {
                return Err(IngestError::Validation(
                    "dataset has a running document execution; retry archive after recovery".into(),
                ));
            }
        }
        if integer(&d, "content_revision") < 0 {
            return Err(IngestError::Validation(
                "dataset index publication is still pending".into(),
            ));
        }
        if d["index_config"]["retrieval"]
            .get("_index_deletion_fence")
            .is_some()
        {
            return Err(IngestError::Validation(
                "dataset index deletion is pending".into(),
            ));
        }
        let updated:Value=sqlx::query_scalar("UPDATE knowledge.datasets SET is_archived=$2,archived_at=CASE WHEN $2 THEN NOW() ELSE NULL END,archived_by=CASE WHEN $2 THEN $3 ELSE NULL END,archive_reason=CASE WHEN $2 THEN $4 ELSE NULL END,content_revision=COALESCE(content_revision,0)+1,updated_at=NOW() WHERE dataset_id=$1 AND is_deleted=FALSE RETURNING to_jsonb(datasets)").bind(id).bind(archived).bind(&actor.user_id).bind(reason).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        lease.release().await?;
        let mut result = super::service::redact_server_config(&updated);
        result["my_permission"] = json!("owner");
        Ok(result)
    }
    pub async fn grant_dataset_permission(
        &self,
        actor: &Actor,
        id: &str,
        subject_type: &str,
        subject_id: &str,
        permission: &str,
    ) -> Result<()> {
        self.store
            .require_dataset_access(actor, id, "owner")
            .await?;
        if !["user", "role"].contains(&subject_type)
            || !["owner", "editor", "viewer"].contains(&permission)
        {
            return Err(IngestError::Validation(
                "invalid permission subject or level".into(),
            ));
        }
        sqlx::query("INSERT INTO knowledge.dataset_permissions(dataset_id,subject_type,subject_id,permission,subject_tenant_id) SELECT dataset_id,$2,$3,$4,CASE WHEN $2::varchar='tenant_role' THEN tenant_id ELSE NULL END FROM knowledge.datasets WHERE dataset_id=$1 ON CONFLICT(dataset_id,subject_type,subject_id) DO UPDATE SET permission=EXCLUDED.permission,updated_at=NOW()").bind(id).bind(if subject_type=="role"{"tenant_role"}else{subject_type}).bind(subject_id).bind(permission).execute(crate::database::executor(&self.store.pool)).await?;
        Ok(())
    }
    pub async fn revoke_dataset_permission(
        &self,
        actor: &Actor,
        id: &str,
        subject_type: &str,
        subject_id: &str,
    ) -> Result<bool> {
        self.store
            .require_dataset_access(actor, id, "owner")
            .await?;
        Ok(sqlx::query("DELETE FROM knowledge.dataset_permissions WHERE dataset_id=$1 AND subject_type=$2 AND subject_id=$3").bind(id).bind(if subject_type=="role"{"tenant_role"}else{subject_type}).bind(subject_id).execute(crate::database::executor(&self.store.pool)).await?.rows_affected()>0)
    }
    pub async fn list_dataset_permissions(&self, actor: &Actor, id: &str) -> Result<Vec<Value>> {
        self.store
            .require_dataset_access(actor, id, "owner")
            .await?;
        Ok(sqlx::query_scalar("SELECT to_jsonb(p) FROM (SELECT id,dataset_id,CASE WHEN subject_type='tenant_role' THEN 'role' ELSE subject_type END AS subject_type,subject_id,permission,created_at,updated_at FROM knowledge.dataset_permissions WHERE dataset_id=$1 ORDER BY created_at ASC) p").bind(id).fetch_all(crate::database::executor(&self.store.pool)).await?)
    }
}
