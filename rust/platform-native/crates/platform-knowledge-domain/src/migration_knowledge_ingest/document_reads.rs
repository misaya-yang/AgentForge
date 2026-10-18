//! Canonical management reads with active-state filtering and content fences.
use super::service::with_display_status;
use super::{Actor, IngestError, IngestionService, KnowledgeWriteStore, Result, object, string};
use serde_json::{Value, json};
use sqlx::{Postgres, QueryBuilder};
use std::collections::BTreeSet;

pub const DOCUMENT_LIST_PAGE_CAP: i64 = 200;
pub const SEGMENT_LIST_PAGE_CAP: i64 = 500;
pub const CONTENT_CHANGED: &str =
    "dataset content generation changed during read; retry the request";

pub(super) fn generation(dataset: &Value) -> Result<(String, Value)> {
    let retrieval = object(&object(&dataset["index_config"])["retrieval"]);
    if let Some(marker) = retrieval.get("_index_deletion_fence") {
        if !marker.is_object()
            || !["dataset_delete", "document_delete", "segment_delete"]
                .contains(&string(marker, "operation"))
            || string(marker, "target_id").trim().is_empty()
            || marker["status"] != "pending"
            || marker["version"] != 1
        {
            return Err(IngestError::Validation(
                "dataset index deletion fence is malformed".into(),
            ));
        }
        return Err(IngestError::Validation(
            "dataset index deletion is pending; indexed content is unavailable".into(),
        ));
    }
    Ok((
        string(dataset, "tenant_id").trim().to_owned(),
        dataset["content_revision"].clone(),
    ))
}
struct ReadScope {
    dataset: Value,
    generation: (String, Value),
    active_only: bool,
}
impl IngestionService {
    async fn read_scope(&self, actor: &Actor, dataset: &str) -> Result<ReadScope> {
        let value = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let generation = generation(&value)?;
        let active_only = self
            .store
            .effective_dataset_permission(actor, &value)
            .await?
            < 2;
        Ok(ReadScope {
            dataset: value,
            generation,
            active_only,
        })
    }
    pub(super) async fn require_read_generation(
        &self,
        actor: &Actor,
        dataset: &str,
        expected: &(String, Value),
    ) -> Result<()> {
        let current = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        if generation(&current)? != *expected {
            return Err(IngestError::Validation(CONTENT_CHANGED.into()));
        }
        Ok(())
    }
    pub async fn list_documents(
        &self,
        actor: &Actor,
        dataset: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Value>> {
        let scope = self.read_scope(actor, dataset).await?;
        let rows = self
            .store
            .list_documents_scoped(
                dataset,
                limit.clamp(1, DOCUMENT_LIST_PAGE_CAP),
                offset.max(0),
                scope.active_only,
                string(&scope.dataset, "tenant_id"),
            )
            .await?;
        self.require_read_generation(actor, dataset, &scope.generation)
            .await?;
        Ok(rows
            .into_iter()
            .filter(|r| {
                crate::migration_knowledge_query::document_acl::allowed(
                    string(r, "source_type"),
                    &r["metadata"],
                    Some(actor),
                )
            })
            .map(with_display_status)
            .collect())
    }
    pub async fn list_documents_page(
        &self,
        actor: &Actor,
        dataset: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Value> {
        let scope = self.read_scope(actor, dataset).await?;
        let limit = limit.clamp(1, DOCUMENT_LIST_PAGE_CAP);
        let offset = offset.max(0);
        let tenant = string(&scope.dataset, "tenant_id");
        let total = self
            .store
            .count_documents_scoped(dataset, scope.active_only, tenant)
            .await?;
        let rows = self
            .store
            .list_documents_scoped(dataset, limit, offset, scope.active_only, tenant)
            .await?;
        self.require_read_generation(actor, dataset, &scope.generation)
            .await?;
        Ok(
            json!({"items":rows.into_iter().filter(|r|crate::migration_knowledge_query::document_acl::allowed(string(r,"source_type"),&r["metadata"],Some(actor))).map(with_display_status).collect::<Vec<_>>(),"total":total,"limit":limit,"offset":offset}),
        )
    }
    pub async fn count_documents(&self, actor: &Actor, dataset: &str) -> Result<i64> {
        // Source count-only methods apply authorization/visibility but do not
        // acquire the list page's before/after content-generation fence.
        let value = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let active = self
            .store
            .effective_dataset_permission(actor, &value)
            .await?
            < 2;
        self.store
            .count_documents_scoped(dataset, active, string(&value, "tenant_id"))
            .await
    }
    pub async fn list_all_document_ids(&self, actor: &Actor, dataset: &str) -> Result<Vec<String>> {
        let scope = self.read_scope(actor, dataset).await?;
        let ids = self
            .store
            .list_document_ids_scoped(
                dataset,
                scope.active_only,
                string(&scope.dataset, "tenant_id"),
            )
            .await?;
        self.require_read_generation(actor, dataset, &scope.generation)
            .await?;
        Ok(ids)
    }
    pub async fn get_document(
        &self,
        actor: &Actor,
        dataset: &str,
        document: &str,
    ) -> Result<Value> {
        let value = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let expected = generation(&value)?;
        let document_value = self
            .store
            .get_document(dataset, document)
            .await
            .map_err(|error| {
                if error == IngestError::NotFound {
                    IngestError::Validation("document not found".into())
                } else {
                    error
                }
            })?;
        self.require_read_generation(actor, dataset, &expected)
            .await?;
        if self
            .store
            .effective_dataset_permission(actor, &value)
            .await?
            < 2
            && !self
                .store
                .filter_active_document_ids(
                    dataset,
                    string(&value, "tenant_id"),
                    &[document.to_owned()],
                )
                .await?
                .contains(document)
        {
            return Err(IngestError::Validation("document not found".into()));
        }
        if !crate::migration_knowledge_query::document_acl::allowed(
            string(&document_value, "source_type"),
            &document_value["metadata"],
            Some(actor),
        ) {
            return Err(IngestError::NotFound);
        }
        Ok(with_display_status(document_value))
    }
    pub async fn get_active_document_source(
        &self,
        actor: &Actor,
        dataset: &str,
        document: &str,
    ) -> Result<Value> {
        self.get_document(actor, dataset, document).await?;
        let value = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let tenant = string(&value, "tenant_id").trim();
        let dataset = dataset.trim();
        let document = document.trim();
        if tenant.is_empty()
            || dataset.is_empty()
            || document.is_empty()
            || self
                .store
                .filter_active_document_ids(dataset, tenant, &[document.to_owned()])
                .await?
                != BTreeSet::from([document.to_owned()])
        {
            return Err(IngestError::Validation("Document not found".into()));
        }
        Ok(with_display_status(
            self.store.get_document(dataset, document).await?,
        ))
    }
    pub async fn list_segments(
        &self,
        actor: &Actor,
        dataset: &str,
        document: Option<&str>,
        query: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Value>> {
        let scope = self.read_scope(actor, dataset).await?;
        let rows = self
            .store
            .list_segments_scoped(
                dataset,
                document,
                query,
                limit.clamp(1, SEGMENT_LIST_PAGE_CAP),
                offset.max(0),
                scope.active_only,
                string(&scope.dataset, "tenant_id"),
            )
            .await?;
        self.require_read_generation(actor, dataset, &scope.generation)
            .await?;
        Ok(rows)
    }
    pub async fn list_segments_page(
        &self,
        actor: &Actor,
        dataset: &str,
        document: Option<&str>,
        query: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Value> {
        let scope = self.read_scope(actor, dataset).await?;
        let limit = limit.clamp(1, SEGMENT_LIST_PAGE_CAP);
        let offset = offset.max(0);
        let tenant = string(&scope.dataset, "tenant_id");
        let total = self
            .store
            .count_segments_scoped(dataset, document, query, scope.active_only, tenant)
            .await?;
        let rows = self
            .store
            .list_segments_scoped(
                dataset,
                document,
                query,
                limit,
                offset,
                scope.active_only,
                tenant,
            )
            .await?;
        self.require_read_generation(actor, dataset, &scope.generation)
            .await?;
        Ok(json!({"items":rows,"total":total,"limit":limit,"offset":offset}))
    }
    pub async fn count_segments(
        &self,
        actor: &Actor,
        dataset: &str,
        document: Option<&str>,
        query: Option<&str>,
    ) -> Result<i64> {
        let value = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let active = self
            .store
            .effective_dataset_permission(actor, &value)
            .await?
            < 2;
        self.store
            .count_segments_scoped(
                dataset,
                document,
                query,
                active,
                string(&value, "tenant_id"),
            )
            .await
    }
    pub async fn get_document_statistics(
        &self,
        actor: &Actor,
        dataset: &str,
        document: &str,
    ) -> Result<Value> {
        let value = self
            .store
            .require_dataset_access(actor, dataset, "viewer")
            .await?;
        let expected = generation(&value)?;
        self.get_document(actor, dataset, document).await?;
        let stats:Value=sqlx::query_scalar("SELECT to_jsonb(result) FROM (SELECT d.document_id, COALESCE(d.word_count,0) AS word_count,d.status,d.enabled,d.archived,COUNT(s.segment_id)::bigint AS segment_count,COALESCE(SUM(s.hit_count),0)::bigint AS hit_count FROM knowledge.documents AS d LEFT JOIN knowledge.segments AS s ON s.dataset_id=d.dataset_id AND s.document_id=d.document_id WHERE d.dataset_id=$1 AND d.document_id=$2 GROUP BY d.document_id,d.word_count,d.status,d.enabled,d.archived) result")
            .bind(dataset).bind(document).fetch_optional(crate::database::executor(&self.store.pool)).await?.ok_or_else(||IngestError::Validation("document not found".into()))?;
        self.require_read_generation(actor, dataset, &expected)
            .await?;
        Ok(stats)
    }
}

fn require_active_tenant(active: bool, tenant: &str) -> Result<()> {
    if active && tenant.trim().is_empty() {
        return Err(IngestError::Validation(
            "tenant_id is required for active content listing".into(),
        ));
    }
    Ok(())
}
fn document_filters(
    builder: &mut QueryBuilder<'_, Postgres>,
    dataset: &str,
    active: bool,
    tenant: &str,
) {
    builder
        .push(" WHERE d.dataset_id=")
        .push_bind(dataset.to_owned());
    builder.push(crate::migration_knowledge_query::document_acl::current_predicate());
    if active {
        builder.push(" AND COALESCE(d.enabled,TRUE)=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND NOT(COALESCE(d.metadata,'{}'::jsonb) ? '_document_lifecycle_reindex') AND EXISTS(SELECT 1 FROM knowledge.datasets ds WHERE ds.dataset_id=d.dataset_id AND ds.tenant_id=").push_bind(tenant.to_owned()).push(" AND ds.is_deleted=FALSE)");
    }
}
fn segment_filters(
    builder: &mut QueryBuilder<'_, Postgres>,
    dataset: &str,
    document: Option<&str>,
    query: Option<&str>,
    active: bool,
    tenant: &str,
) {
    builder
        .push(" WHERE s.dataset_id=")
        .push_bind(dataset.to_owned());
    if active {
        builder.push(" AND COALESCE(s.enabled,TRUE)=TRUE AND s.status='completed' AND EXISTS(SELECT 1 FROM knowledge.documents d JOIN knowledge.datasets ds ON ds.dataset_id=d.dataset_id WHERE d.document_id=s.document_id AND d.dataset_id=s.dataset_id AND ds.tenant_id=").push_bind(tenant.to_owned()).push(" AND ds.is_deleted=FALSE AND COALESCE(d.enabled,TRUE)=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND NOT(COALESCE(d.metadata,'{}'::jsonb) ? '_document_lifecycle_reindex'))");
    }
    builder.push(" AND EXISTS(SELECT 1 FROM knowledge.documents d WHERE d.document_id=s.document_id AND d.dataset_id=s.dataset_id ").push(crate::migration_knowledge_query::document_acl::current_predicate()).push(")");
    if let Some(document) = document.filter(|s| !s.is_empty()) {
        builder
            .push(" AND s.document_id=")
            .push_bind(document.to_owned());
    }
    if let Some(query) = query.filter(|s| !s.is_empty()) {
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        builder
            .push(" AND s.text ILIKE ")
            .push_bind(format!("%{escaped}%"));
    }
}
impl KnowledgeWriteStore {
    pub async fn list_documents_scoped(
        &self,
        dataset: &str,
        limit: i64,
        offset: i64,
        active: bool,
        tenant: &str,
    ) -> Result<Vec<Value>> {
        require_active_tenant(active, tenant)?;
        let mut query = QueryBuilder::<Postgres>::new(
            "SELECT to_jsonb(result) FROM (SELECT d.document_id,d.dataset_id,d.title,d.source_type,d.source_uri,d.mime_type,d.size_bytes,d.status,d.progress,d.error,d.metadata,d.enabled,d.disabled_at,d.archived,d.archived_reason,d.archived_at,d.parsing_started_at,d.splitting_started_at,d.indexing_started_at,d.started_at,d.completed_at,d.created_at,d.updated_at,COALESCE(h.hit_count,0)::bigint AS hit_count FROM knowledge.documents AS d LEFT JOIN(SELECT document_id,SUM(hit_count)::bigint AS hit_count FROM knowledge.segments WHERE dataset_id=",
        );
        query
            .push_bind(dataset.to_owned())
            .push(" GROUP BY document_id) AS h ON h.document_id=d.document_id");
        document_filters(&mut query, dataset, active, tenant);
        query
            .push(" ORDER BY d.created_at DESC LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset)
            .push(") result");
        Ok(query
            .build_query_scalar()
            .fetch_all(crate::database::executor(&self.pool))
            .await?)
    }
    pub async fn count_documents_scoped(
        &self,
        dataset: &str,
        active: bool,
        tenant: &str,
    ) -> Result<i64> {
        require_active_tenant(active, tenant)?;
        let mut query =
            QueryBuilder::<Postgres>::new("SELECT COUNT(*) FROM knowledge.documents AS d");
        document_filters(&mut query, dataset, active, tenant);
        Ok(query
            .build_query_scalar()
            .fetch_one(crate::database::executor(&self.pool))
            .await?)
    }
    pub async fn list_document_ids_scoped(
        &self,
        dataset: &str,
        active: bool,
        tenant: &str,
    ) -> Result<Vec<String>> {
        require_active_tenant(active, tenant)?;
        let mut query =
            QueryBuilder::<Postgres>::new("SELECT d.document_id FROM knowledge.documents AS d");
        document_filters(&mut query, dataset, active, tenant);
        Ok(query
            .build_query_scalar()
            .fetch_all(crate::database::executor(&self.pool))
            .await?)
    }
    pub async fn filter_active_document_ids(
        &self,
        dataset: &str,
        tenant: &str,
        ids: &[String],
    ) -> Result<BTreeSet<String>> {
        let normalized: BTreeSet<String> = ids
            .iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        if normalized.is_empty() {
            return Ok(normalized);
        }
        if dataset.trim().is_empty() || tenant.trim().is_empty() {
            return Err(IngestError::Validation(
                "dataset_id and tenant_id are required for active document filtering".into(),
            ));
        }
        let ids:Vec<String>=sqlx::query_scalar(&format!("SELECT DISTINCT d.document_id FROM knowledge.documents AS d JOIN knowledge.datasets AS ds ON ds.dataset_id=d.dataset_id WHERE d.dataset_id=$1 AND ds.tenant_id=$2 AND ds.is_deleted=FALSE AND d.document_id=ANY($3::text[]) AND COALESCE(d.enabled,TRUE)=TRUE AND COALESCE(d.archived,FALSE)=FALSE AND NOT(COALESCE(d.metadata,'{{}}'::jsonb) ? '_document_lifecycle_reindex'){}",crate::migration_knowledge_query::document_acl::current_predicate()))
            .bind(dataset.trim()).bind(tenant.trim()).bind(normalized.into_iter().collect::<Vec<_>>()).fetch_all(crate::database::executor(&self.pool)).await?;
        Ok(ids
            .into_iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect())
    }
    pub async fn list_segments_scoped(
        &self,
        dataset: &str,
        document: Option<&str>,
        query_text: Option<&str>,
        limit: i64,
        offset: i64,
        active: bool,
        tenant: &str,
    ) -> Result<Vec<Value>> {
        require_active_tenant(active, tenant)?;
        let mut query =
            QueryBuilder::<Postgres>::new("SELECT to_jsonb(s) FROM knowledge.segments AS s");
        segment_filters(&mut query, dataset, document, query_text, active, tenant);
        query.push(" ORDER BY s.document_id ASC,s.position ASC,s.content_type ASC,s.segment_id ASC LIMIT ").push_bind(limit).push(" OFFSET ").push_bind(offset);
        Ok(query
            .build_query_scalar()
            .fetch_all(crate::database::executor(&self.pool))
            .await?)
    }
    pub async fn count_segments_scoped(
        &self,
        dataset: &str,
        document: Option<&str>,
        query_text: Option<&str>,
        active: bool,
        tenant: &str,
    ) -> Result<i64> {
        require_active_tenant(active, tenant)?;
        let mut query =
            QueryBuilder::<Postgres>::new("SELECT COUNT(*) FROM knowledge.segments AS s");
        segment_filters(&mut query, dataset, document, query_text, active, tenant);
        Ok(query
            .build_query_scalar()
            .fetch_one(crate::database::executor(&self.pool))
            .await?)
    }
}
