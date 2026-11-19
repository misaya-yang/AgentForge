//! Tenant-scoped account persistence. Construction is inert.
//!
//! All writes use a shared tenant/actor/target guard in one supervised operation.
//! Creation and role assignments are atomic; an ID collision never updates an
//! existing account. Permission invalidation follows commit. See the owning
//! tenant-account-administration architecture document for authority boundaries.
//! Hosts must join aborted callers and drain_cancelled before releasing pools.
mod guard;
#[path = "users_store/object_policy.rs"]
mod object_policy;

use crate::{
    postgres_operation::{PgOperationError, PgOperationStorage},
    service_config_api::{ServiceConfigError, ServiceConfigFuture},
};
use chrono::{DateTime, NaiveDateTime, SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use sqlx::{Column, PgPool, Postgres, QueryBuilder, Row, TypeInfo, ValueRef, postgres::PgRow};
use std::{fmt, sync::Arc};

pub type UsersFuture<'a, T> = ServiceConfigFuture<'a, T>;
pub trait UserPermissionInvalidation: Send + Sync {
    fn invalidate<'a>(&'a self, user_id: &'a str) -> UsersFuture<'a, ()>;
}
/// Valid only for hosts whose permission authority performs fresh reads. Does not
/// clear any Python/remote permission cache; cached hosts must supply that cache.
pub struct FreshUserPermissions;
impl UserPermissionInvalidation for FreshUserPermissions {
    fn invalidate<'a>(&'a self, _user_id: &'a str) -> UsersFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
#[derive(Clone, Default)]
pub struct UserListFilter {
    pub status: Option<String>,
    pub search: Option<String>,
    pub tenant_id: Option<String>,
    pub limit: i64,
    pub offset: i64,
}
#[derive(Clone, Default)]
pub struct UserMutation {
    pub fields: Map<String, Value>,
    pub roles: Option<Vec<String>>,
    pub extra_permissions: Option<Vec<String>>,
    pub granted_by: Option<String>,
    pub profile_only: bool,
}
/// Password hash deliberately has no Debug or Serialize implementation.
pub struct NewUser {
    pub user: Value,
    password_hash: String,
}
impl NewUser {
    pub fn new(user: Value, password_hash: String) -> Self {
        Self {
            user,
            password_hash,
        }
    }
}
pub trait UsersDatabase: Send + Sync {
    /// Effective source object truthiness AND its enabled flag.
    fn enabled(&self) -> bool;
    fn get_user<'a>(&'a self, user_id: &'a str) -> UsersFuture<'a, Option<Value>>;
    fn get_user_for_tenant<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: &'a str,
    ) -> UsersFuture<'a, Option<Value>>;
    fn get_user_by_email<'a>(&'a self, email: &'a str) -> UsersFuture<'a, Option<Value>>;
    fn list_users_paginated<'a>(
        &'a self,
        filter: &'a UserListFilter,
    ) -> UsersFuture<'a, (Vec<Value>, i64)>;
    fn save_user_with_password<'a>(&'a self, user: &'a NewUser) -> UsersFuture<'a, ()>;
    fn reset_user_password_for_tenant<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: &'a str,
        password_hash: &'a str,
        actor_id: &'a str,
    ) -> UsersFuture<'a, bool>;
    fn update_user_for_tenant<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: &'a str,
        mutation: &'a UserMutation,
    ) -> UsersFuture<'a, bool>;
    fn delete_user_for_tenant<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: &'a str,
        actor_id: &'a str,
    ) -> UsersFuture<'a, bool>;
    fn get_user_extra_permissions<'a>(&'a self, user_id: &'a str) -> UsersFuture<'a, Vec<Value>>;
}
#[derive(Clone, PartialEq, Eq)]
pub enum UsersStoreError {
    Database,
    Cancelled,
    InvalidCancellationPool,
    IndeterminateCancellation,
}
impl fmt::Debug for UsersStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UsersStoreError(redacted)")
    }
}
impl fmt::Display for UsersStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("users_store_unavailable")
    }
}
impl std::error::Error for UsersStoreError {}
impl From<PgOperationError> for UsersStoreError {
    fn from(error: PgOperationError) -> Self {
        match error {
            PgOperationError::Cancelled => Self::Cancelled,
            PgOperationError::InvalidCancellationPool => Self::InvalidCancellationPool,
            PgOperationError::IndeterminateCancellation => Self::IndeterminateCancellation,
            PgOperationError::Database { .. } => Self::Database,
        }
    }
}
fn error(error: PgOperationError) -> ServiceConfigError {
    if error == PgOperationError::Cancelled {
        ServiceConfigError::Cancelled
    } else if error.sqlstate() == Some("23505") {
        guard::Denied::Conflict.response()
    } else {
        ServiceConfigError::Internal
    }
}
macro_rules! authorize {
    ($check:expr) => {
        match $check? {
            Ok(value) => value,
            Err(denied) => return Ok(Err(denied)),
        }
    };
}
pub struct PgUsersDatabase {
    enabled: bool,
    operations: Option<PgOperationStorage>,
    invalidation: Arc<dyn UserPermissionInvalidation>,
}
impl PgUsersDatabase {
    pub fn new(
        pool: PgPool,
        independent_control_pool: PgPool,
        invalidation: Arc<dyn UserPermissionInvalidation>,
    ) -> Result<Self, UsersStoreError> {
        Ok(Self {
            enabled: true,
            operations: Some(PgOperationStorage::new(pool, independent_control_pool)?),
            invalidation,
        })
    }
    pub fn disconnected(enabled: bool, invalidation: Arc<dyn UserPermissionInvalidation>) -> Self {
        Self {
            enabled,
            operations: None,
            invalidation,
        }
    }
    pub async fn drain_cancelled(&self) -> Result<(), UsersStoreError> {
        if let Some(operations) = &self.operations {
            operations.drain_cancelled().await?;
        }
        Ok(())
    }
    async fn find(
        &self,
        query: &'static str,
        first: &str,
        second: Option<&str>,
    ) -> Result<Option<Value>, ServiceConfigError> {
        let Some(operations) = &self.operations else {
            return Ok(None);
        };
        let first = first.to_owned();
        let second = second.map(str::to_owned);
        operations
            .acquire()
            .await
            .map_err(error)?
            .execute(move |connection| {
                Box::pin(async move {
                    let mut query = sqlx::query(query).bind(first);
                    if let Some(second) = second {
                        query = query.bind(second);
                    }
                    query.fetch_optional(connection).await?.map(row).transpose()
                })
            })
            .await
            .map_err(error)
    }
}
fn datetime(value: DateTime<Utc>) -> Value {
    json!(value.to_rfc3339_opts(
        if value.timestamp_subsec_micros() == 0 {
            SecondsFormat::Secs
        } else {
            SecondsFormat::Micros
        },
        false
    ))
}
/// Mirrors _row_to_dict on the canonical schema without serializing secrets to
/// HTTP. Unknown non-response columns are intentionally not projected by the API.
fn row(row: PgRow) -> Result<Value, sqlx::Error> {
    let mut out = Map::new();
    for column in row.columns() {
        let name = column.name();
        if row.try_get_raw(name)?.is_null() {
            out.insert(name.into(), Value::Null);
            continue;
        }
        let value = match column.type_info().name() {
            "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => json!(row.try_get::<String, _>(name)?),
            "BOOL" => json!(row.try_get::<bool, _>(name)?),
            "INT2" => json!(row.try_get::<i16, _>(name)?),
            "INT4" => json!(row.try_get::<i32, _>(name)?),
            "INT8" => json!(row.try_get::<i64, _>(name)?),
            "TIMESTAMPTZ" => datetime(row.try_get(name)?),
            "TIMESTAMP" => {
                let t: NaiveDateTime = row.try_get(name)?;
                json!(
                    t.format(if t.and_utc().timestamp_subsec_micros() == 0 {
                        "%Y-%m-%dT%H:%M:%S"
                    } else {
                        "%Y-%m-%dT%H:%M:%S%.6f"
                    })
                    .to_string()
                )
            }
            "TEXT[]" | "VARCHAR[]" | "_text" | "_varchar" => {
                json!(row.try_get::<Vec<Option<String>>, _>(name)?)
            }
            "JSON" | "JSONB" => {
                let value: Value = row.try_get(name)?;
                if matches!(name, "metadata" | "roles") {
                    value
                } else {
                    json!(
                        row.try_get::<sqlx::types::Json<Box<serde_json::value::RawValue>>, _>(
                            name
                        )?
                        .0
                        .get()
                    )
                }
            }
            _ => continue,
        };
        let value = if matches!(name, "metadata" | "roles")
            && value.is_string()
            && !matches!(column.type_info().name(), "JSON" | "JSONB")
        {
            serde_json::from_str(value.as_str().expect("string")).unwrap_or_else(|_| {
                if name == "metadata" {
                    json!({})
                } else {
                    json!([])
                }
            })
        } else {
            value
        };
        out.insert(name.into(), value);
    }
    Ok(Value::Object(out))
}
fn binding_error() -> sqlx::Error {
    sqlx::Error::Protocol("invalid user binding".into())
}
fn string(value: &Value, name: &str, default: Option<&str>) -> Result<Option<String>, sqlx::Error> {
    match value.get(name) {
        None => Ok(default.map(str::to_owned)),
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        _ => Err(binding_error()),
    }
}
fn strings(
    value: &Value,
    name: &str,
    default: &[&str],
) -> Result<Option<Vec<Option<String>>>, sqlx::Error> {
    match value.get(name) {
        None => Ok(Some(default.iter().map(|s| Some((*s).into())).collect())),
        Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| match v {
                Value::Null => Ok(None),
                Value::String(s) => Ok(Some(s.clone())),
                _ => Err(binding_error()),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        _ => Err(binding_error()),
    }
}
impl UsersDatabase for PgUsersDatabase {
    fn enabled(&self) -> bool {
        self.enabled
    }
    fn get_user<'a>(&'a self, user_id: &'a str) -> UsersFuture<'a, Option<Value>> {
        Box::pin(self.find("SELECT * FROM users WHERE user_id = $1", user_id, None))
    }
    fn get_user_for_tenant<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: &'a str,
    ) -> UsersFuture<'a, Option<Value>> {
        Box::pin(self.find(
            "SELECT * FROM users WHERE user_id = $1 AND tenant_id = $2",
            user_id,
            Some(tenant_id),
        ))
    }
    fn get_user_by_email<'a>(&'a self, email: &'a str) -> UsersFuture<'a, Option<Value>> {
        Box::pin(self.find(
            "SELECT * FROM users WHERE LOWER(email) = LOWER($1)",
            email,
            None,
        ))
    }
    fn list_users_paginated<'a>(
        &'a self,
        filter: &'a UserListFilter,
    ) -> UsersFuture<'a, (Vec<Value>, i64)> {
        Box::pin(async move {
            let Some(operations) = &self.operations else {
                return Ok((vec![], 0));
            };
            let mut suffix = " FROM users WHERE 1=1".to_owned();
            let mut params = vec![];
            for (name, value) in [("status", &filter.status), ("tenant_id", &filter.tenant_id)] {
                if let Some(value) = value.as_ref().filter(|v| !v.is_empty()) {
                    params.push(value.clone());
                    suffix.push_str(&format!(" AND {name} = ${}", params.len()));
                }
            }
            if let Some(search) = filter.search.as_ref().filter(|v| !v.is_empty()) {
                params.push(format!("%{search}%"));
                let n = params.len();
                suffix.push_str(&format!(
                    " AND (email ILIKE ${n} OR display_name ILIKE ${n} OR username ILIKE ${n})"
                ));
            }
            let mut connection = operations.acquire().await.map_err(error)?;
            let count_query = format!("SELECT COUNT(*){suffix}");
            let count_params = params.clone();
            let total: i64 = connection
                .execute(move |connection| {
                    Box::pin(async move {
                        let mut query = sqlx::query_scalar(&count_query);
                        for p in count_params {
                            query = query.bind(p);
                        }
                        query.fetch_one(connection).await
                    })
                })
                .await
                .map_err(error)?;
            let n = params.len() + 1;
            let query = format!(
                "SELECT *{suffix} ORDER BY created_at DESC LIMIT ${n} OFFSET ${}",
                n + 1
            );
            let limit = filter.limit;
            let offset = filter.offset;
            let rows = connection
                .execute(move |connection| {
                    Box::pin(async move {
                        let mut query = sqlx::query(&query);
                        for p in params {
                            query = query.bind(p);
                        }
                        query
                            .bind(limit)
                            .bind(offset)
                            .fetch_all(connection)
                            .await?
                            .into_iter()
                            .map(row)
                            .collect::<Result<Vec<_>, _>>()
                    })
                })
                .await
                .map_err(error)?;
            Ok((rows, total))
        })
    }
    fn save_user_with_password<'a>(&'a self, input: &'a NewUser) -> UsersFuture<'a, ()> {
        Box::pin(async move {
            let operations = self
                .operations
                .as_ref()
                .ok_or_else(|| ServiceConfigError::detail(503, "Database not available"))?;
            let u = input.user.clone();
            let user_id = input.user["user_id"]
                .as_str()
                .ok_or(ServiceConfigError::Internal)?
                .to_owned();
            let hash = input.password_hash.clone();
            object_policy::execute(operations,move |connection| Box::pin(async move {
                let tenant = string(&u, "tenant_id", None)?.ok_or_else(binding_error)?;
                let actor = string(&u, "created_by", None)?.ok_or_else(binding_error)?;
                let id = string(&u, "user_id", None)?.ok_or_else(binding_error)?;
                let email = string(&u, "email", None)?.ok_or_else(binding_error)?;
                let roles = strings(&u, "roles", &["user"])?;
                let roles = roles.ok_or_else(binding_error)?.into_iter()
                    .collect::<Option<Vec<_>>>().ok_or_else(binding_error)?;
                authorize!(guard::actor(connection, &tenant, &actor, false).await);
                authorize!(guard::roles(connection, &roles).await);
                if id == "admin" { return Ok(Err(guard::Denied::Protected)); }
                // The existing unique lower(email) index also protects races
                // across tenants; conflicts are returned, never upserted.
                let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE user_id=$1 OR LOWER(email)=LOWER($2))")
                    .bind(&id).bind(&email).fetch_one(&mut *connection).await?;
                if exists { return Ok(Err(guard::Denied::Conflict)); }
                let username = string(&u, "username", None)?.unwrap_or_else(|| id.clone());
                sqlx::query("INSERT INTO users (user_id,username,email,display_name,department,tenant_id,tier,roles,permissions,quota_config,status,password_hash,force_password_change,email_verified,created_by,metadata,password_changed_at) VALUES ($1,$2,$3,$4,$5,$6,'normal',$7,ARRAY[]::varchar[],'{}'::jsonb,'active',$8,TRUE,FALSE,$9,'{}'::jsonb,clock_timestamp())")
                    .bind(&id).bind(username).bind(email).bind(string(&u,"display_name",None)?)
                    .bind(string(&u,"department",None)?).bind(&tenant).bind(&roles).bind(hash)
                    .bind(&actor).execute(&mut *connection).await?;
                for role in &roles {
                    sqlx::query("INSERT INTO user_roles(user_id,role_name,granted_by) VALUES($1,$2,$3) ON CONFLICT(user_id,role_name) DO NOTHING")
                        .bind(&id).bind(role).bind(&actor).execute(&mut *connection).await?;
                }
                guard::audit(connection, &tenant, &actor, &id, "user_created").await?;
                Ok(Ok(()))
            })).await.map_err(error)?.map_err(guard::Denied::response)?;
            self.invalidation.invalidate(&user_id).await
        })
    }
    fn reset_user_password_for_tenant<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: &'a str,
        hash: &'a str,
        actor_id: &'a str,
    ) -> UsersFuture<'a, bool> {
        Box::pin(async move {
            let operations = self
                .operations
                .as_ref()
                .ok_or_else(|| ServiceConfigError::detail(503, "Database not available"))?;
            let (id, tenant, hash, actor) = (
                user_id.to_owned(),
                tenant_id.to_owned(),
                hash.to_owned(),
                actor_id.to_owned(),
            );
            object_policy::execute(operations,move |connection| Box::pin(async move {
                authorize!(guard::actor(connection, &tenant, &actor, false).await);
                authorize!(guard::target(connection, &tenant, &actor, &id, false).await);
                sqlx::query("UPDATE users SET password_hash=$1,force_password_change=TRUE,password_changed_at=clock_timestamp(),login_attempts=0,locked_until=NULL,updated_at=NOW() WHERE user_id=$2 AND tenant_id=$3")
                    .bind(hash).bind(&id).bind(&tenant).execute(&mut *connection).await?;
                guard::audit(connection, &tenant, &actor, &id, "user_password_reset").await?;
                Ok(Ok(true))
            })).await.map_err(error)?.map_err(guard::Denied::response)
        })
    }
    fn update_user_for_tenant<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: &'a str,
        mutation: &'a UserMutation,
    ) -> UsersFuture<'a, bool> {
        Box::pin(async move {
            let operations = self
                .operations
                .as_ref()
                .ok_or_else(|| ServiceConfigError::detail(503, "Database not available"))?;
            let (id, tenant, mutation) =
                (user_id.to_owned(), tenant_id.to_owned(), mutation.clone());
            let actor = mutation
                .granted_by
                .clone()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| guard::Denied::Identity.response())?;
            let allowed: &[&str] = if mutation.profile_only {
                &["display_name"]
            } else {
                &["display_name", "department", "tier", "status", "metadata"]
            };
            if mutation
                .fields
                .keys()
                .any(|field| !allowed.contains(&field.as_str()))
                || (mutation.profile_only
                    && (mutation.roles.is_some() || mutation.extra_permissions.is_some()))
            {
                return Err(ServiceConfigError::detail(
                    422,
                    "Unsupported account fields",
                ));
            }
            if mutation
                .fields
                .get("status")
                .is_some_and(|v| !matches!(v.as_str(), Some("active" | "disabled")))
                || mutation.fields.get("tier").is_some_and(|v| {
                    !matches!(
                        v.as_str(),
                        Some("normal" | "premium" | "enterprise" | "admin")
                    )
                })
            {
                return Err(ServiceConfigError::detail(
                    422,
                    "Invalid account status or tier",
                ));
            }
            let invalidate = mutation.roles.is_some() || mutation.extra_permissions.is_some();
            let updated = object_policy::execute(operations,move |connection| Box::pin(async move {
                authorize!(guard::actor(connection, &tenant, &actor, mutation.profile_only).await);
                let before = authorize!(guard::target(connection, &tenant, &actor, &id, mutation.profile_only).await);
                if let Some(roles) = &mutation.roles { authorize!(guard::roles(connection, roles).await); }
                if let Some(permissions) = &mutation.extra_permissions { authorize!(guard::permissions(connection, permissions).await); }
                authorize!(guard::retain_administrator(connection, &tenant, &id, &before,
                    mutation.roles.as_deref().unwrap_or(&before.roles),
                    mutation.fields.get("status").and_then(Value::as_str).unwrap_or(&before.status)).await);
                let disabling = mutation.fields.get("status").and_then(Value::as_str) == Some("disabled");
                let mut builder: QueryBuilder<Postgres> = QueryBuilder::new("UPDATE users SET updated_at=NOW()");
                for (key, value) in mutation.fields {
                    builder.push(", ").push(&key).push(" = ");
                    if key == "metadata" {
                        if !value.is_object() { return Err(binding_error()); }
                        builder.push_bind(value);
                    } else {
                        let value = match value { Value::Null => None, Value::String(v) => Some(v), _ => return Err(binding_error()) };
                        builder.push_bind(value);
                    }
                }
                if disabling { builder.push(", password_changed_at=clock_timestamp()"); }
                if mutation.roles.is_some() || mutation.extra_permissions.is_some() {
                    // Canonical assignments replace legacy fallback authority.
                    builder.push(", permissions=ARRAY[]::varchar[]");
                }
                builder.push(" WHERE user_id = ").push_bind(&id).push(" AND tenant_id = ").push_bind(&tenant);
                builder.build().execute(&mut *connection).await?;
                if disabling {
                    sqlx::query("UPDATE api_keys SET enabled=FALSE WHERE user_id=$1 AND tenant_id=$2")
                        .bind(&id).bind(&tenant).execute(&mut *connection).await?;
                }
                if let Some(roles) = mutation.roles {
                    sqlx::query("DELETE FROM user_roles WHERE user_id=$1").bind(&id).execute(&mut *connection).await?;
                    for role in &roles {
                        sqlx::query("INSERT INTO user_roles(user_id,role_name,granted_by) VALUES($1,$2,$3) ON CONFLICT(user_id,role_name) DO NOTHING")
                            .bind(&id).bind(role).bind(&actor).execute(&mut *connection).await?;
                    }
                    sqlx::query("UPDATE users SET roles=$1,updated_at=NOW() WHERE user_id=$2 AND tenant_id=$3")
                        .bind(roles).bind(&id).bind(&tenant).execute(&mut *connection).await?;
                }
                if let Some(permissions) = mutation.extra_permissions {
                    sqlx::query("DELETE FROM user_permissions WHERE user_id=$1").bind(&id).execute(&mut *connection).await?;
                    for permission in permissions {
                        sqlx::query("INSERT INTO user_permissions(user_id,permission_code,granted_by) VALUES($1,$2,$3) ON CONFLICT(user_id,permission_code) DO NOTHING")
                            .bind(&id).bind(permission).bind(&actor).execute(&mut *connection).await?;
                    }
                }
                guard::audit(connection, &tenant, &actor, &id, if mutation.profile_only { "user_profile_updated" } else { "user_updated" }).await?;
                Ok(Ok(true))
            })).await.map_err(error)?.map_err(guard::Denied::response)?;
            if updated && invalidate {
                self.invalidation.invalidate(user_id).await?;
            }
            Ok(updated)
        })
    }
    fn delete_user_for_tenant<'a>(
        &'a self,
        user_id: &'a str,
        tenant_id: &'a str,
        actor_id: &'a str,
    ) -> UsersFuture<'a, bool> {
        Box::pin(async move {
            let operations = self
                .operations
                .as_ref()
                .ok_or_else(|| ServiceConfigError::detail(503, "Database not available"))?;
            let (id, tenant, actor) = (
                user_id.to_owned(),
                tenant_id.to_owned(),
                actor_id.to_owned(),
            );
            let deleted = object_policy::execute(operations, move |connection| {
                Box::pin(async move {
                    authorize!(guard::actor(connection, &tenant, &actor, false).await);
                    let before =
                        authorize!(guard::target(connection, &tenant, &actor, &id, false).await);
                    authorize!(
                        guard::retain_administrator(
                            connection,
                            &tenant,
                            &id,
                            &before,
                            &[],
                            "disabled"
                        )
                        .await
                    );
                    sqlx::query("DELETE FROM user_roles WHERE user_id=$1")
                        .bind(&id)
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query("DELETE FROM user_permissions WHERE user_id=$1")
                        .bind(&id)
                        .execute(&mut *connection)
                        .await?;
                    sqlx::query(
                        "UPDATE api_keys SET enabled=FALSE WHERE user_id=$1 AND tenant_id=$2",
                    )
                    .bind(&id)
                    .bind(&tenant)
                    .execute(&mut *connection)
                    .await?;
                    sqlx::query("DELETE FROM users WHERE user_id=$1 AND tenant_id=$2")
                        .bind(&id)
                        .bind(&tenant)
                        .execute(&mut *connection)
                        .await?;
                    guard::audit(connection, &tenant, &actor, &id, "user_deleted").await?;
                    Ok(Ok(true))
                })
            })
            .await
            .map_err(error)?
            .map_err(guard::Denied::response)?;
            if deleted {
                self.invalidation.invalidate(user_id).await?;
            }
            Ok(deleted)
        })
    }
    fn get_user_extra_permissions<'a>(&'a self, user_id: &'a str) -> UsersFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let Some(operations) = &self.operations else {
                return Ok(vec![]);
            };
            // Authority-read failures must not appear as an empty grant list.
            let mut connection = operations.acquire().await.map_err(error)?;
            let id = user_id.to_owned();
            match connection.execute(move |connection| Box::pin(async move {
                sqlx::query("SELECT permission_code,granted_by,granted_at,expires_at,note FROM user_permissions WHERE user_id=$1 AND (expires_at IS NULL OR expires_at>NOW()) ORDER BY granted_at DESC").bind(id).fetch_all(connection).await?.into_iter().map(row).collect()
            })).await {
                Ok(rows) => Ok(rows),
                Err(e) => Err(error(e)),
            }
        })
    }
}
