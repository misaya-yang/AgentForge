//! Shared owner-scoped assistant preference helpers. No runtime or pool is started.
//!
//! Value parity is scoped to the existing native finite JSON number model and
//! serde's depth-128 decoder boundary. Python can retain integers beyond u64/i64
//! exactly and decode deeper values; this module does not claim that domain.
//! In particular, a serde depth failure falls back to raw text here, unlike a
//! Python RecursionError (which escapes). The package does not enable global
//! arbitrary_precision or unbounded recursive parsing to change frozen modules.
use serde_json::Value;
use std::{fmt, future::Future, pin::Pin, sync::Arc};

pub const MEMORY_CONTROL_KEY: &str = "__assistant_memory_control__";
pub const MEMORY_EFFECT: &str =
    "New runs only; off disables both long-term and session memory for that run";
pub type AssistantMemoryFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, AssistantMemoryError>> + Send + 'a>>;

/// Trusted resolver output. Caller-supplied ownership headers are not identities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssistantMemoryIdentity {
    pub tenant_id: String,
    pub user_id: String,
    pub is_authenticated: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub enum AssistantMemoryError {
    Http { status: u16, detail: Value },
    Storage,
    Cancelled,
    Internal,
}
impl AssistantMemoryError {
    pub fn detail(status: u16, detail: impl Into<String>) -> Self {
        Self::Http {
            status,
            detail: Value::String(detail.into()),
        }
    }
}
impl fmt::Display for AssistantMemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Http { .. } => "assistant_memory_http_error",
            Self::Storage => "assistant_memory_storage_unavailable",
            Self::Cancelled => "assistant_memory_cancelled",
            Self::Internal => "assistant_memory_internal_error",
        })
    }
}
impl std::error::Error for AssistantMemoryError {}

/// Each method is one separately committed source database operation. In
/// particular, a failing readback or access-count update cannot undo an upsert.
pub trait AssistantMemoryStore: Send + Sync {
    /// Source-projected rows (including one JSON parse), newest first, limit 101.
    fn list_active<'a>(
        &'a self,
        owner: &'a AssistantMemoryIdentity,
    ) -> AssistantMemoryFuture<'a, Vec<Value>>;
    /// Raw source-driver value. This lookup deliberately ignores expiry.
    fn read_control<'a>(
        &'a self,
        owner: &'a AssistantMemoryIdentity,
    ) -> AssistantMemoryFuture<'a, Option<Value>>;
    fn upsert<'a>(
        &'a self,
        owner: &'a AssistantMemoryIdentity,
        key: &'a str,
        value: Value,
        control: bool,
    ) -> AssistantMemoryFuture<'a, ()>;
    /// Raw source-driver value; None is no active row, Some(Null) is a JSON null.
    fn read_active<'a>(
        &'a self,
        owner: &'a AssistantMemoryIdentity,
        key: &'a str,
    ) -> AssistantMemoryFuture<'a, Option<Value>>;
    fn increment_access<'a>(
        &'a self,
        owner: &'a AssistantMemoryIdentity,
        key: &'a str,
    ) -> AssistantMemoryFuture<'a, ()>;
    fn delete<'a>(
        &'a self,
        owner: &'a AssistantMemoryIdentity,
        key: &'a str,
    ) -> AssistantMemoryFuture<'a, ()>;
    fn drain_cancelled(&self) -> AssistantMemoryFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Exact source `_value`: parse at most once. A stored JSON string containing
/// JSON-looking text must remain text when the default asyncpg driver is used.
pub fn source_value(raw: Value) -> Value {
    match raw {
        Value::String(text) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
        other => other,
    }
}
pub fn memory_database(
    store: Option<&Arc<dyn AssistantMemoryStore>>,
) -> Result<&dyn AssistantMemoryStore, AssistantMemoryError> {
    store
        .map(Arc::as_ref)
        .ok_or_else(|| AssistantMemoryError::detail(503, "Memory storage is unavailable"))
}
pub fn memory_enabled<'a>(
    store: Option<&'a Arc<dyn AssistantMemoryStore>>,
    owner: &'a AssistantMemoryIdentity,
) -> AssistantMemoryFuture<'a, bool> {
    Box::pin(async move {
        let database = memory_database(store)?;
        let row = database
            .read_control(owner)
            .await
            .map_err(|error| match error {
                AssistantMemoryError::Http { .. } => error,
                AssistantMemoryError::Cancelled => AssistantMemoryError::Cancelled,
                _ => AssistantMemoryError::detail(503, "Memory preference is unavailable"),
            })?;
        Ok(match row {
            None => true,
            Some(raw) => source_value(raw) == Value::Bool(true),
        })
    })
}
pub fn effective_assistant_memory_mode<'a>(
    store: Option<&'a Arc<dyn AssistantMemoryStore>>,
    owner: &'a AssistantMemoryIdentity,
    requested: &'a str,
) -> AssistantMemoryFuture<'a, String> {
    Box::pin(async move {
        Ok(if memory_enabled(store, owner).await? {
            requested
        } else {
            "off"
        }
        .to_owned())
    })
}

/// Optional host-owned sink for the source MemoryService's sanitized failure
/// events. It receives only fixed event names, never SQL, owner data or errors.
/// Implementations must not panic; panics are not caught or converted to success.
pub trait AssistantMemoryFailureObserver: Send + Sync {
    fn record(&self, event: &'static str);
}
pub const MEMORY_SET_FAILURE_EVENT: &str = "assistant.memory.set_user.internal_failure";
pub const MEMORY_DELETE_FAILURE_EVENT: &str = "assistant.memory.delete_user.internal_failure";
