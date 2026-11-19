//! Canonical scoped usage pricing and the real dispatcher-to-recorder bridge.
//!
//! Source: ai_gateway_core/metrics/usage_recorder.py record(),
//! _normalize_record_dimensions(), _calculate_cost(), and the tenant-scoped
//! branch of _get_model_pricing(). The separate unscoped mutable pricing cache
//! is intentionally not exposed here: recording always has a normalized tenant.
//! PgBillingStore supplies the exact tenant/model/provider LIMIT 2 query and
//! immutable-catalog miss fallback. Ambiguity is explicitly zero-priced with
//! ambiguous_provider status; unknown catalog models use the catalog defaults.
//!
//! Dimension normalization precedes receipt selection, as in Python. Therefore
//! a missing provider can require storage even when a previous receipt exists.
//! Once dimensions are known, the recorder selects the first receipt under its
//! lock before consulting mutable pricing. Database errors never become fallback
//! prices. No production listener, database, or provider is created implicitly.

use crate::{
    billing::{PricingStatus, TokenRates, resolve_pricing},
    billing_store::{BillingStoreError, ModelPrice, PgBillingStore},
    gateway_dispatcher::{DispatcherUsageSink, UsageFuture as SinkFuture, UsageSinkError},
    pricing_receipt::build_pricing_snapshot,
    usage_parser::{python_value_to_string, python_whitespace},
    usage_recorder::{
        FlushReport, UsageFuture, UsageRecord, UsageRecorder, UsageRecorderError,
        UsageSnapshotResolver,
    },
};
use serde_json::{Map, Value, json};
use sqlx::PgPool;
use std::sync::Arc;

pub const UNATTRIBUTED_PROVIDER: &str = "unattributed";
pub const UNATTRIBUTED_MODEL: &str = "unattributed_model";
pub const UNATTRIBUTED_SERVICE: &str = "unattributed_service";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsagePricingStatus {
    TenantModel,
    Catalog,
    ProviderModel,
    Unknown,
    AmbiguousProvider,
    PriceBookV2,
}
impl UsagePricingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TenantModel => "tenant_model",
            Self::Catalog => "catalog",
            Self::ProviderModel => "provider_model",
            Self::Unknown => "unknown",
            Self::AmbiguousProvider => "ambiguous_provider",
            Self::PriceBookV2 => "price_book_v2",
        }
    }
}
impl From<PricingStatus> for UsagePricingStatus {
    fn from(status: PricingStatus) -> Self {
        match status {
            PricingStatus::TenantModel => Self::TenantModel,
            PricingStatus::Catalog => Self::Catalog,
            PricingStatus::ProviderModel => Self::ProviderModel,
            PricingStatus::Unknown => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsagePrice {
    pub provider: String,
    pub rates: TokenRates,
    pub status: UsagePricingStatus,
}
impl UsagePrice {
    fn catalog(model: &str) -> Self {
        let price = resolve_pricing(model);
        Self {
            provider: price.provider.into(),
            rates: price.rates,
            status: price.status.into(),
        }
    }
    fn ambiguous() -> Self {
        Self {
            provider: UNATTRIBUTED_PROVIDER.into(),
            rates: TokenRates::parse("0", "0").expect("zero rates are valid"),
            status: UsagePricingStatus::AmbiguousProvider,
        }
    }
}
impl From<ModelPrice> for UsagePrice {
    fn from(price: ModelPrice) -> Self {
        Self {
            provider: price.provider,
            rates: price.rates,
            status: price.pricing_status.into(),
        }
    }
}

/// Scoped lookup collaborator. Implementations must bind tenant/model exactly,
/// filter by provider when supplied, detect ambiguity, and use only the immutable
/// catalog on a miss. Errors must propagate, never become catalog/zero prices.
pub trait UsagePricingRepository: Send + Sync {
    fn lookup_scoped<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: Option<&'a str>,
    ) -> UsageFuture<'a, UsagePrice>;
}
impl UsagePricingRepository for PgBillingStore {
    fn lookup_scoped<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: Option<&'a str>,
    ) -> UsageFuture<'a, UsagePrice> {
        Box::pin(async move {
            match self.get_model_pricing(tenant, model, provider).await {
                Ok(price) => Ok(price.into()),
                // Billing's interactive lookup requires a provider; the usage
                // recorder instead retains an explicitly ambiguous zero receipt.
                Err(BillingStoreError::ModelProviderRequired) => Ok(UsagePrice::ambiguous()),
                Err(error) => Err(error.into()),
            }
        })
    }
}

