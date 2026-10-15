//! Tenant-scoped native MCP management persistence.
//!
//! Canonical source: ai_gateway_core.persistence.repositories.mcp_repository
//! management methods (create_server through list_tools), plus mcp.py audit SQL.
//! Each ordinary statement is separately supervised and committed, including the
//! two delete_server writes. Discovery is the one deliberate multi-statement
//! transaction. No constructor starts tasks, connects, resolves secrets or probes.
//! Hosts must abort AND join callers, drain_cancelled, then close both pools.
//! Search-path/schema ownership and independent session-pinned cancellation pools
//! are host responsibilities; all resource predicates still bind the tenant.
use crate::{
    mcp_discovery,
    postgres_operation::{PgOperationError, PgOperationStorage},
    protocol::ProtocolTimestamp,
};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sqlx::{Column, PgPool, Row, TypeInfo, ValueRef};
use std::{fmt, future::Future, pin::Pin, sync::Arc};

pub type McpFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, McpError>> + Send + 'a>>;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpError {
    NotFound(String),
    Conflict(String),
    Validation(String),
    Authorization(String),
    Broker(String),
    Storage,
    Cancelled,
    /// Canonical exceptions not mapped by the source HTTP error mapper.
    Unhandled,
}
impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotFound(code)
            | Self::Conflict(code)
            | Self::Validation(code)
            | Self::Authorization(code)
            | Self::Broker(code) => code,
            Self::Storage => "MCP_STORAGE_UNAVAILABLE",
            Self::Cancelled => "MCP_OPERATION_CANCELLED",
            Self::Unhandled => "MCP_INTERNAL_ERROR",
        })
    }
}
impl std::error::Error for McpError {}
#[derive(Clone, Debug, Default)]
pub struct McpContext {
    pub tenant_id: String,
    pub user_id: String,
    pub authenticated: bool,
}
pub trait McpRepository: Send + Sync {
    fn call<'a>(
        &'a self,
        operation: &'a str,
        context: &'a McpContext,
        arguments: Value,
    ) -> McpFuture<'a, Value>;
}

/// Never retains backend error text, SQL, bound values or opaque secret refs.
#[derive(Clone, PartialEq, Eq)]
pub enum McpDatabaseError {
    Database { sqlstate: Option<String> },
    Cancelled,
}
impl fmt::Debug for McpDatabaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database { .. } => "McpDatabaseError::Database",
            Self::Cancelled => "McpDatabaseError::Cancelled",
        })
    }
}
impl From<PgOperationError> for McpDatabaseError {
    fn from(value: PgOperationError) -> Self {
        match value {
            PgOperationError::Cancelled => Self::Cancelled,
            value => Self::Database {
                sqlstate: value.sqlstate().map(str::to_owned),
            },
        }
    }
}
impl McpDatabaseError {
    fn map(self, conflict: &'static str) -> McpError {
        match self {
            Self::Cancelled => McpError::Cancelled,
            Self::Database { sqlstate } if sqlstate.as_deref() == Some("23505") => {
                McpError::Conflict(conflict.into())
            }
            Self::Database { .. } => McpError::Unhandled,
        }
    }
}
pub type McpDatabaseFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, McpDatabaseError>> + Send + 'a>>;

