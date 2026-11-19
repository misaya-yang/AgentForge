//! AuthService owns all JWT/account/current-authority decisions; this is only a port adapter.
use ai_platform_service_primitives::actor_verifier::{
    ActorVerificationError, ActorVerificationFuture, JwtActorVerifier, VerifiedActor,
};
impl JwtActorVerifier for crate::service::AuthService {
    fn verify_jwt_token<'a>(&'a self, token: &'a str) -> ActorVerificationFuture<'a> {
        Box::pin(async move {
            let principal = self
                .authenticate(token)
                .await
                .map_err(|_| ActorVerificationError)?;
            Ok(VerifiedActor {
                user_id: principal.account.user_id,
                tenant_id: principal.account.tenant_id,
                tier: principal.authority.tier,
                roles: principal.authority.roles,
            })
        })
    }
}
