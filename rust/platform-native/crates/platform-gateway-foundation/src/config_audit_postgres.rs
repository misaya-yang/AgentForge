//! Caller-owned PostgreSQL fallback for the accepted configuration audit writer.
//!
//! This adapter consumes that writer's exact thirteen-argument INSERT. It does
//! not build events, redact, encode JSON, choose services, or retry failures.
//! The canonical audit table stores user/resource identifiers and client IP as
//! VARCHAR, not UUID/INET. JSON is already encoded text; PostgreSQL evaluates
//! the existing `$9::jsonb` / `$10::jsonb` casts. `duration_ms` is optional INT4.
//! There is no timestamp bind: the schema supplies `created_at DEFAULT NOW()`.
//!
//! Construction is inert. The host supplies independent data/control pools for
//! the same server, database and role, through direct or session-pinned routes.
//! It also owns search_path, deadlines, startup, and shutdown. The single INSERT
//! is one supervised commit, never combined with the preceding configuration
//! mutation. As in other native adapters, supervision starts BEGIN one round
//! trip before the source autocommit statement, which shifts transaction NOW().
//! Cancellation cannot undo COMMIT already dispatched. Abort and join callers
//! before awaiting `drain_cancelled`; keep both pools open until drain finishes.

use crate::{
    config_audit_writer::{
        CONFIG_AUDIT_INSERT_SQL, ConfigAuditError, ConfigAuditFuture, ConfigAuditPool,
        ConfigAuditStatement,
    },
    postgres_operation::{PgOperationError, PgOperationStorage},
};
use serde_json::Value;
use sqlx::PgPool;
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigAuditPostgresStage {
    Acquire,
    Execute,
    Drain,
}

/// Only stage and SQLSTATE survive a backend error. Neither Display nor Debug
/// exposes SQL, server messages, connection details, identifiers, or payloads.
#[derive(Clone, PartialEq, Eq)]
pub enum ConfigAuditPostgresError {
    Database {
        stage: ConfigAuditPostgresStage,
        sqlstate: Option<String>,
    },
    InvalidStatement,
    Cancelled,
    InvalidCancellationPool,
    IndeterminateCancellation,
}
impl ConfigAuditPostgresError {
    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            Self::Database { sqlstate, .. } => sqlstate.as_deref(),
            _ => None,
        }
    }
    pub fn stage(&self) -> Option<ConfigAuditPostgresStage> {
        match self {
            Self::Database { stage, .. } => Some(*stage),
            _ => None,
        }
    }
    fn operation(error: PgOperationError, stage: ConfigAuditPostgresStage) -> Self {
        match error {
            PgOperationError::Database { sqlstate } => Self::Database { stage, sqlstate },
            PgOperationError::Cancelled => Self::Cancelled,
            PgOperationError::InvalidCancellationPool => Self::InvalidCancellationPool,
            PgOperationError::IndeterminateCancellation => Self::IndeterminateCancellation,
        }
    }
    fn writer_error(self) -> ConfigAuditError {
        match self {
            Self::Cancelled => ConfigAuditError::Cancelled,
            Self::Database {
                stage: ConfigAuditPostgresStage::Acquire,
                ..
            }
            | Self::InvalidCancellationPool => ConfigAuditError::PoolAcquire,
            Self::Database {
                stage: ConfigAuditPostgresStage::Drain,
                ..
            }
            | Self::IndeterminateCancellation => ConfigAuditError::PoolRelease,
            Self::Database {
                stage: ConfigAuditPostgresStage::Execute,
                ..
            }
            | Self::InvalidStatement => ConfigAuditError::PoolExecute,
        }
    }
}
impl fmt::Display for ConfigAuditPostgresError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database { .. } => "config_audit_postgres_unavailable",
            Self::InvalidStatement => "config_audit_postgres_invalid_statement",
            Self::Cancelled => "config_audit_postgres_cancelled",
            Self::InvalidCancellationPool => {
                "config_audit_postgres_independent_control_pool_required"
            }
            Self::IndeterminateCancellation => "config_audit_postgres_cancellation_indeterminate",
        })
    }
}
impl fmt::Debug for ConfigAuditPostgresError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for ConfigAuditPostgresError {}

