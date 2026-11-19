//! Redacted operation failure contract; no pools or statement supervision.
use std::fmt;
/// Retain only the machine-readable SQLSTATE, never backend text, SQL or values.
#[derive(Clone, PartialEq, Eq)]
pub enum PgOperationError {
    Database { sqlstate: Option<String> },
    Cancelled,
    InvalidCancellationPool,
    IndeterminateCancellation,
}
impl PgOperationError {
    #[doc(hidden)]
    pub fn database() -> Self {
        Self::Database { sqlstate: None }
    }
    pub fn sqlstate(&self) -> Option<&str> {
        match self {
            Self::Database { sqlstate } => sqlstate.as_deref(),
            _ => None,
        }
    }
}
impl From<sqlx::Error> for PgOperationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database {
            sqlstate: error
                .as_database_error()
                .and_then(|error| error.code())
                .map(|code| code.into_owned()),
        }
    }
}
impl fmt::Display for PgOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database { .. } => "postgres_operation_unavailable",
            Self::Cancelled => "postgres_operation_cancelled",
            Self::InvalidCancellationPool => "postgres_independent_control_pool_required",
            Self::IndeterminateCancellation => "postgres_cancellation_indeterminate",
        })
    }
}
impl fmt::Debug for PgOperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for PgOperationError {}
