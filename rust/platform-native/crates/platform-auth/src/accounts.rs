//! Existing-schema PostgreSQL adapter. No schema creation or migration is performed.
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use std::collections::BTreeSet;
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum StoreError {
    #[error("Account authority unavailable")]
    Unavailable,
    #[error("Account authority changed")]
    Changed,
    #[error("Invalid authority request")]
    InvalidInput,
}
pub struct Authority {
    pub roles: Vec<String>,
    pub tier: String,
    pub permissions: Vec<String>,
}
#[derive(FromRow)]
pub struct Account {
    pub user_id: String,
    pub tenant_id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub department: Option<String>,
    pub tier: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub status: String,
    pub password_hash: Option<String>,
    pub force_password_change: bool,
    pub login_attempts: i32,
    pub locked_until: Option<DateTime<Utc>>,
    pub password_changed_at: Option<DateTime<Utc>>,
}
pub struct AuditContext {
    pub ip: String,
    pub user_agent: String,
}
pub enum AuditAction {
    MissingUser,
    DisabledAccount,
    DisabledTenant,
    Logout,
}
pub struct PgAccounts {
    pub(crate) pool: PgPool,
}

fn project_authority(
    legacy_roles: Vec<String>,
    current_roles: Vec<String>,
    tier: String,
    role_permissions: Vec<String>,
    role_fallback_permissions: Vec<String>,
    legacy_permissions: Vec<String>,
    legacy_role_permissions: Vec<String>,
    direct_permissions: Vec<String>,
    current_only: bool,
) -> Authority {
    let mut permissions = role_permissions;
    if permissions.is_empty() {
        permissions = role_fallback_permissions;
    }
    if permissions.is_empty() && !current_only {
        permissions = legacy_permissions;
        permissions.extend(legacy_role_permissions);
    }
    permissions.extend(direct_permissions);
    Authority {
        roles: if current_only {
            current_roles
        } else {
            legacy_roles
        },
        tier,
        permissions: permissions
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    }
}

