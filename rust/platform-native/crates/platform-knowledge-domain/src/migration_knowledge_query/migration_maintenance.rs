//! Existing retained-generation reclamation, owned by embedding lifecycle.
use super::*;
impl EmbeddingMigration {
    pub(crate) fn with_job_fence(mut self,row: Value) -> Self { self.job_fence=Some(row); self }
    pub(super) fn job_id(&self) -> Option<&str> { self.job_fence.as_ref().and_then(|row| row["job_id"].as_str()) }
    pub(super) fn check_job_value(&self,current: &Value) -> MResult<()> {
        if let Some(row)=&self.job_fence {
            crate::migration_knowledge_ingest::vector_jobs::check_expected(row,current)
                .map_err(|_| MigrationError::conflict("queued embedding publication generation changed"))?;
        }
        Ok(())
    }
    pub(super) async fn check_job_fence(&self,dataset: &str) -> MResult<()> {
        if self.job_fence.is_some() {
            self.check_job_value(&serde_json::to_value(self.query.dataset(dataset).await?).map_err(|_| MigrationError::invalid("invalid dataset snapshot"))?)?;
        }
        Ok(())
    }

    pub async fn reclaim_expired(&self, delete_collections: bool) -> MResult<Vec<String>> {
        let candidates:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(b) FROM dataset_collection_bindings b WHERE state='retained' AND retained_until<=NOW()").fetch_all(&self.store.pool).await?;
        let mut reclaimed = Vec::new();
        for b in candidates {
            let mut tx = self.store.pool.begin().await?;
            dataset_lock(&mut tx, text(&b, "dataset_id")).await?;
            let row:Option<Value>=sqlx::query_scalar("UPDATE dataset_collection_bindings SET state='retired',retired_at=NOW() WHERE binding_id=$1::uuid AND state='retained' AND retained_until<=NOW() RETURNING to_jsonb(dataset_collection_bindings)").bind(text(&b,"binding_id")).fetch_optional(&mut *tx).await?;
            tx.commit().await?;
            if row.is_none() {
                continue;
            }
            if delete_collections {
                let base = Dataset {
                    dataset_id: text(&b, "dataset_id").into(),
                    tenant_id: text(&b, "tenant_id").into(),
                    ..Default::default()
                };
                let d = binding_dataset(&base, &b);
                if self.vectors.collection_exists(d.collection()?).await? {
                    self.vectors.delete_collection(&d).await?;
                }
            }
            reclaimed.push(text(&b, "binding_id").into());
        }
        Ok(reclaimed)
    }
}
