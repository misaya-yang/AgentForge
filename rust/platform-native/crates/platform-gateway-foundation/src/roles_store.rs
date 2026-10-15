//! Global role definitions with tenant-scoped account membership projections.
//! Only platform administrators may mutate shared definitions through RolesApi.
//!
//! Each source autocommit statement is a separate supervised execute on one
//! acquired connection. Earlier successful statements survive later errors or
//! cancellation; association rows and the denormalized permissions array may then
//! disagree, exactly as in the source. No catalog FK, retry or transaction is added.
//! Global permission invalidation follows a completed create/update after release,
//! and a completed delete before release. A missing callback means the host uses
//! fresh authority reads; it does not claim to clear another process's cache.
//! Construction is inert. Hosts must abort AND join callers, then drain_cancelled
//! before closing either independent same-server/database/role session-pinned pool.
//! The mature supervisor's cancellation/COMMIT race boundaries apply unchanged.
//! Native timestamp decoding supports finite chrono-representable PostgreSQL
//! timestamps; infinity and out-of-range dates are an explicit host/data boundary.
use crate::postgres_operation::{PgOperationConnection, PgOperationError, PgOperationStorage};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{fmt, future::Future, pin::Pin, sync::Arc};

pub type RolesStoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, RolesStoreError>> + Send + 'a>>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RolesStoreError {
    Database,
    Cancelled,
    InvalidCancellationPool,
    IndeterminateCancellation,
    InvalidValue,
}
impl fmt::Display for RolesStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Database => "roles_database_unavailable",
            Self::Cancelled => "roles_cancelled",
            Self::InvalidCancellationPool => "roles_independent_control_pool_required",
            Self::IndeterminateCancellation => "roles_cancellation_indeterminate",
            Self::InvalidValue => "roles_invalid_value",
        })
    }
}
impl std::error::Error for RolesStoreError {}
impl From<PgOperationError> for RolesStoreError {
    fn from(error: PgOperationError) -> Self {
        match error {
            PgOperationError::Cancelled => Self::Cancelled,
            PgOperationError::InvalidCancellationPool => Self::InvalidCancellationPool,
            PgOperationError::IndeterminateCancellation => Self::IndeterminateCancellation,
            _ => Self::Database,
        }
    }
}
pub trait RolesDatabase: Send + Sync {
    fn enabled(&self) -> bool;
    fn truthy(&self) -> bool {
        true
    }
    fn list_roles(&self) -> RolesStoreFuture<'_, Vec<Value>>;
    fn get_role<'a>(&'a self, role: &'a str) -> RolesStoreFuture<'a, Option<Value>>;
    fn create_role<'a>(
        &'a self,
        role: &'a str,
        description: Option<&'a str>,
        permissions: &'a [String],
    ) -> RolesStoreFuture<'a, ()>;
    fn update_role<'a>(
        &'a self,
        role: &'a str,
        description: Option<&'a str>,
        permissions: Option<&'a [String]>,
    ) -> RolesStoreFuture<'a, ()>;
    fn delete_role<'a>(&'a self, role: &'a str) -> RolesStoreFuture<'a, bool>;
    fn list_permissions<'a>(
        &'a self,
        category: Option<&'a str>,
    ) -> RolesStoreFuture<'a, Vec<Value>>;
    fn get_role_user_count<'a>(&'a self, role: &'a str) -> RolesStoreFuture<'a, i64>;
    fn get_users_by_role<'a>(&'a self, role: &'a str) -> RolesStoreFuture<'a, Vec<Value>>;
    fn tenant_role_user_count<'a>(&'a self, role: &'a str, tenant: &'a str) -> RolesStoreFuture<'a, i64> {
        let _ = (role, tenant);
        Box::pin(async { Err(RolesStoreError::InvalidValue) })
    }
    fn tenant_users_by_role<'a>(&'a self, role: &'a str, tenant: &'a str) -> RolesStoreFuture<'a, Vec<Value>> {
        let _ = (role, tenant);
        Box::pin(async { Err(RolesStoreError::InvalidValue) })
    }
    fn platform_administrator<'a>(&'a self, user: &'a str, tenant: &'a str) -> RolesStoreFuture<'a, bool> {
        let _ = (user, tenant);
        Box::pin(async { Ok(false) })
    }
}
pub trait RolesPermissionCache: Send + Sync {
    /// Invalidate all cached permissions, never only the caller's tenant/user.
    fn invalidate_all(&self) -> RolesStoreFuture<'_, ()>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RolesStatementKind {
    Roles,
    Permissions,
    Members,
    Count,
    Mutation,
}
#[derive(Clone, PartialEq)]
pub struct RolesStatement {
    pub sql: String,
    pub arguments: Vec<Value>,
    pub kind: RolesStatementKind,
}
impl fmt::Debug for RolesStatement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RolesStatement")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Default, PartialEq)]
pub struct RolesSqlResult {
    pub rows: Vec<Value>,
    pub count: i64,
    pub affected: u64,
}
impl fmt::Debug for RolesSqlResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RolesSqlResult([REDACTED])")
    }
}
pub trait RolesConnection: Send {
    /// Exactly one canonical autocommit statement. Never group calls atomically.
    fn run(&mut self, statement: RolesStatement) -> RolesStoreFuture<'_, RolesSqlResult>;
}
pub trait RolesPool: Send + Sync {
    fn acquire(&self) -> RolesStoreFuture<'_, Box<dyn RolesConnection>>;
}
pub struct SqlRolesDatabase {
    enabled: bool,
    pool: Option<Arc<dyn RolesPool>>,
    permission_cache: Option<Arc<dyn RolesPermissionCache>>,
}
impl SqlRolesDatabase {
    pub fn new(
        enabled: bool,
        pool: Option<Arc<dyn RolesPool>>,
        permission_cache: Option<Arc<dyn RolesPermissionCache>>,
    ) -> Self {
        Self {
            enabled,
            pool,
            permission_cache,
        }
    }
    async fn invalidate(&self) -> Result<(), RolesStoreError> {
        if let Some(cache) = &self.permission_cache {
            cache.invalidate_all().await?;
        }
        Ok(())
    }
    async fn query(
        &self,
        sql: &str,
        arguments: Vec<Value>,
        kind: RolesStatementKind,
    ) -> Result<RolesSqlResult, RolesStoreError> {
        let Some(pool) = &self.pool else {
            return Ok(RolesSqlResult::default());
        };
        pool.acquire()
            .await?
            .run(statement(sql, arguments, kind))
            .await
    }
}
fn statement(sql: &str, arguments: Vec<Value>, kind: RolesStatementKind) -> RolesStatement {
    RolesStatement {
        sql: sql.to_owned(),
        arguments,
        kind,
    }
}
async fn mutation(
    connection: &mut dyn RolesConnection,
    sql: &str,
    arguments: Vec<Value>,
) -> Result<u64, RolesStoreError> {
    Ok(connection
        .run(statement(sql, arguments, RolesStatementKind::Mutation))
        .await?
        .affected)
}
impl RolesDatabase for SqlRolesDatabase {
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn list_roles(&self) -> RolesStoreFuture<'_, Vec<Value>> {
        Box::pin(async move {
            Ok(self
                .query(LIST_ROLES_SQL, vec![], RolesStatementKind::Roles)
                .await?
                .rows)
        })
    }
    fn get_role<'a>(&'a self, role: &'a str) -> RolesStoreFuture<'a, Option<Value>> {
        Box::pin(async move {
            Ok(self
                .query(GET_ROLE_SQL, vec![json!(role)], RolesStatementKind::Roles)
                .await?
                .rows
                .into_iter()
                .next())
        })
    }
    fn create_role<'a>(
        &'a self,
        role: &'a str,
        description: Option<&'a str>,
        permissions: &'a [String],
    ) -> RolesStoreFuture<'a, ()> {
        Box::pin(async move {
            let Some(pool) = &self.pool else {
                return Ok(());
            };
            {
                let mut connection = pool.acquire().await?;
                mutation(
                    connection.as_mut(),
                    CREATE_ROLE_SQL,
                    vec![json!(role), json!(description), json!(permissions)],
                )
                .await?;
                for permission in permissions {
                    mutation(
                        connection.as_mut(),
                        INSERT_PERMISSION_SQL,
                        vec![json!(role), json!(permission)],
                    )
                    .await?;
                }
            }
            self.invalidate().await
        })
    }
    fn update_role<'a>(
        &'a self,
        role: &'a str,
        description: Option<&'a str>,
        permissions: Option<&'a [String]>,
    ) -> RolesStoreFuture<'a, ()> {
        Box::pin(async move {
            let Some(pool) = &self.pool else {
                return Ok(());
            };
            {
                let mut connection = pool.acquire().await?;
                match (description, permissions) {
                    (Some(description), Some(permissions)) => {
                        mutation(
                            connection.as_mut(),
                            UPDATE_BOTH_SQL,
                            vec![json!(description), json!(permissions), json!(role)],
                        )
                        .await?;
                    }
                    (Some(description), None) => {
                        mutation(
                            connection.as_mut(),
                            UPDATE_DESCRIPTION_SQL,
                            vec![json!(description), json!(role)],
                        )
                        .await?;
                    }
                    (None, Some(permissions)) => {
                        mutation(
                            connection.as_mut(),
                            UPDATE_PERMISSIONS_SQL,
                            vec![json!(permissions), json!(role)],
                        )
                        .await?;
                    }
                    (None, None) => (),
                }
                if let Some(permissions) = permissions {
                    mutation(
                        connection.as_mut(),
                        DELETE_PERMISSIONS_SQL,
                        vec![json!(role)],
                    )
                    .await?;
                    for permission in permissions {
                        mutation(
                            connection.as_mut(),
                            INSERT_PERMISSION_SQL,
                            vec![json!(role), json!(permission)],
                        )
                        .await?;
                    }
                }
            }
            self.invalidate().await
        })
    }
    fn delete_role<'a>(&'a self, role: &'a str) -> RolesStoreFuture<'a, bool> {
        Box::pin(async move {
            let Some(pool) = &self.pool else {
                return Ok(false);
            };
            let mut connection = pool.acquire().await?;
            mutation(
                connection.as_mut(),
                DELETE_PERMISSIONS_SQL,
                vec![json!(role)],
            )
            .await?;
            mutation(
                connection.as_mut(),
                DELETE_USER_ROLES_SQL,
                vec![json!(role)],
            )
            .await?;
            let affected =
                mutation(connection.as_mut(), DELETE_ROLE_SQL, vec![json!(role)]).await?;
            // Deliberately before release, unlike create/update.
            self.invalidate().await?;
            Ok(affected == 1)
        })
    }
    fn list_permissions<'a>(
        &'a self,
        category: Option<&'a str>,
    ) -> RolesStoreFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let (sql, args) = match category.filter(|s| !s.is_empty()) {
                Some(c) => (LIST_CATEGORY_SQL, vec![json!(c)]),
                None => (LIST_PERMISSIONS_SQL, vec![]),
            };
            Ok(self
                .query(sql, args, RolesStatementKind::Permissions)
                .await?
                .rows)
        })
    }
    fn tenant_role_user_count<'a>(&'a self, role: &'a str, tenant: &'a str) -> RolesStoreFuture<'a, i64> {
        Box::pin(async move {
            Ok(self.query(TENANT_COUNT_ROLE_SQL, vec![json!(role), json!(tenant)], RolesStatementKind::Count).await?.count)
        })
    }
    fn tenant_users_by_role<'a>(&'a self, role: &'a str, tenant: &'a str) -> RolesStoreFuture<'a, Vec<Value>> {
        Box::pin(async move {
            Ok(self.query(TENANT_MEMBERS_SQL, vec![json!(role), json!(tenant)], RolesStatementKind::Members).await?.rows)
        })
    }
    fn platform_administrator<'a>(&'a self, user: &'a str, tenant: &'a str) -> RolesStoreFuture<'a, bool> {
        Box::pin(async move {
            Ok(self.query(PLATFORM_ADMIN_SQL, vec![json!(user), json!(tenant)], RolesStatementKind::Count).await?.count == 1)
        })
    }
    fn get_role_user_count<'a>(&'a self, role: &'a str) -> RolesStoreFuture<'a, i64> {
        Box::pin(async move {
            Ok(self
                .query(COUNT_ROLE_SQL, vec![json!(role)], RolesStatementKind::Count)
                .await?
                .count)
        })
    }
    fn get_users_by_role<'a>(&'a self, role: &'a str) -> RolesStoreFuture<'a, Vec<Value>> {
        Box::pin(async move {
            Ok(self
                .query(MEMBERS_SQL, vec![json!(role)], RolesStatementKind::Members)
                .await?
                .rows)
        })
    }
}
/// Source _row_to_dict datetime.isoformat policy belongs to the configured host.
/// The default asyncpg timestamptz codec normally returns UTC-aware values.
pub trait RolesTimestampFormatter: Send + Sync {
    fn format(&self, value: DateTime<Utc>) -> Result<String, RolesStoreError>;
}
pub struct PgRolesPool {
    storage: PgOperationStorage,
    timestamps: Arc<dyn RolesTimestampFormatter>,
}
impl PgRolesPool {
    pub fn new(
        pool: PgPool,
        control_pool: PgPool,
        timestamps: Arc<dyn RolesTimestampFormatter>,
    ) -> Result<Self, RolesStoreError> {
        Ok(Self {
            storage: PgOperationStorage::new(pool, control_pool)?,
            timestamps,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), RolesStoreError> {
        self.storage.drain_cancelled().await.map_err(Into::into)
    }
}
impl RolesPool for PgRolesPool {
    fn acquire(&self) -> RolesStoreFuture<'_, Box<dyn RolesConnection>> {
        Box::pin(async move {
            Ok(Box::new(PgRolesConnection {
                connection: self.storage.acquire().await?,
                timestamps: self.timestamps.clone(),
            }) as Box<dyn RolesConnection>)
        })
    }
}
struct PgRolesConnection {
    connection: PgOperationConnection,
    timestamps: Arc<dyn RolesTimestampFormatter>,
}
impl RolesConnection for PgRolesConnection {
    fn run(&mut self, statement: RolesStatement) -> RolesStoreFuture<'_, RolesSqlResult> {
        Box::pin(async move {
            let kind = statement.kind;
            let (rows, affected) = self
                .connection
                .execute(move |connection| {
                    Box::pin(async move {
                        let mut query = sqlx::query(&statement.sql);
                        for value in &statement.arguments {
                            query = match value {
                                Value::String(value) => query.bind(value),
                                Value::Null => query.bind(Option::<String>::None),
                                Value::Array(values) => query.bind(
                                    values
                                        .iter()
                                        .map(|v| {
                                            v.as_str().ok_or_else(|| {
                                                sqlx::Error::Protocol(
                                                    "roles_invalid_binding".into(),
                                                )
                                            })
                                        })
                                        .collect::<Result<Vec<_>, _>>()?,
                                ),
                                _ => {
                                    return Err(sqlx::Error::Protocol(
                                        "roles_invalid_binding".into(),
                                    ));
                                }
                            };
                        }
                        if kind == RolesStatementKind::Mutation {
                            Ok((vec![], query.execute(connection).await?.rows_affected()))
                        } else {
                            Ok((query.fetch_all(connection).await?, 0))
                        }
                    })
                })
                .await?;
            let error = |_: sqlx::Error| RolesStoreError::Database;
            if kind == RolesStatementKind::Count {
                return Ok(RolesSqlResult {
                    count: rows
                        .first()
                        .map(|r| r.try_get::<i64, _>(0))
                        .transpose()
                        .map_err(error)?
                        .unwrap_or(0),
                    ..Default::default()
                });
            }
            let mut values = vec![];
            for row in rows {
                let mut value = json!({});
                let (texts, timestamps): (&[&str], &[&str]) = match kind {
                    RolesStatementKind::Roles => {
                        value["id"] = json!(row.try_get::<i32, _>("id").map_err(error)?);
                        value["permissions"] = json!(
                            row.try_get::<Vec<String>, _>("permissions")
                                .map_err(error)?
                        );
                        value["is_system"] =
                            json!(row.try_get::<bool, _>("is_system").map_err(error)?);
                        (&["role_name", "description"], &["created_at", "updated_at"])
                    }
                    RolesStatementKind::Permissions => {
                        value["id"] = json!(row.try_get::<i32, _>("id").map_err(error)?);
                        value["is_system"] =
                            json!(row.try_get::<bool, _>("is_system").map_err(error)?);
                        (
                            &[
                                "permission_code",
                                "name",
                                "description",
                                "category",
                                "resource",
                                "action",
                            ],
                            &["created_at", "updated_at"],
                        )
                    }
                    RolesStatementKind::Members => (
                        &["user_id", "email", "display_name", "status"],
                        &["granted_at"],
                    ),
                    _ => return Err(RolesStoreError::InvalidValue),
                };
                for field in texts {
                    value[*field] = json!(row.try_get::<Option<String>, _>(*field).map_err(error)?);
                }
                for field in timestamps {
                    value[*field] = json!(
                        row.try_get::<Option<DateTime<Utc>>, _>(*field)
                            .map_err(error)?
                            .map(|v| self.timestamps.format(v))
                            .transpose()?
                    );
                }
                values.push(value);
            }
            Ok(RolesSqlResult {
                rows: values,
                affected,
                ..Default::default()
            })
        })
    }
}
pub const LIST_ROLES_SQL: &str = "SELECT * FROM rbac_roles ORDER BY is_system DESC, role_name";
pub const GET_ROLE_SQL: &str = "SELECT * FROM rbac_roles WHERE role_name = $1";
pub const CREATE_ROLE_SQL: &str = "INSERT INTO rbac_roles (role_name, description, permissions, is_system) VALUES ($1, $2, $3, FALSE)";
pub const INSERT_PERMISSION_SQL: &str = "INSERT INTO role_permissions (role_name, permission_code) VALUES ($1, $2) ON CONFLICT DO NOTHING";
pub const UPDATE_BOTH_SQL: &str = "UPDATE rbac_roles SET description = $1, permissions = $2, updated_at = NOW() WHERE role_name = $3";
pub const UPDATE_DESCRIPTION_SQL: &str =
    "UPDATE rbac_roles SET description = $1, updated_at = NOW() WHERE role_name = $2";
