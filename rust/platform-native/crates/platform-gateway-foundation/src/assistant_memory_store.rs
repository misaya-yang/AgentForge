//! Concrete PostgreSQL closure of the source assistant memory controls.
//!
//! Construction is inert. The host owns independent data/control pools reaching
//! the same server, database and role with direct or session-pinned connections.
//! Its verified search_path must match Python's unqualified `user_memory` table
//! and that table must have tenant-qualified uniqueness. No fallback to an
//! unscoped conflict target or alternate schema is permitted.
//!
//! Every trait method is one supervised source autocommit operation. Upsert,
//! readback and access-statistics updates intentionally commit independently:
//! readback/stats failure must not roll back an earlier successful upsert.
//! The supervisor's BEGIN establishes NOW one round-trip before the source
//! statement; cancellation cannot undo an already-dispatched COMMIT. The host
//! must drain cancelled callers before shutting down its independent control
//! pool. Query deadlines remain host-owned.
//!
//! Default asyncpg decodes JSONB as text. Readback/control therefore expose a
//! Value::String holding raw JSON text, while list projection applies exactly
//! one source JSON parse. Custom source JSON codecs need a reviewed adapter.

use crate::{
    assistant_memory_types::{
        AssistantMemoryError, AssistantMemoryFailureObserver, AssistantMemoryFuture,
        AssistantMemoryIdentity, AssistantMemoryStore, MEMORY_CONTROL_KEY,
        MEMORY_DELETE_FAILURE_EVENT, MEMORY_SET_FAILURE_EVENT, source_value,
    },
    postgres_operation::{PgOperationError, PgOperationStorage},
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct PgAssistantMemoryStore {
    operations: PgOperationStorage,
    failure_observer: Option<Arc<dyn AssistantMemoryFailureObserver>>,
}

fn pg_error(error: PgOperationError) -> AssistantMemoryError {
    match error {
        PgOperationError::Cancelled => AssistantMemoryError::Cancelled,
        _ => AssistantMemoryError::Storage,
    }
}

// Python datetime.isoformat() has exactly six fractional digits when its
// microseconds are nonzero. asyncpg returns timestamptz in UTC even when the
// PostgreSQL session uses another timezone.
fn python_datetime(value: DateTime<Utc>) -> String {
    if value.timestamp_subsec_micros() == 0 {
        value.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    } else {
        value.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
    }
}

#[derive(sqlx::FromRow)]
struct MemoryItemRow {
    key: String,
    value: String,
    namespace: String,
    source: String,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
}

impl MemoryItemRow {
    fn source_projection(self) -> Value {
        json!({
            "key": self.key,
            "value": source_value(Value::String(self.value)),
            "namespace": self.namespace,
            "source": self.source,
            "created_at": self.created_at.map(python_datetime),
            "updated_at": self.updated_at.map(python_datetime),
            "expires_at": self.expires_at.map(python_datetime),
        })
    }
}

impl PgAssistantMemoryStore {
    pub fn new(
        pool: PgPool,
        independent_control_pool: PgPool,
    ) -> Result<Self, AssistantMemoryError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool, independent_control_pool)
                .map_err(pg_error)?,
            failure_observer: None,
        })
    }

    /// Optional host-owned, non-panicking sink for fixed source failure events.
    /// No callback runs during construction; SQL, values and errors are withheld.
    pub fn with_failure_observer(
        mut self,
        observer: Arc<dyn AssistantMemoryFailureObserver>,
    ) -> Self {
        self.failure_observer = Some(observer);
        self
    }

    fn mutation_result(
        &self,
        event: &'static str,
        result: Result<(), AssistantMemoryError>,
    ) -> Result<(), AssistantMemoryError> {
        if let Err(error) = &result
            && !matches!(error, AssistantMemoryError::Cancelled)
            && let Some(observer) = &self.failure_observer
        {
            observer.record(event);
        }
        result
    }

    /// Await after aborting callers, before closing the independent control pool.
    pub fn drain_cancelled(&self) -> AssistantMemoryFuture<'_, ()> {
        Box::pin(async move { self.operations.drain_cancelled().await.map_err(pg_error) })
    }
}

