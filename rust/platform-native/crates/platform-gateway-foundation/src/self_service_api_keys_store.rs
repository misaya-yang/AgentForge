//! Native supervised PostgreSQL self-service API-key CRUD. No pools or routes
//! are created. All discovery/read/write statements use PgOperationStorage.
//! Read the supervisor's direct/session-pinned pool requirements. Abort and join
//! callers, then drain_cancelled before shutting down the runtime/pools. Cancelling
//! an already accepted COMMIT cannot undo it. No retries or cross-statement atomicity.
//!
//! Discovery prefers scopes over permissions across current_schemas(FALSE),
//! defaults to scopes when neither exists, and is intentionally per operation:
//! Python constructs a fresh APIKeyService per endpoint. SQL names are allowlisted.
//! The host binds naive datetimes using its asyncpg-compatible local-zone policy.
//! Native errors erase SQL text and values. Raw hashes never enter response SQL.
//! JSONB rate_limit retains asyncpg default-codec text, including JSON null as
//! the string "null". Nullable response fields/elements remain null until HTTP
//! projection, so ownership rejection precedes response-validation failure.
use crate::{
    postgres_operation::{PgOperationConnection, PgOperationError, PgOperationStorage},
    protocol::ProtocolTimestamp,
    self_service_api_keys::{
        KeyFilter, KeyFuture, SelfServiceApiKeyError, SelfServiceApiKeyStore, StoredCreateKey,
    },
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Row, postgres::PgRow};
use std::{fmt, sync::Arc};

