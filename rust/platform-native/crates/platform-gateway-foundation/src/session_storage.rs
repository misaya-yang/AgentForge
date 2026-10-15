//! PostgreSQL implementation of the legacy `SessionStorage` seam.
//!
//! SQL is migrated from `ai_gateway_core.persistence.database.DatabaseStorage`,
//! not the older `SessionRepository` (whose owner checks and expiry filtering
//! differ). Production queries always target `assistant.sessions`; this adapter
//! neither creates a schema nor opens a connection, runs maintenance or mounts
//! HTTP routes. ID-only operations require an owner-authorized internal caller.
//! `DatabaseSessionManager::get_or_create` checks tenant, user and service.
//!
//! Atomic insert, append, owner-scoped upsert, tombstone fallback and expiry-only
//! updates retain the source row shapes. Epoch14 adds a durable lifecycle fence
//! around owned create/save so deletion cannot race explicit-ID recreation.
//! Two deliberate typed/fail-closed differences:
//! an owner-conflicting upsert returns `PermissionDenied` rather than reporting a
//! successful no-op that could seed the manager's cache; JSONB state is decoded
//! as JSON rather than leaking asyncpg's raw string through Python annotations.
//! Each mutation has an owned transaction supervisor: dropping the caller signals
//! server cancellation and rollback while retaining the exact backend until drain.
//! COMMIT already sent can win cancellation; no post-COMMIT rollback is promised.
//! NOW() uses the explicit BEGIN timestamp, slightly earlier than Python's implicit
//! statement transaction. Ordinary query timeouts remain host pool/server policy.
//! Storage failures are redacted and never reported as successful persistence.
//! Expiry SQL never explicitly changes history or `updated_at`; the canonical
//! schema's unconditional BEFORE UPDATE trigger can still advance `updated_at`.
//! This adapter preserves that trigger behavior and does not change the schema.
//!
//! No Redis cache, full-record list API or immutable Agent runtime binding is
//! implemented here. Save/create intentionally write only the ten legacy columns,
//! exactly as the source does; runtime dimensions on existing rows are retained.
use crate::postgres_operation::{PgOperationError, PgOperationStorage};
use crate::session_manager::{
    SessionError, SessionFuture, SessionMessage, SessionStorage, SessionSummaryQuery,
    SessionTimestamp,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder, Row};

const TABLE: &str = "assistant.sessions";
const SAVE: &str = r#"
INSERT INTO assistant.sessions (
    session_id, service_id, user_id, tenant_id,
    state, history, metadata, config, status, expires_at
) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
ON CONFLICT (session_id) DO UPDATE SET
    service_id = EXCLUDED.service_id,
    state = EXCLUDED.state,
    history = EXCLUDED.history,
    metadata = EXCLUDED.metadata,
    config = EXCLUDED.config,
    status = EXCLUDED.status,
    expires_at = EXCLUDED.expires_at,
    updated_at = NOW()
WHERE assistant.sessions.user_id = EXCLUDED.user_id
  AND assistant.sessions.tenant_id IS NOT DISTINCT FROM EXCLUDED.tenant_id
"#;
const CREATE: &str = r#"
INSERT INTO assistant.sessions (
    session_id, service_id, user_id, tenant_id,
    state, history, metadata, config, status, expires_at
) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
ON CONFLICT DO NOTHING
RETURNING session_id
"#;
// JSON projection includes existing/future runtime dimensions without requiring
// a parallel schema model. Typed timestamps below preserve asyncpg UTC formatting.
const GET: &str = "SELECT to_jsonb(s) AS document, created_at, updated_at, expires_at FROM assistant.sessions s WHERE session_id = $1 AND status <> 'deleted'";
const APPEND: &str = r#"
UPDATE assistant.sessions
SET history = history || $2::jsonb,
    updated_at = NOW()
WHERE session_id = $1
"#;
const APPEND_METADATA: &str = r#"
UPDATE assistant.sessions
SET history = history || $2::jsonb,
    metadata = metadata || $3::jsonb,
    updated_at = NOW()
