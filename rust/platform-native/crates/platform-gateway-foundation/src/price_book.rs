//! Opt-in price book v2. Exact identities, half-open time ranges, append-only
//! content-addressed terms. No prefix/catalog/provider inference or zero fallback.
//! The USD token bridge retains billing::TokenRates arithmetic and rounding.
use crate::{billing::TokenRates, pricing_receipt::build_pricing_snapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt};

pub const SCHEMA: &str = "agentforge.price_book/v2";
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PriceBookError {
    InvalidTerms,
    MissingPrice,
    AmbiguousPrice,
    IdentityMismatch,
    InvalidReference,
    UnsupportedLegacyBridge,
    PrecisionExceeded,
    Arithmetic,
    Storage,
    Decode,
    NotActivated,
    RevisionConflict,
    Forbidden,
    UnknownModel,
    CommitUncertain,
}
impl fmt::Display for PriceBookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "price_book_v2_{self:?}")
    }
}
impl std::error::Error for PriceBookError {}
type Result<T> = std::result::Result<T, PriceBookError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Component {
    Input,
    CachedInput,
    CacheWrite,
    Output,
    Reasoning,
    ToolCall,
    Request,
    Image,
    Batch,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Tokens,
    Calls,
    Images,
    Requests,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceKey {
    /// Immutable price publication; host selects this explicitly at activation.
    pub book_version: String,
    pub provider: String,
    pub model: String,
    /// Explicit immutable model revision, never inferred from a model alias.
    pub model_version: String,
    pub component: Component,
    /// Three uppercase ASCII letters; no implicit currency conversion.
    pub currency: String,
}
impl PriceKey {
    fn validate(&self) -> Result<()> {
        if [
            &self.book_version,
            &self.provider,
            &self.model,
            &self.model_version,
        ]
        .iter()
        .any(|v| !identity(v))
            || self.currency.len() != 3
            || !self.currency.bytes().all(|b| b.is_ascii_uppercase())
        {
            return Err(PriceBookError::InvalidTerms);
        }
        Ok(())
    }
}
fn identity(s: &str) -> bool {
    !s.is_empty() && s == s.trim() && !s.chars().any(char::is_control)
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectiveRange {
    /// Unix milliseconds, inclusive.
    pub from_ms: i64,
    /// Unix milliseconds, exclusive. None means no end.
    pub to_ms: Option<i64>,
}
impl EffectiveRange {
    fn validate(&self) -> Result<()> {
        if self.to_ms.is_some_and(|to| to <= self.from_ms) {
            Err(PriceBookError::InvalidTerms)
        } else {
            Ok(())
        }
    }
    fn contains(&self, at: i64) -> bool {
        at >= self.from_ms && self.to_ms.is_none_or(|to| at < to)
    }
    fn overlaps(&self, other: &Self) -> bool {
        self.to_ms.is_none_or(|to| other.from_ms < to)
            && other.to_ms.is_none_or(|to| self.from_ms < to)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceEntry {
    pub key: PriceKey,
    /// None is provider base price; Some is an exact tenant override.
    pub tenant: Option<String>,
    pub effective: EffectiveRange,
    pub unit: Unit,
    pub units_per_rate: u64,
    /// Integer billionths of currency per units_per_rate. Finer precision fails.
    pub rate_nanos: u64,
}
impl PriceEntry {
    fn validate(&self) -> Result<()> {
        self.key.validate()?;
        self.effective.validate()?;
        if self.units_per_rate == 0
            || self.units_per_rate > i64::MAX as u64
            || self.rate_nanos > i64::MAX as u64
            || self.tenant.as_deref().is_some_and(|s| !identity(s))
        {
            return Err(PriceBookError::InvalidTerms);
        }
        Ok(())
    }
    pub fn reference(&self) -> Result<String> {
        self.validate()?;
        digest("entry", self)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantDiscount {
    pub tenant: String,
    pub key: PriceKey,
    pub effective: EffectiveRange,
    /// Applied after an override. 0 = no discount; 10000 = explicitly free.
    pub discount_bps: u16,
}
impl TenantDiscount {
    fn validate(&self) -> Result<()> {
        self.key.validate()?;
        self.effective.validate()?;
        if !identity(&self.tenant) || self.discount_bps > 10000 {
            return Err(PriceBookError::InvalidTerms);
        }
        Ok(())
    }
    pub fn reference(&self) -> Result<String> {
        self.validate()?;
        digest("discount", self)
    }
}
fn digest(kind: &str, value: &impl Serialize) -> Result<String> {
    let bytes = serde_json::to_vec(value).map_err(|_| PriceBookError::Decode)?;
    let mut hash = Sha256::new();
    hash.update(SCHEMA);
    hash.update([0]);
    hash.update(kind);
    hash.update([0]);
    hash.update(bytes);
    Ok(format!("{SCHEMA}/{kind}/{:x}", hash.finalize()))
}

/// No mutable update API. Reconstruct a book when an authorized append is published.
#[derive(Clone, Debug)]
pub struct PriceBook {
    entries: Vec<PriceEntry>,
    discounts: Vec<TenantDiscount>,
}
impl PriceBook {
    pub fn new(entries: Vec<PriceEntry>, discounts: Vec<TenantDiscount>) -> Result<Self> {
        let mut references = BTreeSet::new();
        for (i, entry) in entries.iter().enumerate() {
            entry.validate()?;
            if !references.insert(entry.reference()?) {
                return Err(PriceBookError::AmbiguousPrice);
            }
            if entries[..i].iter().any(|old| {
                old.key == entry.key
                    && old.tenant == entry.tenant
                    && old.effective.overlaps(&entry.effective)
            }) {
                return Err(PriceBookError::AmbiguousPrice);
            }
        }
        for (i, discount) in discounts.iter().enumerate() {
            discount.validate()?;
            if discounts[..i].iter().any(|old| {
                old.key == discount.key
                    && old.tenant == discount.tenant
                    && old.effective.overlaps(&discount.effective)
            }) {
                return Err(PriceBookError::AmbiguousPrice);
            }
        }
        Ok(Self { entries, discounts })
    }
    pub fn resolve(&self, tenant: &str, key: &PriceKey, at_ms: i64) -> Result<PriceReceipt> {
        key.validate()?;
        if !identity(tenant) {
            return Err(PriceBookError::InvalidTerms);
        }
        let eligible = |e: &&PriceEntry| &e.key == key && e.effective.contains(at_ms);
        let entry = self
            .entries
            .iter()
            .filter(eligible)
            .find(|e| e.tenant.as_deref() == Some(tenant))
            .or_else(|| {
                self.entries
                    .iter()
                    .filter(eligible)
                    .find(|e| e.tenant.is_none())
            })
            .ok_or(PriceBookError::MissingPrice)?;
        let discount = self
            .discounts
            .iter()
            .find(|d| d.tenant == tenant && &d.key == key && d.effective.contains(at_ms))
            .cloned();
        let mut receipt = PriceReceipt {
            schema: SCHEMA.into(),
            tenant: tenant.into(),
            selected_at_ms: at_ms,
            entry_reference: entry.reference()?,
            entry: entry.clone(),
            discount_reference: discount
                .as_ref()
                .map(TenantDiscount::reference)
                .transpose()?,
            discount,
            reference: String::new(),
        };
        receipt.reference = receipt.expected_reference()?;
        Ok(receipt)
    }
}

/// Frozen terms accompany immutable references, so replay does not consult today's book.
/// A content hash is integrity, not authorization: only an authorized resolver may issue it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceReceipt {
    pub schema: String,
    pub tenant: String,
    pub selected_at_ms: i64,
    pub entry_reference: String,
    pub entry: PriceEntry,
    pub discount_reference: Option<String>,
    pub discount: Option<TenantDiscount>,
    pub reference: String,
}
impl PriceReceipt {
    fn expected_reference(&self) -> Result<String> {
        let mut terms = self.clone();
        terms.reference.clear();
        digest("receipt", &terms)
    }
    pub fn validate(&self, tenant: &str, key: &PriceKey) -> Result<()> {
        self.entry.validate()?;
        if self.schema != SCHEMA
            || !identity(tenant)
            || self.tenant != tenant
            || &self.entry.key != key
            || self.entry.tenant.as_deref().is_some_and(|t| t != tenant)
            || !self.entry.effective.contains(self.selected_at_ms)
        {
            return Err(PriceBookError::IdentityMismatch);
        }
        if self.entry_reference != self.entry.reference()?
            || self.reference != self.expected_reference()?
        {
            return Err(PriceBookError::InvalidReference);
        }
        match (&self.discount, &self.discount_reference) {
            (None, None) => (),
            (Some(d), Some(reference)) => {
                d.validate()?;
                if d.tenant != tenant
                    || &d.key != key
                    || !d.effective.contains(self.selected_at_ms)
                    || reference != &d.reference()?
                {
                    return Err(PriceBookError::InvalidReference);
                }
            }
            _ => return Err(PriceBookError::InvalidReference),
        }
        Ok(())
    }
    /// Exact adjusted rate in nanocurrency. No lossy rounding of discounts.
    pub fn adjusted_rate_nanos(&self) -> Result<u64> {
        self.validate(&self.tenant, &self.entry.key)?;
        let bps = u128::from(10000 - self.discount.as_ref().map_or(0, |d| d.discount_bps));
        let numerator = u128::from(self.entry.rate_nanos)
            .checked_mul(bps)
            .ok_or(PriceBookError::Arithmetic)?;
        if numerator % 10000 != 0 {
            return Err(PriceBookError::PrecisionExceeded);
        }
        u64::try_from(numerator / 10000).map_err(|_| PriceBookError::Arithmetic)
    }
    /// Generic component quote, rounded half-even to nanocurrency explicitly in v2.
    /// Token settlement into the legacy ledger must use token_snapshot instead.
    pub fn quote_nanos(&self, units: u64) -> Result<u64> {
        let numerator = u128::from(self.adjusted_rate_nanos()?)
            .checked_mul(u128::from(units))
            .ok_or(PriceBookError::Arithmetic)?;
        let denominator = u128::from(self.entry.units_per_rate);
        let q = numerator / denominator;
        let r = numerator % denominator;
        let rounded = q + u128::from(r * 2 > denominator || (r * 2 == denominator && q % 2 == 1));
        u64::try_from(rounded).map_err(|_| PriceBookError::Arithmetic)
    }
}

/// Explicit v2 envelope around the unchanged v1 USD/token snapshot. Existing
/// snapshots retain their exact interpretation; non-token/currency components
/// cannot accidentally enter legacy USD columns.
pub fn token_snapshot(input: &PriceReceipt, output: &PriceReceipt) -> Result<Value> {
    input.validate(&input.tenant, &input.entry.key)?;
    output.validate(&input.tenant, &output.entry.key)?;
    let a = &input.entry;
    let b = &output.entry;
    if a.key.book_version != b.key.book_version
        || a.key.provider != b.key.provider
        || a.key.model != b.key.model
        || a.key.model_version != b.key.model_version
        || a.key.component != Component::Input
        || b.key.component != Component::Output
        || a.key.currency != "USD"
        || b.key.currency != "USD"
        || a.unit != Unit::Tokens
        || b.unit != Unit::Tokens
        || a.units_per_rate != 1000
        || b.units_per_rate != 1000
        || input.selected_at_ms != output.selected_at_ms
    {
        return Err(PriceBookError::UnsupportedLegacyBridge);
    }
    let rate = |r: &PriceReceipt| -> Result<String> {
        let nanos = r.adjusted_rate_nanos()?;
        Ok(format!(
            "{}.{:09}",
            nanos / 1_000_000_000,
            nanos % 1_000_000_000
        ))
    };
    let input_rate = rate(input)?;
    let output_rate = rate(output)?;
    TokenRates::parse(&input_rate, &output_rate).map_err(|_| PriceBookError::Arithmetic)?;
    let legacy = build_pricing_snapshot(
        &input.tenant,
        &a.key.provider,
        &a.key.model,
        &input_rate,
        &output_rate,
        "price_book_v2",
    )
    .map_err(|_| PriceBookError::Arithmetic)?;
    let mut value = legacy.to_value();
    value["price_book_schema"] = Value::String(SCHEMA.into());
    value["price_book_input"] = serde_json::to_value(input).map_err(|_| PriceBookError::Decode)?;
    value["price_book_output"] =
        serde_json::to_value(output).map_err(|_| PriceBookError::Decode)?;
    Ok(value)
}
/// Verify every referenced term and prove the legacy rates derive from those terms.
/// Caller separately validates the v1 hash and record tenant/provider/model binding.
pub fn validate_token_snapshot(value: &Value) -> Result<()> {
    if value.get("price_book_schema").and_then(Value::as_str) != Some(SCHEMA) {
        return Err(PriceBookError::InvalidReference);
    }
    let input = serde_json::from_value::<PriceReceipt>(
        value
            .get("price_book_input")
            .cloned()
            .ok_or(PriceBookError::Decode)?,
    )
    .map_err(|_| PriceBookError::Decode)?;
    let output = serde_json::from_value::<PriceReceipt>(
        value
            .get("price_book_output")
            .cloned()
            .ok_or(PriceBookError::Decode)?,
    )
    .map_err(|_| PriceBookError::Decode)?;
    let mut expected = token_snapshot(&input, &output)?;
    for (field, component) in [
        ("price_book_cached_input", Component::CachedInput),
        ("price_book_cache_write", Component::CacheWrite),
    ] {
        if let Some(raw) = value.get(field) {
            let receipt: PriceReceipt =
                serde_json::from_value(raw.clone()).map_err(|_| PriceBookError::Decode)?;
            validate_cache_term(&input, &receipt, component)?;
            expected[field] = raw.clone();
        }
    }
    if &expected != value {
        return Err(PriceBookError::InvalidReference);
    }
    Ok(())
}

/// Opt-in resolver; construction, not metadata supplied by a user, activates v2.
/// Model revision is fixed by admission/configuration, never inferred from aliases.
pub struct PriceBookTokenResolver {
    pub book_version: String,
    pub book: PriceBook,
    pub model_version: String,
}
impl crate::usage_recorder::UsageSnapshotResolver for PriceBookTokenResolver {
    fn resolve<'a>(
        &'a self,
        record: &'a crate::usage_recorder::UsageRecord,
    ) -> crate::usage_recorder::UsageFuture<'a, Value> {
        Box::pin(async move {
            let select = |component| {
                self.book.resolve(
                    &record.tenant_id,
                    &PriceKey {
                        book_version: self.book_version.clone(),
                        provider: record.provider.clone(),
                        model: record.model.clone(),
                        model_version: self.model_version.clone(),
                        component,
                        currency: "USD".into(),
                    },
                    record.timestamp.timestamp_millis(),
                )
            };
            let result = select(Component::Input).and_then(|input| {
                let mut snapshot = token_snapshot(&input, &select(Component::Output)?)?;
                for (field, component) in [
                    ("price_book_cached_input", Component::CachedInput),
                    ("price_book_cache_write", Component::CacheWrite),
                ] {
                    match select(component) {
                        Ok(term) => {
                            validate_cache_term(&input, &term, component)?;
                            snapshot[field] =
                                serde_json::to_value(term).map_err(|_| PriceBookError::Decode)?;
                        }
                        Err(PriceBookError::MissingPrice) => {}
                        Err(error) => return Err(error),
                    }
                }
                Ok(snapshot)
            });
            result.map_err(crate::usage_recorder::UsageRecorderError::from)
        })
    }
}

/// Load an immutable, tenant-scoped view with one statement. SQL/storage failures
/// remain typed errors. The database proposal must be admitted before using this.
pub async fn load_postgres(pool: &sqlx::PgPool, tenant: &str) -> Result<PriceBook> {
    use sqlx::Row;
    if !identity(tenant) {
        return Err(PriceBookError::InvalidTerms);
    }
    let rows = sqlx::query(
        "SELECT reference,kind,payload FROM gateway.price_book_v2_terms WHERE tenant_id IS NULL OR tenant_id=$1",
    )
    .bind(tenant)
    .fetch_all(pool)
    .await
    .map_err(|_| PriceBookError::Storage)?;
    let mut entries = Vec::new();
    let mut discounts = Vec::new();
    for row in rows {
        let payload: Value = row.try_get("payload").map_err(|_| PriceBookError::Decode)?;
        let kind: String = row.try_get("kind").map_err(|_| PriceBookError::Decode)?;
        let reference: String = row
            .try_get("reference")
            .map_err(|_| PriceBookError::Decode)?;
        let expected = match kind.as_str() {
            "entry" => serde_json::from_value::<PriceEntry>(payload.clone())
                .map_err(|_| PriceBookError::Decode)?
                .reference()?,
            "discount" => serde_json::from_value::<TenantDiscount>(payload.clone())
                .map_err(|_| PriceBookError::Decode)?
                .reference()?,
            _ => return Err(PriceBookError::Decode),
        };
        if reference != expected {
            return Err(PriceBookError::InvalidReference);
        }
        match kind.as_str() {
            "entry" => {
                entries.push(serde_json::from_value(payload).map_err(|_| PriceBookError::Decode)?)
            }
            "discount" => {
                discounts.push(serde_json::from_value(payload).map_err(|_| PriceBookError::Decode)?)
            }
            _ => return Err(PriceBookError::Decode),
        }
    }
    PriceBook::new(entries, discounts)
}

/// Append reviewed terms atomically. Authentication/authorization and schema
/// admission belong to the Gateway host; this function grants neither.
/// Overlap and duplicate conflicts propagate; existing terms are never replaced.
pub async fn append_postgres(
    pool: &sqlx::PgPool,
    entries: Vec<PriceEntry>,
    discounts: Vec<TenantDiscount>,
) -> Result<()> {
    let book = PriceBook::new(entries, discounts)?;
    let mut tx = pool.begin().await.map_err(|_| PriceBookError::Storage)?;
    for entry in &book.entries {
        let payload = serde_json::to_value(entry).map_err(|_| PriceBookError::Decode)?;
        sqlx::query(
            "INSERT INTO gateway.price_book_v2_terms(reference,kind,payload) VALUES ($1,'entry',$2)",
        )
        .bind(entry.reference()?)
        .bind(payload)
        .execute(&mut *tx)
        .await
        .map_err(|_| PriceBookError::Storage)?;
    }
    for discount in &book.discounts {
        let payload = serde_json::to_value(discount).map_err(|_| PriceBookError::Decode)?;
        sqlx::query(
            "INSERT INTO gateway.price_book_v2_terms(reference,kind,payload) VALUES ($1,'discount',$2)",
        )
        .bind(discount.reference()?)
        .bind(payload)
        .execute(&mut *tx)
        .await
        .map_err(|_| PriceBookError::Storage)?;
    }
    tx.commit()
        .await
        .map_err(|_| PriceBookError::CommitUncertain)?;
    Ok(())
}

/// Migration input is an exported historical row, never a live-table rewrite.
#[derive(Clone, Debug)]
pub struct LegacyPriceRow {
    pub source_id: String,
    pub model: String,
    pub provider: Option<String>,
    pub input_per_1k: String,
    pub output_per_1k: String,
    pub effective: EffectiveRange,
    pub active: bool,
}
/// Operator-reviewed attribution. No prefix inference or model-name matching.
#[derive(Clone, Debug)]
pub struct LegacyAttribution {
    pub book_version: String,
    pub source_id: String,
    pub provider: String,
    pub model_version: String,
}
#[derive(Debug, Serialize)]
pub struct QuarantinedPrice {
    pub source_id: String,
    pub reason: &'static str,
}
#[derive(Debug, Serialize)]
pub struct AttributedPrice {
    pub source_id: String,
    pub provider: String,
    pub model_version: String,
    pub book_version: String,
    pub input_reference: String,
    pub output_reference: String,
}
#[derive(Debug, Default, Serialize)]
pub struct LegacyImport {
    pub attributed: Vec<AttributedPrice>,
    pub entries: Vec<PriceEntry>,
    pub quarantine: Vec<QuarantinedPrice>,
    pub inactive: Vec<String>,
}
/// Exact decimal to nanocurrency with no floats, exponent coercion or rounding.
fn parse_nanos(s: &str) -> Result<u64> {
    let (whole, fraction) = s.split_once('.').unwrap_or((s, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 9
    {
        return Err(PriceBookError::PrecisionExceeded);
    }
    let whole: u64 = whole.parse().map_err(|_| PriceBookError::Arithmetic)?;
    let frac: u64 = if fraction.is_empty() {
        0
    } else {
        fraction.parse().map_err(|_| PriceBookError::Arithmetic)?
    };
    whole
        .checked_mul(1_000_000_000)
        .and_then(|v| v.checked_add(frac * 10_u64.pow(9 - fraction.len() as u32)))
        .ok_or(PriceBookError::Arithmetic)
}
pub fn plan_legacy_import(
    rows: &[LegacyPriceRow],
    attributions: &[LegacyAttribution],
) -> LegacyImport {
    let mut report = LegacyImport::default();
    let mut candidates: Vec<(String, Vec<PriceEntry>)> = Vec::new();
    for row in rows {
        if !row.active {
            report.inactive.push(row.source_id.clone());
            continue;
        }
        let mappings: Vec<_> = attributions
            .iter()
            .filter(|a| a.source_id == row.source_id)
            .collect();
        let reason = if mappings.len() != 1 {
            Some("provider_attribution_missing_or_ambiguous")
        } else if row
            .provider
            .as_deref()
            .is_some_and(|p| !p.is_empty() && p != mappings[0].provider)
        {
            Some("provider_attribution_conflict")
        } else if rows.iter().filter(|r| r.source_id == row.source_id).count() != 1 {
            Some("duplicate_source_identity")
        } else {
            None
        };
        if let Some(reason) = reason {
            report.quarantine.push(QuarantinedPrice {
                source_id: row.source_id.clone(),
                reason,
            });
            continue;
        }
        let attribution = mappings[0];
        let make = |component, rate: &str| -> Result<PriceEntry> {
            let entry = PriceEntry {
                key: PriceKey {
                    book_version: attribution.book_version.clone(),
                    provider: attribution.provider.clone(),
                    model: row.model.clone(),
                    model_version: attribution.model_version.clone(),
                    component,
                    currency: "USD".into(),
                },
                tenant: None,
                effective: row.effective.clone(),
                unit: Unit::Tokens,
                units_per_rate: 1000,
                rate_nanos: parse_nanos(rate)?,
            };
            entry.validate()?;
            Ok(entry)
        };
        match make(Component::Input, &row.input_per_1k).and_then(|input| {
            make(Component::Output, &row.output_per_1k).map(|output| vec![input, output])
        }) {
            Ok(entries) => candidates.push((row.source_id.clone(), entries)),
            Err(_) => report.quarantine.push(QuarantinedPrice {
                source_id: row.source_id.clone(),
                reason: "invalid_or_unrepresentable_terms",
            }),
        }
    }
    for (i, (source_id, entries)) in candidates.iter().enumerate() {
        let conflict = candidates.iter().enumerate().any(|(j, (_, others))| {
            i != j
                && entries.iter().any(|e| {
                    others
                        .iter()
                        .any(|other| e.key == other.key && e.effective.overlaps(&other.effective))
                })
        });
        if conflict {
            report.quarantine.push(QuarantinedPrice {
                source_id: source_id.clone(),
                reason: "overlapping_attributed_terms",
            });
        } else {
            // Entries were validated above, so these references cannot fail.
            match (entries[0].reference(), entries[1].reference()) {
                (Ok(input_reference), Ok(output_reference)) => {
                    report.attributed.push(AttributedPrice {
                        source_id: source_id.clone(),
                        provider: entries[0].key.provider.clone(),
                        model_version: entries[0].key.model_version.clone(),
                        book_version: entries[0].key.book_version.clone(),
                        input_reference,
                        output_reference,
                    });
                    report.entries.extend(entries.iter().cloned());
                }
                _ => report.quarantine.push(QuarantinedPrice {
                    source_id: source_id.clone(),
                    reason: "invalid_reference",
                }),
            }
        }
    }
    report
}

/// Publish the reviewed migration and its audit atomically. All IDs are bound
/// as text then cast by PostgreSQL; no UUID SQLx feature/dependency is needed.
/// This does not activate v2 or update legacy source rows.
pub async fn persist_legacy_import(
    pool: &sqlx::PgPool,
    import_id: &str,
    report: &LegacyImport,
) -> Result<()> {
    PriceBook::new(report.entries.clone(), vec![])?;
    let mut tx = pool.begin().await.map_err(|_| PriceBookError::Storage)?;
    for entry in &report.entries {
        sqlx::query(
            "INSERT INTO gateway.price_book_v2_terms(reference,kind,payload) VALUES ($1,'entry',$2)",
        )
        .bind(entry.reference()?)
        .bind(serde_json::to_value(entry).map_err(|_| PriceBookError::Decode)?)
        .execute(&mut *tx)
        .await
        .map_err(|_| PriceBookError::Storage)?;
    }
    for row in &report.attributed {
        sqlx::query("INSERT INTO gateway.price_book_v2_import_audit(import_id,source_id,disposition,provider_id,model_version,book_version,input_reference,output_reference) VALUES ($1::text::uuid,$2::text::uuid,'attributed',$3,$4,$5,$6,$7)")
            .bind(import_id).bind(&row.source_id).bind(&row.provider).bind(&row.model_version).bind(&row.book_version).bind(&row.input_reference).bind(&row.output_reference)
            .execute(&mut *tx).await.map_err(|_| PriceBookError::Storage)?;
    }
    for row in &report.quarantine {
        sqlx::query("INSERT INTO gateway.price_book_v2_import_audit(import_id,source_id,disposition,reason) VALUES ($1::text::uuid,$2::text::uuid,'quarantined',$3)")
            .bind(import_id).bind(&row.source_id).bind(row.reason)
            .execute(&mut *tx).await.map_err(|_| PriceBookError::Storage)?;
    }
    for source_id in &report.inactive {
        sqlx::query("INSERT INTO gateway.price_book_v2_import_audit(import_id,source_id,disposition) VALUES ($1::text::uuid,$2::text::uuid,'inactive')")
            .bind(import_id).bind(source_id).execute(&mut *tx).await.map_err(|_| PriceBookError::Storage)?;
    }
    tx.commit()
        .await
        .map_err(|_| PriceBookError::CommitUncertain)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(provider: &str, tenant: Option<&str>, component: Component, rate: u64) -> PriceEntry {
        PriceEntry {
            key: PriceKey {
                book_version: "publication-1".into(),
                provider: provider.into(),
                model: "same-model".into(),
                model_version: "revision-1".into(),
                component,
                currency: "USD".into(),
            },
            tenant: tenant.map(str::to_owned),
            effective: EffectiveRange {
                from_ms: 0,
                to_ms: Some(100),
            },
            unit: Unit::Tokens,
            units_per_rate: 1000,
            rate_nanos: rate,
        }
    }
    #[test]
    fn provider_and_tenant_collisions_are_isolated() {
        let base = entry("p1", None, Component::Input, 100);
        let other = entry("p2", None, Component::Input, 200);
        let override_a = entry("p1", Some("a"), Component::Input, 300);
        let discount = TenantDiscount {
            tenant: "a".into(),
            key: base.key.clone(),
            effective: base.effective.clone(),
            discount_bps: 1000,
        };
        let book = PriceBook::new(
            vec![base.clone(), other.clone(), override_a],
            vec![discount],
        )
        .unwrap();
        assert_eq!(
            book.resolve("a", &base.key, 1)
                .unwrap()
                .adjusted_rate_nanos(),
            Ok(270)
        );
        assert_eq!(
            book.resolve("b", &base.key, 1)
                .unwrap()
                .adjusted_rate_nanos(),
            Ok(100)
        );
        assert_eq!(
            book.resolve("a", &other.key, 1)
                .unwrap()
                .adjusted_rate_nanos(),
            Ok(200)
        );
        let mut missing = base.key.clone();
        missing.provider = "missing".into();
        assert_eq!(
            book.resolve("a", &missing, 1),
            Err(PriceBookError::MissingPrice)
        );
        missing = base.key.clone();
        missing.model_version = "revision-2".into();
        assert_eq!(
            book.resolve("a", &missing, 1),
            Err(PriceBookError::MissingPrice)
        );
        assert_eq!(
            book.resolve("a", &base.key, 100),
            Err(PriceBookError::MissingPrice)
        );
    }
    #[test]
    fn new_publication_does_not_reinterpret_old_receipts() {
        let old = entry("p", None, Component::Input, 100);
        let mut new = old.clone();
        new.key.book_version = "publication-2".into();
        new.rate_nanos = 200;
        let book = PriceBook::new(vec![old.clone(), new.clone()], vec![]).unwrap();
        let receipt = book.resolve("a", &old.key, 1).unwrap();
        assert_eq!(
            book.resolve("a", &new.key, 1)
                .unwrap()
                .adjusted_rate_nanos(),
            Ok(200)
        );
        assert_eq!(receipt.adjusted_rate_nanos(), Ok(100));
    }
    #[test]
    fn overlap_rejected_and_boundary_selects_exact_revision() {
        let first = entry("p", None, Component::Input, 100);
        assert!(matches!(
            PriceBook::new(vec![first.clone(), first.clone()], vec![]),
            Err(PriceBookError::AmbiguousPrice)
        ));
        let mut next = first.clone();
        next.effective.from_ms = 100;
        next.effective.to_ms = None;
        next.rate_nanos = 200;
        let book = PriceBook::new(vec![first.clone(), next], vec![]).unwrap();
        assert_eq!(
            book.resolve("a", &first.key, 99)
                .unwrap()
                .adjusted_rate_nanos(),
            Ok(100)
        );
        assert_eq!(
            book.resolve("a", &first.key, 100)
                .unwrap()
                .adjusted_rate_nanos(),
            Ok(200)
        );
    }
    #[test]
    fn immutable_receipt_detects_tampering_and_tenant_replay() {
        let e = entry("p", None, Component::Input, 100);
        let book = PriceBook::new(vec![e.clone()], vec![]).unwrap();
        let mut receipt = book.resolve("a", &e.key, 1).unwrap();
        assert_eq!(
            receipt.validate("b", &e.key),
            Err(PriceBookError::IdentityMismatch)
        );
        receipt.entry.rate_nanos = 0;
        assert_eq!(
            receipt.validate("a", &e.key),
            Err(PriceBookError::InvalidReference)
        );
    }
    #[test]
    fn cache_components_partition_input_and_require_frozen_prices() {
        // Rates are nanodollars per 1,000 tokens: $0.10 input, $0.20
        // output, $0.01 cached input. The legacy *_cost_cents columns use
        // 1,000,000 units/USD (microUSD), despite their historical name.
        // 20 ordinary + 80 cached inputs: .10*20/1000 + .01*80/1000
        // = $.0028 = 2,800 stored units; 10 outputs = $.002 = 2,000.
        let input = entry("p", None, Component::Input, 100_000_000);
        let output = entry("p", None, Component::Output, 200_000_000);
        let cache = entry("p", None, Component::CachedInput, 10_000_000);
        let book =
            PriceBook::new(vec![input.clone(), output.clone(), cache.clone()], vec![]).unwrap();
        let mut snapshot = token_snapshot(
            &book.resolve("a", &input.key, 1).unwrap(),
            &book.resolve("a", &output.key, 1).unwrap(),
        )
        .unwrap();
        let mut record = crate::usage_recorder::UsageRecord::new("a", "u", "m", chrono::Utc::now());
        record.model = input.key.model.clone();
        record.provider = "p".into();
        record.input_tokens = 100;
        record.output_tokens = 10;
        record.metadata.insert("provider_cache_usage".into(),serde_json::to_value(crate::usage_parser::cache::CacheUsage::observe("p",&serde_json::json!({"usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":80}}}))).unwrap());
        assert!(crate::usage_recorder::prepare_with_snapshot(&mut record, &snapshot).is_err());
        snapshot["price_book_cached_input"] =
            serde_json::to_value(book.resolve("a", &cache.key, 1).unwrap()).unwrap();
        crate::usage_recorder::prepare_with_snapshot(&mut record, &snapshot).unwrap();
        assert_eq!(record.input_cost_cents, 2800);
        assert_eq!(record.output_cost_cents, 2000);
        let mut altered = snapshot.clone();
        altered["price_book_cached_input"]["entry"]["rate_nanos"] = 0.into();
        assert!(crate::usage_recorder::prepare_with_snapshot(&mut record, &altered).is_err());
        // v1 continues to interpret all 100 input tokens at its original rate.
        let mut legacy = snapshot.clone();
        for key in [
            "price_book_schema",
            "price_book_input",
            "price_book_output",
            "price_book_cached_input",
        ] {
            legacy.as_object_mut().unwrap().remove(key);
        }
        // It still carries v2 status, so stripping the envelope fails closed.
        assert!(crate::usage_recorder::prepare_with_snapshot(&mut record, &legacy).is_err());
    }
    #[test]
    fn token_envelope_preserves_legacy_costs_and_fails_tampering() {
        let input = entry("p", None, Component::Input, 1_000_000);
        let output = entry("p", None, Component::Output, 2_000_000);
        let book = PriceBook::new(vec![input.clone(), output.clone()], vec![]).unwrap();
        let snapshot = token_snapshot(
            &book.resolve("a", &input.key, 1).unwrap(),
            &book.resolve("a", &output.key, 1).unwrap(),
        )
        .unwrap();
        validate_token_snapshot(&snapshot).unwrap();
        let mut record =
            crate::usage_recorder::UsageRecord::new("a", "u", "same-model", chrono::Utc::now());
        record.provider = "p".into();
        record.input_tokens = 1000;
        record.output_tokens = 1000;
        crate::usage_recorder::prepare_with_snapshot(&mut record, &snapshot).unwrap();
        assert_eq!(record.input_cost_cents, 1000);
        assert_eq!(record.output_cost_cents, 2000);
        assert_eq!(record.metadata["pricing_snapshot"], snapshot);
        let mut tampered = snapshot.clone();
        tampered["price_book_input"]["entry_reference"] = Value::String("fake".into());
        assert!(crate::usage_recorder::prepare_with_snapshot(&mut record, &tampered).is_err());
        record.tenant_id = "b".into();
        assert!(crate::usage_recorder::prepare_with_snapshot(&mut record, &snapshot).is_err());
    }
    #[test]
    fn currencies_and_non_token_components_cannot_enter_legacy_ledger() {
        let mut input = entry("p", None, Component::Input, 100);
        input.key.currency = "EUR".into();
        let output = entry("p", None, Component::Output, 100);
        let book = PriceBook::new(vec![input.clone(), output.clone()], vec![]).unwrap();
        assert_eq!(
            token_snapshot(
                &book.resolve("a", &input.key, 1).unwrap(),
                &book.resolve("a", &output.key, 1).unwrap()
            ),
            Err(PriceBookError::UnsupportedLegacyBridge)
        );
    }
    #[test]
    fn missing_provider_is_quarantined_not_inferred() {
        let row = LegacyPriceRow {
            source_id: "old-1".into(),
            model: "gpt-prefix-is-not-authority".into(),
            provider: None,
            input_per_1k: "0.001000".into(),
            output_per_1k: "0.002000".into(),
            effective: EffectiveRange {
                from_ms: 0,
                to_ms: None,
            },
            active: true,
        };
        let report = plan_legacy_import(&[row.clone()], &[]);
        assert!(report.entries.is_empty());
        assert_eq!(report.quarantine.len(), 1);
        let a = LegacyAttribution {
            book_version: "reviewed-import-1".into(),
            source_id: "old-1".into(),
            provider: "explicit".into(),
            model_version: "legacy-reviewed".into(),
        };
        let report = plan_legacy_import(&[row.clone()], &[a.clone()]);
        assert_eq!(report.entries.len(), 2);
        assert!(report.quarantine.is_empty());
        assert_eq!(report.entries[0].rate_nanos, 1_000_000);
        assert!(
            plan_legacy_import(&[row], &[a.clone(), a])
                .entries
                .is_empty()
        );
    }
    #[test]
    fn precision_and_overflow_are_errors_not_zero() {
        assert_eq!(
            parse_nanos("0.0000000001"),
            Err(PriceBookError::PrecisionExceeded)
        );
        let e = entry("p", None, Component::Input, 1);
        let discount = TenantDiscount {
            tenant: "a".into(),
            key: e.key.clone(),
            effective: e.effective.clone(),
            discount_bps: 1,
        };
        let book = PriceBook::new(vec![e.clone()], vec![discount]).unwrap();
        assert_eq!(
            book.resolve("a", &e.key, 1).unwrap().quote_nanos(1),
            Err(PriceBookError::PrecisionExceeded)
        );
        let mut big = e;
        big.units_per_rate = 1;
        big.rate_nanos = i64::MAX as u64;
        let book = PriceBook::new(vec![big.clone()], vec![]).unwrap();
        assert_eq!(
            book.resolve("a", &big.key, 1)
                .unwrap()
                .quote_nanos(u64::MAX),
            Err(PriceBookError::Arithmetic)
        );
    }
}

#[path = "price_book_api.rs"]
pub mod api;
#[path = "price_book_service.rs"]
pub mod service;

/// Optional cache terms are frozen with the input/output publication. Never
/// resolve missing cache prices against a newer book during settlement.
pub fn validate_cache_term(
    input: &PriceReceipt,
    cache: &PriceReceipt,
    component: Component,
) -> Result<()> {
    let mut key = input.entry.key.clone();
    key.component = component;
    cache.validate(&input.tenant, &key)?;
    if cache.selected_at_ms != input.selected_at_ms
        || cache.entry.unit != Unit::Tokens
        || cache.entry.units_per_rate != 1000
    {
        return Err(PriceBookError::UnsupportedLegacyBridge);
    }
    Ok(())
}
