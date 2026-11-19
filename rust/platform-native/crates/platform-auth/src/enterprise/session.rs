//! Enterprise proofs issue the same opaque BFF/allowlist session as local login.
// SPDX-License-Identifier: Apache-2.0
use super::*;
use crate::{
    Claims,
    browser_authority::{BROWSER_ABSOLUTE_TTL, BROWSER_IDLE_TTL},
    browser_policy::cookie,
    sessions::browser::{BrowserSession, opaque},
};
pub(super) struct SessionGrant {
    pub body: Value,
    pub cookie: String,
}
impl EnterpriseService {
    pub(super) async fn establish(
        &self,
        user: &str,
        tenant: &str,
        method: &str,
        expected_epoch: u64,
    ) -> Result<SessionGrant> {
        if !matches!(method, "oidc" | "oidc_mfa" | "passkey" | "recovery") {
            return Err(EnterpriseError::Invalid);
        }
        let account = self
            .auth
            .accounts
            .by_id(user)
            .await
            .map_err(|_| EnterpriseError::Unavailable)?
            .ok_or(EnterpriseError::Proof)?;
        if account.user_id != user
            || account.tenant_id != tenant
            || account.status != "active"
            || !self
                .auth
                .accounts
                .tenant_active(tenant)
                .await
                .map_err(|_| EnterpriseError::Unavailable)?
        {
            return Err(EnterpriseError::Proof);
        }
        let now = self.now()?;
        let expires = now
            .checked_add(BROWSER_ABSOLUTE_TTL)
            .ok_or(EnterpriseError::Invalid)?;
        let jti = uuid::Uuid::new_v4().to_string();
        let changed = json!(account.password_changed_at.map(|v| v.to_rfc3339()));
        let current_policy = policy::password_policy(&self.pool, tenant, user).await?;
        verify_grant_policy(&current_policy, method, expected_epoch)?;
        let epoch = current_policy.auth_epoch;
        let data = json!({"user_id":user,"credential_revision":changed,"auth_epoch":epoch,"identity_method":method});
        self.auth
            .sessions
            .save(&jti, user, &data, BROWSER_ABSOLUTE_TTL)
            .await
            .map_err(|_| EnterpriseError::Unavailable)?;
        let claims = Claims {
            sub: user.into(),
            user_id: user.into(),
            tenant_id: tenant.into(),
            email: None,
            roles: vec![],
            permissions: vec![],
            tier: String::new(),
            exp: expires,
            iat: now,
            nbf: None,
            jti: jti.clone(),
            aud: None,
            iss: None,
        };
        let result=async {
            let p=self.auth.authenticate_claims(claims).await.map_err(|_|EnterpriseError::Proof)?;
            policy::verify_session_epoch(&self.pool,tenant,user,Some(&json!(epoch))).await?;
            sqlx::query("INSERT INTO gateway.enterprise_session_assurance(jti,tenant_id,user_id,method,expires_at) VALUES($1::text::uuid,$2,$3,$4,to_timestamp($5::bigint::double precision))").bind(&jti).bind(tenant).bind(user).bind(method).bind(i64::try_from(expires).map_err(|_|EnterpriseError::Invalid)?).execute(&self.pool).await?;
            let id=opaque();let csrf=opaque();let binding=BrowserSession {jti:jti.clone(),tenant_id:tenant.into(),user_id:user.into(),csrf:csrf.clone(),credential_revision:changed,issued_at:now,idle_expires_at:expires,expires_at:expires};
            self.auth.sessions.save_browser(&id,&binding,BROWSER_IDLE_TTL).await.map_err(|_|EnterpriseError::Unavailable)?;
            Ok(SessionGrant {body:json!({"user":{"user_id":p.account.user_id,"tenant_id":p.account.tenant_id,"email":p.account.email,"display_name":p.account.display_name,"department":p.account.department,"roles":p.authority.roles,"permissions":p.authority.permissions,"tier":p.authority.tier},"force_password_change":p.account.force_password_change,"expires_in":BROWSER_ABSOLUTE_TTL,"session_type":"cookie","csrf_token":csrf}),cookie:cookie(&id,BROWSER_ABSOLUTE_TTL)})
        }.await;
        if result.is_err() {
            let _ = self.auth.sessions.revoke(&jti, user).await;
        }
        result
    }
}

// Validate policy and revision from ONE snapshot. A non-MFA OIDC proof cannot
// adopt a newer epoch after concurrent MFA enforcement or credential changes.
fn verify_grant_policy(
    current: &policy::PasswordPolicy,
    method: &str,
    expected_epoch: u64,
) -> Result<()> {
    if current.auth_epoch != expected_epoch || (current.mfa_required && method == "oidc") {
        return Err(EnterpriseError::Proof);
    }
    if !matches!(method, "oidc" | "oidc_mfa" | "passkey" | "recovery") {
        return Err(EnterpriseError::Invalid);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_and_proof_epoch_must_match_without_mfa_downgrade() {
        let enforced = policy::PasswordPolicy {
            auth_epoch: 8,
            mfa_required: true,
        };
        assert!(verify_grant_policy(&enforced, "oidc", 8).is_err());
        for method in ["oidc_mfa", "passkey", "recovery"] {
            assert!(verify_grant_policy(&enforced, method, 8).is_ok());
            assert!(verify_grant_policy(&enforced, method, 7).is_err());
        }
        let optional = policy::PasswordPolicy {
            auth_epoch: 8,
            mfa_required: false,
        };
        assert!(verify_grant_policy(&optional, "oidc", 8).is_ok());
        assert!(verify_grant_policy(&optional, "oidc", 7).is_err());
        assert!(verify_grant_policy(&optional, "claimed-mfa", 8).is_err());
    }
}
