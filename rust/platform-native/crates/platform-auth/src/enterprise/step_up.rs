//! WebAuthn re-authentication of the current BFF session, never a new login.
use super::*;
use crate::{browser_policy::session_cookie, sessions::browser::BrowserSession};
use serde::{Deserialize, Serialize};

pub const TTL: u64 = 300;

/// One shared policy for all browser mutations. Match path segments, including
/// legacy config keys and account administration which can assign roles.
pub fn sensitive_mutation(method: &axum::http::Method, path: &str) -> bool {
    use axum::http::Method;
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return false;
    }
    let under =
        |root: &str| path == root || path.strip_prefix(root).is_some_and(|p| p.starts_with('/'));
    [
        "/api/v1/api-keys",
        "/api/v1/config/api-keys",
        "/api/v1/providers",
        "/api/v1/roles",
        "/api/v1/config/auth",
        "/api/v1/sso",
        "/api/v1/config/sso",
        "/api/auth/enterprise/identities",
        "/api/auth/enterprise/scim-credentials",
    ]
    .iter()
    .any(|root| under(root))
        || (under("/api/v1/users") && !path.ends_with("/profile"))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    method: String,
    jti: String,
    tenant: String,
    user: String,
    epoch: u64,
    binding: String,
    verified_at: u64,
    expires_at: u64,
}
fn binding(id: &str, session: &BrowserSession) -> String {
    digest(
        format!(
            "{id}\0{}\0{}\0{}\0{}",
            session.jti, session.tenant_id, session.user_id, session.csrf
        )
        .as_bytes(),
    )
}
fn valid_receipt(
    raw: Value,
    p: &Principal,
    epoch: u64,
    now: u64,
    expected_binding: Option<&str>,
) -> Result<bool> {
    let r: Receipt = serde_json::from_value(raw).map_err(|_| EnterpriseError::Unavailable)?;
    Ok(r.method == "passkey"
        && r.jti == p.jti
        && r.tenant == p.account.tenant_id
        && r.user == p.account.user_id
        && r.epoch == epoch
        && r.verified_at <= now
        && now < r.expires_at
        && r.expires_at.checked_sub(r.verified_at) == Some(TTL)
        && expected_binding.is_none_or(|b| r.binding == b))
}

/// Called only after normal live-session authentication and CSRF validation.
pub(crate) async fn recent(
    auth: &AuthService,
    p: &Principal,
    browser: Option<(&str, &BrowserSession)>,
) -> Result<bool> {
    let record = auth
        .sessions
        .record(&p.jti)
        .await
        .map_err(|_| EnterpriseError::Unavailable)?
        .ok_or(EnterpriseError::Proof)?;
    let epoch = record
        .get("auth_epoch")
        .and_then(Value::as_u64)
        .ok_or(EnterpriseError::Proof)?;
    policy::verify_session_epoch(
        &auth.accounts.pool,
        &p.account.tenant_id,
        &p.account.user_id,
        Some(&json!(epoch)),
    )
    .await?;
    let Some(raw) = auth
        .sessions
        .step_up(&p.jti)
        .await
        .map_err(|_| EnterpriseError::Unavailable)?
    else {
        return Ok(false);
    };
    let expected = browser.map(|(id, session)| binding(id, session));
    valid_receipt(
        raw,
        p,
        epoch,
        (auth.clock)().map_err(|_| EnterpriseError::Unavailable)?,
        expected.as_deref(),
    )
}

