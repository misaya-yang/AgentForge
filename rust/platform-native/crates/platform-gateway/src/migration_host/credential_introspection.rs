//! GET /auth/me-only metadata adapter. Never creates/caches an authorization Principal.
use super::jwt::JwtDecoder;
use ai_platform_auth::{
    accounts::PgAccounts, contract::CurrentUserResponse, http::CredentialIntrospectionResolver,
    service::ServiceError, sessions::RedisSessions,
};
use axum::http::{HeaderMap, HeaderName, header::AUTHORIZATION};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::{future::Future, pin::Pin, sync::Arc};

/// Metadata decoding only; this must never manufacture a protected Principal.
pub trait IntrospectionDecoder: Send + Sync {
    fn decode(&self, token: &str) -> Result<Value, String>;
    fn enterprise_policy_enabled(&self) -> bool {
        false
    }
}
impl IntrospectionDecoder for JwtDecoder {
    fn decode(&self, token: &str) -> Result<Value, String> {
        JwtDecoder::decode(self, token)
    }
}
pub type NativeIntrospectionVerifier = dyn Fn(&str) -> Result<Value, String> + Send + Sync;
/// Dispatch by the signed algorithm family. Native EdDSA uses the current key
/// ring, issuer/audience and native claim rules; external HS profiles retain the
/// existing permissive decoder. A failed EdDSA decode never falls back to HS.
pub struct RotatingIntrospectionDecoder {
    legacy: Arc<JwtDecoder>,
    native: Arc<NativeIntrospectionVerifier>,
    enterprise_policy_enabled: bool,
}
impl RotatingIntrospectionDecoder {
    pub fn new(legacy: Arc<JwtDecoder>, native: Arc<NativeIntrospectionVerifier>) -> Self {
        Self {
            legacy,
            native,
            enterprise_policy_enabled: false,
        }
    }
    pub fn with_enterprise_policy_enabled(mut self, enabled: bool) -> Self {
        self.enterprise_policy_enabled = enabled;
        self
    }
}
impl IntrospectionDecoder for RotatingIntrospectionDecoder {
    fn enterprise_policy_enabled(&self) -> bool {
        self.enterprise_policy_enabled
    }
    fn decode(&self, token: &str) -> Result<Value, String> {
        let header = jsonwebtoken::decode_header(token)
            .map_err(|_| "invalid introspection credential".to_owned())?;
        if header.alg == jsonwebtoken::Algorithm::EdDSA {
            (self.native)(token)
        } else {
            self.legacy.decode(token)
        }
    }
}

pub struct CredentialIntrospection {
    pub pool: PgPool,
    pub decoder: Arc<dyn IntrospectionDecoder>,
    pub sessions: Option<RedisSessions>,
    pub jwt_enabled: bool,
    pub api_key_enabled: bool,
    pub api_key_header: HeaderName,
    pub static_key_hashes: Vec<String>,
    pub static_key_role: String,
}
fn strings(value: Option<&Value>) -> Vec<String> {
    let values = match value {
        Some(Value::Array(v)) => v.iter().filter_map(Value::as_str).collect(),
        Some(Value::String(s)) => vec![s.as_str()],
        _ => vec![],
    };
    let mut out = vec![];
    for value in values {
        let v = value.trim().to_owned();
        if !v.is_empty() && !out.contains(&v) {
            out.push(v);
        }
    }
    out
}
fn tier(roles: &[String], base: &str) -> String {
    if roles.iter().any(|s| s == "admin") {
        "admin"
    } else if roles.iter().any(|s| s == "enterprise") {
        "enterprise"
    } else if roles.iter().any(|s| s == "premium" || s == "vip") {
        "premium"
    } else {
        base
    }
    .into()
}

/// Native local logins pin the durable account revision in their Redis session.
/// External identities without this marker keep their existing profile fallback.
fn is_native_local_session(session: &Value) -> bool {
    session.get("credential_revision").is_some()
}

fn is_local_session_for_user(session: &Value, user: &str) -> bool {
    is_native_local_session(session) || session.get("user_id").and_then(Value::as_str) == Some(user)
}

