//! Worker model calls run as the current persisted initiator, never a synthetic admin.
use super::{IngestError, IngestionService, Result, string};
use crate::migration_knowledge_query::model_plane::{self, ModelResource};
use ai_platform_service_primitives::knowledge_model::with_actor_scope;
pub async fn ingest(
    service: &IngestionService,
    dataset: &str,
    document: &str,
) -> Result<Vec<String>> {
    let controlled = crate::database::control_enabled(
        &service.store.pool,
        "knowledge.document_ingest_scope_v1(text,text)",
    )
    .await?;
    if service.model_plane.is_none() && !controlled {
        return service.ingest_document(dataset, document).await;
    }
    let (tenant, user, execution): (String, String, Option<String>) = if controlled {
        // Resolve only persisted ownership before opening any document payload.
        sqlx::query_as(
            "SELECT tenant_id,user_id,execution_id FROM knowledge.document_ingest_scope_v1($1,$2)",
        )
        .bind(dataset)
        .bind(document)
        .fetch_optional(crate::database::executor(&service.store.pool))
        .await?
        .ok_or(IngestError::Permission)?
    } else {
        let doc = service.store.get_document(dataset, document).await?;
        let d = service.store.get_dataset(dataset).await?;
        let execution = doc["metadata"]["_document_pipeline_execution_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        let initiator: Option<String> = if let Some(id) = &execution {
            sqlx::query_scalar("SELECT triggered_by FROM knowledge.document_pipeline_executions WHERE execution_id=$1 AND dataset_id=$2 AND document_id=$3 AND status='running'")
                .bind(id).bind(dataset).bind(document)
                .fetch_optional(crate::database::executor(&service.store.pool)).await?.flatten()
        } else {
            None
        };
        (
            string(&d, "tenant_id").into(),
            initiator.unwrap_or_else(|| string(&doc, "created_by").into()),
            execution,
        )
    };
    if tenant.is_empty() || user.is_empty() {
        return Err(IngestError::Permission);
    }
    let actors = service
        .vector_job_actor_authority
        .as_ref()
        .ok_or(IngestError::Permission)?;
    let actor = actors.resolve(&tenant, &user).await?;
    with_actor_scope(actor.clone(), async {
        service
            .store
            .require_dataset_access(&actor, dataset, "editor")
            .await?;
        match service.model_plane.clone() {
            Some(client) => {
                model_plane::run_scoped(
                    client,
                    ModelResource {
                        dataset_id: Some(dataset.into()),
                        document_id: Some(document.into()),
                        execution_id: execution,
                    },
                    actor,
                    service.ingest_document(dataset, document),
                )
                .await
            }
            None => service.ingest_document(dataset, document).await,
        }
    })
    .await
}