WHERE session_id = $1
"#;
const PATCH_METADATA: &str = "UPDATE assistant.sessions SET metadata = COALESCE(metadata, '{}'::jsonb) || $2::jsonb, updated_at = NOW() WHERE session_id = $1";
const PATCH_CONFIG: &str = "UPDATE assistant.sessions SET config = COALESCE(config, '{}'::jsonb) || $2::jsonb, updated_at = NOW() WHERE session_id = $1";
const DELETE: &str = "DELETE FROM assistant.sessions WHERE session_id = $1";
const TOMBSTONE: &str = "UPDATE assistant.sessions SET status = 'deleted', updated_at = NOW() WHERE session_id = $1 AND status <> 'deleted'";
const EXTEND: &str = "UPDATE assistant.sessions SET expires_at = $1 WHERE session_id = $2";

/// Shared durable lifecycle fence. Call inside an owned transaction, before the
/// canonical session row lock. Existing canonical owner conflicts are checked
/// before barrier mutation; caller repeats its owner predicate on the actual DML.
/// Nullable legacy scopes have no authenticated SDK/deletion-barrier identity.
pub async fn lock_session_barrier(connection: &mut sqlx::PgConnection, tenant: Option<&str>, user: Option<&str>, session: &str) -> Result<bool, sqlx::Error> {
    let (Some(tenant), Some(user)) = (tenant.filter(|v| !v.is_empty()), user.filter(|v| !v.is_empty())) else { return Ok(true); };
    let foreign: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM assistant.sessions WHERE session_id=$3 AND (tenant_id IS DISTINCT FROM $1 OR user_id IS DISTINCT FROM $2))")
        .bind(tenant).bind(user).bind(session).fetch_one(&mut *connection).await?;
    if foreign { return Ok(false); }
    sqlx::query("INSERT INTO gateway.sdk_session_barriers(tenant_id,user_id,session_id) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
        .bind(tenant).bind(user).bind(session).execute(&mut *connection).await?;
    let deleting: bool = sqlx::query_scalar("SELECT deleting FROM gateway.sdk_session_barriers WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 FOR UPDATE")
        .bind(tenant).bind(user).bind(session).fetch_one(&mut *connection).await?;
    Ok(!deleting)
}

#[derive(Clone)]
pub struct PgSessionStorage {
    pool: PgPool,
    operations: PgOperationStorage,
    // Production construction always uses the canonical table. A private test
    // constructor substitutes only a generated, validated isolated fixture name.
    table: String,
}
impl PgSessionStorage {
    /// Inert construction: independent pools must target the same database and
    /// role over direct or session-pinned connections. Keep both pools alive
    /// through cancellation drain; transaction-pooling proxies are unsupported.
    pub fn new(pool: PgPool, control_pool: PgPool) -> Result<Self, SessionError> {
        let operations =
            PgOperationStorage::new(pool.clone(), control_pool).map_err(operation_error)?;
        Ok(Self {
            pool,
            operations,
            table: TABLE.into(),
        })
    }

    /// After aborting and awaiting mutation callers, wait for server cancellation
    /// and transaction cleanup before shutdown or releasing external resources.
    /// Drain errors mean cleanup could not be established, never rollback proof.
    pub async fn drain_cancelled(&self) -> Result<(), SessionError> {
        self.operations
            .drain_cancelled()
            .await
            .map_err(operation_error)
    }

    async fn merge_map(
        &self,
        id: &str,
        patch: Map<String, Value>,
        config: bool,
    ) -> Result<bool, SessionError> {
        if patch.is_empty() {
            return Ok(true);
        }
        let sql = self.sql(if config { PATCH_CONFIG } else { PATCH_METADATA });
        let id = id.to_owned();
        let mut connection = self.operations.acquire().await.map_err(operation_error)?;
        let result = connection
            .execute(move |connection| {
                Box::pin(async move {
                    sqlx::query(&sql)
                        .bind(id)
                        .bind(Value::Object(patch))
                        .execute(connection)
                        .await
                })
            })
            .await
            .map_err(operation_error)?;
        Ok(result.rows_affected() == 1)
    }
    fn sql(&self, statement: &str) -> String {
        statement.replace(TABLE, &self.table)
    }
}