#[derive(Clone, PartialEq, Eq)]
pub enum KeyStoreError {
    Database { sqlstate: Option<String> },
    Cancelled,
    InvalidCancellationPool,
    IndeterminateCancellation,
}
impl fmt::Debug for KeyStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SelfServiceKeyStoreError(redacted)")
    }
}
impl fmt::Display for KeyStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("self_service_api_key_store_unavailable")
    }
}
impl std::error::Error for KeyStoreError {}
impl From<PgOperationError> for KeyStoreError {
    fn from(e: PgOperationError) -> Self {
        match e {
            PgOperationError::Database { sqlstate } => Self::Database { sqlstate },
            PgOperationError::Cancelled => Self::Cancelled,
            PgOperationError::InvalidCancellationPool => Self::InvalidCancellationPool,
            PgOperationError::IndeterminateCancellation => Self::IndeterminateCancellation,
        }
    }
}
impl KeyStoreError {
    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            Self::Database { sqlstate } => sqlstate.as_deref(),
            _ => None,
        }
    }
}
fn error(e: PgOperationError) -> SelfServiceApiKeyError {
    if e == PgOperationError::Cancelled {
        SelfServiceApiKeyError::Cancelled
    } else {
        SelfServiceApiKeyError::Internal
    }
}
pub trait KeyTimestampBinding: Send + Sync {
    fn bind(&self, value: ProtocolTimestamp) -> Result<DateTime<Utc>, SelfServiceApiKeyError>;
}
pub struct PgSelfServiceApiKeyStore {
    operations: PgOperationStorage,
    timestamp: Arc<dyn KeyTimestampBinding>,
}
impl PgSelfServiceApiKeyStore {
    pub fn new(
        pool: PgPool,
        independent_control_pool: PgPool,
        timestamp: Arc<dyn KeyTimestampBinding>,
    ) -> Result<Self, KeyStoreError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool, independent_control_pool)
                .map_err(KeyStoreError::from)?,
            timestamp,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), KeyStoreError> {
        self.operations.drain_cancelled().await.map_err(Into::into)
    }
    async fn acquire(&self) -> Result<PgOperationConnection, SelfServiceApiKeyError> {
        self.operations.acquire().await.map_err(error)
    }
    async fn scope(
        connection: &mut PgOperationConnection,
    ) -> Result<&'static str, SelfServiceApiKeyError> {
        let column: Option<String> = connection.execute(|connection| Box::pin(async move {
            sqlx::query_scalar("SELECT column_name FROM information_schema.columns WHERE table_schema = ANY (current_schemas(FALSE)) AND table_name = 'api_keys' AND column_name IN ('scopes', 'permissions') ORDER BY CASE column_name WHEN 'scopes' THEN 0 ELSE 1 END LIMIT 1")
                .fetch_optional(connection).await
        })).await.map_err(error)?;
        Ok(if column.as_deref() == Some("permissions") {
            "permissions"
        } else {
            "scopes"
        })
    }
    async fn mutate(
        &self,
        id: &str,
        filter: &KeyFilter,
        revoke: bool,
    ) -> Result<bool, SelfServiceApiKeyError> {
        let mut conditions = vec![format!("{PUBLIC_ID} = $1")];
        let mut params = vec![id.to_owned()];
        filters(filter, &mut conditions, &mut params);
        let query = if revoke {
            format!(
                "UPDATE api_keys SET enabled = FALSE, updated_at = NOW() WHERE {} RETURNING {PUBLIC_ID} AS key_id",
                conditions.join(" AND ")
            )
        } else {
            format!(
                "DELETE FROM api_keys WHERE {} RETURNING {PUBLIC_ID} AS key_id",
                conditions.join(" AND ")
            )
        };
        self.acquire()
            .await?
            .execute(move |connection| {
                Box::pin(async move {
                    let mut q = sqlx::query(&query);
                    for value in params {
                        q = q.bind(value);
                    }
                    Ok(q.fetch_optional(connection).await?.is_some())
                })
            })
            .await
            .map_err(error)
    }
}
const PUBLIC_ID: &str = "COALESCE(key_id, ('ak_legacy_' || id::text))";
fn filters(filter: &KeyFilter, conditions: &mut Vec<String>, params: &mut Vec<String>) {
    if let Some(user) = filter.user_id.as_ref().filter(|v| !v.is_empty()) {
        params.push(user.clone());
        conditions.push(format!("user_id = ${}", params.len()));
    }
    if let Some(tenant) = filter.tenant_id.as_ref().filter(|v| !v.is_empty()) {
        params.push(tenant.clone());
        conditions.push(format!(
            "COALESCE(NULLIF(tenant_id, ''), 'default') = ${}",
            params.len()
        ));
    }
}
fn select(scope: &str) -> String {
    format!(
        "SELECT {PUBLIC_ID} AS key_id, key_prefix, name, description, user_id, tenant_id, {scope} AS scopes, roles, tier, rate_limit, enabled AS is_active, created_at, ('apikey:' || SUBSTRING(key_hash, 1, 16)) AS derived_user_id, last_used_at, expires_at, use_count FROM api_keys"
    )
}
fn timestamp(value: Option<DateTime<Utc>>) -> Value {
    value
        .map(|t| {
            json!(t.to_rfc3339_opts(
                if t.timestamp_subsec_micros() == 0 {
                    chrono::SecondsFormat::Secs
                } else {
                    chrono::SecondsFormat::Micros
                },
                true
            ))
        })
        .unwrap_or(Value::Null)
}
fn row(row: PgRow, create: bool) -> Result<Value, sqlx::Error> {
    let mut result = json!({
        "key_id":row.try_get::<Option<String>,_>("key_id")?,
        "key_prefix":row.try_get::<Option<String>,_>("key_prefix")?,
        "name":row.try_get::<Option<String>,_>("name")?,
        "description":row.try_get::<Option<String>,_>("description")?,
        "scopes":row.try_get::<Option<Vec<Option<String>>>,_>("scopes")?,
        "roles":row.try_get::<Option<Vec<Option<String>>>,_>("roles")?,
        "tier":row.try_get::<Option<String>,_>("tier")?,
        "rate_limit":row.try_get::<Option<sqlx::types::Json<Box<serde_json::value::RawValue>>>,_>("rate_limit")?.map(|v|v.0.get().to_owned()),
        "created_at":timestamp(row.try_get("created_at")?),
        "expires_at":timestamp(row.try_get("expires_at")?),
    });
    if create {
        result["id"] = json!(row.try_get::<i32, _>("id")?);
        result["enabled"] = json!(row.try_get::<Option<bool>, _>("enabled")?);
    } else {
        result["is_active"] = json!(row.try_get::<Option<bool>, _>("is_active")?);
        for field in ["user_id", "tenant_id", "derived_user_id"] {
            result[field] = json!(row.try_get::<Option<String>, _>(field)?);
        }
        result["last_used_at"] = timestamp(row.try_get("last_used_at")?);
        result["use_count"] = json!(row.try_get::<Option<i64>, _>("use_count")?);
    }
    Ok(result)
}
impl SelfServiceApiKeyStore for PgSelfServiceApiKeyStore {
    fn create<'a>(&'a self, input: &'a StoredCreateKey) -> KeyFuture<'a, Value> {
        Box::pin(async move {
            let mut connection = self.acquire().await?;
            let scope = Self::scope(&mut connection).await?;
            let input = input.clone();
            let expires = input
                .request
                .expires_at
                .map(|v| self.timestamp.bind(v))
                .transpose()?;
            let query = format!(
                "INSERT INTO api_keys (key_id, key_hash, key_prefix, name, description, user_id, tenant_id, {scope}, roles, tier, rate_limit, expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING id, key_id, key_prefix, name, description, {scope} AS scopes, roles, tier, rate_limit, created_at, expires_at, enabled"
            );
            connection
                .execute(move |connection| {
                    Box::pin(async move {
                        let result = sqlx::query(&query)
                            .bind(input.key_id)
                            .bind(input.key_hash)
                            .bind(input.key_prefix)
                            .bind(input.request.name)
                            .bind(input.request.description)
                            .bind(input.request.user_id)
                            .bind(input.request.tenant_id)
                            .bind(input.request.scopes)
                            .bind(vec!["user".to_owned()])
                            .bind(input.request.tier)
                            .bind(None::<Value>)
                            .bind(expires)
                            .fetch_one(connection)
                            .await?;
                        row(result, true)
                    })
                })
                .await
                .map_err(error)
        })
    }
    fn list<'a>(&'a self, filter: &'a KeyFilter) -> KeyFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let mut conditions = Vec::new();
            let mut params = Vec::new();
            filters(filter, &mut conditions, &mut params);
            if !filter.include_inactive {
                conditions.push("enabled = TRUE".into());
            }
            if conditions.is_empty() {
                conditions.push("1=1".into());
            }
            let mut connection = self.acquire().await?;
            let scope = Self::scope(&mut connection).await?;
            let query = format!(
                "{} WHERE {} ORDER BY created_at DESC",
                select(scope),
                conditions.join(" AND ")
            );
            connection
                .execute(move |connection| {
                    Box::pin(async move {
                        let mut q = sqlx::query(&query);
                        for value in params {
                            q = q.bind(value);
                        }
                        q.fetch_all(connection)
                            .await?
                            .into_iter()
                            .map(|r| row(r, false))
                            .collect()
                    })
                })
                .await
                .map_err(error)
        })
    }
    fn get<'a>(&'a self, id: &'a str, tenant: Option<&'a str>) -> KeyFuture<'a, Option<Value>> {
        Box::pin(async move {
            let mut connection = self.acquire().await?;
            let scope = Self::scope(&mut connection).await?;
            let mut conditions = vec![format!("{PUBLIC_ID} = $1")];
            let mut params = vec![id.to_owned()];
            filters(
                &KeyFilter {
                    tenant_id: tenant.map(str::to_owned),
                    ..Default::default()
                },
                &mut conditions,
                &mut params,
            );
            let query = format!("{} WHERE {}", select(scope), conditions.join(" AND "));
            connection
                .execute(move |connection| {
                    Box::pin(async move {
                        let mut q = sqlx::query(&query);
                        for value in params {
                            q = q.bind(value);
                        }
                        q.fetch_optional(connection)
                            .await?
                            .map(|r| row(r, false))
                            .transpose()
                    })
                })
                .await
                .map_err(error)
        })
    }
    fn revoke<'a>(&'a self, id: &'a str, filter: &'a KeyFilter) -> KeyFuture<'a, bool> {
        Box::pin(self.mutate(id, filter, true))
    }
    fn delete<'a>(&'a self, id: &'a str, filter: &'a KeyFilter) -> KeyFuture<'a, bool> {
        Box::pin(self.mutate(id, filter, false))
    }
}
