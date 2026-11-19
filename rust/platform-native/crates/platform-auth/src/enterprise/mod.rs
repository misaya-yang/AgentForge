//! Enterprise identity augments existing accounts, authority and BFF sessions.
// SPDX-License-Identifier: Apache-2.0
pub mod http;
pub mod oidc;
pub mod passkeys;
pub mod policy;
pub mod scim;
mod session;
pub mod step_up;
mod transient;
use crate::{
    browser_policy::BrowserPolicy,
    service::{AuthService, Principal},
};
use axum::{
    Json,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{collections::BTreeMap, sync::Arc};
use webauthn_rs::prelude::*;
#[derive(Debug, thiserror::Error)]
pub enum EnterpriseError {
    #[error("invalid enterprise identity request")]
    Invalid,
    #[error("enterprise identity proof rejected")]
    Proof,
    #[error("enterprise identity policy denied")]
    Forbidden,
    #[error("enterprise resource not found")]
    NotFound,
    #[error("enterprise resource conflict")]
    Conflict,
    #[error("enterprise resource version mismatch")]
    Precondition,
    #[error("enterprise identity rate limited")]
    Rate,
    #[error("enterprise identity authority unavailable")]
    Unavailable,
}
pub type Result<T> = std::result::Result<T, EnterpriseError>;
impl IntoResponse for EnterpriseError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Invalid => StatusCode::BAD_REQUEST,
            Self::Proof => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict => StatusCode::CONFLICT,
            Self::Precondition => StatusCode::PRECONDITION_FAILED,
            Self::Rate => StatusCode::TOO_MANY_REQUESTS,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        };
        (status, Json(json!({"detail":self.to_string()}))).into_response()
    }
}
impl From<sqlx::Error> for EnterpriseError {
    fn from(e: sqlx::Error) -> Self {
        if e.as_database_error()
            .and_then(|e| e.code())
            .is_some_and(|c| c == "23505")
        {
            Self::Conflict
        } else {
            Self::Unavailable
        }
    }
}
#[derive(Clone)]
pub struct EnterpriseService {
    auth: Arc<AuthService>,
    pool: PgPool,
    transient: transient::Transient,
    browser: BrowserPolicy,
    webauthn: Arc<Webauthn>,
    providers: Arc<BTreeMap<String, oidc::OidcProvider>>,
}
impl EnterpriseService {
    pub fn new(
        auth: Arc<AuthService>,
        redis: redis::aio::ConnectionManager,
        origins: Vec<String>,
        rp_id: &str,
        rp_origin: &str,
        providers: Vec<oidc::OidcProvider>,
    ) -> Result<Self> {
        let origin = Url::parse(rp_origin).map_err(|_| EnterpriseError::Invalid)?;
        if origin.scheme() != "https" || !origins.iter().any(|o| o == rp_origin) {
            return Err(EnterpriseError::Invalid);
        }
        let webauthn = WebauthnBuilder::new(rp_id, &origin)
            .map_err(|_| EnterpriseError::Invalid)?
            .rp_name("AgentForge")
            .build()
            .map_err(|_| EnterpriseError::Invalid)?;
        let mut mapped = BTreeMap::new();
        for provider in providers {
            let target = Url::parse(provider.return_url()).map_err(|_| EnterpriseError::Invalid)?;
            if !origins
                .iter()
                .any(|origin| origin == &target.origin().ascii_serialization())
            {
                return Err(EnterpriseError::Invalid);
            }
            if mapped.insert(provider.id().to_owned(), provider).is_some() {
                return Err(EnterpriseError::Invalid);
            }
        }
        Ok(Self {
            pool: auth.accounts.pool.clone(),
            auth,
            transient: transient::Transient::new(redis),
            browser: BrowserPolicy::new(origins).map_err(|_| EnterpriseError::Invalid)?,
            webauthn: Arc::new(webauthn),
            providers: Arc::new(mapped),
        })
    }
    fn now(&self) -> Result<u64> {
        (self.auth.clock)().map_err(|_| EnterpriseError::Unavailable)
    }
    async fn principal(&self, headers: &HeaderMap) -> Result<Principal> {
        if headers.get_all("authorization").iter().count() != 1 {
            return Err(EnterpriseError::Proof);
        }
        let raw = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or(EnterpriseError::Proof)?;
        let mut parts = raw.split_whitespace();
        if !parts
            .next()
            .is_some_and(|v| v.eq_ignore_ascii_case("bearer"))
        {
            return Err(EnterpriseError::Proof);
        }
        let token = parts.next().ok_or(EnterpriseError::Proof)?;
        if parts.next().is_some() {
            return Err(EnterpriseError::Proof);
        }
        let p = self.auth.authenticate(token).await.map_err(|e| {
            if matches!(e, crate::service::ServiceError::Unavailable) {
                EnterpriseError::Unavailable
            } else {
                EnterpriseError::Proof
            }
        })?;
        let record = self
            .auth
            .sessions
            .record(&p.jti)
            .await
            .map_err(|_| EnterpriseError::Unavailable)?
            .ok_or(EnterpriseError::Proof)?;
        policy::verify_session_epoch(
            &self.pool,
            &p.account.tenant_id,
            &p.account.user_id,
            record.get("auth_epoch"),
        )
        .await?;
        Ok(p)
    }

    async fn password(&self, p: &Principal, password: String) -> Result<()> {
        let hash = p
            .account
            .password_hash
            .clone()
            .ok_or(EnterpriseError::Proof)?;
        if !self
            .auth
            .password_engine()
            .verify(password, hash)
            .await
            .map_err(|_| EnterpriseError::Unavailable)?
        {
            return Err(EnterpriseError::Proof);
        }
        Ok(())
    }
}
fn bounded(value: &str, max: usize) -> Result<()> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        Err(EnterpriseError::Invalid)
    } else {
        Ok(())
    }
}
fn digest(value: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value))
}
fn revision(p: &Principal) -> Value {
    json!(p.account.password_changed_at.map(|v| v.to_rfc3339()))
}
