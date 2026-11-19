//! Pure billing and quota contracts migrated from the Python Gateway.
//!
//! Source authorities:
//! - `packages/ai-gateway-core/src/ai_gateway_core/billing/pricing_catalog.py`
//! - `src/services/billing/model_pricing.py::ModelPrice.calculate_cost`
//! - `src/services/billing/quota_service.py::QuotaService.check_quota`
//! - `packages/ai-gateway-core/src/ai_gateway_core/metrics/usage_recorder.py`
//!
//! There are deliberately three different rounding contracts: model-price cents
//! truncate toward zero; catalog cents round the combined cost half-even; usage
//! storage rounds each component half-even to *legacy microcents*. Those legacy
//! units are one million per USD, despite their name and the `*_cost_cents`
//! database column names. They must not be confused with quota cents (100/USD).
//!
//! No storage, provider calls, caches, quota resets, RPM reservations, or receipt
//! persistence live here. The caller must supply tenant-scoped current usage and
//! an RPM observation from its existing limiter. These helpers are not wired to
//! a public route and do not authorize or settle a financial transaction.
//!
//! Decimal arithmetic is exact within the supported 28-significant-digit,
//! 28-fractional-digit envelope. Unrepresentable inputs/intermediates and integer
//! overflow fail explicitly instead of rounding silently or wrapping. Python's
//! arbitrary-size integers and decimal-context rounding outside that envelope
//! remain a migration gap. Floating point appears only in legacy presentation.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;

pub const MICROCENTS_PER_USD: i64 = 1_000_000;
pub const MICROCENTS_PER_CENT: i64 = 10_000;
pub const BILLING_CURRENCY: &str = "USD";
pub const BILLING_PRICE_UNIT: &str = "per_1k_tokens";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BillingError {
    InvalidModelPrice,
    DecimalPrecisionExceeded,
    IntegerOverflow,
}

impl fmt::Display for BillingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidModelPrice => "invalid_model_price",
            Self::DecimalPrecisionExceeded => "billing_decimal_precision_exceeded",
            Self::IntegerOverflow => "billing_integer_overflow",
        })
    }
}

impl std::error::Error for BillingError {}

fn pow10(power: u32) -> Result<i128, BillingError> {
    10_i128
        .checked_pow(power)
        .ok_or(BillingError::DecimalPrecisionExceeded)
}

/// Construct exactly, removing trailing zeros before checking the bounds.
fn exact_decimal(mut mantissa: i128, mut scale: u32) -> Result<Decimal, BillingError> {
    if mantissa == 0 {
        return Ok(Decimal::ZERO);
    }
    while scale > 0 && mantissa % 10 == 0 {
        mantissa /= 10;
        scale -= 1;
    }
    // Match the default Python Decimal context without silently rounding it.
    if scale > 28 || mantissa.unsigned_abs() >= 10_u128.pow(28) {
        return Err(BillingError::DecimalPrecisionExceeded);
    }
    Decimal::try_from_i128_with_scale(mantissa, scale)
        .map_err(|_| BillingError::DecimalPrecisionExceeded)
}

fn exact_add(left: Decimal, right: Decimal) -> Result<Decimal, BillingError> {
    let scale = left.scale().max(right.scale());
    let left = left
        .mantissa()
        .checked_mul(pow10(scale - left.scale())?)
        .ok_or(BillingError::DecimalPrecisionExceeded)?;
    let right = right
        .mantissa()
        .checked_mul(pow10(scale - right.scale())?)
        .ok_or(BillingError::DecimalPrecisionExceeded)?;
    exact_decimal(
        left.checked_add(right)
            .ok_or(BillingError::DecimalPrecisionExceeded)?,
        scale,
    )
}

fn token_cost(rate: Decimal, tokens: i64) -> Result<Decimal, BillingError> {
    let mantissa = rate
        .mantissa()
        .checked_mul(i128::from(tokens))
        .ok_or(BillingError::DecimalPrecisionExceeded)?;
    exact_decimal(mantissa, rate.scale() + 3)
}