#[derive(Clone)]
pub struct ScopedUsagePricing {
    repository: Option<Arc<dyn UsagePricingRepository>>,
    price_book: Option<Arc<crate::price_book::service::PgPriceBookService>>,
}
impl ScopedUsagePricing {
    /// Explicitly select the no-database behavior of the Python recorder.
    pub fn catalog_only() -> Self {
        Self {
            repository: None,
            price_book: None,
        }
    }
    pub fn with_repository(repository: Arc<dyn UsagePricingRepository>) -> Self {
        Self {
            repository: Some(repository),
            price_book: None,
        }
    }
    /// Explicit post-successor activation. Missing/ambiguous prices are typed errors;
    /// this constructor has no catalog/zero fallback and no provider inference.
    pub fn with_price_book_v2(pool: PgPool) -> Self {
        Self {
            repository: None,
            price_book: Some(Arc::new(
                crate::price_book::service::PgPriceBookService::new(pool),
            )),
        }
    }
    pub fn uses_price_book_v2(&self) -> bool {
        self.price_book.is_some()
    }
    pub fn with_postgres(pool: PgPool) -> Self {
        Self::with_repository(Arc::new(PgBillingStore::new(pool)))
    }
    pub async fn lookup(
        &self,
        tenant: &str,
        model: &str,
        provider: Option<&str>,
    ) -> Result<UsagePrice, UsageRecorderError> {
        if let Some(service) = &self.price_book {
            let provider = provider
                .filter(|p| !p.is_empty() && *p != UNATTRIBUTED_PROVIDER)
                .ok_or(crate::price_book::PriceBookError::InvalidTerms)?;
            let snapshot = service
                .token_quote(
                    tenant,
                    provider,
                    model,
                    chrono::Utc::now().timestamp_millis(),
                )
                .await?;
            let receipt = crate::pricing_receipt::validate_pricing_snapshot(
                &snapshot, tenant, provider, model,
            )?;
            return Ok(UsagePrice {
                provider: provider.into(),
                rates: TokenRates::parse(
                    &receipt.input_price_per_1k,
                    &receipt.output_price_per_1k,
                )?,
                status: UsagePricingStatus::PriceBookV2,
            });
        }
        let provider =
            provider.filter(|value| !value.is_empty() && *value != UNATTRIBUTED_PROVIDER);
        match &self.repository {
            Some(repository) => repository.lookup_scoped(tenant, model, provider).await,
            None => Ok(UsagePrice::catalog(model)),
        }
    }
    async fn provider_for_model(
        &self,
        tenant: &str,
        model: &str,
    ) -> Result<String, UsageRecorderError> {
        let price = self.lookup(tenant, model, None).await?;
        // Python tests truthiness before strip: a whitespace-only provider
        // returns empty here, without subsequently trying prefix inference.
        if !price.provider.is_empty() {
            return Ok(trim(&price.provider).into());
        }
        let model = model.to_lowercase();
        const PREFIXES: &[(&str, &str)] = &[
            ("deepseek", "deepseek"),
            ("mixtral", "mistral"),
            ("mistral", "mistral"),
            ("gemini", "google"),
            ("claude", "anthropic"),
            ("command", "cohere"),
            ("llama", "meta"),
            ("qwen", "dashscope"),
            ("gpt", "openai"),
        ];
        Ok(PREFIXES
            .iter()
            .find(|(prefix, _)| model.starts_with(prefix))
            .map_or("", |(_, provider)| *provider)
            .into())
    }
    /// Source-ordered normalization, including provider inference before price
    /// calculation. Request/assistant identity and existing metadata are retained.
    pub async fn normalize_record(
        &self,
        record: &mut UsageRecord,
    ) -> Result<(), UsageRecorderError> {
        if self.price_book.is_some() {
            if [
                &record.tenant_id,
                &record.provider,
                &record.model,
                &record.request_id,
            ]
            .iter()
            .any(|s| s.is_empty() || s.trim() != s.as_str())
                || record.provider == UNATTRIBUTED_PROVIDER
            {
                return Err(crate::price_book::PriceBookError::InvalidTerms.into());
            }
            // A caller-supplied snapshot cannot bypass the durable first-receipt
            // authority. Recorder-owned prior observations still win under lock.
            record.metadata.remove("pricing_snapshot");
            record.metadata.remove("_price_book_v2_cumulative");
            return Ok(());
        }
        let model = if record.model.is_empty() {
            metadata_identity(&record.metadata, &["model", "model_id", "llm_model"])
        } else {
            record.model.clone()
        };
        record.model = normalized_identity(&model, UNATTRIBUTED_MODEL);
        record.service_id = normalized_identity(&record.service_id, UNATTRIBUTED_SERVICE);
        record.tenant_id = normalized_identity(&record.tenant_id, "default");
        record.user_id = normalized_identity(&record.user_id, "anonymous");
        let provider = if record.provider.is_empty() {
            metadata_identity(
                &record.metadata,
                &["provider", "provider_id", "vendor", "llm_provider"],
            )
        } else {
            record.provider.clone()
        };
        let provider = normalized_identity(&provider, "");
        let provider = if provider.is_empty() {
            self.provider_for_model(&record.tenant_id, &record.model)
                .await?
        } else {
            provider
        };
        record.provider = if provider.is_empty() {
            UNATTRIBUTED_PROVIDER.into()
        } else {
            provider
        };
        record
            .metadata
            .entry("provider")
            .or_insert_with(|| json!(record.provider));
        record
            .metadata
            .entry("model")
            .or_insert_with(|| json!(record.model));
        if record.provider == UNATTRIBUTED_PROVIDER {
            record
                .metadata
                .entry("attribution_issue")
                .or_insert_with(|| json!("missing_provider"));
        }
        record.metadata.entry("token_source").or_insert_with(|| {
            json!(if record.input_tokens > 0 || record.output_tokens > 0 {
                "upstream"
            } else if record.status.to_lowercase() != "success" {
                "zero_on_failure"
            } else {
                "estimated"
            })
        });
        if record.request_total_duration_ms <= 0 && record.latency_ms > 0 {
            record.request_total_duration_ms = record.latency_ms;
        }
        if let Some(error) = &record.error_type
            && !error.is_empty()
        {
            let error = trim(error).to_lowercase();
            record.error_type = Some(match error.as_str() {
                "timeout" | "provider_error" | "rate_limit" | "auth_error" | "tool_error"
                | "content_filter" | "unknown" => error,
                _ => "unknown".into(),
            });
        }
        Ok(())
    }
}
impl UsageSnapshotResolver for ScopedUsagePricing {
    fn resolve<'a>(&'a self, record: &'a UsageRecord) -> UsageFuture<'a, Value> {
        Box::pin(async move {
            if let Some(service) = &self.price_book {
                return service
                    .freeze_usage(record)
                    .await
                    .map_err(UsageRecorderError::from);
            }
            let price = self
                .lookup(&record.tenant_id, &record.model, Some(&record.provider))
                .await?;
            // Bind the receipt to normalized record dimensions, even when a
            // catalog miss/provider ambiguity reports a different price provider.
            Ok(build_pricing_snapshot(
                &record.tenant_id,
                &record.provider,
                &record.model,
                &price.rates.input_price_per_1k().to_string(),
                &price.rates.output_price_per_1k().to_string(),
                price.status.as_str(),
            )?
            .to_value())
        })
    }
}

