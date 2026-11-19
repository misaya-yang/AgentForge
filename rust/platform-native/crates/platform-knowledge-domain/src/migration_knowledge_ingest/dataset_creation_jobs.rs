//! API-owned metadata and ownership are committed with the durable intent. The
//! worker's existing UPDATE grant acknowledges its vector work atomically with
//! job success; it never needs INSERT on datasets or dataset_permissions.
use super::{Actor, IngestError, IngestionService, Mutation, Result, generation, identity, string};
use crate::migration_knowledge_ingest::{Dataset, store::KnowledgeWriteStore};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;

fn conflict() -> IngestError {
    IngestError::Validation("dataset_id already exists with a different creation intent".into())
}
fn require_owner(actor: &Actor, dataset: &Value) -> Result<()> {
    if actor.anonymous()
        || string(dataset, "tenant_id") != actor.tenant_id
        || string(dataset, "created_by") != actor.user_id
        || dataset["is_deleted"] == true
    {
        return Err(IngestError::Permission);
    }
    Ok(())
}
fn require_binding(actor: &Actor, dataset: &Value, row: &Value) -> Result<()> {
    require_owner(actor, dataset)?;
    if string(row, "tenant_id") != actor.tenant_id
        || string(row, "requested_by") != actor.user_id
        || string(row, "dataset_id") != string(dataset, "dataset_id")
        || row["payload"]["kind"] != "create_dataset"
        || string(&row["payload"]["input"], "dataset_id") != string(dataset, "dataset_id")
        || string(&row["payload"]["input"], "tenant_id") != actor.tenant_id
        || string(&row["payload"]["input"], "created_by") != actor.user_id
        || string(row, "job_id")
            != identity(
                &actor.tenant_id,
                &actor.user_id,
                string(dataset, "dataset_id"),
                string(row, "expected_generation"),
                &row["payload"],
            )
    {
        return Err(IngestError::Permission);
    }
    Ok(())
}

pub(crate) async fn submit_creation(
    service: &IngestionService,
    actor: &Actor,
    desired: &Value,
) -> Result<Value> {
    require_owner(actor, desired)?;
    let dataset = string(desired, "dataset_id");
    let payload = serde_json::to_value(Mutation::CreateDataset {
        input: desired.clone(),
    })?;
    let mut tx = service.store.pool.begin().await?;
    crate::database::bind_current(&mut tx).await?;
    // The same dataset lock protects finalization and native deletion/publication.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("knowledge-dataset-index:{dataset}"))
        .execute(&mut *tx)
        .await?;
    let existing: Option<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(d) FROM knowledge.datasets d WHERE dataset_id=$1 FOR UPDATE",
    )
    .bind(dataset)
    .fetch_optional(&mut *tx)
    .await?;
    let newly_created = existing.is_none();
    let current = if let Some(current) = existing {
        require_owner(actor, &current)?;
        current
    } else {
        if !KnowledgeWriteStore::insert_dataset_with_owner(&mut tx, desired, &actor.user_id).await?
        {
            return Err(conflict());
        }
        KnowledgeWriteStore::dataset_on(&mut tx, dataset).await?
    };
    let prior: Option<Value> = if newly_created {
        None
    } else {
        sqlx::query_scalar("SELECT to_jsonb(j) FROM knowledge.vector_mutation_jobs j WHERE tenant_id=$1 AND dataset_id=$2 AND requested_by=$3 AND payload=$4 ORDER BY created_at DESC,job_id LIMIT 1 FOR UPDATE")
        .bind(&actor.tenant_id).bind(dataset).bind(&actor.user_id).bind(&payload)
        .fetch_optional(&mut *tx).await?
    };
    let id = if let Some(row) = prior {
        require_binding(actor, &current, &row)?;
        if row["state"] == "succeeded" {
            // An acknowledgement can be replayed only for the same completed
            // generation, never over later edits, deletion or recreation.
            if generation(&current) != generation(&row["result"]) {
                return Err(IngestError::GenerationChanged);
            }
            tx.commit().await?;
            return Ok(row["result"].clone());
        }
        super::check_expected(&row, &current)?;
        let id = string(&row, "job_id").to_owned();
        sqlx::query("UPDATE knowledge.vector_mutation_jobs SET state='pending',error_class=NULL,updated_at=NOW() WHERE job_id=$1 AND state='failed'")
            .bind(&id).execute(&mut *tx).await?;
        id
    } else {
        // Existing unrelated metadata must never receive a new owner or be
        // treated as a successfully resumed creation.
        if !newly_created {
            return Err(conflict());
        }
        let expected = generation(&current);
        let id = identity(
            &actor.tenant_id,
            &actor.user_id,
            dataset,
            &expected,
            &payload,
        );
        sqlx::query("INSERT INTO knowledge.vector_mutation_jobs(job_id,tenant_id,dataset_id,requested_by,expected_generation,payload,expected_revision) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(&id).bind(&actor.tenant_id).bind(dataset).bind(&actor.user_id)
            .bind(expected).bind(payload).bind(current["content_revision"].as_i64())
            .execute(&mut *tx).await?;
        id
    };
    tx.commit().await?;
    super::wait_for_result(&service.store.pool, actor, dataset, &id).await
}

