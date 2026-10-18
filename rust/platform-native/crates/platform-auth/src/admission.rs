//! Request-scoped JWT / bound database-key admission. No listener or static-key fallback.
//!
//! Source: `src/api/deps.py` request caching and auth-failure recording, and
//! `ai_gateway_core/persistence/database.py` key usage / security daily aggregates.
//! Authority failures fail closed; observational writes remain best-effort, as in Python.
use crate::{
    accounts::{PgAccounts, StoreError},
    api_keys::{ApiKeyAuthority, SelectedCredential, select_credential},
    service::{AuthService, Principal, ServiceError},
};
use axum::http::{HeaderMap, HeaderName, header::AUTHORIZATION};
use sqlx::PgPool;
use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};
use tokio::sync::OnceCell;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    #[error("Authentication required")]
    Credentials,
    #[error("Authentication authority unavailable")]
    Unavailable,
}
impl AdmissionError {
    pub fn status(self) -> u16 {
        match self {
            Self::Credentials => 401,
            Self::Unavailable => 503,
        }
    }
}

/// Observable outcome only: never contains credentials, hashes or database errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditWrite {
    NotAttempted,
    /// The operation is pending or was cancelled; it must not be retried blindly.
    Unconfirmed,
    Recorded,
    /// The database did not confirm success. A timeout/commit error may still have written;
    /// no retry is performed because this schema has no per-request idempotency key.
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionAudit {
    pub key_usage: AuditWrite,
    pub auth_failure: AuditWrite,
}
#[derive(Default)]
struct WriteOnce(AtomicU8);
impl WriteOnce {
    fn begin(&self) -> bool {
        self.0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    fn finish(&self, result: Result<(), StoreError>) {
        self.0
            .store(if result.is_ok() { 2 } else { 3 }, Ordering::Release);
    }
    fn outcome(&self) -> AuditWrite {
        match self.0.load(Ordering::Acquire) {
            0 => AuditWrite::NotAttempted,
            1 => AuditWrite::Unconfirmed,
            2 => AuditWrite::Recorded,
            _ => AuditWrite::Unavailable,
        }
    }
}

enum VerifiedCredential {
    Jwt(Principal),
    ApiKey(ApiKeyAuthority),
}
/// Current authenticated identity. Deliberately has no Debug or Serialize implementation.
pub struct RequestPrincipal {
    credential: VerifiedCredential,
    roles: Vec<String>,
    tier: String,
}
impl RequestPrincipal {
    fn jwt(principal: Principal) -> Self {
        let mut roles = principal.authority.roles.clone();
        // Local-account tier is current database authority. Role names grant
        // permissions through RBAC but do not promote the account's tier.
        let tier = principal.authority.tier.clone();
        for permission in &principal.authority.permissions {
            if !roles.contains(permission) {
                roles.push(permission.clone());
            }
        }
        Self {
            credential: VerifiedCredential::Jwt(principal),
            roles,
            tier,
        }
    }
    fn key(authority: ApiKeyAuthority) -> Self {
        Self {
            roles: authority.roles.clone(),
            tier: authority.tier.clone(),
            credential: VerifiedCredential::ApiKey(authority),
        }
    }
    pub fn auth_type(&self) -> &'static str {
        match &self.credential {
            VerifiedCredential::Jwt(_) => "jwt",
            VerifiedCredential::ApiKey(_) => "api_key",
        }
    }
    pub fn user_id(&self) -> &str {
        match &self.credential {
            VerifiedCredential::Jwt(p) => &p.account.user_id,
            VerifiedCredential::ApiKey(p) => &p.user_id,
        }
    }
    pub fn tenant_id(&self) -> &str {
        match &self.credential {
            VerifiedCredential::Jwt(p) => &p.account.tenant_id,
            VerifiedCredential::ApiKey(p) => &p.tenant_id,
        }
    }
    pub fn roles(&self) -> &[String] {
        &self.roles
    }
    pub fn tier(&self) -> &str {
        &self.tier
    }
    pub fn permissions(&self) -> &[String] {
        match &self.credential {
            VerifiedCredential::Jwt(p) => &p.authority.permissions,
            VerifiedCredential::ApiKey(p) => &p.effective_permissions,
        }
    }
    /// Explicit JWT-only account/session authority; an API key cannot impersonate a session.
    pub fn jwt_principal(&self) -> Option<&Principal> {
        match &self.credential {
            VerifiedCredential::Jwt(p) => Some(p),
            VerifiedCredential::ApiKey(_) => None,
        }
    }
    /// Key-declared scopes and restrictions remain distinct from owner permissions.
    /// Callers must independently enforce service, model, rate and resource policy.
    pub fn api_key(&self) -> Option<&ApiKeyAuthority> {
        match &self.credential {
            VerifiedCredential::ApiKey(p) => Some(p),
            VerifiedCredential::Jwt(_) => None,
        }
    }
}

