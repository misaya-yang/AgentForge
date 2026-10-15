//! Canonical DatabaseStorage.save_api_key/list_api_keys boundary.
//!
//! SQL preserves ON CONFLICT(key_hash), empty-role fallback, nullable fields and
//! optional truthy tenant/user filters. API calls list with no filters. Projection
//! excludes hash/key_id/key_prefix. Native row timestamp spelling is host supplied.
//! rate_limit retains raw PostgreSQL JSONB text, as the default asyncpg codec and
//! canonical _row_to_dict do; JSON null remains the string "null", unlike SQL NULL.
//! No pool means zero/empty, even with enabled=true. Constructors are inert.
//! Owned PgOperationStorage supervises statements; callers must abort/join work,
//! then drain_cancelled before closing either independent caller-owned pool.
//! BEGIN timing and already-dispatched COMMIT limitations are inherited unchanged.
//! Errors and Debug omit SQL values, hashes, credentials, connection/backend text.
use crate::{
    postgres_operation::{PgOperationError, PgOperationStorage},
    service_registry::python_truthy,
    usage_parser::python_value_to_string,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{fmt, future::Future, pin::Pin, sync::Arc};
pub type ConfigApiKeysStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ConfigApiKeysStoreError>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigApiKeysStoreError {
    Database,
    Cancelled,
    InvalidCancellationPool,
    IndeterminateCancellation,
    InvalidValue,
}
impl fmt::Display for ConfigApiKeysStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database => "config_api_keys_database_error",
            Self::Cancelled => "config_api_keys_cancelled",
            Self::InvalidCancellationPool => "config_api_keys_independent_control_pool_required",
            Self::IndeterminateCancellation => "config_api_keys_cancellation_indeterminate",
            Self::InvalidValue => "config_api_keys_invalid_value",
        })
    }
}
impl std::error::Error for ConfigApiKeysStoreError {}
impl From<PgOperationError> for ConfigApiKeysStoreError {
    fn from(value: PgOperationError) -> Self {
        match value {
            PgOperationError::Cancelled => Self::Cancelled,
            PgOperationError::InvalidCancellationPool => Self::InvalidCancellationPool,
            PgOperationError::IndeterminateCancellation => Self::IndeterminateCancellation,
            _ => Self::Database,
        }
    }
}
#[derive(Clone)]
pub struct ConfigApiKeySave {
    pub key_hash: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub tenant_id: Option<String>,
    pub user_id: Option<String>,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub tier: String,
    pub rate_limit: Option<Value>,
    pub allowed_services: Vec<String>,
    pub allowed_models: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}