pub const UPDATE_PERMISSIONS_SQL: &str =
    "UPDATE rbac_roles SET permissions = $1, updated_at = NOW() WHERE role_name = $2";
pub const DELETE_PERMISSIONS_SQL: &str = "DELETE FROM role_permissions WHERE role_name = $1";
pub const DELETE_USER_ROLES_SQL: &str = "DELETE FROM user_roles WHERE role_name = $1";
pub const DELETE_ROLE_SQL: &str =
    "DELETE FROM rbac_roles WHERE role_name = $1 AND is_system = FALSE";
pub const LIST_CATEGORY_SQL: &str =
    "SELECT * FROM permissions WHERE category = $1 ORDER BY permission_code";
pub const LIST_PERMISSIONS_SQL: &str =
    "SELECT * FROM permissions ORDER BY category, permission_code";
pub const COUNT_ROLE_SQL: &str = "SELECT COUNT(*) FROM user_roles WHERE role_name = $1";
pub const MEMBERS_SQL: &str = "SELECT u.user_id, u.email, u.display_name, u.status, ur.granted_at FROM users u JOIN user_roles ur ON u.user_id = ur.user_id WHERE ur.role_name = $1 ORDER BY ur.granted_at DESC";

pub const TENANT_COUNT_ROLE_SQL: &str = "SELECT COUNT(*) FROM users u JOIN user_roles ur ON ur.user_id=u.user_id WHERE ur.role_name=$1 AND u.tenant_id=$2 AND (ur.expires_at IS NULL OR ur.expires_at>clock_timestamp())";
pub const TENANT_MEMBERS_SQL: &str = "SELECT u.user_id,u.email,u.display_name,u.status,ur.granted_at FROM users u JOIN user_roles ur ON ur.user_id=u.user_id WHERE ur.role_name=$1 AND u.tenant_id=$2 AND (ur.expires_at IS NULL OR ur.expires_at>clock_timestamp()) ORDER BY ur.granted_at DESC,u.user_id";
pub const PLATFORM_ADMIN_SQL: &str = "SELECT COUNT(*) FROM users u JOIN tenants t ON t.tenant_id=u.tenant_id WHERE u.user_id=$1 AND u.tenant_id=$2 AND u.status='active' AND t.status='active' AND u.roles @> ARRAY['platform_admin']::varchar[] AND EXISTS(SELECT 1 FROM user_roles ur WHERE ur.user_id=u.user_id AND ur.role_name='platform_admin' AND (ur.expires_at IS NULL OR ur.expires_at>clock_timestamp()))";