fn decimal_float(value: Decimal) -> Result<f64, BillingError> {
    // A single correctly rounded decimal-to-binary conversion, matching
    // Python float(Decimal). Do not divide separately rounded float mantissas.
    value
        .to_string()
        .parse()
        .map_err(|_| BillingError::DecimalPrecisionExceeded)
}

/// Scale USD to integer minor units; Python `int` truncates, `round` is half-even.
fn integer_units(value: Decimal, places: u32, half_even: bool) -> Result<i64, BillingError> {
    let mantissa = value.mantissa();
    let integer = if value.scale() <= places {
        mantissa
            .checked_mul(pow10(places - value.scale())?)
            .ok_or(BillingError::IntegerOverflow)?
    } else {
        let divisor = pow10(value.scale() - places)?;
        let quotient = mantissa / divisor;
        let remainder = (mantissa % divisor).abs();
        let round_up = half_even
            && (remainder > divisor / 2 || (remainder == divisor / 2 && quotient % 2 != 0));
        quotient + if round_up { mantissa.signum() } else { 0 }
    };
    i64::try_from(integer).map_err(|_| BillingError::IntegerOverflow)
}

pub fn usd_to_microcents(value: Decimal) -> Result<i64, BillingError> {
    integer_units(value, 6, true)
}