pub(super) async fn execute_creation(
    service: &IngestionService,
    actor: &Actor,
    row: &Value,
    _input: &Value,
) -> Result<Value> {
    let dataset = string(row, "dataset_id");
    let current = service
        .store
        .require_dataset_access(actor, dataset, "owner")
        .await?;
    require_binding(actor, &current, row)?;
    super::check_expected(row, &current)?;
    super::super::require_index_writable(&current)?;
    let typed: Dataset = serde_json::from_value(current.clone())?;
    let resolved = service.indexer.embeddings.resolve(&typed).await?;
    if typed.embedding_dimension != Some(resolved.dimension) {
        return Err(IngestError::GenerationChanged);
    }
    if resolved.provider == "local_semantic" {
        service
            .indexer
            .embeddings
            .embed_resolved(
                &resolved,
                &["Semantic model availability".into()],
                "document",
            )
            .await?;
    }
    // Repeated attempts verify the original tenant/dataset ownership metadata;
    // a collection belonging to another intent is never adopted.
    let collection = service
        .indexer
        .vectors
        .resume_dataset_collection(&typed, resolved.dimension)
        .await?;
    let mut tx = service.store.pool.begin().await?;
    crate::database::bind_current(&mut tx).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind(format!("knowledge-dataset-index:{dataset}"))
        .execute(&mut *tx)
        .await?;
    let current: Value = sqlx::query_scalar("SELECT to_jsonb(d) FROM knowledge.datasets d WHERE dataset_id=$1 AND is_deleted=FALSE FOR UPDATE")
        .bind(dataset).fetch_optional(&mut *tx).await?.ok_or(IngestError::NotFound)?;
    require_binding(actor, &current, row)?;
    super::check_expected(row, &current)?;
    let locked: Value = sqlx::query_scalar(
        "SELECT to_jsonb(j) FROM knowledge.vector_mutation_jobs j WHERE job_id=$1 FOR UPDATE",
    )
    .bind(string(row, "job_id"))
    .fetch_one(&mut *tx)
    .await?;
    require_binding(actor, &current, &locked)?;
    if locked["state"] != "running"
        || locked["expected_generation"] != row["expected_generation"]
        || locked["payload"] != row["payload"]
    {
        return Err(IngestError::GenerationChanged);
    }
    let completed: Value = sqlx::query_scalar("UPDATE knowledge.datasets SET collection_name=$2,embedding_dimension=$3,updated_at=NOW() WHERE dataset_id=$1 AND tenant_id=$4 AND is_deleted=FALSE RETURNING to_jsonb(datasets)")
        .bind(dataset).bind(collection).bind(resolved.dimension as i32).bind(&actor.tenant_id)
        .fetch_one(&mut *tx).await?;
    let result = super::super::service::redact_server_config(&completed);
    sqlx::query("UPDATE knowledge.vector_mutation_jobs SET state='succeeded',result=$2,error_class=NULL,updated_at=NOW() WHERE job_id=$1 AND state='running'")
        .bind(string(row, "job_id")).bind(&result).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn intent() -> (Actor, Value, Value) {
        let actor: Actor =
            serde_json::from_value(json!({"tenant_id":"tenant-a","user_id":"owner"})).unwrap();
        let dataset = json!({"tenant_id":"tenant-a","dataset_id":"kb-a","created_by":"owner","embedding_provider":"local","embedding_model":"hash-384","embedding_dimension":384,"content_revision":0,"index_config":{}});
        let payload = json!({"kind":"create_dataset","input":dataset});
        let expected = generation(&dataset);
        let row = json!({"tenant_id":"tenant-a","requested_by":"owner","dataset_id":"kb-a","expected_generation":expected,"payload":payload,"job_id":identity("tenant-a","owner","kb-a",&expected,&payload)});
        (actor, dataset, row)
    }
    #[test]
    fn creation_replay_cannot_cross_actor_tenant_or_dataset() {
        let (actor, dataset, row) = intent();
        assert!(require_binding(&actor, &dataset, &row).is_ok());
        for (field, value) in [
            ("tenant_id", "tenant-b"),
            ("requested_by", "other"),
            ("dataset_id", "kb-b"),
            ("job_id", "forged"),
        ] {
            let mut changed = row.clone();
            changed[field] = json!(value);
            assert!(
                require_binding(&actor, &dataset, &changed).is_err(),
                "{field}"
            );
        }
        let mut deleted = dataset.clone();
        deleted["is_deleted"] = json!(true);
        assert!(require_binding(&actor, &deleted, &row).is_err());
    }
    #[test]
    fn creation_resume_retains_generation_and_payload_fences() {
        let (actor, dataset, row) = intent();
        assert!(super::super::check_expected(&row, &dataset).is_ok());
        let mut changed = dataset.clone();
        changed["content_revision"] = json!(1);
        assert!(super::super::check_expected(&row, &changed).is_err());
        changed = dataset.clone();
        changed["embedding_model"] = json!("other");
        assert!(super::super::check_expected(&row, &changed).is_err());
        let mut tampered = row.clone();
        tampered["payload"]["input"]["embedding_model"] = json!("other");
        assert!(require_binding(&actor, &dataset, &tampered).is_err());
    }
}
