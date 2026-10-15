//! Explicit native catalog review. Discovery collects proposals without a model write.
//! Receipts are actor/tenant/provider scoped, bounded and process-local. They do not
//! survive a restart; an uncertain apply is read back, never automatically replayed.
use crate::{
    model_catalog_sync::{
        CatalogHttp, CatalogModels, CatalogProviders, CatalogUpsert, CatalogUpsertFuture,
        ModelCatalogSyncService,
    },
    provider_templates::CatalogModelKwargs,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub mod postgres;
#[cfg(test)]
pub(crate) mod tests;
pub type ReviewFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ReviewError>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewError {
    NotFound,
    Conflict,
    Invalid,
    Unavailable,
    Unknown,
}
impl ReviewError {
    pub fn status(self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::Conflict => 409,
            Self::Invalid => 422,
            Self::Unavailable => 503,
            Self::Unknown => 503,
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::NotFound => {
                "Catalog review not found or expired. Refresh the model list before starting another review."
            }
            Self::Conflict => {
                "Configuration or review changed. Generate a new preview; nothing was applied by this attempt."
            }
            Self::Invalid => {
                "Select reviewed model IDs and explicitly acknowledge existing metadata replacements."
            }
            Self::Unavailable => "Catalog review is unavailable. No automatic sync was attempted.",
            Self::Unknown => {
                "Apply outcome is unknown. Read back this review and current models; do not repeat the write."
            }
        }
    }
}
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub provider_revision: Value,
    pub models: Vec<Value>,
    pub references: Vec<Value>,
    pub reference_coverage: String,
}
pub trait ReviewDatabase: Send + Sync {
    fn snapshot<'a>(
        &'a self,
        tenant: &'a str,
        actor: &'a str,
        provider: &'a str,
    ) -> ReviewFuture<'a, Snapshot>;
    /// Must atomically compare the complete snapshot before any mutation. No
    /// partial application, network discovery, removal or automatic retry here.
    fn apply<'a>(
        &'a self,
        tenant: &'a str,
        actor: &'a str,
        provider: &'a str,
        before: &'a Snapshot,
        proposals: Vec<CatalogModelKwargs>,
    ) -> ReviewFuture<'a, Vec<Value>>;
}
pub use crate::native_contract::CatalogApply as ApplySelection;
struct Collector(Mutex<Vec<CatalogModelKwargs>>);
impl CatalogModels for Collector {
    fn upsert_model_from_catalog<'a>(
        &'a self,
        _tenant: &'a str,
        provider: &'a str,
        kwargs: CatalogModelKwargs,
    ) -> CatalogUpsertFuture<'a> {
        Box::pin(async move {
            let model = json!({"model_id":kwargs.model_id,"provider_id":provider,"display_name":kwargs.display_name,"is_enabled":false}).as_object().unwrap().clone();
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(kwargs);
            Ok(CatalogUpsert {
                status: "created".into(),
                model,
                capability_changed: false,
            })
        })
    }
}
pub fn metadata(k: &CatalogModelKwargs) -> Value {
    json!({"model_id":k.model_id,"display_name":k.display_name,"context_window":k.context_window,"max_output_tokens":k.max_output_tokens,"supports_vision":k.supports_vision,"supports_tools":k.supports_tools,"sort_order":k.sort_order,"catalog_capabilities":k.catalog_capabilities})
}
const FIELDS: &[&str] = &[
    "display_name",
    "context_window",
    "max_output_tokens",
    "supports_vision",
    "supports_tools",
    "sort_order",
    "catalog_capabilities",
];
fn changes(before: Option<&Value>, proposal: &CatalogModelKwargs) -> Value {
    let after = metadata(proposal);
    let changed: Vec<_> = FIELDS.iter().filter(|field| before.and_then(|b|b.get(**field)) != after.get(**field)).map(|field|json!({"field":field,"before":before.and_then(|b|b.get(*field)),"after":after.get(*field)})).collect();
    json!({"model_id":proposal.model_id,"kind":if before.is_none(){"add"}else if changed.is_empty(){"unchanged"}else{"change"},"changes":changed,"existing":before,"proposed":after,"new_model_enabled":false,"new_model_policy":if before.is_none(){json!({"access_level":proposal.access_level,"input_price_per_1k":proposal.input_price_per_1k.to_string(),"output_price_per_1k":proposal.output_price_per_1k.to_string()})}else{Value::Null},"preserved_fields":["input_price_per_1k","output_price_per_1k","access_level","is_enabled","capability_overrides"]})
}
struct Receipt {
    tenant: String,
    actor: String,
    provider: String,
    created: Instant,
    before: Snapshot,
    proposals: Vec<CatalogModelKwargs>,
    state: &'static str,
    result: Option<Value>,
}
pub struct CatalogReview {
    providers: Arc<dyn CatalogProviders>,
    http: Arc<dyn CatalogHttp>,
    database: Arc<dyn ReviewDatabase>,
    receipts: Mutex<BTreeMap<String, Receipt>>,
}
impl CatalogReview {
    pub fn new(
        providers: Arc<dyn CatalogProviders>,
        http: Arc<dyn CatalogHttp>,
        database: Arc<dyn ReviewDatabase>,
    ) -> Self {
        Self {
            providers,
            http,
            database,
            receipts: Mutex::new(BTreeMap::new()),
        }
    }
    pub async fn preview(
        &self,
        tenant: &str,
        actor: &str,
        provider: &str,
    ) -> Result<Value, ReviewError> {
        let before = self.database.snapshot(tenant, actor, provider).await?;
        let collector = Collector(Mutex::new(Vec::new()));
        let discovery =
            ModelCatalogSyncService::new(self.providers.as_ref(), &collector, self.http.as_ref())
                .sync_provider_models(tenant, provider, true)
                .await
                .map_err(|_| ReviewError::Unavailable)?;
        // A provider/model/reference edit during discovery invalidates the entire
        // preview. The later transaction repeats this guard at the write boundary.
        if before != self.database.snapshot(tenant, actor, provider).await? {
            return Err(ReviewError::Conflict);
        }
        let proposals = collector.0.into_inner().unwrap_or_else(|e| e.into_inner());
        if proposals.len() > 500 || before.models.len() > 2000 {
            return Err(ReviewError::Unavailable);
        }
        let entries: Vec<_> = proposals
            .iter()
            .map(|p| {
                changes(
                    before.models.iter().find(|v| v["model_id"] == p.model_id),
                    p,
                )
            })
            .collect();
        let retained: Vec<_> = before
            .models
            .iter()
            .filter(|v| !proposals.iter().any(|p| v["model_id"] == p.model_id))
            .cloned()
            .collect();
        let id = uuid::Uuid::new_v4().to_string();
        let preview = json!({"review_id":id,"provider_id":provider,"state":"ready","expires_in_seconds":900,"provider_revision":before.provider_revision,"changes":entries,"retained_models":retained,"affected_references":before.references,"reference_coverage":before.reference_coverage,"discovery_warnings":discovery["discovery_warnings"],"skipped_models":discovery["skipped_models"],"automatic_removals":false,"notice":"Discovery is not a capability test. Existing metadata replacements require explicit approval; new models are disabled. Missing discovered IDs are not proof of removal. Receipts are session-only."});
        let mut receipts = self.receipts.lock().unwrap_or_else(|e| e.into_inner());
        receipts.retain(|_, r| r.created.elapsed() < Duration::from_secs(900));
        if receipts.len() >= 128 {
            return Err(ReviewError::Unavailable);
        }
        receipts.insert(
            id,
            Receipt {
                tenant: tenant.into(),
                actor: actor.into(),
                provider: provider.into(),
                created: Instant::now(),
                before,
                proposals,
                state: "ready",
                result: None,
            },
        );
        Ok(preview)
    }
    fn scoped<'a>(
        receipts: &'a mut BTreeMap<String, Receipt>,
        tenant: &str,
        actor: &str,
        provider: &str,
        id: &str,
    ) -> Result<&'a mut Receipt, ReviewError> {
        receipts
            .get_mut(id)
            .filter(|r| {
                r.tenant == tenant
                    && r.actor == actor
                    && r.provider == provider
                    && r.created.elapsed() < Duration::from_secs(900)
            })
            .ok_or(ReviewError::NotFound)
    }
    pub async fn apply(
        &self,
        tenant: &str,
        actor: &str,
        provider: &str,
        selection: ApplySelection,
    ) -> Result<Value, ReviewError> {
        let (before, proposals) = {
            let mut receipts = self.receipts.lock().unwrap_or_else(|e| e.into_inner());
            let r = Self::scoped(&mut receipts, tenant, actor, provider, &selection.review_id)?;
            if r.state != "ready" {
                return Err(ReviewError::Unknown);
            }
            let ids: BTreeSet<_> = selection.model_ids.iter().collect();
            if ids.is_empty()
                || ids.len() != selection.model_ids.len()
                || ids.len() > 500
                || ids
                    .iter()
                    .any(|id| !r.proposals.iter().any(|p| &p.model_id == *id))
            {
                return Err(ReviewError::Invalid);
            }
            let proposals: Vec<_> = r
                .proposals
                .iter()
                .filter(|p| ids.contains(&p.model_id))
                .cloned()
                .collect();
            if !selection.replace_existing_metadata
                && proposals
                    .iter()
                    .any(|p| r.before.models.iter().any(|v| v["model_id"] == p.model_id))
            {
                return Err(ReviewError::Invalid);
            }
            r.state = "outcome_unknown"; // Set before the await: cancellation cannot restore retry permission.
            (r.before.clone(), proposals)
        };
        let outcome = self
            .database
            .apply(tenant, actor, provider, &before, proposals)
            .await;
        let mut receipts = self.receipts.lock().unwrap_or_else(|e| e.into_inner());
        let r = Self::scoped(&mut receipts, tenant, actor, provider, &selection.review_id)?;
        match outcome {
            Ok(models) => {
                r.state = "applied";
                r.result = Some(
                    json!({"models":models,"message":"Selected changes committed. New models remain disabled; capability testing is separate."}),
                );
            }
            Err(ReviewError::Conflict) => {
                r.state = "stale";
                r.result = Some(json!({"message":ReviewError::Conflict.message()}));
                return Err(ReviewError::Conflict);
            }
            Err(ReviewError::Invalid) => {
                r.state = "rejected";
                r.result = Some(
                    json!({"message":"Model validation rejected this transaction; no changes committed. Edit the configuration and preview again."}),
                );
                return Err(ReviewError::Invalid);
            }
            Err(_) => return Err(ReviewError::Unknown),
        }
        Ok(
            json!({"review_id":selection.review_id,"provider_id":provider,"state":r.state,"result":r.result}),
        )
    }
    pub async fn read(
        &self,
        tenant: &str,
        actor: &str,
        provider: &str,
        id: &str,
    ) -> Result<Value, ReviewError> {
        let mut value = {
            let mut receipts = self.receipts.lock().unwrap_or_else(|e| e.into_inner());
            let r = Self::scoped(&mut receipts, tenant, actor, provider, id)?;
            json!({"review_id":id,"provider_id":provider,"state":r.state,"result":r.result})
        };
        value["current"] =
            serde_json::to_value(self.database.snapshot(tenant, actor, provider).await?)
                .map_err(|_| ReviewError::Unavailable)?;
        Ok(value)
    }
}