impl AssistantMemoryStore for PgAssistantMemoryStore {
    fn list_active<'a>(
        &'a self,
        owner: &'a AssistantMemoryIdentity,
    ) -> AssistantMemoryFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let owner = owner.clone();
            let rows: Vec<MemoryItemRow> = self.operations.acquire().await.map_err(pg_error)?
                .execute(move |connection| Box::pin(async move {
                    sqlx::query_as(
                        "SELECT key, value::text AS value, namespace, source, created_at, updated_at, expires_at \
                         FROM user_memory WHERE tenant_id = $1 AND user_id = $2 \
                         AND key <> $3 AND (expires_at IS NULL OR expires_at > NOW()) \
                         ORDER BY updated_at DESC LIMIT 101",
                    )
                    .bind(owner.tenant_id)
                    .bind(owner.user_id)
                    .bind(MEMORY_CONTROL_KEY)
                    .fetch_all(connection)
                    .await
                }))
                .await.map_err(pg_error)?;
            Ok(rows
                .into_iter()
                .map(MemoryItemRow::source_projection)
                .collect())
        })
    }

    fn read_control<'a>(
        &'a self,
        owner: &'a AssistantMemoryIdentity,
    ) -> AssistantMemoryFuture<'a, Option<Value>> {
        Box::pin(async move {
            let owner = owner.clone();
            let value: Option<String> = self.operations.acquire().await.map_err(pg_error)?
                .execute(move |connection| Box::pin(async move {
                    sqlx::query_scalar(
                        "SELECT value::text FROM user_memory WHERE tenant_id = $1 AND user_id = $2 AND key = $3",
                    )
                    .bind(owner.tenant_id)
                    .bind(owner.user_id)
                    .bind(MEMORY_CONTROL_KEY)
                    .fetch_optional(connection)
                    .await
                }))
                .await.map_err(pg_error)?;
            Ok(value.map(Value::String))
        })
    }

    fn upsert<'a>(
        &'a self,
        owner: &'a AssistantMemoryIdentity,
        key: &'a str,
        value: Value,
        control: bool,
    ) -> AssistantMemoryFuture<'a, ()> {
        Box::pin(async move {
            let owner = owner.clone();
            let key = key.to_owned();
            let default_namespace = if control {
                "assistant-control"
            } else {
                "default"
            };
            let namespace = key
                .split_once('/')
                .map(|(prefix, _)| prefix)
                .filter(|prefix| !prefix.is_empty())
                .unwrap_or(default_namespace)
                .to_owned();
            let source = if control {
                "user-preference"
            } else {
                "user-edit"
            };
            let metadata =
                json!({"namespace": namespace, "sensitivity": "normal", "source": source});
            let result = async {
                self.operations.acquire().await.map_err(pg_error)?
                .execute(move |connection| Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO user_memory (tenant_id, user_id, key, value, metadata, \
                         namespace, expires_at, sensitivity, source, updated_at) \
                         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW()) \
                         ON CONFLICT (tenant_id, user_id, key) DO UPDATE SET \
                         value = EXCLUDED.value, \
                         metadata = COALESCE(EXCLUDED.metadata, user_memory.metadata), \
                         namespace = EXCLUDED.namespace, \
                         expires_at = COALESCE(EXCLUDED.expires_at, user_memory.expires_at), \
                         sensitivity = EXCLUDED.sensitivity, source = EXCLUDED.source, updated_at = NOW()",
                    )
                    .bind(owner.tenant_id)
                    .bind(owner.user_id)
                    .bind(key)
                    .bind(value)
                    .bind(metadata)
                    .bind(namespace)
                    .bind(None::<DateTime<Utc>>)
                    .bind("normal")
                    .bind(source)
                    .execute(connection)
                    .await?;
                    Ok(())
                }))
                .await.map_err(pg_error)
            }.await;
            self.mutation_result(MEMORY_SET_FAILURE_EVENT, result)
        })
    }

    fn read_active<'a>(
        &'a self,
        owner: &'a AssistantMemoryIdentity,
        key: &'a str,
    ) -> AssistantMemoryFuture<'a, Option<Value>> {
        Box::pin(async move {
            let owner = owner.clone();
            let key = key.to_owned();
            let value: Option<String> = self
                .operations
                .acquire()
                .await
                .map_err(pg_error)?
                .execute(move |connection| {
                    Box::pin(async move {
                        sqlx::query_scalar(
                            "SELECT value::text FROM user_memory \
                         WHERE tenant_id = $1 AND user_id = $2 AND key = $3 \
                         AND (expires_at IS NULL OR expires_at > NOW())",
                        )
                        .bind(owner.tenant_id)
                        .bind(owner.user_id)
                        .bind(key)
                        .fetch_optional(connection)
                        .await
                    })
                })
                .await
                .map_err(pg_error)?;
            Ok(value.map(Value::String))
        })
    }

    fn increment_access<'a>(
        &'a self,
        owner: &'a AssistantMemoryIdentity,
        key: &'a str,
    ) -> AssistantMemoryFuture<'a, ()> {
        Box::pin(async move {
            let owner = owner.clone();
            let key = key.to_owned();
            self.operations.acquire().await.map_err(pg_error)?
                .execute(move |connection| Box::pin(async move {
                    sqlx::query(
                        "UPDATE user_memory SET access_count = access_count + 1, last_accessed_at = NOW() \
                         WHERE tenant_id = $1 AND user_id = $2 AND key = $3",
                    )
                    .bind(owner.tenant_id)
                    .bind(owner.user_id)
                    .bind(key)
                    .execute(connection)
                    .await?;
                    Ok(())
                }))
                .await.map_err(pg_error)
        })
    }

    fn delete<'a>(
        &'a self,
        owner: &'a AssistantMemoryIdentity,
        key: &'a str,
    ) -> AssistantMemoryFuture<'a, ()> {
        Box::pin(async move {
            let owner = owner.clone();
            let key = key.to_owned();
            let result = async {
                self.operations.acquire().await.map_err(pg_error)?
                .execute(move |connection| Box::pin(async move {
                    sqlx::query(
                        "DELETE FROM user_memory WHERE tenant_id = $1 AND user_id = $2 AND key = $3",
                    )
                    .bind(owner.tenant_id)
                    .bind(owner.user_id)
                    .bind(key)
                    .execute(connection)
                    .await?;
                    // Missing rows are successful, exactly like the source.
                    Ok(())
                }))
                .await.map_err(pg_error)
            }.await;
            self.mutation_result(MEMORY_DELETE_FAILURE_EVENT, result)
        })
    }

    fn drain_cancelled(&self) -> AssistantMemoryFuture<'_, ()> {
        PgAssistantMemoryStore::drain_cancelled(self)
    }
}
