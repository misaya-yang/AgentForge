//! Source-faithful orchestration of `src/services/llm/model_service.py`.
//!
//! No pool, pricing singleton, routes, background work, or network is constructed.
//! The host supplies a database, synchronous pricing-cache invalidation, and clock.
//! SQL is copied from the source (whitespace is immaterial). Database adapters must
//! bind Decimal as NUMERIC, Jsonb as the compact JSON text expected by asyncpg, and
//! Timestamp as UTC; return NUMERIC values as decimal strings to preserve receipts.
//! Cancellation drops the awaited database future and never starts later work.
//! Whether an in-flight mutation committed is the adapter's responsibility; this
//! layer does not promise rollback or undo a write when cache invalidation fails.
//!
//! Limits: rows use JSON-compatible values (timestamps remain adapter values),
//! integers are i64, decimals use the existing 28-digit Rust decimal envelope.
//! Capability validation uses the existing native validator, whose typed error
//! deliberately omits Python's detailed validation message. Invalid persisted
//! profiles fall back to safe capabilities; the injected observer can log this.
//! Compact JSONB is semantically identical; object key order is not guaranteed.

use crate::{
    capabilities,
    pricing_receipt::{self, PricingReceiptError},
};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use std::{fmt, future::Future, pin::Pin};

pub type ModelRow = Map<String, Value>;
pub type ModelFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ModelServiceError>> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelServiceError {
    /// Redacted PostgreSQL unique violation, retained for source HTTP 409 mapping.
    UniqueConflict,
    ProviderAmbiguous,
    CapabilityRevisionConflict,
    Capability,
    Pricing(PricingReceiptError),
    Database,
    PricingCache,
    InvalidRow,
}
impl fmt::Display for ModelServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProviderAmbiguous => f.write_str("model_provider_required"),
            Self::CapabilityRevisionConflict => f.write_str("model capability revision conflict"),
            Self::Capability => f.write_str("invalid_model_capability_profile"),
            Self::Pricing(e) => e.fmt(f),
            Self::Database => f.write_str("model_database_error"),
            Self::UniqueConflict => f.write_str("duplicate key"),
            Self::PricingCache => f.write_str("model_pricing_cache_error"),
            Self::InvalidRow => f.write_str("invalid_model_row"),
        }
    }
}
impl std::error::Error for ModelServiceError {}
impl From<PricingReceiptError> for ModelServiceError {
    fn from(e: PricingReceiptError) -> Self {
        Self::Pricing(e)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ModelParam {
    Text(String),
    Integer(i64),
    Bool(bool),
    Decimal(Decimal),
    Jsonb(String),
    Timestamp(DateTime<Utc>),
}
impl From<&str> for ModelParam {
    fn from(v: &str) -> Self {
        Self::Text(v.to_owned())
    }
}
impl From<String> for ModelParam {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}
impl From<i64> for ModelParam {
    fn from(v: i64) -> Self {
        Self::Integer(v)
    }
}
impl From<bool> for ModelParam {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}
impl From<Decimal> for ModelParam {
    fn from(v: Decimal) -> Self {
        Self::Decimal(v)
    }
}

/// Owned statements keep adapters independent of SQLx and avoid interpolated data.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelQuery {
    pub sql: String,
    pub params: Vec<ModelParam>,
}
pub trait ModelDatabase: Send + Sync {
    fn fetch(&self, query: ModelQuery) -> ModelFuture<'_, Vec<ModelRow>>;
    fn fetchrow(&self, query: ModelQuery) -> ModelFuture<'_, Option<ModelRow>>;
    fn execute(&self, query: ModelQuery) -> ModelFuture<'_, String>;
}
pub trait ModelPricingCache: Send + Sync {
    /// Source `get_pricing_service().invalidate_cache()`, not a catalog write.
    fn invalidate_cache(&self) -> Result<(), ModelServiceError>;
}
pub trait ModelClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}
pub struct UtcModelClock;
impl ModelClock for UtcModelClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
pub trait ModelObserver: Send + Sync {
    fn invalid_persisted_capabilities(&self, _provider: &str, _model: &str) {}
}
pub struct QuietModelObserver;
impl ModelObserver for QuietModelObserver {}

