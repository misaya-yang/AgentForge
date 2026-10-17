//! Durable API-to-worker vector mutations. Payloads contain identities, never keys.
//! A session advisory lock owns each job across external I/O; native mutation
//! methods retain their own publication/document leases and recovery fences.
use super::{Actor, IngestError, IngestionService, Result, canonical_json, sha256, string};
use crate::migration_knowledge_query::{
    actor::RuntimeActorClient, lexical_lifecycle::LexicalLifecycle, migration::SessionLease,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mutation {
    EmbeddingStart {
        target: Value,
        capabilities: Option<Value>,
    },
    EmbeddingCutover {
        migration: String,
        retention: i64,
    },
    EmbeddingRollback {
        migration: String,
        keep_shadow: bool,
    },
    EmbeddingAbort {
        migration: String,
        reason: String,
        purge: bool,
    },
    RestoreVersion {
        document: String,
        version: i32,
        reason: Option<String>,
    },
    CreateSegment {
        segment: String,
        document: String,
        text: String,
        answer: Option<String>,
        keywords: Option<Vec<String>>,
    },
    CreateDataset {
        input: Value,
    },
    UpdateDataset {
        input: Value,
    },
    UpdateSegment {
        segment: String,
        text: String,
        answer: Option<String>,
        keywords: Option<Vec<String>>,
    },
    DeleteDocument {
        document: String,
    },
    DeleteDataset {
        confirmed: bool,
        reason: Option<String>,
    },
    DeleteSegment {
        segment: String,
    },
    SetSegmentEnabled {
        segment: String,
        enabled: bool,
    },
    SetDocumentEnabled {
        document: String,
        enabled: bool,
    },
    SetDocumentArchived {
        document: String,
        archived: bool,
        reason: Option<String>,
    },
    LexicalCutover {
        manifest: Option<String>,
    },
    LexicalRollback {
        keep_shadow: bool,
    },
}
impl Mutation {
    fn permission(&self) -> &'static str {
        match self {
            Self::EmbeddingStart { .. }
            | Self::EmbeddingCutover { .. }
            | Self::EmbeddingRollback { .. }
            | Self::EmbeddingAbort { .. }
            | Self::CreateDataset { .. }
            | Self::UpdateDataset { .. }
            | Self::DeleteDataset { .. }
            | Self::LexicalCutover { .. }
            | Self::LexicalRollback { .. } => "owner",
            _ => "editor",
        }
    }
    fn lexical(&self) -> bool {
        matches!(
            self,
            Self::LexicalCutover { .. } | Self::LexicalRollback { .. }
        )
    }
}
/// Exclude the recovery fence only: it is owned by the exact-target native
/// deletion protocol. Every publication/model/lexical revision remains bound.
fn generation(dataset: &Value) -> String {
    let mut index = dataset["index_config"].clone();
    if let Some(retrieval) = index.get_mut("retrieval").and_then(Value::as_object_mut) {
        retrieval.remove("_index_deletion_fence");
    }
    sha256(canonical_json(&json!({"identity":super::dataset_ingestion_identity(dataset), "content_revision":dataset["content_revision"], "index_config":index})).as_bytes())
}
fn operator(actor: &Actor) -> bool {
    actor.user_tier == "admin"
        || actor
            .roles
            .iter()
            .any(|role| role == "admin" || role == "operator")
}
fn matches_generation(row: &Value, current: &Value, mutation: &Mutation) -> bool {
    if generation(current) == string(row, "expected_generation") {
        return true;
    }
    let target = match mutation {
        Mutation::DeleteDocument { document } => ("document_delete", document.as_str()),
        Mutation::DeleteSegment { segment } => ("segment_delete", segment.as_str()),
        Mutation::DeleteDataset { .. } => ("dataset_delete", string(row, "dataset_id")),
        _ => return false,
    };
    let Some(expected) = row["expected_revision"].as_i64() else {
        return false;
    };
    let actual = current["content_revision"].as_i64();
    let fence = &current["index_config"]["retrieval"]["_index_deletion_fence"];
    let pending = json!({"operation":target.0,"target_id":target.1,"status":"pending","version":1});
    if !((actual == expected.checked_add(1) && fence == &pending)
        || (actual == expected.checked_add(2) && fence.is_null()))
    {
        return false;
    }
    let mut prior = current.clone();
    prior["content_revision"] = json!(expected);
    generation(&prior) == string(row, "expected_generation")
}
pub(crate) fn check_expected(row: &Value, current: &Value) -> Result<()> {
    let mutation: Mutation = serde_json::from_value(row["payload"].clone())?;
    if matches_generation(row, current, &mutation) {
        Ok(())
    } else {
        Err(IngestError::GenerationChanged)
    }
}
fn identity(tenant: &str, user: &str, dataset: &str, generation: &str, payload: &Value) -> String {
    sha256(
        canonical_json(&json!([
            "knowledge-vector-job/v1",
            tenant,
            user,
            dataset,
            generation,
            payload
        ]))
        .as_bytes(),
    )
}
/// Wait for the actual result; timeout never reports an asynchronous mutation as
/// completed. Retrying the same operation resumes its durable identity.
pub async fn submit(
    pool: &PgPool,
    actor: &Actor,
    dataset: &str,
    mutation: Mutation,
) -> Result<Value> {
    if actor.anonymous() {
        return Err(IngestError::Permission);
    }
    let store = super::store::KnowledgeWriteStore::new(pool.clone());
    let d = if matches!(mutation, Mutation::CreateDataset { .. }) {
        json!({"tenant_id":actor.tenant_id,"dataset_id":dataset})
    } else {
        store
            .require_dataset_access(actor, dataset, mutation.permission())
            .await?
    };
    if mutation.lexical() && !operator(actor) {
        return Err(IngestError::Permission);
    }
    if matches!(
        mutation,
        Mutation::DeleteDataset {
            confirmed: false,
            ..
        }
    ) {
        return Err(IngestError::Permission);
    }
    match &mutation {
        Mutation::CreateDataset { input } | Mutation::UpdateDataset { input }
            if super::service::contains_server_config(&input["embedding_config"])
                || super::service::contains_server_config(
                    &input["index_config"]["retrieval"]["rerank"],
                ) =>
        {
            return Err(IngestError::Validation(
                "credentials and endpoints are server-owned".into(),
            ));
        }
        _ => {}
    }
    let expected = generation(&d);
    let payload = serde_json::to_value(&mutation)?;
    let id = identity(
        &actor.tenant_id,
        &actor.user_id,
        dataset,
        &expected,
        &payload,
    );
    sqlx::query("INSERT INTO knowledge.vector_mutation_jobs(job_id,tenant_id,dataset_id,requested_by,expected_generation,payload,expected_revision) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(job_id) DO UPDATE SET state='pending',error_class=NULL,updated_at=NOW() WHERE vector_mutation_jobs.state='failed'")
        .bind(&id).bind(&actor.tenant_id).bind(dataset).bind(&actor.user_id).bind(&expected).bind(payload).bind(d["content_revision"].as_i64()).execute(pool).await?;
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let row: Value = sqlx::query_scalar("SELECT to_jsonb(j) FROM knowledge.vector_mutation_jobs j WHERE job_id=$1 AND tenant_id=$2 AND requested_by=$3")
            .bind(&id).bind(&actor.tenant_id).bind(&actor.user_id).fetch_one(pool).await?;
        match string(&row, "state") {
            "succeeded" => return Ok(row["result"].clone()),
            "failed" => {
                return Err(match string(&row, "error_class") {
                    "permission" => IngestError::Permission,
                    "not_found" => IngestError::NotFound,
                    "generation_changed" => IngestError::GenerationChanged,
                    "lease_busy" => IngestError::LeaseBusy,
                    "validation" => IngestError::Validation(format!(
                        "durable vector mutation {id} failed validation"
                    )),
                    _ => IngestError::Index(format!(
                        "durable vector mutation {id} failed; retry the same operation"
                    )),
                });
            }
            _ if Instant::now() >= deadline => {
                return Err(IngestError::Index(format!(
                    "durable vector mutation {id} is pending; poll /api/v1/knowledge/datasets/{dataset}/vector-mutations/{id} or retry the same operation"
                )));
            }
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

/// Polling never exposes stored inputs, actors or provider configuration.
pub async fn status(pool: &PgPool, actor: &Actor, dataset: &str, id: &str) -> Result<Value> {
    if actor.anonymous() {
        return Err(IngestError::Permission);
    }
    let row: Value = sqlx::query_scalar("SELECT to_jsonb(j) FROM knowledge.vector_mutation_jobs j WHERE job_id=$1 AND tenant_id=$2 AND dataset_id=$3 AND requested_by=$4")
        .bind(id).bind(&actor.tenant_id).bind(dataset).bind(&actor.user_id).fetch_optional(pool).await?.ok_or(IngestError::NotFound)?;
    let store = super::store::KnowledgeWriteStore::new(pool.clone());
    match store.require_dataset_access(actor, dataset, "viewer").await {
        Ok(_) => {}
        Err(IngestError::NotFound)
            if row["payload"]["kind"] == "delete_dataset"
                && row["state"] == "succeeded"
                && row["result"] == true => {}
        Err(error) => return Err(error),
    }
    Ok(
        json!({"job_id":row["job_id"],"dataset_id":dataset,"state":row["state"],"attempts":row["attempts"],"result":row["result"],"error_class":row["error_class"],"created_at":row["created_at"],"updated_at":row["updated_at"]}),
    )
}

pub async fn dispatch_one(service: &IngestionService, actors: &RuntimeActorClient) -> Result<bool> {
    if service.indexer.vectors.is_read_only() {
        return Err(IngestError::Permission);
    }
    let ids: Vec<String> = sqlx::query_scalar("SELECT job_id FROM knowledge.vector_mutation_jobs WHERE state IN ('pending','running') ORDER BY created_at,job_id LIMIT 32").fetch_all(&service.store.pool).await?;
    for id in ids {
        let Ok(mut lease) =
            SessionLease::acquire(&service.store.pool, format!("knowledge-vector-job:{id}")).await
        else {
            continue;
        };
        let row: Option<Value> = sqlx::query_scalar("UPDATE knowledge.vector_mutation_jobs SET state='running',attempts=attempts+1,updated_at=NOW() WHERE job_id=$1 AND state IN ('pending','running') RETURNING to_jsonb(vector_mutation_jobs)")
            .bind(&id).fetch_optional(lease.connection()).await?;
        let Some(row) = row else {
            let _ = lease.release().await;
            continue;
        };
        let result = execute(service, actors, &row).await;
        let (state, value, error) = match result {
            Ok(value) => ("succeeded", value, None),
            Err(error) => (
                "failed",
                Value::Null,
                Some(match error {
                    IngestError::Permission => "permission",
                    IngestError::NotFound => "not_found",
                    IngestError::GenerationChanged => "generation_changed",
                    IngestError::LeaseBusy | IngestError::PublicationContended => "lease_busy",
                    IngestError::Validation(_) | IngestError::Parser(_) => "validation",
                    _ => "mutation_failed",
                }),
            ),
        };
        // Do not persist error text: provider details and credentials must never
        // leak through a durable status row. The typed class is sufficient.
        let class = error;
        sqlx::query("UPDATE knowledge.vector_mutation_jobs SET state=$2,result=$3,error_class=$4,updated_at=NOW() WHERE job_id=$1 AND state='running'")
            .bind(&id).bind(state).bind(value).bind(class).execute(lease.connection()).await?;
        lease.release().await.map_err(|_| IngestError::Database)?;
        return Ok(true);
    }
    Ok(false)
}
fn execute<'a>(service:&'a IngestionService,actors:&'a RuntimeActorClient,row:&'a Value)->std::pin::Pin<Box<dyn std::future::Future<Output=Result<Value>>+Send+'a>> {
    Box::pin(async move {
        if let Some(client)=service.model_plane.clone() {
            let actor=actors.resolve(string(row,"tenant_id"),string(row,"requested_by")).await?;
            let resource=crate::migration_knowledge_query::model_plane::ModelResource{dataset_id:Some(string(row,"dataset_id").into()),document_id:row["payload"]["document"].as_str().map(str::to_owned),execution_id:Some(string(row,"job_id").into())};
            crate::migration_knowledge_query::model_plane::run_scoped(client,resource,actor,execute_inner(service,actors,row)).await
        } else {execute_inner(service,actors,row).await}
    })
}
fn execute_inner<'a>(
    service: &'a IngestionService,
    actors: &'a RuntimeActorClient,
    row: &'a Value,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + 'a>> {
    Box::pin(async move {
        let actor = actors
            .resolve(string(row, "tenant_id"), string(row, "requested_by"))
            .await?;
        if actor.tenant_id != string(row, "tenant_id")
            || actor.user_id != string(row, "requested_by")
            || actor.anonymous()
        {
            return Err(IngestError::Permission);
        }
        let mutation: Mutation = serde_json::from_value(row["payload"].clone())?;
        let dataset = string(row, "dataset_id");
        if let Mutation::DeleteDataset { .. } = &mutation {
            let tombstone: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.datasets WHERE dataset_id=$1 AND tenant_id=$2 AND is_deleted=TRUE AND deleted_by=$3)")
            .bind(dataset).bind(&actor.tenant_id).bind(&actor.user_id).fetch_one(&service.store.pool).await?;
            if tombstone {
                return Ok(json!(true));
            }
        }
        if let Mutation::CreateDataset { input } = &mutation {
            return Box::pin(service.create_dataset(&actor, input)).await;
        }
        let current = service
            .store
            .require_dataset_access(&actor, dataset, mutation.permission())
            .await?;
        if let Some(result) = recover_embedding(service, dataset, &current, row, &mutation).await? {
            return Ok(result);
        }
        if !matches_generation(row, &current, &mutation) {
            return Err(IngestError::GenerationChanged);
        }
        if mutation.lexical() && !operator(&actor) {
            return Err(IngestError::Permission);
        }
        match &mutation {
            Mutation::DeleteDocument { document } => {
                let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.documents WHERE dataset_id=$1 AND document_id=$2)").bind(dataset).bind(document).fetch_one(&service.store.pool).await?;
                if !exists
                    && current["index_config"]["retrieval"]["_index_deletion_fence"].is_null()
                {
                    return Ok(json!(true));
                }
            }
            Mutation::DeleteSegment { segment } => {
                let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.segments WHERE dataset_id=$1 AND segment_id=$2)").bind(dataset).bind(segment).fetch_one(&service.store.pool).await?;
                if !exists
                    && current["index_config"]["retrieval"]["_index_deletion_fence"].is_null()
                {
                    return Ok(json!(true));
                }
            }
            _ => {}
        }
        if generation(&current) != string(row, "expected_generation")
            && current["index_config"]["retrieval"]["_index_deletion_fence"].is_null()
        {
            return Err(IngestError::GenerationChanged);
        }
        let mut execution = service.clone();
        execution.vector_job_expected = Some(row.clone());
        let service = &execution;
        if matches!(
            &mutation,
            Mutation::EmbeddingStart { .. }
                | Mutation::EmbeddingCutover { .. }
                | Mutation::EmbeddingRollback { .. }
                | Mutation::EmbeddingAbort { .. }
        ) {
            return execute_embedding(service, &actor, dataset, &current, row, mutation).await;
        }
        // Box only the service calls that can enqueue on API instances; execution
        // always uses the separate write-role instance so recursion is unreachable.
        match mutation {
            Mutation::EmbeddingStart { .. }
            | Mutation::EmbeddingCutover { .. }
            | Mutation::EmbeddingRollback { .. }
            | Mutation::EmbeddingAbort { .. } => unreachable!("handled by embedding executor"),
            Mutation::RestoreVersion {
                document,
                version,
                reason,
            } => {
                Box::pin(service.restore_version(
                    &actor,
                    dataset,
                    &document,
                    version,
                    reason.as_deref(),
                ))
                .await
            }
            Mutation::CreateSegment {
                segment,
                document,
                text,
                answer,
                keywords,
            } => {
                Box::pin(service.create_segment_owned(
                    &actor,
                    dataset,
                    &document,
                    &text,
                    answer.as_deref(),
                    keywords.as_deref(),
                    &segment,
                ))
                .await
            }
            Mutation::CreateDataset { .. } => {
                unreachable!("handled before existing dataset authorization")
            }
            Mutation::UpdateDataset { input } => {
                Box::pin(service.update_dataset(&actor, dataset, &input)).await
            }
            Mutation::UpdateSegment {
                segment,
                text,
                answer,
                keywords,
            } => {
                Box::pin(service.update_segment(
                    &actor,
                    dataset,
                    &segment,
                    &text,
                    answer.as_deref(),
                    keywords.as_deref(),
                ))
                .await
            }
            Mutation::DeleteDocument { document } => Ok(json!(
                Box::pin(service.delete_document(&actor, dataset, &document)).await?
            )),
            Mutation::DeleteDataset { confirmed, reason } => Ok(json!(
                Box::pin(service.delete_dataset(&actor, dataset, confirmed, reason.as_deref()))
                    .await?
            )),
            Mutation::DeleteSegment { segment } => Ok(json!(
                Box::pin(service.delete_segment(&actor, dataset, &segment)).await?
            )),
            Mutation::SetSegmentEnabled { segment, enabled } => {
                Box::pin(service.set_segment_enabled(&actor, dataset, &segment, enabled)).await
            }
            Mutation::SetDocumentEnabled { document, enabled } => {
                Box::pin(service.set_document_enabled(&actor, dataset, &document, enabled)).await
            }
            Mutation::SetDocumentArchived {
                document,
                archived,
                reason,
            } => {
                Box::pin(service.set_document_archived(
                    &actor,
                    dataset,
                    &document,
                    archived,
                    reason.as_deref(),
                ))
                .await
            }
            Mutation::LexicalCutover { manifest } => Ok(LexicalLifecycle::new(
                service.store.pool.clone(),
                service.indexer.vectors.clone(),
            )
            .with_job_fence(row.clone())
            .cutover(dataset, true, manifest.as_deref())
            .await?),
            Mutation::LexicalRollback { keep_shadow } => Ok(LexicalLifecycle::new(
                service.store.pool.clone(),
                service.indexer.vectors.clone(),
            )
            .with_job_fence(row.clone())
            .rollback(dataset, true, keep_shadow)
            .await?),
        }
    })
}
fn migration_scope(state: &Value, dataset: &str) -> Result<()> {
    if state["dataset_id"] == dataset {
        Ok(())
    } else {
        Err(IngestError::NotFound)
    }
}
fn committed_embedding_job(state: &Value, row: &Value, state_name: &str, marker: &str) -> bool {
    row["job_id"].as_str().is_some_and(|id| !id.is_empty())
        && state["state"] == state_name
        && state["totals"][marker] == row["job_id"]
}
async fn recover_embedding(
    service: &IngestionService,
    dataset: &str,
    current: &Value,
    row: &Value,
    mutation: &Mutation,
) -> Result<Option<Value>> {
    let (id, state_name, marker, binding_key) = match mutation {
        Mutation::EmbeddingCutover { migration, .. } => (
            migration,
            "completed",
            "vector_cutover_job_id",
            "target_binding_id",
        ),
        Mutation::EmbeddingRollback { migration, .. } => (
            migration,
            "rolled_back",
            "vector_rollback_job_id",
            "source_binding_id",
        ),
        _ => return Ok(None),
    };
    let store = crate::migration_knowledge_query::migration::MigrationStore {
        pool: service.store.pool.clone(),
    };
    let state = store.migration(id).await.map_err(embedding_failure)?;
    migration_scope(&state, dataset)?;
    if !committed_embedding_job(&state, row, state_name, marker) {
        return Ok(None);
    }
    let binding = store
        .binding(string(&state, binding_key))
        .await
        .map_err(embedding_failure)?;
    if binding["state"] != "serving"
        || binding["tenant_id"] != row["tenant_id"]
        || binding["dataset_id"] != dataset
        || binding["collection_name"] != current["collection_name"]
    {
        return Err(IngestError::GenerationChanged);
    }
    Ok(Some(json!({"migration":state})))
}
fn embedding_failure(
    error: crate::migration_knowledge_query::migration::MigrationError,
) -> IngestError {
    match error.status {
        403 => IngestError::Permission,
        404 => IngestError::NotFound,
        409 => IngestError::GenerationChanged,
        400 => IngestError::Validation(error.message),
        _ => IngestError::Index("embedding mutation dependencies unavailable".into()),
    }
}
async fn execute_embedding(
    service: &IngestionService,
    _actor: &Actor,
    dataset: &str,
    current: &Value,
    row: &Value,
    mutation: Mutation,
) -> Result<Value> {
    let migration = crate::migration_knowledge_query::migration::EmbeddingMigration::new(
        service.store.pool.clone(),
        service.indexer.vectors.clone(),
        service.indexer.embeddings.clone(),
    )
    .with_job_fence(row.clone());
    let id = match &mutation {
        Mutation::EmbeddingCutover { migration, .. }
        | Mutation::EmbeddingRollback { migration, .. }
        | Mutation::EmbeddingAbort { migration, .. } => Some(migration.as_str()),
        _ => None,
    };
    if let Some(id) = id {
        let state = migration
            .store
            .migration(id)
            .await
            .map_err(embedding_failure)?;
        migration_scope(&state, dataset)?;
    }
    match mutation {
        Mutation::EmbeddingStart {
            target,
            capabilities,
        } => migration
            .start(
                &serde_json::from_value(current.clone())?,
                target,
                capabilities,
                "",
            )
            .await
            .map_err(embedding_failure),
        Mutation::EmbeddingCutover {
            migration: id,
            retention,
        } => migration
            .cutover(&id, retention)
            .await
            .map_err(embedding_failure),
        Mutation::EmbeddingRollback {
            migration: id,
            keep_shadow,
        } => migration
            .rollback(&id, keep_shadow)
            .await
            .map_err(embedding_failure),
        Mutation::EmbeddingAbort {
            migration: id,
            reason,
            purge,
        } => migration
            .abort(&id, &reason, purge)
            .await
            .map_err(embedding_failure),
        _ => Err(IngestError::Validation(
            "invalid embedding vector job payload".into(),
        )),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn committed_embedding_recovery_requires_own_job_and_exact_terminal_state() {
        let state = json!({"state":"completed","totals":{"vector_cutover_job_id":"ours"}});
        assert!(committed_embedding_job(
            &state,
            &json!({"job_id":"ours"}),
            "completed",
            "vector_cutover_job_id"
        ));
        assert!(!committed_embedding_job(
            &state,
            &json!({"job_id":"other"}),
            "completed",
            "vector_cutover_job_id"
        ));
        assert!(!committed_embedding_job(
            &state,
            &json!({"job_id":"ours"}),
            "rolled_back",
            "vector_cutover_job_id"
        ));
        assert!(!committed_embedding_job(
            &json!({"state":"completed"}),
            &json!({}),
            "completed",
            "vector_cutover_job_id"
        ));
    }
    #[test]
    fn embedding_payloads_keep_owner_gate_and_exact_dataset_scope() {
        for operation in [
            Mutation::EmbeddingStart {
                target: json!({}),
                capabilities: None,
            },
            Mutation::EmbeddingCutover {
                migration: "m".into(),
                retention: 7,
            },
            Mutation::EmbeddingRollback {
                migration: "m".into(),
                keep_shadow: true,
            },
            Mutation::EmbeddingAbort {
                migration: "m".into(),
                reason: "stop".into(),
                purge: true,
            },
        ] {
            assert_eq!(operation.permission(), "owner");
            let wire = serde_json::to_value(&operation).unwrap();
            assert!(serde_json::from_value::<Mutation>(wire).is_ok());
        }
        assert!(migration_scope(&json!({"dataset_id":"other"}), "ours").is_err());
        assert!(migration_scope(&json!({"dataset_id":"ours"}), "ours").is_ok());
        assert!(
            serde_json::from_value::<Mutation>(
                json!({"kind":"embedding_force_cutover","migration":"m"})
            )
            .is_err()
        );
        let a = serde_json::to_value(Mutation::EmbeddingCutover {
            migration: "a".into(),
            retention: 7,
        })
        .unwrap();
        let b = serde_json::to_value(Mutation::EmbeddingCutover {
            migration: "b".into(),
            retention: 7,
        })
        .unwrap();
        assert_ne!(
            identity("t", "u", "d", "g", &a),
            identity("t", "u", "d", "g", &b)
        );
    }
    #[test]
    fn identity_separates_tenant_actor_generation_and_arguments() {
        let p = json!({"kind":"delete_document","document":"d"});
        let base = identity("a", "u", "ds", "g", &p);
        for alternate in [
            identity("b", "u", "ds", "g", &p),
            identity("a", "v", "ds", "g", &p),
            identity("a", "u", "ds", "h", &p),
            identity(
                "a",
                "u",
                "ds",
                "g",
                &json!({"kind":"delete_document","document":"e"}),
            ),
        ] {
            assert_ne!(base, alternate);
        }
    }
    #[test]
    fn recovery_requires_exact_target_revision_and_publication_identity() {
        let original = json!({"tenant_id":"t","dataset_id":"ds","collection_name":"c","content_revision":4,"index_config":{"retrieval":{}}});
        let mutation = Mutation::DeleteDocument {
            document: "doc".into(),
        };
        let row = json!({"dataset_id":"ds","expected_revision":4,"expected_generation":generation(&original)});
        let mut pending = original.clone();
        pending["content_revision"] = json!(5);
        pending["index_config"]["retrieval"]["_index_deletion_fence"] =
            json!({"operation":"document_delete","target_id":"doc","status":"pending","version":1});
        assert!(matches_generation(&row, &pending, &mutation));
        pending["index_config"]["retrieval"]["_index_deletion_fence"]["target_id"] = json!("other");
        assert!(!matches_generation(&row, &pending, &mutation));
        pending["index_config"]["retrieval"]["_index_deletion_fence"]["target_id"] = json!("doc");
        pending["collection_name"] = json!("next-generation");
        assert!(!matches_generation(&row, &pending, &mutation));
        assert!(matches_generation(&row, &original, &mutation));
        let mut other = original.clone();
        other["content_revision"] = json!(5);
        assert!(!matches_generation(
            &row,
            &other,
            &Mutation::LexicalRollback { keep_shadow: true }
        ));
    }
    #[test]
    fn recovery_fence_is_not_a_new_publication() {
        let d = json!({"tenant_id":"t","collection_name":"c","content_revision":4,"index_config":{"retrieval":{}}});
        let mut recovery = d.clone();
        recovery["index_config"]["retrieval"]["_index_deletion_fence"] = json!({"target_id":"doc"});
        assert_eq!(generation(&d), generation(&recovery));
        recovery["content_revision"] = json!(5);
        assert_ne!(generation(&d), generation(&recovery));
    }
}