/// Explicit dependencies only; never reads environment configuration or issues keys.
pub struct AdmissionService {
    jwt: Arc<AuthService>,
    accounts: PgAccounts,
    pool: PgPool,
    key_header: HeaderName,
}
impl AdmissionService {
    pub fn new(
        jwt: Arc<AuthService>,
        pool: PgPool,
        key_header: HeaderName,
    ) -> Result<Self, StoreError> {
        if key_header == AUTHORIZATION {
            return Err(StoreError::InvalidInput);
        }
        Ok(Self {
            jwt,
            accounts: PgAccounts::new(pool.clone()),
            pool,
            key_header,
        })
    }
    /// Create exactly one value per incoming request and share it between dependencies.
    /// Keep headers/path immutable while resolving. Never retain this across requests.
    pub fn request<'a>(&'a self, headers: &'a HeaderMap, path: &'a str) -> AdmissionRequest<'a> {
        AdmissionRequest {
            inner: Resolution::new(self, headers, &self.key_header, path),
        }
    }
}
/// Local cache, including denials, bound to the selected immutable request inputs.
pub struct AdmissionRequest<'a> {
    inner: Resolution<'a, AdmissionService>,
}
impl AdmissionRequest<'_> {
    pub async fn resolve(&self) -> Result<&RequestPrincipal, AdmissionError> {
        let principal = self.inner.resolve().await?;
        crate::cedar::resources::remember_subject(crate::cedar::resources::ObjectSubject {
            tenant_id: principal.tenant_id().into(),
            user_id: principal.user_id().into(),
            roles: principal.roles().to_vec(),
            permissions: principal.permissions().to_vec(),
            tier: principal.tier().into(),
            anonymous: false,
        });
        Ok(principal)
    }
    pub fn audit(&self) -> AdmissionAudit {
        self.inner.audit()
    }
}