fn trim(value: &str) -> &str {
    value.trim_matches(python_whitespace)
}
fn normalized_identity(value: &str, fallback: &str) -> String {
    let value = trim(value);
    if value.is_empty()
        || matches!(
            value.to_lowercase().as_str(),
            "unknown" | "none" | "null" | "n/a" | "na"
        )
    {
        fallback.into()
    } else {
        value.into()
    }
}
fn metadata_identity(metadata: &Map<String, Value>, keys: &[&str]) -> String {
    keys.iter()
        .filter_map(|key| metadata.get(*key))
        .filter(|value| !value.is_null())
        .map(python_value_to_string)
        .map(|value| trim(&value).to_owned())
        .find(|value| !value.is_empty())
        .unwrap_or_default()
}

/// Actual dispatcher accounting adapter. Construction requires an existing
/// recorder and an explicit pricing source, with no implicit no-op sink.
#[derive(Clone)]
pub struct PricedUsageSink {
    recorder: UsageRecorder,
    pricing: Arc<ScopedUsagePricing>,
}
impl PricedUsageSink {
    pub fn new(recorder: UsageRecorder, pricing: Arc<ScopedUsagePricing>) -> Self {
        Self { recorder, pricing }
    }
    pub fn recorder(&self) -> &UsageRecorder {
        &self.recorder
    }
    /// Typed internal entrypoint retains flush reports, including requeue and
    /// uncertain-commit outcomes. Once polled, normalization and recording are
    /// owned by the runtime so cancellation of the dispatcher waiter cannot
    /// discard an observation that is in the pricing/recording operation.
    pub async fn record_usage(
        &self,
        mut record: UsageRecord,
    ) -> Result<Option<FlushReport>, UsageRecorderError> {
        let owned = self.clone();
        tokio::spawn(async move {
            owned.pricing.normalize_record(&mut record).await?;
            owned
                .recorder
                .record_with_resolver(record, owned.pricing)
                .await
        })
        .await
        .expect("owned priced usage sink task panicked")
    }
}
impl DispatcherUsageSink for PricedUsageSink {
    fn record(&self, record: UsageRecord) -> SinkFuture<'_> {
        Box::pin(async move {
            self.record_usage(record)
                .await
                .map(|_| ())
                .map_err(|_| UsageSinkError)
        })
    }
}
