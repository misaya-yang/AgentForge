//! Source-aware last-reference removal under the exclusive dataset lifecycle lease.
use super::sync::Execution;
use super::*;
use sqlx::Connection;
impl Sources {
    pub async fn detach_source(&self, e: &Execution, page: &Value) -> Result<bool> {
        let actor = self
            .guard_deletion(e, Some(text(page, "document_id")))
            .await?;
        let dataset = text(&e.binding, "dataset_id");
        let id = text(page, "document_id");
        let record = text(page, "id");
        let page_id = text(page, "page_id");
        if record.is_empty()
            || page_id.is_empty()
            || text(page, "binding_id") != text(&e.binding, "binding_id")
        {
            return Err(Error::conflict(
                "Confluence source ownership changed; removal refused",
            ));
        }
        let mut lease = self
            .ingestion
            .store
            .dataset_delete_lease(dataset)
            .await
            .map_err(|_| Error::conflict("Dataset lifecycle lease is busy"))?;
        self.guard_deletion_on(e, lease.connection(), Some(id))
            .await?;
        let current: Option<Value> =
            sqlx::query_scalar("SELECT to_jsonb(p) FROM confluence_pages p WHERE id=$1")
                .bind(record)
                .fetch_optional(ai_platform_knowledge_domain::database::connection(
                    lease.connection(),
                ))
                .await?;
        let Some(current) = current else {
            lease
                .release()
                .await
                .map_err(|_| Error::unavailable("Dataset lease release failed"))?;
            return Ok(false);
        };
        for key in [
            "id",
            "binding_id",
            "page_id",
            "document_id",
            "version",
            "content_hash",
            "updated_at",
        ] {
            if current[key] != page[key] {
                return Err(Error::conflict(
                    "Confluence source ownership changed; removal refused",
                ));
            }
        }
        let doc: Option<Value> =
            sqlx::query_scalar("SELECT to_jsonb(d) FROM documents d WHERE document_id=$1")
                .bind(id)
                .fetch_optional(ai_platform_knowledge_domain::database::connection(
                    lease.connection(),
                ))
                .await?;
        let shared: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM confluence_pages WHERE document_id=$1 AND id<>$2)",
        )
        .bind(id)
        .bind(record)
        .fetch_one(ai_platform_knowledge_domain::database::connection(
            lease.connection(),
        ))
        .await?;
        if id.is_empty() || shared {
            if id.is_empty() && doc.is_some() {
                return Err(Error::conflict(
                    "Confluence source ownership changed; removal refused",
                ));
            }
            delete_record(lease.connection(), page).await?;
            lease
                .release()
                .await
                .map_err(|_| Error::unavailable("Dataset lease release failed"))?;
            return Ok(false);
        }
        let doc = doc.ok_or_else(|| {
            Error::conflict("Confluence source ownership changed; removal refused")
        })?;
        let canonical = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            format!(
                "ai-platform:confluence:{dataset}:{}:{page_id}",
                text(&e.binding, "connection_id")
            )
            .as_bytes(),
        )
        .to_string();
        let legacy = text(&doc, "confluence_binding_id") == text(&e.binding, "binding_id")
            && text(&doc, "confluence_page_id") == page_id;
        if text(&doc, "dataset_id") != dataset
            || doc["source_type"] != "confluence"
            || (id != canonical && !legacy)
            || !doc["metadata"].is_object()
            || doc["metadata"]
                .get("confluence_page_id")
                .is_some_and(|v| !v.is_null() && v != page_id)
        {
            return Err(Error::conflict(
                "Confluence source ownership changed; removal refused",
            ));
        }
        let ds = ingest::store::KnowledgeWriteStore::set_deletion_fence(
            lease.connection(),
            dataset,
            "document_delete",
            id,
        )
        .await
        .map_err(|_| Error::conflict("Dataset index deletion fence changed"))?;
        let typed: query::types::Dataset = serde_json::from_value(ds)
            .map_err(|_| Error::unavailable("Dataset shape unavailable"))?;
        if typed.tenant_id != actor.tenant_id {
            return Err(Error::forbidden());
        }
        self.ingestion
            .indexer
            .vectors
            .delete_document_points(&typed, id)
            .await
            .map_err(|_| Error::unavailable("Confluence document vector removal failed"))?;
        self.ingestion
            .assets
            .delete_document_assets(&typed.tenant_id, id)
            .await
            .map_err(|_| Error::unavailable("Confluence document assets removal failed"))?;
        // Commit reference removal, document deletion and exact fence clear atomically.
        let mut tx = lease.connection().begin().await?;
        ai_platform_knowledge_domain::database::bind_current(&mut tx).await?;
        delete_record(&mut tx, page).await?;
        let deleted = sqlx::query("DELETE FROM documents WHERE document_id=$1 AND dataset_id=$2")
            .bind(id)
            .bind(dataset)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if deleted != 1 {
            return Err(Error::conflict("Confluence document deletion failed"));
        }
        let cleared = ingest::store::KnowledgeWriteStore::clear_deletion_fence(
            &mut tx,
            dataset,
            "document_delete",
            id,
        )
        .await
        .map_err(|_| Error::unavailable("Dataset deletion fence could not be cleared"))?;
        if !cleared {
            return Err(Error::conflict(
                "Confluence deletion fence could not be cleared",
            ));
        }
        tx.commit().await?;
        lease
            .release()
            .await
            .map_err(|_| Error::unavailable("Dataset lease release failed"))?;
        Ok(true)
    }
}
async fn delete_record(conn: &mut PgConnection, page: &Value) -> Result<()> {
    let n=sqlx::query("DELETE FROM confluence_pages WHERE id=$1 AND binding_id=$2 AND page_id=$3 AND document_id IS NOT DISTINCT FROM $4").bind(text(page,"id")).bind(text(page,"binding_id")).bind(text(page,"page_id")).bind(page["document_id"].as_str()).execute(ai_platform_knowledge_domain::database::connection(&mut *conn)).await?.rows_affected();
    if n != 1 {
        return Err(Error::conflict(
            "Confluence source ownership changed; removal refused",
        ));
    }
    Ok(())
}
