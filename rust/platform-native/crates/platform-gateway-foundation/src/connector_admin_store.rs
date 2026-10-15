//! Canonical connector_configs/user_connectors statement boundary.
//! Unqualified table names intentionally retain the host's canonical search_path.
//! Each statement is separately supervised/committed: later failures do not roll
//! back earlier route statements. Construction is inert. Hosts must abort AND join
//! callers, then drain_cancelled before closing pools; dispatched COMMIT is final.
use crate::{
    postgres_operation::{PgOperationError, PgOperationStorage},
    protocol::ProtocolTimestamp,
};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde_json::{Map, Value, json};
use sqlx::{Column, PgPool, Row, TypeInfo, ValueRef};
use std::{fmt, future::Future, pin::Pin, sync::Arc};

pub type ConnectorStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ConnectorStoreError>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorStoreError {
    Database,
    Cancelled,
    InvalidCancellationPool,
    IndeterminateCancellation,
}
impl fmt::Display for ConnectorStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database => "connector_database_error",
            Self::Cancelled => "connector_cancelled",
            Self::InvalidCancellationPool => "connector_independent_control_pool_required",
            Self::IndeterminateCancellation => "connector_cancellation_indeterminate",
        })
    }
}
impl std::error::Error for ConnectorStoreError {}
impl From<PgOperationError> for ConnectorStoreError {
    fn from(e: PgOperationError) -> Self {
        match e {
            PgOperationError::Cancelled => Self::Cancelled,
            PgOperationError::InvalidCancellationPool => Self::InvalidCancellationPool,
            PgOperationError::IndeterminateCancellation => Self::IndeterminateCancellation,
            _ => Self::Database,
        }
    }
}
/// Values have no Debug implementation: arguments can contain ciphertext.
#[derive(Clone, PartialEq)]
pub enum ConnectorArgument {
    Text(Option<String>),
    Bool(Option<bool>),
    Json(Value),
}
impl ConnectorArgument {
    pub fn text(v: impl Into<String>) -> Self {
        Self::Text(Some(v.into()))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorStatementKind {
    List,
    TenantRow,
    GlobalExists,
    TenantExists,
    ConnectedUsers,
    Insert,
    Update,
    MergeExtra,
    Toggle,
    Delete,
}
impl ConnectorStatementKind {
    fn writes(self) -> bool {
        matches!(
            self,
            Self::Insert | Self::Update | Self::MergeExtra | Self::Toggle | Self::Delete
        )
    }
}
#[derive(Clone, PartialEq)]
pub struct ConnectorStatement {
    pub kind: ConnectorStatementKind,
    pub sql: String,
    pub arguments: Vec<ConnectorArgument>,
}
impl fmt::Debug for ConnectorStatement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectorStatement")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
pub trait ConnectorAdminDatabase: Send + Sync {
    fn run(&self, statement: ConnectorStatement) -> ConnectorStoreFuture<'_, Vec<Value>>;
}
pub trait ConnectorAdminPool: Send + Sync {
    fn run(&self, statement: ConnectorStatement) -> ConnectorStoreFuture<'_, Vec<Value>>;
}
pub struct SqlConnectorAdminDatabase {
    pool: Option<Arc<dyn ConnectorAdminPool>>,
    enabled: bool,
}
impl SqlConnectorAdminDatabase {
    pub fn new(enabled: bool, pool: Option<Arc<dyn ConnectorAdminPool>>) -> Self {
        Self { pool, enabled }
    }
    pub fn enabled(&self) -> bool {
        self.enabled
    }
}
impl ConnectorAdminDatabase for SqlConnectorAdminDatabase {
    fn run(&self, s: ConnectorStatement) -> ConnectorStoreFuture<'_, Vec<Value>> {
        Box::pin(async move {
            match &self.pool {
                Some(p) => p.run(s).await,
                None => Ok(vec![]),
            }
        })
    }
}
pub struct PgConnectorAdminPool {
    storage: PgOperationStorage,
}
impl PgConnectorAdminPool {
    pub fn new(data: PgPool, control: PgPool) -> Result<Self, ConnectorStoreError> {
        Ok(Self {
            storage: PgOperationStorage::new(data, control)?,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), ConnectorStoreError> {
        self.storage.drain_cancelled().await.map_err(Into::into)
    }
}
impl ConnectorAdminPool for PgConnectorAdminPool {
    fn run(&self, statement: ConnectorStatement) -> ConnectorStoreFuture<'_, Vec<Value>> {
        Box::pin(async move {
            let mut connection = self.storage.acquire().await?;
            let rows = connection
                .execute(move |connection| {
                    Box::pin(async move {
                        let mut query = sqlx::query(&statement.sql);
                        for value in &statement.arguments {
                            query = match value {
                                ConnectorArgument::Text(v) => query.bind(v.as_deref()),
                                ConnectorArgument::Bool(v) => query.bind(*v),
                                ConnectorArgument::Json(v) => query.bind(sqlx::types::Json(v)),
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
fn raw_row(row: &sqlx::postgres::PgRow) -> Result<Value, ConnectorStoreError> {
    let bad = |_: sqlx::Error| ConnectorStoreError::Database;
    let mut out = Map::new();
    for column in row.columns() {
        let i = column.ordinal();
        let cell = row.try_get_raw(i).map_err(bad)?;
        let value = if cell.is_null() {
            Value::Null
        } else {
            match column.type_info().name() {
                "INT4" => json!(row.try_get::<i32, _>(i).map_err(bad)?),
                "INT8" => json!(row.try_get::<i64, _>(i).map_err(bad)?),
                "BOOL" => json!(row.try_get::<bool, _>(i).map_err(bad)?),
                "UUID" => json!(
                    uuid::Uuid::from_slice(
                        cell.as_bytes().map_err(|_| ConnectorStoreError::Database)?
                    )
                    .map_err(|_| ConnectorStoreError::Database)?
                    .to_string()
                ),
                "TIMESTAMPTZ" => json!(timestamp(
                    cell.as_bytes().map_err(|_| ConnectorStoreError::Database)?
                )?),
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
fn timestamp(bytes: &[u8]) -> Result<String, ConnectorStoreError> {
    let micros = i64::from_be_bytes(
        bytes
            .try_into()
            .map_err(|_| ConnectorStoreError::Database)?,
    );
    if matches!(micros, i64::MIN | i64::MAX) {
        return Err(ConnectorStoreError::Database);
    }
    let epoch = NaiveDate::from_ymd_opt(2000, 1, 1)
        .expect("valid epoch")
        .and_hms_opt(0, 0, 0)
        .expect("midnight");
    let value = epoch
        .checked_add_signed(Duration::microseconds(micros))
        .filter(|v| (1..=9999).contains(&v.year()))
        .ok_or(ConnectorStoreError::Database)?;
    Ok(ProtocolTimestamp::from(
        DateTime::<Utc>::from_naive_utc_and_offset(value, Utc).fixed_offset(),
    )
    .to_isoformat())
}