fn object() -> Value {
    json!({})
}
fn array() -> Value {
    json!([])
}
fn active() -> Option<String> {
    Some("active".into())
}
/// Missing fields receive the same defaults as dict.get; explicit JSON null is
/// kept distinct from omission. Runtime and created/updated fields are ignored.
#[derive(Deserialize)]
struct WriteSession {
    session_id: String,
    service_id: Option<String>,
    user_id: Option<String>,
    tenant_id: Option<String>,
    #[serde(default = "object")]
    state: Value,
    #[serde(default = "array")]
    history: Value,
    #[serde(default = "object")]
    metadata: Value,
    #[serde(default = "object")]
    config: Value,
    #[serde(default = "active")]
    status: Option<String>,
    expires_at: Option<SessionTimestamp>,
}
impl WriteSession {
    fn parse(value: Value) -> Result<Self, SessionError> {
        serde_json::from_value(value)
            .map_err(|_| SessionError::InvalidData("invalid_session_write"))
    }
}

fn utc(time: SessionTimestamp) -> DateTime<Utc> {
    match time {
        SessionTimestamp::Naive(time) => time.and_utc(),
        SessionTimestamp::Aware(time) => time.with_timezone(&Utc),
    }
}
fn timestamp(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(
        if time.timestamp_subsec_micros() == 0 {
            chrono::SecondsFormat::Secs
        } else {
            chrono::SecondsFormat::Micros
        },
        false,
    )
}
fn unavailable(_: sqlx::Error) -> SessionError {
    // Never expose SQL, row content, connection settings or backend diagnostics.
    SessionError::StorageUnavailable
}
fn operation_error(error: PgOperationError) -> SessionError {
    match error {
        PgOperationError::InvalidCancellationPool => {
            SessionError::InvalidConfig("invalid_session_cancellation_pool")
        }
        _ => SessionError::StorageUnavailable,
    }
}

#[derive(FromRow)]
struct SessionRow {
    document: Value,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
}
impl SessionRow {
    fn into_value(mut self) -> Result<Value, SessionError> {
        let document = self
            .document
            .as_object_mut()
            .ok_or(SessionError::InvalidData("invalid_session_row"))?;
        for (key, time) in [
            ("created_at", self.created_at),
            ("updated_at", self.updated_at),
            ("expires_at", self.expires_at),
        ] {
            document.insert(
                key.into(),
                time.map(timestamp).map_or(Value::Null, Value::String),
            );
        }
        Ok(self.document)
    }
}

