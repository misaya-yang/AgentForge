//! Shared write boundary for the existing one-account/one-tenant schema.
//! All checks run before writes inside the caller's supervised transaction.
use crate::service_config_api::ServiceConfigError;
use serde_json::json;
use sqlx::{PgConnection, Row};

#[derive(Clone, Copy)]
pub(super) enum Denied {
    Identity,
    Administrator,
    Missing,
    Protected,
    SelfMutation,
    LastAdministrator,
    InvalidRole,
    InvalidPermission,
    Conflict,
}
impl Denied {
    pub(super) fn response(self) -> ServiceConfigError {
        let (status, detail) = match self {
            Self::Identity => (403, "Active tenant account required"),
            Self::Administrator => (403, "Tenant administrator role required"),
            Self::Missing => (404, "User not found"),
            Self::Protected => (403, "Platform operator accounts cannot be managed here"),
            Self::SelfMutation => (400, "Use your profile or password settings to change your own account"),
            Self::LastAdministrator => (409, "The tenant must retain an active administrator"),
            Self::InvalidRole => (422, "Roles must be existing non-platform roles"),
            Self::InvalidPermission => (422, "Unknown or invalid direct permission"),
            Self::Conflict => (409, "Account already exists; refresh before trying again"),
        };
        ServiceConfigError::detail(status, detail)
    }
}
pub(super) struct Snapshot {
    pub roles: Vec<String>,
    pub status: String,
}
pub(super) fn administrator(roles: &[String]) -> bool {
    roles.iter().any(|role| matches!(role.as_str(), "admin" | "platform_admin"))
}
/// Lock order is tenant, actor, target. All native account mutations use this
/// order, so two administrators cannot concurrently remove each other's access.
pub(super) async fn actor(
    connection: &mut PgConnection,
    tenant: &str,
    actor: &str,
    profile_only: bool,
) -> Result<Result<(), Denied>, sqlx::Error> {
    if tenant.trim().is_empty() || actor.trim().is_empty() {
        return Ok(Err(Denied::Identity));
    }
    let active: Option<String> = sqlx::query_scalar(
        "SELECT status FROM tenants WHERE tenant_id=$1 FOR UPDATE",
    ).bind(tenant).fetch_optional(&mut *connection).await?;
    if active.as_deref() != Some("active") {
        return Ok(Err(Denied::Identity));
    }
    let row = sqlx::query("SELECT roles,status FROM users WHERE user_id=$1 AND tenant_id=$2 FOR UPDATE")
        .bind(actor).bind(tenant).fetch_optional(&mut *connection).await?;
    let Some(row) = row else { return Ok(Err(Denied::Identity)); };
    let roles: Vec<String> = row.try_get("roles")?;
    if row.try_get::<String, _>("status")? != "active" {
        return Ok(Err(Denied::Identity));
    }
    if !profile_only {
        let current: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM user_roles WHERE user_id=$1 AND role_name=ANY($2) AND role_name IN ('admin','platform_admin') AND (expires_at IS NULL OR expires_at>clock_timestamp()))")
            .bind(actor).bind(&roles).fetch_one(&mut *connection).await?;
        if !administrator(&roles) || !current { return Ok(Err(Denied::Administrator)); }
    }
    Ok(Ok(()))
}
pub(super) async fn target(
    connection: &mut PgConnection,
    tenant: &str,
    actor: &str,
    target: &str,
    profile_only: bool,
) -> Result<Result<Snapshot, Denied>, sqlx::Error> {
    let row = sqlx::query("SELECT roles,status FROM users WHERE user_id=$1 AND tenant_id=$2 FOR UPDATE")
        .bind(target).bind(tenant).fetch_optional(&mut *connection).await?;
    let Some(row) = row else { return Ok(Err(Denied::Missing)); };
    let snapshot = Snapshot { roles: row.try_get("roles")?, status: row.try_get("status")? };
    if profile_only {
        return Ok(if actor == target { Ok(snapshot) } else { Err(Denied::Identity) });
    }
    if target == actor {
        return Ok(Err(Denied::SelfMutation));
    }
    if target == "admin" || snapshot.roles.iter().any(|role| role == "platform_admin") {
        return Ok(Err(Denied::Protected));
    }
    Ok(Ok(snapshot))
}
pub(super) async fn retain_administrator(
    connection: &mut PgConnection,
    tenant: &str,
    target: &str,
    before: &Snapshot,
    after_roles: &[String],
    after_status: &str,
) -> Result<Result<(), Denied>, sqlx::Error> {
    if before.status != "active" || !administrator(&before.roles)
        || (after_status == "active" && administrator(after_roles)) {
        return Ok(Ok(()));
    }
    let remaining: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE tenant_id=$1 AND user_id<>$2 AND status='active' AND (roles @> ARRAY['admin']::varchar[] OR roles @> ARRAY['platform_admin']::varchar[]) AND EXISTS(SELECT 1 FROM user_roles ur WHERE ur.user_id=users.user_id AND ur.role_name=ANY(users.roles) AND ur.role_name IN ('admin','platform_admin') AND (ur.expires_at IS NULL OR ur.expires_at>clock_timestamp())))")
        .bind(tenant).bind(target).fetch_one(connection).await?;
    Ok(if remaining { Ok(()) } else { Err(Denied::LastAdministrator) })
}
pub(super) async fn roles(
    connection: &mut PgConnection,
    requested: &[String],
) -> Result<Result<(), Denied>, sqlx::Error> {
    if requested.len() > 32 || requested.iter().any(|role|
        role.is_empty() || role.len() > 50 || role == "platform_admin"
        || !role.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_')) {
        return Ok(Err(Denied::InvalidRole));
    }
    let found: Vec<String> = sqlx::query_scalar("SELECT role_name FROM rbac_roles WHERE role_name=ANY($1) FOR SHARE")
        .bind(requested).fetch_all(connection).await?;
    Ok(if requested.iter().all(|role| found.contains(role)) { Ok(()) } else { Err(Denied::InvalidRole) })
}
pub(super) async fn permissions(
    connection: &mut PgConnection,
    requested: &[String],
) -> Result<Result<(), Denied>, sqlx::Error> {
    if requested.len() > 256 || requested.iter().any(|permission| permission.is_empty() || permission.len() > 100) {
        return Ok(Err(Denied::InvalidPermission));
    }
    let found: Vec<String> = sqlx::query_scalar("SELECT permission_code FROM permissions WHERE permission_code=ANY($1) FOR SHARE")
        .bind(requested).fetch_all(connection).await?;
    Ok(if requested.iter().all(|permission| permission == "admin:*" || found.contains(permission)) {
        Ok(())
    } else { Err(Denied::InvalidPermission) })
}
pub(super) async fn audit(
    connection: &mut PgConnection,
    tenant: &str,
    actor: &str,
    target: &str,
    action: &str,
) -> Result<(), sqlx::Error> {
    // Deliberately excludes passwords, email, profile values and credentials.
    sqlx::query("INSERT INTO login_audit(user_id,action,details) VALUES($1,$2,$3)")
        .bind(target).bind(action).bind(json!({"actor_user_id":actor,"tenant_id":tenant}))
        .execute(connection).await?;
    Ok(())
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn actual_guard_denials_match_declared_user_status_and_detail_envelopes() {
        let document = crate::native_contract::document();
        for (denied, status) in [
            (Denied::Identity, 403),
            (Denied::Administrator, 403),
            (Denied::Missing, 404),
            (Denied::Protected, 403),
            (Denied::SelfMutation, 400),
            (Denied::LastAdministrator, 409),
            (Denied::InvalidRole, 422),
            (Denied::InvalidPermission, 422),
            (Denied::Conflict, 409),
        ] {
            let (actual, body) = denied.response().response(&Default::default()).unwrap();
            assert_eq!(actual, status);
            assert!(body["detail"].is_string());
            let schema = &document["paths"]["/api/v1/users/{user_id}"]["put"]["responses"]
                [status.to_string()]["content"]["application/json"]["schema"];
            assert!(jsonschema::is_valid(schema, &body), "{status}: {body}");
        }
    }
}
