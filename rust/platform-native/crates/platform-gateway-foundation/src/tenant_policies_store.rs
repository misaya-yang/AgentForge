//! Raw DatabaseStorage fetch/fetchrow/execute boundary for tenant policy administration.
//!
//! Construction is inert. A present object without a pool returns empty/no-op even
//! when disabled; the API checks only object presence. Each source statement gets
//! its own supervised transaction/acquisition. Audit rows and count are separate
//! reads, not a snapshot. Raw JSON/JSONB remains asyncpg text including JSON null.
//! All canonical SELECT * columns are retained. No audit writer or policy engine.
//! Timestamp wire support is finite Python datetime years 1..9999; PostgreSQL
//! infinities/out-of-range values return a redacted error rather than panicking.
//! Hosts abort AND join callers, then drain_cancelled before closing pools. An
//! already-dispatched COMMIT cannot be undone; normal deadlines are host owned.
use crate::{
    postgres_operation::{PgOperationError, PgOperationStorage},
    protocol::ProtocolTimestamp,
    usage_parser::python_value_to_string,
};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde_json::{Map, Value, json};
use sqlx::{Column, PgPool, Row, TypeInfo, ValueRef};
use std::{fmt, future::Future, pin::Pin, sync::Arc};

pub type TenantPoliciesStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TenantPoliciesStoreError>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TenantPoliciesStoreError {
    Database,
    Cancelled,
    InvalidCancellationPool,
    IndeterminateCancellation,
}
impl fmt::Display for TenantPoliciesStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database => "tenant_policies_database_error",
            Self::Cancelled => "tenant_policies_cancelled",
            Self::InvalidCancellationPool => "tenant_policies_independent_control_pool_required",
            Self::IndeterminateCancellation => "tenant_policies_cancellation_indeterminate",
        })
    }
}
impl std::error::Error for TenantPoliciesStoreError {}
impl From<PgOperationError> for TenantPoliciesStoreError {
    fn from(error: PgOperationError) -> Self {
        match error {
            PgOperationError::Cancelled => Self::Cancelled,
            PgOperationError::InvalidCancellationPool => Self::InvalidCancellationPool,
            PgOperationError::IndeterminateCancellation => Self::IndeterminateCancellation,
            _ => Self::Database,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TenantPoliciesStatementKind {
    ToolList,
    ToolGet,
    ToolUpsert,
    ToolDelete,
    McpList,
    McpGet,
    McpUpsert,
    McpDelete,
    AuditRows,
    AuditCount,
    ConfigAuditRows,
}
impl TenantPoliciesStatementKind {
    fn writes(self) -> bool {
        matches!(
            self,
            Self::ToolUpsert | Self::ToolDelete | Self::McpUpsert | Self::McpDelete
        )
    }
}
#[derive(Clone, PartialEq)]
pub struct TenantPoliciesStatement {
    pub kind: TenantPoliciesStatementKind,
    pub sql: String,
    pub arguments: Vec<Value>,
}
impl fmt::Debug for TenantPoliciesStatement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TenantPoliciesStatement")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
pub trait TenantPoliciesDatabase: Send + Sync {
    /// Explicit capability: do not turn an absent read backend into an empty audit history.
    fn config_audit_available(&self) -> bool {
        false
    }
    fn run(&self, statement: TenantPoliciesStatement) -> TenantPoliciesStoreFuture<'_, Vec<Value>>;
}
pub trait TenantPoliciesPool: Send + Sync {
    fn run(&self, statement: TenantPoliciesStatement) -> TenantPoliciesStoreFuture<'_, Vec<Value>>;
}
pub struct SqlTenantPoliciesDatabase {
    pool: Option<Arc<dyn TenantPoliciesPool>>,
    enabled: bool,
}
impl SqlTenantPoliciesDatabase {
    pub fn new(enabled: bool, pool: Option<Arc<dyn TenantPoliciesPool>>) -> Self {
        Self { pool, enabled }
    }
    /// Observable source state, intentionally not an endpoint availability gate.
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}
impl TenantPoliciesDatabase for SqlTenantPoliciesDatabase {
    fn config_audit_available(&self) -> bool {
        self.enabled && self.pool.is_some()
    }
    fn run(&self, statement: TenantPoliciesStatement) -> TenantPoliciesStoreFuture<'_, Vec<Value>> {
        Box::pin(async move {
            match &self.pool {
                Some(pool) => pool.run(statement).await,
                None => Ok(vec![]),
            }
        })
    }
}
pub struct PgTenantPoliciesPool {
    storage: PgOperationStorage,
}
impl PgTenantPoliciesPool {
    pub fn new(data: PgPool, control: PgPool) -> Result<Self, TenantPoliciesStoreError> {
        Ok(Self {
            storage: PgOperationStorage::new(data, control)?,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), TenantPoliciesStoreError> {
        self.storage.drain_cancelled().await.map_err(Into::into)
    }
}
impl TenantPoliciesPool for PgTenantPoliciesPool {
    fn run(&self, statement: TenantPoliciesStatement) -> TenantPoliciesStoreFuture<'_, Vec<Value>> {
        Box::pin(async move {
            let mut connection = self.storage.acquire().await?;
            let rows = connection
                .execute(move |connection| {
                    Box::pin(async move {
                        let invalid =
                            || sqlx::Error::Protocol("tenant_policies_invalid_binding".into());
                        let mut query = sqlx::query(&statement.sql);
                        for (i, value) in statement.arguments.iter().enumerate() {
                            query = if (statement.kind == TenantPoliciesStatementKind::ToolUpsert
                                && (1..=3).contains(&i))
                                || (statement.kind == TenantPoliciesStatementKind::McpUpsert
                                    && i == 1)
                            {
                                let array = if value.is_null() {
                                    None
                                } else {
                                    Some(
                                        value
                                            .as_array()
                                            .ok_or_else(invalid)?
                                            .iter()
                                            .map(|v| v.as_str().ok_or_else(invalid))
                                            .collect::<Result<Vec<_>, _>>()?,
                                    )
                                };
                                query.bind(array)
                            } else if statement.kind == TenantPoliciesStatementKind::McpUpsert
                                && i == 2
                            {
                                let raw = value
                                    .as_str()
                                    .map(|s| serde_json::value::RawValue::from_string(s.to_owned()))
                                    .transpose();
                                // RawValue owns the original JSON serialization; SQL NULL stays None.
                                let raw = match raw {
                                    Ok(v) => v,
                                    Err(_) => return Err(invalid()),
                                };
                                query.bind(raw.map(sqlx::types::Json))
                            } else if (statement.kind == TenantPoliciesStatementKind::ToolUpsert
                                && i == 4)
                                || (statement.kind == TenantPoliciesStatementKind::McpUpsert
                                    && i == 3)
                            {
                                query.bind(
                                    i32::try_from(value.as_i64().ok_or_else(invalid)?)
                                        .map_err(|_| invalid())?,
                                )
                            } else {
                                match value {
                                    Value::String(v) => query.bind(v),
                                    Value::Number(v) => query.bind(v.as_i64().ok_or_else(invalid)?),
                                    _ => return Err(invalid()),
                                }
                            };
                        }
                        if statement.kind.writes() {
                            query.execute(connection).await?;
                            Ok(vec![])
                        } else {
                            query.fetch_all(connection).await
                        }
                    })
                })
                .await?;
            rows.iter().map(raw_row).collect()
        })
    }
}
fn raw_row(row: &sqlx::postgres::PgRow) -> Result<Value, TenantPoliciesStoreError> {
    let bad = |_: sqlx::Error| TenantPoliciesStoreError::Database;
    let mut out = Map::new();
    for column in row.columns() {
        let i = column.ordinal();
        let cell = row.try_get_raw(i).map_err(bad)?;
        let value = if cell.is_null() {
            Value::Null
        } else {
            match column.type_info().name() {
                "INT8" => json!(row.try_get::<i64, _>(i).map_err(bad)?),
                "INT4" => json!(row.try_get::<i32, _>(i).map_err(bad)?),
                "FLOAT8" => {
                    let v = row.try_get::<f64, _>(i).map_err(bad)?;
                    if !v.is_finite() {
                        return Err(TenantPoliciesStoreError::Database);
                    }
                    json!(v)
                }
                "TIMESTAMPTZ" => json!(raw_timestamp(
                    cell.as_bytes()
                        .map_err(|_| TenantPoliciesStoreError::Database)?
                )?),
                "UUID" => json!(
                    uuid::Uuid::from_slice(
                        cell.as_bytes()
                            .map_err(|_| TenantPoliciesStoreError::Database)?
                    )
                    .map_err(|_| TenantPoliciesStoreError::Database)?
                    .to_string()
                ),
                "TEXT[]" | "_text" => json!(row.try_get::<Vec<Option<String>>, _>(i).map_err(bad)?),
                "JSON" | "JSONB" => json!(
                    row.try_get::<sqlx::types::Json<Box<serde_json::value::RawValue>>, _>(i)
                        .map_err(bad)?
                        .0
                        .get()
                ),
                _ => json!(row.try_get::<String, _>(i).map_err(bad)?),
            }
        };
        out.insert(column.name().into(), value);
    }
    Ok(Value::Object(out))
}
fn raw_timestamp(bytes: &[u8]) -> Result<String, TenantPoliciesStoreError> {
    let micros = i64::from_be_bytes(
        bytes
            .try_into()
            .map_err(|_| TenantPoliciesStoreError::Database)?,
    );
    if matches!(micros, i64::MIN | i64::MAX) {
        return Err(TenantPoliciesStoreError::Database);
    }
    let epoch = NaiveDate::from_ymd_opt(2000, 1, 1)
        .expect("valid epoch")
        .and_hms_opt(0, 0, 0)
        .expect("midnight");
    let value = epoch
        .checked_add_signed(Duration::microseconds(micros))
        .filter(|v| (1..=9999).contains(&v.year()))
        .ok_or(TenantPoliciesStoreError::Database)?;
    Ok(ProtocolTimestamp::from(
        DateTime::<Utc>::from_naive_utc_and_offset(value, Utc).fixed_offset(),
    )
    .to_isoformat())
}
/// Python json.dumps defaults used by the canonical MCP upsert (not a new codec).
pub fn tenant_policy_json_dumps(value: &Value) -> String {
    match value {
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(tenant_policy_json_dumps)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(k, v)| format!(
                    "{}: {}",
                    tenant_policy_json_dumps(&json!(k)),
                    tenant_policy_json_dumps(v)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Number(_) => python_value_to_string(value),
        _ => serde_json::to_string(value)
            .expect("JSON scalar")
            .chars()
            .map(|c| {
                if c <= '\u{7e}' {
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
pub const TOOL_LIST_SQL: &str = "SELECT * FROM tenant_tool_policies ORDER BY tenant_id";
pub const TOOL_GET_SQL: &str = "SELECT * FROM tenant_tool_policies WHERE tenant_id = $1";
pub const TOOL_UPSERT_SQL: &str = "\n        INSERT INTO tenant_tool_policies\n            (tenant_id, allowed_tools, blocked_tools, allowed_categories,\n             max_calls_per_minute, updated_at)\n        VALUES ($1, $2, $3, $4, $5, NOW())\n        ON CONFLICT (tenant_id) DO UPDATE SET\n            allowed_tools = EXCLUDED.allowed_tools,\n            blocked_tools = EXCLUDED.blocked_tools,\n            allowed_categories = EXCLUDED.allowed_categories,\n            max_calls_per_minute = EXCLUDED.max_calls_per_minute,\n            updated_at = NOW()\n        ";
pub const TOOL_DELETE_SQL: &str = "DELETE FROM tenant_tool_policies WHERE tenant_id = $1";
pub const MCP_LIST_SQL: &str = "SELECT * FROM tenant_mcp_configs ORDER BY tenant_id";
pub const MCP_GET_SQL: &str = "SELECT * FROM tenant_mcp_configs WHERE tenant_id = $1";
pub const MCP_UPSERT_SQL: &str = "\n        INSERT INTO tenant_mcp_configs\n            (tenant_id, allowed_servers, server_overrides, max_connections, updated_at)\n        VALUES ($1, $2, $3::jsonb, $4, NOW())\n        ON CONFLICT (tenant_id) DO UPDATE SET\n            allowed_servers = EXCLUDED.allowed_servers,\n            server_overrides = EXCLUDED.server_overrides,\n            max_connections = EXCLUDED.max_connections,\n            updated_at = NOW()\n        ";
pub const MCP_DELETE_SQL: &str = "DELETE FROM tenant_mcp_configs WHERE tenant_id = $1";