/// Exact USD amount. Convert to a JSON float only at the presentation boundary.
pub fn microcents_to_usd(value: i64) -> Decimal {
    Decimal::new(value, 6)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenRates {
    input_price_per_1k: Decimal,
    output_price_per_1k: Decimal,
}

impl TokenRates {
    /// Prices are finite, nonnegative USD per 1K tokens. Strings avoid binary
    /// float conversion. The validation follows the immutable Python receipt.
    /// Fixed-point and scientific notation are parsed without rounding.
    pub fn parse(input: &str, output: &str) -> Result<Self, BillingError> {
        fn price(value: &str) -> Result<Decimal, BillingError> {
            let mut parts = value.trim().split(['e', 'E']);
            let coefficient = parts.next().ok_or(BillingError::InvalidModelPrice)?;
            let parsed = Decimal::from_str_exact(coefficient)
                .map_err(|_| BillingError::InvalidModelPrice)?;
            let exponent = parts
                .next()
                .map(|value| {
                    value
                        .parse::<i32>()
                        .map_err(|_| BillingError::InvalidModelPrice)
                })
                .transpose()?
                .unwrap_or(0);
            if parts.next().is_some() {
                return Err(BillingError::InvalidModelPrice);
            }
            if parsed < Decimal::ZERO {
                return Err(BillingError::InvalidModelPrice);
            }
            let scale = i64::from(parsed.scale()) - i64::from(exponent);
            if scale >= 0 {
                exact_decimal(
                    parsed.mantissa(),
                    u32::try_from(scale).map_err(|_| BillingError::DecimalPrecisionExceeded)?,
                )
            } else {
                let shift =
                    u32::try_from(-scale).map_err(|_| BillingError::DecimalPrecisionExceeded)?;
                let mantissa = parsed
                    .mantissa()
                    .checked_mul(pow10(shift)?)
                    .ok_or(BillingError::DecimalPrecisionExceeded)?;
                exact_decimal(mantissa, 0)
            }
        }
        Ok(Self {
            input_price_per_1k: price(input)?,
            output_price_per_1k: price(output)?,
        })
    }

    pub fn input_price_per_1k(self) -> Decimal {
        self.input_price_per_1k
    }
    pub fn output_price_per_1k(self) -> Decimal {
        self.output_price_per_1k
    }

    /// Preserve ModelPrice/UsageRecorder's signed-token arithmetic. Only the
    /// catalog helper below clamps negative token counts, just like Python.
    pub fn calculate_cost(
        self,
        input_tokens: i64,
        output_tokens: i64,
    ) -> Result<TokenCost, BillingError> {
        let input_cost_usd = token_cost(self.input_price_per_1k, input_tokens)?;
        let output_cost_usd = token_cost(self.output_price_per_1k, output_tokens)?;
        Ok(TokenCost {
            input_cost_usd,
            output_cost_usd,
            total_cost_usd: exact_add(input_cost_usd, output_cost_usd)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenCost {
    pub input_cost_usd: Decimal,
    pub output_cost_usd: Decimal,
    pub total_cost_usd: Decimal,
}

impl TokenCost {
    /// Exact existing ModelPrice response keys, with JSON floats only here.
    pub fn model_price_response(self, pricing_status: &str) -> Result<Value, BillingError> {
        Ok(json!({
            "input_cost_usd": decimal_float(self.input_cost_usd)?,
            "output_cost_usd": decimal_float(self.output_cost_usd)?,
            "total_cost_usd": decimal_float(self.total_cost_usd)?,
            "input_cost_cents": integer_units(self.input_cost_usd, 2, false)?,
            "output_cost_cents": integer_units(self.output_cost_usd, 2, false)?,
            "total_cost_cents": integer_units(self.total_cost_usd, 2, false)?,
            "pricing_status": pricing_status,
        }))
    }

    /// Legacy usage-record columns round components separately before summing.
    pub fn usage_storage_cost(self) -> Result<UsageStorageCost, BillingError> {
        Ok(UsageStorageCost {
            input_cost_cents: usd_to_microcents(self.input_cost_usd)?,
            output_cost_cents: usd_to_microcents(self.output_cost_usd)?,
        })
    }
}

/// Field spelling mirrors legacy DB rows; both fields contain microcents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageStorageCost {
    pub input_cost_cents: i64,
    pub output_cost_cents: i64,
}

impl UsageStorageCost {
    pub fn total_microcents(self) -> Result<i64, BillingError> {
        self.input_cost_cents
            .checked_add(self.output_cost_cents)
            .ok_or(BillingError::IntegerOverflow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingStatus {
    Catalog,
    ProviderModel,
    Unknown,
    TenantModel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedPricing {
    pub catalog_model: &'static str,
    pub provider: &'static str,
    pub rates: TokenRates,
    pub status: PricingStatus,
}

// Mirrored from the canonical Python catalog, not updated from live prices.
// The source-parity test below detects catalog and alias drift.
const CATALOG: &[(&str, &str, &str, &str)] = &[
    ("gpt-4o", "0.0025", "0.01", "openai"),
    ("gpt-4o-mini", "0.00015", "0.0006", "openai"),
    ("o1", "0.015", "0.06", "openai"),
    ("o1-mini", "0.0011", "0.0044", "openai"),
    ("gpt-4-turbo", "0.01", "0.03", "openai"),
    ("gpt-4", "0.03", "0.06", "openai"),
    ("gpt-3.5-turbo", "0.0005", "0.0015", "openai"),
    ("claude-sonnet-4-20250514", "0.003", "0.015", "anthropic"),
    ("claude-3-5-sonnet-20241022", "0.003", "0.015", "anthropic"),
    ("claude-3-5-haiku-20241022", "0.0008", "0.004", "anthropic"),
    ("claude-3-opus", "0.015", "0.075", "anthropic"),
    ("claude-3-sonnet", "0.003", "0.015", "anthropic"),
    ("claude-3-5-sonnet", "0.003", "0.015", "anthropic"),
    ("claude-3-haiku", "0.00025", "0.00125", "anthropic"),
    ("deepseek-chat", "0.00028", "0.00042", "deepseek"),
    ("deepseek-reasoner", "0.00028", "0.00042", "deepseek"),
    ("deepseek-coder", "0.00028", "0.00042", "deepseek"),
    ("qwen-turbo", "0.0003", "0.0006", "dashscope"),
    ("qwen-plus", "0.0004", "0.0012", "dashscope"),
    ("qwen-max", "0.0012", "0.006", "dashscope"),
    ("qwen-vl-max", "0.00023", "0.000574", "dashscope"),
    ("qwen-vl-plus", "0.008", "0.008", "dashscope"),
    ("qwen3.7-plus", "0.0005", "0.003", "dashscope"),
    ("qwen3.6-plus", "0", "0", "dashscope"),
    ("gemini-3-pro-preview", "0.002", "0.012", "google"),
    ("gemini-3-flash-preview", "0.0005", "0.003", "google"),
    ("gemini-2.5-pro", "0.00125", "0.01", "google"),
    ("gemini-2.5-flash", "0.0003", "0.0025", "google"),
    ("gemini-2.5-flash-lite", "0.0001", "0.0004", "google"),
    ("gemini-2.0-flash", "0.00015", "0.0006", "google"),
    ("default", "0.001", "0.002", "unknown"),
];

const ALIASES: &[(&str, &str)] = &[
    ("gemini-3.0-pro", "gemini-3-pro-preview"),
    ("gemini-3.0-flash", "gemini-3-flash-preview"),
    ("claude-3-opus-20240229", "claude-3-opus"),
    ("claude-3-sonnet-20240229", "claude-3-sonnet"),
    ("claude-3-haiku-20240307", "claude-3-haiku"),
];

pub fn normalize_model_id(model: &str) -> String {
    const PREFIXES: &[&str] = &[
        "models/",
        "model/",
        "model:",
        "openai/",
        "anthropic/",
        "google/",
        "vertex/",
        "deepseek/",
        "dashscope/",
        "aliyun/",
    ];
    let mut normalized = model.trim();
    loop {
        let lower = normalized.to_ascii_lowercase();
        let Some(prefix) = PREFIXES.iter().find(|prefix| lower.starts_with(**prefix)) else {
            return normalized.to_owned();
        };
        normalized = normalized[prefix.len()..].trim();
    }
}

pub fn resolve_pricing(model: &str) -> ResolvedPricing {
    let normalized = normalize_model_id(model).to_ascii_lowercase();
    let canonical = ALIASES
        .iter()
        .find(|(alias, _)| *alias == normalized)
        .map_or(normalized.as_str(), |(_, canonical)| *canonical);
    let exact = CATALOG.iter().find(|(model, _, _, _)| *model == canonical);
    let inferred = CATALOG
        .iter()
        .filter(|(model, _, _, _)| *model != "default" && canonical.starts_with(*model))
        .max_by_key(|(model, _, _, _)| model.len());
    let (entry, status) = if let Some(entry) = exact {
        (entry, PricingStatus::Catalog)
    } else if let Some(entry) = inferred {
        (entry, PricingStatus::ProviderModel)
    } else {
        (
            CATALOG.last().expect("catalog has a fallback"),
            PricingStatus::Unknown,
        )
    };
    ResolvedPricing {
        catalog_model: entry.0,
        provider: entry.3,
        rates: TokenRates::parse(entry.1, entry.2)
            .expect("catalog prices are exact valid decimals"),
        status,
    }
}

/// Ordered immutable defaults for the legacy model-pricing cache. Keep catalog
/// insertion order: its first-prefix lookup intentionally differs from resolve_pricing.
pub fn model_pricing_defaults() -> impl Iterator<Item = (&'static str, ResolvedPricing)> {
    CATALOG.iter().map(|entry| (entry.0, resolve_pricing(entry.0)))
}

/// Catalog contract: clamp each count, sum exact USD, round total cents half-even.
pub fn calculate_token_cost_cents(
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
) -> Result<(i64, PricingStatus), BillingError> {
    let price = resolve_pricing(model);
    let cost = price
        .rates
        .calculate_cost(input_tokens.max(0), output_tokens.max(0))?;
    Ok((
        integer_units(cost.total_cost_usd, 2, true)?.max(0),
        price.status,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    Ok,
    Warning,
    Exceeded,
    Blocked,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverageStrategy {
    HardBlock,
    RateLimit,
    DowngradeModel,
    #[default]
    AllowButAlert,
}

impl OverageStrategy {
    pub fn coerce(value: Option<&str>) -> Self {
        match value.unwrap_or_default().trim() {
            "hard_block" => Self::HardBlock,
            "rate_limit" => Self::RateLimit,
            "downgrade_model" => Self::DowngradeModel,
            _ => Self::AllowButAlert,
        }
    }
}

/// The setter/RPM normalization contract. Do not apply this to loaded quota
/// records implicitly: the Python read path preserves their raw integer limits.
pub fn normalize_limit(value: Option<i64>) -> Option<i64> {
    value.filter(|value| *value > 0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserQuota {
    pub tenant_id: String,
    pub user_id: String,
    pub daily_token_limit: Option<i64>,
    pub monthly_token_limit: Option<i64>,
    pub monthly_cost_limit_cents: Option<i64>,
    pub requests_per_minute: Option<i64>,
    pub requests_per_day: Option<i64>,
    pub current_daily_tokens: i64,
    pub current_monthly_tokens: i64,
    pub current_monthly_cost_cents: i64,
    pub current_daily_requests: i64,
    pub is_blocked: bool,
    pub blocked_reason: Option<String>,
    pub warning_threshold: i64,
    pub overage_strategy: OverageStrategy,
    pub downgraded_model: Option<String>,
    pub temporary_extra_tokens: i64,
    pub temporary_extra_cost_cents: i64,
    pub temporary_expires_at: Option<DateTime<Utc>>,
}

impl UserQuota {
    pub fn new(tenant_id: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
            daily_token_limit: None,
            monthly_token_limit: None,
            monthly_cost_limit_cents: None,
            requests_per_minute: None,
            requests_per_day: None,
            current_daily_tokens: 0,
            current_monthly_tokens: 0,
            current_monthly_cost_cents: 0,
            current_daily_requests: 0,
            is_blocked: false,
            blocked_reason: None,
            warning_threshold: 80,
            overage_strategy: OverageStrategy::AllowButAlert,
            downgraded_model: None,
            temporary_extra_tokens: 0,
            temporary_extra_cost_cents: 0,
            temporary_expires_at: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MinuteQuotaCheck {
    pub allowed: bool,
    pub used: i64,
    pub retry_after_seconds: i64,
}

impl Default for MinuteQuotaCheck {
    fn default() -> Self {
        Self {
            allowed: true,
            used: 0,
            retry_after_seconds: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaCheckResult {
    pub status: QuotaStatus,
    pub message: String,
    pub daily_tokens_used: i64,
    pub daily_tokens_limit: Option<i64>,
    pub monthly_cost_used: i64,
    pub monthly_cost_limit: Option<i64>,
    pub daily_requests_used: i64,
    pub daily_requests_limit: Option<i64>,
    pub requests_per_minute_used: i64,
    pub requests_per_minute_limit: Option<i64>,
    pub retry_after_seconds: i64,
    pub warning_threshold: i64,
    pub overage_strategy: OverageStrategy,
    pub downgraded_model: Option<String>,
}

impl Default for QuotaCheckResult {
    fn default() -> Self {
        Self {
            status: QuotaStatus::Ok,
            message: String::new(),
            daily_tokens_used: 0,
            daily_tokens_limit: None,
            monthly_cost_used: 0,
            monthly_cost_limit: None,
            daily_requests_used: 0,
            daily_requests_limit: None,
            requests_per_minute_used: 0,
            requests_per_minute_limit: None,
            retry_after_seconds: 0,
            warning_threshold: 80,
            overage_strategy: OverageStrategy::AllowButAlert,
            downgraded_model: None,
        }
    }
}

fn percentage(used: i64, limit: Option<i64>) -> f64 {
    limit
        .filter(|value| *value != 0)
        .map_or(0.0, |limit| used as f64 / limit as f64 * 100.0)
}

fn rounded_display(value: f64, places: usize) -> f64 {
    // Formatting rounds the actual IEEE value to decimal places, unlike the
    // tempting (x * 10).round()/10 which changes Python ties such as 80.05.
    format!("{value:.places$}")
        .parse()
        .expect("finite decimal display")
}

impl QuotaCheckResult {
    pub fn can_proceed(&self) -> bool {
        matches!(self.status, QuotaStatus::Ok | QuotaStatus::Warning)
            || (self.status == QuotaStatus::Exceeded
                && matches!(
                    self.overage_strategy,
                    OverageStrategy::AllowButAlert | OverageStrategy::DowngradeModel
                ))
    }

    pub fn to_dict(&self) -> Value {
        json!({
            "status": self.status,
            "message": self.message,
            "can_proceed": self.can_proceed(),
            "daily_tokens": {
                "used": self.daily_tokens_used, "limit": self.daily_tokens_limit,
                "percentage": rounded_display(percentage(self.daily_tokens_used, self.daily_tokens_limit), 1),
            },
            "monthly_cost": {
                "used_cents": self.monthly_cost_used,
                "used_usd": rounded_display(self.monthly_cost_used as f64 / 100.0, 2),
                "limit_cents": self.monthly_cost_limit,
                "limit_usd": self.monthly_cost_limit.filter(|value| *value != 0)
                    .map(|value| rounded_display(value as f64 / 100.0, 2)),
                "percentage": rounded_display(percentage(self.monthly_cost_used, self.monthly_cost_limit), 1),
            },
            "daily_requests": { "used": self.daily_requests_used, "limit": self.daily_requests_limit },
            "minute_requests": {
                "used": self.requests_per_minute_used, "limit": self.requests_per_minute_limit,
                "retry_after_seconds": self.retry_after_seconds,
            },
            "policy": { "overage_strategy": self.overage_strategy, "downgraded_model": self.downgraded_model },
        })
    }

    fn exceeded(mut self, message: &str) -> Self {
        self.status = if self.overage_strategy == OverageStrategy::HardBlock {
            QuotaStatus::Blocked
        } else {
            QuotaStatus::Exceeded
        };
        self.message = match self.overage_strategy {
            OverageStrategy::HardBlock => message.to_owned(),
            OverageStrategy::RateLimit => format!("{message}; strategy=rate_limit"),
            OverageStrategy::AllowButAlert => format!("{message}; strategy=allow_but_alert"),
            OverageStrategy::DowngradeModel => {
                let suffix = self
                    .downgraded_model
                    .as_deref()
                    .filter(|model| !model.is_empty())
                    .map_or_else(String::new, |model| format!("; downgrade={model}"));
                format!("{message}; strategy=downgrade_model{suffix}")
            }
        };
        self
    }
}

fn boosted_limit(limit: Option<i64>, extra: i64) -> Result<Option<i64>, BillingError> {
    limit
        .map(|limit| {
            limit
                .checked_add(extra)
                .ok_or(BillingError::IntegerOverflow)
        })
        .transpose()
}

/// The loaded-record effective limits at one instant. Null stays unlimited;
/// stored zero is preserved and can become finite under an active boost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EffectiveQuotaLimits {
    pub daily_tokens: Option<i64>,
    pub monthly_tokens: Option<i64>,
    pub monthly_cost_cents: Option<i64>,
    pub temporary_boost_active: bool,
}

/// Pure shared arithmetic for admission and read-only overview observations.
/// This does not normalize stored limits, reset usage, or reserve an RPM slot.
pub(crate) fn effective_quota_limits(
    daily_token_limit: Option<i64>,
    monthly_token_limit: Option<i64>,
    monthly_cost_limit_cents: Option<i64>,
    temporary_extra_tokens: i64,
    temporary_extra_cost_cents: i64,
    temporary_expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<EffectiveQuotaLimits, BillingError> {
    let temporary_boost_active = temporary_expires_at.is_some_and(|expires| expires > now);
    let extra_tokens = if temporary_boost_active {
        temporary_extra_tokens
    } else {
        0
    };
    let extra_cost = if temporary_boost_active {
        temporary_extra_cost_cents
    } else {
        0
    };
    Ok(EffectiveQuotaLimits {
        daily_tokens: boosted_limit(daily_token_limit, extra_tokens)?,
        monthly_tokens: boosted_limit(monthly_token_limit, extra_tokens)?,
        monthly_cost_cents: boosted_limit(monthly_cost_limit_cents, extra_cost)?,
        temporary_boost_active,
    })
}

/// Check loaded quota data in Python's exact precedence order. The callback is
/// invoked only after static limits pass, and only for a positive RPM limit.
/// The owning service must check/reserve a slot for this tenant/user atomically;
/// this module neither owns distributed limiting nor changes that policy.
pub fn check_quota(
    quota: Option<&UserQuota>,
    estimated_tokens: i64,
    now: DateTime<Utc>,
    check_minute: impl FnOnce(i64) -> MinuteQuotaCheck,
) -> Result<QuotaCheckResult, BillingError> {
    let Some(quota) = quota else {
        return Ok(QuotaCheckResult {
            message: "No quota restrictions".into(),
            ..Default::default()
        });
    };
    if quota.is_blocked {
        return Ok(QuotaCheckResult {
            status: QuotaStatus::Blocked,
            message: format!(
                "User is blocked: {}",
                quota
                    .blocked_reason
                    .as_deref()
                    .filter(|reason| !reason.is_empty())
                    .unwrap_or("Unknown reason")
            ),
            requests_per_minute_limit: quota.requests_per_minute,
            overage_strategy: quota.overage_strategy,
            downgraded_model: quota.downgraded_model.clone(),
            ..Default::default()
        });
    }
    let limits = effective_quota_limits(
        quota.daily_token_limit,
        quota.monthly_token_limit,
        quota.monthly_cost_limit_cents,
        quota.temporary_extra_tokens,
        quota.temporary_extra_cost_cents,
        quota.temporary_expires_at,
        now,
    )?;
    let daily_limit = limits.daily_tokens;
    let monthly_limit = limits.monthly_tokens;
    let cost_limit = limits.monthly_cost_cents;
    let mut result = QuotaCheckResult {
        daily_tokens_used: quota.current_daily_tokens,
        daily_tokens_limit: daily_limit,
        monthly_cost_used: quota.current_monthly_cost_cents,
        monthly_cost_limit: cost_limit,
        daily_requests_used: quota.current_daily_requests,
        daily_requests_limit: quota.requests_per_day,
        requests_per_minute_limit: quota.requests_per_minute,
        warning_threshold: quota.warning_threshold,
        overage_strategy: quota.overage_strategy,
        downgraded_model: quota.downgraded_model.clone(),
        ..Default::default()
    };
    // Use i128 to compare sums exactly even at the i64 boundary; no wraparound.
    if daily_limit.is_some_and(|limit| {
        limit != 0
            && i128::from(quota.current_daily_tokens) + i128::from(estimated_tokens)
                > i128::from(limit)
    }) {
        return Ok(result.exceeded("Daily token limit exceeded"));
    }
    if monthly_limit.is_some_and(|limit| {
        limit != 0
            && i128::from(quota.current_monthly_tokens) + i128::from(estimated_tokens)
                > i128::from(limit)
    }) {
        return Ok(result.exceeded("Monthly token limit exceeded"));
    }
    if cost_limit.is_some_and(|limit| limit != 0 && quota.current_monthly_cost_cents >= limit) {
        return Ok(result.exceeded("Monthly cost limit exceeded"));
    }
    if quota
        .requests_per_day
        .is_some_and(|limit| limit != 0 && quota.current_daily_requests >= limit)
    {
        return Ok(result.exceeded("Daily request limit exceeded"));
    }
    let minute = normalize_limit(quota.requests_per_minute)
        .map(check_minute)
        .unwrap_or_default();
    result.requests_per_minute_used = minute.used;
    if !minute.allowed {
        result.retry_after_seconds = minute.retry_after_seconds;
        return Ok(result.exceeded("Minute request limit exceeded"));
    }
    let mut warnings = Vec::new();
    if daily_limit.is_some_and(|limit| limit != 0) {
        let pct = percentage(quota.current_daily_tokens, daily_limit);
        if pct >= quota.warning_threshold as f64 {
            warnings.push(format!("Daily token usage at {pct:.1}%"));
        }
    }
    if cost_limit.is_some_and(|limit| limit != 0) {
        let pct = percentage(quota.current_monthly_cost_cents, cost_limit);
        if pct >= quota.warning_threshold as f64 {
            warnings.push(format!("Monthly cost at {pct:.1}%"));
        }
    }
    if warnings.is_empty() {
        result.message = "Quota check passed".into();
    } else {
        result.status = QuotaStatus::Warning;
        result.message = warnings.join("; ");
    }
    Ok(result)
}