impl PgAccounts {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn audit_event(
        &self,
        user: Option<&str>,
        email: Option<&str>,
        action: AuditAction,
        context: &AuditContext,
    ) -> Result<(), StoreError> {
        let (action, reason) = match action {
            AuditAction::MissingUser => ("login_failed", Some("user_not_found")),
            AuditAction::DisabledAccount => ("login_failed", Some("account_disabled")),
            AuditAction::DisabledTenant => ("login_failed", Some("tenant_disabled")),
            AuditAction::Logout => ("logout", None),
        };
        let details = reason.map_or_else(
            || serde_json::json!({}),
            |reason| serde_json::json!({"reason":reason}),
        );
        sqlx::query("INSERT INTO gateway.login_audit(user_id,email,action,ip_address,user_agent,details) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(user).bind(email).bind(action).bind(&context.ip).bind(&context.user_agent).bind(details).execute(&self.pool).await.map_err(|_|StoreError::Unavailable)?;
        Ok(())
    }
    pub async fn by_email(&self, email: &str) -> Result<Option<Account>, StoreError> {
        sqlx::query_as::<_,Account>("SELECT user_id,tenant_id,email,display_name,department,tier,roles,permissions,status,password_hash,force_password_change,login_attempts,locked_until,password_changed_at FROM gateway.users WHERE LOWER(email)=LOWER($1)")
            .bind(email).fetch_optional(&self.pool).await.map_err(|_|StoreError::Unavailable)
    }
    pub async fn by_id(&self, user: &str) -> Result<Option<Account>, StoreError> {
        sqlx::query_as::<_,Account>("SELECT user_id,tenant_id,email,display_name,department,tier,roles,permissions,status,password_hash,force_password_change,login_attempts,locked_until,password_changed_at FROM gateway.users WHERE user_id=$1")
            .bind(user).fetch_optional(&self.pool).await.map_err(|_|StoreError::Unavailable)
    }
    pub async fn tenant_active(&self, tenant: &str) -> Result<bool, StoreError> {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM gateway.tenants WHERE tenant_id=$1 AND status='active')",
        )
        .bind(tenant)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| StoreError::Unavailable)
    }
    /// Resolve current request authority without falling back to denormalized
    /// account fields that can retain removed or expired grants.
    pub async fn authority(&self, user: &str, tenant: &str) -> Result<Authority, StoreError> {
        self.authority_with_mode(user, tenant, true).await
    }
    /// Named form for call sites where the live-authority boundary should be
    /// explicit in the code.
    pub async fn current_authority(
        &self,
        user: &str,
        tenant: &str,
    ) -> Result<Authority, StoreError> {
        self.authority(user, tenant).await
    }
    /// Compatibility projection for login and read-only profile metadata.
    /// Protected-route admission must use `authority`/`current_authority`.
    pub async fn legacy_authority(
        &self,
        user: &str,
        tenant: &str,
    ) -> Result<Authority, StoreError> {
        self.authority_with_mode(user, tenant, false).await
    }
    async fn authority_with_mode(
        &self,
        user: &str,
        tenant: &str,
        current_only: bool,
    ) -> Result<Authority, StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let authority = Self::authority_in_mode(&mut tx, user, tenant, current_only).await?;
        tx.commit().await.map_err(|_| StoreError::Unavailable)?;
        Ok(authority)
    }
    pub(crate) async fn current_authority_in(
        connection: &mut sqlx::PgConnection,
        user: &str,
        tenant: &str,
    ) -> Result<Authority, StoreError> {
        Self::authority_in_mode(connection, user, tenant, true).await
    }
    async fn authority_in_mode(
        connection: &mut sqlx::PgConnection,
        user: &str,
        tenant: &str,
        current_only: bool,
    ) -> Result<Authority, StoreError> {
        let (legacy_roles,legacy_permissions,tier):(Vec<String>,Vec<String>,String)=sqlx::query_as("SELECT u.roles,u.permissions,u.tier FROM gateway.users u JOIN gateway.tenants t ON t.tenant_id=u.tenant_id WHERE u.user_id=$1 AND u.tenant_id=$2 AND u.status='active' AND t.status='active'")
            .bind(user).bind(tenant).fetch_optional(&mut *connection).await.map_err(|_|StoreError::Unavailable)?.ok_or(StoreError::Changed)?;
        let current_roles: Vec<String> = sqlx::query_scalar("SELECT role_name FROM gateway.user_roles WHERE user_id=$1 AND (expires_at IS NULL OR expires_at>NOW()) ORDER BY granted_at")
            .bind(user).fetch_all(&mut *connection).await.map_err(|_|StoreError::Unavailable)?;
        let role_permissions:Vec<String>=sqlx::query_scalar("SELECT DISTINCT rp.permission_code FROM gateway.user_roles ur JOIN gateway.role_permissions rp ON ur.role_name=rp.role_name WHERE ur.user_id=$1 AND (ur.expires_at IS NULL OR ur.expires_at>NOW())")
            .bind(user).fetch_all(&mut *connection).await.map_err(|_|StoreError::Unavailable)?;
        let role_fallback_permissions: Vec<String> = if role_permissions.is_empty() {
            sqlx::query_scalar("SELECT DISTINCT unnest(rr.permissions) FROM gateway.user_roles ur JOIN gateway.rbac_roles rr ON ur.role_name=rr.role_name WHERE ur.user_id=$1 AND (ur.expires_at IS NULL OR ur.expires_at>NOW())")
                .bind(user).fetch_all(&mut *connection).await.map_err(|_|StoreError::Unavailable)?
        } else {
            Vec::new()
        };
        let (legacy_role_permissions, direct_permissions) = if role_permissions.is_empty()
            && role_fallback_permissions.is_empty()
            && !current_only
        {
            let rows: Vec<Vec<String>> = sqlx::query_scalar(
                "SELECT permissions FROM gateway.rbac_roles WHERE role_name=ANY($1)",
            )
            .bind(&legacy_roles)
            .fetch_all(&mut *connection)
            .await
            .map_err(|_| StoreError::Unavailable)?;
            let direct: Vec<String> = sqlx::query_scalar("SELECT permission_code FROM gateway.user_permissions WHERE user_id=$1 AND (expires_at IS NULL OR expires_at>NOW())")
            .bind(user).fetch_all(&mut *connection).await.map_err(|_|StoreError::Unavailable)?;
            (rows.into_iter().flatten().collect(), direct)
        } else {
            let direct: Vec<String> = sqlx::query_scalar("SELECT permission_code FROM gateway.user_permissions WHERE user_id=$1 AND (expires_at IS NULL OR expires_at>NOW())")
                .bind(user).fetch_all(&mut *connection).await.map_err(|_|StoreError::Unavailable)?;
            (Vec::new(), direct)
        };
        Ok(project_authority(
            legacy_roles,
            current_roles,
            tier,
            role_permissions,
            role_fallback_permissions,
            legacy_permissions,
            legacy_role_permissions,
            direct_permissions,
            current_only,
        ))
    }
    /// A single row update atomically increments attempts and establishes the lock.
    /// The expected hash prevents racing a password reset with a stale verification.
    pub async fn failed_password(
        &self,
        user: &str,
        tenant: &str,
        expected_hash: Option<&str>,
        audit: &AuditContext,
    ) -> Result<i32, StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let attempts:i32=sqlx::query_scalar("UPDATE gateway.users SET login_attempts=login_attempts+1, locked_until=CASE WHEN login_attempts+1>=5 THEN NOW()+INTERVAL '30 minutes' ELSE locked_until END, updated_at=NOW() WHERE user_id=$1 AND tenant_id=$2 AND password_hash IS NOT DISTINCT FROM $3 AND status='active' AND (locked_until IS NULL OR locked_until<=NOW()) RETURNING login_attempts")
            .bind(user).bind(tenant).bind(expected_hash).fetch_optional(&mut *tx).await.map_err(|_|StoreError::Unavailable)?.ok_or(StoreError::Changed)?;
        sqlx::query("INSERT INTO gateway.login_audit(user_id,action,ip_address,user_agent,details) VALUES($1,'login_failed',$2,$3,$4)")
            .bind(user).bind(&audit.ip).bind(&audit.user_agent).bind(serde_json::json!({"reason":"invalid_password","attempts":attempts})).execute(&mut *tx).await.map_err(|_|StoreError::Unavailable)?;
        tx.commit().await.map_err(|_| StoreError::Unavailable)?;
        Ok(attempts)
    }
    pub async fn successful_password(
        &self,
        user: &str,
        tenant: &str,
        expected_hash: &str,
        audit: &AuditContext,
    ) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let changed=sqlx::query("UPDATE gateway.users SET login_attempts=0,locked_until=NULL,last_login_at=NOW(),last_active_at=NOW(),last_login_ip=$4 WHERE user_id=$1 AND tenant_id=$2 AND password_hash=$3 AND status='active' AND (locked_until IS NULL OR locked_until<=NOW())")
            .bind(user).bind(tenant).bind(expected_hash).bind(&audit.ip).execute(&mut *tx).await.map_err(|_|StoreError::Unavailable)?.rows_affected();
        if changed != 1 {
            return Err(StoreError::Changed);
        }
        sqlx::query("INSERT INTO gateway.login_audit(user_id,action,ip_address,user_agent) VALUES($1,'login_success',$2,$3)")
            .bind(user).bind(&audit.ip).bind(&audit.user_agent).execute(&mut *tx).await.map_err(|_|StoreError::Unavailable)?;
        tx.commit().await.map_err(|_| StoreError::Unavailable)
    }
    pub async fn change_password(
        &self,
        user: &str,
        tenant: &str,
        old_hash: &str,
        new_hash: &str,
        audit: &AuditContext,
    ) -> Result<(), StoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let changed=sqlx::query("UPDATE gateway.users SET password_hash=$4,force_password_change=FALSE,password_changed_at=clock_timestamp(),updated_at=NOW() WHERE user_id=$1 AND tenant_id=$2 AND password_hash=$3 AND status='active'")
            .bind(user).bind(tenant).bind(old_hash).bind(new_hash).execute(&mut *tx).await.map_err(|_|StoreError::Unavailable)?.rows_affected();
        if changed != 1 {
            return Err(StoreError::Changed);
        }
        sqlx::query("INSERT INTO gateway.login_audit(user_id,action,ip_address,user_agent) VALUES($1,'password_change',$2,$3)")
            .bind(user).bind(&audit.ip).bind(&audit.user_agent).execute(&mut *tx).await.map_err(|_|StoreError::Unavailable)?;
        tx.commit().await.map_err(|_| StoreError::Unavailable)
    }
}