impl fmt::Debug for ConfigApiKeySave {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConfigApiKeySave([REDACTED])")
    }
}
pub trait ConfigApiKeysDatabase: Send + Sync {
    fn enabled(&self) -> bool;
    fn list(&self) -> ConfigApiKeysStoreFuture<'_, Vec<Value>>;
    fn save<'a>(&'a self, key: &'a ConfigApiKeySave) -> ConfigApiKeysStoreFuture<'a, i64>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigApiKeysStatementKind {
    List,
    Save,
}
#[derive(Clone, PartialEq)]
pub struct ConfigApiKeysStatement {
    pub sql: String,
    pub arguments: Vec<Value>,
    pub kind: ConfigApiKeysStatementKind,
}
impl fmt::Debug for ConfigApiKeysStatement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigApiKeysStatement")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Default, PartialEq)]
pub struct ConfigApiKeysSqlResult {
    pub rows: Vec<Value>,
    pub inserted_id: Option<i64>,
}
impl fmt::Debug for ConfigApiKeysSqlResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConfigApiKeysSqlResult([REDACTED])")
    }
}
pub trait ConfigApiKeysPool: Send + Sync {
    /// Owns acquisition, statement and context release; failure is observable.
    fn run<'a>(
        &'a self,
        statement: ConfigApiKeysStatement,
    ) -> ConfigApiKeysStoreFuture<'a, ConfigApiKeysSqlResult>;
}
pub struct SqlConfigApiKeysDatabase {
    enabled: bool,
    pool: Option<Arc<dyn ConfigApiKeysPool>>,
}
impl SqlConfigApiKeysDatabase {
    pub fn new(enabled: bool, pool: Option<Arc<dyn ConfigApiKeysPool>>) -> Self {
        Self { enabled, pool }
    }
    pub async fn list_api_keys(
        &self,
        tenant_id: Option<&str>,
        user_id: Option<&str>,
        enabled: Option<bool>,
    ) -> Result<Vec<Value>, ConfigApiKeysStoreError> {
        let Some(pool) = &self.pool else {
            return Ok(vec![]);
        };
        let mut sql = CONFIG_API_KEYS_SELECT_SQL.to_owned();
        let mut arguments = vec![];
        for (name, value) in [("tenant_id", tenant_id), ("user_id", user_id)] {
            if let Some(value) = value.filter(|s| !s.is_empty()) {
                arguments.push(json!(value));
                sql.push_str(&format!(" AND {name} = ${}", arguments.len()));
            }
        }
        if let Some(value) = enabled {
            arguments.push(json!(value));
            sql.push_str(&format!(" AND enabled = ${}", arguments.len()));
        }
        sql.push_str(" ORDER BY created_at DESC");
        Ok(pool
            .run(ConfigApiKeysStatement {
                sql,
                arguments,
                kind: ConfigApiKeysStatementKind::List,
            })
            .await?
            .rows)
    }
    pub async fn save_api_key(
        &self,
        key: &ConfigApiKeySave,
    ) -> Result<i64, ConfigApiKeysStoreError> {
        let Some(pool) = &self.pool else { return Ok(0) };
        let roles = if key.roles.is_empty() {
            vec!["user".to_owned()]
        } else {
            key.roles.clone()
        };
        let rate = key
            .rate_limit
            .as_ref()
            .filter(|v| python_truthy(v))
            .map(json_dumps_ascii);
        let arguments = vec![
            json!(key.key_hash),
            json!(key.name),
            json!(key.description),
            json!(key.tenant_id),
            json!(key.user_id),
            json!(roles),
            json!(key.permissions),
            json!(key.tier),
            json!(rate),
            json!(key.allowed_services),
            json!(key.allowed_models),
            json!(key.expires_at),
        ];
        Ok(pool
            .run(ConfigApiKeysStatement {
                sql: CONFIG_API_KEYS_SAVE_SQL.into(),
                arguments,
                kind: ConfigApiKeysStatementKind::Save,
            })
            .await?
            .inserted_id
            .unwrap_or(0))
    }
}
impl ConfigApiKeysDatabase for SqlConfigApiKeysDatabase {
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn list(&self) -> ConfigApiKeysStoreFuture<'_, Vec<Value>> {
        Box::pin(self.list_api_keys(None, None, None))
    }
    fn save<'a>(&'a self, key: &'a ConfigApiKeySave) -> ConfigApiKeysStoreFuture<'a, i64> {
        Box::pin(self.save_api_key(key))
    }
}
// Source json.dumps default ASCII escaping, spaces and Python numeric spelling.
fn json_dumps_ascii(value: &Value) -> String {
    match value {
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(json_dumps_ascii)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(k, v)| format!("{}: {}", json_dumps_ascii(&json!(k)), json_dumps_ascii(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Number(_) => python_value_to_string(value),
        _ => serde_json::to_string(value)
            .expect("JSON scalar")
            .chars()
            .map(|c| {
                if c.is_ascii() {
                    c.to_string()
                } else {
                    let mut units = [0; 2];
                    c.encode_utf16(&mut units)
                        .iter()
                        .map(|u| format!("\\u{u:04x}"))
                        .collect::<String>()
                }
            })
            .collect(),
    }
}
pub trait ConfigApiKeysTimestampFormatter: Send + Sync {
    fn format(&self, value: DateTime<Utc>) -> Result<String, ConfigApiKeysStoreError>;
}
pub struct PgConfigApiKeysPool {
    storage: PgOperationStorage,
    timestamps: Arc<dyn ConfigApiKeysTimestampFormatter>,
}
impl PgConfigApiKeysPool {
    pub fn new(
        pool: PgPool,
        control_pool: PgPool,
        timestamps: Arc<dyn ConfigApiKeysTimestampFormatter>,
    ) -> Result<Self, ConfigApiKeysStoreError> {
        Ok(Self {
            storage: PgOperationStorage::new(pool, control_pool)?,
            timestamps,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), ConfigApiKeysStoreError> {
        self.storage.drain_cancelled().await.map_err(Into::into)
    }
}
impl ConfigApiKeysPool for PgConfigApiKeysPool {
    fn run<'a>(
        &'a self,
        statement: ConfigApiKeysStatement,
    ) -> ConfigApiKeysStoreFuture<'a, ConfigApiKeysSqlResult> {
        Box::pin(async move {
            let mut connection = self.storage.acquire().await?;
            let kind = statement.kind;
            let rows = connection
                .execute(move |connection| {
                    Box::pin(async move {
                        let invalid =
                            || sqlx::Error::Protocol("config_api_keys_invalid_binding".into());
                        let mut query = sqlx::query(&statement.sql);
                        for (index, value) in statement.arguments.iter().enumerate() {
                            query = if kind == ConfigApiKeysStatementKind::Save
                                && matches!(index, 5 | 6 | 9 | 10)
                            {
                                let array = value
                                    .as_array()
                                    .ok_or_else(invalid)?
                                    .iter()
                                    .map(|v| v.as_str().ok_or_else(invalid))
                                    .collect::<Result<Vec<_>, _>>()?;
                                query.bind(array)
                            } else if kind == ConfigApiKeysStatementKind::Save && index == 8 {
                                let value = value
                                    .as_str()
                                    .map(serde_json::from_str::<Value>)
                                    .transpose()
                                    .map_err(|_| invalid())?;
                                query.bind(value.map(sqlx::types::Json))
                            } else if kind == ConfigApiKeysStatementKind::Save && index == 11 {
                                let value = value
                                    .as_str()
                                    .map(|s| {
                                        DateTime::parse_from_rfc3339(s)
                                            .map(|v| v.with_timezone(&Utc))
                                    })
                                    .transpose()
                                    .map_err(|_| invalid())?;
                                query.bind(value)
                            } else {
                                match value {
                                    Value::String(v) => query.bind(v),
                                    Value::Bool(v) => query.bind(*v),
                                    Value::Null => query.bind(Option::<String>::None),
                                    _ => return Err(invalid()),
                                }
                            };
                        }
                        query.fetch_all(connection).await
                    })
                })
                .await?;
            if kind == ConfigApiKeysStatementKind::Save {
                return Ok(ConfigApiKeysSqlResult {
                    rows: vec![],
                    inserted_id: rows
                        .first()
                        .map(|row| row.try_get::<i32, _>("id").map(i64::from))
                        .transpose()
                        .map_err(|_| ConfigApiKeysStoreError::Database)?,
                });
            }
            let mut values = vec![];
            for row in rows {
                let error = |_: sqlx::Error| ConfigApiKeysStoreError::Database;
                let mut out = json!({"id":row.try_get::<i32,_>("id").map_err(error)?,"roles":row.try_get::<Vec<Option<String>>,_>("roles").map_err(error)?,"permissions":row.try_get::<Vec<Option<String>>,_>("permissions").map_err(error)?,"allowed_services":row.try_get::<Vec<Option<String>>,_>("allowed_services").map_err(error)?,"allowed_models":row.try_get::<Vec<Option<String>>,_>("allowed_models").map_err(error)?,"enabled":row.try_get::<bool,_>("enabled").map_err(error)?,"use_count":row.try_get::<i64,_>("use_count").map_err(error)?,"rate_limit":row.try_get::<Option<sqlx::types::Json<Box<serde_json::value::RawValue>>>,_>("rate_limit").map_err(error)?.map(|value|value.0.get().to_owned())});
                for field in ["name", "description", "tenant_id", "user_id", "tier"] {
                    out[field] = json!(row.try_get::<Option<String>, _>(field).map_err(error)?);
                }
                for field in ["expires_at", "last_used_at", "created_at", "updated_at"] {
                    out[field] = json!(
                        row.try_get::<Option<DateTime<Utc>>, _>(field)
                            .map_err(error)?
                            .map(|v| self.timestamps.format(v))
                            .transpose()?
                    );
                }
                values.push(out);
            }
            Ok(ConfigApiKeysSqlResult {
                rows: values,
                inserted_id: None,
            })
        })
    }
}
pub const CONFIG_API_KEYS_SAVE_SQL: &str = "\n                INSERT INTO api_keys (\n                    key_hash, name, description, tenant_id, user_id,\n                    roles, permissions, tier, rate_limit, allowed_services, allowed_models, expires_at\n                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)\n                ON CONFLICT (key_hash) DO UPDATE SET\n                    name = EXCLUDED.name,\n                    description = EXCLUDED.description,\n                    tenant_id = EXCLUDED.tenant_id,\n                    user_id = EXCLUDED.user_id,\n                    roles = EXCLUDED.roles,\n                    permissions = EXCLUDED.permissions,\n                    tier = EXCLUDED.tier,\n                    rate_limit = EXCLUDED.rate_limit,\n                    allowed_services = EXCLUDED.allowed_services,\n                    allowed_models = EXCLUDED.allowed_models,\n                    expires_at = EXCLUDED.expires_at,\n                    updated_at = NOW()\n                RETURNING id\n            ";
pub const CONFIG_API_KEYS_SELECT_SQL: &str = "\n            SELECT id, name, description, tenant_id, user_id, roles,\n                   permissions, tier, rate_limit, allowed_services, allowed_models,\n                   expires_at, enabled, last_used_at, use_count, created_at, updated_at\n            FROM api_keys WHERE 1=1\n        ";
