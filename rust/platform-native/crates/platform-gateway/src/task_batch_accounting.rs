//! Explicit Batch publications, immutable first receipt, and idempotent verified accounting.
//! No online pricing route, inferred discount, or caller-provided rate is consulted.
use ai_platform_gateway_foundation::{
    price_book::{Component, PriceKey, PriceReceipt, Unit, service::Publication},
    provider_batch::*,
    provider_batch_store::BatchCoordinator,
    task_delivery::DeliveryIdentity,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{collections::BTreeMap, sync::Arc};
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    tenant: String,
    provider: String,
    model: String,
    model_version: String,
    book_version: String,
    api_key: String,
}
#[derive(Clone)]
struct Binding {
    tenant: String,
    key: PriceKey,
}
pub struct PgBatchAccounting {
    pool: PgPool,
    bindings: Vec<Binding>,
}
impl PgBatchAccounting {
    fn binding(&self, id: &DeliveryIdentity, plan: &BatchPlan) -> Result<&Binding, BatchError> {
        self.bindings
            .iter()
            .find(|b| {
                b.tenant == id.tenant_id
                    && b.key.provider == plan.provider_id
                    && b.key.model == plan.model_id
            })
            .ok_or(BatchError::AccountingRequired)
    }
    async fn freeze(&self, id: &DeliveryIdentity, plan: &BatchPlan) -> Result<Value, BatchError> {
        let binding = self.binding(id, plan)?;
        // Activation marker is owner-installed only after batch schema/checkpoint acceptance.
        let admitted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM gateway.task_batch_contract WHERE version=1)",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|_| BatchError::AccountingRequired)?;
        if !admitted {
            return Err(BatchError::AccountingRequired);
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| BatchError::Unavailable)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "batch:{}:{}:{}",
                id.tenant_id, id.request_id, id.service_id
            ))
            .execute(&mut *tx)
            .await
            .map_err(|_| BatchError::Unavailable)?;
        let existing:Option<Value>=sqlx::query_scalar("SELECT snapshot FROM gateway.price_book_v2_usage_receipts WHERE tenant_id=$1 AND request_id=$2 AND service_id=$3 AND request_type='provider_batch'").bind(&id.tenant_id).bind(&id.request_id).bind(&id.service_id).fetch_optional(&mut *tx).await.map_err(|_|BatchError::Unavailable)?;
        let snapshot = if let Some(value) = existing {
            validate_snapshot(&value, id, binding)?;
            value
        } else {
            let row=sqlx::query("SELECT reference,payload FROM gateway.price_book_v2_publications WHERE tenant_id=$1 AND provider_id=$2 AND model_id=$3 AND model_version=$4 AND book_version=$5 AND currency='USD'").bind(&id.tenant_id).bind(&binding.key.provider).bind(&binding.key.model).bind(&binding.key.model_version).bind(&binding.key.book_version).fetch_one(&mut *tx).await.map_err(|_|BatchError::AccountingRequired)?;
            let publication: Publication = serde_json::from_value(
                row.try_get("payload")
                    .map_err(|_| BatchError::AccountingRequired)?,
            )
            .map_err(|_| BatchError::AccountingRequired)?;
            let reference: String = row
                .try_get("reference")
                .map_err(|_| BatchError::AccountingRequired)?;
            if publication
                .reference()
                .map_err(|_| BatchError::AccountingRequired)?
                != reference
            {
                return Err(BatchError::AccountingRequired);
            }
            let receipt = publication
                .validate(&id.tenant_id, false)
                .and_then(|book| {
                    book.resolve(
                        &id.tenant_id,
                        &binding.key,
                        chrono::Utc::now().timestamp_millis(),
                    )
                })
                .map_err(|_| BatchError::AccountingRequired)?;
            if receipt.entry.unit != Unit::Requests {
                return Err(BatchError::AccountingRequired);
            }
            let value = json!({"price_book_schema":"agentforge.price_book/v2","pricing_meter":"provider_batch","unit":"per_batch_request","currency":"USD","tenant_id":id.tenant_id,"provider_id":plan.provider_id,"model_id":plan.model_id,"task_id":id.task_id,"batch_receipt":receipt});
            sqlx::query("INSERT INTO gateway.price_book_v2_usage_receipts(tenant_id,request_id,service_id,request_type,provider_id,model_id,snapshot) VALUES($1,$2,$3,'provider_batch',$4,$5,$6)").bind(&id.tenant_id).bind(&id.request_id).bind(&id.service_id).bind(&plan.provider_id).bind(&plan.model_id).bind(&value).execute(&mut *tx).await.map_err(|_|BatchError::AccountingRequired)?;
            value
        };
        tx.commit().await.map_err(|_| BatchError::Unavailable)?;
        Ok(snapshot)
    }
}
fn validate_snapshot(
    value: &Value,
    id: &DeliveryIdentity,
    binding: &Binding,
) -> Result<PriceReceipt, BatchError> {
    if value["task_id"] != id.task_id
        || value["tenant_id"] != id.tenant_id
        || value["pricing_meter"] != "provider_batch"
        || value["unit"] != "per_batch_request"
    {
        return Err(BatchError::BindingMismatch);
    }
    let receipt: PriceReceipt = serde_json::from_value(value["batch_receipt"].clone())
        .map_err(|_| BatchError::AccountingRequired)?;
    receipt
        .validate(&id.tenant_id, &receipt.entry.key)
        .map_err(|_| BatchError::AccountingRequired)?;
    if receipt.entry.key.provider != binding.key.provider
        || receipt.entry.key.model != binding.key.model
        || receipt.entry.key.component != Component::Batch
        || receipt.entry.key.currency != "USD"
    {
        return Err(BatchError::BindingMismatch);
    }
    if receipt.entry.unit != Unit::Requests {
        return Err(BatchError::AccountingRequired);
    }
    Ok(receipt)
}
fn completed_units(receipt: &Value) -> Result<u64, BatchError> {
    let counts = &receipt["request_counts"];
    let total = counts["total"]
        .as_u64()
        .ok_or(BatchError::BindingMismatch)?;
    let completed = counts["completed"]
        .as_u64()
        .ok_or(BatchError::BindingMismatch)?;
    let failed = counts["failed"]
        .as_u64()
        .ok_or(BatchError::BindingMismatch)?;
    if total > 50_000 || completed.checked_add(failed).is_none_or(|n| n > total) {
        return Err(BatchError::BindingMismatch);
    }
    Ok(completed)
}
impl BatchAccounting for PgBatchAccounting {
    fn reserve<'a>(
        &'a self,
        id: &'a DeliveryIdentity,
        claim: Uuid,
        plan: &'a BatchPlan,
    ) -> BatchFuture<'a, ()> {
        Box::pin(async move {
            let snapshot = self.freeze(id, plan).await?;
            let pricing = validate_snapshot(&snapshot, id, self.binding(id, plan)?)?;
            ai_platform_gateway_foundation::provider_batch_quota::reserve(
                &self.pool, id, claim, plan, &pricing,
            )
            .await
        })
    }
    fn admit<'a>(&'a self, id: &'a DeliveryIdentity, plan: &'a BatchPlan) -> BatchFuture<'a, ()> {
        Box::pin(async move { self.freeze(id, plan).await.map(|_| ()) })
    }
    fn settle<'a>(
        &'a self,
        id: &'a DeliveryIdentity,
        claim: Uuid,
        plan: &'a BatchPlan,
        receipt: &'a Value,
    ) -> BatchFuture<'a, ()> {
        Box::pin(async move {
            let snapshot = self.freeze(id, plan).await?;
            let pricing = validate_snapshot(&snapshot, id, self.binding(id, plan)?)?;
            let units = completed_units(receipt)?;
            let nanos = pricing
                .quote_nanos(units)
                .map_err(|_| BatchError::AccountingRequired)?;
            let job = receipt["id"].as_str().ok_or(BatchError::BindingMismatch)?;
            let observation = json!({"tenant_id":id.tenant_id,"request_id":id.request_id,"service_id":id.service_id,"request_type":"provider_batch","task_id":id.task_id,"claim_id":claim,"provider_job_id":job,"completed_requests":units,"cost_nanos":nanos,"metadata":{"pricing_snapshot":snapshot}});
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|_| BatchError::Unavailable)?;
            let user_lock = serde_json::to_string(&(&id.tenant_id, &id.user_id))
                .map_err(|_| BatchError::Unavailable)?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,71107))")
                .bind(user_lock)
                .execute(&mut *tx)
                .await
                .map_err(|_| BatchError::Unavailable)?;
            sqlx::query("INSERT INTO gateway.price_book_v2_accounting_checkpoints(tenant_id,request_id,service_id,request_type,stage,observation) VALUES($1,$2,$3,'provider_batch','batch_receipt_verified',$4) ON CONFLICT DO NOTHING").bind(&id.tenant_id).bind(&id.request_id).bind(&id.service_id).bind(&observation).execute(&mut *tx).await.map_err(|_|BatchError::AccountingRequired)?;
            let saved:Value=sqlx::query_scalar("SELECT observation FROM gateway.price_book_v2_accounting_checkpoints WHERE tenant_id=$1 AND request_id=$2 AND service_id=$3 AND request_type='provider_batch' AND stage='batch_receipt_verified' FOR UPDATE").bind(&id.tenant_id).bind(&id.request_id).bind(&id.service_id).fetch_one(&mut *tx).await.map_err(|_|BatchError::Unavailable)?;
            if saved != observation {
                return Err(BatchError::BindingMismatch);
            }
            ai_platform_gateway_foundation::provider_batch_settlement::apply(
                &mut tx, id, claim, &pricing, &snapshot, receipt, units, nanos,
            )
            .await?;
            tx.commit().await.map_err(|_| BatchError::Unavailable)?;
            Ok(())
        })
    }
}
/// Operator-only configuration; never read credentials from task JSON or return them in errors.
pub fn configure(
    pool: PgPool,
    control: PgPool,
    preflight: Arc<ai_platform_gateway_foundation::dispatch_preflight::DispatchPreflight>,
    environment: &BTreeMap<String, String>,
    admission: Arc<dyn ai_platform_gateway_foundation::dispatcher::DispatchAdmission>,
    capacity: Arc<ai_platform_gateway_foundation::capacity::CapacityResolver>,
) -> Result<Option<Arc<BatchCoordinator>>, BatchError> {
    let Some(raw) = environment.get("GATEWAY_PROVIDER_BATCH_PROFILES_JSON") else {
        return Ok(None);
    };
    let profiles: Vec<Profile> =
        serde_json::from_str(raw).map_err(|_| BatchError::NotConfigured)?;
    if profiles.is_empty() || profiles.len() > 128 {
        return Err(BatchError::NotConfigured);
    }
    let mut providers = BatchProviders::default();
    let mut bindings = vec![];
    for profile in profiles {
        if [
            &profile.tenant,
            &profile.provider,
            &profile.model,
            &profile.model_version,
            &profile.book_version,
        ]
        .iter()
        .any(|s| s.is_empty() || s.len() > 255 || s.chars().any(char::is_control))
        {
            return Err(BatchError::NotConfigured);
        }
        // One profile per tenant/provider avoids ambiguous credential routing.
        providers.register(
            &profile.tenant,
            &profile.provider,
            Arc::new(OpenAiBatchProvider::new(profile.api_key)?),
        )?;
        bindings.push(Binding {
            tenant: profile.tenant,
            key: PriceKey {
                book_version: profile.book_version,
                provider: profile.provider,
                model: profile.model,
                model_version: profile.model_version,
                component: Component::Batch,
                currency: "USD".into(),
            },
        });
    }
    let accounting = Arc::new(PgBatchAccounting {
        pool: pool.clone(),
        bindings,
    });
    BatchCoordinator::new(
        pool,
        control,
        preflight,
        Arc::new(providers),
        accounting,
        Arc::new(
            ai_platform_gateway_foundation::provider_batch_lane::BatchLane::new(
                admission, capacity,
            ),
        ),
    )
    .map(Arc::new)
    .map(Some)
    .map_err(|_| BatchError::Unavailable)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_do_not_bill_failed_or_invent_usage() {
        assert_eq!(
            completed_units(&json!({"request_counts":{"total":4,"completed":3,"failed":1}})),
            Ok(3)
        );
        for v in [
            json!({}),
            json!({"request_counts":{"total":1,"completed":2,"failed":0}}),
            json!({"request_counts":{"total":50001,"completed":0,"failed":0}}),
        ] {
            assert!(completed_units(&v).is_err());
        }
    }
}