#[derive(Serialize, Deserialize)]
struct Flow {
    assertion: passkeys::AuthenticationFlow,
    jti: String,
    revision: Value,
    binding: String,
}
impl EnterpriseService {
    /// Independently enforce the cookie and CSRF contract even if mounted
    /// without BrowserAuth::bridge. A bearer alone never starts re-authentication.
    pub(super) async fn step_up_browser(
        &self,
        headers: &HeaderMap,
        method: &axum::http::Method,
    ) -> Result<(String, BrowserSession, Principal)> {
        if !self.auth.enterprise_policy_enabled() {
            return Err(EnterpriseError::Unavailable);
        }
        let id = session_cookie(headers)
            .map_err(|_| EnterpriseError::Proof)?
            .ok_or(EnterpriseError::Proof)?;
        let session = self
            .auth
            .sessions
            .browser(&id)
            .await
            .map_err(|_| EnterpriseError::Unavailable)?
            .ok_or(EnterpriseError::Proof)?;
        self.browser
            .check(method, headers, &session.csrf)
            .map_err(|_| EnterpriseError::Forbidden)?;
        let p = self
            .auth
            .authenticate_browser(&session)
            .await
            .map_err(|error| match error {
                crate::service::ServiceError::Unavailable => EnterpriseError::Unavailable,
                _ => EnterpriseError::Proof,
            })?;
        policy::verify_session_epoch(
            &self.pool,
            &session.tenant_id,
            &session.user_id,
            Some(&json!(self.authenticated_session_epoch(&p).await?)),
        )
        .await?;
        Ok((id, session, p))
    }
    pub(super) async fn step_up_start(&self, headers: &HeaderMap) -> Result<Value> {
        let (id, session, p) = self
            .step_up_browser(headers, &axum::http::Method::POST)
            .await?;
        self.transient
            .rate("step-up-start", &p.jti, 10, TTL)
            .await?;
        let epoch = self.authenticated_session_epoch(&p).await?;
        let (mut challenge, assertion) = self
            .assertion_challenge(&session.tenant_id, &session.user_id, epoch)
            .await?;
        let flow_id = crate::sessions::browser::opaque();
        let bound = binding(&id, &session);
        self.transient
            .put(
                "passkey-step-up",
                &flow_id,
                &bound,
                &Flow {
                    assertion,
                    jti: p.jti.clone(),
                    revision: revision(&p),
                    binding: bound.clone(),
                },
            )
            .await?;
        challenge["flow_id"] = json!(flow_id);
        Ok(challenge)
    }
    pub(super) async fn step_up_finish(
        &self,
        headers: &HeaderMap,
        input: passkeys::AuthenticationFinish,
    ) -> Result<Value> {
        let (id, session, p) = self
            .step_up_browser(headers, &axum::http::Method::POST)
            .await?;
        self.transient
            .rate("step-up-finish", &p.jti, 10, TTL)
            .await?;
        let bound = binding(&id, &session);
        let flow: Flow = self
            .transient
            .take("passkey-step-up", &input.flow_id, &bound)
            .await?;
        if flow.jti != p.jti
            || flow.binding != bound
            || flow.revision != revision(&p)
            || flow.assertion.tenant != session.tenant_id
            || flow.assertion.user != session.user_id
            || flow.assertion.epoch != self.authenticated_session_epoch(&p).await?
        {
            return Err(EnterpriseError::Proof);
        }
        let mut tx = self.pool.begin().await?;
        self.verify_assertion(&mut tx, &flow.assertion, &input.credential)
            .await?;
        tx.commit().await?;
        // This neither rotates nor extends the existing cookie/JTI. Recheck
        // live authority after verification before writing the bound receipt.
        let (_, current, current_p) = self
            .step_up_browser(headers, &axum::http::Method::POST)
            .await?;
        if binding(&id, &current) != bound || current_p.jti != p.jti {
            return Err(EnterpriseError::Proof);
        }
        let now = self.now()?;
        let expires_at = now.checked_add(TTL).ok_or(EnterpriseError::Unavailable)?;
        let receipt = serde_json::to_value(Receipt {
            method: "passkey".into(),
            jti: p.jti.clone(),
            tenant: session.tenant_id.clone(),
            user: session.user_id.clone(),
            epoch: flow.assertion.epoch,
            binding: bound,
            verified_at: now,
            expires_at,
        })
        .map_err(|_| EnterpriseError::Unavailable)?;
        let record = self
            .auth
            .sessions
            .record(&p.jti)
            .await
            .map_err(|_| EnterpriseError::Unavailable)?
            .ok_or(EnterpriseError::Proof)?;
        if record.get("auth_epoch").and_then(Value::as_u64) != Some(flow.assertion.epoch) {
            return Err(EnterpriseError::Proof);
        }
        self.auth
            .sessions
            .save_step_up(
                &id,
                &session,
                &record,
                &receipt,
                TTL.min(session.expires_at.saturating_sub(now)),
            )
            .await
            .map_err(|error| match error {
                crate::accounts::StoreError::Unavailable => EnterpriseError::Unavailable,
                _ => EnterpriseError::Proof,
            })?;
        Ok(json!({"verified":true,"expires_at":expires_at.min(session.expires_at)}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn route_policy_covers_sensitive_mutations_without_reads_or_prefix_collisions() {
        for root in [
            "/api/v1/api-keys",
            "/api/v1/config/api-keys",
            "/api/v1/providers",
            "/api/v1/roles",
            "/api/v1/users",
            "/api/v1/config/auth",
            "/api/v1/config/sso",
            "/api/auth/enterprise/identities",
            "/api/auth/enterprise/scim-credentials",
        ] {
            for method in [
                axum::http::Method::POST,
                axum::http::Method::PUT,
                axum::http::Method::PATCH,
                axum::http::Method::DELETE,
            ] {
                assert!(sensitive_mutation(&method, root));
                assert!(sensitive_mutation(&method, &format!("{root}/id/revoke")));
            }
            assert!(!sensitive_mutation(&axum::http::Method::GET, root));
            assert!(!sensitive_mutation(
                &axum::http::Method::POST,
                &format!("{root}-unrelated")
            ));
        }
        assert!(!sensitive_mutation(
            &axum::http::Method::PUT,
            "/api/v1/users/user/profile"
        ));
        assert!(!sensitive_mutation(
            &axum::http::Method::POST,
            "/api/auth/enterprise/passkeys/step-up/start"
        ));
    }
    #[test]
    fn opaque_session_and_csrf_are_part_of_the_proof_binding() {
        let mut s = BrowserSession {
            jti: "j".into(),
            tenant_id: "t".into(),
            user_id: "u".into(),
            csrf: "csrf".into(),
            issued_at: 0,
            idle_expires_at: 100,
            expires_at: 100,
            credential_revision: Value::Null,
        };
        let initial = binding("opaque", &s);
        assert_ne!(initial, binding("other-opaque", &s));
        s.csrf = "rotated".into();
        assert_ne!(initial, binding("opaque", &s));
        s.csrf = "csrf".into();
        s.tenant_id = "other".into();
        assert_ne!(initial, binding("opaque", &s));
    }
    fn principal() -> Principal {
        Principal {
            account: crate::accounts::Account {
                user_id: "u".into(),
                tenant_id: "t".into(),
                email: None,
                display_name: None,
                department: None,
                tier: "normal".into(),
                roles: vec![],
                permissions: vec![],
                status: "active".into(),
                password_hash: None,
                force_password_change: false,
                login_attempts: 0,
                locked_until: None,
                password_changed_at: None,
            },
            authority: crate::accounts::Authority {
                roles: vec![],
                permissions: vec![],
                tier: "normal".into(),
            },
            jti: "same-existing-jti".into(),
            expires_at: 1000,
        }
    }
    #[test]
    fn assurance_rejects_expiry_future_proof_login_only_and_each_cross_session_dimension() {
        let p = principal();
        let receipt = json!({"method":"passkey","jti":p.jti,"tenant":"t","user":"u","epoch":7,
            "binding":"opaque-and-csrf-digest","verified_at":100,"expires_at":400});
        assert!(
            valid_receipt(receipt.clone(), &p, 7, 100, Some("opaque-and-csrf-digest")).unwrap()
        );
        assert!(
            valid_receipt(receipt.clone(), &p, 7, 399, Some("opaque-and-csrf-digest")).unwrap()
        );
        assert!(!valid_receipt(receipt.clone(), &p, 7, 400, None).unwrap());
        assert!(!valid_receipt(receipt.clone(), &p, 7, 99, None).unwrap());
        assert!(!valid_receipt(receipt.clone(), &p, 8, 110, None).unwrap());
        assert!(
            !valid_receipt(receipt.clone(), &p, 7, 110, Some("another-cookie-or-csrf")).unwrap()
        );
        for (field, other) in [
            ("jti", json!("another-login")),
            ("tenant", json!("another-tenant")),
            ("user", json!("another-user")),
            ("method", json!("oidc_mfa")),
            ("method", json!("recovery")),
            ("expires_at", json!(401)),
            ("epoch", json!(6)),
        ] {
            let mut changed = receipt.clone();
            changed[field] = other;
            assert!(
                !valid_receipt(changed, &p, 7, 110, None).unwrap(),
                "{field}"
            );
        }
        // Existing login assurance rows and client assertions have no bound
        // receipt shape; neither can stand in for server-produced re-auth.
        assert!(valid_receipt(json!({"method":"passkey","jti":p.jti}), &p, 7, 110, None).is_err());
        assert!(valid_receipt(json!({"verified":true}), &p, 7, 110, None).is_err());
    }
}
