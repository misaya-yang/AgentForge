//! Caller-owned PostgreSQL adapters for the frozen provider/model services.
//!
//! Constructors are inert. Pools must reach the same database/role through direct
//! or session-pinned connections; the independent control pool stays open through
//! `drain_cancelled`. No schema, startup, credentials, routes or cache is created.
//! The service SQL is executed unchanged, including its unqualified table names:
//! the host owns the connection search_path. These APIs accept trusted service SQL,
//! not arbitrary user SQL. Ordinary SELECTs use the data pool; mutations own one
//! supervised transaction per statement. Provider deletion therefore remains two
//! separately committed statements, never one atomic operation. Caller cancellation
//! requires drain before shutdown; a COMMIT already sent cannot be undone.
//!
//! Native methods preserve SQLSTATE with redacted diagnostics. Frozen service
//! traits retain only unique violation 23505 as safe UniqueConflict; other SQLSTATEs
//! are deliberately erased at that boundary. NUMERIC is decoded directly to exact decimal text (including
//! scale), without a floating-point or 28-digit decoding intermediate. Typed model
//! inputs retain their existing rust_decimal envelope. Timestamps use ISO 8601 UTC.
use crate::{
    model_service::{
        ModelDatabase, ModelFuture, ModelParam, ModelQuery, ModelRow, ModelServiceError,
    },
    postgres_operation::{PgOperationError, PgOperationStorage},
    provider_service::{Bind, ProviderDatabase, ProviderRow, ServiceError, ServiceFuture},
};
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::{
    Column, PgPool, Postgres, Row, TypeInfo, ValueRef,
    postgres::{PgArguments, PgRow, PgValueFormat},
    query::Query,
};
use std::fmt;

#[derive(Clone, PartialEq, Eq)]
pub enum ModelProviderStorageError {
    Database { sqlstate: Option<String> },
    InvalidRow,
    Cancelled,
    InvalidCancellationPool,
    IndeterminateCancellation,
}
impl ModelProviderStorageError {
    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            Self::Database { sqlstate } => sqlstate.as_deref(),
            _ => None,
        }
    }
}
impl fmt::Display for ModelProviderStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database { .. } => "model_provider_database_unavailable",
            Self::InvalidRow => "model_provider_invalid_row",
            Self::Cancelled => "model_provider_operation_cancelled",
            Self::InvalidCancellationPool => "model_provider_independent_control_pool_required",
            Self::IndeterminateCancellation => "model_provider_cancellation_indeterminate",
        })
    }
}
impl fmt::Debug for ModelProviderStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for ModelProviderStorageError {}
impl From<sqlx::Error> for ModelProviderStorageError {
    fn from(error: sqlx::Error) -> Self {
        PgOperationError::from(error).into()
    }
}
impl From<PgOperationError> for ModelProviderStorageError {
    fn from(error: PgOperationError) -> Self {
        match error {
            PgOperationError::Database { sqlstate } => Self::Database { sqlstate },
            PgOperationError::Cancelled => Self::Cancelled,
            PgOperationError::InvalidCancellationPool => Self::InvalidCancellationPool,
            PgOperationError::IndeterminateCancellation => Self::IndeterminateCancellation,
        }
    }
}
fn provider_error(error: ModelProviderStorageError) -> ServiceError {
    if error.sqlstate() == Some("23505") {
        ServiceError::UniqueConflict
    } else if error == ModelProviderStorageError::InvalidRow {
        ServiceError::InvalidRow
    } else {
        ServiceError::Database
    }
}
fn model_error(error: ModelProviderStorageError) -> ModelServiceError {
    if error.sqlstate() == Some("23505") {
        ModelServiceError::UniqueConflict
    } else if error == ModelProviderStorageError::InvalidRow {
        ModelServiceError::InvalidRow
    } else {
        ModelServiceError::Database
    }
}

// Never Debug: arguments may contain provider credentials.
enum Parameter {
    Provider(Bind),
    Model(ModelParam),
}
fn bind_query<'a>(
    sql: &'a str,
    params: Vec<Parameter>,
) -> Result<Query<'a, Postgres, PgArguments>, sqlx::Error> {
    let mut query = sqlx::query(sql);
    for param in params {
        query = match param {
            Parameter::Provider(Bind::Text(v)) | Parameter::Model(ModelParam::Text(v)) => {
                query.bind(v)
            }
            Parameter::Provider(Bind::Null) => query.bind(Option::<String>::None),
            Parameter::Provider(Bind::Bool(v)) | Parameter::Model(ModelParam::Bool(v)) => {
                query.bind(v)
            }
            Parameter::Provider(Bind::Timestamp(v))
            | Parameter::Model(ModelParam::Timestamp(v)) => query.bind(v),
            Parameter::Model(ModelParam::Integer(v)) => query.bind(v),
            Parameter::Model(ModelParam::Decimal(v)) => query.bind(v),
            Parameter::Model(ModelParam::Jsonb(v)) => {
                let value: Value = serde_json::from_str(&v)
                    .map_err(|_| sqlx::Error::Protocol("model_provider_invalid_json".into()))?;
                query.bind(sqlx::types::Json(value))
            }
        };
    }
    Ok(query)
}

