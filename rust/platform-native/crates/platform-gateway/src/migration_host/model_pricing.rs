//! Mutable ModelPricingService from src/services/billing/model_pricing.py.
//!
//! Unscoped legacy mirror reads retain database/catalog insertion order, exact
//! keys, first-prefix matching and metadata. Scoped reads bypass that cache and
//! reuse PgBillingStore's tenant/model/provider lookup and ambiguity handling.
//! Neither an absent database nor a scoped miss can expose another tenant's
//! legacy mirror. No connection, refresh, mutation or task runs at construction.
//!
//! Source SQL is retained; mutation errors are deliberately redacted rather than
//! returning Python's raw database exception string. Existing TokenRates bounds
//! still apply (finite nonnegative rates and checked decimal/i64 arithmetic).
use crate::{
    billing::{BillingError, PricingStatus, TokenRates, model_pricing_defaults, resolve_pricing},
    billing_store::{BillingStoreError, ModelPrice as BillingModelPrice, PgBillingStore},
    model_service::{ModelPricingCache, ModelServiceError},
};
use ai_platform_gateway_foundation::price_book::{
    self, PriceBookError,
    service::{Activation, PgPriceBookService, Publication},
};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use std::{
    ops::Deref,
    sync::{Arc, Mutex, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

pub const DEFAULT_CACHE_TTL_SECONDS: f64 = 300.0;

/// Wall time, as in time.time(); clock reversal and zero/negative TTL retain
/// the source comparison rather than being silently converted to monotonic TTL.
pub trait PricingClock: Send + Sync {
    fn now(&self) -> f64;
}
pub struct SystemPricingClock;
impl PricingClock for SystemPricingClock {
    fn now(&self) -> f64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(value) => value.as_secs_f64(),
            Err(error) => -error.duration().as_secs_f64(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum PricingEvent {
    UpdateFailed,
    ActiveStatusFailed,
    RefreshFailed,
    DecodeFailed,
    Refreshed(usize),
}
pub trait PricingObserver: Send + Sync {
    fn record(&self, event: PricingEvent);
}
/// Redacted stderr diagnostics; a host may install its configured log sink.
/// Debug refresh counts are delivered to observers but suppressed by default.
pub struct StderrPricingObserver;
impl PricingObserver for StderrPricingObserver {
    fn record(&self, event: PricingEvent) {
        use std::io::Write;
        let message = match event {
            PricingEvent::UpdateFailed => "Failed to update model pricing",
            PricingEvent::ActiveStatusFailed => "Failed to set model active status",
            PricingEvent::RefreshFailed => "Failed to refresh model pricing cache",
            PricingEvent::DecodeFailed => "Failed to decode model pricing cache",
            PricingEvent::Refreshed(_) => return,
        };
        let _ = writeln!(std::io::stderr(), "{message}");
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPrice {
    pub price: BillingModelPrice,
    pub display_name: String,
    pub context_window: i64,
    pub max_output_tokens: i64,
    pub supports_vision: bool,
    pub supports_tools: bool,
    pub is_active: bool,
}
impl Deref for ModelPrice {
    type Target = BillingModelPrice;
    fn deref(&self) -> &Self::Target {
        &self.price
    }
}
impl From<BillingModelPrice> for ModelPrice {
    fn from(price: BillingModelPrice) -> Self {
        Self {
            price,
            display_name: String::new(),
            context_window: 0,
            max_output_tokens: 0,
            supports_vision: false,
            supports_tools: true,
            is_active: true,
        }
    }
}
impl ModelPrice {
    fn default_for(model: &str) -> Self {
        let resolved = resolve_pricing(model);
        BillingModelPrice {
            model: model.into(),
            provider: resolved.provider.into(),
            rates: resolved.rates,
            pricing_status: resolved.status,
        }
        .into()
    }
    pub fn calculate_cost(
        &self,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Result<Value, BillingError> {
        let status = match self.pricing_status {
            PricingStatus::Catalog => "catalog",
            PricingStatus::ProviderModel => "provider_model",
            PricingStatus::Unknown => "unknown",
            PricingStatus::TenantModel => "tenant_model",
        };
        self.rates
            .calculate_cost(input_tokens, output_tokens)?
            .model_price_response(status)
    }
    /// The source intentionally omits pricing_status from metadata projection.
    pub fn to_dict(&self) -> Value {
        let mut value = self.price.to_dict();
        value["display_name"] = json!(if self.display_name.is_empty() {
            &self.model
        } else {
            &self.display_name
        });
        value["capabilities"] = json!({"context_window":self.context_window,"max_output_tokens":self.max_output_tokens,
            "supports_vision":self.supports_vision,"supports_tools":self.supports_tools});
        value["is_active"] = json!(self.is_active);
        value
    }
}

/// Rates cross the SQL boundary as decimal text, avoiding binary rounding.
/// Optional metadata remains SQL NULL so INSERT and COALESCE-update semantics
/// stay distinct. is_active is controlled by set_model_active, not this write.
pub struct PricingUpdate {
    pub input_price_per_1k: String,
    pub output_price_per_1k: String,
    pub provider: Option<String>,
    pub display_name: Option<String>,
    pub context_window: Option<i32>,
    pub max_output_tokens: Option<i32>,
    pub supports_vision: Option<bool>,
    pub supports_tools: Option<bool>,
}
#[derive(FromRow)]
struct PricingRow {
    model: String,
    provider: Option<String>,
    display_name: Option<String>,
    input_price: String,
    output_price: String,
    context_window: Option<i32>,
    max_output_tokens: Option<i32>,
    supports_vision: Option<bool>,
    supports_tools: Option<bool>,
    is_active: Option<bool>,
}
impl PricingRow {
    fn into_price(self) -> Result<ModelPrice, BillingError> {
        let display_name = self
            .display_name
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| self.model.clone());
        Ok(ModelPrice {
            price: BillingModelPrice {
                model: self.model,
                provider: self
                    .provider
                    .filter(|v| !v.is_empty())
                    .unwrap_or_else(|| "unknown".into()),
                rates: TokenRates::parse(&self.input_price, &self.output_price)?,
                pricing_status: PricingStatus::Catalog,
            },
            display_name,
            context_window: i64::from(self.context_window.unwrap_or(0)),
            max_output_tokens: i64::from(self.max_output_tokens.unwrap_or(0)),
            supports_vision: self.supports_vision.unwrap_or(false),
            supports_tools: self.supports_tools.unwrap_or(true),
            is_active: self.is_active.unwrap_or(true),
        })
    }
}
#[derive(Default)]
struct Cache {
    // SQL's UNIQUE(model) and the immutable catalog provide unique ordered keys.
    models: Vec<ModelPrice>,
    refreshed_at: f64,
}
pub struct ModelPricingService {
    database: RwLock<Option<PgPool>>,
    cache_ttl: f64,
    cache: Mutex<Cache>,
    clock: Arc<dyn PricingClock>,
    observer: Arc<dyn PricingObserver>,
    price_book_v2: bool,
}
impl ModelPricingService {
    pub fn new(database: Option<PgPool>, cache_ttl_seconds: f64) -> Self {
        Self::with_clock(database, cache_ttl_seconds, Arc::new(SystemPricingClock))
    }
    pub fn with_clock(
        database: Option<PgPool>,
        cache_ttl_seconds: f64,
        clock: Arc<dyn PricingClock>,
    ) -> Self {
        Self {
            database: RwLock::new(database),
            cache_ttl: cache_ttl_seconds,
            cache: Mutex::new(Cache::default()),
            clock,
            observer: Arc::new(StderrPricingObserver),
            price_book_v2: false,
        }
    }
    /// Explicit activation after price-book schema admission. Legacy methods remain callable,
    /// while tenant-scoped quotes/costs use the same persisted v2 publications
    /// and frozen-snapshot contract as ScopedUsagePricing::with_price_book_v2.
    pub fn with_price_book_v2(mut self) -> Self {
        self.price_book_v2 = true;
        self
    }
    pub fn price_book_service(&self) -> Result<Arc<PgPriceBookService>, PriceBookError> {
        Ok(Arc::new(PgPriceBookService::new(
            self.database().ok_or(PriceBookError::Storage)?,
        )))
    }
    pub fn price_book_router(
        &self,
        identity: Arc<
            dyn ai_platform_gateway_foundation::model_admin_api::ModelAdminIdentityResolver,
        >,
    ) -> Result<axum::Router, PriceBookError> {
        Ok(price_book::api::router(
            self.price_book_service()?,
            identity,
        ))
    }
    pub async fn publish_price_book(
        &self,
        tenant: &str,
        actor: &str,
        publication: Publication,
    ) -> Result<String, PriceBookError> {
        self.price_book_service()?
            .publish(tenant, actor, publication)
            .await
    }
    pub async fn activate_price_book(
        &self,
        tenant: &str,
        actor: &str,
        activation: &Activation,
    ) -> Result<i64, PriceBookError> {
        self.price_book_service()?
            .activate(
                tenant,
                actor,
                activation,
                chrono::Utc::now().timestamp_millis(),
            )
            .await
    }
    pub async fn calculate_cost_v2(
        &self,
        tenant: &str,
        provider: &str,
        model: &str,
        input: i64,
        output: i64,
    ) -> Result<Value, BillingStoreError> {
        let snapshot = self
            .price_book_service()?
            .token_quote(
                tenant,
                provider,
                model,
                chrono::Utc::now().timestamp_millis(),
            )
            .await?;
        let input_rate = snapshot["input_price_per_1k"]
            .as_str()
            .ok_or(PriceBookError::InvalidReference)?;
        let output_rate = snapshot["output_price_per_1k"]
            .as_str()
            .ok_or(PriceBookError::InvalidReference)?;
        let mut cost = TokenRates::parse(input_rate, output_rate)?
            .calculate_cost(input, output)?
            .model_price_response("price_book_v2")?;
        cost["model"] = json!(model);
        cost["provider"] = json!(provider);
        cost["pricing_snapshot"] = snapshot;
        Ok(cost)
    }
    pub fn with_observer(mut self, observer: Arc<dyn PricingObserver>) -> Self {
        self.observer = observer;
        self
    }
    fn database(&self) -> Option<PgPool> {
        self.database
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    /// Source set_database does not invalidate an already-fresh cache.
    pub fn set_database(&self, database: PgPool) {
        *self.database.write().unwrap_or_else(|e| e.into_inner()) = Some(database);
    }
    pub fn invalidate_cache(&self) {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .refreshed_at = 0.0;
    }
    pub async fn get_model_pricing(
        &self,
        model: &str,
        tenant_id: Option<&str>,
        provider_id: Option<&str>,
    ) -> Result<ModelPrice, BillingStoreError> {
        if self.price_book_v2 {
            let tenant = tenant_id
                .filter(|v| !v.is_empty())
                .ok_or(PriceBookError::InvalidTerms)?;
            let provider = provider_id
                .filter(|v| !v.is_empty())
                .ok_or(PriceBookError::InvalidTerms)?;
            let snapshot = self
                .price_book_service()?
                .token_quote(
                    tenant,
                    provider,
                    model,
                    chrono::Utc::now().timestamp_millis(),
                )
                .await?;
            let input = snapshot["input_price_per_1k"]
                .as_str()
                .ok_or(PriceBookError::InvalidReference)?;
            let output = snapshot["output_price_per_1k"]
                .as_str()
                .ok_or(PriceBookError::InvalidReference)?;
            return Ok(BillingModelPrice {
                model: model.into(),
                provider: provider.into(),
                rates: TokenRates::parse(input, output)?,
                pricing_status: PricingStatus::TenantModel,
            }
            .into());
        }
        if let Some(tenant) = tenant_id {
            return match self.database() {
                Some(pool) => PgBillingStore::new(pool)
                    .get_model_pricing(tenant, model, provider_id)
                    .await
                    .map(Into::into),
                None => Ok(ModelPrice::default_for(model)),
            };
        }
        self.ensure_cache().await;
        let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(price) = cache.models.iter().find(|p| p.model == model) {
            return Ok(price.clone());
        }
        // Deliberately case-sensitive, first ordered prefix, and cloned with the
        // cached model ID. Do not replace with the catalog's longest-prefix resolver.
        if let Some(price) = cache
            .models
            .iter()
            .find(|p| model.starts_with(p.model.as_str()))
        {
            let mut price = price.clone();
            price.price.pricing_status = PricingStatus::ProviderModel;
            return Ok(price);
        }
        Ok(cache
            .models
            .iter()
            .find(|p| p.model == "default")
            .cloned()
            .unwrap_or_else(|| ModelPrice::default_for(model)))
    }
    pub async fn get_all_pricing(&self, active_only: bool) -> Vec<ModelPrice> {
        self.ensure_cache().await;
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .models
            .iter()
            .filter(|p| !active_only || p.is_active)
            .cloned()
            .collect()
    }
    pub async fn calculate_cost(
        &self,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
        tenant_id: Option<&str>,
        provider_id: Option<&str>,
    ) -> Result<Value, BillingStoreError> {
        if self.price_book_v2 {
            return self
                .calculate_cost_v2(
                    tenant_id.ok_or(PriceBookError::InvalidTerms)?,
                    provider_id.ok_or(PriceBookError::InvalidTerms)?,
                    model,
                    input_tokens,
                    output_tokens,
                )
                .await;
        }
        let price = self
            .get_model_pricing(model, tenant_id, provider_id)
            .await?;
        let mut cost = price.calculate_cost(input_tokens, output_tokens)?;
        cost["model"] = json!(price.model);
        cost["provider"] = json!(price.provider);
        Ok(cost)
    }
    /// Legacy wire adapter; failures retain JSON errors, never a false success.
    pub async fn update_pricing(&self, model: &str, update: PricingUpdate) -> Value {
        match self.try_update_pricing(model, update).await {
            Ok(()) => json!({"success":true,"model":model}),
            Err(PriceBookError::RevisionConflict) => {
                json!({"error":"model_pricing_provider_collision"})
            }
            Err(PriceBookError::InvalidTerms) => {
                json!({"error":"model_pricing_explicit_provider_and_valid_rates_required"})
            }
            Err(_) => {
                self.observer.record(PricingEvent::UpdateFailed);
                json!({"error":"model_pricing_database_error"})
            }
        }
    }
    /// Old UNIQUE(model) cannot represent two providers: require attribution and
    /// refuse a collision instead of overwriting the existing provider's price.
    /// Null/ambiguous historical rows must migrate through reviewed quarantine.
    pub async fn try_update_pricing(
        &self,
        model: &str,
        update: PricingUpdate,
    ) -> Result<(), PriceBookError> {
        if model.trim().is_empty()
            || model.trim() != model
            || update
                .provider
                .as_deref()
                .is_none_or(|p| p.trim().is_empty() || p.trim() != p)
        {
            return Err(PriceBookError::InvalidTerms);
        }
        TokenRates::parse(&update.input_price_per_1k, &update.output_price_per_1k)
            .map_err(|_| PriceBookError::InvalidTerms)?;
        let pool = self.database().ok_or(PriceBookError::Storage)?;
        let result = sqlx::query(UPDATE_PRICING)
            .bind(model)
            .bind(update.provider)
            .bind(update.display_name)
            .bind(update.input_price_per_1k)
            .bind(update.output_price_per_1k)
            .bind(update.context_window)
            .bind(update.max_output_tokens)
            .bind(update.supports_vision)
            .bind(update.supports_tools)
            .execute(&pool)
            .await
            .map_err(|_| PriceBookError::Storage)?;
        if result.rows_affected() != 1 {
            return Err(PriceBookError::RevisionConflict);
        }
        self.invalidate_cache();
        Ok(())
    }
    pub async fn set_model_active(&self, model: &str, is_active: bool) -> bool {
        let Some(pool) = self.database() else {
            return false;
        };
        match sqlx::query(
            "UPDATE model_pricing SET is_active=$2,updated_at=CURRENT_TIMESTAMP WHERE model=$1",
        )
        .bind(model)
        .bind(is_active)
        .execute(&pool)
        .await
        {
            Ok(_) => {
                self.invalidate_cache();
                true
            }
            Err(_) => {
                self.observer.record(PricingEvent::ActiveStatusFailed);
                false
            }
        }
    }
    async fn ensure_cache(&self) {
        let now = self.clock.now();
        {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if now - cache.refreshed_at < self.cache_ttl && !cache.models.is_empty() {
                return;
            }
        }
        // No extra single-flight lock: source concurrent refreshes may complete
        // in either order. No synchronous lock crosses a database await.
        self.refresh_cache().await;
    }
    async fn refresh_cache(&self) {
        if let Some(pool) = self.database() {
            match sqlx::query_as::<_, PricingRow>(SELECT_PRICING)
                .fetch_all(&pool)
                .await
            {
                Ok(rows) => match rows
                    .into_iter()
                    .map(PricingRow::into_price)
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(models) => {
                        let count = models.len();
                        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = Cache {
                            models,
                            refreshed_at: self.clock.now(),
                        };
                        self.observer.record(PricingEvent::Refreshed(count));
                        return;
                    }
                    Err(_) => self.observer.record(PricingEvent::DecodeFailed),
                },
                Err(_) => self.observer.record(PricingEvent::RefreshFailed),
            }
        }
        self.load_default_pricing();
    }
    fn load_default_pricing(&self) {
        let models = model_pricing_defaults()
            .map(|(model, resolved)| {
                let pricing_status = if model == "default" {
                    PricingStatus::Unknown
                } else {
                    PricingStatus::Catalog
                };
                BillingModelPrice {
                    model: model.into(),
                    provider: resolved.provider.into(),
                    rates: resolved.rates,
                    pricing_status,
                }
                .into()
            })
            .collect();
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) = Cache {
            models,
            refreshed_at: self.clock.now(),
        };
    }
}
impl Default for ModelPricingService {
    fn default() -> Self {
        Self::new(None, DEFAULT_CACHE_TTL_SECONDS)
    }
}
impl ModelPricingCache for ModelPricingService {
    fn invalidate_cache(&self) -> Result<(), ModelServiceError> {
        ModelPricingService::invalidate_cache(self);
        Ok(())
    }
}
const SELECT_PRICING: &str = "SELECT model,provider,display_name,input_price_per_1k::text AS input_price,output_price_per_1k::text AS output_price,context_window,max_output_tokens,supports_vision,supports_tools,is_active FROM model_pricing";
const UPDATE_PRICING: &str = "INSERT INTO model_pricing (model,provider,display_name,input_price_per_1k,output_price_per_1k,context_window,max_output_tokens,supports_vision,supports_tools) VALUES ($1,$2,$3,$4::text::numeric,$5::text::numeric,$6,$7,$8,$9) ON CONFLICT (model) DO UPDATE SET input_price_per_1k=EXCLUDED.input_price_per_1k,output_price_per_1k=EXCLUDED.output_price_per_1k,provider=COALESCE(EXCLUDED.provider,model_pricing.provider),display_name=COALESCE(EXCLUDED.display_name,model_pricing.display_name),context_window=COALESCE(EXCLUDED.context_window,model_pricing.context_window),max_output_tokens=COALESCE(EXCLUDED.max_output_tokens,model_pricing.max_output_tokens),supports_vision=COALESCE(EXCLUDED.supports_vision,model_pricing.supports_vision),supports_tools=COALESCE(EXCLUDED.supports_tools,model_pricing.supports_tools),updated_at=CURRENT_TIMESTAMP WHERE model_pricing.provider=EXCLUDED.provider";

#[cfg(test)]
mod price_book_tests {
    use super::*;
    fn update(provider: Option<&str>) -> PricingUpdate {
        PricingUpdate {
            input_price_per_1k: "0.001".into(),
            output_price_per_1k: "0.002".into(),
            provider: provider.map(str::to_owned),
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            supports_vision: None,
            supports_tools: None,
        }
    }
    #[tokio::test]
    async fn legacy_update_requires_explicit_provider_before_database_access() {
        let service = ModelPricingService::new(None, 300.0);
        assert_eq!(
            service.try_update_pricing("same", update(None)).await,
            Err(PriceBookError::InvalidTerms)
        );
        assert_eq!(
            service.try_update_pricing("same", update(Some("  "))).await,
            Err(PriceBookError::InvalidTerms)
        );
        assert_eq!(
            service.try_update_pricing("same", update(Some("p"))).await,
            Err(PriceBookError::Storage)
        );
    }
    #[tokio::test]
    async fn activated_v2_cannot_fall_back_to_unscoped_catalog() {
        let service = ModelPricingService::new(None, 300.0).with_price_book_v2();
        assert!(matches!(
            service
                .get_model_pricing("gpt-example", Some("tenant"), None)
                .await,
            Err(BillingStoreError::PriceBook(PriceBookError::InvalidTerms))
        ));
        assert!(matches!(
            service
                .get_model_pricing("gpt-example", Some("tenant"), Some("p"))
                .await,
            Err(BillingStoreError::PriceBook(PriceBookError::Storage))
        ));
    }
}
