//! Worker vector completion is a durable phase receipt. Only the API principal
//! removes control-plane ownership/history and marks the whole deletion complete.
use super::{Actor, IngestError, IngestionService, Mutation, Result, generation, identity, string};
use crate::migration_knowledge_ingest::store::KnowledgeWriteStore;
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, PgPool};
const SCHEMA: &str = "knowledge-dataset-vector-delete/v1";

pub(super) fn is_receipt(row: &Value) -> bool {
    row["state"] == "running"
        && row["result"]["schema"] == SCHEMA
        && row["payload"]["kind"] == "delete_dataset"
}
// The API has no UPDATE(result) grant. Keep the worker-owned receipt immutable
// and project the original boolean response only after API commit succeeds.
pub(super) fn result(row: &Value) -> Value {
    if row["state"] == "succeeded"
        && row["payload"]["kind"] == "delete_dataset"
        && row["result"]["schema"] == SCHEMA
    {
        json!(true)
    } else {
        row["result"].clone()
    }
}
fn require_binding(actor: &Actor, row: &Value) -> Result<()> {
    let mutation: Mutation = serde_json::from_value(row["payload"].clone())?;
    if actor.anonymous()
        || actor.tenant_id != string(row, "tenant_id")
        || actor.user_id != string(row, "requested_by")
        || !matches!(
            mutation,
            Mutation::DeleteDataset {
                confirmed: true,
                ..
            }
        )
        || string(row, "job_id")
            != identity(
                &actor.tenant_id,
                &actor.user_id,
                string(row, "dataset_id"),
                string(row, "expected_generation"),
                &row["payload"],
            )
    {
        return Err(IngestError::Permission);
    }
    Ok(())
}
fn require_receipt(actor: &Actor, row: &Value, current: &Value) -> Result<()> {
    require_binding(actor, row)?;
    let receipt = &row["result"];
    let dataset = string(row, "dataset_id");
    if !is_receipt(row)
        || receipt["job_id"] != row["job_id"]
        || receipt["expected_generation"] != row["expected_generation"]
        || string(receipt, "dataset_generation") != generation(current)
        || string(current, "dataset_id") != dataset
        || string(current, "tenant_id") != actor.tenant_id
        || current["is_deleted"] == true
        || current["index_config"]["retrieval"]["_index_deletion_fence"]
            != json!({"operation":"dataset_delete","target_id":dataset,"status":"pending","version":1})
    {
        return Err(IngestError::GenerationChanged);
    }
    super::check_expected(row, current)
}

