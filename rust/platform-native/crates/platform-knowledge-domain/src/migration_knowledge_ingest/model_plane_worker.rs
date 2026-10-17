//! Worker model calls run as the current persisted initiator, never a synthetic admin.
use super::{IngestError, IngestionService, Result, string};
use crate::migration_knowledge_query::model_plane::{self, ModelResource};
pub async fn ingest(
    service: &IngestionService,
    dataset: &str,
    document: &str,
) -> Result<Vec<String>> {
    let Some(client) = service.model_plane.clone() else {
        return service.ingest_document(dataset, document).await;
    };
    let doc = service.store.get_document(dataset, document).await?;
    let d = service.store.get_dataset(dataset).await?;
    let execution = doc["metadata"]["_document_pipeline_execution_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    let initiator: Option<String> = if let Some(id) = &execution {
        sqlx::query_scalar("SELECT triggered_by FROM knowledge.document_pipeline_executions WHERE execution_id=$1 AND dataset_id=$2 AND document_id=$3 AND status='running'").bind(id).bind(dataset).bind(document).fetch_optional(&service.store.pool).await?.flatten()
    } else {
        None
    };
    let user = initiator
        .as_deref()
        .unwrap_or_else(|| string(&doc, "created_by"));
    if user.is_empty() {
        return Err(IngestError::Permission);
    }
    let actors = service
        .vector_job_actor_authority
        .as_ref()
        .ok_or(IngestError::Permission)?;
    let actor = actors.resolve(string(&d, "tenant_id"), user).await?;
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
