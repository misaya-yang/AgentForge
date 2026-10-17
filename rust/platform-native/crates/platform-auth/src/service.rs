//! Authentication service orchestration. Live PostgreSQL/Redis acceptance is required.
pub use crate::contract::{LoginResponse, UserProfile};
use crate::{
    Claims, PasswordEngine, TokenCodec,
    accounts::{Account, AuditAction, AuditContext, Authority, PgAccounts, StoreError},
    normalize_email,
    sessions::RedisSessions,
};
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("Invalid request")]
    Invalid,
    #[error("Invalid email or password")]
    Credentials,
    #[error("Current password is incorrect")]
    CurrentPassword,
    #[error("Account is disabled")]
    Disabled,
    #[error("Account is locked")]
    Locked,
    #[error("Authentication authority unavailable")]
    Unavailable,
}
impl ServiceError {
    pub fn status(&self) -> u16 {
        match self {
            Self::Invalid => 422,
            Self::Credentials => 401,
            Self::CurrentPassword => 400,
            Self::Disabled => 403,
            Self::Locked => 423,
            Self::Unavailable => 503,
        }
    }
}
/// Never derives Debug: contains credential-sensitive account and token state.
pub struct Principal {
    pub account: Account,
    pub authority: Authority,
    pub jti: String,
    /// Verified absolute JWT expiry; non-bearer metadata for queued authority.
    pub expires_at: u64,
}
pub struct AuthService {
    pub(crate) accounts: PgAccounts,
    pub(crate) sessions: RedisSessions,
    pub(crate) codec: TokenCodec,
    passwords: PasswordEngine,
    domain: String,
    enterprise_policy_enabled: bool,
    pub(crate) clock: fn() -> Result<u64, ServiceError>,
}
impl AuthService {
    pub fn new(
        accounts: PgAccounts,
        sessions: RedisSessions,
        codec: TokenCodec,
        passwords: PasswordEngine,
        domain: String,
    ) -> Self {
        Self {
            accounts,
            sessions,
            codec,
            passwords,
            domain,
            enterprise_policy_enabled: false,
            clock: || {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|t| t.as_secs())
                    .map_err(|_| ServiceError::Unavailable)
            },
        }
    }
    /// Activate only with the measured enterprise schema. Every missing policy
    /// or epoch authority is then a hard error, never a password-only fallback.
    pub fn with_enterprise_policy_enabled(mut self, enabled: bool) -> Self {
        self.enterprise_policy_enabled = enabled;
        self
    }
    pub fn enterprise_policy_enabled(&self) -> bool {
        self.enterprise_policy_enabled
    }
    pub(crate) fn password_engine(&self) -> PasswordEngine {
        self.passwords.clone()
    }
    fn account_state(account: &Account, now: u64) -> Result<(), ServiceError> {
        if account
            .locked_until
            .is_some_and(|time| time.timestamp() > 0 && time.timestamp() as u64 > now)
        {
            return Err(ServiceError::Locked);
        }
        if account.status != "active" {
            return Err(ServiceError::Disabled);
        }
        Ok(())
    }
    pub async fn login(
        &self,
        email: &str,
        password: String,
        audit: &AuditContext,
    ) -> Result<LoginResponse, ServiceError> {
        let now = (self.clock)()?;
        if password.is_empty() {
            return Err(ServiceError::Invalid);
        }
        let email = normalize_email(email, &self.domain).map_err(|_| ServiceError::Invalid)?;
        let account = match self
            .accounts
            .by_email(&email)
            .await
            .map_err(|_| ServiceError::Unavailable)?
        {
            Some(a) => Some(a),
            None => self
                .accounts
                .by_id(email.split('@').next().ok_or(ServiceError::Invalid)?)
                .await
                .map_err(|_| ServiceError::Unavailable)?,
        };
        let Some(account) = account else {
            self.accounts
                .audit_event(None, Some(&email), AuditAction::MissingUser, audit)
                .await
                .map_err(|_| ServiceError::Unavailable)?;
            return Err(ServiceError::Credentials);
        };
        if let Err(error) = Self::account_state(&account, now) {
            if matches!(error, ServiceError::Disabled) {
                self.accounts
                    .audit_event(
                        Some(&account.user_id),
                        Some(&email),
                        AuditAction::DisabledAccount,
                        audit,
                    )
                    .await
                    .map_err(|_| ServiceError::Unavailable)?;
            }
            return Err(error);
        }
        if !self
            .accounts
            .tenant_active(&account.tenant_id)
            .await
            .map_err(|_| ServiceError::Unavailable)?
        {
            self.accounts
                .audit_event(
                    Some(&account.user_id),
                    Some(&email),
                    AuditAction::DisabledTenant,
                    audit,
                )
                .await
                .map_err(|_| ServiceError::Unavailable)?;
            return Err(ServiceError::Disabled);
        }
        let interactive_policy = if self.enterprise_policy_enabled {
            let policy = crate::enterprise::policy::password_policy(
                &self.accounts.pool,
                &account.tenant_id,
                &account.user_id,
            )
            .await
            .map_err(enterprise_failure)?;
            if policy.mfa_required {
                return Err(ServiceError::Credentials);
            }
            Some(policy)
        } else {
            None
        };
        let verified = self
            .passwords
            .verify_upgrade(password, account.password_hash.clone().unwrap_or_default())
            .await
            .map_err(|_| ServiceError::Unavailable)?;
        if !verified.valid {
            match self
                .accounts
                .failed_password(
                    &account.user_id,
                    &account.tenant_id,
                    account.password_hash.as_deref(),
                    audit,
                )
                .await
            {
                Ok(attempts) if attempts >= 5 => return Err(ServiceError::Locked),
                Ok(_) => return Err(ServiceError::Credentials),
                Err(StoreError::Changed) => {
                    let current = self
                        .accounts
                        .by_id(&account.user_id)
                        .await
                        .map_err(|_| ServiceError::Unavailable)?
                        .ok_or(ServiceError::Credentials)?;
                    if current.tenant_id != account.tenant_id {
                        return Err(ServiceError::Credentials);
                    }
                    Self::account_state(&current, now)?;
                    return Err(ServiceError::Credentials);
                }
                Err(_) => return Err(ServiceError::Unavailable),
            }
        }
        self.accounts
            .successful_password(
                &account.user_id,
                &account.tenant_id,
                account
                    .password_hash
                    .as_deref()
                    .ok_or(ServiceError::Credentials)?,
                audit,
            )
            .await
            .map_err(|e| {
                if e == StoreError::Changed {
                    ServiceError::Credentials
                } else {
                    ServiceError::Unavailable
                }
            })?;
        if let Some(replacement) = verified.replacement {
            self.accounts
                .rehash_password(
                    &account.user_id,
                    &account.tenant_id,
                    account
                        .password_hash
                        .as_deref()
                        .ok_or(ServiceError::Credentials)?,
                    &replacement,
                )
                .await
                .map_err(|e| {
                    if e == StoreError::Changed {
                        ServiceError::Credentials
                    } else {
                        ServiceError::Unavailable
                    }
                })?;
        }
        if let Some(original) = &interactive_policy {
            let current = crate::enterprise::policy::password_policy(
                &self.accounts.pool,
                &account.tenant_id,
                &account.user_id,
            )
            .await
            .map_err(enterprise_failure)?;
            if current.mfa_required || current.auth_epoch != original.auth_epoch {
                return Err(ServiceError::Credentials);
            }
        }
        let authority = self
            .accounts
            .legacy_authority(&account.user_id, &account.tenant_id)
            .await
            .map_err(|e| {
                if e == StoreError::Changed {
                    ServiceError::Credentials
                } else {
                    ServiceError::Unavailable
                }
            })?;
        let tier = if authority.roles.iter().any(|r| r == "admin") {
            "admin".into()
        } else {
            authority.tier
        };
        let claims = Claims {
            sub: account.user_id.clone(),
            user_id: account.user_id.clone(),
            tenant_id: account.tenant_id.clone(),
            email: account.email.clone(),
            roles: authority.roles.clone(),
            permissions: authority.permissions.clone(),
            tier: tier.clone(),
            exp: 0,
            iat: 0,
            nbf: None,
            jti: String::new(),
            aud: None,
            iss: None,
        };
        let now = (self.clock)()?;
        let token = self
            .codec
            .issue(claims, now)
            .map_err(|_| ServiceError::Unavailable)?;
        let issued = self
            .codec
            .verify(&token, now)
            .map_err(|_| ServiceError::Unavailable)?;
        let mut data = serde_json::json!({"user_id":account.user_id,"email":account.email,"roles":authority.roles,"tier":tier,"ip":audit.ip,"credential_revision":account.password_changed_at.map(|time| time.to_rfc3339())});
        if let Some(policy) = interactive_policy {
            data["auth_epoch"] = serde_json::json!(policy.auth_epoch);
        }
        let ttl = issued
            .exp
            .checked_sub(now)
            .ok_or(ServiceError::Unavailable)?;
        if self
            .sessions
            .save(&issued.jti, &issued.user_id, &data, ttl)
            .await
            .is_err()
        {
            // A timeout/runtime error can have persisted part of the Redis transaction.
            // Never return the token. Best-effort removal does not turn this into success.
            let _ = self.sessions.revoke(&issued.jti, &issued.user_id).await;
            return Err(ServiceError::Unavailable);
        }
        Ok(LoginResponse {
            access_token: token,
            token_type: "bearer",
            expires_in: ttl,
            force_password_change: account.force_password_change,
            user: UserProfile {
                user_id: account.user_id,
                email: account.email,
                display_name: account.display_name,
                department: account.department,
                roles: authority.roles,
                permissions: authority.permissions,
                tier,
            },
        })
    }
    /// Identity headers are never accepted. Grants come from current database authority.
    /// Cryptographic projection for host observability only. Protected routes
    /// must still call authenticate/current-authority admission.
    pub fn verify_access_claims(&self, token: &str) -> Result<Claims, ServiceError> {
        self.codec
            .verify(token, (self.clock)()?)
            .map_err(|_| ServiceError::Credentials)
    }
    pub async fn authenticate(&self, token: &str) -> Result<Principal, ServiceError> {
        let now = (self.clock)()?;
        let claims = self
            .codec
            .verify(token, now)
            .map_err(|_| ServiceError::Credentials)?;
        self.authenticate_claims(claims).await
    }
    pub(crate) async fn authenticate_claims(
        &self,
        claims: Claims,
    ) -> Result<Principal, ServiceError> {
        let now = (self.clock)()?;
        if claims.exp <= now
            || claims.iat > now
            || claims.user_id != claims.sub
            || claims.tenant_id.is_empty()
            || uuid::Uuid::parse_str(&claims.jti).is_err()
        {
            return Err(ServiceError::Credentials);
        }
        let session = self
            .sessions
            .record(&claims.jti)
            .await
            .map_err(|_| ServiceError::Unavailable)?
            .ok_or(ServiceError::Credentials)?;
        if session.get("user_id").and_then(|value| value.as_str()) != Some(claims.sub.as_str()) {
            return Err(ServiceError::Credentials);
        }
        let account = self
            .accounts
            .by_id(&claims.sub)
            .await
            .map_err(|_| ServiceError::Unavailable)?
            .ok_or(ServiceError::Credentials)?;
        if account.tenant_id != claims.tenant_id || account.status != "active" {
            return Err(ServiceError::Credentials);
        }
        // The precise database revision avoids accepting a pre-reset token or
        // rejecting a fresh login merely because both happened in one second.
        if let Some(revision) = session.get("credential_revision") {
            let current =
                serde_json::json!(account.password_changed_at.map(|time| time.to_rfc3339()));
            if revision != &current {
                return Err(ServiceError::Credentials);
            }
        } else if account.password_changed_at.is_some_and(|changed| {
            changed.timestamp() >= 0 && claims.iat <= changed.timestamp() as u64
        }) {
            // Interoperable older session records lack a precise revision.
            // Same-second ambiguity fails closed and requires a fresh login.
            return Err(ServiceError::Credentials);
        }
        if self.enterprise_policy_enabled {
            crate::enterprise::policy::verify_session_epoch(
                &self.accounts.pool,
                &claims.tenant_id,
                &claims.sub,
                session.get("auth_epoch"),
            )
            .await
            .map_err(enterprise_failure)?;
        }
        if !self
            .accounts
            .tenant_active(&claims.tenant_id)
            .await
            .map_err(|_| ServiceError::Unavailable)?
        {
            return Err(ServiceError::Credentials);
        }
        let authority = self
            .accounts
            .current_authority(&claims.sub, &claims.tenant_id)
            .await
            .map_err(|e| {
                if e == StoreError::Changed {
                    ServiceError::Credentials
                } else {
                    ServiceError::Unavailable
                }
            })?;
        Ok(Principal {
            account,
            authority,
            jti: claims.jti,
            expires_at: claims.exp,
        })
    }
    pub async fn change_password(
        &self,
        token: &str,
        current: String,
        new: String,
        confirmation: &str,
        audit: &AuditContext,
    ) -> Result<(), ServiceError> {
        crate::validate_password_change(&current, &new, confirmation)
            .map_err(|_| ServiceError::Invalid)?;
        let principal = self.authenticate(token).await?;
        let old_hash = principal
            .account
            .password_hash
            .as_deref()
            .ok_or(ServiceError::CurrentPassword)?;
        if !self
            .passwords
            .verify(current, old_hash.to_owned())
            .await
            .map_err(|_| ServiceError::Unavailable)?
        {
            return Err(ServiceError::CurrentPassword);
        }
        let new_hash = self
            .passwords
            .hash(new)
            .await
            .map_err(|_| ServiceError::Unavailable)?;
        self.accounts
            .change_password(
                &principal.account.user_id,
                &principal.account.tenant_id,
                old_hash,
                &new_hash,
                audit,
            )
            .await
            .map_err(|e| {
                if e == StoreError::Changed {
                    ServiceError::Credentials
                } else {
                    ServiceError::Unavailable
                }
            })
    }
    pub async fn logout(&self, token: &str, audit: &AuditContext) -> Result<(), ServiceError> {
        let now = (self.clock)()?;
        let claims = self
            .codec
            .verify(token, now)
            .map_err(|_| ServiceError::Credentials)?;
        self.sessions
            .revoke(&claims.jti, &claims.user_id)
            .await
            .map_err(|_| ServiceError::Unavailable)?;
        self.accounts
            .audit_event(Some(&claims.user_id), None, AuditAction::Logout, audit)
            .await
            .map_err(|_| ServiceError::Unavailable)
    }
}

fn enterprise_failure(error: crate::enterprise::EnterpriseError) -> ServiceError {
    if matches!(error, crate::enterprise::EnterpriseError::Unavailable) {
        ServiceError::Unavailable
    } else {
        ServiceError::Credentials
    }
}