/// Defaults match both create_model and upsert_model_from_catalog.
#[derive(Debug, Clone)]
pub struct ModelFields {
    pub display_name: String,
    pub context_window: i64,
    pub max_output_tokens: i64,
    pub supports_vision: bool,
    pub supports_tools: bool,
    pub input_price_per_1k: Decimal,
    pub output_price_per_1k: Decimal,
    pub access_level: String,
    pub sort_order: i64,
    /// Upsert None preserves an existing catalog; Some(empty) explicitly clears it.
    pub catalog_capabilities: Option<ModelRow>,
}
impl Default for ModelFields {
    fn default() -> Self {
        Self {
            display_name: String::new(),
            context_window: 128000,
            max_output_tokens: 4096,
            supports_vision: false,
            supports_tools: true,
            input_price_per_1k: Decimal::ZERO,
            output_price_per_1k: Decimal::ZERO,
            access_level: "public".into(),
            sort_order: 0,
            catalog_capabilities: None,
        }
    }
}
#[derive(Debug, Clone)]
pub struct CreateModel {
    pub fields: ModelFields,
    pub is_enabled: bool,
    pub capability_overrides: ModelRow,
}
impl Default for CreateModel {
    fn default() -> Self {
        Self {
            fields: ModelFields::default(),
            is_enabled: true,
            capability_overrides: Map::new(),
        }
    }
}
#[derive(Debug, Clone, Default)]
pub struct UpdateModel {
    pub new_model_id: Option<String>,
    pub display_name: Option<String>,
    pub context_window: Option<i64>,
    pub max_output_tokens: Option<i64>,
    pub supports_vision: Option<bool>,
    pub supports_tools: Option<bool>,
    pub input_price_per_1k: Option<Decimal>,
    pub output_price_per_1k: Option<Decimal>,
    pub access_level: Option<String>,
    pub is_enabled: Option<bool>,
    pub sort_order: Option<i64>,
    pub catalog_capabilities: Option<ModelRow>,
    pub capability_overrides: Option<ModelRow>,
    pub expected_capability_revision: Option<i64>,
}
const COLUMNS: &str = "model_id, tenant_id, provider_id, display_name, context_window, max_output_tokens, supports_vision, supports_tools, catalog_capabilities, capability_overrides, capability_revision, input_price_per_1k, output_price_per_1k, access_level, is_enabled, sort_order, created_at, updated_at";
fn query(sql: String, params: Vec<ModelParam>) -> ModelQuery {
    ModelQuery { sql, params }
}
fn jsonb(object: &ModelRow) -> ModelParam {
    ModelParam::Jsonb(serde_json::to_string(object).expect("JSON object serializes"))
}
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}
fn boolean(row: &ModelRow, name: &str, default: bool) -> bool {
    row.get(name).map(truthy).unwrap_or(default)
}
fn string(row: &ModelRow, name: &str) -> String {
    row.get(name)
        .filter(|v| truthy(v))
        .and_then(Value::as_str)
        .unwrap_or("")
        .into()
}
fn required_identity(row: &ModelRow, name: &str) -> Result<String, ModelServiceError> {
    row.get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(ModelServiceError::InvalidRow)
}
fn revision(row: &ModelRow, name: &str) -> Result<i64, ModelServiceError> {
    let Some(v) = row.get(name).filter(|v| truthy(v)) else {
        return Ok(1);
    };
    v.as_i64()
        .or_else(|| v.as_str().and_then(|v| v.parse().ok()))
        .ok_or(ModelServiceError::InvalidRow)
}
fn profile(row: &ModelRow, name: &str) -> ModelRow {
    row.get(name)
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}
fn deep_merge(base: &mut Value, patch: &ModelRow) {
    for (key, value) in patch {
        if let (Some(old), Some(new)) = (
            base.get_mut(key).filter(|v| v.is_object()),
            value.as_object(),
        ) {
            deep_merge(old, new);
        } else {
            base.as_object_mut()
                .expect("profile object")
                .insert(key.clone(), value.clone());
        }
    }
}
fn strict_profile(
    catalog: &ModelRow,
    overrides: &ModelRow,
    tools: bool,
    vision: bool,
) -> Result<Value, ModelServiceError> {
    let mut value = capabilities::safe(tools, vision);
    deep_merge(&mut value, catalog);
    deep_merge(&mut value, overrides);
    // Python's schema-version equality accepts True and 1.0. Keep that
    // boundary behavior without changing the stricter shared native validator.
    let original_schema = value.get("schema_version").cloned();
    let compatible_schema = original_schema
        .as_ref()
        .is_some_and(|v| v == &Value::Bool(true) || v.as_f64() == Some(1.0));
    if compatible_schema {
        value["schema_version"] = json!(1);
    }
    let mut result = capabilities::validate(value).map_err(|_| ModelServiceError::Capability)?;
    if compatible_schema {
        result["schema_version"] = original_schema.unwrap();
    }
    Ok(result)
}
// Python dictionary/list equality compares bools, integers and floats by
// numeric value. This affects whether a capability revision changes.
fn profile_equal(left: &ModelRow, right: &ModelRow) -> bool {
    left.len() == right.len()
        && left.iter().all(|(key, value)| {
            right
                .get(key)
                .is_some_and(|other| python_equal(value, other))
        })
}
fn python_equal(left: &Value, right: &Value) -> bool {
    enum Numeric {
        Integer(i128),
        Float(f64),
    }
    fn numeric(v: &Value) -> Option<Numeric> {
        match v {
            Value::Bool(v) => Some(Numeric::Integer(i128::from(*v))),
            Value::Number(v) => v
                .as_i64()
                .map(|v| Numeric::Integer(i128::from(v)))
                .or_else(|| v.as_u64().map(|v| Numeric::Integer(i128::from(v))))
                .or_else(|| v.as_f64().map(Numeric::Float)),
            _ => None,
        }
    }
    match (left, right) {
        (Value::Object(a), Value::Object(b)) => profile_equal(a, b),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| python_equal(a, b))
        }
        _ => match (numeric(left), numeric(right)) {
            (Some(Numeric::Integer(a)), Some(Numeric::Integer(b))) => a == b,
            (Some(Numeric::Float(a)), Some(Numeric::Float(b))) => a == b,
            (Some(Numeric::Integer(a)), Some(Numeric::Float(b)))
            | (Some(Numeric::Float(b)), Some(Numeric::Integer(a))) => {
                b.is_finite() && b.fract() == 0.0 && b as i128 == a
            }
            _ => left == right,
        },
    }
}
fn price(row: &ModelRow, name: &str) -> Result<String, ModelServiceError> {
    match row.get(name).filter(|v| truthy(v)) {
        None => Ok("0".into()),
        Some(Value::String(v)) => {
            // The adapter encodes NUMERIC as text; Python Decimal zero is falsy.
            let normalized = pricing_receipt::normalize_price(v)?;
            Ok(if normalized == "0" || normalized == "-0" {
                "0".into()
            } else {
                v.clone()
            })
        }
        Some(Value::Number(v)) => Ok(v.to_string()),
        _ => Err(ModelServiceError::InvalidRow),
    }
}