/// Deliberately no Debug: even opaque credential references are private.
#[derive(Clone, PartialEq)]
pub enum McpArgument {
    Text(Option<String>),
    TextArray(Option<Vec<String>>),
    Integer(Option<i32>),
    Boolean(Option<bool>),
    Timestamp(Option<DateTime<Utc>>),
    Json(Value),
}
impl McpArgument {
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text(Some(value.into()))
    }
}
#[derive(Clone, PartialEq)]
pub struct McpStatement {
    pub name: &'static str,
    pub sql: String,
    pub arguments: Vec<McpArgument>,
    pub returns_rows: bool,
}
impl fmt::Debug for McpStatement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpStatement")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}
pub trait McpDatabase: Send + Sync {
    fn run(&self, statement: McpStatement) -> McpDatabaseFuture<'_, Vec<Value>>;
    /// Entire callback, including locks, writes and result assembly, must share a
    /// single supervised transaction. Tools have been validated before entry.
    fn record_discovery(
        &self,
        context: McpContext,
        arguments: Value,
    ) -> McpDatabaseFuture<'_, Value>;
}
pub struct PgMcpDatabase {
    storage: PgOperationStorage,
}
impl PgMcpDatabase {
    pub fn new(data: PgPool, control: PgPool) -> Result<Self, McpError> {
        Ok(Self {
            storage: PgOperationStorage::new(data, control)
                .map_err(|error| McpDatabaseError::from(error).map("MCP_RESOURCE_CONFLICT"))?,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), McpError> {
        self.storage
            .drain_cancelled()
            .await
            .map_err(|error| McpDatabaseError::from(error).map("MCP_RESOURCE_CONFLICT"))
    }
}
impl McpDatabase for PgMcpDatabase {
    fn run(&self, statement: McpStatement) -> McpDatabaseFuture<'_, Vec<Value>> {
        Box::pin(async move {
            let mut connection = self
                .storage
                .acquire()
                .await
                .map_err(McpDatabaseError::from)?;
            connection
                .execute(move |connection| {
                    Box::pin(async move {
                        let mut query = sqlx::query(&statement.sql);
                        for argument in &statement.arguments {
                            query = match argument {
                                McpArgument::Text(value) => query.bind(value.as_deref()),
                                McpArgument::TextArray(value) => query.bind(value.as_deref()),
                                McpArgument::Integer(value) => query.bind(*value),
                                McpArgument::Boolean(value) => query.bind(*value),
                                McpArgument::Timestamp(value) => query.bind(*value),
                                McpArgument::Json(value) => query.bind(sqlx::types::Json(value)),
                            };
                        }
                        if statement.returns_rows {
                            query
                                .fetch_all(connection)
                                .await?
                                .iter()
                                .map(raw_row)
                                .collect()
                        } else {
                            query.execute(connection).await?;
                            Ok(Vec::new())
                        }
                    })
                })
                .await
                .map_err(McpDatabaseError::from)
        })
    }
    fn record_discovery(
        &self,
        context: McpContext,
        arguments: Value,
    ) -> McpDatabaseFuture<'_, Value> {
        Box::pin(async move {
            let mut connection = self
                .storage
                .acquire()
                .await
                .map_err(McpDatabaseError::from)?;
            connection
                .execute(move |connection| {
                    Box::pin(async move {
                        mcp_discovery::record_discovery_native(connection, &context, arguments)
                            .await
                    })
                })
                .await
                .map_err(McpDatabaseError::from)
        })
    }
}
fn binding_error() -> sqlx::Error {
    sqlx::Error::Protocol("mcp_storage_value_invalid".into())
}
fn raw_row(row: &sqlx::postgres::PgRow) -> Result<Value, sqlx::Error> {
    let mut object = Map::new();
    for column in row.columns() {
        let index = column.ordinal();
        let cell = row.try_get_raw(index)?;
        let value = if cell.is_null() {
            Value::Null
        } else {
            match column.type_info().name() {
                "INT2" => json!(row.try_get::<i16, _>(index)?),
                "INT4" => json!(row.try_get::<i32, _>(index)?),
                "INT8" => json!(row.try_get::<i64, _>(index)?),
                "BOOL" => json!(row.try_get::<bool, _>(index)?),
                "UUID" => json!(
                    uuid::Uuid::from_slice(cell.as_bytes().map_err(|_| binding_error())?)
                        .map_err(|_| binding_error())?
                        .to_string()
                ),
                "TIMESTAMPTZ" => json!(
                    ProtocolTimestamp::from(row.try_get::<DateTime<Utc>, _>(index)?.fixed_offset())
                        .to_isoformat()
                ),
                "JSON" | "JSONB" => row.try_get::<sqlx::types::Json<Value>, _>(index)?.0,
                "TEXT[]" | "VARCHAR[]" | "_text" | "_varchar" => {
                    json!(row.try_get::<Vec<Option<String>>, _>(index)?)
                }
                _ => json!(row.try_get::<String, _>(index)?),
            }
        };
        object.insert(column.name().into(), value);
    }
    Ok(Value::Object(object))
}