impl SessionStorage for PgSessionStorage {
    fn list_session_summaries(&self, query: SessionSummaryQuery) -> SessionFuture<'_, Vec<Value>> {
        Box::pin(async move {
            // Exact source projection: no history/state/config and no expiry filter.
            let mut sql = QueryBuilder::<Postgres>::new(self.sql("SELECT session_id, service_id, user_id, tenant_id, jsonb_build_object('title', metadata->'title', 'langgraph_thread_id', metadata->'langgraph_thread_id', 'folder', metadata->'folder', 'pinned', metadata->'pinned') as metadata, status, expires_at, created_at, updated_at FROM assistant.sessions WHERE 1=1"));
            if let Some(user) = query.user_id.filter(|value| !value.is_empty()) {
                sql.push(" AND user_id = ").push_bind(user);
            }
            if let Some(tenant) = query.tenant_id.filter(|value| !value.is_empty()) {
                sql.push(" AND tenant_id = ").push_bind(tenant);
            }
            if !query.service_ids.is_empty() {
                sql.push(if query.include_null_service_id {
                    " AND (service_id = ANY("
                } else {
                    " AND service_id = ANY("
                });
                sql.push_bind(query.service_ids).push("::text[])");
                if query.include_null_service_id {
                    sql.push(" OR service_id IS NULL OR service_id = '')");
                }
            }
            if !query.status.is_empty() {
                sql.push(" AND status = ").push_bind(query.status);
            }
            sql.push(" ORDER BY updated_at DESC LIMIT ")
                .push_bind(query.limit);
            let rows = sql
                .build()
                .fetch_all(&self.pool)
                .await
                .map_err(unavailable)?;
            rows.into_iter().map(|row| {
                let created: Option<DateTime<Utc>> = row.try_get("created_at").map_err(unavailable)?;
                let updated: Option<DateTime<Utc>> = row.try_get("updated_at").map_err(unavailable)?;
                let expires: Option<DateTime<Utc>> = row.try_get("expires_at").map_err(unavailable)?;
                Ok(json!({
                    "session_id": row.try_get::<String,_>("session_id").map_err(unavailable)?,
                    "service_id": row.try_get::<Option<String>,_>("service_id").map_err(unavailable)?,
                    "user_id": row.try_get::<Option<String>,_>("user_id").map_err(unavailable)?,
                    "tenant_id": row.try_get::<Option<String>,_>("tenant_id").map_err(unavailable)?,
                    "metadata": row.try_get::<Value,_>("metadata").map_err(unavailable)?,
                    "status": row.try_get::<Option<String>,_>("status").map_err(unavailable)?,
                    "created_at": created.map(timestamp), "updated_at": updated.map(timestamp),
                    "expires_at": expires.map(timestamp)
                }))
            }).collect()
        })
    }
    fn update_metadata<'a>(
        &'a self,
        id: &'a str,
        patch: Map<String, Value>,
    ) -> SessionFuture<'a, bool> {
        Box::pin(async move { self.merge_map(id, patch, false).await })
    }
    fn update_config<'a>(
        &'a self,
        id: &'a str,
        patch: Map<String, Value>,
    ) -> SessionFuture<'a, bool> {
        Box::pin(async move { self.merge_map(id, patch, true).await })
    }
    fn get<'a>(&'a self, id: &'a str) -> SessionFuture<'a, Option<Value>> {
        Box::pin(async move {
            sqlx::query_as::<_, SessionRow>(&self.sql(GET))
                .bind(id)
                .fetch_optional(&self.pool)
                .await
                .map_err(unavailable)?
                .map(SessionRow::into_value)
                .transpose()
        })
    }

    fn save(&self, session: Value) -> SessionFuture<'_, ()> {
        Box::pin(async move {
            let row = WriteSession::parse(session)?;
            let sql = self.sql(SAVE);
            let mut connection = self.operations.acquire().await.map_err(operation_error)?;
            let result = connection
                .execute(move |connection| {
                    Box::pin(async move {
                        sqlx::query("SAVEPOINT session_lifecycle_write").execute(&mut *connection).await?;
                        if !lock_session_barrier(&mut *connection,row.tenant_id.as_deref(),row.user_id.as_deref(),&row.session_id).await? {
                            sqlx::query("ROLLBACK TO SAVEPOINT session_lifecycle_write").execute(&mut *connection).await?;
                            return Ok(None);
                        }
                        let result = sqlx::query(&sql)
                            .bind(row.session_id)
                            .bind(row.service_id)
                            .bind(row.user_id)
                            .bind(row.tenant_id)
                            .bind(row.state)
                            .bind(row.history)
                            .bind(row.metadata)
                            .bind(row.config)
                            .bind(row.status)
                            .bind(row.expires_at.map(utc))
                            .execute(&mut *connection)
                            .await?;
                        if result.rows_affected() != 1 {
                            sqlx::query("ROLLBACK TO SAVEPOINT session_lifecycle_write").execute(&mut *connection).await?;
                        }
                        Ok(Some(result))
                    })
                })
                .await
                .map_err(operation_error)?;
            if result.as_ref().is_none_or(|value| value.rows_affected() != 1) {
                return Err(SessionError::PermissionDenied(
                    "Session does not belong to current user",
                ));
            }
            Ok(())
        })
    }

    fn create_if_absent(&self, session: Value) -> SessionFuture<'_, bool> {
        Box::pin(async move {
            let row = WriteSession::parse(session)?;
            let sql = self.sql(CREATE);
            let mut connection = self.operations.acquire().await.map_err(operation_error)?;
            let created: Option<String> = connection
                .execute(move |connection| {
                    Box::pin(async move {
                        sqlx::query("SAVEPOINT session_lifecycle_write").execute(&mut *connection).await?;
                        if !lock_session_barrier(&mut *connection,row.tenant_id.as_deref(),row.user_id.as_deref(),&row.session_id).await? {
                            sqlx::query("ROLLBACK TO SAVEPOINT session_lifecycle_write").execute(&mut *connection).await?;
                            return Ok(None);
                        }
                        let created = sqlx::query_scalar::<_, String>(&sql)
                            .bind(row.session_id)
                            .bind(row.service_id)
                            .bind(row.user_id)
                            .bind(row.tenant_id)
                            .bind(row.state)
                            .bind(row.history)
                            .bind(row.metadata)
                            .bind(row.config)
                            .bind(row.status)
                            .bind(row.expires_at.map(utc))
                            .fetch_optional(&mut *connection)
                            .await?;
                        if created.is_none() {
                            sqlx::query("ROLLBACK TO SAVEPOINT session_lifecycle_write").execute(&mut *connection).await?;
                        }
                        Ok(created)
                    })
                })
                .await
                .map_err(operation_error)?;
            Ok(created.is_some())
        })
    }

    fn append_message<'a>(
        &'a self,
        id: &'a str,
        message: SessionMessage,
        metadata_update: Option<Map<String, Value>>,
    ) -> SessionFuture<'a, bool> {
        Box::pin(async move {
            let messages = json!([message]);
            let metadata = metadata_update.filter(|value| !value.is_empty());
            let sql = self.sql(if metadata.is_some() {
                APPEND_METADATA
            } else {
                APPEND
            });
            let id = id.to_owned();
            let mut connection = self.operations.acquire().await.map_err(operation_error)?;
            let result = connection
                .execute(move |connection| {
                    Box::pin(async move {
                        let mut query = sqlx::query(&sql).bind(id).bind(messages);
                        if let Some(metadata) = metadata {
                            query = query.bind(Value::Object(metadata));
                        }
                        query.execute(connection).await
                    })
                })
                .await
                .map_err(operation_error)?;
            Ok(result.rows_affected() == 1)
        })
    }

    fn delete<'a>(&'a self, id: &'a str) -> SessionFuture<'a, bool> {
        Box::pin(async move {
            // Two supervised transactions preserve Python's separate statements.
            // The DELETE error must be rolled back before the fallback UPDATE;
            // running it inside the aborted transaction would lose the tombstone.
            let sql = self.sql(DELETE);
            let owned_id = id.to_owned();
            let mut connection = self.operations.acquire().await.map_err(operation_error)?;
            match connection
                .execute(move |connection| {
                    Box::pin(
                        async move { sqlx::query(&sql).bind(owned_id).execute(connection).await },
                    )
                })
                .await
            {
                Ok(result) => Ok(result.rows_affected() == 1),
                Err(error) if error.sqlstate() == Some("23503") => {
                    let sql = self.sql(TOMBSTONE);
                    let id = id.to_owned();
                    let result = connection
                        .execute(move |connection| {
                            Box::pin(
                                async move { sqlx::query(&sql).bind(id).execute(connection).await },
                            )
                        })
                        .await
                        .map_err(operation_error)?;
                    Ok(result.rows_affected() == 1)
                }
                Err(error) => Err(operation_error(error)),
            }
        })
    }

    fn extend_expiry<'a>(
        &'a self,
        id: &'a str,
        expires_at: SessionTimestamp,
    ) -> SessionFuture<'a, ()> {
        Box::pin(async move {
            let sql = self.sql(EXTEND);
            let id = id.to_owned();
            let mut connection = self.operations.acquire().await.map_err(operation_error)?;
            connection
                .execute(move |connection| {
                    Box::pin(async move {
                        sqlx::query(&sql)
                            .bind(utc(expires_at))
                            .bind(id)
                            .execute(connection)
                            .await
                    })
                })
                .await
                .map_err(operation_error)?;
            // The canonical operation treats a missing row as a successful no-op.
            Ok(())
        })
    }
}


