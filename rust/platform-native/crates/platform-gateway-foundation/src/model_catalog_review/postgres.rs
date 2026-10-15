//! Atomic native adapter. No constructor I/O. Existing model validation and cache
//! invalidation are reused; cancellation drops the uncommitted transaction.
//! A COMMIT transport error is unknown, never a retry signal.
use super::*;
use crate::{
    model_provider_storage::{bind_model_query, decode_row},
    model_service::{
        CreateModel, ModelDatabase, ModelFields, ModelFuture, ModelPricingCache, ModelQuery,
        ModelRow, ModelService, ModelServiceError, UpdateModel,
    },
};
use sqlx::{PgPool, Postgres, Transaction};
use tokio::sync::Mutex as AsyncMutex;

pub struct PgReviewDatabase {
    pool: PgPool,
    pricing: Arc<dyn ModelPricingCache>,
}
impl PgReviewDatabase {
    pub fn new(pool: PgPool, pricing: Arc<dyn ModelPricingCache>) -> Self {
        Self { pool, pricing }
    }
}
struct TransactionDatabase(AsyncMutex<Transaction<'static, Postgres>>);
impl ModelDatabase for &TransactionDatabase {
    fn fetch(&self, q: ModelQuery) -> ModelFuture<'_, Vec<ModelRow>> {
        Box::pin(async move {
            let mut tx = self.0.lock().await;
            bind_model_query(&q.sql, q.params)
                .map_err(|_| ModelServiceError::Database)?
                .fetch_all(&mut **tx)
                .await
                .map_err(|_| ModelServiceError::Database)?
                .into_iter()
                .map(|r| decode_row(r).map_err(|_| ModelServiceError::InvalidRow))
                .collect()
        })
    }
    fn fetchrow(&self, q: ModelQuery) -> ModelFuture<'_, Option<ModelRow>> {
        Box::pin(async move { Ok(self.fetch(q).await?.into_iter().next()) })
    }
    fn execute(&self, q: ModelQuery) -> ModelFuture<'_, String> {
        Box::pin(async move {
            let mut tx = self.0.lock().await;
            let r = bind_model_query(&q.sql, q.params)
                .map_err(|_| ModelServiceError::Database)?
                .execute(&mut **tx)
                .await
                .map_err(|_| ModelServiceError::Database)?;
            Ok(format!("UPDATE {}", r.rows_affected()))
        })
    }
}
struct DeferredCache;
impl ModelPricingCache for DeferredCache {
    fn invalidate_cache(&self) -> Result<(), ModelServiceError> {
        Ok(())
    }
}
async fn snapshot(
    tx: &mut Transaction<'_, Postgres>,
    tenant: &str,
    actor: &str,
    provider: &str,
    lock: bool,
) -> Result<Snapshot, ReviewError> {
    let sql = if lock {
        "SELECT jsonb_build_object('updated_at',updated_at) FROM llm_providers WHERE tenant_id=$1 AND provider_id=$2 FOR UPDATE"
    } else {
        "SELECT jsonb_build_object('updated_at',updated_at) FROM llm_providers WHERE tenant_id=$1 AND provider_id=$2"
    };
    let revision = sqlx::query_scalar::<_, Value>(sql)
        .bind(tenant)
        .bind(provider)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| ReviewError::Unavailable)?
        .ok_or(ReviewError::NotFound)?;
    let sql = if lock {
        "SELECT to_jsonb(safe) FROM (SELECT model_id,provider_id,display_name,context_window,max_output_tokens,supports_vision,supports_tools,input_price_per_1k,output_price_per_1k,access_level,is_enabled,sort_order,catalog_capabilities,capability_overrides,capability_revision,created_at,updated_at FROM llm_models WHERE tenant_id=$1 AND provider_id=$2 ORDER BY model_id LIMIT 2001 FOR UPDATE) safe"
    } else {
        "SELECT to_jsonb(safe) FROM (SELECT model_id,provider_id,display_name,context_window,max_output_tokens,supports_vision,supports_tools,input_price_per_1k,output_price_per_1k,access_level,is_enabled,sort_order,catalog_capabilities,capability_overrides,capability_revision,created_at,updated_at FROM llm_models WHERE tenant_id=$1 AND provider_id=$2 ORDER BY model_id LIMIT 2001) safe"
    };
    let models = sqlx::query_scalar::<_, Value>(sql)
        .bind(tenant)
        .bind(provider)
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| ReviewError::Unavailable)?;
    let references = sqlx::query_scalar::<_, Value>(include_str!("references.sql"))
        .bind(tenant)
        .bind(provider)
        .bind(actor)
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| ReviewError::Unavailable)?;
    if models.len() > 2000 || references.len() > 2000 {
        return Err(ReviewError::Unavailable);
    }
    Ok(Snapshot{provider_revision:revision,models,references,reference_coverage:"Current drafts and active publications of Agents where you are an explicit member only; legacy unqualified model references are potential matches. Other Agents, process defaults, service defaults and external clients are not assessed. Open the authorized Agent to inspect defaults before replacing metadata.".into()})
}
fn fields(k: CatalogModelKwargs) -> Result<ModelFields, ReviewError> {
    Ok(ModelFields {
        display_name: k.display_name,
        context_window: k.context_window,
        max_output_tokens: k.max_output_tokens,
        supports_vision: k.supports_vision,
        supports_tools: k.supports_tools,
        input_price_per_1k: k.input_price_per_1k,
        output_price_per_1k: k.output_price_per_1k,
        access_level: k.access_level,
        sort_order: k.sort_order,
        catalog_capabilities: k
            .catalog_capabilities
            .map(|v| v.as_object().cloned().ok_or(ReviewError::Invalid))
            .transpose()?,
    })
}
impl ReviewDatabase for PgReviewDatabase {
    fn snapshot<'a>(
        &'a self,
        tenant: &'a str,
        actor: &'a str,
        provider: &'a str,
    ) -> ReviewFuture<'a, Snapshot> {
        Box::pin(async move {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|_| ReviewError::Unavailable)?;
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
                .execute(&mut *tx)
                .await
                .map_err(|_| ReviewError::Unavailable)?;
            let value = snapshot(&mut tx, tenant, actor, provider, false).await?;
            tx.rollback().await.map_err(|_| ReviewError::Unavailable)?;
            Ok(value)
        })
    }
    fn apply<'a>(
        &'a self,
        tenant: &'a str,
        actor: &'a str,
        provider: &'a str,
        before: &'a Snapshot,
        proposals: Vec<CatalogModelKwargs>,
    ) -> ReviewFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|_| ReviewError::Unavailable)?;
            sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
                .execute(&mut *tx)
                .await
                .map_err(|_| ReviewError::Unavailable)?;
            if &snapshot(&mut tx, tenant, actor, provider, true).await? != before {
                return Err(ReviewError::Conflict);
            }
            let database = TransactionDatabase(AsyncMutex::new(tx));
            let service = ModelService::new(&database, DeferredCache);
            let mut results = Vec::new();
            for proposal in proposals {
                let id = proposal.model_id.clone();
                let f = fields(proposal)?;
                let row = if before.models.iter().any(|m| m["model_id"] == id) {
                    service
                        .update_model(
                            tenant,
                            &id,
                            Some(provider),
                            UpdateModel {
                                display_name: Some(f.display_name),
                                context_window: Some(f.context_window),
                                max_output_tokens: Some(f.max_output_tokens),
                                supports_vision: Some(f.supports_vision),
                                supports_tools: Some(f.supports_tools),
                                sort_order: Some(f.sort_order),
                                catalog_capabilities: f.catalog_capabilities,
                                ..Default::default()
                            },
                        )
                        .await
                        .map_err(|_| ReviewError::Invalid)?
                        .ok_or(ReviewError::Conflict)?
                } else {
                    service
                        .create_model(
                            tenant,
                            &id,
                            provider,
                            CreateModel {
                                fields: f,
                                is_enabled: false,
                                capability_overrides: Default::default(),
                            },
                        )
                        .await
                        .map_err(|_| ReviewError::Invalid)?
                };
                results.push(Value::Object(row));
            }
            drop(service);
            database
                .0
                .into_inner()
                .commit()
                .await
                .map_err(|_| ReviewError::Unknown)?;
            // A failure after commit cannot safely be called rollback; callers retain
            // their receipt and recover through readback without another write.
            self.pricing
                .invalidate_cache()
                .map_err(|_| ReviewError::Unknown)?;
            Ok(results)
        })
    }
}