pub struct SqlMcpRepository {
    enabled: bool,
    database: Option<Arc<dyn McpDatabase>>,
}
impl SqlMcpRepository {
    pub fn new(enabled: bool, database: Option<Arc<dyn McpDatabase>>) -> Self {
        Self { enabled, database }
    }
    fn database(&self) -> Result<&dyn McpDatabase, McpError> {
        if !self.enabled {
            return Err(McpError::Storage);
        }
        self.database.as_deref().ok_or(McpError::Storage)
    }
    async fn run(
        &self,
        name: &'static str,
        sql: impl Into<String>,
        arguments: Vec<McpArgument>,
        returns_rows: bool,
        conflict: &'static str,
    ) -> Result<Vec<Value>, McpError> {
        self.database()?
            .run(McpStatement {
                name,
                sql: sql.into(),
                arguments,
                returns_rows,
            })
            .await
            .map_err(|error| error.map(conflict))
    }
    async fn rows(
        &self,
        name: &'static str,
        sql: impl Into<String>,
        arguments: Vec<McpArgument>,
    ) -> Result<Vec<Value>, McpError> {
        self.run(name, sql, arguments, true, "MCP_RESOURCE_CONFLICT")
            .await
    }
    async fn get_server(
        &self,
        context: &McpContext,
        id: &str,
        include_deleted: bool,
    ) -> Result<Value, McpError> {
        let deleted = if include_deleted {
            ""
        } else {
            "AND deleted_at IS NULL"
        };
        let rows = self.rows("get_server", format!(
            "SELECT * FROM mcp_servers WHERE tenant_id = $1 AND server_id = $2::uuid {deleted}"),
            tenant_id_args(context, id)).await?;
        Ok(redact_server(first(rows, "MCP_SERVER_NOT_FOUND")?))
    }
    async fn create_server(
        &self,
        context: &McpContext,
        arguments: &Value,
    ) -> Result<Value, McpError> {
        validate_server_config(arguments)?;
        let rows = self
            .run(
                "create_server",
                r#"
            INSERT INTO mcp_servers (
                tenant_id, name, description, base_url, transport,
                auth_method, oauth_metadata_url, oauth_resource,
                oauth_audience, allowed_origins, timeout_ms,
                max_concurrency, response_limit_bytes, created_by, updated_by
            ) VALUES ($1, $2, $3, $4, 'streamable_http', $5, $6, $7, $8,
                      $9::text[], $10, $11, $12, $13, $13) RETURNING *"#,
                vec![
                    McpArgument::text(&context.tenant_id),
                    text(arguments, "name")?,
                    text(arguments, "description")?,
                    text(arguments, "base_url")?,
                    text(arguments, "auth_method")?,
                    text(arguments, "oauth_metadata_url")?,
                    text(arguments, "oauth_resource")?,
                    text(arguments, "oauth_audience")?,
                    text_array(arguments, "allowed_origins")?,
                    integer(arguments, "timeout_ms")?,
                    integer(arguments, "max_concurrency")?,
                    integer(arguments, "response_limit_bytes")?,
                    McpArgument::text(&context.user_id),
                ],
                true,
                "MCP_SERVER_NAME_CONFLICT",
            )
            .await?;
        // RETURNING on INSERT is guaranteed; null would indicate a broken adapter.
        Ok(redact_server(
            rows.into_iter().next().ok_or(McpError::Unhandled)?,
        ))
    }
    async fn update_server(
        &self,
        context: &McpContext,
        arguments: &Value,
    ) -> Result<Value, McpError> {
        let id = required_string(arguments, "server_id")?;
        let changes = arguments
            .get("changes")
            .and_then(Value::as_object)
            .ok_or(McpError::Unhandled)?;
        let items: Vec<_> = SERVER_MUTABLE
            .iter()
            .filter_map(|key| changes.get(*key).map(|value| (*key, value)))
            .collect();
        if items.is_empty() {
            return self.get_server(context, id, false).await;
        }
        let current = first(
            self.rows(
                "update_server_current",
                r#"
            SELECT * FROM mcp_servers
            WHERE tenant_id = $1 AND server_id = $2::uuid AND deleted_at IS NULL"#,
                tenant_id_args(context, id),
            )
            .await?,
            "MCP_SERVER_NOT_FOUND",
        )?;
        let mut merged = current.as_object().cloned().ok_or(McpError::Unhandled)?;
        for (key, value) in &items {
            merged.insert((*key).into(), (*value).clone());
        }
        validate_server_config(&Value::Object(merged))?;
        if AUTH_FIELDS.iter().any(|key| changes.contains_key(*key)) {
            let count = self
                .rows(
                    "update_server_connections",
                    r#"
                SELECT COUNT(*) AS count FROM mcp_connections
                WHERE tenant_id = $1 AND server_id = $2::uuid
                  AND enabled = TRUE AND revoked_at IS NULL"#,
                    tenant_id_args(context, id),
                )
                .await?;
            if count
                .first()
                .and_then(|row| row.get("count"))
                .and_then(Value::as_i64)
                .unwrap_or(0)
                > 0
            {
                return Err(validation("MCP_CONNECTION_REBIND_REQUIRED"));
            }
        }
        let mut values = tenant_id_args(context, id);
        let mut assignments = Vec::new();
        for (key, _) in items {
            values.push(match key {
                "allowed_origins" => text_array(&arguments["changes"], key)?,
                "timeout_ms" | "max_concurrency" | "response_limit_bytes" => {
                    integer(&arguments["changes"], key)?
                }
                "enabled" => boolean(&arguments["changes"], key)?,
                _ => text(&arguments["changes"], key)?,
            });
            let cast = if key == "allowed_origins" {
                "::text[]"
            } else {
                ""
            };
            // key is exclusively drawn from SERVER_MUTABLE, never caller SQL.
            assignments.push(format!("{key} = ${}{cast}", values.len()));
        }
        values.push(McpArgument::text(&context.user_id));
        assignments.push(format!("updated_by = ${}", values.len()));
        assignments.push("updated_at = NOW()".into());
        let rows = self.run("update_server", format!(
            "UPDATE mcp_servers SET {} WHERE tenant_id = $1 AND server_id = $2::uuid AND deleted_at IS NULL RETURNING *",
            assignments.join(", ")), values, true, "MCP_SERVER_NAME_CONFLICT").await?;
        Ok(redact_server(first(rows, "MCP_SERVER_NOT_FOUND")?))
    }
    async fn delete_server(
        &self,
        context: &McpContext,
        arguments: &Value,
    ) -> Result<Value, McpError> {
        let id = required_string(arguments, "server_id")?;
        let mut values = tenant_id_args(context, id);
        values.push(McpArgument::text(&context.user_id));
        first(
            self.rows(
                "delete_server",
                r#"
            UPDATE mcp_servers
            SET enabled = FALSE, deleted_at = NOW(), updated_at = NOW(), updated_by = $3
            WHERE tenant_id = $1 AND server_id = $2::uuid AND deleted_at IS NULL
            RETURNING server_id"#,
                values.clone(),
            )
            .await?,
            "MCP_SERVER_NOT_FOUND",
        )?;
        // Intentionally a second committed operation: a revocation failure must
        // not roll back the canonical server soft-delete.
        self.run(
            "delete_server_revoke_connections",
            r#"
            UPDATE mcp_connections
            SET enabled = FALSE, revoked_at = COALESCE(revoked_at, NOW()),
                updated_at = NOW(), updated_by = $3
            WHERE tenant_id = $1 AND server_id = $2::uuid AND revoked_at IS NULL"#,
            values,
            false,
            "MCP_RESOURCE_CONFLICT",
        )
        .await?;
        Ok(Value::Null)
    }
    async fn create_connection(
        &self,
        context: &McpContext,
        arguments: &Value,
    ) -> Result<Value, McpError> {
        let principal = required_string(arguments, "principal_type")?;
        if !is_principal(principal) {
            return Err(validation("MCP_PRINCIPAL_TYPE_INVALID"));
        }
        let owner = optional_string(arguments, "owner_user_id")?;
        if principal == "service_account" && owner.is_some() {
            return Err(validation("MCP_PRINCIPAL_OWNER_INVALID"));
        }
        if principal == "user_delegated" && owner.is_none_or(str::is_empty) {
            return Err(validation("MCP_PRINCIPAL_OWNER_REQUIRED"));
        }
        let secret = optional_string(arguments, "secret_ref")?;
        validate_secret_ref(secret)?;
        let id = required_string(arguments, "server_id")?;
        let server = first(
            self.rows(
                "create_connection_server",
                r#"
            SELECT auth_method, oauth_audience FROM mcp_servers
            WHERE tenant_id = $1 AND server_id = $2::uuid
              AND enabled = TRUE AND deleted_at IS NULL"#,
                tenant_id_args(context, id),
            )
            .await?,
            "MCP_SERVER_NOT_FOUND",
        )?;
        let auth = server
            .get("auth_method")
            .and_then(Value::as_str)
            .ok_or(McpError::Unhandled)?;
        if auth == "none" && secret.is_some() {
            return Err(validation("MCP_SECRET_NOT_ALLOWED"));
        }
        if matches!(auth, "bearer" | "oauth") && secret.is_none() {
            return Err(validation("MCP_SECRET_REF_REQUIRED"));
        }
        let audience = optional_string(arguments, "audience")?;
        if auth == "oauth"
            && (audience.is_none_or(str::is_empty)
                || !truthy(&server["oauth_audience"])
                || server["oauth_audience"].as_str() != audience)
        {
            return Err(validation("MCP_OAUTH_AUDIENCE_MISMATCH"));
        }
        if auth == "oauth" && !truthy(&arguments["scopes"]) {
            return Err(validation("MCP_OAUTH_SCOPES_REQUIRED"));
        }
        let rows = self
            .run(
                "create_connection",
                r#"
            INSERT INTO mcp_connections (
                tenant_id, server_id, principal_type, owner_user_id,
                secret_ref, scopes, audience, expires_at, created_by, updated_by
            ) VALUES ($1, $2::uuid, $3, $4, $5, $6::text[], $7, $8, $9, $9)
            RETURNING *"#,
                vec![
                    McpArgument::text(&context.tenant_id),
                    McpArgument::text(id),
                    McpArgument::text(principal),
                    text(arguments, "owner_user_id")?,
                    text(arguments, "secret_ref")?,
                    text_array(arguments, "scopes")?,
                    text(arguments, "audience")?,
                    timestamp(arguments, "expires_at")?,
                    McpArgument::text(&context.user_id),
                ],
                true,
                "MCP_CONNECTION_CONFLICT",
            )
            .await?;
        Ok(redact_connection(
            rows.into_iter().next().ok_or(McpError::Unhandled)?,
        ))
    }
    async fn resolve_discovery_connection(
        &self,
        context: &McpContext,
        arguments: &Value,
    ) -> Result<Value, McpError> {
        let principal = required_string(arguments, "principal_type")?;
        if !is_principal(principal) {
            return Err(authorization("MCP_PRINCIPAL_POLICY_DENIED"));
        }
        let rows = self
            .rows(
                "resolve_discovery_connection",
                r#"
            SELECT srv.server_id, srv.name, srv.base_url, srv.auth_method,
                   srv.oauth_resource, srv.oauth_audience, srv.allowed_origins,
                   srv.timeout_ms, srv.max_concurrency,
                   srv.response_limit_bytes, srv.enabled,
                   c.connection_id, c.principal_type, c.owner_user_id,
                   c.secret_ref, c.scopes, c.audience, c.expires_at
            FROM mcp_servers srv
            JOIN mcp_connections c
              ON c.tenant_id = srv.tenant_id AND c.server_id = srv.server_id
            WHERE srv.tenant_id = $1 AND srv.server_id = $2::uuid
              AND c.connection_id = $3::uuid AND c.principal_type = $4
              AND srv.enabled = TRUE AND srv.deleted_at IS NULL
              AND c.enabled = TRUE AND c.revoked_at IS NULL
              AND (c.expires_at IS NULL OR c.expires_at > NOW())"#,
                vec![
                    McpArgument::text(&context.tenant_id),
                    text(arguments, "server_id")?,
                    text(arguments, "connection_id")?,
                    McpArgument::text(principal),
                ],
            )
            .await?;
        let item = rows
            .into_iter()
            .next()
            .ok_or_else(|| authorization("MCP_CONNECTION_UNAVAILABLE"))?;
        if principal == "user_delegated"
            && (!context.authenticated
                || item["owner_user_id"].as_str() != Some(context.user_id.as_str()))
        {
            return Err(authorization("MCP_DELEGATED_PRINCIPAL_DENIED"));
        }
        if item["auth_method"] == "oauth"
            && (!truthy(&item["oauth_audience"]) || item["audience"] != item["oauth_audience"])
        {
            return Err(authorization("MCP_OAUTH_AUDIENCE_MISMATCH"));
        }
        // Internal broker-only result. Do not add this operation to public reads.
        // Source intentionally does not SELECT revision timestamps here.
        Ok(item)
    }
    async fn revoke_connection(
        &self,
        context: &McpContext,
        arguments: &Value,
    ) -> Result<Value, McpError> {
        let mut values = tenant_id_args(context, required_string(arguments, "connection_id")?);
        values.push(McpArgument::text(&context.user_id));
        first(
            self.rows(
                "revoke_connection",
                r#"
            UPDATE mcp_connections
            SET enabled = FALSE, revoked_at = COALESCE(revoked_at, NOW()),
                updated_at = NOW(), updated_by = $3
            WHERE tenant_id = $1 AND connection_id = $2::uuid RETURNING connection_id"#,
                values,
            )
            .await?,
            "MCP_CONNECTION_NOT_FOUND",
        )?;
        Ok(Value::Null)
    }
    async fn grant_channel(
        &self,
        context: &McpContext,
        arguments: &Value,
    ) -> Result<Value, McpError> {
        let channel = required_string(arguments, "channel")?;
        if !matches!(
            channel,
            "preview" | "hosted_private" | "hosted_public" | "embed" | "api"
        ) {
            return Err(validation("MCP_CHANNEL_INVALID"));
        }
        let read_only = arguments
            .get("read_only_only")
            .and_then(Value::as_bool)
            .ok_or(McpError::Unhandled)?;
        let target = self
            .rows(
                "grant_channel_target",
                r#"
            SELECT c.principal_type, c.server_id AS connection_server_id,
                   t.server_id AS tool_server_id, s.schema_hash
            FROM mcp_connections c
            JOIN mcp_tools t ON t.tenant_id = c.tenant_id AND t.tool_id = $3::uuid
            JOIN mcp_tool_snapshots s
              ON s.tenant_id = t.tenant_id AND s.snapshot_id = t.current_snapshot_id
            WHERE c.tenant_id = $1 AND c.connection_id = $2::uuid
              AND c.enabled = TRUE AND c.revoked_at IS NULL AND t.enabled = TRUE"#,
                vec![
                    McpArgument::text(&context.tenant_id),
                    text(arguments, "connection_id")?,
                    text(arguments, "tool_id")?,
                ],
            )
            .await?;
        let target = first(target, "MCP_CHANNEL_GRANT_TARGET_NOT_FOUND")?;
        if target["connection_server_id"] != target["tool_server_id"] {
            return Err(not_found("MCP_CHANNEL_GRANT_TARGET_NOT_FOUND"));
        }
        if matches!(channel, "hosted_public" | "embed")
            && (target["principal_type"] != "service_account" || !read_only)
        {
            return Err(validation("MCP_PUBLIC_CHANNEL_DENIED"));
        }
        first(
            self.rows(
                "grant_channel",
                r#"
            INSERT INTO mcp_channel_grants (
                tenant_id, connection_id, tool_id, channel,
                read_only_only, approved_schema_hash, authorized_by
            ) VALUES ($1, $2::uuid, $3::uuid, $4, $5, $6, $7)
            ON CONFLICT (tenant_id, connection_id, tool_id, channel)
            DO UPDATE SET read_only_only = EXCLUDED.read_only_only,
                          approved_schema_hash = EXCLUDED.approved_schema_hash,
                          enabled = TRUE, authorized_by = EXCLUDED.authorized_by,
                          updated_at = NOW()
            RETURNING connection_id, tool_id, channel, read_only_only,
                      approved_schema_hash, enabled, authorized_by, created_at, updated_at"#,
                vec![
                    McpArgument::text(&context.tenant_id),
                    text(arguments, "connection_id")?,
                    text(arguments, "tool_id")?,
                    McpArgument::text(channel),
                    McpArgument::Boolean(Some(read_only)),
                    text(&target, "schema_hash")?,
                    McpArgument::text(&context.user_id),
                ],
            )
            .await?,
            "MCP_CHANNEL_GRANT_TARGET_NOT_FOUND",
        )
    }
    async fn audit_mutation(
        &self,
        context: &McpContext,
        arguments: &Value,
    ) -> Result<Value, McpError> {
        // Match the canonical top-level exclusion. API owns the bounded summaries.
        let mut summary = arguments
            .get("summary")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        for key in ["secret_ref", "token", "authorization", "credential"] {
            summary.remove(key);
        }
        let rows = self
            .rows(
                "audit_mutation",
                r#"
            INSERT INTO audit_logs (
                event_type, user_id, tenant_id, resource_type, resource_id,
                action, request_summary, status
            ) VALUES ('mcp_registry', $1, $2, $3, $4, $5, $6::jsonb, 'success') RETURNING id"#,
                vec![
                    McpArgument::text(&context.user_id),
                    McpArgument::text(&context.tenant_id),
                    text(arguments, "resource_type")?,
                    text(arguments, "resource_id")?,
                    text(arguments, "action")?,
                    McpArgument::Json(Value::Object(summary)),
                ],
            )
            .await?;
        Ok(rows
            .first()
            .and_then(|row| row.get("id"))
            .cloned()
            .unwrap_or(Value::Null))
    }
}
impl McpRepository for SqlMcpRepository {
    fn call<'a>(
        &'a self,
        operation: &'a str,
        context: &'a McpContext,
        arguments: Value,
    ) -> McpFuture<'a, Value> {
        Box::pin(async move {
            self.database()?;
            match operation {
                "create_server" => self.create_server(context, &arguments).await,
                "list_servers" => Ok(Value::Array(
                    self.rows(
                        "list_servers",
                        r#"
                    SELECT * FROM mcp_servers WHERE tenant_id = $1 AND deleted_at IS NULL
                    ORDER BY updated_at DESC, server_id"#,
                        vec![McpArgument::text(&context.tenant_id)],
                    )
                    .await?
                    .into_iter()
                    .map(redact_server)
                    .collect(),
                )),
                "get_server" => {
                    self.get_server(
                        context,
                        required_string(&arguments, "server_id")?,
                        arguments
                            .get("include_deleted")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    )
                    .await
                }
                "update_server" => self.update_server(context, &arguments).await,
                "delete_server" => self.delete_server(context, &arguments).await,
                "create_connection" => self.create_connection(context, &arguments).await,
                "list_connections" => Ok(Value::Array(
                    self.rows(
                        "list_connections",
                        r#"
                    SELECT * FROM mcp_connections WHERE tenant_id = $1 AND server_id = $2::uuid
                    ORDER BY created_at DESC, connection_id"#,
                        tenant_id_args(context, required_string(&arguments, "server_id")?),
                    )
                    .await?
                    .into_iter()
                    .map(redact_connection)
                    .collect(),
                )),
                "resolve_discovery_connection" => {
                    self.resolve_discovery_connection(context, &arguments).await
                }
                "revoke_connection" => self.revoke_connection(context, &arguments).await,
                "grant_channel" => self.grant_channel(context, &arguments).await,
                "list_tools" => {
                    let rows = self.rows("list_tools", r#"
                        SELECT t.tool_id, t.server_id, t.upstream_name, t.runtime_name,
                               t.enabled, s.snapshot_id, s.schema_version, s.schema_hash,
                               s.description, s.input_schema, s.risk_level, s.read_only, s.discovered_at
                        FROM mcp_tools t
                        JOIN mcp_tool_snapshots s
                          ON s.tenant_id = t.tenant_id AND s.snapshot_id = t.current_snapshot_id
                        WHERE t.tenant_id = $1 AND t.server_id = $2::uuid ORDER BY t.upstream_name"#,
                        tenant_id_args(context, required_string(&arguments, "server_id")?)).await?;
                    Ok(Value::Array(
                        rows.into_iter()
                            .map(|mut row| {
                                row["input_schema"] = json_object(&row["input_schema"]);
                                row
                            })
                            .collect(),
                    ))
                }
                "record_discovery" => {
                    mcp_discovery::validate_tools(arguments.get("tools").unwrap_or(&Value::Null))?;
                    unwrap_discovery(
                        self.database()?
                            .record_discovery(context.clone(), arguments)
                            .await
                            .map_err(|error| error.map("MCP_RESOURCE_CONFLICT"))?,
                    )
                }
                "audit_mutation" => self.audit_mutation(context, &arguments).await,
                _ => Err(McpError::Unhandled),
            }
        })
    }
}
const SERVER_PUBLIC: &[&str] = &[
    "server_id",
    "name",
    "description",
    "base_url",
    "transport",
    "auth_method",
    "oauth_metadata_url",
    "oauth_resource",
    "oauth_audience",
    "allowed_origins",
    "timeout_ms",
    "max_concurrency",
    "response_limit_bytes",
    "enabled",
    "health_status",
    "circuit_state",
    "consecutive_failures",
    "last_health_at",
    "last_error_code",
    "created_at",
    "updated_at",
];
const SERVER_MUTABLE: &[&str] = &[
    "name",
    "description",
    "base_url",
    "auth_method",
    "oauth_metadata_url",
    "oauth_resource",
    "oauth_audience",
    "allowed_origins",
    "timeout_ms",
    "max_concurrency",
    "response_limit_bytes",
    "enabled",
];
const AUTH_FIELDS: &[&str] = &[
    "auth_method",
    "oauth_metadata_url",
    "oauth_resource",
    "oauth_audience",
];
fn redact_server(row: Value) -> Value {
    Value::Object(
        SERVER_PUBLIC
            .iter()
            .map(|key| ((*key).into(), row.get(*key).cloned().unwrap_or(Value::Null)))
            .collect(),
    )
}
fn redact_connection(row: Value) -> Value {
    let mut out: Map<String, Value> = [
        "connection_id",
        "server_id",
        "principal_type",
        "owner_user_id",
        "audience",
        "expires_at",
        "revoked_at",
        "enabled",
        "created_at",
        "updated_at",
    ]
    .iter()
    .map(|key| ((*key).into(), row.get(*key).cloned().unwrap_or(Value::Null)))
    .collect();
    out.insert(
        "scopes".into(),
        if truthy(&row["scopes"]) {
            row["scopes"].clone()
        } else {
            json!([])
        },
    );
    out.insert(
        "credential_configured".into(),
        json!(truthy(&row["secret_ref"])),
    );
    Value::Object(out)
}
fn validate_server_config(value: &Value) -> Result<(), McpError> {
    if !value
        .get("allowed_origins")
        .and_then(Value::as_array)
        .is_some_and(|v| (1..=64).contains(&v.len()))
    {
        return Err(validation("MCP_ALLOWED_ORIGIN_REQUIRED"));
    }
    let fields = ["oauth_metadata_url", "oauth_resource", "oauth_audience"];
    if value["auth_method"] == "oauth" && !fields.iter().all(|key| truthy(&value[*key])) {
        return Err(validation("MCP_OAUTH_CONFIG_INCOMPLETE"));
    }
    if value["auth_method"] != "oauth" && fields.iter().any(|key| truthy(&value[*key])) {
        return Err(validation("MCP_OAUTH_CONFIG_NOT_ALLOWED"));
    }
    Ok(())
}
pub fn validate_secret_ref(value: Option<&str>) -> Result<(), McpError> {
    let Some(value) = value else {
        return Ok(());
    };
    let valid = value.split_once("://").is_some_and(|(scheme, rest)| {
        !matches!(scheme, "http" | "https")
            && !rest.is_empty()
            && !rest.chars().any(python_whitespace)
            && scheme
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_lowercase)
            && scheme.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"+.-".contains(&byte)
            })
    });
    if valid {
        Ok(())
    } else {
        Err(validation("MCP_SECRET_REF_INVALID"))
    }
}
fn python_whitespace(value: char) -> bool {
    value.is_whitespace() || ('\u{001c}'..='\u{001f}').contains(&value)
}
fn is_principal(value: &str) -> bool {
    matches!(value, "service_account" | "user_delegated")
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}
fn json_object(value: &Value) -> Value {
    if value.is_object() {
        return value.clone();
    }
    value
        .as_str()
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}
fn unwrap_discovery(value: Value) -> Result<Value, McpError> {
    let Some(error) = value.get("__mcp_error") else {
        return Ok(value);
    };
    match (
        error.get("kind").and_then(Value::as_str),
        error.get("code").and_then(Value::as_str),
    ) {
        (Some("not_found"), Some("MCP_SERVER_NOT_FOUND")) => Err(not_found("MCP_SERVER_NOT_FOUND")),
        (Some("validation"), Some("MCP_TOOL_SCHEMA_INVALID")) => {
            Err(validation("MCP_TOOL_SCHEMA_INVALID"))
        }
        _ => Err(McpError::Unhandled),
    }
}
fn tenant_id_args(context: &McpContext, id: &str) -> Vec<McpArgument> {
    vec![McpArgument::text(&context.tenant_id), McpArgument::text(id)]
}
fn first(rows: Vec<Value>, code: &'static str) -> Result<Value, McpError> {
    rows.into_iter().next().ok_or_else(|| not_found(code))
}
fn validation(code: &str) -> McpError {
    McpError::Validation(code.into())
}
fn authorization(code: &str) -> McpError {
    McpError::Authorization(code.into())
}
fn not_found(code: &str) -> McpError {
    McpError::NotFound(code.into())
}
fn required_string<'a>(value: &'a Value, key: &str) -> Result<&'a str, McpError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(McpError::Unhandled)
}
fn optional_string<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>, McpError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(McpError::Unhandled),
    }
}
fn text(value: &Value, key: &str) -> Result<McpArgument, McpError> {
    Ok(McpArgument::Text(
        optional_string(value, key)?.map(str::to_owned),
    ))
}
fn text_array(value: &Value, key: &str) -> Result<McpArgument, McpError> {
    let array = match value.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::Array(values)) => Some(
            values
                .iter()
                .map(|value| value.as_str().map(str::to_owned).ok_or(McpError::Unhandled))
                .collect::<Result<_, _>>()?,
        ),
        _ => return Err(McpError::Unhandled),
    };
    Ok(McpArgument::TextArray(array))
}
fn integer(value: &Value, key: &str) -> Result<McpArgument, McpError> {
    let value = match value.get(key) {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .ok_or(McpError::Unhandled)?,
        ),
    };
    Ok(McpArgument::Integer(value))
}
fn boolean(value: &Value, key: &str) -> Result<McpArgument, McpError> {
    let value = match value.get(key) {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_bool().ok_or(McpError::Unhandled)?),
    };
    Ok(McpArgument::Boolean(value))
}
fn timestamp(value: &Value, key: &str) -> Result<McpArgument, McpError> {
    let value = match optional_string(value, key)? {
        None => None,
        Some(value) => {
            match ProtocolTimestamp::parse_isoformat(value).map_err(|_| McpError::Unhandled)? {
                ProtocolTimestamp::Offset(value) => Some(value.with_timezone(&Utc)),
                // Canonical asyncpg does not coerce naive datetimes for timestamptz.
                ProtocolTimestamp::Naive(_) => return Err(McpError::Unhandled),
            }
        }
    };
    Ok(McpArgument::Timestamp(value))
}
