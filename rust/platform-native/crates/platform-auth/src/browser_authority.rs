//! An opaque BFF binding is server-side authority; it is never a long-lived JWT.
// SPDX-License-Identifier: Apache-2.0
use crate::{
    Claims,
    service::{AuthService, Principal, ServiceError},
    sessions::browser::BrowserSession,
};
pub const BROWSER_ABSOLUTE_TTL: u64 = 10_800;
pub const BROWSER_IDLE_TTL: u64 = 10_800;
impl AuthService {
    pub(crate) async fn authenticate_browser(
        &self,
        session: &BrowserSession,
    ) -> Result<Principal, ServiceError> {
        let now = (self.clock)()?;
        validate_binding(session, now)?;
        let principal = self
            .authenticate_claims(Claims {
                sub: session.user_id.clone(),
                user_id: session.user_id.clone(),
                tenant_id: session.tenant_id.clone(),
                email: None,
                roles: vec![],
                permissions: vec![],
                tier: String::new(),
                exp: session.expires_at,
                iat: session.issued_at,
                nbf: None,
                jti: session.jti.clone(),
                aud: None,
                iss: None,
            })
            .await?;
        if session.credential_revision
            != serde_json::json!(
                principal
                    .account
                    .password_changed_at
                    .map(|time| time.to_rfc3339())
            )
        {
            return Err(ServiceError::Credentials);
        }
        Ok(principal)
    }
    /// Existing Gateway adapters consume bearer headers. This request-local
    /// token is short-lived, tied to the same live allowlist and current account,
    /// and is never persisted in the BFF record or returned to the browser.
    pub(crate) fn browser_request_bearer(&self, p: &Principal) -> Result<String, ServiceError> {
        let now = (self.clock)()?;
        self.codec
            .issue_for_session(
                Claims {
                    sub: p.account.user_id.clone(),
                    user_id: p.account.user_id.clone(),
                    tenant_id: p.account.tenant_id.clone(),
                    email: p.account.email.clone(),
                    roles: p.authority.roles.clone(),
                    permissions: p.authority.permissions.clone(),
                    tier: p.authority.tier.clone(),
                    exp: 0,
                    iat: 0,
                    nbf: None,
                    jti: String::new(),
                    aud: None,
                    iss: None,
                },
                now,
                &p.jti,
            )
            .map_err(|_| ServiceError::Unavailable)
    }
}

fn validate_binding(session: &BrowserSession, now: u64) -> Result<(), ServiceError> {
    if session.expires_at <= now
        || session.idle_expires_at <= now
        || session.issued_at > now
        || session
            .expires_at
            .checked_sub(session.issued_at)
            .is_none_or(|ttl| ttl > BROWSER_ABSOLUTE_TTL)
        || session.idle_expires_at > session.expires_at
    {
        return Err(ServiceError::Credentials);
    }
    if session.user_id.is_empty()
        || session.tenant_id.is_empty()
        || uuid::Uuid::parse_str(&session.jti).is_err()
    {
        return Err(ServiceError::Credentials);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_lifetime_is_three_hours_and_cannot_be_renewed_after_expiry() {
        let mut binding = BrowserSession {
            jti: uuid::Uuid::new_v4().to_string(),
            tenant_id: "t".into(),
            user_id: "u".into(),
            issued_at: 100,
            expires_at: 10900,
            idle_expires_at: 10900,
            csrf: "a".repeat(64),
            credential_revision: serde_json::Value::Null,
        };
        assert!(validate_binding(&binding, 401).is_ok()); // API token expired; BFF remains live.
        assert!(validate_binding(&binding, 10899).is_ok());
        assert!(validate_binding(&binding, 10900).is_err());
        binding.idle_expires_at = 500;
        assert!(validate_binding(&binding, 500).is_err());
        binding.idle_expires_at = 10900;
        binding.expires_at = 10901;
        assert!(validate_binding(&binding, 101).is_err());
        let serialized = serde_json::to_value(binding).unwrap();
        assert!(serialized.get("bearer").is_none());
        assert!(serialized.get("access_token").is_none());
    }
}