fn local_session_revision_matches(
    session: &Value,
    user: &str,
    password_changed_at: Option<&chrono::DateTime<chrono::Utc>>,
    issued_at: Option<u64>,
) -> bool {
    if session.get("user_id").and_then(Value::as_str) != Some(user) {
        return false;
    }
    if let Some(revision) = session.get("credential_revision") {
        let current = serde_json::json!(password_changed_at.map(|value| value.to_rfc3339()));
        revision == &current
    } else {
        !password_changed_at.is_some_and(|changed| {
            changed.timestamp() >= 0
                && issued_at.is_some_and(|issued| issued <= changed.timestamp() as u64)
        })
    }
}

impl CredentialIntrospection {
    async fn inspect(&self, headers: &HeaderMap) -> Result<CurrentUserResponse, ServiceError> {
        let authorization = headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let (user, tenant, mut roles, mut permissions, mut user_tier, session, issued_at) = if self
            .jwt_enabled
            && authorization.to_ascii_lowercase().starts_with("bearer ")
        {
            if headers.get_all(AUTHORIZATION).iter().count() != 1 {
                return Err(ServiceError::Credentials);
            }
            let claims = self
                .decoder
                .decode(authorization[7..].trim())
                .map_err(|_| ServiceError::Credentials)?;
            let user = claims
                .get("sub")
                .or_else(|| claims.get("user_id"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .ok_or(ServiceError::Credentials)?
                .to_owned();
            let tenant = claims
                .get("tenant_id")
                .or_else(|| claims.get("tenant"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let session = if let (Some(sessions), Some(jti)) = (
                &self.sessions,
                claims
                    .get("jti")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty()),
            ) {
                Some(
                    sessions
                        .record(jti)
                        .await
                        .map_err(|e| {
                            if e == ai_platform_auth::accounts::StoreError::InvalidInput {
                                ServiceError::Credentials
                            } else {
                                ServiceError::Unavailable
                            }
                        })?
                        .ok_or(ServiceError::Credentials)?,
                )
            } else {
                None
            };
            let issued_at = claims
                .get("iat")
                .and_then(Value::as_u64)
                .or_else(|| claims.get("iat").and_then(Value::as_str)?.parse().ok());
            let mut roles = strings(claims.get("roles").or_else(|| claims.get("role")));
            if roles.is_empty() {
                roles.push("user".into());
            }
            let level = tier(
                &roles,
                claims
                    .get("tier")
                    .and_then(Value::as_str)
                    .unwrap_or("normal"),
            );
            (
                user,
                tenant,
                roles,
                strings(claims.get("permissions")),
                level,
                session,
                issued_at,
            )
        } else if self.api_key_enabled {
            if headers.get_all(&self.api_key_header).iter().count() != 1 {
                return Err(ServiceError::Credentials);
            }
            let key = headers
                .get(&self.api_key_header)
                .and_then(|s| s.to_str().ok())
                .filter(|s| !s.is_empty() && s.len() <= 8192)
                .ok_or(ServiceError::Credentials)?;
            let hash = format!("{:x}", Sha256::digest(key.as_bytes()));
            let row = sqlx::query("SELECT id,user_id,tenant_id,roles,permissions,tier FROM gateway.api_keys WHERE key_hash=$1 AND enabled=TRUE AND (expires_at IS NULL OR expires_at>NOW())")
                .bind(&hash).fetch_optional(&self.pool).await.map_err(|_| ServiceError::Unavailable)?;
            if let Some(row) = row {
                let user: String = row
                    .try_get("user_id")
                    .map_err(|_| ServiceError::Unavailable)?;
                let tenant: String = row
                    .try_get("tenant_id")
                    .map_err(|_| ServiceError::Unavailable)?;
                if user.is_empty() || tenant.is_empty() {
                    return Err(ServiceError::Credentials);
                }
                let id: i32 = row.try_get("id").map_err(|_| ServiceError::Unavailable)?;
                let _ = sqlx::query("UPDATE gateway.api_keys SET last_used_at=NOW(),use_count=COALESCE(use_count,0)+1 WHERE id=$1 AND key_hash=$2 AND user_id=$3 AND tenant_id=$4")
                    .bind(id).bind(&hash).bind(&user).bind(&tenant).execute(&self.pool).await;
                let mut roles: Vec<String> = row
                    .try_get("roles")
                    .map_err(|_| ServiceError::Unavailable)?;
                if roles.is_empty() {
                    roles.push("user".into());
                }
                (
                    user,
                    tenant,
                    roles,
                    row.try_get::<Vec<String>, _>("permissions")
                        .map_err(|_| ServiceError::Unavailable)?,
                    row.try_get::<String, _>("tier")
                        .map_err(|_| ServiceError::Unavailable)?,
                    None,
                    None,
                )
            } else {
                if !self
                    .static_key_hashes
                    .iter()
                    .fold(false, |matched, allowed| {
                        crate::migration_content::capabilities::constant_time_equal(&hash, allowed)
                            | matched
                    })
                {
                    return Err(ServiceError::Credentials);
                }
                let role = if ["user", "admin", "operator", "developer"]
                    .contains(&self.static_key_role.trim())
                {
                    self.static_key_role.trim()
                } else {
                    "user"
                };
                return Ok(CurrentUserResponse {
                    user_id: format!("apikey:{}", &hash[..16]),
                    tenant_id: String::new(),
                    email: None,
                    display_name: None,
                    department: None,
                    roles: vec![role.into()],
                    permissions: vec![],
                    effective_permissions: vec![],
                    tier: if role == "admin" { "admin" } else { "normal" }.into(),
                    force_password_change: false,
                });
            }
        } else {
            return Err(ServiceError::Credentials);
        };
        let accounts = PgAccounts::new(self.pool.clone());
        let account = accounts
            .by_id(&user)
            .await
            .map_err(|_| ServiceError::Unavailable)?;
        if let Some(account) = account {
            if account.status != "active" || account.user_id != user || account.tenant_id != tenant
            {
                return Err(ServiceError::Credentials);
            }
            let authority = if let Some(session) = session.as_ref() {
                if is_local_session_for_user(session, &user) {
                    if !local_session_revision_matches(
                        session,
                        &user,
                        account.password_changed_at.as_ref(),
                        issued_at,
                    ) {
                        return Err(ServiceError::Credentials);
                    }
                    if self.decoder.enterprise_policy_enabled() {
                        ai_platform_auth::enterprise::policy::verify_session_epoch(
                            &self.pool,
                            &tenant,
                            &user,
                            session.get("auth_epoch"),
                        )
                        .await
                        .map_err(|e| {
                            if matches!(
                                e,
                                ai_platform_auth::enterprise::EnterpriseError::Unavailable
                            ) {
                                ServiceError::Unavailable
                            } else {
                                ServiceError::Credentials
                            }
                        })?;
                    }
                    accounts
                        .current_authority(&user, &tenant)
                        .await
                        .map_err(|error| {
                            if error == ai_platform_auth::accounts::StoreError::Changed {
                                ServiceError::Credentials
                            } else {
                                ServiceError::Unavailable
                            }
                        })?
                } else {
                    accounts
                        .legacy_authority(&user, &tenant)
                        .await
                        .map_err(|_| ServiceError::Unavailable)?
                }
            } else {
                accounts
                    .legacy_authority(&user, &tenant)
                    .await
                    .map_err(|_| ServiceError::Unavailable)?
            };
            // Local native sessions expose the same nonexpired assignments used
            // by protected admission; external-profile compatibility is unchanged.
            return Ok(CurrentUserResponse {
                user_id: user,
                tenant_id: tenant,
                email: account.email,
                display_name: account.display_name,
                department: account.department,
                roles: authority.roles,
                permissions: authority.permissions,
                effective_permissions: vec![],
                tier: account.tier,
                force_password_change: account.force_password_change,
            });
        }
        // A deleted local account must not fall through to the external identity
        // projection while its local session revision record is still alive.
        if session.as_ref().is_some_and(is_native_local_session) {
            return Err(ServiceError::Credentials);
        }
        // Missing local profile is an external identity, not proof of lookup failure.
        let mut current: Vec<String> = sqlx::query_scalar("SELECT DISTINCT rp.permission_code FROM gateway.user_roles ur JOIN gateway.role_permissions rp ON ur.role_name=rp.role_name WHERE ur.user_id=$1 AND (ur.expires_at IS NULL OR ur.expires_at>NOW())")
            .bind(&user).fetch_all(&self.pool).await.map_err(|_| ServiceError::Unavailable)?;
        if current.is_empty() {
            current = sqlx::query_scalar("SELECT DISTINCT unnest(rr.permissions) FROM gateway.user_roles ur JOIN gateway.rbac_roles rr ON ur.role_name=rr.role_name WHERE ur.user_id=$1 AND (ur.expires_at IS NULL OR ur.expires_at>NOW())")
                .bind(&user).fetch_all(&self.pool).await.map_err(|_| ServiceError::Unavailable)?;
        }
        current.extend(sqlx::query_scalar::<_, String>("SELECT permission_code FROM gateway.user_permissions WHERE user_id=$1 AND (expires_at IS NULL OR expires_at>NOW())").bind(&user).fetch_all(&self.pool).await.map_err(|_| ServiceError::Unavailable)?);
        permissions.extend(current);
        for permission in permissions {
            if !roles.contains(&permission) {
                roles.push(permission);
            }
        }
        if user_tier.is_empty() {
            user_tier = "normal".into();
        }
        let permissions = roles.iter().filter(|s| s.contains(':')).cloned().collect();
        Ok(CurrentUserResponse {
            user_id: user,
            tenant_id: tenant,
            email: None,
            display_name: None,
            department: None,
            roles,
            permissions,
            effective_permissions: vec![],
            tier: user_tier,
            force_password_change: false,
        })
    }
}
impl CredentialIntrospectionResolver for CredentialIntrospection {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
    ) -> Pin<Box<dyn Future<Output = Result<CurrentUserResponse, ServiceError>> + Send + 'a>> {
        Box::pin(self.inspect(headers))
    }
}

#[cfg(test)]
mod local_session_tests {
    use super::{
        is_local_session_for_user, is_native_local_session, local_session_revision_matches,
    };
    use serde_json::json;

