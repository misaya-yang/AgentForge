//! Fair restart-safe document batches; idempotent producers reuse their original
//! snapshot and cancelled operations cannot create fresh claims.
use super::{Actor, IngestError, IngestionService, Result, batch_sql as sql, integer, string};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use std::collections::BTreeSet;
#[derive(Clone)]
pub struct DocumentBatchStore {
    pub pool: PgPool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaimedDocumentBatchItem {
    pub operation_id: String,
    pub tenant_id: String,
    pub dataset_id: String,
    pub operation: String,
    pub document_id: String,
    pub created_by: String,
    pub actor_roles: Vec<String>,
}
fn row_json(sql: &str) -> String {
    format!("SELECT to_jsonb(q) FROM ({sql}) q")
}
/// Admit only exact-target recovery without clearing the durable fence.
/// The HTTP caller must authorize editor access before selecting a retry.
pub(super) fn deletion_selection(dataset: &Value, document_ids: &[String]) -> Result<Vec<String>> {
    let Some(fence) = dataset["index_config"]["retrieval"].get("_index_deletion_fence") else {
        super::require_index_writable(dataset)?;
        return Ok(document_ids.to_vec());
    };
    let target = string(fence, "target_id");
    let expected =
        json!({"operation":"document_delete","target_id":target,"status":"pending","version":1});
    if target.trim().is_empty()
        || fence != &expected
        || !document_ids.iter().any(|id| id.trim() == target)
    {
        return Err(IngestError::Validation(
            "batch deletion recovery must include the pending document deletion target".into(),
        ));
    }
    // Validate the remaining index configuration on a copy. Persistent retrieval
    // remains fenced until delete_document finishes the exact cleanup under lease.
    let mut validation_dataset = dataset.clone();
    validation_dataset["index_config"]["retrieval"]
        .as_object_mut()
        .expect("validated deletion fence has an object parent")
        .remove("_index_deletion_fence");
    super::require_index_writable(&validation_dataset)?;
    let mut selected = vec![target.to_owned()];
    selected.extend(
        document_ids
            .iter()
            .filter(|id| id.trim() != target)
            .cloned(),
    );
    Ok(selected)
}

impl DocumentBatchStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn create_operation(
        &self,
        tenant: &str,
        dataset: &str,
        operation: &str,
        created_by: &str,
        roles: &[String],
        document_ids: &[String],
        all_documents: bool,
        operation_id: Option<&str>,
    ) -> Result<Value> {
        if !["reembed", "delete"].contains(&operation) {
            return Err(IngestError::Validation(
                "unsupported document batch operation".into(),
            ));
        }
        if all_documents && operation != "reembed" {
            return Err(IngestError::Validation(
                "all_documents is supported only for reembed".into(),
            ));
        }
        let mut seen = BTreeSet::new();
        let ids: Vec<String> = document_ids
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .filter(|s| seen.insert(s.to_string()))
            .map(str::to_string)
            .collect();
        if !all_documents && ids.is_empty() {
            return Err(IngestError::Validation(
                "document_ids must not be empty".into(),
            ));
        }
        let id = operation_id
            .map(str::trim)
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        uuid::Uuid::parse_str(&id)
            .map_err(|_| IngestError::Validation("operation_id must be a UUID".into()))?;
        let roles: Vec<_> = roles
            .iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut tx = self.pool.begin().await?;
        crate::database::bind_current(&mut tx).await?;
        let d: Value = sqlx::query_scalar(&row_json(sql::CREATE_OPERATION_0))
            .bind(dataset)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(IngestError::NotFound)?;
        if string(&d, "tenant_id") != tenant {
            return Err(IngestError::NotFound);
        }
        let inserted: Option<bool> = sqlx::query_scalar(sql::CREATE_OPERATION_1)
            .bind(&id)
            .bind(tenant)
            .bind(dataset)
            .bind(operation)
            .bind(created_by)
            .bind(json!(roles))
            .fetch_optional(&mut *tx)
            .await?;
        if inserted != Some(true) {
            let existing: Value = sqlx::query_scalar(&row_json(sql::CREATE_OPERATION_3))
                .bind(&id)
                .fetch_one(&mut *tx)
                .await?;
            if string(&existing, "tenant_id") != tenant
                || string(&existing, "dataset_id") != dataset
                || string(&existing, "operation") != operation
            {
                return Err(IngestError::Validation(
                    "operation_id is already bound to another job".into(),
                ));
            }
            if string(&existing, "status") == "failed" {
                let claiming: i64 = sqlx::query_scalar(sql::CREATE_OPERATION_10)
                    .bind(&id)
                    .fetch_one(&mut *tx)
                    .await?;
                if claiming > 0 {
                    return Err(IngestError::Validation(
                        "cancelled document batch is still draining an owned claim".into(),
                    ));
                }
                sqlx::query(sql::CREATE_OPERATION_6)
                    .bind(&id)
                    .execute(&mut *tx)
                    .await?;
                let counts: Value = sqlx::query_scalar(&row_json(sql::CREATE_OPERATION_7))
                    .bind(&id)
                    .fetch_one(&mut *tx)
                    .await?;
                sqlx::query(sql::CREATE_OPERATION_8)
                    .bind(&id)
                    .bind(integer(&counts, "queued"))
                    .bind(integer(&counts, "skipped"))
                    .bind(integer(&counts, "failed"))
                    .bind(integer(&counts, "pending"))
                    .execute(&mut *tx)
                    .await?;
            }
        } else {
            if all_documents {
                sqlx::query(sql::CREATE_OPERATION_4)
                    .bind(&id)
                    .bind(dataset)
                    .execute(&mut *tx)
                    .await?;
            } else {
                for (ordinal, document) in ids.iter().enumerate() {
                    sqlx::query(sql::CREATE_OPERATION_5)
                        .bind(&id)
                        .bind(document)
                        .bind(ordinal as i64)
                        .execute(&mut *tx)
                        .await?;
                }
            }
            let total: i64 = sqlx::query_scalar(sql::CREATE_OPERATION_9)
                .bind(&id)
                .fetch_one(&mut *tx)
                .await?;
            sqlx::query(sql::CREATE_OPERATION_2)
                .bind(&id)
                .bind(total)
                .bind(if total == 0 { "completed" } else { "pending" })
                .execute(&mut *tx)
                .await?;
        }
        let result = Self::get_on(&mut tx, &id, tenant, dataset)
            .await?
            .ok_or(IngestError::Database)?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn get_operation(
        &self,
        id: &str,
        tenant: &str,
        dataset: &str,
    ) -> Result<Option<Value>> {
        Self::get_on(&mut *self.pool.acquire().await?, id, tenant, dataset).await
    }
    async fn get_on(
        conn: &mut PgConnection,
        id: &str,
        tenant: &str,
        dataset: &str,
    ) -> Result<Option<Value>> {
        let Some(mut value): Option<Value> =
            sqlx::query_scalar(&row_json(sql::GET_OPERATION_ON_CONNECTION_0))
                .bind(id)
                .bind(tenant)
                .bind(dataset)
                .fetch_optional(crate::database::connection(&mut *conn))
                .await?
        else {
            return Ok(None);
        };
        let failures: Vec<Value> =
            sqlx::query_scalar(&row_json(sql::GET_OPERATION_ON_CONNECTION_1))
                .bind(id)
                .fetch_all(crate::database::connection(&mut *conn))
                .await?;
        value["problem_items_truncated"] = json!(
            integer(&value, "skipped_count") + integer(&value, "failed_count")
                > failures.len() as i64
        );
        value["problem_items"] = json!(failures);
        Ok(Some(value))
    }
    pub async fn cancel_operation(
        &self,
        id: &str,
        tenant: &str,
        dataset: &str,
    ) -> Result<Option<Value>> {
        let mut tx = self.pool.begin().await?;
        crate::database::bind_current(&mut tx).await?;
        let Some(current): Option<Value> = sqlx::query_scalar(&row_json(sql::CANCEL_OPERATION_0))
            .bind(id)
            .bind(tenant)
            .bind(dataset)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(None);
        };
        if !(string(&current, "status") == "failed"
            && string(&current, "error") == "cancelled by operator")
        {
            if ["completed", "partial", "failed"].contains(&string(&current, "status")) {
                return Err(IngestError::Validation(format!(
                    "document batch in state '{}' cannot be cancelled",
                    string(&current, "status")
                )));
            }
            sqlx::query(sql::CANCEL_OPERATION_1)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            let counts: Value = sqlx::query_scalar(&row_json(sql::CANCEL_OPERATION_2))
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
            sqlx::query(sql::CANCEL_OPERATION_3)
                .bind(id)
                .bind(integer(&counts, "queued"))
                .bind(integer(&counts, "skipped"))
                .bind(integer(&counts, "failed"))
                .execute(&mut *tx)
                .await?;
        }
        let result = Self::get_on(&mut tx, id, tenant, dataset).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn claim_next_item(
        &self,
        worker: &str,
        stale_seconds: i32,
    ) -> Result<Option<ClaimedDocumentBatchItem>> {
        if crate::database::control_enabled(
            &self.pool,
            "knowledge.claim_document_batch_item_v1(text,double precision)",
        )
        .await?
        {
            let row: Option<Value> =
                sqlx::query_scalar("SELECT knowledge.claim_document_batch_item_v1($1,$2)")
                    .bind(worker)
                    .bind(f64::from(stale_seconds.max(1)))
                    .fetch_one(crate::database::executor(&self.pool))
                    .await?;
            return row
                .map(serde_json::from_value)
                .transpose()
                .map_err(Into::into);
        }
        let mut tx = self.pool.begin().await?;
        crate::database::bind_current(&mut tx).await?;
        let Some(op): Option<Value> = sqlx::query_scalar(&row_json(sql::CLAIM_NEXT_ITEM_0))
            .bind(stale_seconds.max(1) as f64)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(None);
        };
        let id = string(&op, "operation_id");
        let Some(document): Option<String> = sqlx::query_scalar(sql::CLAIM_NEXT_ITEM_1)
            .bind(id)
            .bind(stale_seconds.max(1) as f64)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(None);
        };
        sqlx::query(sql::CLAIM_NEXT_ITEM_2)
            .bind(id)
            .bind(&document)
            .bind(worker)
            .execute(&mut *tx)
            .await?;
        sqlx::query(sql::CLAIM_NEXT_ITEM_3)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(ClaimedDocumentBatchItem {
            operation_id: id.into(),
            tenant_id: string(&op, "tenant_id").into(),
            dataset_id: string(&op, "dataset_id").into(),
            operation: string(&op, "operation").into(),
            document_id: document,
            created_by: string(&op, "created_by").into(),
            actor_roles: op["actor_roles"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        }))
    }
    pub async fn complete_item(
        &self,
        id: &str,
        document: &str,
        status: &str,
        error_code: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        if !["queued", "skipped", "failed"].contains(&status) {
            return Err(IngestError::Validation(
                "invalid document batch item terminal status".into(),
            ));
        }
        let safe_error = error
            .map(|s| s.trim().chars().take(2000).collect::<String>())
            .filter(|s| !s.is_empty());
        let safe_code = error_code
            .map(|s| s.trim().chars().take(64).collect::<String>())
            .filter(|s| !s.is_empty());
        let mut tx = self.pool.begin().await?;
        crate::database::bind_current(&mut tx).await?;
        let changed: Option<i32> = sqlx::query_scalar(sql::COMPLETE_ITEM_0)
            .bind(id)
            .bind(document)
            .bind(status)
            .bind(safe_code)
            .bind(safe_error)
            .fetch_optional(&mut *tx)
            .await?;
        if changed.is_none() {
            tx.commit().await?;
            return Ok(());
        }
        let counts: Value = sqlx::query_scalar(&row_json(sql::COMPLETE_ITEM_1))
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        let queued = integer(&counts, "queued");
        let skipped = integer(&counts, "skipped");
        let failed = integer(&counts, "failed");
        let remaining = integer(&counts, "remaining");
        let state = if remaining > 0 {
            "running"
        } else if queued > 0 && skipped == 0 && failed == 0 {
            "completed"
        } else if queued > 0 || skipped > 0 {
            "partial"
        } else {
            "failed"
        };
        sqlx::query(sql::COMPLETE_ITEM_2)
            .bind(id)
            .bind(state)
            .bind(queued)
            .bind(skipped)
            .bind(failed)
            .bind(remaining)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn release_item(&self, id: &str, document: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        crate::database::bind_current(&mut tx).await?;
        sqlx::query(sql::RELEASE_ITEM_0)
            .bind(id)
            .bind(document)
            .execute(&mut *tx)
            .await?;
        sqlx::query(sql::RELEASE_ITEM_1)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
pub async fn dispatch_one(
    service: &IngestionService,
    store: &DocumentBatchStore,
    worker: &str,
) -> Result<bool> {
    let Some(item) = store.claim_next_item(worker, 300).await? else {
        return Ok(false);
    };
    crate::database::with_job_scope(&item.tenant_id, async {
        let result = async {
            let authority = service
                .vector_job_actor_authority
                .as_ref()
                .ok_or(IngestError::Permission)?;
            let actor = authority.resolve(&item.tenant_id, &item.created_by).await?;
            ai_platform_service_primitives::knowledge_model::with_actor_scope(
                actor.clone(),
                async {
                    service
                        .store
                        .require_dataset_access(&actor, &item.dataset_id, "editor")
                        .await?;
                    if item.operation == "reembed" {
                        service
                            .store
                            .enqueue(
                                &item.dataset_id,
                                &item.document_id,
                                Some("reembed"),
                                None,
                                None,
                                false,
                            )
                            .await
                    } else {
                        service
                            .delete_document(&actor, &item.dataset_id, &item.document_id)
                            .await
                    }
                },
            )
            .await
        }
        .await;
        match result {
            Ok(true) => {
                store
                    .complete_item(&item.operation_id, &item.document_id, "queued", None, None)
                    .await?
            }
            Ok(false) => {
                store
                    .complete_item(
                        &item.operation_id,
                        &item.document_id,
                        "skipped",
                        Some("not_found"),
                        None,
                    )
                    .await?
            }
            Err(IngestError::LeaseBusy) => {
                store
                    .release_item(&item.operation_id, &item.document_id)
                    .await?
            }
            Err(e) => {
                store
                    .complete_item(
                        &item.operation_id,
                        &item.document_id,
                        "failed",
                        Some("operation_failed"),
                        Some(&e.to_string()),
                    )
                    .await?
            }
        }
        Ok(true)
    })
    .await?
}

#[cfg(test)]
mod deletion_recovery_tests {
    use super::*;
    use crate::migration_knowledge_query::Dataset;

    fn dataset() -> Value {
        json!({"dataset_id":"dataset-a","tenant_id":"tenant-a","name":"test",
            "is_deleted":false,"needs_reindex":false,"content_revision":3,
            "index_config":{"retrieval":{}},"embedding_config":{}})
    }
    fn fence(dataset: &mut Value, operation: &str, target: &str) {
        dataset["index_config"]["retrieval"]["_index_deletion_fence"] =
            json!({"operation":operation,"target_id":target,"status":"pending","version":1});
    }
    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ordinary_delete_selection_preserves_order() {
        assert_eq!(
            deletion_selection(&dataset(), &ids(&["b", "a"])).unwrap(),
            ids(&["b", "a"])
        );
    }

    #[test]
    fn retry_prioritizes_exact_document_without_mutating_fence_or_reopening_reads() {
        let mut d = dataset();
        fence(&mut d, "document_delete", "a");
        let before = d.clone();
        assert!(super::super::require_index_writable(&d).is_err());
        assert_eq!(
            deletion_selection(&d, &ids(&["b", "a", "c"])).unwrap(),
            ids(&["a", "b", "c"])
        );
        assert_eq!(d, before);
        let typed: Dataset = serde_json::from_value(d).unwrap();
        assert!(typed.generation().is_err());
    }

    #[test]
    fn retry_matches_store_normalization_but_does_not_add_unrequested_documents() {
        let mut d = dataset();
        fence(&mut d, "document_delete", "a");
        assert_eq!(deletion_selection(&d, &ids(&[" a "])).unwrap(), ids(&["a"]));
        assert!(deletion_selection(&d, &ids(&["b"])).is_err());
        assert!(deletion_selection(&d, &[]).is_err());
    }

    #[test]
    fn other_operations_and_malformed_fences_fail_closed() {
        for operation in ["segment_delete", "dataset_delete", "unknown"] {
            let mut d = dataset();
            fence(&mut d, operation, "a");
            assert!(deletion_selection(&d, &ids(&["a"])).is_err());
        }
        for malformed in [
            Value::Null,
            json!(false),
            json!({}),
            json!({"operation":"document_delete","target_id":"a","status":"done","version":1}),
            json!({"operation":"document_delete","target_id":"a","status":"pending","version":2}),
            json!({"operation":"document_delete","target_id":"a","status":"pending","version":1,"extra":true}),
            json!({"operation":"document_delete","target_id":" ","status":"pending","version":1}),
        ] {
            let mut d = dataset();
            d["index_config"]["retrieval"]["_index_deletion_fence"] = malformed;
            assert!(deletion_selection(&d, &ids(&["a", " "])).is_err());
        }
    }

    #[test]
    fn recovery_still_rejects_deleted_dataset_and_invalid_index_configuration() {
        let mut d = dataset();
        fence(&mut d, "document_delete", "a");
        d["is_deleted"] = json!(true);
        assert!(deletion_selection(&d, &ids(&["a"])).is_err());
        d["is_deleted"] = json!(false);
        d["index_config"]["chunking"] = json!({"mode":"unsupported-mode"});
        assert!(deletion_selection(&d, &ids(&["a"])).is_err());
    }

    #[test]
    fn completed_cleanup_returns_to_ordinary_admission() {
        let mut d = dataset();
        fence(&mut d, "document_delete", "a");
        assert!(deletion_selection(&d, &ids(&["b"])).is_err());
        d["index_config"]["retrieval"]
            .as_object_mut()
            .unwrap()
            .remove("_index_deletion_fence");
        d["content_revision"] = json!(5);
        assert_eq!(deletion_selection(&d, &ids(&["b"])).unwrap(), ids(&["b"]));
        let typed: Dataset = serde_json::from_value(d).unwrap();
        assert!(typed.generation().is_ok());
    }
}
