//! Provisioning credentials cannot manage canonical administrator accounts.
//! Keep the existing users API's exact admin/platform_admin and system-admin
//! boundary; SCIM write scopes do not imply interactive account-admin authority.
use super::*;
pub(super) async fn lock_mutable_target(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: &str,
    user: &str,
) -> Result<()> {
    // Lock the same owner row that role-assignment epoch triggers update. A
    // concurrent promotion therefore serializes before or after this mutation.
    // Joining the managed row avoids revealing unrelated tenant account records.
    let legacy:Vec<String>=sqlx::query_scalar("SELECT u.roles FROM gateway.users u JOIN gateway.enterprise_scim_users s USING(tenant_id,user_id) WHERE u.tenant_id=$1 AND u.user_id=$2 AND NOT s.deleted FOR UPDATE OF u,s")
        .bind(tenant).bind(user).fetch_optional(&mut **tx).await?.ok_or(EnterpriseError::NotFound)?;
    let current:Vec<String>=sqlx::query_scalar("SELECT role_name FROM gateway.user_roles WHERE user_id=$1 AND (expires_at IS NULL OR expires_at>CURRENT_TIMESTAMP)")
        .bind(user).fetch_all(&mut **tx).await?;
    if protected(user, &legacy, &current) {
        return Err(EnterpriseError::Forbidden);
    }
    Ok(())
}
fn protected(user: &str, legacy: &[String], current: &[String]) -> bool {
    user == "admin"
        || legacy
            .iter()
            .chain(current)
            .any(|role| matches!(role.as_str(), "admin" | "platform_admin"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn managed_provisioning_never_overrides_account_administrator_protection() {
        assert!(protected("admin", &[], &[]));
        assert!(protected("scim_x", &["admin".into()], &[]));
        assert!(protected("scim_x", &[], &["platform_admin".into()]));
        // Exact canonical roles, never substring-based privilege guesses.
        assert!(!protected(
            "scim_admin",
            &["user".into()],
            &["admin_support".into()]
        ));
        assert!(!protected("normal", &[], &[]));
    }
}