    fn timestamp(value: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(value)
            .expect("valid test timestamp")
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn native_local_marker_pins_the_exact_password_revision() {
        let initial = timestamp("2026-10-16T10:00:00+00:00");
        let session = json!({
            "user_id": "member-a",
            "credential_revision": initial.to_rfc3339(),
        });
        assert!(is_native_local_session(&session));
        assert!(local_session_revision_matches(
            &session,
            "member-a",
            Some(&initial),
            Some(1_792_142_400),
        ));

        // Reset, disable, and re-enable all retain the changed revision. The old
        // token cannot revive merely because account status becomes active again.
        let changed = timestamp("2026-10-16T10:00:01+00:00");
        assert!(!local_session_revision_matches(
            &session,
            "member-a",
            Some(&changed),
            Some(1_792_142_400),
        ));
    }

    #[test]
    fn native_local_revision_is_bound_to_its_user_and_deleted_profiles_stay_local() {
        let session = json!({"user_id":"member-a","credential_revision":null});
        assert!(is_native_local_session(&session));
        assert!(is_local_session_for_user(&session, "member-a"));
        assert!(local_session_revision_matches(
            &session,
            "member-a",
            None,
            Some(1_792_142_400),
        ));
        assert!(!local_session_revision_matches(
            &session,
            "member-b",
            None,
            Some(1_792_142_400),
        ));
    }

    #[test]
    fn legacy_local_session_uses_issued_at_and_stays_bound_to_its_user() {
        let session = json!({"user_id":"member-a"});
        let changed = timestamp("2026-10-16T10:00:00+00:00");
        assert!(!is_native_local_session(&session));
        assert!(is_local_session_for_user(&session, "member-a"));
        assert!(!local_session_revision_matches(
            &session,
            "member-a",
            Some(&changed),
            Some(changed.timestamp() as u64 - 1),
        ));
        assert!(!local_session_revision_matches(
            &session,
            "member-b",
            Some(&changed),
            Some(changed.timestamp() as u64 + 1),
        ));
        assert!(local_session_revision_matches(
            &session,
            "member-a",
            Some(&changed),
            None,
        ));
    }

    #[test]
    fn unmarked_external_identity_sessions_keep_legacy_compatibility() {
        let session = json!({"user_id":"external-subject"});
        assert!(!is_native_local_session(&session));
        // The compatibility path does not apply a local account revision to an
        // external identity session that lacks the native revision marker.
    }
}

#[cfg(test)]
mod rotating_decoder_tests {
    use super::*;
    use ai_platform_auth::{
        Claims, TokenCodec,
        access_tokens::{EdDsaKeyRing, PublicKeyConfig},
    };
    use ed25519_dalek::{
        SigningKey,
        pkcs8::{EncodePrivateKey, EncodePublicKey, spki::der::pem::LineEnding},
    };
    use jsonwebtoken::{Algorithm, EncodingKey, Header};
    use serde_json::json;
    const SECRET: &[u8] = b"introspection-synthetic-legacy-key-only";
    fn legacy() -> Arc<JwtDecoder> {
        Arc::new(
            JwtDecoder::new(
                std::str::from_utf8(SECRET).unwrap(),
                &["HS256".into()],
                None,
                None,
            )
            .unwrap(),
        )
    }
    fn codec(kid: &str) -> Arc<TokenCodec> {
        let signer = SigningKey::from_bytes(&[7; 32]);
        let ring = EdDsaKeyRing::new(
            kid.into(),
            signer.to_pkcs8_pem(LineEnding::LF).unwrap().as_str(),
            vec![PublicKeyConfig {
                kid: kid.into(),
                public_pem: signer
                    .verifying_key()
                    .to_public_key_pem(LineEnding::LF)
                    .unwrap(),
                not_before: 1,
                not_after: 10000,
            }],
        )
        .unwrap();
        Arc::new(
            TokenCodec::new(SECRET, Some("api".into()), Some("gateway".into()))
                .unwrap()
                .with_eddsa(ring)
                .unwrap(),
        )
    }
    fn claims() -> Claims {
        Claims {
            sub: "local-user".into(),
            user_id: "local-user".into(),
            tenant_id: "tenant".into(),
            email: None,
            roles: vec!["user".into()],
            permissions: vec![],
            tier: "normal".into(),
            exp: 0,
            iat: 0,
            nbf: None,
            jti: String::new(),
            aud: None,
            iss: None,
        }
    }
    fn decoder(codec: Arc<TokenCodec>, now: u64) -> RotatingIntrospectionDecoder {
        RotatingIntrospectionDecoder::new(
            legacy(),
            Arc::new(move |token| {
                codec
                    .verify(token, now)
                    .and_then(|claims| {
                        serde_json::to_value(claims).map_err(|_| ai_platform_auth::AuthError::Token)
                    })
                    .map_err(|_| "invalid native metadata credential".into())
            }),
        )
    }
    #[test]
    fn eddsa_metadata_routes_through_native_ring_and_failures_do_not_fallback() {
        let current = codec("current");
        let token = current.issue(claims(), 100).unwrap();
        let decoded = decoder(current.clone(), 101).decode(&token).unwrap();
        assert_eq!(decoded["sub"], "local-user");
        assert_eq!(decoded["tenant_id"], "tenant");
        assert!(decoded["jti"].as_str().is_some());
        // Exact expiry and retired/unknown kid must fail, even though the test
        // uses the same mathematical key under a different kid.
        assert!(decoder(current, 400).decode(&token).is_err());
        assert!(decoder(codec("replacement"), 101).decode(&token).is_err());
        let broken = format!("{}x", token);
        assert!(decoder(codec("current"), 101).decode(&broken).is_err());
    }
    #[test]
    fn external_hs_profile_does_not_acquire_native_required_claims() {
        let external =
            json!({"sub":"external-subject","tenant":"external-tenant","role":"premium"});
        let token = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &external,
            &EncodingKey::from_secret(SECRET),
        )
        .unwrap();
        // In particular there is no native user_id/iat/jti/exp requirement on
        // this historical metadata-only path. The native verifier must not run.
        let decoder = RotatingIntrospectionDecoder::new(
            legacy(),
            Arc::new(|_| panic!("external HS metadata entered native verifier")),
        );
        assert_eq!(decoder.decode(&token).unwrap(), external);
        let rejected = jsonwebtoken::encode(
            &Header::new(Algorithm::HS256),
            &external,
            &EncodingKey::from_secret(b"wrong synthetic secret"),
        )
        .unwrap();
        assert!(decoder.decode(&rejected).is_err());
    }
}