trait Backend: Sync {
    fn jwt(&self, token: &str) -> impl Future<Output = Result<Principal, AdmissionError>> + Send;
    fn key(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<ApiKeyAuthority, AdmissionError>> + Send;
    fn usage(
        &self,
        authority: &ApiKeyAuthority,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;
    fn failure(&self, path: &str) -> impl Future<Output = Result<(), StoreError>> + Send;
}
struct Resolution<'a, B> {
    backend: &'a B,
    headers: &'a HeaderMap,
    key_header: &'a HeaderName,
    path: &'a str,
    result: OnceCell<Result<RequestPrincipal, AdmissionError>>,
    usage: WriteOnce,
    failure: WriteOnce,
}
impl<'a, B: Backend> Resolution<'a, B> {
    fn new(
        backend: &'a B,
        headers: &'a HeaderMap,
        key_header: &'a HeaderName,
        path: &'a str,
    ) -> Self {
        Self {
            backend,
            headers,
            key_header,
            path,
            result: OnceCell::new(),
            usage: WriteOnce::default(),
            failure: WriteOnce::default(),
        }
    }
    async fn resolve(&self) -> Result<&RequestPrincipal, AdmissionError> {
        self.result
            .get_or_init(|| async {
                let result = match select_credential(self.headers, self.key_header) {
                    Ok(SelectedCredential::Jwt(token)) => {
                        self.backend.jwt(token).await.map(RequestPrincipal::jwt)
                    }
                    Ok(SelectedCredential::ApiKey(key)) => match self.backend.key(key).await {
                        Ok(authority) => {
                            // Mark before awaiting: cancellation must not double-count an uncertain write.
                            if self.usage.begin() {
                                self.usage.finish(self.backend.usage(&authority).await);
                            }
                            Ok(RequestPrincipal::key(authority))
                        }
                        Err(error) => Err(error),
                    },
                    Err(_) => Err(AdmissionError::Credentials),
                };
                if result.is_err() && self.failure.begin() {
                    self.failure.finish(self.backend.failure(self.path).await);
                }
                result
            })
            .await
            .as_ref()
            .map_err(|error| *error)
    }
    fn audit(&self) -> AdmissionAudit {
        AdmissionAudit {
            key_usage: self.usage.outcome(),
            auth_failure: self.failure.outcome(),
        }
    }
}

fn service_id(path: &str) -> Option<&str> {
    ["/api/v1/proxy/", "/proxy/"]
        .into_iter()
        .find_map(|prefix| path.strip_prefix(prefix))
        .and_then(|rest| rest.split('/').next())
        .filter(|value| !value.is_empty())
}
impl Backend for AdmissionService {
    async fn jwt(&self, token: &str) -> Result<Principal, AdmissionError> {
        self.jwt
            .authenticate(token)
            .await
            .map_err(|error| match error {
                ServiceError::Unavailable => AdmissionError::Unavailable,
                _ => AdmissionError::Credentials,
            })
    }
    async fn key(&self, key: &str) -> Result<ApiKeyAuthority, AdmissionError> {
        self.accounts
            .bound_api_key(key)
            .await
            .map_err(|error| match error {
                StoreError::Unavailable => AdmissionError::Unavailable,
                _ => AdmissionError::Credentials,
            })
    }
    async fn usage(&self, authority: &ApiKeyAuthority) -> Result<(), StoreError> {
        // Bind the already-verified owner and hash; never mutate a rebound/replaced row.
        let result = sqlx::query("UPDATE public.api_keys SET last_used_at=NOW(),use_count=COALESCE(use_count,0)+1 WHERE id=$1 AND key_hash=$2 AND user_id=$3 AND tenant_id=$4")
            .bind(authority.id).bind(&authority.key_hash).bind(&authority.user_id).bind(&authority.tenant_id)
            .execute(&self.pool).await.map_err(|_| StoreError::Unavailable)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(StoreError::Changed)
        }
    }
    async fn failure(&self, path: &str) -> Result<(), StoreError> {
        // Failed credentials do not establish tenant/user authority. Never decode unverified
        // claims or trust caller identity headers to attribute an authentication failure.
        let service = service_id(path).unwrap_or("");
        let date = chrono::Utc::now().date_naive();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
            .execute(&mut *tx)
            .await
            .map_err(|_| StoreError::Unavailable)?;
        // Target-free conflict handling supports both migration 018's expression index and
        // migration 030's plain unique constraint. A second statement gets a fresh snapshot
        // after waiting for a competing insert, unlike a single INSERT/UPDATE CTE.
        let inserted = sqlx::query("INSERT INTO public.security_event_daily_aggregates(tenant_id,user_id,service_id,event_type,date,event_count) VALUES('public','',$1,'auth_failed',$2,1) ON CONFLICT DO NOTHING")
            .bind(service).bind(date).execute(&mut *tx).await.map_err(|_| StoreError::Unavailable)?.rows_affected();
        if inserted == 0 {
            let updated = sqlx::query("UPDATE public.security_event_daily_aggregates SET event_count=COALESCE(event_count,0)+1,updated_at=CURRENT_TIMESTAMP WHERE tenant_id='public' AND COALESCE(user_id,'')='' AND COALESCE(service_id,'')=$1 AND event_type='auth_failed' AND date=$2")
                .bind(service).bind(date).execute(&mut *tx).await.map_err(|_| StoreError::Unavailable)?.rows_affected();
            if updated != 1 {
                return Err(StoreError::Changed);
            }
        }
        tx.commit().await.map_err(|_| StoreError::Unavailable)
    }
}

#[cfg(test)]
mod principal_tests {
    use super::{Principal, RequestPrincipal};
    use crate::accounts::{Account, Authority};

    #[test]
    fn jwt_role_does_not_promote_the_current_account_tier() {
        let principal = Principal {
            account: Account {
                user_id: "user-a".into(),
                tenant_id: "tenant-a".into(),
                email: None,
                display_name: None,
                department: None,
                tier: "normal".into(),
                roles: vec!["admin".into()],
                permissions: vec![],
                status: "active".into(),
                password_hash: None,
                force_password_change: false,
                login_attempts: 0,
                locked_until: None,
                password_changed_at: None,
            },
            authority: Authority {
                roles: vec!["admin".into()],
                tier: "normal".into(),
                permissions: vec![],
            },
            jti: "session-a".into(),
            expires_at: 1,
        };

        let identity = RequestPrincipal::jwt(principal);
        assert_eq!(identity.roles(), &["admin"]);
        assert_eq!(identity.tier(), "normal");
    }
}