#[derive(Clone)]
struct Storage {
    pool: PgPool,
    operations: PgOperationStorage,
}
impl Storage {
    fn new(pool: PgPool, control: PgPool) -> Result<Self, ModelProviderStorageError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool.clone(), control)?,
            pool,
        })
    }
    async fn rows(
        &self,
        sql: String,
        params: Vec<Parameter>,
    ) -> Result<Vec<Map<String, Value>>, ModelProviderStorageError> {
        let rows = if sql
            .split_whitespace()
            .next()
            .is_some_and(|v| v.eq_ignore_ascii_case("SELECT"))
        {
            bind_query(&sql, params)?.fetch_all(&self.pool).await?
        } else {
            let mut connection = self.operations.acquire().await?;
            connection
                .execute(move |connection| {
                    Box::pin(async move { bind_query(&sql, params)?.fetch_all(connection).await })
                })
                .await?
        };
        rows.into_iter().map(decode_row).collect()
    }
    async fn execute(
        &self,
        sql: String,
        params: Vec<Parameter>,
    ) -> Result<String, ModelProviderStorageError> {
        let command = sql
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let mut connection = self.operations.acquire().await?;
        let count = connection
            .execute(move |connection| {
                Box::pin(async move {
                    Ok(bind_query(&sql, params)?
                        .execute(connection)
                        .await?
                        .rows_affected())
                })
            })
            .await?;
        Ok(if command == "INSERT" {
            format!("INSERT 0 {count}")
        } else {
            format!("{command} {count}")
        })
    }
}

#[derive(Clone)]
pub struct PgProviderDatabase {
    storage: Storage,
}
impl PgProviderDatabase {
    pub fn new(pool: PgPool, control_pool: PgPool) -> Result<Self, ModelProviderStorageError> {
        Ok(Self {
            storage: Storage::new(pool, control_pool)?,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), ModelProviderStorageError> {
        self.storage
            .operations
            .drain_cancelled()
            .await
            .map_err(Into::into)
    }
    pub async fn fetch_rows(
        &self,
        sql: &str,
        params: Vec<Bind>,
    ) -> Result<Vec<ProviderRow>, ModelProviderStorageError> {
        self.storage
            .rows(
                sql.to_owned(),
                params.into_iter().map(Parameter::Provider).collect(),
            )
            .await
    }
    pub async fn execute_statement(
        &self,
        sql: &str,
        params: Vec<Bind>,
    ) -> Result<String, ModelProviderStorageError> {
        self.storage
            .execute(
                sql.to_owned(),
                params.into_iter().map(Parameter::Provider).collect(),
            )
            .await
    }
}
impl ProviderDatabase for PgProviderDatabase {
    fn fetch<'a>(&'a self, sql: &'a str, params: Vec<Bind>) -> ServiceFuture<'a, Vec<ProviderRow>> {
        Box::pin(async move { self.fetch_rows(sql, params).await.map_err(provider_error) })
    }
    fn fetchrow<'a>(
        &'a self,
        sql: &'a str,
        params: Vec<Bind>,
    ) -> ServiceFuture<'a, Option<ProviderRow>> {
        Box::pin(async move {
            Ok(self
                .fetch_rows(sql, params)
                .await
                .map_err(provider_error)?
                .into_iter()
                .next())
        })
    }
    fn execute<'a>(&'a self, sql: &'a str, params: Vec<Bind>) -> ServiceFuture<'a, String> {
        Box::pin(async move {
            self.execute_statement(sql, params)
                .await
                .map_err(provider_error)
        })
    }
}

#[derive(Clone)]
pub struct PgModelDatabase {
    storage: Storage,
}
impl PgModelDatabase {
    pub fn new(pool: PgPool, control_pool: PgPool) -> Result<Self, ModelProviderStorageError> {
        Ok(Self {
            storage: Storage::new(pool, control_pool)?,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), ModelProviderStorageError> {
        self.storage
            .operations
            .drain_cancelled()
            .await
            .map_err(Into::into)
    }
    pub async fn fetch_rows(
        &self,
        query: ModelQuery,
    ) -> Result<Vec<ModelRow>, ModelProviderStorageError> {
        self.storage
            .rows(
                query.sql,
                query.params.into_iter().map(Parameter::Model).collect(),
            )
            .await
    }
    pub async fn execute_statement(
        &self,
        query: ModelQuery,
    ) -> Result<String, ModelProviderStorageError> {
        self.storage
            .execute(
                query.sql,
                query.params.into_iter().map(Parameter::Model).collect(),
            )
            .await
    }
}
impl ModelDatabase for PgModelDatabase {
    fn fetch(&self, query: ModelQuery) -> ModelFuture<'_, Vec<ModelRow>> {
        Box::pin(async move { self.fetch_rows(query).await.map_err(model_error) })
    }
    fn fetchrow(&self, query: ModelQuery) -> ModelFuture<'_, Option<ModelRow>> {
        Box::pin(async move {
            Ok(self
                .fetch_rows(query)
                .await
                .map_err(model_error)?
                .into_iter()
                .next())
        })
    }
    fn execute(&self, query: ModelQuery) -> ModelFuture<'_, String> {
        Box::pin(async move { self.execute_statement(query).await.map_err(model_error) })
    }
}

