//! Bind remaining public owner-scoped APIs to the shared endpoint authority.
//! The host request-context layer supplies peer/anonymous state. Caller-provided
//! tenant/user headers never establish ownership.
use crate::{
    artifact_shares_api::{
        ArtifactSharesError, ArtifactSharesFuture, ArtifactSharesIdentity,
        ArtifactSharesIdentityResolver,
    },
    assistant_memory_api::AssistantMemoryIdentityResolver,
    assistant_memory_types::{
        AssistantMemoryError, AssistantMemoryFuture, AssistantMemoryIdentity,
    },
    migration_content::{ContentAccess, access::NativeContentAccess},
};
use axum::http::HeaderMap;

impl AssistantMemoryIdentityResolver for NativeContentAccess {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        _path: &'a str,
    ) -> AssistantMemoryFuture<'a, AssistantMemoryIdentity> {
        Box::pin(async move {
            let user = ContentAccess::resolve(self, headers).await.map_err(|e| {
                AssistantMemoryError::Http {
                    status: e.status,
                    detail: e.detail,
                }
            })?;
            Ok(AssistantMemoryIdentity {
                tenant_id: user.tenant_id,
                user_id: user.user_id,
                is_authenticated: user.is_authenticated,
            })
        })
    }
}
impl ArtifactSharesIdentityResolver for NativeContentAccess {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
    ) -> ArtifactSharesFuture<'a, ArtifactSharesIdentity> {
        Box::pin(async move {
            let user = ContentAccess::resolve(self, headers)
                .await
                .map_err(|e| ArtifactSharesError::detail(e.status, e.detail))?;
            Ok(ArtifactSharesIdentity {
                tenant_id: Some(user.tenant_id),
                user_id: user.user_id,
                is_authenticated: user.is_authenticated,
                roles: user.roles,
                permissions: user.permissions,
                tier: user.tier,
            })
        })
    }
}
