//! Regression tests for the trusted, non-JWT read identity adapter.
use super::*;
use crate::assistant_catalog::CatalogIdentityResolver;
use crate::migration_content::{ContentAccess, ContentError, ContentFuture, ContentIdentity};

struct Resolved(ContentIdentity);
impl ContentAccess for Resolved {
    fn resolve<'a>(&'a self, _: &'a HeaderMap) -> ContentFuture<'a, ContentIdentity> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
    fn require_capability<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async { Err(ContentError::new(403, "unused")) })
    }
    fn rate_limit<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
        _: Option<&'a str>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async { Err(ContentError::new(403, "unused")) })
    }
    fn authorize_session<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
    ) -> ContentFuture<'a, Value> {
        Box::pin(async { Err(ContentError::new(403, "unused")) })
    }
    fn authorize_artifact_source<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a migration_content::artifacts::Artifact,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async { Err(ContentError::new(403, "unused")) })
    }
    fn require_vision_model<'a>(
        &'a self,
        _: &'a ContentIdentity,
        _: &'a str,
        _: Option<&'a str>,
    ) -> ContentFuture<'a, ()> {
        Box::pin(async { Err(ContentError::new(403, "unused")) })
    }
}
#[tokio::test]
async fn catalog_adapter_preserves_api_key_authority_and_ignores_forged_identity_headers() {
    let adapter = CatalogReadIdentity(Arc::new(Resolved(ContentIdentity {
        user_id: "key-owner".into(),
        tenant_id: "real-tenant".into(),
        tier: "premium".into(),
        roles: vec!["developer".into()],
        permissions: vec!["console:services:view".into()],
        is_authenticated: true,
        allowed_models: None,
    })));
    let mut headers = HeaderMap::new();
    headers.insert("x-ai-user-id", "attacker".parse().unwrap());
    headers.insert("x-ai-tenant-id", "foreign".parse().unwrap());
    let user = adapter.resolve(&headers).await.unwrap();
    assert_eq!(user.user_id, "key-owner");
    assert_eq!(user.tenant_id, "real-tenant");
    assert_eq!(user.tier, "premium");
    assert_eq!(user.roles, vec!["developer"]);
    assert!(user.is_authenticated);
}
#[tokio::test]
async fn catalog_adapter_never_promotes_guest_to_authenticated() {
    let adapter = CatalogReadIdentity(Arc::new(Resolved(ContentIdentity {
        user_id: "guest_cookie".into(),
        tenant_id: "public".into(),
        tier: "anonymous".into(),
        roles: vec!["guest".into()],
        permissions: vec![],
        is_authenticated: false,
        allowed_models: None,
    })));
    let user = adapter.resolve(&HeaderMap::new()).await.unwrap();
    assert!(!user.is_authenticated);
    assert_eq!(user.user_id, "guest_cookie");
    assert_eq!(user.tenant_id, "public");
    assert_eq!(user.tier, "anonymous");
}
