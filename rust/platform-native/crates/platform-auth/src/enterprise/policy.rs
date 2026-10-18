//! Explicitly activated password-MFA gate; no missing-schema success fallback.
// SPDX-License-Identifier: Apache-2.0
use super::*;
pub struct PasswordPolicy {
    pub auth_epoch: u64,
    pub mfa_required: bool,
}
pub async fn password_policy(pool: &PgPool, tenant: &str, user: &str) -> Result<PasswordPolicy> {
    let (epoch,required):(i64,bool)=sqlx::query_as("SELECT u.auth_epoch,COALESCE(t.require_mfa,false) OR COALESCE(s.mfa_required,false) FROM gateway.users u LEFT JOIN gateway.enterprise_tenant_policy t USING(tenant_id) LEFT JOIN gateway.enterprise_security_state s USING(tenant_id,user_id) WHERE u.tenant_id=$1 AND u.user_id=$2 AND u.status='active' AND EXISTS(SELECT 1 FROM gateway.tenants x WHERE x.tenant_id=u.tenant_id AND x.status='active')").bind(tenant).bind(user).fetch_optional(pool).await?.ok_or(EnterpriseError::Proof)?;
    Ok(PasswordPolicy {
        auth_epoch: u64::try_from(epoch).map_err(|_| EnterpriseError::Unavailable)?,
        mfa_required: required,
    })
}

pub async fn verify_session_epoch(
    pool: &PgPool,
    tenant: &str,
    user: &str,
    stored: Option<&Value>,
) -> Result<()> {
    let current = password_policy(pool, tenant, user).await?.auth_epoch;
    let stored = match stored {
        None => 0,
        Some(v) => v.as_u64().ok_or(EnterpriseError::Proof)?,
    };
    if stored != current {
        return Err(EnterpriseError::Proof);
    }
    Ok(())
}
pub async fn password_login_allowed(pool: &PgPool, tenant: &str, user: &str) -> Result<bool> {
    Ok(!password_policy(pool, tenant, user).await?.mfa_required)
}

impl EnterpriseService {
    /// Security mutations must pin the already authenticated session revision,
    /// never adopt a fresh epoch after waiting for another transaction.
    pub(super) async fn authenticated_session_epoch(&self, p: &Principal) -> Result<u64> {
        let record = self
            .auth
            .sessions
            .record(&p.jti)
            .await
            .map_err(|_| EnterpriseError::Unavailable)?
            .ok_or(EnterpriseError::Proof)?;
        pinned_epoch(&record, &p.account.user_id)
    }
    pub(super) async fn require_enrollment_proof(
        &self,
        p: &Principal,
        password: Option<String>,
    ) -> Result<()> {
        if super::step_up::recent(&self.auth, p, None).await? {
            return Ok(());
        }
        let method:Option<String>=sqlx::query_scalar("SELECT method FROM gateway.enterprise_session_assurance WHERE jti=$1::text::uuid AND tenant_id=$2 AND user_id=$3 AND method IN ('passkey','oidc_mfa','recovery') AND verified_at>CURRENT_TIMESTAMP-interval '5 minutes' AND expires_at>CURRENT_TIMESTAMP").bind(&p.jti).bind(&p.account.tenant_id).bind(&p.account.user_id).fetch_optional(&self.pool).await?;
        match method.as_deref() {
            Some("passkey" | "oidc_mfa") => Ok(()),
            Some("recovery") => {
                self.password(p, password.ok_or(EnterpriseError::Proof)?)
                    .await
            }
            _ => Err(EnterpriseError::Forbidden),
        }
    }
    pub(super) async fn require_recent_mfa(&self, p: &Principal) -> Result<()> {
        if super::step_up::recent(&self.auth, p, None).await? {
            return Ok(());
        }
        let recent:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway.enterprise_session_assurance WHERE jti=$1::text::uuid AND tenant_id=$2 AND user_id=$3 AND method IN ('passkey','oidc_mfa') AND verified_at>CURRENT_TIMESTAMP-interval '5 minutes' AND expires_at>CURRENT_TIMESTAMP)").bind(&p.jti).bind(&p.account.tenant_id).bind(&p.account.user_id).fetch_one(&self.pool).await?;
        if !recent {
            return Err(EnterpriseError::Forbidden);
        }
        Ok(())
    }
}

fn pinned_epoch(record: &Value, user: &str) -> Result<u64> {
    if record.get("user_id").and_then(Value::as_str) != Some(user) {
        return Err(EnterpriseError::Proof);
    }
    record
        .get("auth_epoch")
        .and_then(Value::as_u64)
        .ok_or(EnterpriseError::Proof)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mutation_epoch_comes_from_the_same_existing_session() {
        assert_eq!(
            pinned_epoch(&json!({"user_id":"u","auth_epoch":0}), "u").unwrap(),
            0
        );
        assert_eq!(
            pinned_epoch(&json!({"user_id":"u","auth_epoch":9}), "u").unwrap(),
            9
        );
        for record in [
            json!({"user_id":"other","auth_epoch":9}),
            json!({"user_id":"u"}),
            json!({"user_id":"u","auth_epoch":-1}),
            json!({"user_id":"u","auth_epoch":"9"}),
        ] {
            assert!(pinned_epoch(&record, "u").is_err());
        }
    }
}