pub struct ModelService<D, P, C = UtcModelClock, O = QuietModelObserver> {
    database: D,
    pricing: P,
    clock: C,
    observer: O,
}
impl<D: ModelDatabase, P: ModelPricingCache> ModelService<D, P> {
    pub fn new(database: D, pricing: P) -> Self {
        Self {
            database,
            pricing,
            clock: UtcModelClock,
            observer: QuietModelObserver,
        }
    }
}
impl<D: ModelDatabase, P: ModelPricingCache, C: ModelClock, O: ModelObserver>
    ModelService<D, P, C, O>
{
    pub fn with_collaborators(database: D, pricing: P, clock: C, observer: O) -> Self {
        Self {
            database,
            pricing,
            clock,
            observer,
        }
    }

    pub fn row_to_dict(&self, row: Option<ModelRow>) -> Result<ModelRow, ModelServiceError> {
        let Some(mut row) = row.filter(|v| !v.is_empty()) else {
            return Ok(Map::new());
        };
        row.remove("provider_matches");
        let input = price(&row, "input_price_per_1k")?;
        let output = price(&row, "output_price_per_1k")?;
        let snapshot = pricing_receipt::build_pricing_snapshot(
            &string(&row, "tenant_id"),
            &string(&row, "provider_id"),
            &string(&row, "model_id"),
            &input,
            &output,
            pricing_receipt::DEFAULT_PRICING_STATUS,
        )?;
        row.insert("pricing_snapshot".into(), snapshot.to_value());
        for name in ["catalog_capabilities", "capability_overrides"] {
            let value = match row.get(name) {
                Some(Value::Object(v)) => v.clone(),
                Some(Value::String(v)) => serde_json::from_str::<Value>(v)
                    .ok()
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default(),
                _ => Map::new(),
            };
            row.insert(name.into(), Value::Object(value));
        }
        for (name, value) in [
            ("input_price_per_1k", input),
            ("output_price_per_1k", output),
        ] {
            if row.get(name).is_some_and(|v| !v.is_null()) {
                let number: f64 = row
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or(&value)
                    .parse()
                    .map_err(|_| ModelServiceError::InvalidRow)?;
                let number =
                    serde_json::Number::from_f64(number).ok_or(ModelServiceError::InvalidRow)?;
                row.insert(name.into(), Value::Number(number));
            }
        }
        if profile(&row, "catalog_capabilities").is_empty() {
            row.insert(
                "catalog_capabilities".into(),
                capabilities::builtin(&string(&row, "provider_id"), &string(&row, "model_id"))
                    .unwrap_or_else(|| json!({})),
            );
        }
        let tools = boolean(&row, "supports_tools", true);
        let vision = boolean(&row, "supports_vision", false);
        let effective = strict_profile(
            &profile(&row, "catalog_capabilities"),
            &profile(&row, "capability_overrides"),
            tools,
            vision,
        )
        .unwrap_or_else(|_| {
            self.observer.invalid_persisted_capabilities(
                &string(&row, "provider_id"),
                &string(&row, "model_id"),
            );
            capabilities::safe(tools, vision)
        });
        row.insert("effective_capabilities".into(), effective);
        Ok(row)
    }
    pub async fn list_models(
        &self,
        tenant: &str,
        provider: Option<&str>,
        include_disabled: bool,
        access: Option<&str>,
    ) -> Result<Vec<ModelRow>, ModelServiceError> {
        let mut conditions = vec!["tenant_id = $1".to_owned()];
        let mut params = vec![tenant.into()];
        if let Some(provider) = provider.filter(|v| !v.is_empty()) {
            conditions.push("provider_id = $2".into());
            params.push(provider.into());
        }
        if !include_disabled {
            conditions.push("is_enabled = true".into());
        }
        match access {
            Some("public") => conditions.push("access_level = 'public'".into()),
            Some("premium") => conditions.push("access_level IN ('public', 'premium')".into()),
            _ => (),
        }
        self.database.fetch(query(format!("SELECT {COLUMNS} FROM llm_models WHERE {} ORDER BY provider_id, sort_order, display_name",conditions.join(" AND ")),params)).await?.into_iter().map(|r| self.row_to_dict(Some(r))).collect()
    }
    pub async fn get_model(
        &self,
        tenant: &str,
        model: &str,
        provider: Option<&str>,
    ) -> Result<Option<ModelRow>, ModelServiceError> {
        let q = if let Some(provider) = provider {
            query(
                format!(
                    "SELECT {COLUMNS} FROM llm_models WHERE tenant_id = $1 AND provider_id = $2 AND model_id = $3"
                ),
                vec![tenant.into(), provider.into(), model.into()],
            )
        } else {
            query(
                format!(
                    "SELECT COUNT(*) OVER () AS provider_matches, {COLUMNS} FROM llm_models WHERE tenant_id = $1 AND model_id = $2 ORDER BY sort_order, provider_id LIMIT 1"
                ),
                vec![tenant.into(), model.into()],
            )
        };
        let row = self.database.fetchrow(q).await?;
        if provider.is_none()
            && row
                .as_ref()
                .map(|r| revision(r, "provider_matches"))
                .transpose()?
                .is_some_and(|n| n > 1)
        {
            return Err(ModelServiceError::ProviderAmbiguous);
        }
        row.filter(|v| !v.is_empty())
            .map(|r| self.row_to_dict(Some(r)))
            .transpose()
    }
    pub async fn get_provider_model(
        &self,
        tenant: &str,
        provider: &str,
        model: &str,
    ) -> Result<Option<ModelRow>, ModelServiceError> {
        self.get_model(tenant, model, Some(provider)).await
    }

    pub async fn create_model(
        &self,
        tenant: &str,
        model: &str,
        provider: &str,
        data: CreateModel,
    ) -> Result<ModelRow, ModelServiceError> {
        let mut f = data.fields;
        let catalog_capabilities = f.catalog_capabilities.take().unwrap_or_default();
        pricing_receipt::build_pricing_snapshot(
            tenant,
            provider,
            model,
            &f.input_price_per_1k.to_string(),
            &f.output_price_per_1k.to_string(),
            pricing_receipt::DEFAULT_PRICING_STATUS,
        )?;
        let validation_catalog = if catalog_capabilities.is_empty() {
            capabilities::builtin(provider, model)
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default()
        } else {
            catalog_capabilities.clone()
        };
        strict_profile(
            &validation_catalog,
            &data.capability_overrides,
            f.supports_tools,
            f.supports_vision,
        )?;
        let sql = format!(
            "INSERT INTO llm_models ( model_id, tenant_id, provider_id, display_name, context_window, max_output_tokens, supports_vision, supports_tools, catalog_capabilities, capability_overrides, capability_revision, input_price_per_1k, output_price_per_1k, access_level, is_enabled, sort_order ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 1, $11, $12, $13, $14, $15) RETURNING {COLUMNS}"
        );
        let params = vec![
            model.into(),
            tenant.into(),
            provider.into(),
            f.display_name.into(),
            f.context_window.into(),
            f.max_output_tokens.into(),
            f.supports_vision.into(),
            f.supports_tools.into(),
            jsonb(&catalog_capabilities),
            jsonb(&data.capability_overrides),
            f.input_price_per_1k.into(),
            f.output_price_per_1k.into(),
            f.access_level.into(),
            data.is_enabled.into(),
            f.sort_order.into(),
        ];
        let row = self.database.fetchrow(query(sql, params)).await?;
        let result = self.row_to_dict(row)?;
        self.sync_single_model_pricing(tenant, model).await?;
        Ok(result)
    }

    pub async fn update_model(
        &self,
        tenant: &str,
        model: &str,
        provider: Option<&str>,
        data: UpdateModel,
    ) -> Result<Option<ModelRow>, ModelServiceError> {
        for price in [data.input_price_per_1k, data.output_price_per_1k]
            .into_iter()
            .flatten()
        {
            if price < Decimal::ZERO {
                return Err(PricingReceiptError::InvalidModelPrice.into());
            }
        }
        let current_model = if provider.is_none() {
            self.get_model(tenant, model, None).await?
        } else {
            None
        };
        if provider.is_none() && current_model.is_none() {
            return Ok(None);
        }
        let resolved = current_model
            .as_ref()
            .map(|r| required_identity(r, "provider_id"))
            .transpose()?;
        let provider = provider
            .or(resolved.as_deref())
            .ok_or(ModelServiceError::InvalidRow)?;
        let mut updates = Vec::new();
        let mut params = Vec::new();
        fn add(
            updates: &mut Vec<String>,
            params: &mut Vec<ModelParam>,
            name: &str,
            v: impl Into<ModelParam>,
        ) {
            params.push(v.into());
            updates.push(format!("{name} = ${}", params.len()));
        }
        if let Some(v) = data.new_model_id.as_ref().filter(|v| v.as_str() != model) {
            add(&mut updates, &mut params, "model_id", v.clone());
        }
        if let Some(v) = data.display_name.as_ref() {
            add(&mut updates, &mut params, "display_name", v.clone());
        }
        if let Some(v) = data.context_window {
            add(&mut updates, &mut params, "context_window", v);
        }
        if let Some(v) = data.max_output_tokens {
            add(&mut updates, &mut params, "max_output_tokens", v);
        }
        if let Some(v) = data.supports_vision {
            add(&mut updates, &mut params, "supports_vision", v);
        }
        if let Some(v) = data.supports_tools {
            add(&mut updates, &mut params, "supports_tools", v);
        }
        if let Some(v) = data.input_price_per_1k {
            add(&mut updates, &mut params, "input_price_per_1k", v);
        }
        if let Some(v) = data.output_price_per_1k {
            add(&mut updates, &mut params, "output_price_per_1k", v);
        }
        if let Some(v) = data.access_level.as_ref() {
            add(&mut updates, &mut params, "access_level", v.clone());
        }
        if let Some(v) = data.is_enabled {
            add(&mut updates, &mut params, "is_enabled", v);
        }
        if let Some(v) = data.sort_order {
            add(&mut updates, &mut params, "sort_order", v);
        }
        let touched = data.catalog_capabilities.is_some()
            || data.capability_overrides.is_some()
            || data.supports_tools.is_some()
            || data.supports_vision.is_some();
        let guarded = touched && data.expected_capability_revision.is_some();
        if touched {
            let current = match current_model {
                Some(r) => Some(r),
                None => self.get_model(tenant, model, Some(provider)).await?,
            };
            let Some(current) = current else {
                return Ok(None);
            };
            let old_catalog = profile(&current, "catalog_capabilities");
            let old_overrides = profile(&current, "capability_overrides");
            let catalog = data.catalog_capabilities.as_ref().unwrap_or(&old_catalog);
            let overrides = data.capability_overrides.as_ref().unwrap_or(&old_overrides);
            let tools = data
                .supports_tools
                .unwrap_or_else(|| boolean(&current, "supports_tools", true));
            let vision = data
                .supports_vision
                .unwrap_or_else(|| boolean(&current, "supports_vision", false));
            strict_profile(catalog, overrides, tools, vision)?;
            if let Some(expected) = data.expected_capability_revision
                && revision(&current, "capability_revision")? != expected
            {
                return Err(ModelServiceError::CapabilityRevisionConflict);
            }
            if data.catalog_capabilities.is_some() {
                add(
                    &mut updates,
                    &mut params,
                    "catalog_capabilities",
                    jsonb(catalog),
                );
            }
            if data.capability_overrides.is_some() {
                add(
                    &mut updates,
                    &mut params,
                    "capability_overrides",
                    jsonb(overrides),
                );
            }
            let changed = data.catalog_capabilities.is_some()
                && !profile_equal(catalog, &old_catalog)
                || data.capability_overrides.is_some() && !profile_equal(overrides, &old_overrides)
                || tools != boolean(&current, "supports_tools", true)
                || vision != boolean(&current, "supports_vision", false);
            if changed {
                updates.push("capability_revision = capability_revision + 1".into());
            }
        }
        if updates.is_empty() {
            return self.get_model(tenant, model, Some(provider)).await;
        }
        add(
            &mut updates,
            &mut params,
            "updated_at",
            ModelParam::Timestamp(self.clock.now()),
        );
        let index = params.len() + 1;
        params.extend([tenant.into(), provider.into(), model.into()]);
        let mut where_clause = format!(
            "WHERE tenant_id = ${index} AND provider_id = ${} AND model_id = ${}",
            index + 1,
            index + 2
        );
        if guarded {
            params.push(data.expected_capability_revision.unwrap().into());
            where_clause.push_str(&format!(" AND capability_revision = ${}", params.len()));
        }
        let row = self
            .database
            .fetchrow(query(
                format!(
                    "UPDATE llm_models SET {} {where_clause} RETURNING {COLUMNS}",
                    updates.join(", ")
                ),
                params,
            ))
            .await?;
        if let Some(row) = row.filter(|v| !v.is_empty()) {
            let model = string(&row, "model_id");
            let result = self.row_to_dict(Some(row))?;
            self.sync_single_model_pricing(tenant, &model).await?;
            return Ok(Some(result));
        }
        if guarded
            && self
                .get_model(tenant, model, Some(provider))
                .await?
                .is_some()
        {
            return Err(ModelServiceError::CapabilityRevisionConflict);
        }
        Ok(None)
    }
    pub async fn reset_model_capabilities(
        &self,
        tenant: &str,
        model: &str,
        provider: Option<&str>,
        expected: Option<i64>,
    ) -> Result<Option<ModelRow>, ModelServiceError> {
        self.update_model(
            tenant,
            model,
            provider,
            UpdateModel {
                capability_overrides: Some(Map::new()),
                expected_capability_revision: expected,
                ..Default::default()
            },
        )
        .await
    }
    pub async fn toggle_model(
        &self,
        tenant: &str,
        model: &str,
        enabled: bool,
        provider: Option<&str>,
    ) -> Result<Option<ModelRow>, ModelServiceError> {
        self.update_model(
            tenant,
            model,
            provider,
            UpdateModel {
                is_enabled: Some(enabled),
                ..Default::default()
            },
        )
        .await
    }
    pub async fn delete_model(
        &self,
        tenant: &str,
        model: &str,
        provider: Option<&str>,
    ) -> Result<bool, ModelServiceError> {
        let current = if provider.is_none() {
            self.get_model(tenant, model, None).await?
        } else {
            None
        };
        if provider.is_none() && current.is_none() {
            return Ok(false);
        }
        let resolved = current
            .as_ref()
            .map(|r| required_identity(r, "provider_id"))
            .transpose()?;
        let provider = provider
            .or(resolved.as_deref())
            .ok_or(ModelServiceError::InvalidRow)?;
        let result = self.database.execute(query("DELETE FROM llm_models WHERE tenant_id = $1 AND provider_id = $2 AND model_id = $3".into(),vec![tenant.into(),provider.into(),model.into()])).await?;
        Ok(result.contains("DELETE") && !result.contains("DELETE 0"))
    }
    pub async fn get_models_for_assistant(
        &self,
        tenant: &str,
        access: &str,
    ) -> Result<Vec<ModelRow>, ModelServiceError> {
        let filter = match access {
            "admin" => "",
            "premium" => "AND m.access_level IN ('public', 'premium')",
            _ => "AND m.access_level = 'public'",
        };
        let columns = COLUMNS
            .split(", ")
            .map(|v| format!("m.{v}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {columns} FROM llm_models m INNER JOIN llm_providers p ON m.tenant_id = p.tenant_id AND m.provider_id = p.provider_id WHERE m.tenant_id = $1 AND m.is_enabled = true AND p.is_enabled = true {filter} ORDER BY p.display_name, m.sort_order, m.display_name"
        );
        self.database
            .fetch(query(sql, vec![tenant.into()]))
            .await?
            .into_iter()
            .map(|r| self.row_to_dict(Some(r)))
            .collect()
    }
    pub async fn sync_single_model_pricing(
        &self,
        _tenant: &str,
        _model: &str,
    ) -> Result<bool, ModelServiceError> {
        self.pricing.invalidate_cache()?;
        Ok(true)
    }
    pub async fn sync_pricing_from_llm_models(
        &self,
        tenant: &str,
        include_disabled: bool,
    ) -> Result<usize, ModelServiceError> {
        let models = self
            .list_models(tenant, None, include_disabled, None)
            .await?;
        let mut synced = 0;
        for row in models {
            if self
                .sync_single_model_pricing(tenant, &string(&row, "model_id"))
                .await?
            {
                synced += 1;
            }
        }
        Ok(synced)
    }
    /// Catalog policy values initialize new rows only. Existing tenant access,
    /// prices, enabled state and capability overrides remain administrator-owned.
    pub async fn upsert_model_from_catalog(
        &self,
        tenant: &str,
        provider: &str,
        model: &str,
        fields: ModelFields,
        enable_new: bool,
    ) -> Result<(&'static str, ModelRow, bool), ModelServiceError> {
        if let Some(existing) = self.get_provider_model(tenant, provider, model).await? {
            let catalog = if !profile_equal(
                &profile(&existing, "catalog_capabilities"),
                &fields.catalog_capabilities.clone().unwrap_or_default(),
            ) {
                fields.catalog_capabilities
            } else {
                None
            };
            let updated = self
                .update_model(
                    tenant,
                    model,
                    Some(provider),
                    UpdateModel {
                        display_name: Some(fields.display_name),
                        context_window: Some(fields.context_window),
                        max_output_tokens: Some(fields.max_output_tokens),
                        supports_vision: Some(fields.supports_vision),
                        supports_tools: Some(fields.supports_tools),
                        // Tenant access and prices are administrator-owned after creation.
                        // Omit them, rather than rewriting a stale pre-sync snapshot.
                        sort_order: Some(fields.sort_order),
                        catalog_capabilities: catalog,
                        ..Default::default()
                    },
                )
                .await?;
            let result = updated.unwrap_or_else(|| existing.clone());
            let changed = revision(&result, "capability_revision")?
                != revision(&existing, "capability_revision")?;
            Ok(("updated", result, changed))
        } else {
            let created = self
                .create_model(
                    tenant,
                    model,
                    provider,
                    CreateModel {
                        fields,
                        is_enabled: enable_new,
                        capability_overrides: Map::new(),
                    },
                )
                .await?;
            Ok(("created", created, true))
        }
    }
}

#[cfg(test)]
mod catalog_policy_tests;