// Deliberately not Debug or Serialize: these values can include private data.
struct Arguments {
    event_type: String,
    user_id: Option<String>,
    tenant_id: String,
    ip_address: String,
    user_agent: String,
    resource_type: String,
    resource_id: String,
    action: String,
    request_summary: String,
    response_summary: String,
    status: String,
    error_message: Option<String>,
    duration_ms: Option<i32>,
}
impl Arguments {
    fn from_statement(statement: &ConfigAuditStatement) -> Result<Self, ConfigAuditPostgresError> {
        if statement.sql != CONFIG_AUDIT_INSERT_SQL || statement.arguments.len() != 13 {
            return Err(ConfigAuditPostgresError::InvalidStatement);
        }
        let arguments = &statement.arguments;
        let text = |index: usize| {
            arguments[index]
                .as_str()
                .map(str::to_owned)
                .ok_or(ConfigAuditPostgresError::InvalidStatement)
        };
        let optional_text = |index: usize| {
            if arguments[index].is_null() {
                Ok(None)
            } else {
                text(index).map(Some)
            }
        };
        let duration_ms = match &arguments[12] {
            Value::Null => None,
            value => Some(
                value
                    .as_i64()
                    .and_then(|value| i32::try_from(value).ok())
                    .ok_or(ConfigAuditPostgresError::InvalidStatement)?,
            ),
        };
        Ok(Self {
            event_type: text(0)?,
            user_id: optional_text(1)?,
            tenant_id: text(2)?,
            ip_address: text(3)?,
            user_agent: text(4)?,
            resource_type: text(5)?,
            resource_id: text(6)?,
            action: text(7)?,
            request_summary: text(8)?,
            response_summary: text(9)?,
            status: text(10)?,
            error_message: optional_text(11)?,
            duration_ms,
        })
    }
}

#[derive(Clone)]
pub struct PgConfigAuditPool {
    operations: PgOperationStorage,
}
impl PgConfigAuditPool {
    /// No connections, tasks, environment reads, or writes occur here.
    pub fn new(pool: PgPool, control_pool: PgPool) -> Result<Self, ConfigAuditPostgresError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool, control_pool).map_err(|error| {
                ConfigAuditPostgresError::operation(error, ConfigAuditPostgresStage::Acquire)
            })?,
        })
    }

    /// Native diagnostic boundary for the same statement accepted by the trait.
    /// Values are bound, never interpolated. SQLSTATE is retained only here;
    /// the established ConfigAuditPool trait retains its original error enum.
    pub async fn execute_statement(
        &self,
        statement: &ConfigAuditStatement,
    ) -> Result<(), ConfigAuditPostgresError> {
        let arguments = Arguments::from_statement(statement)?;
        let mut connection = self.operations.acquire().await.map_err(|error| {
            ConfigAuditPostgresError::operation(error, ConfigAuditPostgresStage::Acquire)
        })?;
        connection
            .execute(move |connection| {
                Box::pin(async move {
                    sqlx::query(CONFIG_AUDIT_INSERT_SQL)
                        .bind(arguments.event_type)
                        .bind(arguments.user_id)
                        .bind(arguments.tenant_id)
                        .bind(arguments.ip_address)
                        .bind(arguments.user_agent)
                        .bind(arguments.resource_type)
                        .bind(arguments.resource_id)
                        .bind(arguments.action)
                        .bind(arguments.request_summary)
                        .bind(arguments.response_summary)
                        .bind(arguments.status)
                        .bind(arguments.error_message)
                        .bind(arguments.duration_ms)
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .await
            .map_err(|error| {
                ConfigAuditPostgresError::operation(error, ConfigAuditPostgresStage::Execute)
            })
    }

    /// Stop producing calls, abort and join outstanding callers, then await this
    /// before closing either pool. This drains cancelled operations; it is not
    /// a replacement for joining healthy callers and does not stop future calls.
    pub async fn drain_cancelled(&self) -> Result<(), ConfigAuditPostgresError> {
        self.operations.drain_cancelled().await.map_err(|error| {
            ConfigAuditPostgresError::operation(error, ConfigAuditPostgresStage::Drain)
        })
    }
}
impl ConfigAuditPool for PgConfigAuditPool {
    fn execute_in_acquired_connection<'a>(
        &'a self,
        statement: &'a ConfigAuditStatement,
    ) -> ConfigAuditFuture<'a> {
        Box::pin(async move {
            self.execute_statement(statement)
                .await
                .map_err(ConfigAuditPostgresError::writer_error)
        })
    }
}