pub(crate) async fn record_vector_deletion(
    _service: &IngestionService,
    actor: &Actor,
    expected: &Value,
    connection: &mut PgConnection,
) -> Result<()> {
    require_binding(actor, expected)?;
    let mut tx = connection.begin().await?;
    crate::database::bind_current(&mut tx).await?;
    let current:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM knowledge.datasets d WHERE dataset_id=$1 AND tenant_id=$2 AND NOT is_deleted FOR UPDATE")
        .bind(string(expected,"dataset_id")).bind(&actor.tenant_id).fetch_optional(&mut *tx).await?.ok_or(IngestError::NotFound)?;
    super::check_expected(expected, &current)?;
    let row: Value = sqlx::query_scalar(
        "SELECT to_jsonb(j) FROM knowledge.vector_mutation_jobs j WHERE job_id=$1 FOR UPDATE",
    )
    .bind(string(expected, "job_id"))
    .fetch_one(&mut *tx)
    .await?;
    require_binding(actor, &row)?;
    if row["state"] != "running"
        || row["payload"] != expected["payload"]
        || row["expected_generation"] != expected["expected_generation"]
    {
        return Err(IngestError::GenerationChanged);
    }
    let receipt = json!({"schema":SCHEMA,"job_id":row["job_id"],"expected_generation":row["expected_generation"],"dataset_generation":generation(&current)});
    sqlx::query("UPDATE knowledge.vector_mutation_jobs SET result=$2,error_class=NULL,updated_at=NOW() WHERE job_id=$1 AND state='running'")
        .bind(string(&row,"job_id")).bind(receipt).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

// Serialize admission with the worker's deletion fence and the API finalizer.
// A retry after fence installation must keep the original durable job identity.
pub(super) async fn enqueue_delete(
    pool: &PgPool,
    actor: &Actor,
    dataset: &str,
    payload: &Value,
) -> Result<Option<String>> {
    let store = KnowledgeWriteStore::new(pool.clone());
    let mut lease = store.dataset_delete_lease(dataset).await?;
    let mut tx = lease.connection().begin().await?;
    crate::database::bind_current(&mut tx).await?;
    let current:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM knowledge.datasets d WHERE dataset_id=$1 AND tenant_id=$2 FOR UPDATE")
        .bind(dataset).bind(&actor.tenant_id).fetch_optional(&mut *tx).await?.ok_or(IngestError::NotFound)?;
    if current["is_deleted"] == true {
        let done:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(j) FROM knowledge.vector_mutation_jobs j WHERE dataset_id=$1 AND tenant_id=$2 AND requested_by=$3 AND state='succeeded' AND payload->>'kind'='delete_dataset' ORDER BY created_at DESC LIMIT 1")
            .bind(dataset).bind(&actor.tenant_id).bind(&actor.user_id).fetch_optional(&mut *tx).await?;
        if let Some(row) = done {
            require_binding(actor, &row)?;
            if current["deleted_by"] == actor.user_id && result(&row) == true {
                tx.commit().await?;
                lease.release().await?;
                return Ok(None);
            }
        }
        return Err(IngestError::NotFound);
    }
    let candidates:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(j) FROM knowledge.vector_mutation_jobs j WHERE dataset_id=$1 AND tenant_id=$2 AND requested_by=$3 AND payload=$4 AND state IN ('pending','running','failed') ORDER BY created_at DESC LIMIT 32 FOR UPDATE")
        .bind(dataset).bind(&actor.tenant_id).bind(&actor.user_id).bind(payload).fetch_all(&mut *tx).await?;
    for row in candidates {
        require_binding(actor, &row)?;
        if super::check_expected(&row, &current).is_ok() {
            let id = string(&row, "job_id").to_owned();
            sqlx::query("UPDATE knowledge.vector_mutation_jobs SET state=CASE WHEN result->>'schema'='knowledge-dataset-vector-delete/v1' THEN 'running' ELSE 'pending' END,error_class=NULL,updated_at=NOW() WHERE job_id=$1 AND state='failed'")
                .bind(&id).execute(&mut *tx).await?;
            tx.commit().await?;
            lease.release().await?;
            return Ok(Some(id));
        }
    }
    let expected = generation(&current);
    let id = identity(
        &actor.tenant_id,
        &actor.user_id,
        dataset,
        &expected,
        payload,
    );
    sqlx::query("INSERT INTO knowledge.vector_mutation_jobs(job_id,tenant_id,dataset_id,requested_by,expected_generation,payload,expected_revision) VALUES($1,$2,$3,$4,$5,$6,$7)")
        .bind(&id).bind(&actor.tenant_id).bind(dataset).bind(&actor.user_id).bind(expected).bind(payload).bind(current["content_revision"].as_i64())
        .execute(&mut *tx).await?;
    tx.commit().await?;
    lease.release().await?;
    Ok(Some(id))
}

pub(super) async fn completed_delete(pool: &PgPool, actor: &Actor, dataset: &str) -> Result<bool> {
    let row:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(j) FROM knowledge.vector_mutation_jobs j JOIN knowledge.datasets d ON d.dataset_id=j.dataset_id AND d.tenant_id=j.tenant_id WHERE j.dataset_id=$1 AND j.tenant_id=$2 AND j.requested_by=$3 AND j.payload->>'kind'='delete_dataset' AND j.state='succeeded' AND (j.result='true'::jsonb OR j.result->>'schema'='knowledge-dataset-vector-delete/v1') AND d.is_deleted AND d.deleted_by=$3 ORDER BY j.created_at DESC LIMIT 1")
        .bind(dataset).bind(&actor.tenant_id).bind(&actor.user_id).fetch_optional(crate::database::executor(pool)).await?;
    if let Some(row) = row {
        require_binding(actor, &row)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub(super) async fn finalize(pool: &PgPool, actor: &Actor, expected: &Value) -> Result<()> {
    require_binding(actor, expected)?;
    let id = string(expected, "job_id");
    let latest: Value = sqlx::query_scalar(
        "SELECT to_jsonb(j) FROM knowledge.vector_mutation_jobs j WHERE job_id=$1",
    )
    .bind(id)
    .fetch_one(crate::database::executor(pool))
    .await?;
    if latest["state"] == "succeeded" && result(&latest) == true {
        return Ok(());
    }
    let store = KnowledgeWriteStore::new(pool.clone());
    // Do not hold a pool connection while resolving Cedar/owner access.
    match store
        .require_dataset_access(actor, string(expected, "dataset_id"), "owner")
        .await
    {
        Ok(current) => require_receipt(actor, &latest, &current)?,
        Err(IngestError::NotFound)
            if completed_delete(pool, actor, string(expected, "dataset_id")).await? =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    }
    let mut lease = store
        .dataset_delete_lease(string(expected, "dataset_id"))
        .await?;
    let mut tx = lease.connection().begin().await?;
    crate::database::bind_current(&mut tx).await?;
    let current:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM knowledge.datasets d WHERE dataset_id=$1 AND tenant_id=$2 FOR UPDATE")
        .bind(string(expected,"dataset_id")).bind(&actor.tenant_id).fetch_one(&mut *tx).await?;
    let row: Value = sqlx::query_scalar(
        "SELECT to_jsonb(j) FROM knowledge.vector_mutation_jobs j WHERE job_id=$1 FOR UPDATE",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    require_binding(actor, &row)?;
    if row["state"] == "succeeded"
        && result(&row) == true
        && current["is_deleted"] == true
        && current["deleted_by"] == actor.user_id
    {
        tx.commit().await?;
        lease.release().await?;
        return Ok(());
    }
    require_receipt(actor, &row, &current)?;
    if row["payload"] != expected["payload"]
        || row["expected_generation"] != expected["expected_generation"]
    {
        return Err(IngestError::GenerationChanged);
    }
    let dataset = string(&row, "dataset_id");
    sqlx::query("UPDATE knowledge.datasets SET is_deleted=TRUE,deleted_at=NOW(),deleted_by=$2,delete_reason=$3,updated_at=NOW() WHERE dataset_id=$1 AND tenant_id=$4 AND NOT is_deleted")
        .bind(dataset).bind(&actor.user_id).bind(row["payload"]["reason"].as_str()).bind(&actor.tenant_id).execute(&mut *tx).await?;
    sqlx::query(
        "DELETE FROM gateway.version_retention_policies WHERE dataset_id=$1 AND tenant_id=$2",
    )
    .bind(dataset)
    .bind(&actor.tenant_id)
    .execute(&mut *tx)
    .await?;
    // child_chunks.document_id is NOT NULL and its authority-owned FK cascades
    // from documents. The API needs no read privilege on child chunk content.
    for table in [
        "confluence_space_bindings",
        "dataset_keyword_tables",
        "dataset_process_rules",
        "dataset_queries",
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
    sqlx::query("UPDATE knowledge.vector_mutation_jobs SET state='succeeded',error_class=NULL,updated_at=NOW() WHERE job_id=$1 AND state='running'")
        .bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    lease.release().await?;
    Ok(())
}

/// The API lifespan owns recovery even if the original HTTP caller disconnects.
/// Actors are resolved from durable ownership, never synthesized from job fields.
pub async fn recover_api_deletions(service: &IngestionService) -> Result<bool> {
    if !service.indexer.vectors.is_read_only() {
        return Err(IngestError::Permission);
    }
    let actors = service
        .vector_job_actor_authority
        .as_ref()
        .ok_or(IngestError::Permission)?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(j) FROM knowledge.vector_mutation_jobs j WHERE state='running' AND payload->>'kind'='delete_dataset' AND result->>'schema'='knowledge-dataset-vector-delete/v1' ORDER BY updated_at,job_id LIMIT 16")
        .fetch_all(crate::database::executor(&service.store.pool)).await?;
    let mut busy = false;
    for row in rows {
        let Ok(actor) = actors
            .resolve(string(&row, "tenant_id"), string(&row, "requested_by"))
            .await
        else {
            continue;
        };
        let completed = crate::cedar_dataset_policy::scoped(
            service.store.object_policy.clone(),
            ai_platform_service_primitives::knowledge_model::with_actor_scope(
                actor.clone(),
                finalize(&service.store.pool, &actor, &row),
            ),
        )
        .await;
        if completed.is_ok() {
            busy = true;
        }
    }
    Ok(busy)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vector_delete_receipt_requires_exact_job_actor_generation_and_fence() {
        let actor: Actor = serde_json::from_value(json!({"tenant_id":"t","user_id":"u"})).unwrap();
        let dataset = json!({"dataset_id":"d","tenant_id":"t","content_revision":1,"index_config":{"retrieval":{"_index_deletion_fence":{"operation":"dataset_delete","target_id":"d","status":"pending","version":1}}}});
        let mut prior = dataset.clone();
        prior["content_revision"] = json!(0);
        prior["index_config"] = json!({});
        let expected = generation(&prior);
        let payload = json!({"kind":"delete_dataset","confirmed":true,"reason":null});
        let id = identity("t", "u", "d", &expected, &payload);
        let row = json!({"state":"running","tenant_id":"t","requested_by":"u","dataset_id":"d","job_id":id,"expected_generation":expected,"expected_revision":0,"payload":payload,"result":{"schema":SCHEMA,"job_id":id,"expected_generation":expected,"dataset_generation":generation(&dataset)}});
        assert!(require_receipt(&actor, &row, &dataset).is_ok());
        assert_ne!(result(&row), json!(true));
        let mut completed = row.clone();
        completed["state"] = json!("succeeded");
        assert_eq!(result(&completed), json!(true));
        completed["result"]["schema"] = json!("forged");
        assert_ne!(result(&completed), json!(true));
        for key in ["job_id", "expected_generation", "dataset_generation"] {
            let mut bad = row.clone();
            bad["result"][key] = json!("forged");
            assert!(require_receipt(&actor, &bad, &dataset).is_err(), "{key}");
        }
        let mut bad = dataset.clone();
        bad["index_config"]["retrieval"]["_index_deletion_fence"]["target_id"] = json!("another");
        assert!(require_receipt(&actor, &row, &bad).is_err());
        bad = dataset.clone();
        bad["content_revision"] = json!(2);
        assert!(require_receipt(&actor, &row, &bad).is_err());
        let mut other = actor.clone();
        other.tenant_id = "other".into();
        assert!(require_receipt(&other, &row, &dataset).is_err());
    }
}
