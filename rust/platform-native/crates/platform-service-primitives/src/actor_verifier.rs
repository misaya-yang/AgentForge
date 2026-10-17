//! Explicit JWT verification port. Token verification and current principal authority
//! stay in the authentication service; this module has no keys, claims parser or store.
use std::{future::Future, pin::Pin, sync::Arc};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedActor {
    pub user_id: String,
    pub tenant_id: String,
    pub tier: String,
    pub roles: Vec<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActorVerificationError;
pub type ActorVerificationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<VerifiedActor, ActorVerificationError>> + Send + 'a>>;
pub trait JwtActorVerifier: Send + Sync {
    fn verify_jwt_token<'a>(&'a self, token: &'a str) -> ActorVerificationFuture<'a>;
}
impl<T: JwtActorVerifier + ?Sized> JwtActorVerifier for Arc<T> {
    fn verify_jwt_token<'a>(&'a self, token: &'a str) -> ActorVerificationFuture<'a> {
        self.as_ref().verify_jwt_token(token)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialSelectionError {
    InvalidInput,
    Changed,
}
pub enum SelectedCredential<'a> {
    Jwt(&'a str),
    ApiKey(&'a str),
}
pub fn select_credential<'a>(
    headers: &'a axum::http::HeaderMap,
    key_header: &axum::http::HeaderName,
) -> Result<SelectedCredential<'a>, CredentialSelectionError> {
    if key_header == axum::http::header::AUTHORIZATION {
        return Err(CredentialSelectionError::InvalidInput);
    }
    let authorization = headers.get_all(axum::http::header::AUTHORIZATION);
    if authorization.iter().count() > 1 || headers.get_all(key_header).iter().count() > 1 {
        return Err(CredentialSelectionError::InvalidInput);
    }
    if let Some(value) = authorization.iter().next() {
        let value = value
            .to_str()
            .map_err(|_| CredentialSelectionError::InvalidInput)?;
        let mut parts = value.split_whitespace();
        if parts
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
        {
            let token = parts.next().ok_or(CredentialSelectionError::InvalidInput)?;
            if parts.next().is_some() {
                return Err(CredentialSelectionError::InvalidInput);
            }
            // Verification failure must not trigger fallback to an accompanying key.
            return Ok(SelectedCredential::Jwt(token));
        }
    }
    let key = headers
        .get(key_header)
        .ok_or(CredentialSelectionError::Changed)?
        .to_str()
        .map_err(|_| CredentialSelectionError::InvalidInput)?;
    if key.is_empty() || key.len() > 8192 {
        return Err(CredentialSelectionError::InvalidInput);
    }
    Ok(SelectedCredential::ApiKey(key))
}
