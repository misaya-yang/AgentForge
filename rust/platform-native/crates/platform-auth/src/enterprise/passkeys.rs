//! Complete WebAuthn ceremonies, UV-only MFA and one-use recovery.
// SPDX-License-Identifier: Apache-2.0
use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sqlx::Row;
#[derive(Serialize, Deserialize)]
struct RegistrationFlow {
    tenant: String,
    user: String,
    epoch: u64,
    revision: Value,
    state: PasskeyRegistration,
}
#[derive(Serialize, Deserialize)]
struct AuthenticationFlow {
    tenant: String,
    user: String,
    epoch: u64,
    keys: Vec<(String, i64)>,
    state: PasskeyAuthentication,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationStart {
    pub current_password: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationFinish {
    pub flow_id: String,
    pub credential: RegisterPublicKeyCredential,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationStart {
    pub tenant_id: String,
    pub user_name: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticationFinish {
    pub flow_id: String,
    pub credential: PublicKeyCredential,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryInput {
    pub tenant_id: String,
    pub user_name: String,
    pub current_password: String,
    pub recovery_code: String,
}
fn credential_id(passkey: &Passkey) -> String {
    let raw: &[u8] = passkey.cred_id().as_ref();
    URL_SAFE_NO_PAD.encode(raw)
}
fn code_hash(tenant: &str, user: &str, code: &str) -> String {
    digest(format!("{tenant}\0{user}\0{code}").as_bytes())
}
fn counter_valid(old: i64, new: u32) -> bool {
    old == 0 && new == 0 || i64::from(new) > old
}
impl EnterpriseService {
    async fn lookup_user(&self, tenant: &str, name: &str) -> Result<String> {
        bounded(tenant, 255)?;
        bounded(name, 255)?;
        let users:Vec<String>=sqlx::query_scalar("SELECT user_id FROM gateway.users WHERE tenant_id=$1 AND status='active' AND (user_id=$2 OR username=$2 OR LOWER(email)=LOWER($2)) LIMIT 2").bind(tenant).bind(name).fetch_all(&self.pool).await?;
        if users.len() != 1 {
            return Err(EnterpriseError::Proof);
        }
        Ok(users[0].clone())
    }
    pub async fn passkey_registration_start(
        &self,
        p: &Principal,
        input: RegistrationStart,
    ) -> Result<Value> {
        self.transient
            .rate(
                "passkey-enroll",
                &format!("{}:{}", p.account.tenant_id, p.account.user_id),
                5,
                300,
            )
            .await?;
        let policy =
            policy::password_policy(&self.pool, &p.account.tenant_id, &p.account.user_id).await?;
        if policy.mfa_required || input.current_password.is_none() {
            self.require_enrollment_proof(p, input.current_password)
                .await?;
        } else {
            self.password(p, input.current_password.ok_or(EnterpriseError::Proof)?)
                .await?;
        }
        let handle:String=sqlx::query_scalar("INSERT INTO gateway.enterprise_security_state(tenant_id,user_id,user_handle) VALUES($1,$2,$3::text::uuid) ON CONFLICT(tenant_id,user_id) DO UPDATE SET user_id=EXCLUDED.user_id RETURNING user_handle::text").bind(&p.account.tenant_id).bind(&p.account.user_id).bind(uuid::Uuid::new_v4().to_string()).fetch_one(&self.pool).await?;
        let stored: Vec<Value> = sqlx::query_scalar(
            "SELECT passkey FROM gateway.enterprise_passkeys WHERE tenant_id=$1 AND user_id=$2",
        )
        .bind(&p.account.tenant_id)
        .bind(&p.account.user_id)
        .fetch_all(&self.pool)
        .await?;
        if stored.len() >= 16 {
            return Err(EnterpriseError::Forbidden);
        }
        let keys = stored
            .into_iter()
            .map(|v| serde_json::from_value::<Passkey>(v).map_err(|_| EnterpriseError::Unavailable))
            .collect::<Result<Vec<_>>>()?;
        let exclude = keys.iter().map(|key| key.cred_id().clone()).collect();
        let (challenge, state) = self
            .webauthn
            .start_passkey_registration(
                uuid::Uuid::parse_str(&handle).map_err(|_| EnterpriseError::Unavailable)?,
                &p.account.user_id,
                p.account
                    .display_name
                    .as_deref()
                    .unwrap_or(&p.account.user_id),
                Some(exclude),
            )
            .map_err(|_| EnterpriseError::Proof)?;
        let id = crate::sessions::browser::opaque();
        self.transient
            .put(
                "passkey-register",
                &id,
                &p.jti,
                &RegistrationFlow {
                    tenant: p.account.tenant_id.clone(),
                    user: p.account.user_id.clone(),
                    epoch: policy.auth_epoch,
                    revision: revision(p),
                    state,
                },
            )
            .await?;
        Ok(json!({"flow_id":id,"publicKey":challenge.public_key}))
    }
    pub(super) async fn passkey_registration_finish(
        &self,
        p: &Principal,
        input: RegistrationFinish,
    ) -> Result<(session::SessionGrant, Vec<String>)> {
        let flow: RegistrationFlow = self
            .transient
            .take("passkey-register", &input.flow_id, &p.jti)
            .await?;
        if flow.tenant != p.account.tenant_id
            || flow.user != p.account.user_id
            || flow.revision != revision(p)
        {
            return Err(EnterpriseError::Proof);
        }
        let key = self
            .webauthn
            .finish_passkey_registration(&input.credential, &flow.state)
            .map_err(|_| EnterpriseError::Proof)?;
        // Read-only access to library credential internals initializes the real
        // registration counter; no origin/UV/attestation state is rewritten.
        let initial: Credential = key.clone().into();
        if !initial.user_verified {
            return Err(EnterpriseError::Proof);
        }
        let mut tx = self.pool.begin().await?;
        bump_epoch(&mut tx, &flow.tenant, &flow.user, flow.epoch).await?;
        sqlx::query("INSERT INTO gateway.enterprise_passkeys(credential_id,tenant_id,user_id,passkey,counter) VALUES($1,$2,$3,$4,$5)").bind(credential_id(&key)).bind(&flow.tenant).bind(&flow.user).bind(serde_json::to_value(key).map_err(|_|EnterpriseError::Unavailable)?).bind(i64::from(initial.counter)).execute(&mut *tx).await?;
        sqlx::query("UPDATE gateway.enterprise_security_state SET mfa_required=true,recovery_epoch=recovery_epoch+1 WHERE tenant_id=$1 AND user_id=$2").bind(&flow.tenant).bind(&flow.user).execute(&mut *tx).await?;
        let codes = replace_recovery_codes(&mut tx, &flow.tenant, &flow.user).await?;
        tx.commit().await?;
        Ok((
            self.establish(
                &flow.user,
                &flow.tenant,
                "passkey",
                flow.epoch.checked_add(1).ok_or(EnterpriseError::Invalid)?,
            )
            .await?,
            codes,
        ))
    }
    pub async fn passkey_authentication_start(
        &self,
        input: AuthenticationStart,
        browser_binding: &str,
    ) -> Result<Value> {
        self.transient
            .rate(
                "passkey-login",
                &format!(
                    "{}:{}",
                    input.tenant_id,
                    input.user_name.to_ascii_lowercase()
                ),
                10,
                300,
            )
            .await?;
        let user = self.lookup_user(&input.tenant_id, &input.user_name).await?;
        let policy = policy::password_policy(&self.pool, &input.tenant_id, &user).await?;
        let rows=sqlx::query("SELECT credential_id,passkey,revision FROM gateway.enterprise_passkeys WHERE tenant_id=$1 AND user_id=$2 ORDER BY credential_id").bind(&input.tenant_id).bind(&user).fetch_all(&self.pool).await?;
        if rows.is_empty() || rows.len() > 16 {
            return Err(EnterpriseError::Proof);
        }
        let mut keys = Vec::new();
        let mut versions = Vec::new();
        for row in rows {
            keys.push(
                serde_json::from_value::<Passkey>(row.try_get("passkey")?)
                    .map_err(|_| EnterpriseError::Unavailable)?,
            );
            versions.push((row.try_get("credential_id")?, row.try_get("revision")?));
        }
        let (challenge, state) = self
            .webauthn
            .start_passkey_authentication(&keys)
            .map_err(|_| EnterpriseError::Proof)?;
        let id = crate::sessions::browser::opaque();
        self.transient
            .put(
                "passkey-auth",
                &id,
                browser_binding,
                &AuthenticationFlow {
                    tenant: input.tenant_id,
                    user,
                    epoch: policy.auth_epoch,
                    keys: versions,
                    state,
                },
            )
            .await?;
        Ok(json!({"flow_id":id,"publicKey":challenge.public_key}))
    }
    pub(super) async fn passkey_authentication_finish(
        &self,
        input: AuthenticationFinish,
        browser_binding: &str,
    ) -> Result<session::SessionGrant> {
        self.transient
            .rate("passkey-finish", browser_binding, 10, 300)
            .await?;
        let flow: AuthenticationFlow = self
            .transient
            .take("passkey-auth", &input.flow_id, browser_binding)
            .await?;
        let result = self
            .webauthn
            .finish_passkey_authentication(&input.credential, &flow.state)
            .map_err(|_| EnterpriseError::Proof)?;
        if !result.user_verified() {
            return Err(EnterpriseError::Proof);
        }
        let raw: &[u8] = result.cred_id().as_ref();
        let id = URL_SAFE_NO_PAD.encode(raw);
        let expected = flow
            .keys
            .iter()
            .find(|(key, _)| key == &id)
            .ok_or(EnterpriseError::Proof)?
            .1;
        let mut tx = self.pool.begin().await?;
        let epoch:i64=sqlx::query_scalar("SELECT auth_epoch FROM gateway.users WHERE tenant_id=$1 AND user_id=$2 AND status='active' FOR UPDATE").bind(&flow.tenant).bind(&flow.user).fetch_optional(&mut *tx).await?.ok_or(EnterpriseError::Proof)?;
        if u64::try_from(epoch).ok() != Some(flow.epoch) {
            return Err(EnterpriseError::Proof);
        }
        let row=sqlx::query("SELECT passkey,counter,revision FROM gateway.enterprise_passkeys WHERE credential_id=$1 AND tenant_id=$2 AND user_id=$3 FOR UPDATE").bind(&id).bind(&flow.tenant).bind(&flow.user).fetch_optional(&mut *tx).await?.ok_or(EnterpriseError::Proof)?;
        if row.try_get::<i64, _>("revision")? != expected
            || !counter_valid(row.try_get("counter")?, result.counter())
        {
            return Err(EnterpriseError::Proof);
        }
        let mut key: Passkey = serde_json::from_value(row.try_get("passkey")?)
            .map_err(|_| EnterpriseError::Unavailable)?;
        if key.update_credential(&result).is_none() {
            return Err(EnterpriseError::Proof);
        }
        sqlx::query("UPDATE gateway.enterprise_passkeys SET passkey=$2,counter=$3,revision=revision+1 WHERE credential_id=$1").bind(id).bind(serde_json::to_value(key).map_err(|_|EnterpriseError::Unavailable)?).bind(i64::from(result.counter())).execute(&mut *tx).await?;
        tx.commit().await?;
        self.establish(&flow.user, &flow.tenant, "passkey", flow.epoch)
            .await
    }
    pub(super) async fn recovery(
        &self,
        input: RecoveryInput,
        browser_binding: &str,
    ) -> Result<session::SessionGrant> {
        self.transient
            .rate("recovery-browser", browser_binding, 5, 300)
            .await?;
        self.transient
            .rate(
                "recovery-user",
                &format!(
                    "{}:{}",
                    input.tenant_id,
                    input.user_name.to_ascii_lowercase()
                ),
                5,
                900,
            )
            .await?;
        if input.recovery_code.len() != 32
            || !input.recovery_code.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(EnterpriseError::Proof);
        }
        let user = self.lookup_user(&input.tenant_id, &input.user_name).await?;
        self.transient
            .rate(
                "recovery-account",
                &format!("{}:{user}", input.tenant_id),
                5,
                900,
            )
            .await?;
        let account = self
            .auth
            .accounts
            .by_id(&user)
            .await
            .map_err(|_| EnterpriseError::Unavailable)?
            .ok_or(EnterpriseError::Proof)?;
        let expected_hash = account.password_hash.ok_or(EnterpriseError::Proof)?;
        let epoch = policy::password_policy(&self.pool, &input.tenant_id, &user)
            .await?
            .auth_epoch;
        if !self
            .auth
            .password_engine()
            .verify(input.current_password, expected_hash.clone())
            .await
            .map_err(|_| EnterpriseError::Unavailable)?
        {
            return Err(EnterpriseError::Proof);
        }
        let mut tx = self.pool.begin().await?;
        let consumed=sqlx::query("UPDATE gateway.enterprise_recovery_codes SET consumed_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND user_id=$2 AND code_hash=$3 AND consumed_at IS NULL").bind(&input.tenant_id).bind(&user).bind(code_hash(&input.tenant_id,&user,&input.recovery_code)).execute(&mut *tx).await?.rows_affected();
        if consumed != 1 {
            return Err(EnterpriseError::Proof);
        }
        // Recovery proves the password plus one high-entropy second factor.
        // It invalidates prior sessions; it never silently disables MFA.
        if sqlx::query("UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE tenant_id=$1 AND user_id=$2 AND status='active' AND auth_epoch=$3 AND password_hash=$4").bind(&input.tenant_id).bind(&user).bind(i64::try_from(epoch).map_err(|_|EnterpriseError::Invalid)?).bind(expected_hash).execute(&mut *tx).await?.rows_affected()!=1{return Err(EnterpriseError::Proof);}
        tx.commit().await?;
        self.establish(
            &user,
            &input.tenant_id,
            "recovery",
            epoch.checked_add(1).ok_or(EnterpriseError::Invalid)?,
        )
        .await
    }
    pub async fn remove_passkey(&self, p: &Principal, id: &str) -> Result<()> {
        self.require_recent_mfa(p).await?;
        let expected_epoch = self.authenticated_session_epoch(p).await?;
        let mut tx = self.pool.begin().await?;
        // Serialize deletion decisions for this account: two removals must not
        // both observe two credentials and leave the account with none.
        let epoch:i64=sqlx::query_scalar("SELECT auth_epoch FROM gateway.users WHERE tenant_id=$1 AND user_id=$2 AND status='active' FOR UPDATE")
            .bind(&p.account.tenant_id).bind(&p.account.user_id)
            .fetch_optional(&mut *tx).await?.ok_or(EnterpriseError::Proof)?;
        if u64::try_from(epoch).ok() != Some(expected_epoch) {
            return Err(EnterpriseError::Proof);
        }
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM gateway.enterprise_passkeys WHERE tenant_id=$1 AND user_id=$2",
        )
        .bind(&p.account.tenant_id)
        .bind(&p.account.user_id)
        .fetch_one(&mut *tx)
        .await?;
        if total <= 1 {
            return Err(EnterpriseError::Forbidden);
        }
        if sqlx::query("DELETE FROM gateway.enterprise_passkeys WHERE tenant_id=$1 AND user_id=$2 AND credential_id=$3").bind(&p.account.tenant_id).bind(&p.account.user_id).bind(id).execute(&mut *tx).await?.rows_affected()!=1{return Err(EnterpriseError::NotFound);}
        sqlx::query(
            "UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE tenant_id=$1 AND user_id=$2",
        )
        .bind(&p.account.tenant_id)
        .bind(&p.account.user_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn rotate_recovery_codes(&self, p: &Principal) -> Result<Vec<String>> {
        self.require_recent_mfa(p).await?;
        let expected_epoch = self.authenticated_session_epoch(p).await?;
        let mut tx = self.pool.begin().await?;
        bump_epoch(
            &mut tx,
            &p.account.tenant_id,
            &p.account.user_id,
            expected_epoch,
        )
        .await?;
        let codes =
            replace_recovery_codes(&mut tx, &p.account.tenant_id, &p.account.user_id).await?;
        tx.commit().await?;
        Ok(codes)
    }
}
async fn bump_epoch(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: &str,
    user: &str,
    epoch: u64,
) -> Result<()> {
    if sqlx::query("UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE tenant_id=$1 AND user_id=$2 AND status='active' AND auth_epoch=$3").bind(tenant).bind(user).bind(i64::try_from(epoch).map_err(|_|EnterpriseError::Invalid)?).execute(&mut **tx).await?.rows_affected()!=1{return Err(EnterpriseError::Proof);}
    Ok(())
}
async fn replace_recovery_codes(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: &str,
    user: &str,
) -> Result<Vec<String>> {
    sqlx::query("DELETE FROM gateway.enterprise_recovery_codes WHERE tenant_id=$1 AND user_id=$2")
        .bind(tenant)
        .bind(user)
        .execute(&mut **tx)
        .await?;
    let mut codes = Vec::new();
    for _ in 0..10 {
        let code = crate::sessions::browser::opaque()[..32].to_owned();
        sqlx::query("INSERT INTO gateway.enterprise_recovery_codes(tenant_id,user_id,code_hash) VALUES($1,$2,$3)").bind(tenant).bind(user).bind(code_hash(tenant,user,&code)).execute(&mut **tx).await?;
        codes.push(code);
    }
    Ok(codes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counters_and_recovery_material_do_not_cross_users() {
        assert!(counter_valid(0, 0));
        assert!(counter_valid(8, 9));
        assert!(!counter_valid(8, 8));
        assert!(!counter_valid(8, 7));
        assert_ne!(
            code_hash("tenant", "alice", "code"),
            code_hash("tenant", "bob", "code")
        );
    }
}