pub(crate) fn decode_row(row: PgRow) -> Result<Map<String, Value>, ModelProviderStorageError> {
    let mut result = Map::new();
    for (index, column) in row.columns().iter().enumerate() {
        let raw = row
            .try_get_raw(index)
            .map_err(|_| ModelProviderStorageError::InvalidRow)?;
        let value = if raw.is_null() {
            Value::Null
        } else {
            let decoded: Result<Value, sqlx::Error> = match column.type_info().name() {
                "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => {
                    row.try_get::<String, _>(index).map(Value::String)
                }
                "BOOL" => row.try_get::<bool, _>(index).map(Value::Bool),
                "INT2" => row.try_get::<i16, _>(index).map(|v| json!(v)),
                "INT4" => row.try_get::<i32, _>(index).map(|v| json!(v)),
                "INT8" => row.try_get::<i64, _>(index).map(|v| json!(v)),
                "JSON" | "JSONB" => row.try_get::<Value, _>(index),
                "TIMESTAMPTZ" => row
                    .try_get::<DateTime<Utc>, _>(index)
                    .map(|v| json!(v.to_rfc3339())),
                "TIMESTAMP" => row
                    .try_get::<NaiveDateTime, _>(index)
                    .map(|v| json!(v.format("%Y-%m-%dT%H:%M:%S%.f").to_string())),
                "NUMERIC" => {
                    let bytes = raw
                        .as_bytes()
                        .map_err(|_| ModelProviderStorageError::InvalidRow)?;
                    let value = match raw.format() {
                        PgValueFormat::Binary => numeric_text(bytes)?,
                        PgValueFormat::Text => std::str::from_utf8(bytes)
                            .map_err(|_| ModelProviderStorageError::InvalidRow)?
                            .to_owned(),
                    };
                    Ok(Value::String(value))
                }
                _ => return Err(ModelProviderStorageError::InvalidRow),
            };
            decoded.map_err(|_| ModelProviderStorageError::InvalidRow)?
        };
        result.insert(column.name().to_owned(), value);
    }
    Ok(result)
}

// PostgreSQL NUMERIC binary representation: base-10000 groups plus decimal scale.
// Preserve canonical server digits and trailing zeros; reject nonfinite values,
// invalid lengths, scales, signs and base-10000 digits.
fn numeric_text(bytes: &[u8]) -> Result<String, ModelProviderStorageError> {
    let bad = ModelProviderStorageError::InvalidRow;
    if bytes.len() < 8 || !bytes.len().is_multiple_of(2) {
        return Err(bad);
    }
    let word = |i| u16::from_be_bytes([bytes[i], bytes[i + 1]]);
    let count = usize::from(word(0));
    let weight = i32::from(word(2) as i16);
    let sign = word(4);
    let scale = usize::from(word(6));
    if bytes.len() != 8 + count * 2 || !matches!(sign, 0 | 0x4000) || scale > 16383 {
        return Err(bad);
    }
    let digits: Vec<_> = (0..count).map(|i| word(8 + i * 2)).collect();
    if digits.iter().any(|d| *d > 9999) {
        return Err(bad);
    }
    let group = |position: i32| -> u16 {
        let index = weight - position;
        if index >= 0 {
            digits.get(index as usize).copied().unwrap_or(0)
        } else {
            0
        }
    };
    let mut value = String::new();
    if sign == 0x4000 && digits.iter().any(|d| *d != 0) {
        value.push('-');
    }
    if weight < 0 {
        value.push('0');
    } else {
        value.push_str(&group(weight).to_string());
        for position in (0..weight).rev() {
            value.push_str(&format!("{:04}", group(position)));
        }
    }
    if scale > 0 {
        value.push('.');
        let fraction_start = value.len();
        for index in 1..=scale.div_ceil(4) {
            value.push_str(&format!("{:04}", group(-(index as i32))));
        }
        value.truncate(fraction_start + scale);
    }
    Ok(value)
}


/// Shared typed binder for the transaction-scoped reviewed catalog adapter.
pub(crate) fn bind_model_query(sql: &str, params: Vec<ModelParam>) -> Result<Query<'_ , Postgres, PgArguments>, sqlx::Error> { bind_query(sql, params.into_iter().map(Parameter::Model).collect()) }
